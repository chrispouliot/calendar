use calendar::backend::caldav::map_icalendar_event;
use calendar::backend::{CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{Calendar, CalendarSource, Event, EventSchedule, RecurrenceId};
use calendar::month_view::{
    AgendaGroup, EventChip, project_agenda_range_with_detached_events_in_timezone,
    project_month_with_detached_events_in_timezone, project_week_with_detached_events_in_timezone,
};
use calendar::recurrence_form::{
    EndCondition, Frequency, RecurrenceForm, Weekday, recurrence_from_form, split_recurrence_at,
};
use chrono::{DateTime, Datelike, FixedOffset, NaiveDate};
use std::path::PathBuf;
use uuid::Uuid;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).unwrap()
}

fn at(value: &str) -> DateTime<FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

fn unique_temp_db_path() -> PathBuf {
    std::env::temp_dir().join(format!(
        "calendar_app_created_following_{}.sqlite",
        Uuid::new_v4()
    ))
}

struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
        }
    }
}

fn recurrence_date(recurrence_id: &RecurrenceId) -> NaiveDate {
    match recurrence_id {
        RecurrenceId::AllDay(date) => *date,
        RecurrenceId::Timed {
            date_time,
            timezone,
        } => match timezone.as_deref() {
            Some("America/New_York") => date_time
                .with_timezone(&chrono_tz::America::New_York)
                .date_naive(),
            _ => date_time.date_naive(),
        },
    }
}

fn chips_from_agenda(groups: &[AgendaGroup]) -> Vec<&EventChip> {
    groups
        .iter()
        .filter_map(|group| match group {
            AgendaGroup::EventDay(day) => Some(day),
            AgendaGroup::EmptyRange { .. } => None,
        })
        .flat_map(|day| day.all_day.iter().chain(&day.timed))
        .collect()
}

#[test]
fn app_created_third_occurrence_chip_can_split_following_for_every_simple_form_shape() {
    let db_path = unique_temp_db_path();
    let _cleanup = TempDb(db_path.clone());
    let calendar = Calendar {
        id: Uuid::new_v4(),
        name: "Personal".to_owned(),
        color: "#3366cc".to_owned(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let cases = [
        (
            "daily-all-day-never",
            EventSchedule::AllDay {
                start_date: date(2026, 1, 31),
                end_date_exclusive: date(2026, 2, 1),
            },
            RecurrenceForm {
                frequency: Frequency::Daily,
                interval: 1,
                weekdays: vec![],
                end: EndCondition::Never,
            },
        ),
        (
            "daily-tzid-until",
            EventSchedule::Timed {
                start: at("2026-10-19T09:00:00-04:00"),
                end: at("2026-10-19T10:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            RecurrenceForm {
                frequency: Frequency::Daily,
                interval: 1,
                weekdays: vec![],
                end: EndCondition::Until(date(2026, 11, 2)),
            },
        ),
        (
            "weekly-tzid-multi-day-interval-count",
            EventSchedule::Timed {
                start: at("2026-10-19T09:00:00-04:00"),
                end: at("2026-10-19T10:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            RecurrenceForm {
                frequency: Frequency::Weekly,
                interval: 2,
                weekdays: vec![Weekday::Monday, Weekday::Wednesday],
                end: EndCondition::Count(6),
            },
        ),
        (
            "weekly-all-day-one-day-current-reproduction",
            EventSchedule::AllDay {
                start_date: date(2026, 7, 6),
                end_date_exclusive: date(2026, 7, 7),
            },
            RecurrenceForm {
                frequency: Frequency::Weekly,
                interval: 1,
                weekdays: vec![Weekday::Monday],
                end: EndCondition::Never,
            },
        ),
        (
            "monthly-all-day-count",
            EventSchedule::AllDay {
                start_date: date(2026, 1, 31),
                end_date_exclusive: date(2026, 2, 1),
            },
            RecurrenceForm {
                frequency: Frequency::Monthly,
                interval: 1,
                weekdays: vec![],
                end: EndCondition::Count(6),
            },
        ),
        (
            "yearly-tzid-never",
            EventSchedule::Timed {
                start: at("2026-10-19T09:00:00-04:00"),
                end: at("2026-10-19T10:00:00-04:00"),
                timezone: Some("America/New_York".to_owned()),
            },
            RecurrenceForm {
                frequency: Frequency::Yearly,
                interval: 1,
                weekdays: vec![],
                end: EndCondition::Never,
            },
        ),
    ];
    let normalized_event_id = Uuid::new_v4();

    {
        let mut repository = SqliteRepository::open(&db_path).expect("open isolated database");
        repository.save_calendar(&calendar).expect("save calendar");
        for (name, schedule, form) in &cases {
            let recurrence = recurrence_from_form(form, schedule)
                .expect("the app form is valid")
                .expect("repeating form creates recurrence");
            repository
                .save_event(&Event {
                    id: Uuid::new_v4(),
                    calendar_id: calendar.id,
                    title: (*name).to_owned(),
                    location: String::new(),
                    description: String::new(),
                    schedule: schedule.clone(),
                    recurrence: Some(recurrence),
                    reminders: vec![],
                })
                .expect("save app-created master");
        }

        let schedule = EventSchedule::AllDay {
            start_date: date(2026, 7, 6),
            end_date_exclusive: date(2026, 7, 7),
        };
        let app_recurrence = recurrence_from_form(
            &RecurrenceForm {
                frequency: Frequency::Weekly,
                interval: 1,
                weekdays: vec![Weekday::Monday],
                end: EndCondition::Count(6),
            },
            &schedule,
        )
        .expect("the app weekly form is valid")
        .expect("the app weekly form creates a recurrence");
        assert_eq!(
            app_recurrence.rrule,
            vec!["RRULE:FREQ=WEEKLY;BYDAY=MO;COUNT=6"],
            "the server-normalized resource must start with the app-created simple rule"
        );
        let normalized = map_icalendar_event(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:app-created-weekly-all-day\r\nSUMMARY:weekly-all-day-wkst-server-normalized\r\nDTSTART;VALUE=DATE:20260706\r\nDTEND;VALUE=DATE:20260707\r\nRRULE:FREQ=WEEKLY;WKST=MO;BYDAY=MO;COUNT=6\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
            normalized_event_id,
            calendar.id,
        )
        .expect("a CalDAV server may normalize the app-created weekly rule with WKST");
        assert_eq!(
            normalized.event.recurrence.as_ref().unwrap().rrule,
            vec!["RRULE:FREQ=WEEKLY;WKST=MO;BYDAY=MO;COUNT=6"],
            "the mapped server resource must retain its normalized simple rule"
        );
        repository
            .save_event(&normalized.event)
            .expect("save server-normalized app-created master");
    }

    let repository = SqliteRepository::open(&db_path).expect("reopen isolated database");
    let reloaded = repository.list_events_for_calendar(calendar.id);
    let pairs: Vec<_> = reloaded.into_iter().map(|event| (event, vec![])).collect();
    let viewer_timezone = chrono_tz::America::New_York;
    let agenda = project_agenda_range_with_detached_events_in_timezone(
        date(2026, 1, 1),
        date(2030, 1, 1),
        std::slice::from_ref(&calendar),
        &pairs,
        &viewer_timezone,
    );

    for (name, _, _) in cases {
        let master = pairs
            .iter()
            .find(|(event, _)| event.title == name)
            .unwrap()
            .0
            .clone();
        let occurrence_chips: Vec<_> = chips_from_agenda(&agenda)
            .into_iter()
            .filter(|chip| chip.event_id == master.id)
            .collect();
        for (ordinal, chip) in [
            ("second", occurrence_chips[1]),
            ("third", occurrence_chips[2]),
        ] {
            let recurrence_id = chip
                .original_recurrence_id
                .clone()
                .expect("generated chip carries the UI recurrence identity");
            let occurrence_date = recurrence_date(&recurrence_id);

            let month = project_month_with_detached_events_in_timezone(
                occurrence_date.year(),
                occurrence_date.month(),
                std::slice::from_ref(&calendar),
                &pairs,
                &viewer_timezone,
            );
            assert!(
                month
                    .iter()
                    .flat_map(|day| day.all_day.iter().chain(&day.timed))
                    .any(|projected| projected.event_id == master.id
                        && projected.original_recurrence_id.as_ref() == Some(&recurrence_id)),
                "{name} {ordinal}: month must pass the generated identity to the UI"
            );
            let week = project_week_with_detached_events_in_timezone(
                occurrence_date,
                std::slice::from_ref(&calendar),
                &pairs,
                &viewer_timezone,
            );
            assert!(
                week.iter()
                    .flat_map(|day| day.all_day.iter().chain(&day.timed))
                    .any(|projected| projected.event_id == master.id
                        && projected.original_recurrence_id.as_ref() == Some(&recurrence_id)),
                "{name} {ordinal}: week must pass the generated identity to the UI"
            );

            split_recurrence_at(&master, &recurrence_id).unwrap_or_else(|error| {
                panic!("{name} {ordinal}: generated UI chip must split: {error:?}")
            });
        }
    }

    let normalized_master = pairs
        .iter()
        .find(|(event, _)| event.id == normalized_event_id)
        .expect("server-normalized master must reload")
        .0
        .clone();
    let normalized_chips: Vec<_> = chips_from_agenda(&agenda)
        .into_iter()
        .filter(|chip| chip.event_id == normalized_master.id)
        .collect();
    let second_recurrence_id = normalized_chips[1]
        .original_recurrence_id
        .clone()
        .expect("second server-normalized chip carries the UI recurrence identity");
    split_recurrence_at(&normalized_master, &second_recurrence_id).unwrap_or_else(|error| {
        panic!(
            "weekly-all-day-wkst-server-normalized second: generated UI chip must split: {error:?}"
        )
    });
}
