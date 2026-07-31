use chrono::{DateTime, Datelike, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use rrule::{RRuleSet, Tz as RRuleTz};
use std::str::FromStr;

use crate::model::{Event, EventSchedule, RecurrenceId, RecurrenceSpec};

/// The frequencies exposed by the simple recurrence editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Frequency {
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weekday {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndCondition {
    Never,
    Count(u32),
    Until(NaiveDate),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceForm {
    pub frequency: Frequency,
    pub interval: u32,
    pub weekdays: Vec<Weekday>,
    pub end: EndCondition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecurrencePresentation {
    Editable {
        form: RecurrenceForm,
        summary: String,
    },
    Custom {
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceFormError {
    InvalidInterval,
    WeeklyRequiresWeekday,
    WeekdaysOnlyApplyToWeekly,
    InvalidCount,
    InvalidTimezone,
}

impl std::fmt::Display for RecurrenceFormError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidInterval => "recurrence interval must be at least one",
            Self::WeeklyRequiresWeekday => "weekly recurrence requires a weekday",
            Self::WeekdaysOnlyApplyToWeekly => "weekdays only apply to weekly recurrence",
            Self::InvalidCount => "recurrence count must be at least one",
            Self::InvalidTimezone => {
                "recurrence boundary is not representable in the event timezone"
            }
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RecurrenceFormError {}

/// The two masters and the schedule needed to represent a recurrence split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecurrenceSplit {
    pub original_recurrence: RecurrenceSpec,
    pub future_schedule: EventSchedule,
    pub future_recurrence: RecurrenceSpec,
}

/// An occurrence split is intentionally limited to the recurrence shapes that
/// the simple editor can understand.  In particular, a split must not turn a
/// recurrence containing detached dates into two different recurrences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecurrenceSplitError {
    MissingRecurrence,
    UnsupportedRecurrence,
    InvalidRecurrence,
    MismatchedIdentity,
    FirstOccurrence,
    NotGenerated,
}

impl std::fmt::Display for RecurrenceSplitError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::MissingRecurrence => "event has no recurrence",
            Self::UnsupportedRecurrence => "recurrence is not a supported simple rule",
            Self::InvalidRecurrence => "recurrence is invalid",
            Self::MismatchedIdentity => "recurrence identity does not match the event",
            Self::FirstOccurrence => "the first recurrence occurrence cannot be split",
            Self::NotGenerated => "recurrence identity is not a generated occurrence",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for RecurrenceSplitError {}

/// Split a simple recurrence at one of its generated occurrences.
///
/// The input master is only read.  The returned future schedule is built from
/// the generated occurrence (rather than by adding a fixed UTC duration to the
/// master), which preserves a TZID recurrence's local wall-clock time over a
/// daylight-saving transition.
pub fn split_recurrence_at(
    master: &Event,
    recurrence_id: &RecurrenceId,
) -> Result<RecurrenceSplit, RecurrenceSplitError> {
    let recurrence = master
        .recurrence
        .as_ref()
        .ok_or(RecurrenceSplitError::MissingRecurrence)?;
    if !recurrence.rdate.is_empty() || !recurrence.exdate.is_empty() {
        return Err(RecurrenceSplitError::UnsupportedRecurrence);
    }
    if recurrence.rrule.len() != 1 {
        return Err(RecurrenceSplitError::UnsupportedRecurrence);
    }
    if !identity_matches_schedule(&master.schedule, recurrence_id) {
        return Err(RecurrenceSplitError::MismatchedIdentity);
    }

    // Besides checking the supported subset, this parses UNTIL using the
    // schedule's timezone rules.  The recurrence source below is the same
    // DTSTART form used by the projection code, so the generated identities
    // have precisely the same DST behavior as displayed occurrences.
    let form = parse_simple_rule_for_split(&recurrence.rrule[0], &master.schedule)
        .ok_or(RecurrenceSplitError::UnsupportedRecurrence)?;
    let source = split_recurrence_source(&master.schedule, &recurrence.rrule[0])
        .ok_or(RecurrenceSplitError::InvalidRecurrence)?;
    let set = source
        .parse::<RRuleSet>()
        .map_err(|_| RecurrenceSplitError::InvalidRecurrence)?;

    let first = schedule_start(&master.schedule);
    let target = identity_target(recurrence_id, &master.schedule)
        .ok_or(RecurrenceSplitError::MismatchedIdentity)?;
    if target <= first {
        return Err(RecurrenceSplitError::FirstOccurrence);
    }

    // RRuleSet::after is exclusive.  A one-second margin includes DTSTART and
    // the selected occurrence without needing to know the rule's interval.
    let lower = first
        .checked_sub_signed(Duration::seconds(1))
        .ok_or(RecurrenceSplitError::InvalidRecurrence)?;
    let upper = target
        .checked_add_signed(Duration::seconds(1))
        .ok_or(RecurrenceSplitError::InvalidRecurrence)?;
    let timezone = recurrence_rrule_timezone(&master.schedule)
        .ok_or(RecurrenceSplitError::InvalidRecurrence)?;
    let dates = set
        .after(lower.with_timezone(&timezone))
        .before(upper.with_timezone(&timezone))
        .all(u16::MAX)
        .dates;

    let mut occurrences = vec![first];
    occurrences.extend(dates.into_iter().map(|date| date.fixed_offset()));
    occurrences.sort();
    occurrences.dedup();

    let selected_index = occurrences
        .iter()
        .position(|occurrence| occurrence_matches(*occurrence, recurrence_id, &master.schedule))
        .ok_or(RecurrenceSplitError::NotGenerated)?;
    if selected_index == 0 {
        return Err(RecurrenceSplitError::FirstOccurrence);
    }
    let total = match form.end {
        EndCondition::Count(count) => Some(count),
        EndCondition::Never | EndCondition::Until(_) => None,
    };
    let selected = occurrences[selected_index];
    let past_count = selected_index as u32;
    let future_count = total.map(|total| total.saturating_sub(past_count));
    if future_count == Some(0) {
        return Err(RecurrenceSplitError::NotGenerated);
    }

    let original_rule = split_rule_with_count(&recurrence.rrule[0], past_count, false);
    let future_rule = match future_count {
        Some(count) => split_rule_with_count(&recurrence.rrule[0], count, true),
        None => recurrence.rrule[0].clone(),
    };
    let duration = schedule_duration(&master.schedule);
    let future_schedule = schedule_at_occurrence(&master.schedule, selected, duration)
        .ok_or(RecurrenceSplitError::InvalidRecurrence)?;

    Ok(RecurrenceSplit {
        original_recurrence: RecurrenceSpec {
            rrule: vec![original_rule],
            rdate: Vec::new(),
            exdate: Vec::new(),
        },
        future_schedule,
        future_recurrence: RecurrenceSpec {
            rrule: vec![future_rule],
            rdate: Vec::new(),
            exdate: Vec::new(),
        },
    })
}

fn identity_matches_schedule(schedule: &EventSchedule, identity: &RecurrenceId) -> bool {
    match (schedule, identity) {
        (EventSchedule::AllDay { .. }, RecurrenceId::AllDay(_)) => true,
        (
            EventSchedule::Timed { timezone, .. },
            RecurrenceId::Timed {
                timezone: identity_timezone,
                ..
            },
        ) => timezone == identity_timezone,
        _ => false,
    }
}

fn schedule_start(schedule: &EventSchedule) -> DateTime<FixedOffset> {
    match schedule {
        EventSchedule::AllDay { start_date, .. } => {
            let utc = FixedOffset::east_opt(0).expect("UTC offset is valid");
            utc.from_local_datetime(
                &start_date
                    .and_hms_opt(0, 0, 0)
                    .expect("valid date has a midnight"),
            )
            .single()
            .expect("UTC midnight is unambiguous")
        }
        EventSchedule::Timed { start, .. } => start.with_timezone(&Utc).fixed_offset(),
    }
}

fn identity_target(
    identity: &RecurrenceId,
    schedule: &EventSchedule,
) -> Option<DateTime<FixedOffset>> {
    match (schedule, identity) {
        (EventSchedule::AllDay { .. }, RecurrenceId::AllDay(date)) => {
            let utc = FixedOffset::east_opt(0)?;
            utc.from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
                .single()
        }
        (EventSchedule::Timed { .. }, RecurrenceId::Timed { date_time, .. }) => {
            Some(date_time.with_timezone(&Utc).fixed_offset())
        }
        _ => None,
    }
}

fn occurrence_matches(
    occurrence: DateTime<FixedOffset>,
    identity: &RecurrenceId,
    schedule: &EventSchedule,
) -> bool {
    match (schedule, identity) {
        (EventSchedule::AllDay { .. }, RecurrenceId::AllDay(date)) => {
            occurrence.date_naive() == *date
        }
        (
            EventSchedule::Timed {
                timezone: Some(timezone),
                ..
            },
            RecurrenceId::Timed { date_time, .. },
        ) => Tz::from_str(timezone)
            .map(|timezone| {
                occurrence.with_timezone(&timezone).naive_local()
                    == date_time.with_timezone(&timezone).naive_local()
            })
            .unwrap_or(false),
        (EventSchedule::Timed { timezone: None, .. }, RecurrenceId::Timed { date_time, .. }) => {
            occurrence == *date_time
        }
        _ => false,
    }
}

fn recurrence_rrule_timezone(schedule: &EventSchedule) -> Option<RRuleTz> {
    match schedule {
        EventSchedule::AllDay { .. } => Some(RRuleTz::UTC),
        EventSchedule::Timed { timezone: None, .. } => Some(RRuleTz::UTC),
        EventSchedule::Timed {
            timezone: Some(timezone),
            ..
        } => Tz::from_str(timezone).ok().map(RRuleTz::from),
    }
}

fn split_recurrence_source(schedule: &EventSchedule, rule: &str) -> Option<String> {
    let dtstart = match schedule {
        EventSchedule::AllDay { start_date, .. } => {
            // Use an explicit UTC midnight so date-only recurrence expansion
            // does not depend on the host's local timezone.
            format!("DTSTART:{}T000000Z", start_date.format("%Y%m%d"))
        }
        EventSchedule::Timed {
            start,
            timezone: None,
            ..
        } => format!(
            "DTSTART:{}",
            start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
        ),
        EventSchedule::Timed {
            start,
            timezone: Some(timezone),
            ..
        } => {
            let parsed_timezone = Tz::from_str(timezone).ok()?;
            format!(
                "DTSTART;TZID={timezone}:{}",
                start
                    .with_timezone(&parsed_timezone)
                    .format("%Y%m%dT%H%M%S")
            )
        }
    };
    let rule = if matches!(schedule, EventSchedule::AllDay { .. }) {
        split_all_day_rule_for_source(rule)
    } else {
        rule.to_owned()
    };
    Some(format!("{dtstart}\n{rule}"))
}

fn split_all_day_rule_for_source(rule: &str) -> String {
    let Some((property, value)) = rule.split_once(':') else {
        return rule.to_owned();
    };
    let value = value
        .split(';')
        .map(|item| {
            let Some((key, until)) = item.split_once('=') else {
                return item.to_owned();
            };
            if key.eq_ignore_ascii_case("UNTIL") && until.len() == 8 {
                format!("{key}={until}T000000Z")
            } else {
                item.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(";");
    format!("{property}:{value}")
}

fn schedule_duration(schedule: &EventSchedule) -> Duration {
    match schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => Duration::days((*end_date_exclusive - *start_date).num_days()),
        EventSchedule::Timed { start, end, .. } => *end - *start,
    }
}

fn schedule_at_occurrence(
    schedule: &EventSchedule,
    occurrence: DateTime<FixedOffset>,
    duration: Duration,
) -> Option<EventSchedule> {
    match schedule {
        EventSchedule::AllDay { .. } => {
            let start_date = occurrence.date_naive();
            Some(EventSchedule::AllDay {
                start_date,
                end_date_exclusive: start_date.checked_add_signed(duration)?,
            })
        }
        EventSchedule::Timed { timezone, .. } => Some(EventSchedule::Timed {
            start: occurrence,
            end: occurrence.checked_add_signed(duration)?,
            timezone: timezone.clone(),
        }),
    }
}

fn split_rule_with_count(rule: &str, count: u32, preserve_end: bool) -> String {
    let (property, value) = rule
        .split_once(':')
        .expect("split rules have already been parsed");
    let mut found = false;
    let mut items = Vec::new();
    for item in value.split(';') {
        if let Some((key, _)) = item.split_once('=') {
            if key.eq_ignore_ascii_case("COUNT") {
                items.push(format!("COUNT={count}"));
                found = true;
                continue;
            }
            if !preserve_end && key.eq_ignore_ascii_case("UNTIL") {
                continue;
            }
        }
        items.push(item.to_owned());
    }
    if !found {
        items.push(format!("COUNT={count}"));
    }
    format!("{property}:{}", items.join(";"))
}

/// Turn the state of the simple recurrence editor into the durable model.
pub fn recurrence_from_form(
    form: &RecurrenceForm,
    schedule: &EventSchedule,
) -> Result<Option<RecurrenceSpec>, RecurrenceFormError> {
    if matches!(form.frequency, Frequency::None) {
        return Ok(None);
    }
    if form.interval == 0 {
        return Err(RecurrenceFormError::InvalidInterval);
    }
    if !matches!(form.frequency, Frequency::Weekly) && !form.weekdays.is_empty() {
        return Err(RecurrenceFormError::WeekdaysOnlyApplyToWeekly);
    }
    if matches!(form.frequency, Frequency::Weekly) && form.weekdays.is_empty() {
        return Err(RecurrenceFormError::WeeklyRequiresWeekday);
    }
    if matches!(form.end, EndCondition::Count(0)) {
        return Err(RecurrenceFormError::InvalidCount);
    }
    let frequency = match form.frequency {
        Frequency::None => unreachable!(),
        Frequency::Daily => "DAILY",
        Frequency::Weekly => "WEEKLY",
        Frequency::Monthly => "MONTHLY",
        Frequency::Yearly => "YEARLY",
    };
    let mut rule = format!("RRULE:FREQ={frequency}");
    if form.interval != 1 {
        rule.push_str(&format!(";INTERVAL={}", form.interval));
    }
    if matches!(form.frequency, Frequency::Weekly) {
        let mut weekdays = form.weekdays.clone();
        weekdays.sort_by_key(|weekday| weekday_number(*weekday));
        weekdays.dedup();
        if weekdays.is_empty() {
            return Err(RecurrenceFormError::WeeklyRequiresWeekday);
        }
        rule.push_str(";BYDAY=");
        rule.push_str(
            &weekdays
                .iter()
                .map(|weekday| weekday_code(*weekday))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    append_end_condition(&mut rule, &form.end, schedule)?;

    Ok(Some(RecurrenceSpec {
        rrule: vec![rule],
        rdate: Vec::new(),
        exdate: Vec::new(),
    }))
}

/// Return a form when the recurrence is one of the rules represented by the
/// editor.  Other recurrence properties remain custom so callers can preserve
/// them rather than accidentally rewriting them through the simple editor.
pub fn recurrence_presentation(
    recurrence: Option<&RecurrenceSpec>,
    schedule: &EventSchedule,
) -> RecurrencePresentation {
    let Some(recurrence) = recurrence else {
        let form = RecurrenceForm {
            frequency: Frequency::None,
            interval: 1,
            weekdays: Vec::new(),
            end: EndCondition::Never,
        };
        return RecurrencePresentation::Editable {
            summary: summary_for_form(&form),
            form,
        };
    };

    if !recurrence.rdate.is_empty() || !recurrence.exdate.is_empty() || recurrence.rrule.len() != 1
    {
        return RecurrencePresentation::Custom {
            summary: custom_summary(recurrence),
        };
    }

    match parse_simple_rule(&recurrence.rrule[0], schedule) {
        Some(form) => RecurrencePresentation::Editable {
            summary: summary_for_form(&form),
            form,
        },
        None => RecurrencePresentation::Custom {
            summary: custom_summary(recurrence),
        },
    }
}

fn append_end_condition(
    rule: &mut String,
    end: &EndCondition,
    schedule: &EventSchedule,
) -> Result<(), RecurrenceFormError> {
    match end {
        EndCondition::Never => {}
        EndCondition::Count(count) => rule.push_str(&format!(";COUNT={count}")),
        EndCondition::Until(date) => {
            rule.push_str(";UNTIL=");
            rule.push_str(&until_value(*date, schedule)?);
        }
    }
    Ok(())
}

fn until_value(date: NaiveDate, schedule: &EventSchedule) -> Result<String, RecurrenceFormError> {
    match schedule {
        EventSchedule::AllDay { .. } => Ok(date.format("%Y%m%d").to_string()),
        EventSchedule::Timed {
            start, timezone, ..
        } => {
            let (local_time, timezone) = match timezone.as_deref() {
                None => (start.with_timezone(&Utc).time(), None),
                Some(tzid) => {
                    let timezone = tzid
                        .parse::<Tz>()
                        .map_err(|_| RecurrenceFormError::InvalidTimezone)?;
                    (start.with_timezone(&timezone).time(), Some(timezone))
                }
            };
            let local_date_time = date.and_time(local_time);
            let utc = match timezone {
                None => Utc.from_utc_datetime(&local_date_time),
                Some(timezone) => timezone
                    .from_local_datetime(&local_date_time)
                    .single()
                    .ok_or(RecurrenceFormError::InvalidTimezone)?
                    .with_timezone(&Utc),
            };
            Ok(utc.format("%Y%m%dT%H%M%SZ").to_string())
        }
    }
}

fn parse_simple_rule(rule: &str, schedule: &EventSchedule) -> Option<RecurrenceForm> {
    parse_simple_rule_with_wkst(rule, schedule, false)
}

fn parse_simple_rule_for_split(rule: &str, schedule: &EventSchedule) -> Option<RecurrenceForm> {
    let (_, value) = rule.split_once(':')?;
    let has_wkst = value.split(';').any(|item| {
        item.split_once('=')
            .is_some_and(|(key, _)| key.eq_ignore_ascii_case("WKST"))
    });
    if has_wkst {
        // WKST is intentionally not part of the Phase 1 editor's editable
        // subset.  A split, however, can preserve it as long as the complete
        // DTSTART/RRULE pair is valid according to the recurrence parser.
        let source = split_recurrence_source(schedule, rule)?;
        source.parse::<RRuleSet>().ok()?;
    }
    parse_simple_rule_with_wkst(rule, schedule, has_wkst)
}

fn parse_simple_rule_with_wkst(
    rule: &str,
    schedule: &EventSchedule,
    allow_wkst: bool,
) -> Option<RecurrenceForm> {
    let (property, value) = rule.split_once(':')?;
    if !property.eq_ignore_ascii_case("RRULE") {
        return None;
    }
    let mut frequency = None;
    let mut interval = 1;
    let mut has_interval = false;
    let mut weekdays = None;
    let mut has_weekdays = false;
    let mut has_wkst = false;
    let mut end = EndCondition::Never;
    let mut has_end = false;

    for item in value.split(';') {
        let (key, value) = item.split_once('=')?;
        if key.is_empty() || value.is_empty() {
            return None;
        }
        match key.to_ascii_uppercase().as_str() {
            "FREQ" if frequency.is_none() => {
                frequency = Some(match value.to_ascii_uppercase().as_str() {
                    "DAILY" => Frequency::Daily,
                    "WEEKLY" => Frequency::Weekly,
                    "MONTHLY" => Frequency::Monthly,
                    "YEARLY" => Frequency::Yearly,
                    _ => return None,
                });
            }
            "INTERVAL" if !has_interval => {
                interval = value.parse().ok()?;
                if interval == 0 {
                    return None;
                }
                has_interval = true;
            }
            "BYDAY" if !has_weekdays => {
                let parsed = parse_weekdays(value)?;
                if parsed.is_empty() {
                    return None;
                }
                weekdays = Some(parsed);
                has_weekdays = true;
            }
            "WKST" if allow_wkst && !has_wkst => {
                has_wkst = true;
            }
            "COUNT" if !has_end => {
                let count = value.parse().ok()?;
                if count == 0 {
                    return None;
                }
                end = EndCondition::Count(count);
                has_end = true;
            }
            "UNTIL" if !has_end => {
                end = EndCondition::Until(parse_until(value, schedule)?);
                has_end = true;
            }
            _ => return None,
        }
    }

    let frequency = frequency?;
    if !matches!(frequency, Frequency::Weekly) && weekdays.is_some() {
        return None;
    }
    let weekdays = match frequency {
        Frequency::Weekly => weekdays.unwrap_or_else(|| vec![schedule_weekday(schedule)]),
        _ => Vec::new(),
    };
    Some(RecurrenceForm {
        frequency,
        interval,
        weekdays,
        end,
    })
}

fn parse_weekdays(value: &str) -> Option<Vec<Weekday>> {
    let mut weekdays = Vec::new();
    for code in value.split(',') {
        let weekday = match code.to_ascii_uppercase().as_str() {
            "MO" => Weekday::Monday,
            "TU" => Weekday::Tuesday,
            "WE" => Weekday::Wednesday,
            "TH" => Weekday::Thursday,
            "FR" => Weekday::Friday,
            "SA" => Weekday::Saturday,
            "SU" => Weekday::Sunday,
            _ => return None,
        };
        if weekdays.contains(&weekday) {
            return None;
        }
        weekdays.push(weekday);
    }
    weekdays.sort_by_key(|weekday| weekday_number(*weekday));
    Some(weekdays)
}

fn parse_until(value: &str, schedule: &EventSchedule) -> Option<NaiveDate> {
    match schedule {
        EventSchedule::AllDay { .. } => NaiveDate::parse_from_str(value, "%Y%m%d").ok(),
        EventSchedule::Timed { .. } => {
            if value.len() != 16 || value.as_bytes().get(8) != Some(&b'T') || !value.ends_with('Z')
            {
                return None;
            }
            let date = NaiveDate::parse_from_str(&value[..8], "%Y%m%d").ok()?;
            (until_value(date, schedule).ok()? == value).then_some(date)
        }
    }
}

fn schedule_weekday(schedule: &EventSchedule) -> Weekday {
    let date = match schedule {
        EventSchedule::AllDay { start_date, .. } => *start_date,
        EventSchedule::Timed { start, .. } => start.date_naive(),
    };
    match date.weekday().number_from_monday() {
        1 => Weekday::Monday,
        2 => Weekday::Tuesday,
        3 => Weekday::Wednesday,
        4 => Weekday::Thursday,
        5 => Weekday::Friday,
        6 => Weekday::Saturday,
        _ => Weekday::Sunday,
    }
}

fn weekday_number(weekday: Weekday) -> u8 {
    match weekday {
        Weekday::Monday => 1,
        Weekday::Tuesday => 2,
        Weekday::Wednesday => 3,
        Weekday::Thursday => 4,
        Weekday::Friday => 5,
        Weekday::Saturday => 6,
        Weekday::Sunday => 7,
    }
}

fn weekday_code(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Monday => "MO",
        Weekday::Tuesday => "TU",
        Weekday::Wednesday => "WE",
        Weekday::Thursday => "TH",
        Weekday::Friday => "FR",
        Weekday::Saturday => "SA",
        Weekday::Sunday => "SU",
    }
}

fn summary_for_form(form: &RecurrenceForm) -> String {
    let mut summary = match form.frequency {
        Frequency::None => "Does not repeat".to_owned(),
        Frequency::Daily => unit_summary(form.interval, "day"),
        Frequency::Weekly => {
            let days = form
                .weekdays
                .iter()
                .map(|weekday| weekday_name(*weekday))
                .collect::<Vec<_>>()
                .join(", ");
            format!("{} on {days}", unit_summary(form.interval, "week"))
        }
        Frequency::Monthly => unit_summary(form.interval, "month"),
        Frequency::Yearly => unit_summary(form.interval, "year"),
    };
    match form.end {
        EndCondition::Never => {}
        EndCondition::Count(count) => summary.push_str(&format!(" ({count} times)")),
        EndCondition::Until(date) => summary.push_str(&format!(" until {date}")),
    }
    summary
}

fn unit_summary(interval: u32, unit: &str) -> String {
    if interval == 1 {
        return format!("Every {unit}");
    }
    format!("Every {interval} {unit}s")
}

fn weekday_name(weekday: Weekday) -> &'static str {
    match weekday {
        Weekday::Monday => "Monday",
        Weekday::Tuesday => "Tuesday",
        Weekday::Wednesday => "Wednesday",
        Weekday::Thursday => "Thursday",
        Weekday::Friday => "Friday",
        Weekday::Saturday => "Saturday",
        Weekday::Sunday => "Sunday",
    }
}

fn custom_summary(recurrence: &RecurrenceSpec) -> String {
    let mut details = Vec::new();
    if !recurrence.rrule.is_empty() {
        details.push("rule".to_owned());
    }
    if !recurrence.rdate.is_empty() {
        details.push("additional dates".to_owned());
    }
    if !recurrence.exdate.is_empty() {
        details.push("excluded dates".to_owned());
    }
    if details.is_empty() {
        "Custom recurrence".to_owned()
    } else {
        format!("Custom recurrence ({})", details.join(", "))
    }
}
