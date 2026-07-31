// Public contract pinned by this acceptance test:
//
//     pub mod backend::caldav {
//         pub struct MappedResource {
//             pub master: MappedEvent,
//             pub exceptions: Vec<DetachedEvent>,
//         }
//
//         pub enum DetachedEvent {
//             Modified {
//                 recurrence_id: RecurrenceId,
//                 title: String,
//                 location: String,
//                 description: String,
//                 schedule: EventSchedule,
//                 reminders: Vec<ReminderSpec>,
//             },
//             Cancelled { recurrence_id: RecurrenceId },
//         }
//
//         pub enum RecurrenceId {
//             AllDay(NaiveDate),
//             Timed { date_time: DateTime<FixedOffset>, timezone: Option<String> },
//         }
//
//         pub enum ResourceMappingError {
//             MixedUids,
//             DuplicateMasters,
//             OrphanException,
//             // additional parse/unsupported-data variants may exist
//         }
//
//         pub fn map_icalendar_resource(
//             resource: &str,
//             event_id: Uuid,
//             calendar_id: Uuid,
//         ) -> Result<MappedResource, ResourceMappingError>;
//     }
//
// This is a pure mapping boundary: it neither contacts a CalDAV server nor
// reads or writes persistence state.

use calendar::backend::caldav::{
    DetachedEvent, RecurrenceId, ResourceMappingError, map_icalendar_event, map_icalendar_resource,
};
use calendar::model::{EventSchedule, RecurrenceSpec, ReminderSpec};
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

fn resource(events: &str) -> String {
    format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{events}END:VCALENDAR\r\n")
}

fn resource_error(resource: &str, event_id: Uuid, calendar_id: Uuid) -> ResourceMappingError {
    match map_icalendar_resource(resource, event_id, calendar_id) {
        Err(error) => error,
        Ok(_) => panic!("resource must be rejected"),
    }
}

#[test]
fn phase2a_maps_a_recurring_resource_with_detached_instances() {
    let (event_id, calendar_id) = ids();
    let mapped = map_icalendar_resource(
        &resource(
            "BEGIN:VEVENT\r\nUID:weekly-berlin\r\nSUMMARY:Team meeting\r\nDTSTART;TZID=Europe/Berlin:20260701T090000\r\nDTEND;TZID=Europe/Berlin:20260701T100000\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-berlin\r\nRECURRENCE-ID;TZID=Europe/Berlin:20260708T090000\r\nSUMMARY:Team meeting moved\r\nLOCATION:Room B\r\nDESCRIPTION:Bring the draft\r\nDTSTART;TZID=Europe/Berlin:20260708T110000\r\nDTEND;TZID=Europe/Berlin:20260708T120000\r\nRDATE;TZID=Europe/Berlin:20260722T090000\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT30M\r\nDESCRIPTION:Prepare deck\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:weekly-berlin\r\nRECURRENCE-ID;TZID=Europe/Berlin:20260715T090000\r\nSTATUS:CANCELLED\r\nDTSTART;TZID=Europe/Berlin:20260715T090000\r\nDTEND;TZID=Europe/Berlin:20260715T100000\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("a recurring master and its detached instances belong to one resource");
    assert_eq!(mapped.master.remote_uid, "weekly-berlin");
    assert_eq!(mapped.master.event.id, event_id);
    assert_eq!(mapped.master.event.calendar_id, calendar_id);
    assert_eq!(
        mapped.master.event.recurrence,
        Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;COUNT=3".to_owned()],
            rdate: Vec::new(),
            exdate: Vec::new(),
        }),
        "recurrence properties on detached VEVENTs must not alter the master recurrence"
    );
    assert_eq!(mapped.exceptions.len(), 2);
    assert!(matches!(
        &mapped.exceptions[0],
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed { date_time, timezone },
            title,
            location,
            description,
            schedule: EventSchedule::Timed { start, end, timezone: schedule_timezone },
            reminders,
        } if *date_time == DateTime::parse_from_rfc3339("2026-07-08T09:00:00+02:00").unwrap()
            && timezone.as_deref() == Some("Europe/Berlin")
            && title == "Team meeting moved"
            && location == "Room B"
            && description == "Bring the draft"
            && start == &DateTime::parse_from_rfc3339("2026-07-08T11:00:00+02:00").unwrap()
            && end == &DateTime::parse_from_rfc3339("2026-07-08T12:00:00+02:00").unwrap()
            && schedule_timezone.as_deref() == Some("Europe/Berlin")
            && reminders == &vec![ReminderSpec {
                seconds_before_start: 30 * 60,
                description: "Prepare deck".to_owned(),
            }]
    ));
    assert!(matches!(
        &mapped.exceptions[1],
        DetachedEvent::Cancelled {
            recurrence_id: RecurrenceId::Timed { date_time, timezone },
        } if *date_time == DateTime::parse_from_rfc3339("2026-07-15T09:00:00+02:00").unwrap()
            && timezone.as_deref() == Some("Europe/Berlin")
    ));

    let all_day = map_icalendar_resource(
        &resource(
            "BEGIN:VEVENT\r\nUID:daily-holiday\r\nSUMMARY:Holiday\r\nDTSTART;VALUE=DATE:20260714\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:daily-holiday\r\nRECURRENCE-ID;VALUE=DATE:20260715\r\nSTATUS:CANCELLED\r\nDTSTART;VALUE=DATE:20260715\r\nEND:VEVENT\r\n",
        ),
        event_id,
        calendar_id,
    )
    .expect("all-day recurrence identities must remain dates, not midnight timed values");
    assert!(matches!(
        &all_day.exceptions[0],
        DetachedEvent::Cancelled {
            recurrence_id: RecurrenceId::AllDay(date),
        } if *date == NaiveDate::from_ymd_opt(2026, 7, 15).unwrap()
    ));

    assert!(matches!(
        resource_error(
            &resource(
                "BEGIN:VEVENT\r\nUID:one\r\nDTSTART;VALUE=DATE:20260714\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:two\r\nRECURRENCE-ID;VALUE=DATE:20260715\r\nDTSTART;VALUE=DATE:20260715\r\nEND:VEVENT\r\n"
            ),
            event_id,
            calendar_id,
        ),
        ResourceMappingError::MixedUids
    ));
    assert!(matches!(
        resource_error(
            &resource(
                "BEGIN:VEVENT\r\nUID:duplicate\r\nDTSTART;VALUE=DATE:20260714\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:duplicate\r\nDTSTART;VALUE=DATE:20260715\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n"
            ),
            event_id,
            calendar_id,
        ),
        ResourceMappingError::DuplicateMasters
    ));
    assert!(matches!(
        resource_error(
            &resource(
                "BEGIN:VEVENT\r\nUID:orphan\r\nRECURRENCE-ID;VALUE=DATE:20260715\r\nDTSTART;VALUE=DATE:20260715\r\nEND:VEVENT\r\n"
            ),
            event_id,
            calendar_id
        ),
        ResourceMappingError::OrphanException
    ));

    let ordinary = map_icalendar_event(
        &resource("BEGIN:VEVENT\r\nUID:ordinary\r\nSUMMARY:One event\r\nDTSTART;VALUE=DATE:20260714\r\nEND:VEVENT\r\n"),
        event_id,
        calendar_id,
    )
    .expect("the existing single-VEVENT mapper remains available for ordinary resources");
    assert_eq!(ordinary.remote_uid, "ordinary");
}
