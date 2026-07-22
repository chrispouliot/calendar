use reqwest::Url;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use uuid::Uuid;

use super::caldav::{CaldavClient, CaldavError, serialize_icalendar_event};
use super::{
    CalendarRepository, EventRepository, PendingSyncOperationRepository, RemoteSnapshotSummary,
    RepositoryError, SqliteRepository, SyncStateRepository,
};
use crate::model::{Account, CalendarSource, EventSyncState, PendingSyncOperation};

#[derive(Debug)]
pub enum PullSyncError {
    Caldav(CaldavError),
    MissingCalendarSyncState,
    Repository(RepositoryError),
}

/// The aggregate result of the initial full pull for one account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitialPullSummary {
    pub calendars: usize,
    pub added: usize,
    pub updated: usize,
    pub deleted: usize,
    pub skipped: usize,
}

/// Errors sent by the initial-pull worker deliberately contain no account or
/// credential data, so formatting a terminal result cannot disclose secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitialPullWorkerError {
    InvalidCredential,
    Caldav,
    MissingCalendarSyncState,
    Repository,
    WorkerPanic,
}

/// The aggregate result of syncing all persisted CalDAV calendars for one
/// account. It contains only counts and no account or credential data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountSyncSummary {
    pub calendars: usize,
    pub pushed: PendingPushSummary,
    pub pulled: RemoteSnapshotSummary,
}

/// Errors sent by the account-sync worker deliberately omit account,
/// credential, and transport details so formatting a terminal result is safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountSyncWorkerError {
    InvalidCredential,
    Caldav,
    MissingCalendarSyncState,
    Repository,
    WorkerPanic,
}

/// Start an account sync on one dedicated worker. The returned receiver is
/// ready before any blocking database or network work begins.
pub fn sync_account_on_worker(
    database_path: PathBuf,
    account: Account,
    password: oo7::Secret,
) -> Receiver<Result<AccountSyncSummary, AccountSyncWorkerError>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_sender = sender.clone();
    let worker = move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            sync_account(&database_path, &account, password)
        }))
        .unwrap_or(Err(AccountSyncWorkerError::WorkerPanic));
        let _ = worker_sender.send(result);
    };

    if std::thread::Builder::new()
        .name("caldav-account-sync".to_owned())
        .spawn(worker)
        .is_err()
    {
        let _ = sender.send(Err(AccountSyncWorkerError::WorkerPanic));
    }
    receiver
}

fn sync_account(
    database_path: &std::path::Path,
    account: &Account,
    password: oo7::Secret,
) -> Result<AccountSyncSummary, AccountSyncWorkerError> {
    if password.content_type() != oo7::ContentType::Text {
        return Err(AccountSyncWorkerError::InvalidCredential);
    }

    let mut repository =
        SqliteRepository::open(database_path).map_err(|_| AccountSyncWorkerError::Repository)?;
    let client = CaldavClient::new_with_secret(
        account.server_url.clone(),
        account.username.clone(),
        password,
    );
    let calendars = repository
        .list_calendars()
        .into_iter()
        .filter(|calendar| {
            matches!(
                calendar.source,
                CalendarSource::CalDav { account_id } if account_id == account.id
            )
        })
        .collect::<Vec<_>>();

    let mut summary = AccountSyncSummary {
        calendars: calendars.len(),
        pushed: PendingPushSummary {
            created: 0,
            updated: 0,
            deleted: 0,
            conflicts: 0,
            skipped: 0,
        },
        pulled: RemoteSnapshotSummary {
            added: 0,
            updated: 0,
            deleted: 0,
            skipped: 0,
        },
    };

    for calendar in calendars {
        repository
            .queue_orphan_event_creates(calendar.id, account.id)
            .map_err(|_| AccountSyncWorkerError::Repository)?;
        let pending_before_push = repository.list_pending_sync_operations(calendar.id);
        let pushed = push_pending_operations(&client, &mut repository, calendar.id)
            .map_err(account_push_error)?;
        summary.pushed.created += pushed.created;
        summary.pushed.updated += pushed.updated;
        summary.pushed.deleted += pushed.deleted;
        summary.pushed.conflicts += pushed.conflicts;
        summary.pushed.skipped += pushed.skipped;

        // A server may acknowledge a PUT before its collection report includes
        // that resource. Keep successful uploads protected for this immediate
        // pull; the temporary intents are removed after reconciliation.
        let recently_pushed = pending_before_push
            .into_iter()
            .filter_map(|operation| {
                let event_id = match &operation {
                    PendingSyncOperation::Create { event_id, .. }
                    | PendingSyncOperation::Update { event_id, .. }
                    | PendingSyncOperation::Delete { event_id, .. } => *event_id,
                };
                if repository.get_pending_sync_operation(event_id).is_some() {
                    return None;
                }
                match operation {
                    PendingSyncOperation::Delete { .. } => Some((event_id, operation)),
                    PendingSyncOperation::Create { .. } | PendingSyncOperation::Update { .. } => {
                        repository.get_event_sync_state(event_id).map(|state| {
                            let temporary = PendingSyncOperation::Update {
                                calendar_id: state.calendar_id,
                                event_id: state.event_id,
                                remote_href: state.remote_href.clone(),
                                remote_uid: state.remote_uid.clone(),
                                base_etag: state.etag.clone(),
                            };
                            (event_id, temporary)
                        })
                    }
                }
            })
            .collect::<Vec<_>>();
        for (_, operation) in &recently_pushed {
            repository
                .upsert_pending_sync_operation(operation)
                .map_err(|_| AccountSyncWorkerError::Repository)?;
        }

        let pulled = pull_calendar_snapshot(&client, &mut repository, calendar.id);
        for (event_id, _) in &recently_pushed {
            repository.remove_pending_sync_operation(*event_id);
        }
        let pulled = pulled.map_err(account_pull_error)?;
        summary.pulled.added += pulled.added;
        summary.pulled.updated += pulled.updated;
        summary.pulled.deleted += pulled.deleted;
        summary.pulled.skipped += pulled.skipped;
    }

    Ok(summary)
}

fn account_push_error(error: PushSyncError) -> AccountSyncWorkerError {
    match error {
        PushSyncError::Caldav(_) => AccountSyncWorkerError::Caldav,
        PushSyncError::MissingCalendarSyncState => AccountSyncWorkerError::MissingCalendarSyncState,
        PushSyncError::Repository(_) => AccountSyncWorkerError::Repository,
    }
}

fn account_pull_error(error: PullSyncError) -> AccountSyncWorkerError {
    match error {
        PullSyncError::Caldav(_) => AccountSyncWorkerError::Caldav,
        PullSyncError::MissingCalendarSyncState => AccountSyncWorkerError::MissingCalendarSyncState,
        PullSyncError::Repository(_) => AccountSyncWorkerError::Repository,
    }
}

/// Start an initial, full CalDAV baseline for all calendars provisioned for an
/// account. The returned receiver is ready before any blocking database or
/// network work begins.
pub fn initial_pull_after_provisioning_on_worker(
    database_path: PathBuf,
    account: Account,
    password: oo7::Secret,
) -> Receiver<Result<InitialPullSummary, InitialPullWorkerError>> {
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker_sender = sender.clone();
    let worker = move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            initial_pull_after_provisioning(&database_path, &account, password)
        }))
        .unwrap_or(Err(InitialPullWorkerError::WorkerPanic));
        let _ = worker_sender.send(result);
    };

    if std::thread::Builder::new()
        .name("caldav-initial-pull".to_owned())
        .spawn(worker)
        .is_err()
    {
        let _ = sender.send(Err(InitialPullWorkerError::WorkerPanic));
    }
    receiver
}

fn initial_pull_after_provisioning(
    database_path: &std::path::Path,
    account: &Account,
    password: oo7::Secret,
) -> Result<InitialPullSummary, InitialPullWorkerError> {
    if password.content_type() != oo7::ContentType::Text {
        return Err(InitialPullWorkerError::InvalidCredential);
    }

    let mut repository =
        SqliteRepository::open(database_path).map_err(|_| InitialPullWorkerError::Repository)?;
    let client = CaldavClient::new_with_secret(
        account.server_url.clone(),
        account.username.clone(),
        password,
    );
    let mut summary = InitialPullSummary {
        calendars: 0,
        added: 0,
        updated: 0,
        deleted: 0,
        skipped: 0,
    };

    let calendars = repository
        .list_calendars()
        .into_iter()
        .filter(|calendar| {
            matches!(
                calendar.source,
                CalendarSource::CalDav { account_id } if account_id == account.id
            )
        })
        .collect::<Vec<_>>();
    for calendar in calendars {
        let pulled = pull_calendar_full_snapshot(&client, &mut repository, calendar.id)
            .map_err(initial_pull_error)?;
        summary.calendars += 1;
        summary.added += pulled.added;
        summary.updated += pulled.updated;
        summary.deleted += pulled.deleted;
        summary.skipped += pulled.skipped;
    }
    Ok(summary)
}

fn pull_calendar_full_snapshot(
    client: &CaldavClient,
    repository: &mut SqliteRepository,
    calendar_id: Uuid,
) -> Result<RemoteSnapshotSummary, PullSyncError> {
    let sync_state = repository
        .get_calendar_sync_state(calendar_id)
        .ok_or(PullSyncError::MissingCalendarSyncState)?;
    let resources = client
        .fetch_resources(&sync_state.remote_url)
        .map_err(PullSyncError::Caldav)?;
    repository
        .reconcile_remote_snapshot(calendar_id, &resources)
        .map_err(PullSyncError::Repository)
}

fn initial_pull_error(error: PullSyncError) -> InitialPullWorkerError {
    match error {
        PullSyncError::Caldav(_) => InitialPullWorkerError::Caldav,
        PullSyncError::MissingCalendarSyncState => InitialPullWorkerError::MissingCalendarSyncState,
        PullSyncError::Repository(_) => InitialPullWorkerError::Repository,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingPushSummary {
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub conflicts: usize,
    pub skipped: usize,
}

#[derive(Debug)]
pub enum PushSyncError {
    Caldav(CaldavError),
    MissingCalendarSyncState,
    Repository(RepositoryError),
}

/// Pull and reconcile a complete calendar snapshot.
///
/// This blocking boundary must be called off GTK's main thread.
pub fn pull_calendar_snapshot(
    client: &CaldavClient,
    repository: &mut SqliteRepository,
    calendar_id: Uuid,
) -> Result<RemoteSnapshotSummary, PullSyncError> {
    let sync_state = repository
        .get_calendar_sync_state(calendar_id)
        .ok_or(PullSyncError::MissingCalendarSyncState)?;

    let Some(sync_token) = sync_state
        .sync_token
        .as_deref()
        .filter(|token| !token.trim().is_empty())
    else {
        let resources = client
            .fetch_resources(&sync_state.remote_url)
            .map_err(PullSyncError::Caldav)?;
        return repository
            .reconcile_remote_snapshot(calendar_id, &resources)
            .map_err(PullSyncError::Repository);
    };

    match client.fetch_changes(&sync_state.remote_url, sync_token) {
        Ok(changes) => repository
            .reconcile_remote_changes(calendar_id, &changes)
            .map_err(PullSyncError::Repository),
        Err(CaldavError::HttpStatus { status: 403 }) => {
            let resources = client
                .fetch_resources(&sync_state.remote_url)
                .map_err(PullSyncError::Caldav)?;
            repository
                .reconcile_remote_snapshot_clearing_sync_token(calendar_id, &resources)
                .map_err(PullSyncError::Repository)
        }
        Err(error) => Err(PullSyncError::Caldav(error)),
    }
}

/// Push durable local changes to CalDAV in repository order.
///
/// This blocking boundary must be called off GTK's main thread.
pub fn push_pending_operations(
    client: &CaldavClient,
    repository: &mut SqliteRepository,
    calendar_id: Uuid,
) -> Result<PendingPushSummary, PushSyncError> {
    // Read the calendar identity before looking at pending work. Apart from
    // avoiding an HTTP request for an unconfigured calendar, this makes the
    // blocking boundary's missing-state behavior deterministic.
    let sync_state = repository
        .get_calendar_sync_state(calendar_id)
        .ok_or(PushSyncError::MissingCalendarSyncState)?;
    let operations = repository.list_pending_sync_operations(calendar_id);
    let mut summary = PendingPushSummary {
        created: 0,
        updated: 0,
        deleted: 0,
        conflicts: 0,
        skipped: 0,
    };

    for operation in operations {
        match &operation {
            PendingSyncOperation::Create {
                event_id,
                remote_uid,
                ..
            } => {
                let Some(event) = repository.get_event(*event_id) else {
                    summary.skipped += 1;
                    continue;
                };
                let Ok(calendar_data) = serialize_icalendar_event(&event, remote_uid) else {
                    summary.skipped += 1;
                    continue;
                };
                let resource_url = resource_url(&sync_state.remote_url, *event_id)?;

                let result = match client.create_resource(&resource_url, &calendar_data) {
                    Ok(result) => result,
                    Err(CaldavError::HttpStatus { status: 412 }) => {
                        summary.conflicts += 1;
                        continue;
                    }
                    Err(error) => return Err(PushSyncError::Caldav(error)),
                };
                repository
                    .finalize_event_upload(&EventSyncState {
                        calendar_id,
                        event_id: *event_id,
                        remote_href: resource_url,
                        remote_uid: remote_uid.clone(),
                        etag: result.etag,
                    })
                    .map_err(PushSyncError::Repository)?;
                summary.created += 1;
            }
            PendingSyncOperation::Update {
                event_id,
                remote_href,
                remote_uid,
                base_etag,
                ..
            } => {
                let Some(base_etag) = base_etag.as_deref() else {
                    summary.skipped += 1;
                    continue;
                };
                let Some(event) = repository.get_event(*event_id) else {
                    summary.skipped += 1;
                    continue;
                };
                let Ok(calendar_data) = serialize_icalendar_event(&event, remote_uid) else {
                    summary.skipped += 1;
                    continue;
                };

                let result = match client.update_resource(remote_href, &calendar_data, base_etag) {
                    Ok(result) => result,
                    Err(CaldavError::HttpStatus { status: 412 }) => {
                        summary.conflicts += 1;
                        continue;
                    }
                    Err(error) => return Err(PushSyncError::Caldav(error)),
                };
                repository
                    .finalize_event_upload(&EventSyncState {
                        calendar_id,
                        event_id: *event_id,
                        remote_href: remote_href.clone(),
                        remote_uid: remote_uid.clone(),
                        // Keep the base validator when a server omits a new
                        // ETag, rather than clearing useful local metadata.
                        etag: result.etag.or_else(|| Some(base_etag.to_owned())),
                    })
                    .map_err(PushSyncError::Repository)?;
                summary.updated += 1;
            }
            PendingSyncOperation::Delete {
                event_id,
                remote_href,
                base_etag,
                ..
            } => {
                let Some(base_etag) = base_etag.as_deref() else {
                    summary.skipped += 1;
                    continue;
                };

                match client.delete_resource(remote_href, base_etag) {
                    Ok(()) | Err(CaldavError::HttpStatus { status: 404 }) => {}
                    Err(CaldavError::HttpStatus { status: 412 }) => {
                        summary.conflicts += 1;
                        continue;
                    }
                    Err(error) => return Err(PushSyncError::Caldav(error)),
                }
                repository
                    .finalize_event_delete(*event_id)
                    .map_err(PushSyncError::Repository)?;
                summary.deleted += 1;
            }
        }
    }

    Ok(summary)
}

fn resource_url(calendar_url: &str, event_id: Uuid) -> Result<String, PushSyncError> {
    Url::parse(calendar_url)
        .map_err(|_| PushSyncError::Caldav(CaldavError::Url))?
        .join(&format!("{event_id}.ics"))
        .map(|url| url.to_string())
        .map_err(|_| PushSyncError::Caldav(CaldavError::Url))
}
