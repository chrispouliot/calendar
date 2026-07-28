use adw::gio;
use adw::prelude::*;
use calendar::backend::credentials::{CredentialError, lookup_on_worker};
use calendar::backend::reminders::reminder_occurrences_in_window;
use calendar::backend::sync::{AccountSyncSummary, AccountSyncWorkerError, sync_account_on_worker};
use calendar::backend::{AccountRepository, CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{Account, Event};
use calendar::preferences::format_wall_time;
use calendar::viewer_time::{now_local_fixed, to_local_fixed};
use chrono::{DateTime, Duration as ChronoDuration, FixedOffset};
use gtk::glib;
use std::cell::RefCell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;
use uuid::Uuid;

const REMINDER_NOTIFICATION_ID: &str = "calendar-reminders";
const REMINDER_LOOKBACK: ChronoDuration = ChronoDuration::hours(1);

pub(crate) fn database_path() -> PathBuf {
    let mut path = glib::user_data_dir();
    path.push("dev.chris.calendar");
    path.push("calendar.sqlite");
    path
}

pub(crate) struct BackgroundController {
    sync: Option<SyncScheduler>,
    reminders: Option<ReminderScheduler>,
}

impl BackgroundController {
    pub(crate) fn new(app: &adw::Application) -> Self {
        let path = database_path();
        Self {
            sync: Some(SyncScheduler::new(app, path.clone())),
            reminders: Some(ReminderScheduler::new(app, path)),
        }
    }

    pub(crate) fn stop(&mut self) {
        if let Some(scheduler) = self.sync.take() {
            scheduler.stop();
        }
        if let Some(scheduler) = self.reminders.take() {
            scheduler.stop();
        }
    }
}

enum SyncPhase {
    Accounts {
        receiver: Receiver<Vec<Account>>,
    },
    Lookup {
        accounts: Vec<Account>,
        next: usize,
        account: Account,
        receiver: Receiver<Result<Option<oo7::Secret>, CredentialError>>,
    },
    Sync {
        accounts: Vec<Account>,
        next: usize,
        receiver: Receiver<Result<AccountSyncSummary, AccountSyncWorkerError>>,
    },
}

struct SyncSchedulerState {
    database_path: PathBuf,
    phase: Option<SyncPhase>,
    pending: bool,
    terminal_progress: bool,
}

struct SyncScheduler {
    startup_source: Rc<RefCell<Option<glib::SourceId>>>,
    interval_source: glib::SourceId,
    poll_source: glib::SourceId,
}

impl SyncScheduler {
    fn new(app: &adw::Application, database_path: PathBuf) -> Self {
        let state = Rc::new(RefCell::new(SyncSchedulerState {
            database_path,
            phase: None,
            pending: false,
            terminal_progress: false,
        }));
        let app_weak = app.downgrade();

        let startup_source = Rc::new(RefCell::new(None));
        let startup_source_for_callback = startup_source.clone();
        let startup_state = state.clone();
        let startup_app = app_weak.clone();
        let source = glib::timeout_add_local_once(Duration::from_secs(1), move || {
            startup_source_for_callback.borrow_mut().take();
            request_sync_batch(&startup_app, &startup_state);
        });
        startup_source.borrow_mut().replace(source);

        let interval_app = app_weak.clone();
        let interval_state = state.clone();
        let interval_source = glib::timeout_add_local(Duration::from_secs(60), move || {
            request_sync_batch(&interval_app, &interval_state);
            glib::ControlFlow::Continue
        });

        let poll_app = app_weak;
        let poll_source = glib::timeout_add_local(Duration::from_millis(100), move || {
            poll_sync_batch(&poll_app, &state)
        });

        Self {
            startup_source,
            interval_source,
            poll_source,
        }
    }

    fn stop(self) {
        if let Some(source) = self.startup_source.borrow_mut().take() {
            source.remove();
        }
        self.interval_source.remove();
        self.poll_source.remove();
    }
}

fn load_accounts_on_worker(database_path: PathBuf) -> Receiver<Vec<Account>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = move || {
        let accounts = SqliteRepository::open(database_path)
            .ok()
            .map(|repo| {
                repo.list_accounts()
                    .into_iter()
                    .filter(|account| account.enabled)
                    .collect()
            })
            .unwrap_or_default();
        let _ = sender.send(accounts);
    };
    if std::thread::Builder::new()
        .name("calendar-background-accounts".to_owned())
        .spawn(worker)
        .is_err()
    {
        return disconnected_receiver();
    }
    receiver
}

fn disconnected_receiver<T>() -> Receiver<T> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    drop(sender);
    receiver
}

fn request_sync_batch(
    _app_weak: &glib::WeakRef<adw::Application>,
    state: &Rc<RefCell<SyncSchedulerState>>,
) {
    if state.borrow().phase.is_some() {
        state.borrow_mut().pending = true;
        return;
    }

    state.borrow_mut().terminal_progress = false;
    let database_path = state.borrow().database_path.clone();
    state.borrow_mut().phase = Some(SyncPhase::Accounts {
        receiver: load_accounts_on_worker(database_path),
    });
}

fn start_account_lookup(
    app_weak: &glib::WeakRef<adw::Application>,
    state: &Rc<RefCell<SyncSchedulerState>>,
    accounts: Vec<Account>,
    next: usize,
) {
    if next >= accounts.len() {
        finish_sync_batch(app_weak, state);
        return;
    }
    let account = accounts[next].clone();
    let receiver = lookup_on_worker(account.id);
    state.borrow_mut().phase = Some(SyncPhase::Lookup {
        accounts,
        next,
        account,
        receiver,
    });
}

fn advance_sync_batch(
    app_weak: &glib::WeakRef<adw::Application>,
    state: &Rc<RefCell<SyncSchedulerState>>,
    accounts: Vec<Account>,
    next: usize,
) {
    state.borrow_mut().terminal_progress = true;
    start_account_lookup(app_weak, state, accounts, next);
}

fn finish_sync_batch(
    app_weak: &glib::WeakRef<adw::Application>,
    state: &Rc<RefCell<SyncSchedulerState>>,
) {
    let (refresh, pending) = {
        let mut state = state.borrow_mut();
        state.phase = None;
        let refresh = state.terminal_progress;
        state.terminal_progress = false;
        let pending = state.pending;
        state.pending = false;
        (refresh, pending)
    };

    if refresh {
        refresh_open_window(app_weak);
    }
    if pending {
        request_sync_batch(app_weak, state);
    }
}

fn poll_sync_batch(
    app_weak: &glib::WeakRef<adw::Application>,
    state: &Rc<RefCell<SyncSchedulerState>>,
) -> glib::ControlFlow {
    if app_weak.upgrade().is_none() {
        return glib::ControlFlow::Break;
    }
    let Some(phase) = state.borrow_mut().phase.take() else {
        return glib::ControlFlow::Continue;
    };

    match phase {
        SyncPhase::Accounts { receiver } => match receiver.try_recv() {
            Err(TryRecvError::Empty) => {
                state.borrow_mut().phase = Some(SyncPhase::Accounts { receiver });
            }
            Err(TryRecvError::Disconnected) => finish_sync_batch(app_weak, state),
            Ok(accounts) => {
                if accounts.is_empty() {
                    finish_sync_batch(app_weak, state);
                } else {
                    start_account_lookup(app_weak, state, accounts, 0);
                }
            }
        },
        SyncPhase::Lookup {
            accounts,
            next,
            account,
            receiver,
        } => match receiver.try_recv() {
            Err(TryRecvError::Empty) => {
                state.borrow_mut().phase = Some(SyncPhase::Lookup {
                    accounts,
                    next,
                    account,
                    receiver,
                });
            }
            Err(TryRecvError::Disconnected) | Ok(Err(_)) | Ok(Ok(None)) => {
                advance_sync_batch(app_weak, state, accounts, next + 1);
            }
            Ok(Ok(Some(password))) => {
                let database_path = state.borrow().database_path.clone();
                let receiver = sync_account_on_worker(database_path, account, password);
                state.borrow_mut().phase = Some(SyncPhase::Sync {
                    accounts,
                    next,
                    receiver,
                });
            }
        },
        SyncPhase::Sync {
            accounts,
            next,
            receiver,
        } => match receiver.try_recv() {
            Err(TryRecvError::Empty) => {
                state.borrow_mut().phase = Some(SyncPhase::Sync {
                    accounts,
                    next,
                    receiver,
                });
            }
            Err(TryRecvError::Disconnected) | Ok(Err(_)) | Ok(Ok(_)) => {
                advance_sync_batch(app_weak, state, accounts, next + 1);
            }
        },
    }
    glib::ControlFlow::Continue
}

fn refresh_open_window(app_weak: &glib::WeakRef<adw::Application>) {
    let Some(app) = app_weak.upgrade() else {
        return;
    };
    if let Some(window) = app
        .windows()
        .into_iter()
        .find_map(|window| window.downcast::<crate::window::CalendarWindow>().ok())
    {
        window.refresh_from_background();
    }
}

struct ReminderScheduler {
    source: Rc<RefCell<Option<glib::SourceId>>>,
    poll_source: glib::SourceId,
    app: adw::Application,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct DeliveredReminder {
    event_id: Uuid,
    occurrence_start: DateTime<FixedOffset>,
    trigger_at: DateTime<FixedOffset>,
    description: String,
}

struct ReminderSchedulerState {
    app: adw::Application,
    last_checked: DateTime<FixedOffset>,
    delivered: HashSet<DeliveredReminder>,
    startup_receiver: Option<(DateTime<FixedOffset>, Receiver<Vec<Event>>)>,
    check_receiver: Option<(DateTime<FixedOffset>, Receiver<Vec<Event>>)>,
}

impl ReminderScheduler {
    fn new(app: &adw::Application, database_path: PathBuf) -> Self {
        let now = now_local_fixed();
        let state = Rc::new(RefCell::new(ReminderSchedulerState {
            app: app.clone(),
            last_checked: now,
            delivered: HashSet::new(),
            startup_receiver: Some((now, load_reminder_events_on_worker(database_path.clone()))),
            check_receiver: None,
        }));
        app.withdraw_notification(REMINDER_NOTIFICATION_ID);

        let source = Rc::new(RefCell::new(None));
        let interval_state = state.clone();
        let interval_source_for_callback = source.clone();
        let interval_database_path = database_path;
        let source_id = glib::timeout_add_local(Duration::from_secs(15), move || {
            let mut state = interval_state.borrow_mut();
            if state.startup_receiver.is_none() && state.check_receiver.is_none() {
                let now = now_local_fixed();
                state.check_receiver = Some((
                    now,
                    load_reminder_events_on_worker(interval_database_path.clone()),
                ));
            }
            let _ = &interval_source_for_callback;
            glib::ControlFlow::Continue
        });
        source.borrow_mut().replace(source_id);

        let poll_state = state;
        let poll_source = glib::timeout_add_local(Duration::from_millis(100), move || {
            poll_reminders(&poll_state)
        });

        Self {
            source,
            poll_source,
            app: app.clone(),
        }
    }

    fn stop(self) {
        if let Some(source) = self.source.borrow_mut().take() {
            source.remove();
        }
        self.poll_source.remove();
        self.app.withdraw_notification(REMINDER_NOTIFICATION_ID);
    }
}

fn load_reminder_events_on_worker(database_path: PathBuf) -> Receiver<Vec<Event>> {
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let worker = move || {
        let events = SqliteRepository::open(database_path)
            .ok()
            .map(|repo| {
                repo.list_calendars()
                    .into_iter()
                    .flat_map(|calendar| repo.list_events_for_calendar(calendar.id))
                    .collect()
            })
            .unwrap_or_default();
        let _ = sender.send(events);
    };
    if std::thread::Builder::new()
        .name("calendar-background-reminders".to_owned())
        .spawn(worker)
        .is_err()
    {
        return disconnected_receiver();
    }
    receiver
}

fn reminder_window_start(now: DateTime<FixedOffset>) -> DateTime<FixedOffset> {
    now.checked_sub_signed(REMINDER_LOOKBACK).unwrap_or(now)
}

fn delivered_reminder_key(
    event_id: Uuid,
    occurrence_start: DateTime<FixedOffset>,
    trigger_at: DateTime<FixedOffset>,
    description: String,
) -> DeliveredReminder {
    DeliveredReminder {
        event_id,
        occurrence_start,
        trigger_at,
        description,
    }
}

fn poll_reminders(state: &Rc<RefCell<ReminderSchedulerState>>) -> glib::ControlFlow {
    let startup = state.borrow_mut().startup_receiver.take();
    if let Some((now, receiver)) = startup {
        match receiver.try_recv() {
            Err(TryRecvError::Empty) => state.borrow_mut().startup_receiver = Some((now, receiver)),
            Err(TryRecvError::Disconnected) => {}
            Ok(events) => seed_startup_reminders(state, now, &events),
        }
        return glib::ControlFlow::Continue;
    }

    let check = state.borrow_mut().check_receiver.take();
    let Some((now, receiver)) = check else {
        return glib::ControlFlow::Continue;
    };
    match receiver.try_recv() {
        Err(TryRecvError::Empty) => state.borrow_mut().check_receiver = Some((now, receiver)),
        Err(TryRecvError::Disconnected) => {}
        Ok(events) => check_reminders(state, now, &events),
    }
    glib::ControlFlow::Continue
}

fn seed_startup_reminders(
    state: &Rc<RefCell<ReminderSchedulerState>>,
    now: DateTime<FixedOffset>,
    events: &[Event],
) {
    let start = reminder_window_start(now);
    let mut state = state.borrow_mut();
    for event in events {
        for occurrence in reminder_occurrences_in_window(event, start, now) {
            state.delivered.insert(delivered_reminder_key(
                occurrence.event_id,
                occurrence.occurrence_start,
                occurrence.trigger_at,
                occurrence.description,
            ));
        }
    }
}

fn check_reminders(
    state: &Rc<RefCell<ReminderSchedulerState>>,
    now: DateTime<FixedOffset>,
    events: &[Event],
) {
    let start = reminder_window_start(now);
    {
        let mut state = state.borrow_mut();
        state.last_checked = now;
        state
            .delivered
            .retain(|reminder| reminder.trigger_at > start);
    }

    let mut due: Vec<(String, DateTime<FixedOffset>, String)> = Vec::new();
    for event in events {
        for occurrence in reminder_occurrences_in_window(event, start, now) {
            let key = delivered_reminder_key(
                occurrence.event_id,
                occurrence.occurrence_start,
                occurrence.trigger_at,
                occurrence.description.clone(),
            );
            if state.borrow_mut().delivered.insert(key) {
                due.push((
                    event.title.clone(),
                    occurrence.occurrence_start,
                    occurrence.description,
                ));
            }
        }
    }
    if due.is_empty() {
        return;
    }

    let (title, body) = if due.len() == 1 {
        let (event_title, occurrence_start, description) = &due[0];
        let due_line = format!(
            "Due at {}",
            format_wall_time(to_local_fixed(occurrence_start).time())
        );
        let description = description.trim();
        if description.is_empty()
            || description.eq_ignore_ascii_case(&format!("Reminder for {event_title}"))
        {
            (event_title.clone(), due_line)
        } else {
            (event_title.clone(), format!("{due_line}\n{description}"))
        }
    } else {
        let event_lines = due
            .iter()
            .map(|(event_title, occurrence_start, _)| {
                format!(
                    "{event_title} — {}",
                    format_wall_time(to_local_fixed(occurrence_start).time())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        (format!("{} calendar reminders", due.len()), event_lines)
    };
    let app = state.borrow().app.clone();
    app.send_notification(
        Some(REMINDER_NOTIFICATION_ID),
        &reminder_notification(&title, &body),
    );
}

fn reminder_notification(title: &str, body: &str) -> gio::Notification {
    let notification = gio::Notification::new(title);
    notification.set_body(Some(body));
    notification.set_icon(&gio::ThemedIcon::new("dev.chris.calendar"));
    notification.set_priority(gio::NotificationPriority::Normal);
    notification.set_default_action("app.reminder-open");
    notification
}
