// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         pub fn create_event_with_sync(&mut self, event: &Event) -> Result<(), RepositoryError>;
//         pub fn update_event_with_sync(&mut self, event: &Event) -> Result<(), RepositoryError>;
//         pub fn delete_event_with_sync_undo(
//             &mut self, id: Uuid,
//         ) -> Result<EventDeletionUndo, RepositoryError>;
//         pub fn undo_event_with_sync(
//             &mut self, undo: &mut EventDeletionUndo,
//         ) -> Result<(), RepositoryError>;
//     }
//
// These concrete local-edit methods inspect a calendar's source and read-only
// flag, atomically maintaining events, sync identity, and upload intent.

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

fn event(id: &str, calendar_id: Uuid, title: &str) -> Event {
    Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id,
        title: title.to_string(),
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
fn phase11_local_event_crud_coalesces_caldav_intent_and_restores_it_on_undo() {
    let db_path = unique_temp_db_path("local_sync_crud");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::parse_str("ac110041-0000-0000-0000-000000000001").unwrap(),
        name: "Work".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let local = Calendar {
        id: Uuid::parse_str("ca110041-0000-0000-0000-000000000001").unwrap(),
        name: "Local".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let remote = Calendar {
        id: Uuid::parse_str("ca110042-0000-0000-0000-000000000002").unwrap(),
        name: "Remote".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let read_only = Calendar {
        id: Uuid::parse_str("ca110043-0000-0000-0000-000000000003").unwrap(),
        name: "Archive".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: true,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local_event = event("e1100411-0000-0000-0000-000000000001", local.id, "Local");
    let remote_event = event("e1100412-0000-0000-0000-000000000002", remote.id, "Draft");
    let read_only_event = event(
        "e1100413-0000-0000-0000-000000000003",
        read_only.id,
        "Read-only",
    );
    let created = PendingSyncOperation::Create {
        calendar_id: remote.id,
        event_id: remote_event.id,
        remote_uid: remote_event.id.to_string(),
    };

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        repo.save_account(&account).unwrap();
        repo.save_calendar(&local).unwrap();
        repo.save_calendar(&remote).unwrap();
        repo.save_calendar(&read_only).unwrap();

        repo.create_event_with_sync(&local_event).unwrap();
        let mut edited_local = local_event.clone();
        edited_local.title = "Edited local".to_string();
        repo.update_event_with_sync(&edited_local).unwrap();
        let mut local_undo = repo.delete_event_with_sync_undo(local_event.id).unwrap();
        assert!(repo.get_event(local_event.id).is_none());
        assert!(repo.get_pending_sync_operation(local_event.id).is_none());
        repo.undo_event_with_sync(&mut local_undo).unwrap();
        assert_eq!(repo.get_event(local_event.id), Some(edited_local));
        assert!(repo.get_pending_sync_operation(local_event.id).is_none());

        repo.create_event_with_sync(&remote_event).unwrap();
        assert_eq!(repo.get_event(remote_event.id), Some(remote_event.clone()));
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(created.clone())
        );
        let mut edited_remote = remote_event.clone();
        edited_remote.title = "Draft revised".to_string();
        repo.update_event_with_sync(&edited_remote).unwrap();
        assert_eq!(repo.get_event(remote_event.id), Some(edited_remote));
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(created)
        );
    }

    let tracked_state = EventSyncState {
        calendar_id: remote.id,
        event_id: remote_event.id,
        remote_href: "https://caldav.example.test/dav/work/draft.ics".to_string(),
        remote_uid: remote_event.id.to_string(),
        etag: Some("\"v1\"".to_string()),
    };
    let pending_update = PendingSyncOperation::Update {
        calendar_id: remote.id,
        event_id: remote_event.id,
        remote_href: tracked_state.remote_href.clone(),
        remote_uid: tracked_state.remote_uid.clone(),
        base_etag: tracked_state.etag.clone(),
    };

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        let mut unsynced_undo = repo.delete_event_with_sync_undo(remote_event.id).unwrap();
        assert!(repo.get_event(remote_event.id).is_none());
        assert!(repo.get_pending_sync_operation(remote_event.id).is_none());
        repo.undo_event_with_sync(&mut unsynced_undo).unwrap();
        assert_eq!(
            repo.get_event(remote_event.id).unwrap().title,
            "Draft revised"
        );
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(PendingSyncOperation::Create {
                calendar_id: remote.id,
                event_id: remote_event.id,
                remote_uid: remote_event.id.to_string(),
            })
        );
        assert!(repo.undo_event_with_sync(&mut unsynced_undo).is_err());

        // Raw repositories model successful upload and a later pull refresh.
        repo.upsert_event_sync_state(&tracked_state).unwrap();
        assert!(repo.remove_pending_sync_operation(remote_event.id));
        let mut first_tracked_edit = repo.get_event(remote_event.id).unwrap();
        first_tracked_edit.title = "Tracked edit".to_string();
        repo.update_event_with_sync(&first_tracked_edit).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(pending_update.clone())
        );

        let refreshed_state = EventSyncState {
            etag: Some("\"v2\"".to_string()),
            ..tracked_state.clone()
        };
        repo.upsert_event_sync_state(&refreshed_state).unwrap();
        let mut second_tracked_edit = first_tracked_edit.clone();
        second_tracked_edit.title = "Tracked edit again".to_string();
        repo.update_event_with_sync(&second_tracked_edit).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(pending_update.clone())
        );

        let mut tracked_undo = repo.delete_event_with_sync_undo(remote_event.id).unwrap();
        assert!(repo.get_event(remote_event.id).is_none());
        assert!(repo.get_event_sync_state(remote_event.id).is_none());
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(PendingSyncOperation::Delete {
                calendar_id: remote.id,
                event_id: remote_event.id,
                remote_href: tracked_state.remote_href.clone(),
                remote_uid: tracked_state.remote_uid.clone(),
                base_etag: tracked_state.etag.clone(),
            })
        );
        repo.undo_event_with_sync(&mut tracked_undo).unwrap();
        assert_eq!(repo.get_event(remote_event.id), Some(second_tracked_edit));
        assert_eq!(
            repo.get_event_sync_state(remote_event.id),
            Some(refreshed_state.clone())
        );
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(pending_update)
        );
        assert!(repo.undo_event_with_sync(&mut tracked_undo).is_err());

        assert!(repo.remove_pending_sync_operation(remote_event.id));
        let mut clean_tracked_undo = repo.delete_event_with_sync_undo(remote_event.id).unwrap();
        assert_eq!(
            repo.get_pending_sync_operation(remote_event.id),
            Some(PendingSyncOperation::Delete {
                calendar_id: remote.id,
                event_id: remote_event.id,
                remote_href: refreshed_state.remote_href.clone(),
                remote_uid: refreshed_state.remote_uid.clone(),
                base_etag: refreshed_state.etag.clone(),
            })
        );
        repo.undo_event_with_sync(&mut clean_tracked_undo).unwrap();
        assert_eq!(
            repo.get_event_sync_state(remote_event.id),
            Some(refreshed_state)
        );
        assert!(repo.get_pending_sync_operation(remote_event.id).is_none());

        assert!(repo.create_event_with_sync(&read_only_event).is_err());
        assert!(repo.get_event(read_only_event.id).is_none());
        repo.save_event(&read_only_event).unwrap();
        let mut edited_read_only = read_only_event.clone();
        edited_read_only.title = "Forbidden edit".to_string();
        assert!(repo.update_event_with_sync(&edited_read_only).is_err());
        assert!(
            repo.delete_event_with_sync_undo(read_only_event.id)
                .is_err()
        );
        assert_eq!(
            repo.get_event(read_only_event.id),
            Some(read_only_event.clone())
        );
        assert!(
            repo.get_pending_sync_operation(read_only_event.id)
                .is_none()
        );
    }
}
