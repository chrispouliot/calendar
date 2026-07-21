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
pub struct EmptyQuickAddTitle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidEvent;

/// Normalize and validate a complete event candidate before persistence.
pub fn validate_event(mut candidate: Event) -> Result<Event, InvalidEvent> {
    if candidate.title.trim().is_empty() {
        return Err(InvalidEvent);
    }

    match &candidate.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } if end_date_exclusive <= start_date => return Err(InvalidEvent),
        EventSchedule::Timed { start, end, .. } if end <= start => return Err(InvalidEvent),
        _ => {}
    }

    candidate.title = candidate.title.trim().to_string();
    Ok(candidate)
}

/// Build the base all-day one-day `Event` for a quick-add popover.
///
/// Pure: no clock, no ID generation (caller supplies both UUIDs), no
/// GTK, no repository writes. The `title` is trimmed; a title that
/// is empty after trimming is rejected with `EmptyQuickAddTitle`.
/// The result has empty `location`/`description`, no `recurrence`,
/// no `reminders`, and an `AllDay` schedule spanning exactly
/// `date..date+1 day` as an exclusive end.
pub fn new_quick_add_event(
    event_id: Uuid,
    calendar_id: Uuid,
    title: &str,
    date: NaiveDate,
) -> Result<Event, EmptyQuickAddTitle> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(EmptyQuickAddTitle);
    }

    let next_day = date
        .succ_opt()
        .expect("NaiveDate::succ_opt never fails for valid dates");

    Ok(Event {
        id: event_id,
        calendar_id,
        title: trimmed.to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: date,
            end_date_exclusive: next_day,
        },
        recurrence: None,
        reminders: Vec::new(),
    })
}

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
