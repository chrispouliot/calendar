use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::backend::{CalendarRepository, EventRepository, InMemoryRepository};
use calendar::model::{Calendar, CalendarSource, Event, EventSchedule};
use chrono::NaiveDate;
use gtk::{gio, glib};
use std::cell::RefCell;
use uuid::Uuid;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/window.ui")]
    pub struct CalendarWindow {
        #[template_child]
        pub overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub views_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub date_chooser_bin: TemplateChild<adw::Bin>,

        // Phase 5: Month view and navigation title.
        #[template_child]
        pub month_view_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,

        // In-memory repository with seeded test data.
        pub repository: RefCell<InMemoryRepository>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarWindow {
        const NAME: &'static str = "CalendarWindow";
        type Type = super::CalendarWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CalendarWindow {
        fn constructed(&self) {
            self.parent_constructed();

            // ── Sidebar date chooser ──
            let chooser = crate::ui::date_chooser::DateChooser::new();
            self.date_chooser_bin.set_child(Some(&chooser));

            // ── Seed in-memory repository with temporary test data ──
            //
            // TODO(Phase 6): Remove this seed data when real event creation
            // is wired up. This exists solely so event-chip styling can be
            // inspected during Phase 5 development.
            self.seed_repository();

            // ── Create and place MonthView ──
            let month_view = crate::ui::month_view::MonthView::new();
            let win = self.obj();
            let imp = self;
            let win_weak = win.downgrade();

            // Connect the quick-add activation placeholder: re-clicking an
            // already-selected empty day fires this callback.
            month_view.set_on_activate({
                let win_weak = win_weak.clone();
                move |date| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp()
                            .overlay
                            .add_toast(adw::Toast::new(&format!("New event on {date}")));
                    }
                }
            });

            // Connect the month-changed callback for title updates (fired when
            // scrolling changes which month is dominant among visible weeks).
            month_view.set_on_month_changed({
                let win_weak = win_weak.clone();
                move |y, m| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp().update_title(y, m);
                    }
                }
            });

            imp.month_view_bin.set_child(Some(&month_view));

            // ── Window actions ──

            let previous_date = gio::SimpleAction::new("previous-date", None);
            let win_weak = win.downgrade();
            previous_date.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.navigate_previous();
                }
            });
            win.add_action(&previous_date);

            let next_date = gio::SimpleAction::new("next-date", None);
            let win_weak = win.downgrade();
            next_date.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.navigate_next();
                }
            });
            win.add_action(&next_date);

            let today = gio::SimpleAction::new("today", None);
            let win_weak = win.downgrade();
            today.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.navigate_today();
                }
            });
            win.add_action(&today);

            let new_event = gio::SimpleAction::new("new-event", None);
            let win_weak = win.downgrade();
            new_event.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    let msg = win
                        .imp()
                        .with_month_view(|mv| mv.selected_date())
                        .flatten()
                        .map(|d| format!("New event on {d}"))
                        .unwrap_or_else(|| "New event".to_string());
                    win.imp().overlay.add_toast(adw::Toast::new(&msg));
                }
            });
            win.add_action(&new_event);

            // ── Initial render: jump to today ──
            self.render_all_from_today();
        }
    }

    impl WidgetImpl for CalendarWindow {}
    impl WindowImpl for CalendarWindow {}
    impl ApplicationWindowImpl for CalendarWindow {}
    impl AdwApplicationWindowImpl for CalendarWindow {}
}

glib::wrapper! {
    pub struct CalendarWindow(ObjectSubclass<imp::CalendarWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                   gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

// ── Public API (window callbacks from actions) ──

impl CalendarWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder()
            .property("application", Some(app))
            .build()
    }

    fn navigate_previous(&self) {
        let imp = self.imp();
        imp.with_month_view(|mv| mv.navigate_previous());
        imp.render_month_view();
    }

    fn navigate_next(&self) {
        let imp = self.imp();
        imp.with_month_view(|mv| mv.navigate_next());
        imp.render_month_view();
    }

    fn navigate_today(&self) {
        let imp = self.imp();
        imp.with_month_view(|mv| mv.go_today());
        imp.render_month_view();
    }
}

// ── Private helpers ──

impl imp::CalendarWindow {
    /// Borrow the MonthView widget inside `month_view_bin` and call `f` on it.
    fn with_month_view<R>(
        &self,
        f: impl FnOnce(&crate::ui::month_view::MonthView) -> R,
    ) -> Option<R> {
        let child = self.month_view_bin.child()?;
        let mv = child.downcast::<crate::ui::month_view::MonthView>().ok()?;
        Some(f(&mv))
    }

    /// Load calendars + events from the repository and tell the MonthView to
    /// re-render.  Updates the navigation title afterwards.
    fn render_month_view(&self) {
        let repo = self.repository.borrow();
        let calendars = repo.list_calendars();
        let all_events: Vec<Event> = calendars
            .iter()
            .flat_map(|c| repo.list_events_for_calendar(c.id))
            .collect();
        drop(repo);

        self.with_month_view(|mv| mv.render(&calendars, &all_events));

        // Update the navigation title from the MonthView's dominant month.
        self.with_month_view(|mv| {
            let (y, m) = mv.dominant_year_month();
            self.update_title(y, m);
        });
    }

    /// Set the navigation title to "Month Year".
    fn update_title(&self, year: i32, month: u32) {
        let month_name = match month {
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
        };
        self.title_label
            .set_label(&format!("{} {}", month_name, year));
    }

    /// Jump to today and render.
    fn render_all_from_today(&self) {
        self.with_month_view(|mv| mv.go_today());
        self.render_month_view();
    }

    /// Seed the repository with temporary test data.
    ///
    /// Temporary — remove when Phase 6 adds real event creation.
    fn seed_repository(&self) {
        let mut repo = self.repository.borrow_mut();

        // Calendars: two visible, one hidden.
        let cal_id = Uuid::parse_str("e1111111-e111-1111-1111-111111111111").unwrap();
        let _ = repo.save_calendar(&Calendar {
            id: cal_id,
            name: "Personal".to_string(),
            color: "#3366cc".to_string(),
            visible: true,
            read_only: false,
            source: CalendarSource::Local,
        });

        let work_cal_id = Uuid::parse_str("e2222222-e222-2222-2222-222222222222").unwrap();
        let _ = repo.save_calendar(&Calendar {
            id: work_cal_id,
            name: "Work".to_string(),
            color: "#cc3333".to_string(),
            visible: true,
            read_only: false,
            source: CalendarSource::Local,
        });

        let hidden_cal_id = Uuid::parse_str("e3333333-e333-3333-3333-333333333333").unwrap();
        let _ = repo.save_calendar(&Calendar {
            id: hidden_cal_id,
            name: "Hidden".to_string(),
            color: "#999999".to_string(),
            visible: false,
            read_only: false,
            source: CalendarSource::Local,
        });

        // Determine today's date (fallback used for deterministic builds).
        let now = glib::DateTime::now_local().unwrap();
        let today =
            NaiveDate::from_ymd_opt(now.year(), now.month() as u32, now.day_of_month() as u32)
                .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());

        // All-day event on today.
        let _ = repo.save_event(&Event {
            id: Uuid::parse_str("f1111111-f111-1111-1111-111111111111").unwrap(),
            calendar_id: cal_id,
            title: "Test Event".to_string(),
            location: String::new(),
            description: String::new(),
            schedule: EventSchedule::AllDay {
                start_date: today,
                end_date_exclusive: today
                    .succ_opt()
                    .unwrap_or_else(|| today + chrono::Duration::days(1)),
            },
            recurrence: None,
            reminders: Vec::new(),
        });

        // All-day event on the same day in the Work calendar (multiple chips).
        let _ = repo.save_event(&Event {
            id: Uuid::parse_str("f2222222-f222-2222-2222-222222222222").unwrap(),
            calendar_id: work_cal_id,
            title: "Standup".to_string(),
            location: String::new(),
            description: String::new(),
            schedule: EventSchedule::AllDay {
                start_date: today,
                end_date_exclusive: today
                    .succ_opt()
                    .unwrap_or_else(|| today + chrono::Duration::days(1)),
            },
            recurrence: None,
            reminders: Vec::new(),
        });

        // Timed event three days from now.
        let three_days = today + chrono::Duration::days(3);
        let tz_utc = chrono::FixedOffset::east_opt(0).unwrap();
        let _ = repo.save_event(&Event {
            id: Uuid::parse_str("f3333333-f333-3333-3333-333333333333").unwrap(),
            calendar_id: cal_id,
            title: "Lunch Meeting".to_string(),
            location: String::new(),
            description: String::new(),
            schedule: EventSchedule::Timed {
                start: three_days
                    .and_hms_opt(12, 0, 0)
                    .unwrap()
                    .and_local_timezone(tz_utc)
                    .single()
                    .unwrap(),
                end: three_days
                    .and_hms_opt(13, 0, 0)
                    .unwrap()
                    .and_local_timezone(tz_utc)
                    .single()
                    .unwrap(),
                timezone: None,
            },
            recurrence: None,
            reminders: Vec::new(),
        });

        // Multi-day all-day event spanning days 2–5 from today.
        let two_days = today + chrono::Duration::days(2);
        let five_days = today + chrono::Duration::days(5);
        let _ = repo.save_event(&Event {
            id: Uuid::parse_str("f4444444-f444-4444-4444-444444444444").unwrap(),
            calendar_id: cal_id,
            title: "Multi-day Trip".to_string(),
            location: String::new(),
            description: String::new(),
            schedule: EventSchedule::AllDay {
                start_date: two_days,
                end_date_exclusive: five_days,
            },
            recurrence: None,
            reminders: Vec::new(),
        });
    }
}
