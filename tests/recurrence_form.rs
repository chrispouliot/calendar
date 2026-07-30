// Public contract pinned by this acceptance test:
//
//     pub enum Frequency { None, Daily, Weekly, Monthly, Yearly }
//     pub enum Weekday { Monday, Tuesday, Wednesday, Thursday, Friday, Saturday, Sunday }
//     pub enum EndCondition { Never, Count(u32), Until(NaiveDate) }
//     pub struct RecurrenceForm { pub frequency: Frequency, pub interval: u32,
//         pub weekdays: Vec<Weekday>, pub end: EndCondition }
//     pub enum RecurrencePresentation {
//         Editable { form: RecurrenceForm, summary: String },
//         Custom { summary: String },
//     }
//     pub fn recurrence_from_form(
//         form: &RecurrenceForm, schedule: &EventSchedule,
//     ) -> Result<Option<RecurrenceSpec>, RecurrenceFormError>;
//     pub fn recurrence_presentation(
//         recurrence: Option<&RecurrenceSpec>, schedule: &EventSchedule,
//     ) -> RecurrencePresentation;
//
// This module is deliberately GTK-free: the editor supplies structured state and
// decides how to preserve a `Custom` recurrence when the user does not replace it.

use calendar::backend::caldav::{map_icalendar_event, serialize_icalendar_event};
use calendar::model::{Event, EventSchedule, RecurrenceSpec};
use calendar::recurrence_form::{
    EndCondition, Frequency, RecurrenceForm, RecurrencePresentation, Weekday, recurrence_from_form,
    recurrence_presentation,
};
use chrono::{DateTime, NaiveDate};
use rrule::RRuleSet;
use uuid::Uuid;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn assert_until_round_trips(schedule: EventSchedule, until: NaiveDate, expected_rule: &str) {
    let form = RecurrenceForm {
        frequency: Frequency::Monthly,
        interval: 1,
        weekdays: vec![],
        end: EndCondition::Until(until),
    };
    let recurrence = recurrence_from_form(&form, &schedule)
        .expect("an until-date is valid for a simple recurrence")
        .expect("a repeating form must produce a recurrence spec");
    assert_eq!(recurrence.rrule, vec![expected_rule]);

    let source = Event {
        id: Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap(),
        calendar_id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
        title: "Until boundary".to_owned(),
        location: String::new(),
        description: String::new(),
        schedule: schedule.clone(),
        recurrence: Some(recurrence.clone()),
        reminders: Vec::new(),
    };
    let resource = serialize_icalendar_event(&source, "until-boundary")
        .expect("a schedule-compatible UNTIL must serialize through CalDAV");
    assert!(resource.contains(expected_rule));
    let mapped = map_icalendar_event(&resource, source.id, source.calendar_id)
        .expect("a serialized schedule-compatible UNTIL must map through CalDAV");
    assert_eq!(mapped.event, source);

    match recurrence_presentation(Some(&recurrence), &schedule) {
        RecurrencePresentation::Editable { form: parsed, .. } => {
            assert_eq!(
                parsed, form,
                "schedule-compatible UNTIL must remain editable"
            )
        }
        RecurrencePresentation::Custom { .. } => {
            panic!("a generated schedule-compatible UNTIL must not become custom")
        }
    }
}

#[test]
fn phase1_recurrence_form_builds_editable_rules_and_preserves_complex_imports() {
    let schedule = EventSchedule::AllDay {
        start_date: date(2026, 7, 6),
        end_date_exclusive: date(2026, 7, 7),
    };
    let never = EndCondition::Never;

    assert_eq!(
        recurrence_from_form(
            &RecurrenceForm {
                frequency: Frequency::None,
                interval: 1,
                weekdays: vec![],
                end: never.clone(),
            },
            &schedule,
        )
        .expect("no-repeat is a valid form state"),
        None,
        "no-repeat must not create an empty recurrence spec"
    );

    let cases = [
        (
            RecurrenceForm {
                frequency: Frequency::Daily,
                interval: 1,
                weekdays: vec![],
                end: never.clone(),
            },
            "RRULE:FREQ=DAILY",
        ),
        (
            RecurrenceForm {
                frequency: Frequency::Weekly,
                interval: 2,
                weekdays: vec![Weekday::Monday, Weekday::Wednesday],
                end: EndCondition::Count(4),
            },
            "RRULE:FREQ=WEEKLY;INTERVAL=2;BYDAY=MO,WE;COUNT=4",
        ),
        (
            RecurrenceForm {
                frequency: Frequency::Yearly,
                interval: 1,
                weekdays: vec![],
                end: never.clone(),
            },
            "RRULE:FREQ=YEARLY",
        ),
    ];
    for (form, expected_rule) in cases {
        let recurrence = recurrence_from_form(&form, &schedule)
            .expect("a complete simple form must build a recurrence")
            .expect("a repeating form must produce a recurrence spec");
        assert_eq!(recurrence.rrule, vec![expected_rule]);
        assert!(recurrence.rdate.is_empty() && recurrence.exdate.is_empty());
        format!("DTSTART:20260706T000000\n{}\n", recurrence.rrule[0])
            .parse::<RRuleSet>()
            .expect("every generated RRULE must be valid iCalendar recurrence syntax");

        match recurrence_presentation(Some(&recurrence), &schedule) {
            RecurrencePresentation::Editable {
                form: parsed,
                summary,
            } => {
                assert_eq!(parsed, form, "simple generated rules must remain editable");
                assert!(
                    !summary.trim().is_empty(),
                    "an editable recurrence needs a user-facing summary"
                );
            }
            RecurrencePresentation::Custom { .. } => {
                panic!("a simple generated rule must not become read-only")
            }
        }
    }

    assert_until_round_trips(
        schedule.clone(),
        date(2026, 12, 31),
        "RRULE:FREQ=MONTHLY;UNTIL=20261231",
    );
    assert_until_round_trips(
        EventSchedule::Timed {
            start: DateTime::parse_from_rfc3339("2026-01-15T09:30:00+00:00").unwrap(),
            end: DateTime::parse_from_rfc3339("2026-01-15T10:30:00+00:00").unwrap(),
            timezone: None,
        },
        date(2026, 2, 20),
        "RRULE:FREQ=MONTHLY;UNTIL=20260220T093000Z",
    );
    assert_until_round_trips(
        EventSchedule::Timed {
            start: DateTime::parse_from_rfc3339("2026-03-02T09:30:00+01:00").unwrap(),
            end: DateTime::parse_from_rfc3339("2026-03-02T10:30:00+01:00").unwrap(),
            timezone: Some("Europe/Berlin".to_owned()),
        },
        date(2026, 4, 6),
        "RRULE:FREQ=MONTHLY;UNTIL=20260406T073000Z",
    );

    assert!(
        recurrence_from_form(
            &RecurrenceForm {
                frequency: Frequency::Weekly,
                interval: 1,
                weekdays: vec![],
                end: never,
            },
            &schedule,
        )
        .is_err(),
        "weekly recurrence requires at least one selected weekday"
    );

    let imported = RecurrenceSpec {
        rrule: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO".to_owned()],
        rdate: vec!["RDATE;VALUE=DATE:20260716".to_owned()],
        exdate: vec!["EXDATE;VALUE=DATE:20260713".to_owned()],
    };
    match recurrence_presentation(Some(&imported), &schedule) {
        RecurrencePresentation::Custom { summary } => assert!(
            !summary.trim().is_empty(),
            "a custom imported recurrence still needs a user-facing summary"
        ),
        RecurrencePresentation::Editable { .. } => {
            panic!("RDATE/EXDATE recurrence must be read-only rather than simplified")
        }
    }
    assert_eq!(
        imported,
        RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=WEEKLY;BYDAY=MO".to_owned()],
            rdate: vec!["RDATE;VALUE=DATE:20260716".to_owned()],
            exdate: vec!["EXDATE;VALUE=DATE:20260713".to_owned()],
        },
        "classifying a custom recurrence must leave the caller's preservable spec intact"
    );
}
