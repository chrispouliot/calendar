use calendar::viewer_time::timezone_for;
use chrono::DateTime;

fn parse(value: &str) -> DateTime<chrono::FixedOffset> {
    DateTime::parse_from_rfc3339(value).unwrap()
}

#[test]
fn local_wall_clock_times_take_the_named_timezone() {
    let start = parse("2026-07-08T11:00:00+02:00");
    let end = parse("2026-07-08T12:00:00+02:00");
    assert_eq!(
        timezone_for(&start, &end, "Europe/Berlin").as_deref(),
        Some("Europe/Berlin")
    );
}

#[test]
fn utc_system_zone_keeps_utc_serialization() {
    let start = parse("2026-07-08T11:00:00+00:00");
    let end = parse("2026-07-08T12:00:00+00:00");
    assert_eq!(timezone_for(&start, &end, "UTC"), None);
    assert_eq!(timezone_for(&start, &end, "Etc/UTC"), None);
}

#[test]
fn unknown_or_mismatched_zone_falls_back_to_utc() {
    let start = parse("2026-07-08T11:00:00+00:00");
    let end = parse("2026-07-08T12:00:00+00:00");
    assert_eq!(timezone_for(&start, &end, "Europe/Berlin"), None);
    assert_eq!(timezone_for(&start, &end, "Not/AZone"), None);
}

#[test]
fn ambiguous_fall_back_time_falls_back_to_utc() {
    // 02:30 happens twice in Berlin on 2026-10-25.
    let start = parse("2026-10-25T02:30:00+01:00");
    let end = parse("2026-10-25T03:30:00+01:00");
    assert_eq!(timezone_for(&start, &end, "Europe/Berlin"), None);
}
