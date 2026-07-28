use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, Event, EventSchedule};
use calendar::month_view::{AgendaGroup, EventChip, project_agenda_range};
use calendar::preferences::format_wall_time;
use calendar::viewer_time::{now_local_fixed, to_local_fixed};
use chrono::{Datelike, Duration, NaiveDate};
use gtk::glib;
use uuid::Uuid;

type EventActivateFn = Box<dyn Fn(Uuid, gtk::Widget)>;

const INITIAL_BEFORE_DAYS: i64 = 56;
const INITIAL_AFTER_DAYS: i64 = 56;
const CHUNK_DAYS: i64 = 28;
const EDGE_THRESHOLD: f64 = 420.0;
const EMPTY_RANGE_ROW_DAYS: i64 = 7;

#[derive(Clone, Copy)]
pub enum Edge {
    Top,
    Bottom,
}

#[derive(Clone, Copy)]
pub struct Anchor {
    date: NaiveDate,
    offset: f64,
}

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate)]
    #[template(resource = "/dev/chris/calendar/ui/views/agenda-view.ui")]
    pub struct AgendaView {
        #[template_child]
        pub agenda_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub agenda_days_box: TemplateChild<gtk::Box>,
        pub active_date: Cell<NaiveDate>,
        pub range_start: Cell<Option<NaiveDate>>,
        pub range_end: Cell<Option<NaiveDate>>,
        pub cached_calendars: RefCell<Vec<Calendar>>,
        pub cached_events: RefCell<Vec<Event>>,
        pub rendered_groups: RefCell<Vec<AgendaGroup>>,
        pub pending_work: Cell<bool>,
        pub pending_edge: Cell<Option<Edge>>,
        pub pending_target: RefCell<Option<NaiveDate>>,
        pub programmatic_guard: Cell<bool>,
        pub deferred_work: Cell<bool>,
        pub restore_request: RefCell<Option<Restore>>,
        pub restore_tick: RefCell<Option<gtk::TickCallbackId>>,
        pub restore_idle: RefCell<Option<glib::SourceId>>,
        pub on_event_activate: RefCell<Option<EventActivateFn>>,
    }

    impl Default for AgendaView {
        fn default() -> Self {
            let fallback = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            Self {
                agenda_scroll: TemplateChild::default(),
                agenda_days_box: TemplateChild::default(),
                active_date: Cell::new(fallback),
                range_start: Cell::new(None),
                range_end: Cell::new(None),
                cached_calendars: RefCell::new(Vec::new()),
                cached_events: RefCell::new(Vec::new()),
                rendered_groups: RefCell::new(Vec::new()),
                pending_work: Cell::new(false),
                pending_edge: Cell::new(None),
                pending_target: RefCell::new(None),
                programmatic_guard: Cell::new(false),
                deferred_work: Cell::new(false),
                restore_request: RefCell::new(None),
                restore_tick: RefCell::new(None),
                restore_idle: RefCell::new(None),
                on_event_activate: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AgendaView {
        const NAME: &'static str = "AgendaView";
        type Type = super::AgendaView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AgendaView {
        fn constructed(&self) {
            self.parent_constructed();
            let now = now_local_fixed();
            if let Some(today) = NaiveDate::from_ymd_opt(now.year(), now.month(), now.day()) {
                self.active_date.set(today);
            }

            let view_weak = self.obj().downgrade();
            self.agenda_scroll
                .vadjustment()
                .connect_value_changed(move |_| {
                    if let Some(view) = view_weak.upgrade() {
                        view.queue_edge_extension();
                    }
                });
        }

        fn dispose(&self) {
            self.obj().cancel_restore_callbacks();
        }
    }

    impl WidgetImpl for AgendaView {
        fn map(&self) {
            self.parent_map();
            self.obj().schedule_restore_sequence();
        }

        fn unmap(&self) {
            self.obj().cancel_restore_callbacks();
            self.parent_unmap();
        }
    }
    impl BinImpl for AgendaView {}
}

glib::wrapper! {
    pub struct AgendaView(ObjectSubclass<imp::AgendaView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AgendaView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_on_event_activate<F: Fn(Uuid, gtk::Widget) + 'static>(&self, f: F) {
        *self.imp().on_event_activate.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_active_date(&self, date: NaiveDate) {
        let imp = self.imp();
        imp.active_date.set(date);
        if !self.date_is_loaded(date) {
            self.set_initial_range(date);
        }
        *imp.pending_target.borrow_mut() = Some(date);
        self.queue_work(None);
    }

    pub fn active_date(&self) -> NaiveDate {
        self.imp().active_date.get()
    }

    pub fn render(&self, calendars: &[Calendar], events: &[Event]) {
        let imp = self.imp();
        *imp.cached_calendars.borrow_mut() = calendars.to_vec();
        *imp.cached_events.borrow_mut() = events.to_vec();
        if imp.range_start.get().is_none() {
            self.set_initial_range(self.active_date());
        }
        self.queue_work(None);
    }

    fn set_initial_range(&self, date: NaiveDate) {
        self.imp()
            .range_start
            .set(Some(date - Duration::days(INITIAL_BEFORE_DAYS)));
        self.imp()
            .range_end
            .set(Some(date + Duration::days(INITIAL_AFTER_DAYS)));
    }

    fn date_is_loaded(&self, date: NaiveDate) -> bool {
        let imp = self.imp();
        matches!((imp.range_start.get(), imp.range_end.get()), (Some(start), Some(end)) if date >= start && date < end)
    }

    fn queue_edge_extension(&self) {
        let imp = self.imp();
        if imp.programmatic_guard.get() {
            return;
        }
        let adjustment = imp.agenda_scroll.vadjustment();
        let remaining = adjustment.upper() - adjustment.value() - adjustment.page_size();
        if adjustment.upper() <= adjustment.page_size() || remaining < 0.0 {
            return;
        }
        let edge = if adjustment.value() <= EDGE_THRESHOLD {
            Some(Edge::Top)
        } else if remaining <= EDGE_THRESHOLD {
            Some(Edge::Bottom)
        } else {
            None
        };
        if let Some(edge) = edge {
            imp.pending_edge.set(Some(edge));
            self.queue_work(None);
        }
    }

    fn queue_work(&self, edge: Option<Edge>) {
        let imp = self.imp();
        if edge.is_some() {
            imp.pending_edge.set(edge);
        }
        if imp.programmatic_guard.get() {
            imp.deferred_work.set(true);
            return;
        }
        if imp.pending_work.replace(true) {
            return;
        }

        let view_weak = self.downgrade();
        glib::idle_add_local_once(move || {
            if let Some(view) = view_weak.upgrade() {
                view.run_pending_work();
            }
        });
    }

    fn run_pending_work(&self) {
        let imp = self.imp();
        imp.pending_work.set(false);
        let edge = imp.pending_edge.take();
        let target = imp.pending_target.borrow_mut().take();
        let anchor = self.capture_anchor();

        imp.programmatic_guard.set(true);
        if let Some(edge) = edge {
            self.extend_range(edge);
        }
        self.rebuild_groups(target, anchor);
    }

    fn extend_range(&self, edge: Edge) {
        let imp = self.imp();
        let (Some(mut start), Some(mut end)) = (imp.range_start.get(), imp.range_end.get()) else {
            self.set_initial_range(self.active_date());
            return;
        };
        match edge {
            Edge::Top => {
                start -= Duration::days(CHUNK_DAYS);
                end -= Duration::days(CHUNK_DAYS);
            }
            Edge::Bottom => {
                start += Duration::days(CHUNK_DAYS);
                end += Duration::days(CHUNK_DAYS);
            }
        }
        imp.range_start.set(Some(start));
        imp.range_end.set(Some(end));
    }

    fn rebuild_groups(&self, target: Option<NaiveDate>, anchor: Option<Anchor>) {
        let imp = self.imp();
        let (Some(start), Some(end)) = (imp.range_start.get(), imp.range_end.get()) else {
            return;
        };
        let calendars = imp.cached_calendars.borrow();
        let events = imp.cached_events.borrow();
        let groups = subdivide_empty_ranges(project_agenda_range(start, end, &calendars, &events));
        let event_map: HashMap<Uuid, &Event> =
            events.iter().map(|event| (event.id, event)).collect();
        let view_weak = self.downgrade();

        while let Some(child) = imp.agenda_days_box.first_child() {
            imp.agenda_days_box.remove(&child);
        }
        for group in &groups {
            imp.agenda_days_box
                .append(&create_group_widget(group, &event_map, &view_weak));
        }
        *imp.rendered_groups.borrow_mut() = groups;

        let restore = target
            .map(Restore::Date)
            .or_else(|| anchor.map(Restore::Anchor));
        if let Some(restore) = restore {
            *imp.restore_request.borrow_mut() = Some(restore);
            self.schedule_restore_sequence();
        } else {
            self.finish_programmatic_work();
        }
    }

    fn capture_anchor(&self) -> Option<Anchor> {
        let imp = self.imp();
        let adjustment = imp.agenda_scroll.vadjustment();
        let value = adjustment.value();
        let groups = imp.rendered_groups.borrow();
        let mut child = imp.agenda_days_box.first_child();
        for group in groups.iter() {
            let Some(widget) = child else { break };
            let Some(bounds) =
                widget.compute_bounds(imp.agenda_days_box.upcast_ref::<gtk::Widget>())
            else {
                child = widget.next_sibling();
                continue;
            };
            if f64::from(bounds.y() + bounds.height()) > value {
                return Some(Anchor {
                    date: group_start(group),
                    offset: value - f64::from(bounds.y()),
                });
            }
            child = widget.next_sibling();
        }
        groups.first().map(|group| Anchor {
            date: group_start(group),
            offset: 0.0,
        })
    }

    fn apply_restore(&self, restore: Restore) {
        let imp = self.imp();
        let date = match restore {
            Restore::Date(date) => date,
            Restore::Anchor(anchor) => anchor.date,
        };
        let groups = imp.rendered_groups.borrow();
        let Some(index) = groups.iter().position(|group| group_contains(group, date)) else {
            self.finish_programmatic_work();
            return;
        };
        let Some(widget) = child_at(&imp.agenda_days_box, index) else {
            self.finish_programmatic_work();
            return;
        };
        let offset = match restore {
            Restore::Date(_) => 8.0,
            Restore::Anchor(anchor) => anchor.offset,
        };
        let adjustment = imp.agenda_scroll.vadjustment();
        let max_value = (adjustment.upper() - adjustment.page_size()).max(adjustment.lower());
        let Some(bounds) = widget.compute_bounds(imp.agenda_days_box.upcast_ref::<gtk::Widget>())
        else {
            self.finish_programmatic_work();
            return;
        };
        let value = (f64::from(bounds.y()) + offset).clamp(adjustment.lower(), max_value);
        adjustment.set_value(value);
        self.finish_programmatic_work();
    }

    fn finish_programmatic_work(&self) {
        let imp = self.imp();
        imp.restore_request.borrow_mut().take();
        imp.programmatic_guard.set(false);
        if imp.deferred_work.replace(false) {
            self.queue_work(None);
        }
    }

    fn schedule_restore_sequence(&self) {
        let imp = self.imp();
        if imp.restore_request.borrow().is_none()
            || imp.restore_tick.borrow().is_some()
            || imp.restore_idle.borrow().is_some()
            || !self.is_mapped()
        {
            return;
        }

        let view_weak = self.downgrade();
        let tick_id = self.add_tick_callback(move |_, _| {
            if let Some(view) = view_weak.upgrade() {
                view.schedule_restore_idle();
            }
            glib::ControlFlow::Break
        });
        *imp.restore_tick.borrow_mut() = Some(tick_id);
    }

    fn schedule_restore_idle(&self) {
        let imp = self.imp();
        imp.restore_tick.borrow_mut().take();
        if imp.restore_request.borrow().is_none()
            || imp.restore_idle.borrow().is_some()
            || !self.is_mapped()
        {
            return;
        }

        let view_weak = self.downgrade();
        let idle_id = glib::idle_add_local_once(move || {
            if let Some(view) = view_weak.upgrade() {
                view.apply_restore_idle();
            }
        });
        *imp.restore_idle.borrow_mut() = Some(idle_id);
    }

    fn apply_restore_idle(&self) {
        let imp = self.imp();
        imp.restore_idle.borrow_mut().take();
        let Some(restore) = imp.restore_request.borrow_mut().take() else {
            self.finish_programmatic_work();
            return;
        };
        self.apply_restore(restore);
    }

    fn cancel_restore_callbacks(&self) {
        let imp = self.imp();
        if let Some(tick_id) = imp.restore_tick.borrow_mut().take() {
            tick_id.remove();
        }
        if let Some(idle_id) = imp.restore_idle.borrow_mut().take() {
            idle_id.remove();
        }
        imp.programmatic_guard.set(false);
    }
}

fn child_at(container: &gtk::Box, index: usize) -> Option<gtk::Widget> {
    let mut child = container.first_child();
    for _ in 0..index {
        child = child.and_then(|widget| widget.next_sibling());
    }
    child
}

#[derive(Clone, Copy)]
pub enum Restore {
    Date(NaiveDate),
    Anchor(Anchor),
}

fn subdivide_empty_ranges(groups: Vec<AgendaGroup>) -> Vec<AgendaGroup> {
    let mut result = Vec::with_capacity(groups.len());
    for group in groups {
        let AgendaGroup::EmptyRange {
            start_date,
            end_date_exclusive,
        } = group
        else {
            result.push(group);
            continue;
        };

        let mut start = start_date;
        while start < end_date_exclusive {
            let end = (start + Duration::days(EMPTY_RANGE_ROW_DAYS)).min(end_date_exclusive);
            result.push(AgendaGroup::EmptyRange {
                start_date: start,
                end_date_exclusive: end,
            });
            start = end;
        }
    }
    result
}

fn create_group_widget(
    group: &AgendaGroup,
    event_map: &HashMap<Uuid, &Event>,
    view_weak: &glib::WeakRef<AgendaView>,
) -> gtk::Box {
    let day_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .build();
    let heading = gtk::Label::builder()
        .label(match group {
            AgendaGroup::EventDay(day) => day_heading(day.date, local_today()),
            AgendaGroup::EmptyRange {
                start_date,
                end_date_exclusive,
            } => empty_range_heading(*start_date, *end_date_exclusive, local_today()),
        })
        .halign(gtk::Align::Start)
        .css_classes(["caption-heading", "agenda-day-heading"])
        .build();
    day_box.append(&heading);

    match group {
        AgendaGroup::EventDay(day) => {
            for chip in day.all_day.iter().chain(day.timed.iter()) {
                if let Some(event) = event_map.get(&chip.event_id) {
                    day_box.append(&create_event_row(chip, event, view_weak));
                }
            }
        }
        AgendaGroup::EmptyRange { .. } => {
            day_box.append(
                &gtk::Label::builder()
                    .label("No events")
                    .halign(gtk::Align::Start)
                    .css_classes(["no-events", "agenda-empty-row"])
                    .build(),
            );
        }
    }
    day_box
}

fn create_event_row(
    chip: &EventChip,
    event: &Event,
    view_weak: &glib::WeakRef<AgendaView>,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .css_classes(["agenda-event", "flat"])
        .halign(gtk::Align::Fill)
        .can_focus(true)
        .tooltip_text(&event.title)
        .build();
    button.set_cursor_from_name(Some("pointer"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(10)
        .margin_start(10)
        .margin_end(10)
        .margin_top(7)
        .margin_bottom(7)
        .build();
    let color = sanitize_color(&chip.color);
    let swatch = gtk::Label::builder()
        .label("●")
        .valign(gtk::Align::Start)
        .build();
    swatch.set_markup(&format!("<span foreground=\"#{color}\">●</span>"));
    content.append(&swatch);

    let details = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(2)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .build();
    details.append(
        &gtk::Label::builder()
            .label(event_time_label(event))
            .halign(gtk::Align::Start)
            .css_classes(["agenda-event-time", "dim-label"])
            .build(),
    );
    details.append(
        &gtk::Label::builder()
            .label(&event.title)
            .halign(gtk::Align::Start)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build(),
    );
    if !event.location.trim().is_empty() {
        details.append(
            &gtk::Label::builder()
                .label(event.location.trim())
                .halign(gtk::Align::Start)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .css_classes(["agenda-event-location", "dim-label"])
                .build(),
        );
    }
    content.append(&details);
    button.set_child(Some(&content));

    let event_id = chip.event_id;
    let button_weak = button.downgrade();
    let view_weak = view_weak.clone();
    button.connect_clicked(move |_| {
        if let Some(view) = view_weak.upgrade()
            && let Some(button) = button_weak.upgrade()
            && let Some(callback) = view.imp().on_event_activate.borrow().as_ref()
        {
            callback(event_id, button.upcast::<gtk::Widget>());
        }
    });
    button
}

fn group_start(group: &AgendaGroup) -> NaiveDate {
    match group {
        AgendaGroup::EventDay(day) => day.date,
        AgendaGroup::EmptyRange { start_date, .. } => *start_date,
    }
}

fn group_contains(group: &AgendaGroup, date: NaiveDate) -> bool {
    match group {
        AgendaGroup::EventDay(day) => day.date == date,
        AgendaGroup::EmptyRange {
            start_date,
            end_date_exclusive,
        } => date >= *start_date && date < *end_date_exclusive,
    }
}

fn event_time_label(event: &Event) -> String {
    match &event.schedule {
        EventSchedule::AllDay { .. } => "All day".to_string(),
        EventSchedule::Timed { start, .. } => format_wall_time(to_local_fixed(start).time()),
    }
}

fn day_heading(date: NaiveDate, today: NaiveDate) -> String {
    match (date - today).num_days() {
        -1 => "Yesterday".to_string(),
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        _ => full_date(date, today.year() != date.year()),
    }
}

fn empty_range_heading(start: NaiveDate, end_exclusive: NaiveDate, today: NaiveDate) -> String {
    let end = end_exclusive - Duration::days(1);
    if start == end {
        return day_heading(start, today);
    }
    format!(
        "{} – {}",
        full_date(start, start.year() != end.year()),
        full_date(end, true),
    )
}

fn full_date(date: NaiveDate, include_year: bool) -> String {
    if include_year {
        format!(
            "{}, {} {}, {}",
            weekday_name(date),
            month_name(date.month()),
            date.day(),
            date.year()
        )
    } else {
        format!(
            "{}, {} {}",
            weekday_name(date),
            month_name(date.month()),
            date.day()
        )
    }
}

fn local_today() -> NaiveDate {
    let now = now_local_fixed();
    NaiveDate::from_ymd_opt(now.year(), now.month(), now.day())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 1, 5).unwrap())
}

fn weekday_name(date: NaiveDate) -> &'static str {
    match date.weekday() {
        chrono::Weekday::Mon => "Monday",
        chrono::Weekday::Tue => "Tuesday",
        chrono::Weekday::Wed => "Wednesday",
        chrono::Weekday::Thu => "Thursday",
        chrono::Weekday::Fri => "Friday",
        chrono::Weekday::Sat => "Saturday",
        chrono::Weekday::Sun => "Sunday",
    }
}

fn month_name(month: u32) -> &'static str {
    [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ][month as usize - 1]
}

fn sanitize_color(color: &str) -> String {
    color
        .trim_start_matches('#')
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect()
}
