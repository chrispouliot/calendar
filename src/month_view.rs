use std::collections::HashMap;
use std::str::FromStr;

use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc};
use rrule::{RRuleSet, Tz as RRuleTz};

use crate::calendar_grid::month_grid;
use crate::model::{Calendar, DetachedEvent, Event, EventSchedule, RecurrenceId};
use crate::viewer_time::to_local_fixed;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventChip {
    pub event_id: uuid::Uuid,
    pub title: String,
    pub calendar_id: uuid::Uuid,
    pub color: String,
    pub is_all_day: bool,
    pub start_time: Option<NaiveTime>,
    pub viewer_local_end: ViewerLocalEnd,
    pub viewer_local_schedule: ViewerLocalSchedule,
    pub original_recurrence_id: Option<RecurrenceId>,
}

impl EventChip {
    /// Returns whether the event ended strictly before the supplied instant.
    pub fn is_past_at(&self, now: DateTime<FixedOffset>) -> bool {
        match &self.viewer_local_end {
            ViewerLocalEnd::Timed(end) => *end < now,
            ViewerLocalEnd::AllDay(end_date) => {
                end_date.and_time(NaiveTime::MIN) < now.naive_local()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerLocalEnd {
    Timed(DateTime<FixedOffset>),
    AllDay(NaiveDate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewerLocalSchedule {
    Timed {
        start: DateTime<FixedOffset>,
        end: DateTime<FixedOffset>,
    },
    AllDay {
        start_date: NaiveDate,
        end_date_exclusive: NaiveDate,
    },
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
    let localizer = GlibLocalizer;
    project_month_with_localizer(year, month, calendars, events, &localizer)
}

/// Projects events onto a fixed 42-cell, Monday-first month grid using the
/// supplied timezone for timed-event date boundaries.
pub fn project_month_in_timezone<Tz: TimeZone>(
    year: i32,
    month: u32,
    calendars: &[Calendar],
    events: &[Event],
    viewer_timezone: &Tz,
) -> [DayProjection; 42] {
    let localizer = TimezoneLocalizer(viewer_timezone);
    project_month_with_localizer(year, month, calendars, events, &localizer)
}

/// Projects a fixed month grid while applying detached recurring-instance
/// changes using the supplied timezone for timed-event date boundaries.
pub fn project_month_with_detached_events_in_timezone<Tz: TimeZone>(
    year: i32,
    month: u32,
    calendars: &[Calendar],
    events: &[(Event, Vec<DetachedEvent>)],
    viewer_timezone: &Tz,
) -> [DayProjection; 42] {
    let localizer = TimezoneLocalizer(viewer_timezone);
    let grid = month_grid(year, month);
    let projection = project_dates_with_detached_events(
        grid.into_iter().map(|cell| {
            let date =
                NaiveDate::from_ymd_opt(cell.year, cell.month, cell.day).expect("valid grid cell");
            (date, cell.in_displayed_month)
        }),
        calendars,
        events,
        &localizer,
    );

    projection.try_into().expect("month grid has 42 cells")
}

fn project_month_with_localizer(
    year: i32,
    month: u32,
    calendars: &[Calendar],
    events: &[Event],
    localizer: &impl ViewerLocalizer,
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
        localizer,
    );

    projection.try_into().expect("month grid has 42 cells")
}

/// Projects the Monday-first week containing `active_date`.
pub fn project_week(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
) -> [DayProjection; 7] {
    let localizer = GlibLocalizer;
    project_week_with_localizer(active_date, calendars, events, &localizer)
}

/// Projects the Monday-first week containing `active_date` using the
/// supplied timezone for timed-event date boundaries.
pub fn project_week_in_timezone<Tz: TimeZone>(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    viewer_timezone: &Tz,
) -> [DayProjection; 7] {
    let localizer = TimezoneLocalizer(viewer_timezone);
    project_week_with_localizer(active_date, calendars, events, &localizer)
}

/// Projects the Monday-first week containing `active_date` while applying
/// detached recurring-instance changes using the supplied timezone.
pub fn project_week_with_detached_events_in_timezone<Tz: TimeZone>(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[(Event, Vec<DetachedEvent>)],
    viewer_timezone: &Tz,
) -> [DayProjection; 7] {
    let localizer = TimezoneLocalizer(viewer_timezone);
    let monday = active_date - Duration::days(active_date.weekday().num_days_from_monday() as i64);
    let projection = project_dates_with_detached_events(
        (0..7).map(|offset| (monday + Duration::days(offset), true)),
        calendars,
        events,
        &localizer,
    );

    projection.try_into().expect("week has 7 days")
}

fn project_week_with_localizer(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    localizer: &impl ViewerLocalizer,
) -> [DayProjection; 7] {
    let monday = active_date - Duration::days(active_date.weekday().num_days_from_monday() as i64);
    let projection = project_dates(
        (0..7).map(|offset| (monday + Duration::days(offset), true)),
        calendars,
        events,
        localizer,
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
    let localizer = GlibLocalizer;
    project_agenda_with_localizer(active_date, calendars, events, &localizer)
}

/// Projects `active_date` through the end of its Monday-first calendar week
/// using the supplied timezone for timed-event date boundaries.
pub fn project_agenda_in_timezone<Tz: TimeZone>(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    viewer_timezone: &Tz,
) -> Vec<DayProjection> {
    let localizer = TimezoneLocalizer(viewer_timezone);
    project_agenda_with_localizer(active_date, calendars, events, &localizer)
}

fn project_agenda_with_localizer(
    active_date: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    localizer: &impl ViewerLocalizer,
) -> Vec<DayProjection> {
    let week = project_week_with_localizer(active_date, calendars, events, localizer);
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
    let localizer = GlibLocalizer;
    project_agenda_range_with_localizer(
        start_date,
        end_date_exclusive,
        calendars,
        events,
        &localizer,
    )
}

/// Projects a date range into event-day and maximal empty-range groups using
/// the supplied timezone for timed-event date boundaries.
pub fn project_agenda_range_in_timezone<Tz: TimeZone>(
    start_date: NaiveDate,
    end_date_exclusive: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    viewer_timezone: &Tz,
) -> Vec<AgendaGroup> {
    let localizer = TimezoneLocalizer(viewer_timezone);
    project_agenda_range_with_localizer(
        start_date,
        end_date_exclusive,
        calendars,
        events,
        &localizer,
    )
}

/// Projects a date range while applying detached recurring-instance changes.
///
/// Detached instances retain the master's event id and are resolved against
/// the generated occurrence identified by their `RecurrenceId`.  The normal
/// projection entry points intentionally remain unchanged; callers without
/// detached instances should continue to use those APIs.
pub fn project_agenda_range_with_detached_events_in_timezone<Tz: TimeZone>(
    start_date: NaiveDate,
    end_date_exclusive: NaiveDate,
    calendars: &[Calendar],
    events: &[(Event, Vec<DetachedEvent>)],
    viewer_timezone: &Tz,
) -> Vec<AgendaGroup> {
    if start_date >= end_date_exclusive {
        return Vec::new();
    }

    let localizer = TimezoneLocalizer(viewer_timezone);
    let mut dates = Vec::new();
    let mut date = start_date;
    while date < end_date_exclusive {
        dates.push((date, true));
        date += Duration::days(1);
    }

    let days = project_dates_with_detached_events(dates, calendars, events, &localizer);
    agenda_groups(days, end_date_exclusive)
}

fn project_agenda_range_with_localizer(
    start_date: NaiveDate,
    end_date_exclusive: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    localizer: &impl ViewerLocalizer,
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

    let days = project_dates(dates, calendars, events, localizer);
    agenda_groups(days, end_date_exclusive)
}

fn agenda_groups(days: Vec<DayProjection>, end_date_exclusive: NaiveDate) -> Vec<AgendaGroup> {
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

fn project_dates_with_detached_events(
    dates: impl IntoIterator<Item = (NaiveDate, bool)>,
    calendars: &[Calendar],
    events: &[(Event, Vec<DetachedEvent>)],
    localizer: &impl ViewerLocalizer,
) -> Vec<DayProjection> {
    let dates: Vec<(NaiveDate, bool)> = dates.into_iter().collect();
    let projected_events = expand_events_with_detached(&dates, events);
    let projection = dates
        .iter()
        .map(|(date, in_displayed_month)| DayProjection {
            date: *date,
            in_displayed_month: *in_displayed_month,
            all_day: Vec::new(),
            timed: Vec::new(),
        })
        .collect();
    project_projected_events(projection, calendars, projected_events, localizer)
}

fn expand_events_with_detached(
    dates: &[(NaiveDate, bool)],
    events: &[(Event, Vec<DetachedEvent>)],
) -> Vec<ProjectedEvent> {
    let Some((first_date, _)) = dates.first() else {
        return Vec::new();
    };
    let Some((last_date, _)) = dates.last() else {
        return Vec::new();
    };
    let mut projected_events = Vec::new();

    for (master, detached_events) in events {
        if detached_events.is_empty() {
            projected_events.extend(expand_event_with_identity(master, *first_date, *last_date));
            continue;
        }
        if master.recurrence.is_none() {
            projected_events.push(ProjectedEvent {
                event: master.clone(),
                original_recurrence_id: None,
            });
            continue;
        }

        for occurrence in expand_event(master, *first_date, *last_date) {
            if detached_events.iter().any(|detached| {
                detached_recurrence_id(detached)
                    .is_some_and(|id| recurrence_id_matches(master, &occurrence.schedule, id))
            }) {
                continue;
            }
            projected_events.push(ProjectedEvent {
                original_recurrence_id: recurrence_id_for_generated_occurrence(master, &occurrence),
                event: occurrence,
            });
        }

        for detached in detached_events {
            let DetachedEvent::Modified {
                recurrence_id,
                title,
                location,
                description,
                schedule,
                reminders,
            } = detached
            else {
                continue;
            };

            if generated_occurrence_for_recurrence_id(master, recurrence_id).is_none() {
                continue;
            }

            projected_events.push(ProjectedEvent {
                event: Event {
                    id: master.id,
                    calendar_id: master.calendar_id,
                    title: title.clone(),
                    location: location.clone(),
                    description: description.clone(),
                    schedule: schedule.clone(),
                    recurrence: None,
                    reminders: reminders.clone(),
                },
                original_recurrence_id: Some(recurrence_id.clone()),
            });
        }
    }

    projected_events
}

fn detached_recurrence_id(detached: &DetachedEvent) -> Option<&RecurrenceId> {
    match detached {
        DetachedEvent::Modified { recurrence_id, .. }
        | DetachedEvent::Cancelled { recurrence_id } => Some(recurrence_id),
    }
}

fn generated_occurrence_for_recurrence_id(
    master: &Event,
    recurrence_id: &RecurrenceId,
) -> Option<Event> {
    master.recurrence.as_ref()?;
    let target_date = recurrence_id_date(master, recurrence_id)?;
    expand_event(master, target_date, target_date)
        .into_iter()
        .find(|occurrence| {
            occurrence.recurrence.is_none()
                && recurrence_id_matches(master, &occurrence.schedule, recurrence_id)
        })
}

/// Resolve one generated recurring occurrence, applying its detached change
/// when present.  The returned event is always recurrence-free and retains the
/// master's durable identity for both generated and detached instances.
pub fn event_for_recurrence_id(
    master: &Event,
    detached_events: &[DetachedEvent],
    recurrence_id: &RecurrenceId,
) -> Option<Event> {
    let generated = generated_occurrence_for_recurrence_id(master, recurrence_id)?;
    let detached = detached_events.iter().find(|detached| {
        detached_recurrence_id(detached).is_some_and(|detached_id| {
            recurrence_id_matches(master, &generated.schedule, detached_id)
        })
    });

    match detached {
        Some(DetachedEvent::Cancelled { .. }) => None,
        Some(DetachedEvent::Modified {
            title,
            location,
            description,
            schedule,
            reminders,
            ..
        }) => Some(Event {
            id: master.id,
            calendar_id: master.calendar_id,
            title: title.clone(),
            location: location.clone(),
            description: description.clone(),
            schedule: schedule.clone(),
            recurrence: None,
            reminders: reminders.clone(),
        }),
        None => Some(generated),
    }
}

fn recurrence_id_date(master: &Event, recurrence_id: &RecurrenceId) -> Option<NaiveDate> {
    match (&master.schedule, recurrence_id) {
        (EventSchedule::AllDay { .. }, RecurrenceId::AllDay(date)) => Some(*date),
        (
            EventSchedule::Timed {
                timezone: master_timezone,
                ..
            },
            RecurrenceId::Timed {
                date_time,
                timezone: recurrence_timezone,
            },
        ) if master_timezone == recurrence_timezone => match master_timezone.as_deref() {
            Some(timezone) => {
                let timezone = chrono_tz::Tz::from_str(timezone).ok()?;
                Some(date_time.with_timezone(&timezone).date_naive())
            }
            None => Some(date_time.with_timezone(&Utc).date_naive()),
        },
        _ => None,
    }
}

fn recurrence_id_matches(
    master: &Event,
    occurrence_schedule: &EventSchedule,
    recurrence_id: &RecurrenceId,
) -> bool {
    match (&master.schedule, occurrence_schedule, recurrence_id) {
        (
            EventSchedule::AllDay { .. },
            EventSchedule::AllDay { start_date, .. },
            RecurrenceId::AllDay(recurrence_date),
        ) => start_date == recurrence_date,
        (
            EventSchedule::Timed {
                timezone: master_timezone,
                ..
            },
            EventSchedule::Timed { start, .. },
            RecurrenceId::Timed {
                date_time,
                timezone: recurrence_timezone,
            },
        ) if master_timezone == recurrence_timezone => match master_timezone.as_deref() {
            Some(timezone) => chrono_tz::Tz::from_str(timezone)
                .map(|timezone| {
                    start.with_timezone(&timezone).naive_local()
                        == date_time.with_timezone(&timezone).naive_local()
                })
                .unwrap_or(false),
            None => *start == *date_time,
        },
        _ => false,
    }
}

trait ViewerLocalizer {
    fn localize(&self, value: &DateTime<FixedOffset>) -> DateTime<FixedOffset>;
}

struct ProjectedEvent {
    event: Event,
    original_recurrence_id: Option<RecurrenceId>,
}

struct GlibLocalizer;

impl ViewerLocalizer for GlibLocalizer {
    fn localize(&self, value: &DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        to_local_fixed(value)
    }
}

struct TimezoneLocalizer<'a, Tz>(&'a Tz);

impl<Tz: TimeZone> ViewerLocalizer for TimezoneLocalizer<'_, Tz> {
    fn localize(&self, value: &DateTime<FixedOffset>) -> DateTime<FixedOffset> {
        value.with_timezone(self.0).fixed_offset()
    }
}

fn project_dates(
    dates: impl IntoIterator<Item = (NaiveDate, bool)>,
    calendars: &[Calendar],
    events: &[Event],
    localizer: &impl ViewerLocalizer,
) -> Vec<DayProjection> {
    let dates: Vec<(NaiveDate, bool)> = dates.into_iter().collect();
    let projection = dates
        .iter()
        .map(|(date, in_displayed_month)| DayProjection {
            date: *date,
            in_displayed_month: *in_displayed_month,
            all_day: Vec::new(),
            timed: Vec::new(),
        })
        .collect();

    let first_date = dates.first().map(|(date, _)| *date);
    let last_date = dates.last().map(|(date, _)| *date);
    let expanded_events: Vec<ProjectedEvent> = match (first_date, last_date) {
        (Some(first), Some(last)) => events
            .iter()
            .flat_map(|event| expand_event_with_identity(event, first, last))
            .collect(),
        _ => Vec::new(),
    };

    project_projected_events(projection, calendars, expanded_events, localizer)
}

fn project_projected_events(
    mut projection: Vec<DayProjection>,
    calendars: &[Calendar],
    mut expanded_events: Vec<ProjectedEvent>,
    localizer: &impl ViewerLocalizer,
) -> Vec<DayProjection> {
    // Visible calendars indexed by id.
    let cal_map: HashMap<uuid::Uuid, &Calendar> = calendars
        .iter()
        .filter(|c| c.visible)
        .map(|c| (c.id, c))
        .collect();

    expanded_events.sort_by(|a, b| match (&a.event.schedule, &b.event.schedule) {
        (
            EventSchedule::Timed { start: a_start, .. },
            EventSchedule::Timed { start: b_start, .. },
        ) => a_start
            .cmp(b_start)
            .then_with(|| a.event.id.cmp(&b.event.id)),
        (EventSchedule::AllDay { .. }, EventSchedule::Timed { .. }) => std::cmp::Ordering::Less,
        (EventSchedule::Timed { .. }, EventSchedule::AllDay { .. }) => std::cmp::Ordering::Greater,
        (EventSchedule::AllDay { .. }, EventSchedule::AllDay { .. }) => std::cmp::Ordering::Equal,
    });

    // Timed events are already ordered by each expanded occurrence's start.
    for projected_event in &expanded_events {
        let event = &projected_event.event;
        let cal = match cal_map.get(&event.calendar_id) {
            Some(c) => c,
            None => continue,
        };

        let Some((first_event_date, last_event_date)) =
            event_date_bounds(&event.schedule, localizer)
        else {
            continue;
        };

        match &event.schedule {
            EventSchedule::AllDay { .. } => {
                let mut date = first_event_date;
                while date <= last_event_date {
                    if let Some(day) = projection.iter_mut().find(|d| d.date == date) {
                        day.all_day.push(EventChip {
                            event_id: event.id,
                            title: event.title.clone(),
                            calendar_id: event.calendar_id,
                            color: cal.color.clone(),
                            is_all_day: true,
                            start_time: None,
                            viewer_local_end: ViewerLocalEnd::AllDay(
                                last_event_date + Duration::days(1),
                            ),
                            viewer_local_schedule: ViewerLocalSchedule::AllDay {
                                start_date: first_event_date,
                                end_date_exclusive: last_event_date + Duration::days(1),
                            },
                            original_recurrence_id: projected_event.original_recurrence_id.clone(),
                        });
                    }
                    date += Duration::days(1);
                }
            }
            EventSchedule::Timed { start, end, .. } => {
                let start = localizer.localize(start);
                let end = localizer.localize(end);

                let mut date = first_event_date;
                while date <= last_event_date {
                    if let Some(day) = projection.iter_mut().find(|d| d.date == date) {
                        day.timed.push(EventChip {
                            event_id: event.id,
                            title: event.title.clone(),
                            calendar_id: event.calendar_id,
                            color: cal.color.clone(),
                            is_all_day: false,
                            start_time: Some(start.time()),
                            viewer_local_end: ViewerLocalEnd::Timed(end),
                            viewer_local_schedule: ViewerLocalSchedule::Timed { start, end },
                            original_recurrence_id: projected_event.original_recurrence_id.clone(),
                        });
                    }
                    date += Duration::days(1);
                }
            }
        }
    }

    projection
}

fn event_date_bounds(
    schedule: &EventSchedule,
    localizer: &impl ViewerLocalizer,
) -> Option<(NaiveDate, NaiveDate)> {
    match schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => end_date_exclusive
            .checked_sub_signed(Duration::days(1))
            .filter(|last_date| start_date <= last_date)
            .map(|last_date| (*start_date, last_date)),
        EventSchedule::Timed { start, end, .. } => {
            let start = localizer.localize(start);
            let end = localizer.localize(end);
            if end <= start {
                return None;
            }
            let last_date = if end.time() == NaiveTime::MIN {
                end.date_naive().checked_sub_signed(Duration::days(1))?
            } else {
                end.date_naive()
            };
            Some((start.date_naive(), last_date))
        }
    }
}

/// Return the first date on which a visible event can be projected on or after
/// `today`.
///
/// This uses the same recurrence expansion and timezone date-boundary rules as
/// the normal agenda projection. Invalid recurrence data deliberately falls
/// back to the event's non-recurring schedule, matching `expand_event`.
pub(crate) fn earliest_projected_event_date_in_timezone<Tz: TimeZone>(
    today: NaiveDate,
    calendars: &[Calendar],
    events: &[Event],
    viewer_timezone: &Tz,
) -> Option<NaiveDate> {
    let visible_calendar_ids: std::collections::HashSet<uuid::Uuid> = calendars
        .iter()
        .filter(|calendar| calendar.visible)
        .map(|calendar| calendar.id)
        .collect();

    events
        .iter()
        .filter(|event| visible_calendar_ids.contains(&event.calendar_id))
        .filter_map(|event| earliest_event_date(event, today, viewer_timezone))
        .min()
}

const RECURRENCE_LIMIT: u16 = 4096;

fn expand_event_with_identity(
    event: &Event,
    first: NaiveDate,
    last: NaiveDate,
) -> Vec<ProjectedEvent> {
    expand_event(event, first, last)
        .into_iter()
        .map(|occurrence| {
            let original_recurrence_id = recurrence_id_for_generated_occurrence(event, &occurrence);
            ProjectedEvent {
                event: occurrence,
                original_recurrence_id,
            }
        })
        .collect()
}

fn recurrence_id_for_generated_occurrence(
    master: &Event,
    occurrence: &Event,
) -> Option<RecurrenceId> {
    if master.recurrence.is_none() || occurrence.recurrence.is_some() {
        return None;
    }
    match (&master.schedule, &occurrence.schedule) {
        (EventSchedule::AllDay { .. }, EventSchedule::AllDay { start_date, .. }) => {
            Some(RecurrenceId::AllDay(*start_date))
        }
        (EventSchedule::Timed { timezone, .. }, EventSchedule::Timed { start, .. }) => {
            Some(RecurrenceId::Timed {
                date_time: *start,
                timezone: timezone.clone(),
            })
        }
        _ => None,
    }
}

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
        .filter_map(|date| occurrence_event(event, date, duration))
        .collect()
}

fn earliest_event_date<Tz: TimeZone>(
    event: &Event,
    today: NaiveDate,
    viewer_timezone: &Tz,
) -> Option<NaiveDate> {
    let localizer = TimezoneLocalizer(viewer_timezone);
    if event.recurrence.is_none() {
        return event_date_bounds(&event.schedule, &localizer)
            .filter(|(_, last_date)| *last_date >= today)
            .map(|(first_date, _)| first_date.max(today));
    }
    let Ok(source) = recurrence_source(event) else {
        return event_date_bounds(&event.schedule, &localizer)
            .filter(|(_, last_date)| *last_date >= today)
            .map(|(first_date, _)| first_date.max(today));
    };
    let Ok(set) = source.parse::<RRuleSet>() else {
        return event_date_bounds(&event.schedule, &localizer)
            .filter(|(_, last_date)| *last_date >= today)
            .map(|(first_date, _)| first_date.max(today));
    };
    let duration = match &event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => Duration::days((*end_date_exclusive - *start_date).num_days()),
        EventSchedule::Timed { start, end, .. } => *end - *start,
    };

    // Apply the recurrence limit after seeking to today.  Limiting from the
    // DTSTART would exhaust the result set on long-running unbounded rules
    // before any upcoming occurrence is reached.  The small lookahead also
    // covers an occurrence whose end is exactly today's local midnight; its
    // date bounds do not occupy today even though its start is just inside
    // this lower bound.
    let recurrence_timezone = recurrence_timezone(event);
    let today_start = viewer_timezone
        .with_ymd_and_hms(today.year(), today.month(), today.day(), 0, 0, 0)
        .single()
        .expect("projection dates are valid")
        .with_timezone(&recurrence_timezone);
    let lower = today_start - duration - Duration::seconds(1);

    // RRuleSet does not emit DTSTART when the source has only RDATE
    // properties.  DTSTART is nevertheless the event's first recurrence
    // instance, unless explicitly excluded.
    let recurrence_start = *set.get_dt_start();
    let start_is_excluded = set
        .get_exdate()
        .iter()
        .any(|date| date.timestamp() == recurrence_start.timestamp());

    let mut dates = set
        .after(lower)
        .all(2)
        .dates
        .into_iter()
        .collect::<Vec<_>>();

    if recurrence_start > lower && !start_is_excluded {
        dates.push(recurrence_start);
    }

    dates.sort_by(|a, b| a.partial_cmp(b).expect("rrule dates are orderable"));
    dates.dedup();
    dates
        .into_iter()
        .filter_map(|date| occurrence_event(event, date, duration))
        .filter_map(|event| event_date_bounds(&event.schedule, &localizer))
        .filter(|(_, last_date)| *last_date >= today)
        .map(|(first_date, _)| first_date.max(today))
        .min()
}

fn occurrence_event(event: &Event, date: DateTime<RRuleTz>, duration: Duration) -> Option<Event> {
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
