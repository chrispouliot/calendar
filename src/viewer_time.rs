use chrono::{DateTime, FixedOffset, Utc};
use gtk::glib;

/// Convert an instant to the system-local fixed offset used by the viewer.
pub fn to_local_fixed(value: &DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    let Some(local) = glib::DateTime::from_unix_local(value.timestamp()).ok() else {
        return *value;
    };
    let Ok(offset_seconds) = i32::try_from(local.utc_offset().as_seconds()) else {
        return *value;
    };
    let Some(offset) = FixedOffset::east_opt(offset_seconds) else {
        return *value;
    };
    value.with_timezone(&offset)
}

/// Return the current instant with its system-local fixed offset.
pub fn now_local_fixed() -> DateTime<FixedOffset> {
    let Some(now) = glib::DateTime::now_local().ok() else {
        return Utc::now().fixed_offset();
    };
    let Some(value) = DateTime::from_timestamp(now.to_unix(), (now.microsecond() * 1_000) as u32)
    else {
        return Utc::now().fixed_offset();
    };
    to_local_fixed(&value.fixed_offset())
}

/// Return the system-local IANA timezone identifier (for example
/// `Europe/London`) when GLib reports one that chrono-tz recognizes.
pub fn local_timezone_id() -> Option<String> {
    let identifier = glib::TimeZone::local().identifier();
    let identifier = identifier.trim_start_matches(':');
    identifier
        .parse::<chrono_tz::Tz>()
        .ok()
        .map(|timezone| timezone.name().to_owned())
}

/// Return the system-local timezone to attach to a new timed event, or
/// `None` to store and upload it as UTC. See [`timezone_for`].
pub fn local_timezone_for(
    start: &DateTime<FixedOffset>,
    end: &DateTime<FixedOffset>,
) -> Option<String> {
    timezone_for(start, end, &local_timezone_id()?)
}

/// Return `tzid` when it names a non-UTC zone in which both endpoints resolve
/// unambiguously with the offsets they already carry. Otherwise `None`,
/// meaning the event is stored and uploaded as UTC.
pub fn timezone_for(
    start: &DateTime<FixedOffset>,
    end: &DateTime<FixedOffset>,
    tzid: &str,
) -> Option<String> {
    let timezone = tzid.parse::<chrono_tz::Tz>().ok()?;
    if matches!(timezone, chrono_tz::UTC | chrono_tz::Etc::UTC) {
        return None;
    }
    [start, end]
        .into_iter()
        .all(|value| fits_timezone(value, timezone))
        .then(|| timezone.name().to_owned())
}

fn fits_timezone(value: &DateTime<FixedOffset>, timezone: chrono_tz::Tz) -> bool {
    use chrono::{Offset, TimeZone};
    timezone
        .from_local_datetime(&value.naive_local())
        .single()
        .is_some_and(|resolved| resolved.offset().fix() == *value.offset())
}
