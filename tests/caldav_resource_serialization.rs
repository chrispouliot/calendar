// Public contract pinned by this acceptance test:
//
//     pub fn serialize_icalendar_resource(
//         master: &Event,
//         detached_events: &[DetachedEvent],
//         remote_uid: &str,
//     ) -> Result<String, EventSerializationError>;
//
// This is a pure serialization boundary: it neither contacts a CalDAV server
// nor reads or writes persistence state.

use calendar::backend::caldav::{
    EventSerializationError, map_icalendar_resource, serialize_icalendar_resource,
};
use calendar::model::{
    DetachedEvent, Event, EventSchedule, RecurrenceId, RecurrenceSpec, ReminderSpec,
};
use chrono::{DateTime, NaiveDate};
use uuid::Uuid;

const EVENT_ID: &str = "11111111-1111-1111-1111-111111111111";
const CALENDAR_ID: &str = "22222222-2222-2222-2222-222222222222";

fn ids() -> (Uuid, Uuid) {
    (
        Uuid::parse_str(EVENT_ID).unwrap(),
        Uuid::parse_str(CALENDAR_ID).unwrap(),
    )
}

fn serialization_error(
    master: &Event,
    detached_events: &[DetachedEvent],
) -> EventSerializationError {
    match serialize_icalendar_resource(master, detached_events, "weekly-planning") {
        Err(error) => error,
        Ok(_) => panic!("detached recurrence identity must be rejected"),
    }
}

#[test]
fn phase2a_serializes_a_recurring_resource_with_modified_and_cancelled_instances() {
    let (event_id, calendar_id) = ids();
    let master = Event {
        id: event_id,
        calendar_id,
        title: "Weekly planning".to_owned(),
        location: "Room A".to_owned(),
        description: "Plan the week".to_owned(),
        schedule: EventSchedule::Timed {
            start: DateTime::parse_from_rfc3339("2026-07-01T09:00:00+02:00").unwrap(),
            end: DateTime::parse_from_rfc3339("2026-07-01T10:00:00+02:00").unwrap(),
            timezone: Some("Europe/Berlin".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;COUNT=3".to_owned()],
            rdate: Vec::new(),
            exdate: Vec::new(),
        }),
        reminders: Vec::new(),
    };
    let detached_events = vec![
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: DateTime::parse_from_rfc3339("2026-07-08T09:00:00+02:00").unwrap(),
                timezone: Some("Europe/Berlin".to_owned()),
            },
            title: "Weekly planning moved".to_owned(),
            location: "Room B".to_owned(),
            description: "Bring the revised roadmap".to_owned(),
            schedule: EventSchedule::Timed {
                start: DateTime::parse_from_rfc3339("2026-07-08T11:00:00+01:00").unwrap(),
                end: DateTime::parse_from_rfc3339("2026-07-08T12:30:00+01:00").unwrap(),
                timezone: Some("Europe/London".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 30 * 60,
                description: "Prepare the roadmap".to_owned(),
            }],
        },
        DetachedEvent::Cancelled {
            recurrence_id: RecurrenceId::Timed {
                date_time: DateTime::parse_from_rfc3339("2026-07-15T09:00:00+02:00").unwrap(),
                timezone: Some("Europe/Berlin".to_owned()),
            },
        },
    ];

    let resource = serialize_icalendar_resource(&master, &detached_events, "weekly-planning")
        .expect("a recurring master and its detached instances must serialize as one resource");

    assert_eq!(resource.matches("BEGIN:VEVENT").count(), 3);
    assert_eq!(resource.matches("UID:weekly-planning").count(), 3);
    assert!(resource.contains("RECURRENCE-ID;TZID=Europe/Berlin:20260708T090000"));
    assert!(resource.contains("DTSTART;TZID=Europe/London:20260708T110000"));
    assert!(resource.contains("DTEND;TZID=Europe/London:20260708T123000"));
    assert!(resource.contains("STATUS:CANCELLED"));
    assert!(resource.contains("RECURRENCE-ID;TZID=Europe/Berlin:20260715T090000"));
    assert!(resource.contains("DTSTART;TZID=Europe/Berlin:20260715T090000"));
    assert!(resource.contains("DTEND;TZID=Europe/Berlin:20260715T100000"));

    let mapped = map_icalendar_resource(&resource, event_id, calendar_id)
        .expect("the serialized VCALENDAR must map back to its master and detached instances");
    assert_eq!(mapped.master.remote_uid, "weekly-planning");
    assert_eq!(mapped.master.event, master);
    assert_eq!(mapped.exceptions, detached_events);

    let kind_mismatch = [DetachedEvent::Modified {
        recurrence_id: RecurrenceId::AllDay(NaiveDate::from_ymd_opt(2026, 7, 8).unwrap()),
        title: "Weekly planning moved".to_owned(),
        location: "Room B".to_owned(),
        description: "Bring the revised roadmap".to_owned(),
        schedule: EventSchedule::Timed {
            start: DateTime::parse_from_rfc3339("2026-07-08T11:00:00+01:00").unwrap(),
            end: DateTime::parse_from_rfc3339("2026-07-08T12:30:00+01:00").unwrap(),
            timezone: Some("Europe/London".to_owned()),
        },
        reminders: vec![ReminderSpec {
            seconds_before_start: 30 * 60,
            description: "Prepare the roadmap".to_owned(),
        }],
    }];
    assert_eq!(
        serialization_error(&master, &kind_mismatch),
        EventSerializationError::InvalidSchedule,
        "a modified timed occurrence must retain the timed identity of its master"
    );

    let timezone_mismatch = [DetachedEvent::Cancelled {
        recurrence_id: RecurrenceId::Timed {
            date_time: DateTime::parse_from_rfc3339("2026-07-15T09:00:00+01:00").unwrap(),
            timezone: Some("Europe/London".to_owned()),
        },
    }];
    assert_eq!(
        serialization_error(&master, &timezone_mismatch),
        EventSerializationError::InvalidSchedule,
        "a cancelled occurrence must retain the master's TZID for its recurrence identity"
    );
}
