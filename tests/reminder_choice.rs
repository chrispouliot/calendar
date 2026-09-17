// Pure contract for the reminder lists shared by the event editor and the
// Preferences dialog, and for the defaults applied to newly created events.
// No GTK, filesystem, or clock is involved.

use calendar::model::{Event, EventSchedule, ReminderSpec};
use calendar::reminder_choice::{DefaultReminders, ReminderChoice};
use calendar::time_format::{TimeFormatPreference, format_time};
use chrono::{FixedOffset, NaiveDate, NaiveTime, TimeZone};
use uuid::Uuid;

fn twelve_hour(time: NaiveTime) -> String {
    format_time(time, TimeFormatPreference::TwelveHour, "24h")
}

fn twenty_four_hour(time: NaiveTime) -> String {
    format_time(time, TimeFormatPreference::TwentyFourHour, "12h")
}

fn all_day_event(title: &str) -> Event {
    let start = NaiveDate::from_ymd_opt(2026, 9, 17).unwrap();
    Event {
        id: Uuid::nil(),
        calendar_id: Uuid::nil(),
        title: title.to_owned(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: start,
            end_date_exclusive: start.succ_opt().unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    }
}

fn timed_event(title: &str) -> Event {
    let offset = FixedOffset::east_opt(0).unwrap();
    Event {
        schedule: EventSchedule::Timed {
            start: offset.with_ymd_and_hms(2026, 9, 17, 9, 0, 0).unwrap(),
            end: offset.with_ymd_and_hms(2026, 9, 17, 10, 0, 0).unwrap(),
            timezone: None,
        },
        ..all_day_event(title)
    }
}

#[test]
fn timed_and_all_day_lists_have_their_own_labels_in_display_order() {
    assert_eq!(
        ReminderChoice::labels(false, twelve_hour),
        vec![
            "No reminder",
            "5 minutes before",
            "10 minutes before",
            "15 minutes before",
            "30 minutes before",
            "1 hour before",
            "1 day before",
        ]
    );
    assert_eq!(
        ReminderChoice::labels(true, twelve_hour),
        vec![
            "No reminder",
            "Day before at 9:00 AM",
            "Evening before at 9:00 PM",
            "Day of at 9:00 AM",
        ]
    );
    assert_eq!(
        ReminderChoice::labels(true, twenty_four_hour),
        vec![
            "No reminder",
            "Day before at 09:00",
            "Evening before at 21:00",
            "Day of at 09:00",
        ],
        "all-day labels must follow the clock-format preference",
    );
}

#[test]
fn choices_round_trip_their_index_config_token_and_offset_within_their_kind() {
    for all_day in [false, true] {
        for (position, choice) in ReminderChoice::list(all_day).iter().enumerate() {
            assert!(choice.is_for(all_day));
            assert_eq!(choice.index(all_day), Some(position as u32));
            assert_eq!(
                ReminderChoice::from_index(all_day, position as u32),
                Some(*choice)
            );
            assert_eq!(
                ReminderChoice::from_config_value(choice.config_value()),
                Some(*choice),
                "config token must round-trip for {choice:?}",
            );
            if let Some(seconds) = choice.seconds_before_start() {
                assert_eq!(
                    ReminderChoice::from_seconds_before_start(all_day, seconds),
                    Some(*choice)
                );
            }
        }
        assert_eq!(
            ReminderChoice::from_index(all_day, ReminderChoice::list(all_day).len() as u32),
            None
        );
    }

    // No reminder belongs to both lists; every other preset to exactly one.
    assert!(ReminderChoice::None.is_for(false) && ReminderChoice::None.is_for(true));
    assert!(!ReminderChoice::TenMinutes.is_for(true));
    assert_eq!(ReminderChoice::TenMinutes.index(true), None);
    assert!(!ReminderChoice::DayOfMorning.is_for(false));
    assert_eq!(ReminderChoice::from_seconds_before_start(true, 600), None);
    assert_eq!(
        ReminderChoice::from_seconds_before_start(false, -9 * 60 * 60),
        None
    );
    assert_eq!(ReminderChoice::from_config_value("tomorrow"), None);
}

#[test]
fn all_day_presets_are_wall_clock_moments_relative_to_midnight() {
    assert_eq!(
        ReminderChoice::DayBeforeMorning.seconds_before_start(),
        Some(15 * 60 * 60),
        "9 AM the day before is 15 hours before midnight",
    );
    assert_eq!(
        ReminderChoice::EveningBefore.seconds_before_start(),
        Some(3 * 60 * 60),
        "9 PM the evening before is 3 hours before midnight",
    );
    assert_eq!(
        ReminderChoice::DayOfMorning.seconds_before_start(),
        Some(-9 * 60 * 60),
        "9 AM on the day is 9 hours after midnight, stored as a negative offset",
    );
    assert_eq!(ReminderChoice::None.seconds_before_start(), None);
    assert_eq!(ReminderChoice::TenMinutes.seconds_before_start(), Some(600));
}

#[test]
fn choices_build_the_same_reminder_specs_the_editor_saves() {
    assert!(ReminderChoice::None.reminders("Standup").is_empty());
    assert_eq!(
        ReminderChoice::TenMinutes.reminders("Standup"),
        vec![ReminderSpec {
            seconds_before_start: 600,
            description: "Reminder for Standup".to_owned(),
        }]
    );
    assert_eq!(
        ReminderChoice::DayOfMorning.reminders("Holiday"),
        vec![ReminderSpec {
            seconds_before_start: -9 * 60 * 60,
            description: "Reminder for Holiday".to_owned(),
        }]
    );
}

#[test]
fn built_in_defaults_are_ten_minutes_for_timed_and_none_for_all_day() {
    let defaults = DefaultReminders::default();
    assert_eq!(defaults.timed, ReminderChoice::TenMinutes);
    assert_eq!(defaults.all_day, ReminderChoice::None);
    assert_eq!(defaults.for_all_day(false), ReminderChoice::TenMinutes);
    assert_eq!(defaults.for_all_day(true), ReminderChoice::None);
}

#[test]
fn normalizing_replaces_choices_of_the_wrong_kind_with_built_in_defaults() {
    let mismatched = DefaultReminders {
        timed: ReminderChoice::DayOfMorning,
        all_day: ReminderChoice::FiveMinutes,
    };
    assert_eq!(mismatched.normalized(), DefaultReminders::default());

    let valid = DefaultReminders {
        timed: ReminderChoice::OneHour,
        all_day: ReminderChoice::EveningBefore,
    };
    assert_eq!(valid.normalized(), valid);
}

#[test]
fn new_events_receive_the_default_matching_their_schedule_kind() {
    let defaults = DefaultReminders {
        timed: ReminderChoice::ThirtyMinutes,
        all_day: ReminderChoice::DayOfMorning,
    };

    let timed = defaults.with_default_reminders(timed_event("Dentist"));
    assert_eq!(
        timed.reminders,
        vec![ReminderSpec {
            seconds_before_start: 30 * 60,
            description: "Reminder for Dentist".to_owned(),
        }]
    );

    let all_day = defaults.with_default_reminders(all_day_event("Holiday"));
    assert_eq!(
        all_day.reminders,
        vec![ReminderSpec {
            seconds_before_start: -9 * 60 * 60,
            description: "Reminder for Holiday".to_owned(),
        }]
    );

    let none = DefaultReminders {
        timed: ReminderChoice::None,
        all_day: ReminderChoice::None,
    };
    assert!(
        none.with_default_reminders(timed_event("Quiet"))
            .reminders
            .is_empty()
    );
}
