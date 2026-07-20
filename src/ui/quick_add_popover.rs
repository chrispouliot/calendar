use std::cell::{Cell, RefCell};

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::Calendar;
use chrono::{Datelike, NaiveDate};
use gtk::glib;
use uuid::Uuid;

/// Save callback: invoked with the validated (trimmed_title, calendar_id, date)
/// when the user presses Save (or Enter in the title).  The host window
/// builds the event via the pure model seam and persists it.
type SaveFn = Box<dyn Fn(String, Uuid, NaiveDate)>;

/// Edit-Details callback: invoked when the user presses Edit Details.
/// Phase 6 has no full editor — the host window shows a toast.
type EditDetailsFn = Box<dyn Fn()>;

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/quick-add-popover.ui")]
    pub struct QuickAddPopover {
        #[template_child]
        pub date_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub title_entry: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub calendar_scroll: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub calendars_list_box: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub no_calendars_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub edit_details_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub save_button: TemplateChild<gtk::Button>,

        /// Filtered (visible, non-read-only) calendars in display order.
        pub calendars: RefCell<Vec<Calendar>>,
        /// Currently selected calendar UUID.
        pub selected_calendar_id: RefCell<Option<Uuid>>,
        /// The date the popover is currently creating an event for.
        pub current_date: Cell<Option<NaiveDate>>,
        /// Today's date (read once at construction) for header labels.
        pub today: Cell<NaiveDate>,

        pub on_save: RefCell<Option<SaveFn>>,
        pub on_edit_details: RefCell<Option<EditDetailsFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for QuickAddPopover {
        const NAME: &'static str = "QuickAddPopover";
        const ABSTRACT: bool = false;
        type Type = super::QuickAddPopover;
        type ParentType = gtk::Popover;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for QuickAddPopover {
        fn constructed(&self) {
            self.parent_constructed();

            let now = glib::DateTime::now_local().unwrap();
            let today =
                NaiveDate::from_ymd_opt(now.year(), now.month() as u32, now.day_of_month() as u32)
                    .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 7, 20).unwrap());
            self.today.set(today);

            // Save starts insensitive (mirrors reference).
            self.save_button.set_sensitive(false);

            let popover_weak = self.obj().downgrade();

            // ── Title text changed → update save sensitivity and toggle
            //    the error class just like GNOME's summary_entry_text_changed.
            //    Error appears *only* from actual typing, never from
            //    programmatic title resets or initialisation. ──
            let entry = self.title_entry.get();
            let pw = popover_weak.clone();
            entry.connect_changed(move |_| {
                if let Some(p) = pw.upgrade() {
                    p.imp().on_title_text_changed();
                }
            });

            // ── Enter in the title → save or Edit Details placeholder. ──
            let pw = popover_weak.clone();
            entry.connect_entry_activated(move |_| {
                if let Some(p) = pw.upgrade() {
                    p.imp().on_entry_activated();
                }
            });

            // ── Save button click ──
            let save_btn = self.save_button.get();
            let pw = popover_weak.clone();
            save_btn.connect_clicked(move |_| {
                if let Some(p) = pw.upgrade() {
                    p.imp().on_save_clicked();
                }
            });

            // ── Edit Details click → fire placeholder callback. ──
            let edit_btn = self.edit_details_button.get();
            let pw = popover_weak.clone();
            edit_btn.connect_clicked(move |_| {
                if let Some(p) = pw.upgrade() {
                    p.imp().fire_edit_details();
                }
            });

            // ── Show → grab focus + select-all.
            //    Mirrors reference's show => gtk_widget_grab_focus. ──
            let pw = popover_weak.clone();
            self.obj().connect_show(move |_| {
                if let Some(p) = pw.upgrade() {
                    let imp = p.imp();
                    imp.title_entry.grab_focus();
                    imp.title_entry.select_region(0, -1);
                }
            });

            // ── Closed → single cleanup path.
            //    Mirrors reference's gcal_quick_add_popover_closed
            //    (clear text, remove error, clear date). ──
            let pw = popover_weak.clone();
            self.obj().connect_closed(move |_| {
                if let Some(p) = pw.upgrade() {
                    p.imp().on_closed();
                }
            });
        }
    }

    impl WidgetImpl for QuickAddPopover {}
    impl PopoverImpl for QuickAddPopover {}
}

glib::wrapper! {
    pub struct QuickAddPopover(ObjectSubclass<imp::QuickAddPopover>)
        @extends gtk::Popover, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::ShortcutManager;
}

impl Default for QuickAddPopover {
    fn default() -> Self {
        Self::new()
    }
}

impl QuickAddPopover {
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Filter the supplied calendars to visible + non-read-only and rebuild
    /// the calendar list.  Callers invoke this before each `popup()`.
    ///
    /// When no writable calendars are available, the scrollable list is
    /// replaced with an explanatory label, the title entry is disabled, and
    /// Save stays insensitive.  The label uses accurate wording that does
    /// not reference non-existent sidebar management.
    pub fn set_calendars(&self, calendars: &[Calendar]) {
        let imp = self.imp();

        let mut filtered: Vec<Calendar> = calendars
            .iter()
            .filter(|c| c.visible && !c.read_only)
            .cloned()
            .collect();
        // Stable order: by name then id.
        filtered.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));
        *imp.calendars.borrow_mut() = filtered;

        // Clear and rebuild the calendars_list_box.
        while let Some(child) = imp.calendars_list_box.first_child() {
            imp.calendars_list_box.remove(&child);
        }

        let cal_refs: Vec<Calendar> = imp.calendars.borrow().clone();

        if cal_refs.is_empty() {
            // No writable calendar: show explanatory label, hide list.
            imp.calendar_scroll.set_visible(false);
            imp.no_calendars_label.set_visible(true);
            imp.title_entry.set_sensitive(false);
            *imp.selected_calendar_id.borrow_mut() = None;
        } else {
            imp.calendar_scroll.set_visible(true);
            imp.no_calendars_label.set_visible(false);
            imp.title_entry.set_sensitive(true);

            // Build radio group: first button is the leader.
            let mut first: Option<gtk::CheckButton> = None;
            for cal in &cal_refs {
                let row = adw::ActionRow::builder()
                    .title(&cal.name)
                    .activatable(true)
                    .selectable(false)
                    .build();

                // Color swatch on the left.
                let swatch = gtk::Box::builder()
                    .width_request(14)
                    .height_request(14)
                    .valign(gtk::Align::Center)
                    .css_classes(["calendar-color-swatch"])
                    .build();
                apply_color_class(&swatch, &cal.color);
                row.add_prefix(&swatch);

                // Radio-style check button on the right.
                let check = gtk::CheckButton::builder()
                    .valign(gtk::Align::Center)
                    .active(false)
                    .can_focus(false)
                    .can_target(false)
                    .build();
                if let Some(leader) = first.as_ref() {
                    check.set_group(Some(leader));
                } else {
                    first = Some(check.clone());
                }
                row.add_suffix(&check);
                row.set_activatable_widget(Some(&check));

                let cal_id = cal.id;
                let popover_weak = self.downgrade();
                check.connect_toggled(move |btn| {
                    if !btn.is_active() {
                        return;
                    }
                    if let Some(p) = popover_weak.upgrade() {
                        *p.imp().selected_calendar_id.borrow_mut() = Some(cal_id);
                        p.imp().refresh_save_sensitive();
                    }
                });

                imp.calendars_list_box.append(&row);
            }

            // Preselect the first calendar.
            if let Some(leader) = first.as_ref() {
                *imp.selected_calendar_id.borrow_mut() = Some(cal_refs[0].id);
                leader.set_active(true);
            }
        }

        // Update save sensitivity only — do NOT touch the error class.
        imp.refresh_save_sensitive();
    }

    /// Set the date for which the next event will be created and refresh
    /// the date-context header (GNOME's `get_date_string_for_day` wording).
    pub fn set_date(&self, date: NaiveDate) {
        let imp = self.imp();
        imp.current_date.set(Some(date));
        imp.date_label
            .set_label(&get_date_string_for_day(date, imp.today.get()));
    }

    /// Register the save callback.  Fired with (trimmed_title, calendar_id,
    /// date).  The host is responsible for building the event via the pure
    /// model seam and persisting it.
    pub fn set_on_save<F: Fn(String, Uuid, NaiveDate) + 'static>(&self, f: F) {
        *self.imp().on_save.borrow_mut() = Some(Box::new(f));
    }

    /// Register the Edit Details placeholder callback.
    pub fn set_on_edit_details<F: Fn() + 'static>(&self, f: F) {
        *self.imp().on_edit_details.borrow_mut() = Some(Box::new(f));
    }
}

// ── Private helpers on the implementation struct ──

impl imp::QuickAddPopover {
    /// Update save button sensitivity from current title + calendar
    /// selection state.  Does **not** touch the error class; that is
    /// managed exclusively by `on_title_text_changed` so a freshly opened
    /// popover with an empty clean title never starts red.
    fn refresh_save_sensitive(&self) {
        let title = self.title_entry.text().trim().to_string();
        let has_cal = self.selected_calendar_id.borrow().is_some();
        let can_save = !title.is_empty() && has_cal;
        self.save_button.set_sensitive(can_save);
    }

    /// Fired on every text change.  Updates save sensitivity AND mirrors
    /// the reference's `summary_entry_text_changed` error toggling.
    fn on_title_text_changed(&self) {
        self.refresh_save_sensitive();
        let title = self.title_entry.text().trim().to_string();
        if title.is_empty() {
            self.title_entry.add_css_class("error");
        } else {
            self.title_entry.remove_css_class("error");
        }
    }

    /// Enter in the title.  If valid → save; otherwise → fire Edit Details
    /// placeholder (mirrors `summary_entry_activated`).
    fn on_entry_activated(&self) {
        let title = self.title_entry.text().trim().to_string();
        if !title.is_empty()
            && self.selected_calendar_id.borrow().is_some()
            && self.current_date.get().is_some()
        {
            self.fire_save();
        } else {
            self.fire_edit_details();
        }
    }

    /// Save button clicked.
    fn on_save_clicked(&self) {
        self.fire_save();
    }

    /// Closed → single cleanup path (mirrors gcal_quick_add_popover_closed).
    fn on_closed(&self) {
        self.title_entry.set_text("");
        self.title_entry.remove_css_class("error");
        self.current_date.set(None);
    }

    /// Fire the save callback with (trimmed_title, calendar_id, date).
    fn fire_save(&self) {
        let title = self.title_entry.text().trim().to_string();
        if title.is_empty() {
            self.refresh_save_sensitive();
            return;
        }
        let Some(cal_id) = *self.selected_calendar_id.borrow() else {
            return;
        };
        let Some(date) = self.current_date.get() else {
            return;
        };

        if let Some(cb) = self.on_save.borrow().as_ref() {
            cb(title, cal_id, date);
        }
    }

    fn fire_edit_details(&self) {
        if let Some(cb) = self.on_edit_details.borrow().as_ref() {
            cb();
        }
    }
}

// ── Free helpers ──

/// Apply a CSS class encoding the calendar color as a swatch dot.
fn apply_color_class(swatch: &gtk::Box, color: &str) {
    let norm = color.trim_start_matches('#').to_ascii_lowercase();
    let known = [
        ("3366cc", 1),
        ("cc3333", 2),
        ("33aa55", 3),
        ("dd8833", 4),
        ("9966cc", 5),
        ("339999", 6),
        ("999999", 7),
    ];
    let class = match known.iter().find(|(hex, _)| *hex == norm) {
        Some((_, n)) => format!("calendar-color-{n}"),
        None => "calendar-color-7".to_string(),
    };
    swatch.add_css_class(&class);
}

/// GNOME-style header wording for a single-day base event
/// (cf. `get_date_string_for_day` in gcal-quick-add-popover.c).
///
///   0 days:  "New Event Today"
///  -1 days:  "New Event Tomorrow"
///   1 day:   "New Event Yesterday"
///  -2..-6:   "New Event Monday" etc.
///  -7 or less: explicit month/day
fn get_date_string_for_day(date: NaiveDate, today: NaiveDate) -> String {
    let diff = date - today;
    let n_days = -diff.num_days();

    match n_days {
        0 => "New Event Today".to_string(),
        -1 => "New Event Tomorrow".to_string(),
        1 => "New Event Yesterday".to_string(),
        -6..=-2 => {
            let weekday = weekday_name(date.weekday().num_days_from_sunday());
            format!("New Event {weekday}")
        }
        _ => {
            let month = month_name(date.month());
            format!("New Event on {month} {}", date.day())
        }
    }
}

fn weekday_name(sun0: u32) -> &'static str {
    match sun0 {
        0 => "Sunday",
        1 => "Monday",
        2 => "Tuesday",
        3 => "Wednesday",
        4 => "Thursday",
        5 => "Friday",
        6 => "Saturday",
        _ => unreachable!(),
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

#[cfg(test)]
mod tests {
    //! Acceptance test for the pure Quick Add header formatter.
    //!
    //! `get_date_string_for_day` is a free function in this module; calling
    //! it directly does not require GTK initialization, the GResource, or
    //! any popover/window construction. The test pins the wording of the
    //! Quick Add popover's date-context header from deterministic
    //! `NaiveDate` literals.
    use super::get_date_string_for_day;
    use chrono::NaiveDate;

    #[test]
    fn get_date_string_for_day_header_wording() {
        // Fixed reference "today" — Monday 2026-07-20. Every other date
        // is derived from this anchor as a deterministic offset.
        let today = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
        let d = |y: i32, m: u32, day: u32| NaiveDate::from_ymd_opt(y, m, day).unwrap();

        // (label, target date, expected header wording)
        let cases: &[(&str, NaiveDate, &str)] = &[
            ("same day", d(2026, 7, 20), "New Event Today"),
            ("tomorrow", d(2026, 7, 21), "New Event Tomorrow"),
            ("yesterday", d(2026, 7, 19), "New Event Yesterday"),
            // Future 2..=6 days: full weekday, no "next".
            ("+2 days (Wed)", d(2026, 7, 22), "New Event Wednesday"),
            ("+3 days (Thu)", d(2026, 7, 23), "New Event Thursday"),
            ("+4 days (Fri)", d(2026, 7, 24), "New Event Friday"),
            ("+5 days (Sat)", d(2026, 7, 25), "New Event Saturday"),
            ("+6 days (Sun)", d(2026, 7, 26), "New Event Sunday"),
            // Exactly 7 days and beyond: explicit "Month Day" wording.
            ("+7 days", d(2026, 7, 27), "New Event on July 27"),
            (
                "+31 days (next month)",
                d(2026, 8, 20),
                "New Event on August 20",
            ),
            ("+365 days", d(2027, 7, 20), "New Event on July 20"),
            // Past dates older than yesterday: explicit "Month Day".
            ("-2 days", d(2026, 7, 18), "New Event on July 18"),
            (
                "-35 days (last month)",
                d(2026, 6, 15),
                "New Event on June 15",
            ),
        ];

        for (label, date, expected) in cases {
            let got = get_date_string_for_day(*date, today);
            assert_eq!(
                got, *expected,
                "{label}: get_date_string_for_day({date}, today={today})",
            );
        }
    }
}
