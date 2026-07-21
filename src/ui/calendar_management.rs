use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::backend::caldav::{CaldavDiscovery, DiscoveryWorkerError, discover_on_worker};
use calendar::backend::credentials::{CredentialError, delete_on_worker, store_on_worker};
use calendar::backend::sync::{
    InitialPullSummary, InitialPullWorkerError, initial_pull_after_provisioning_on_worker,
};
use calendar::model::{Account, Calendar, CalendarSource, validate_calendar};
use gtk::glib;
use oo7::Secret;
use std::path::PathBuf;
use uuid::Uuid;

type ListCalendarsFn = Box<dyn Fn() -> Vec<Calendar>>;
type ListAccountsFn = Box<dyn Fn() -> Vec<Account>>;
type SaveCalendarFn = Box<dyn Fn(&Calendar) -> Result<(), String>>;
type UpdateCalendarFn = Box<dyn Fn(&Calendar) -> Result<(), String>>;
type DeleteCalendarFn = Box<dyn Fn(Uuid) -> Result<(), String>>;
type DeleteAccountFn = Box<dyn Fn(Uuid) -> Result<(), String>>;
type ProvisionCaldavFn = Box<dyn Fn(&Account, &CaldavDiscovery) -> Result<PathBuf, String>>;
type RefreshViewsFn = Box<dyn Fn()>;

const COLOR_PRESETS: [(&str, &str); 7] = [
    ("Blue", "#62a0ea"),
    ("Red", "#f66151"),
    ("Green", "#57e389"),
    ("Orange", "#ffbe6f"),
    ("Purple", "#dc8add"),
    ("Teal", "#5bc8c9"),
    ("Gray", "#becedd"),
];

#[derive(Clone)]
struct OnlineAccountPage {
    page: adw::NavigationPage,
    name: adw::EntryRow,
    server_url: adw::EntryRow,
    username: adw::EntryRow,
    password: adw::PasswordEntryRow,
    error: gtk::Label,
    connect: gtk::Button,
    cancel: gtk::Button,
    spinner: gtk::Spinner,
}

enum OnlineWorker {
    Discovery {
        receiver: Receiver<Result<CaldavDiscovery, DiscoveryWorkerError>>,
        account: Account,
        password: Secret,
    },
    Store {
        receiver: Receiver<Result<(), CredentialError>>,
        account: Account,
        discovery: CaldavDiscovery,
        password: Secret,
    },
    InitialPull {
        receiver: Receiver<Result<InitialPullSummary, InitialPullWorkerError>>,
    },
    Cleanup {
        receiver: Receiver<Result<(), CredentialError>>,
    },
}

enum OnlinePoll {
    Discovery(Result<Result<CaldavDiscovery, DiscoveryWorkerError>, TryRecvError>),
    Store(Result<Result<(), CredentialError>, TryRecvError>),
    InitialPull(Result<Result<InitialPullSummary, InitialPullWorkerError>, TryRecvError>),
    Cleanup(Result<Result<(), CredentialError>, TryRecvError>),
}

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/calendar-management.ui")]
    pub struct CalendarManagementDialog {
        #[template_child]
        pub navigation_view: TemplateChild<adw::NavigationView>,
        pub main_page: RefCell<Option<adw::NavigationPage>>,
        pub calendars_list: RefCell<Option<gtk::ListBox>>,
        pub accounts_list: RefCell<Option<gtk::ListBox>>,
        pub list_calendars: RefCell<Option<ListCalendarsFn>>,
        pub list_accounts: RefCell<Option<ListAccountsFn>>,
        pub on_save: RefCell<Option<SaveCalendarFn>>,
        pub on_update: RefCell<Option<UpdateCalendarFn>>,
        pub on_delete: RefCell<Option<DeleteCalendarFn>>,
        pub on_delete_account: RefCell<Option<DeleteAccountFn>>,
        pub on_provision_caldav: RefCell<Option<ProvisionCaldavFn>>,
        pub on_refresh_views: RefCell<Option<RefreshViewsFn>>,
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

    pub fn set_list_accounts<F: Fn() -> Vec<Account> + 'static>(&self, callback: F) {
        *self.imp().list_accounts.borrow_mut() = Some(Box::new(callback));
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

    pub fn set_on_delete_account<F: Fn(Uuid) -> Result<(), String> + 'static>(&self, callback: F) {
        *self.imp().on_delete_account.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_on_provision_caldav<
        F: Fn(&Account, &CaldavDiscovery) -> Result<PathBuf, String> + 'static,
    >(
        &self,
        callback: F,
    ) {
        *self.imp().on_provision_caldav.borrow_mut() = Some(Box::new(callback));
    }

    pub fn set_on_refresh_views<F: Fn() + 'static>(&self, callback: F) {
        *self.imp().on_refresh_views.borrow_mut() = Some(Box::new(callback));
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

        let accounts_group = adw::PreferencesGroup::new();
        accounts_group.set_title("Online Accounts");
        let accounts_list = gtk::ListBox::new();
        accounts_list.add_css_class("boxed-list");
        accounts_list.set_selection_mode(gtk::SelectionMode::None);
        accounts_group.add(&accounts_list);
        page.add(&accounts_group);

        let group = adw::PreferencesGroup::new();
        group.set_title("Calendars");
        let list = gtk::ListBox::new();
        list.add_css_class("boxed-list");
        list.set_selection_mode(gtk::SelectionMode::None);
        group.add(&list);
        page.add(&group);
        toolbar.set_content(Some(&page));
        *self.imp().calendars_list.borrow_mut() = Some(list.clone());
        *self.imp().accounts_list.borrow_mut() = Some(accounts_list);

        let add_row = adw::ButtonRow::new();
        add_row.set_title("Add Calendar");
        add_row.set_end_icon_name(Some("go-next-symbolic"));
        add_row.add_css_class("suggested-action");
        let online_row = adw::ButtonRow::new();
        online_row.set_title("Add Online Account");
        online_row.set_end_icon_name(Some("go-next-symbolic"));
        let add_list = gtk::ListBox::new();
        add_list.set_selection_mode(gtk::SelectionMode::None);
        add_list.add_css_class("boxed-list");
        add_list.set_margin_start(12);
        add_list.set_margin_end(12);
        add_list.set_margin_top(12);
        add_list.set_margin_bottom(12);
        add_list.append(&add_row);
        add_list.append(&online_row);
        toolbar.add_bottom_bar(&add_list);

        let dialog_weak = self.downgrade();
        add_row.connect_activated(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.open_new_calendar();
            }
        });

        let dialog_weak = self.downgrade();
        online_row.connect_activated(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.open_online_account();
            }
        });

        let navigation_page = adw::NavigationPage::new(&toolbar, "Calendars");
        navigation_page.set_tag(Some("calendars"));
        navigation_page
    }

    fn populate_calendars_page(&self) {
        self.populate_accounts_page();
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

    fn populate_accounts_page(&self) {
        let Some(list) = self.imp().accounts_list.borrow().clone() else {
            return;
        };
        while let Some(child) = list.first_child() {
            list.remove(&child);
        }

        let mut accounts = self
            .imp()
            .list_accounts
            .borrow()
            .as_ref()
            .map(|callback| callback())
            .unwrap_or_default();
        accounts.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.id.cmp(&b.id)));

        for account in accounts {
            list.append(&self.account_row(&account));
        }
        if list.first_child().is_none() {
            let empty = adw::ActionRow::new();
            empty.set_title("No online accounts");
            empty.set_subtitle("Add an online account to sync calendars.");
            empty.set_sensitive(false);
            list.append(&empty);
        }
    }

    fn account_row(&self, account: &Account) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_title(&account.name);
        row.set_subtitle(&account.server_url);
        row.set_activatable(true);

        let arrow = gtk::Image::from_icon_name("go-next-symbolic");
        arrow.set_valign(gtk::Align::Center);
        arrow.set_tooltip_text(Some("Open account details"));
        row.add_suffix(&arrow);

        let account_id = account.id;
        let dialog_weak = self.downgrade();
        row.connect_activated(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.open_account_details(account_id);
            }
        });
        row
    }

    fn calendar_row(&self, calendar: &Calendar) -> adw::ActionRow {
        let row = adw::ActionRow::new();
        row.set_title(&calendar.name);
        row.set_subtitle(match (&calendar.source, calendar.read_only) {
            (CalendarSource::CalDav { .. }, true) => "Read-only online calendar",
            (CalendarSource::CalDav { .. }, false) => "Online calendar",
            (CalendarSource::Local, true) => "Read-only local calendar",
            (CalendarSource::Local, false) => "Local calendar",
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

    fn open_online_account(&self) {
        let controls = online_account_page();
        let flow: Rc<RefCell<Option<OnlineWorker>>> = Rc::new(RefCell::new(None));
        let account_id = Uuid::new_v4();

        let dialog_weak = self.downgrade();
        let connect_controls = controls.clone();
        let connect_flow = flow.clone();
        controls.connect.connect_clicked(move |_| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let name = connect_controls.name.text().trim().to_owned();
            let server_url = connect_controls.server_url.text().trim().to_owned();
            let username = connect_controls.username.text().trim().to_owned();
            let password_text = connect_controls.password.text().to_string();
            let valid_server_url = reqwest::Url::parse(&server_url).is_ok_and(|url| {
                url.host_str().is_some() && matches!(url.scheme(), "http" | "https")
            });
            if name.is_empty()
                || server_url.is_empty()
                || !valid_server_url
                || username.is_empty()
                || password_text.is_empty()
            {
                show_inline_error(
                    &connect_controls.error,
                    "Enter an account name, an HTTP(S) server URL, username, and password.",
                );
                return;
            }

            connect_controls.set_busy(true);
            connect_controls.error.set_visible(false);
            let account = Account {
                id: account_id,
                name,
                server_url: server_url.clone(),
                username: username.clone(),
                enabled: true,
            };
            let password = Secret::text(password_text);
            let receiver = discover_on_worker(server_url, username, password.clone());
            *connect_flow.borrow_mut() = Some(OnlineWorker::Discovery {
                receiver,
                account,
                password,
            });
            poll_online_account(
                dialog.downgrade(),
                connect_controls.clone(),
                connect_flow.clone(),
            );
        });

        let dialog_weak = self.downgrade();
        let cancel_controls = controls.clone();
        controls.cancel.connect_clicked(move |_| {
            cancel_controls.password.set_text("");
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.imp().navigation_view.pop();
            }
        });

        let password = controls.password.clone();
        let popped_flow = flow.clone();
        let navigation_view = self.imp().navigation_view.clone();
        let navigation_weak = navigation_view.downgrade();
        let popped_handler = Rc::new(RefCell::new(None));
        let popped_handler_clone = popped_handler.clone();
        let handler = navigation_view.connect_popped(move |_, page| {
            if page.tag().as_deref() == Some("new-online-account") {
                password.set_text("");
                popped_flow.borrow_mut().take();
                if let (Some(navigation), Some(handler)) = (
                    navigation_weak.upgrade(),
                    popped_handler_clone.borrow_mut().take(),
                ) {
                    navigation.disconnect(handler);
                }
            }
        });
        *popped_handler.borrow_mut() = Some(handler);

        self.imp().navigation_view.push(&controls.page);
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

    fn open_account_details(&self, id: Uuid) {
        let Some(account) = self
            .imp()
            .list_accounts
            .borrow()
            .as_ref()
            .and_then(|callback| callback().into_iter().find(|account| account.id == id))
        else {
            return;
        };

        let (page, remove) = account_details_page(&account);
        let dialog_weak = self.downgrade();
        remove.connect_clicked(move |_| {
            if let Some(dialog) = dialog_weak.upgrade() {
                dialog.confirm_remove_account(id);
            }
        });
        self.imp().navigation_view.push(&page);
    }

    fn confirm_remove_account(&self, id: Uuid) {
        let confirmation = adw::AlertDialog::new(
            Some("Remove Account?"),
            Some(
                "This removes the account's local calendars, events, reminders, sync state, and pending operations. Data on the server is not changed.",
            ),
        );
        confirmation.add_response("cancel", "Cancel");
        confirmation.add_response("remove", "Remove Account");
        confirmation.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
        confirmation.set_close_response("cancel");
        let dialog_weak = self.downgrade();
        confirmation.connect_response(Some("remove"), move |_, _| {
            let Some(dialog) = dialog_weak.upgrade() else {
                return;
            };
            let result = dialog
                .imp()
                .on_delete_account
                .borrow()
                .as_ref()
                .map(|callback| callback(id))
                .unwrap_or_else(|| Err("Account storage is unavailable.".to_string()));
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

impl OnlineAccountPage {
    fn set_busy(&self, busy: bool) {
        self.name.set_sensitive(!busy);
        self.server_url.set_sensitive(!busy);
        self.username.set_sensitive(!busy);
        self.password.set_sensitive(!busy);
        self.cancel.set_sensitive(!busy);
        self.connect.set_sensitive(!busy);
        self.page.set_can_pop(!busy);
        self.spinner.set_visible(busy);
        self.spinner.set_spinning(busy);
    }
}

fn online_account_page() -> OnlineAccountPage {
    let name = adw::EntryRow::new();
    name.set_title("Account Display Name");
    let server_url = adw::EntryRow::new();
    server_url.set_title("Server URL");
    server_url.set_text("https://");
    let username = adw::EntryRow::new();
    username.set_title("Username");
    let password = adw::PasswordEntryRow::new();
    password.set_title("Password");

    let error = error_label();
    let group = adw::PreferencesGroup::new();
    group.set_title("Connect an Online Account");
    group.add(&name);
    group.add(&server_url);
    group.add(&username);
    group.add(&password);
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

    let cancel = gtk::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    let connect = gtk::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    let spinner = gtk::Spinner::new();
    spinner.set_visible(false);
    let action_bar = gtk::ActionBar::new();
    action_bar.pack_start(&cancel);
    action_bar.pack_end(&spinner);
    action_bar.pack_end(&connect);
    toolbar.add_bottom_bar(&action_bar);

    let page = adw::NavigationPage::new(&toolbar, "Add Online Account");
    page.set_tag(Some("new-online-account"));
    OnlineAccountPage {
        page,
        name,
        server_url,
        username,
        password,
        error,
        connect,
        cancel,
        spinner,
    }
}

fn poll_online_account(
    dialog_weak: glib::WeakRef<CalendarManagementDialog>,
    controls: OnlineAccountPage,
    flow: Rc<RefCell<Option<OnlineWorker>>>,
) {
    glib::timeout_add_local(Duration::from_millis(100), move || {
        let poll = {
            let state = flow.borrow();
            match state.as_ref() {
                Some(OnlineWorker::Discovery { receiver, .. }) => {
                    OnlinePoll::Discovery(receiver.try_recv())
                }
                Some(OnlineWorker::Store { receiver, .. }) => {
                    OnlinePoll::Store(receiver.try_recv())
                }
                Some(OnlineWorker::InitialPull { receiver }) => {
                    OnlinePoll::InitialPull(receiver.try_recv())
                }
                Some(OnlineWorker::Cleanup { receiver }) => {
                    OnlinePoll::Cleanup(receiver.try_recv())
                }
                None => return glib::ControlFlow::Break,
            }
        };

        match poll {
            OnlinePoll::Discovery(Err(TryRecvError::Empty))
            | OnlinePoll::Store(Err(TryRecvError::Empty))
            | OnlinePoll::InitialPull(Err(TryRecvError::Empty))
            | OnlinePoll::Cleanup(Err(TryRecvError::Empty)) => glib::ControlFlow::Continue,
            OnlinePoll::Discovery(Err(TryRecvError::Disconnected)) => {
                flow.borrow_mut().take();
                controls.set_busy(false);
                show_inline_error(
                    &controls.error,
                    "The CalDAV discovery worker stopped unexpectedly.",
                );
                controls.password.set_text("");
                glib::ControlFlow::Break
            }
            OnlinePoll::Store(Err(TryRecvError::Disconnected)) => {
                flow.borrow_mut().take();
                controls.set_busy(false);
                show_inline_error(
                    &controls.error,
                    "The credential store worker stopped unexpectedly.",
                );
                controls.password.set_text("");
                glib::ControlFlow::Break
            }
            OnlinePoll::Cleanup(Err(TryRecvError::Disconnected)) | OnlinePoll::Cleanup(Ok(_)) => {
                flow.borrow_mut().take();
                controls.set_busy(false);
                glib::ControlFlow::Break
            }
            OnlinePoll::Discovery(Ok(Err(error))) => {
                flow.borrow_mut().take();
                controls.set_busy(false);
                show_inline_error(&controls.error, discovery_error_message(error));
                controls.password.set_text("");
                glib::ControlFlow::Break
            }
            OnlinePoll::Discovery(Ok(Ok(discovery))) => {
                let Some(OnlineWorker::Discovery {
                    account, password, ..
                }) = flow.borrow_mut().take()
                else {
                    return glib::ControlFlow::Break;
                };
                let receiver = store_on_worker(account.id, password.clone());
                *flow.borrow_mut() = Some(OnlineWorker::Store {
                    receiver,
                    account,
                    discovery,
                    password,
                });
                glib::ControlFlow::Continue
            }
            OnlinePoll::Store(Ok(Err(_))) => {
                let Some(OnlineWorker::Store { password, .. }) = flow.borrow_mut().take() else {
                    return glib::ControlFlow::Break;
                };
                drop(password);
                controls.set_busy(false);
                show_inline_error(
                    &controls.error,
                    "Could not securely store the account password.",
                );
                controls.password.set_text("");
                glib::ControlFlow::Break
            }
            OnlinePoll::Store(Ok(Ok(()))) => {
                let Some(OnlineWorker::Store {
                    account,
                    discovery,
                    password,
                    ..
                }) = flow.borrow_mut().take()
                else {
                    return glib::ControlFlow::Break;
                };
                let result = dialog_weak
                    .upgrade()
                    .map(|dialog| {
                        dialog
                            .imp()
                            .on_provision_caldav
                            .borrow()
                            .as_ref()
                            .map(|callback| callback(&account, &discovery))
                            .unwrap_or_else(|| Err("Calendar storage is unavailable.".to_string()))
                    })
                    .unwrap_or_else(|| {
                        Err("The calendar window is no longer available.".to_string())
                    });
                match result {
                    Ok(database_path) => {
                        let receiver = initial_pull_after_provisioning_on_worker(
                            database_path,
                            account,
                            password,
                        );
                        *flow.borrow_mut() = Some(OnlineWorker::InitialPull { receiver });
                        controls.set_busy(true);
                    }
                    Err(message) => {
                        drop(password);
                        controls.password.set_text("");
                        let receiver = delete_on_worker(account.id);
                        *flow.borrow_mut() = Some(OnlineWorker::Cleanup { receiver });
                        controls.set_busy(true);
                        show_inline_error(&controls.error, &message);
                        return glib::ControlFlow::Continue;
                    }
                }
                glib::ControlFlow::Continue
            }
            OnlinePoll::InitialPull(Err(TryRecvError::Disconnected)) => {
                flow.borrow_mut().take();
                controls.password.set_text("");
                controls.set_busy(false);
                show_inline_error(
                    &controls.error,
                    "The account was added, but its initial calendar import stopped unexpectedly. Try again later.",
                );
                glib::ControlFlow::Break
            }
            OnlinePoll::InitialPull(Ok(Err(error))) => {
                flow.borrow_mut().take();
                controls.password.set_text("");
                controls.set_busy(false);
                show_inline_error(&controls.error, initial_pull_error_message(error));
                glib::ControlFlow::Break
            }
            OnlinePoll::InitialPull(Ok(Ok(_summary))) => {
                flow.borrow_mut().take();
                controls.password.set_text("");
                controls.set_busy(false);
                if let Some(dialog) = dialog_weak.upgrade() {
                    if let Some(callback) = dialog.imp().on_refresh_views.borrow().as_ref() {
                        callback();
                    }
                    dialog.refresh();
                }
                glib::ControlFlow::Break
            }
        }
    });
}

fn discovery_error_message(error: DiscoveryWorkerError) -> &'static str {
    match error {
        DiscoveryWorkerError::InvalidCredential => "The password is not a valid text credential.",
        DiscoveryWorkerError::Http => "Could not connect to the CalDAV server.",
        DiscoveryWorkerError::Parse => "The CalDAV server returned an invalid response.",
        DiscoveryWorkerError::WorkerPanic => "The CalDAV discovery worker failed.",
    }
}

fn initial_pull_error_message(error: InitialPullWorkerError) -> &'static str {
    match error {
        InitialPullWorkerError::InvalidCredential => {
            "The account was added, but its password could not be used for import. Check the password and try again."
        }
        InitialPullWorkerError::Caldav => {
            "The account was added, but its calendars could not be imported. Check the server connection and try again."
        }
        InitialPullWorkerError::MissingCalendarSyncState => {
            "The account was added, but calendar sync setup is incomplete. Remove and add the account again."
        }
        InitialPullWorkerError::Repository => {
            "The account was added, but imported events could not be saved. Check available disk space and try again."
        }
        InitialPullWorkerError::WorkerPanic => {
            "The account was added, but its initial calendar import failed unexpectedly. Try again later."
        }
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
    remove.set_visible(!calendar.read_only && calendar.source == CalendarSource::Local);
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

fn account_details_page(account: &Account) -> (adw::NavigationPage, gtk::Button) {
    let name = adw::ActionRow::new();
    name.set_title("Account Name");
    name.set_subtitle(&account.name);
    let server = adw::ActionRow::new();
    server.set_title("Server");
    server.set_subtitle(&account.server_url);
    let username = adw::ActionRow::new();
    username.set_title("Username");
    username.set_subtitle(&account.username);

    let group = adw::PreferencesGroup::new();
    group.set_title("Online Account");
    group.add(&name);
    group.add(&server);
    group.add(&username);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_margin_start(18);
    content.set_margin_end(18);
    content.set_margin_top(18);
    content.set_margin_bottom(18);
    content.append(&group);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));

    let remove = gtk::Button::with_label("Remove Account");
    remove.add_css_class("destructive-action");
    let action_bar = gtk::ActionBar::new();
    action_bar.pack_start(&remove);
    toolbar.add_bottom_bar(&action_bar);

    let page = adw::NavigationPage::new(&toolbar, "Account Details");
    page.set_tag(Some("account-details"));
    (page, remove)
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
