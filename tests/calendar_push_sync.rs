// Public contract pinned by this acceptance test:
//
//     pub mod backend::sync {
//         pub struct PendingPushSummary {
//             pub created: usize, pub updated: usize, pub deleted: usize,
//             pub conflicts: usize, pub skipped: usize,
//         }
//         pub enum PushSyncError {
//             Caldav(caldav::CaldavError), MissingCalendarSyncState,
//             Repository(RepositoryError),
//         }
//         pub fn push_pending_operations(
//             client: &caldav::CaldavClient, repository: &mut SqliteRepository,
//             calendar_id: Uuid,
//         ) -> Result<PendingPushSummary, PushSyncError>;
//     }
//
// This is a blocking upload boundary and must be called off GTK's main thread.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, CaldavError, map_icalendar_event};
use calendar::backend::sync::{PushSyncError, push_pending_operations};
use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, Event, EventSchedule, EventSyncState,
    PendingSyncOperation, RecurrenceSpec,
};
use chrono::NaiveDate;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn phase11_pushes_durable_operations_and_keeps_conflicts_and_skips_pending() {
    let db_path = unique_temp_db_path("push_sync");
    let _cleanup = TempDb(db_path.clone());
    let fixture = Fixture::start(vec![
        response(201, "Created", Some("\"create-v1\"")),
        response(204, "No Content", Some("\"update-v2\"")),
        response(404, "Not Found", None),
        response(412, "Precondition Failed", None),
        response(412, "Precondition Failed", None),
        response(412, "Precondition Failed", None),
        response(201, "Created", Some("\"later-v1\"")),
    ]);
    let account = account();
    let calendar = remote_calendar(account.id);
    let calendar_url = format!("{}/calendars/ada/work/", fixture.origin());
    let mut repository = SqliteRepository::open(&db_path).unwrap();
    repository.save_account(&account).unwrap();
    repository.save_calendar(&calendar).unwrap();
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: calendar_url.clone(),
            sync_token: None,
        })
        .unwrap();

    let create = event(
        "e1100311-0000-0000-0000-000000000001",
        calendar.id,
        "Created",
    );
    let update = event(
        "e1100312-0000-0000-0000-000000000002",
        calendar.id,
        "Updated",
    );
    let conflict_create = event(
        "e1100314-0000-0000-0000-000000000004",
        calendar.id,
        "Create conflict",
    );
    let conflict_update = event(
        "e1100315-0000-0000-0000-000000000005",
        calendar.id,
        "Update conflict",
    );
    let missing_event = Uuid::parse_str("e1100317-0000-0000-0000-000000000007").unwrap();
    let no_etag_update = event(
        "e1100318-0000-0000-0000-000000000008",
        calendar.id,
        "No update ETag",
    );
    let unsupported = Event {
        recurrence: Some(RecurrenceSpec::default()),
        ..event(
            "e1100320-0000-0000-0000-000000000010",
            calendar.id,
            "Unsupported",
        )
    };
    let later = event("e1100321-0000-0000-0000-000000000011", calendar.id, "Later");
    for local in [
        &create,
        &update,
        &conflict_create,
        &conflict_update,
        &no_etag_update,
        &unsupported,
        &later,
    ] {
        repository.save_event(local).unwrap();
    }
    let update_href = format!("{}/calendars/ada/work/updated.ics", fixture.origin());
    let conflict_update_href = format!(
        "{}/calendars/ada/work/conflict-update.ics",
        fixture.origin()
    );
    repository
        .upsert_event_sync_state(&EventSyncState {
            calendar_id: calendar.id,
            event_id: update.id,
            remote_href: update_href.clone(),
            remote_uid: "updated@example.test".into(),
            etag: Some("\"update-v1\"".into()),
        })
        .unwrap();
    let conflict_update_state = EventSyncState {
        calendar_id: calendar.id,
        event_id: conflict_update.id,
        remote_href: conflict_update_href.clone(),
        remote_uid: "conflict-update@example.test".into(),
        etag: Some("\"conflict-v1\"".into()),
    };
    repository
        .upsert_event_sync_state(&conflict_update_state)
        .unwrap();

    let operations = vec![
        PendingSyncOperation::Create {
            calendar_id: calendar.id,
            event_id: create.id,
            remote_uid: "created@example.test".into(),
        },
        PendingSyncOperation::Update {
            calendar_id: calendar.id,
            event_id: update.id,
            remote_href: update_href.clone(),
            remote_uid: "updated@example.test".into(),
            base_etag: Some("\"update-v1\"".into()),
        },
        PendingSyncOperation::Delete {
            calendar_id: calendar.id,
            event_id: Uuid::parse_str("e1100313-0000-0000-0000-000000000003").unwrap(),
            remote_href: format!("{}/calendars/ada/work/gone.ics", fixture.origin()),
            remote_uid: "gone@example.test".into(),
            base_etag: Some("\"gone-v1\"".into()),
        },
        PendingSyncOperation::Create {
            calendar_id: calendar.id,
            event_id: conflict_create.id,
            remote_uid: "conflict-create@example.test".into(),
        },
        PendingSyncOperation::Update {
            calendar_id: calendar.id,
            event_id: conflict_update.id,
            remote_href: conflict_update_href,
            remote_uid: "conflict-update@example.test".into(),
            base_etag: Some("\"conflict-v1\"".into()),
        },
        PendingSyncOperation::Delete {
            calendar_id: calendar.id,
            event_id: Uuid::parse_str("e1100316-0000-0000-0000-000000000006").unwrap(),
            remote_href: format!(
                "{}/calendars/ada/work/conflict-delete.ics",
                fixture.origin()
            ),
            remote_uid: "conflict-delete@example.test".into(),
            base_etag: Some("\"delete-v1\"".into()),
        },
        PendingSyncOperation::Create {
            calendar_id: calendar.id,
            event_id: missing_event,
            remote_uid: "missing@example.test".into(),
        },
        PendingSyncOperation::Update {
            calendar_id: calendar.id,
            event_id: no_etag_update.id,
            remote_href: format!("{}/calendars/ada/work/no-etag.ics", fixture.origin()),
            remote_uid: "no-etag@example.test".into(),
            base_etag: None,
        },
        PendingSyncOperation::Delete {
            calendar_id: calendar.id,
            event_id: Uuid::parse_str("e1100319-0000-0000-0000-000000000009").unwrap(),
            remote_href: format!("{}/calendars/ada/work/no-delete-etag.ics", fixture.origin()),
            remote_uid: "no-delete-etag@example.test".into(),
            base_etag: None,
        },
        PendingSyncOperation::Create {
            calendar_id: calendar.id,
            event_id: unsupported.id,
            remote_uid: "unsupported@example.test".into(),
        },
        PendingSyncOperation::Create {
            calendar_id: calendar.id,
            event_id: later.id,
            remote_uid: "later@example.test".into(),
        },
    ];
    for operation in &operations {
        repository.upsert_pending_sync_operation(operation).unwrap();
    }

    let summary = push_pending_operations(
        &CaldavClient::new(fixture.origin(), "ada".into(), "secret".into()),
        &mut repository,
        calendar.id,
    )
    .unwrap();
    assert_eq!(
        (
            summary.created,
            summary.updated,
            summary.deleted,
            summary.conflicts,
            summary.skipped
        ),
        (2, 1, 1, 3, 4)
    );
    assert_eq!(
        repository.get_event_sync_state(create.id),
        Some(EventSyncState {
            calendar_id: calendar.id,
            event_id: create.id,
            remote_href: format!("{calendar_url}{}.ics", create.id),
            remote_uid: "created@example.test".into(),
            etag: Some("\"create-v1\"".into())
        })
    );
    assert_eq!(
        repository.get_event_sync_state(update.id),
        Some(EventSyncState {
            calendar_id: calendar.id,
            event_id: update.id,
            remote_href: update_href,
            remote_uid: "updated@example.test".into(),
            etag: Some("\"update-v2\"".into())
        })
    );
    for completed in [create.id, update.id, operations[2].event_id(), later.id] {
        assert!(repository.get_pending_sync_operation(completed).is_none());
    }
    for operation in &operations[3..10] {
        assert_eq!(
            repository.get_pending_sync_operation(operation.event_id()),
            Some(operation.clone())
        );
    }
    assert_eq!(
        repository.get_event(conflict_create.id),
        Some(conflict_create.clone())
    );
    assert_eq!(
        repository.get_event_sync_state(conflict_update.id),
        Some(conflict_update_state)
    );

    let requests = fixture.finish();
    assert_eq!(requests.len(), 7);
    assert_put(
        &requests[0],
        &format!("/calendars/ada/work/{}.ics", create.id),
        "if-none-match",
        "*",
        (create.id, calendar.id),
        "created@example.test",
        "Created",
    );
    assert_put(
        &requests[1],
        "/calendars/ada/work/updated.ics",
        "if-match",
        "\"update-v1\"",
        (update.id, calendar.id),
        "updated@example.test",
        "Updated",
    );
    assert_delete(&requests[2], "/calendars/ada/work/gone.ics", "\"gone-v1\"");
    assert_put(
        &requests[3],
        &format!("/calendars/ada/work/{}.ics", conflict_create.id),
        "if-none-match",
        "*",
        (conflict_create.id, calendar.id),
        "conflict-create@example.test",
        "Create conflict",
    );
    assert_put(
        &requests[4],
        "/calendars/ada/work/conflict-update.ics",
        "if-match",
        "\"conflict-v1\"",
        (conflict_update.id, calendar.id),
        "conflict-update@example.test",
        "Update conflict",
    );
    assert_delete(
        &requests[5],
        "/calendars/ada/work/conflict-delete.ics",
        "\"delete-v1\"",
    );
    assert_put(
        &requests[6],
        &format!("/calendars/ada/work/{}.ics", later.id),
        "if-none-match",
        "*",
        (later.id, calendar.id),
        "later@example.test",
        "Later",
    );

    let failure_calendar = Calendar {
        id: Uuid::parse_str("ca110032-0000-0000-0000-000000000002").unwrap(),
        ..remote_calendar(account.id)
    };
    let failure_event = event(
        "e1100322-0000-0000-0000-000000000012",
        failure_calendar.id,
        "Failure",
    );
    repository.save_calendar(&failure_calendar).unwrap();
    repository.save_event(&failure_event).unwrap();
    let unavailable = Fixture::start(vec![response(503, "Service Unavailable", None)]);
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: failure_calendar.id,
            remote_url: format!("{}/failure/", unavailable.origin()),
            sync_token: None,
        })
        .unwrap();
    let failure_operation = PendingSyncOperation::Create {
        calendar_id: failure_calendar.id,
        event_id: failure_event.id,
        remote_uid: "failure@example.test".into(),
    };
    repository
        .upsert_pending_sync_operation(&failure_operation)
        .unwrap();
    assert!(matches!(
        push_pending_operations(
            &CaldavClient::new(unavailable.origin(), "ada".into(), "secret".into()),
            &mut repository,
            failure_calendar.id
        ),
        Err(PushSyncError::Caldav(CaldavError::HttpStatus {
            status: 503
        }))
    ));
    assert_eq!(
        repository.get_pending_sync_operation(failure_event.id),
        Some(failure_operation)
    );
    assert_eq!(unavailable.finish().len(), 1);

    let missing = Calendar {
        id: Uuid::parse_str("ca110033-0000-0000-0000-000000000003").unwrap(),
        ..remote_calendar(account.id)
    };
    repository.save_calendar(&missing).unwrap();
    let quiet = Fixture::quiet();
    assert!(matches!(
        push_pending_operations(
            &CaldavClient::new(quiet.origin(), "ada".into(), "secret".into()),
            &mut repository,
            missing.id
        ),
        Err(PushSyncError::MissingCalendarSyncState)
    ));
    assert!(quiet.finish().is_empty());
}

trait OperationId {
    fn event_id(&self) -> Uuid;
}
impl OperationId for PendingSyncOperation {
    fn event_id(&self) -> Uuid {
        match self {
            Self::Create { event_id, .. }
            | Self::Update { event_id, .. }
            | Self::Delete { event_id, .. } => *event_id,
        }
    }
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
    path.push(format!(
        "calendar_phase11_{label}_{}_{}.sqlite",
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
        Self {
            origin,
            server: thread::spawn(move || server_fn(&listener)),
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
        .unwrap_or("0")
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
fn response(status: u16, reason: &str, etag: Option<&str>) -> String {
    let etag = etag.map_or_else(String::new, |value| format!("ETag: {value}\r\n"));
    format!("HTTP/1.1 {status} {reason}\r\n{etag}Content-Length: 0\r\nConnection: close\r\n\r\n")
}
fn assert_put(
    request: &str,
    path: &str,
    precondition: &str,
    value: &str,
    ids: (Uuid, Uuid),
    uid: &str,
    title: &str,
) {
    assert!(request.starts_with(&format!("PUT {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, precondition), Some(value));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    let mapped = map_icalendar_event(body, ids.0, ids.1).unwrap();
    assert_eq!(mapped.remote_uid, uid);
    assert_eq!(mapped.event.title, title);
}
fn assert_delete(request: &str, path: &str, etag: &str) {
    assert!(request.starts_with(&format!("DELETE {path} HTTP/1.1\r\n")));
    assert_eq!(header_value(request, "if-match"), Some(etag));
}
