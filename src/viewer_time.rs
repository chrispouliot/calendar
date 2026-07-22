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
