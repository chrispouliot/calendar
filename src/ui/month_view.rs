use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, Event};
use calendar::month_view::project_month;
use calendar::weeks_buffer::{TOTAL_ROWS, VISIBLE_START, WeeksBuffer};
use chrono::{Datelike, NaiveDate};
use gtk::glib;

/// Callback type for day activation (re-click on already-selected day).
type ActivateFn = Box<dyn Fn(NaiveDate)>;

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

        /// Selected date — `None` means nothing selected.
        pub selected_date: Cell<Option<NaiveDate>>,

        /// Today (read from clock at construction).
        pub today_date: Cell<NaiveDate>,

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

        /// Callback fired when dominant month changes (for title update).
        pub on_month_changed: RefCell<Option<MonthChangedFn>>,

        /// Callback fired when an already-selected empty day is re-clicked.
        pub on_activate: RefCell<Option<ActivateFn>>,
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
                selected_date: Cell::new(None),
                today_date: Cell::new(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap()),
                last_title_ym: Cell::new((2026, 1)),
                cached_calendars: RefCell::new(Vec::new()),
                cached_events: RefCell::new(Vec::new()),
                day_buttons: RefCell::new(Vec::new()),
                chip_boxes: RefCell::new(Vec::new()),
                row_boxes: RefCell::new(Vec::new()),
                row_height: Cell::new(80.0),
                recycling_guard: Cell::new(false),
                initialized: Cell::new(false),
                on_month_changed: RefCell::new(None),
                on_activate: RefCell::new(None),
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
            let now = glib::DateTime::now_local().unwrap();
            let today =
                NaiveDate::from_ymd_opt(now.year(), now.month() as u32, now.day_of_month() as u32)
                    .expect("valid today");
            self.today_date.set(today);

            // Position so today's week is the first visible week (refinement 2).
            let buf = WeeksBuffer::new(monday_of_week(today));
            *self.weeks_buffer.borrow_mut() = buf;

            // No day selected initially.
            self.selected_date.set(None);

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

            // Run once outside GTK's active allocation pass. setup_scroll
            // establishes explicit row geometry and the complete adjustment,
            // so no retry or range polling is needed.
            let obj_weak5 = obj_weak.clone();
            self.week_scroll.connect_realize(move |_| {
                let obj_weak5 = obj_weak5.clone();
                glib::source::idle_add_local_once(move || {
                    if let Some(obj) = obj_weak5.upgrade() {
                        obj.setup_scroll();
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
    }

    impl WidgetImpl for MonthView {}
    impl BinImpl for MonthView {}
}

glib::wrapper! {
    pub struct MonthView(ObjectSubclass<imp::MonthView>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget;
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

    /// Currently selected date, if any.
    pub fn selected_date(&self) -> Option<NaiveDate> {
        self.imp().selected_date.get()
    }

    /// Register a callback invoked when an already-selected day is re-clicked
    /// (the quick-add placeholder activation path).
    pub fn set_on_activate<F: Fn(NaiveDate) + 'static>(&self, f: F) {
        *self.imp().on_activate.borrow_mut() = Some(Box::new(f));
    }

    /// Register a callback invoked when the viewport centre month changes
    /// (title update).
    pub fn set_on_month_changed<F: Fn(i32, u32) + 'static>(&self, f: F) {
        *self.imp().on_month_changed.borrow_mut() = Some(Box::new(f));
    }

    // ── Navigation (targets calendar months, clears selection) ──

    /// Navigate to the previous calendar month. The Monday on/before that
    /// month's 1st becomes the first visible row's Monday.  Selection is
    /// cleared and the viewport resets to its centred position.
    pub fn navigate_previous(&self) {
        let (y, m) = self.viewport_center_ym();
        let (py, pm) = if m == 1 { (y - 1, 12u32) } else { (y, m - 1) };
        self.set_buffer_to_month(py, pm);
    }

    /// Navigate to the next calendar month.  See `navigate_previous`.
    pub fn navigate_next(&self) {
        let (y, m) = self.viewport_center_ym();
        let (ny, nm) = if m == 12 { (y + 1, 1u32) } else { (y, m + 1) };
        self.set_buffer_to_month(ny, nm);
    }

    /// Jump to today.  Today's week becomes the first visible week (refinement 2).
    /// Selection is cleared.
    pub fn go_today(&self) {
        let imp = self.imp();
        let today = imp.today_date.get();
        let buf = WeeksBuffer::new(monday_of_week(today));
        *imp.weeks_buffer.borrow_mut() = buf;
        imp.selected_date.set(None);
        self.reset_scroll_position();
        self.after_navigation();
        self.repopulate_rows();
    }

    // ── Rendering ──

    /// Store fresh calendars/events and fully repopulate all 105 cells.
    pub fn render(&self, calendars: &[Calendar], events: &[Event]) {
        let imp = self.imp();
        *imp.cached_calendars.borrow_mut() = calendars.to_vec();
        *imp.cached_events.borrow_mut() = events.to_vec();
        self.repopulate_rows();
    }
}

// ── Chip widget ──

fn create_chip_widget(chip: &calendar::month_view::EventChip) -> gtk::Widget {
    let hbox = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .spacing(3)
        .margin_start(2)
        .halign(gtk::Align::Start)
        .css_classes(["monthview-chip"])
        .build();

    let dot = gtk::Label::builder()
        .label("\u{25CF}")
        .css_classes(["monthview-chip-dot"])
        .use_markup(true)
        .build();
    let sanitized_color = chip.color.replace('#', "");
    dot.set_markup(&format!(
        "<span foreground=\"#{}\">\u{25CF}</span>",
        sanitized_color
    ));
    hbox.append(&dot);

    let title = gtk::Label::builder()
        .label(&chip.title)
        .css_classes(["monthview-chip-title"])
        .halign(gtk::Align::Start)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .max_width_chars(12)
        .build();
    hbox.append(&title);

    hbox.upcast()
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
    fn handle_day_click(&self, row: usize, col: usize) {
        let imp = self.imp();
        let buf = imp.weeks_buffer.borrow();
        let date = buf.row_dates(row)[col];
        drop(buf);

        let sel = imp.selected_date.get();
        let is_selected = sel == Some(date);

        if is_selected {
            let idx = row * 7 + col;
            let is_empty = imp
                .chip_boxes
                .borrow()
                .get(idx)
                .is_none_or(|chip_box| chip_box.first_child().is_none());

            if is_empty {
                if let Some(cb) = imp.on_activate.borrow().as_ref() {
                    cb(date);
                }
                return;
            }
        }

        imp.selected_date.set(Some(date));
        self.refresh_cell_styles();
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

    /// Set the WeeksBuffer so the Monday on/before the given month's 1st is
    /// the first visible row's Monday.  Selection cleared, scroll reset,
    /// title callback fired.
    fn set_buffer_to_month(&self, year: i32, month: u32) {
        let imp = self.imp();
        let first_of = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month target");
        let first_visible = monday_of_week(first_of);
        *imp.weeks_buffer.borrow_mut() = WeeksBuffer::new(first_visible);
        imp.selected_date.set(None);
        self.reset_scroll_position();
        self.after_navigation();
        self.repopulate_rows();
    }

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
        if let Some(cb) = imp.on_month_changed.borrow().as_ref() {
            cb(y, m);
        }
    }

    // ── Rendering internals ──

    /// Rebuild all 105 cells from the cached calendars/events and current
    /// WeeksBuffer.  Updates labels, chips, and CSS classes.
    fn repopulate_rows(&self) {
        let imp = self.imp();
        let calendars = imp.cached_calendars.borrow();
        let events = imp.cached_events.borrow();

        let buf = imp.weeks_buffer.borrow();
        let all_projections = project_buffer_dates(&buf, &calendars, &events);
        let projection_map: HashMap<NaiveDate, &DayProjection> =
            all_projections.iter().map(|d| (d.date, d)).collect();
        let (dom_y, dom_m) = self.ref_year_month();
        let today = imp.today_date.get();
        let sel = imp.selected_date.get();
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
                        let chip_widget = create_chip_widget(chip);
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
                    if Some(date) == sel {
                        classes.push("selected");
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
    /// recomputed so scrolling/recycling/selection/title-month changes cannot
    /// strip them.
    fn refresh_cell_styles(&self) {
        let imp = self.imp();
        let buf = imp.weeks_buffer.borrow();
        let (dom_y, dom_m) = self.ref_year_month();
        let today = imp.today_date.get();
        let sel = imp.selected_date.get();
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
                    if Some(date) == sel {
                        classes.push("selected");
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
