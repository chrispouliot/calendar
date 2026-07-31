use calendar::backend::reminders::reminder_occurrences_with_detached_in_window;
use calendar::model::{
    DetachedEvent, Event, EventSchedule, RecurrenceId, RecurrenceSpec, ReminderSpec,
};
use chrono::{DateTime, FixedOffset};
use uuid::Uuid;

fn time(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

#[test]
fn detached_instances_replace_master_reminders_and_schedule_only_generated_identities() {
    let master = Event {
        id: Uuid::parse_str("2b000002-0000-0000-0000-000000000002").unwrap(),
        calendar_id: Uuid::parse_str("2b000001-0000-0000-0000-000000000001").unwrap(),
        title: "Daily standup".to_owned(),
        location: "Room 1".to_owned(),
        description: "Team sync".to_owned(),
        schedule: EventSchedule::Timed {
            start: time("2026-07-06T09:00:00-04:00"),
            end: time("2026-07-06T09:30:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["FREQ=DAILY;COUNT=6".to_owned()],
            ..Default::default()
        }),
        reminders: vec![ReminderSpec {
            seconds_before_start: 10 * 60,
            description: "Master reminder".to_owned(),
        }],
    };
    let detached_events = vec![
        // Its original recurrence trigger is before the window, but its moved
        // schedule and own reminder must be included inside the window.
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-06T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            title: "Moved into window".to_owned(),
            location: "Zoom".to_owned(),
            description: "Remote standup".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-07-08T11:00:00-04:00"),
                end: time("2026-07-08T11:30:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 15 * 60,
                description: "Join Zoom".to_owned(),
            }],
        },
        DetachedEvent::Cancelled {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-08T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
        },
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-09T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            title: "Rescheduled".to_owned(),
            location: "Room 2".to_owned(),
            description: "Release review".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-07-10T10:00:00-04:00"),
                end: time("2026-07-10T11:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 10 * 60,
                description: "Prepare release notes".to_owned(),
            }],
        },
        // The July 10 generated master reminder must not remain after this
        // occurrence is moved beyond the window.
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-10T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            title: "Moved out".to_owned(),
            location: "Room 3".to_owned(),
            description: "Later standup".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-07-12T09:00:00-04:00"),
                end: time("2026-07-12T09:30:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 10 * 60,
                description: "Later reminder".to_owned(),
            }],
        },
        // An override for an identity the master never generates must not add
        // a reminder merely because its replacement schedule is in the window.
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-20T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            title: "Invalid identity".to_owned(),
            location: "Nowhere".to_owned(),
            description: "Not an instance".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-07-09T11:00:00-04:00"),
                end: time("2026-07-09T11:30:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 10 * 60,
                description: "Must not fire".to_owned(),
            }],
        },
    ];

    let reminders = reminder_occurrences_with_detached_in_window(
        &master,
        &detached_events,
        time("2026-07-07T08:00:00-04:00"),
        time("2026-07-10T10:00:00-04:00"),
    );

    assert_eq!(
        reminders
            .iter()
            .map(|reminder| (
                reminder.event_id,
                reminder.occurrence_start,
                reminder.trigger_at,
                reminder.title.as_str(),
                reminder.description.as_str(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                master.id,
                time("2026-07-07T09:00:00-04:00"),
                time("2026-07-07T08:50:00-04:00"),
                "Daily standup",
                "Master reminder",
            ),
            (
                master.id,
                time("2026-07-08T11:00:00-04:00"),
                time("2026-07-08T10:45:00-04:00"),
                "Moved into window",
                "Join Zoom",
            ),
            (
                master.id,
                time("2026-07-10T10:00:00-04:00"),
                time("2026-07-10T09:50:00-04:00"),
                "Rescheduled",
                "Prepare release notes",
            ),
        ],
        "cancelled, moved, and non-generated recurrence identities must replace rather than supplement master reminders",
    );
}
