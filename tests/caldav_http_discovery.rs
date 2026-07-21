// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct CaldavClient;
//         pub struct CaldavDiscovery {
//             pub principal_url: String,
//             pub calendar_home_url: String,
//             pub calendars: Vec<DiscoveredCalendar>,
//         }
//         pub enum CaldavError {
//             HttpStatus { status: u16 },
//             // other transport and parsing variants are permitted
//         }
//         impl CaldavClient {
//             pub fn new(server_url: String, username: String, password: String) -> Self;
//             pub fn discover(&self) -> Result<CaldavDiscovery, CaldavError>;
//         }
//     }

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, CaldavError};

const TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn phase11_discovers_calendars_over_authenticated_propfind_requests() {
    let fixture = Fixture::start(vec![
        principal_response(),
        home_response(),
        calendars_response(),
    ]);
    let origin = fixture.origin();
    let server_url = format!("{origin}/dav/root/");

    let discovery = CaldavClient::new(server_url, "ada".into(), "s3cret".into())
        .discover()
        .expect("three successful multistatus responses must discover calendars");

    let requests = fixture.finish();
    assert_eq!(requests.len(), 3);
    assert_propfind(&requests[0], "/dav/root/", "0", &["current-user-principal"]);
    assert_propfind(
        &requests[1],
        "/principals/ada/",
        "0",
        &["calendar-home-set"],
    );
    assert_propfind(
        &requests[2],
        "/calendars/ada/",
        "1",
        &[
            "resourcetype",
            "displayname",
            "sync-token",
            "calendar-color",
            "current-user-privilege-set",
        ],
    );
    for request in &requests {
        assert_eq!(
            header_value(request, "authorization"),
            Some("Basic YWRhOnMzY3JldA==")
        );
    }

    assert_eq!(discovery.principal_url, format!("{origin}/principals/ada/"));
    assert_eq!(
        discovery.calendar_home_url,
        format!("{origin}/calendars/ada/")
    );
    assert_eq!(discovery.calendars.len(), 2);
    assert_eq!(
        discovery.calendars[0].href,
        format!("{origin}/calendars/ada/work/")
    );
    assert_eq!(discovery.calendars[0].display_name.as_deref(), Some("Work"));
    assert_eq!(
        discovery.calendars[0].sync_token.as_deref(),
        Some("work-token")
    );
    assert_eq!(discovery.calendars[0].color.as_deref(), Some("#336699FF"));
    assert!(discovery.calendars[0].writable);
    assert_eq!(
        discovery.calendars[1].href,
        format!("{origin}/calendars/ada/read-only/")
    );
    assert_eq!(
        discovery.calendars[1].display_name.as_deref(),
        Some("Read only")
    );
    assert_eq!(
        discovery.calendars[1].sync_token.as_deref(),
        Some("read-token")
    );
    assert_eq!(discovery.calendars[1].color, None);
    assert!(!discovery.calendars[1].writable);

    let unauthorized = Fixture::start(vec![http_response(401, "Unauthorized", "")]);
    let error = CaldavClient::new(
        format!("{}/protected/", unauthorized.origin()),
        "ada".into(),
        "s3cret".into(),
    )
    .discover()
    .expect_err("an HTTP error must not be parsed as a multistatus response");
    assert!(matches!(error, CaldavError::HttpStatus { status: 401 }));
    assert_eq!(unauthorized.finish().len(), 1);
}

#[test]
fn discovery_excludes_calendars_that_explicitly_lack_vevent_support() {
    let fixture = Fixture::start(vec![
        principal_response(),
        home_response(),
        mixed_component_calendars_response(),
    ]);
    let origin = fixture.origin();

    let discovery = CaldavClient::new(format!("{origin}/dav/root/"), "ada".into(), "s3cret".into())
        .discover()
        .expect("mixed calendar component support must be discoverable");

    let requests = fixture.finish();
    assert_propfind(
        &requests[2],
        "/calendars/ada/",
        "1",
        &["supported-calendar-component-set"],
    );
    assert_eq!(
        discovery
            .calendars
            .iter()
            .map(|calendar| calendar.href.clone())
            .collect::<Vec<_>>(),
        vec![
            format!("{origin}/calendars/ada/events/"),
            format!("{origin}/calendars/ada/legacy/"),
        ]
    );
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

fn assert_propfind(request: &str, path: &str, depth: &str, properties: &[&str]) {
    assert!(request.starts_with(&format!("PROPFIND {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, "depth"), Some(depth));
    assert!(
        header_value(request, "content-type")
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/xml"))
    );
    for property in properties {
        assert!(
            request.contains(property),
            "request must ask for {property}"
        );
    }
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

fn principal_response() -> String {
    http_response(
        207,
        "Multi-Status",
        r#"<d:multistatus xmlns:d="DAV:"><d:response><d:propstat><d:prop><d:current-user-principal><d:href>/principals/ada/</d:href></d:current-user-principal></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
    )
}

fn home_response() -> String {
    http_response(
        207,
        "Multi-Status",
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:propstat><d:prop><c:calendar-home-set><d:href>/calendars/ada/</d:href></c:calendar-home-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
    )
}

fn calendars_response() -> String {
    http_response(
        207,
        "Multi-Status",
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/"><d:response><d:href>work/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><d:displayname>Work</d:displayname><d:sync-token>work-token</d:sync-token><a:calendar-color>#336699FF</a:calendar-color><d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/calendars/ada/read-only/</d:href><d:propstat><d:prop><d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Read only</d:displayname><d:sync-token>read-token</d:sync-token><d:current-user-privilege-set><d:privilege><d:read/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
    )
}

fn mixed_component_calendars_response() -> String {
    http_response(
        207,
        "Multi-Status",
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>events/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><c:supported-calendar-component-set><c:comp name="VEVENT"/></c:supported-calendar-component-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>tasks/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype><c:supported-calendar-component-set><c:comp name="VTODO"/></c:supported-calendar-component-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>legacy/</d:href><d:propstat><d:prop><d:resourcetype><d:collection/><c:calendar/></d:resourcetype></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
    )
}
