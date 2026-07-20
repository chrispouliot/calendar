// Public contract pinned by this acceptance test:
//
//     pub mod weeks_buffer {
//         use chrono::NaiveDate;
//
//         /// Total rows in the buffer: 5 above + 5 visible + 5 below.
//         pub const TOTAL_ROWS: usize = 15;
//         /// First visible row index (inclusive).
//         pub const VISIBLE_START: usize = 5;
//         /// Row after the last visible row (exclusive).
//         pub const VISIBLE_END: usize = 10;
//
//         /// A pure, deterministic buffer of 15 consecutive Monday-first
//         /// week rows. Construction takes the Monday on which the
//         /// first visible row begins. Each row starts seven days
//         /// after the previous and exposes seven consecutive
//         /// NaiveDates, Monday-first. The buffer is shifted in
//         /// whole-week increments; positive shifts slide the buffer
//         /// forward in time, negative shifts slide it backward,
//         /// without gaps or duplicates in the union of observed row
//         /// starts across a sequence of shifts.
//         ///
//         /// The type is pure: it must not read the clock, the
//         /// locale, the filesystem, or any GTK/Adwaita state.
//         #[derive(Debug, Clone, PartialEq, Eq)]
//         pub struct WeeksBuffer { /* private fields */ }
//
//         impl WeeksBuffer {
//             /// Build a buffer whose first visible row begins on
//             /// `first_visible_monday`. The caller is responsible
//             /// for passing a date that is a Monday; the
//             /// constructor does not validate.
//             pub fn new(first_visible_monday: NaiveDate) -> Self;
//
//             /// The Monday on which the first visible row begins.
//             pub fn first_visible_monday(&self) -> NaiveDate;
//
//             /// The Monday on which `row` (0..TOTAL_ROWS) begins.
//             pub fn row_start(&self, row: usize) -> NaiveDate;
//
//             /// The seven consecutive NaiveDates filling `row`
//             /// (0..TOTAL_ROWS), Monday-first.
//             pub fn row_dates(&self, row: usize) -> [NaiveDate; 7];
//
//             /// Slide the buffer by `weeks` rows. `weeks > 0`
//             /// moves the buffer forward in time (a new row
//             /// appears at the bottom); `weeks < 0` moves it
//             /// backward. The buffer slides monotonically: across
//             /// a sequence of shifts the union of observed row
//             /// starts is a contiguous run of Mondays with no
//             /// gaps and no duplicates.
//             pub fn shift_weeks(&mut self, weeks: i32);
//         }
//     }
//
// Every value in this test is constructed from deterministic chrono
// literals. The test does not read the clock, the locale, the
// filesystem, or any GTK/Adwaita state. Each `WeeksBuffer` is
// freshly constructed inside this test and is not shared with any
// other test.

use calendar::weeks_buffer::{TOTAL_ROWS, VISIBLE_END, VISIBLE_START, WeeksBuffer};
use chrono::{Datelike, Duration, NaiveDate, Weekday};

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("test fixture must be a valid date")
}

#[test]
fn phase6_weeks_buffer_construction_and_shifts() {
    // Deterministic first-visible Monday: 2026-01-05.
    let monday = date(2026, 1, 5);
    assert_eq!(
        monday.weekday(),
        Weekday::Mon,
        "test fixture must be a Monday"
    );

    let buf = WeeksBuffer::new(monday);

    // 1) Buffer shape: 15 rows total, visible window is 5..10.
    assert_eq!(TOTAL_ROWS, 15, "buffer must hold 15 rows total");
    assert_eq!(VISIBLE_START, 5, "visible range must start at row 5");
    assert_eq!(
        VISIBLE_END, 10,
        "visible range must end at row 10 (exclusive)"
    );
    assert_eq!(
        VISIBLE_END - VISIBLE_START,
        5,
        "visible range must be 5 rows wide"
    );

    // 2) First visible row begins on the construction Monday.
    assert_eq!(
        buf.first_visible_monday(),
        monday,
        "first_visible_monday must be the construction value"
    );
    assert_eq!(
        buf.row_start(VISIBLE_START),
        monday,
        "row at VISIBLE_START must begin on first_visible_monday"
    );

    // 3) Every row is exactly 7 days after the previous and exposes
    //    7 consecutive Monday-first NaiveDates.
    for row in 0..TOTAL_ROWS {
        let offset_days = 7 * (row as i64 - VISIBLE_START as i64);
        let expected_start = monday + Duration::days(offset_days);
        assert_eq!(
            buf.row_start(row),
            expected_start,
            "row {row} must start {offset_days} days from first_visible_monday"
        );
        let dates = buf.row_dates(row);
        assert_eq!(dates.len(), 7, "row {row} must expose 7 dates");
        for (i, d) in dates.iter().enumerate() {
            assert_eq!(
                *d,
                expected_start + Duration::days(i as i64),
                "row {row} day index {i} must be consecutive"
            );
        }
    }

    // 4) Buffered rows above and below the visible window.
    assert_eq!(
        buf.row_start(0),
        monday - Duration::days(5 * 7),
        "top buffer row 0 must start 5 weeks before the visible first row"
    );
    let bottom_offset_days = (TOTAL_ROWS - 1 - VISIBLE_START) as i64 * 7;
    assert_eq!(
        buf.row_start(TOTAL_ROWS - 1),
        monday + Duration::days(bottom_offset_days),
        "bottom buffer row {} must start {} weeks after the visible first row",
        TOTAL_ROWS - 1,
        bottom_offset_days / 7
    );

    // 5) Shift forward by one week: first_visible_monday advances by
    //    7 days; a new consecutive row appears at the bottom.
    let mut buf = WeeksBuffer::new(monday);
    let bottom_before = buf.row_start(TOTAL_ROWS - 1);
    buf.shift_weeks(1);
    assert_eq!(
        buf.first_visible_monday(),
        monday + Duration::days(7),
        "shift_weeks(+1) must advance first_visible_monday by 7 days"
    );
    assert_eq!(
        buf.row_start(TOTAL_ROWS - 1),
        bottom_before + Duration::days(7),
        "shift_weeks(+1) must produce a new bottom row one week after the previous bottom"
    );

    // 6) Shift backward by one week reverses a forward shift.
    buf.shift_weeks(-1);
    assert_eq!(
        buf.first_visible_monday(),
        monday,
        "shift_weeks(-1) after shift_weeks(+1) must restore the original Monday"
    );
    for row in 0..TOTAL_ROWS {
        let expected = monday + Duration::days(7 * (row as i64 - VISIBLE_START as i64));
        assert_eq!(
            buf.row_start(row),
            expected,
            "row {row} must be back to its original start after -1"
        );
    }

    // 7) Multi-week shift crosses a year boundary: 52 weeks forward
    //    from 2026-01-05 lands 364 days later, on 2027-01-04 (a
    //    Monday).
    let mut buf = WeeksBuffer::new(monday);
    buf.shift_weeks(52);
    let expected_after_52 = monday + Duration::days(52 * 7);
    assert_eq!(
        buf.first_visible_monday(),
        expected_after_52,
        "shift_weeks(+52) must advance by exactly 52 weeks"
    );
    assert_eq!(
        expected_after_52.weekday(),
        Weekday::Mon,
        "52 weeks after a Monday must still be a Monday"
    );
    assert_eq!(
        expected_after_52,
        date(2027, 1, 4),
        "52 weeks after 2026-01-05 must be 2027-01-04"
    );

    // 8) Multi-week shift is involutive: +N then -N restores the
    //    original buffer exactly.
    let mut buf = WeeksBuffer::new(monday);
    buf.shift_weeks(8);
    buf.shift_weeks(-8);
    assert_eq!(
        buf.first_visible_monday(),
        monday,
        "shift_weeks(+8) then shift_weeks(-8) must restore the original Monday"
    );
    for row in 0..TOTAL_ROWS {
        let expected = monday + Duration::days(7 * (row as i64 - VISIBLE_START as i64));
        assert_eq!(
            buf.row_start(row),
            expected,
            "row {row} must be back to its original start after +8/-8"
        );
    }

    // 9) Sliding the buffer over 105 buffer positions (steps 0
    //    through 104) yields a contiguous, duplicate-free run of
    //    Mondays — no gaps, no double-counting — across month and
    //    year boundaries.
    //
    //    At step k the buffer's first visible Monday is
    //    `monday + k*7`, and its rows span offsets `-VISIBLE_START`
    //    through `TOTAL_ROWS - 1 - VISIBLE_START` relative to that
    //    first visible Monday. The union of observed row starts
    //    across steps 0..=104 is therefore the contiguous interval
    //    [monday - VISIBLE_START*7,
    //     monday + (104 + TOTAL_ROWS - 1 - VISIBLE_START)*7]
    //    of `104 + TOTAL_ROWS` distinct Mondays (119 for the pinned
    //    constants).
    let mut buf = WeeksBuffer::new(monday);
    let mut seen: Vec<NaiveDate> = Vec::new();
    for _ in 0..=104 {
        for row in 0..TOTAL_ROWS {
            seen.push(buf.row_start(row));
        }
        buf.shift_weeks(1);
    }
    let mut sorted = seen.clone();
    sorted.sort();
    let mut unique: Vec<NaiveDate> = Vec::with_capacity(sorted.len());
    for d in &sorted {
        if unique.last() != Some(d) {
            unique.push(*d);
        }
    }
    let span_days = (*unique.last().unwrap() - *unique.first().unwrap()).num_days();
    assert_eq!(
        span_days % 7,
        0,
        "extreme observed Mondays must be an exact number of weeks apart"
    );
    assert_eq!(
        unique.len() as i64,
        span_days / 7 + 1,
        "the union of observed Mondays must be a contiguous run (no gaps, no duplicates)"
    );
    let first_expected = monday - Duration::days(VISIBLE_START as i64 * 7);
    let last_expected = monday + Duration::days((104 + TOTAL_ROWS - 1 - VISIBLE_START) as i64 * 7);
    assert_eq!(
        *unique.first().unwrap(),
        first_expected,
        "first observed Monday must be VISIBLE_START weeks before the construction Monday"
    );
    assert_eq!(
        *unique.last().unwrap(),
        last_expected,
        "last observed Monday must be {} weeks past the construction Monday",
        (104 + TOTAL_ROWS - 1 - VISIBLE_START) as i64
    );
}
