use calendar::backend::caldav::{
    EventMappingError, map_icalendar_event, serialize_icalendar_event,
};
use calendar::backend::{CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{Calendar, CalendarSource};
use calendar::month_view::{AgendaGroup, project_agenda_range};
use chrono::NaiveDate;
use std::path::PathBuf;
use uuid::Uuid;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn unique_temp_db_path() -> PathBuf {
    std::env::temp_dir().join(format!("calendar_recurrence_{}.sqlite", Uuid::new_v4()))
}

fn property_value<'a>(resource: &'a str, name: &str) -> Option<&'a str> {
    resource.lines().find_map(|line| {
        let (property, value) = line.split_once(':')?;
        property.starts_with(name).then_some(value)
    })
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

#[test]
fn weekly_recurrence_round_trips_through_caldav_sqlite_and_agenda_projection() {
    let event_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let calendar_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let resource = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:weekly-holiday\r\nSUMMARY:Weekly holiday\r\nDTSTART;VALUE=DATE:20260706\r\nDTEND;VALUE=DATE:20260708\r\nRRULE:FREQ=WEEKLY;COUNT=4\r\nEXDATE;VALUE=DATE:20260713\r\nRDATE;VALUE=DATE:20260716\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    let mapped = map_icalendar_event(resource, event_id, calendar_id)
        .expect("a weekly master with EXDATE and RDATE must map");
    assert_eq!(mapped.remote_uid, "weekly-holiday");
    assert!(matches!(
        map_icalendar_event(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:detached-instance\r\nSUMMARY:Detached instance\r\nDTSTART;VALUE=DATE:20260713\r\nRECURRENCE-ID;VALUE=DATE:20260713\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            event_id,
            calendar_id,
        ),
        Err(EventMappingError::UnsupportedRecurrence)
    ));

    let db_path = unique_temp_db_path();
    let _cleanup = TempDb(db_path.clone());
    {
        let mut repository =
            SqliteRepository::open(&db_path).expect("temporary database must open");
        repository
            .save_calendar(&Calendar {
                id: calendar_id,
                name: "Holidays".to_owned(),
                color: "#3366cc".to_owned(),
                visible: true,
                read_only: false,
                source: CalendarSource::Local,
            })
            .expect("calendar must persist");
        repository
            .save_event(&mapped.event)
            .expect("recurring master must persist");
    }

    let repository = SqliteRepository::open(&db_path).expect("database must reopen");
    let reloaded = repository
        .get_event(event_id)
        .expect("recurring master must reload");
    assert_eq!(
        reloaded, mapped.event,
        "recurrence must round-trip without loss"
    );

    let calendars = repository.list_calendars();
    let events = repository.list_events_for_calendar(calendar_id);
    let agenda = project_agenda_range(date(2026, 7, 6), date(2026, 8, 3), &calendars, &events);
    let occurrence_days: Vec<_> = agenda
        .iter()
        .filter_map(|group| match group {
            AgendaGroup::EventDay(day) if !day.all_day.is_empty() => Some((day.date, &day.all_day)),
            _ => None,
        })
        .collect();
    assert_eq!(
        occurrence_days
            .iter()
            .map(|(day, _)| *day)
            .collect::<Vec<_>>(),
        vec![
            date(2026, 7, 6),
            date(2026, 7, 7),
            date(2026, 7, 16),
            date(2026, 7, 17),
            date(2026, 7, 20),
            date(2026, 7, 21),
            date(2026, 7, 27),
            date(2026, 7, 28),
        ],
        "weekly occurrences must retain their two-day duration, omit EXDATE, and include RDATE"
    );
    assert!(occurrence_days.iter().all(|(_, chips)| {
        chips.iter().all(|chip| {
            chip.title == "Weekly holiday" && chip.calendar_id == calendar_id && chip.is_all_day
        })
    }));

    let serialized = serialize_icalendar_event(&reloaded, &mapped.remote_uid)
        .expect("reloaded recurring master must serialize");
    let rrule = property_value(&serialized, "RRULE").expect("serialized event must have RRULE");
    assert!(rrule.split(';').any(|part| part == "FREQ=WEEKLY"));
    assert!(rrule.split(';').any(|part| part == "COUNT=4"));
    assert_eq!(
        property_value(&serialized, "EXDATE"),
        Some("20260713"),
        "serialized event must retain EXDATE"
    );
    assert_eq!(
        property_value(&serialized, "RDATE"),
        Some("20260716"),
        "serialized event must retain RDATE"
    );
}
