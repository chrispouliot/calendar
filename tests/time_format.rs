// Public contract pinned by this acceptance test:
//
//     pub mod time_format {
//         use chrono::NaiveTime;
//
//         #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//         pub enum TimeFormatPreference {
//             System,
//             TwelveHour,
//             TwentyFourHour,
//         }
//
//         /// Format a wall-clock time using the app preference. `system_clock_format`
//         /// is the caller-provided GNOME `clock-format` value (such as `"12h"`
//         /// or `"24h"`), so this pure helper never reads GSettings itself.
//         pub fn format_time(
//             time: NaiveTime,
//             preference: TimeFormatPreference,
//             system_clock_format: &str,
//         ) -> String;
//     }
//
// The test uses only fixed values and an injected system preference; it has no
// GTK initialization, GSettings, locale, or clock dependency.

use calendar::time_format::{TimeFormatPreference, format_time};
use chrono::NaiveTime;

fn time(hour: u32, minute: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, minute, 0).expect("test fixture must be a valid time")
}

#[test]
fn formats_clock_times_for_explicit_and_injected_system_preferences() {
    let midnight = time(0, 5);
    let noon = time(12, 5);
    let afternoon = time(14, 30);

    for (value, expected) in [
        (midnight, "12:05 AM"),
        (noon, "12:05 PM"),
        (afternoon, "2:30 PM"),
    ] {
        assert_eq!(
            format_time(value, TimeFormatPreference::TwelveHour, "24h"),
            expected,
            "the explicit 12-hour preference must override the injected system value",
        );
    }

    for (value, expected) in [(midnight, "00:05"), (noon, "12:05"), (afternoon, "14:30")] {
        assert_eq!(
            format_time(value, TimeFormatPreference::TwentyFourHour, "12h"),
            expected,
            "the explicit 24-hour preference must override the injected system value",
        );
    }

    assert_eq!(
        format_time(midnight, TimeFormatPreference::System, "12h"),
        "12:05 AM",
        "the system preference must honor an injected GNOME 12h value",
    );
    assert_eq!(
        format_time(afternoon, TimeFormatPreference::System, "24h"),
        "14:30",
        "the system preference must honor an injected GNOME 24h value",
    );
}
