// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct CaldavClient;
//         pub struct SyncCollection {
//             pub sync_token: String,
//             pub changes: Vec<ResourceRecord>,
//         }
//         pub enum CaldavError {
//             InvalidSyncToken,
//             HttpStatus { status: u16 },
//             Xml(ParseError),
//             Url,
//             // other transport variants are permitted
//         }
//         impl CaldavClient {
//             pub fn new(server_url: String, username: String, password: String) -> Self;
//             pub fn fetch_changes(
//                 &self, calendar_url: &str, prior_sync_token: &str,
//             ) -> Result<SyncCollection, CaldavError>;
//         }
//     }

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, CaldavError};

const TIMEOUT: Duration = Duration::from_secs(2);
const PRIOR_TOKEN: &str = "urn:token:old&cursor=<one>";
const ICALENDAR: &str =
    "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:changed-1\nEND:VEVENT\nEND:VCALENDAR\n";

#[test]
fn phase11_fetches_authenticated_sync_collection_changes() {
    let invalid_token_fixture = Fixture::start(vec![]);
    let invalid_token = CaldavClient::new(
        invalid_token_fixture.origin(),
        "credential-user".into(),
        "s3cret".into(),
    )
    .fetch_changes(
        &format!("{}/calendars/ada/work/", invalid_token_fixture.origin()),
        " \t\n",
    )
    .expect_err("an empty sync token must be rejected before any HTTP request");
    assert!(matches!(invalid_token, CaldavError::InvalidSyncToken));
    assert!(invalid_token_fixture.finish().is_empty());

    let fixture = Fixture::start(vec![
        sync_response(),
        response(403, "Forbidden", "invalid sync token"),
        response(207, "Multi-Status", "<d:multistatus"),
        response(302, "Found", ""),
    ]);
    let origin = fixture.origin();
    let calendar_url = format!("{origin}/calendars/ada/work/");
    let client = CaldavClient::new(origin.clone(), "credential-user".into(), "s3cret".into());

    let changes = client
        .fetch_changes(&calendar_url, PRIOR_TOKEN)
        .expect("a successful sync-collection multistatus must return changes");
    assert_eq!(changes.sync_token, "urn:token:new&cursor=2");
    assert_eq!(changes.changes.len(), 2, "changes retain server order");
    assert_eq!(
        changes.changes[0].href,
        format!("{calendar_url}changed.ics")
    );
    assert_eq!(changes.changes[0].response_status, None);
    assert_eq!(changes.changes[0].etag.as_deref(), Some("\"etag-1\""));
    assert_eq!(changes.changes[0].calendar_data.as_deref(), Some(ICALENDAR));
    assert_eq!(
        changes.changes[1].href,
        "http://absolute.example/calendars/ada/work/deleted.ics"
    );
    assert_eq!(changes.changes[1].response_status, Some(404));
    assert_eq!(changes.changes[1].etag, None);
    assert_eq!(changes.changes[1].calendar_data, None);

    let forbidden = client
        .fetch_changes(&calendar_url, PRIOR_TOKEN)
        .expect_err("an invalid-token response must remain an HTTP status error");
    assert!(matches!(forbidden, CaldavError::HttpStatus { status: 403 }));

    let malformed = client
        .fetch_changes(&calendar_url, PRIOR_TOKEN)
        .expect_err("malformed successful XML must report an XML error");
    assert!(matches!(malformed, CaldavError::Xml(_)));

    let redirect = client
        .fetch_changes(&calendar_url, PRIOR_TOKEN)
        .expect_err("sync requests must not follow redirects");
    assert!(matches!(redirect, CaldavError::HttpStatus { status: 302 }));

    let relative_url = client
        .fetch_changes("/calendars/ada/work/", PRIOR_TOKEN)
        .expect_err("calendar URLs must be absolute");
    assert!(matches!(relative_url, CaldavError::Url));
    let non_http_url = client
        .fetch_changes("ftp://example.invalid/work/", PRIOR_TOKEN)
        .expect_err("calendar URLs must use HTTP(S)");
    assert!(matches!(non_http_url, CaldavError::Url));

    let requests = fixture.finish();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_sync_collection(request, "/calendars/ada/work/");
        assert_eq!(
            header_value(request, "authorization"),
            Some("Basic Y3JlZGVudGlhbC11c2VyOnMzY3JldA==")
        );
    }
    for value in [
        format!("{client:?}"),
        format!("{changes:?}"),
        format!("{forbidden:?}"),
        forbidden.to_string(),
    ] {
        assert!(!value.contains("credential-user"));
        assert!(!value.contains("s3cret"));
    }
}

struct Fixture {
    origin: String,
    server: JoinHandle<Result<Vec<String>, String>>,
}

impl Fixture {
    fn start(responses: Vec<String>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated local fixture");
        listener
            .set_nonblocking(true)
            .expect("configure fixture listener");
        let origin = format!("http://{}", listener.local_addr().expect("fixture address"));
        let server = thread::spawn(move || {
            let mut requests = Vec::with_capacity(responses.len());
            for response in responses {
                let (mut stream, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
                let request = read_request(&mut stream)?;
                stream
                    .write_all(response.as_bytes())
                    .map_err(|error| error.to_string())?;
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
            .expect("fixture thread")
            .expect("bounded requests")
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
                thread::sleep(Duration::from_millis(5))
            }
            Err(error) => return Err(error.to_string()),
        }
    }
}

fn read_request(stream: &mut TcpStream) -> Result<String, String> {
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|error| error.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let header_end = loop {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("peer closed before headers".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|error| error.to_string())?;
    let length = header_value(headers, "content-length")
        .ok_or("missing Content-Length")?
        .parse::<usize>()
        .map_err(|error| error.to_string())?;
    while bytes.len() < header_end + length {
        let count = stream
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("peer closed before body".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).map_err(|error| error.to_string())
}

fn assert_sync_collection(request: &str, path: &str) {
    assert!(request.starts_with(&format!("REPORT {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, "depth"), Some("1"));
    assert!(
        header_value(request, "content-type")
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/xml"))
    );
    let body = request.split_once("\r\n\r\n").expect("complete request").1;
    assert!(body.contains("sync-collection"));
    assert!(body.contains("urn:token:old&amp;cursor=&lt;one&gt;"));
    assert!(body.contains("sync-level>1<"));
    assert!(body.contains("getetag"));
    assert!(body.contains("calendar-data"));
}

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn response(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn sync_response() -> String {
    response(
        207,
        "Multi-Status",
        &format!(
            r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:sync-token>urn:token:new&amp;cursor=2</d:sync-token><d:response><d:href>changed.ics</d:href><d:propstat><d:prop><d:getetag>"etag-1"</d:getetag><c:calendar-data><![CDATA[{ICALENDAR}]]></c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>http://absolute.example/calendars/ada/work/deleted.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response></d:multistatus>"#
        ),
    )
}
