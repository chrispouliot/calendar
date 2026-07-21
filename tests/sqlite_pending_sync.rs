// Public contract pinned by this acceptance test:
//
//     pub enum PendingSyncOperation {
//         Create { calendar_id: Uuid, event_id: Uuid, remote_uid: String },
//         Update { calendar_id: Uuid, event_id: Uuid, remote_href: String,
//                  remote_uid: String, base_etag: Option<String> },
//         Delete { calendar_id: Uuid, event_id: Uuid, remote_href: String,
//                  remote_uid: String, base_etag: Option<String> },
//     }
//
//     pub trait PendingSyncOperationRepository {
//         fn upsert_pending_sync_operation(&mut self, operation: &PendingSyncOperation)
//             -> Result<(), RepositoryError>;
//         fn get_pending_sync_operation(&self, event_id: Uuid) -> Option<PendingSyncOperation>;
//         fn list_pending_sync_operations(&self, calendar_id: Uuid)
//             -> Vec<PendingSyncOperation>;
//         fn remove_pending_sync_operation(&mut self, event_id: Uuid) -> bool;
//     }
//
// This is durable upload intent only: it deliberately excludes HTTP, event
// serialization, retry state, timestamps, conflicts, credentials, and scheduling.

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, Event, EventSchedule, EventSyncState, PendingSyncOperation,
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

fn calendar(id: &str, account_id: Uuid, name: &str) -> Calendar {
    Calendar {
        id: Uuid::parse_str(id).unwrap(),
        name: name.to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav { account_id },
    }
}

fn event(id: &str, calendar_id: Uuid) -> Event {
    Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id,
        title: "Planning".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 7, 21).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    }
}

#[test]
fn phase11_pending_caldav_upload_operations_survive_their_lifecycle() {
    let db_path = unique_temp_db_path("pending_sync");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::parse_str("ac110031-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let work = calendar("ca110031-0000-0000-0000-000000000001", account.id, "Work");
    let personal = calendar(
        "ca110032-0000-0000-0000-000000000002",
        account.id,
        "Personal",
    );
    let synced_event = event("e1100311-0000-0000-0000-000000000001", work.id);
    let later_event_id = Uuid::parse_str("e1100312-0000-0000-0000-000000000002").unwrap();
    let personal_event_id = Uuid::parse_str("e1100313-0000-0000-0000-000000000003").unwrap();
    let create = PendingSyncOperation::Create {
        calendar_id: work.id,
        event_id: synced_event.id,
        remote_uid: "planning@example.test".to_string(),
    };
    let update = PendingSyncOperation::Update {
        calendar_id: work.id,
        event_id: synced_event.id,
        remote_href: "https://caldav.example.test/dav/work/planning.ics".to_string(),
        remote_uid: "planning@example.test".to_string(),
        base_etag: Some("\"planning-v1\"".to_string()),
    };
    let delete = PendingSyncOperation::Delete {
        calendar_id: work.id,
        event_id: synced_event.id,
        remote_href: "https://caldav.example.test/dav/work/planning.ics".to_string(),
        remote_uid: "planning@example.test".to_string(),
        base_etag: Some("\"planning-v2\"".to_string()),
    };
    let later_create = PendingSyncOperation::Create {
        calendar_id: work.id,
        event_id: later_event_id,
        remote_uid: "later@example.test".to_string(),
    };
    let personal_create = PendingSyncOperation::Create {
        calendar_id: personal.id,
        event_id: personal_event_id,
        remote_uid: "personal@example.test".to_string(),
    };

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        repo.save_account(&account).unwrap();
        repo.save_calendar(&work).unwrap();
        repo.save_calendar(&personal).unwrap();
        repo.save_event(&synced_event).unwrap();
        repo.upsert_event_sync_state(&EventSyncState {
            calendar_id: work.id,
            event_id: synced_event.id,
            remote_href: "https://caldav.example.test/dav/work/planning.ics".to_string(),
            remote_uid: "planning@example.test".to_string(),
            etag: Some("\"planning-v1\"".to_string()),
        })
        .unwrap();
        repo.upsert_pending_sync_operation(&create).unwrap();
    }

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(synced_event.id),
            Some(create)
        );
        repo.upsert_pending_sync_operation(&update).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(synced_event.id),
            Some(update.clone())
        );
        repo.upsert_pending_sync_operation(&later_create).unwrap();
        repo.upsert_pending_sync_operation(&personal_create)
            .unwrap();
    }

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(synced_event.id),
            Some(update.clone())
        );
        assert_eq!(
            repo.list_pending_sync_operations(work.id),
            vec![update, later_create],
            "operations for a calendar must be deterministic by local event ID"
        );
        assert_eq!(
            repo.list_pending_sync_operations(personal.id),
            vec![personal_create.clone()],
            "operations from other calendars must remain isolated"
        );

        repo.upsert_pending_sync_operation(&delete).unwrap();
        assert!(repo.delete_event(synced_event.id));
        assert!(repo.get_event_sync_state(synced_event.id).is_none());
        assert_eq!(
            repo.get_pending_sync_operation(synced_event.id),
            Some(delete.clone())
        );
    }

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(synced_event.id),
            Some(delete)
        );
        assert!(repo.remove_pending_sync_operation(synced_event.id));
        assert!(!repo.remove_pending_sync_operation(synced_event.id));
        assert!(repo.delete_calendar(work.id));
        assert!(repo.list_pending_sync_operations(work.id).is_empty());
        assert_eq!(
            repo.list_pending_sync_operations(personal.id),
            vec![personal_create]
        );
    }
}
