use std::cell::Cell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, NaiveDate};
use gtk::glib;

type ChangedFn = Box<dyn Fn()>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/date-time-chooser.ui")]
    pub struct DateTimeChooser {
        #[template_child]
        pub date_row: TemplateChild<crate::ui::date_chooser_row::DateChooserRow>,
        #[template_child]
        pub time_row: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub time_popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub time_button: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub hour_spin: TemplateChild<gtk::SpinButton>,
        #[template_child]
        pub minute_spin: TemplateChild<gtk::SpinButton>,
        pub hour: Cell<i32>,
        pub minute: Cell<i32>,
        pub updating: Cell<bool>,
        pub on_changed: std::cell::RefCell<Option<ChangedFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DateTimeChooser {
        const NAME: &'static str = "DateTimeChooser";
        type Type = super::DateTimeChooser;
        type ParentType = adw::PreferencesGroup;

        fn class_init(klass: &mut Self::Class) {
            crate::ui::date_chooser_row::DateChooserRow::static_type();
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DateTimeChooser {
        fn constructed(&self) {
            self.parent_constructed();
            self.time_row
                .set_activatable_widget(Some(&self.time_button.get()));
            let weak = self.obj().downgrade();
            self.time_row.connect_activated(move |_| {
                if let Some(chooser) = weak.upgrade() {
                    chooser.imp().time_popover.popup();
                }
            });
            self.hour_spin.set_range(0.0, 23.0);
            self.minute_spin.set_range(0.0, 59.0);
            self.hour_spin.set_increments(1.0, 5.0);
            self.minute_spin.set_increments(1.0, 10.0);
            self.hour_spin.set_wrap(true);
            self.minute_spin.set_wrap(true);
            self.hour_spin.connect_output(padded_spin_output);
            self.minute_spin.connect_output(padded_spin_output);

            self.date_row.set_on_date_changed({
                let weak = self.obj().downgrade();
                move |_| {
                    if let Some(chooser) = weak.upgrade() {
                        if chooser.imp().updating.get() {
                            return;
                        }
                        chooser.imp().update_time_label();
                        chooser.imp().emit_changed();
                    }
                }
            });

            let weak = self.obj().downgrade();
            self.hour_spin.connect_value_changed(move |spin| {
                if let Some(chooser) = weak.upgrade() {
                    if chooser.imp().updating.get() {
                        return;
                    }
                    chooser.imp().hour.set(spin.value_as_int());
                    chooser.imp().update_time_label();
                    chooser.imp().emit_changed();
                }
            });

            let weak = self.obj().downgrade();
            self.minute_spin.connect_value_changed(move |spin| {
                if let Some(chooser) = weak.upgrade() {
                    if chooser.imp().updating.get() {
                        return;
                    }
                    chooser.imp().minute.set(spin.value_as_int());
                    chooser.imp().update_time_label();
                    chooser.imp().emit_changed();
                }
            });
        }
    }

    impl WidgetImpl for DateTimeChooser {}
    impl PreferencesGroupImpl for DateTimeChooser {}
}

glib::wrapper! {
    pub struct DateTimeChooser(ObjectSubclass<imp::DateTimeChooser>)
        @extends adw::PreferencesGroup, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DateTimeChooser {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_labels(&self, date_label: &str, time_label: &str) {
        let imp = self.imp();
        imp.date_row.set_title(date_label);
        imp.time_row.set_title(time_label);
    }

    pub fn set_date_time(&self, date: NaiveDate, hour: i32, minute: i32) {
        let imp = self.imp();
        let hour = hour.clamp(0, 23);
        let minute = minute.clamp(0, 59);
        imp.updating.set(true);
        imp.date_row.set_date(date);
        imp.hour.set(hour);
        imp.minute.set(minute);
        imp.hour_spin.set_value(f64::from(hour));
        imp.minute_spin.set_value(f64::from(minute));
        imp.update_time_label();
        imp.updating.set(false);
    }

    pub fn date_time_parts(&self) -> Option<(NaiveDate, i32, i32)> {
        let date = self.imp().date_row.date()?;
        Some((date, self.imp().hour.get(), self.imp().minute.get()))
    }

    pub fn set_on_changed<F: Fn() + 'static>(&self, callback: F) {
        *self.imp().on_changed.borrow_mut() = Some(Box::new(callback));
    }
}

impl imp::DateTimeChooser {
    fn emit_changed(&self) {
        if let Some(callback) = self.on_changed.borrow().as_ref() {
            callback();
        }
    }

    fn update_time_label(&self) {
        let Some(date) = self.date_row.date() else {
            return;
        };
        let text = glib::DateTime::new(
            &glib::TimeZone::local(),
            date.year(),
            date.month() as i32,
            date.day() as i32,
            self.hour.get(),
            self.minute.get(),
            0.0,
        )
        .ok()
        .and_then(|date_time| date_time.format("%R").ok())
        .map(|text| text.to_string())
        .unwrap_or_else(|| format!("{:02}:{:02}", self.hour.get(), self.minute.get()));
        self.time_row.set_subtitle(&text);
    }
}

fn padded_spin_output(spin: &gtk::SpinButton) -> glib::Propagation {
    spin.set_text(&format!("{:02}", spin.value_as_int()));
    glib::Propagation::Stop
}
