// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         pub fn reconcile_remote_changes(
//             &mut self, calendar_id: Uuid, changes: &SyncCollection,
//         ) -> Result<RemoteSnapshotSummary, RepositoryError>;
//         pub fn reconcile_remote_snapshot(
//             &mut self, calendar_id: Uuid, resources: &[ResourceRecord],
//         ) -> Result<RemoteSnapshotSummary, RepositoryError>;
//     }
//
// Remote reconciliation must not overwrite, delete, or resurrect resources
// protected by a durable pending local update or delete operation.

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
    caldav::{ResourceRecord, SyncCollection},
};
use calendar::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, Event, EventSchedule, EventSyncState,
    PendingSyncOperation,
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
        location: "Local room".to_string(),
        description: "Local notes".to_string(),
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
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{summary}\r\nDTSTART;VALUE=DATE:{day}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn resource(href: &str, uid: &str, title: &str, day: &str, etag: &str) -> ResourceRecord {
    ResourceRecord {
        href: href.to_string(),
        response_status: Some(200),
        etag: Some(etag.to_string()),
        calendar_data: Some(ics(uid, title, day)),
    }
}

#[test]
fn phase11_pending_local_operations_protect_remote_pull_reconciliation() {
    let db_path = unique_temp_db_path("pending_pull_protection");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::parse_str("ac110041-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::parse_str("ca110041-0000-0000-0000-000000000001").unwrap(),
        name: "Work".to_string(),
        color: "#d946ef".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let protected = event(
        "e1100411-0000-0000-0000-000000000001",
        calendar.id,
        "Locally edited planning",
        1,
    );
    let unrelated_update = event(
        "e1100412-0000-0000-0000-000000000002",
        calendar.id,
        "Old unrelated update",
        2,
    );
    let unrelated_delete = event(
        "e1100413-0000-0000-0000-000000000003",
        calendar.id,
        "Unrelated deletion",
        3,
    );
    let tombstone_event_id = Uuid::parse_str("e1100414-0000-0000-0000-000000000004").unwrap();
    let protected_href = "https://caldav.example.test/dav/work/protected.ics";
    let update_href = "https://caldav.example.test/dav/work/unrelated-update.ics";
    let delete_href = "https://caldav.example.test/dav/work/unrelated-delete.ics";
    let tombstone_href = "https://caldav.example.test/dav/work/tombstone.ics";
    let protected_state = EventSyncState {
        calendar_id: calendar.id,
        event_id: protected.id,
        remote_href: protected_href.to_string(),
        remote_uid: "protected@example.test".to_string(),
        etag: Some("\"protected-v1\"".to_string()),
    };
    let protected_pending = PendingSyncOperation::Update {
        calendar_id: calendar.id,
        event_id: protected.id,
        remote_href: protected_href.to_string(),
        remote_uid: "protected@example.test".to_string(),
        base_etag: Some("\"protected-v1\"".to_string()),
    };
    let tombstone = PendingSyncOperation::Delete {
        calendar_id: calendar.id,
        event_id: tombstone_event_id,
        remote_href: tombstone_href.to_string(),
        remote_uid: "deleted@example.test".to_string(),
        base_etag: Some("\"deleted-v1\"".to_string()),
    };

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        repo.save_account(&account).unwrap();
        repo.save_calendar(&calendar).unwrap();
        repo.upsert_calendar_sync_state(&CalendarSyncState {
            calendar_id: calendar.id,
            remote_url: "https://caldav.example.test/dav/work/".to_string(),
            sync_token: Some("token-old".to_string()),
        })
        .unwrap();
        for saved in [&protected, &unrelated_update, &unrelated_delete] {
            repo.save_event(saved).unwrap();
        }
        for state in [
            protected_state.clone(),
            EventSyncState {
                calendar_id: calendar.id,
                event_id: unrelated_update.id,
                remote_href: update_href.to_string(),
                remote_uid: "unrelated-update@example.test".to_string(),
                etag: Some("\"update-v1\"".to_string()),
            },
            EventSyncState {
                calendar_id: calendar.id,
                event_id: unrelated_delete.id,
                remote_href: delete_href.to_string(),
                remote_uid: "unrelated-delete@example.test".to_string(),
                etag: Some("\"delete-v1\"".to_string()),
            },
        ] {
            repo.upsert_event_sync_state(&state).unwrap();
        }
        repo.upsert_pending_sync_operation(&protected_pending)
            .unwrap();
        repo.upsert_pending_sync_operation(&tombstone).unwrap();

        let protected_remote = resource(
            protected_href,
            "protected@example.test",
            "Remote overwrite",
            "20260810",
            "\"protected-v2\"",
        );
        let tombstone_remote = resource(
            tombstone_href,
            "deleted@example.test",
            "Remote resurrection",
            "20260811",
            "\"deleted-v2\"",
        );
        let summary = repo
            .reconcile_remote_changes(
                calendar.id,
                &SyncCollection {
                    sync_token: "token-incremental".to_string(),
                    changes: vec![
                        protected_remote.clone(),
                        tombstone_remote.clone(),
                        resource(
                            update_href,
                            "unrelated-update@example.test",
                            "Remote unrelated update",
                            "20260812",
                            "\"update-v2\"",
                        ),
                        ResourceRecord {
                            href: delete_href.to_string(),
                            response_status: Some(404),
                            etag: None,
                            calendar_data: None,
                        },
                    ],
                },
            )
            .unwrap();
        assert_eq!(
            (summary.updated, summary.deleted, summary.skipped),
            (1, 1, 2)
        );
        assert_eq!(repo.get_event(protected.id), Some(protected.clone()));
        assert_eq!(
            repo.get_event_sync_state(protected.id),
            Some(protected_state.clone())
        );
        assert_eq!(
            repo.get_pending_sync_operation(protected.id),
            Some(protected_pending.clone())
        );
        assert!(repo.get_event(tombstone_event_id).is_none());
        assert!(repo.get_event_sync_state(tombstone_event_id).is_none());
        assert_eq!(
            repo.get_pending_sync_operation(tombstone_event_id),
            Some(tombstone.clone())
        );
        assert_eq!(
            repo.get_event(unrelated_update.id).unwrap().title,
            "Remote unrelated update"
        );
        assert!(repo.get_event(unrelated_delete.id).is_none());
        assert_eq!(
            repo.get_calendar_sync_state(calendar.id)
                .unwrap()
                .sync_token
                .as_deref(),
            Some("token-incremental")
        );

        let summary = repo
            .reconcile_remote_snapshot(
                calendar.id,
                &[
                    tombstone_remote,
                    resource(
                        update_href,
                        "unrelated-update@example.test",
                        "Remote snapshot update",
                        "20260813",
                        "\"update-v3\"",
                    ),
                ],
            )
            .unwrap();
        assert_eq!(
            (summary.updated, summary.deleted, summary.skipped),
            (1, 0, 2)
        );
        assert_eq!(repo.get_event(protected.id), Some(protected.clone()));
        assert_eq!(
            repo.get_event_sync_state(protected.id),
            Some(protected_state.clone())
        );
        assert_eq!(
            repo.get_pending_sync_operation(protected.id),
            Some(protected_pending.clone())
        );
        assert!(repo.get_event(tombstone_event_id).is_none());
        assert!(repo.get_event_sync_state(tombstone_event_id).is_none());
        assert_eq!(
            repo.get_pending_sync_operation(tombstone_event_id),
            Some(tombstone.clone())
        );
        assert_eq!(
            repo.get_event(unrelated_update.id).unwrap().title,
            "Remote snapshot update"
        );

        drop(repo);
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        assert_eq!(repo.get_event(protected.id), Some(protected.clone()));
        assert_eq!(
            repo.get_event_sync_state(protected.id),
            Some(protected_state.clone())
        );
        assert_eq!(
            repo.get_pending_sync_operation(protected.id),
            Some(protected_pending.clone())
        );
        assert!(repo.get_event(tombstone_event_id).is_none());
        assert!(repo.get_event_sync_state(tombstone_event_id).is_none());
        assert_eq!(
            repo.get_pending_sync_operation(tombstone_event_id),
            Some(tombstone.clone())
        );

        assert!(repo.remove_pending_sync_operation(protected.id));
        let summary = repo
            .reconcile_remote_changes(
                calendar.id,
                &SyncCollection {
                    sync_token: "token-applied".to_string(),
                    changes: vec![protected_remote],
                },
            )
            .unwrap();
        assert_eq!((summary.updated, summary.skipped), (1, 0));
        assert_eq!(
            repo.get_event(protected.id).unwrap().title,
            "Remote overwrite"
        );
        assert_eq!(
            repo.get_event_sync_state(protected.id)
                .unwrap()
                .etag
                .as_deref(),
            Some("\"protected-v2\"")
        );
        assert!(repo.get_pending_sync_operation(protected.id).is_none());
    }

    let repo = SqliteRepository::open(&db_path).unwrap();
    assert_eq!(
        repo.get_event(protected.id).unwrap().title,
        "Remote overwrite"
    );
    assert!(repo.get_event(tombstone_event_id).is_none());
    assert!(repo.get_event_sync_state(tombstone_event_id).is_none());
    assert_eq!(
        repo.get_pending_sync_operation(tombstone_event_id),
        Some(tombstone)
    );
    assert_eq!(
        repo.get_event(unrelated_update.id).unwrap().title,
        "Remote snapshot update"
    );
    assert!(repo.get_event(unrelated_delete.id).is_none());
    assert_eq!(
        repo.get_calendar_sync_state(calendar.id)
            .unwrap()
            .sync_token
            .as_deref(),
        Some("token-applied")
    );
}
