use chrono::{DateTime, FixedOffset, NaiveDate};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Calendar {
    pub id: Uuid,
    pub name: String,
    pub color: String,
    pub visible: bool,
    pub read_only: bool,
    pub source: CalendarSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CalendarSource {
    Local,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub id: Uuid,
    pub calendar_id: Uuid,
    pub title: String,
    pub location: String,
    pub description: String,
    pub schedule: EventSchedule,
    pub recurrence: Option<RecurrenceSpec>,
    pub reminders: Vec<ReminderSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventSchedule {
    AllDay {
        start_date: NaiveDate,
        end_date_exclusive: NaiveDate,
    },
    Timed {
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
        timezone: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecurrenceSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReminderSpec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTimeRange {
    pub start: DateTime<FixedOffset>,
    pub end: DateTime<FixedOffset>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidDateTimeRange;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RangeOverlap {
    None,
    Intersection,
    Contains,
    ContainedIn,
    Equal,
}

impl DateTimeRange {
    /// Construct a new DateTimeRange.
    ///
    /// Returns `Err(InvalidDateTimeRange)` when `end <= start` (zero-length or inverted).
    pub fn new(
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    ) -> Result<Self, InvalidDateTimeRange> {
        if end <= start {
            return Err(InvalidDateTimeRange);
        }
        Ok(DateTimeRange { start, end })
    }

    /// Classify the overlap relationship between `self` and `other`.
    ///
    /// Intervals are treated as start-inclusive, end-exclusive: `[start, end)`.
    /// Ranges that merely touch at the boundary (e.g. `[1,3)` and `[3,5)`)
    /// are classified as `None`.
    pub fn overlap(&self, other: &Self) -> RangeOverlap {
        // No overlap: one range ends before or at the other's start.
        if self.end <= other.start || other.end <= self.start {
            return RangeOverlap::None;
        }

        // Equal: identical start and end.
        if self.start == other.start && self.end == other.end {
            return RangeOverlap::Equal;
        }

        // Self fully contains other.
        if self.start <= other.start && other.end <= self.end {
            return RangeOverlap::Contains;
        }

        // Self is fully contained in other.
        if other.start <= self.start && self.end <= other.end {
            return RangeOverlap::ContainedIn;
        }

        // Partial overlap that is neither containment nor equality.
        RangeOverlap::Intersection
    }
}
