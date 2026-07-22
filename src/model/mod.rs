use chrono::{DateTime, FixedOffset, NaiveDate};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: Uuid,
    pub name: String,
    pub server_url: String,
    pub username: String,
    pub enabled: bool,
}

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
    CalDav { account_id: Uuid },
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

/// Durable identity for a calendar on its remote CalDAV server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarSyncState {
    pub calendar_id: Uuid,
    pub remote_url: String,
    pub sync_token: Option<String>,
}

/// Durable identity for a locally stored event on its remote CalDAV server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventSyncState {
    pub calendar_id: Uuid,
    pub event_id: Uuid,
    pub remote_href: String,
    pub remote_uid: String,
    pub etag: Option<String>,
}

/// Durable intent to upload a local CalDAV change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingSyncOperation {
    Create {
        calendar_id: Uuid,
        event_id: Uuid,
        remote_uid: String,
    },
    Update {
        calendar_id: Uuid,
        event_id: Uuid,
        remote_href: String,
        remote_uid: String,
        base_etag: Option<String>,
    },
    Delete {
        calendar_id: Uuid,
        event_id: Uuid,
        remote_href: String,
        remote_uid: String,
        base_etag: Option<String>,
    },
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

/// The recurrence properties belonging to an event master.
///
/// The lines retain their iCalendar parameters (for example `TZID` and
/// `VALUE=DATE`) so that persistence does not reduce a valid remote rule to a
/// boolean flag.  They are validated and evaluated through `rrule` at the
/// CalDAV and view-projection boundaries.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RecurrenceSpec {
    pub rrule: Vec<String>,
    pub rdate: Vec<String>,
    pub exdate: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderSpec {
    pub seconds_before_start: i64,
    pub description: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmptyQuickAddTitle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCalendar;

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

/// Normalize and validate a complete calendar candidate before persistence.
pub fn validate_calendar(mut candidate: Calendar) -> Result<Calendar, InvalidCalendar> {
    let trimmed_name = candidate.name.trim();
    let color = candidate
        .color
        .strip_prefix('#')
        .unwrap_or(&candidate.color);

    if trimmed_name.is_empty()
        || color.len() != 6
        || !color.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(InvalidCalendar);
    }

    candidate.name = trimmed_name.to_string();
    candidate.color = format!("#{color}").to_ascii_lowercase();
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
