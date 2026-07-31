// Public contract pinned by this acceptance test:
//
//     pub struct FollowingEditResult { /* opaque */ }
//     impl FollowingEditResult {
//         pub fn future_master_id(&self) -> Uuid;
//     }
//     pub struct FollowingUndo { /* opaque, one-shot */ }
//     impl SqliteRepository {
//         /// Atomically split a writable recurring master immediately before
//         /// `edited.recurrence_id`.  `edited` must be a generated, non-first
//         /// Modified occurrence whose identity matches the master.  Its
//         /// resolved fields become the future master's template.
//         pub fn edit_this_and_following_with_sync(
//             &mut self, master_event_id: Uuid, edited: &DetachedEvent,
//         ) -> Result<FollowingEditResult, RepositoryError>;
//
//         /// As above, but use `future_recurrence` for the new master rather
//         /// than deriving it from the original master's rule. This permits a
//         /// following edit to change both schedule kind and recurrence.
//         pub fn edit_this_and_following_with_sync_and_recurrence(
//             &mut self, master_event_id: Uuid, edited: &DetachedEvent,
//             future_recurrence: &RecurrenceSpec,
//         ) -> Result<FollowingEditResult, RepositoryError>;
//
//         /// Atomically remove the selected generated occurrence and every
//         /// following occurrence, returning a token that restores the exact
//         /// prior database and upload intent once.
//         pub fn delete_this_and_following_with_sync_undo(
//             &mut self, master_event_id: Uuid, recurrence_id: &RecurrenceId,
//         ) -> Result<FollowingUndo, RepositoryError>;
//         pub fn undo_this_and_following_with_sync(
//             &mut self, undo: &mut FollowingUndo,
//         ) -> Result<(), RepositoryError>;
//     }

use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, FollowingEditResult, FollowingUndo,
    PendingSyncOperationRepository, SqliteRepository, SyncStateRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, DetachedEvent, Event, EventSchedule, EventSyncState,
    PendingSyncOperation, RecurrenceId, RecurrenceSpec, ReminderSpec,
};
use calendar::month_view::event_for_recurrence_id;
use chrono::{DateTime, FixedOffset, NaiveDate};
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "calendar_phase2c_following_{}.sqlite",
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

fn recurrence_id(day: u32) -> RecurrenceId {
    recurrence_id_at(day, 9)
}

fn recurrence_id_at(day: u32, hour: u32) -> RecurrenceId {
    recurrence_id_on(7, day, hour)
}

fn recurrence_id_on(month: u32, day: u32, hour: u32) -> RecurrenceId {
    RecurrenceId::Timed {
        date_time: at(&format!("2026-{month:02}-{day:02}T{hour:02}:00:00-04:00")),
        timezone: Some("America/New_York".to_owned()),
    }
}

fn master(id: Uuid, calendar_id: Uuid) -> Event {
    Event {
        id,
        calendar_id,
        title: "Weekly team".to_owned(),
        location: "Room A".to_owned(),
        description: "Original description".to_owned(),
        schedule: EventSchedule::Timed {
            start: at("2026-07-06T09:00:00-04:00"),
            end: at("2026-07-06T10:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;COUNT=5".to_owned()],
            ..Default::default()
        }),
        reminders: vec![ReminderSpec {
            seconds_before_start: 900,
            description: "Original reminder".to_owned(),
        }],
    }
}

fn modified(day: u32, title: &str) -> DetachedEvent {
    DetachedEvent::Modified {
        recurrence_id: recurrence_id(day),
        title: title.to_owned(),
        location: "Video room".to_owned(),
        description: "Edited future description".to_owned(),
        // The recurrence identity remains 09:00; changing the selected
        // occurrence's same-day time is explicitly supported by this phase.
        schedule: EventSchedule::Timed {
            start: at(&format!("2026-07-{day:02}T11:00:00-04:00")),
            end: at(&format!("2026-07-{day:02}T12:00:00-04:00")),
            timezone: Some("America/New_York".to_owned()),
        },
        reminders: vec![ReminderSpec {
            seconds_before_start: 300,
            description: "Edited reminder".to_owned(),
        }],
    }
}

fn with_recurrence_id(detached: &DetachedEvent, recurrence_id: RecurrenceId) -> DetachedEvent {
    match detached {
        DetachedEvent::Modified {
            title,
            location,
            description,
            schedule,
            reminders,
            ..
        } => DetachedEvent::Modified {
            recurrence_id,
            title: title.clone(),
            location: location.clone(),
            description: description.clone(),
            schedule: schedule.clone(),
            reminders: reminders.clone(),
        },
        DetachedEvent::Cancelled { .. } => DetachedEvent::Cancelled { recurrence_id },
    }
}

#[test]
fn phase2c_following_split_delete_and_undo_preserve_sync_boundaries_atomically() {
    let db_path = unique_temp_db_path();
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
    let read_only = Calendar {
        id: Uuid::new_v4(),
        read_only: true,
        ..remote.clone()
    };
    let remote_master = master(Uuid::new_v4(), remote.id);
    let delete_master = master(Uuid::new_v4(), remote.id);
    let local_master = master(Uuid::new_v4(), local.id);
    let readonly_master = master(Uuid::new_v4(), read_only.id);
    let sync = EventSyncState {
        calendar_id: remote.id,
        event_id: remote_master.id,
        remote_href: "https://caldav.example.test/dav/work/weekly.ics".to_owned(),
        remote_uid: "weekly@example.test".to_owned(),
        etag: Some("\"v1\"".to_owned()),
    };
    // These masters deliberately have distinct remote resources.  Besides
    // matching CalDAV identity semantics, this makes setup require the SQLite
    // UNIQUE(calendar_id, remote_href) invariant rather than masking it.
    let delete_sync = EventSyncState {
        calendar_id: remote.id,
        event_id: delete_master.id,
        remote_href: "https://caldav.example.test/dav/work/delete-weekly.ics".to_owned(),
        remote_uid: "delete-weekly@example.test".to_owned(),
        etag: Some("\"delete-v1\"".to_owned()),
    };
    let expected_update = |state: &EventSyncState| PendingSyncOperation::Update {
        calendar_id: state.calendar_id,
        event_id: state.event_id,
        remote_href: state.remote_href.clone(),
        remote_uid: state.remote_uid.clone(),
        base_etag: state.etag.clone(),
    };

    let before_child = modified(13, "Past override");
    let selected_child = modified(20, "Old selected override");
    let later_child = modified(27, "Later override");
    let later_cancellation = DetachedEvent::Cancelled {
        recurrence_id: recurrence_id_on(8, 3, 9),
    };
    let rebased_later_child = with_recurrence_id(&later_child, recurrence_id_at(27, 11));
    let rebased_later_cancellation =
        with_recurrence_id(&later_cancellation, recurrence_id_on(8, 3, 11));
    let delete_past = modified(13, "Delete past override");
    let delete_selected = modified(20, "Delete selected override");
    let delete_later = modified(27, "Delete later override");
    let edited = modified(20, "Renamed future series");

    let mut repo = SqliteRepository::open(&db_path).unwrap();
    repo.save_account(&account).unwrap();
    for calendar in [&remote, &local, &read_only] {
        repo.save_calendar(calendar).unwrap();
    }
    for event in [
        &remote_master,
        &delete_master,
        &local_master,
        &readonly_master,
    ] {
        repo.save_event(event).unwrap();
    }
    repo.replace_detached_events(
        remote_master.id,
        &[
            before_child.clone(),
            selected_child,
            later_child.clone(),
            later_cancellation,
        ],
    )
    .unwrap();
    repo.replace_detached_events(
        delete_master.id,
        &[delete_past.clone(), delete_selected, delete_later],
    )
    .unwrap();
    repo.upsert_event_sync_state(&sync).unwrap();
    repo.upsert_event_sync_state(&delete_sync).unwrap();

    let split: FollowingEditResult = repo
        .edit_this_and_following_with_sync(remote_master.id, &edited)
        .unwrap();
    let future_id = split.future_master_id();
    assert_ne!(future_id, remote_master.id);
    assert_eq!(
        repo.get_event_sync_state(remote_master.id),
        Some(sync.clone())
    );
    assert_eq!(
        repo.get_event(remote_master.id)
            .unwrap()
            .recurrence
            .unwrap()
            .rrule,
        vec!["RRULE:FREQ=WEEKLY;COUNT=2"]
    );
    let future = repo.get_event(future_id).unwrap();
    assert_eq!(
        future.schedule,
        match &edited {
            DetachedEvent::Modified { schedule, .. } => schedule.clone(),
            _ => unreachable!(),
        }
    );
    assert_eq!(
        future.recurrence.as_ref().unwrap().rrule,
        vec!["RRULE:FREQ=WEEKLY;COUNT=3"]
    );
    assert_eq!(
        (
            future.title.clone(),
            future.location.clone(),
            future.description.clone(),
            future.reminders.clone()
        ),
        match &edited {
            DetachedEvent::Modified {
                title,
                location,
                description,
                reminders,
                ..
            } => (
                title.clone(),
                location.clone(),
                description.clone(),
                reminders.clone()
            ),
            _ => unreachable!(),
        }
    );
    assert_eq!(
        repo.list_detached_events(remote_master.id),
        vec![before_child]
    );
    let future_children = repo.list_detached_events(future_id);
    assert_eq!(
        future_children,
        vec![
            rebased_later_child.clone(),
            rebased_later_cancellation.clone()
        ],
        "future children retain their payload but are rebased to the future master's 11:00 generated identities"
    );
    assert_eq!(
        event_for_recurrence_id(&future, &future_children, &recurrence_id_at(27, 11)),
        Some(Event {
            id: future.id,
            calendar_id: future.calendar_id,
            title: "Later override".to_owned(),
            location: "Video room".to_owned(),
            description: "Edited future description".to_owned(),
            schedule: match &later_child {
                DetachedEvent::Modified { schedule, .. } => schedule.clone(),
                _ => unreachable!(),
            },
            recurrence: None,
            reminders: match &later_child {
                DetachedEvent::Modified { reminders, .. } => reminders.clone(),
                _ => unreachable!(),
            },
        }),
        "a reparented override must resolve through the future master using its rebased identity"
    );
    assert_eq!(
        event_for_recurrence_id(&future, &future_children, &recurrence_id_on(8, 3, 11)),
        None,
        "a reparented cancellation must also use the future master's identity"
    );
    assert_eq!(
        repo.get_pending_sync_operation(remote_master.id),
        Some(expected_update(&sync))
    );
    assert!(
        matches!(repo.get_pending_sync_operation(future_id), Some(PendingSyncOperation::Create { remote_uid, .. }) if remote_uid != sync.remote_uid)
    );

    let mut undo: FollowingUndo = repo
        .delete_this_and_following_with_sync_undo(delete_master.id, &recurrence_id(20))
        .unwrap();
    assert_eq!(
        repo.get_event(delete_master.id)
            .unwrap()
            .recurrence
            .unwrap()
            .rrule,
        vec!["RRULE:FREQ=WEEKLY;COUNT=2"]
    );
    assert_eq!(
        repo.list_detached_events(delete_master.id),
        vec![delete_past.clone()]
    );
    assert_eq!(
        repo.get_pending_sync_operation(delete_master.id),
        Some(expected_update(&delete_sync))
    );
    repo.undo_this_and_following_with_sync(&mut undo).unwrap();
    assert_eq!(
        repo.get_event(delete_master.id),
        Some(delete_master.clone())
    );
    assert_eq!(
        repo.list_detached_events(delete_master.id),
        vec![
            delete_past,
            modified(20, "Delete selected override"),
            modified(27, "Delete later override")
        ]
    );
    assert_eq!(
        repo.get_pending_sync_operation(delete_master.id),
        None,
        "undo restores the exact prior upload intent"
    );
    assert!(
        repo.undo_this_and_following_with_sync(&mut undo).is_err(),
        "following undo is one-shot"
    );

    let local_split = repo
        .edit_this_and_following_with_sync(local_master.id, &edited)
        .unwrap();
    assert!(repo.get_pending_sync_operation(local_master.id).is_none());
    assert!(
        repo.get_pending_sync_operation(local_split.future_master_id())
            .is_none()
    );
    let _local_delete = repo
        .delete_this_and_following_with_sync_undo(local_master.id, &recurrence_id(13))
        .unwrap();
    assert!(repo.get_pending_sync_operation(local_master.id).is_none());

    // Rejection is transactional: custom rules, first/non-generated/mismatched
    // identities, and read-only calendars leave every stored row untouched.
    let unsupported = Event {
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=DAILY;COUNT=5".to_owned()],
            rdate: vec!["RDATE;VALUE=DATE:20260710".to_owned()],
            exdate: Vec::new(),
        }),
        ..master(Uuid::new_v4(), remote.id)
    };
    repo.save_event(&unsupported).unwrap();
    let unsupported_before = repo.get_event(unsupported.id);
    assert!(
        repo.edit_this_and_following_with_sync(unsupported.id, &edited)
            .is_err()
    );
    assert_eq!(repo.get_event(unsupported.id), unsupported_before);
    let readonly_before = repo.get_event(readonly_master.id);
    assert!(
        repo.delete_this_and_following_with_sync_undo(readonly_master.id, &recurrence_id(20))
            .is_err()
    );
    assert_eq!(repo.get_event(readonly_master.id), readonly_before);
    let original_before = repo.get_event(remote_master.id);
    let children_before = repo.list_detached_events(remote_master.id);
    assert!(
        repo.edit_this_and_following_with_sync(remote_master.id, &modified(6, "First"))
            .is_err()
    );
    assert!(
        repo.delete_this_and_following_with_sync_undo(
            remote_master.id,
            &RecurrenceId::Timed {
                date_time: at("2026-07-20T09:00:00-04:00"),
                timezone: Some("Europe/London".to_owned())
            }
        )
        .is_err()
    );
    assert_eq!(repo.get_event(remote_master.id), original_before);
    assert_eq!(repo.list_detached_events(remote_master.id), children_before);
}

#[test]
fn following_split_can_replace_an_all_day_friday_rule_with_a_timed_tuesday_series() {
    let db_path = unique_temp_db_path();
    let _cleanup = TempDb(db_path.clone());
    let account = Account {
        id: Uuid::new_v4(),
        name: "Work".to_owned(),
        server_url: "https://caldav.example.test/dav/".to_owned(),
        username: "ada".to_owned(),
        enabled: true,
    };
    let calendar = Calendar {
        id: Uuid::new_v4(),
        name: "Remote".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let master = Event {
        id: Uuid::new_v4(),
        calendar_id: calendar.id,
        title: "Friday all-day series".to_owned(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 8, 14).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 8, 15).unwrap(),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;BYDAY=FR;COUNT=6".to_owned()],
            ..Default::default()
        }),
        reminders: Vec::new(),
    };
    let sync = EventSyncState {
        calendar_id: calendar.id,
        event_id: master.id,
        remote_href: "https://caldav.example.test/dav/work/friday.ics".to_owned(),
        remote_uid: "friday@example.test".to_owned(),
        etag: Some("\"v1\"".to_owned()),
    };
    let edited = DetachedEvent::Modified {
        // This is the generated identity of the second Friday occurrence, even
        // though the edited future master will begin on the following Tuesday.
        recurrence_id: RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 8, 21).unwrap()),
        title: "Tuesday timed series".to_owned(),
        location: "Video room".to_owned(),
        description: "Moved future meetings".to_owned(),
        schedule: EventSchedule::Timed {
            start: at("2026-08-25T10:30:00-04:00"),
            end: at("2026-08-25T12:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        reminders: Vec::new(),
    };
    let desired_recurrence = RecurrenceSpec {
        rrule: vec!["RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=TU;COUNT=3".to_owned()],
        ..Default::default()
    };

    let mut repo = SqliteRepository::open(&db_path).unwrap();
    repo.save_account(&account).unwrap();
    repo.save_calendar(&calendar).unwrap();
    repo.save_event(&master).unwrap();
    repo.upsert_event_sync_state(&sync).unwrap();

    let split = repo
        .edit_this_and_following_with_sync_and_recurrence(master.id, &edited, &desired_recurrence)
        .expect("a valid all-day second occurrence can become a timed future series");
    let future_id = split.future_master_id();
    let old_master = repo.get_event(master.id).unwrap();
    let future_master = repo.get_event(future_id).unwrap();

    assert_eq!(
        old_master.recurrence.unwrap().rrule,
        vec!["RRULE:FREQ=WEEKLY;BYDAY=FR;COUNT=1"],
        "the original master retains only the August 14 occurrence before the split"
    );
    assert_eq!(
        future_master.schedule,
        match &edited {
            DetachedEvent::Modified { schedule, .. } => schedule.clone(),
            DetachedEvent::Cancelled { .. } => unreachable!(),
        }
    );
    assert_eq!(future_master.recurrence, Some(desired_recurrence));
    assert_ne!(
        future_master.recurrence.unwrap().rrule,
        vec!["RRULE:FREQ=WEEKLY;BYDAY=FR;COUNT=5"],
        "the future master must not silently retain the old Friday cadence"
    );
    assert_eq!(
        repo.get_pending_sync_operation(master.id),
        Some(PendingSyncOperation::Update {
            calendar_id: sync.calendar_id,
            event_id: sync.event_id,
            remote_href: sync.remote_href.clone(),
            remote_uid: sync.remote_uid.clone(),
            base_etag: sync.etag.clone(),
        })
    );
    assert!(
        matches!(repo.get_pending_sync_operation(future_id), Some(PendingSyncOperation::Create { remote_uid, .. }) if remote_uid != sync.remote_uid),
        "the edited future resource is created with a distinct UID"
    );
}
