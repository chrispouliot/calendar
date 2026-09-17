use crate::reminder_choice::{DefaultReminders, ReminderChoice};
use crate::time_format::{TimeFormatPreference, format_time as format_wall_time_pure};
use chrono::NaiveTime;
use gtk::gio::prelude::SettingsExt;
use gtk::{gio, glib};
use std::fs;
use std::path::PathBuf;

const SETTINGS_SCHEMA: &str = "org.gnome.desktop.interface";
const SETTINGS_KEY: &str = "clock-format";

const TIME_FORMAT_KEY: &str = "time-format";
const TIMED_REMINDER_KEY: &str = "default-reminder-timed";
const ALL_DAY_REMINDER_KEY: &str = "default-reminder-all-day";

fn preference_path() -> PathBuf {
    let mut path = glib::user_config_dir();
    path.push("dev.chris.calendar");
    path.push("preferences.conf");
    path
}

/// Parse the `key=value` lines of the preferences file. Blank lines and lines
/// without `=` are ignored; a repeated key keeps its first value.
pub fn parse_preference_entries(contents: &str) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || entries.iter().any(|(existing, _)| existing == key) {
            continue;
        }
        entries.push((key.to_owned(), value.trim().to_owned()));
    }
    entries
}

/// Set `key` to `value`, replacing an existing entry in place so unrelated
/// preferences survive every save.
pub fn upsert_preference_entry(entries: &mut Vec<(String, String)>, key: &str, value: &str) {
    match entries.iter_mut().find(|(existing, _)| existing == key) {
        Some(entry) => entry.1 = value.to_owned(),
        None => entries.push((key.to_owned(), value.to_owned())),
    }
}

/// Render entries back into the `key=value` file format.
pub fn render_preference_entries(entries: &[(String, String)]) -> String {
    entries
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}

fn read_entries() -> Vec<(String, String)> {
    fs::read_to_string(preference_path())
        .map(|contents| parse_preference_entries(&contents))
        .unwrap_or_default()
}

fn read_entry<'a>(entries: &'a [(String, String)], key: &str) -> Option<&'a str> {
    entries
        .iter()
        .find(|(existing, _)| existing == key)
        .map(|(_, value)| value.as_str())
}

fn write_entries(updates: &[(&str, &str)]) {
    let path = preference_path();
    let Some(parent) = path.parent() else {
        return;
    };
    if fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut entries = read_entries();
    for (key, value) in updates {
        upsert_preference_entry(&mut entries, key, value);
    }
    let _ = fs::write(path, render_preference_entries(&entries));
}

/// Load the app-owned choice. An absent or malformed file means System.
pub fn load_time_format_preference() -> TimeFormatPreference {
    match read_entry(&read_entries(), TIME_FORMAT_KEY) {
        Some("system") => TimeFormatPreference::System,
        Some("12-hour") => TimeFormatPreference::TwelveHour,
        Some("24-hour") => TimeFormatPreference::TwentyFourHour,
        _ => TimeFormatPreference::System,
    }
}

/// Persist only the app preference; the desktop setting is never modified.
pub fn save_time_format_preference(preference: TimeFormatPreference) {
    let value = match preference {
        TimeFormatPreference::System => "system",
        TimeFormatPreference::TwelveHour => "12-hour",
        TimeFormatPreference::TwentyFourHour => "24-hour",
    };
    write_entries(&[(TIME_FORMAT_KEY, value)]);
}

/// Load the reminder choices applied to new events. A missing or
/// unrecognised value falls back to the built-in default for that kind.
pub fn load_default_reminders() -> DefaultReminders {
    let entries = read_entries();
    let fallback = DefaultReminders::default();
    let choice = |key: &str, fallback: ReminderChoice| {
        read_entry(&entries, key)
            .and_then(ReminderChoice::from_config_value)
            .unwrap_or(fallback)
    };
    DefaultReminders {
        timed: choice(TIMED_REMINDER_KEY, fallback.timed),
        all_day: choice(ALL_DAY_REMINDER_KEY, fallback.all_day),
    }
    .normalized()
}

pub fn save_default_reminders(defaults: DefaultReminders) {
    write_entries(&[
        (TIMED_REMINDER_KEY, defaults.timed.config_value()),
        (ALL_DAY_REMINDER_KEY, defaults.all_day.config_value()),
    ]);
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
