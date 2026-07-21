use calendar::backend::{
    AccountRepository, CalendarRepository, EventRepository, SqliteRepository, SyncStateRepository,
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

#[test]
fn phase11_caldav_sync_identity_survives_reopen_and_cascades() {
    // Public contract: CalDAV sync identity is durable metadata separate from Event.
    let db_path = unique_temp_db_path("sync_state");
    let _cleanup = TempDb(db_path.clone());

    let account = Account {
        id: Uuid::parse_str("ac110011-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let local_calendar = Calendar {
        id: Uuid::parse_str("ca110011-0000-0000-0000-000000000001").unwrap(),
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let remote_calendar = Calendar {
        id: Uuid::parse_str("ca110012-0000-0000-0000-000000000002").unwrap(),
        name: "Work".to_string(),
        color: "#d946ef".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };
    let local_event = Event {
        id: Uuid::parse_str("e1100111-0000-0000-0000-000000000001").unwrap(),
        calendar_id: local_calendar.id,
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
    let remote_event = Event {
        id: Uuid::parse_str("e1100122-0000-0000-0000-000000000002").unwrap(),
        calendar_id: remote_calendar.id,
        title: "Planning".to_string(),
        location: "Office".to_string(),
        description: "Quarterly planning".to_string(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 7, 23).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 24).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let calendar_state = CalendarSyncState {
        calendar_id: remote_calendar.id,
        remote_url: "https://caldav.example.test/dav/work/".to_string(),
        sync_token: None,
    };
    let event_state = EventSyncState {
        calendar_id: remote_calendar.id,
        event_id: remote_event.id,
        remote_href: "https://caldav.example.test/dav/work/planning.ics".to_string(),
        remote_uid: "planning-2026@example.test".to_string(),
        etag: None,
    };

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("opening a fresh sqlite database must succeed");
        repo.save_account(&account)
            .expect("saving the CalDAV account must succeed");
        repo.save_calendar(&local_calendar)
            .expect("saving the local calendar must succeed");
        repo.save_calendar(&remote_calendar)
            .expect("saving the remote calendar must succeed");
        repo.save_event(&local_event)
            .expect("saving the local event must succeed");
        repo.save_event(&remote_event)
            .expect("saving the mapped local event must succeed");
        repo.upsert_calendar_sync_state(&calendar_state)
            .expect("saving calendar sync identity must succeed");
        repo.upsert_event_sync_state(&event_state)
            .expect("saving event sync identity must succeed");
        assert_eq!(
            repo.get_event(remote_event.id),
            Some(remote_event.clone()),
            "sync metadata must remain separate from the app event"
        );
    }

    let updated_calendar_state = CalendarSyncState {
        sync_token: Some("sync-token-2".to_string()),
        ..calendar_state.clone()
    };
    let updated_event_state = EventSyncState {
        etag: Some("\"etag-2\"".to_string()),
        ..event_state.clone()
    };
    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("reopening the sqlite database must succeed");
        assert_eq!(
            repo.get_calendar_sync_state(remote_calendar.id),
            Some(calendar_state)
        );
        assert_eq!(
            repo.get_event_sync_state(remote_event.id),
            Some(event_state.clone())
        );
        assert_eq!(
            repo.find_event_sync_state_by_remote_href(remote_calendar.id, &event_state.remote_href),
            Some(event_state.clone())
        );
        assert_eq!(
            repo.list_event_sync_states(remote_calendar.id),
            vec![event_state.clone()],
            "event sync states must be deterministic"
        );

        repo.upsert_calendar_sync_state(&updated_calendar_state)
            .expect("updating the sync token must succeed");
        repo.upsert_event_sync_state(&updated_event_state)
            .expect("updating the ETag must succeed");
    }

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("reopening after sync updates must succeed");
        assert_eq!(
            repo.get_calendar_sync_state(remote_calendar.id),
            Some(updated_calendar_state.clone())
        );
        assert_eq!(
            repo.get_event_sync_state(remote_event.id),
            Some(updated_event_state.clone())
        );

        assert!(repo.delete_event(remote_event.id));
        assert!(repo.get_event_sync_state(remote_event.id).is_none());
        assert_eq!(
            repo.get_calendar_sync_state(remote_calendar.id),
            Some(updated_calendar_state)
        );

        assert!(repo.delete_calendar(remote_calendar.id));
        assert!(repo.get_calendar_sync_state(remote_calendar.id).is_none());
        assert_eq!(repo.get_calendar(local_calendar.id), Some(local_calendar));
        assert_eq!(repo.get_event(local_event.id), Some(local_event));
    }
}
