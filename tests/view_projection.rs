use calendar::model::{Calendar, CalendarSource, Event, EventSchedule};
use calendar::month_view::{
    DayProjection, EventChip, project_agenda_in_timezone, project_week_in_timezone,
};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

const TWO_HOURS_SECS: i32 = 2 * 3600;

fn at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap();
    FixedOffset::east_opt(TWO_HOURS_SECS)
        .unwrap()
        .from_local_datetime(&naive)
        .single()
        .unwrap()
}

fn day(days: &[DayProjection], date: NaiveDate) -> &DayProjection {
    days.iter().find(|day| day.date == date).unwrap()
}

fn titles(day: &DayProjection) -> Vec<&str> {
    day.all_day
        .iter()
        .chain(&day.timed)
        .map(|chip| chip.title.as_str())
        .collect()
}

fn partition_titles(chips: &[EventChip]) -> Vec<&str> {
    chips.iter().map(|chip| chip.title.as_str()).collect()
}

#[test]
fn phase8_week_and_agenda_share_month_event_projection_rules() {
    let personal_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let work_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let hidden_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let orphan_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
    let calendars = vec![
        Calendar {
            id: personal_id,
            name: "Personal".to_string(),
            color: "#3366cc".to_string(),
            visible: true,
            read_only: false,
            source: CalendarSource::Local,
        },
        Calendar {
            id: work_id,
            name: "Work".to_string(),
            color: "#cc3333".to_string(),
            visible: true,
            read_only: false,
            source: CalendarSource::Local,
        },
        Calendar {
            id: hidden_id,
            name: "Hidden".to_string(),
            color: "#999999".to_string(),
            visible: false,
            read_only: false,
            source: CalendarSource::Local,
        },
    ];
    let event = |id: &str, calendar_id, title: &str, schedule| Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id,
        title: title.to_string(),
        location: String::new(),
        description: String::new(),
        schedule,
        recurrence: None,
        reminders: Vec::new(),
    };
    let events = vec![
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb00",
            work_id,
            "Before Active Date",
            EventSchedule::Timed {
                start: at(2026, 5, 12, 9, 0),
                end: at(2026, 5, 12, 10, 0),
                timezone: None,
            },
        ),
        event(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa01",
            personal_id,
            "Conference",
            EventSchedule::AllDay {
                start_date: NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
                end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb01",
            work_id,
            "Early",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 8, 0),
                end: at(2026, 5, 14, 9, 0),
                timezone: None,
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb02",
            work_id,
            "Standup",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 9, 0),
                end: at(2026, 5, 14, 10, 0),
                timezone: None,
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb03",
            work_id,
            "Afternoon",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 14, 0),
                end: at(2026, 5, 14, 15, 0),
                timezone: None,
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb04",
            work_id,
            "Night Shift",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 23, 0),
                end: at(2026, 5, 15, 1, 0),
                timezone: None,
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb05",
            personal_id,
            "Late Dinner",
            EventSchedule::Timed {
                start: at(2026, 5, 16, 22, 0),
                end: at(2026, 5, 17, 0, 0),
                timezone: None,
            },
        ),
        event(
            "cccccccc-cccc-cccc-cccc-ccccccccc001",
            hidden_id,
            "Hidden",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 10, 0),
                end: at(2026, 5, 14, 11, 0),
                timezone: None,
            },
        ),
        event(
            "dddddddd-dddd-dddd-dddd-dddddddddd01",
            orphan_id,
            "Orphan",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 12, 0),
                end: at(2026, 5, 14, 13, 0),
                timezone: None,
            },
        ),
        event(
            "eeeeeeee-eeee-eeee-eeee-eeeeeeeeee01",
            personal_id,
            "Outside Window",
            EventSchedule::AllDay {
                start_date: NaiveDate::from_ymd_opt(2026, 5, 18).unwrap(),
                end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 19).unwrap(),
            },
        ),
    ];
    let active_date = NaiveDate::from_ymd_opt(2026, 5, 13).unwrap();

    let viewer_timezone = FixedOffset::east_opt(2 * 3600).unwrap();
    let week = project_week_in_timezone(active_date, &calendars, &events, &viewer_timezone);
    assert_eq!(
        week.each_ref().map(|day| day.date),
        [
            NaiveDate::from_ymd_opt(2026, 5, 11).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 12).unwrap(),
            active_date,
            NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 17).unwrap(),
        ]
    );
    let may_14 = day(&week, NaiveDate::from_ymd_opt(2026, 5, 14).unwrap());
    assert_eq!(
        partition_titles(&day(&week, NaiveDate::from_ymd_opt(2026, 5, 12).unwrap()).timed),
        vec!["Before Active Date"]
    );
    assert_eq!(partition_titles(&may_14.all_day), vec!["Conference"]);
    assert_eq!(
        partition_titles(&may_14.timed),
        vec!["Early", "Standup", "Afternoon", "Night Shift"]
    );
    assert_eq!(
        titles(may_14),
        vec!["Conference", "Early", "Standup", "Afternoon", "Night Shift"]
    );
    let may_15 = day(&week, NaiveDate::from_ymd_opt(2026, 5, 15).unwrap());
    assert_eq!(partition_titles(&may_15.all_day), vec!["Conference"]);
    assert_eq!(partition_titles(&may_15.timed), vec!["Night Shift"]);
    let may_16 = day(&week, NaiveDate::from_ymd_opt(2026, 5, 16).unwrap());
    assert!(may_16.all_day.is_empty());
    assert_eq!(partition_titles(&may_16.timed), vec!["Late Dinner"]);
    assert!(
        day(&week, NaiveDate::from_ymd_opt(2026, 5, 17).unwrap())
            .timed
            .is_empty()
    );
    assert!(
        week.iter()
            .flat_map(|day| day.all_day.iter().chain(&day.timed))
            .all(|chip| chip.calendar_id != hidden_id && chip.calendar_id != orphan_id)
    );

    let agenda = project_agenda_in_timezone(active_date, &calendars, &events, &viewer_timezone);
    assert_eq!(
        agenda.iter().map(|day| day.date).collect::<Vec<_>>(),
        vec![
            active_date,
            NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
            NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        ]
    );
    assert!(agenda[0].all_day.is_empty() && agenda[0].timed.is_empty());
    for agenda_day in &agenda[1..] {
        assert_eq!(agenda_day, day(&week, agenda_day.date));
    }
}
