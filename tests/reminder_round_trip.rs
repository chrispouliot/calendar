use calendar::backend::caldav::{
    EventMappingError, map_icalendar_event, serialize_icalendar_event,
};
use calendar::backend::{CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{Calendar, CalendarSource, ReminderSpec};
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_db_path() -> PathBuf {
    std::env::temp_dir().join(format!("calendar_reminder_{}.sqlite", Uuid::new_v4()))
}

struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let path = PathBuf::from(format!("{}{suffix}", self.0.display()));
            let _ = std::fs::remove_file(path);
        }
    }
}

fn resource(alarm: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:planning\r\nSUMMARY:Planning\r\nDTSTART:20260701T090000Z\r\nDTEND:20260701T100000Z\r\n{alarm}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[test]
fn display_reminders_round_trip_through_caldav_and_sqlite_and_unsupported_alarms_fail_mapping() {
    let event_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let calendar_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let reminder = ReminderSpec {
        seconds_before_start: 600,
        description: "Join the video call".to_owned(),
    };
    let mapped = map_icalendar_event(
        &resource(
            "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=START:-PT10M\r\nDESCRIPTION:Join the video call\r\nEND:VALARM\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("a relative DISPLAY alarm must map");
    assert_eq!(mapped.event.reminders, vec![reminder.clone()]);

    let db_path = unique_temp_db_path();
    let _cleanup = TempDb(db_path.clone());
    {
        let mut repository =
            SqliteRepository::open(&db_path).expect("temporary database must open");
        repository
            .save_calendar(&Calendar {
                id: calendar_id,
                name: "Work".to_owned(),
                color: "#3366cc".to_owned(),
                visible: true,
                read_only: false,
                source: CalendarSource::Local,
            })
            .expect("calendar must persist");
        repository
            .save_event(&mapped.event)
            .expect("reminded event must persist");
    }

    let repository = SqliteRepository::open(&db_path).expect("database must reopen");
    let reloaded = repository
        .get_event(event_id)
        .expect("reminded event must reload");
    assert_eq!(
        reloaded, mapped.event,
        "reminder must round-trip through SQLite"
    );

    let serialized = serialize_icalendar_event(&reloaded, &mapped.remote_uid)
        .expect("reloaded reminder must serialize");
    let remapped = map_icalendar_event(&serialized, event_id, calendar_id)
        .expect("serialized reminder must map");
    assert_eq!(remapped.event.reminders, vec![reminder]);

    for unsupported_alarm in [
        "BEGIN:VALARM\r\nACTION:AUDIO\r\nTRIGGER;RELATED=START:-PT10M\r\nEND:VALARM\r\n",
        "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:20260701T085000Z\r\nDESCRIPTION:Absolute\r\nEND:VALARM\r\n",
        "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=START:-PT10M\r\nREPEAT:2\r\nDURATION:PT5M\r\nDESCRIPTION:Repeating\r\nEND:VALARM\r\n",
    ] {
        assert!(matches!(
            map_icalendar_event(&resource(unsupported_alarm), event_id, calendar_id),
            Err(EventMappingError::UnsupportedData(_))
        ));
    }
}

#[test]
fn after_start_display_reminders_round_trip_as_positive_triggers() {
    let event_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let calendar_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let all_day = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:holiday\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20260701\r\nDTEND;VALUE=DATE:20260702\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=START:PT9H\r\nDESCRIPTION:Morning of\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    let mapped = map_icalendar_event(all_day, event_id, calendar_id)
        .expect("a positive trigger is an alarm after the start and must map");
    assert_eq!(
        mapped.event.reminders,
        vec![ReminderSpec {
            seconds_before_start: -9 * 60 * 60,
            description: "Morning of".to_owned(),
        }]
    );

    let serialized = serialize_icalendar_event(&mapped.event, &mapped.remote_uid)
        .expect("an after-start reminder must serialize");
    assert!(
        serialized.contains("TRIGGER;RELATED=START:PT32400S"),
        "an after-start reminder must serialize as a positive duration, got:\n{serialized}"
    );
    let remapped = map_icalendar_event(&serialized, event_id, calendar_id)
        .expect("serialized after-start reminder must map");
    assert_eq!(remapped.event.reminders, mapped.event.reminders);

    let at_start = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:holiday\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20260701\r\nDTEND;VALUE=DATE:20260702\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:PT0S\r\nDESCRIPTION:At start\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    assert!(
        matches!(
            map_icalendar_event(at_start, event_id, calendar_id),
            Err(EventMappingError::UnsupportedData(_))
        ),
        "a zero trigger still cannot be represented"
    );
}
