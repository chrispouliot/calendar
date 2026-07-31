// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         pub fn replace_detached_events(
//             &mut self,
//             master_event_id: Uuid,
//             exceptions: &[DetachedEvent],
//         ) -> Result<(), RepositoryError>;
//         pub fn list_detached_events(&self, master_event_id: Uuid) -> Vec<DetachedEvent>;
//     }

use calendar::backend::{CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{
    Calendar, CalendarSource, DetachedEvent, Event, EventSchedule, RecurrenceId, RecurrenceSpec,
    ReminderSpec,
};
use chrono::{DateTime, FixedOffset};
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

fn date_time(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

#[test]
fn detached_exceptions_replace_round_trip_and_cascade_with_their_master() {
    let db_path = unique_temp_db_path("event_exceptions");
    let _cleanup = TempDb(db_path.clone());
    let calendar = Calendar {
        id: Uuid::parse_str("2a000001-0000-0000-0000-000000000001").unwrap(),
        name: "Work".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let master = Event {
        id: Uuid::parse_str("2a000002-0000-0000-0000-000000000002").unwrap(),
        calendar_id: calendar.id,
        title: "Standup".to_owned(),
        location: "Room 1".to_owned(),
        description: "Daily team sync".to_owned(),
        schedule: EventSchedule::Timed {
            start: date_time("2026-07-13T09:00:00-04:00"),
            end: date_time("2026-07-13T09:30:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["FREQ=WEEKLY".to_owned()],
            ..Default::default()
        }),
        reminders: Vec::new(),
    };
    let modified = DetachedEvent::Modified {
        recurrence_id: RecurrenceId::Timed {
            date_time: date_time("2026-07-20T09:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        title: "Standup moved".to_owned(),
        location: "Zoom".to_owned(),
        description: "Discuss the release".to_owned(),
        schedule: EventSchedule::Timed {
            start: date_time("2026-07-20T10:00:00-04:00"),
            end: date_time("2026-07-20T10:45:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        reminders: vec![ReminderSpec {
            seconds_before_start: 900,
            description: "Join the call".to_owned(),
        }],
    };
    let cancelled = DetachedEvent::Cancelled {
        recurrence_id: RecurrenceId::Timed {
            date_time: date_time("2026-07-27T09:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
    };
    let duplicate_recurrence_id = DetachedEvent::Cancelled {
        recurrence_id: RecurrenceId::Timed {
            date_time: date_time("2026-07-20T09:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
    };
    let updated_modified = DetachedEvent::Modified {
        recurrence_id: RecurrenceId::Timed {
            date_time: date_time("2026-07-20T09:00:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        title: "Standup moved again".to_owned(),
        location: "Zoom".to_owned(),
        description: "Discuss the release".to_owned(),
        schedule: EventSchedule::Timed {
            start: date_time("2026-07-20T10:00:00-04:00"),
            end: date_time("2026-07-20T10:45:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        reminders: vec![ReminderSpec {
            seconds_before_start: 900,
            description: "Join the call".to_owned(),
        }],
    };

    {
        let mut repository = SqliteRepository::open(&db_path).expect("open isolated database");
        repository.save_calendar(&calendar).expect("save calendar");
        repository
            .save_event(&master)
            .expect("save recurring master");

        repository
            .replace_detached_events(master.id, &[cancelled.clone(), modified.clone()])
            .expect("replace detached exceptions");
        assert_eq!(
            repository.list_detached_events(master.id),
            vec![modified.clone(), cancelled.clone()],
            "exceptions must round-trip in recurrence-id order rather than caller order"
        );

        assert!(
            repository
                .replace_detached_events(master.id, &[modified.clone(), duplicate_recurrence_id],)
                .is_err(),
            "a master must reject multiple detached exceptions with one RECURRENCE-ID"
        );
        assert_eq!(
            repository.list_detached_events(master.id),
            vec![modified.clone(), cancelled.clone()],
            "a rejected duplicate replacement must leave the prior exception set intact"
        );

        repository
            .replace_detached_events(master.id, std::slice::from_ref(&updated_modified))
            .expect("replace the complete detached exception set");
        assert_eq!(
            repository.list_detached_events(master.id),
            vec![updated_modified.clone()],
            "replacement must update retained exceptions and remove stale children together"
        );
    }

    {
        let mut repository = SqliteRepository::open(&db_path).expect("reopen isolated database");
        assert_eq!(
            repository.list_detached_events(master.id),
            vec![updated_modified],
            "the replacement set must survive reopening the database"
        );
        assert!(
            repository.delete_event(master.id),
            "delete the master event"
        );
        assert!(
            repository.list_detached_events(master.id).is_empty(),
            "deleting a master must cascade-delete its detached exceptions"
        );
    }
}
