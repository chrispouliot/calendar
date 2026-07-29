use chrono::{DateTime, FixedOffset, NaiveDate};

use crate::model::EventSchedule;
use crate::month_view::{AgendaGroup, EventChip, ViewerLocalEnd};
use crate::time_format::{TimeFormatPreference, format_time};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgendaEventState {
    Past,
    Current,
    Upcoming,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgendaTimeLayout {
    Desktop,
    Compact,
}

/// Classify an event at an injected instant using start-inclusive,
/// end-exclusive timed-event boundaries.
pub fn event_state(schedule: &EventSchedule, now: DateTime<FixedOffset>) -> AgendaEventState {
    match schedule {
        EventSchedule::Timed { start, end, .. } => {
            if now < *start {
                AgendaEventState::Upcoming
            } else if now >= *end {
                AgendaEventState::Past
            } else {
                AgendaEventState::Current
            }
        }
        EventSchedule::AllDay {
            end_date_exclusive, ..
        } => {
            if now.date_naive() >= *end_date_exclusive {
                AgendaEventState::Past
            } else {
                AgendaEventState::Upcoming
            }
        }
    }
}

/// Format the time label for an agenda event chip.
pub fn time_text(
    chip: &EventChip,
    layout: AgendaTimeLayout,
    preference: TimeFormatPreference,
    system_clock_format: &str,
) -> Option<String> {
    let start = chip.start_time?;
    let start = format_time(start, preference, system_clock_format);

    match (&chip.viewer_local_end, layout) {
        (ViewerLocalEnd::Timed(end), AgendaTimeLayout::Desktop) => Some(format!(
            "{start}–{}",
            format_time(end.time(), preference, system_clock_format)
        )),
        (ViewerLocalEnd::Timed(_), AgendaTimeLayout::Compact) => Some(start),
        (ViewerLocalEnd::AllDay(_), _) => None,
    }
}

/// Convert a projected range into the future-facing groups shown by Agenda.
///
/// Event days before `today` are not part of the Agenda presentation. Empty
/// ranges are clipped at `today`, with an empty Today kept as its own card;
/// the rest of each range remains a single maximal span.
pub fn display_groups(groups: &[AgendaGroup], today: NaiveDate) -> Vec<AgendaGroup> {
    let mut displayed = Vec::with_capacity(groups.len() + 1);

    for group in groups {
        match group {
            AgendaGroup::EventDay(day) if day.date >= today => {
                displayed.push(group.clone());
            }
            AgendaGroup::EventDay(_) => {}
            AgendaGroup::EmptyRange {
                start_date,
                end_date_exclusive,
            } => {
                let start = (*start_date).max(today);
                if start >= *end_date_exclusive {
                    continue;
                }

                if start == today {
                    let today_end = (today + chrono::Duration::days(1)).min(*end_date_exclusive);
                    displayed.push(AgendaGroup::EmptyRange {
                        start_date: today,
                        end_date_exclusive: today_end,
                    });
                    if today_end < *end_date_exclusive {
                        displayed.push(AgendaGroup::EmptyRange {
                            start_date: today_end,
                            end_date_exclusive: *end_date_exclusive,
                        });
                    }
                } else {
                    displayed.push(AgendaGroup::EmptyRange {
                        start_date: start,
                        end_date_exclusive: *end_date_exclusive,
                    });
                }
            }
        }
    }

    displayed
}

/// Return whether the agenda has no event day today or later.
pub fn has_no_upcoming_events(groups: &[AgendaGroup], today: NaiveDate) -> bool {
    !groups
        .iter()
        .any(|group| matches!(group, AgendaGroup::EventDay(day) if day.date >= today))
}
