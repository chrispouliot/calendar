use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, Event, EventSchedule, validate_event};
use chrono::{
    DateTime, Datelike, Duration, FixedOffset, NaiveDate, NaiveDateTime, TimeZone, Timelike,
};
use gtk::glib;
use uuid::Uuid;

type SaveFn = Box<dyn Fn(Event, bool) -> bool>;

#[derive(Clone)]
pub struct OriginalTimedEvent {
    start: DateTime<FixedOffset>,
    end: DateTime<FixedOffset>,
    timezone: Option<String>,
}

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/event-editor.ui")]
    pub struct EventEditor {
        #[template_child]
        pub title_entry: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub location_entry: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub calendar_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub schedule_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub start_date_row: TemplateChild<crate::ui::date_chooser_row::DateChooserRow>,
        #[template_child]
        pub end_date_row: TemplateChild<crate::ui::date_chooser_row::DateChooserRow>,
        #[template_child]
        pub start_date_time: TemplateChild<crate::ui::date_time_chooser::DateTimeChooser>,
        #[template_child]
        pub end_date_time: TemplateChild<crate::ui::date_time_chooser::DateTimeChooser>,
        #[template_child]
        pub description_view: TemplateChild<gtk::TextView>,
        #[template_child]
        pub error_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub cancel_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub save_button: TemplateChild<gtk::Button>,

        pub calendars: RefCell<Vec<Calendar>>,
        pub editing_event: RefCell<Option<Event>>,
        pub original_timed_event: RefCell<Option<OriginalTimedEvent>>,
        pub start_date_row_state: RefCell<Option<crate::ui::date_chooser_row::DateChooserRow>>,
        pub end_date_row_state: RefCell<Option<crate::ui::date_chooser_row::DateChooserRow>>,
        pub start_date_time_state: RefCell<Option<crate::ui::date_time_chooser::DateTimeChooser>>,
        pub end_date_time_state: RefCell<Option<crate::ui::date_time_chooser::DateTimeChooser>>,
        pub on_save: RefCell<Option<SaveFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EventEditor {
        const NAME: &'static str = "EventEditor";
        type Type = super::EventEditor;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            crate::ui::date_chooser_row::DateChooserRow::static_type();
            crate::ui::date_time_chooser::DateTimeChooser::static_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for EventEditor {
        fn constructed(&self) {
            self.parent_constructed();

            let start_date_row = self.start_date_row.get();
            let end_date_row = self.end_date_row.get();
            let start_date_time = self.start_date_time.get();
            let end_date_time = self.end_date_time.get();
            start_date_time.set_labels("Start Date", "Start Time");
            end_date_time.set_labels("End Date", "End Time");
            *self.start_date_row_state.borrow_mut() = Some(start_date_row.clone());
            *self.end_date_row_state.borrow_mut() = Some(end_date_row.clone());
            *self.start_date_time_state.borrow_mut() = Some(start_date_time.clone());
            *self.end_date_time_state.borrow_mut() = Some(end_date_time.clone());

            let weak = self.obj().downgrade();
            start_date_row.set_on_date_changed(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.imp().schedule_changed();
                }
            });
            let weak = self.obj().downgrade();
            end_date_row.set_on_date_changed(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.imp().schedule_changed();
                }
            });
            let weak = self.obj().downgrade();
            start_date_time.set_on_changed(move || {
                if let Some(editor) = weak.upgrade() {
                    editor.imp().schedule_changed();
                }
            });
            let weak = self.obj().downgrade();
            end_date_time.set_on_changed(move || {
                if let Some(editor) = weak.upgrade() {
                    editor.imp().schedule_changed();
                }
            });

            let weak = self.obj().downgrade();
            self.schedule_stack
                .connect_visible_child_name_notify(move |_| {
                    if let Some(editor) = weak.upgrade() {
                        editor.imp().schedule_changed();
                    }
                });

            let weak = self.obj().downgrade();
            self.save_button.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    editor.imp().save_clicked();
                }
            });

            let weak = self.obj().downgrade();
            self.cancel_button.connect_clicked(move |_| {
                if let Some(editor) = weak.upgrade() {
                    adw::prelude::AdwDialogExt::close(&editor);
                }
            });
        }
    }

    impl WidgetImpl for EventEditor {}
    impl AdwDialogImpl for EventEditor {}
}

glib::wrapper! {
    pub struct EventEditor(ObjectSubclass<imp::EventEditor>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl Default for EventEditor {
    fn default() -> Self {
        Self::new()
    }
}

impl EventEditor {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_on_save<F: Fn(Event, bool) -> bool + 'static>(&self, callback: F) {
        *self.imp().on_save.borrow_mut() = Some(Box::new(callback));
    }

    /// Set writable repository calendars. The list is deliberately rebuilt on
    /// every open so stale or read-only choices cannot be submitted.
    pub fn set_calendars(&self, calendars: &[Calendar]) {
        let mut writable: Vec<Calendar> = calendars
            .iter()
            .filter(|calendar| !calendar.read_only)
            .cloned()
            .collect();
        writable.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        let names: Vec<&str> = writable
            .iter()
            .map(|calendar| calendar.name.as_str())
            .collect();
        let model = gtk::StringList::new(&names);
        self.imp().calendar_row.set_model(Some(&model));
        *self.imp().calendars.borrow_mut() = writable;
        self.imp().calendar_row.set_selected(0);
    }

    pub fn set_create_defaults(&self, title: &str, calendar_id: Uuid, date: NaiveDate) {
        let imp = self.imp();
        *imp.editing_event.borrow_mut() = None;
        imp.title_entry.set_text(title);
        imp.location_entry.set_text("");
        imp.description_view.buffer().set_text("");
        *imp.original_timed_event.borrow_mut() = None;
        select_calendar(&imp.calendar_row, &imp.calendars.borrow(), calendar_id);
        imp.schedule_stack.set_visible_child_name("all-day");
        if let (
            Some(start_date_row),
            Some(end_date_row),
            Some(start_date_time),
            Some(end_date_time),
        ) = (
            imp.start_date_row_state.borrow().as_ref(),
            imp.end_date_row_state.borrow().as_ref(),
            imp.start_date_time_state.borrow().as_ref(),
            imp.end_date_time_state.borrow().as_ref(),
        ) {
            start_date_row.set_date(date);
            end_date_row.set_date(date);
            start_date_time.set_date_time(date, 9, 0);
            end_date_time.set_date_time(date, 10, 0);
        }
        imp.clear_error();
        imp.ensure_forward_range();
    }

    pub fn set_event(&self, event: &Event) {
        let imp = self.imp();
        *imp.editing_event.borrow_mut() = Some(event.clone());
        imp.title_entry.set_text(&event.title);
        imp.location_entry.set_text(&event.location);
        imp.description_view.buffer().set_text(&event.description);
        select_calendar(
            &imp.calendar_row,
            &imp.calendars.borrow(),
            event.calendar_id,
        );

        match &event.schedule {
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            } => {
                *imp.original_timed_event.borrow_mut() = None;
                imp.schedule_stack.set_visible_child_name("all-day");
                if let (Some(start_date_row), Some(end_date_row)) = (
                    imp.start_date_row_state.borrow().as_ref(),
                    imp.end_date_row_state.borrow().as_ref(),
                ) {
                    start_date_row.set_date(*start_date);
                    end_date_row
                        .set_date(end_date_exclusive.pred_opt().unwrap_or(*end_date_exclusive));
                }
                if let (Some(start_date_time), Some(end_date_time)) = (
                    imp.start_date_time_state.borrow().as_ref(),
                    imp.end_date_time_state.borrow().as_ref(),
                ) {
                    start_date_time.set_date_time(*start_date, 9, 0);
                    end_date_time.set_date_time(
                        end_date_exclusive.pred_opt().unwrap_or(*end_date_exclusive),
                        10,
                        0,
                    );
                }
            }
            EventSchedule::Timed {
                start,
                end,
                timezone,
            } => {
                *imp.original_timed_event.borrow_mut() = Some(OriginalTimedEvent {
                    start: *start,
                    end: *end,
                    timezone: timezone.clone(),
                });
                imp.schedule_stack.set_visible_child_name("time-slot");
                if let (Some(start_date_time), Some(end_date_time)) = (
                    imp.start_date_time_state.borrow().as_ref(),
                    imp.end_date_time_state.borrow().as_ref(),
                ) {
                    start_date_time.set_date_time(
                        start.date_naive(),
                        start.hour() as i32,
                        start.minute() as i32,
                    );
                    end_date_time.set_date_time(
                        end.date_naive(),
                        end.hour() as i32,
                        end.minute() as i32,
                    );
                }
            }
        }
        imp.clear_error();
        imp.ensure_forward_range();
    }
}

impl imp::EventEditor {
    fn schedule_changed(&self) {
        let all_day = self.schedule_stack.visible_child_name().as_deref() == Some("all-day");
        if all_day {
            let (Some(start), Some(end)) = (
                self.start_date_row_state
                    .borrow()
                    .as_ref()
                    .and_then(|row| row.date()),
                self.end_date_row_state
                    .borrow()
                    .as_ref()
                    .and_then(|row| row.date()),
            ) else {
                return;
            };
            if let (Some(start_chooser), Some(end_chooser)) = (
                self.start_date_time_state.borrow().as_ref(),
                self.end_date_time_state.borrow().as_ref(),
            ) {
                let start_time = start_chooser
                    .date_time_parts()
                    .map(|(_, hour, minute)| (hour, minute))
                    .unwrap_or((9, 0));
                let end_time = end_chooser
                    .date_time_parts()
                    .map(|(_, hour, minute)| (hour, minute))
                    .unwrap_or((10, 0));
                start_chooser.set_date_time(start, start_time.0, start_time.1);
                end_chooser.set_date_time(end, end_time.0, end_time.1);
            }
        } else if let (Some(start), Some(end)) = (
            self.start_date_time_state
                .borrow()
                .as_ref()
                .and_then(|chooser| chooser.date_time_parts()),
            self.end_date_time_state
                .borrow()
                .as_ref()
                .and_then(|chooser| chooser.date_time_parts()),
        ) && let (Some(start_row), Some(end_row)) = (
            self.start_date_row_state.borrow().as_ref(),
            self.end_date_row_state.borrow().as_ref(),
        ) {
            start_row.set_date(start.0);
            end_row.set_date(end.0);
        }
        self.ensure_forward_range();
    }

    fn ensure_forward_range(&self) {
        let all_day = self.schedule_stack.visible_child_name().as_deref() == Some("all-day");
        if all_day {
            let (Some(start), Some(end)) = (
                self.start_date_row_state
                    .borrow()
                    .as_ref()
                    .and_then(|row| row.date()),
                self.end_date_row_state
                    .borrow()
                    .as_ref()
                    .and_then(|row| row.date()),
            ) else {
                return;
            };
            if end < start
                && let Some(row) = self.end_date_row_state.borrow().as_ref()
            {
                row.set_date(start);
                if let Some(chooser) = self.end_date_time_state.borrow().as_ref()
                    && let Some((_, hour, minute)) = chooser.date_time_parts()
                {
                    chooser.set_date_time(start, hour, minute);
                }
            }
            return;
        }

        let (Some(start_parts), Some(end_parts)) = (
            self.start_date_time_state
                .borrow()
                .as_ref()
                .and_then(|chooser| chooser.date_time_parts()),
            self.end_date_time_state
                .borrow()
                .as_ref()
                .and_then(|chooser| chooser.date_time_parts()),
        ) else {
            return;
        };
        let original = self.original_timed_event.borrow();
        let Some(start) = endpoint_datetime(
            start_parts.0,
            start_parts.1,
            start_parts.2,
            original.as_ref().map(|event| &event.start),
        ) else {
            return;
        };
        let Some(end) = endpoint_datetime(
            end_parts.0,
            end_parts.1,
            end_parts.2,
            original.as_ref().map(|event| &event.end),
        ) else {
            return;
        };
        if end > start {
            return;
        }

        let start_unchanged = original
            .as_ref()
            .is_some_and(|event| same_displayed_minute(&event.start, start_parts));
        let end_unchanged = original
            .as_ref()
            .is_some_and(|event| same_displayed_minute(&event.end, end_parts));
        let (target, shifted) = if !end_unchanged || start_unchanged {
            (false, shift_wall_clock(start_parts, 60))
        } else if !start_unchanged {
            (true, shift_wall_clock(end_parts, -60))
        } else {
            (false, shift_wall_clock(start_parts, 60))
        };
        if let Some((date, hour, minute)) = shifted {
            if target {
                if let Some(chooser) = self.start_date_time_state.borrow().as_ref() {
                    chooser.set_date_time(date, hour, minute);
                }
                if let Some(row) = self.start_date_row_state.borrow().as_ref() {
                    row.set_date(date);
                }
            } else {
                if let Some(chooser) = self.end_date_time_state.borrow().as_ref() {
                    chooser.set_date_time(date, hour, minute);
                }
                if let Some(row) = self.end_date_row_state.borrow().as_ref() {
                    row.set_date(date);
                }
            }
        }
    }

    fn clear_error(&self) {
        self.error_label.set_visible(false);
        self.error_label.set_label("");
    }

    fn show_error(&self, message: &str) {
        self.error_label.set_label(message);
        self.error_label.set_visible(true);
    }

    fn save_clicked(&self) {
        self.clear_error();
        let calendars = self.calendars.borrow();
        let Some(calendar) = calendars.get(self.calendar_row.selected() as usize) else {
            self.show_error("Choose a writable calendar.");
            return;
        };
        let calendar_id = calendar.id;
        drop(calendars);

        let base = self.editing_event.borrow().clone();
        let all_day = self.schedule_stack.visible_child_name().as_deref() == Some("all-day");
        let schedule = if all_day {
            let Some(start_date) = self
                .start_date_row_state
                .borrow()
                .as_ref()
                .and_then(|row| row.date())
            else {
                self.show_error("Choose a valid start date.");
                return;
            };
            let Some(end_date) = self
                .end_date_row_state
                .borrow()
                .as_ref()
                .and_then(|row| row.date())
            else {
                self.show_error("Choose a valid end date.");
                return;
            };
            let Some(end_exclusive) = end_date.succ_opt() else {
                self.show_error("The end date is out of range.");
                return;
            };
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive: end_exclusive,
            }
        } else {
            let Some((start_date, start_hour, start_minute)) = self
                .start_date_time_state
                .borrow()
                .as_ref()
                .and_then(|chooser| chooser.date_time_parts())
            else {
                self.show_error("Choose a valid start time.");
                return;
            };
            let Some((end_date, end_hour, end_minute)) = self
                .end_date_time_state
                .borrow()
                .as_ref()
                .and_then(|chooser| chooser.date_time_parts())
            else {
                self.show_error("Choose a valid end time.");
                return;
            };
            let original = self.original_timed_event.borrow();
            let Some(start) = endpoint_datetime(
                start_date,
                start_hour,
                start_minute,
                original.as_ref().map(|event| &event.start),
            ) else {
                self.show_error("Choose a valid start time.");
                return;
            };
            let Some(end) = endpoint_datetime(
                end_date,
                end_hour,
                end_minute,
                original.as_ref().map(|event| &event.end),
            ) else {
                self.show_error("Choose a valid end time.");
                return;
            };
            let timezone = original.as_ref().and_then(|event| event.timezone.clone());
            EventSchedule::Timed {
                start,
                end,
                timezone,
            }
        };

        let buffer = self.description_view.buffer();
        let (start, end) = buffer.bounds();
        let description = buffer.text(&start, &end, false).to_string();
        let event = Event {
            id: base
                .as_ref()
                .map(|event| event.id)
                .unwrap_or_else(Uuid::new_v4),
            calendar_id,
            title: self.title_entry.text().to_string(),
            location: self.location_entry.text().to_string(),
            description,
            schedule,
            recurrence: base.as_ref().and_then(|event| event.recurrence),
            reminders: base
                .as_ref()
                .map(|event| event.reminders.clone())
                .unwrap_or_default(),
        };
        let Ok(event) = validate_event(event) else {
            self.show_error("Enter a title and a range where the end is after the start.");
            return;
        };

        let editing = base.is_some();
        if let Some(callback) = self.on_save.borrow().as_ref()
            && callback(event, editing)
        {
            let editor = self.obj().clone();
            adw::prelude::AdwDialogExt::force_close(&editor);
        }
    }
}

fn select_calendar(row: &adw::ComboRow, calendars: &[Calendar], id: Uuid) {
    if let Some(index) = calendars.iter().position(|calendar| calendar.id == id) {
        row.set_selected(index as u32);
    }
}

fn endpoint_datetime(
    date: NaiveDate,
    hour: i32,
    minute: i32,
    original: Option<&DateTime<FixedOffset>>,
) -> Option<DateTime<FixedOffset>> {
    if let Some(original) = original
        && original.date_naive() == date
        && original.hour() == hour as u32
        && original.minute() == minute as u32
    {
        return Some(*original);
    }

    local_datetime(date, hour, minute)
}

fn same_displayed_minute(
    original: &DateTime<FixedOffset>,
    displayed: (NaiveDate, i32, i32),
) -> bool {
    original.date_naive() == displayed.0
        && original.hour() == displayed.1 as u32
        && original.minute() == displayed.2 as u32
}

fn shift_wall_clock(
    displayed: (NaiveDate, i32, i32),
    minutes: i64,
) -> Option<(NaiveDate, i32, i32)> {
    let value = NaiveDateTime::new(
        displayed.0,
        chrono::NaiveTime::from_hms_opt(displayed.1 as u32, displayed.2 as u32, 0)?,
    ) + Duration::minutes(minutes);
    Some((value.date(), value.hour() as i32, value.minute() as i32))
}

fn local_datetime(date: NaiveDate, hour: i32, minute: i32) -> Option<DateTime<FixedOffset>> {
    let local = glib::DateTime::new(
        &glib::TimeZone::local(),
        date.year(),
        date.month() as i32,
        date.day() as i32,
        hour,
        minute,
        0.0,
    )
    .ok()?;
    let offset_seconds = i32::try_from(local.utc_offset().as_seconds()).ok()?;
    let naive = date.and_hms_opt(hour as u32, minute as u32, 0)?;
    let offset = FixedOffset::east_opt(offset_seconds)?;
    offset.from_local_datetime(&naive).single()
}
