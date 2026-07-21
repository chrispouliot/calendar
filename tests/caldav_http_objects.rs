// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct CaldavClient;
//         pub struct ResourceRecord {
//             pub href: String,
//             pub response_status: Option<u16>,
//             pub etag: Option<String>,
//             pub calendar_data: Option<String>,
//         }
//         pub enum CaldavError {
//             HttpStatus { status: u16 },
//             Xml(ParseError),
//             // other transport and URL variants are permitted
//         }
//         impl CaldavClient {
//             pub fn new(server_url: String, username: String, password: String) -> Self;
//             pub fn fetch_resources(
//                 &self,
//                 calendar_url: &str,
//             ) -> Result<Vec<ResourceRecord>, CaldavError>;
//         }
//     }

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, CaldavError};

const TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn phase11_fetches_calendar_objects_with_a_calendar_query_report() {
    let fixture = Fixture::start(vec![multistatus_response()]);
    let origin = fixture.origin();
    let calendar_url = format!("{origin}/calendars/ada/work/");
    let client = CaldavClient::new(origin.clone(), "credential-user".into(), "s3cret".into());

    let resources = client
        .fetch_resources(&calendar_url)
        .expect("a successful calendar-query multistatus must return resource records");

    let requests = fixture.finish();
    assert_eq!(requests.len(), 1, "one calendar-query REPORT is sufficient");
    assert_calendar_query(&requests[0], "/calendars/ada/work/");
    assert_eq!(
        header_value(&requests[0], "authorization"),
        Some("Basic Y3JlZGVudGlhbC11c2VyOnMzY3JldA==")
    );

    assert_eq!(
        resources.len(),
        2,
        "response records must retain server order"
    );
    assert_eq!(
        resources[0].href,
        format!("{calendar_url}team-meeting.ics"),
        "a relative href is resolved against the supplied calendar URL"
    );
    assert_eq!(resources[0].response_status, None);
    assert_eq!(resources[0].etag.as_deref(), Some("\"etag-1\""));
    assert_eq!(resources[0].calendar_data.as_deref(), Some(ICALENDAR));
    assert_eq!(
        resources[1].href,
        format!("{origin}/calendars/ada/work/deleted.ics")
    );
    assert_eq!(resources[1].response_status, Some(404));
    assert_eq!(resources[1].etag, None);
    assert_eq!(resources[1].calendar_data, None);
    let records_debug = format!("{resources:?}");
    assert!(!records_debug.contains("credential-user"));
    assert!(!records_debug.contains("s3cret"));

    let http_error = Fixture::start(vec![http_response(503, "Service Unavailable", "not xml")]);
    let error = CaldavClient::new(
        http_error.origin(),
        "credential-user".into(),
        "s3cret".into(),
    )
    .fetch_resources(&format!("{}/calendars/ada/work/", http_error.origin()))
    .expect_err("a non-2xx response must fail before its body is parsed");
    assert!(matches!(error, CaldavError::HttpStatus { status: 503 }));
    assert_eq!(http_error.finish().len(), 1);

    let malformed = Fixture::start(vec![http_response(207, "Multi-Status", "<d:multistatus")]);
    let error = CaldavClient::new(
        malformed.origin(),
        "credential-user".into(),
        "s3cret".into(),
    )
    .fetch_resources(&format!("{}/calendars/ada/work/", malformed.origin()))
    .expect_err("a malformed successful response must report an XML error");
    assert!(matches!(error, CaldavError::Xml(_)));
    assert_eq!(malformed.finish().len(), 1);
}

const ICALENDAR: &str =
    "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:team-1\nEND:VEVENT\nEND:VCALENDAR\n";

struct Fixture {
    origin: String,
    server: JoinHandle<Result<Vec<String>, String>>,
}

impl Fixture {
    fn start(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated local fixture");
        listener
            .set_nonblocking(true)
            .expect("configure fixture listener timeout");
        let origin = format!("http://{}", listener.local_addr().expect("fixture address"));
        let server = thread::spawn(move || {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut stream, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
                let request = read_request(&mut stream)?;
                stream
                    .write_all(response.as_bytes())
                    .map_err(|error| format!("write response: {error}"))?;
                requests.push(request);
            }
            Ok(requests)
        });
        Self { origin, server }
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }

    fn finish(self) -> Vec<String> {
        self.server
            .join()
            .expect("fixture server thread must not panic")
            .expect("fixture server must receive its bounded request sequence")
    }
}

fn accept_before(
    listener: &TcpListener,
    deadline: Instant,
) -> Result<(TcpStream, std::net::SocketAddr), String> {
    loop {
        match listener.accept() {
            Ok(connection) => return Ok(connection),
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(format!("accept request: {error}")),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<String, String> {
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|error| format!("set request timeout: {error}"))?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let header_end = loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("read request: {error}"))?;
        if count == 0 {
            return Err("peer closed before request headers".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end])
        .map_err(|error| format!("request headers are not UTF-8: {error}"))?;
    let content_length = header_value(headers, "content-length")
        .ok_or_else(|| "request has no Content-Length".to_owned())?
        .parse::<usize>()
        .map_err(|error| format!("invalid Content-Length: {error}"))?;
    while bytes.len() < header_end + content_length {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| format!("read request body: {error}"))?;
        if count == 0 {
            return Err("peer closed before complete request body".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).map_err(|error| format!("request is not UTF-8: {error}"))
}

fn assert_calendar_query(request: &str, path: &str) {
    assert!(request.starts_with(&format!("REPORT {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, "depth"), Some("1"));
    assert!(
        header_value(request, "content-type")
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/xml"))
    );
    assert!(request.contains("calendar-query"));
    assert!(request.contains("getetag"));
    assert!(request.contains("calendar-data"));
    assert!(request.contains("comp-filter name=\"VCALENDAR\""));
    assert!(request.contains("comp-filter name=\"VEVENT\""));
}

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn http_response(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn multistatus_response() -> String {
    let body = format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>team-meeting.ics</d:href><d:propstat><d:prop><d:getetag>"etag-1"</d:getetag><c:calendar-data><![CDATA[{ICALENDAR}]]></c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/calendars/ada/work/deleted.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response></d:multistatus>"#,
    );
    http_response(207, "Multi-Status", &body)
}
