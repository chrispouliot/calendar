// Public contract pinned by this acceptance test:
//
//     pub fn sync_account_on_worker(
//         database_path: PathBuf, account: Account, password: oo7::Secret,
//     ) -> std::sync::mpsc::Receiver<Result<AccountSyncSummary, AccountSyncWorkerError>>;
//
// The bounded receiver returns immediately; a named worker independently opens
// SQLite, pushes then pulls each of the account's CalDAV calendars, and sends
// one redacted aggregate terminal result.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::sync::sync_account_on_worker;
use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, Event, EventSchedule,
    PendingSyncOperation,
};
use chrono::NaiveDate;
use oo7::Secret;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(2);
const USERNAME: &str = "account-sync-user-not-in-results";
const PASSWORD: &str = "account-sync-password-not-in-results";

#[test]
fn account_sync_worker_returns_immediately_pushes_before_pulling_and_persists_both_sides() {
    let db_path = unique_temp_db_path("account_sync");
    let _cleanup = TempDb(db_path.clone());
    let fixture = Fixture::start();
    let account = Account {
        id: Uuid::parse_str("ac130031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        server_url: fixture.origin(),
        username: USERNAME.into(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("ca130031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        color: "#d946ef".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local = event(
        "e1300031-0000-0000-0000-000000000001",
        calendar.id,
        "Pending local event",
    );
    {
        let mut repository = SqliteRepository::open(&db_path).expect("open isolated database");
        repository.save_account(&account).unwrap();
        repository.save_calendar(&calendar).unwrap();
        repository
            .upsert_calendar_sync_state(&CalendarSyncState {
                calendar_id: calendar.id,
                remote_url: format!("{}/calendars/ada/work/", fixture.origin()),
                sync_token: None,
            })
            .unwrap();
        repository.save_event(&local).unwrap();
        repository
            .upsert_pending_sync_operation(&PendingSyncOperation::Create {
                calendar_id: calendar.id,
                event_id: local.id,
                remote_uid: "pending-local@example.test".into(),
            })
            .unwrap();
    }

    let (returned_tx, returned_rx) = mpsc::sync_channel(1);
    let worker_db_path = db_path.clone();
    let worker_account = account.clone();
    thread::spawn(move || {
        returned_tx
            .send(sync_account_on_worker(
                worker_db_path,
                worker_account,
                Secret::text(PASSWORD),
            ))
            .expect("caller must receive the immediately returned receiver");
    });

    fixture
        .first_request_received
        .recv_timeout(TIMEOUT)
        .expect("worker must reach the intentionally blocked PUT response");
    let receiver = returned_rx
        .try_recv()
        .expect("starting account sync must return before the PUT response is released");
    fixture
        .release_first_response
        .send(())
        .expect("fixture must still await release");

    let terminal = receiver
        .recv_timeout(TIMEOUT)
        .expect("worker must send one terminal result")
        .expect("the authenticated push and pull must succeed");
    assert_redacted(&format!("{terminal:?}"));
    assert!(matches!(
        receiver.recv_timeout(TIMEOUT),
        Err(mpsc::RecvTimeoutError::Disconnected)
    ));

    let repository = SqliteRepository::open(&db_path).expect("reopen worker-owned database");
    assert!(repository.get_pending_sync_operation(local.id).is_none());
    assert_eq!(
        repository
            .get_event_sync_state(local.id)
            .unwrap()
            .etag
            .as_deref(),
        Some("\"local-v1\"")
    );
    let events = repository.list_events_for_calendar(calendar.id);
    assert_eq!(events.len(), 2);
    assert!(
        events
            .iter()
            .any(|event| event.title == "Remote imported event")
    );

    let requests = fixture.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with(&format!(
        "PUT /calendars/ada/work/{}.ics HTTP/1.1\r\n",
        local.id
    )));
    assert!(requests[1].starts_with("REPORT /calendars/ada/work/ HTTP/1.1\r\n"));
    for request in requests {
        assert_eq!(
            header_value(&request, "authorization"),
            Some(
                "Basic YWNjb3VudC1zeW5jLXVzZXItbm90LWluLXJlc3VsdHM6YWNjb3VudC1zeW5jLXBhc3N3b3JkLW5vdC1pbi1yZXN1bHRz"
            )
        );
    }
}

#[test]
fn account_sync_worker_uploads_orphan_local_events_before_pulling() {
    let db_path = unique_temp_db_path("account_sync_orphan");
    let _cleanup = TempDb(db_path.clone());
    let fixture = Fixture::start();
    let account = Account {
        id: Uuid::parse_str("ac130032-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        server_url: fixture.origin(),
        username: USERNAME.into(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("ca130032-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        color: "#d946ef".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local = event(
        "e1300032-0000-0000-0000-000000000001",
        calendar.id,
        "Orphan local event",
    );
    {
        let mut repository = SqliteRepository::open(&db_path).expect("open isolated database");
        repository.save_account(&account).unwrap();
        repository.save_calendar(&calendar).unwrap();
        repository
            .upsert_calendar_sync_state(&CalendarSyncState {
                calendar_id: calendar.id,
                remote_url: format!("{}/calendars/ada/work/", fixture.origin()),
                sync_token: None,
            })
            .unwrap();
        repository.save_event(&local).unwrap();
        assert!(repository.get_event_sync_state(local.id).is_none());
        assert!(repository.get_pending_sync_operation(local.id).is_none());
    }

    let receiver = sync_account_on_worker(db_path.clone(), account, Secret::text(PASSWORD));
    fixture
        .first_request_received
        .recv_timeout(TIMEOUT)
        .expect("worker must reach the upload before pulling");
    fixture
        .release_first_response
        .send(())
        .expect("fixture must still await release");

    let terminal = receiver
        .recv_timeout(TIMEOUT)
        .expect("worker must send one terminal result")
        .expect("the authenticated push and pull must succeed");
    assert_eq!(terminal.pushed.created, 1);

    let repository = SqliteRepository::open(&db_path).expect("reopen worker-owned database");
    let state = repository
        .get_event_sync_state(local.id)
        .expect("orphan event must receive remote sync metadata");
    assert_eq!(
        state.remote_href,
        format!("{}/calendars/ada/work/{}.ics", fixture.origin(), local.id)
    );
    assert_eq!(state.remote_uid, local.id.to_string());
    assert!(repository.get_pending_sync_operation(local.id).is_none());

    let requests = fixture.finish();
    assert_eq!(requests.len(), 2);
    assert!(requests[0].starts_with(&format!(
        "PUT /calendars/ada/work/{}.ics HTTP/1.1\r\n",
        local.id
    )));
    assert!(requests[0].contains(&format!("\r\nUID:{}\r\n", state.remote_uid)));
    assert!(requests[1].starts_with("REPORT /calendars/ada/work/ HTTP/1.1\r\n"));
    for request in requests {
        assert_eq!(
            header_value(&request, "authorization"),
            Some(
                "Basic YWNjb3VudC1zeW5jLXVzZXItbm90LWluLXJlc3VsdHM6YWNjb3VudC1zeW5jLXBhc3N3b3JkLW5vdC1pbi1yZXN1bHRz"
            )
        );
    }
}

fn assert_redacted(text: &str) {
    assert!(!text.contains(USERNAME));
    assert!(!text.contains(PASSWORD));
}

fn event(id: &str, calendar_id: Uuid, title: &str) -> Event {
    Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id,
        title: title.into(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    }
}

fn unique_temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "calendar_{label}_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
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
    first_request_received: Receiver<()>,
    release_first_response: SyncSender<()>,
    server: JoinHandle<Result<Vec<String>, String>>,
}

impl Fixture {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind isolated fixture");
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let (first_request_tx, first_request_received) = mpsc::sync_channel(1);
        let (release_first_response, release_first_response_rx) = mpsc::sync_channel(1);
        let server = thread::spawn(move || {
            let (mut put, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
            let first = read_request(&mut put)?;
            first_request_tx
                .send(())
                .map_err(|_| "caller disappeared".to_owned())?;
            release_first_response_rx
                .recv_timeout(TIMEOUT)
                .map_err(|_| "PUT response was not released".to_owned())?;
            if first.starts_with("REPORT ") {
                put.write_all(multistatus().as_bytes())
                    .map_err(|error| error.to_string())?;
                return Ok(vec![first]);
            }
            put.write_all(response(201, "Created", Some("\"local-v1\"")).as_bytes())
                .map_err(|error| error.to_string())?;

            let (mut report, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
            let second = read_request(&mut report)?;
            report
                .write_all(multistatus().as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(vec![first, second])
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

fn response(status: u16, reason: &str, etag: Option<&str>) -> String {
    let etag = etag.map_or_else(String::new, |value| format!("ETag: {value}\r\n"));
    format!("HTTP/1.1 {status} {reason}\r\n{etag}Content-Length: 0\r\nConnection: close\r\n\r\n")
}

fn multistatus() -> String {
    let calendar = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:remote-imported@example.test\r\nSUMMARY:Remote imported event\r\nDTSTART;VALUE=DATE:20260811\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let body = format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>remote.ics</d:href><d:propstat><d:prop><d:getetag>"remote-v1"</d:getetag><c:calendar-data><![CDATA[{calendar}]]></c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
    );
    format!(
        "HTTP/1.1 207 Multi-Status\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
