mod ui;
mod window;

use adw::prelude::*;
use adw::{Application, gio};
use gtk::{gdk, glib};

const APP_ID: &str = "dev.chris.calendar";

fn main() {
    glib::set_application_name("Calendar");

    // Register embedded GResource before any template loading.
    gio::resources_register_include!("calendar.gresource").expect("Failed to register GResource");

    let app = Application::new(Some(APP_ID), gio::ApplicationFlags::empty());

    add_application_actions(&app);
    app.connect_activate(build_ui);

    app.run();
}

fn build_ui(app: &Application) {
    if let Some(display) = gdk::Display::default() {
        gtk::IconTheme::for_display(&display).add_resource_path("/dev/chris/calendar/icons/");
    }
    load_css();
    let win = window::CalendarWindow::new(app);
    app.set_accels_for_action("win.show-calendars", &["F8"]);
    gtk::prelude::GtkWindowExt::present(&win);
}

fn load_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_resource("/dev/chris/calendar/style.css");

    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn add_application_actions(app: &Application) {
    let quit = gio::SimpleAction::new("quit", None);
    let app_weak = app.downgrade();
    quit.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            app.quit();
        }
    });
    app.add_action(&quit);
    app.set_accels_for_action("app.quit", &["<primary>q"]);

    let about = gio::SimpleAction::new("about", None);
    let app_weak = app.downgrade();
    about.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            let about_dialog = adw::AboutDialog::new();
            about_dialog.set_application_name("Calendar");
            about_dialog.set_application_icon(APP_ID);
            about_dialog.set_version("0.1.0");

            if let Some(window) = app.active_window() {
                about_dialog.present(Some(window.upcast_ref::<gtk::Widget>()));
            } else {
                about_dialog.present(None::<&gtk::Widget>);
            }
        }
    });
    app.add_action(&about);

    let activate = gio::SimpleAction::new("activate", None);
    let app_weak = app.downgrade();
    activate.connect_activate(move |_, _| {
        if let Some(app) = app_weak.upgrade() {
            if let Some(window) = app.active_window() {
                window.present();
            } else {
                app.activate();
            }
        }
    });
    app.add_action(&activate);
}
