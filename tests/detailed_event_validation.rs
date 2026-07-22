// Public contract pinned by this acceptance test:
//
//     pub fn validate_event(candidate: Event) -> Result<Event, InvalidEvent>;
//
// The pure model-layer validation normalizes a nonempty title by trimming its
// surrounding whitespace, preserves every other event field for persistence,
// and rejects empty titles and non-forward schedules.

use calendar::model::{Event, EventSchedule, RecurrenceSpec, ReminderSpec, validate_event};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap();
    FixedOffset::east_opt(2 * 3600)
        .unwrap()
        .from_utc_datetime(&naive)
}

#[test]
fn phase9_detailed_event_validation() {
    let event_id = Uuid::parse_str("99999999-9999-9999-9999-999999999999").unwrap();
    let calendar_id = Uuid::parse_str("88888888-8888-8888-8888-888888888888").unwrap();
    let start_date = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
    let end_date = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();

    let detailed = Event {
        id: event_id,
        calendar_id,
        title: "  Planning session  ".to_string(),
        location: "Room 4B".to_string(),
        description: "Bring the roadmap.".to_string(),
        schedule: EventSchedule::AllDay {
            start_date,
            end_date_exclusive: end_date,
        },
        recurrence: Some(RecurrenceSpec::default()),
        reminders: vec![ReminderSpec {
            seconds_before_start: 600,
            description: "Join the meeting".to_owned(),
        }],
    };
    let normalized = validate_event(detailed).expect("a complete forward all-day event is valid");
    assert_eq!(normalized.id, event_id, "event id must remain available");
    assert_eq!(
        normalized.calendar_id, calendar_id,
        "calendar must remain available"
    );
    assert_eq!(
        normalized.title, "Planning session",
        "title must be trimmed"
    );
    assert_eq!(
        normalized.location, "Room 4B",
        "location must remain available"
    );
    assert_eq!(
        normalized.description, "Bring the roadmap.",
        "description must remain available"
    );
    assert!(
        normalized.recurrence.is_some(),
        "recurrence must remain available"
    );
    assert_eq!(
        normalized.reminders,
        vec![ReminderSpec {
            seconds_before_start: 600,
            description: "Join the meeting".to_owned(),
        }],
        "reminders must remain available"
    );
    assert_eq!(
        normalized.schedule,
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive: end_date,
        },
        "schedule must remain available"
    );

    assert!(
        validate_event(Event {
            title: " \t\n ".to_string(),
            ..normalized.clone()
        })
        .is_err(),
        "a whitespace-only title must be rejected"
    );
    assert!(
        validate_event(Event {
            schedule: EventSchedule::AllDay {
                start_date,
                end_date_exclusive: start_date,
            },
            ..normalized.clone()
        })
        .is_err(),
        "an all-day schedule must end after its start date"
    );

    let timed_start = at(2026, 7, 20, 9, 0);
    let timed_end = at(2026, 7, 20, 10, 0);
    assert!(
        validate_event(Event {
            schedule: EventSchedule::Timed {
                start: timed_end,
                end: timed_start,
                timezone: Some("Europe/Berlin".to_string()),
            },
            ..normalized.clone()
        })
        .is_err(),
        "a timed schedule must end after its start"
    );
    assert!(
        validate_event(Event {
            schedule: EventSchedule::Timed {
                start: timed_start,
                end: timed_end,
                timezone: Some("Europe/Berlin".to_string()),
            },
            ..normalized
        })
        .is_ok(),
        "a forward timed event must be accepted"
    );
}
