use chrono::NaiveTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeFormatPreference {
    System,
    TwelveHour,
    TwentyFourHour,
}

/// Format a wall-clock time according to the requested preference.
///
/// The system value is supplied by the caller so formatting remains a pure
/// operation and does not depend on GSettings or the process environment.
pub fn format_time(
    time: NaiveTime,
    preference: TimeFormatPreference,
    system_clock_format: &str,
) -> String {
    let twelve_hour = match preference {
        TimeFormatPreference::System => system_clock_format == "12h",
        TimeFormatPreference::TwelveHour => true,
        TimeFormatPreference::TwentyFourHour => false,
    };
    let format = if twelve_hour { "%I:%M %p" } else { "%H:%M" };

    let formatted = time.format(format).to_string();
    if twelve_hour {
        formatted.strip_prefix('0').unwrap_or(&formatted).to_owned()
    } else {
        formatted
    }
}
