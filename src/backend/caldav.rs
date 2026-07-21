use chrono::{DateTime, FixedOffset, TimeZone};
use chrono_tz::Tz;
use icalendar::{
    Calendar, CalendarComponent, CalendarDateTime, Component, DatePerhapsTime, EventLike,
};
use quick_xml::escape::unescape;
use quick_xml::events::Event as XmlEvent;
use quick_xml::name::{Namespace, ResolveResult};
use quick_xml::reader::NsReader;
use std::str::FromStr;
use uuid::Uuid;

const DAV_NAMESPACE: &[u8] = b"DAV:";
const CALDAV_NAMESPACE: &[u8] = b"urn:ietf:params:xml:ns:caldav";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MappedEvent {
    pub event: crate::model::Event,
    pub remote_uid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EventMappingError {
    MissingUid,
    MissingDtstart,
    FloatingTime,
    UnsupportedRecurrence,
    MultipleEvents,
    NoEvents,
    Parse(String),
    UnsupportedData(String),
}

impl std::fmt::Display for EventMappingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingUid => write!(formatter, "VEVENT has no UID"),
            Self::MissingDtstart => write!(formatter, "VEVENT has no DTSTART"),
            Self::FloatingTime => write!(formatter, "floating DATE-TIME values are unsupported"),
            Self::UnsupportedRecurrence => write!(formatter, "recurrence is unsupported"),
            Self::MultipleEvents => write!(formatter, "calendar contains multiple VEVENTs"),
            Self::NoEvents => write!(formatter, "calendar contains no VEVENT"),
            Self::Parse(message) => write!(formatter, "invalid iCalendar data: {message}"),
            Self::UnsupportedData(message) => {
                write!(formatter, "unsupported iCalendar data: {message}")
            }
        }
    }
}

impl std::error::Error for EventMappingError {}

/// Map the one-event, non-recurring subset accepted by the local event model.
///
/// The CalDAV resource identity (href and ETag) deliberately does not enter
/// this value; callers persist that identity alongside the returned event.
pub fn map_icalendar_event(
    resource: &str,
    event_id: Uuid,
    calendar_id: Uuid,
) -> Result<MappedEvent, EventMappingError> {
    let normalized = icalendar::parser::unfold(resource);
    let roots = icalendar::parser::read_calendar_simple(&normalized)
        .map_err(|error| EventMappingError::Parse(error.to_string()))?;
    if roots.len() != 1 || roots[0].name.as_ref() != "VCALENDAR" {
        return Err(EventMappingError::Parse(
            "resource is not exactly one VCALENDAR".to_owned(),
        ));
    }
    let calendar = icalendar::parser::read_calendar(&normalized)
        .map_err(EventMappingError::Parse)
        .map(Into::<Calendar>::into)?;
    let mut events = calendar
        .components
        .iter()
        .filter_map(|component| match component {
            CalendarComponent::Event(event) => Some(event),
            _ => None,
        });
    let event = events.next().ok_or(EventMappingError::NoEvents)?;
    if events.next().is_some() {
        return Err(EventMappingError::MultipleEvents);
    }

    let uid = event
        .get_uid()
        .filter(|uid| !uid.trim().is_empty())
        .ok_or(EventMappingError::MissingUid)?;
    if has_property(event, "RRULE")
        || has_property(event, "RDATE")
        || has_property(event, "EXDATE")
        || has_property(event, "RECURRENCE-ID")
    {
        return Err(EventMappingError::UnsupportedRecurrence);
    }
    if has_property(event, "DURATION") {
        return Err(EventMappingError::UnsupportedData(
            "DURATION is not supported".to_owned(),
        ));
    }
    if !event.components().is_empty() {
        return Err(EventMappingError::UnsupportedData(
            "nested VEVENT components are not supported".to_owned(),
        ));
    }

    let start = event.get_start().ok_or_else(|| {
        if has_property(event, "DTSTART") {
            EventMappingError::Parse("invalid DTSTART".to_owned())
        } else {
            EventMappingError::MissingDtstart
        }
    })?;
    let end = event.get_end();
    let schedule = map_schedule(start, end)?;
    let mapped = crate::model::Event {
        id: event_id,
        calendar_id,
        title: event.get_summary().unwrap_or_default().to_owned(),
        location: event.get_location().unwrap_or_default().to_owned(),
        description: event.get_description().unwrap_or_default().to_owned(),
        schedule,
        recurrence: None,
        reminders: Vec::new(),
    };

    Ok(MappedEvent {
        event: mapped,
        remote_uid: uid.to_owned(),
    })
}

fn has_property(event: &icalendar::Event, key: &str) -> bool {
    event.properties().contains_key(key) || event.multi_properties().contains_key(key)
}

fn map_schedule(
    start: DatePerhapsTime,
    end: Option<DatePerhapsTime>,
) -> Result<crate::model::EventSchedule, EventMappingError> {
    match start {
        DatePerhapsTime::Date(start_date) => {
            let end_date = match end {
                None => start_date
                    .succ_opt()
                    .ok_or_else(|| invalid_schedule("all-day start has no following date"))?,
                Some(DatePerhapsTime::Date(end_date)) => end_date,
                Some(DatePerhapsTime::DateTime(_)) => {
                    return Err(invalid_schedule(
                        "DTSTART and DTEND have different value types",
                    ));
                }
            };
            if end_date <= start_date {
                return Err(invalid_schedule("all-day range is not increasing"));
            }
            Ok(crate::model::EventSchedule::AllDay {
                start_date,
                end_date_exclusive: end_date,
            })
        }
        DatePerhapsTime::DateTime(start_time) => {
            let end_time = match end {
                Some(DatePerhapsTime::DateTime(end_time)) => end_time,
                None => return Err(invalid_schedule("timed event has no DTEND")),
                Some(DatePerhapsTime::Date(_)) => {
                    return Err(invalid_schedule(
                        "DTSTART and DTEND have different value types",
                    ));
                }
            };
            let (start, timezone) = map_datetime(start_time)?;
            let (end, end_timezone) = map_datetime(end_time)?;
            if timezone != end_timezone {
                return Err(invalid_schedule(
                    "DTSTART and DTEND use different time zones",
                ));
            }
            if end <= start {
                return Err(invalid_schedule("timed range is not increasing"));
            }
            Ok(crate::model::EventSchedule::Timed {
                start,
                end,
                timezone,
            })
        }
    }
}

fn map_datetime(
    value: CalendarDateTime,
) -> Result<(DateTime<FixedOffset>, Option<String>), EventMappingError> {
    match value {
        CalendarDateTime::Floating(_) => Err(EventMappingError::FloatingTime),
        CalendarDateTime::Utc(value) => Ok((value.fixed_offset(), None)),
        CalendarDateTime::WithTimezone { date_time, tzid } => {
            let timezone = Tz::from_str(&tzid)
                .map_err(|_| EventMappingError::UnsupportedData(format!("unknown TZID {tzid}")))?;
            let localized = timezone
                .from_local_datetime(&date_time)
                .single()
                .ok_or_else(|| invalid_schedule("TZID value is ambiguous or does not exist"))?;
            Ok((localized.fixed_offset(), Some(tzid)))
        }
    }
}

fn invalid_schedule(message: &str) -> EventMappingError {
    EventMappingError::UnsupportedData(message.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRecord {
    pub href: String,
    pub response_status: Option<u16>,
    pub etag: Option<String>,
    pub calendar_data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError(String);

impl std::fmt::Display for ParseError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "invalid CalDAV multistatus XML: {}", self.0)
    }
}

impl std::error::Error for ParseError {}

impl From<quick_xml::Error> for ParseError {
    fn from(error: quick_xml::Error) -> Self {
        Self(error.to_string())
    }
}

pub fn parse_multistatus(xml: &str) -> Result<Vec<ResourceRecord>, ParseError> {
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack = Vec::new();
    let mut records = Vec::new();
    let mut response = None;
    let mut propstat = None;
    let mut capture = None;
    let mut root_seen = false;

    loop {
        let (resolved_namespace, event) = reader.read_resolved_event()?;
        let namespace = NamespaceKind::from_resolved(resolved_namespace);
        match event {
            XmlEvent::Start(start) => {
                let local_name = start.local_name().as_ref().to_vec();
                if stack.is_empty() {
                    if root_seen {
                        return Err(malformed("multiple XML roots"));
                    }
                    root_seen = true;
                    if namespace != NamespaceKind::Dav || local_name != b"multistatus" {
                        return Err(malformed("root is not DAV:multistatus"));
                    }
                }

                let parent = stack.last();
                if namespace == NamespaceKind::Dav
                    && local_name == b"response"
                    && response.is_none()
                {
                    response = Some(ResponseState::default());
                } else if namespace == NamespaceKind::Dav
                    && local_name == b"propstat"
                    && response.is_some()
                    && propstat.is_none()
                {
                    propstat = Some(PropstatState::default());
                } else if let Some(parent) = parent
                    && let Some(kind) = capture_kind(
                        parent,
                        namespace,
                        &local_name,
                        response.is_some(),
                        propstat.is_some(),
                    )
                {
                    capture = Some(CaptureState {
                        depth: stack.len() + 1,
                        kind,
                        value: String::new(),
                    });
                }

                stack.push(Element {
                    namespace,
                    local_name,
                });
            }
            XmlEvent::Empty(empty) => {
                let local_name = empty.local_name().as_ref().to_vec();
                if stack.is_empty() {
                    if root_seen {
                        return Err(malformed("multiple XML roots"));
                    }
                    root_seen = true;
                    if namespace != NamespaceKind::Dav || local_name != b"multistatus" {
                        return Err(malformed("root is not DAV:multistatus"));
                    }
                }
                let parent = stack.last();
                if namespace == NamespaceKind::Dav
                    && local_name == b"response"
                    && response.is_none()
                {
                    records.push(ResponseState::default().finish());
                } else if namespace == NamespaceKind::Dav
                    && local_name == b"propstat"
                    && response.is_some()
                    && propstat.is_none()
                {
                    let state = PropstatState::default();
                    if let Some(response) = response.as_mut() {
                        response.apply_propstat(state);
                    }
                } else if let Some(parent) = parent
                    && let Some(kind) = capture_kind(
                        parent,
                        namespace,
                        &local_name,
                        response.is_some(),
                        propstat.is_some(),
                    )
                {
                    finish_capture(
                        &mut capture,
                        kind,
                        String::new(),
                        &mut response,
                        &mut propstat,
                    );
                }
            }
            XmlEvent::Text(value) => {
                if let Some(capture) = capture.as_mut() {
                    let decoded = value
                        .decode()
                        .map_err(|error| malformed(&error.to_string()))?;
                    let unescaped = unescape(decoded.as_ref())
                        .map_err(|error| malformed(&error.to_string()))?;
                    capture.value.push_str(&unescaped);
                }
            }
            XmlEvent::CData(value) => {
                if let Some(capture) = capture.as_mut() {
                    let decoded = value
                        .decode()
                        .map_err(|error| malformed(&error.to_string()))?;
                    capture.value.push_str(&decoded);
                }
            }
            XmlEvent::GeneralRef(value) => {
                if let Some(capture) = capture.as_mut() {
                    let decoded = value
                        .decode()
                        .map_err(|error| malformed(&error.to_string()))?;
                    let reference = format!("&{};", decoded);
                    let unescaped =
                        unescape(&reference).map_err(|error| malformed(&error.to_string()))?;
                    capture.value.push_str(&unescaped);
                }
            }
            XmlEvent::End(end) => {
                let element = stack.pop().ok_or_else(|| malformed("unmatched end tag"))?;
                if element.namespace != namespace || element.local_name != end.local_name().as_ref()
                {
                    return Err(malformed("mismatched end tag"));
                }

                if capture
                    .as_ref()
                    .is_some_and(|capture| capture.depth == stack.len() + 1)
                    && let Some(completed) = capture.take()
                {
                    finish_capture(
                        &mut capture,
                        completed.kind,
                        completed.value,
                        &mut response,
                        &mut propstat,
                    );
                }

                if namespace == NamespaceKind::Dav && element.local_name == b"propstat" {
                    let state = propstat
                        .take()
                        .ok_or_else(|| malformed("unmatched DAV:propstat"))?;
                    if let Some(response) = response.as_mut() {
                        response.apply_propstat(state);
                    }
                } else if namespace == NamespaceKind::Dav && element.local_name == b"response" {
                    let state = response
                        .take()
                        .ok_or_else(|| malformed("unmatched DAV:response"))?;
                    records.push(state.finish());
                }
            }
            XmlEvent::Eof => {
                if !root_seen
                    || !stack.is_empty()
                    || response.is_some()
                    || propstat.is_some()
                    || capture.is_some()
                {
                    return Err(malformed("truncated multistatus XML"));
                }
                return Ok(records);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NamespaceKind {
    Dav,
    Caldav,
    Other,
}

impl NamespaceKind {
    fn from_resolved(namespace: ResolveResult<'_>) -> Self {
        match namespace {
            ResolveResult::Bound(Namespace(value)) if value == DAV_NAMESPACE => Self::Dav,
            ResolveResult::Bound(Namespace(value)) if value == CALDAV_NAMESPACE => Self::Caldav,
            _ => Self::Other,
        }
    }
}

struct Element {
    namespace: NamespaceKind,
    local_name: Vec<u8>,
}

#[derive(Default)]
struct ResponseState {
    href: String,
    response_status: Option<u16>,
    successful_properties: Vec<Property>,
}

#[derive(Default)]
struct PropstatState {
    status: Option<u16>,
    properties: Vec<Property>,
}

struct Property {
    namespace: NamespaceKind,
    local_name: Vec<u8>,
    value: String,
}

enum CaptureKind {
    ResponseHref,
    ResponseStatus,
    PropstatStatus,
    Property(NamespaceKind, &'static [u8]),
}

struct CaptureState {
    depth: usize,
    kind: CaptureKind,
    value: String,
}

fn capture_kind(
    parent: &Element,
    namespace: NamespaceKind,
    local_name: &[u8],
    has_response: bool,
    has_propstat: bool,
) -> Option<CaptureKind> {
    if parent.namespace != NamespaceKind::Dav {
        return None;
    }
    if parent.local_name == b"response" && has_response && namespace == NamespaceKind::Dav {
        return match local_name {
            b"href" => Some(CaptureKind::ResponseHref),
            b"status" => Some(CaptureKind::ResponseStatus),
            _ => None,
        };
    }
    if parent.local_name == b"propstat" && has_propstat && namespace == NamespaceKind::Dav {
        return (local_name == b"status").then_some(CaptureKind::PropstatStatus);
    }
    if parent.local_name == b"prop" && has_propstat {
        return match (namespace, local_name) {
            (NamespaceKind::Dav, b"getetag") => {
                Some(CaptureKind::Property(NamespaceKind::Dav, b"getetag"))
            }
            (NamespaceKind::Caldav, b"calendar-data") => Some(CaptureKind::Property(
                NamespaceKind::Caldav,
                b"calendar-data",
            )),
            _ => None,
        };
    }
    None
}

fn finish_capture(
    capture: &mut Option<CaptureState>,
    kind: CaptureKind,
    value: String,
    response: &mut Option<ResponseState>,
    propstat: &mut Option<PropstatState>,
) {
    match kind {
        CaptureKind::ResponseHref => {
            if let Some(response) = response.as_mut() {
                response.href = value;
            }
        }
        CaptureKind::ResponseStatus => {
            if let Some(response) = response.as_mut() {
                response.response_status = parse_status_code(&value);
            }
        }
        CaptureKind::PropstatStatus => {
            if let Some(propstat) = propstat.as_mut() {
                propstat.status = parse_status_code(&value);
            }
        }
        CaptureKind::Property(namespace, local_name) => {
            if let Some(propstat) = propstat.as_mut() {
                propstat.properties.push(Property {
                    namespace,
                    local_name: local_name.to_vec(),
                    value,
                });
            }
        }
    }
    *capture = None;
}

impl ResponseState {
    fn apply_propstat(&mut self, propstat: PropstatState) {
        if propstat
            .status
            .is_some_and(|status| (200..300).contains(&status))
        {
            self.successful_properties.extend(propstat.properties);
        }
    }

    fn finish(self) -> ResourceRecord {
        let mut record = ResourceRecord {
            href: self.href,
            response_status: self.response_status,
            etag: None,
            calendar_data: None,
        };
        for property in self.successful_properties {
            match (property.namespace, property.local_name.as_slice()) {
                (NamespaceKind::Dav, b"getetag") => record.etag = Some(property.value),
                (NamespaceKind::Caldav, b"calendar-data") => {
                    record.calendar_data = Some(property.value)
                }
                _ => {}
            }
        }
        record
    }
}

fn parse_status_code(status: &str) -> Option<u16> {
    status.split_whitespace().nth(1)?.parse().ok()
}

fn malformed(message: &str) -> ParseError {
    ParseError(message.to_owned())
}
