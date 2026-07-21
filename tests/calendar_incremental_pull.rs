// Public contract pinned by this acceptance test:
//
//     pub fn pull_calendar_snapshot(
//         client: &CaldavClient,
//         repository: &mut SqliteRepository,
//         calendar_id: Uuid,
//     ) -> Result<RemoteSnapshotSummary, PullSyncError>;
//
// A persisted token selects sync-collection; an invalid token retries once with
// a complete calendar-query snapshot and clears that token atomically.

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
fn phase11_pull_runner_selects_incremental_and_recovers_invalid_tokens() {
    let db_path = unique_temp_db_path("incremental_pull");
    let _cleanup = TempDb(db_path.clone());
    let account = account();
    let calendar = calendar(account.id);
    let old_token = "token-old";
    let initial = Fixture::start(vec![sync_multistatus(
        "token-new",
        Some("Incremental remote"),
    )]);
    let calendar_url = format!("{}/calendars/ada/work/", initial.origin());
    let mut repository = SqliteRepository::open(&db_path).unwrap();
    repository.save_account(&account).unwrap();
    repository.save_calendar(&calendar).unwrap();
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
            remote_href: format!("{calendar_url}changed.ics"),
            remote_uid: "changed@example.test".into(),
            etag: Some("\"old\"".into()),
        })
        .unwrap();
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: calendar_url.clone(),
            sync_token: Some(old_token.into()),
        })
        .unwrap();

    let client = CaldavClient::new(initial.origin(), "ada".into(), "secret".into());
    let updated = pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("a token selects and applies its incremental collection");
    assert_eq!(updated.updated, 1);
    assert_eq!(
        repository.get_event(cached.id).unwrap().title,
        "Incremental remote"
    );
    assert_eq!(
        repository
            .get_calendar_sync_state(calendar.id)
            .unwrap()
            .sync_token
            .as_deref(),
        Some("token-new")
    );
    let initial_requests = initial.finish();
    assert_eq!(initial_requests.len(), 1);
    assert_sync_collection(&initial_requests[0], "/calendars/ada/work/", old_token);

    let invalid = Fixture::start(vec![
        response(403, "Forbidden", "invalid sync token"),
        full_multistatus(None),
    ]);
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: format!("{}/calendars/ada/work/", invalid.origin()),
            sync_token: Some("token-new".into()),
        })
        .unwrap();
    let recovered = pull_calendar_snapshot(
        &CaldavClient::new(invalid.origin(), "ada".into(), "secret".into()),
        &mut repository,
        calendar.id,
    )
    .expect("an invalid token retries with a complete snapshot");
    assert_eq!(
        recovered.deleted, 1,
        "fallback snapshot deletes absent cached resources"
    );
    assert!(repository.get_event(cached.id).is_none());
    assert_eq!(
        repository
            .get_calendar_sync_state(calendar.id)
            .unwrap()
            .sync_token,
        None
    );
    drop(repository);
    let mut repository = SqliteRepository::open(&db_path).unwrap();
    assert_eq!(
        repository
            .get_calendar_sync_state(calendar.id)
            .unwrap()
            .sync_token,
        None
    );
    let invalid_requests = invalid.finish();
    assert_eq!(invalid_requests.len(), 2);
    assert_sync_collection(&invalid_requests[0], "/calendars/ada/work/", "token-new");
    assert_calendar_query(&invalid_requests[1], "/calendars/ada/work/");

    let no_token = Fixture::start(vec![full_multistatus(None)]);
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: format!("{}/calendars/ada/work/", no_token.origin()),
            sync_token: None,
        })
        .unwrap();
    pull_calendar_snapshot(
        &CaldavClient::new(no_token.origin(), "ada".into(), "secret".into()),
        &mut repository,
        calendar.id,
    )
    .expect("a cleared token uses a complete snapshot directly");
    let no_token_requests = no_token.finish();
    assert_eq!(no_token_requests.len(), 1);
    assert_calendar_query(&no_token_requests[0], "/calendars/ada/work/");

    let protected = event(
        "e1100311-0000-0000-0000-000000000002",
        calendar.id,
        "Protected cache",
    );
    repository.save_event(&protected).unwrap();
    repository
        .upsert_event_sync_state(&EventSyncState {
            calendar_id: calendar.id,
            event_id: protected.id,
            remote_href: "https://cached.test/protected.ics".into(),
            remote_uid: "protected".into(),
            etag: Some("\"v1\"".into()),
        })
        .unwrap();
    let unavailable = Fixture::start(vec![response(503, "Service Unavailable", "unavailable")]);
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: format!("{}/calendars/ada/work/", unavailable.origin()),
            sync_token: Some("retry-token".into()),
        })
        .unwrap();
    let error = pull_calendar_snapshot(
        &CaldavClient::new(unavailable.origin(), "ada".into(), "secret".into()),
        &mut repository,
        calendar.id,
    )
    .expect_err("non-token HTTP failures must not fall back");
    assert!(matches!(
        error,
        PullSyncError::Caldav(CaldavError::HttpStatus { status: 503 })
    ));
    assert_eq!(repository.get_event(protected.id), Some(protected));
    assert_eq!(
        repository
            .get_calendar_sync_state(calendar.id)
            .unwrap()
            .sync_token
            .as_deref(),
        Some("retry-token")
    );
    assert_eq!(unavailable.finish().len(), 1);

    let missing = Calendar {
        id: Uuid::new_v4(),
        name: "Missing state".into(),
        color: "#000000".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    repository.save_calendar(&missing).unwrap();
    let quiet = Fixture::quiet();
    let error = pull_calendar_snapshot(
        &CaldavClient::new(quiet.origin(), "ada".into(), "secret".into()),
        &mut repository,
        missing.id,
    )
    .expect_err("missing state must not request or mutate");
    assert!(matches!(error, PullSyncError::MissingCalendarSyncState));
    assert!(quiet.finish().is_empty());
}

fn account() -> Account {
    Account {
        id: Uuid::parse_str("ac110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        server_url: "https://example.test".into(),
        username: "ada".into(),
        enabled: true,
    }
}
fn calendar(account_id: Uuid) -> Calendar {
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
    std::env::temp_dir().join(format!(
        "calendar_phase11_{label}_{}_{}.sqlite",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
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
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    Err(e) => return Err(e.to_string()),
                }
            }
            Ok(Vec::new())
        })
    }
    fn spawn(f: impl FnOnce(&TcpListener) -> Result<Vec<String>, String> + Send + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        Self {
            origin,
            server: thread::spawn(move || f(&listener)),
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
            Ok(c) => return Ok(c),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(5))
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}
fn read_request(stream: &mut TcpStream) -> Result<String, String> {
    stream
        .set_read_timeout(Some(TIMEOUT))
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    let mut buffer = [0; 1024];
    let end = loop {
        let n = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("peer closed before headers".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(i) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let headers = std::str::from_utf8(&bytes[..end]).map_err(|e| e.to_string())?;
    let length = header_value(headers, "content-length")
        .ok_or("missing Content-Length")?
        .parse::<usize>()
        .map_err(|e| e.to_string())?;
    while bytes.len() < end + length {
        let n = stream.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("peer closed before body".into());
        }
        bytes.extend_from_slice(&buffer[..n]);
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}
fn header_value<'a>(request: &'a str, name: &str) -> Option<&'a str> {
    request.lines().skip(1).find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.eq_ignore_ascii_case(name).then_some(value.trim())
    })
}
fn assert_sync_collection(request: &str, path: &str, token: &str) {
    assert!(request.starts_with(&format!("REPORT {path} HTTP/1.1\r\n")));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert!(body.contains("sync-collection"));
    assert!(body.contains(&format!("sync-token>{token}<")));
    assert!(!body.contains("calendar-query"));
}
fn assert_calendar_query(request: &str, path: &str) {
    assert!(request.starts_with(&format!("REPORT {path} HTTP/1.1\r\n")));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert!(body.contains("calendar-query"));
    assert!(!body.contains("sync-collection"));
}
fn response(status: u16, reason: &str, body: &str) -> String {
    format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}
fn sync_multistatus(token: &str, summary: Option<&str>) -> String {
    response(207, "Multi-Status", &multistatus(Some(token), summary))
}
fn full_multistatus(summary: Option<&str>) -> String {
    response(207, "Multi-Status", &multistatus(None, summary))
}
fn multistatus(token: Option<&str>, summary: Option<&str>) -> String {
    let token = token.map_or(String::new(), |t| {
        format!("<d:sync-token>{t}</d:sync-token>")
    });
    let resource = summary.map_or(String::new(), |summary| {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:changed@example.test\r\nSUMMARY:{summary}\r\nDTSTART;VALUE=DATE:20260810\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        format!(
            r#"<d:response><d:href>changed.ics</d:href><d:propstat><d:prop><d:getetag>"new"</d:getetag><c:calendar-data><![CDATA[{ics}]]></c:calendar-data></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response>"#
        )
    });
    format!(
        r#"<d:multistatus xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav">{token}{resource}</d:multistatus>"#
    )
}
