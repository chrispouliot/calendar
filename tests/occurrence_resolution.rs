use calendar::model::{
    DetachedEvent, Event, EventSchedule, RecurrenceId, RecurrenceSpec, ReminderSpec,
};
use calendar::month_view::event_for_recurrence_id;
use chrono::{DateTime, FixedOffset, NaiveDate};
use uuid::Uuid;

fn time(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

#[test]
fn resolves_generated_tzid_and_all_day_occurrences_with_detached_overrides() {
    let timed_master = Event {
        id: Uuid::parse_str("2b100001-0000-0000-0000-000000000001").unwrap(),
        calendar_id: Uuid::parse_str("2b100002-0000-0000-0000-000000000002").unwrap(),
        title: "DST standup".to_owned(),
        location: "Room 1".to_owned(),
        description: "Master description".to_owned(),
        schedule: EventSchedule::Timed {
            start: time("2026-03-02T09:00:00-05:00"),
            end: time("2026-03-02T10:30:00-05:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;COUNT=3".to_owned()],
            ..Default::default()
        }),
        reminders: vec![ReminderSpec {
            seconds_before_start: 15 * 60,
            description: "Master reminder".to_owned(),
        }],
    };
    let dst_identity = RecurrenceId::Timed {
        date_time: time("2026-03-09T09:00:00-04:00"),
        timezone: Some("America/New_York".to_owned()),
    };
    let modified_identity = RecurrenceId::Timed {
        date_time: time("2026-03-16T09:00:00-04:00"),
        timezone: Some("America/New_York".to_owned()),
    };
    let timed_detached = vec![DetachedEvent::Modified {
        recurrence_id: modified_identity.clone(),
        title: "Moved standup".to_owned(),
        location: "Zoom".to_owned(),
        description: "Override description".to_owned(),
        schedule: EventSchedule::Timed {
            start: time("2026-03-17T14:00:00-04:00"),
            end: time("2026-03-17T15:45:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        reminders: vec![ReminderSpec {
            seconds_before_start: 5 * 60,
            description: "Override reminder".to_owned(),
        }],
    }];

    let resolved_dst = event_for_recurrence_id(&timed_master, &timed_detached, &dst_identity);
    assert_eq!(
        resolved_dst,
        Some(Event {
            recurrence: None,
            schedule: EventSchedule::Timed {
                start: time("2026-03-09T09:00:00-04:00"),
                end: time("2026-03-09T10:30:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            ..timed_master.clone()
        }),
        "an unaffected TZID occurrence must retain master identity and fields, shift by local recurrence time across DST, and preserve its 90-minute duration"
    );
    assert_eq!(
        event_for_recurrence_id(&timed_master, &timed_detached, &modified_identity),
        Some(Event {
            id: timed_master.id,
            calendar_id: timed_master.calendar_id,
            title: "Moved standup".to_owned(),
            location: "Zoom".to_owned(),
            description: "Override description".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-03-17T14:00:00-04:00"),
                end: time("2026-03-17T15:45:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            recurrence: None,
            reminders: vec![ReminderSpec {
                seconds_before_start: 5 * 60,
                description: "Override reminder".to_owned(),
            }],
        }),
        "a modified identity must be the exact detached preview rather than a mutation of the master"
    );
    assert_eq!(
        event_for_recurrence_id(
            &timed_master,
            &timed_detached,
            &RecurrenceId::Timed {
                date_time: time("2026-03-23T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
        ),
        None,
        "a recurrence identity outside COUNT must not resolve"
    );
    assert_eq!(
        event_for_recurrence_id(
            &timed_master,
            &timed_detached,
            &RecurrenceId::AllDay(date(2026, 3, 9)),
        ),
        None,
        "an all-day identity cannot resolve against a timed master"
    );
    assert_eq!(
        event_for_recurrence_id(
            &timed_master,
            &timed_detached,
            &RecurrenceId::Timed {
                date_time: time("2026-03-09T09:00:00-04:00"),
                timezone: Some("UTC".to_owned()),
            },
        ),
        None,
        "a timed identity with a mismatched TZID cannot resolve"
    );

    let all_day_master = Event {
        id: Uuid::parse_str("2b100003-0000-0000-0000-000000000003").unwrap(),
        calendar_id: Uuid::parse_str("2b100004-0000-0000-0000-000000000004").unwrap(),
        title: "Two-day retreat".to_owned(),
        location: "Campus".to_owned(),
        description: "Master all-day description".to_owned(),
        schedule: EventSchedule::AllDay {
            start_date: date(2026, 6, 1),
            end_date_exclusive: date(2026, 6, 3),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=DAILY;COUNT=4".to_owned()],
            ..Default::default()
        }),
        reminders: vec![ReminderSpec {
            seconds_before_start: 24 * 60 * 60,
            description: "Pack".to_owned(),
        }],
    };
    let all_day_detached = vec![
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::AllDay(date(2026, 6, 3)),
            title: "Retreat moved".to_owned(),
            location: "Offsite".to_owned(),
            description: "Detached all-day description".to_owned(),
            schedule: EventSchedule::AllDay {
                start_date: date(2026, 6, 20),
                end_date_exclusive: date(2026, 6, 23),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 60 * 60,
                description: "Leave now".to_owned(),
            }],
        },
        DetachedEvent::Cancelled {
            recurrence_id: RecurrenceId::AllDay(date(2026, 6, 4)),
        },
    ];
    assert_eq!(
        event_for_recurrence_id(
            &all_day_master,
            &all_day_detached,
            &RecurrenceId::AllDay(date(2026, 6, 2)),
        ),
        Some(Event {
            recurrence: None,
            schedule: EventSchedule::AllDay {
                start_date: date(2026, 6, 2),
                end_date_exclusive: date(2026, 6, 4),
            },
            ..all_day_master.clone()
        }),
        "an unaffected all-day occurrence must retain master fields and its two-day exclusive-end duration"
    );
    assert_eq!(
        event_for_recurrence_id(
            &all_day_master,
            &all_day_detached,
            &RecurrenceId::AllDay(date(2026, 6, 3)),
        ),
        Some(Event {
            id: all_day_master.id,
            calendar_id: all_day_master.calendar_id,
            title: "Retreat moved".to_owned(),
            location: "Offsite".to_owned(),
            description: "Detached all-day description".to_owned(),
            schedule: EventSchedule::AllDay {
                start_date: date(2026, 6, 20),
                end_date_exclusive: date(2026, 6, 23),
            },
            recurrence: None,
            reminders: vec![ReminderSpec {
                seconds_before_start: 60 * 60,
                description: "Leave now".to_owned(),
            }],
        })
    );
    assert_eq!(
        event_for_recurrence_id(
            &all_day_master,
            &all_day_detached,
            &RecurrenceId::AllDay(date(2026, 6, 4)),
        ),
        None,
        "a cancelled generated all-day identity must not produce a preview event"
    );
}
