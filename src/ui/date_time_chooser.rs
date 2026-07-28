use std::cell::Cell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::NaiveDate;
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
        #[template_child]
        pub period_dropdown: TemplateChild<gtk::DropDown>,
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

            self.period_dropdown
                .set_model(Some(&gtk::StringList::new(&["AM", "PM"])));

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
                    let hour = spin.value_as_int();
                    let hour = if chooser.imp().period_dropdown.is_visible() {
                        to_24_hour(hour, chooser.imp().period_dropdown.selected())
                    } else {
                        hour
                    };
                    chooser.imp().hour.set(hour);
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

            let weak = self.obj().downgrade();
            self.period_dropdown
                .connect_selected_notify(move |dropdown| {
                    if let Some(chooser) = weak.upgrade() {
                        if chooser.imp().updating.get() {
                            return;
                        }
                        let display_hour = chooser.imp().hour_spin.value_as_int();
                        chooser
                            .imp()
                            .hour
                            .set(to_24_hour(display_hour, dropdown.selected()));
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
        imp.configure_time_controls();
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

    pub fn refresh_time_format(&self) {
        let imp = self.imp();
        imp.updating.set(true);
        imp.configure_time_controls();
        imp.update_time_label();
        imp.updating.set(false);
    }
}

impl imp::DateTimeChooser {
    fn emit_changed(&self) {
        if let Some(callback) = self.on_changed.borrow().as_ref() {
            callback();
        }
    }

    fn update_time_label(&self) {
        let Some(_date) = self.date_row.date() else {
            return;
        };
        let time =
            chrono::NaiveTime::from_hms_opt(self.hour.get() as u32, self.minute.get() as u32, 0);
        let text = time
            .map(calendar::preferences::format_wall_time)
            .unwrap_or_else(|| format!("{:02}:{:02}", self.hour.get(), self.minute.get()));
        self.time_row.set_subtitle(&text);
    }

    fn configure_time_controls(&self) {
        let twelve_hour = matches!(
            calendar::preferences::resolved_time_format(),
            calendar::time_format::TimeFormatPreference::TwelveHour
        );
        self.period_dropdown.set_visible(twelve_hour);
        if twelve_hour {
            self.hour_spin.set_range(1.0, 12.0);
            self.hour_spin
                .set_value(f64::from(to_12_hour(self.hour.get())));
            self.period_dropdown
                .set_selected(if self.hour.get() >= 12 { 1 } else { 0 });
        } else {
            self.hour_spin.set_range(0.0, 23.0);
            self.hour_spin.set_value(f64::from(self.hour.get()));
        }
    }
}

fn to_12_hour(hour: i32) -> i32 {
    let hour = hour % 12;
    if hour == 0 { 12 } else { hour }
}

fn to_24_hour(hour: i32, period: u32) -> i32 {
    let hour = hour.clamp(1, 12) % 12;
    hour + if period == 1 { 12 } else { 0 }
}

fn padded_spin_output(spin: &gtk::SpinButton) -> glib::Propagation {
    spin.set_text(&format!("{:02}", spin.value_as_int()));
    glib::Propagation::Stop
}
