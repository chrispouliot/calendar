use calendar::model::{
    Calendar, CalendarSource, DetachedEvent, Event, EventSchedule, RecurrenceId, RecurrenceSpec,
    ReminderSpec,
};
use calendar::month_view::{
    AgendaGroup, DayProjection, ViewerLocalSchedule,
    project_agenda_range_with_detached_events_in_timezone,
    project_month_with_detached_events_in_timezone, project_week_with_detached_events_in_timezone,
};
use chrono::{DateTime, FixedOffset, NaiveDate};
use uuid::Uuid;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn time(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

fn event_day(groups: &[AgendaGroup], target: NaiveDate) -> &DayProjection {
    groups
        .iter()
        .find_map(|group| match group {
            AgendaGroup::EventDay(day) if day.date == target => Some(day),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected an event day on {target}"))
}

fn timed_titles(groups: &[AgendaGroup], target: NaiveDate) -> Vec<&str> {
    event_day(groups, target)
        .timed
        .iter()
        .map(|chip| chip.title.as_str())
        .collect()
}

fn projection_day(days: &[DayProjection], target: NaiveDate) -> &DayProjection {
    days.iter()
        .find(|day| day.date == target)
        .unwrap_or_else(|| panic!("expected a projected day on {target}"))
}

// Public projection metadata contract:
//
//     pub enum ViewerLocalSchedule {
//         Timed { start: DateTime<FixedOffset>, end: DateTime<FixedOffset> },
//         AllDay { start_date: NaiveDate, end_date_exclusive: NaiveDate },
//     }
//
//     pub struct EventChip {
//         // ...existing fields...
//         pub viewer_local_schedule: ViewerLocalSchedule,
//         pub original_recurrence_id: Option<RecurrenceId>,
//     }
//
// `viewer_local_schedule` is the occurrence schedule after conversion to the
// supplied viewer timezone. `original_recurrence_id` identifies the generated
// instance that a detached modification replaces, rather than its moved time.
fn timed_recurrence_id(day: u32) -> RecurrenceId {
    RecurrenceId::Timed {
        date_time: time(&format!("2026-07-{day:02}T09:00:00-04:00")),
        timezone: Some("America/New_York".to_owned()),
    }
}

fn assert_exception_projection(days: &[DayProjection], master_id: Uuid) {
    let july_7 = projection_day(days, date(2026, 7, 7));
    let generated = july_7
        .timed
        .iter()
        .find(|chip| chip.title == "Daily standup")
        .expect("the unaffected occurrence must remain");
    assert_eq!(generated.event_id, master_id);
    assert_eq!(
        generated.original_recurrence_id,
        Some(timed_recurrence_id(7))
    );
    assert_eq!(
        generated.viewer_local_schedule,
        ViewerLocalSchedule::Timed {
            start: time("2026-07-07T09:00:00-04:00"),
            end: time("2026-07-07T09:30:00-04:00"),
        }
    );
    let all_day = july_7
        .all_day
        .iter()
        .find(|chip| chip.title == "All-day note")
        .expect("the non-recurring all-day event must remain");
    assert_eq!(all_day.original_recurrence_id, None);
    assert_eq!(
        all_day.viewer_local_schedule,
        ViewerLocalSchedule::AllDay {
            start_date: date(2026, 7, 7),
            end_date_exclusive: date(2026, 7, 8),
        }
    );
    assert_eq!(
        projection_day(days, date(2026, 7, 8))
            .timed
            .iter()
            .map(|chip| chip.title.as_str())
            .collect::<Vec<_>>(),
        Vec::<&str>::new(),
        "the original generated July 8 slot must be suppressed"
    );
    assert!(
        projection_day(days, date(2026, 7, 9)).timed.is_empty(),
        "the cancelled July 9 occurrence must be omitted"
    );
    let moved: Vec<_> = projection_day(days, date(2026, 7, 10))
        .timed
        .iter()
        .filter(|chip| chip.title == "Standup rescheduled")
        .collect();
    assert_eq!(
        moved.len(),
        2,
        "same-looking modified overrides must remain separate projected instances"
    );
    assert_eq!(
        days.iter()
            .flat_map(|day| day.timed.iter())
            .filter(|chip| chip.title == "Standup rescheduled")
            .count(),
        2,
        "the collision overrides must not also remain at their original generated slots"
    );
    for chip in moved {
        assert_eq!(chip.event_id, master_id);
        assert_eq!(
            chip.viewer_local_schedule,
            ViewerLocalSchedule::Timed {
                start: time("2026-07-10T10:00:00-04:00"),
                end: time("2026-07-10T11:00:00-04:00"),
            }
        );
    }
    let identities = projection_day(days, date(2026, 7, 10))
        .timed
        .iter()
        .filter(|chip| chip.title == "Standup rescheduled")
        .map(|chip| chip.original_recurrence_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(identities.len(), 2);
    assert!(
        identities.contains(&Some(timed_recurrence_id(6)))
            && identities.contains(&Some(timed_recurrence_id(8))),
        "identical title/time/duration overrides must retain their distinct original identities"
    );
}

#[test]
fn detached_instances_replace_tzid_matched_generated_occurrences_and_project_moves() {
    let calendar_id = Uuid::parse_str("2a000001-0000-0000-0000-000000000001").unwrap();
    let master_id = Uuid::parse_str("2a000002-0000-0000-0000-000000000002").unwrap();
    let calendars = vec![Calendar {
        id: calendar_id,
        name: "Work".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    }];
    let master = Event {
        id: master_id,
        calendar_id,
        title: "Daily standup".to_owned(),
        location: "Room 1".to_owned(),
        description: "Team sync".to_owned(),
        schedule: EventSchedule::Timed {
            start: time("2026-07-06T09:00:00-04:00"),
            end: time("2026-07-06T09:30:00-04:00"),
            timezone: Some("America/New_York".to_owned()),
        },
        recurrence: Some(RecurrenceSpec {
            rrule: vec!["FREQ=DAILY;COUNT=4".to_owned()],
            ..Default::default()
        }),
        reminders: Vec::new(),
    };
    let exceptions = vec![
        // Its recurrence date is outside the requested range, but its new
        // schedule must still be projected inside it. It deliberately
        // collides with the override for the July 8 recurrence below.
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-06T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            title: "Standup rescheduled".to_owned(),
            location: "Zoom".to_owned(),
            description: "Remote standup".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-07-10T10:00:00-04:00"),
                end: time("2026-07-10T11:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 900,
                description: "Join Zoom".to_owned(),
            }],
        },
        DetachedEvent::Modified {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-08T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            title: "Standup rescheduled".to_owned(),
            location: "Room 2".to_owned(),
            description: "Release review".to_owned(),
            schedule: EventSchedule::Timed {
                start: time("2026-07-10T10:00:00-04:00"),
                end: time("2026-07-10T11:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            reminders: vec![ReminderSpec {
                seconds_before_start: 600,
                description: "Prepare release notes".to_owned(),
            }],
        },
        DetachedEvent::Cancelled {
            recurrence_id: RecurrenceId::Timed {
                date_time: time("2026-07-09T09:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
        },
    ];
    let non_recurring_all_day = Event {
        id: Uuid::parse_str("2a000003-0000-0000-0000-000000000003").unwrap(),
        calendar_id,
        title: "All-day note".to_owned(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: date(2026, 7, 7),
            end_date_exclusive: date(2026, 7, 8),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let viewer_timezone = FixedOffset::west_opt(4 * 3600).unwrap();

    let projection = project_agenda_range_with_detached_events_in_timezone(
        date(2026, 7, 7),
        date(2026, 7, 11),
        &calendars,
        &[
            (master.clone(), exceptions.clone()),
            (non_recurring_all_day.clone(), Vec::new()),
        ],
        &viewer_timezone,
    );

    assert_eq!(
        timed_titles(&projection, date(2026, 7, 7)),
        vec!["Daily standup"]
    );
    assert!(
        projection.iter().all(
            |group| !matches!(group, AgendaGroup::EventDay(day) if day.date == date(2026, 7, 8))
        ),
        "the July 8 generated slot belongs to a moved exception and must be suppressed"
    );
    assert!(
        projection.iter().all(
            |group| !matches!(group, AgendaGroup::EventDay(day) if day.date == date(2026, 7, 9))
        ),
        "the cancelled July 9 occurrence must be omitted"
    );
    assert_eq!(
        timed_titles(&projection, date(2026, 7, 10)),
        vec!["Standup rescheduled", "Standup rescheduled"],
        "the collision overrides must both appear at their shared new schedule"
    );
    let agenda_moved = &event_day(&projection, date(2026, 7, 10)).timed;
    let agenda_identities = agenda_moved
        .iter()
        .map(|chip| chip.original_recurrence_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(agenda_identities.len(), 2);
    assert!(
        agenda_identities.contains(&Some(timed_recurrence_id(6)))
            && agenda_identities.contains(&Some(timed_recurrence_id(8)))
    );
    assert!(agenda_moved.iter().all(|chip| {
        chip.viewer_local_schedule
            == ViewerLocalSchedule::Timed {
                start: time("2026-07-10T10:00:00-04:00"),
                end: time("2026-07-10T11:00:00-04:00"),
            }
    }));

    let month = project_month_with_detached_events_in_timezone(
        2026,
        7,
        &calendars,
        &[
            (master.clone(), exceptions.clone()),
            (non_recurring_all_day.clone(), Vec::new()),
        ],
        &viewer_timezone,
    );
    assert_exception_projection(&month, master_id);

    let week = project_week_with_detached_events_in_timezone(
        date(2026, 7, 8),
        &calendars,
        &[(master, exceptions), (non_recurring_all_day, Vec::new())],
        &viewer_timezone,
    );
    assert_exception_projection(&week, master_id);
}
