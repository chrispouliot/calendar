use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, Event};
use calendar::month_view::project_month;
use calendar::viewer_time::now_local_fixed;
use calendar::weeks_buffer::{TOTAL_ROWS, VISIBLE_START, WeeksBuffer};
use chrono::{Datelike, NaiveDate, Timelike};
use gtk::{gdk, glib, graphene};
use uuid::Uuid;

/// Callback type for day activation: first click on an empty day cell
/// opens Quick Add.  Carries the (row, col) of the originating cell so
/// the host can anchor the popover at that cell rather than as a
/// free-floating dialog.  Days that contain event chips do not fire this
/// callback — event-chip preview is handled by `on_event_activate`.
type ActivateFn = Box<dyn Fn(usize, usize, NaiveDate)>;

/// Callback type for event-chip activation: fired when the user clicks
/// a chip widget inside a day cell.  Carries the event UUID and a
/// reference to the chip widget so the host can resolve the event from
/// the repository and anchor the preview popover at the chip.
type EventActivateFn = Box<dyn Fn(uuid::Uuid, gtk::Widget)>;

/// Callback type for dominant-month change (title update).
type MonthChangedFn = Box<dyn Fn(i32, u32)>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate)]
    #[template(resource = "/dev/chris/calendar/ui/views/month-view.ui")]
    pub struct MonthView {
        #[template_child]
        pub headers_grid: TemplateChild<gtk::Grid>,
        #[template_child]
        pub week_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub week_rows_box: TemplateChild<gtk::Box>,

        /// Pure WeeksBuffer for date management; initialised in constructed().
        pub weeks_buffer: RefCell<WeeksBuffer>,

        /// Today (read from clock at construction).
        pub today_date: Cell<NaiveDate>,

        /// Date represented by the currently displayed month.
        pub active_date: Cell<NaiveDate>,

        /// Do not report the transient dominant month while a host is
        /// synchronising the view to its shared active date.
        pub active_date_syncing: Cell<bool>,

        /// Last reported first-visible-week (year, month) for the title callback.
        pub last_title_ym: Cell<(i32, u32)>,

        /// Cached calendars and events for repopulation after recycling.
        pub cached_calendars: RefCell<Vec<Calendar>>,
        pub cached_events: RefCell<Vec<Event>>,

        /// Day buttons: 105 buttons (15 rows × 7 columns), created once.
        pub day_buttons: RefCell<Vec<gtk::Button>>,
        /// Chip containers: 105 vertical boxes, created once.
        pub chip_boxes: RefCell<Vec<gtk::Box>>,
        /// Row containers (15 horizontal boxes), created once for explicit sizing.
        pub row_boxes: RefCell<Vec<gtk::Box>>,

        /// Fixed row height measured from the initial viewport allocation.
        pub row_height: Cell<f64>,

        /// Guard flag to prevent recursion when updating the scroll adjustment.
        pub recycling_guard: Cell<bool>,

        /// True after the first scroll setup is complete.
        pub initialized: Cell<bool>,

        /// Minute-aligned refresh source while the view is mapped.
        pub clock_source: RefCell<Option<glib::SourceId>>,

        /// Callback fired when dominant month changes (for title update).
        pub on_month_changed: RefCell<Option<MonthChangedFn>>,

        /// Callback fired on first click of an empty day cell (Quick Add).
        pub on_activate: RefCell<Option<ActivateFn>>,

        /// Callback fired when a chip widget is clicked.
        pub on_event_activate: RefCell<Option<EventActivateFn>>,
    }

    impl Default for MonthView {
        fn default() -> Self {
            Self {
                headers_grid: TemplateChild::default(),
                week_scroll: TemplateChild::default(),
                week_rows_box: TemplateChild::default(),
                weeks_buffer: RefCell::new(WeeksBuffer::new(
                    NaiveDate::from_ymd_opt(2026, 1, 5).unwrap(),
                )),
                today_date: Cell::new(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
                active_date: Cell::new(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
                active_date_syncing: Cell::new(false),
                last_title_ym: Cell::new((2026, 1)),
                cached_calendars: RefCell::new(Vec::new()),
                cached_events: RefCell::new(Vec::new()),
                day_buttons: RefCell::new(Vec::new()),
                chip_boxes: RefCell::new(Vec::new()),
                row_boxes: RefCell::new(Vec::new()),
                row_height: Cell::new(80.0),
                recycling_guard: Cell::new(false),
                initialized: Cell::new(false),
                clock_source: RefCell::new(None),
                on_month_changed: RefCell::new(None),
                on_activate: RefCell::new(None),
                on_event_activate: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for MonthView {
        const NAME: &'static str = "MonthView";
        type Type = super::MonthView;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MonthView {
        fn constructed(&self) {
            self.parent_constructed();

            // ── Weekday headers (Mon–Sun) ──
            let day_names = ["MON", "TUE", "WED", "THU", "FRI", "SAT", "SUN"];
            for (i, name) in day_names.iter().enumerate() {
                let label = gtk::Label::builder()
                    .label(*name)
                    .halign(gtk::Align::Start)
                    .css_classes(["monthview-weekday", "heading"])
                    .margin_start(12)
                    .margin_end(4)
                    .build();
                self.headers_grid.attach(&label, i as i32, 0, 1, 1);
            }

            let obj = self.obj();
            let obj_weak = obj.downgrade();

            // ── Build 15 week-row widgets, each with 7 day buttons ──
            let mut day_buttons: Vec<gtk::Button> = Vec::with_capacity(TOTAL_ROWS * 7);
            let mut chip_boxes: Vec<gtk::Box> = Vec::with_capacity(TOTAL_ROWS * 7);
            let mut row_boxes: Vec<gtk::Box> = Vec::with_capacity(TOTAL_ROWS);

            for row in 0..TOTAL_ROWS {
                let row_box = gtk::Box::builder()
                    .orientation(gtk::Orientation::Horizontal)
                    .homogeneous(true)
                    .hexpand(true)
                    .vexpand(false)
                    .build();

                row_boxes.push(row_box.clone());

                for col in 0..7 {
                    let btn = gtk::Button::builder()
                        .css_classes(["monthview-cell", "flat"])
                        .halign(gtk::Align::Fill)
                        .valign(gtk::Align::Fill)
                        .hexpand(true)
                        .vexpand(false)
                        .can_focus(false)
                        .build();

                    // Click handler — capture (row, col) and weak obj reference.
                    let obj_weak2 = obj_weak.clone();
                    btn.connect_clicked(move |_| {
                        let Some(obj) = obj_weak2.upgrade() else {
                            return;
                        };
                        obj.handle_day_click(row, col);
                    });

                    // Vertical chip container (label + chips).
                    let chip_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(2)
                        .hexpand(true)
                        .vexpand(true)
                        .halign(gtk::Align::Fill)
                        .valign(gtk::Align::Fill)
                        .build();

                    // Day number label at top.
                    let day_label = gtk::Label::builder()
                        .css_classes(["monthview-day-label"])
                        .halign(gtk::Align::Start)
                        .margin_start(3)
                        .margin_top(2)
                        .build();

                    let cell_box = gtk::Box::builder()
                        .orientation(gtk::Orientation::Vertical)
                        .spacing(4)
                        .hexpand(true)
                        .vexpand(true)
                        .halign(gtk::Align::Fill)
                        .valign(gtk::Align::Fill)
                        .build();
                    cell_box.append(&day_label);
                    cell_box.append(&chip_box);

                    btn.set_child(Some(&cell_box));
                    row_box.append(&btn);

                    day_buttons.push(btn);
                    chip_boxes.push(chip_box);
                }

                self.week_rows_box.append(&row_box);
            }

            *self.row_boxes.borrow_mut() = row_boxes;
            *self.day_buttons.borrow_mut() = day_buttons;
            *self.chip_boxes.borrow_mut() = chip_boxes;

            // ── Initialise date state from the local clock ──
            let now = now_local_fixed();
            let today =
                NaiveDate::from_ymd_opt(now.year(), now.month(), now.day()).expect("valid today");
            self.today_date.set(today);
            self.active_date.set(today);

            // Position so today's week is the first visible week (refinement 2).
            let buf = WeeksBuffer::new(monday_of_week(today));
            *self.weeks_buffer.borrow_mut() = buf;

            // ── Discrete scroll controller (mouse wheel snaps one week) ──
            // Sits at Capture phase. SMOOTH / KINETIC events pass through
            // untouched so the ScrolledWindow's native kinetic scrolling works.
            let disc_ctrl = gtk::EventControllerScroll::new(
                gtk::EventControllerScrollFlags::VERTICAL
                    | gtk::EventControllerScrollFlags::DISCRETE,
            );
            disc_ctrl.set_propagation_phase(gtk::PropagationPhase::Capture);
            let obj_weak3 = obj_weak.clone();
            disc_ctrl.connect_scroll(move |_ctrl, _dx, dy| {
                let Some(obj) = obj_weak3.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let imp = obj.imp();
                let row_h = imp.row_height.get();
                if row_h <= 0.0 {
                    return glib::Propagation::Stop;
                }
                let adj = imp.week_scroll.vadjustment();
                let new_val = (adj.value() + dy * row_h).clamp(0.0, adj.upper() - adj.page_size());
                adj.set_value(new_val);
                glib::Propagation::Stop
            });
            self.week_scroll.add_controller(disc_ctrl);

            // ── Monitor adjustment for row recycling and centre-month tracking ──
            let obj_weak4 = obj_weak.clone();
            let vadj = self.week_scroll.vadjustment();
            vadj.connect_value_changed(move |_adj| {
                let Some(obj) = obj_weak4.upgrade() else {
                    return;
                };
                obj.check_recycle();
            });

            // Retry outside GTK's active allocation pass until the viewport
            // has an allocation; stop when setup succeeds or the widget dies.
            let obj_weak5 = obj_weak.clone();
            self.week_scroll.connect_realize(move |_| {
                let obj_weak5 = obj_weak5.clone();
                glib::source::idle_add_local(move || {
                    let Some(obj) = obj_weak5.upgrade() else {
                        return glib::ControlFlow::Break;
                    };
                    obj.setup_scroll();
                    if obj.imp().initialized.get() {
                        glib::ControlFlow::Break
                    } else {
                        glib::ControlFlow::Continue
                    }
                });
            });

            // Fire initial month-changed callback from the first visible week.
            let (y, m) = obj.first_visible_week_ym();
            self.last_title_ym.set((y, m));
            if let Some(cb) = self.on_month_changed.borrow().as_ref() {
                cb(y, m);
            }
        }

        fn dispose(&self) {
            self.obj().stop_clock();
        }
    }

    impl WidgetImpl for MonthView {
        fn map(&self) {
            self.parent_map();
            self.obj().start_clock();
        }

        fn unmap(&self) {
            self.obj().stop_clock();
            self.parent_unmap();
        }
    }
    impl BinImpl for MonthView {}
}

glib::wrapper! {
    pub struct MonthView(ObjectSubclass<imp::MonthView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

// ── Public API ──

impl MonthView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// The (year, month) of the **first visible complete week** — used for the
    /// topbar title and month-changed callbacks.  Picks Thursday (col 3) of
    /// row `VISIBLE_START` as a stable mid-week reference so a week spanning
    /// two months does not switch the title prematurely.
    pub fn dominant_year_month(&self) -> (i32, u32) {
        self.first_visible_week_ym()
    }

    /// Register a callback invoked when a **first click** on an empty day
    /// cell fires (the month view has no selection state).  The (row, col)
    /// of the cell is supplied so the host can position popovers at the
    /// originating day cell.  Days containing event chips do not fire this
    /// callback — event-chip preview is handled by `on_event_activate`.
    pub fn set_on_activate<F: Fn(usize, usize, NaiveDate) + 'static>(&self, f: F) {
        *self.imp().on_activate.borrow_mut() = Some(Box::new(f));
    }

    /// Register a callback invoked when a chip widget is clicked inside
    /// a day cell.  The (event_id, chip_widget) pair lets the host resolve
    /// the event from the repository and anchor a preview popover at the
    /// chip's on-screen location.
    pub fn set_on_event_activate<F: Fn(Uuid, gtk::Widget) + 'static>(&self, f: F) {
        *self.imp().on_event_activate.borrow_mut() = Some(Box::new(f));
    }

    /// The pixel rectangle of the day cell at `row, col` in the coordinate
    /// space of `target` (typically the application window).  Returns
    /// `None` if the cell index is out of range or the transform fails.
    pub fn day_cell_rect(
        &self,
        row: usize,
        col: usize,
        target: &impl IsA<gtk::Widget>,
    ) -> Option<gdk::Rectangle> {
        let imp = self.imp();
        let idx = row.checked_mul(7)?.checked_add(col)?;
        let buttons = imp.day_buttons.borrow();
        let button = buttons.get(idx)?.clone();
        drop(buttons);
        let origin = button.compute_point(target, &graphene::Point::new(0.0, 0.0))?;
        Some(gdk::Rectangle::new(
            origin.x() as i32,
            origin.y() as i32,
            button.width(),
            button.height(),
        ))
    }

    /// Register a callback invoked when the viewport centre month changes
    /// (title update).
    pub fn set_on_month_changed<F: Fn(i32, u32) + 'static>(&self, f: F) {
        *self.imp().on_month_changed.borrow_mut() = Some(Box::new(f));
    }

    // ── Navigation (targets calendar months) ──

    /// Navigate to the previous calendar month. The Monday on/before that
    /// month's 1st becomes the first visible row's Monday.  The viewport
    /// resets to its centred position.
    pub fn navigate_previous(&self) {
        let date = self.imp().active_date.get();
        self.set_active_date(shift_month(date, -1));
    }

    /// Navigate to the next calendar month.  See `navigate_previous`.
    pub fn navigate_next(&self) {
        let date = self.imp().active_date.get();
        self.set_active_date(shift_month(date, 1));
    }

    /// Jump to today.  Today's week becomes the first visible week (refinement 2).
    pub fn go_today(&self) {
        self.set_active_date(self.imp().today_date.get());
    }

    /// Return the date retained while navigating between calendar views.
    pub fn active_date(&self) -> NaiveDate {
        self.imp().active_date.get()
    }

    /// Display the month containing `date`, retaining its day where the
    /// destination month has that day and clamping it otherwise.
    pub fn set_active_date(&self, date: NaiveDate) {
        let imp = self.imp();
        imp.active_date.set(date);
        imp.active_date_syncing.set(true);
        *imp.weeks_buffer.borrow_mut() = WeeksBuffer::new(monday_of_week(date));
        self.reset_scroll_position();
        self.after_navigation();
        self.repopulate_rows();
        imp.active_date_syncing.set(false);
        imp.active_date.set(date);
    }

    // ── Rendering ──

    /// Store fresh calendars/events and fully repopulate all 105 cells.
    pub fn render(&self, calendars: &[Calendar], events: &[Event]) {
        let imp = self.imp();
        *imp.cached_calendars.borrow_mut() = calendars.to_vec();
        *imp.cached_events.borrow_mut() = events.to_vec();
        self.repopulate_rows();
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
        if !self.is_mapped() {
            return;
        }
        self.refresh_clock();
        self.schedule_clock_tick();
    }

    fn refresh_clock(&self) {
        let now = now_local_fixed();
        if let Some(today) = NaiveDate::from_ymd_opt(now.year(), now.month(), now.day()) {
            self.imp().today_date.set(today);
        }
        self.repopulate_rows();
    }
}

// ── Chip widget ──

fn create_chip_widget(
    chip: &calendar::month_view::EventChip,
    month_view_weak: &glib::WeakRef<MonthView>,
    now: chrono::DateTime<chrono::FixedOffset>,
) -> gtk::Widget {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(4)
        .margin_start(2)
        .margin_end(2)
        .hexpand(true)
        .halign(gtk::Align::Fill)
        .css_classes(["monthview-chip"])
        .build();
    hbox.set_cursor_from_name(Some("pointer"));
    if chip.is_past_at(now) {
        hbox.add_css_class("past");
    }
    apply_chip_color(&hbox, &chip.color);

    let title = gtk::Label::builder()
        .label(&chip.title)
        .css_classes(["monthview-chip-title"])
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(12)
        .hexpand(true)
        .build();
    hbox.append(&title);

    if !chip.is_all_day
        && let Some(start_time) = chip.start_time
    {
        let time = gtk::Label::builder()
            .label(start_time.format("%R").to_string())
            .css_classes(["monthview-chip-time"])
            .halign(gtk::Align::End)
            .build();
        hbox.append(&time);
    }

    // Chip click → fire event-activation callback.  We capture the event_id
    // and a weak MonthView reference so the closure can access the callback
    // without owning the chip data past render.
    let event_id = chip.event_id;
    let mv_weak = month_view_weak.clone();
    let hbox_weak = hbox.downgrade();
    let gesture = gtk::GestureClick::new();
    gesture.set_button(gdk::BUTTON_PRIMARY);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    gesture.connect_pressed(|gesture, _n_press, _x, _y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
    });
    gesture.connect_released(move |gesture, _n_press, _x, _y| {
        gesture.set_state(gtk::EventSequenceState::Claimed);
        if let Some(mv) = mv_weak.upgrade()
            && let Some(hb) = hbox_weak.upgrade()
            && let Some(cb) = mv.imp().on_event_activate.borrow().as_ref()
        {
            cb(event_id, hb.upcast::<gtk::Widget>());
        }
    });
    hbox.add_controller(gesture);

    hbox.upcast()
}

fn apply_chip_color(chip: &gtk::Box, color: &str) {
    let color = sanitize_color(color);
    let css = format!(
        ".monthview-chip {{\
            border-color: color-mix(in srgb, #{color} 68%, var(--window-bg-color));\
            border-left-color: #{color};\
            background-color: color-mix(in srgb, #{color} 18%, var(--window-bg-color));\
        }}\
        .monthview-chip:hover {{\
            border-color: color-mix(in srgb, #{color} 68%, var(--window-bg-color));\
            border-left-color: #{color};\
            background-color: color-mix(in srgb, #{color} 32%, var(--window-bg-color));\
        }}"
    );
    let provider = gtk::CssProvider::new();
    provider.load_from_string(&css);
    // Keep the provider scoped to this chip rather than accumulating it on the display.
    #[allow(deprecated)]
    chip.style_context()
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

// ── A helper to hold a cloned DayProjection ──

#[derive(Debug, Clone)]
struct DayProjection {
    date: NaiveDate,
    #[allow(dead_code)]
    in_displayed_month: bool,
    all_day: Vec<calendar::month_view::EventChip>,
    timed: Vec<calendar::month_view::EventChip>,
}

/// Project events for all (year, month) pairs covered by the buffer and
/// return one DayProjection per unique date.
fn project_buffer_dates(
    buf: &WeeksBuffer,
    calendars: &[Calendar],
    events: &[Event],
) -> Vec<DayProjection> {
    let mut unique_dates: Vec<NaiveDate> = Vec::new();
    for row in 0..TOTAL_ROWS {
        for &date in &buf.row_dates(row) {
            if !unique_dates.contains(&date) {
                unique_dates.push(date);
            }
        }
    }

    let mut months: Vec<(i32, u32)> = Vec::new();
    for &date in &unique_dates {
        let ym = (date.year(), date.month());
        if !months.contains(&ym) {
            months.push(ym);
        }
    }

    let mut date_map: HashMap<NaiveDate, DayProjection> = HashMap::new();
    for &(year, month) in &months {
        let proj = project_month(year, month, calendars, events);
        for day in proj {
            if unique_dates.contains(&day.date) && !date_map.contains_key(&day.date) {
                date_map.insert(
                    day.date,
                    DayProjection {
                        date: day.date,
                        in_displayed_month: day.in_displayed_month,
                        all_day: day.all_day,
                        timed: day.timed,
                    },
                );
            }
        }
    }

    let mut result: Vec<DayProjection> = Vec::with_capacity(unique_dates.len());
    for date in &unique_dates {
        if let Some(proj) = date_map.remove(date) {
            result.push(proj);
        }
    }
    result
}

// ── Private helpers on the outer MonthView ──

impl MonthView {
    /// Handle a click on the day cell at (row, col).
    ///
    /// If the cell has no event chips, fire the activation callback
    /// immediately (first click → Quick Add).  Chips are handled
    /// separately by the per-chip gesture controller, which intercepts
    /// clicks at Capture phase before they reach this handler.
    fn handle_day_click(&self, row: usize, col: usize) {
        let imp = self.imp();
        let idx = row * 7 + col;
        let has_chips = imp
            .chip_boxes
            .borrow()
            .get(idx)
            .map(|chip_box| chip_box.first_child().is_some())
            .unwrap_or(false);

        if has_chips {
            return; // individual chip gestures handle event preview
        }

        // First click on an empty day → open Quick Add.
        let buf = imp.weeks_buffer.borrow();
        let date = buf.row_dates(row)[col];
        drop(buf);

        if let Some(cb) = imp.on_activate.borrow().as_ref() {
            cb(row, col, date);
        }
    }

    // ── Viewport-centre helpers ──

    /// The pixel position of the viewport centre in content space.
    fn viewport_centre_pixel(&self) -> f64 {
        let imp = self.imp();
        if !imp.initialized.get() {
            return VISIBLE_START as f64 * imp.row_height.get() + imp.row_height.get() * 2.5;
        }
        let adj = imp.week_scroll.vadjustment();
        adj.value() + adj.page_size() / 2.0
    }

    /// The (year, month) of the Thursday nearest the viewport centre.
    /// Thursday (col 3) is a stable mid-week reference for determining the
    /// human-friendly "current month".
    fn viewport_center_ym(&self) -> (i32, u32) {
        let imp = self.imp();
        let row_h = imp.row_height.get();
        if row_h <= 0.0 {
            let buf = imp.weeks_buffer.borrow();
            let d = buf.row_dates(VISIBLE_START + 2)[3];
            return (d.year(), d.month());
        }
        let px = self.viewport_centre_pixel();
        let row = ((px / row_h).floor() as usize).clamp(0, TOTAL_ROWS - 1);
        let buf = imp.weeks_buffer.borrow();
        let d = buf.row_dates(row)[3];
        (d.year(), d.month())
    }

    /// The (year, month) of the first **completely visible** row's Thursday
    /// (col 3), derived from the live scroll adjustment and row height so the
    /// title tracks actual viewport position rather than a fixed buffer index.
    ///
    /// During smooth/kinetic scrolling the top row may be partially clipped;
    /// in that case the next complete row is used.  Before the widget is
    /// initialised or when geometry is invalid, falls back to row
    /// `VISIBLE_START` — this ensures construction and setup report July for a
    /// July‑20 initial buffer rather than a hidden pre-buffer row.
    fn first_visible_week_ym(&self) -> (i32, u32) {
        let imp = self.imp();
        let row_h = imp.row_height.get();
        if !imp.initialized.get() || row_h <= 0.0 {
            let buf = imp.weeks_buffer.borrow();
            let d = buf.row_dates(VISIBLE_START)[3];
            return (d.year(), d.month());
        }
        let adj = imp.week_scroll.vadjustment();
        let val = adj.value();
        let row = ((val / row_h).ceil() as usize).clamp(0, TOTAL_ROWS - 1);
        let buf = imp.weeks_buffer.borrow();
        let d = buf.row_dates(row)[3];
        (d.year(), d.month())
    }

    /// The reference (year, month) used for `other-month` styling — derived
    /// from the viewport centre (separate from the title reference).
    fn ref_year_month(&self) -> (i32, u32) {
        self.viewport_center_ym()
    }

    // ── Navigation internals ──

    /// Reset the scroll position so the viewport shows the intended visible
    /// window (rows VISIBLE_START..VISIBLE_END).
    fn reset_scroll_position(&self) {
        let imp = self.imp();
        if !imp.initialized.get() {
            return;
        }
        let row_h = imp.row_height.get();
        if row_h <= 0.0 {
            return;
        }
        let adj = imp.week_scroll.vadjustment();
        imp.recycling_guard.set(true);
        adj.set_value(VISIBLE_START as f64 * row_h);
        imp.recycling_guard.set(false);
    }

    /// Fire the month-changed callback from the first visible week and update
    /// the cached title month.
    fn after_navigation(&self) {
        let (y, m) = self.first_visible_week_ym();
        let imp = self.imp();
        imp.last_title_ym.set((y, m));
        if !imp.active_date_syncing.get() {
            imp.active_date
                .set(reconcile_month(self.active_date(), y, m));
            if let Some(cb) = imp.on_month_changed.borrow().as_ref() {
                cb(y, m);
            }
        }
    }

    // ── Rendering internals ──

    /// Rebuild all 105 cells from the cached calendars/events and current
    /// WeeksBuffer.  Updates labels, chips, and CSS classes.
    fn repopulate_rows(&self) {
        let imp = self.imp();
        let calendars = imp.cached_calendars.borrow();
        let events = imp.cached_events.borrow();

        let month_view_weak = self.downgrade();

        let buf = imp.weeks_buffer.borrow();
        let all_projections = project_buffer_dates(&buf, &calendars, &events);
        let projection_map: HashMap<NaiveDate, &DayProjection> =
            all_projections.iter().map(|d| (d.date, d)).collect();
        let (dom_y, dom_m) = self.ref_year_month();
        let today = imp.today_date.get();
        let now = now_local_fixed();
        drop(buf);

        for chip_box in imp.chip_boxes.borrow().iter() {
            while let Some(child) = chip_box.first_child() {
                chip_box.remove(&child);
            }
        }

        let buttons = imp.day_buttons.borrow();
        let chip_boxes = imp.chip_boxes.borrow();

        for row in 0..TOTAL_ROWS {
            for col in 0..7 {
                let idx = row * 7 + col;
                let buf2 = imp.weeks_buffer.borrow();
                let date = buf2.row_dates(row)[col];
                drop(buf2);

                // Day label text and CSS classes
                if let Some(btn) = buttons.get(idx)
                    && let Some(child) = btn.child()
                    && let Ok(cell_box) = child.downcast::<gtk::Box>()
                    && let Some(first) = cell_box.first_child()
                    && let Ok(label) = first.downcast::<gtk::Label>()
                {
                    let day = date.day();
                    if day == 1 {
                        label.set_label(month_name(date.month()));
                        label.add_css_class("first-day");
                    } else {
                        label.set_label(&day.to_string());
                        label.remove_css_class("first-day");
                    }
                }

                // Chips
                if let Some(chip_box) = chip_boxes.get(idx)
                    && let Some(proj) = projection_map.get(&date)
                {
                    let chips: Vec<&calendar::month_view::EventChip> =
                        proj.all_day.iter().chain(proj.timed.iter()).collect();

                    let max_visible = 3;
                    for (ci, chip) in chips.iter().enumerate() {
                        if ci >= max_visible {
                            let overflow_count = chips.len() - max_visible;
                            let overflow_label = gtk::Label::builder()
                                .label(format!("+{}", overflow_count))
                                .css_classes(["monthview-overflow"])
                                .halign(gtk::Align::Start)
                                .margin_start(4)
                                .build();
                            chip_box.append(&overflow_label);
                            break;
                        }
                        let chip_widget = create_chip_widget(chip, &month_view_weak, now);
                        chip_box.append(&chip_widget);
                    }
                }

                // CSS classes on cell (button)
                if let Some(btn) = buttons.get(idx) {
                    let mut classes = vec!["monthview-cell", "flat"];
                    if date.year() != dom_y || date.month() != dom_m {
                        classes.push("other-month");
                    }
                    if date == today {
                        classes.push("today");
                    }
                    // Month-boundary separator classes (refinement 3).
                    let day = date.day();
                    if day == 1 {
                        classes.push("separator-top");
                        classes.push("separator-side");
                    } else if day <= 7 {
                        classes.push("separator-top");
                    }
                    btn.set_css_classes(&classes);
                }
            }
        }
    }

    /// Reapply CSS classes to all 105 buttons and their day labels without
    /// touching chips or day numbers.  Separator and first-day classes are
    /// recomputed so scrolling/recycling/title-month changes cannot strip them.
    fn refresh_cell_styles(&self) {
        let imp = self.imp();
        let buf = imp.weeks_buffer.borrow();
        let (dom_y, dom_m) = self.ref_year_month();
        let today = imp.today_date.get();
        let buttons = imp.day_buttons.borrow();

        for row in 0..TOTAL_ROWS {
            for col in 0..7 {
                let idx = row * 7 + col;
                let date = buf.row_dates(row)[col];
                if let Some(btn) = buttons.get(idx) {
                    let mut classes = vec!["monthview-cell", "flat"];
                    if date.year() != dom_y || date.month() != dom_m {
                        classes.push("other-month");
                    }
                    if date == today {
                        classes.push("today");
                    }
                    // Month-boundary separator classes (refinement 3).
                    let day = date.day();
                    if day == 1 {
                        classes.push("separator-top");
                        classes.push("separator-side");
                    } else if day <= 7 {
                        classes.push("separator-top");
                    }
                    btn.set_css_classes(&classes);

                    // First-day label class (refinement 3).
                    if let Some(child) = btn.child()
                        && let Ok(cell_box) = child.downcast::<gtk::Box>()
                        && let Some(first) = cell_box.first_child()
                        && let Ok(label) = first.downcast::<gtk::Label>()
                    {
                        if day == 1 {
                            label.add_css_class("first-day");
                        } else {
                            label.remove_css_class("first-day");
                        }
                    }
                }
            }
        }
    }

    // ── Scroll management ──

    /// One-shot initialisation from the viewport's allocated height.
    ///
    /// Once the scrolled window has a valid allocation, this computes the row
    /// height (1/5 of the viewport), explicitly sizes each of the 15 rows,
    /// sets the content extent, and establishes every adjustment parameter
    /// (lower, upper, page-size, step/page-increments, and value) atomically
    /// from the known geometry — no waiting for GTK to infer the range.
    ///
    /// The recycling guard prevents the value-changed → check_recycle path
    /// from firing during setup.  After marking `initialized`, continuous
    /// scrolling and row recycling work as normal.  If GTK later recomputes
    /// the adjustment from the explicit content geometry it converges to the
    /// same bounds and value.
    ///
    /// Runs exactly once — the `initialized` guard returns early thereafter.
    fn setup_scroll(&self) {
        let imp = self.imp();
        if imp.initialized.get() {
            return;
        }

        let h = imp.week_scroll.height() as f64;
        if h <= 0.0 {
            return;
        }

        imp.recycling_guard.set(true);

        let row_pixels = (h as i32) / 5;
        if row_pixels <= 0 {
            imp.recycling_guard.set(false);
            return;
        }
        let r = f64::from(row_pixels);
        imp.row_height.set(r);

        // Use one whole-pixel height consistently for sizing and scrolling.
        for row_box in imp.row_boxes.borrow().iter() {
            row_box.set_height_request(row_pixels);
        }

        // The outer content box gets the full 15-row extent.
        let total = TOTAL_ROWS as i32 * row_pixels;
        imp.week_rows_box.set_height_request(total);

        // Establish adjustment bounds/increments/value atomically from
        // known geometry so the viewport opens at VISIBLE_START.
        let adj = imp.week_scroll.vadjustment();
        adj.configure(VISIBLE_START as f64 * r, 0.0, total as f64, r, h, h);

        imp.recycling_guard.set(false);
        imp.initialized.set(true);

        let (y, m) = self.first_visible_week_ym();
        imp.last_title_ym.set((y, m));
        if let Some(cb) = imp.on_month_changed.borrow().as_ref() {
            cb(y, m);
        }
        self.refresh_cell_styles();
    }

    /// Called on every adjustment `value-changed` signal.  Reports
    /// centre-month transitions (even without recycling) and recycles buffer
    /// rows when the scroll position nears an edge.
    fn check_recycle(&self) {
        let imp = self.imp();
        if imp.recycling_guard.get() {
            return;
        }
        if !imp.initialized.get() {
            return;
        }

        let row_h = imp.row_height.get();
        if row_h <= 0.0 {
            return;
        }

        let adj = imp.week_scroll.vadjustment();
        let val = adj.value();
        let max_val = adj.upper() - adj.page_size();

        // Title-month transition (fires even without recycling) — uses the
        // first visible complete week, not the viewport centre.
        let new_ym = self.first_visible_week_ym();
        let old_ym = imp.last_title_ym.get();
        if new_ym != old_ym {
            imp.last_title_ym.set(new_ym);
            imp.active_date
                .set(reconcile_month(self.active_date(), new_ym.0, new_ym.1));
            if let Some(cb) = imp.on_month_changed.borrow().as_ref() {
                cb(new_ym.0, new_ym.1);
            }
        }

        // Row recycling.
        let need_top = val < row_h * 0.5;
        let need_bot = val > max_val - row_h * 0.5;

        if need_top {
            imp.recycling_guard.set(true);
            {
                let mut buf = imp.weeks_buffer.borrow_mut();
                buf.shift_weeks(-1);
            }
            adj.set_value(val + row_h);
            imp.recycling_guard.set(false);
            self.repopulate_rows();
        } else if need_bot {
            imp.recycling_guard.set(true);
            {
                let mut buf = imp.weeks_buffer.borrow_mut();
                buf.shift_weeks(1);
            }
            adj.set_value(val - row_h);
            imp.recycling_guard.set(false);
            self.repopulate_rows();
        }
    }
}

/// Find the Monday of the ISO week containing `date`.
fn monday_of_week(date: NaiveDate) -> NaiveDate {
    let weekday_num = date.weekday().num_days_from_monday(); // 0 = Mon, 6 = Sun
    date - chrono::Duration::days(weekday_num as i64)
}

/// Full English month name for the given month number (1=January).
fn month_name(month: u32) -> &'static str {
    match month {
        1 => "January",
        2 => "February",
        3 => "March",
        4 => "April",
        5 => "May",
        6 => "June",
        7 => "July",
        8 => "August",
        9 => "September",
        10 => "October",
        11 => "November",
        12 => "December",
        _ => unreachable!(),
    }
}

fn shift_month(date: NaiveDate, direction: i64) -> NaiveDate {
    let month_index = i64::from(date.year()) * 12 + i64::from(date.month0()) + direction;
    let year = i32::try_from(month_index.div_euclid(12)).expect("month navigation exceeded range");
    let month = month_index.rem_euclid(12) as u32 + 1;
    let day = date.day().min(days_in_month(year, month));
    NaiveDate::from_ymd_opt(year, month, day).expect("month navigation produced invalid date")
}

fn reconcile_month(date: NaiveDate, year: i32, month: u32) -> NaiveDate {
    let day = date.day().min(days_in_month(year, month));
    NaiveDate::from_ymd_opt(year, month, day).expect("month reconciliation produced invalid date")
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let first_of_next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("month navigation exceeded range");
    (first_of_next - chrono::Duration::days(1)).day()
}
