use chrono::{Duration, FixedOffset, NaiveDate};

use crate::agenda_presentation::{display_groups, has_no_upcoming_events};
use crate::model::{Calendar, DetachedEvent, Event};
use crate::month_view::{
    AgendaGroup, earliest_projected_event_date_in_timezone,
    project_agenda_range_with_detached_events_in_timezone,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgendaRange {
    pub start_date: NaiveDate,
    pub end_date_exclusive: NaiveDate,
}

impl AgendaRange {
    /// Construct a forward-facing range beginning at `today`.
    pub fn new(today: NaiveDate, initial_future_days: i64) -> Self {
        Self {
            start_date: today,
            end_date_exclusive: add_days(today, initial_future_days.max(0)),
        }
    }

    /// Include `target` and the following day without moving the range start.
    pub fn ensure_target(&mut self, target: NaiveDate) {
        if target < self.start_date || target < self.end_date_exclusive {
            return;
        }
        self.end_date_exclusive = target.succ_opt().unwrap_or(self.end_date_exclusive);
    }

    /// Extend the range end forward by `days`.
    pub fn extend_bottom(&mut self, days: i64) {
        self.end_date_exclusive = add_days(self.end_date_exclusive, days.max(0));
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgendaRenderPlan {
    NoUpcoming,
    Groups(Vec<AgendaGroup>),
}

/// Build a deterministic, future-facing agenda plan from calendar sources.
pub fn render_agenda(
    today: NaiveDate,
    range: &AgendaRange,
    calendars: &[Calendar],
    events: &[Event],
    viewer_timezone: &FixedOffset,
) -> AgendaRenderPlan {
    let events_with_detached = events
        .iter()
        .cloned()
        .map(|event| (event, Vec::new()))
        .collect::<Vec<_>>();
    render_agenda_with_detached_events_in_timezone(
        today,
        range,
        calendars,
        &events_with_detached,
        viewer_timezone,
    )
}

/// Build an agenda plan while applying detached recurring-instance changes.
pub fn render_agenda_with_detached_events_in_timezone(
    today: NaiveDate,
    range: &AgendaRange,
    calendars: &[Calendar],
    events: &[(Event, Vec<DetachedEvent>)],
    viewer_timezone: &FixedOffset,
) -> AgendaRenderPlan {
    let masters = events
        .iter()
        .map(|(event, _)| event.clone())
        .collect::<Vec<_>>();
    let source_end =
        earliest_projected_event_date_in_timezone(today, calendars, &masters, viewer_timezone)
            .and_then(|date| date.succ_opt());
    let end_date_exclusive = source_end
        .unwrap_or(range.end_date_exclusive)
        .max(range.end_date_exclusive);

    let projected = project_agenda_range_with_detached_events_in_timezone(
        today,
        end_date_exclusive,
        calendars,
        events,
        viewer_timezone,
    );
    let projected = trim_trailing_empty_ranges(projected);
    let displayed = display_groups(&projected, today);

    if has_no_upcoming_events(&displayed, today) {
        AgendaRenderPlan::NoUpcoming
    } else {
        AgendaRenderPlan::Groups(displayed)
    }
}

fn trim_trailing_empty_ranges(mut groups: Vec<AgendaGroup>) -> Vec<AgendaGroup> {
    while matches!(groups.last(), Some(AgendaGroup::EmptyRange { .. })) {
        groups.pop();
    }
    groups
}

fn add_days(date: NaiveDate, days: i64) -> NaiveDate {
    date.checked_add_signed(Duration::days(days))
        .unwrap_or(date)
}
