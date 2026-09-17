use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::preferences::{
    format_wall_time, load_default_reminders, load_time_format_preference, save_default_reminders,
    save_time_format_preference,
};
use calendar::reminder_choice::{DefaultReminders, ReminderChoice};
use calendar::time_format::TimeFormatPreference;
use gtk::glib;

type ChangedFn = Box<dyn Fn() + 'static>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/preferences.ui")]
    pub struct PreferencesDialog {
        #[template_child]
        pub time_format_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub timed_reminder_row: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub all_day_reminder_row: TemplateChild<adw::ComboRow>,
        pub on_changed: RefCell<Option<ChangedFn>>,
        pub reminder_rows_syncing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreferencesDialog {
        const NAME: &'static str = "PreferencesDialog";
        type Type = super::PreferencesDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PreferencesDialog {
        fn constructed(&self) {
            self.parent_constructed();
            let model = gtk::StringList::new(&["System Default", "12-hour", "24-hour"]);
            self.time_format_row.set_model(Some(&model));
            self.time_format_row
                .set_selected(preference_index(load_time_format_preference()));
            let weak = self.obj().downgrade();
            self.time_format_row.connect_selected_notify(move |row| {
                let preference = match row.selected() {
                    1 => TimeFormatPreference::TwelveHour,
                    2 => TimeFormatPreference::TwentyFourHour,
                    _ => TimeFormatPreference::System,
                };
                save_time_format_preference(preference);
                if let Some(dialog) = weak.upgrade() {
                    // All-day labels carry wall-clock times in the new format.
                    dialog.imp().sync_reminder_rows(load_default_reminders());
                    if let Some(callback) = dialog.imp().on_changed.borrow().as_ref() {
                        callback();
                    }
                }
            });

            for row in [&self.timed_reminder_row, &self.all_day_reminder_row] {
                let weak = self.obj().downgrade();
                row.connect_selected_notify(move |_| {
                    if let Some(dialog) = weak.upgrade()
                        && !dialog.imp().reminder_rows_syncing.get()
                    {
                        save_default_reminders(dialog.imp().selected_default_reminders());
                    }
                });
            }
            self.sync_reminder_rows(load_default_reminders());
        }
    }

    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}

    impl PreferencesDialog {
        /// Rebuild both rows' lists with current clock-format labels and
        /// select the stored defaults without triggering a save.
        pub fn sync_reminder_rows(&self, defaults: DefaultReminders) {
            self.reminder_rows_syncing.set(true);
            for (row, all_day, choice) in [
                (&self.timed_reminder_row, false, defaults.timed),
                (&self.all_day_reminder_row, true, defaults.all_day),
            ] {
                let labels = ReminderChoice::labels(all_day, format_wall_time);
                let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                row.set_model(Some(&gtk::StringList::new(&labels)));
                row.set_selected(choice.index(all_day).unwrap_or(0));
            }
            self.reminder_rows_syncing.set(false);
        }

        fn selected_default_reminders(&self) -> DefaultReminders {
            let fallback = DefaultReminders::default();
            DefaultReminders {
                timed: ReminderChoice::from_index(false, self.timed_reminder_row.selected())
                    .unwrap_or(fallback.timed),
                all_day: ReminderChoice::from_index(true, self.all_day_reminder_row.selected())
                    .unwrap_or(fallback.all_day),
            }
        }
    }
}

glib::wrapper! {
    pub struct PreferencesDialog(ObjectSubclass<imp::PreferencesDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl PreferencesDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_on_changed<F: Fn() + 'static>(&self, callback: F) {
        *self.imp().on_changed.borrow_mut() = Some(Box::new(callback));
    }

    pub fn refresh(&self) {
        self.imp()
            .time_format_row
            .set_selected(preference_index(load_time_format_preference()));
        self.imp().sync_reminder_rows(load_default_reminders());
    }
}

fn preference_index(preference: TimeFormatPreference) -> u32 {
    match preference {
        TimeFormatPreference::System => 0,
        TimeFormatPreference::TwelveHour => 1,
        TimeFormatPreference::TwentyFourHour => 2,
    }
}
