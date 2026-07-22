// Public contract pinned by this acceptance test:
//
//     pub mod viewer_time {
//         use chrono::{DateTime, FixedOffset};
//
//         pub fn to_local_fixed(value: &DateTime<FixedOffset>) -> DateTime<FixedOffset>;
//     }

use calendar::viewer_time::to_local_fixed;
use chrono::{DateTime, FixedOffset};
use gtk::glib;

#[test]
fn converts_fixed_utc_instant_to_system_local_offset_without_losing_precision() {
    let original = DateTime::from_timestamp(1_784_954_096, 123_456_789)
        .expect("fixed test timestamp must be valid")
        .fixed_offset();
    let glib_local = glib::DateTime::from_unix_local(original.timestamp())
        .expect("GLib must resolve the system-local time for a valid Unix timestamp");
    let expected_offset = FixedOffset::east_opt(
        i32::try_from(glib_local.utc_offset().as_seconds())
            .expect("GLib UTC offset must fit chrono's FixedOffset range"),
    )
    .expect("GLib UTC offset must be a valid chrono FixedOffset");

    let local = to_local_fixed(&original);

    assert_eq!(local.timestamp(), original.timestamp());
    assert_eq!(
        local.timestamp_subsec_nanos(),
        original.timestamp_subsec_nanos()
    );
    assert_eq!(local.offset(), &expected_offset);
}
