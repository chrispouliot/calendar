use std::str::FromStr;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use rrule::{RRuleSet, Tz as RRuleTz};
use uuid::Uuid;

use crate::model::{Event, EventSchedule};

const RECURRENCE_LIMIT: u16 = u16::MAX;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderOccurrence {
    pub event_id: Uuid,
    pub occurrence_start: DateTime<FixedOffset>,
    pub trigger_at: DateTime<FixedOffset>,
    pub description: String,
}

/// Return reminder triggers in `(start_exclusive, end_inclusive]`.
///
/// Recurring events are expanded only between the corresponding occurrence
/// bounds, and the rrule crate's finite result limit prevents unbounded
/// expansion for an accidentally unbounded rule.
pub fn reminder_occurrences_in_window(
    event: &Event,
    start_exclusive: DateTime<FixedOffset>,
    end_inclusive: DateTime<FixedOffset>,
) -> Vec<ReminderOccurrence> {
    if start_exclusive >= end_inclusive || event.reminders.is_empty() {
        return Vec::new();
    }

    let (event_start, timezone, all_day_offset) = match &event.schedule {
        EventSchedule::Timed {
            start,
            end,
            timezone,
        } if end > start => (*start, timezone.clone(), None),
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } if end_date_exclusive > start_date => {
            let offset = *start_exclusive.offset();
            let event_start = offset
                .from_local_datetime(
                    &start_date
                        .and_hms_opt(0, 0, 0)
                        .expect("valid all-day date has a midnight"),
                )
                .single()
                .expect("a fixed offset has no ambiguous local times");
            (event_start, None, Some(offset))
        }
        _ => return Vec::new(),
    };

    let reminders: Vec<_> = event
        .reminders
        .iter()
        .filter(|reminder| reminder.seconds_before_start > 0)
        .collect();
    if reminders.is_empty() {
        return Vec::new();
    }

    let mut occurrences = if let Some(recurrence) = &event.recurrence {
        let offsets = reminders
            .iter()
            .map(|reminder| reminder.seconds_before_start);
        let dates = if let Some(offset) = all_day_offset {
            expand_all_day_recurrence(
                event_start.date_naive(),
                offset,
                recurrence,
                start_exclusive,
                end_inclusive,
                offsets,
            )
        } else {
            expand_recurrence(
                event_start,
                timezone.as_deref(),
                recurrence,
                start_exclusive,
                end_inclusive,
                offsets,
            )
        };
        let Some(dates) = dates else {
            return Vec::new();
        };
        dates
    } else {
        vec![event_start]
    };

    let mut result = Vec::new();
    for occurrence_start in occurrences.drain(..) {
        for reminder in &reminders {
            let Some(trigger_at) = occurrence_start
                .checked_sub_signed(Duration::seconds(reminder.seconds_before_start))
            else {
                continue;
            };
            if trigger_at > start_exclusive && trigger_at <= end_inclusive {
                result.push(ReminderOccurrence {
                    event_id: event.id,
                    occurrence_start,
                    trigger_at,
                    description: reminder.description.clone(),
                });
            }
        }
    }

    result.sort_by(|a, b| {
        a.trigger_at
            .cmp(&b.trigger_at)
            .then_with(|| a.occurrence_start.cmp(&b.occurrence_start))
    });
    result
}

fn expand_all_day_recurrence(
    event_start: NaiveDate,
    offset: FixedOffset,
    recurrence: &crate::model::RecurrenceSpec,
    start_exclusive: DateTime<FixedOffset>,
    end_inclusive: DateTime<FixedOffset>,
    reminder_offsets: impl Iterator<Item = i64>,
) -> Option<Vec<DateTime<FixedOffset>>> {
    let source = all_day_recurrence_source(event_start, recurrence);
    let set = source.parse::<RRuleSet>().ok()?;

    let offsets: Vec<_> = reminder_offsets.collect();
    let min_offset = *offsets.iter().min()?;
    let max_offset = *offsets.iter().max()?;
    let lower = start_exclusive.checked_add_signed(Duration::seconds(min_offset))?;
    let upper = end_inclusive.checked_add_signed(Duration::seconds(max_offset))?;
    let upper_exclusive = upper.checked_add_signed(Duration::nanoseconds(1))?;
    if lower >= upper_exclusive {
        return Some(Vec::new());
    }
    let occurrence_lower = lower;
    let occurrence_upper_exclusive = upper_exclusive;

    // rrule represents VALUE=DATE values at UTC midnight. Query by the local
    // dates covered by the trigger bounds, then restore the scheduler offset.
    let lower_date = lower.with_timezone(&offset).date_naive();
    let upper_date = upper_exclusive.with_timezone(&offset).date_naive();
    let lower = utc_midnight(lower_date)?;
    let upper_exclusive = utc_midnight(upper_date.succ_opt()?)?;

    let start_is_excluded = set
        .get_exdate()
        .iter()
        .any(|date| date.date_naive() == event_start);
    let recurrence_start = utc_midnight(event_start)?;
    let mut dates = set
        .after(lower)
        .before(upper_exclusive)
        .all(RECURRENCE_LIMIT)
        .dates;
    // DTSTART is the first instance of a recurrence set even when the set has
    // only RDATE/EXDATE properties (rrule does not emit it by itself).
    let recurrence_start_local = offset
        .from_local_datetime(
            &event_start
                .and_hms_opt(0, 0, 0)
                .expect("valid all-day date has a midnight"),
        )
        .single()?;
    if !start_is_excluded
        && recurrence_start_local > occurrence_lower
        && recurrence_start_local < occurrence_upper_exclusive
    {
        dates.push(recurrence_start);
    }
    dates.sort_by(|a, b| a.partial_cmp(b).expect("rrule dates are orderable"));
    dates.dedup_by(|a, b| a.date_naive() == b.date_naive());
    dates
        .into_iter()
        .map(|date| {
            offset
                .from_local_datetime(
                    &date
                        .date_naive()
                        .and_hms_opt(0, 0, 0)
                        .expect("valid recurrence date has a midnight"),
                )
                .single()
        })
        .collect()
}

fn all_day_recurrence_source(
    event_start: NaiveDate,
    recurrence: &crate::model::RecurrenceSpec,
) -> String {
    let mut source = format!("DTSTART;VALUE=DATE:{}", event_start.format("%Y%m%d"));
    for line in recurrence
        .rrule
        .iter()
        .chain(&recurrence.rdate)
        .chain(&recurrence.exdate)
    {
        source.push('\n');
        source.push_str(line);
    }
    source
}

fn utc_midnight(date: NaiveDate) -> Option<DateTime<RRuleTz>> {
    let utc = FixedOffset::east_opt(0)?;
    let midnight = utc
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()?;
    Some(midnight.with_timezone(&RRuleTz::UTC))
}

fn expand_recurrence(
    event_start: DateTime<FixedOffset>,
    timezone: Option<&str>,
    recurrence: &crate::model::RecurrenceSpec,
    start_exclusive: DateTime<FixedOffset>,
    end_inclusive: DateTime<FixedOffset>,
    reminder_offsets: impl Iterator<Item = i64>,
) -> Option<Vec<DateTime<FixedOffset>>> {
    let source = recurrence_source(event_start, timezone, recurrence)?;
    let set = source.parse::<RRuleSet>().ok()?;

    let offsets: Vec<_> = reminder_offsets.collect();
    let min_offset = *offsets.iter().min()?;
    let max_offset = *offsets.iter().max()?;
    let lower = start_exclusive.checked_add_signed(Duration::seconds(min_offset))?;
    let upper = end_inclusive.checked_add_signed(Duration::seconds(max_offset))?;
    let upper_exclusive = upper.checked_add_signed(Duration::nanoseconds(1))?;
    if lower >= upper_exclusive {
        return Some(Vec::new());
    }

    let timezone = recurrence_timezone(timezone);
    let recurrence_start = event_start.with_timezone(&timezone);
    let start_is_excluded = set
        .get_exdate()
        .iter()
        .any(|date| date.timestamp() == recurrence_start.timestamp());
    let lower = lower.with_timezone(&timezone);
    let upper_exclusive = upper_exclusive.with_timezone(&timezone);
    let mut dates = set
        .after(lower)
        .before(upper_exclusive)
        .all(RECURRENCE_LIMIT)
        .dates;
    // DTSTART is the first instance of a recurrence set even when the set
    // has only RDATE/EXDATE properties (rrule does not emit it by itself).
    if !start_is_excluded && recurrence_start > lower && recurrence_start < upper_exclusive {
        dates.push(recurrence_start);
    }
    dates.sort_by(|a, b| a.partial_cmp(b).expect("rrule dates are orderable"));
    dates.dedup();
    Some(dates.into_iter().map(|date| date.fixed_offset()).collect())
}

fn recurrence_source(
    event_start: DateTime<FixedOffset>,
    timezone: Option<&str>,
    recurrence: &crate::model::RecurrenceSpec,
) -> Option<String> {
    let dtstart = match timezone {
        None => format!(
            "DTSTART:{}",
            event_start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
        ),
        Some(tzid) => {
            let timezone = chrono_tz::Tz::from_str(tzid).ok()?;
            format!(
                "DTSTART;TZID={tzid}:{}",
                event_start.with_timezone(&timezone).format("%Y%m%dT%H%M%S")
            )
        }
    };
    let mut source = dtstart;
    for line in recurrence
        .rrule
        .iter()
        .chain(&recurrence.rdate)
        .chain(&recurrence.exdate)
    {
        source.push('\n');
        source.push_str(line);
    }
    Some(source)
}

fn recurrence_timezone(timezone: Option<&str>) -> RRuleTz {
    match timezone {
        Some(timezone) => chrono_tz::Tz::from_str(timezone)
            .map(RRuleTz::from)
            .unwrap_or(RRuleTz::UTC),
        None => RRuleTz::UTC,
    }
}
