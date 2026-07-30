use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Timelike};
use gtk::glib;

type ChangedFn = Box<dyn Fn()>;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Endpoint {
    #[default]
    Start,
    End,
}

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/date-time-chooser.ui")]
    pub struct DateTimeChooser {
        #[template_child]
        pub start_date_row: TemplateChild<crate::ui::date_chooser_row::DateChooserRow>,
        #[template_child]
        pub end_date_row: TemplateChild<crate::ui::date_chooser_row::DateChooserRow>,
        #[template_child]
        pub start_summary: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub end_summary: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub start_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub end_caption: TemplateChild<gtk::Label>,
        #[template_child]
        pub start_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub end_value: TemplateChild<gtk::Label>,
        #[template_child]
        pub hour_grid: TemplateChild<gtk::Grid>,
        #[template_child]
        pub minute_grid: TemplateChild<gtk::Grid>,
        #[template_child]
        pub period_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub am_button: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub pm_button: TemplateChild<gtk::ToggleButton>,
        pub start_hour: Cell<i32>,
        pub start_minute: Cell<i32>,
        pub end_hour: Cell<i32>,
        pub end_minute: Cell<i32>,
        pub previous_start: Cell<Option<(NaiveDate, i32, i32)>>,
        pub(super) active_endpoint: Cell<Endpoint>,
        pub updating: Cell<bool>,
        pub hour_buttons: RefCell<Vec<gtk::Button>>,
        pub minute_buttons: RefCell<Vec<gtk::Button>>,
        pub on_changed: RefCell<Option<ChangedFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DateTimeChooser {
        const NAME: &'static str = "DateTimeChooser";
        type Type = super::DateTimeChooser;
        type ParentType = gtk::Box;

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
            self.start_summary.set_group(Some(&self.end_summary.get()));
            self.start_summary.set_active(true);
            self.am_button.set_group(Some(&self.pm_button.get()));
            self.am_button.set_active(true);

            let weak = self.obj().downgrade();
            self.start_summary.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(chooser) = weak.upgrade()
                {
                    chooser.imp().active_endpoint.set(Endpoint::Start);
                    chooser.imp().update_display();
                }
            });
            let weak = self.obj().downgrade();
            self.end_summary.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(chooser) = weak.upgrade()
                {
                    chooser.imp().active_endpoint.set(Endpoint::End);
                    chooser.imp().update_display();
                }
            });

            for (row, endpoint) in [
                (&self.start_date_row.get(), Endpoint::Start),
                (&self.end_date_row.get(), Endpoint::End),
            ] {
                row.set_on_date_changed({
                    let weak = self.obj().downgrade();
                    move |date| {
                        if let Some(chooser) = weak.upgrade()
                            && !chooser.imp().updating.get()
                        {
                            chooser.imp().date_changed(endpoint, date);
                        }
                    }
                });
            }

            let weak = self.obj().downgrade();
            self.am_button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(chooser) = weak.upgrade()
                {
                    chooser.imp().set_active_period(0);
                }
            });
            let weak = self.obj().downgrade();
            self.pm_button.connect_toggled(move |button| {
                if button.is_active()
                    && let Some(chooser) = weak.upgrade()
                {
                    chooser.imp().set_active_period(1);
                }
            });

            self.configure_controls();
            self.update_display();
        }
    }

    impl WidgetImpl for DateTimeChooser {}
    impl BoxImpl for DateTimeChooser {}
}

glib::wrapper! {
    pub struct DateTimeChooser(ObjectSubclass<imp::DateTimeChooser>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DateTimeChooser {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_date_times(
        &self,
        start_date: NaiveDate,
        start_hour: i32,
        start_minute: i32,
        end_date: NaiveDate,
        end_hour: i32,
        end_minute: i32,
    ) {
        let imp = self.imp();
        imp.updating.set(true);
        imp.start_date_row.set_date(start_date);
        imp.end_date_row.set_date(end_date);
        imp.start_hour.set(start_hour.clamp(0, 23));
        imp.start_minute.set(start_minute.clamp(0, 59));
        imp.end_hour.set(end_hour.clamp(0, 23));
        imp.end_minute.set(end_minute.clamp(0, 59));
        imp.previous_start.set(Some((
            start_date,
            imp.start_hour.get(),
            imp.start_minute.get(),
        )));
        imp.active_endpoint.set(Endpoint::Start);
        imp.start_summary.set_active(true);
        imp.configure_controls();
        imp.updating.set(false);
        imp.update_display();
    }

    pub fn set_start_date_time(&self, date: NaiveDate, hour: i32, minute: i32) {
        let Some((end_date, end_hour, end_minute)) = self.end_date_time_parts() else {
            return;
        };
        self.set_date_times(date, hour, minute, end_date, end_hour, end_minute);
    }

    pub fn set_end_date_time(&self, date: NaiveDate, hour: i32, minute: i32) {
        let Some((start_date, start_hour, start_minute)) = self.start_date_time_parts() else {
            return;
        };
        self.set_date_times(start_date, start_hour, start_minute, date, hour, minute);
    }

    pub fn start_date_time_parts(&self) -> Option<(NaiveDate, i32, i32)> {
        Some((
            self.imp().start_date_row.date()?,
            self.imp().start_hour.get(),
            self.imp().start_minute.get(),
        ))
    }

    pub fn end_date_time_parts(&self) -> Option<(NaiveDate, i32, i32)> {
        Some((
            self.imp().end_date_row.date()?,
            self.imp().end_hour.get(),
            self.imp().end_minute.get(),
        ))
    }

    pub fn set_on_changed<F: Fn() + 'static>(&self, callback: F) {
        *self.imp().on_changed.borrow_mut() = Some(Box::new(callback));
    }

    pub fn refresh_time_format(&self) {
        let imp = self.imp();
        imp.updating.set(true);
        imp.configure_controls();
        imp.updating.set(false);
        imp.update_display();
    }

    pub fn close_time_popover(&self) {
        self.imp().start_date_row.close_date_popover();
        self.imp().end_date_row.close_date_popover();
    }

    pub fn is_time_popover_widget(&self, widget: &gtk::Widget) -> bool {
        self.imp().start_date_row.is_date_popover_widget(widget)
            || self.imp().end_date_row.is_date_popover_widget(widget)
    }
}

impl imp::DateTimeChooser {
    fn emit_changed(&self) {
        if let Some(callback) = self.on_changed.borrow().as_ref() {
            callback();
        }
    }

    fn configure_controls(&self) {
        let twelve_hour = matches!(
            calendar::preferences::resolved_time_format(),
            calendar::time_format::TimeFormatPreference::TwelveHour
        );
        self.period_box.set_visible(twelve_hour);
        self.rebuild_hour_grid(twelve_hour);
        self.rebuild_minute_grid();
        self.update_display();
    }

    fn rebuild_hour_grid(&self, twelve_hour: bool) {
        while let Some(child) = self.hour_grid.first_child() {
            self.hour_grid.remove(&child);
        }
        self.hour_buttons.borrow_mut().clear();
        let count = if twelve_hour { 12 } else { 24 };
        let columns = if twelve_hour { 4 } else { 6 };
        self.hour_grid.set_column_homogeneous(true);
        self.hour_grid.set_row_homogeneous(true);
        for index in 0..count {
            let display_hour = if twelve_hour { index + 1 } else { index };
            let label = if twelve_hour {
                display_hour.to_string()
            } else {
                format!("{display_hour:02}")
            };
            let button = gtk::Button::with_label(&label);
            button.set_hexpand(true);
            button.set_css_classes(&["time-choice"]);
            let weak = self.obj().downgrade();
            button.connect_clicked(move |_| {
                if let Some(chooser) = weak.upgrade() {
                    chooser.imp().set_active_hour(display_hour);
                }
            });
            self.hour_grid
                .attach(&button, index % columns, index / columns, 1, 1);
            self.hour_buttons.borrow_mut().push(button);
        }
    }

    fn rebuild_minute_grid(&self) {
        while let Some(child) = self.minute_grid.first_child() {
            self.minute_grid.remove(&child);
        }
        self.minute_buttons.borrow_mut().clear();
        for index in 0..12 {
            let minute = index * 5;
            let button = gtk::Button::with_label(&format!("{minute:02}"));
            button.set_hexpand(true);
            button.set_css_classes(&["time-choice"]);
            let weak = self.obj().downgrade();
            button.connect_clicked(move |_| {
                if let Some(chooser) = weak.upgrade() {
                    chooser.imp().set_active_minute(minute);
                }
            });
            self.minute_grid.attach(&button, index % 4, index / 4, 1, 1);
            self.minute_buttons.borrow_mut().push(button);
        }
    }

    fn set_active_hour(&self, display_hour: i32) {
        let old_start = self.start_parts();
        let old_end = self.end_parts();
        let hour = if self.period_box.is_visible() {
            to_24_hour(display_hour, if self.active_period() { 1 } else { 0 })
        } else {
            display_hour
        };
        self.set_endpoint_hour(hour);
        if self.active_endpoint.get() == Endpoint::Start {
            self.shift_end(old_start, self.start_parts(), old_end);
        }
        self.update_display();
        self.emit_changed();
    }

    fn set_active_minute(&self, minute: i32) {
        let old_start = self.start_parts();
        let old_end = self.end_parts();
        self.set_endpoint_minute(minute);
        if self.active_endpoint.get() == Endpoint::Start {
            self.shift_end(old_start, self.start_parts(), old_end);
        }
        self.update_display();
        self.emit_changed();
    }

    fn set_active_period(&self, period: u32) {
        if self.updating.get() {
            return;
        }
        let old_start = self.start_parts();
        let old_end = self.end_parts();
        let display_hour = to_12_hour(self.endpoint_hour());
        self.set_endpoint_hour(to_24_hour(display_hour, period));
        if self.active_endpoint.get() == Endpoint::Start {
            self.shift_end(old_start, self.start_parts(), old_end);
        }
        self.update_display();
        self.emit_changed();
    }

    fn date_changed(&self, endpoint: Endpoint, date: NaiveDate) {
        match endpoint {
            Endpoint::Start => {
                let old_start = self.previous_start.get();
                let new_start = self
                    .start_parts()
                    .map(|(_, hour, minute)| (date, hour, minute));
                self.shift_end(old_start, new_start, self.end_parts());
                self.previous_start.set(new_start);
            }
            Endpoint::End => {
                self.previous_start.set(self.start_parts());
            }
        }
        self.update_display();
        self.emit_changed();
    }

    fn start_parts(&self) -> Option<(NaiveDate, i32, i32)> {
        Some((
            self.start_date_row.date()?,
            self.start_hour.get(),
            self.start_minute.get(),
        ))
    }

    fn end_parts(&self) -> Option<(NaiveDate, i32, i32)> {
        Some((
            self.end_date_row.date()?,
            self.end_hour.get(),
            self.end_minute.get(),
        ))
    }

    fn shift_end(
        &self,
        old_start: Option<(NaiveDate, i32, i32)>,
        new_start: Option<(NaiveDate, i32, i32)>,
        old_end: Option<(NaiveDate, i32, i32)>,
    ) {
        let (Some(old_start), Some(new_start), Some(old_end)) = (old_start, new_start, old_end)
        else {
            return;
        };
        let (date, hour, minute) = shift_end_for_start_change(old_start, new_start, old_end);
        self.end_date_row.set_date(date);
        self.end_hour.set(hour);
        self.end_minute.set(minute);
        self.previous_start.set(Some(new_start));
    }

    fn endpoint_hour(&self) -> i32 {
        match self.active_endpoint.get() {
            Endpoint::Start => self.start_hour.get(),
            Endpoint::End => self.end_hour.get(),
        }
    }

    fn set_endpoint_hour(&self, hour: i32) {
        match self.active_endpoint.get() {
            Endpoint::Start => self.start_hour.set(hour),
            Endpoint::End => self.end_hour.set(hour),
        }
    }

    fn set_endpoint_minute(&self, minute: i32) {
        match self.active_endpoint.get() {
            Endpoint::Start => self.start_minute.set(minute),
            Endpoint::End => self.end_minute.set(minute),
        }
    }

    fn endpoint_minute(&self) -> i32 {
        match self.active_endpoint.get() {
            Endpoint::Start => self.start_minute.get(),
            Endpoint::End => self.end_minute.get(),
        }
    }

    fn active_period(&self) -> bool {
        self.endpoint_hour() >= 12
    }

    fn update_display(&self) {
        let twelve_hour = self.period_box.is_visible();
        self.start_caption.set_label("START");
        let duration = self.duration_text();
        self.end_caption.set_label(
            &duration.map_or_else(|| "END".to_owned(), |value| format!("END · {value}")),
        );
        self.start_value.set_label(&format_wall_time(
            self.start_hour.get(),
            self.start_minute.get(),
            twelve_hour,
        ));
        self.end_value.set_label(&format_wall_time(
            self.end_hour.get(),
            self.end_minute.get(),
            twelve_hour,
        ));
        self.am_button.set_active(!self.active_period());
        self.pm_button.set_active(self.active_period());
        for (index, button) in self.hour_buttons.borrow().iter().enumerate() {
            let display_hour = index as i32 + if twelve_hour { 1 } else { 0 };
            button.set_css_classes(
                if display_hour
                    == if twelve_hour {
                        to_12_hour(self.endpoint_hour())
                    } else {
                        self.endpoint_hour()
                    }
                {
                    &["time-choice", "selected"]
                } else {
                    &["time-choice"]
                },
            );
        }
        for (index, button) in self.minute_buttons.borrow().iter().enumerate() {
            button.set_css_classes(if index as i32 * 5 == self.endpoint_minute() {
                &["time-choice", "selected"]
            } else {
                &["time-choice"]
            });
        }
    }

    fn duration_text(&self) -> Option<String> {
        let start = NaiveDateTime::new(
            self.start_date_row.date()?,
            NaiveTime::from_hms_opt(
                self.start_hour.get() as u32,
                self.start_minute.get() as u32,
                0,
            )?,
        );
        let end = NaiveDateTime::new(
            self.end_date_row.date()?,
            NaiveTime::from_hms_opt(self.end_hour.get() as u32, self.end_minute.get() as u32, 0)?,
        );
        let duration = end - start;
        (duration > Duration::zero()).then(|| format_duration(duration))
    }
}

fn format_wall_time(hour: i32, minute: i32, twelve_hour: bool) -> String {
    let Some(time) = NaiveTime::from_hms_opt(hour as u32, minute as u32, 0) else {
        return format!("{hour:02}:{minute:02}");
    };
    if twelve_hour {
        let suffix = if hour >= 12 { "PM" } else { "AM" };
        format!("{}:{minute:02} {suffix}", to_12_hour(time.hour() as i32))
    } else {
        format!("{hour:02}:{minute:02}")
    }
}

fn format_duration(duration: Duration) -> String {
    let minutes = duration.num_minutes();
    let days = minutes / (24 * 60);
    let hours = (minutes % (24 * 60)) / 60;
    let remainder = minutes % 60;
    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}hr"));
    }
    if remainder > 0 {
        parts.push(format!("{remainder}m"));
    }
    if parts.is_empty() {
        "0m".to_owned()
    } else {
        parts.join(" ")
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

fn shift_end_for_start_change(
    old_start: (NaiveDate, i32, i32),
    new_start: (NaiveDate, i32, i32),
    old_end: (NaiveDate, i32, i32),
) -> (NaiveDate, i32, i32) {
    let datetime = |(date, hour, minute)| {
        NaiveDateTime::new(
            date,
            NaiveTime::from_hms_opt(hour as u32, minute as u32, 0).unwrap(),
        )
    };
    let shifted_end = datetime(old_end) + (datetime(new_start) - datetime(old_start));
    (
        shifted_end.date(),
        shifted_end.time().hour() as i32,
        shifted_end.time().minute() as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::shift_end_for_start_change;
    use chrono::NaiveDate;

    #[test]
    fn start_time_shift_preserves_duration() {
        let d = |year, month, day| NaiveDate::from_ymd_opt(year, month, day).unwrap();

        assert_eq!(
            shift_end_for_start_change(
                (d(2026, 7, 20), 9, 0),
                (d(2026, 7, 20), 2, 0),
                (d(2026, 7, 20), 10, 0),
            ),
            (d(2026, 7, 20), 3, 0),
            "moving Start from 09:00 to 02:00 moves End from 10:00 to 03:00",
        );

        assert_eq!(
            shift_end_for_start_change(
                (d(2026, 7, 20), 23, 30),
                (d(2026, 7, 21), 0, 30),
                (d(2026, 7, 21), 0, 30),
            ),
            (d(2026, 7, 21), 1, 30),
            "the End follows a Start shift across midnight",
        );
    }
}
