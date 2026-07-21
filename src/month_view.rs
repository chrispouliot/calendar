use std::collections::HashMap;

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveTime};

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

    // Collect start times for timed events so we can sort chips later.
    let mut event_start: HashMap<uuid::Uuid, DateTime<FixedOffset>> = HashMap::new();

    // First pass: record timed-event start times.
    for event in events {
        if let EventSchedule::Timed { start, .. } = &event.schedule {
            event_start.insert(event.id, *start);
        }
    }

    // Second pass: place chips.
    for event in events {
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

    // Sort each day's timed chips by start time ascending; ties broken by event_id.
    for day in projection.iter_mut() {
        day.timed.sort_by(|a, b| {
            let a_start = event_start.get(&a.event_id);
            let b_start = event_start.get(&b.event_id);
            a_start
                .cmp(&b_start)
                .then_with(|| a.event_id.cmp(&b.event_id))
        });
    }

    projection
}
