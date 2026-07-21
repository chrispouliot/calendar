// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct MappedEvent {
//             pub event: Event,
//             pub remote_uid: String,
//         }
//
//         pub enum EventMappingError {
//             MissingUid,
//             MissingDtstart,
//             FloatingTime,
//             UnsupportedRecurrence,
//             MultipleEvents,
//             // additional parse/unsupported-data variants may exist
//         }
//
//         pub fn map_icalendar_event(
//             resource: &str,
//             event_id: Uuid,
//             calendar_id: Uuid,
//         ) -> Result<MappedEvent, EventMappingError>;
//     }
//
// This is a pure mapping boundary: it neither contacts a CalDAV server nor
// reads or writes persistence state.

use calendar::backend::caldav::{EventMappingError, map_icalendar_event};
use calendar::model::EventSchedule;
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

fn resource(event: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{event}END:VCALENDAR\r\n")
}

fn mapping_error(resource: &str, event_id: Uuid, calendar_id: Uuid) -> EventMappingError {
    match map_icalendar_event(resource, event_id, calendar_id) {
        Err(error) => error,
        Ok(_) => panic!("resource must be rejected"),
    }
}

#[test]
fn phase11_maps_safe_icalendar_event_subset_and_rejects_unsupported_resources() {
    let (event_id, calendar_id) = ids();

    let one_day = map_icalendar_event(
        &resource(
            "BEGIN:VEVENT\r\nUID:all-day-one\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20260714\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("a one-day all-day event is in the supported mapping subset");
    assert_eq!(one_day.remote_uid, "all-day-one");
    assert_eq!(one_day.event.id, event_id);
    assert_eq!(one_day.event.calendar_id, calendar_id);
    assert_eq!(one_day.event.title, "Holiday");
    assert_eq!(one_day.event.location, "");
    assert_eq!(one_day.event.description, "");
    assert!(one_day.event.recurrence.is_none());
    assert!(one_day.event.reminders.is_empty());
    assert_eq!(
        one_day.event.schedule,
        EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 7, 14).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 15).unwrap(),
        }
    );

    let multi_day = map_icalendar_event(
        &resource(
            "BEGIN:VEVENT\r\nUID:all-day-many\r\nSUMMARY:Conference\r\nLOCATION:Auditorium\r\nDESCRIPTION:Three day event\r\nDTSTART;VALUE=DATE:20261005\r\nDTEND;VALUE=DATE:20261008\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("an all-day event with DATE DTEND is in the supported mapping subset");
    assert_eq!(multi_day.remote_uid, "all-day-many");
    assert_eq!(multi_day.event.title, "Conference");
    assert_eq!(multi_day.event.location, "Auditorium");
    assert_eq!(multi_day.event.description, "Three day event");
    assert_eq!(
        multi_day.event.schedule,
        EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 10, 8).unwrap(),
        }
    );

    let utc = map_icalendar_event(
        &resource(
            "BEGIN:VEVENT\r\nUID:utc-timed\r\nSUMMARY:Remote call\r\nDTSTART:20260115T093000Z\r\nDTEND:20260115T103000Z\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("a UTC timed event is in the supported mapping subset");
    assert_eq!(utc.remote_uid, "utc-timed");
    assert_eq!(utc.event.location, "");
    assert_eq!(utc.event.description, "");
    match utc.event.schedule {
        EventSchedule::Timed {
            start,
            end,
            timezone,
        } => {
            assert_eq!(
                start,
                DateTime::parse_from_rfc3339("2026-01-15T09:30:00+00:00").unwrap()
            );
            assert_eq!(
                end,
                DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00").unwrap()
            );
            assert_eq!(timezone, None);
        }
        other => panic!("UTC DTSTART must map to a timed schedule, got {other:?}"),
    }

    let berlin = map_icalendar_event(
        &resource(
            "BEGIN:VEVENT\r\nUID:berlin-timed\r\nSUMMARY:Summer meeting\r\nLOCATION:Berlin\r\nDESCRIPTION:Bring notes\r\nDTSTART;TZID=Europe/Berlin:20260701T093000\r\nDTEND;TZID=Europe/Berlin:20260701T103000\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("a Europe/Berlin timed event is in the supported mapping subset");
    assert_eq!(berlin.remote_uid, "berlin-timed");
    assert_eq!(berlin.event.id, event_id);
    assert_eq!(berlin.event.calendar_id, calendar_id);
    assert_eq!(berlin.event.title, "Summer meeting");
    assert_eq!(berlin.event.location, "Berlin");
    assert_eq!(berlin.event.description, "Bring notes");
    assert!(berlin.event.recurrence.is_none());
    assert!(berlin.event.reminders.is_empty());
    match berlin.event.schedule {
        EventSchedule::Timed {
            start,
            end,
            timezone,
        } => {
            assert_eq!(
                start,
                DateTime::parse_from_rfc3339("2026-07-01T09:30:00+02:00").unwrap()
            );
            assert_eq!(
                end,
                DateTime::parse_from_rfc3339("2026-07-01T10:30:00+02:00").unwrap()
            );
            assert_eq!(timezone.as_deref(), Some("Europe/Berlin"));
        }
        other => panic!("TZID DTSTART must map to a timed schedule, got {other:?}"),
    }

    let missing_uid = mapping_error(
        &resource(
            "BEGIN:VEVENT\r\nSUMMARY:No UID\r\nDTSTART;VALUE=DATE:20260714\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    );
    assert!(matches!(missing_uid, EventMappingError::MissingUid));

    let missing_dtstart = mapping_error(
        &resource("BEGIN:VEVENT\r\nUID:no-start\r\nSUMMARY:No start\r\nEND:VEVENT\r\n"),
        event_id,
        calendar_id,
    );
    assert!(matches!(missing_dtstart, EventMappingError::MissingDtstart));

    let floating = mapping_error(
        &resource(
            "BEGIN:VEVENT\r\nUID:floating\r\nSUMMARY:Floating\r\nDTSTART:20260701T093000\r\nDTEND:20260701T103000\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    );
    assert!(matches!(floating, EventMappingError::FloatingTime));

    for unsupported in [
        resource(
            "BEGIN:VEVENT\r\nUID:recurring\r\nSUMMARY:Recurring\r\nDTSTART;VALUE=DATE:20260714\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n",
        ),
        resource(
            "BEGIN:VEVENT\r\nUID:instance\r\nSUMMARY:Instance\r\nDTSTART;VALUE=DATE:20260714\r\nRECURRENCE-ID;VALUE=DATE:20260714\r\nEND:VEVENT\r\n",
        ),
    ] {
        let error = mapping_error(&unsupported, event_id, calendar_id);
        assert!(matches!(error, EventMappingError::UnsupportedRecurrence));
    }

    let multiple_events = mapping_error(
        &resource(
            "BEGIN:VEVENT\r\nUID:first\r\nSUMMARY:First\r\nDTSTART;VALUE=DATE:20260714\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:second\r\nSUMMARY:Second\r\nDTSTART;VALUE=DATE:20260715\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    );
    assert!(matches!(multiple_events, EventMappingError::MultipleEvents));
}
