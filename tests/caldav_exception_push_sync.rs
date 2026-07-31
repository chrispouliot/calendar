use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use calendar::backend::caldav::{CaldavClient, map_icalendar_resource};
use calendar::backend::sync::push_pending_operations;
use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, DetachedEvent, Event, EventSchedule,
    EventSyncState, PendingSyncOperation, RecurrenceId, RecurrenceSpec,
};
use chrono::NaiveDate;
use uuid::Uuid;

const TIMEOUT: Duration = Duration::from_secs(2);

#[test]
fn phase2a_pushes_a_recurring_resource_with_all_detached_exceptions() {
    let db_path = unique_temp_db_path("exception_push");
    let _cleanup = TempDb(db_path.clone());
    let fixture = Fixture::start(response(204, "No Content", Some("\"updated-v2\"")));
    let account = Account {
        id: Uuid::parse_str("2a100001-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        server_url: fixture.origin(),
        username: "ada".into(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("2a100002-0000-0000-0000-000000000002").unwrap(),
        name: "Work".into(),
        color: "#3366cc".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let master = Event {
        id: Uuid::parse_str("2a100003-0000-0000-0000-000000000003").unwrap(),
        calendar_id: calendar.id,
        title: "Standup".into(),
        location: "Room 1".into(),
        description: "Daily sync".into(),
        schedule: all_day(10),
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;COUNT=3".into()],
            ..Default::default()
        }),
        reminders: Vec::new(),
    };
    let modified = DetachedEvent::Modified {
        recurrence_id: RecurrenceId::AllDay(date(17)),
        title: "Standup moved".into(),
        location: "Zoom".into(),
        description: "Release discussion".into(),
        schedule: all_day(18),
        reminders: Vec::new(),
    };
    let cancelled = DetachedEvent::Cancelled {
        recurrence_id: RecurrenceId::AllDay(date(24)),
    };
    let remote_href = format!("{}/calendars/ada/work/standup.ics", fixture.origin());
    let initial_state = EventSyncState {
        calendar_id: calendar.id,
        event_id: master.id,
        remote_href: remote_href.clone(),
        remote_uid: "standup@example.test".into(),
        etag: Some("\"updated-v1\"".into()),
    };

    let mut repository = SqliteRepository::open(&db_path).unwrap();
    repository.save_account(&account).unwrap();
    repository.save_calendar(&calendar).unwrap();
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: format!("{}/calendars/ada/work/", fixture.origin()),
            sync_token: None,
        })
        .unwrap();
    repository.save_event(&master).unwrap();
    repository.upsert_event_sync_state(&initial_state).unwrap();
    repository
        .replace_detached_events(master.id, &[modified.clone(), cancelled.clone()])
        .unwrap();
    let mut edited_master = master.clone();
    edited_master.title = "Standup revised".into();
    repository.update_event_with_sync(&edited_master).unwrap();
    assert!(matches!(
        repository.get_pending_sync_operation(master.id),
        Some(PendingSyncOperation::Update { .. })
    ));

    push_pending_operations(
        &CaldavClient::new(fixture.origin(), "ada".into(), "secret".into()),
        &mut repository,
        calendar.id,
    )
    .unwrap();

    assert_eq!(
        repository.get_event_sync_state(master.id),
        Some(EventSyncState {
            etag: Some("\"updated-v2\"".into()),
            ..initial_state.clone()
        })
    );
    assert!(repository.get_pending_sync_operation(master.id).is_none());
    assert_eq!(
        repository.list_detached_events(master.id),
        vec![modified.clone(), cancelled.clone()],
        "uploading the master must not alter stored exceptions"
    );

    let request = fixture.finish();
    assert!(request.starts_with("PUT /calendars/ada/work/standup.ics HTTP/1.1\r\n"));
    assert_eq!(header_value(&request, "if-match"), Some("\"updated-v1\""));
    let body = request.split_once("\r\n\r\n").unwrap().1;
    assert_eq!(body.matches("BEGIN:VCALENDAR").count(), 1);
    assert_eq!(body.matches("BEGIN:VEVENT").count(), 3);
    assert_eq!(body.matches("UID:standup@example.test").count(), 3);
    assert_eq!(body.matches("RECURRENCE-ID;VALUE=DATE:").count(), 2);
    assert!(body.contains("STATUS:CANCELLED"));
    let mapped = map_icalendar_resource(body, master.id, calendar.id).unwrap();
    assert_eq!(mapped.master.remote_uid, "standup@example.test");
    assert_eq!(mapped.master.event, edited_master);
    assert_eq!(mapped.exceptions, vec![modified, cancelled]);
}

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 8, day).unwrap()
}

fn all_day(day: u32) -> EventSchedule {
    EventSchedule::AllDay {
        start_date: date(day),
        end_date_exclusive: date(day + 1),
    }
}

fn unique_temp_db_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "calendar_phase2a_{label}_{}_{}.sqlite",
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
    server: JoinHandle<Result<String, String>>,
}

impl Fixture {
    fn start(response: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = accept_before(&listener, Instant::now() + TIMEOUT)?;
            let request = read_request(&mut stream)?;
            stream
                .write_all(response.as_bytes())
                .map_err(|error| error.to_string())?;
            Ok(request)
        });
        Self { origin, server }
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
                thread::sleep(Duration::from_millis(5));
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
        .unwrap_or("0")
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
