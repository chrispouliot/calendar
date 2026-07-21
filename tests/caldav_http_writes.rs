// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct CaldavClient;
//         pub struct ResourceWriteResult {
//             pub etag: Option<String>,
//         }
//         pub enum CaldavError {
//             HttpStatus { status: u16 },
//             Url,
//             // other transport variants are permitted
//         }
//         impl CaldavClient {
//             pub fn new(server_url: String, username: String, password: String) -> Self;
//             pub fn create_resource(
//                 &self, resource_url: &str, calendar_data: &str,
//             ) -> Result<ResourceWriteResult, CaldavError>;
//             pub fn update_resource(
//                 &self, resource_url: &str, calendar_data: &str, base_etag: &str,
//             ) -> Result<ResourceWriteResult, CaldavError>;
//             pub fn delete_resource(
//                 &self, resource_url: &str, base_etag: &str,
//             ) -> Result<(), CaldavError>;
//         }
//     }

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, CaldavError};

const TIMEOUT: Duration = Duration::from_secs(2);
const ICALENDAR: &str =
    "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:write-1\nEND:VEVENT\nEND:VCALENDAR\n";
const AUTHORIZATION: &str = "Basic Y3JlZGVudGlhbC11c2VyOnMzY3JldA==";

#[test]
fn phase11_conditionally_writes_authenticated_calendar_resources() {
    let fixture = Fixture::start(vec![
        response(201, "Created", Some("\"created-etag\"")),
        response(204, "No Content", Some("\"updated-etag\"")),
        response(204, "No Content", None),
        response(412, "Precondition Failed", None),
        response(401, "Unauthorized", None),
        redirect_response(),
    ]);
    let origin = fixture.origin();
    let resource_url = format!("{origin}/calendars/ada/work/write-1.ics");
    let client = CaldavClient::new(origin, "credential-user".into(), "s3cret".into());

    let created = client
        .create_resource(&resource_url, ICALENDAR)
        .expect("a 2xx create must succeed");
    assert_eq!(created.etag.as_deref(), Some("\"created-etag\""));

    let updated = client
        .update_resource(&resource_url, ICALENDAR, "\"base-etag\"")
        .expect("a 2xx conditional update must succeed");
    assert_eq!(updated.etag.as_deref(), Some("\"updated-etag\""));

    client
        .delete_resource(&resource_url, "\"delete-etag\"")
        .expect("a 2xx conditional delete must succeed");

    let conflict = client
        .create_resource(&resource_url, ICALENDAR)
        .expect_err("a precondition failure must remain observable");
    assert!(matches!(&conflict, CaldavError::HttpStatus { status: 412 }));

    let unauthorized = client
        .update_resource(&resource_url, ICALENDAR, "\"base-etag\"")
        .expect_err("an authentication failure must remain an HTTP status error");
    assert!(matches!(
        &unauthorized,
        CaldavError::HttpStatus { status: 401 }
    ));

    let redirect = client
        .delete_resource(&resource_url, "\"delete-etag\"")
        .expect_err("writes must not follow redirects");
    assert!(matches!(&redirect, CaldavError::HttpStatus { status: 302 }));

    let invalid_url = client
        .create_resource("/calendars/ada/work/write-1.ics", ICALENDAR)
        .expect_err("resource URLs must be absolute HTTP(S) URLs");
    assert!(matches!(&invalid_url, CaldavError::Url));
    let invalid_scheme = client
        .create_resource("ftp://example.invalid/write-1.ics", ICALENDAR)
        .expect_err("resource URLs must use HTTP(S)");
    assert!(matches!(&invalid_scheme, CaldavError::Url));

    let requests = fixture.finish();
    assert_eq!(requests.len(), 6);
    assert_create(&requests[0], "/calendars/ada/work/write-1.ics");
    assert_update(&requests[1], "/calendars/ada/work/write-1.ics");
    assert_delete(&requests[2], "/calendars/ada/work/write-1.ics");
    assert_create(&requests[3], "/calendars/ada/work/write-1.ics");
    assert_update(&requests[4], "/calendars/ada/work/write-1.ics");
    assert_delete(&requests[5], "/calendars/ada/work/write-1.ics");

    for value in [
        format!("{client:?}"),
        format!("{created:?}"),
        format!("{updated:?}"),
        format!("{conflict:?}"),
        conflict.to_string(),
        format!("{unauthorized:?}"),
        unauthorized.to_string(),
        format!("{redirect:?}"),
        redirect.to_string(),
        format!("{invalid_url:?}"),
        invalid_url.to_string(),
        format!("{invalid_scheme:?}"),
        invalid_scheme.to_string(),
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
        .map_or(Ok(0), |value| value.parse::<usize>())
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

fn assert_create(request: &str, path: &str) {
    assert_put(request, path);
    assert_eq!(header_value(request, "if-none-match"), Some("*"));
    assert_eq!(header_value(request, "if-match"), None);
}

fn assert_update(request: &str, path: &str) {
    assert_put(request, path);
    assert_eq!(header_value(request, "if-match"), Some("\"base-etag\""));
    assert_eq!(header_value(request, "if-none-match"), None);
}

fn assert_put(request: &str, path: &str) {
    assert!(request.starts_with(&format!("PUT {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, "authorization"), Some(AUTHORIZATION));
    assert_eq!(
        header_value(request, "content-type"),
        Some("text/calendar; charset=utf-8")
    );
    assert_eq!(request_body(request), ICALENDAR);
}

fn assert_delete(request: &str, path: &str) {
    assert!(request.starts_with(&format!("DELETE {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, "authorization"), Some(AUTHORIZATION));
    assert_eq!(header_value(request, "if-match"), Some("\"delete-etag\""));
}

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (header_name, value) = line.split_once(':')?;
        header_name
            .eq_ignore_ascii_case(name)
            .then_some(value.trim())
    })
}

fn request_body(request: &str) -> &str {
    request
        .split_once("\r\n\r\n")
        .expect("request has complete headers")
        .1
}

fn response(status: u16, reason: &str, etag: Option<&str>) -> String {
    let etag = etag.map_or_else(String::new, |value| format!("ETag: {value}\r\n"));
    format!("HTTP/1.1 {status} {reason}\r\n{etag}Content-Length: 0\r\nConnection: close\r\n\r\n")
}

fn redirect_response() -> String {
    "HTTP/1.1 302 Found\r\nLocation: /redirected.ics\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into()
}
