use std::collections::HashMap;
use std::str::FromStr;

use chrono::{Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use rrule::{RRuleSet, Tz as RRuleTz};

use crate::calendar_grid::month_grid;
use crate::model::{Calendar, Event, EventSchedule};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChip {
    pub event_id: uuid::Uuid,
    pub title: String,
    pub calendar_id: uuid::Uuid,
    pub color: String,
    pub is_all_day: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayProjection {
    pub date: NaiveDate,
    pub in_displayed_month: bool,
    pub all_day: Vec<EventChip>,
    pub timed: Vec<EventChip>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgendaGroup {
    EventDay(DayProjection),
    EmptyRange {
        start_date: NaiveDate,
        end_date_exclusive: NaiveDate,
    },
}

/// Projects events onto a fixed 42-cell, Monday-first month grid.
pub fn project_month(
    year: i32,
    month: u32,
    calendars: &[Calendar],
    events: &[Event],
) -> [DayProjection; 42] {
    let grid = month_grid(year, month);
    let projection = project_dates(
        grid.into_iter().map(|cell| {
            let date =
                NaiveDate::from_ymd_opt(cell.year, cell.month, cell.day).expect("valid grid cell");
            (date, cell.in_displayed_month)
        }),
        calendars,
        events,
    );

    projection.try_into().expect("month grid has 42 cells")
}

/// Projects the Monday-first week containing `active_date`.
pub fn project_week(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
) -> [DayProjection; 7] {
    let monday = active_date - Duration::days(active_date.weekday().num_days_from_monday() as i64);
    let projection = project_dates(
        (0..7).map(|offset| (monday + Duration::days(offset), true)),
        calendars,
        events,
    );

    projection.try_into().expect("week has 7 days")
}

/// Projects `active_date` through the end of its Monday-first calendar week.
/// The active date is retained even when it has no events; later empty days
/// are omitted.
pub fn project_agenda(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
) -> Vec<DayProjection> {
    let week = project_week(active_date, calendars, events);
    week.into_iter()
        .filter(|day| {
            day.date >= active_date
                && (day.date == active_date || !day.all_day.is_empty() || !day.timed.is_empty())
        })
        .collect()
}

/// Projects a date range into event-day and maximal empty-range groups.
pub fn project_agenda_range(
    start_date: NaiveDate,
    end_date_exclusive: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
) -> Vec<AgendaGroup> {
    if start_date >= end_date_exclusive {
        return Vec::new();
    }

    let mut dates = Vec::new();
    let mut date = start_date;
    while date < end_date_exclusive {
        dates.push((date, true));
        date += Duration::days(1);
    }

    let days = project_dates(dates, calendars, events);
    let mut groups = Vec::new();
    let mut empty_start = None;

    for day in days {
        let has_events = !day.all_day.is_empty() || !day.timed.is_empty();
        if has_events {
            if let Some(empty_start) = empty_start.take() {
                groups.push(AgendaGroup::EmptyRange {
                    start_date: empty_start,
                    end_date_exclusive: day.date,
                });
            }
            groups.push(AgendaGroup::EventDay(day));
        } else if empty_start.is_none() {
            empty_start = Some(day.date);
        }
    }

    if let Some(empty_start) = empty_start {
        groups.push(AgendaGroup::EmptyRange {
            start_date: empty_start,
            end_date_exclusive,
        });
    }

    groups
}

fn project_dates(
    dates: impl IntoIterator<Item = (NaiveDate, bool)>,
    calendars: &[Calendar],
    events: &[Event],
) -> Vec<DayProjection> {
    // Visible calendars indexed by id.
    let cal_map: HashMap<uuid::Uuid, &Calendar> = calendars
        .iter()
        .filter(|c| c.visible)
        .map(|c| (c.id, c))
        .collect();

    // Build an empty projection in the requested date order.
    let mut projection: Vec<DayProjection> = dates
        .into_iter()
        .map(|(date, in_displayed_month)| DayProjection {
            date,
            in_displayed_month,
            all_day: Vec::new(),
            timed: Vec::new(),
        })
        .collect();

    let first_date = projection.first().map(|day| day.date);
    let last_date = projection.last().map(|day| day.date);
    let mut expanded_events: Vec<Event> = match (first_date, last_date) {
        (Some(first), Some(last)) => events
            .iter()
            .flat_map(|event| expand_event(event, first, last))
            .collect(),
        _ => Vec::new(),
    };
    expanded_events.sort_by(|a, b| match (&a.schedule, &b.schedule) {
        (
            EventSchedule::Timed { start: a_start, .. },
            EventSchedule::Timed { start: b_start, .. },
        ) => a_start.cmp(b_start).then_with(|| a.id.cmp(&b.id)),
        (EventSchedule::AllDay { .. }, EventSchedule::Timed { .. }) => std::cmp::Ordering::Less,
        (EventSchedule::Timed { .. }, EventSchedule::AllDay { .. }) => std::cmp::Ordering::Greater,
        (EventSchedule::AllDay { .. }, EventSchedule::AllDay { .. }) => std::cmp::Ordering::Equal,
    });

    // Timed events are already ordered by each expanded occurrence's start.
    for event in &expanded_events {
        let cal = match cal_map.get(&event.calendar_id) {
            Some(c) => c,
            None => continue,
        };

        match &event.schedule {
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            } => {
                let mut date = *start_date;
                while date < *end_date_exclusive {
                    if let Some(day) = projection.iter_mut().find(|d| d.date == date) {
                        day.all_day.push(EventChip {
                            event_id: event.id,
                            title: event.title.clone(),
                            calendar_id: event.calendar_id,
                            color: cal.color.clone(),
                            is_all_day: true,
                        });
                    }
                    date += Duration::days(1);
                }
            }
            EventSchedule::Timed { start, end, .. } => {
                let start_date = start.date_naive();
                let end_date = end.date_naive();
                let end_is_midnight =
                    end.time() == NaiveTime::from_hms_opt(0, 0, 0).expect("valid midnight");

                let mut date = start_date;
                while date <= end_date {
                    if date == end_date && end_is_midnight {
                        break;
                    }
                    if let Some(day) = projection.iter_mut().find(|d| d.date == date) {
                        day.timed.push(EventChip {
                            event_id: event.id,
                            title: event.title.clone(),
                            calendar_id: event.calendar_id,
                            color: cal.color.clone(),
                            is_all_day: false,
                        });
                    }
                    date += Duration::days(1);
                }
            }
        }
    }

    projection
}

const RECURRENCE_LIMIT: u16 = 4096;

fn expand_event(event: &Event, first: NaiveDate, last: NaiveDate) -> Vec<Event> {
    if event.recurrence.is_none() {
        return vec![event.clone()];
    }
    let Ok(source) = recurrence_source(event) else {
        return vec![event.clone()];
    };
    let Ok(set) = source.parse::<RRuleSet>() else {
        return vec![event.clone()];
    };
    let duration = match &event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => Duration::days((*end_date_exclusive - *start_date).num_days()),
        EventSchedule::Timed { start, end, .. } => *end - *start,
    };
    let tz = recurrence_timezone(event);
    let lower = bound_datetime(tz, first) - duration - Duration::seconds(1);
    let upper = bound_datetime(tz, last + Duration::days(1));
    let dates = set.after(lower).before(upper).all(RECURRENCE_LIMIT).dates;

    dates
        .into_iter()
        .filter_map(|date| {
            let schedule = match &event.schedule {
                EventSchedule::AllDay { .. } => {
                    let start_date = date.date_naive();
                    let end_date_exclusive = start_date.checked_add_signed(duration)?;
                    EventSchedule::AllDay {
                        start_date,
                        end_date_exclusive,
                    }
                }
                EventSchedule::Timed { timezone, .. } => {
                    let start = date.fixed_offset();
                    let end = start.checked_add_signed(duration)?;
                    EventSchedule::Timed {
                        start,
                        end,
                        timezone: timezone.clone(),
                    }
                }
            };
            Some(Event {
                schedule,
                recurrence: None,
                ..event.clone()
            })
        })
        .collect()
}

fn recurrence_source(event: &Event) -> Result<String, ()> {
    let Some(recurrence) = &event.recurrence else {
        return Err(());
    };
    let dtstart = match &event.schedule {
        EventSchedule::AllDay { start_date, .. } => {
            format!("DTSTART:{}T000000", start_date.format("%Y%m%d"))
        }
        EventSchedule::Timed {
            start, timezone, ..
        } => match timezone.as_deref() {
            None => format!(
                "DTSTART:{}",
                start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
            ),
            Some(tzid) => {
                let timezone = chrono_tz::Tz::from_str(tzid).map_err(|_| ())?;
                format!(
                    "DTSTART;TZID={tzid}:{}",
                    start.with_timezone(&timezone).format("%Y%m%dT%H%M%S")
                )
            }
        },
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
    Ok(source)
}

fn recurrence_timezone(event: &Event) -> RRuleTz {
    match event.schedule {
        EventSchedule::AllDay { .. } => RRuleTz::LOCAL,
        EventSchedule::Timed {
            timezone: Some(ref timezone),
            ..
        } => chrono_tz::Tz::from_str(timezone)
            .map(RRuleTz::from)
            .unwrap_or(RRuleTz::UTC),
        EventSchedule::Timed { timezone: None, .. } => RRuleTz::UTC,
    }
}

fn bound_datetime(timezone: RRuleTz, date: NaiveDate) -> chrono::DateTime<RRuleTz> {
    timezone
        .with_ymd_and_hms(date.year(), date.month(), date.day(), 0, 0, 0)
        .single()
        .expect("projection dates are valid")
}
