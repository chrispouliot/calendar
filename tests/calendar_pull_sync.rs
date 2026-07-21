// Public contract pinned by this acceptance test:
//
//     pub mod backend::sync {
//         pub enum PullSyncError {
//             Caldav(caldav::CaldavError),
//             MissingCalendarSyncState,
//             Repository(RepositoryError),
//         }
//
//         pub fn pull_calendar_snapshot(
//             client: &caldav::CaldavClient,
//             repository: &mut SqliteRepository,
//             calendar_id: Uuid,
//         ) -> Result<RemoteSnapshotSummary, PullSyncError>;
//     }
//
// This is a blocking pull-only boundary: it reads the persisted calendar URL,
// fetches a complete snapshot, then reconciles it atomically.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, CaldavError};
use calendar::backend::sync::{PullSyncError, pull_calendar_snapshot};
use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, Event, EventSchedule, EventSyncState,
};
use chrono::NaiveDate;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn phase11_pull_calendar_sync_reconciles_complete_snapshots_and_preserves_cache_on_errors() {
    let db_path = unique_temp_db_path("pull_sync");
    let _cleanup = TempDb(db_path.clone());
    let fixture = Fixture::start(vec![
        multistatus(Some(("standup.ics", "\"standup-v1\""))),
        multistatus(None),
    ]);
    let account = account();
    let calendar = remote_calendar(account.id);
    let calendar_url = format!("{}/calendars/ada/work/", fixture.origin());
    let mut repository =
        SqliteRepository::open(&db_path).expect("opening isolated sqlite database");
    repository.save_account(&account).unwrap();
    repository.save_calendar(&calendar).unwrap();
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: calendar_url,
            sync_token: None,
        })
        .unwrap();
    let client = CaldavClient::new(fixture.origin(), "ada".into(), "secret".into());

    let imported = pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("the persisted URL's complete snapshot must import its event");
    assert_eq!(imported.added, 1);
    let state = repository
        .find_event_sync_state_by_remote_href(
            calendar.id,
            &format!("{}/calendars/ada/work/standup.ics", fixture.origin()),
        )
        .expect("imported event must be located through its sync state");
    assert_eq!(
        repository.get_event(state.event_id).unwrap().title,
        "Daily standup"
    );
    assert_eq!(state.remote_uid, "standup-2026@example.test");
    assert_eq!(state.etag.as_deref(), Some("\"standup-v1\""));

    let emptied = pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("an empty complete snapshot must reconcile");
    assert_eq!(emptied.deleted, 1);
    assert!(repository.get_event(state.event_id).is_none());
    assert!(repository.get_event_sync_state(state.event_id).is_none());
    let requests = fixture.finish();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert!(request.starts_with("REPORT /calendars/ada/work/ HTTP/1.1\r\n"));
        assert_eq!(
            header_value(request, "authorization"),
            Some("Basic YWRhOnNlY3JldA==")
        );
    }

    let cached = event(
        "e1100311-0000-0000-0000-000000000001",
        calendar.id,
        "Cached",
    );
    repository.save_event(&cached).unwrap();
    repository
        .upsert_event_sync_state(&EventSyncState {
            calendar_id: calendar.id,
            event_id: cached.id,
            remote_href: "https://cached.example.test/cached.ics".into(),
            remote_uid: "cached".into(),
            etag: Some("\"cached-v1\"".into()),
        })
        .unwrap();
    let unavailable = Fixture::start(vec![http_response(
        503,
        "Service Unavailable",
        "unavailable",
    )]);
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: format!("{}/unavailable/", unavailable.origin()),
            sync_token: None,
        })
        .unwrap();
    let error = pull_calendar_snapshot(
        &CaldavClient::new(unavailable.origin(), "ada".into(), "secret".into()),
        &mut repository,
        calendar.id,
    )
    .expect_err("HTTP failure must not reconcile the cached snapshot");
    assert!(matches!(
        error,
        PullSyncError::Caldav(CaldavError::HttpStatus { status: 503 })
    ));
    assert_eq!(repository.get_event(cached.id), Some(cached.clone()));
    assert_eq!(
        repository
            .get_event_sync_state(cached.id)
            .unwrap()
            .etag
            .as_deref(),
        Some("\"cached-v1\"")
    );
    assert_eq!(unavailable.finish().len(), 1);

    let missing_calendar = Calendar {
        id: Uuid::parse_str("ca110031-0000-0000-0000-000000000002").unwrap(),
        name: "No state".into(),
        color: "#3366cc".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local = event(
        "e1100311-0000-0000-0000-000000000002",
        missing_calendar.id,
        "Local",
    );
    repository.save_calendar(&missing_calendar).unwrap();
    repository.save_event(&local).unwrap();
    let quiet = Fixture::quiet();
    let error = pull_calendar_snapshot(
        &CaldavClient::new(quiet.origin(), "ada".into(), "secret".into()),
        &mut repository,
        missing_calendar.id,
    )
    .expect_err("a calendar without persisted sync state must not request or mutate");
    assert!(matches!(error, PullSyncError::MissingCalendarSyncState));
    assert_eq!(repository.get_event(local.id), Some(local));
    assert!(
        quiet.finish().is_empty(),
        "missing state must make no HTTP request"
    );
}

fn account() -> Account {
    Account {
        id: Uuid::parse_str("ac110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        server_url: "https://example.test/".into(),
        username: "ada".into(),
        enabled: true,
    }
}

fn remote_calendar(account_id: Uuid) -> Calendar {
    Calendar {
        id: Uuid::parse_str("ca110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        color: "#d946ef".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav { account_id },
    }
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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    path.push(format!(
        "calendar_phase11_{label}_{}_{nanos}.sqlite",
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
    server: JoinHandle<Result<Vec<String>, String>>,
}
impl Fixture {
    fn start(responses: Vec<String>) -> Self {
        Self::spawn(move |listener| {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = accept_before(listener, Instant::now() + TIMEOUT)?;
                requests.push(read_request(&mut stream)?);
                stream
                    .write_all(response.as_bytes())
                    .map_err(|e| e.to_string())?;
            }
            Ok(requests)
        })
    }
    fn quiet() -> Self {
        Self::spawn(|listener| {
            let deadline = Instant::now() + Duration::from_millis(200);
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok(_) => return Err("unexpected HTTP request".into()),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            Ok(Vec::new())
        })
    }
    fn spawn(
        server_fn: impl FnOnce(&TcpListener) -> Result<Vec<String>, String> + Send + 'static,
    ) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || server_fn(&listener));
        Self { origin, server }
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
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let header_end = loop {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("peer closed before headers".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..header_end]).map_err(|e| e.to_string())?;
    let length = header_value(headers, "content-length")
        .ok_or("missing Content-Length")?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            return Err("peer closed before body".into());
        }
        bytes.extend_from_slice(&buffer[..count]);
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
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
fn multistatus(resource: Option<(&str, &str)>) -> String {
    let body = resource.map_or_else(
        || "<d:multistatus xmlns:d=\"DAV:\"/>".into(),
        |(href, etag)| {
            let calendar = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:standup-2026@example.test\r\nSUMMARY:Daily standup\r\nDTSTART;VALUE=DATE:20260810\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
            format!(
                r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:response><d:href>{href}</d:href><d:propstat><d:prop><d:getetag>{etag}</d:getetag><c:calendar-data><![CDATA[{calendar}]]></c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response></d:multistatus>"#
            )
        },
    );
    http_response(207, "Multi-Status", &body)
}
