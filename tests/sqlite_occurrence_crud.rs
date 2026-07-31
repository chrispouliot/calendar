// Public contract pinned by this acceptance test:
//
//     pub struct OccurrenceUndo { /* opaque, one-shot undo token */ }
//
//     impl SqliteRepository {
//         /// Replaces only `exception.recurrence_id` on a generated occurrence
//         /// of a recurring, writable master and records master-level sync intent.
//         pub fn upsert_occurrence_with_sync(
//             &mut self,
//             master_event_id: Uuid,
//             exception: &DetachedEvent,
//         ) -> Result<(), RepositoryError>;
//
//         /// Cancels one generated recurrence identity and returns a one-shot
//         /// token that restores its exact prior child and pending-sync state.
//         pub fn cancel_occurrence_with_sync_undo(
//             &mut self,
//             master_event_id: Uuid,
//             recurrence_id: &RecurrenceId,
//         ) -> Result<OccurrenceUndo, RepositoryError>;
//
//         pub fn undo_occurrence_with_sync(
//             &mut self,
//             undo: &mut OccurrenceUndo,
//         ) -> Result<(), RepositoryError>;
//     }

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, PendingSyncOperationRepository,
    SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, DetachedEvent, Event, EventSchedule, EventSyncState,
    PendingSyncOperation, RecurrenceId, RecurrenceSpec,
};
use chrono::{DateTime, FixedOffset, NaiveDate};
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_db_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "calendar_phase2b_{label}_{}.sqlite",
        Uuid::new_v4()
    ))
}

struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn at(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

fn recurring_master(id: Uuid, calendar_id: Uuid) -> Event {
    Event {
        id,
        calendar_id,
        title: "Weekly team".to_owned(),
        location: "Room A".to_owned(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at("2026-07-06T09:00:00-04:00"),
            end: at("2026-07-06T10:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;COUNT=4".to_owned()],
            ..Default::default()
        }),
        reminders: Vec::new(),
    }
}

fn recurrence_id(day: u32) -> RecurrenceId {
    RecurrenceId::Timed {
        date_time: at(&format!("2026-07-{day:02}T09:00:00-04:00")),
        timezone: Some("America/New_York".to_owned()),
    }
}

fn modified(day: u32, title: &str) -> DetachedEvent {
    DetachedEvent::Modified {
        recurrence_id: recurrence_id(day),
        title: title.to_owned(),
        location: "Video".to_owned(),
        description: "Changed instance".to_owned(),
        schedule: EventSchedule::Timed {
            start: at(&format!("2026-07-{day:02}T11:00:00-04:00")),
            end: at(&format!("2026-07-{day:02}T12:00:00-04:00")),
            timezone: Some("America/New_York".to_owned()),
        },
        reminders: Vec::new(),
    }
}

#[test]
fn occurrence_crud_preserves_siblings_sync_intent_and_exact_undo_state() {
    let db_path = unique_temp_db_path("occurrence_crud");
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::new_v4(),
        name: "Work".to_owned(),
        server_url: "https://caldav.example.test/dav/".to_owned(),
        username: "ada".to_owned(),
        enabled: true,
    };
    let remote = Calendar {
        id: Uuid::new_v4(),
        name: "Remote".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local = Calendar {
        id: Uuid::new_v4(),
        name: "Local".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let remote_master = recurring_master(Uuid::new_v4(), remote.id);
    let local_master = recurring_master(Uuid::new_v4(), local.id);
    let prior_modified = modified(13, "Existing sibling");
    let cancelled_sibling = DetachedEvent::Cancelled {
        recurrence_id: recurrence_id(20),
    };
    let replacement = modified(27, "Changed only this occurrence");
    let expected_update = PendingSyncOperation::Update {
        calendar_id: remote.id,
        event_id: remote_master.id,
        remote_href: "https://caldav.example.test/dav/work/weekly-team.ics".to_owned(),
        remote_uid: "weekly-team@example.test".to_owned(),
        base_etag: Some("\"v1\"".to_owned()),
    };

    let mut repo = SqliteRepository::open(&db_path).unwrap();
    repo.save_account(&account).unwrap();
    repo.save_calendar(&remote).unwrap();
    repo.save_calendar(&local).unwrap();
    repo.save_event(&remote_master).unwrap();
    repo.save_event(&local_master).unwrap();
    repo.replace_detached_events(
        remote_master.id,
        &[prior_modified.clone(), cancelled_sibling.clone()],
    )
    .unwrap();
    repo.upsert_event_sync_state(&EventSyncState {
        calendar_id: remote.id,
        event_id: remote_master.id,
        remote_href: "https://caldav.example.test/dav/work/weekly-team.ics".to_owned(),
        remote_uid: "weekly-team@example.test".to_owned(),
        etag: Some("\"v1\"".to_owned()),
    })
    .unwrap();

    repo.upsert_occurrence_with_sync(remote_master.id, &replacement)
        .unwrap();
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![
            prior_modified.clone(),
            cancelled_sibling.clone(),
            replacement.clone()
        ]
    );
    assert_eq!(
        repo.get_pending_sync_operation(remote_master.id),
        Some(expected_update.clone())
    );

    let replaced_sibling = modified(13, "Replaced only this occurrence");
    repo.upsert_occurrence_with_sync(remote_master.id, &replaced_sibling)
        .unwrap();
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![
            replaced_sibling.clone(),
            cancelled_sibling.clone(),
            replacement.clone()
        ],
        "an upsert must replace its matching child without changing siblings"
    );

    let mut absent_undo = repo
        .cancel_occurrence_with_sync_undo(remote_master.id, &recurrence_id(6))
        .unwrap();
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![
            DetachedEvent::Cancelled {
                recurrence_id: recurrence_id(6)
            },
            replaced_sibling.clone(),
            cancelled_sibling.clone(),
            replacement.clone()
        ]
    );
    repo.undo_occurrence_with_sync(&mut absent_undo).unwrap();
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![
            replaced_sibling.clone(),
            cancelled_sibling.clone(),
            replacement.clone()
        ]
    );
    assert_eq!(
        repo.get_pending_sync_operation(remote_master.id),
        Some(expected_update.clone())
    );

    let mut modified_undo = repo
        .cancel_occurrence_with_sync_undo(remote_master.id, &recurrence_id(13))
        .unwrap();
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![
            DetachedEvent::Cancelled {
                recurrence_id: recurrence_id(13)
            },
            cancelled_sibling.clone(),
            replacement.clone()
        ]
    );
    repo.undo_occurrence_with_sync(&mut modified_undo).unwrap();
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![
            replaced_sibling.clone(),
            cancelled_sibling.clone(),
            replacement.clone()
        ]
    );
    assert_eq!(
        repo.get_pending_sync_operation(remote_master.id),
        Some(expected_update.clone())
    );

    repo.upsert_occurrence_with_sync(local_master.id, &modified(13, "Local override"))
        .unwrap();
    let mut local_undo = repo
        .cancel_occurrence_with_sync_undo(local_master.id, &recurrence_id(20))
        .unwrap();
    assert_eq!(
        repo.list_detached_events(local_master.id),
        vec![
            modified(13, "Local override"),
            DetachedEvent::Cancelled {
                recurrence_id: recurrence_id(20)
            }
        ]
    );
    repo.undo_occurrence_with_sync(&mut local_undo).unwrap();
    assert_eq!(
        repo.list_detached_events(local_master.id),
        vec![modified(13, "Local override")]
    );
    assert!(repo.get_pending_sync_operation(local_master.id).is_none());

    let before_invalid = repo.list_detached_events(remote_master.id);
    assert!(
        repo.upsert_occurrence_with_sync(Uuid::new_v4(), &replacement)
            .is_err()
    );
    let non_recurring = Event {
        recurrence: None,
        ..recurring_master(Uuid::new_v4(), remote.id)
    };
    repo.save_event(&non_recurring).unwrap();
    assert!(
        repo.cancel_occurrence_with_sync_undo(non_recurring.id, &recurrence_id(6))
            .is_err()
    );
    assert!(
        repo.cancel_occurrence_with_sync_undo(
            remote_master.id,
            &RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 7, 6).unwrap())
        )
        .is_err()
    );
    assert!(
        repo.cancel_occurrence_with_sync_undo(
            remote_master.id,
            &RecurrenceId::Timed {
                date_time: at("2026-07-06T09:00:00-04:00"),
                timezone: Some("Europe/London".to_owned())
            }
        )
        .is_err()
    );
    assert!(
        repo.cancel_occurrence_with_sync_undo(remote_master.id, &recurrence_id(31))
            .is_err()
    );
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        before_invalid,
        "rejected occurrence edits must not mutate siblings or the target set"
    );
    assert_eq!(
        repo.get_pending_sync_operation(remote_master.id),
        Some(expected_update)
    );
}
