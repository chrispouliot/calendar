use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{gio, glib};

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/window.ui")]
    pub struct CalendarWindow {
        #[template_child]
        pub overlay: TemplateChild<adw::ToastOverlay>,
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub views_stack: TemplateChild<adw::ViewStack>,
        #[template_child]
        pub date_chooser_bin: TemplateChild<adw::Bin>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CalendarWindow {
        const NAME: &'static str = "CalendarWindow";
        type Type = super::CalendarWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CalendarWindow {
        fn constructed(&self) {
            self.parent_constructed();

            // Place the custom date chooser inside the sidebar bin.
            let chooser = crate::ui::date_chooser::DateChooser::new();
            self.date_chooser_bin.set_child(Some(&chooser));

            let win = self.obj();

            // Window-level placeholder actions referenced by Blueprint buttons.
            // Later phases will attach real behavior.
            let previous_date = gio::SimpleAction::new("previous-date", None);
            win.add_action(&previous_date);

            let next_date = gio::SimpleAction::new("next-date", None);
            win.add_action(&next_date);

            let today = gio::SimpleAction::new("today", None);
            win.add_action(&today);

            let new_event = gio::SimpleAction::new("new-event", None);
            win.add_action(&new_event);
        }
    }

    impl WidgetImpl for CalendarWindow {}

    impl WindowImpl for CalendarWindow {}

    impl ApplicationWindowImpl for CalendarWindow {}

    impl AdwApplicationWindowImpl for CalendarWindow {}
}

glib::wrapper! {
    pub struct CalendarWindow(ObjectSubclass<imp::CalendarWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                   gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

impl CalendarWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder()
            .property("application", Some(app))
            .build()
    }
}
