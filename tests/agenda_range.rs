use calendar::model::{Calendar, CalendarSource, Event, EventSchedule};
use calendar::month_view::{
    AgendaGroup, DayProjection, EventChip, project_agenda_range, project_agenda_range_in_timezone,
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

fn partition_titles(chips: &[EventChip]) -> Vec<&str> {
    chips.iter().map(|chip| chip.title.as_str()).collect()
}

fn event_day(group: &AgendaGroup) -> &DayProjection {
    match group {
        AgendaGroup::EventDay(day) => day,
        AgendaGroup::EmptyRange { .. } => panic!("expected event day"),
    }
}

#[test]
fn phase8_agenda_range_groups_every_requested_date() {
    let personal_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let work_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let hidden_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let orphan_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();
    let calendars = vec![
        Calendar {
            id: personal_id,
            name: "Personal".into(),
            color: "#3366cc".into(),
            visible: true,
            read_only: false,
            source: CalendarSource::Local,
        },
        Calendar {
            id: work_id,
            name: "Work".into(),
            color: "#cc3333".into(),
            visible: true,
            read_only: false,
            source: CalendarSource::Local,
        },
        Calendar {
            id: hidden_id,
            name: "Hidden".into(),
            color: "#999999".into(),
            visible: false,
            read_only: false,
            source: CalendarSource::Local,
        },
    ];
    let event = |id: &str, calendar_id, title: &str, schedule| Event {
        id: Uuid::parse_str(id).unwrap(),
        calendar_id,
        title: title.into(),
        location: String::new(),
        description: String::new(),
        schedule,
        recurrence: None,
        reminders: Vec::new(),
    };
    let events = vec![
        event(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa09",
            personal_id,
            "Before Range",
            EventSchedule::Timed {
                start: at(2026, 5, 9, 9, 0),
                end: at(2026, 5, 9, 10, 0),
                timezone: None,
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb12",
            work_id,
            "Before Active Date",
            EventSchedule::Timed {
                start: at(2026, 5, 12, 9, 0),
                end: at(2026, 5, 12, 10, 0),
                timezone: None,
            },
        ),
        event(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa14",
            personal_id,
            "Conference",
            EventSchedule::AllDay {
                start_date: NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
                end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb14",
            work_id,
            "Early",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 8, 0),
                end: at(2026, 5, 14, 9, 0),
                timezone: None,
            },
        ),
        event(
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb15",
            work_id,
            "Night Shift",
            EventSchedule::Timed {
                start: at(2026, 5, 14, 23, 0),
                end: at(2026, 5, 15, 1, 0),
                timezone: None,
            },
        ),
        event(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa16",
            personal_id,
            "Late Dinner",
            EventSchedule::Timed {
                start: at(2026, 5, 16, 22, 0),
                end: at(2026, 5, 17, 0, 0),
                timezone: None,
            },
        ),
        event(
            "cccccccc-cccc-cccc-cccc-ccccccccc017",
            hidden_id,
            "Hidden",
            EventSchedule::Timed {
                start: at(2026, 5, 17, 10, 0),
                end: at(2026, 5, 17, 11, 0),
                timezone: None,
            },
        ),
        event(
            "dddddddd-dddd-dddd-dddd-dddddddddd18",
            orphan_id,
            "Orphan",
            EventSchedule::Timed {
                start: at(2026, 5, 18, 10, 0),
                end: at(2026, 5, 18, 11, 0),
                timezone: None,
            },
        ),
        event(
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa19",
            personal_id,
            "After Range",
            EventSchedule::Timed {
                start: at(2026, 5, 19, 9, 0),
                end: at(2026, 5, 19, 10, 0),
                timezone: None,
            },
        ),
    ];
    let start = NaiveDate::from_ymd_opt(2026, 5, 10).unwrap();
    let end = NaiveDate::from_ymd_opt(2026, 5, 19).unwrap();

    let viewer_timezone = FixedOffset::east_opt(2 * 3600).unwrap();
    let groups =
        project_agenda_range_in_timezone(start, end, &calendars, &events, &viewer_timezone);

    assert!(
        matches!(groups[0], AgendaGroup::EmptyRange { start_date, end_date_exclusive } if start_date == start && end_date_exclusive == NaiveDate::from_ymd_opt(2026, 5, 12).unwrap())
    );
    assert_eq!(
        event_day(&groups[1]).date,
        NaiveDate::from_ymd_opt(2026, 5, 12).unwrap()
    );
    assert!(
        matches!(groups[2], AgendaGroup::EmptyRange { start_date, end_date_exclusive } if start_date == NaiveDate::from_ymd_opt(2026, 5, 13).unwrap() && end_date_exclusive == NaiveDate::from_ymd_opt(2026, 5, 14).unwrap())
    );
    assert_eq!(
        event_day(&groups[3]).date,
        NaiveDate::from_ymd_opt(2026, 5, 14).unwrap()
    );
    assert_eq!(
        event_day(&groups[4]).date,
        NaiveDate::from_ymd_opt(2026, 5, 15).unwrap()
    );
    assert_eq!(
        event_day(&groups[5]).date,
        NaiveDate::from_ymd_opt(2026, 5, 16).unwrap()
    );
    assert!(
        matches!(groups[6], AgendaGroup::EmptyRange { start_date, end_date_exclusive } if start_date == NaiveDate::from_ymd_opt(2026, 5, 17).unwrap() && end_date_exclusive == end)
    );
    assert_eq!(groups.len(), 7);

    assert_eq!(
        partition_titles(&event_day(&groups[1]).all_day),
        Vec::<&str>::new()
    );
    assert_eq!(
        partition_titles(&event_day(&groups[1]).timed),
        vec!["Before Active Date"]
    );
    assert_eq!(
        partition_titles(&event_day(&groups[3]).all_day),
        vec!["Conference"]
    );
    assert_eq!(
        partition_titles(&event_day(&groups[3]).timed),
        vec!["Early", "Night Shift"]
    );
    assert_eq!(
        partition_titles(&event_day(&groups[4]).all_day),
        vec!["Conference"]
    );
    assert_eq!(
        partition_titles(&event_day(&groups[4]).timed),
        vec!["Night Shift"]
    );
    assert_eq!(
        partition_titles(&event_day(&groups[5]).all_day),
        Vec::<&str>::new()
    );
    assert_eq!(
        partition_titles(&event_day(&groups[5]).timed),
        vec!["Late Dinner"]
    );

    assert!(project_agenda_range(end, start, &calendars, &events).is_empty());
    assert!(project_agenda_range(start, start, &calendars, &events).is_empty());
}
