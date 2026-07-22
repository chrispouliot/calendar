// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         pub fn reconcile_remote_snapshot(
//             &mut self,
//             calendar_id: Uuid,
//             resources: &[backend::caldav::ResourceRecord],
//         ) -> Result<RemoteSnapshotSummary, RepositoryError>;
//     }
//
//     pub struct RemoteSnapshotSummary {
//         pub added: usize,
//         pub updated: usize,
//         pub deleted: usize,
//         pub skipped: usize,
//     }
//
// This pull-only boundary receives an already-fetched complete remote snapshot.
// It atomically reconciles Events and separate EventSyncState without HTTP or UI.

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, SqliteRepository, SyncStateRepository,
    caldav::ResourceRecord,
};
use calendar::model::{Account, Calendar, CalendarSource, Event, EventSchedule, EventSyncState};
use chrono::NaiveDate;
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!("calendar_phase11_{label}_{pid}_{nanos}.sqlite"));
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

fn event(id: &str, calendar_id: Uuid, title: &str, day: u32) -> Event {
    Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id,
        title: title.to_string(),
        location: "Old location".to_string(),
        description: "Old description".to_string(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 8, day).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 8, day + 1).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    }
}

fn ics(uid: &str, summary: &str, day: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{summary}\r\nLOCATION:Remote room\r\nDESCRIPTION:Remote notes\r\nDTSTART;VALUE=DATE:{day}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[test]
fn phase11_reconciles_a_complete_remote_snapshot_durably() {
    let db_path = unique_temp_db_path("remote_snapshot");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::parse_str("ac110021-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("ca110021-0000-0000-0000-000000000001").unwrap(),
        name: "Work".to_string(),
        color: "#d946ef".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let existing = event(
        "e1100211-0000-0000-0000-000000000001",
        calendar.id,
        "Old planning",
        1,
    );
    let absent = event(
        "e1100212-0000-0000-0000-000000000002",
        calendar.id,
        "Gone",
        2,
    );
    let not_found = event(
        "e1100213-0000-0000-0000-000000000003",
        calendar.id,
        "Also gone",
        3,
    );
    let unsupported = event(
        "e1100214-0000-0000-0000-000000000004",
        calendar.id,
        "Keep cached",
        4,
    );
    let local_only = event(
        "e1100215-0000-0000-0000-000000000005",
        calendar.id,
        "Local draft",
        5,
    );
    let existing_href = "https://caldav.example.test/dav/work/planning.ics";
    let absent_href = "https://caldav.example.test/dav/work/absent.ics";
    let not_found_href = "https://caldav.example.test/dav/work/not-found.ics";
    let unsupported_href = "https://caldav.example.test/dav/work/recurring.ics";
    let new_href = "https://caldav.example.test/dav/work/new.ics";

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("opening a fresh sqlite database must succeed");
        repo.save_account(&account).unwrap();
        repo.save_calendar(&calendar).unwrap();
        for saved in [&existing, &absent, &not_found, &unsupported, &local_only] {
            repo.save_event(saved).unwrap();
        }
        for state in [
            EventSyncState {
                calendar_id: calendar.id,
                event_id: existing.id,
                remote_href: existing_href.to_string(),
                remote_uid: "old-planning".to_string(),
                etag: Some("\"old-planning\"".to_string()),
            },
            EventSyncState {
                calendar_id: calendar.id,
                event_id: absent.id,
                remote_href: absent_href.to_string(),
                remote_uid: "absent".to_string(),
                etag: Some("\"old-absent\"".to_string()),
            },
            EventSyncState {
                calendar_id: calendar.id,
                event_id: not_found.id,
                remote_href: not_found_href.to_string(),
                remote_uid: "not-found".to_string(),
                etag: Some("\"old-not-found\"".to_string()),
            },
            EventSyncState {
                calendar_id: calendar.id,
                event_id: unsupported.id,
                remote_href: unsupported_href.to_string(),
                remote_uid: "cached-recurring".to_string(),
                etag: Some("\"old-recurring\"".to_string()),
            },
        ] {
            repo.upsert_event_sync_state(&state).unwrap();
        }

        let invalid_calendar = Uuid::parse_str("ca110021-0000-0000-0000-000000000099").unwrap();
        assert!(
            repo.reconcile_remote_snapshot(invalid_calendar, &[])
                .is_err()
        );
        assert_eq!(repo.get_event(existing.id), Some(existing.clone()));
        assert_eq!(repo.list_event_sync_states(calendar.id).len(), 4);

        let resources = vec![
            ResourceRecord {
                href: existing_href.to_string(),
                response_status: Some(200),
                etag: Some("\"planning-v2\"".to_string()),
                calendar_data: Some(ics("planning-remote", "Remote planning", "20260810")),
            },
            ResourceRecord {
                href: new_href.to_string(),
                response_status: Some(200),
                etag: Some("\"new-v1\"".to_string()),
                calendar_data: Some(ics("new-remote", "New remote event", "20260811")),
            },
            ResourceRecord {
                href: not_found_href.to_string(),
                response_status: Some(404),
                etag: None,
                calendar_data: None,
            },
            ResourceRecord {
                href: unsupported_href.to_string(),
                response_status: Some(200),
                etag: Some("\"recurring-v2\"".to_string()),
                calendar_data: Some(
                    ics("recurring-remote", "Do not overwrite", "20260812")
                        .replace("END:VEVENT", "RRULE:FREQ=DAILY\r\nEND:VEVENT"),
                ),
            },
        ];
        let summary = repo
            .reconcile_remote_snapshot(calendar.id, &resources)
            .unwrap();
        assert_eq!(summary.added, 1);
        assert_eq!(summary.updated, 2);
        assert_eq!(summary.deleted, 2);
        assert_eq!(summary.skipped, 0);

        let updated = repo
            .get_event(existing.id)
            .expect("existing remote event must keep its id");
        assert_eq!(updated.id, existing.id);
        assert_eq!(updated.title, "Remote planning");
        assert_eq!(
            repo.get_event_sync_state(existing.id).unwrap().remote_uid,
            "planning-remote"
        );
        assert_eq!(
            repo.get_event_sync_state(existing.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"planning-v2\"")
        );

        let new_state = repo
            .find_event_sync_state_by_remote_href(calendar.id, new_href)
            .expect("new href must receive sync state");
        let new_event = repo
            .get_event(new_state.event_id)
            .expect("new sync state must map to an event");
        assert_eq!(new_event.calendar_id, calendar.id);
        assert_eq!(new_event.title, "New remote event");
        assert_eq!(new_state.remote_uid, "new-remote");
        assert_eq!(new_state.etag.as_deref(), Some("\"new-v1\""));

        assert!(repo.get_event(absent.id).is_none());
        assert!(repo.get_event_sync_state(absent.id).is_none());
        assert!(repo.get_event(not_found.id).is_none());
        assert!(repo.get_event_sync_state(not_found.id).is_none());
        assert_eq!(repo.get_event(local_only.id), Some(local_only.clone()));
        assert_eq!(
            repo.get_event(unsupported.id).unwrap().title,
            "Do not overwrite"
        );
        assert!(repo.get_event(unsupported.id).unwrap().recurrence.is_some());
        assert_eq!(
            repo.get_event_sync_state(unsupported.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"recurring-v2\"")
        );
    }

    {
        let repo =
            SqliteRepository::open(&db_path).expect("reopening after reconciliation must succeed");
        assert_eq!(
            repo.get_event(existing.id).unwrap().title,
            "Remote planning"
        );
        assert_eq!(
            repo.get_event_sync_state(existing.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"planning-v2\"")
        );
        assert_eq!(
            repo.get_event_sync_state(existing.id).unwrap().remote_uid,
            "planning-remote"
        );
        let new_state = repo
            .find_event_sync_state_by_remote_href(calendar.id, new_href)
            .expect("new state must survive reopen");
        assert_eq!(
            repo.get_event(new_state.event_id).unwrap().title,
            "New remote event"
        );
        assert_eq!(new_state.remote_uid, "new-remote");
        assert_eq!(new_state.etag.as_deref(), Some("\"new-v1\""));
        assert!(repo.get_event(absent.id).is_none());
        assert!(repo.get_event_sync_state(absent.id).is_none());
        assert!(repo.get_event(not_found.id).is_none());
        assert!(repo.get_event_sync_state(not_found.id).is_none());
        assert_eq!(repo.get_event(local_only.id), Some(local_only));
        assert_eq!(
            repo.get_event(unsupported.id).unwrap().title,
            "Do not overwrite"
        );
        assert!(repo.get_event(unsupported.id).unwrap().recurrence.is_some());
        assert_eq!(
            repo.get_event_sync_state(unsupported.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"recurring-v2\"")
        );
    }
}
