use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, Event, EventSchedule};
use calendar::month_view::{DayProjection, EventChip, project_week};
use calendar::view_state::{ViewKind, ViewState};
use calendar::viewer_time::{now_local_fixed, to_local_fixed};
use chrono::{Datelike, NaiveDate, Timelike};
use gtk::{gdk, glib, graphene, gsk};
use uuid::Uuid;

type EventActivateFn = Box<dyn Fn(Uuid, gtk::Widget)>;

const HOUR_HEIGHT: f64 = 72.0;
const MINUTES_PER_DAY: usize = 24 * 60;
const TIMELINE_HEIGHT: i32 = 24 * 72;

pub struct TimedButton {
    button: gtk::Button,
    day: usize,
    start_minutes: f64,
    end_minutes: f64,
    lane: usize,
    lane_count: usize,
}

mod grid_imp {
    use super::*;

    pub struct WeekGrid {
        pub dates: RefCell<[NaiveDate; 7]>,
        pub today: Cell<NaiveDate>,
        pub now_minutes: Cell<Option<f64>>,
        pub timed_buttons: RefCell<Vec<TimedButton>>,
        pub on_event_activate: RefCell<Option<EventActivateFn>>,
    }

    impl Default for WeekGrid {
        fn default() -> Self {
            let fallback = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            Self {
                dates: RefCell::new(std::array::from_fn(|day| {
                    fallback + chrono::Duration::days(day as i64)
                })),
                today: Cell::new(fallback),
                now_minutes: Cell::new(None),
                timed_buttons: RefCell::new(Vec::new()),
                on_event_activate: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WeekGrid {
        const NAME: &'static str = "WeekGrid";
        type Type = super::WeekGrid;
        type ParentType = gtk::Widget;
    }

    impl ObjectImpl for WeekGrid {
        fn dispose(&self) {
            for timed_button in self.timed_buttons.borrow_mut().drain(..) {
                timed_button.button.unparent();
            }
        }
    }

    impl WidgetImpl for WeekGrid {
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let mut minimum = 0;
            let mut natural = 0;
            for timed_button in self.timed_buttons.borrow().iter() {
                let child_for_size = if orientation == gtk::Orientation::Vertical {
                    for_size
                        .try_into()
                        .ok()
                        .map(|width: u32| {
                            ((width as f64 / 7.0 / timed_button.lane_count as f64) - 4.0).max(0.0)
                                as i32
                        })
                        .unwrap_or(for_size)
                } else {
                    for_size
                };
                let (child_minimum, child_natural, _, _) =
                    timed_button.button.measure(orientation, child_for_size);
                minimum = minimum.max(child_minimum);
                natural = natural.max(child_natural);
            }

            if orientation == gtk::Orientation::Vertical {
                (TIMELINE_HEIGHT, TIMELINE_HEIGHT, -1, -1)
            } else {
                (minimum.saturating_mul(7), natural.saturating_mul(7), -1, -1)
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().allocate_timed_buttons(width, height);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().snapshot_grid(snapshot);
        }
    }
}

glib::wrapper! {
    pub struct WeekGrid(ObjectSubclass<grid_imp::WeekGrid>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl WeekGrid {
    fn new() -> Self {
        let grid: Self = glib::Object::new();
        grid.add_css_class("weekview-grid");
        grid
    }

    fn set_on_event_activate<F: Fn(Uuid, gtk::Widget) + 'static>(&self, f: F) {
        *self.imp().on_event_activate.borrow_mut() = Some(Box::new(f));
    }

    fn render(
        &self,
        dates: &[NaiveDate; 7],
        today: NaiveDate,
        now_minutes: Option<f64>,
        projections: &[DayProjection; 7],
        events: &[Event],
    ) {
        let imp = self.imp();
        for timed_button in imp.timed_buttons.borrow_mut().drain(..) {
            timed_button.button.unparent();
        }
        *imp.dates.borrow_mut() = *dates;
        imp.today.set(today);
        imp.now_minutes.set(now_minutes);

        let event_map: HashMap<Uuid, &Event> =
            events.iter().map(|event| (event.id, event)).collect();
        let grid_weak = self.downgrade();
        let mut buttons = Vec::new();

        for (day, projection) in projections.iter().enumerate() {
            let mut segments = Vec::new();
            for chip in &projection.timed {
                let Some(event) = event_map.get(&chip.event_id) else {
                    continue;
                };
                let EventSchedule::Timed { start, end, .. } = &event.schedule else {
                    continue;
                };
                let start = to_local_fixed(start);
                let end = to_local_fixed(end);
                let start_minutes = if start.date_naive() == dates[day] {
                    time_minutes(start.time())
                } else {
                    0.0
                };
                let end_minutes = if end.date_naive() == dates[day] {
                    time_minutes(end.time())
                } else {
                    MINUTES_PER_DAY as f64
                };
                if end_minutes > start_minutes {
                    segments.push((chip, start_minutes, end_minutes));
                }
            }

            segments.sort_by(|a, b| {
                a.1.total_cmp(&b.1)
                    .then_with(|| a.2.total_cmp(&b.2))
                    .then_with(|| a.0.event_id.cmp(&b.0.event_id))
            });
            let mut lane_ends = Vec::new();
            let mut placements = Vec::new();
            for (chip, start_minutes, end_minutes) in segments {
                let lane = lane_ends
                    .iter()
                    .position(|end| *end <= start_minutes)
                    .unwrap_or_else(|| {
                        lane_ends.push(0.0);
                        lane_ends.len() - 1
                    });
                lane_ends[lane] = end_minutes;
                placements.push((chip, start_minutes, end_minutes, lane));
            }
            let lane_count = lane_ends.len().max(1);
            for (chip, start_minutes, end_minutes, lane) in placements {
                let button = create_event_button(chip, &grid_weak, "weekview-event");
                button.set_parent(self);
                buttons.push(TimedButton {
                    button,
                    day,
                    start_minutes,
                    end_minutes,
                    lane,
                    lane_count,
                });
            }
        }
        *imp.timed_buttons.borrow_mut() = buttons;
        self.queue_resize();
        self.queue_draw();
    }

    fn set_clock(&self, today: NaiveDate, now_minutes: Option<f64>) {
        let imp = self.imp();
        imp.today.set(today);
        imp.now_minutes.set(now_minutes);
        self.queue_draw();
    }

    fn allocate_timed_buttons(&self, width: i32, _height: i32) {
        let column_width = width as f64 / 7.0;
        for timed_button in self.imp().timed_buttons.borrow().iter() {
            let lane_width = column_width / timed_button.lane_count as f64;
            let x = timed_button.day as f64 * column_width
                + timed_button.lane as f64 * lane_width
                + 2.0;
            let y = timed_button.start_minutes / 60.0 * HOUR_HEIGHT;
            let button_width = (lane_width - 4.0).max(20.0) as i32;
            let duration_height = ((timed_button.end_minutes - timed_button.start_minutes) / 60.0
                * HOUR_HEIGHT) as i32;
            let (minimum_height, _, _, _) = timed_button
                .button
                .measure(gtk::Orientation::Vertical, button_width);
            let button_height = duration_height.max(minimum_height);
            let transform =
                gsk::Transform::new().translate(&graphene::Point::new(x as f32, y as f32));
            timed_button
                .button
                .allocate(button_width, button_height, -1, Some(transform));
        }
    }

    fn snapshot_grid(&self, snapshot: &gtk::Snapshot) {
        let width = self.width() as f32;
        let height = self.height().max(TIMELINE_HEIGHT) as f32;
        let column_width = width / 7.0;
        let hour_color = gdk::RGBA::new(0.45, 0.45, 0.45, 0.24);
        let half_hour_color = gdk::RGBA::new(0.45, 0.45, 0.45, 0.10);
        for half_hour in 0..=48 {
            let y = half_hour as f32 * HOUR_HEIGHT as f32 / 2.0;
            let color = if half_hour % 2 == 0 {
                &hour_color
            } else {
                &half_hour_color
            };
            snapshot.append_color(color, &graphene::Rect::new(0.0, y, width, 1.0));
        }
        let separator_color = gdk::RGBA::new(0.45, 0.45, 0.45, 0.14);
        for day in 0..=7 {
            let x = day as f32 * column_width;
            snapshot.append_color(&separator_color, &graphene::Rect::new(x, 0.0, 1.0, height));
        }
        for timed_button in self.imp().timed_buttons.borrow().iter() {
            self.snapshot_child(&timed_button.button, snapshot);
        }

        let dates = self.imp().dates.borrow();
        let Some(day) = dates
            .iter()
            .position(|date| *date == self.imp().today.get())
        else {
            return;
        };
        let Some(minutes) = self.imp().now_minutes.get() else {
            return;
        };
        let accent = self.color();
        let y = minutes / 60.0 * HOUR_HEIGHT;
        snapshot.append_color(
            &accent,
            &graphene::Rect::new(day as f32 * column_width, y as f32, column_width, 2.0),
        );
    }
}

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate)]
    #[template(resource = "/dev/chris/calendar/ui/views/week-view.ui")]
    pub struct WeekView {
        #[template_child]
        pub headers_grid: TemplateChild<gtk::Grid>,
        #[template_child]
        pub all_day_columns: TemplateChild<gtk::Box>,
        #[template_child]
        pub timeline_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub time_labels_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub timeline_grid_bin: TemplateChild<adw::Bin>,

        pub active_date: Cell<NaiveDate>,
        pub today_date: Cell<NaiveDate>,
        pub cached_calendars: RefCell<Vec<Calendar>>,
        pub cached_events: RefCell<Vec<Event>>,
        pub on_event_activate: RefCell<Option<EventActivateFn>>,
        pub initial_scroll_eligible: Cell<bool>,
        pub initial_scroll_pending: Cell<bool>,
        pub initial_scroll_source: RefCell<Option<glib::SourceId>>,
        pub focus_scroll_source: RefCell<Option<glib::SourceId>>,
        pub clock_source: RefCell<Option<glib::SourceId>>,
        pub week_grid: RefCell<Option<WeekGrid>>,
    }

    impl Default for WeekView {
        fn default() -> Self {
            let fallback = NaiveDate::from_ymd_opt(2026, 1, 5).unwrap();
            Self {
                headers_grid: TemplateChild::default(),
                all_day_columns: TemplateChild::default(),
                timeline_scroll: TemplateChild::default(),
                time_labels_box: TemplateChild::default(),
                timeline_grid_bin: TemplateChild::default(),
                active_date: Cell::new(fallback),
                today_date: Cell::new(fallback),
                cached_calendars: RefCell::new(Vec::new()),
                cached_events: RefCell::new(Vec::new()),
                on_event_activate: RefCell::new(None),
                initial_scroll_eligible: Cell::new(true),
                initial_scroll_pending: Cell::new(false),
                initial_scroll_source: RefCell::new(None),
                focus_scroll_source: RefCell::new(None),
                clock_source: RefCell::new(None),
                week_grid: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for WeekView {
        const NAME: &'static str = "WeekView";
        type Type = super::WeekView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for WeekView {
        fn constructed(&self) {
            self.parent_constructed();

            let now = now_local_fixed();
            let today = NaiveDate::from_ymd_opt(now.year(), now.month(), now.day())
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
            self.today_date.set(today);
            self.active_date.set(today);

            for hour in 0..24 {
                let label = gtk::Label::builder()
                    .label(format_hour(hour))
                    .halign(gtk::Align::End)
                    .valign(gtk::Align::Start)
                    .margin_end(8)
                    .height_request(HOUR_HEIGHT as i32)
                    .css_classes(["weekview-hour-label"])
                    .build();
                self.time_labels_box.append(&label);
            }

            let week_grid = WeekGrid::new();
            let view_weak = self.obj().downgrade();
            week_grid.set_on_event_activate(move |event_id, event_widget| {
                if let Some(view) = view_weak.upgrade()
                    && let Some(callback) = view.imp().on_event_activate.borrow().as_ref()
                {
                    callback(event_id, event_widget);
                }
            });
            self.timeline_grid_bin.set_child(Some(&week_grid));
            *self.week_grid.borrow_mut() = Some(week_grid);

            let obj_weak = self.obj().downgrade();
            self.timeline_scroll
                .vadjustment()
                .connect_changed(move |_| {
                    if let Some(obj) = obj_weak.upgrade() {
                        obj.schedule_initial_scroll();
                    }
                });
        }

        fn dispose(&self) {
            let obj = self.obj();
            obj.cancel_focus_scroll_suppression();
            obj.stop_clock();
        }
    }

    impl WidgetImpl for WeekView {
        fn map(&self) {
            self.parent_map();
            self.obj().suppress_focus_scrolling();
            self.obj().request_initial_scroll();
            self.obj().start_clock();
        }

        fn unmap(&self) {
            self.obj().cancel_initial_scroll();
            self.obj().cancel_focus_scroll_suppression();
            self.obj().stop_clock();
            self.parent_unmap();
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().schedule_initial_scroll();
        }
    }
    impl BinImpl for WeekView {}
}

glib::wrapper! {
    pub struct WeekView(ObjectSubclass<imp::WeekView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl WeekView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_on_event_activate<F: Fn(Uuid, gtk::Widget) + 'static>(&self, f: F) {
        *self.imp().on_event_activate.borrow_mut() = Some(Box::new(f));
    }

    /// Return the date used to derive the currently rendered week.
    pub fn active_date(&self) -> NaiveDate {
        self.imp().active_date.get()
    }

    /// Synchronise the rendered week with the shared window date.
    pub fn set_active_date(&self, date: NaiveDate) {
        let today = self.imp().today_date.get();
        let was_current_week = self.week_contains(today);
        self.imp().active_date.set(date);
        let is_current_week = self.week_contains(today);
        if is_current_week && !was_current_week {
            self.request_initial_scroll();
        } else if !is_current_week {
            self.cancel_initial_scroll();
        }
        let (calendars, events) = {
            let imp = self.imp();
            (
                imp.cached_calendars.borrow().clone(),
                imp.cached_events.borrow().clone(),
            )
        };
        self.render(&calendars, &events);
    }

    pub fn render(&self, calendars: &[Calendar], events: &[Event]) {
        self.suppress_focus_scrolling();
        let imp = self.imp();
        *imp.cached_calendars.borrow_mut() = calendars.to_vec();
        *imp.cached_events.borrow_mut() = events.to_vec();

        let state = ViewState::new(ViewKind::Week, imp.active_date.get());
        let dates = state.current_week_dates();
        let projections = project_week(state.active_date(), calendars, events);
        self.render_headers(&dates);
        self.render_all_day(&projections, events);
        if let Some(grid) = self.imp().week_grid.borrow().as_ref() {
            grid.render(
                &dates,
                imp.today_date.get(),
                self.local_now_minutes(),
                &projections,
                events,
            );
        }
    }

    fn render_headers(&self, dates: &[NaiveDate; 7]) {
        let imp = self.imp();
        while let Some(child) = imp.headers_grid.first_child() {
            imp.headers_grid.remove(&child);
        }

        for (index, date) in dates.iter().enumerate() {
            let weekday = gtk::Label::builder()
                .label(weekday_name(index))
                .css_classes(["weekview-weekday"])
                .build();
            let date_label = gtk::Label::builder()
                .label(date.day().to_string())
                .css_classes(["weekview-date"])
                .build();
            let box_ = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_top(8)
                .margin_bottom(8)
                .build();
            box_.append(&weekday);
            box_.append(&date_label);
            if *date == imp.today_date.get() {
                box_.add_css_class("today");
            }
            imp.headers_grid.attach(&box_, index as i32, 0, 1, 1);
        }
    }

    fn render_all_day(&self, projections: &[DayProjection; 7], events: &[Event]) {
        let imp = self.imp();
        while let Some(child) = imp.all_day_columns.first_child() {
            imp.all_day_columns.remove(&child);
        }

        let event_map: HashMap<Uuid, &Event> =
            events.iter().map(|event| (event.id, event)).collect();
        let grid_weak = self
            .imp()
            .week_grid
            .borrow()
            .as_ref()
            .map(WeekGrid::downgrade);
        for projection in projections {
            let day_box = gtk::Box::builder()
                .orientation(gtk::Orientation::Vertical)
                .spacing(2)
                .margin_start(2)
                .margin_end(2)
                .build();
            for chip in &projection.all_day {
                if event_map.contains_key(&chip.event_id)
                    && let Some(grid_weak) = grid_weak.as_ref()
                {
                    day_box.append(&create_event_button(
                        chip,
                        grid_weak,
                        "weekview-all-day-event",
                    ));
                }
            }
            if projection.all_day.is_empty() {
                day_box.append(&gtk::Box::builder().height_request(28).build());
            }
            imp.all_day_columns.append(&day_box);
        }
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
        let now = self.local_now();
        let elapsed = now.second() as u64 * 1_000_000 + now.nanosecond() as u64 / 1_000;
        let delay = Duration::from_micros(60_000_000_u64.saturating_sub(elapsed).max(1_000));
        let obj_weak = self.downgrade();
        let source = glib::timeout_add_local_once(delay, move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.clock_tick();
            }
        });
        *self.imp().clock_source.borrow_mut() = Some(source);
    }

    fn clock_tick(&self) {
        self.imp().clock_source.borrow_mut().take();
        self.refresh_clock();
        self.schedule_clock_tick();
    }

    fn refresh_clock(&self) {
        let now = self.local_now();
        let today = NaiveDate::from_ymd_opt(now.year(), now.month(), now.day());
        if let Some(today) = today {
            let date_changed = self.imp().today_date.replace(today) != today;
            if date_changed {
                let state = ViewState::new(ViewKind::Week, self.active_date());
                self.render_headers(&state.current_week_dates());
                if self.week_contains(today) {
                    self.request_initial_scroll();
                } else {
                    self.cancel_initial_scroll();
                }
            }
        }
        if let Some(grid) = self.imp().week_grid.borrow().as_ref() {
            grid.set_clock(
                self.imp().today_date.get(),
                Some(now.hour() as f64 * 60.0 + now.minute() as f64),
            );
        }
    }

    fn local_now(&self) -> chrono::DateTime<chrono::FixedOffset> {
        now_local_fixed()
    }

    fn local_now_minutes(&self) -> Option<f64> {
        let now = self.local_now();
        Some(now.hour() as f64 * 60.0 + now.minute() as f64)
    }

    fn request_initial_scroll(&self) {
        let imp = self.imp();
        if !imp.initial_scroll_eligible.get()
            || !self.is_mapped()
            || !self.week_contains(imp.today_date.get())
        {
            return;
        }
        imp.initial_scroll_eligible.set(false);
        imp.initial_scroll_pending.set(true);
        self.schedule_initial_scroll();
    }

    fn cancel_initial_scroll(&self) {
        self.imp().initial_scroll_pending.set(false);
        if let Some(source) = self.imp().initial_scroll_source.borrow_mut().take() {
            source.remove();
        }
    }

    fn suppress_focus_scrolling(&self) {
        let Some(viewport) = self.timeline_viewport() else {
            return;
        };
        viewport.set_scroll_to_focus(false);
        let imp = self.imp();
        if imp.focus_scroll_source.borrow().is_some() || !self.is_mapped() {
            return;
        }
        let obj_weak = self.downgrade();
        let source = glib::idle_add_local_once(move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.imp().focus_scroll_source.borrow_mut().take();
                obj.enable_focus_scrolling();
            }
        });
        *imp.focus_scroll_source.borrow_mut() = Some(source);
    }

    fn cancel_focus_scroll_suppression(&self) {
        if let Some(source) = self.imp().focus_scroll_source.borrow_mut().take() {
            source.remove();
        }
        self.enable_focus_scrolling();
    }

    fn enable_focus_scrolling(&self) {
        if let Some(viewport) = self.timeline_viewport() {
            viewport.set_scroll_to_focus(true);
        }
    }

    fn timeline_viewport(&self) -> Option<gtk::Viewport> {
        self.imp()
            .timeline_scroll
            .child()
            .and_downcast::<gtk::Viewport>()
    }

    fn schedule_initial_scroll(&self) {
        let imp = self.imp();
        if !imp.initial_scroll_pending.get()
            || !self.is_mapped()
            || imp.initial_scroll_source.borrow().is_some()
        {
            return;
        }
        let obj_weak = self.downgrade();
        let source = glib::idle_add_local_once(move || {
            if let Some(obj) = obj_weak.upgrade() {
                obj.imp().initial_scroll_source.borrow_mut().take();
                obj.scroll_to_current_time();
            }
        });
        *imp.initial_scroll_source.borrow_mut() = Some(source);
    }

    fn scroll_to_current_time(&self) {
        let imp = self.imp();
        if !imp.initial_scroll_pending.get() || !self.is_mapped() {
            return;
        }
        let today = imp.today_date.get();
        if !self.week_contains(today) {
            imp.initial_scroll_pending.set(false);
            return;
        }
        let adjustment = imp.timeline_scroll.vadjustment();
        let upper = adjustment.upper();
        let page_size = adjustment.page_size();
        if page_size <= 0.0 || upper <= adjustment.lower() {
            let obj_weak = self.downgrade();
            let source = glib::timeout_add_local_once(Duration::from_millis(16), move || {
                if let Some(obj) = obj_weak.upgrade() {
                    obj.imp().initial_scroll_source.borrow_mut().take();
                    obj.schedule_initial_scroll();
                }
            });
            *imp.initial_scroll_source.borrow_mut() = Some(source);
            return;
        }
        let now = self.local_now();
        let minutes = now.hour() as f64 * 60.0 + now.minute() as f64;
        let target = minutes / 60.0 * HOUR_HEIGHT - imp.timeline_scroll.height() as f64 / 3.0;
        let max_value = (upper - page_size).max(adjustment.lower());
        imp.initial_scroll_pending.set(false);
        adjustment.set_value(target.clamp(adjustment.lower(), max_value));
    }

    fn week_contains(&self, date: NaiveDate) -> bool {
        ViewState::new(ViewKind::Week, self.active_date())
            .current_week_dates()
            .contains(&date)
    }
}

fn create_event_button(
    chip: &EventChip,
    grid_weak: &glib::WeakRef<WeekGrid>,
    css_class: &str,
) -> gtk::Button {
    let button = gtk::Button::builder()
        .css_classes([css_class, "flat"])
        .halign(gtk::Align::Fill)
        .valign(gtk::Align::Fill)
        .can_focus(true)
        .focus_on_click(false)
        .tooltip_text(&chip.title)
        .build();
    button.set_cursor_from_name(Some("pointer"));
    apply_event_color(&button, &chip.color);

    let content = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_start(4)
        .margin_end(4)
        .margin_top(2)
        .margin_bottom(2)
        .valign(gtk::Align::Start)
        .build();
    let title = gtk::Label::builder()
        .label(&chip.title)
        .halign(gtk::Align::Start)
        .valign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .hexpand(true)
        .build();
    content.append(&title);
    button.set_child(Some(&content));

    let event_id = chip.event_id;
    let button_weak = button.downgrade();
    let grid_weak = grid_weak.clone();
    button.connect_clicked(move |_| {
        if let Some(grid) = grid_weak.upgrade()
            && let Some(button) = button_weak.upgrade()
            && let Some(callback) = grid.imp().on_event_activate.borrow().as_ref()
        {
            callback(event_id, button.upcast::<gtk::Widget>());
        }
    });
    button
}

fn time_minutes(time: chrono::NaiveTime) -> f64 {
    time.hour() as f64 * 60.0 + time.minute() as f64 + time.second() as f64 / 60.0
}

fn format_hour(hour: u32) -> String {
    if hour == 0 {
        "12 AM".to_string()
    } else if hour < 12 {
        format!("{hour} AM")
    } else if hour == 12 {
        "12 PM".to_string()
    } else {
        format!("{} PM", hour - 12)
    }
}

fn weekday_name(index: usize) -> &'static str {
    ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"][index]
}

fn apply_event_color(button: &gtk::Button, color: &str) {
    let color = sanitize_color(color);

    // Keep the provider on this button's style context so it is released with
    // the event card instead of accumulating on the display.
    let css = format!(
        "button {{\
            border-color: color-mix(in srgb, #{color} 68%, var(--window-bg-color));\
            border-left-color: #{color};\
            background-color: color-mix(in srgb, #{color} 18%, var(--window-bg-color));\
        }}\
        button:hover {{\
            border-color: color-mix(in srgb, #{color} 68%, var(--window-bg-color));\
            border-left-color: #{color};\
            background-color: color-mix(in srgb, #{color} 32%, var(--window-bg-color));\
        }}\
        button:focus-visible {{\
            outline: 2px solid #{color};\
            outline-offset: -2px;\
        }}"
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    // No current per-widget replacement exists; keep the provider scoped to this button.
    #[allow(deprecated)]
    button
        .style_context()
        .add_provider(&provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
}

fn sanitize_color(color: &str) -> String {
    let value = color.trim().strip_prefix('#').unwrap_or(color.trim());
    if value.len() == 6 && value.chars().all(|character| character.is_ascii_hexdigit()) {
        value.to_ascii_lowercase()
    } else {
        "808080".to_string()
    }
}
