// Public contract pinned by this acceptance test:
//
//     pub mod agenda_presentation {
//         use chrono::{DateTime, FixedOffset, NaiveDate};
//         use crate::model::EventSchedule;
//         use crate::month_view::{AgendaGroup, EventChip};
//         use crate::time_format::TimeFormatPreference;
//
//         #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//         pub enum AgendaEventState { Past, Current, Upcoming }
//
//         #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//         pub enum AgendaTimeLayout { Desktop, Compact }
//
//         pub fn event_state(
//             schedule: &EventSchedule,
//             now: DateTime<FixedOffset>,
//         ) -> AgendaEventState;
//
//         pub fn time_text(
//             chip: &EventChip,
//             layout: AgendaTimeLayout,
//             preference: TimeFormatPreference,
//             system_clock_format: &str,
//         ) -> Option<String>;
//
//         pub fn has_no_upcoming_events(groups: &[AgendaGroup], today: NaiveDate) -> bool;
//     }
//
// All inputs are injected projections and fixed dates/times: this presentation
// contract must not initialize GTK or read preferences, a clock, or GSettings.

use calendar::agenda_presentation::{
    AgendaEventState, AgendaTimeLayout, event_state, has_no_upcoming_events, time_text,
};
use calendar::model::EventSchedule;
use calendar::month_view::{AgendaGroup, DayProjection, EventChip, ViewerLocalEnd};
use calendar::time_format::TimeFormatPreference;
use chrono::{DateTime, FixedOffset, NaiveDate, NaiveTime, TimeZone};
use uuid::Uuid;

const TWO_HOURS_SECS: i32 = 2 * 3600;

fn at(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, minute, 0)
        .unwrap();
    FixedOffset::east_opt(TWO_HOURS_SECS)
        .unwrap()
        .from_local_datetime(&naive)
        .single()
        .unwrap()
}

fn time(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).unwrap()
}

fn timed_chip() -> EventChip {
    EventChip {
        event_id: Uuid::nil(),
        title: "Planning".into(),
        calendar_id: Uuid::nil(),
        color: "#3366cc".into(),
        is_all_day: false,
        start_time: Some(time(14, 30)),
        viewer_local_end: ViewerLocalEnd::Timed(at(2026, 5, 14, 15, 45)),
    }
}

fn event_day(date: NaiveDate) -> AgendaGroup {
    AgendaGroup::EventDay(DayProjection {
        date,
        in_displayed_month: true,
        all_day: Vec::new(),
        timed: vec![timed_chip()],
    })
}

#[test]
fn agenda_presentation_classifies_events_formats_times_and_detects_no_upcoming_events() {
    let start = at(2026, 5, 14, 14, 30);
    let end = at(2026, 5, 14, 15, 45);
    let timed = EventSchedule::Timed {
        start,
        end,
        timezone: None,
    };
    let all_day = EventSchedule::AllDay {
        start_date: NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
        end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
    };

    assert_eq!(
        event_state(&timed, at(2026, 5, 14, 14, 29)),
        AgendaEventState::Upcoming
    );
    assert_eq!(
        event_state(&timed, start),
        AgendaEventState::Current,
        "a timed event starts being current at its inclusive start"
    );
    assert_eq!(
        event_state(&timed, at(2026, 5, 14, 15, 44)),
        AgendaEventState::Current
    );
    assert_eq!(
        event_state(&timed, end),
        AgendaEventState::Past,
        "a timed event is past at its exclusive end"
    );
    assert_ne!(
        event_state(&all_day, at(2026, 5, 14, 14, 30)),
        AgendaEventState::Current,
        "all-day events are never rendered as current"
    );

    let chip = timed_chip();
    assert_eq!(
        time_text(
            &chip,
            AgendaTimeLayout::Desktop,
            TimeFormatPreference::TwelveHour,
            "24h",
        ),
        Some("2:30 PM–3:45 PM".into())
    );
    assert_eq!(
        time_text(
            &chip,
            AgendaTimeLayout::Compact,
            TimeFormatPreference::TwentyFourHour,
            "12h",
        ),
        Some("14:30".into())
    );
    assert_eq!(
        time_text(
            &chip,
            AgendaTimeLayout::Desktop,
            TimeFormatPreference::TwentyFourHour,
            "12h",
        ),
        Some("14:30–15:45".into())
    );
    assert_eq!(
        time_text(
            &chip,
            AgendaTimeLayout::Compact,
            TimeFormatPreference::TwelveHour,
            "24h",
        ),
        Some("2:30 PM".into())
    );

    let today = NaiveDate::from_ymd_opt(2026, 5, 14).unwrap();
    let past_only = vec![
        event_day(NaiveDate::from_ymd_opt(2026, 5, 13).unwrap()),
        AgendaGroup::EmptyRange {
            start_date: today,
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 17).unwrap(),
        },
    ];
    assert!(
        has_no_upcoming_events(&past_only, today),
        "past event days and empty ranges do not count as upcoming events"
    );
    assert!(
        !has_no_upcoming_events(&[event_day(today)], today),
        "an event day today suppresses the no-upcoming state"
    );
    assert!(
        !has_no_upcoming_events(
            &[event_day(NaiveDate::from_ymd_opt(2026, 5, 15).unwrap())],
            today,
        ),
        "a future event day suppresses the no-upcoming state"
    );
}
