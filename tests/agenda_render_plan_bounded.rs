use calendar::agenda_render_plan::{AgendaRange, AgendaRenderPlan, render_agenda};
use calendar::model::{Calendar, CalendarSource, Event, EventSchedule, RecurrenceSpec};
use calendar::month_view::AgendaGroup;
use chrono::{FixedOffset, NaiveDate};
use uuid::Uuid;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

#[test]
fn render_agenda_bounds_an_unbounded_daily_recurrence_to_the_requested_range() {
    let today = date(2026, 7, 28);
    let calendar_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let calendars = vec![Calendar {
        id: calendar_id,
        name: "Visible calendar".into(),
        color: "#3366cc".into(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    }];
    let events = vec![Event {
        id: Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap(),
        calendar_id,
        title: "Daily since 2000".into(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: date(2000, 1, 1),
            end_date_exclusive: date(2000, 1, 2),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["RRULE:FREQ=DAILY".into()],
            rdate: Vec::new(),
            exdate: Vec::new(),
        }),
        reminders: Vec::new(),
    }];
    let range = AgendaRange::new(today, 30);
    let utc = FixedOffset::east_opt(0).unwrap();

    let AgendaRenderPlan::Groups(groups) = render_agenda(today, &range, &calendars, &events, &utc)
    else {
        panic!("the visible daily recurrence must remain upcoming");
    };

    let event_days: Vec<_> = groups
        .iter()
        .filter_map(|group| match group {
            AgendaGroup::EventDay(day) => Some(day.date),
            AgendaGroup::EmptyRange { .. } => None,
        })
        .collect();

    assert!(
        !event_days.is_empty(),
        "the requested range contains occurrences"
    );
    assert!(
        event_days
            .iter()
            .all(|date| *date >= today && *date < range.end_date_exclusive),
        "an unbounded recurrence must not extend the agenda horizon beyond the requested range"
    );
    assert!(
        groups.len() <= 31 && event_days.len() <= 30,
        "a 30-day agenda range must not render thousands of recurrence occurrences"
    );
}
