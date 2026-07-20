// Public contract pinned by this acceptance test:
//
//     pub mod model {
//         use chrono::{DateTime, FixedOffset, NaiveDate};
//         use uuid::Uuid;
//
//         pub struct Calendar {
//             pub id: Uuid,
//             pub name: String,
//             pub color: String,
//             pub visible: bool,
//             pub read_only: bool,
//             pub source: CalendarSource,
//         }
//
//         pub enum CalendarSource {
//             Local,
//             // additional variants may exist; only Local is pinned here
//         }
//
//         pub struct Event {
//             pub id: Uuid,
//             pub calendar_id: Uuid,
//             pub title: String,
//             pub location: String,
//             pub description: String,
//             pub schedule: EventSchedule,
//             pub recurrence: Option<RecurrenceSpec>, // None == empty
//             pub reminders: Vec<ReminderSpec>,       // empty == empty
//         }
//
//         pub enum EventSchedule {
//             AllDay {
//                 start_date: NaiveDate,
//                 end_date_exclusive: NaiveDate,
//             },
//             Timed {
//                 start: DateTime<FixedOffset>,
//                 end: DateTime<FixedOffset>,
//                 timezone: Option<String>, // IANA name, e.g. "Europe/Berlin"
//             },
//         }
//
//         // Opaque placeholder types for recurrence/reminder bodies that
//         // later phases will flesh out. The test never inspects them; it
//         // just sets them to their "empty" form (None / empty vec).
//         pub struct RecurrenceSpec;
//         pub struct ReminderSpec;
//
//         pub struct DateTimeRange {
//             pub start: DateTime<FixedOffset>,
//             pub end: DateTime<FixedOffset>,
//         }
//
//         impl DateTimeRange {
//             pub fn new(
//                 start: DateTime<FixedOffset>,
//                 end: DateTime<FixedOffset>,
//             ) -> Result<Self, InvalidDateTimeRange>;
//
//             pub fn overlap(&self, other: &Self) -> RangeOverlap;
//         }
//
//         pub struct InvalidDateTimeRange;
//
//         pub enum RangeOverlap {
//             None,
//             Intersection,
//             Contains,    // self fully contains other
//             ContainedIn, // self is fully contained in other
//             Equal,
//         }
//     }
//
// Every value in the test is constructed from deterministic literals
// (fixed UUIDs and fixed chrono datetimes at a +02:00 fixed offset). The
// test does not read the clock, the locale, the filesystem, or any
// GTK/Adwaita state.

use calendar::model::{
    Calendar, CalendarSource, DateTimeRange, Event, EventSchedule, RangeOverlap,
};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

const TWO_HOURS_SECS: i32 = 2 * 3600;

fn at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap();
    FixedOffset::east_opt(TWO_HOURS_SECS)
        .unwrap()
        .from_utc_datetime(&naive)
}

#[test]
fn phase4_core_models_and_range_overlap() {
    // ----- Calendar: deterministic UUID + local source + visibility / read-only
    let cal_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let calendar = Calendar {
        id: cal_id,
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    assert_eq!(calendar.id, cal_id);
    assert_eq!(calendar.name, "Personal");
    assert_eq!(calendar.color, "#3366cc");
    assert!(calendar.visible);
    assert!(!calendar.read_only);
    assert!(matches!(calendar.source, CalendarSource::Local));

    // ----- Event with AllDay schedule: dates, not midnight timestamps
    let event_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let day = NaiveDate::from_ymd_opt(2026, 5, 15).unwrap();
    let next = NaiveDate::from_ymd_opt(2026, 5, 16).unwrap();
    let all_day = EventSchedule::AllDay {
        start_date: day,
        end_date_exclusive: next,
    };
    let event = Event {
        id: event_id,
        calendar_id: cal_id,
        title: "Holiday".to_string(),
        location: String::new(),
        description: "Bank holiday".to_string(),
        schedule: all_day,
        recurrence: None,
        reminders: Vec::new(),
    };
    assert_eq!(event.id, event_id);
    assert_eq!(event.calendar_id, cal_id);
    assert_eq!(event.title, "Holiday");
    assert!(event.location.is_empty());
    assert_eq!(event.description, "Bank holiday");
    assert!(event.recurrence.is_none());
    assert!(event.reminders.is_empty());
    match event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => {
            assert_eq!(start_date, day, "all-day start must remain a NaiveDate");
            assert_eq!(
                end_date_exclusive, next,
                "all-day end must remain a NaiveDate"
            );
        }
        other => panic!("all-day schedule must be stored as dates, not timestamps: got {other:?}"),
    }

    // ----- EventSchedule::Timed: FixedOffset timestamps + optional IANA timezone
    let t_start = at(2026, 7, 1, 9, 30);
    let t_end = at(2026, 7, 1, 10, 30);
    let timed = EventSchedule::Timed {
        start: t_start,
        end: t_end,
        timezone: Some("Europe/Berlin".to_string()),
    };
    match timed {
        EventSchedule::Timed {
            start,
            end,
            timezone,
        } => {
            assert_eq!(start, t_start);
            assert_eq!(end, t_end);
            assert_eq!(timezone.as_deref(), Some("Europe/Berlin"));
        }
        other => panic!("expected a Timed schedule, got {other:?}"),
    }

    // ----- DateTimeRange constructor: forward range is OK; equal / inverted are rejected
    let a = at(2026, 5, 1, 9, 0);
    let b = at(2026, 5, 1, 10, 0);
    let r = DateTimeRange::new(a, b).expect("forward range must build");
    assert_eq!(r.start, a);
    assert_eq!(r.end, b);
    assert!(
        DateTimeRange::new(b, b).is_err(),
        "zero-length range (end == start) must be rejected",
    );
    assert!(
        DateTimeRange::new(b, a).is_err(),
        "inverted range (end before start) must be rejected",
    );

    // ----- Boundary rule: [a, b) and [b, c) do NOT overlap
    let c = at(2026, 5, 1, 11, 0);
    let r1 = DateTimeRange::new(a, b).unwrap();
    let r2 = DateTimeRange::new(b, c).unwrap();
    assert!(
        matches!(r1.overlap(&r2), RangeOverlap::None),
        "ranges touching only at the boundary must not overlap",
    );
    assert!(
        matches!(r2.overlap(&r1), RangeOverlap::None),
        "boundary rule must be symmetric",
    );

    // ----- Genuine intersection: [1, 3) vs [2, 4) -> Intersection
    let p1 = at(2026, 5, 1, 1, 0);
    let p2 = at(2026, 5, 1, 3, 0);
    let p3 = at(2026, 5, 1, 2, 0);
    let p4 = at(2026, 5, 1, 4, 0);
    let rp1 = DateTimeRange::new(p1, p2).unwrap();
    let rp2 = DateTimeRange::new(p3, p4).unwrap();
    assert!(
        matches!(rp1.overlap(&rp2), RangeOverlap::Intersection),
        "partial overlap must be classified as Intersection",
    );
    assert!(
        matches!(rp2.overlap(&rp1), RangeOverlap::Intersection),
        "intersection must be symmetric",
    );

    // ----- Containment: outer [0, 5) contains inner [1, 4)
    let q0 = at(2026, 5, 1, 0, 0);
    let q1 = at(2026, 5, 1, 1, 0);
    let q4 = at(2026, 5, 1, 4, 0);
    let q5 = at(2026, 5, 1, 5, 0);
    let outer = DateTimeRange::new(q0, q5).unwrap();
    let inner = DateTimeRange::new(q1, q4).unwrap();
    assert!(
        matches!(outer.overlap(&inner), RangeOverlap::Contains),
        "outer.overlap(inner) must be Contains",
    );
    assert!(
        matches!(inner.overlap(&outer), RangeOverlap::ContainedIn),
        "inner.overlap(outer) must be ContainedIn",
    );

    // ----- Equality: [a, b) vs [a, b) -> Equal
    let eq1 = DateTimeRange::new(a, b).unwrap();
    let eq2 = DateTimeRange::new(a, b).unwrap();
    assert!(
        matches!(eq1.overlap(&eq2), RangeOverlap::Equal),
        "two identical ranges must be classified as Equal",
    );
}
