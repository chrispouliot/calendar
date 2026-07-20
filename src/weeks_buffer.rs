use chrono::{Duration, NaiveDate};

/// Total rows in the buffer: 5 above + 5 visible + 5 below.
pub const TOTAL_ROWS: usize = 15;

/// First visible row index (inclusive).
pub const VISIBLE_START: usize = 5;

/// Row after the last visible row (exclusive).
pub const VISIBLE_END: usize = 10;

/// A buffer of 15 consecutive Monday-first week rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WeeksBuffer {
    /// The Monday on which the first visible row (index `VISIBLE_START`)
    /// begins.  All other rows are computed as offsets from this day.
    first_visible_monday: NaiveDate,
}

impl WeeksBuffer {
    /// Build a buffer whose first visible row begins on
    /// `first_visible_monday`. The caller is responsible for passing a
    /// date that is a Monday; the constructor does not validate.
    pub fn new(first_visible_monday: NaiveDate) -> Self {
        Self {
            first_visible_monday,
        }
    }

    /// The Monday on which the first visible row begins.
    pub fn first_visible_monday(&self) -> NaiveDate {
        self.first_visible_monday
    }

    /// The Monday on which `row` (0..TOTAL_ROWS) begins.
    ///
    /// Row 0 is 5 weeks before the first visible row.
    pub fn row_start(&self, row: usize) -> NaiveDate {
        assert!(row < TOTAL_ROWS, "row index {row} out of range");
        let offset = row as i64 - VISIBLE_START as i64;
        self.first_visible_monday + Duration::days(offset * 7)
    }

    /// The seven consecutive NaiveDates filling `row` (0..TOTAL_ROWS),
    /// Monday-first.
    pub fn row_dates(&self, row: usize) -> [NaiveDate; 7] {
        let start = self.row_start(row);
        std::array::from_fn(|i| start + Duration::days(i as i64))
    }

    /// Slide the buffer by `weeks` rows.
    ///
    /// `weeks > 0` moves the buffer forward in time (a new row appears at
    /// the bottom); `weeks < 0` moves it backward. The buffer slides
    /// monotonically: across a sequence of shifts the union of observed
    /// row starts is a contiguous run of Mondays with no gaps and no
    /// duplicates.
    pub fn shift_weeks(&mut self, weeks: i32) {
        self.first_visible_monday += Duration::days(weeks as i64 * 7);
    }
}
