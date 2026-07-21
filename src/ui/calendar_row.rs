use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::Calendar;
use gtk::glib;
use uuid::Uuid;

type VisibilityChangedFn = Box<dyn Fn(Uuid, bool) -> bool>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/calendar-row.ui")]
    pub struct CalendarRow {
        #[template_child]
        pub color_swatch: TemplateChild<gtk::DrawingArea>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub checkmark_image: TemplateChild<gtk::Image>,

        pub calendar: RefCell<Option<Calendar>>,
        pub color: RefCell<Option<gtk::gdk::RGBA>>,
        pub visible: Cell<bool>,
        pub syncing: Cell<bool>,
        pub on_visibility_changed: RefCell<Option<VisibilityChangedFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarRow {
        const NAME: &'static str = "CalendarRow";
        type Type = super::CalendarRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CalendarRow {
        fn constructed(&self) {
            self.parent_constructed();

            let row_weak = self.obj().downgrade();
            self.color_swatch
                .set_draw_func(move |area, context, width, height| {
                    let Some(row) = row_weak.upgrade() else {
                        return;
                    };
                    let color = (*row.imp().color.borrow()).unwrap_or_else(fallback_color);
                    let size = f64::from(width.min(height));
                    let scale = f64::from(area.scale_factor().max(1));
                    let inset = 1.0 / scale;
                    let radius = (size / 2.0 - inset).max(0.0);
                    let centre = size / 2.0;

                    context.arc(centre, centre, radius, 0.0, std::f64::consts::TAU);
                    context.set_source_rgba(
                        f64::from(color.red()),
                        f64::from(color.green()),
                        f64::from(color.blue()),
                        f64::from(color.alpha()),
                    );
                    let _ = context.fill_preserve();
                    context.set_source_rgba(0.0, 0.0, 0.0, 0.24);
                    context.set_line_width(inset);
                    let _ = context.stroke();
                });

            let row_weak = self.obj().downgrade();
            self.obj().connect_activated(move |_| {
                let Some(row) = row_weak.upgrade() else {
                    return;
                };
                row.toggle_visibility();
            });
        }
    }

    impl WidgetImpl for CalendarRow {}
    impl ListBoxRowImpl for CalendarRow {}
    impl PreferencesRowImpl for CalendarRow {}
    impl ActionRowImpl for CalendarRow {}
}

glib::wrapper! {
    pub struct CalendarRow(ObjectSubclass<imp::CalendarRow>)
        @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl CalendarRow {
    pub fn new(calendar: &Calendar) -> Self {
        let row: Self = glib::Object::new();
        row.set_calendar(calendar);
        row
    }

    pub fn set_on_visibility_changed<F: Fn(Uuid, bool) -> bool + 'static>(&self, callback: F) {
        *self.imp().on_visibility_changed.borrow_mut() = Some(Box::new(callback));
    }

    fn set_calendar(&self, calendar: &Calendar) {
        let imp = self.imp();
        *imp.calendar.borrow_mut() = Some(calendar.clone());
        imp.title_label.set_label(&calendar.name);
        imp.title_label.set_tooltip_text(Some(&calendar.name));
        self.set_tooltip_text(Some(&format!(
            "{} calendar — {}",
            calendar.name,
            if calendar.visible {
                "visible"
            } else {
                "hidden"
            }
        )));
        *imp.color.borrow_mut() = Some(parse_color(&calendar.color));
        imp.color_swatch.queue_draw();
        self.set_visible_state(calendar.visible);
    }

    fn set_visible_state(&self, visible: bool) {
        let imp = self.imp();
        imp.syncing.set(true);
        imp.visible.set(visible);
        imp.checkmark_image.set_visible(visible);
        if visible {
            self.set_state_flags(gtk::StateFlags::CHECKED, false);
        } else {
            self.unset_state_flags(gtk::StateFlags::CHECKED);
        }
        self.set_tooltip_text(Some(&format!(
            "{} calendar — {}",
            imp.calendar
                .borrow()
                .as_ref()
                .map(|calendar| calendar.name.as_str())
                .unwrap_or("Calendar"),
            if visible { "visible" } else { "hidden" }
        )));
        imp.syncing.set(false);
    }

    fn toggle_visibility(&self) {
        let imp = self.imp();
        if imp.syncing.get() {
            return;
        }
        let old_visible = imp.visible.get();
        let new_visible = !old_visible;
        let Some(calendar_id) = imp.calendar.borrow().as_ref().map(|calendar| calendar.id) else {
            return;
        };

        self.set_visible_state(new_visible);
        let accepted = imp
            .on_visibility_changed
            .borrow()
            .as_ref()
            .is_none_or(|callback| callback(calendar_id, new_visible));
        if !accepted {
            self.set_visible_state(old_visible);
        }
    }
}

fn parse_color(value: &str) -> gtk::gdk::RGBA {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() == 6 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        let red = u8::from_str_radix(&hex[0..2], 16).unwrap_or(153);
        let green = u8::from_str_radix(&hex[2..4], 16).unwrap_or(153);
        let blue = u8::from_str_radix(&hex[4..6], 16).unwrap_or(153);
        return gtk::gdk::RGBA::new(
            f32::from(red) / 255.0,
            f32::from(green) / 255.0,
            f32::from(blue) / 255.0,
            1.0,
        );
    }
    fallback_color()
}

fn fallback_color() -> gtk::gdk::RGBA {
    gtk::gdk::RGBA::new(0.6, 0.6, 0.6, 1.0)
}
