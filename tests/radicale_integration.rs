use std::collections::VecDeque;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use calendar::backend::caldav::{CaldavClient, map_icalendar_event, serialize_icalendar_event};
use calendar::backend::sync::{pull_calendar_snapshot, push_pending_operations};
use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, Event, EventSchedule,
    PendingSyncOperation,
};
use chrono::NaiveDate;
use reqwest::Method;
use uuid::Uuid;

const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STARTUP_OUTPUT: usize = 16 * 1024;

#[test]
#[ignore = "requires Radicale 3.7.6 on PATH"]
fn phase11_discovers_calendar_created_on_radicale() {
    let fixture = RadicaleFixture::start();
    let work_url = format!("{}/ada/work/", fixture.origin());

    let response = reqwest::blocking::Client::new()
        .request(
            Method::from_bytes(b"MKCALENDAR").expect("MKCALENDAR is a valid HTTP method"),
            &work_url,
        )
        .basic_auth("ada", Some("testpass"))
        .header("Content-Type", "application/xml; charset=utf-8")
        .body(MKCALENDAR_BODY)
        .send()
        .expect("MKCALENDAR request to Radicale must succeed");
    assert_eq!(response.status().as_u16(), 201, "Radicale must create Work");

    let discovery = CaldavClient::new(fixture.origin(), "ada".into(), "testpass".into())
        .discover()
        .expect("CaldavClient must discover Radicale calendars");

    assert_eq!(
        discovery.principal_url,
        format!("{}/ada/", fixture.origin())
    );
    assert_eq!(
        discovery.calendar_home_url,
        format!("{}/ada/", fixture.origin())
    );
    let work = discovery
        .calendars
        .iter()
        .find(|calendar| calendar.href == work_url)
        .expect("discovery must include the created Work calendar");
    assert_eq!(work.display_name.as_deref(), Some("Work"));
    assert_eq!(work.color.as_deref(), Some("#336699FF"));
    assert!(
        work.sync_token
            .as_deref()
            .is_some_and(|token| !token.is_empty()),
        "Work must have a nonempty sync token"
    );
    assert!(
        work.writable,
        "owner_only must grant Ada write access to Work"
    );

    let resource_url = format!("{work_url}standup.ics");
    let response = reqwest::blocking::Client::new()
        .put(&resource_url)
        .basic_auth("ada", Some("testpass"))
        .header("Content-Type", "text/calendar; charset=utf-8")
        .header("If-None-Match", "*")
        .body(STANDUP_ICALENDAR)
        .send()
        .expect("PUT event request to Radicale must succeed");
    assert!(
        matches!(response.status().as_u16(), 201 | 204),
        "Radicale must create the event resource, got {}",
        response.status()
    );

    let account = Account {
        id: Uuid::parse_str("ac110031-0000-0000-0000-000000000001").unwrap(),
        name: "Ada's CalDAV".into(),
        server_url: fixture.origin(),
        username: "ada".into(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("ca110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".into(),
        color: "#336699".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let mut repository = SqliteRepository::open(fixture.db_path())
        .expect("open SQLite database in fixture directory");
    repository.save_account(&account).unwrap();
    repository.save_calendar(&calendar).unwrap();
    repository
        .upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: work_url.clone(),
            sync_token: None,
        })
        .unwrap();

    let client = CaldavClient::new(fixture.origin(), "ada".into(), "testpass".into());
    let imported = pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("pulling Radicale's complete snapshot must import the event");
    assert_eq!(imported.added, 1);
    let state = repository
        .find_event_sync_state_by_remote_href(calendar.id, &resource_url)
        .expect("imported event must have state keyed by its absolute resource URL");
    let event = repository
        .get_event(state.event_id)
        .expect("imported sync state must reference an event");
    assert_eq!(event.title, "Daily standup");
    assert_eq!(event.location, "Room 101");
    assert_eq!(event.description, "Team status update");
    assert_eq!(
        event.schedule,
        EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 8, 10).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
        }
    );
    assert_eq!(state.remote_uid, "standup-2026@example.test");
    assert!(
        state.etag.as_deref().is_some_and(|etag| !etag.is_empty()),
        "Radicale must provide an ETag for the imported resource"
    );

    let mut delete = reqwest::blocking::Client::new()
        .delete(&resource_url)
        .basic_auth("ada", Some("testpass"));
    if let Some(etag) = &state.etag {
        delete = delete.header("If-Match", etag);
    }
    let response = delete
        .send()
        .expect("DELETE event request to Radicale must succeed");
    assert!(
        matches!(response.status().as_u16(), 200 | 202 | 204),
        "Radicale must delete the event resource, got {}",
        response.status()
    );

    let emptied = pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("pulling Radicale's empty complete snapshot must reconcile deletion");
    assert_eq!(emptied.deleted, 1);
    assert!(repository.get_event(state.event_id).is_none());
    assert!(repository.get_event_sync_state(state.event_id).is_none());

    let event_id = Uuid::parse_str("e1100310-0000-0000-0000-000000000001").unwrap();
    let remote_uid = "pending-lifecycle-2026@example.test";
    let mut local_event = Event {
        id: event_id,
        calendar_id: calendar.id,
        title: "Pending upload".into(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 8, 12).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 8, 13).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    repository.save_event(&local_event).unwrap();
    repository
        .upsert_pending_sync_operation(&PendingSyncOperation::Create {
            calendar_id: calendar.id,
            event_id,
            remote_uid: remote_uid.into(),
        })
        .unwrap();

    let created = push_pending_operations(&client, &mut repository, calendar.id)
        .expect("creating the pending event on Radicale must succeed");
    assert_eq!(created.created, 1);
    assert!(repository.get_pending_sync_operation(event_id).is_none());
    let resource_url = format!("{work_url}{event_id}.ics");
    let created_state = repository
        .get_event_sync_state(event_id)
        .expect("a created event must receive sync state");
    assert_eq!(created_state.remote_href, resource_url);
    assert_eq!(created_state.remote_uid, remote_uid);

    pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("pulling after create must obtain Radicale's current ETag");
    let created_state = repository
        .get_event_sync_state(event_id)
        .expect("the created event must remain synchronized after pull");
    let created_resource = client
        .fetch_resources(&work_url)
        .expect("Radicale resource query after create must succeed")
        .into_iter()
        .find(|resource| resource.href == resource_url)
        .expect("created event resource must be returned by Radicale");
    let created_remote = map_icalendar_event(
        created_resource
            .calendar_data
            .as_deref()
            .expect("created resource must include calendar data"),
        event_id,
        calendar.id,
    )
    .expect("created resource must map to a supported event");
    assert_eq!(created_remote.remote_uid, remote_uid);
    assert_eq!(created_remote.event.title, "Pending upload");
    assert_eq!(created_state.etag, created_resource.etag);

    local_event.title = "Pending upload updated".into();
    repository.update_event(&local_event).unwrap();
    let update_operation = PendingSyncOperation::Update {
        calendar_id: calendar.id,
        event_id,
        remote_href: resource_url.clone(),
        remote_uid: remote_uid.into(),
        base_etag: created_state.etag.clone(),
    };
    repository
        .upsert_pending_sync_operation(&update_operation)
        .unwrap();
    let updated = push_pending_operations(&client, &mut repository, calendar.id)
        .expect("updating the pending event on Radicale must succeed");
    assert_eq!(updated.updated, 1);
    assert!(repository.get_pending_sync_operation(event_id).is_none());

    pull_calendar_snapshot(&client, &mut repository, calendar.id)
        .expect("pulling after update must obtain Radicale's current ETag");
    let updated_state = repository
        .get_event_sync_state(event_id)
        .expect("updated event must retain sync state");
    let updated_resource = client
        .fetch_resources(&work_url)
        .expect("Radicale resource query after update must succeed")
        .into_iter()
        .find(|resource| resource.href == resource_url)
        .expect("updated event resource must be returned by Radicale");
    let updated_remote = map_icalendar_event(
        updated_resource
            .calendar_data
            .as_deref()
            .expect("updated resource must include calendar data"),
        event_id,
        calendar.id,
    )
    .expect("updated resource must map to a supported event");
    assert_eq!(updated_remote.event.title, "Pending upload updated");
    assert_eq!(updated_state.etag, updated_resource.etag);

    let stale_etag = updated_state
        .etag
        .clone()
        .expect("Radicale must provide an ETag before a conditional update");
    let server_event = Event {
        title: "Server-side edit".into(),
        ..local_event.clone()
    };
    let server_body = serialize_icalendar_event(&server_event, remote_uid)
        .expect("server edit must use supported iCalendar data");
    client
        .update_resource(&resource_url, &server_body, &stale_etag)
        .expect("conditional server-side edit must succeed");
    let server_resource = client
        .fetch_resources(&work_url)
        .expect("Radicale resource query after concurrent edit must succeed")
        .into_iter()
        .find(|resource| resource.href == resource_url)
        .expect("concurrently edited resource must be returned by Radicale");
    let actual_remote_etag = server_resource
        .etag
        .clone()
        .expect("Radicale must provide the new ETag after a concurrent edit");

    local_event.title = "Local conflicting edit".into();
    repository.update_event(&local_event).unwrap();
    let conflict_operation = PendingSyncOperation::Update {
        calendar_id: calendar.id,
        event_id,
        remote_href: resource_url.clone(),
        remote_uid: remote_uid.into(),
        base_etag: Some(stale_etag),
    };
    repository
        .upsert_pending_sync_operation(&conflict_operation)
        .unwrap();
    let conflicted = push_pending_operations(&client, &mut repository, calendar.id)
        .expect("a precondition conflict must not abort the pending push");
    assert_eq!(conflicted.conflicts, 1);
    assert_eq!(
        repository.get_pending_sync_operation(event_id),
        Some(conflict_operation)
    );
    assert_eq!(repository.get_event(event_id), Some(local_event.clone()));
    assert_eq!(
        repository.get_event_sync_state(event_id),
        Some(updated_state)
    );

    let delete_operation = PendingSyncOperation::Delete {
        calendar_id: calendar.id,
        event_id,
        remote_href: resource_url.clone(),
        remote_uid: remote_uid.into(),
        base_etag: Some(actual_remote_etag),
    };
    repository
        .upsert_pending_sync_operation(&delete_operation)
        .unwrap();
    assert!(repository.delete_event(event_id));
    assert!(repository.get_event_sync_state(event_id).is_none());
    assert_eq!(
        repository.get_pending_sync_operation(event_id),
        Some(delete_operation)
    );
    let deleted = push_pending_operations(&client, &mut repository, calendar.id)
        .expect("deleting the tombstone on Radicale must succeed");
    assert_eq!(deleted.deleted, 1);
    assert!(repository.get_pending_sync_operation(event_id).is_none());
    assert!(
        client
            .fetch_resources(&work_url)
            .expect("Radicale resource query after delete must succeed")
            .into_iter()
            .all(|resource| resource.href != resource_url),
        "deleted resource must no longer be returned by Radicale"
    );
}

const MKCALENDAR_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<c:mkcalendar xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/">
  <d:set><d:prop><d:displayname>Work</d:displayname><a:calendar-color>#336699FF</a:calendar-color></d:prop></d:set>
</c:mkcalendar>"#;

const STANDUP_ICALENDAR: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Calendar Integration Test//EN\r\nBEGIN:VEVENT\r\nUID:standup-2026@example.test\r\nSUMMARY:Daily standup\r\nLOCATION:Room 101\r\nDESCRIPTION:Team status update\r\nDTSTART;VALUE=DATE:20260810\r\nDTEND;VALUE=DATE:20260811\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

struct RadicaleFixture {
    _temp_dir: TempDir,
    child: Child,
    readers: Vec<JoinHandle<()>>,
    output: VecDeque<String>,
    output_receiver: Receiver<String>,
    origin: String,
}

impl RadicaleFixture {
    fn start() -> Self {
        let temp_dir = TempDir::new();
        let config = temp_dir.path().join("radicale.conf");
        let htpasswd = temp_dir.path().join("htpasswd");
        let storage = temp_dir.path().join("storage");
        fs::create_dir(&storage).expect("create isolated Radicale storage directory");
        fs::write(&htpasswd, "ada:testpass\n").expect("write Radicale test credentials");
        fs::write(&config, radicale_config(&htpasswd, &storage)).expect("write Radicale config");

        let mut child = Command::new("radicale")
            .arg("--config")
            .arg(&config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start radicale from PATH");
        let (output_sender, output_receiver) = mpsc::sync_channel(128);
        let readers = vec![
            spawn_reader(
                child.stdout.take().expect("capture Radicale stdout"),
                output_sender.clone(),
            ),
            spawn_reader(
                child.stderr.take().expect("capture Radicale stderr"),
                output_sender,
            ),
        ];

        let mut fixture = Self {
            _temp_dir: temp_dir,
            child,
            readers,
            output: VecDeque::new(),
            output_receiver,
            origin: String::new(),
        };
        fixture.origin = fixture.wait_for_origin();
        fixture
    }

    fn origin(&self) -> String {
        self.origin.clone()
    }

    fn db_path(&self) -> PathBuf {
        self._temp_dir.path().join("calendar.sqlite")
    }

    fn wait_for_origin(&mut self) -> String {
        let deadline = Instant::now() + STARTUP_TIMEOUT;
        loop {
            if let Some(origin) = self
                .output
                .iter()
                .find_map(|line| parse_loopback_origin(line))
            {
                return origin;
            }
            if let Some(status) = self
                .child
                .try_wait()
                .expect("check Radicale startup status")
            {
                panic!("Radicale exited before reporting its loopback address ({status})");
            }
            let now = Instant::now();
            if now >= deadline {
                panic!("timed out waiting for Radicale to report its loopback address");
            }
            if let Ok(line) = self.output_receiver.recv_timeout(deadline - now) {
                self.record_output(line);
            }
        }
    }

    fn record_output(&mut self, line: String) {
        let retained = self.output.iter().map(String::len).sum::<usize>();
        if retained + line.len() <= MAX_STARTUP_OUTPUT {
            self.output.push_back(line);
        }
    }
}

impl Drop for RadicaleFixture {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        for reader in self.readers.drain(..) {
            let _ = reader.join();
        }
    }
}

fn spawn_reader(
    reader: impl std::io::Read + Send + 'static,
    sender: SyncSender<String>,
) -> JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(reader).lines().map_while(Result::ok) {
            match sender.try_send(line) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => break,
            }
        }
    })
}

fn parse_loopback_origin(line: &str) -> Option<String> {
    if !line.contains("Listening") {
        return None;
    }
    let address = line.split("127.0.0.1:").nth(1)?;
    let port = address
        .chars()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    (!port.is_empty()).then_some(format!("http://127.0.0.1:{port}"))
}

fn radicale_config(htpasswd: &Path, storage: &Path) -> String {
    format!(
        "[server]\nhosts = 127.0.0.1:0\n\n[auth]\ntype = htpasswd\nhtpasswd_filename = {}\nhtpasswd_encryption = plain\n\n[rights]\ntype = owner_only\n\n[storage]\ntype = multifilesystem\nfilesystem_folder = {}\n\n[logging]\nlevel = info\n",
        htpasswd.display(),
        storage.display(),
    )
}

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT_ID: AtomicU64 = AtomicU64::new(0);
        let unique = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must be after the Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "calendar-radicale-integration-{}-{timestamp}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("create unique isolated temporary directory");
        Self(fs::canonicalize(path).expect("temporary directory must have an absolute path"))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
