// Public contract pinned by this acceptance test:
//
//     pub mod agenda_render_plan {
//         use chrono::{FixedOffset, NaiveDate};
//         use crate::model::{Calendar, Event};
//         use crate::month_view::AgendaGroup;
//
//         #[derive(Debug, Clone, PartialEq, Eq)]
//         pub struct AgendaRange {
//             pub start_date: NaiveDate,
//             pub end_date_exclusive: NaiveDate,
//         }
//
//         impl AgendaRange {
//             // `initial_future_days` is the exclusive range length in days.
//             pub fn new(today: NaiveDate, initial_future_days: i64) -> Self;
//             // Makes the target date part of the range without moving its start.
//             pub fn ensure_target(&mut self, target: NaiveDate);
//             // Extends only the exclusive end by the supplied number of days.
//             pub fn extend_bottom(&mut self, days: i64);
//         }
//
//         #[derive(Debug, Clone, PartialEq, Eq)]
//         pub enum AgendaRenderPlan { NoUpcoming, Groups(Vec<AgendaGroup>) }
//
//         // Pure: no GTK, filesystem, clock, or settings access. The plan starts
//         // at `today`, reaches visible future events even beyond `range`, and
//         // stops at its last event rather than adding a trailing horizon gap.
//         pub fn render_agenda(
//             today: NaiveDate,
//             range: &AgendaRange,
//             calendars: &[Calendar],
//             events: &[Event],
//             viewer_timezone: &FixedOffset,
//         ) -> AgendaRenderPlan;
//     }

use calendar::agenda_render_plan::{AgendaRange, AgendaRenderPlan, render_agenda};
use calendar::model::{Calendar, CalendarSource, Event, EventSchedule};
use calendar::month_view::AgendaGroup;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

fn date(month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, month, day).unwrap()
}

fn utc_at(month: u32, day: u32) -> DateTime<FixedOffset> {
    FixedOffset::east_opt(0)
        .unwrap()
        .with_ymd_and_hms(2026, month, day, 9, 0, 0)
        .single()
        .unwrap()
}

fn calendar(id: Uuid, visible: bool) -> Calendar {
    Calendar {
        id,
        name: "Calendar".into(),
        color: "#3366cc".into(),
        visible,
        read_only: false,
        source: CalendarSource::Local,
    }
}

fn event(id: Uuid, calendar_id: Uuid, title: &str, month: u32, day: u32) -> Event {
    let start = utc_at(month, day);
    Event {
        id,
        calendar_id,
        title: title.into(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start,
            end: start + Duration::hours(1),
            timezone: Some("UTC".into()),
        },
        recurrence: None,
        reminders: Vec::new(),
    }
}

fn group_dates(groups: &[AgendaGroup]) -> Vec<(NaiveDate, NaiveDate)> {
    groups
        .iter()
        .map(|group| match group {
            AgendaGroup::EventDay(day) => (day.date, day.date),
            AgendaGroup::EmptyRange {
                start_date,
                end_date_exclusive,
            } => (*start_date, *end_date_exclusive),
        })
        .collect()
}

#[test]
fn agenda_render_plan_is_forward_only_and_renders_a_future_facing_unbounded_plan() {
    let today = date(7, 28);
    let mut range = AgendaRange::new(today, 3);
    assert_eq!(
        range.start_date, today,
        "the range must never start before today"
    );
    assert_eq!(range.end_date_exclusive, date(7, 31));

    range.ensure_target(date(7, 22));
    assert_eq!(
        range.start_date, today,
        "a past target cannot move the range backward"
    );
    assert_eq!(range.end_date_exclusive, date(7, 31));

    range.ensure_target(date(8, 5));
    assert_eq!(range.start_date, today);
    assert_eq!(range.end_date_exclusive, date(8, 6));
    range.extend_bottom(7);
    let first_extended_end = range.end_date_exclusive;
    range.extend_bottom(7);
    assert_eq!(range.start_date, today, "bottom extension is forward-only");
    assert!(range.end_date_exclusive > first_extended_end);

    let visible_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let hidden_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let calendars = vec![calendar(visible_id, true), calendar(hidden_id, false)];
    let events = vec![
        event(Uuid::new_v4(), visible_id, "Past", 7, 22),
        event(Uuid::new_v4(), visible_id, "Jul 30", 7, 30),
        event(Uuid::new_v4(), visible_id, "Aug 5", 8, 5),
    ];
    let utc = FixedOffset::east_opt(0).unwrap();

    let AgendaRenderPlan::Groups(groups) = render_agenda(today, &range, &calendars, &events, &utc)
    else {
        panic!("visible future events must produce display groups");
    };
    assert_eq!(
        group_dates(&groups),
        vec![
            (date(7, 28), date(7, 29)),
            (date(7, 29), date(7, 30)),
            (date(7, 30), date(7, 30)),
            (date(7, 31), date(8, 5)),
            (date(8, 5), date(8, 5)),
        ],
        "Today is its own empty day; subsequent gaps are maximal and there is no trailing horizon gap"
    );
    assert!(groups.iter().all(|group| match group {
        AgendaGroup::EventDay(day) => day.date >= today,
        AgendaGroup::EmptyRange { start_date, .. } => *start_date >= today,
    }));
    assert!(matches!(&groups[2], AgendaGroup::EventDay(day) if day.timed[0].title == "Jul 30"));
    assert!(matches!(&groups[4], AgendaGroup::EventDay(day) if day.timed[0].title == "Aug 5"));

    let short_range = AgendaRange::new(today, 2);
    let AgendaRenderPlan::Groups(beyond_initial_range) =
        render_agenda(today, &short_range, &calendars, &events[2..], &utc)
    else {
        panic!("a visible non-recurring event beyond the initial range is still upcoming");
    };
    assert!(
        matches!(beyond_initial_range.last(), Some(AgendaGroup::EventDay(day)) if day.date == date(8, 5))
    );

    assert_eq!(
        render_agenda(
            today,
            &short_range,
            &calendars,
            &[
                events[0].clone(),
                event(Uuid::new_v4(), hidden_id, "Hidden", 8, 5)
            ],
            &utc,
        ),
        AgendaRenderPlan::NoUpcoming,
        "past events and events on hidden calendars do not count as upcoming"
    );
}
