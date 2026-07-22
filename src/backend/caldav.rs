use chrono::{DateTime, FixedOffset, Offset, TimeZone, Utc};
use chrono_tz::Tz;
use icalendar::{
    Alarm, Calendar, CalendarComponent, CalendarDateTime, Component, DatePerhapsTime, EventLike,
    Related,
};
use quick_xml::escape::unescape;
use quick_xml::events::{BytesDecl, BytesEnd, BytesStart, BytesText, Event as XmlEvent};
use quick_xml::name::{Namespace, ResolveResult};
use quick_xml::reader::NsReader;
use quick_xml::writer::Writer;
use reqwest::blocking::Client;
use reqwest::header::{CONTENT_TYPE, ETAG, IF_MATCH, IF_NONE_MATCH, USER_AGENT};
use reqwest::{Method, Url};
use rrule::RRuleSet;
use std::str::FromStr;
use std::sync::mpsc::{self, Receiver};
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
<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:a="http://apple.com/ns/ical/"><d:prop><d:resourcetype/><d:displayname/><d:sync-token/><a:calendar-color/><d:current-user-privilege-set/><c:supported-calendar-component-set/></d:prop></d:propfind>"#;
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
    InvalidSyncToken,
    HttpStatus { status: u16 },
    Transport,
    Url,
    Xml(ParseError),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoveryWorkerError {
    InvalidCredential,
    Http,
    Parse,
    WorkerPanic,
}

impl std::fmt::Display for DiscoveryWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidCredential => "invalid CalDAV credential",
            Self::Http => "CalDAV HTTP request failed",
            Self::Parse => "invalid CalDAV response",
            Self::WorkerPanic => "CalDAV worker failed",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DiscoveryWorkerError {}

impl std::fmt::Display for CaldavError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidSyncToken => write!(formatter, "sync token must not be blank"),
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
    password: oo7::Secret,
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
            password: oo7::Secret::text(password),
        }
    }

    pub(crate) fn new_with_secret(
        server_url: String,
        username: String,
        password: oo7::Secret,
    ) -> Self {
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

    pub fn fetch_changes(
        &self,
        calendar_url: &str,
        prior_sync_token: &str,
    ) -> Result<SyncCollection, CaldavError> {
        if prior_sync_token.trim().is_empty() {
            return Err(CaldavError::InvalidSyncToken);
        }
        let calendar_url = Url::parse(calendar_url)
            .map_err(|_| CaldavError::Url)
            .and_then(validate_http_url)?;
        let client = self.http_client()?;
        let body = sync_collection_request(prior_sync_token)?;
        let response = self.request_method(&client, REPORT, &calendar_url, "1", &body)?;
        let mut collection = parse_sync_collection(&response).map_err(CaldavError::Xml)?;
        for change in &mut collection.changes {
            change.href = resolve_href(&calendar_url, &change.href)?.to_string();
        }
        Ok(collection)
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
        let password = self.password.as_str().unwrap_or("");
        client
            .request(method, url.clone())
            .header(USER_AGENT, USER_AGENT_VALUE)
            .basic_auth(&self.username, Some(password))
    }
}

pub fn discover_on_worker(
    server_url: String,
    username: String,
    password: oo7::Secret,
) -> Receiver<Result<CaldavDiscovery, DiscoveryWorkerError>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_sender = sender.clone();
    let worker = move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if password.content_type() != oo7::ContentType::Text {
                return Err(DiscoveryWorkerError::InvalidCredential);
            }

            CaldavClient::new_with_secret(server_url, username, password)
                .discover()
                .map_err(|error| match error {
                    CaldavError::Xml(_) => DiscoveryWorkerError::Parse,
                    _ => DiscoveryWorkerError::Http,
                })
        }))
        .unwrap_or(Err(DiscoveryWorkerError::WorkerPanic));
        let _ = worker_sender.send(result);
    };

    if std::thread::Builder::new()
        .name("caldav-discovery".to_owned())
        .spawn(worker)
        .is_err()
    {
        let _ = sender.send(Err(DiscoveryWorkerError::WorkerPanic));
    }
    receiver
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

fn sync_collection_request(prior_sync_token: &str) -> Result<String, CaldavError> {
    let mut writer = Writer::new(Vec::new());
    writer
        .write_event(XmlEvent::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))
        .map_err(sync_request_error)?;

    let mut root = BytesStart::new("d:sync-collection");
    root.push_attribute(("xmlns:d", "DAV:"));
    root.push_attribute(("xmlns:c", "urn:ietf:params:xml:ns:caldav"));
    writer
        .write_event(XmlEvent::Start(root))
        .map_err(sync_request_error)?;

    writer
        .write_event(XmlEvent::Start(BytesStart::new("d:sync-token")))
        .map_err(sync_request_error)?;
    writer
        .write_event(XmlEvent::Text(BytesText::new(prior_sync_token)))
        .map_err(sync_request_error)?;
    writer
        .write_event(XmlEvent::End(BytesEnd::new("d:sync-token")))
        .map_err(sync_request_error)?;

    writer
        .write_event(XmlEvent::Start(BytesStart::new("d:sync-level")))
        .map_err(sync_request_error)?;
    writer
        .write_event(XmlEvent::Text(BytesText::new("1")))
        .map_err(sync_request_error)?;
    writer
        .write_event(XmlEvent::End(BytesEnd::new("d:sync-level")))
        .map_err(sync_request_error)?;

    writer
        .write_event(XmlEvent::Start(BytesStart::new("d:prop")))
        .map_err(sync_request_error)?;
    write_empty_element(&mut writer, "d:getetag")?;
    write_empty_element(&mut writer, "c:calendar-data")?;
    writer
        .write_event(XmlEvent::End(BytesEnd::new("d:prop")))
        .map_err(sync_request_error)?;
    writer
        .write_event(XmlEvent::End(BytesEnd::new("d:sync-collection")))
        .map_err(sync_request_error)?;

    String::from_utf8(writer.into_inner())
        .map_err(|_| CaldavError::Xml(malformed("generated sync request is not UTF-8")))
}

fn write_empty_element(writer: &mut Writer<Vec<u8>>, name: &str) -> Result<(), CaldavError> {
    writer
        .write_event(XmlEvent::Empty(BytesStart::new(name)))
        .map_err(sync_request_error)
}

fn sync_request_error(error: std::io::Error) -> CaldavError {
    CaldavError::Xml(malformed(&error.to_string()))
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

    for reminder in &event.reminders {
        if reminder.seconds_before_start <= 0 {
            return Err(EventSerializationError::UnsupportedReminders);
        }
        serialized_event.alarm(Alarm::display(
            &reminder.description,
            (
                -chrono::Duration::seconds(reminder.seconds_before_start),
                Related::Start,
            ),
        ));
    }

    if let Some(recurrence) = &event.recurrence {
        validate_recurrence(&event.schedule, recurrence)
            .map_err(|_| EventSerializationError::UnsupportedRecurrence)?;
        for line in &recurrence.rrule {
            add_recurrence_property(&mut serialized_event, line, false)?;
        }
        for line in &recurrence.rdate {
            add_recurrence_property(&mut serialized_event, line, true)?;
        }
        for line in &recurrence.exdate {
            add_recurrence_property(&mut serialized_event, line, true)?;
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
        None => Ok((
            CalendarDateTime::from(start.with_timezone(&Utc)),
            CalendarDateTime::from(end.with_timezone(&Utc)),
        )),
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

/// Map one supported VEVENT master into the local event model.
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
    if has_property(event, "RECURRENCE-ID") {
        return Err(EventMappingError::UnsupportedRecurrence);
    }
    if has_property(event, "DURATION") {
        return Err(EventMappingError::UnsupportedData(
            "DURATION is not supported".to_owned(),
        ));
    }
    let mut reminders = Vec::new();
    for component in event.components() {
        if component.component_kind().eq_ignore_ascii_case("VALARM") {
            reminders.push(map_alarm(component)?);
        } else {
            return Err(EventMappingError::UnsupportedData(
                "nested non-VALARM components are not supported".to_owned(),
            ));
        }
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
    let recurrence = recurrence_spec(&normalized, &schedule)?;
    let mapped = crate::model::Event {
        id: event_id,
        calendar_id,
        title: event.get_summary().unwrap_or_default().to_owned(),
        location: event.get_location().unwrap_or_default().to_owned(),
        description: event.get_description().unwrap_or_default().to_owned(),
        schedule,
        recurrence,
        reminders,
    };

    Ok(MappedEvent {
        event: mapped,
        remote_uid: uid.to_owned(),
    })
}

fn map_alarm<C: Component>(component: &C) -> Result<crate::model::ReminderSpec, EventMappingError> {
    let action = component
        .properties()
        .get("ACTION")
        .filter(|property| property.value().eq_ignore_ascii_case("DISPLAY"))
        .ok_or_else(|| {
            EventMappingError::UnsupportedData("VALARM action is unsupported".to_owned())
        })?;
    if component.multi_properties().contains_key("ACTION")
        || component.multi_properties().contains_key("TRIGGER")
        || component.multi_properties().contains_key("DESCRIPTION")
        || !component.components().is_empty()
    {
        return Err(EventMappingError::UnsupportedData(
            "VALARM has repeated required properties".to_owned(),
        ));
    }
    let _ = action;
    if component.properties().contains_key("REPEAT")
        || component.properties().contains_key("DURATION")
        || component.multi_properties().contains_key("REPEAT")
        || component.multi_properties().contains_key("DURATION")
    {
        return Err(EventMappingError::UnsupportedData(
            "repeating VALARMs are unsupported".to_owned(),
        ));
    }

    let trigger = component
        .properties()
        .get("TRIGGER")
        .ok_or_else(|| EventMappingError::UnsupportedData("VALARM has no TRIGGER".to_owned()))?;
    if let Some(value) = trigger.params().get("VALUE") {
        if value.value().eq_ignore_ascii_case("DATE-TIME") {
            return Err(EventMappingError::UnsupportedData(
                "absolute VALARM triggers are unsupported".to_owned(),
            ));
        }
        if !value.value().eq_ignore_ascii_case("DURATION") {
            return Err(EventMappingError::UnsupportedData(
                "VALARM trigger value type is malformed".to_owned(),
            ));
        }
    }
    let related = trigger.params().get("RELATED").map(|value| value.value());
    if related.is_some_and(|value| value.eq_ignore_ascii_case("END")) {
        return Err(EventMappingError::UnsupportedData(
            "VALARM trigger must be related to START".to_owned(),
        ));
    }
    if related.is_some_and(|value| !value.eq_ignore_ascii_case("START")) {
        return Err(EventMappingError::UnsupportedData(
            "VALARM trigger relationship is malformed".to_owned(),
        ));
    }
    let seconds = parse_ical_duration(trigger.value()).ok_or_else(|| {
        EventMappingError::UnsupportedData("VALARM trigger is malformed".to_owned())
    })?;
    if seconds >= 0 || seconds == i64::MIN {
        return Err(EventMappingError::UnsupportedData(
            "VALARM trigger must be a negative whole-second duration".to_owned(),
        ));
    }
    let seconds_before_start = -seconds;
    let description = component
        .properties()
        .get("DESCRIPTION")
        .ok_or_else(|| {
            EventMappingError::UnsupportedData("DISPLAY VALARM has no DESCRIPTION".to_owned())
        })?
        .value()
        .to_owned();
    Ok(crate::model::ReminderSpec {
        seconds_before_start,
        description,
    })
}

fn parse_ical_duration(value: &str) -> Option<i64> {
    let (negative, value) = match value.as_bytes().first()? {
        b'-' => (true, &value[1..]),
        b'+' => (false, &value[1..]),
        _ => (false, value),
    };
    let value = value.strip_prefix('P')?;
    if value.is_empty() {
        return None;
    }
    let mut total = 0i64;
    let mut number = 0i64;
    let mut have_number = false;
    let mut in_time = false;
    let mut used_week = false;
    let mut used_date = false;
    for byte in value.bytes() {
        match byte {
            b'0'..=b'9' => {
                number = number
                    .checked_mul(10)?
                    .checked_add(i64::from(byte - b'0'))?;
                have_number = true;
            }
            b'T' if !in_time && !used_week => {
                in_time = true;
                have_number = false;
            }
            designator @ (b'W' | b'D' | b'H' | b'M' | b'S') if have_number => {
                let multiplier = match designator {
                    b'W' if !in_time && !used_date && !used_week => {
                        used_week = true;
                        7 * 24 * 60 * 60
                    }
                    b'D' if !in_time && !used_week && !used_date => {
                        used_date = true;
                        24 * 60 * 60
                    }
                    b'H' if in_time => 60 * 60,
                    b'M' if in_time => 60,
                    b'S' if in_time => 1,
                    _ => return None,
                };
                total = total.checked_add(number.checked_mul(multiplier)?)?;
                number = 0;
                have_number = false;
            }
            _ => return None,
        }
    }
    if have_number || total == 0 || (used_week && in_time) {
        return None;
    }
    Some(if negative {
        total.checked_neg()?
    } else {
        total
    })
}

fn add_recurrence_property(
    event: &mut icalendar::Event,
    line: &str,
    multi: bool,
) -> Result<(), EventSerializationError> {
    let (key, value) = line
        .split_once(':')
        .ok_or(EventSerializationError::UnsupportedRecurrence)?;
    if key.is_empty() || value.is_empty() || key.eq_ignore_ascii_case("DTSTART") {
        return Err(EventSerializationError::UnsupportedRecurrence);
    }
    if multi {
        event.add_multi_property(key, value);
    } else {
        event.add_property(key, value);
    }
    Ok(())
}

fn recurrence_spec(
    resource: &str,
    schedule: &crate::model::EventSchedule,
) -> Result<Option<crate::model::RecurrenceSpec>, EventMappingError> {
    let mut in_event = false;
    let mut recurrence = crate::model::RecurrenceSpec::default();
    for line in resource.lines() {
        let line = line.trim_end_matches('\r');
        if line.eq_ignore_ascii_case("BEGIN:VEVENT") {
            in_event = true;
            continue;
        }
        if line.eq_ignore_ascii_case("END:VEVENT") {
            in_event = false;
            continue;
        }
        if !in_event {
            continue;
        }
        let Some((key, _)) = line.split_once(':') else {
            continue;
        };
        let name = key.split(';').next().unwrap_or_default();
        match name.to_ascii_uppercase().as_str() {
            "RRULE" => recurrence.rrule.push(line.to_owned()),
            "RDATE" => recurrence.rdate.push(line.to_owned()),
            "EXDATE" => recurrence.exdate.push(line.to_owned()),
            _ => {}
        }
    }
    if recurrence.rrule.is_empty() && recurrence.rdate.is_empty() && recurrence.exdate.is_empty() {
        return Ok(None);
    }
    if recurrence.rrule.len() > 1 {
        return Err(EventMappingError::UnsupportedRecurrence);
    }
    if recurrence
        .rdate
        .iter()
        .chain(&recurrence.exdate)
        .any(|line| line.to_ascii_uppercase().contains("VALUE=PERIOD"))
    {
        return Err(EventMappingError::UnsupportedRecurrence);
    }

    validate_recurrence(schedule, &recurrence)
        .map_err(|message| EventMappingError::UnsupportedData(message.to_owned()))?;
    Ok(Some(recurrence))
}

fn validate_recurrence(
    schedule: &crate::model::EventSchedule,
    recurrence: &crate::model::RecurrenceSpec,
) -> Result<(), &'static str> {
    let mut source = recurrence_dtstart(schedule)?;
    source.push('\n');
    for line in &recurrence.rrule {
        source.push_str(line);
        source.push('\n');
    }
    for line in &recurrence.rdate {
        source.push_str(line);
        source.push('\n');
    }
    for line in &recurrence.exdate {
        source.push_str(line);
        source.push('\n');
    }
    source
        .parse::<RRuleSet>()
        .map(|_| ())
        .map_err(|_| "invalid recurrence properties")
}

fn recurrence_dtstart(schedule: &crate::model::EventSchedule) -> Result<String, &'static str> {
    match schedule {
        crate::model::EventSchedule::AllDay { start_date, .. } => {
            Ok(format!("DTSTART:{}T000000", start_date.format("%Y%m%d")))
        }
        crate::model::EventSchedule::Timed {
            start, timezone, ..
        } => match timezone.as_deref() {
            None => Ok(format!(
                "DTSTART:{}",
                start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
            )),
            Some(tzid) => {
                let timezone = Tz::from_str(tzid).map_err(|_| "unknown event timezone")?;
                Ok(format!(
                    "DTSTART;TZID={tzid}:{}",
                    start.with_timezone(&timezone).format("%Y%m%dT%H%M%S")
                ))
            }
        },
    }
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
pub struct SyncCollection {
    pub sync_token: String,
    pub changes: Vec<ResourceRecord>,
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

pub fn parse_sync_collection(xml: &str) -> Result<SyncCollection, ParseError> {
    let root = parse_xml(xml)?;
    let responses = multistatus_responses(&root)?;
    let mut sync_tokens = root
        .children
        .iter()
        .filter(|child| child.is_named(NamespaceKind::Dav, b"sync-token"));
    let sync_token = sync_tokens
        .next()
        .ok_or_else(|| malformed("missing root DAV:sync-token"))?
        .text_value();
    if sync_tokens.next().is_some() {
        return Err(malformed("multiple root DAV:sync-token elements"));
    }
    if sync_token.trim().is_empty() {
        return Err(malformed("empty root DAV:sync-token"));
    }

    Ok(SyncCollection {
        sync_token,
        changes: responses.into_iter().map(resource_record).collect(),
    })
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
    attributes: Vec<(Vec<u8>, String)>,
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

    fn attribute(&self, name: &[u8]) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(attribute_name, _)| attribute_name == name)
            .map(|(_, value)| value.as_str())
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
                    attributes: start
                        .attributes()
                        .map(|attribute| {
                            let attribute = attribute?;
                            Ok((
                                attribute.key.as_ref().to_vec(),
                                attribute
                                    .decoded_and_normalized_value(
                                        quick_xml::XmlVersion::Implicit1_0,
                                        reader.decoder(),
                                    )?
                                    .into_owned(),
                            ))
                        })
                        .collect::<Result<_, quick_xml::Error>>()?,
                    text: String::new(),
                    children: Vec::new(),
                });
            }
            XmlEvent::Empty(empty) => {
                let node = XmlNode {
                    namespace,
                    local_name: empty.local_name().as_ref().to_vec(),
                    attributes: empty
                        .attributes()
                        .map(|attribute| {
                            let attribute = attribute?;
                            Ok((
                                attribute.key.as_ref().to_vec(),
                                attribute
                                    .decoded_and_normalized_value(
                                        quick_xml::XmlVersion::Implicit1_0,
                                        reader.decoder(),
                                    )?
                                    .into_owned(),
                            ))
                        })
                        .collect::<Result<_, quick_xml::Error>>()?,
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
    let mut supports_vevent = None;
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
            (NamespaceKind::Caldav, b"supported-calendar-component-set") => {
                let property_supports_vevent = property.children.iter().any(|child| {
                    child.is_named(NamespaceKind::Caldav, b"comp")
                        && child.attribute(b"name") == Some("VEVENT")
                });
                supports_vevent =
                    Some(supports_vevent.unwrap_or(false) || property_supports_vevent);
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
    (is_calendar && supports_vevent != Some(false)).then_some(DiscoveredCalendar {
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
