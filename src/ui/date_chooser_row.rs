use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Datelike, NaiveDate};
use gtk::glib;

type DateChangedFn = Box<dyn Fn(NaiveDate)>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/date-chooser-row.ui")]
    pub struct DateChooserRow {
        #[template_child]
        pub date_chooser_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub date_popover: TemplateChild<gtk::Popover>,
        #[template_child]
        pub date_button: TemplateChild<gtk::MenuButton>,
        pub on_date_changed: RefCell<Option<DateChangedFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DateChooserRow {
        const NAME: &'static str = "DateChooserRow";
        type Type = super::DateChooserRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DateChooserRow {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj()
                .set_activatable_widget(Some(&self.date_button.get()));
            let weak = self.obj().downgrade();
            self.obj().connect_activated(move |_| {
                if let Some(row) = weak.upgrade() {
                    row.imp().date_popover.popup();
                }
            });

            let chooser = crate::ui::date_chooser::DateChooser::new();
            chooser.set_on_date_selected({
                let row_weak = self.obj().downgrade();
                move |date| {
                    if let Some(row) = row_weak.upgrade() {
                        row.set_date(date);
                        if let Some(callback) = row.imp().on_date_changed.borrow().as_ref() {
                            callback(date);
                        }
                        row.imp().date_popover.popdown();
                    }
                }
            });
            self.date_chooser_bin.set_child(Some(&chooser));
        }
    }

    impl WidgetImpl for DateChooserRow {}
    impl ListBoxRowImpl for DateChooserRow {}
    impl PreferencesRowImpl for DateChooserRow {}
    impl ActionRowImpl for DateChooserRow {}
}

glib::wrapper! {
    pub struct DateChooserRow(ObjectSubclass<imp::DateChooserRow>)
        @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl DateChooserRow {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_date(&self, date: NaiveDate) {
        let Some(chooser) = self.imp().date_chooser_bin.child().and_then(|child| {
            child
                .downcast::<crate::ui::date_chooser::DateChooser>()
                .ok()
        }) else {
            return;
        };
        chooser.set_date(date);
        let local = glib::DateTime::new(
            &glib::TimeZone::local(),
            date.year(),
            date.month() as i32,
            date.day() as i32,
            0,
            0,
            0.0,
        );
        let text = local
            .ok()
            .and_then(|date_time| date_time.format("%x").ok())
            .map(|text| text.to_string())
            .unwrap_or_else(|| date.format("%Y-%m-%d").to_string());
        self.set_subtitle(&text);
    }

    pub fn date(&self) -> Option<NaiveDate> {
        self.imp()
            .date_chooser_bin
            .child()
            .and_then(|child| {
                child
                    .downcast::<crate::ui::date_chooser::DateChooser>()
                    .ok()
            })
            .and_then(|chooser| chooser.date())
    }

    pub fn set_on_date_changed<F: Fn(NaiveDate) + 'static>(&self, callback: F) {
        *self.imp().on_date_changed.borrow_mut() = Some(Box::new(callback));
    }
}
