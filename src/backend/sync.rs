use reqwest::Url;
use uuid::Uuid;

use super::caldav::{CaldavClient, CaldavError, serialize_icalendar_event};
use super::{
    EventRepository, PendingSyncOperationRepository, RemoteSnapshotSummary, RepositoryError,
    SqliteRepository, SyncStateRepository,
};
use crate::model::{EventSyncState, PendingSyncOperation};

#[derive(Debug)]
pub enum PullSyncError {
    Caldav(CaldavError),
    MissingCalendarSyncState,
    Repository(RepositoryError),
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
