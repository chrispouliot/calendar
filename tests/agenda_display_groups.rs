// Public contract pinned by this acceptance test:
//
//     pub fn display_groups(groups: &[AgendaGroup], today: NaiveDate) -> Vec<AgendaGroup>;
//
// The inputs are already-projected agenda groups and an injected date; this
// presentation function must not depend on GTK, storage, settings, or a clock.

use calendar::agenda_presentation::display_groups;
use calendar::month_view::{
    AgendaGroup, DayProjection, EventChip, ViewerLocalEnd, ViewerLocalSchedule,
};
use chrono::NaiveDate;
use uuid::Uuid;

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 5, day).unwrap()
}

fn event_day(day: u32, title: &str) -> AgendaGroup {
    AgendaGroup::EventDay(DayProjection {
        date: date(day),
        in_displayed_month: true,
        all_day: vec![EventChip {
            event_id: Uuid::nil(),
            title: title.into(),
            calendar_id: Uuid::nil(),
            color: "#3366cc".into(),
            is_all_day: true,
            start_time: None,
            viewer_local_end: ViewerLocalEnd::AllDay(date(day + 1)),
            viewer_local_schedule: ViewerLocalSchedule::AllDay {
                start_date: date(day),
                end_date_exclusive: date(day + 1),
            },
            original_recurrence_id: None,
        }],
        timed: Vec::new(),
    })
}

#[test]
fn display_groups_starts_at_an_empty_today_without_fragmenting_future_empty_time() {
    let today = date(14);
    let projected_groups = vec![
        event_day(12, "Past event"),
        AgendaGroup::EmptyRange {
            start_date: date(13),
            end_date_exclusive: date(21),
        },
        event_day(21, "Future event"),
    ];

    assert_eq!(
        display_groups(&projected_groups, today),
        vec![
            AgendaGroup::EmptyRange {
                start_date: today,
                end_date_exclusive: date(15),
            },
            AgendaGroup::EmptyRange {
                start_date: date(15),
                end_date_exclusive: date(21),
            },
            event_day(21, "Future event"),
        ],
        "today must have its own empty card while the following empty dates remain one range"
    );
}
