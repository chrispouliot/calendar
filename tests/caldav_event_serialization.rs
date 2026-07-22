// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub enum EventSerializationError {
//             EmptyUid,
//             InvalidSchedule,
//             UnsupportedRecurrence,
//             UnsupportedReminders,
//             // additional serialization-failure variants may exist
//         }
//
//         pub fn serialize_icalendar_event(
//             event: &Event,
//             remote_uid: &str,
//         ) -> Result<String, EventSerializationError>;
//     }
//
// This is a pure serialization boundary: it neither contacts a CalDAV server
// nor reads or writes persistence state or the clock.

use calendar::backend::caldav::{
    EventSerializationError, map_icalendar_event, serialize_icalendar_event,
};
use calendar::model::{Event, EventSchedule, RecurrenceSpec, ReminderSpec};
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;

const EVENT_ID: &str = "11111111-1111-1111-1111-111111111111";
const CALENDAR_ID: &str = "22222222-2222-2222-2222-222222222222";

fn ids() -> (Uuid, Uuid) {
    (
        Uuid::parse_str(EVENT_ID).unwrap(),
        Uuid::parse_str(CALENDAR_ID).unwrap(),
    )
}

fn event(schedule: EventSchedule) -> Event {
    let (id, calendar_id) = ids();
    Event {
        id,
        calendar_id,
        title: "Planning, review; notes\\draft\nnext line".to_owned(),
        location: "Room, A; north\\wing\nlevel two".to_owned(),
        description: "Agenda, decisions; follow-up\\owner\nsecond line".to_owned(),
        schedule,
        recurrence: None,
        reminders: Vec::new(),
    }
}

fn serialization_error(event: &Event, remote_uid: &str) -> EventSerializationError {
    match serialize_icalendar_event(event, remote_uid) {
        Err(error) => error,
        Ok(_) => panic!("event must be rejected"),
    }
}

#[test]
fn phase11_serializes_safe_events_as_one_parseable_icalendar_resource() {
    let (event_id, calendar_id) = ids();
    let cases = [
        (
            "all-day",
            event(EventSchedule::AllDay {
                start_date: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
                end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 17).unwrap(),
            }),
        ),
        (
            "utc-timed",
            event(EventSchedule::Timed {
                start: DateTime::parse_from_rfc3339("2026-01-15T09:30:00+00:00").unwrap(),
                end: DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00").unwrap(),
                timezone: None,
            }),
        ),
        (
            "berlin-timed",
            event(EventSchedule::Timed {
                start: DateTime::parse_from_rfc3339("2026-07-01T09:30:00+02:00").unwrap(),
                end: DateTime::parse_from_rfc3339("2026-07-01T10:30:00+02:00").unwrap(),
                timezone: Some("Europe/Berlin".to_owned()),
            }),
        ),
    ];

    for (remote_uid, source) in cases {
        let resource = serialize_icalendar_event(&source, remote_uid)
            .expect("supported local event must serialize without losing data");
        let mapped = map_icalendar_event(&resource, event_id, calendar_id).expect(
            "serialized resource must be one standards-parseable VCALENDAR with one VEVENT",
        );

        assert_eq!(mapped.remote_uid, remote_uid);
        assert_eq!(mapped.event, source);
    }

    let valid = event(EventSchedule::AllDay {
        start_date: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
        end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
    });
    assert!(matches!(
        serialization_error(&valid, ""),
        EventSerializationError::EmptyUid
    ));

    let invalid = event(EventSchedule::AllDay {
        start_date: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
        end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
    });
    assert!(matches!(
        serialization_error(&invalid, "invalid-schedule"),
        EventSerializationError::InvalidSchedule
    ));

    let mut recurring = valid.clone();
    recurring.recurrence = Some(RecurrenceSpec::default());
    assert!(matches!(
        serialization_error(&recurring, "recurring"),
        EventSerializationError::UnsupportedRecurrence
    ));

    let mut reminded = valid;
    reminded.reminders.push(ReminderSpec {
        seconds_before_start: 0,
        description: String::new(),
    });
    assert!(matches!(
        serialization_error(&reminded, "reminded"),
        EventSerializationError::UnsupportedReminders
    ));
}

#[test]
fn serializes_offset_timed_events_without_a_tzid_as_utc() {
    let (event_id, calendar_id) = ids();
    let start = DateTime::parse_from_rfc3339("2026-01-15T09:30:00+02:00").unwrap();
    let end = DateTime::parse_from_rfc3339("2026-01-15T10:45:00+02:00").unwrap();
    let source = event(EventSchedule::Timed {
        start,
        end,
        timezone: None,
    });

    let resource = serialize_icalendar_event(&source, "local-offset")
        .expect("local offset times without a TZID must serialize as UTC");
    assert!(resource.contains("DTSTART:20260115T073000Z"));
    assert!(resource.contains("DTEND:20260115T084500Z"));

    let mapped = map_icalendar_event(&resource, event_id, calendar_id)
        .expect("serialized UTC times must map back to a timed event");
    let EventSchedule::Timed {
        start: mapped_start,
        end: mapped_end,
        timezone,
    } = mapped.event.schedule
    else {
        panic!("serialized timed event must map to a timed schedule");
    };
    assert_eq!(timezone, None);
    assert_eq!(mapped_start.with_timezone(&Utc), start.with_timezone(&Utc));
    assert_eq!(mapped_end.with_timezone(&Utc), end.with_timezone(&Utc));
    assert_eq!(mapped_end - mapped_start, end - start);
}
