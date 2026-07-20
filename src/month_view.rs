use std::collections::HashMap;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime};

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

/// Projects events onto a fixed 42-cell, Monday-first month grid.
pub fn project_month(
    year: i32,
    month: u32,
    calendars: &[Calendar],
    events: &[Event],
) -> [DayProjection; 42] {
    // Visible calendars indexed by id.
    let cal_map: HashMap<uuid::Uuid, &Calendar> = calendars
        .iter()
        .filter(|c| c.visible)
        .map(|c| (c.id, c))
        .collect();

    let grid = month_grid(year, month);

    // Build an empty projection aligned with the grid.
    let mut projection: [DayProjection; 42] = std::array::from_fn(|i| {
        let cell = grid[i];
        let date =
            NaiveDate::from_ymd_opt(cell.year, cell.month, cell.day).expect("valid grid cell");
        DayProjection {
            date,
            in_displayed_month: cell.in_displayed_month,
            all_day: Vec::new(),
            timed: Vec::new(),
        }
    });

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
