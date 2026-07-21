use chrono::{DateTime, FixedOffset, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use icalendar::{
    Calendar, CalendarComponent, CalendarDateTime, Component, DatePerhapsTime, EventLike,
};
use quick_xml::escape::unescape;
use quick_xml::events::Event as XmlEvent;
use quick_xml::name::{Namespace, ResolveResult};
use quick_xml::reader::NsReader;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, USER_AGENT};
use reqwest::{Method, Url};
use std::str::FromStr;
use std::time::Duration;
use uuid::Uuid;

const DAV_NAMESPACE: &[u8] = b"DAV:";
const CALDAV_NAMESPACE: &[u8] = b"urn:ietf:params:xml:ns:caldav";
const APPLE_ICAL_NAMESPACE: &[u8] = b"http://apple.com/ns/ical/";

const PROPFIND: &str = "PROPFIND";
const REPORT: &str = "REPORT";
const USER_AGENT_VALUE: &str = "calendar-caldav/0.1";
const CURRENT_USER_PRINCIPAL_REQUEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
const CALENDAR_HOME_SET_REQUEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-home-set/></d:prop></d:propfind>"#;
const CALENDAR_LIST_REQUEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/"><d:prop><d:resourcetype/><d:displayname/><d:sync-token/><a:calendar-color/><d:current-user-privilege-set/></d:prop></d:propfind>"#;
const CALENDAR_QUERY_REQUEST: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop><c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="VEVENT"/></c:comp-filter></c:filter></c:calendar-query>"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaldavDiscovery {
    pub principal_url: String,
    pub calendar_home_url: String,
    pub calendars: Vec<DiscoveredCalendar>,
}

#[derive(Debug)]
pub enum CaldavError {
    HttpStatus { status: u16 },
    Transport,
    Url,
    Xml(ParseError),
}

impl std::fmt::Display for CaldavError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HttpStatus { status } => {
                write!(formatter, "CalDAV request returned HTTP {status}")
            }
            Self::Transport => write!(formatter, "CalDAV HTTP request failed"),
            Self::Url => write!(formatter, "invalid CalDAV URL"),
            Self::Xml(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for CaldavError {}

/// Credentials remain in this client and its requests only. Because this API
/// is blocking, callers should use it from a dedicated worker thread.
pub struct CaldavClient {
    server_url: String,
    username: String,
    password: String,
}

impl std::fmt::Debug for CaldavClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaldavClient")
            .field("server_url", &self.server_url)
            .finish_non_exhaustive()
    }
}

impl CaldavClient {
    pub fn new(server_url: String, username: String, password: String) -> Self {
        Self {
            server_url,
            username,
            password,
        }
    }

    pub fn discover(&self) -> Result<CaldavDiscovery, CaldavError> {
        let server_url = Url::parse(&self.server_url).map_err(|_| CaldavError::Url)?;
        let client = self.http_client()?;

        let principal_href = self.propfind(
            &client,
            &server_url,
            "0",
            CURRENT_USER_PRINCIPAL_REQUEST,
            parse_current_user_principal,
        )?;
        let principal_url = resolve_href(&server_url, &principal_href)?;

        let home_href = self.propfind(
            &client,
            &principal_url,
            "0",
            CALENDAR_HOME_SET_REQUEST,
            parse_calendar_home_set,
        )?;
        let calendar_home_url = resolve_href(&principal_url, &home_href)?;

        let calendars_xml =
            self.request(&client, &calendar_home_url, "1", CALENDAR_LIST_REQUEST)?;
        let mut calendars =
            parse_calendar_home_multistatus(&calendars_xml).map_err(CaldavError::Xml)?;
        for calendar in &mut calendars {
            calendar.href = resolve_href(&calendar_home_url, &calendar.href)?.to_string();
        }

        Ok(CaldavDiscovery {
            principal_url: principal_url.to_string(),
            calendar_home_url: calendar_home_url.to_string(),
            calendars,
        })
    }

    pub fn fetch_resources(&self, calendar_url: &str) -> Result<Vec<ResourceRecord>, CaldavError> {
        let calendar_url = Url::parse(calendar_url)
            .map_err(|_| CaldavError::Url)
            .and_then(validate_http_url)?;
        let client = self.http_client()?;
        let response =
            self.request_method(&client, REPORT, &calendar_url, "1", CALENDAR_QUERY_REQUEST)?;
        let mut resources = parse_multistatus(&response).map_err(CaldavError::Xml)?;
        for resource in &mut resources {
            resource.href = resolve_href(&calendar_url, &resource.href)?.to_string();
        }
        Ok(resources)
    }

    pub fn create_resource(
        &self,
        resource_url: &str,
        calendar_data: &str,
    ) -> Result<ResourceWriteResult, CaldavError> {
        self.write_resource(resource_url, calendar_data, None, Some("*"))
    }

    pub fn update_resource(
        &self,
        resource_url: &str,
        calendar_data: &str,
        base_etag: &str,
    ) -> Result<ResourceWriteResult, CaldavError> {
        self.write_resource(resource_url, calendar_data, Some(base_etag), None)
    }

    pub fn delete_resource(&self, resource_url: &str, base_etag: &str) -> Result<(), CaldavError> {
        let resource_url = Url::parse(resource_url)
            .map_err(|_| CaldavError::Url)
            .and_then(validate_http_url)?;
        let client = self.http_client()?;
        let response = self
            .authenticated_request(&client, Method::DELETE, &resource_url)
            .header(IF_MATCH, base_etag)
            .send()
            .map_err(|_| CaldavError::Transport)?;
        response_status(response).map(|_| ())
    }

    fn write_resource(
        &self,
        resource_url: &str,
        calendar_data: &str,
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<ResourceWriteResult, CaldavError> {
        let resource_url = Url::parse(resource_url)
            .map_err(|_| CaldavError::Url)
            .and_then(validate_http_url)?;
        let client = self.http_client()?;
        let mut request = self
            .authenticated_request(&client, Method::PUT, &resource_url)
            .header(CONTENT_TYPE, "text/calendar; charset=utf-8");
        if let Some(if_match) = if_match {
            request = request.header(IF_MATCH, if_match);
        }
        if let Some(if_none_match) = if_none_match {
            request = request.header(IF_NONE_MATCH, if_none_match);
        }
        let response = request
            .body(calendar_data.to_owned())
            .send()
            .map_err(|_| CaldavError::Transport)?;
        Ok(ResourceWriteResult {
            etag: response_status(response)?,
        })
    }

    fn http_client(&self) -> Result<Client, CaldavError> {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent(USER_AGENT_VALUE)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CaldavError::Transport)
    }

    fn propfind(
        &self,
        client: &Client,
        url: &Url,
        depth: &str,
        body: &str,
        parser: fn(&str) -> Result<String, ParseError>,
    ) -> Result<String, CaldavError> {
        let response = self.request(client, url, depth, body)?;
        parser(&response).map_err(CaldavError::Xml)
    }

    fn request(
        &self,
        client: &Client,
        url: &Url,
        depth: &str,
        body: &str,
    ) -> Result<String, CaldavError> {
        self.request_method(client, PROPFIND, url, depth, body)
    }

    fn request_method(
        &self,
        client: &Client,
        method_name: &str,
        url: &Url,
        depth: &str,
        body: &str,
    ) -> Result<String, CaldavError> {
        let method =
            Method::from_bytes(method_name.as_bytes()).map_err(|_| CaldavError::Transport)?;
        let response = self
            .authenticated_request(client, method, url)
            .header("Depth", depth)
            .header(CONTENT_TYPE, "application/xml; charset=utf-8")
            .body(body.to_owned())
            .send()
            .map_err(|_| CaldavError::Transport)?;
        response_body(response)
    }

    fn authenticated_request(
        &self,
        client: &Client,
        method: Method,
        url: &Url,
    ) -> reqwest::blocking::RequestBuilder {
        client
            .request(method, url.clone())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .basic_auth(&self.username, Some(&self.password))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceWriteResult {
    pub etag: Option<String>,
}

fn validate_http_url(url: Url) -> Result<Url, CaldavError> {
    if url.host_str().is_none() || !matches!(url.scheme(), "http" | "https") {
        return Err(CaldavError::Url);
    }
    Ok(url)
}

fn response_status(response: reqwest::blocking::Response) -> Result<Option<String>, CaldavError> {
    let status = response.status();
    if !status.is_success() {
        return Err(CaldavError::HttpStatus {
            status: status.as_u16(),
        });
    }
    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok(etag)
}

fn response_body(response: reqwest::blocking::Response) -> Result<String, CaldavError> {
    let status = response.status();
    if !status.is_success() {
        return Err(CaldavError::HttpStatus {
            status: status.as_u16(),
        });
    }
    response.text().map_err(|_| CaldavError::Transport)
}

fn resolve_href(base: &Url, href: &str) -> Result<Url, CaldavError> {
    base.join(href).map_err(|_| CaldavError::Url)
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventSerializationError {
    EmptyUid,
    InvalidSchedule,
    UnsupportedRecurrence,
    UnsupportedReminders,
    Serialization,
}

impl std::fmt::Display for EventSerializationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyUid => write!(formatter, "event has no remote UID"),
            Self::InvalidSchedule => write!(formatter, "event schedule is invalid"),
            Self::UnsupportedRecurrence => write!(formatter, "event recurrence is unsupported"),
            Self::UnsupportedReminders => write!(formatter, "event reminders are unsupported"),
            Self::Serialization => write!(formatter, "failed to serialize iCalendar data"),
        }
    }
}

impl std::error::Error for EventSerializationError {}

pub fn serialize_icalendar_event(
    event: &crate::model::Event,
    remote_uid: &str,
) -> Result<String, EventSerializationError> {
    if remote_uid.trim().is_empty() {
        return Err(EventSerializationError::EmptyUid);
    }
    if event.recurrence.is_some() {
        return Err(EventSerializationError::UnsupportedRecurrence);
    }
    if !event.reminders.is_empty() {
        return Err(EventSerializationError::UnsupportedReminders);
    }

    let mut serialized_event = icalendar::Event::new();
    serialized_event.uid(remote_uid);
    serialized_event.summary(&event.title);
    serialized_event.location(&event.location);
    serialized_event.description(&event.description);

    match &event.schedule {
        crate::model::EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => {
            if end_date_exclusive <= start_date {
                return Err(EventSerializationError::InvalidSchedule);
            }
            serialized_event.starts(*start_date);
            serialized_event.ends(*end_date_exclusive);
        }
        crate::model::EventSchedule::Timed {
            start,
            end,
            timezone,
        } => {
            if end <= start {
                return Err(EventSerializationError::InvalidSchedule);
            }
            let (start, end) = serialize_timed_values(*start, *end, timezone.as_deref())?;
            serialized_event.starts(start);
            serialized_event.ends(end);
        }
    }

    let serialized_event = serialized_event.done();
    let mut calendar = Calendar::new();
    calendar.push(serialized_event);
    std::convert::TryInto::<String>::try_into(&calendar)
        .map_err(|_| EventSerializationError::Serialization)
}

fn serialize_timed_values(
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    timezone: Option<&str>,
) -> Result<(CalendarDateTime, CalendarDateTime), EventSerializationError> {
    match timezone {
        None => {
            if start.offset().local_minus_utc() != 0 || end.offset().local_minus_utc() != 0 {
                return Err(EventSerializationError::InvalidSchedule);
            }
            Ok((
                CalendarDateTime::from(start.with_timezone(&Utc)),
                CalendarDateTime::from(end.with_timezone(&Utc)),
            ))
        }
        Some(tzid) => {
            let timezone =
                Tz::from_str(tzid).map_err(|_| EventSerializationError::InvalidSchedule)?;
            let start = serialize_tz_value(start, tzid, timezone)?;
            let end = serialize_tz_value(end, tzid, timezone)?;
            Ok((start, end))
        }
    }
}

fn serialize_tz_value(
    value: DateTime<FixedOffset>,
    tzid: &str,
    timezone: Tz,
) -> Result<CalendarDateTime, EventSerializationError> {
    let local = value.with_timezone(&timezone).naive_local();
    let resolved = timezone
        .from_local_datetime(&local)
        .single()
        .ok_or(EventSerializationError::InvalidSchedule)?;
    if resolved.offset().fix().local_minus_utc() != value.offset().local_minus_utc() {
        return Err(EventSerializationError::InvalidSchedule);
    }
    Ok(CalendarDateTime::WithTimezone {
        date_time: local,
        tzid: tzid.to_owned(),
    })
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredCalendar {
    pub href: String,
    pub display_name: Option<String>,
    pub sync_token: Option<String>,
    pub color: Option<String>,
    pub writable: bool,
}

pub fn parse_multistatus(xml: &str) -> Result<Vec<ResourceRecord>, ParseError> {
    let root = parse_xml(xml)?;
    let responses = multistatus_responses(&root)?;
    Ok(responses.into_iter().map(resource_record).collect())
}

pub fn parse_current_user_principal(xml: &str) -> Result<String, ParseError> {
    let root = parse_xml(xml)?;
    let responses = multistatus_responses(&root)?;
    for response in responses {
        for property in successful_properties(response) {
            if property.is_named(NamespaceKind::Dav, b"current-user-principal")
                && let Some(href) = property.child(NamespaceKind::Dav, b"href")
            {
                return nonempty_value(href.text_value(), "missing current-user-principal href");
            }
        }
    }
    Err(malformed("missing current-user-principal href"))
}

pub fn parse_calendar_home_set(xml: &str) -> Result<String, ParseError> {
    let root = parse_xml(xml)?;
    let responses = multistatus_responses(&root)?;
    for response in responses {
        for property in successful_properties(response) {
            if property.is_named(NamespaceKind::Caldav, b"calendar-home-set")
                && let Some(href) = property.child(NamespaceKind::Dav, b"href")
            {
                return nonempty_value(href.text_value(), "missing calendar-home-set href");
            }
        }
    }
    Err(malformed("missing calendar-home-set href"))
}

pub fn parse_calendar_home_multistatus(xml: &str) -> Result<Vec<DiscoveredCalendar>, ParseError> {
    let root = parse_xml(xml)?;
    let responses = multistatus_responses(&root)?;
    Ok(responses
        .into_iter()
        .filter_map(discovered_calendar)
        .collect())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NamespaceKind {
    Dav,
    Caldav,
    AppleIcal,
    Other,
}

impl NamespaceKind {
    fn from_resolved(namespace: ResolveResult<'_>) -> Self {
        match namespace {
            ResolveResult::Bound(Namespace(value)) if value == DAV_NAMESPACE => Self::Dav,
            ResolveResult::Bound(Namespace(value)) if value == CALDAV_NAMESPACE => Self::Caldav,
            ResolveResult::Bound(Namespace(value)) if value == APPLE_ICAL_NAMESPACE => {
                Self::AppleIcal
            }
            _ => Self::Other,
        }
    }
}

struct XmlNode {
    namespace: NamespaceKind,
    local_name: Vec<u8>,
    text: String,
    children: Vec<XmlNode>,
}

impl XmlNode {
    fn is_named(&self, namespace: NamespaceKind, local_name: &[u8]) -> bool {
        self.namespace == namespace && self.local_name == local_name
    }

    fn child(&self, namespace: NamespaceKind, local_name: &[u8]) -> Option<&XmlNode> {
        self.children
            .iter()
            .find(|child| child.is_named(namespace, local_name))
    }

    fn text_value(&self) -> String {
        let mut value = self.text.clone();
        for child in &self.children {
            value.push_str(&child.text_value());
        }
        value
    }
}

fn parse_xml(xml: &str) -> Result<XmlNode, ParseError> {
    let mut reader = NsReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut stack: Vec<XmlNode> = Vec::new();
    let mut root = None;

    loop {
        let (resolved_namespace, event) = reader.read_resolved_event()?;
        let namespace = NamespaceKind::from_resolved(resolved_namespace);
        match event {
            XmlEvent::Start(start) => {
                if stack.is_empty() && root.is_some() {
                    return Err(malformed("multiple XML roots"));
                }
                stack.push(XmlNode {
                    namespace,
                    local_name: start.local_name().as_ref().to_vec(),
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            XmlEvent::Empty(empty) => {
                let node = XmlNode {
                    namespace,
                    local_name: empty.local_name().as_ref().to_vec(),
                    text: String::new(),
                    children: Vec::new(),
                };
                append_node(&mut root, &mut stack, node)?;
            }
            XmlEvent::Text(value) => {
                let decoded = decode_text(
                    value
                        .decode()
                        .map_err(|error| malformed(&error.to_string()))?,
                )?;
                append_text(&mut root, &mut stack, decoded)?;
            }
            XmlEvent::CData(value) => {
                let decoded = value
                    .decode()
                    .map_err(|error| malformed(&error.to_string()))?
                    .into_owned();
                append_text(&mut root, &mut stack, decoded)?;
            }
            XmlEvent::GeneralRef(value) => {
                let decoded = value
                    .decode()
                    .map_err(|error| malformed(&error.to_string()))?;
                let reference = format!("&{};", decoded);
                let unescaped = unescape(&reference)
                    .map_err(|error| malformed(&error.to_string()))?
                    .into_owned();
                append_text(&mut root, &mut stack, unescaped)?;
            }
            XmlEvent::End(end) => {
                let node = stack.pop().ok_or_else(|| malformed("unmatched end tag"))?;
                if node.namespace != namespace || node.local_name != end.local_name().as_ref() {
                    return Err(malformed("mismatched end tag"));
                }
                append_node(&mut root, &mut stack, node)?;
            }
            XmlEvent::Eof => {
                if !stack.is_empty() {
                    return Err(malformed("truncated multistatus XML"));
                }
                return root.ok_or_else(|| malformed("missing XML root"));
            }
            _ => {}
        }
    }
}

fn decode_text(decoded: std::borrow::Cow<'_, str>) -> Result<String, ParseError> {
    unescape(decoded.as_ref())
        .map(|value| value.into_owned())
        .map_err(|error| malformed(&error.to_string()))
}

fn append_text(
    root: &mut Option<XmlNode>,
    stack: &mut [XmlNode],
    text: String,
) -> Result<(), ParseError> {
    if let Some(node) = stack.last_mut() {
        node.text.push_str(&text);
    } else if !text.trim().is_empty() {
        return Err(malformed("text outside XML root"));
    } else if root.is_none() {
        return Ok(());
    }
    Ok(())
}

fn append_node(
    root: &mut Option<XmlNode>,
    stack: &mut [XmlNode],
    node: XmlNode,
) -> Result<(), ParseError> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(node);
    } else if root.replace(node).is_some() {
        return Err(malformed("multiple XML roots"));
    }
    Ok(())
}

fn multistatus_responses(root: &XmlNode) -> Result<Vec<&XmlNode>, ParseError> {
    if !root.is_named(NamespaceKind::Dav, b"multistatus") {
        return Err(malformed("root is not DAV:multistatus"));
    }
    Ok(root
        .children
        .iter()
        .filter(|child| child.is_named(NamespaceKind::Dav, b"response"))
        .collect())
}

fn successful_properties(response: &XmlNode) -> impl Iterator<Item = &XmlNode> {
    response
        .children
        .iter()
        .filter(|propstat| propstat.is_named(NamespaceKind::Dav, b"propstat"))
        .filter(|propstat| {
            propstat
                .child(NamespaceKind::Dav, b"status")
                .and_then(|status| parse_status_code(&status.text_value()))
                .is_some_and(|status| (200..300).contains(&status))
        })
        .flat_map(|propstat| {
            propstat
                .child(NamespaceKind::Dav, b"prop")
                .into_iter()
                .flat_map(|prop| prop.children.iter())
        })
}

fn response_href(response: &XmlNode) -> String {
    response
        .child(NamespaceKind::Dav, b"href")
        .map_or_else(String::new, XmlNode::text_value)
}

fn resource_record(response: &XmlNode) -> ResourceRecord {
    let mut record = ResourceRecord {
        href: response_href(response),
        response_status: response
            .child(NamespaceKind::Dav, b"status")
            .and_then(|status| parse_status_code(&status.text_value())),
        etag: None,
        calendar_data: None,
    };
    for property in successful_properties(response) {
        match (property.namespace, property.local_name.as_slice()) {
            (NamespaceKind::Dav, b"getetag") => record.etag = Some(property.text_value()),
            (NamespaceKind::Caldav, b"calendar-data") => {
                record.calendar_data = Some(property.text_value())
            }
            _ => {}
        }
    }
    record
}

fn discovered_calendar(response: &XmlNode) -> Option<DiscoveredCalendar> {
    let mut is_calendar = false;
    let mut display_name = None;
    let mut sync_token = None;
    let mut color = None;
    let mut writable = false;
    for property in successful_properties(response) {
        match (property.namespace, property.local_name.as_slice()) {
            (NamespaceKind::Dav, b"resourcetype") => {
                is_calendar |= property
                    .children
                    .iter()
                    .any(|child| child.is_named(NamespaceKind::Caldav, b"calendar"));
            }
            (NamespaceKind::Dav, b"displayname") => {
                display_name = Some(property.text_value());
            }
            (NamespaceKind::Dav, b"sync-token") => {
                sync_token = Some(property.text_value());
            }
            (NamespaceKind::AppleIcal, b"calendar-color") => {
                color = Some(property.text_value());
            }
            (NamespaceKind::Dav, b"current-user-privilege-set") => {
                writable |= has_write_privilege(property);
            }
            _ => {}
        }
    }
    is_calendar.then_some(DiscoveredCalendar {
        href: response_href(response),
        display_name,
        sync_token,
        color,
        writable,
    })
}

fn has_write_privilege(property: &XmlNode) -> bool {
    property.children.iter().any(|child| {
        child.is_named(NamespaceKind::Dav, b"write")
            || child.is_named(NamespaceKind::Dav, b"write-content")
            || child.is_named(NamespaceKind::Dav, b"all")
            || has_write_privilege(child)
    })
}

fn nonempty_value(value: String, message: &str) -> Result<String, ParseError> {
    if value.is_empty() {
        Err(malformed(message))
    } else {
        Ok(value)
    }
}

fn parse_status_code(status: &str) -> Option<u16> {
    status.split_whitespace().nth(1)?.parse().ok()
}

fn malformed(message: &str) -> ParseError {
    ParseError(message.to_owned())
}
