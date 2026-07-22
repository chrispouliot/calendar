use calendar::backend::reminders::reminder_occurrences_in_window;
use calendar::model::{Event, EventSchedule, RecurrenceSpec, ReminderSpec};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(0)
        .unwrap()
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, minute, 0)
                .unwrap(),
        )
        .single()
        .unwrap()
}

fn at_in(
    offset: FixedOffset,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
) -> DateTime<FixedOffset> {
    offset
        .from_local_datetime(
            &NaiveDate::from_ymd_opt(year, month, day)
                .unwrap()
                .and_hms_opt(hour, minute, 0)
                .unwrap(),
        )
        .single()
        .unwrap()
}

fn timed_event(id: &str, start: DateTime<FixedOffset>) -> Event {
    Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
        title: "Planning".to_owned(),
        location: String::new(),
        description: "Event description".to_owned(),
        schedule: EventSchedule::Timed {
            start,
            end: start + chrono::Duration::hours(1),
            timezone: None,
        },
        recurrence: None,
        reminders: vec![ReminderSpec {
            seconds_before_start: 10 * 60,
            description: "Prepare notes".to_owned(),
        }],
    }
}

#[test]
fn reminder_occurrences_use_trigger_boundaries_and_expand_bounded_recurring_masters() {
    let single = timed_event("11111111-1111-1111-1111-111111111111", at(2026, 7, 1, 9, 0));

    let at_trigger =
        reminder_occurrences_in_window(&single, at(2026, 7, 1, 8, 49), at(2026, 7, 1, 8, 50));
    assert_eq!(at_trigger.len(), 1, "the inclusive end boundary must fire");
    assert_eq!(at_trigger[0].event_id, single.id);
    assert_eq!(at_trigger[0].occurrence_start, at(2026, 7, 1, 9, 0));
    assert_eq!(at_trigger[0].trigger_at, at(2026, 7, 1, 8, 50));
    assert_eq!(at_trigger[0].description, "Prepare notes");
    assert!(
        reminder_occurrences_in_window(&single, at(2026, 7, 1, 8, 40), at(2026, 7, 1, 8, 49),)
            .is_empty()
    );
    assert!(
        reminder_occurrences_in_window(&single, at(2026, 7, 1, 8, 50), at(2026, 7, 1, 8, 51),)
            .is_empty()
    );

    let mut daily = timed_event("33333333-3333-3333-3333-333333333333", at(2026, 7, 1, 9, 0));
    daily.recurrence = Some(RecurrenceSpec {
        rrule: vec!["RRULE:FREQ=DAILY;COUNT=4".to_owned()],
        rdate: Vec::new(),
        exdate: vec!["EXDATE:20260703T090000Z".to_owned()],
    });
    let recurring =
        reminder_occurrences_in_window(&daily, at(2026, 6, 30, 9, 0), at(2026, 7, 5, 9, 0));
    assert_eq!(
        recurring
            .iter()
            .map(|occurrence| (
                occurrence.event_id,
                occurrence.occurrence_start,
                occurrence.trigger_at,
                occurrence.description.as_str(),
            ))
            .collect::<Vec<_>>(),
        vec![
            (
                daily.id,
                at(2026, 7, 1, 9, 0),
                at(2026, 7, 1, 8, 50),
                "Prepare notes"
            ),
            (
                daily.id,
                at(2026, 7, 2, 9, 0),
                at(2026, 7, 2, 8, 50),
                "Prepare notes"
            ),
            (
                daily.id,
                at(2026, 7, 4, 9, 0),
                at(2026, 7, 4, 8, 50),
                "Prepare notes"
            ),
        ],
        "recurrence must be bounded, omit EXDATE, and be chronologically ordered"
    );

    let mut without_reminders =
        timed_event("44444444-4444-4444-4444-444444444444", at(2026, 7, 1, 9, 0));
    without_reminders.reminders.clear();
    assert!(
        reminder_occurrences_in_window(
            &without_reminders,
            at(2026, 7, 1, 8, 0),
            at(2026, 7, 1, 9, 0),
        )
        .is_empty()
    );
}

#[test]
fn all_day_reminders_use_window_offset_midnight_boundaries_and_recurring_dates() {
    let offset = FixedOffset::east_opt(2 * 60 * 60).unwrap();
    let mut all_day = Event {
        id: Uuid::parse_str("55555555-5555-5555-5555-555555555555").unwrap(),
        calendar_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
        title: "Holiday".to_owned(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 7, 2).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 3).unwrap(),
        },
        recurrence: None,
        reminders: vec![ReminderSpec {
            seconds_before_start: 10 * 60,
            description: "Pack".to_owned(),
        }],
    };

    let at_trigger = reminder_occurrences_in_window(
        &all_day,
        at_in(offset, 2026, 7, 1, 23, 49),
        at_in(offset, 2026, 7, 1, 23, 50),
    );
    assert_eq!(
        at_trigger
            .iter()
            .map(|occurrence| (occurrence.occurrence_start, occurrence.trigger_at))
            .collect::<Vec<_>>(),
        vec![(
            at_in(offset, 2026, 7, 2, 0, 0),
            at_in(offset, 2026, 7, 1, 23, 50),
        ),],
        "an all-day reminder must fire at the inclusive end boundary"
    );
    assert!(
        reminder_occurrences_in_window(
            &all_day,
            at_in(offset, 2026, 7, 1, 23, 40),
            at_in(offset, 2026, 7, 1, 23, 49),
        )
        .is_empty()
    );
    assert!(
        reminder_occurrences_in_window(
            &all_day,
            at_in(offset, 2026, 7, 1, 23, 50),
            at_in(offset, 2026, 7, 1, 23, 51),
        )
        .is_empty()
    );

    all_day.recurrence = Some(RecurrenceSpec {
        rrule: vec!["RRULE:FREQ=DAILY;COUNT=3".to_owned()],
        rdate: Vec::new(),
        exdate: Vec::new(),
    });
    let recurring = reminder_occurrences_in_window(
        &all_day,
        at_in(offset, 2026, 7, 1, 23, 40),
        at_in(offset, 2026, 7, 4, 0, 0),
    );
    assert_eq!(
        recurring
            .iter()
            .map(|occurrence| (occurrence.occurrence_start, occurrence.trigger_at))
            .collect::<Vec<_>>(),
        vec![
            (
                at_in(offset, 2026, 7, 2, 0, 0),
                at_in(offset, 2026, 7, 1, 23, 50),
            ),
            (
                at_in(offset, 2026, 7, 3, 0, 0),
                at_in(offset, 2026, 7, 2, 23, 50),
            ),
            (
                at_in(offset, 2026, 7, 4, 0, 0),
                at_in(offset, 2026, 7, 3, 23, 50),
            ),
        ],
        "a bounded all-day master must emit each local-date reminder once"
    );
}
