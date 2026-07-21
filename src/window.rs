use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::backend::caldav::CaldavDiscovery;
use calendar::backend::credentials::delete_on_worker;
use calendar::backend::{
    AccountRepository, CalendarRepository, EventDeletionUndo, EventRepository, RepositoryError,
    SqliteRepository,
};
use calendar::model::{
    Account, Calendar, CalendarSource, EmptyQuickAddTitle, Event, new_quick_add_event,
};
use calendar::view_state::{ViewKind, ViewState};
use chrono::{Datelike, NaiveDate};
use gtk::{gio, glib};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use uuid::Uuid;

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
        #[template_child]
        pub calendar_list_bin: TemplateChild<adw::Bin>,

        // Phase 5: Month view and navigation title.
        #[template_child]
        pub month_view_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub week_view_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub agenda_view_bin: TemplateChild<adw::Bin>,
        #[template_child]
        pub title_label: TemplateChild<gtk::Label>,

        // Phase 6: New-Event button (host of the quick-add popover).
        #[template_child]
        pub new_event_button: TemplateChild<gtk::Button>,

        // SQLite-backed persistent repository.  Initialised in
        // `constructed()`; None only when DB opening failed.
        pub repository: RefCell<Option<SqliteRepository>>,

        // The Quick-Add popover, created once and parented to the window.
        pub quick_add: RefCell<Option<crate::ui::quick_add_popover::QuickAddPopover>>,

        // The event preview popover, created once and parented to the window.
        pub event_popover: RefCell<Option<crate::ui::event_popover::EventPopover>>,

        // The reusable detailed editor, presented transiently for create/edit.
        pub event_editor: RefCell<Option<crate::ui::event_editor::EventEditor>>,

        // Reusable local calendar management dialog.
        pub calendar_management:
            RefCell<Option<crate::ui::calendar_management::CalendarManagementDialog>>,

        /// Shared navigation state for the three pages in `views_stack`.
        pub view_state: RefCell<Option<ViewState>>,
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

            *self.view_state.borrow_mut() = Some(ViewState::new(ViewKind::Month, today_local()));

            // ── Open the persistent SQLite database ──
            let db_path = Self::make_db_path();

            // All three init steps (create-dir, open, seed) are treated
            // as one fallible path.  Any failure shows the fatal dialog
            // and returns early.
            let init_result: Result<(), RepositoryError> = (|| {
                // 1. Create parent directory
                if let Some(parent) = db_path.parent() {
                    std::fs::create_dir_all(parent).map_err(|_| RepositoryError)?;
                }
                // 2. Open (or create) database
                let mut repo = SqliteRepository::open(&db_path)?;
                // 3. Let the repository atomically initialize defaults.
                let defaults = Self::default_calendars();
                repo.seed_default_calendars(&defaults)?;
                *self.repository.borrow_mut() = Some(repo);
                Ok(())
            })();

            if let Err(RepositoryError) = init_result {
                let obj = self.obj();
                Self::show_fatal_db_dialog(
                    obj.upcast_ref::<gtk::Window>(),
                    obj.application(),
                    &db_path,
                );
                return;
            }

            // ── Sidebar date chooser ──
            let chooser = crate::ui::date_chooser::DateChooser::new();
            self.date_chooser_bin.set_child(Some(&chooser));
            let win = self.obj();

            // ── Sidebar calendar list ──
            let calendar_list = crate::ui::calendar_list::CalendarList::new();
            calendar_list.set_on_visibility_changed({
                let win_weak = win.downgrade();
                move |calendar_id, visible| {
                    win_weak
                        .upgrade()
                        .is_some_and(|win| win.imp().set_calendar_visibility(calendar_id, visible))
                }
            });
            self.calendar_list_bin.set_child(Some(&calendar_list));

            // ── Create and place MonthView ──
            let month_view = crate::ui::month_view::MonthView::new();
            let win_weak = win.downgrade();

            // Connect the day-activation callback: first click on an empty
            // Month-view day opens the Quick-Add popover at the originating
            // day cell.  The (row, col) is used to compute the cell's
            // rectangle in window coordinates.  Days containing event chips
            // do not fire this callback — event-chip preview will be
            // implemented in the next unit.
            month_view.set_on_activate({
                let win_weak = win_weak.clone();
                move |row, col, date| {
                    if let Some(win) = win_weak.upgrade() {
                        let cell_rect = win
                            .imp()
                            .with_month_view(|mv| {
                                mv.day_cell_rect(row, col, win.upcast_ref::<gtk::Widget>())
                            })
                            .flatten();
                        win.imp().open_quick_add(date, cell_rect);
                    }
                }
            });

            // Connect the month-changed callback for title updates.
            month_view.set_on_month_changed({
                let win_weak = win_weak.clone();
                move |y, m| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp().reconcile_month_state(y, m);
                    }
                }
            });

            self.month_view_bin.set_child(Some(&month_view));

            // ── Create and place the current WeekView ──
            let week_view = crate::ui::week_view::WeekView::new();
            week_view.set_on_event_activate({
                let win_weak = win_weak.clone();
                move |event_id, event_widget| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp().open_event_preview(event_id, &event_widget);
                    }
                }
            });
            self.week_view_bin.set_child(Some(&week_view));

            // ── Create and place the current AgendaView ──
            let agenda_view = crate::ui::agenda_view::AgendaView::new();
            agenda_view.set_on_event_activate({
                let win_weak = win_weak.clone();
                move |event_id, event_widget| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp().open_event_preview(event_id, &event_widget);
                    }
                }
            });
            self.agenda_view_bin.set_child(Some(&agenda_view));

            // Blueprint wires the ViewSwitcher to this stack, so observe the
            // stack itself to keep the shared date and view kind in sync.
            let stack_win_weak = win.downgrade();
            self.views_stack.connect_visible_child_notify(move |stack| {
                if let Some(win) = stack_win_weak.upgrade() {
                    win.imp()
                        .handle_view_changed(stack.visible_child_name().as_deref());
                }
            });

            // ── Construct the Quick-Add popover ──
            let popover = crate::ui::quick_add_popover::QuickAddPopover::new();
            popover.set_parent(win.upcast_ref::<gtk::Widget>());

            let popover_weak = popover.downgrade();
            popover.set_on_save({
                let win_weak = win_weak.clone();
                let popover_weak = popover_weak.clone();
                move |title, calendar_id, date| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp()
                            .finalize_quick_add_save(&popover_weak, &title, calendar_id, date);
                    }
                }
            });
            let win_weak2 = win_weak.clone();
            popover.set_on_edit_details(move || {
                if let Some(win) = win_weak2.upgrade()
                    && let Some(popover) = popover_weak.upgrade()
                {
                    win.imp().open_event_editor_from_quick_add(&popover);
                }
            });

            *self.quick_add.borrow_mut() = Some(popover);

            // ── Construct the event preview popover ──
            let event_popover = crate::ui::event_popover::EventPopover::new();
            event_popover.set_parent(win.upcast_ref::<gtk::Widget>());

            let editor = crate::ui::event_editor::EventEditor::new();
            editor.set_on_save({
                let win_weak = win_weak.clone();
                move |event, editing| {
                    win_weak
                        .upgrade()
                        .is_some_and(|win| win.imp().persist_editor_event(&event, editing))
                }
            });
            editor.set_on_delete({
                let win_weak = win_weak.clone();
                move |event_id| {
                    win_weak
                        .upgrade()
                        .is_some_and(|win| win.imp().delete_editor_event(event_id))
                }
            });

            let win_weak3 = win_weak.clone();
            event_popover.set_on_edit_details(move |event_id| {
                if let Some(win) = win_weak3.upgrade() {
                    win.imp().open_event_editor_for_event(event_id);
                }
            });

            // Connect MonthView on_event_activate to open the preview.
            month_view.set_on_event_activate({
                let win_weak = win_weak.clone();
                move |event_id, chip_widget| {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp().open_event_preview(event_id, &chip_widget);
                    }
                }
            });

            *self.event_popover.borrow_mut() = Some(event_popover);
            *self.event_editor.borrow_mut() = Some(editor);

            // ── Window actions ──

            let previous_date = gio::SimpleAction::new("previous-date", None);
            let win_weak = win.downgrade();
            previous_date.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.navigate_previous();
                }
            });
            win.add_action(&previous_date);

            let next_date = gio::SimpleAction::new("next-date", None);
            let win_weak = win.downgrade();
            next_date.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.navigate_next();
                }
            });
            win.add_action(&next_date);

            let today = gio::SimpleAction::new("today", None);
            let win_weak = win.downgrade();
            today.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.navigate_today();
                }
            });
            win.add_action(&today);

            let new_event = gio::SimpleAction::new("new-event", None);
            let win_weak = win.downgrade();
            new_event.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade() {
                    win.imp().open_quick_add_from_button();
                }
            });
            win.add_action(&new_event);

            let calendars = crate::ui::calendar_management::CalendarManagementDialog::new();
            calendars.set_list_calendars({
                let win_weak = win.downgrade();
                move || {
                    win_weak
                        .upgrade()
                        .map(|win| win.imp().list_calendars())
                        .unwrap_or_default()
                }
            });
            calendars.set_list_accounts({
                let win_weak = win.downgrade();
                move || {
                    win_weak
                        .upgrade()
                        .map(|win| win.imp().list_accounts())
                        .unwrap_or_default()
                }
            });
            calendars.set_on_save({
                let win_weak = win.downgrade();
                move |calendar| {
                    win_weak
                        .upgrade()
                        .ok_or_else(|| "The calendar window is no longer available.".to_string())
                        .and_then(|win| win.imp().save_managed_calendar(calendar))
                }
            });
            calendars.set_on_update({
                let win_weak = win.downgrade();
                move |calendar| {
                    win_weak
                        .upgrade()
                        .ok_or_else(|| "The calendar window is no longer available.".to_string())
                        .and_then(|win| win.imp().update_managed_calendar(calendar))
                }
            });
            calendars.set_on_delete({
                let win_weak = win.downgrade();
                move |calendar_id| {
                    win_weak
                        .upgrade()
                        .ok_or_else(|| "The calendar window is no longer available.".to_string())
                        .and_then(|win| win.imp().delete_managed_calendar(calendar_id))
                }
            });
            calendars.set_on_delete_account({
                let win_weak = win.downgrade();
                move |account_id| {
                    win_weak
                        .upgrade()
                        .ok_or_else(|| "The calendar window is no longer available.".to_string())
                        .and_then(|win| win.imp().delete_managed_account(account_id))
                }
            });
            calendars.set_on_provision_caldav({
                let win_weak = win.downgrade();
                move |account, discovery| {
                    win_weak
                        .upgrade()
                        .ok_or_else(|| "The calendar window is no longer available.".to_string())
                        .and_then(|win| win.imp().provision_managed_caldav(account, discovery))
                }
            });
            calendars.set_on_refresh_views({
                let win_weak = win.downgrade();
                move || {
                    if let Some(win) = win_weak.upgrade() {
                        win.imp().render_all_from_state();
                    }
                }
            });
            *self.calendar_management.borrow_mut() = Some(calendars);

            let show_calendars = gio::SimpleAction::new("show-calendars", None);
            let win_weak = win.downgrade();
            show_calendars.connect_activate(move |_, _| {
                if let Some(win) = win_weak.upgrade()
                    && let Some(dialog) = win.imp().calendar_management.borrow().clone()
                {
                    dialog.refresh();
                    adw::prelude::AdwDialogExt::present(
                        &dialog,
                        Some(win.upcast_ref::<gtk::Widget>()),
                    );
                }
            });
            win.add_action(&show_calendars);

            // ── Initial render from the shared local-today state ──
            self.render_all_from_state();
        }

        fn dispose(&self) {
            if let Some(popover) = self.quick_add.borrow_mut().take() {
                popover.unparent();
            }
            if let Some(popover) = self.event_popover.borrow_mut().take() {
                popover.unparent();
            }
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
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
                   gtk::ShortcutManager, gio::ActionGroup, gio::ActionMap;
}

// ── Public API (window callbacks from actions) ──

impl CalendarWindow {
    pub fn new(app: &adw::Application) -> Self {
        glib::Object::builder()
            .property("application", Some(app))
            .build()
    }

    fn navigate_previous(&self) {
        let imp = self.imp();
        if let Some(state) = imp.view_state.borrow_mut().as_mut() {
            state.previous();
        }
        imp.render_all_from_state();
    }

    fn navigate_next(&self) {
        let imp = self.imp();
        if let Some(state) = imp.view_state.borrow_mut().as_mut() {
            state.next();
        }
        imp.render_all_from_state();
    }

    fn navigate_today(&self) {
        let imp = self.imp();
        if let Some(state) = imp.view_state.borrow_mut().as_mut() {
            state.set_today(today_local());
        }
        imp.render_all_from_state();
    }
}

// ── Private helpers ──

impl imp::CalendarWindow {
    /// Borrow the MonthView widget inside `month_view_bin` and call `f` on it.
    fn with_month_view<R>(
        &self,
        f: impl FnOnce(&crate::ui::month_view::MonthView) -> R,
    ) -> Option<R> {
        let child = self.month_view_bin.child()?;
        let mv = child.downcast::<crate::ui::month_view::MonthView>().ok()?;
        Some(f(&mv))
    }

    fn reconcile_month_state(&self, year: i32, month: u32) {
        {
            let mut state_guard = self.view_state.borrow_mut();
            let Some(state) = state_guard.as_mut() else {
                return;
            };
            if state.view() != ViewKind::Month {
                return;
            }
            let current = state.active_date();
            let day = current.day().min(days_in_month(year, month));
            let date =
                NaiveDate::from_ymd_opt(year, month, day).expect("valid month callback date");
            *state = ViewState::new(ViewKind::Month, date);
        }
        self.update_title();
    }

    fn handle_view_changed(&self, page_name: Option<&str>) {
        let Some(kind) = (match page_name {
            Some("month") => Some(ViewKind::Month),
            Some("week") => Some(ViewKind::Week),
            Some("agenda") => Some(ViewKind::Agenda),
            _ => None,
        }) else {
            return;
        };

        let old_kind = self.view_state.borrow().as_ref().map(ViewState::view);
        if old_kind == Some(ViewKind::Month) && kind != ViewKind::Month {
            let dominant = self.with_month_view(|mv| mv.dominant_year_month());
            if let Some((year, month)) = dominant {
                self.reconcile_month_state(year, month);
            }
        }

        if let Some(state) = self.view_state.borrow_mut().as_mut() {
            state.set_view(kind);
        }
        self.sync_views_to_state();
        self.render_month_view();
        self.render_week_view();
        self.render_agenda_view();
        self.update_title();
    }

    /// Push the shared active date into every concrete view.
    fn sync_views_to_state(&self) {
        let Some(date) = self
            .view_state
            .borrow()
            .as_ref()
            .map(ViewState::active_date)
        else {
            return;
        };
        self.with_month_view(|mv| mv.set_active_date(date));
        if let Some(child) = self.week_view_bin.child()
            && let Ok(week_view) = child.downcast::<crate::ui::week_view::WeekView>()
        {
            week_view.set_active_date(date);
        }
        if let Some(child) = self.agenda_view_bin.child()
            && let Ok(agenda_view) = child.downcast::<crate::ui::agenda_view::AgendaView>()
        {
            agenda_view.set_active_date(date);
        }
    }

    /// Load calendars + events from the repository and tell the MonthView to
    /// re-render.  Updates the navigation title afterwards.
    fn render_month_view(&self) {
        let (calendars, all_events) = {
            let repo_guard = self.repository.borrow();
            let repo = repo_guard.as_ref().expect("repository must be initialised");
            let calendars = repo.list_calendars();
            let all_events: Vec<Event> = calendars
                .iter()
                .flat_map(|c| repo.list_events_for_calendar(c.id))
                .collect();
            (calendars, all_events)
        };

        self.with_month_view(|mv| mv.render(&calendars, &all_events));

        self.update_title();
    }

    /// Load calendars + events from the repository and tell the WeekView to
    /// render the current Monday-first week.
    fn render_week_view(&self) {
        let (calendars, all_events) = {
            let repo_guard = self.repository.borrow();
            let repo = repo_guard.as_ref().expect("repository must be initialised");
            let calendars = repo.list_calendars();
            let all_events: Vec<Event> = calendars
                .iter()
                .flat_map(|calendar| repo.list_events_for_calendar(calendar.id))
                .collect();
            (calendars, all_events)
        };

        if let Some(child) = self.week_view_bin.child()
            && let Ok(week_view) = child.downcast::<crate::ui::week_view::WeekView>()
        {
            week_view.render(&calendars, &all_events);
        }
    }

    fn render_agenda_view(&self) {
        let (calendars, all_events) = {
            let repo_guard = self.repository.borrow();
            let repo = repo_guard.as_ref().expect("repository must be initialised");
            let calendars = repo.list_calendars();
            let all_events: Vec<Event> = calendars
                .iter()
                .flat_map(|calendar| repo.list_events_for_calendar(calendar.id))
                .collect();
            (calendars, all_events)
        };

        if let Some(child) = self.agenda_view_bin.child()
            && let Ok(agenda_view) = child.downcast::<crate::ui::agenda_view::AgendaView>()
        {
            agenda_view.render(&calendars, &all_events);
        }
    }

    /// Set the navigation title from the active view and shared date.
    fn update_title(&self) {
        let state_guard = self.view_state.borrow();
        let Some(state) = state_guard.as_ref() else {
            return;
        };
        let date = state.active_date();
        let title = match state.view() {
            ViewKind::Month => format!("{} {}", month_name(date.month()), date.year()),
            ViewKind::Week => {
                let week = state.current_week_dates();
                format!(
                    "Week of {} {} {}, {}",
                    month_name(week[0].month()),
                    week[0].day(),
                    week[0].year(),
                    week[6].format("%b %-d")
                )
            }
            ViewKind::Agenda => {
                format!("Agenda — {} {}", month_name(date.month()), date.year())
            }
        };
        self.title_label.set_label(&title);
    }

    /// Synchronise both concrete views and render from shared state.
    fn render_all_from_state(&self) {
        self.sync_views_to_state();
        self.render_month_view();
        self.render_week_view();
        self.render_agenda_view();
        self.render_calendar_list();
        self.update_title();
    }

    fn render_calendar_list(&self) {
        let calendars = self
            .repository
            .borrow()
            .as_ref()
            .expect("repository must be initialised")
            .list_calendars();
        if let Some(child) = self.calendar_list_bin.child()
            && let Ok(calendar_list) = child.downcast::<crate::ui::calendar_list::CalendarList>()
        {
            calendar_list.set_calendars(&calendars);
        }
    }

    fn list_calendars(&self) -> Vec<Calendar> {
        self.repository
            .borrow()
            .as_ref()
            .expect("repository must be initialised")
            .list_calendars()
    }

    fn list_accounts(&self) -> Vec<Account> {
        self.repository
            .borrow()
            .as_ref()
            .expect("repository must be initialised")
            .list_accounts()
    }

    fn save_managed_calendar(&self, calendar: &Calendar) -> Result<(), String> {
        let result = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard
                .as_mut()
                .ok_or_else(|| "Calendar storage is unavailable.".to_string())?;
            repo.save_calendar(calendar)
        };
        result.map_err(|_| "Could not save the calendar.".to_string())?;
        self.render_all_from_state();
        Ok(())
    }

    fn update_managed_calendar(&self, calendar: &Calendar) -> Result<(), String> {
        let result = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard
                .as_mut()
                .ok_or_else(|| "Calendar storage is unavailable.".to_string())?;
            let Some(previous) = repo.get_calendar(calendar.id) else {
                return Err("The calendar no longer exists.".to_string());
            };
            if previous.read_only
                && (previous.name != calendar.name || previous.color != calendar.color)
            {
                return Err("Read-only calendars cannot be renamed or recolored.".to_string());
            }
            repo.update_calendar(calendar)
        };
        result.map_err(|_| "Could not update the calendar.".to_string())?;
        self.render_all_from_state();
        Ok(())
    }

    fn delete_managed_calendar(&self, calendar_id: Uuid) -> Result<(), String> {
        let deleted = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard
                .as_mut()
                .ok_or_else(|| "Calendar storage is unavailable.".to_string())?;
            let Some(calendar) = repo.get_calendar(calendar_id) else {
                return Err("The calendar no longer exists.".to_string());
            };
            if calendar.read_only || calendar.source != CalendarSource::Local {
                return Err("Only writable local calendars can be removed.".to_string());
            }
            repo.delete_calendar(calendar_id)
        };
        if !deleted {
            return Err("Could not remove the calendar.".to_string());
        }
        self.render_all_from_state();
        Ok(())
    }

    fn delete_managed_account(&self, account_id: Uuid) -> Result<(), String> {
        let deleted = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard
                .as_mut()
                .ok_or_else(|| "Account storage is unavailable.".to_string())?;
            if repo.get_account(account_id).is_none() {
                return Err("The account no longer exists.".to_string());
            }
            repo.delete_account(account_id)
        };
        if !deleted {
            return Err("Could not remove the account.".to_string());
        }

        self.render_all_from_state();
        let _ = delete_on_worker(account_id);
        Ok(())
    }

    fn provision_managed_caldav(
        &self,
        account: &Account,
        discovery: &CaldavDiscovery,
    ) -> Result<PathBuf, String> {
        let result = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard
                .as_mut()
                .ok_or_else(|| "Calendar storage is unavailable.".to_string())?;
            repo.provision_caldav_account(account, discovery)
        };
        result.map_err(|_| "Could not add the online account.".to_string())?;
        self.render_all_from_state();
        Ok(Self::make_db_path())
    }

    /// Persist one visibility change without changing any other calendar
    /// fields. The list row restores its prior state when this returns false.
    fn set_calendar_visibility(&self, calendar_id: Uuid, visible: bool) -> bool {
        let result = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard.as_mut().expect("repository must be initialised");
            let Some(mut calendar) = repo.get_calendar(calendar_id) else {
                self.overlay
                    .add_toast(adw::Toast::new("Could not update calendar visibility."));
                return false;
            };
            calendar.visible = visible;
            repo.update_calendar(&calendar)
        };

        match result {
            Ok(()) => {
                self.render_all_from_state();
                true
            }
            Err(RepositoryError) => {
                self.overlay
                    .add_toast(adw::Toast::new("Could not update calendar visibility."));
                false
            }
        }
    }

    /// Build the application-specific database path beneath the
    /// platform's per-user data directory.
    fn make_db_path() -> PathBuf {
        let mut path = glib::user_data_dir();
        path.push("dev.chris.calendar");
        path.push("calendar.sqlite");
        path
    }

    /// Construct the three defaults used for first-run repository
    /// initialization.  The repository owns the durable initialization
    /// marker and transaction that decide whether these are written.
    fn default_calendars() -> [Calendar; 3] {
        [
            Calendar {
                id: Uuid::parse_str("e1111111-e111-1111-1111-111111111111").unwrap(),
                name: "Personal".to_string(),
                color: "#3366cc".to_string(),
                visible: true,
                read_only: false,
                source: CalendarSource::Local,
            },
            Calendar {
                id: Uuid::parse_str("e2222222-e222-2222-2222-222222222222").unwrap(),
                name: "Work".to_string(),
                color: "#cc3333".to_string(),
                visible: true,
                read_only: false,
                source: CalendarSource::Local,
            },
            Calendar {
                id: Uuid::parse_str("e3333333-e333-3333-3333-333333333333").unwrap(),
                name: "Hidden".to_string(),
                color: "#999999".to_string(),
                visible: false,
                read_only: false,
                source: CalendarSource::Local,
            },
        ]
    }

    /// Show a fatal error dialog for a startup database failure,
    /// then quit.  The dialog displays the full path so the user can
    /// diagnose permissions or disk-space issues.
    fn show_fatal_db_dialog(
        parent: &gtk::Window,
        app: Option<gtk::Application>,
        db_path: &std::path::Path,
    ) {
        let dialog = adw::AlertDialog::new(
            Some("Cannot Open Calendar Database"),
            Some(&format!(
                "The calendar data could not be opened at\n\n  {}\n\n\
                 Check that you have write permission and enough \
                 disk space. The application will now close.",
                db_path.display(),
            )),
        );
        dialog.add_response("quit", "Quit");
        dialog.set_close_response("quit");
        dialog.connect_response(Some("quit"), move |_, _| {
            if let Some(app) = app.as_ref() {
                app.quit();
            }
        });
        dialog.present(Some(parent.upcast_ref::<gtk::Widget>()));
    }

    // ── Quick-Add helpers ──

    /// Open the Quick-Add popover positioned at the bottom New-Event button.
    /// The date defaults to local today since Month View has no selected-date
    /// state.
    fn open_quick_add_from_button(&self) {
        let date = today_local();

        let rect = self.compute_widget_rect(&self.new_event_button.get());
        self.open_quick_add(date, rect);
    }

    /// Open the Quick-Add popover for the given date, optionally pointing
    /// at a pre-computed rectangle in the window's coordinate space.  When
    /// `rect` is provided the popover anchors there; otherwise it auto-
    /// centers (fallback for callers that cannot determine geometry).
    fn open_quick_add(&self, date: NaiveDate, rect: Option<gtk::gdk::Rectangle>) {
        let Some(popover) = self.quick_add.borrow().clone() else {
            return;
        };

        // Refresh calendar list from the repository so renames / toggles
        // are reflected in the popover each time it opens.
        let repo = self.repository.borrow();
        let repo = repo.as_ref().expect("repository must be initialised");
        let calendars = repo.list_calendars();
        popover.set_calendars(&calendars);
        popover.set_date(date);

        if let Some(rect) = rect {
            // Default PositionType::Top places the popover below the
            // pointing rect; GTK flips it to Bottom if the rect is too
            // close to the bottom of the screen (e.g. the New-Event
            // button in the action bar).  No explicit position override
            // is needed.
            popover.set_pointing_to(Some(&rect));
        } else {
            popover.set_pointing_to(None);
        }

        popover.popup();
    }

    /// Build, persist, and close the Quick-Add popover.  Uses the pure
    /// model seam `new_quick_add_event` with the caller-supplied trimmed
    /// title.  Failures are surfaced as a toast; on success the MonthView
    /// is refreshed and the popover resets and hides.
    fn finalize_quick_add_save(
        &self,
        popover_weak: &glib::WeakRef<crate::ui::quick_add_popover::QuickAddPopover>,
        title: &str,
        calendar_id: Uuid,
        date: NaiveDate,
    ) {
        let event_id = Uuid::new_v4();
        let event = match new_quick_add_event(event_id, calendar_id, title, date) {
            Ok(ev) => ev,
            Err(EmptyQuickAddTitle) => {
                // Guard against edge case: popover delivered empty title.
                self.overlay
                    .add_toast(adw::Toast::new("Event title cannot be empty."));
                return;
            }
        };

        let result = {
            let mut repo = self.repository.borrow_mut();
            let repo = repo.as_mut().expect("repository must be initialised");
            repo.create_event_with_sync(&event)
        }; // mutable guard dropped before render / popover work
        match result {
            Ok(()) => {
                self.render_month_view();
                self.render_week_view();
                self.render_agenda_view();
                if let Some(popover) = popover_weak.upgrade() {
                    popover.popdown();
                }
            }
            Err(RepositoryError) => {
                self.overlay
                    .add_toast(adw::Toast::new("Could not save event."));
            }
        }
    }

    fn open_event_editor_from_quick_add(
        &self,
        popover: &crate::ui::quick_add_popover::QuickAddPopover,
    ) {
        let Some((title, calendar_id, date)) = popover.details() else {
            return;
        };
        let Some(editor) = self.event_editor.borrow().clone() else {
            return;
        };
        let calendars = self
            .repository
            .borrow()
            .as_ref()
            .expect("repository must be initialised")
            .list_calendars();
        editor.set_calendars(&calendars);
        editor.set_create_defaults(&title, calendar_id, date);
        popover.popdown();
        adw::prelude::AdwDialogExt::present(&editor, Some(self.obj().upcast_ref::<gtk::Widget>()));
    }

    fn open_event_editor_for_event(&self, event_id: Uuid) {
        let (event, calendar, calendars) = {
            let repo_guard = self.repository.borrow();
            let repo = repo_guard.as_ref().expect("repository must be initialised");
            let Some(event) = repo.get_event(event_id) else {
                return;
            };
            let calendar = repo.get_calendar(event.calendar_id);
            let calendars = repo.list_calendars();
            (event, calendar, calendars)
        };
        if calendar.as_ref().is_none_or(|calendar| calendar.read_only) {
            self.overlay
                .add_toast(adw::Toast::new("This event is on a read-only calendar."));
            return;
        }
        let Some(editor) = self.event_editor.borrow().clone() else {
            return;
        };
        editor.set_calendars(&calendars);
        editor.set_event(&event);
        adw::prelude::AdwDialogExt::present(&editor, Some(self.obj().upcast_ref::<gtk::Widget>()));
    }

    fn persist_editor_event(&self, event: &Event, editing: bool) -> bool {
        let result = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard.as_mut().expect("repository must be initialised");
            let Some(calendar) = repo.get_calendar(event.calendar_id) else {
                self.overlay
                    .add_toast(adw::Toast::new("Choose an available calendar."));
                return false;
            };
            if calendar.read_only {
                self.overlay
                    .add_toast(adw::Toast::new("Choose a writable calendar."));
                return false;
            }
            if editing {
                repo.update_event_with_sync(event)
            } else {
                repo.create_event_with_sync(event)
            }
        };
        match result {
            Ok(()) => {
                self.render_all_from_state();
                true
            }
            Err(RepositoryError) => {
                self.overlay
                    .add_toast(adw::Toast::new("Could not save event."));
                false
            }
        }
    }

    fn delete_editor_event(&self, event_id: Uuid) -> bool {
        let undo = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard.as_mut().expect("repository must be initialised");
            let Some(event) = repo.get_event(event_id) else {
                self.overlay
                    .add_toast(adw::Toast::new("Could not delete event."));
                return false;
            };
            let Some(calendar) = repo.get_calendar(event.calendar_id) else {
                self.overlay
                    .add_toast(adw::Toast::new("Could not delete event."));
                return false;
            };
            if calendar.read_only {
                self.overlay
                    .add_toast(adw::Toast::new("This event is on a read-only calendar."));
                return false;
            }
            match repo.delete_event_with_sync_undo(event_id) {
                Ok(undo) => undo,
                Err(RepositoryError) => {
                    self.overlay
                        .add_toast(adw::Toast::new("Could not delete event."));
                    return false;
                }
            }
        };

        self.render_all_from_state();

        let pending = Rc::new(RefCell::new(Some(undo)));
        let toast = adw::Toast::builder()
            .title("Event deleted")
            .button_label("Undo")
            .timeout(5)
            .build();

        let pending_click = pending.clone();
        let win_weak = self.obj().downgrade();
        toast.connect_button_clicked(move |_| {
            let Some(mut undo) = pending_click.borrow_mut().take() else {
                return;
            };
            if let Some(win) = win_weak.upgrade() {
                win.imp().restore_deleted_event(&mut undo);
            }
        });

        let pending_dismiss = pending.clone();
        toast.connect_dismissed(move |_| {
            pending_dismiss.borrow_mut().take();
        });
        self.overlay.add_toast(toast);
        true
    }

    fn restore_deleted_event(&self, undo: &mut EventDeletionUndo) {
        let result = {
            let mut repo_guard = self.repository.borrow_mut();
            let repo = repo_guard.as_mut().expect("repository must be initialised");
            repo.undo_event_with_sync(undo)
        };
        match result {
            Ok(()) => {
                self.render_all_from_state();
                self.overlay.add_toast(adw::Toast::new("Event restored."));
            }
            Err(RepositoryError) => {
                self.overlay
                    .add_toast(adw::Toast::new("Could not restore event."));
            }
        }
    }

    /// Compute the widget's allocation rectangle in the window's
    /// coordinate space.  Returns `None` if the transform is unavailable
    /// (e.g. widget not yet realised).
    fn compute_widget_rect(&self, widget: &impl IsA<gtk::Widget>) -> Option<gtk::gdk::Rectangle> {
        let origin = widget.compute_point(
            self.obj().upcast_ref::<gtk::Widget>(),
            &gtk::graphene::Point::new(0.0, 0.0),
        )?;
        Some(gtk::gdk::Rectangle::new(
            origin.x() as i32,
            origin.y() as i32,
            widget.width(),
            widget.height(),
        ))
    }

    // ── Event preview helper ──

    /// Resolve an event from the repository, populate the preview popover,
    /// anchor it at the chip widget, and open it.  Missing events are
    /// handled silently (the popover simply isn't shown).
    fn open_event_preview(&self, event_id: Uuid, chip_widget: &gtk::Widget) {
        let Some(popover) = self.event_popover.borrow().clone() else {
            return;
        };

        let (event, calendar) = {
            let repo_guard = self.repository.borrow();
            let repo = repo_guard.as_ref().expect("repository must be initialised");
            let event = match repo.get_event(event_id) {
                Some(ev) => ev,
                None => return, // silently ignore missing events
            };
            let calendar = repo.get_calendar(event.calendar_id);
            (event, calendar)
        };

        let today = today_local();
        popover.set_event(&event, calendar.as_ref(), today);

        let rect = self.compute_widget_rect(chip_widget);
        if let Some(r) = rect {
            popover.set_pointing_to(Some(&r));
        }
        popover.popup();
    }
}

// ── Free helper ──

/// Read the local today date (no timezone conversion).  Falls back to a
/// deterministic value if the local clock is unavailable.
fn today_local() -> NaiveDate {
    glib::DateTime::now_local()
        .ok()
        .and_then(|dt| {
            NaiveDate::from_ymd_opt(dt.year(), dt.month() as u32, dt.day_of_month() as u32)
        })
        .unwrap_or_else(|| NaiveDate::from_ymd_opt(2026, 7, 20).unwrap())
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

fn days_in_month(year: i32, month: u32) -> u32 {
    let first_of_next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("month callback exceeded chrono's year range");
    (first_of_next - chrono::Duration::days(1)).day()
}
