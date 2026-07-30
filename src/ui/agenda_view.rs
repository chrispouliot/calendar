use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Duration as StdDuration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::agenda_presentation::{AgendaEventState, AgendaTimeLayout, event_state, time_text};
use calendar::agenda_render_plan::{AgendaRange, AgendaRenderPlan, render_agenda};
use calendar::model::{Calendar, Event, EventSchedule};
use calendar::month_view::{AgendaGroup, EventChip};
use calendar::preferences::{load_time_format_preference, system_clock_format};
use calendar::viewer_time::now_local_fixed;
use chrono::{Datelike, Duration, NaiveDate, Timelike};
use gtk::glib;
use uuid::Uuid;

type EventActivateFn = Box<dyn Fn(Uuid, gtk::Widget)>;
type NewEventFn = Box<dyn Fn()>;

const INITIAL_FUTURE_DAYS: i64 = 56;
const CHUNK_DAYS: i64 = 28;
const EDGE_THRESHOLD: f64 = 420.0;
const COMPACT_WIDTH: i32 = 700;

#[derive(Clone, Copy)]
pub struct Anchor {
    date: NaiveDate,
    offset: f64,
}

pub(crate) struct AgendaRowState {
    button: gtk::Button,
    time_label: gtk::Label,
    title_label: gtk::Label,
    now_indicator: gtk::Label,
    schedule: EventSchedule,
    time_text: String,
    title: String,
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
        pub agenda_range: RefCell<AgendaRange>,
        pub cached_calendars: RefCell<Vec<Calendar>>,
        pub cached_events: RefCell<Vec<Event>>,
        pub rendered_groups: RefCell<Vec<AgendaGroup>>,
        pub(crate) agenda_rows: RefCell<Vec<AgendaRowState>>,
        pub applied_compact: Cell<Option<bool>>,
        pub clock_date: Cell<NaiveDate>,
        pub clock_source: RefCell<Option<glib::SourceId>>,
        pub pending_work: Cell<bool>,
        pub pending_bottom: Cell<bool>,
        pub pending_target: RefCell<Option<NaiveDate>>,
        pub programmatic_guard: Cell<bool>,
        pub deferred_work: Cell<bool>,
        pub restore_request: RefCell<Option<Restore>>,
        pub restore_tick: RefCell<Option<gtk::TickCallbackId>>,
        pub restore_idle: RefCell<Option<glib::SourceId>>,
        pub on_event_activate: RefCell<Option<EventActivateFn>>,
        pub on_new_event: RefCell<Option<NewEventFn>>,
    }

    impl Default for AgendaView {
        fn default() -> Self {
            let fallback = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            Self {
                agenda_scroll: TemplateChild::default(),
                agenda_days_box: TemplateChild::default(),
                active_date: Cell::new(fallback),
                agenda_range: RefCell::new(AgendaRange::new(fallback, INITIAL_FUTURE_DAYS)),
                cached_calendars: RefCell::new(Vec::new()),
                cached_events: RefCell::new(Vec::new()),
                rendered_groups: RefCell::new(Vec::new()),
                agenda_rows: RefCell::new(Vec::new()),
                applied_compact: Cell::new(None),
                clock_date: Cell::new(fallback),
                clock_source: RefCell::new(None),
                pending_work: Cell::new(false),
                pending_bottom: Cell::new(false),
                pending_target: RefCell::new(None),
                programmatic_guard: Cell::new(false),
                deferred_work: Cell::new(false),
                restore_request: RefCell::new(None),
                restore_tick: RefCell::new(None),
                restore_idle: RefCell::new(None),
                on_event_activate: RefCell::new(None),
                on_new_event: RefCell::new(None),
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
                self.clock_date.set(today);
                *self.agenda_range.borrow_mut() = AgendaRange::new(today, INITIAL_FUTURE_DAYS);
            }

            let view_weak = self.obj().downgrade();
            self.agenda_scroll
                .vadjustment()
                .connect_value_changed(move |_| {
                    if let Some(view) = view_weak.upgrade() {
                        view.queue_edge_extension();
                    }
                });

            let view_weak = self.obj().downgrade();
            self.obj().connect_notify_local(Some("width"), move |_, _| {
                if let Some(view) = view_weak.upgrade() {
                    let previous_compact = view.imp().applied_compact.get();
                    view.update_content_layout();
                    if previous_compact != Some(view.compact_for_width()) {
                        view.queue_work();
                    }
                }
            });
        }

        fn dispose(&self) {
            self.obj().stop_clock();
            self.obj().cancel_restore_callbacks();
        }
    }

    impl WidgetImpl for AgendaView {
        fn map(&self) {
            self.parent_map();
            self.obj().start_clock();
            self.obj().schedule_restore_sequence();
        }

        fn unmap(&self) {
            self.obj().stop_clock();
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

    pub fn set_on_new_event<F: Fn() + 'static>(&self, f: F) {
        *self.imp().on_new_event.borrow_mut() = Some(Box::new(f));
    }

    pub fn set_active_date(&self, date: NaiveDate) {
        let imp = self.imp();
        let target = date.max(local_today());
        imp.active_date.set(target);
        imp.agenda_range.borrow_mut().ensure_target(target);
        *imp.pending_target.borrow_mut() = Some(target);
        self.queue_work();
    }

    pub fn active_date(&self) -> NaiveDate {
        self.imp().active_date.get()
    }

    pub fn render(&self, calendars: &[Calendar], events: &[Event]) {
        let imp = self.imp();
        *imp.cached_calendars.borrow_mut() = calendars.to_vec();
        *imp.cached_events.borrow_mut() = events.to_vec();
        self.queue_work();
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
        if adjustment.value() > EDGE_THRESHOLD && remaining <= EDGE_THRESHOLD {
            imp.pending_bottom.set(true);
            self.queue_work();
        }
    }

    fn queue_work(&self) {
        let imp = self.imp();
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
        let bottom = imp.pending_bottom.take();
        let target = imp.pending_target.borrow_mut().take();
        self.update_content_layout();
        let anchor = self.capture_anchor();

        imp.programmatic_guard.set(true);
        if bottom {
            imp.agenda_range.borrow_mut().extend_bottom(CHUNK_DAYS);
        }
        self.rebuild_groups(target, anchor);
    }

    fn rebuild_groups(&self, target: Option<NaiveDate>, anchor: Option<Anchor>) {
        let imp = self.imp();
        let calendars = imp.cached_calendars.borrow();
        let events = imp.cached_events.borrow();
        let today = local_today();
        let viewer_timezone = now_local_fixed().offset().to_owned();
        let plan = render_agenda(
            today,
            &imp.agenda_range.borrow(),
            &calendars,
            &events,
            &viewer_timezone,
        );
        let event_map: HashMap<Uuid, &Event> =
            events.iter().map(|event| (event.id, event)).collect();
        let view_weak = self.downgrade();
        let mut agenda_rows = Vec::new();

        while let Some(child) = imp.agenda_days_box.first_child() {
            imp.agenda_days_box.remove(&child);
        }
        let compact = self.compact_for_width();
        imp.applied_compact.set(Some(compact));
        match plan {
            AgendaRenderPlan::NoUpcoming => {
                imp.agenda_days_box
                    .append(&create_no_upcoming_widget(&view_weak));
                imp.rendered_groups.borrow_mut().clear();
            }
            AgendaRenderPlan::Groups(groups) => {
                for group in &groups {
                    imp.agenda_days_box.append(&create_group_widget(
                        group,
                        &event_map,
                        &view_weak,
                        compact,
                        today,
                        &mut agenda_rows,
                    ));
                }
                *imp.rendered_groups.borrow_mut() = groups;
            }
        }
        *imp.agenda_rows.borrow_mut() = agenda_rows;

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

    fn update_content_layout(&self) {
        let width = self.width();
        if width <= 0 {
            return;
        }
        let compact = width <= COMPACT_WIDTH;
        let margin = if compact { 10 } else { 24 };
        let days_box = &self.imp().agenda_days_box;
        days_box.set_margin_start(margin);
        days_box.set_margin_end(margin);
        days_box.set_margin_top(if compact { 16 } else { 28 });
        days_box.set_margin_bottom(if compact { 16 } else { 28 });
    }

    fn compact_for_width(&self) -> bool {
        self.width() > 0 && self.width() <= COMPACT_WIDTH
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
            self.queue_work();
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

    fn start_clock(&self) {
        if self.imp().clock_source.borrow().is_some() {
            return;
        }
        self.refresh_clock();
        self.schedule_clock_tick();
    }

    fn stop_clock(&self) {
        if let Some(source) = self.imp().clock_source.borrow_mut().take() {
            source.remove();
        }
    }

    fn schedule_clock_tick(&self) {
        if !self.is_mapped() {
            return;
        }
        let now = now_local_fixed();
        let elapsed = now.second() as u64 * 1_000_000 + now.nanosecond() as u64 / 1_000;
        let delay = StdDuration::from_micros(60_000_000_u64.saturating_sub(elapsed).max(1_000));
        let view_weak = self.downgrade();
        let source = glib::timeout_add_local_once(delay, move || {
            if let Some(view) = view_weak.upgrade() {
                view.clock_tick();
            }
        });
        *self.imp().clock_source.borrow_mut() = Some(source);
    }

    fn clock_tick(&self) {
        self.imp().clock_source.borrow_mut().take();
        if !self.is_mapped() {
            return;
        }
        self.refresh_clock();
        self.schedule_clock_tick();
    }

    fn refresh_clock(&self) {
        let now = now_local_fixed();
        let Some(today) = NaiveDate::from_ymd_opt(now.year(), now.month(), now.day()) else {
            return;
        };

        let date_changed = self.imp().clock_date.replace(today) != today;
        if date_changed {
            {
                let mut range = self.imp().agenda_range.borrow_mut();
                range.start_date = range.start_date.max(today);
            }
            self.queue_work();
        }

        for row in self.imp().agenda_rows.borrow().iter() {
            apply_row_state(row, event_state(&row.schedule, now));
        }
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

fn create_group_widget(
    group: &AgendaGroup,
    event_map: &HashMap<Uuid, &Event>,
    view_weak: &glib::WeakRef<AgendaView>,
    compact: bool,
    today: NaiveDate,
    agenda_rows: &mut Vec<AgendaRowState>,
) -> gtk::Box {
    if let AgendaGroup::EmptyRange {
        start_date,
        end_date_exclusive,
    } = group
        && *start_date == today
        && *end_date_exclusive == today + Duration::days(1)
    {
        return create_empty_today_widget(view_weak, today, compact);
    }

    let day_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(if compact { 8 } else { 16 })
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .css_classes(["agenda-group"])
        .build();

    match group {
        AgendaGroup::EventDay(day) => {
            day_box.append(&create_date_rail(day.date, today, compact));
            day_box.append(&create_event_card(
                day.all_day.iter().chain(day.timed.iter()),
                event_map,
                view_weak,
                compact,
                agenda_rows,
            ));
        }
        AgendaGroup::EmptyRange {
            start_date,
            end_date_exclusive,
        } => {
            let rail = gtk::Box::builder()
                .width_request(if compact { 54 } else { 78 })
                .build();
            day_box.append(&rail);
            day_box.append(&create_empty_divider(
                *start_date,
                *end_date_exclusive,
                today,
                compact,
            ));
        }
    }
    day_box
}

fn create_date_rail(date: NaiveDate, today: NaiveDate, compact: bool) -> gtk::Box {
    let rail = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(0)
        .width_request(if compact { 54 } else { 78 })
        .valign(gtk::Align::Start)
        .halign(gtk::Align::Center)
        .css_classes(["agenda-date-rail"])
        .build();
    if date == today {
        rail.add_css_class("today");
    }
    rail.append(
        &gtk::Label::builder()
            .label(weekday_short(date).to_uppercase())
            .css_classes(["agenda-date-weekday"])
            .build(),
    );
    rail.append(
        &gtk::Label::builder()
            .label(date.day().to_string())
            .css_classes(["agenda-date-number"])
            .build(),
    );
    rail.append(
        &gtk::Label::builder()
            .label(month_short(date.month()).to_uppercase())
            .css_classes(["agenda-date-month"])
            .build(),
    );
    rail
}

fn create_event_card<'a>(
    chips: impl Iterator<Item = &'a EventChip>,
    event_map: &HashMap<Uuid, &Event>,
    view_weak: &glib::WeakRef<AgendaView>,
    compact: bool,
    agenda_rows: &mut Vec<AgendaRowState>,
) -> gtk::Box {
    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Start)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["agenda-event-card"])
        .build();
    let now = now_local_fixed();
    for (index, chip) in chips.enumerate() {
        if index > 0 {
            card.append(
                &gtk::Separator::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .css_classes(["agenda-event-separator"])
                    .build(),
            );
        }
        if let Some(event) = event_map.get(&chip.event_id) {
            let (button, row_state) = create_event_row(chip, event, view_weak, compact, now);
            card.append(&button);
            agenda_rows.push(row_state);
        }
    }
    card
}

fn create_event_row(
    chip: &EventChip,
    event: &Event,
    view_weak: &glib::WeakRef<AgendaView>,
    compact: bool,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> (gtk::Button, AgendaRowState) {
    let schedule = occurrence_schedule(event, chip);
    let state = event_state(&schedule, now);
    let preference = load_time_format_preference();
    let system_format = system_clock_format();
    let time = if chip.is_all_day {
        "All day".to_string()
    } else {
        time_text(
            chip,
            if compact {
                AgendaTimeLayout::Compact
            } else {
                AgendaTimeLayout::Desktop
            },
            preference,
            &system_format,
        )
        .unwrap_or_else(|| "All day".to_string())
    };
    let button = gtk::Button::builder()
        .css_classes(["agenda-event-row", "flat"])
        .halign(gtk::Align::Fill)
        .can_focus(true)
        .tooltip_text(&event.title)
        .build();
    match state {
        AgendaEventState::Past => button.add_css_class("past"),
        AgendaEventState::Current => button.add_css_class("current"),
        AgendaEventState::Upcoming => {}
    }
    button.set_cursor_from_name(Some("pointer"));

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(if compact { 8 } else { 12 })
        .margin_start(if compact { 10 } else { 14 })
        .margin_end(if compact { 10 } else { 14 })
        .margin_top(if compact { 9 } else { 12 })
        .margin_bottom(if compact { 9 } else { 12 })
        .build();
    let color = sanitize_color(&chip.color);
    let swatch = gtk::Label::builder()
        .label("●")
        .valign(gtk::Align::Center)
        .css_classes(["agenda-event-dot"])
        .build();
    swatch.set_markup(&format!("<span foreground=\"#{color}\">●</span>"));
    content.append(&swatch);

    let time_label = gtk::Label::builder()
        .label(&time)
        .width_request(if compact { 76 } else { 144 })
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .halign(gtk::Align::Start)
        .xalign(0.0)
        .css_classes(["agenda-event-time", "dim-label"])
        .build();
    if state == AgendaEventState::Current {
        time_label.add_css_class("current-text");
    }
    content.append(&time_label);

    let title = gtk::Label::builder()
        .label(&event.title)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .xalign(0.0)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["agenda-event-title"])
        .build();
    if state == AgendaEventState::Current {
        title.add_css_class("current-text");
    }
    content.append(&title);
    let now_indicator = gtk::Label::builder()
        .label("NOW")
        .css_classes(["agenda-now", "current-text"])
        .visible(state == AgendaEventState::Current)
        .build();
    content.append(&now_indicator);
    button.set_child(Some(&content));
    let row_state = AgendaRowState {
        button: button.clone(),
        time_label: time_label.clone(),
        title_label: title.clone(),
        now_indicator,
        schedule,
        time_text: time,
        title: event.title.clone(),
    };
    apply_row_state(&row_state, state);

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
    (button, row_state)
}

fn occurrence_schedule(event: &Event, chip: &EventChip) -> EventSchedule {
    match (&event.schedule, &chip.viewer_local_end) {
        (
            EventSchedule::Timed {
                start,
                end,
                timezone,
            },
            calendar::month_view::ViewerLocalEnd::Timed(viewer_end),
        ) => EventSchedule::Timed {
            start: viewer_end
                .checked_sub_signed(*end - *start)
                .unwrap_or(*viewer_end),
            end: *viewer_end,
            timezone: timezone.clone(),
        },
        (
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            },
            calendar::month_view::ViewerLocalEnd::AllDay(viewer_end),
        ) => {
            let duration = (*end_date_exclusive - *start_date).num_days();
            let start_date = viewer_end
                .checked_sub_signed(Duration::days(duration))
                .unwrap_or(*viewer_end);
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive: *viewer_end,
            }
        }
        _ => event.schedule.clone(),
    }
}

fn apply_row_state(row: &AgendaRowState, state: AgendaEventState) {
    row.button.remove_css_class("past");
    row.button.remove_css_class("current");
    row.time_label.remove_css_class("current-text");
    row.title_label.remove_css_class("current-text");

    if state == AgendaEventState::Past {
        row.button.add_css_class("past");
    } else if state == AgendaEventState::Current {
        row.button.add_css_class("current");
        row.time_label.add_css_class("current-text");
        row.title_label.add_css_class("current-text");
    }

    let current = state == AgendaEventState::Current;
    if current {
        row.now_indicator.add_css_class("current-text");
    } else {
        row.now_indicator.remove_css_class("current-text");
    }
    row.now_indicator.set_visible(current);
    row.now_indicator
        .set_label(if current { "NOW" } else { "" });
    let accessible_label = if current {
        format!("{}: {}, currently running", row.time_text, row.title)
    } else {
        format!("{}: {}", row.time_text, row.title)
    };
    row.button
        .update_property(&[gtk::accessible::Property::Label(&accessible_label)]);
}

fn create_empty_today_widget(
    view_weak: &glib::WeakRef<AgendaView>,
    today: NaiveDate,
    compact: bool,
) -> gtk::Box {
    let group = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(if compact { 8 } else { 16 })
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .css_classes(["agenda-group"])
        .build();
    group.append(&create_date_rail(today, today, compact));

    let card = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(if compact { 12 } else { 18 })
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["agenda-event-card", "agenda-today-empty"])
        .build();
    card.append(
        &gtk::Label::builder()
            .label("Nothing scheduled today")
            .hexpand(true)
            .halign(gtk::Align::Start)
            .css_classes(["dim-label"])
            .build(),
    );
    let new_event_button = create_new_event_button(view_weak);
    new_event_button.set_valign(gtk::Align::Center);
    card.append(&new_event_button);
    group.append(&card);
    group
}

fn create_empty_divider(
    start: NaiveDate,
    end_exclusive: NaiveDate,
    today: NaiveDate,
    compact: bool,
) -> gtk::Box {
    let divider = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(12)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .css_classes(["agenda-empty-divider"])
        .build();
    divider.append(
        &gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Center)
            .build(),
    );
    divider.append(
        &gtk::Label::builder()
            .label(empty_range_label(start, end_exclusive, today, compact))
            .css_classes(["dim-label"])
            .build(),
    );
    divider.append(
        &gtk::Separator::builder()
            .orientation(gtk::Orientation::Horizontal)
            .hexpand(true)
            .halign(gtk::Align::Fill)
            .valign(gtk::Align::Center)
            .build(),
    );
    divider
}

fn create_new_event_button(view_weak: &glib::WeakRef<AgendaView>) -> gtk::Button {
    let button = gtk::Button::with_label("New event");
    button.add_css_class("agenda-new-event");
    button.set_halign(gtk::Align::Center);
    button.set_tooltip_text(Some("Create a new event"));
    button.update_property(&[gtk::accessible::Property::Label("Create a new event")]);
    let view_weak = view_weak.clone();
    button.connect_clicked(move |_| {
        if let Some(view) = view_weak.upgrade()
            && let Some(callback) = view.imp().on_new_event.borrow().as_ref()
        {
            callback();
        }
    });
    button
}

fn create_no_upcoming_widget(view_weak: &glib::WeakRef<AgendaView>) -> gtk::Box {
    let status = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(12)
        .height_request(300)
        .valign(gtk::Align::Center)
        .halign(gtk::Align::Fill)
        .hexpand(true)
        .css_classes(["agenda-no-upcoming"])
        .build();
    status.append(
        &gtk::Image::builder()
            .icon_name("calendar-today-symbolic")
            .pixel_size(64)
            .halign(gtk::Align::Center)
            .css_classes(["agenda-empty-icon"])
            .build(),
    );
    status.append(
        &gtk::Label::builder()
            .label("No upcoming events")
            .css_classes(["title-2"])
            .halign(gtk::Align::Center)
            .build(),
    );
    status.append(
        &gtk::Label::builder()
            .label("Nothing is scheduled from today onwards. Past events stay available in Month and Week.")
            .wrap(true)
            .justify(gtk::Justification::Center)
            .halign(gtk::Align::Center)
            .css_classes(["dim-label"])
            .build(),
    );
    status.append(&create_new_event_button(view_weak));
    status
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

fn empty_range_label(
    start: NaiveDate,
    end_exclusive: NaiveDate,
    today: NaiveDate,
    compact: bool,
) -> String {
    let end = end_exclusive - Duration::days(1);
    if start == end {
        return match (start - today).num_days() {
            -1 => "nothing yesterday".to_string(),
            0 => "nothing today".to_string(),
            1 => "nothing tomorrow".to_string(),
            _ => format!("nothing {}", short_date(start)),
        };
    }
    let days = (end - start).num_days() + 1;
    if compact {
        return format!("nothing for {days} days");
    }
    format!("nothing {} – {}", short_date(start), short_date(end),)
}

fn short_date(date: NaiveDate) -> String {
    format!("{} {}", date.day(), month_short(date.month()))
}

fn local_today() -> NaiveDate {
    let now = now_local_fixed();
    NaiveDate::from_ymd_opt(now.year(), now.month(), now.day())
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 1, 5).unwrap())
}

fn weekday_short(date: NaiveDate) -> &'static str {
    match date.weekday() {
        chrono::Weekday::Mon => "Mon",
        chrono::Weekday::Tue => "Tue",
        chrono::Weekday::Wed => "Wed",
        chrono::Weekday::Thu => "Thu",
        chrono::Weekday::Fri => "Fri",
        chrono::Weekday::Sat => "Sat",
        chrono::Weekday::Sun => "Sun",
    }
}

fn month_short(month: u32) -> &'static str {
    [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][month as usize - 1]
}

fn sanitize_color(color: &str) -> String {
    color
        .trim_start_matches('#')
        .chars()
        .filter(|character| character.is_ascii_hexdigit())
        .collect()
}
