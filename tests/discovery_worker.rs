// Public contract pinned by this acceptance test:
//
//     pub enum DiscoveryWorkerError { InvalidCredential, Http, Parse, WorkerPanic }
//     impl std::fmt::Display for DiscoveryWorkerError { /* redacted */ }
//     pub fn discover_on_worker(
//         server_url: String,
//         username: String,
//         password: oo7::Secret,
//     ) -> std::sync::mpsc::Receiver<Result<CaldavDiscovery, DiscoveryWorkerError>>;
//
// The returned receiver is backed by a bounded channel. Discovery runs on a
// named, non-GTK worker thread and sends exactly one redacted result.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavDiscovery, DiscoveryWorkerError, discover_on_worker};
use oo7::Secret;

const TIMEOUT: Duration = Duration::from_secs(2);
const USERNAME: &str = "discovery-user-not-in-errors";
const PASSWORD: &str = "discovery-password-not-in-errors";

#[test]
fn discovery_worker_is_nonblocking_authenticates_redacts_failures_and_sends_one_result() {
    let fixture = DiscoveryFixture::start();
    let (returned_tx, returned_rx) = mpsc::sync_channel(1);
    let server_url = format!("{}/dav/root/", fixture.origin());

    thread::spawn(move || {
        returned_tx
            .send(discover_on_worker(
                server_url,
                USERNAME.into(),
                Secret::text(PASSWORD),
            ))
            .expect("test caller must receive the immediately returned handle");
    });

    fixture
        .first_request_received
        .recv_timeout(TIMEOUT)
        .expect("worker must reach the intentionally blocked first response");
    let receiver = returned_rx
        .try_recv()
        .expect("spawning discovery must return before the first response is released");
    fixture
        .release_first_response
        .send(())
        .expect("fixture must still be waiting to release its first response");

    let discovery = receiver
        .recv_timeout(TIMEOUT)
        .expect("worker must always resolve rather than leaving a receiver hanging")
        .expect("text secret must authenticate and discover calendars");
    assert_discovery(&discovery, &fixture.origin());
    assert_redacted_debug(&discovery);
    assert_disconnected_after_one(receiver);
    let requests = fixture.finish();
    assert_eq!(requests.len(), 3);
    for request in requests {
        assert_eq!(
            header_value(&request, "authorization"),
            Some(
                "Basic ZGlzY292ZXJ5LXVzZXItbm90LWluLWVycm9yczpkaXNjb3ZlcnktcGFzc3dvcmQtbm90LWluLWVycm9ycw=="
            )
        );
    }

    let no_http = NoHttpFixture::start();
    let invalid_credential = discover_on_worker(
        format!("{}/must-not-connect", no_http.origin),
        USERNAME.into(),
        Secret::blob(PASSWORD),
    )
    .recv_timeout(TIMEOUT)
    .expect("invalid credentials must resolve without waiting for HTTP")
    .expect_err("a blob secret is not a CalDAV text password");
    assert!(matches!(
        invalid_credential,
        DiscoveryWorkerError::InvalidCredential
    ));
    assert_redacted(&invalid_credential);
    no_http.finish();

    let http = SingleResponseFixture::start(http_response(401, "Unauthorized", ""));
    let http_error = receive_error(discover_on_worker(
        format!("{}/protected/", http.origin),
        USERNAME.into(),
        Secret::text(PASSWORD),
    ));
    assert!(matches!(http_error, DiscoveryWorkerError::Http));
    assert_redacted(&http_error);
    http.finish();

    let parse = SingleResponseFixture::start(http_response(207, "Multi-Status", "not xml"));
    let parse_error = receive_error(discover_on_worker(
        format!("{}/broken/", parse.origin),
        USERNAME.into(),
        Secret::text(PASSWORD),
    ));
    assert!(matches!(parse_error, DiscoveryWorkerError::Parse));
    assert_redacted(&parse_error);
    parse.finish();
}

fn receive_error(
    receiver: Receiver<Result<CaldavDiscovery, DiscoveryWorkerError>>,
) -> DiscoveryWorkerError {
    let error = receiver
        .recv_timeout(TIMEOUT)
        .expect("a worker failure must resolve rather than hanging")
        .expect_err("fixture response must fail discovery");
    assert_disconnected_after_one(receiver);
    error
}

fn assert_disconnected_after_one(
    receiver: Receiver<Result<CaldavDiscovery, DiscoveryWorkerError>>,
) {
    assert!(matches!(
        receiver.recv_timeout(TIMEOUT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));
}

fn assert_redacted(error: &DiscoveryWorkerError) {
    let debug = format!("{error:?}");
    let display = error.to_string();
    assert_redacted_text(&debug);
    assert_redacted_text(&display);
}

fn assert_redacted_debug(discovery: &CaldavDiscovery) {
    assert_redacted_text(&format!("{discovery:?}"));
}

fn assert_redacted_text(representation: &str) {
    assert!(!representation.contains(USERNAME));
    assert!(!representation.contains(PASSWORD));
}

fn assert_discovery(discovery: &CaldavDiscovery, origin: &str) {
    assert_eq!(discovery.principal_url, format!("{origin}/principals/ada/"));
    assert_eq!(
        discovery.calendar_home_url,
        format!("{origin}/calendars/ada/")
    );
    assert_eq!(discovery.calendars.len(), 1);
    assert_eq!(
        discovery.calendars[0].href,
        format!("{origin}/calendars/ada/work/")
    );
    assert_eq!(discovery.calendars[0].display_name.as_deref(), Some("Work"));
    assert!(discovery.calendars[0].writable);
}

struct DiscoveryFixture {
    origin: String,
    first_request_received: Receiver<()>,
    release_first_response: SyncSender<()>,
    server: JoinHandle<Result<Vec<String>, String>>,
}

impl DiscoveryFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated local fixture");
        listener
            .set_nonblocking(true)
            .expect("configure fixture listener");
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (first_request_tx, first_request_received) = mpsc::sync_channel(1);
        let (release_first_response, release_first_response_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let mut requests = Vec::new();
            for (index, response) in [principal_response(), home_response(), calendars_response()]
                .into_iter()
                .enumerate()
            {
                let (mut stream, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
                let request = read_request(&mut stream)?;
                if index == 0 {
                    first_request_tx
                        .send(())
                        .map_err(|_| "caller disappeared".to_owned())?;
                    release_first_response_rx
                        .recv_timeout(TIMEOUT)
                        .map_err(|_| "first response was not released".to_owned())?;
                }
                stream
                    .write_all(response.as_bytes())
                    .map_err(|error| error.to_string())?;
                requests.push(request);
            }
            Ok(requests)
        });
        Self {
            origin,
            first_request_received,
            release_first_response,
            server,
        }
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }

    fn finish(self) -> Vec<String> {
        self.server
            .join()
            .expect("fixture server must not panic")
            .expect("bounded requests")
    }
}

struct NoHttpFixture {
    origin: String,
    listener: TcpListener,
}

impl NoHttpFixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind no-HTTP fixture");
        listener.set_nonblocking(true).unwrap();
        Self {
            origin: format!("http://{}", listener.local_addr().unwrap()),
            listener,
        }
    }

    fn finish(self) {
        assert!(
            matches!(self.listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
        );
    }
}

struct SingleResponseFixture {
    origin: String,
    server: JoinHandle<Result<(), String>>,
}

impl SingleResponseFixture {
    fn start(response: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated local fixture");
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
            read_request(&mut stream)?;
            stream
                .write_all(response.as_bytes())
                .map_err(|error| error.to_string())
        });
        Self { origin, server }
    }

    fn finish(self) {
        self.server
            .join()
            .expect("fixture server must not panic")
            .expect("one request")
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
        .ok_or("missing content length")?
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

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header.eq_ignore_ascii_case(name).then_some(value.trim())
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
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>work/</d:href><d:propstat><d:prop><d:resourcetype><c:calendar/></d:resourcetype><d:displayname>Work</d:displayname><d:current-user-privilege-set><d:privilege><d:write/></d:privilege></d:current-user-privilege-set></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#,
    )
}
