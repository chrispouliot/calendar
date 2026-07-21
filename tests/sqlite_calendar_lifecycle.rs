use calendar::backend::{CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{Calendar, CalendarSource, Event, EventSchedule};
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
    path.push(format!("calendar_phase10_{label}_{pid}_{nanos}.sqlite"));
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

#[test]
fn phase10_calendar_update_and_delete_lifecycle() {
    let db_path = unique_temp_db_path("calendar_lifecycle");
    let _cleanup = TempDb(db_path.clone());

    let calendar = Calendar {
        id: Uuid::parse_str("ca100001-0000-0000-0000-000000000001").unwrap(),
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let event = Event {
        id: Uuid::parse_str("e1100001-0000-0000-0000-000000000001").unwrap(),
        calendar_id: calendar.id,
        title: "Dentist".to_string(),
        location: "Clinic".to_string(),
        description: "Checkup".to_string(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 7, 21).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 22).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let updated_calendar = Calendar {
        name: "Home and Family".to_string(),
        color: "#d946ef".to_string(),
        visible: false,
        read_only: true,
        ..calendar.clone()
    };
    let unknown_calendar = Calendar {
        id: Uuid::parse_str("ca100002-0000-0000-0000-000000000002").unwrap(),
        ..updated_calendar.clone()
    };

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("opening a fresh sqlite database must succeed");
        repo.save_calendar(&calendar)
            .expect("saving the local calendar must succeed");
        repo.save_event(&event)
            .expect("saving the calendar event must succeed");

        repo.update_calendar(&updated_calendar)
            .expect("updating a saved calendar must succeed");
        assert!(
            repo.update_calendar(&unknown_calendar).is_err(),
            "updating an unknown calendar must fail rather than insert it"
        );
        assert!(
            repo.get_calendar(unknown_calendar.id).is_none(),
            "a failed update must not insert the unknown calendar"
        );
    }

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("reopening the sqlite database must succeed");
        assert_eq!(
            repo.get_calendar(calendar.id),
            Some(updated_calendar.clone()),
            "the calendar edit must persist with its ID and source unchanged"
        );
        assert_eq!(
            repo.get_event(event.id),
            Some(event.clone()),
            "updating a calendar must retain its events"
        );

        assert!(
            repo.delete_calendar(calendar.id),
            "deleting the saved calendar must report success"
        );
        assert!(repo.get_calendar(calendar.id).is_none());
        assert!(
            repo.get_event(event.id).is_none(),
            "deleting a calendar must cascade-delete its events"
        );
    }

    let repo = SqliteRepository::open(&db_path).expect("reopening after deletion must succeed");
    assert!(
        repo.get_calendar(calendar.id).is_none(),
        "the deleted calendar must remain absent after reopen"
    );
    assert!(
        repo.get_event(event.id).is_none(),
        "the cascade-deleted event must remain absent after reopen"
    );
}
