use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::preferences::{load_time_format_preference, save_time_format_preference};
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
        pub on_changed: RefCell<Option<ChangedFn>>,
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
                if let Some(dialog) = weak.upgrade()
                    && let Some(callback) = dialog.imp().on_changed.borrow().as_ref()
                {
                    callback();
                }
            });
        }
    }

    impl WidgetImpl for PreferencesDialog {}
    impl AdwDialogImpl for PreferencesDialog {}
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
    }
}

fn preference_index(preference: TimeFormatPreference) -> u32 {
    match preference {
        TimeFormatPreference::System => 0,
        TimeFormatPreference::TwelveHour => 1,
        TimeFormatPreference::TwentyFourHour => 2,
    }
}
