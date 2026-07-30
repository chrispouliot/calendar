use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;

use crate::model::{EventSchedule, RecurrenceSpec};

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
    let (property, value) = rule.split_once(':')?;
    if !property.eq_ignore_ascii_case("RRULE") {
        return None;
    }
    let mut frequency = None;
    let mut interval = 1;
    let mut has_interval = false;
    let mut weekdays = None;
    let mut has_weekdays = false;
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
