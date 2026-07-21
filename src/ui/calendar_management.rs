use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::model::{Calendar, CalendarSource, validate_calendar};
use gtk::glib;
use uuid::Uuid;

type ListCalendarsFn = Box<dyn Fn() -> Vec<Calendar>>;
type SaveCalendarFn = Box<dyn Fn(&Calendar) -> Result<(), String>>;
type UpdateCalendarFn = Box<dyn Fn(&Calendar) -> Result<(), String>>;
type DeleteCalendarFn = Box<dyn Fn(Uuid) -> Result<(), String>>;

const COLOR_PRESETS: [(&str, &str); 7] = [
    ("Blue", "#62a0ea"),
    ("Red", "#f66151"),
    ("Green", "#57e389"),
    ("Orange", "#ffbe6f"),
    ("Purple", "#dc8add"),
    ("Teal", "#5bc8c9"),
    ("Gray", "#becedd"),
];

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/calendar-management.ui")]
    pub struct CalendarManagementDialog {
        #[template_child]
        pub navigation_view: TemplateChild<adw::NavigationView>,
        pub main_page: RefCell<Option<adw::NavigationPage>>,
        pub calendars_list: RefCell<Option<gtk::ListBox>>,
        pub list_calendars: RefCell<Option<ListCalendarsFn>>,
        pub on_save: RefCell<Option<SaveCalendarFn>>,
        pub on_update: RefCell<Option<UpdateCalendarFn>>,
        pub on_delete: RefCell<Option<DeleteCalendarFn>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarManagementDialog {
        const NAME: &'static str = "CalendarManagementDialog";
        type Type = super::CalendarManagementDialog;
        type ParentType = adw::Dialog;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CalendarManagementDialog {}
    impl WidgetImpl for CalendarManagementDialog {}
    impl AdwDialogImpl for CalendarManagementDialog {}
}

glib::wrapper! {
    pub struct CalendarManagementDialog(ObjectSubclass<imp::CalendarManagementDialog>)
        @extends adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl CalendarManagementDialog {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_list_calendars<F: Fn() -> Vec<Calendar> + 'static>(&self, callback: F) {
        *self.imp().list_calendars.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_on_save<F: Fn(&Calendar) -> Result<(), String> + 'static>(&self, callback: F) {
        *self.imp().on_save.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_on_update<F: Fn(&Calendar) -> Result<(), String> + 'static>(&self, callback: F) {
        *self.imp().on_update.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_on_delete<F: Fn(Uuid) -> Result<(), String> + 'static>(&self, callback: F) {
        *self.imp().on_delete.borrow_mut() = Some(Box::new(callback));
    }

    pub fn refresh(&self) {
        let imp = self.imp();
        if imp.main_page.borrow().is_none() {
            let page = self.build_calendars_page();
            imp.navigation_view.push(&page);
            *imp.main_page.borrow_mut() = Some(page.clone());
            self.populate_calendars_page();
            return;
        }

        imp.navigation_view.pop_to_tag("calendars");
        self.populate_calendars_page();
    }

    fn build_calendars_page(&self) -> adw::NavigationPage {
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());

        let page = adw::PreferencesPage::new();
        page.set_vexpand(true);
        let group = adw::PreferencesGroup::new();
        group.set_title("Calendars");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        group.add(&list);
        page.add(&group);
        toolbar.set_content(Some(&page));
        *self.imp().calendars_list.borrow_mut() = Some(list.clone());

        let add_row = adw::ButtonRow::new();
        add_row.set_title("Add Calendar");
        add_row.set_end_icon_name(Some("go-next-symbolic"));
        add_row.add_css_class("suggested-action");
        let add_list = gtk::ListBox::new();
        add_list.set_selection_mode(gtk::SelectionMode::None);
        add_list.add_css_class("boxed-list");
        add_list.set_margin_start(12);
        add_list.set_margin_end(12);
        add_list.set_margin_top(12);
        add_list.set_margin_bottom(12);
        add_list.append(&add_row);
        toolbar.add_bottom_bar(&add_list);

        let dialog_weak = self.downgrade();
        add_row.connect_activated(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.open_new_calendar();
            }
        });

        let navigation_page = adw::NavigationPage::new(&toolbar, "Calendars");
        navigation_page.set_tag(Some("calendars"));
        navigation_page
    }

    fn populate_calendars_page(&self) {
        let Some(list) = self.imp().calendars_list.borrow().clone() else {
            return;
        };
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let mut calendars = self
            .imp()
            .list_calendars
            .borrow()
            .as_ref()
            .map(|callback| callback())
            .unwrap_or_default();
        calendars.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        for calendar in calendars {
            list.append(&self.calendar_row(&calendar));
        }
        if list.first_child().is_none() {
            let empty = adw::ActionRow::new();
            empty.set_title("No calendars");
            empty.set_subtitle("Add a local calendar to get started.");
            empty.set_sensitive(false);
            list.append(&empty);
        }
    }

    fn calendar_row(&self, calendar: &Calendar) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_title(&calendar.name);
        row.set_subtitle(if calendar.read_only {
            "Read-only calendar"
        } else {
            "Local calendar"
        });
        row.set_activatable(true);
        row.add_prefix(&color_swatch(&calendar.color));

        if calendar.read_only {
            let read_only = gtk::Image::from_icon_name("changes-prevent-symbolic");
            read_only.set_pixel_size(18);
            read_only.set_tooltip_text(Some("Read-only calendar"));
            row.add_suffix(&read_only);
        }

        let visibility = gtk::Switch::new();
        visibility.set_valign(gtk::Align::Center);
        visibility.set_active(calendar.visible);
        visibility.set_tooltip_text(Some("Display calendar"));
        row.add_suffix(&visibility);

        let arrow = gtk::Image::from_icon_name("go-next-symbolic");
        arrow.set_valign(gtk::Align::Center);
        arrow.set_tooltip_text(Some("Edit calendar"));
        row.add_suffix(&arrow);

        let id = calendar.id;
        let dialog_weak = self.downgrade();
        row.connect_activated(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.open_edit_calendar(id);
            }
        });

        let old_visible = calendar.visible;
        let syncing = Rc::new(Cell::new(false));
        let syncing_weak = syncing.clone();
        let dialog_weak = self.downgrade();
        visibility.connect_active_notify(move |switch| {
            if syncing_weak.get() {
                return;
            }
            let visible = switch.is_active();
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let Some(mut candidate) = dialog.calendar_by_id(id) else {
                return;
            };
            candidate.visible = visible;
            let result = dialog
                .imp()
                .on_update
                .borrow()
                .as_ref()
                .map(|callback| callback(&candidate));
            if result.as_ref().is_none_or(|result| result.is_err()) {
                syncing_weak.set(true);
                switch.set_active(old_visible);
                syncing_weak.set(false);
                if let Some(Err(message)) = result {
                    dialog.show_error(&message);
                }
            } else if let Some(Ok(())) = result {
                dialog.refresh();
            }
        });
        row
    }

    fn calendar_by_id(&self, id: Uuid) -> Option<Calendar> {
        self.imp()
            .list_calendars
            .borrow()
            .as_ref()
            .and_then(|callback| callback().into_iter().find(|calendar| calendar.id == id))
    }

    fn open_new_calendar(&self) {
        let (page, name, selected, error, add) = new_calendar_page(self);
        let dialog_weak = self.downgrade();
        add.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let candidate = Calendar {
                id: Uuid::new_v4(),
                name: name.text().to_string(),
                color: selected.borrow().clone(),
                visible: true,
                read_only: false,
                source: CalendarSource::Local,
            };
            let Ok(candidate) = validate_calendar(candidate) else {
                show_inline_error(&error, "Enter a calendar name and choose a valid color.");
                return;
            };
            let result = dialog
                .imp()
                .on_save
                .borrow()
                .as_ref()
                .map(|callback| callback(&candidate))
                .unwrap_or_else(|| Err("Calendar storage is unavailable.".to_string()));
            match result {
                Ok(()) => {
                    dialog.refresh();
                }
                Err(message) => show_inline_error(&error, &message),
            }
        });
        self.imp().navigation_view.push(&page);
    }

    fn open_edit_calendar(&self, id: Uuid) {
        let Some(calendar) = self.calendar_by_id(id) else {
            return;
        };
        let (page, name, selected, visible, error, save, remove) = edit_calendar_page(&calendar);

        let dialog_weak = self.downgrade();
        save.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let mut candidate = calendar.clone();
            if !calendar.read_only {
                candidate.name = name.text().to_string();
                candidate.color = selected.borrow().clone();
            }
            candidate.visible = visible.is_active();
            let Ok(candidate) = validate_calendar(candidate) else {
                show_inline_error(&error, "Enter a calendar name and choose a valid color.");
                return;
            };
            let result = dialog
                .imp()
                .on_update
                .borrow()
                .as_ref()
                .map(|callback| callback(&candidate))
                .unwrap_or_else(|| Err("Calendar storage is unavailable.".to_string()));
            match result {
                Ok(()) => dialog.refresh(),
                Err(message) => show_inline_error(&error, &message),
            }
        });

        let dialog_weak = self.downgrade();
        remove.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            dialog.confirm_remove_calendar(id);
        });
        self.imp().navigation_view.push(&page);
    }

    fn confirm_remove_calendar(&self, id: Uuid) {
        let confirmation = adw::AlertDialog::new(
            Some("Remove Calendar?"),
            Some("This calendar and all of its events will be permanently removed."),
        );
        confirmation.add_response("cancel", "Cancel");
        confirmation.add_response("remove", "Remove Calendar");
        confirmation.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        confirmation.set_close_response("cancel");
        let dialog_weak = self.downgrade();
        confirmation.connect_response(Some("remove"), move |_, _| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let result = dialog
                .imp()
                .on_delete
                .borrow()
                .as_ref()
                .map(|callback| callback(id))
                .unwrap_or_else(|| Err("Calendar storage is unavailable.".to_string()));
            match result {
                Ok(()) => dialog.refresh(),
                Err(message) => dialog.show_error(&message),
            }
        });
        confirmation.present(Some(self.upcast_ref::<gtk::Widget>()));
    }

    fn show_error(&self, message: &str) {
        let alert = adw::AlertDialog::new(Some("Calendar Error"), Some(message));
        alert.add_response("ok", "OK");
        alert.set_close_response("ok");
        alert.present(Some(self.upcast_ref::<gtk::Widget>()));
    }
}

fn new_calendar_page(
    dialog: &CalendarManagementDialog,
) -> (
    adw::NavigationPage,
    adw::EntryRow,
    Rc<RefCell<String>>,
    gtk::Label,
    gtk::Button,
) {
    let name = adw::EntryRow::new();
    name.set_title("Calendar Name");
    name.set_show_apply_button(false);
    let selected = Rc::new(RefCell::new("#becedd".to_string()));
    let colors = color_control(selected.clone());
    let colors_row = adw::ActionRow::new();
    colors_row.set_title("Color");
    colors_row.add_suffix(&colors);

    let error = error_label();
    let group = adw::PreferencesGroup::new();
    group.set_title("Create a Local Calendar");
    group.add(&name);
    group.add(&colors_row);
    group.add(&error);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.append(&group);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_show_title(true);
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    let add = gtk::Button::with_label("Add Calendar");
    add.add_css_class("suggested-action");
    let action_bar = gtk::ActionBar::new();
    action_bar.pack_start(&cancel);
    action_bar.pack_end(&add);
    toolbar.add_bottom_bar(&action_bar);
    let dialog_weak = dialog.downgrade();
    cancel.connect_clicked(move |_| {
        if let Some(dialog) = dialog_weak.upgrade() {
            dialog.imp().navigation_view.pop();
        }
    });

    let page = adw::NavigationPage::new(&toolbar, "New Calendar");
    page.set_tag(Some("new-calendar"));
    (page, name, selected, error, add)
}

fn edit_calendar_page(
    calendar: &Calendar,
) -> (
    adw::NavigationPage,
    adw::EntryRow,
    Rc<RefCell<String>>,
    gtk::Switch,
    gtk::Label,
    gtk::Button,
    gtk::Button,
) {
    let name = adw::EntryRow::new();
    name.set_title("Calendar Name");
    name.set_text(&calendar.name);
    let selected = Rc::new(RefCell::new(calendar.color.clone()));
    let colors = color_control(selected.clone());
    let colors_row = adw::ActionRow::new();
    colors_row.set_title("Color");
    colors_row.add_suffix(&colors);
    let visible = gtk::Switch::new();
    visible.set_active(calendar.visible);
    visible.set_valign(gtk::Align::Center);
    let visible_row = adw::ActionRow::new();
    visible_row.set_title("Display Calendar");
    visible_row.add_suffix(&visible);

    if calendar.read_only {
        name.set_editable(false);
        colors_row.set_visible(false);
    }

    let error = error_label();
    let group = adw::PreferencesGroup::new();
    group.set_title(if calendar.read_only {
        "Read-only Calendar"
    } else {
        "Calendar Details"
    });
    group.add(&name);
    group.add(&colors_row);
    group.add(&visible_row);
    group.add(&error);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.append(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));

    let remove = gtk::Button::with_label("Remove Calendar");
    remove.add_css_class("destructive-action");
    remove.set_visible(!calendar.read_only);
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let action_bar = gtk::ActionBar::new();
    action_bar.pack_start(&remove);
    action_bar.pack_end(&save);
    toolbar.add_bottom_bar(&action_bar);

    let page = adw::NavigationPage::new(&toolbar, "Edit Calendar");
    page.set_tag(Some("edit-calendar"));
    (page, name, selected, visible, error, save, remove)
}

fn color_control(selected: Rc<RefCell<String>>) -> gtk::MenuButton {
    let current = gtk::MenuButton::builder()
        .has_frame(false)
        .always_show_arrow(false)
        .valign(gtk::Align::Center)
        .build();
    current.add_css_class("circular");
    current.set_child(Some(&color_swatch(&selected.borrow())));
    current.set_tooltip_text(Some(&color_description(&selected.borrow())));

    let palette = gtk::Popover::new();
    let grid = gtk::Grid::new();
    grid.set_row_spacing(6);
    grid.set_column_spacing(6);
    grid.set_margin_top(8);
    grid.set_margin_bottom(8);
    grid.set_margin_start(8);
    grid.set_margin_end(8);

    let selected_color = selected.borrow().to_ascii_lowercase();
    let first_button: Rc<RefCell<Option<gtk::ToggleButton>>> = Rc::new(RefCell::new(None));

    for (index, (label, color)) in COLOR_PRESETS.iter().enumerate() {
        let button = gtk::ToggleButton::new();
        let tooltip = format!("{label} calendar color");
        button.set_tooltip_text(Some(&tooltip));
        button.add_css_class("calendar-color-preset");
        button.set_child(Some(&color_swatch(color)));
        if let Some(first) = first_button.borrow().as_ref() {
            button.set_group(Some(first));
        } else {
            *first_button.borrow_mut() = Some(button.clone());
        }

        let color_value = (*color).to_string();
        let selected_weak = Rc::downgrade(&selected);
        let current_weak = current.downgrade();
        let palette_weak = palette.downgrade();
        button.connect_toggled(move |button| {
            if !button.is_active() {
                return;
            }
            let Some(selected) = selected_weak.upgrade() else {
                return;
            };
            *selected.borrow_mut() = color_value.clone();
            if let Some(current) = current_weak.upgrade() {
                current.set_child(Some(&color_swatch(&selected.borrow())));
                current.set_tooltip_text(Some(&format!("{label} calendar color")));
            }
            if let Some(palette) = palette_weak.upgrade() {
                palette.popdown();
            }
        });

        // Set the active state only when the existing color is this preset.
        // In particular, never replace a valid custom color with Blue.
        if selected_color == color.to_ascii_lowercase() {
            button.set_active(true);
        }
        grid.attach(&button, (index % 4) as i32, (index / 4) as i32, 1, 1);
    }

    palette.set_child(Some(&grid));
    current.set_popover(Some(&palette));
    current
}

fn error_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    label.set_halign(gtk::Align::Start);
    label.set_wrap(true);
    label.add_css_class("error");
    label.set_visible(false);
    label
}

fn show_inline_error(label: &gtk::Label, message: &str) {
    label.set_label(message);
    label.set_visible(true);
}

fn color_description(value: &str) -> String {
    COLOR_PRESETS
        .iter()
        .find(|(_, color)| color.eq_ignore_ascii_case(value))
        .map(|(label, _)| format!("{label} calendar color"))
        .unwrap_or_else(|| "Custom calendar color".to_string())
}

fn color_swatch(value: &str) -> gtk::DrawingArea {
    let color = parse_color(value);
    let swatch = gtk::DrawingArea::new();
    swatch.set_content_width(24);
    swatch.set_content_height(24);
    swatch.set_draw_func(move |area, context, width, height| {
        draw_color_swatch(area, context, width, height, color);
    });
    swatch
}

fn draw_color_swatch(
    area: &gtk::DrawingArea,
    context: &gtk::cairo::Context,
    width: i32,
    height: i32,
    color: gtk::gdk::RGBA,
) {
    let size = f64::from(width.min(height));
    let radius = (size / 2.0 - 1.0).max(0.0);
    context.arc(size / 2.0, size / 2.0, radius, 0.0, std::f64::consts::TAU);
    context.set_source_rgba(
        f64::from(color.red()),
        f64::from(color.green()),
        f64::from(color.blue()),
        1.0,
    );
    let _ = context.fill_preserve();
    context.set_source_rgba(0.0, 0.0, 0.0, 0.24);
    context.set_line_width(1.0 / f64::from(area.scale_factor().max(1)));
    let _ = context.stroke();
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
    gtk::gdk::RGBA::new(0.6, 0.6, 0.6, 1.0)
}
