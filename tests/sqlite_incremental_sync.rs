// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         pub fn reconcile_remote_changes(
//             &mut self,
//             calendar_id: Uuid,
//             changes: &backend::caldav::SyncCollection,
//         ) -> Result<RemoteSnapshotSummary, RepositoryError>;
//     }
//
// This pull-only boundary applies one CalDAV SyncCollection delta atomically:
// explicit deletions remove tracked resources, while omitted resources remain.

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, SqliteRepository, SyncStateRepository,
    caldav::{ResourceRecord, SyncCollection},
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, Event, EventSchedule, EventSyncState,
};
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
fn phase11_reconciles_incremental_remote_changes_atomically_and_advances_token() {
    let db_path = unique_temp_db_path("incremental_sync");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::parse_str("ac110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let remote_calendar = Calendar {
        id: Uuid::parse_str("ca110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work".to_string(),
        color: "#d946ef".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local_calendar = Calendar {
        id: Uuid::parse_str("ca110032-0000-0000-0000-000000000002").unwrap(),
        name: "Local".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let remote_without_state = Calendar {
        id: Uuid::parse_str("ca110033-0000-0000-0000-000000000003").unwrap(),
        name: "Uninitialized CalDAV".to_string(),
        color: "#2563eb".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let update = event(
        "e1100311-0000-0000-0000-000000000001",
        remote_calendar.id,
        "Old planning",
        1,
    );
    let deleted = event(
        "e1100312-0000-0000-0000-000000000002",
        remote_calendar.id,
        "Deleted",
        2,
    );
    let untouched = event(
        "e1100313-0000-0000-0000-000000000003",
        remote_calendar.id,
        "Unchanged",
        3,
    );
    let unsupported = event(
        "e1100314-0000-0000-0000-000000000004",
        remote_calendar.id,
        "Keep cached",
        4,
    );
    let local_only = event(
        "e1100315-0000-0000-0000-000000000005",
        remote_calendar.id,
        "Local draft",
        5,
    );
    let update_href = "https://caldav.example.test/dav/work/planning.ics";
    let deleted_href = "https://caldav.example.test/dav/work/deleted.ics";
    let untouched_href = "https://caldav.example.test/dav/work/untouched.ics";
    let unsupported_href = "https://caldav.example.test/dav/work/recurring.ics";
    let new_href = "https://caldav.example.test/dav/work/new.ics";
    let old_state = CalendarSyncState {
        calendar_id: remote_calendar.id,
        remote_url: "https://caldav.example.test/dav/work/".to_string(),
        sync_token: Some("token-old".to_string()),
    };

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        repo.save_account(&account).unwrap();
        repo.save_calendar(&remote_calendar).unwrap();
        repo.save_calendar(&local_calendar).unwrap();
        repo.save_calendar(&remote_without_state).unwrap();
        repo.upsert_calendar_sync_state(&old_state).unwrap();
        for saved in [&update, &deleted, &untouched, &unsupported, &local_only] {
            repo.save_event(saved).unwrap();
        }
        for state in [
            EventSyncState {
                calendar_id: remote_calendar.id,
                event_id: update.id,
                remote_href: update_href.to_string(),
                remote_uid: "old-planning".to_string(),
                etag: Some("\"old-planning\"".to_string()),
            },
            EventSyncState {
                calendar_id: remote_calendar.id,
                event_id: deleted.id,
                remote_href: deleted_href.to_string(),
                remote_uid: "deleted".to_string(),
                etag: Some("\"old-deleted\"".to_string()),
            },
            EventSyncState {
                calendar_id: remote_calendar.id,
                event_id: untouched.id,
                remote_href: untouched_href.to_string(),
                remote_uid: "untouched".to_string(),
                etag: Some("\"old-untouched\"".to_string()),
            },
            EventSyncState {
                calendar_id: remote_calendar.id,
                event_id: unsupported.id,
                remote_href: unsupported_href.to_string(),
                remote_uid: "cached-recurring".to_string(),
                etag: Some("\"old-recurring\"".to_string()),
            },
        ] {
            repo.upsert_event_sync_state(&state).unwrap();
        }

        let changes = SyncCollection {
            sync_token: "token-next".to_string(),
            changes: vec![
                ResourceRecord {
                    href: update_href.to_string(),
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
                    href: deleted_href.to_string(),
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
            ],
        };
        let summary = repo
            .reconcile_remote_changes(remote_calendar.id, &changes)
            .unwrap();
        assert_eq!(
            (
                summary.added,
                summary.updated,
                summary.deleted,
                summary.skipped
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(repo.get_event(update.id).unwrap().title, "Remote planning");
        assert_eq!(
            repo.get_event_sync_state(update.id).unwrap().remote_uid,
            "planning-remote"
        );
        assert_eq!(
            repo.get_event_sync_state(update.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"planning-v2\"")
        );
        let new_state = repo
            .find_event_sync_state_by_remote_href(remote_calendar.id, new_href)
            .unwrap();
        assert_eq!(
            repo.get_event(new_state.event_id).unwrap().title,
            "New remote event"
        );
        assert_eq!(new_state.remote_uid, "new-remote");
        assert!(repo.get_event(deleted.id).is_none());
        assert!(repo.get_event_sync_state(deleted.id).is_none());
        assert_eq!(repo.get_event(untouched.id), Some(untouched.clone()));
        assert_eq!(repo.get_event(unsupported.id), Some(unsupported.clone()));
        assert_eq!(
            repo.get_event_sync_state(unsupported.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"old-recurring\"")
        );
        assert_eq!(repo.get_event(local_only.id), Some(local_only.clone()));
        assert_eq!(
            repo.get_calendar_sync_state(remote_calendar.id)
                .unwrap()
                .sync_token
                .as_deref(),
            Some("token-next")
        );

        let before_invalid = repo.get_calendar_sync_state(remote_calendar.id).unwrap();
        for target in [
            Uuid::parse_str("ca110039-0000-0000-0000-000000000009").unwrap(),
            local_calendar.id,
            remote_without_state.id,
        ] {
            assert!(repo.reconcile_remote_changes(target, &changes).is_err());
            assert_eq!(
                repo.get_calendar_sync_state(remote_calendar.id),
                Some(before_invalid.clone())
            );
            assert_eq!(repo.get_event(update.id).unwrap().title, "Remote planning");
        }
        assert!(
            repo.reconcile_remote_changes(
                remote_calendar.id,
                &SyncCollection {
                    sync_token: "  ".to_string(),
                    changes: Vec::new()
                }
            )
            .is_err()
        );
        assert_eq!(
            repo.get_calendar_sync_state(remote_calendar.id),
            Some(before_invalid)
        );
    }

    let repo = SqliteRepository::open(&db_path).unwrap();
    assert_eq!(repo.get_event(update.id).unwrap().title, "Remote planning");
    assert!(repo.get_event(deleted.id).is_none());
    assert_eq!(repo.get_event(untouched.id), Some(untouched));
    assert_eq!(repo.get_event(local_only.id), Some(local_only));
    assert_eq!(
        repo.get_calendar_sync_state(remote_calendar.id)
            .unwrap()
            .sync_token
            .as_deref(),
        Some("token-next")
    );
}
