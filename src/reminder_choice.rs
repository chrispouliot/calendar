use crate::model::{Event, EventSchedule, ReminderSpec};
use chrono::NaiveTime;

const HOUR: i64 = 60 * 60;

/// One entry of the reminder lists shared by the event editor and
/// Preferences.
///
/// Timed events offer offsets before the start. All-day events start at
/// midnight, so they offer wall-clock moments around the event day instead;
/// "day of" fires after the start and is stored as a negative offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReminderChoice {
    #[default]
    None,
    FiveMinutes,
    TenMinutes,
    FifteenMinutes,
    ThirtyMinutes,
    OneHour,
    OneDay,
    /// 9:00 AM on the day before an all-day event.
    DayBeforeMorning,
    /// 9:00 PM on the evening before an all-day event.
    EveningBefore,
    /// 9:00 AM on the day of an all-day event.
    DayOfMorning,
}

impl ReminderChoice {
    pub const TIMED: [ReminderChoice; 7] = [
        ReminderChoice::None,
        ReminderChoice::FiveMinutes,
        ReminderChoice::TenMinutes,
        ReminderChoice::FifteenMinutes,
        ReminderChoice::ThirtyMinutes,
        ReminderChoice::OneHour,
        ReminderChoice::OneDay,
    ];

    pub const ALL_DAY: [ReminderChoice; 4] = [
        ReminderChoice::None,
        ReminderChoice::DayBeforeMorning,
        ReminderChoice::EveningBefore,
        ReminderChoice::DayOfMorning,
    ];

    /// The choices offered for one schedule kind, in display order.
    pub fn list(all_day: bool) -> &'static [ReminderChoice] {
        if all_day {
            &Self::ALL_DAY
        } else {
            &Self::TIMED
        }
    }

    pub fn is_for(self, all_day: bool) -> bool {
        Self::list(all_day).contains(&self)
    }

    /// The label shown for this choice. Wall-clock times are rendered by the
    /// caller so the label follows the clock-format preference.
    pub fn label(self, format_time: impl Fn(NaiveTime) -> String) -> String {
        match self {
            ReminderChoice::None => "No reminder".to_owned(),
            ReminderChoice::FiveMinutes => "5 minutes before".to_owned(),
            ReminderChoice::TenMinutes => "10 minutes before".to_owned(),
            ReminderChoice::FifteenMinutes => "15 minutes before".to_owned(),
            ReminderChoice::ThirtyMinutes => "30 minutes before".to_owned(),
            ReminderChoice::OneHour => "1 hour before".to_owned(),
            ReminderChoice::OneDay => "1 day before".to_owned(),
            ReminderChoice::DayBeforeMorning => {
                format!("Day before at {}", format_time(nine_am()))
            }
            ReminderChoice::EveningBefore => {
                format!("Evening before at {}", format_time(nine_pm()))
            }
            ReminderChoice::DayOfMorning => format!("Day of at {}", format_time(nine_am())),
        }
    }

    /// Labels for one schedule kind, in display order.
    pub fn labels(all_day: bool, format_time: impl Fn(NaiveTime) -> String) -> Vec<String> {
        Self::list(all_day)
            .iter()
            .map(|choice| choice.label(&format_time))
            .collect()
    }

    /// Seconds before the event start, or `None` for no reminder. A negative
    /// value fires after the start.
    pub fn seconds_before_start(self) -> Option<i64> {
        match self {
            ReminderChoice::None => None,
            ReminderChoice::FiveMinutes => Some(5 * 60),
            ReminderChoice::TenMinutes => Some(10 * 60),
            ReminderChoice::FifteenMinutes => Some(15 * 60),
            ReminderChoice::ThirtyMinutes => Some(30 * 60),
            ReminderChoice::OneHour => Some(HOUR),
            ReminderChoice::OneDay => Some(24 * HOUR),
            ReminderChoice::DayBeforeMorning => Some(15 * HOUR),
            ReminderChoice::EveningBefore => Some(3 * HOUR),
            ReminderChoice::DayOfMorning => Some(-9 * HOUR),
        }
    }

    /// The preset of one schedule kind matching a stored offset, if any.
    pub fn from_seconds_before_start(all_day: bool, seconds: i64) -> Option<Self> {
        Self::list(all_day)
            .iter()
            .copied()
            .find(|choice| choice.seconds_before_start() == Some(seconds))
    }

    /// Position of this choice in the list for one schedule kind.
    pub fn index(self, all_day: bool) -> Option<u32> {
        Self::list(all_day)
            .iter()
            .position(|choice| *choice == self)
            .map(|position| position as u32)
    }

    pub fn from_index(all_day: bool, index: u32) -> Option<Self> {
        Self::list(all_day).get(index as usize).copied()
    }

    /// Stable token used in the preferences file.
    pub fn config_value(self) -> &'static str {
        match self {
            ReminderChoice::None => "none",
            ReminderChoice::FiveMinutes => "5m",
            ReminderChoice::TenMinutes => "10m",
            ReminderChoice::FifteenMinutes => "15m",
            ReminderChoice::ThirtyMinutes => "30m",
            ReminderChoice::OneHour => "1h",
            ReminderChoice::OneDay => "1d",
            ReminderChoice::DayBeforeMorning => "day-before-9am",
            ReminderChoice::EveningBefore => "evening-before-9pm",
            ReminderChoice::DayOfMorning => "day-of-9am",
        }
    }

    pub fn from_config_value(value: &str) -> Option<Self> {
        Self::TIMED
            .iter()
            .chain(&Self::ALL_DAY)
            .copied()
            .find(|choice| choice.config_value() == value)
    }

    /// The reminders an event should carry for this choice.
    pub fn reminders(self, title: &str) -> Vec<ReminderSpec> {
        self.seconds_before_start()
            .map(|seconds_before_start| {
                vec![ReminderSpec {
                    seconds_before_start,
                    description: format!("Reminder for {title}"),
                }]
            })
            .unwrap_or_default()
    }
}

fn nine_am() -> NaiveTime {
    NaiveTime::from_hms_opt(9, 0, 0).expect("09:00 is a valid time")
}

fn nine_pm() -> NaiveTime {
    NaiveTime::from_hms_opt(21, 0, 0).expect("21:00 is a valid time")
}

/// The reminder choices applied to newly created events.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefaultReminders {
    pub timed: ReminderChoice,
    pub all_day: ReminderChoice,
}

impl Default for DefaultReminders {
    fn default() -> Self {
        Self {
            timed: ReminderChoice::TenMinutes,
            all_day: ReminderChoice::None,
        }
    }
}

impl DefaultReminders {
    pub fn for_all_day(&self, all_day: bool) -> ReminderChoice {
        if all_day { self.all_day } else { self.timed }
    }

    pub fn for_schedule(&self, schedule: &EventSchedule) -> ReminderChoice {
        self.for_all_day(matches!(schedule, EventSchedule::AllDay { .. }))
    }

    /// Replace any choice that its schedule kind does not offer with the
    /// built-in default for that kind.
    pub fn normalized(self) -> Self {
        let fallback = Self::default();
        Self {
            timed: if self.timed.is_for(false) {
                self.timed
            } else {
                fallback.timed
            },
            all_day: if self.all_day.is_for(true) {
                self.all_day
            } else {
                fallback.all_day
            },
        }
    }

    /// Replace a new event's reminders with the default for its schedule.
    pub fn with_default_reminders(&self, mut event: Event) -> Event {
        event.reminders = self.for_schedule(&event.schedule).reminders(&event.title);
        event
    }
}
