use crate::time_format::{TimeFormatPreference, format_time as format_wall_time_pure};
use chrono::NaiveTime;
use gtk::gio::prelude::SettingsExt;
use gtk::{gio, glib};
use std::fs;
use std::path::PathBuf;

const SETTINGS_SCHEMA: &str = "org.gnome.desktop.interface";
const SETTINGS_KEY: &str = "clock-format";

fn preference_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push("dev.chris.calendar");
    path.push("preferences.conf");
    path
}

/// Load the app-owned choice. An absent or malformed file means System.
pub fn load_time_format_preference() -> TimeFormatPreference {
    let Ok(contents) = fs::read_to_string(preference_path()) else {
        return TimeFormatPreference::System;
    };
    match contents
        .lines()
        .find_map(|line| line.strip_prefix("time-format="))
    {
        Some("system") => TimeFormatPreference::System,
        Some("12-hour") => TimeFormatPreference::TwelveHour,
        Some("24-hour") => TimeFormatPreference::TwentyFourHour,
        _ => TimeFormatPreference::System,
    }
}

/// Persist only the app preference; the desktop setting is never modified.
pub fn save_time_format_preference(preference: TimeFormatPreference) {
    let path = preference_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let value = match preference {
        TimeFormatPreference::System => "system",
        TimeFormatPreference::TwelveHour => "12-hour",
        TimeFormatPreference::TwentyFourHour => "24-hour",
    };
    let _ = fs::write(path, format!("time-format={value}\n"));
}

/// Read GNOME's clock-format only when its schema and key are available.
/// Unknown values deliberately use the application's safe 12-hour fallback.
pub fn system_clock_format() -> String {
    let Some(source) = gio::SettingsSchemaSource::default() else {
        return "12h".to_owned();
    };
    let Some(schema) = source.lookup(SETTINGS_SCHEMA, true) else {
        return "12h".to_owned();
    };
    if !schema.has_key(SETTINGS_KEY) {
        return "12h".to_owned();
    }
    let settings = gio::Settings::new_full(&schema, None::<&gio::SettingsBackend>, None);
    let value = settings.string(SETTINGS_KEY);
    match value.as_str() {
        "12h" | "24h" => value.to_string(),
        _ => "12h".to_owned(),
    }
}

/// Resolve System Default to a concrete display mode for the current process.
pub fn resolved_time_format() -> TimeFormatPreference {
    match load_time_format_preference() {
        TimeFormatPreference::System => match system_clock_format().as_str() {
            "24h" => TimeFormatPreference::TwentyFourHour,
            _ => TimeFormatPreference::TwelveHour,
        },
        preference => preference,
    }
}

/// Format a local wall-clock value using the current persisted app choice.
pub fn format_wall_time(time: NaiveTime) -> String {
    format_wall_time_pure(time, load_time_format_preference(), &system_clock_format())
}
