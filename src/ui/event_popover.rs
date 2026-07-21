use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, Event, EventSchedule};
use chrono::{Datelike, NaiveDate, Timelike};
use gtk::glib;

/// Edit-Details callback: invoked with the persisted event ID when the user
/// presses the action button.
type EditDetailsFn = Box<dyn Fn(uuid::Uuid)>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/event-popover.ui")]
    pub struct EventPopover {
        #[template_child]
        pub summary_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub date_time_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub description_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub description_scrolled_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub placeholder_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub location_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub location_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub action_button: TemplateChild<gtk::Button>,

        pub on_edit_details: RefCell<Option<EditDetailsFn>>,
        pub event_id: RefCell<Option<uuid::Uuid>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EventPopover {
        const NAME: &'static str = "EventPopover";
        const ABSTRACT: bool = false;
        type Type = super::EventPopover;
        type ParentType = gtk::Popover;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[gtk::template_callbacks]
    impl EventPopover {
        #[template_callback]
        fn on_action_button_clicked(&self) {
            if let Some(id) = *self.event_id.borrow()
                && let Some(cb) = self.on_edit_details.borrow().as_ref()
            {
                cb(id);
            }
            self.obj().popdown();
        }
    }

    impl ObjectImpl for EventPopover {
        fn constructed(&self) {
            self.parent_constructed();
        }
    }

    impl WidgetImpl for EventPopover {
        fn map(&self) {
            self.parent_map();
            self.action_button.grab_focus();
            if let Some(root) = self.obj().root()
                && let Ok(window) = root.downcast::<gtk::Window>()
            {
                window.set_focus_visible(false);
            }
        }
    }
    impl PopoverImpl for EventPopover {}
}

glib::wrapper! {
    pub struct EventPopover(ObjectSubclass<imp::EventPopover>)
        @extends gtk::Popover, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::ShortcutManager;
}

impl Default for EventPopover {
    fn default() -> Self {
        Self::new()
    }
}

impl EventPopover {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Register the Edit Details callback.
    pub fn set_on_edit_details<F: Fn(uuid::Uuid) + 'static>(&self, f: F) {
        *self.imp().on_edit_details.borrow_mut() = Some(Box::new(f));
    }

    /// Populate the popover with an event's data and open it.
    ///
    /// `today` is the current date used for relative date labels
    /// (Today / Tomorrow / Yesterday).  `calendar` provides the
    /// read-only flag to select the correct action icon and tooltip.
    pub fn set_event(&self, event: &Event, calendar: Option<&Calendar>, today: NaiveDate) {
        let imp = self.imp();
        *imp.event_id.borrow_mut() = Some(event.id);

        imp.summary_label.set_label(&event.title);

        let schedule_str = format_schedule(&event.schedule, today);
        imp.date_time_label.set_label(&schedule_str);

        // Description
        let desc = event.description.trim();
        if desc.is_empty() {
            imp.description_scrolled_window.set_visible(false);
            imp.placeholder_label.set_visible(true);
        } else {
            imp.description_label.set_label(desc);
            imp.description_scrolled_window.set_visible(true);
            imp.placeholder_label.set_visible(false);
        }

        // Location
        let loc = event.location.trim();
        if loc.is_empty() {
            imp.location_box.set_visible(false);
        } else {
            imp.location_label.set_label(loc);
            imp.location_box.set_visible(true);
        }

        // Action button: read-only → view-only icon/tooltip.
        let is_read_only = calendar.map(|c| c.read_only).unwrap_or(false);
        if is_read_only {
            imp.action_button
                .set_icon_name("dialog-information-symbolic");
            imp.action_button.set_tooltip_text(Some("View Details"));
        } else {
            imp.action_button.set_icon_name("document-edit-symbolic");
            imp.action_button.set_tooltip_text(Some("Edit Details"));
        }
    }
}

// ── Schedule formatting (display-only, no EDS/time-format deps) ──

fn format_schedule(schedule: &EventSchedule, today: NaiveDate) -> String {
    match schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => {
            // Exclusive end → display end is one day prior.
            let end_display = end_date_exclusive.pred_opt().unwrap_or(*end_date_exclusive);
            if *start_date == end_display {
                // Single all-day day.
                format_relative_date(*start_date, today)
            } else {
                // Multi-day all-day.
                format!(
                    "{} \u{2013} {}",
                    format_relative_date(*start_date, today),
                    format_relative_date(end_display, today)
                )
            }
        }
        EventSchedule::Timed {
            start,
            end,
            timezone: _,
        } => {
            let start_naive = start.date_naive();
            let end_naive = end.date_naive();
            let start_time = format!("{:02}:{:02}", start.hour(), start.minute());
            let end_time = format!("{:02}:{:02}", end.hour(), end.minute());

            if start_naive == end_naive {
                // Same-day timed.
                format!(
                    "{}, {} \u{2013} {}",
                    format_relative_date(start_naive, today),
                    start_time,
                    end_time
                )
            } else {
                // Multi-day timed.
                format!(
                    "{}, {} \u{2013} {}, {}",
                    format_relative_date(start_naive, today),
                    start_time,
                    format_relative_date(end_naive, today),
                    end_time
                )
            }
        }
    }
}

fn format_relative_date(date: NaiveDate, today: NaiveDate) -> String {
    let diff = (date - today).num_days();
    let month = month_name(date.month());
    let same_year = date.year() == today.year();

    match diff {
        0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        -1 => "Yesterday".to_string(),
        _ if same_year => format!("{} {}", month, date.day()),
        _ => format!("{} {}, {}", month, date.day(), date.year()),
    }
}

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
