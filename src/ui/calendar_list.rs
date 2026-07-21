use std::cell::RefCell;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::Calendar;
use gtk::glib;
use uuid::Uuid;

type VisibilityChangedFn = Box<dyn Fn(Uuid, bool) -> bool>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/calendar-list.ui")]
    pub struct CalendarList {
        #[template_child]
        pub list_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub calendars_list_box: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub no_calendars_label: TemplateChild<gtk::Label>,
        pub on_visibility_changed: RefCell<Option<VisibilityChangedFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarList {
        const NAME: &'static str = "CalendarList";
        type Type = super::CalendarList;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CalendarList {
        fn constructed(&self) {
            self.parent_constructed();
            self.list_scroll.set_visible(false);
            self.no_calendars_label.set_visible(true);
        }
    }

    impl WidgetImpl for CalendarList {}
    impl BinImpl for CalendarList {}
}

glib::wrapper! {
    pub struct CalendarList(ObjectSubclass<imp::CalendarList>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CalendarList {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_on_visibility_changed<F: Fn(Uuid, bool) -> bool + 'static>(&self, callback: F) {
        *self.imp().on_visibility_changed.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_calendars(&self, calendars: &[Calendar]) {
        let imp = self.imp();
        while let Some(child) = imp.calendars_list_box.first_child() {
            imp.calendars_list_box.remove(&child);
        }

        let mut calendars = calendars.to_vec();
        calendars.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        if calendars.is_empty() {
            imp.list_scroll.set_visible(false);
            imp.no_calendars_label.set_visible(true);
            return;
        }

        let list_weak = self.downgrade();
        for calendar in &calendars {
            let row = crate::ui::calendar_row::CalendarRow::new(calendar);
            let list_weak = list_weak.clone();
            row.set_on_visibility_changed(move |calendar_id, visible| {
                list_weak.upgrade().is_some_and(|list| {
                    list.imp()
                        .on_visibility_changed
                        .borrow()
                        .as_ref()
                        .is_none_or(|callback| callback(calendar_id, visible))
                })
            });
            imp.calendars_list_box.append(&row);
        }
        imp.list_scroll.set_visible(true);
        imp.no_calendars_label.set_visible(false);
    }
}
