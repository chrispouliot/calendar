// Public contract pinned by this acceptance test:
//
//     pub fn initial_pull_after_provisioning_on_worker(
//         database_path: PathBuf,
//         account: Account,
//         password: oo7::Secret,
//     ) -> std::sync::mpsc::Receiver<Result<InitialPullSummary, InitialPullWorkerError>>;
//
// The bounded receiver is returned immediately. A named worker independently
// reopens the database and establishes a full VEVENT baseline for the
// account's provisioned CalDAV calendars.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavDiscovery, DiscoveredCalendar};
use calendar::backend::sync::initial_pull_after_provisioning_on_worker;
use calendar::backend::{EventRepository, SqliteRepository};
use calendar::model::Account;
use oo7::Secret;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(2);
const USERNAME: &str = "initial-pull-user-not-in-errors";
const PASSWORD: &str = "initial-pull-password-not-in-errors";

#[test]
fn initial_pull_worker_returns_immediately_and_baselines_provisioned_calendars_with_tokens() {
    let db_path = unique_temp_db_path("initial_pull");
    let _cleanup = TempDb(db_path.clone());
    let fixture = Fixture::start();
    let account = Account {
        id: Uuid::parse_str("ac120031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        server_url: fixture.origin(),
        username: USERNAME.into(),
        enabled: true,
    };
    let calendar_url = format!("{}/calendars/ada/work/", fixture.origin());
    let calendar_id = {
        let mut repository = SqliteRepository::open(&db_path).expect("open isolated database");
        let provisioned = repository
            .provision_caldav_account(
                &account,
                &CaldavDiscovery {
                    principal_url: format!("{}/principals/ada/", fixture.origin()),
                    calendar_home_url: format!("{}/calendars/ada/", fixture.origin()),
                    calendars: vec![DiscoveredCalendar {
                        href: calendar_url.clone(),
                        display_name: Some("Work".into()),
                        sync_token: Some("already-discovered-token".into()),
                        color: None,
                        writable: true,
                    }],
                },
            )
            .expect("provision account and discovered calendar");
        provisioned[0].id
    };

    let (returned_tx, returned_rx) = mpsc::sync_channel(1);
    let worker_db_path = db_path.clone();
    let worker_account = account.clone();
    thread::spawn(move || {
        returned_tx
            .send(initial_pull_after_provisioning_on_worker(
                worker_db_path,
                worker_account,
                Secret::text(PASSWORD),
            ))
            .expect("caller must receive the immediately returned handle");
    });

    fixture
        .request_received
        .recv_timeout(TIMEOUT)
        .expect("worker must reach the intentionally blocked REPORT response");
    let receiver = returned_rx
        .try_recv()
        .expect("starting the initial pull must return before the REPORT response is released");
    fixture
        .release_response
        .send(())
        .expect("fixture must still await release");

    let terminal = receiver
        .recv_timeout(TIMEOUT)
        .expect("worker must send a terminal result")
        .expect("baseline REPORT must reconcile successfully");
    assert_redacted(&format!("{terminal:?}"));
    assert!(matches!(
        receiver.recv_timeout(TIMEOUT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));

    let repository = SqliteRepository::open(&db_path).expect("reopen worker-owned database");
    let events = repository.list_events_for_calendar(calendar_id);
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].title, "Imported baseline event");
    assert_redacted(&format!("{terminal:?}"));

    let request = fixture.finish();
    assert!(request.starts_with("REPORT /calendars/ada/work/ HTTP/1.1\r\n"));
    assert_eq!(
        header_value(&request, "authorization"),
        Some(
            "Basic aW5pdGlhbC1wdWxsLXVzZXItbm90LWluLWVycm9yczppbml0aWFsLXB1bGwtcGFzc3dvcmQtbm90LWluLWVycm9ycw=="
        )
    );
    assert!(request.contains("calendar-query"));
    assert!(request.contains("comp-filter name=\"VEVENT\""));
    assert!(!request.contains("sync-collection"));
}

fn assert_redacted(text: &str) {
    assert!(!text.contains(USERNAME));
    assert!(!text.contains(PASSWORD));
}

fn unique_temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "calendar_{label}_{}_{nanos}.sqlite",
        std::process::id()
    ));
    path
}

struct TempDb(PathBuf);
impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

struct Fixture {
    origin: String,
    request_received: Receiver<()>,
    release_response: SyncSender<()>,
    server: JoinHandle<Result<String, String>>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated fixture");
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (request_tx, request_received) = mpsc::sync_channel(1);
        let (release_response, release_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut stream, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
            let request = read_request(&mut stream)?;
            request_tx
                .send(())
                .map_err(|_| "caller disappeared".to_owned())?;
            release_rx
                .recv_timeout(TIMEOUT)
                .map_err(|_| "response was not released".to_owned())?;
            stream
                .write_all(multistatus().as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(request)
        });
        Self {
            origin,
            request_received,
            release_response,
            server,
        }
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }

    fn finish(self) -> String {
        self.server.join().unwrap().unwrap()
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

fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (header, value) = line.split_once(':')?;
        header.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}

fn multistatus() -> String {
    let calendar = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:baseline-2026@example.test\r\nSUMMARY:Imported baseline event\r\nDTSTART;VALUE=DATE:20260810\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let body = format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>baseline.ics</d:href><d:propstat><d:prop><d:getetag>\"baseline-v1\"</d:getetag><c:calendar-data><![CDATA[{calendar}]]></c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    );
    format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
