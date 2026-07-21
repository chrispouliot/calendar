use uuid::Uuid;

use super::caldav::{CaldavClient, CaldavError};
use super::{RemoteSnapshotSummary, RepositoryError, SqliteRepository, SyncStateRepository};

#[derive(Debug)]
pub enum PullSyncError {
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
    let resources = client
        .fetch_resources(&sync_state.remote_url)
        .map_err(PullSyncError::Caldav)?;

    repository
        .reconcile_remote_snapshot(calendar_id, &resources)
        .map_err(PullSyncError::Repository)
}
