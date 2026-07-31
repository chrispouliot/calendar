// A single CalDAV resource is one recurring master plus its complete detached
// instance set. Reconciliation must persist that resource as one unit.

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository, caldav::ResourceRecord,
};
use calendar::model::{
    Account, Calendar, CalendarSource, DetachedEvent, EventSchedule, PendingSyncOperation,
    RecurrenceId,
};
use chrono::NaiveDate;
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.push(format!("calendar_phase2a_{label}_{pid}_{nanos}.sqlite"));
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

fn resource(href: &str, etag: &str, calendar_data: String) -> ResourceRecord {
    ResourceRecord {
        href: href.to_owned(),
        response_status: Some(200),
        etag: Some(etag.to_owned()),
        calendar_data: Some(calendar_data),
    }
}

fn recurring_resource(summary: &str, exceptions: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:weekly-team@example.test\r\nSUMMARY:{summary}\r\nDTSTART;VALUE=DATE:20260701\r\nRRULE:FREQ=WEEKLY;COUNT=6\r\nEND:VEVENT\r\n{exceptions}END:VCALENDAR\r\n"
    )
}

fn modified(day: &str, summary: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:weekly-team@example.test\r\nRECURRENCE-ID;VALUE=DATE:{day}\r\nSUMMARY:{summary}\r\nLOCATION:Room B\r\nDESCRIPTION:Remote exception\r\nDTSTART;VALUE=DATE:{day}\r\nEND:VEVENT\r\n"
    )
}

fn cancelled(day: &str) -> String {
    format!(
        "BEGIN:VEVENT\r\nUID:weekly-team@example.test\r\nRECURRENCE-ID;VALUE=DATE:{day}\r\nSTATUS:CANCELLED\r\nDTSTART;VALUE=DATE:{day}\r\nEND:VEVENT\r\n"
    )
}

#[test]
fn phase2a_reconciles_a_remote_recurring_resource_and_its_detached_instances() {
    let db_path = unique_temp_db_path("exception_reconciliation");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::parse_str("2a200001-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_owned(),
        server_url: "https://caldav.example.test/dav/".to_owned(),
        username: "ada".to_owned(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("2a200002-0000-0000-0000-000000000002").unwrap(),
        name: "Work".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let href = "https://caldav.example.test/dav/work/weekly-team.ics";
    let initial = resource(
        href,
        "\"weekly-v1\"",
        recurring_resource(
            "Weekly team",
            &(modified("20260708", "Weekly team moved") + &cancelled("20260715")),
        ),
    );
    let replacement = resource(
        href,
        "\"weekly-v2\"",
        recurring_resource(
            "Weekly team renamed",
            &(modified("20260722", "Weekly team rescheduled") + &cancelled("20260729")),
        ),
    );

    let mut repo = SqliteRepository::open(&db_path).expect("open isolated database");
    repo.save_account(&account).unwrap();
    repo.save_calendar(&calendar).unwrap();

    let summary = repo
        .reconcile_remote_snapshot(calendar.id, std::slice::from_ref(&initial))
        .expect("a recurring resource must reconcile");
    assert_eq!((summary.added, summary.updated, summary.skipped), (1, 0, 0));
    let initial_state = repo
        .find_event_sync_state_by_remote_href(calendar.id, href)
        .expect("the master must receive sync state");
    let master_id = initial_state.event_id;
    assert_eq!(initial_state.remote_uid, "weekly-team@example.test");
    assert_eq!(initial_state.etag.as_deref(), Some("\"weekly-v1\""));
    assert_eq!(repo.get_event(master_id).unwrap().title, "Weekly team");
    let initial_exceptions = repo.list_detached_events(master_id);
    assert_eq!(
        initial_exceptions,
        vec![
            DetachedEvent::Modified {
                recurrence_id: RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 7, 8).unwrap()),
                title: "Weekly team moved".to_owned(),
                location: "Room B".to_owned(),
                description: "Remote exception".to_owned(),
                schedule: EventSchedule::AllDay {
                    start_date: NaiveDate::from_ymd_opt(2026, 7, 8).unwrap(),
                    end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 9).unwrap(),
                },
                reminders: Vec::new(),
            },
            DetachedEvent::Cancelled {
                recurrence_id: RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()),
            },
        ]
    );

    let pending = PendingSyncOperation::Update {
        calendar_id: calendar.id,
        event_id: master_id,
        remote_href: href.to_owned(),
        remote_uid: initial_state.remote_uid.clone(),
        base_etag: initial_state.etag.clone(),
    };
    repo.upsert_pending_sync_operation(&pending).unwrap();
    let protected = repo
        .reconcile_remote_snapshot(calendar.id, std::slice::from_ref(&replacement))
        .unwrap();
    assert_eq!(protected.skipped, 1);
    assert_eq!(repo.get_event(master_id).unwrap().title, "Weekly team");
    assert_eq!(repo.list_detached_events(master_id), initial_exceptions);
    assert!(repo.remove_pending_sync_operation(master_id));

    let summary = repo
        .reconcile_remote_snapshot(calendar.id, std::slice::from_ref(&replacement))
        .unwrap();
    assert_eq!((summary.updated, summary.skipped), (1, 0));
    assert_eq!(
        repo.get_event(master_id).unwrap().title,
        "Weekly team renamed"
    );
    let replacement_exceptions = repo.list_detached_events(master_id);
    assert_eq!(
        replacement_exceptions,
        vec![
            DetachedEvent::Modified {
                recurrence_id: RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 7, 22).unwrap()),
                title: "Weekly team rescheduled".to_owned(),
                location: "Room B".to_owned(),
                description: "Remote exception".to_owned(),
                schedule: EventSchedule::AllDay {
                    start_date: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
                    end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 23).unwrap(),
                },
                reminders: Vec::new(),
            },
            DetachedEvent::Cancelled {
                recurrence_id: RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 7, 29).unwrap()),
            },
        ],
        "a successful body replaces the entire stale detached set"
    );

    let malformed = resource(
        href,
        "\"weekly-bad\"",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:weekly-team@example.test\r\nDTSTART;VALUE=DATE:20260701\r\nDURATION:P1D\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n".to_owned(),
    );
    let skipped = repo
        .reconcile_remote_snapshot(calendar.id, &[malformed])
        .unwrap();
    assert_eq!(skipped.skipped, 1);
    assert_eq!(
        repo.get_event(master_id).unwrap().title,
        "Weekly team renamed"
    );
    assert_eq!(repo.list_detached_events(master_id), replacement_exceptions);

    let deleted = repo
        .reconcile_remote_snapshot(
            calendar.id,
            &[ResourceRecord {
                href: href.to_owned(),
                response_status: Some(404),
                etag: None,
                calendar_data: None,
            }],
        )
        .unwrap();
    assert_eq!(deleted.deleted, 1);
    assert!(repo.get_event(master_id).is_none());
    assert!(repo.get_event_sync_state(master_id).is_none());
    assert!(repo.list_detached_events(master_id).is_empty());
}
