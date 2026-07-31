use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone, Utc};
use reqwest::Url;
use rrule::{RRuleSet, Tz as RRuleTz};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, DateTimeRange, DetachedEvent, Event,
    EventSchedule, EventSyncState, PendingSyncOperation, RecurrenceId, RecurrenceSpec,
    ReminderSpec,
};

use super::{
    AccountRepository, CalendarRepository, EventDeletionUndo, EventRepository,
    PendingSyncOperationRepository, RemoteSnapshotSummary, RepositoryError, SyncStateRepository,
    caldav::{CaldavDiscovery, DiscoveredCalendar},
};
use crate::recurrence_form::{
    RecurrencePresentation, recurrence_presentation, split_recurrence_at,
};

pub struct SqliteRepository {
    conn: Connection,
}

/// One-shot restoration state for a detached occurrence mutation.
#[derive(Debug)]
pub struct OccurrenceUndo {
    master_event_id: Uuid,
    recurrence_id: RecurrenceId,
    prior_detached_event: Option<DetachedEvent>,
    prior_pending_operation: Option<PendingSyncOperation>,
    cancellation_pending_operation: Option<PendingSyncOperation>,
    cancelled_child_id: Uuid,
    restored: bool,
}

/// Result of splitting a recurring event into its original and future masters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FollowingEditResult {
    future_master_id: Uuid,
}

impl FollowingEditResult {
    pub fn future_master_id(&self) -> Uuid {
        self.future_master_id
    }
}

/// One-shot restoration state for deleting an occurrence and all following
/// occurrences from a recurring master.
#[derive(Debug)]
pub struct FollowingUndo {
    master_event_id: Uuid,
    prior_event: Event,
    truncated_event: Event,
    prior_children: Vec<(Uuid, DetachedEvent)>,
    remaining_children: Vec<(Uuid, DetachedEvent)>,
    prior_sync_state: Option<EventSyncState>,
    prior_pending_operation: Option<PendingSyncOperation>,
    deletion_pending_operation: Option<PendingSyncOperation>,
    restored: bool,
}

impl SqliteRepository {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, RepositoryError> {
        let conn = Connection::open(path).map_err(|_| RepositoryError)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(|_| RepositoryError)?;
        let foreign_keys: i32 = conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .map_err(|_| RepositoryError)?;
        if foreign_keys != 1 {
            return Err(RepositoryError);
        }
        // Enable WAL mode for better concurrent-reader behaviour (future-proofing).
        conn.execute_batch("PRAGMA journal_mode=WAL;")
            .map_err(|_| RepositoryError)?;
        let repo = SqliteRepository { conn };
        repo.init_schema()?;
        Ok(repo)
    }

    fn init_schema(&self) -> Result<(), RepositoryError> {
        self.conn
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS accounts (
                    id BLOB PRIMARY KEY,
                    name TEXT NOT NULL,
                    server_url TEXT NOT NULL,
                    username TEXT NOT NULL,
                    enabled INTEGER NOT NULL
                );

                CREATE TABLE IF NOT EXISTS calendars (
                    id BLOB PRIMARY KEY,
                    name TEXT NOT NULL,
                    color TEXT NOT NULL,
                    visible INTEGER NOT NULL,
                    read_only INTEGER NOT NULL,
                    source TEXT NOT NULL,
                    account_id BLOB,
                    FOREIGN KEY (account_id) REFERENCES accounts(id)
                        ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS events (
                    id BLOB PRIMARY KEY,
                    calendar_id BLOB NOT NULL,
                    title TEXT NOT NULL,
                    location TEXT NOT NULL,
                    description TEXT NOT NULL,
                    schedule_type TEXT NOT NULL
                        CHECK (schedule_type IN ('all_day', 'timed')),
                    start_date TEXT,
                    end_date_exclusive TEXT,
                    start_datetime TEXT,
                    end_datetime TEXT,
                    timezone TEXT,
                    start_unix INTEGER NOT NULL DEFAULT 0,
                    end_unix INTEGER NOT NULL DEFAULT 0,
                    recurrence_enabled INTEGER NOT NULL DEFAULT 0,
                    recurrence_data TEXT,
                    FOREIGN KEY (calendar_id) REFERENCES calendars(id)
                        ON DELETE CASCADE
                );

                 CREATE TABLE IF NOT EXISTS reminders (
                     id BLOB PRIMARY KEY,
                     event_id BLOB NOT NULL,
                     seconds_before_start INTEGER NOT NULL,
                     description TEXT NOT NULL,
                     FOREIGN KEY (event_id) REFERENCES events(id)
                         ON DELETE CASCADE
                 );

                 CREATE TABLE IF NOT EXISTS sync_metadata (
                     id BLOB PRIMARY KEY,
                     calendar_id BLOB NOT NULL,
                     remote_url TEXT NOT NULL,
                     sync_token TEXT,
                     UNIQUE (calendar_id),
                     FOREIGN KEY (calendar_id) REFERENCES calendars(id)
                         ON DELETE CASCADE
                 );

                 CREATE TABLE IF NOT EXISTS event_sync_metadata (
                     id BLOB PRIMARY KEY,
                     calendar_id BLOB NOT NULL,
                     event_id BLOB NOT NULL UNIQUE,
                      remote_href TEXT NOT NULL,
                      remote_uid TEXT NOT NULL,
                      etag TEXT,
                      UNIQUE (calendar_id, remote_href),
                      FOREIGN KEY (calendar_id) REFERENCES calendars(id)
                         ON DELETE CASCADE,
                     FOREIGN KEY (event_id) REFERENCES events(id)
                         ON DELETE CASCADE
                 );

                 CREATE INDEX IF NOT EXISTS event_sync_metadata_calendar_idx
                     ON event_sync_metadata(calendar_id);

                  CREATE TABLE IF NOT EXISTS pending_sync_operations (
                      event_id BLOB PRIMARY KEY,
                      calendar_id BLOB NOT NULL,
                      operation_kind TEXT NOT NULL
                          CHECK (operation_kind IN ('create', 'update', 'delete')),
                      remote_href TEXT,
                      remote_uid TEXT NOT NULL,
                      base_etag TEXT,
                      CHECK (
                          (operation_kind = 'create'
                              AND remote_href IS NULL
                              AND base_etag IS NULL)
                          OR (operation_kind IN ('update', 'delete')
                              AND remote_href IS NOT NULL)
                      ),
                      FOREIGN KEY (calendar_id) REFERENCES calendars(id)
                          ON DELETE CASCADE
                  );

                  CREATE INDEX IF NOT EXISTS pending_sync_operations_calendar_idx
                      ON pending_sync_operations(calendar_id, event_id);

                  CREATE TABLE IF NOT EXISTS calendar_seed_state (
                     id INTEGER PRIMARY KEY CHECK (id = 1)
                 );

                 CREATE TABLE IF NOT EXISTS detached_events (
                     id BLOB PRIMARY KEY,
                     master_event_id BLOB NOT NULL,
                     recurrence_kind TEXT NOT NULL
                         CHECK (recurrence_kind IN ('all_day', 'timed')),
                     recurrence_date TEXT,
                     recurrence_datetime TEXT,
                     recurrence_timezone TEXT,
                     recurrence_sort TEXT NOT NULL,
                     cancelled INTEGER NOT NULL,
                     title TEXT NOT NULL DEFAULT '',
                     location TEXT NOT NULL DEFAULT '',
                     description TEXT NOT NULL DEFAULT '',
                     schedule_type TEXT
                         CHECK (schedule_type IS NULL OR schedule_type IN ('all_day', 'timed')),
                     start_date TEXT,
                     end_date_exclusive TEXT,
                     start_datetime TEXT,
                     end_datetime TEXT,
                     timezone TEXT,
                     FOREIGN KEY (master_event_id) REFERENCES events(id)
                         ON DELETE CASCADE
                 );

                 CREATE INDEX IF NOT EXISTS detached_events_master_idx
                     ON detached_events(master_event_id, recurrence_sort, id);

                 CREATE UNIQUE INDEX IF NOT EXISTS detached_events_master_recurrence_idx
                     ON detached_events(
                         master_event_id,
                         recurrence_kind,
                         COALESCE(recurrence_date, ''),
                         COALESCE(recurrence_datetime, ''),
                         COALESCE(recurrence_timezone, '')
                     );

                 CREATE TABLE IF NOT EXISTS detached_event_reminders (
                     id BLOB PRIMARY KEY,
                     detached_event_id BLOB NOT NULL,
                     seconds_before_start INTEGER NOT NULL,
                     description TEXT NOT NULL,
                     FOREIGN KEY (detached_event_id) REFERENCES detached_events(id)
                         ON DELETE CASCADE
                 );",
            )
            .map_err(|_| RepositoryError)?;

        // Migrate databases created by the earlier, uncommitted Phase 7
        // working tree that lacked the unix-timestamp columns used for
        // offset-independent range queries.
        for col in &["start_unix", "end_unix"] {
            let _ = self.conn.execute(
                &format!("ALTER TABLE events ADD COLUMN {col} INTEGER NOT NULL DEFAULT 0"),
                [],
            );
        }
        // Add the Phase 11 account association to databases created before
        // accounts were introduced. Existing rows remain local calendars.
        let _ = self.conn.execute(
            "ALTER TABLE calendars ADD COLUMN account_id BLOB REFERENCES accounts(id) ON DELETE CASCADE",
            [],
        );
        // The original Phase 7 reminders table was a never-populated
        // placeholder. Add the reminder payload columns without rebuilding it.
        let mut migrated_reminders = false;
        for (name, definition) in [
            ("seconds_before_start", "INTEGER NOT NULL DEFAULT 0"),
            ("description", "TEXT NOT NULL DEFAULT ''"),
        ] {
            if !self.reminders_has_column(name)? {
                self.conn
                    .execute(
                        &format!("ALTER TABLE reminders ADD COLUMN {name} {definition}"),
                        [],
                    )
                    .map_err(|_| RepositoryError)?;
                migrated_reminders = true;
            }
        }
        if migrated_reminders {
            self.conn
                .execute("UPDATE sync_metadata SET sync_token = NULL", [])
                .map_err(|_| RepositoryError)?;
        }

        // The original Phase 7 placeholder had only id and calendar_id. Add
        // the identity columns in place so existing placeholder rows remain.
        for (name, definition) in [("remote_url", "TEXT"), ("sync_token", "TEXT")] {
            if !self.sync_metadata_has_column(name)? {
                self.conn
                    .execute(
                        &format!("ALTER TABLE sync_metadata ADD COLUMN {name} {definition}"),
                        [],
                    )
                    .map_err(|_| RepositoryError)?;
            }
        }
        // Preserve the legacy enabled bit when old databases have no rule
        // text; new rows retain the complete recurrence properties.
        let _ = self
            .conn
            .execute("ALTER TABLE events ADD COLUMN recurrence_data TEXT", []);
        // Legacy placeholder rows have a NULL remote_url. They are retained,
        // while actual sync state rows are unique by calendar.
        self.conn
            .execute(
                "CREATE UNIQUE INDEX IF NOT EXISTS sync_metadata_calendar_state_idx
                 ON sync_metadata(calendar_id) WHERE remote_url IS NOT NULL",
                [],
            )
            .map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn sync_metadata_has_column(&self, name: &str) -> Result<bool, RepositoryError> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(sync_metadata)")
            .map_err(|_| RepositoryError)?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|_| RepositoryError)?;
        for column in columns {
            if column.map_err(|_| RepositoryError)? == name {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn reminders_has_column(&self, name: &str) -> Result<bool, RepositoryError> {
        let mut stmt = self
            .conn
            .prepare("PRAGMA table_info(reminders)")
            .map_err(|_| RepositoryError)?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|_| RepositoryError)?;
        for column in columns {
            if column.map_err(|_| RepositoryError)? == name {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Replace the complete detached exception set for one recurring master.
    /// The delete and inserts are committed as one unit so readers never see
    /// a partially replaced resource.
    pub fn replace_detached_events(
        &mut self,
        master_event_id: Uuid,
        exceptions: &[DetachedEvent],
    ) -> Result<(), RepositoryError> {
        let mut recurrence_ids = HashSet::new();
        if exceptions
            .iter()
            .any(|exception| !recurrence_ids.insert(detached_recurrence_identity(exception)))
        {
            return Err(RepositoryError);
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let master_exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
                params![master_event_id],
                |row| row.get(0),
            )
            .map_err(|_| RepositoryError)?;
        if !master_exists {
            return Err(RepositoryError);
        }

        replace_detached_events_in_transaction(&tx, master_event_id, exceptions)?;
        tx.commit().map_err(|_| RepositoryError)
    }

    /// List detached exceptions in recurrence-id order.
    pub fn list_detached_events(&self, master_event_id: Uuid) -> Vec<DetachedEvent> {
        let mut statement = match self.conn.prepare(
            "SELECT id, recurrence_kind, recurrence_date, recurrence_datetime,
                    recurrence_timezone, cancelled, title, location, description,
                    schedule_type, start_date, end_date_exclusive,
                    start_datetime, end_datetime, timezone
             FROM detached_events
             WHERE master_event_id = ?1
             ORDER BY recurrence_sort ASC, recurrence_kind ASC, id ASC",
        ) {
            Ok(statement) => statement,
            Err(_) => return Vec::new(),
        };
        let mut events: Vec<(Uuid, DetachedEvent)> =
            match statement.query_map(params![master_event_id], detached_event_from_row) {
                Ok(rows) => rows.filter_map(|row| row.ok()).collect(),
                Err(_) => return Vec::new(),
            };
        events
            .drain(..)
            .filter_map(|(id, mut event)| {
                if let DetachedEvent::Modified { reminders, .. } = &mut event {
                    *reminders = reminders_for_detached_event(&self.conn, id).ok()?;
                }
                Some(event)
            })
            .collect()
    }

    /// Replace one generated occurrence without disturbing any of its
    /// siblings.  The sync operation is attached to the recurring master,
    /// rather than to a detached row (detached rows have no remote identity).
    pub fn upsert_occurrence_with_sync(
        &mut self,
        master_event_id: Uuid,
        exception: &DetachedEvent,
    ) -> Result<(), RepositoryError> {
        let recurrence_id = detached_recurrence_id(exception);
        if !matches!(exception, DetachedEvent::Modified { .. }) {
            return Err(RepositoryError);
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let master = event_in_transaction(&tx, master_event_id)?;
        if master.recurrence.is_none() {
            return Err(RepositoryError);
        }
        let calendar = calendar_in_transaction(&tx, master.calendar_id)?;
        if calendar.read_only || !occurrence_is_generated(&master, recurrence_id) {
            return Err(RepositoryError);
        }

        let pending = pending_sync_operation_in_transaction(&tx, master_event_id)?;
        let sync_state = event_sync_state_in_transaction(&tx, master_event_id)?;
        let operation =
            occurrence_pending_operation(&calendar, master_event_id, pending, sync_state)?;

        if let Some((child_id, _)) =
            detached_event_for_recurrence_in_transaction(&tx, master_event_id, recurrence_id)?
        {
            tx.execute(
                "DELETE FROM detached_events WHERE id = ?1",
                params![child_id],
            )
            .map_err(|_| RepositoryError)?;
        }
        insert_detached_event(&tx, master_event_id, exception)?;
        if let Some(operation) = operation {
            upsert_pending_sync_operation_in_transaction(&tx, &operation)?;
        } else {
            delete_pending_sync_operation_in_transaction(&tx, master_event_id)?;
        }
        tx.commit().map_err(|_| RepositoryError)
    }

    /// Cancel one generated occurrence and return a one-shot undo token.
    pub fn cancel_occurrence_with_sync_undo(
        &mut self,
        master_event_id: Uuid,
        recurrence_id: &RecurrenceId,
    ) -> Result<OccurrenceUndo, RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let master = event_in_transaction(&tx, master_event_id)?;
        if master.recurrence.is_none() {
            return Err(RepositoryError);
        }
        let calendar = calendar_in_transaction(&tx, master.calendar_id)?;
        if calendar.read_only || !occurrence_is_generated(&master, recurrence_id) {
            return Err(RepositoryError);
        }

        let prior_pending_operation = pending_sync_operation_in_transaction(&tx, master_event_id)?;
        let sync_state = event_sync_state_in_transaction(&tx, master_event_id)?;
        let cancellation_pending_operation = occurrence_pending_operation(
            &calendar,
            master_event_id,
            prior_pending_operation.clone(),
            sync_state,
        )?;
        let prior_detached_event =
            detached_event_for_recurrence_in_transaction(&tx, master_event_id, recurrence_id)?;
        if let Some((child_id, _)) = &prior_detached_event {
            tx.execute(
                "DELETE FROM detached_events WHERE id = ?1",
                params![child_id],
            )
            .map_err(|_| RepositoryError)?;
        }
        insert_detached_event(
            &tx,
            master_event_id,
            &DetachedEvent::Cancelled {
                recurrence_id: recurrence_id.clone(),
            },
        )?;
        let cancelled_child_id =
            detached_event_for_recurrence_in_transaction(&tx, master_event_id, recurrence_id)?
                .map(|(id, _)| id)
                .ok_or(RepositoryError)?;
        if let Some(operation) = &cancellation_pending_operation {
            upsert_pending_sync_operation_in_transaction(&tx, operation)?;
        } else {
            delete_pending_sync_operation_in_transaction(&tx, master_event_id)?;
        }
        tx.commit().map_err(|_| RepositoryError)?;

        Ok(OccurrenceUndo {
            master_event_id,
            recurrence_id: recurrence_id.clone(),
            prior_detached_event: prior_detached_event.map(|(_, event)| event),
            prior_pending_operation,
            cancellation_pending_operation,
            cancelled_child_id,
            restored: false,
        })
    }

    /// Restore the child and pending-sync intent captured by a cancellation.
    pub fn undo_occurrence_with_sync(
        &mut self,
        undo: &mut OccurrenceUndo,
    ) -> Result<(), RepositoryError> {
        if undo.restored {
            return Err(RepositoryError);
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let current = detached_event_for_recurrence_in_transaction(
            &tx,
            undo.master_event_id,
            &undo.recurrence_id,
        )?;
        let current_is_cancelled = current.as_ref().is_some_and(|(id, event)| {
            *id == undo.cancelled_child_id
                && event
                    == &DetachedEvent::Cancelled {
                        recurrence_id: undo.recurrence_id.clone(),
                    }
        });
        if !current_is_cancelled {
            return Err(RepositoryError);
        }
        let current_pending = pending_sync_operation_in_transaction(&tx, undo.master_event_id)?;
        if current_pending != undo.cancellation_pending_operation {
            return Err(RepositoryError);
        }

        tx.execute(
            "DELETE FROM detached_events WHERE id = ?1",
            params![undo.cancelled_child_id],
        )
        .map_err(|_| RepositoryError)?;
        if let Some(event) = &undo.prior_detached_event {
            insert_detached_event(&tx, undo.master_event_id, event)?;
        }
        delete_pending_sync_operation_in_transaction(&tx, undo.master_event_id)?;
        if let Some(operation) = &undo.prior_pending_operation {
            upsert_pending_sync_operation_in_transaction(&tx, operation)?;
        }
        tx.commit().map_err(|_| RepositoryError)?;
        undo.restored = true;
        Ok(())
    }

    /// Split a writable recurring master immediately before one modified
    /// occurrence. Detached exceptions after the split are moved to the new
    /// master and their recurrence identities are rebased to its schedule.
    pub fn edit_this_and_following_with_sync(
        &mut self,
        master_event_id: Uuid,
        edited: &DetachedEvent,
    ) -> Result<FollowingEditResult, RepositoryError> {
        self.edit_this_and_following_with_sync_inner(master_event_id, edited, None)
    }

    /// Split a writable recurring master while explicitly replacing the
    /// recurrence of the future master.  Unlike the compatibility operation
    /// above, this also permits an all-day master to become a timed future
    /// series whose DTSTART is moved to another weekday.
    pub fn edit_this_and_following_with_sync_and_recurrence(
        &mut self,
        master_event_id: Uuid,
        edited: &DetachedEvent,
        future_recurrence: &RecurrenceSpec,
    ) -> Result<FollowingEditResult, RepositoryError> {
        self.edit_this_and_following_with_sync_inner(
            master_event_id,
            edited,
            Some(future_recurrence),
        )
    }

    fn edit_this_and_following_with_sync_inner(
        &mut self,
        master_event_id: Uuid,
        edited: &DetachedEvent,
        requested_future_recurrence: Option<&RecurrenceSpec>,
    ) -> Result<FollowingEditResult, RepositoryError> {
        let (recurrence_id, title, location, description, schedule, reminders) = match edited {
            DetachedEvent::Modified {
                recurrence_id,
                title,
                location,
                description,
                schedule,
                reminders,
            } => (
                recurrence_id,
                title,
                location,
                description,
                schedule,
                reminders,
            ),
            DetachedEvent::Cancelled { .. } => return Err(RepositoryError),
        };

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let master = event_with_reminders_in_transaction(&tx, master_event_id)?;
        let calendar = calendar_in_transaction(&tx, master.calendar_id)?;
        if calendar.read_only
            || !edited_schedule_is_supported(
                &master,
                recurrence_id,
                schedule,
                requested_future_recurrence.is_some(),
            )
        {
            return Err(RepositoryError);
        }
        let split = split_recurrence_at(&master, recurrence_id).map_err(|_| RepositoryError)?;
        let future_recurrence = requested_future_recurrence
            .cloned()
            .unwrap_or(split.future_recurrence);
        if requested_future_recurrence.is_some()
            && !future_recurrence_is_supported(schedule, &future_recurrence)
        {
            return Err(RepositoryError);
        }

        let prior_pending = pending_sync_operation_in_transaction(&tx, master_event_id)?;
        let sync_state = event_sync_state_in_transaction(&tx, master_event_id)?;
        let original_operation = occurrence_pending_operation(
            &calendar,
            master_event_id,
            prior_pending,
            sync_state.clone(),
        )?;
        let future_master_id = new_future_master_id(&tx, sync_state.as_ref())?;
        let future = Event {
            id: future_master_id,
            calendar_id: master.calendar_id,
            title: title.clone(),
            location: location.clone(),
            description: description.clone(),
            schedule: schedule.clone(),
            recurrence: Some(future_recurrence),
            reminders: reminders.clone(),
        };

        // Validate and prepare every child before changing either master. A
        // same-date time edit changes the generated identity of all following
        // timed instances, so merely changing the child master would make
        // those exceptions unresolvable.
        let children = detached_events_in_transaction(&tx, master_event_id)?;
        let split_sort = detached_recurrence_sort(recurrence_id);
        let mut selected_child_ids = Vec::new();
        let mut future_children = Vec::new();
        for (id, child) in &children {
            let child_id = detached_recurrence_id(child);
            if detached_recurrence_ids_match(child_id, recurrence_id) {
                selected_child_ids.push(*id);
            } else if detached_recurrence_sort(child_id) > split_sort {
                if !occurrence_is_generated(&master, child_id) {
                    return Err(RepositoryError);
                }
                let rebased_id = rebase_following_recurrence_id(&master, &future, child_id)?;
                future_children.push((*id, rebase_detached_event(child, rebased_id)));
            }
        }
        let mut rebased_ids = HashSet::new();
        if future_children
            .iter()
            .any(|(_, child)| !rebased_ids.insert(detached_recurrence_identity(child)))
        {
            return Err(RepositoryError);
        }

        let mut truncated = master.clone();
        truncated.recurrence = Some(split.original_recurrence);
        update_event_in_transaction(&tx, &truncated)?;

        insert_event(&tx, &future)?;

        for child_id in selected_child_ids {
            tx.execute(
                "DELETE FROM detached_events WHERE id = ?1",
                params![child_id],
            )
            .map_err(|_| RepositoryError)?;
        }
        for (child_id, child) in future_children {
            tx.execute(
                "DELETE FROM detached_events WHERE id = ?1",
                params![child_id],
            )
            .map_err(|_| RepositoryError)?;
            insert_detached_event_with_id(&tx, future_master_id, &child, child_id)?;
        }

        if let Some(operation) = original_operation {
            upsert_pending_sync_operation_in_transaction(&tx, &operation)?;
        } else {
            delete_pending_sync_operation_in_transaction(&tx, master_event_id)?;
        }
        if !matches!(calendar.source, CalendarSource::Local) {
            let remote_uid = distinct_remote_uid(future_master_id, sync_state.as_ref());
            upsert_pending_sync_operation_in_transaction(
                &tx,
                &PendingSyncOperation::Create {
                    calendar_id: master.calendar_id,
                    event_id: future_master_id,
                    remote_uid,
                },
            )?;
        }
        tx.commit().map_err(|_| RepositoryError)?;
        Ok(FollowingEditResult { future_master_id })
    }

    /// Delete one generated occurrence and all following occurrences from a
    /// recurring master, retaining a one-shot token for exact restoration.
    pub fn delete_this_and_following_with_sync_undo(
        &mut self,
        master_event_id: Uuid,
        recurrence_id: &RecurrenceId,
    ) -> Result<FollowingUndo, RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let prior_event = event_with_reminders_in_transaction(&tx, master_event_id)?;
        let calendar = calendar_in_transaction(&tx, prior_event.calendar_id)?;
        if calendar.read_only || prior_event.recurrence.is_none() {
            return Err(RepositoryError);
        }
        let truncated_recurrence = truncate_recurrence_for_delete(&prior_event, recurrence_id)?;
        let prior_children = detached_events_in_transaction(&tx, master_event_id)?;
        let split_sort = detached_recurrence_sort(recurrence_id);
        let (remaining_children, removed_children): (Vec<_>, Vec<_>) =
            prior_children.iter().cloned().partition(|(_, child)| {
                detached_recurrence_sort(detached_recurrence_id(child)) < split_sort
            });

        let prior_pending_operation = pending_sync_operation_in_transaction(&tx, master_event_id)?;
        let prior_sync_state = event_sync_state_in_transaction(&tx, master_event_id)?;
        let deletion_pending_operation = occurrence_pending_operation(
            &calendar,
            master_event_id,
            prior_pending_operation.clone(),
            prior_sync_state.clone(),
        )?;

        let mut truncated_event = prior_event.clone();
        truncated_event.recurrence = Some(truncated_recurrence);
        update_event_in_transaction(&tx, &truncated_event)?;
        for (child_id, _) in removed_children {
            tx.execute(
                "DELETE FROM detached_events WHERE id = ?1",
                params![child_id],
            )
            .map_err(|_| RepositoryError)?;
        }
        if let Some(operation) = &deletion_pending_operation {
            upsert_pending_sync_operation_in_transaction(&tx, operation)?;
        } else {
            delete_pending_sync_operation_in_transaction(&tx, master_event_id)?;
        }
        tx.commit().map_err(|_| RepositoryError)?;

        Ok(FollowingUndo {
            master_event_id,
            prior_event,
            truncated_event,
            prior_children,
            remaining_children,
            prior_sync_state,
            prior_pending_operation,
            deletion_pending_operation,
            restored: false,
        })
    }

    /// Restore a following-occurrence deletion exactly once.
    pub fn undo_this_and_following_with_sync(
        &mut self,
        undo: &mut FollowingUndo,
    ) -> Result<(), RepositoryError> {
        if undo.restored {
            return Err(RepositoryError);
        }
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let current_event = event_with_reminders_in_transaction(&tx, undo.master_event_id)?;
        let current_children = detached_events_in_transaction(&tx, undo.master_event_id)?;
        let current_pending = pending_sync_operation_in_transaction(&tx, undo.master_event_id)?;
        let current_sync_state = event_sync_state_in_transaction(&tx, undo.master_event_id)?;
        if current_event != undo.truncated_event
            || current_children != undo.remaining_children
            || current_pending != undo.deletion_pending_operation
            || current_sync_state != undo.prior_sync_state
        {
            return Err(RepositoryError);
        }
        let calendar = calendar_in_transaction(&tx, undo.prior_event.calendar_id)?;
        if calendar.read_only {
            return Err(RepositoryError);
        }

        update_event_in_transaction(&tx, &undo.prior_event)?;
        tx.execute(
            "DELETE FROM detached_events WHERE master_event_id = ?1",
            params![undo.master_event_id],
        )
        .map_err(|_| RepositoryError)?;
        for (child_id, child) in &undo.prior_children {
            insert_detached_event_with_id(&tx, undo.master_event_id, child, *child_id)?;
        }
        delete_pending_sync_operation_in_transaction(&tx, undo.master_event_id)?;
        if let Some(operation) = &undo.prior_pending_operation {
            upsert_pending_sync_operation_in_transaction(&tx, operation)?;
        }
        tx.commit().map_err(|_| RepositoryError)?;
        undo.restored = true;
        Ok(())
    }

    /// Atomically persist an account and the calendars returned by CalDAV
    /// discovery. Remote calendar identity is scoped to its account.
    pub fn provision_caldav_account(
        &mut self,
        account: &Account,
        discovery: &CaldavDiscovery,
    ) -> Result<Vec<Calendar>, RepositoryError> {
        let mut hrefs = HashSet::new();
        for discovered in &discovery.calendars {
            validate_discovered_href(&discovered.href)?;
            if !hrefs.insert(discovered.href.as_str()) {
                return Err(RepositoryError);
            }
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        tx.execute(
            "INSERT INTO accounts (id, name, server_url, username, enabled)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 name = excluded.name,
                 server_url = excluded.server_url,
                 username = excluded.username,
                 enabled = excluded.enabled",
            params![
                account.id,
                account.name,
                account.server_url,
                account.username,
                account.enabled as i32,
            ],
        )
        .map_err(|_| RepositoryError)?;

        let mut provisioned = Vec::with_capacity(discovery.calendars.len());
        for discovered in &discovery.calendars {
            let existing = tx
                .query_row(
                    "SELECT c.id, c.visible
                     FROM calendars AS c
                     JOIN sync_metadata AS s ON s.calendar_id = c.id
                     WHERE c.source = 'CalDav'
                       AND c.account_id = ?1
                       AND s.remote_url = ?2
                     ORDER BY c.id
                     LIMIT 1",
                    params![account.id, discovered.href],
                    |row| Ok((row.get::<_, Uuid>(0)?, row.get::<_, i32>(1)? != 0)),
                )
                .optional()
                .map_err(|_| RepositoryError)?;

            let (calendar_id, visible) = existing.unwrap_or((Uuid::new_v4(), true));
            let calendar = Calendar {
                id: calendar_id,
                name: discovered_calendar_name(discovered),
                color: normalize_discovered_color(discovered.color.as_deref()),
                visible,
                read_only: !discovered.writable,
                source: CalendarSource::CalDav {
                    account_id: account.id,
                },
            };
            let (source, account_id) = calendar_source_values(&calendar.source);
            tx.execute(
                "INSERT INTO calendars (id, name, color, visible, read_only, source, account_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                     name = excluded.name,
                     color = excluded.color,
                     visible = excluded.visible,
                     read_only = excluded.read_only,
                     source = excluded.source,
                     account_id = excluded.account_id",
                params![
                    calendar.id,
                    calendar.name,
                    calendar.color,
                    calendar.visible as i32,
                    calendar.read_only as i32,
                    source,
                    account_id,
                ],
            )
            .map_err(|_| RepositoryError)?;

            upsert_calendar_sync_state_in_transaction(
                &tx,
                &CalendarSyncState {
                    calendar_id,
                    remote_url: discovered.href.clone(),
                    sync_token: discovered.sync_token.clone(),
                },
            )?;
            provisioned.push((discovered.href.as_str(), calendar));
        }

        tx.commit().map_err(|_| RepositoryError)?;
        provisioned.sort_by(|left, right| left.0.cmp(right.0));
        Ok(provisioned
            .into_iter()
            .map(|(_, calendar)| calendar)
            .collect())
    }

    /// Finalize an upload while keeping its identity metadata and durable
    /// intent in the same SQLite transaction.
    pub(crate) fn finalize_event_upload(
        &mut self,
        state: &EventSyncState,
    ) -> Result<(), RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        upsert_event_sync_state_in_transaction(&tx, state)?;
        tx.execute(
            "DELETE FROM pending_sync_operations WHERE event_id = ?1",
            params![state.event_id],
        )
        .map_err(|_| RepositoryError)?;
        tx.commit().map_err(|_| RepositoryError)
    }

    /// Remove a completed delete intent transactionally. The event and its
    /// sync metadata may already have been removed by the local delete.
    pub(crate) fn finalize_event_delete(&mut self, event_id: Uuid) -> Result<(), RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        tx.execute(
            "DELETE FROM pending_sync_operations WHERE event_id = ?1",
            params![event_id],
        )
        .map_err(|_| RepositoryError)?;
        tx.commit().map_err(|_| RepositoryError)
    }

    pub fn reconcile_remote_snapshot(
        &mut self,
        calendar_id: Uuid,
        resources: &[super::caldav::ResourceRecord],
    ) -> Result<RemoteSnapshotSummary, RepositoryError> {
        self.reconcile_remote_snapshot_inner(calendar_id, resources, false)
    }

    pub(crate) fn reconcile_remote_snapshot_clearing_sync_token(
        &mut self,
        calendar_id: Uuid,
        resources: &[super::caldav::ResourceRecord],
    ) -> Result<RemoteSnapshotSummary, RepositoryError> {
        self.reconcile_remote_snapshot_inner(calendar_id, resources, true)
    }

    fn reconcile_remote_snapshot_inner(
        &mut self,
        calendar_id: Uuid,
        resources: &[super::caldav::ResourceRecord],
        clear_sync_token: bool,
    ) -> Result<RemoteSnapshotSummary, RepositoryError> {
        let calendar = self.get_calendar(calendar_id).ok_or(RepositoryError)?;
        if !matches!(calendar.source, CalendarSource::CalDav { .. }) {
            return Err(RepositoryError);
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let pending_operations = pending_sync_operations_in_transaction(&tx, calendar_id)?;
        let pending_protected_hrefs: HashSet<String> = pending_operations
            .iter()
            .filter_map(|operation| match operation {
                PendingSyncOperation::Update { remote_href, .. }
                | PendingSyncOperation::Delete { remote_href, .. } => Some(remote_href.clone()),
                PendingSyncOperation::Create { .. } => None,
            })
            .collect();
        let pending_protected_event_ids: HashSet<Uuid> = pending_operations
            .iter()
            .filter_map(|operation| match operation {
                PendingSyncOperation::Update { event_id, .. }
                | PendingSyncOperation::Delete { event_id, .. } => Some(*event_id),
                PendingSyncOperation::Create { .. } => None,
            })
            .collect();
        let tracked_states = {
            let mut statement = tx
                .prepare(
                    "SELECT calendar_id, event_id, remote_href, remote_uid, etag
                     FROM event_sync_metadata
                     WHERE calendar_id = ?1",
                )
                .map_err(|_| RepositoryError)?;
            statement
                .query_map(params![calendar_id], event_sync_state_from_row)
                .map_err(|_| RepositoryError)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| RepositoryError)?
        };
        let tracked_by_href: HashMap<String, EventSyncState> = tracked_states
            .iter()
            .cloned()
            .map(|state| (state.remote_href.clone(), state))
            .collect();

        let mut seen_hrefs = HashSet::new();
        let mut summary = RemoteSnapshotSummary {
            added: 0,
            updated: 0,
            deleted: 0,
            skipped: 0,
        };

        for resource in resources {
            seen_hrefs.insert(resource.href.clone());
            if pending_protected_hrefs.contains(&resource.href) {
                summary.skipped += 1;
                continue;
            }
            let tracked = tracked_by_href.get(&resource.href);

            if resource.response_status == Some(404) {
                if let Some(state) = tracked {
                    delete_remote_event(&tx, state)?;
                    summary.deleted += 1;
                } else {
                    summary.skipped += 1;
                }
                continue;
            }

            if resource
                .response_status
                .is_some_and(|status| !(200..300).contains(&status))
            {
                summary.skipped += 1;
                continue;
            }

            let Some(calendar_data) = resource.calendar_data.as_deref() else {
                summary.skipped += 1;
                continue;
            };

            let event_id = tracked.map_or_else(Uuid::new_v4, |state| state.event_id);
            let mapped =
                match super::caldav::map_icalendar_resource(calendar_data, event_id, calendar_id) {
                    Ok(mapped) => mapped,
                    Err(_) => {
                        summary.skipped += 1;
                        continue;
                    }
                };
            if !detached_events_are_valid(&mapped.exceptions) {
                summary.skipped += 1;
                continue;
            }
            let state = EventSyncState {
                calendar_id,
                event_id,
                remote_href: resource.href.clone(),
                remote_uid: mapped.master.remote_uid.clone(),
                etag: resource.etag.clone(),
            };

            if tracked.is_some() {
                replace_remote_event(&tx, &mapped.master.event)?;
                upsert_event_sync_state_in_transaction(&tx, &state)?;
                replace_detached_events_in_transaction(&tx, event_id, &mapped.exceptions)?;
                summary.updated += 1;
            } else {
                insert_event(&tx, &mapped.master.event)?;
                upsert_event_sync_state_in_transaction(&tx, &state)?;
                replace_detached_events_in_transaction(&tx, event_id, &mapped.exceptions)?;
                summary.added += 1;
            }
        }

        for state in &tracked_states {
            if !seen_hrefs.contains(&state.remote_href)
                && !pending_protected_hrefs.contains(&state.remote_href)
                && !pending_protected_event_ids.contains(&state.event_id)
            {
                delete_remote_event(&tx, state)?;
                summary.deleted += 1;
            } else if !seen_hrefs.contains(&state.remote_href)
                && (pending_protected_hrefs.contains(&state.remote_href)
                    || pending_protected_event_ids.contains(&state.event_id))
            {
                summary.skipped += 1;
            }
        }

        if clear_sync_token {
            let updated = tx
                .execute(
                    "UPDATE sync_metadata
                     SET sync_token = NULL
                     WHERE calendar_id = ?1 AND remote_url IS NOT NULL",
                    params![calendar_id],
                )
                .map_err(|_| RepositoryError)?;
            if updated == 0 {
                return Err(RepositoryError);
            }
        }

        tx.commit().map_err(|_| RepositoryError)?;
        Ok(summary)
    }

    pub fn reconcile_remote_changes(
        &mut self,
        calendar_id: Uuid,
        changes: &super::caldav::SyncCollection,
    ) -> Result<RemoteSnapshotSummary, RepositoryError> {
        let calendar = self.get_calendar(calendar_id).ok_or(RepositoryError)?;
        if !matches!(calendar.source, CalendarSource::CalDav { .. }) {
            return Err(RepositoryError);
        }
        if self.get_calendar_sync_state(calendar_id).is_none()
            || changes.sync_token.trim().is_empty()
        {
            return Err(RepositoryError);
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let pending_operations = pending_sync_operations_in_transaction(&tx, calendar_id)?;
        let pending_protected_hrefs: HashSet<String> = pending_operations
            .iter()
            .filter_map(|operation| match operation {
                PendingSyncOperation::Update { remote_href, .. }
                | PendingSyncOperation::Delete { remote_href, .. } => Some(remote_href.clone()),
                PendingSyncOperation::Create { .. } => None,
            })
            .collect();
        let tracked_states = {
            let mut statement = tx
                .prepare(
                    "SELECT calendar_id, event_id, remote_href, remote_uid, etag
                     FROM event_sync_metadata
                     WHERE calendar_id = ?1",
                )
                .map_err(|_| RepositoryError)?;
            statement
                .query_map(params![calendar_id], event_sync_state_from_row)
                .map_err(|_| RepositoryError)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| RepositoryError)?
        };
        let tracked_by_href: HashMap<String, EventSyncState> = tracked_states
            .iter()
            .cloned()
            .map(|state| (state.remote_href.clone(), state))
            .collect();

        let mut summary = RemoteSnapshotSummary {
            added: 0,
            updated: 0,
            deleted: 0,
            skipped: 0,
        };

        for resource in &changes.changes {
            if pending_protected_hrefs.contains(&resource.href) {
                summary.skipped += 1;
                continue;
            }
            let tracked = tracked_by_href.get(&resource.href);

            if resource.response_status == Some(404) {
                if let Some(state) = tracked {
                    delete_remote_event(&tx, state)?;
                    summary.deleted += 1;
                } else {
                    summary.skipped += 1;
                }
                continue;
            }

            if resource
                .response_status
                .is_some_and(|status| !(200..300).contains(&status))
            {
                summary.skipped += 1;
                continue;
            }

            let Some(calendar_data) = resource.calendar_data.as_deref() else {
                summary.skipped += 1;
                continue;
            };

            let event_id = tracked.map_or_else(Uuid::new_v4, |state| state.event_id);
            let mapped =
                match super::caldav::map_icalendar_resource(calendar_data, event_id, calendar_id) {
                    Ok(mapped) => mapped,
                    Err(_) => {
                        summary.skipped += 1;
                        continue;
                    }
                };
            if !detached_events_are_valid(&mapped.exceptions) {
                summary.skipped += 1;
                continue;
            }
            let state = EventSyncState {
                calendar_id,
                event_id,
                remote_href: resource.href.clone(),
                remote_uid: mapped.master.remote_uid.clone(),
                etag: resource.etag.clone(),
            };

            if tracked.is_some() {
                replace_remote_event(&tx, &mapped.master.event)?;
                upsert_event_sync_state_in_transaction(&tx, &state)?;
                replace_detached_events_in_transaction(&tx, event_id, &mapped.exceptions)?;
                summary.updated += 1;
            } else {
                insert_event(&tx, &mapped.master.event)?;
                upsert_event_sync_state_in_transaction(&tx, &state)?;
                replace_detached_events_in_transaction(&tx, event_id, &mapped.exceptions)?;
                summary.added += 1;
            }
        }

        let updated = tx
            .execute(
                "UPDATE sync_metadata
                 SET sync_token = ?1
                 WHERE calendar_id = ?2 AND remote_url IS NOT NULL",
                params![changes.sync_token, calendar_id],
            )
            .map_err(|_| RepositoryError)?;
        if updated == 0 {
            return Err(RepositoryError);
        }

        tx.commit().map_err(|_| RepositoryError)?;
        Ok(summary)
    }

    /// Seed the database's default calendars exactly once.
    pub fn seed_default_calendars(
        &mut self,
        defaults: &[Calendar],
    ) -> Result<bool, RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let initialized: Option<i64> = tx
            .query_row(
                "SELECT id FROM calendar_seed_state WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| RepositoryError)?;
        if initialized.is_some() {
            tx.commit().map_err(|_| RepositoryError)?;
            return Ok(false);
        }

        for calendar in defaults {
            let (source, account_id) = calendar_source_values(&calendar.source);
            tx.execute(
                "INSERT INTO calendars (id, name, color, visible, read_only, source, account_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO NOTHING",
                params![
                    calendar.id,
                    calendar.name,
                    calendar.color,
                    calendar.visible as i32,
                    calendar.read_only as i32,
                    source,
                    account_id,
                ],
            )
            .map_err(|_| RepositoryError)?;
        }

        tx.execute("INSERT INTO calendar_seed_state (id) VALUES (1)", [])
            .map_err(|_| RepositoryError)?;
        tx.commit().map_err(|_| RepositoryError)?;
        Ok(true)
    }

    pub fn create_event_with_sync(&mut self, event: &Event) -> Result<(), RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let calendar = calendar_in_transaction(&tx, event.calendar_id)?;
        if calendar.read_only {
            return Err(RepositoryError);
        }

        insert_event(&tx, event)?;
        if matches!(calendar.source, CalendarSource::CalDav { .. }) {
            upsert_pending_sync_operation_in_transaction(
                &tx,
                &PendingSyncOperation::Create {
                    calendar_id: event.calendar_id,
                    event_id: event.id,
                    remote_uid: event.id.to_string(),
                },
            )?;
        }
        tx.commit().map_err(|_| RepositoryError)
    }

    /// Recover upload intents for CalDAV events persisted before sync intent
    /// creation was available. Discovery and insertion share one transaction so
    /// a worker cannot observe a partially repaired calendar.
    pub(crate) fn queue_orphan_event_creates(
        &mut self,
        calendar_id: Uuid,
        account_id: Uuid,
    ) -> Result<(), RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let event_ids = {
            let mut statement = tx
                .prepare(
                    "SELECT e.id
                     FROM events AS e
                     JOIN calendars AS c ON c.id = e.calendar_id
                     WHERE e.calendar_id = ?1
                       AND c.source = 'CalDav'
                       AND c.account_id = ?2
                       AND NOT EXISTS (
                           SELECT 1 FROM event_sync_metadata AS m
                           WHERE m.event_id = e.id
                       )
                       AND NOT EXISTS (
                           SELECT 1 FROM pending_sync_operations AS p
                           WHERE p.event_id = e.id
                       )
                     ORDER BY e.id",
                )
                .map_err(|_| RepositoryError)?;
            statement
                .query_map(params![calendar_id, account_id], |row| row.get(0))
                .map_err(|_| RepositoryError)?
                .collect::<Result<Vec<Uuid>, _>>()
                .map_err(|_| RepositoryError)?
        };

        for event_id in event_ids {
            upsert_pending_sync_operation_in_transaction(
                &tx,
                &PendingSyncOperation::Create {
                    calendar_id,
                    event_id,
                    remote_uid: event_id.to_string(),
                },
            )?;
        }

        tx.commit().map_err(|_| RepositoryError)
    }

    pub fn update_event_with_sync(&mut self, event: &Event) -> Result<(), RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let existing_calendar_id: Option<Uuid> = tx
            .query_row(
                "SELECT calendar_id FROM events WHERE id = ?1",
                params![event.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| RepositoryError)?;
        let Some(existing_calendar_id) = existing_calendar_id else {
            return Err(RepositoryError);
        };
        let existing_calendar = calendar_in_transaction(&tx, existing_calendar_id)?;
        let calendar = calendar_in_transaction(&tx, event.calendar_id)?;
        if existing_calendar.read_only || calendar.read_only {
            return Err(RepositoryError);
        }

        let pending = pending_sync_operation_in_transaction(&tx, event.id)?;
        let sync_state = event_sync_state_in_transaction(&tx, event.id)?;
        update_event_in_transaction(&tx, event)?;

        if matches!(calendar.source, CalendarSource::Local) {
            delete_pending_sync_operation_in_transaction(&tx, event.id)?;
        } else {
            let operation = match pending {
                Some(PendingSyncOperation::Create { .. })
                | Some(PendingSyncOperation::Update { .. }) => pending,
                Some(PendingSyncOperation::Delete { .. }) => return Err(RepositoryError),
                None => sync_state.map(|state| PendingSyncOperation::Update {
                    calendar_id: event.calendar_id,
                    event_id: event.id,
                    remote_href: state.remote_href,
                    remote_uid: state.remote_uid,
                    base_etag: state.etag,
                }),
            };
            let operation = operation.unwrap_or(PendingSyncOperation::Create {
                calendar_id: event.calendar_id,
                event_id: event.id,
                remote_uid: event.id.to_string(),
            });
            upsert_pending_sync_operation_in_transaction(&tx, &operation)?;
        }

        tx.commit().map_err(|_| RepositoryError)
    }

    pub fn delete_event_with_sync_undo(
        &mut self,
        id: Uuid,
    ) -> Result<EventDeletionUndo, RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let event = tx
            .query_row(
                "SELECT id, calendar_id, title, location, description, \
                 schedule_type, start_date, end_date_exclusive, \
                 start_datetime, end_datetime, timezone, \
                 start_unix, recurrence_enabled, recurrence_data FROM events WHERE id = ?1",
                params![id],
                event_from_row,
            )
            .optional()
            .map_err(|_| RepositoryError)?
            .ok_or(RepositoryError)?;
        let calendar = calendar_in_transaction(&tx, event.calendar_id)?;
        if calendar.read_only {
            return Err(RepositoryError);
        }

        let prior_pending_operation = pending_sync_operation_in_transaction(&tx, id)?;
        let event_sync_state = event_sync_state_in_transaction(&tx, id)?;
        let mut delete_operation = None;
        let mut delete_tombstone = false;

        if matches!(calendar.source, CalendarSource::CalDav { .. }) {
            match &prior_pending_operation {
                Some(PendingSyncOperation::Create { .. }) => {}
                Some(PendingSyncOperation::Update {
                    calendar_id,
                    event_id,
                    remote_href,
                    remote_uid,
                    base_etag,
                }) => {
                    delete_operation = Some(PendingSyncOperation::Delete {
                        calendar_id: *calendar_id,
                        event_id: *event_id,
                        remote_href: remote_href.clone(),
                        remote_uid: remote_uid.clone(),
                        base_etag: base_etag.clone(),
                    });
                    delete_tombstone = true;
                }
                Some(PendingSyncOperation::Delete { .. }) => return Err(RepositoryError),
                None => {
                    if let Some(state) = &event_sync_state {
                        delete_operation = Some(PendingSyncOperation::Delete {
                            calendar_id: state.calendar_id,
                            event_id: state.event_id,
                            remote_href: state.remote_href.clone(),
                            remote_uid: state.remote_uid.clone(),
                            base_etag: state.etag.clone(),
                        });
                        delete_tombstone = true;
                    }
                }
            }
        }

        tx.execute("DELETE FROM events WHERE id = ?1", params![id])
            .map_err(|_| RepositoryError)?;
        delete_pending_sync_operation_in_transaction(&tx, id)?;
        if let Some(operation) = &delete_operation {
            upsert_pending_sync_operation_in_transaction(&tx, operation)?;
        }
        tx.commit().map_err(|_| RepositoryError)?;

        Ok(EventDeletionUndo {
            event,
            restored: false,
            event_sync_state,
            prior_pending_operation,
            sync_undo: true,
            delete_tombstone,
        })
    }

    pub fn undo_event_with_sync(
        &mut self,
        undo: &mut EventDeletionUndo,
    ) -> Result<(), RepositoryError> {
        if undo.restored {
            return Err(RepositoryError);
        }

        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let calendar = calendar_in_transaction(&tx, undo.event.calendar_id)?;
        if calendar.read_only {
            return Err(RepositoryError);
        }
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
                params![undo.event.id],
                |row| row.get(0),
            )
            .map_err(|_| RepositoryError)?;
        if exists {
            return Err(RepositoryError);
        }

        if undo.sync_undo && undo.delete_tombstone {
            let current_pending = pending_sync_operation_in_transaction(&tx, undo.event.id)?;
            if !matches!(current_pending, Some(PendingSyncOperation::Delete { .. })) {
                return Err(RepositoryError);
            }
        }

        insert_event(&tx, &undo.event)?;
        delete_pending_sync_operation_in_transaction(&tx, undo.event.id)?;
        if let Some(state) = &undo.event_sync_state {
            upsert_event_sync_state_in_transaction(&tx, state)?;
        }
        if let Some(operation) = &undo.prior_pending_operation {
            upsert_pending_sync_operation_in_transaction(&tx, operation)?;
        }
        tx.commit().map_err(|_| RepositoryError)?;
        undo.restored = true;
        Ok(())
    }
}

fn calendar_source_values(source: &CalendarSource) -> (&'static str, Option<Uuid>) {
    match source {
        CalendarSource::Local => ("Local", None),
        CalendarSource::CalDav { account_id } => ("CalDav", Some(*account_id)),
    }
}

fn validate_discovered_href(href: &str) -> Result<(), RepositoryError> {
    let url = Url::parse(href).map_err(|_| RepositoryError)?;
    if url.host_str().is_none() || !matches!(url.scheme(), "http" | "https") {
        return Err(RepositoryError);
    }
    Ok(())
}

fn discovered_calendar_name(discovered: &DiscoveredCalendar) -> String {
    discovered
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("CalDAV Calendar")
        .to_owned()
}

fn normalize_discovered_color(color: Option<&str>) -> String {
    let Some(color) = color.map(str::trim) else {
        return "#3584e4".to_owned();
    };
    let value = color.strip_prefix('#').unwrap_or(color);
    if (value.len() == 6 || value.len() == 8) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return format!("#{}", &value[..6]).to_ascii_lowercase();
    }
    "#3584e4".to_owned()
}

// ---------------------------------------------------------------------------
// Calendar rows
// ---------------------------------------------------------------------------

fn calendar_from_row(row: &rusqlite::Row) -> rusqlite::Result<Calendar> {
    let id: Uuid = row.get(0)?;
    let name: String = row.get(1)?;
    let color: String = row.get(2)?;
    let visible: bool = row.get::<_, i32>(3)? != 0;
    let read_only: bool = row.get::<_, i32>(4)? != 0;
    let source_str: String = row.get(5)?;
    let account_id: Option<Uuid> = row.get(6)?;
    let source = match source_str.as_str() {
        "Local" => CalendarSource::Local,
        "CalDav" => account_id
            .map(|account_id| CalendarSource::CalDav { account_id })
            .unwrap_or(CalendarSource::Local),
        _ => CalendarSource::Local,
    };
    Ok(Calendar {
        id,
        name,
        color,
        visible,
        read_only,
        source,
    })
}

fn account_from_row(row: &rusqlite::Row) -> rusqlite::Result<Account> {
    Ok(Account {
        id: row.get(0)?,
        name: row.get(1)?,
        server_url: row.get(2)?,
        username: row.get(3)?,
        enabled: row.get::<_, i32>(4)? != 0,
    })
}

// ---------------------------------------------------------------------------
// AccountRepository
// ---------------------------------------------------------------------------

impl AccountRepository for SqliteRepository {
    fn save_account(&mut self, account: &Account) -> Result<(), RepositoryError> {
        self.conn
            .execute(
                "INSERT INTO accounts (id, name, server_url, username, enabled)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    server_url = excluded.server_url,
                    username = excluded.username,
                    enabled = excluded.enabled",
                params![
                    account.id,
                    account.name,
                    account.server_url,
                    account.username,
                    account.enabled as i32,
                ],
            )
            .map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn update_account(&mut self, account: &Account) -> Result<(), RepositoryError> {
        let affected = self
            .conn
            .execute(
                "UPDATE accounts SET
                     name = ?1,
                     server_url = ?2,
                     username = ?3,
                     enabled = ?4
                 WHERE id = ?5",
                params![
                    account.name,
                    account.server_url,
                    account.username,
                    account.enabled as i32,
                    account.id,
                ],
            )
            .map_err(|_| RepositoryError)?;
        if affected == 0 {
            return Err(RepositoryError);
        }
        Ok(())
    }

    fn list_accounts(&self) -> Vec<Account> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, name, server_url, username, enabled
             FROM accounts ORDER BY id",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], account_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|r| r.ok()))
            .collect()
    }

    fn get_account(&self, id: Uuid) -> Option<Account> {
        self.conn
            .query_row(
                "SELECT id, name, server_url, username, enabled
                 FROM accounts WHERE id = ?1",
                params![id],
                account_from_row,
            )
            .ok()
    }

    fn delete_account(&mut self, id: Uuid) -> bool {
        self.conn
            .execute("DELETE FROM accounts WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Event rows
//
// Timed events store datetimes as both TEXT (ISO 8601 / RFC 3339, via
// rusqlite's chrono feature) for exact round-tripping and INTEGER (Unix
// seconds) for offset-independent range comparisons.  All-day events
// store dates as TEXT (YYYY-MM-DD).
// ---------------------------------------------------------------------------

fn event_from_row(row: &rusqlite::Row) -> rusqlite::Result<Event> {
    let id: Uuid = row.get(0)?;
    let calendar_id: Uuid = row.get(1)?;
    let title: String = row.get(2)?;
    let location: String = row.get(3)?;
    let description: String = row.get(4)?;
    let schedule_type: String = row.get(5)?;

    let schedule = match schedule_type.as_str() {
        "all_day" => {
            let start_date: NaiveDate = row.get(6)?;
            let end_date_exclusive: NaiveDate = row.get(7)?;
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            }
        }
        "timed" => {
            let start: DateTime<FixedOffset> = row.get(8)?;
            let end: DateTime<FixedOffset> = row.get(9)?;
            let timezone: Option<String> = row.get(10)?;
            EventSchedule::Timed {
                start,
                end,
                timezone,
            }
        }
        _ => {
            return Err(rusqlite::Error::InvalidColumnName(format!(
                "unknown schedule_type: {schedule_type}"
            )));
        }
    };

    let recurrence: Option<RecurrenceSpec> = {
        let enabled: i32 = row.get(12)?;
        let data: Option<String> = row.get(13)?;
        if let Some(data) = data {
            Some(recurrence_from_storage(&data))
        } else if enabled != 0 {
            // The legacy schema did not retain the rule text. Preserve its
            // enabled state rather than silently changing the event.
            Some(RecurrenceSpec::default())
        } else {
            None
        }
    };

    Ok(Event {
        id,
        calendar_id,
        title,
        location,
        description,
        schedule,
        recurrence,
        reminders: Vec::new(),
    })
}

// ---------------------------------------------------------------------------
// CalendarRepository
// ---------------------------------------------------------------------------

impl CalendarRepository for SqliteRepository {
    fn save_calendar(&mut self, calendar: &Calendar) -> Result<(), RepositoryError> {
        let (source, account_id) = calendar_source_values(&calendar.source);
        self.conn
            .execute(
                "INSERT INTO calendars (id, name, color, visible, read_only, source, account_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    color = excluded.color,
                    visible = excluded.visible,
                    read_only = excluded.read_only,
                    source = excluded.source,
                    account_id = excluded.account_id",
                params![
                    calendar.id,
                    calendar.name,
                    calendar.color,
                    calendar.visible as i32,
                    calendar.read_only as i32,
                    source,
                    account_id,
                ],
            )
            .map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn update_calendar(&mut self, calendar: &Calendar) -> Result<(), RepositoryError> {
        let (source, account_id) = calendar_source_values(&calendar.source);
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;
        let affected = tx
            .execute(
                "UPDATE calendars SET
                     name = ?1,
                     color = ?2,
                     visible = ?3,
                     read_only = ?4,
                     source = ?5,
                     account_id = ?6
                 WHERE id = ?7",
                params![
                    calendar.name,
                    calendar.color,
                    calendar.visible as i32,
                    calendar.read_only as i32,
                    source,
                    account_id,
                    calendar.id,
                ],
            )
            .map_err(|_| RepositoryError)?;
        if affected == 0 {
            return Err(RepositoryError);
        }
        tx.commit().map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn list_calendars(&self) -> Vec<Calendar> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, name, color, visible, read_only, source, account_id
                 FROM calendars ORDER BY id",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map([], calendar_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|r| r.ok()))
            .collect()
    }

    fn get_calendar(&self, id: Uuid) -> Option<Calendar> {
        self.conn
            .query_row(
                "SELECT id, name, color, visible, read_only, source, account_id \
                 FROM calendars WHERE id = ?1",
                params![id],
                calendar_from_row,
            )
            .ok()
    }

    fn delete_calendar(&mut self, id: Uuid) -> bool {
        let tx = match self.conn.transaction() {
            Ok(tx) => tx,
            Err(_) => return false,
        };
        if tx
            .execute("DELETE FROM events WHERE calendar_id = ?1", params![id])
            .is_err()
        {
            return false;
        }
        let deleted = match tx.execute("DELETE FROM calendars WHERE id = ?1", params![id]) {
            Ok(count) if count > 0 => true,
            Ok(_) | Err(_) => return false,
        };
        if tx.commit().is_err() {
            return false;
        }
        deleted
    }
}

// ---------------------------------------------------------------------------
// EventRepository
// ---------------------------------------------------------------------------

impl EventRepository for SqliteRepository {
    fn save_event(&mut self, event: &Event) -> Result<(), RepositoryError> {
        // Reject duplicate UUID.
        let exists: bool = self
            .conn
            .query_row(
                "SELECT 1 FROM events WHERE id = ?1",
                params![event.id],
                |_| Ok(()),
            )
            .is_ok();
        if exists {
            return Err(RepositoryError);
        }

        insert_event(&self.conn, event)
    }

    fn update_event(&mut self, event: &Event) -> Result<(), RepositoryError> {
        let tx = self.conn.transaction().map_err(|_| RepositoryError)?;

        let existing_calendar_id: Option<Uuid> = tx
            .query_row(
                "SELECT calendar_id FROM events WHERE id = ?1",
                params![event.id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| RepositoryError)?;
        let Some(existing_calendar_id) = existing_calendar_id else {
            return Err(RepositoryError);
        };

        let has_sync_metadata: bool = tx
            .query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM event_sync_metadata WHERE event_id = ?1
                 )",
                params![event.id],
                |row| row.get::<_, i32>(0).map(|value| value != 0),
            )
            .map_err(|_| RepositoryError)?;
        if has_sync_metadata && existing_calendar_id != event.calendar_id {
            return Err(RepositoryError);
        }

        let recurrence_enabled = if event.recurrence.is_some() { 1 } else { 0 };
        let recurrence_data = recurrence_to_storage(event.recurrence.as_ref());
        let affected = match &event.schedule {
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            } => tx.execute(
                "UPDATE events SET
                         calendar_id = ?1,
                         title = ?2,
                         location = ?3,
                         description = ?4,
                         schedule_type = 'all_day',
                         start_date = ?5,
                         end_date_exclusive = ?6,
                         start_datetime = NULL,
                         end_datetime = NULL,
                         timezone = NULL,
                         start_unix = 0,
                         end_unix = 0,
                          recurrence_enabled = ?7,
                          recurrence_data = ?8
                      WHERE id = ?9",
                params![
                    event.calendar_id,
                    event.title,
                    event.location,
                    event.description,
                    start_date,
                    end_date_exclusive,
                    recurrence_enabled,
                    recurrence_data,
                    event.id,
                ],
            ),
            EventSchedule::Timed {
                start,
                end,
                timezone,
            } => tx.execute(
                "UPDATE events SET
                         calendar_id = ?1,
                         title = ?2,
                         location = ?3,
                         description = ?4,
                         schedule_type = 'timed',
                         start_date = NULL,
                         end_date_exclusive = NULL,
                         start_datetime = ?5,
                         end_datetime = ?6,
                         timezone = ?7,
                         start_unix = ?8,
                         end_unix = ?9,
                          recurrence_enabled = ?10,
                          recurrence_data = ?11
                      WHERE id = ?12",
                params![
                    event.calendar_id,
                    event.title,
                    event.location,
                    event.description,
                    start,
                    end,
                    timezone,
                    start.timestamp(),
                    end.timestamp(),
                    recurrence_enabled,
                    recurrence_data,
                    event.id,
                ],
            ),
        }
        .map_err(|_| RepositoryError)?;
        if affected == 0 {
            return Err(RepositoryError);
        }

        replace_reminders(&tx, event)?;
        tx.commit().map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn get_event(&self, id: Uuid) -> Option<Event> {
        let mut event = self
            .conn
            .query_row(
                "SELECT id, calendar_id, title, location, description, \
                 schedule_type, start_date, end_date_exclusive, \
                 start_datetime, end_datetime, timezone, \
                  start_unix, recurrence_enabled, recurrence_data \
                 FROM events WHERE id = ?1",
                params![id],
                event_from_row,
            )
            .ok()?;
        event.reminders = reminders_for_event(&self.conn, id).ok()?;
        Some(event)
    }

    fn delete_event(&mut self, id: Uuid) -> bool {
        self.conn
            .execute("DELETE FROM events WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
    }

    fn delete_event_with_undo(&mut self, id: Uuid) -> Option<EventDeletionUndo> {
        let event = self.get_event(id)?;
        if self.delete_event(id) {
            Some(EventDeletionUndo {
                event,
                restored: false,
                event_sync_state: None,
                prior_pending_operation: None,
                sync_undo: false,
                delete_tombstone: false,
            })
        } else {
            None
        }
    }

    fn undo_delete_event(&mut self, undo: &mut EventDeletionUndo) -> Result<(), RepositoryError> {
        if undo.restored || self.get_event(undo.event.id).is_some() {
            return Err(RepositoryError);
        }
        insert_event(&self.conn, &undo.event)?;
        undo.restored = true;
        Ok(())
    }

    fn list_events_for_calendar(&self, calendar_id: Uuid) -> Vec<Event> {
        let mut stmt = match self.conn.prepare(
            "SELECT id, calendar_id, title, location, description, \
             schedule_type, start_date, end_date_exclusive, \
             start_datetime, end_datetime, timezone, \
              start_unix, recurrence_enabled, recurrence_data \
             FROM events WHERE calendar_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut events: Vec<Event> = stmt
            .query_map(params![calendar_id], event_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|r| r.ok()))
            .collect();
        for event in &mut events {
            event.reminders = reminders_for_event(&self.conn, event.id).unwrap_or_default();
        }
        events
    }

    fn timed_events_in_range(&self, range: &DateTimeRange) -> Vec<Event> {
        // Compare absolute instants via Unix timestamps so that events
        // with differing FixedOffsets are correctly included/excluded
        // regardless of their RFC 3339 textual representation.
        let range_start_unix = range.start.timestamp();
        let range_end_unix = range.end.timestamp();
        let mut stmt = match self.conn.prepare(
            "SELECT id, calendar_id, title, location, description, \
             schedule_type, start_date, end_date_exclusive, \
             start_datetime, end_datetime, timezone, \
             start_unix, recurrence_enabled, recurrence_data \
             FROM events \
             WHERE schedule_type = 'timed' \
               AND start_unix < ?2 \
               AND end_unix > ?1 \
             ORDER BY start_unix ASC, id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut events: Vec<Event> = stmt
            .query_map(params![range_start_unix, range_end_unix], event_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|r| r.ok()))
            .collect();
        for event in &mut events {
            event.reminders = reminders_for_event(&self.conn, event.id).unwrap_or_default();
        }
        events
    }
}

// ---------------------------------------------------------------------------
// SyncStateRepository
// ---------------------------------------------------------------------------

fn calendar_sync_state_from_row(row: &rusqlite::Row) -> rusqlite::Result<CalendarSyncState> {
    Ok(CalendarSyncState {
        calendar_id: row.get(0)?,
        remote_url: row.get(1)?,
        sync_token: row.get(2)?,
    })
}

fn upsert_calendar_sync_state_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    state: &CalendarSyncState,
) -> Result<(), RepositoryError> {
    let updated = tx
        .execute(
            "UPDATE sync_metadata
             SET remote_url = ?1, sync_token = ?2
             WHERE id = (
                 SELECT id FROM sync_metadata
                 WHERE calendar_id = ?3
                 ORDER BY id LIMIT 1
             )",
            params![
                state.remote_url,
                state.sync_token.as_deref(),
                state.calendar_id
            ],
        )
        .map_err(|_| RepositoryError)?;
    if updated == 0 {
        tx.execute(
            "INSERT INTO sync_metadata (id, calendar_id, remote_url, sync_token)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                state.calendar_id,
                state.calendar_id,
                state.remote_url,
                state.sync_token.as_deref(),
            ],
        )
        .map_err(|_| RepositoryError)?;
    }
    Ok(())
}

fn event_sync_state_from_row(row: &rusqlite::Row) -> rusqlite::Result<EventSyncState> {
    Ok(EventSyncState {
        calendar_id: row.get(0)?,
        event_id: row.get(1)?,
        remote_href: row.get(2)?,
        remote_uid: row.get(3)?,
        etag: row.get(4)?,
    })
}

impl SyncStateRepository for SqliteRepository {
    fn upsert_calendar_sync_state(
        &mut self,
        state: &CalendarSyncState,
    ) -> Result<(), RepositoryError> {
        let updated = self
            .conn
            .execute(
                "UPDATE sync_metadata
                 SET remote_url = ?1, sync_token = ?2
                 WHERE id = (
                     SELECT id FROM sync_metadata
                     WHERE calendar_id = ?3
                     ORDER BY id LIMIT 1
                 )",
                params![
                    state.remote_url,
                    state.sync_token.as_deref(),
                    state.calendar_id
                ],
            )
            .map_err(|_| RepositoryError)?;
        if updated == 0 {
            self.conn
                .execute(
                    "INSERT INTO sync_metadata
                         (id, calendar_id, remote_url, sync_token)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        state.calendar_id,
                        state.calendar_id,
                        state.remote_url,
                        state.sync_token.as_deref(),
                    ],
                )
                .map_err(|_| RepositoryError)?;
        }
        Ok(())
    }

    fn get_calendar_sync_state(&self, calendar_id: Uuid) -> Option<CalendarSyncState> {
        self.conn
            .query_row(
                "SELECT calendar_id, remote_url, sync_token
                 FROM sync_metadata
                 WHERE calendar_id = ?1 AND remote_url IS NOT NULL",
                params![calendar_id],
                calendar_sync_state_from_row,
            )
            .ok()
    }

    fn upsert_event_sync_state(&mut self, state: &EventSyncState) -> Result<(), RepositoryError> {
        self.conn
            .execute(
                "INSERT INTO event_sync_metadata
                     (id, calendar_id, event_id, remote_href, remote_uid, etag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(event_id) DO UPDATE SET
                     calendar_id = excluded.calendar_id,
                     remote_href = excluded.remote_href,
                     remote_uid = excluded.remote_uid,
                     etag = excluded.etag",
                params![
                    state.event_id,
                    state.calendar_id,
                    state.event_id,
                    state.remote_href,
                    state.remote_uid,
                    state.etag.as_deref(),
                ],
            )
            .map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn get_event_sync_state(&self, event_id: Uuid) -> Option<EventSyncState> {
        self.conn
            .query_row(
                "SELECT calendar_id, event_id, remote_href, remote_uid, etag
                 FROM event_sync_metadata WHERE event_id = ?1",
                params![event_id],
                event_sync_state_from_row,
            )
            .ok()
    }

    fn find_event_sync_state_by_remote_href(
        &self,
        calendar_id: Uuid,
        remote_href: &str,
    ) -> Option<EventSyncState> {
        self.conn
            .query_row(
                "SELECT calendar_id, event_id, remote_href, remote_uid, etag
                 FROM event_sync_metadata
                 WHERE calendar_id = ?1 AND remote_href = ?2",
                params![calendar_id, remote_href],
                event_sync_state_from_row,
            )
            .ok()
    }

    fn list_event_sync_states(&self, calendar_id: Uuid) -> Vec<EventSyncState> {
        let mut stmt = match self.conn.prepare(
            "SELECT calendar_id, event_id, remote_href, remote_uid, etag
             FROM event_sync_metadata
             WHERE calendar_id = ?1
             ORDER BY remote_href ASC, event_id ASC",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![calendar_id], event_sync_state_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|row| row.ok()))
            .collect()
    }
}

// ---------------------------------------------------------------------------
// PendingSyncOperationRepository
// ---------------------------------------------------------------------------

fn pending_sync_operation_from_row(row: &rusqlite::Row) -> rusqlite::Result<PendingSyncOperation> {
    let event_id: Uuid = row.get(0)?;
    let calendar_id: Uuid = row.get(1)?;
    let operation_kind: String = row.get(2)?;
    let remote_href: Option<String> = row.get(3)?;
    let remote_uid: String = row.get(4)?;
    let base_etag: Option<String> = row.get(5)?;

    match operation_kind.as_str() {
        "create" => Ok(PendingSyncOperation::Create {
            calendar_id,
            event_id,
            remote_uid,
        }),
        "update" => Ok(PendingSyncOperation::Update {
            calendar_id,
            event_id,
            remote_href: remote_href.ok_or_else(|| {
                rusqlite::Error::InvalidColumnName("missing remote_href".to_string())
            })?,
            remote_uid,
            base_etag,
        }),
        "delete" => Ok(PendingSyncOperation::Delete {
            calendar_id,
            event_id,
            remote_href: remote_href.ok_or_else(|| {
                rusqlite::Error::InvalidColumnName("missing remote_href".to_string())
            })?,
            remote_uid,
            base_etag,
        }),
        _ => Err(rusqlite::Error::InvalidColumnName(
            "unknown operation_kind".to_string(),
        )),
    }
}

impl PendingSyncOperationRepository for SqliteRepository {
    fn upsert_pending_sync_operation(
        &mut self,
        operation: &PendingSyncOperation,
    ) -> Result<(), RepositoryError> {
        let (calendar_id, event_id, operation_kind, remote_href, remote_uid, base_etag) =
            match operation {
                PendingSyncOperation::Create {
                    calendar_id,
                    event_id,
                    remote_uid,
                } => (*calendar_id, *event_id, "create", None, remote_uid, None),
                PendingSyncOperation::Update {
                    calendar_id,
                    event_id,
                    remote_href,
                    remote_uid,
                    base_etag,
                } => (
                    *calendar_id,
                    *event_id,
                    "update",
                    Some(remote_href),
                    remote_uid,
                    base_etag.as_ref(),
                ),
                PendingSyncOperation::Delete {
                    calendar_id,
                    event_id,
                    remote_href,
                    remote_uid,
                    base_etag,
                } => (
                    *calendar_id,
                    *event_id,
                    "delete",
                    Some(remote_href),
                    remote_uid,
                    base_etag.as_ref(),
                ),
            };

        self.conn
            .execute(
                "INSERT INTO pending_sync_operations
                     (event_id, calendar_id, operation_kind, remote_href, remote_uid, base_etag)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(event_id) DO UPDATE SET
                     calendar_id = excluded.calendar_id,
                     operation_kind = excluded.operation_kind,
                     remote_href = excluded.remote_href,
                     remote_uid = excluded.remote_uid,
                     base_etag = excluded.base_etag",
                params![
                    event_id,
                    calendar_id,
                    operation_kind,
                    remote_href,
                    remote_uid,
                    base_etag,
                ],
            )
            .map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn get_pending_sync_operation(&self, event_id: Uuid) -> Option<PendingSyncOperation> {
        self.conn
            .query_row(
                "SELECT event_id, calendar_id, operation_kind, remote_href, remote_uid, base_etag
                 FROM pending_sync_operations
                 WHERE event_id = ?1",
                params![event_id],
                pending_sync_operation_from_row,
            )
            .ok()
    }

    fn list_pending_sync_operations(&self, calendar_id: Uuid) -> Vec<PendingSyncOperation> {
        let mut stmt = match self.conn.prepare(
            "SELECT event_id, calendar_id, operation_kind, remote_href, remote_uid, base_etag
             FROM pending_sync_operations
             WHERE calendar_id = ?1
             ORDER BY event_id ASC",
        ) {
            Ok(stmt) => stmt,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![calendar_id], pending_sync_operation_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|row| row.ok()))
            .collect()
    }

    fn remove_pending_sync_operation(&mut self, event_id: Uuid) -> bool {
        self.conn
            .execute(
                "DELETE FROM pending_sync_operations WHERE event_id = ?1",
                params![event_id],
            )
            .map(|count| count > 0)
            .unwrap_or(false)
    }
}

// ---------------------------------------------------------------------------
// Internal helper – single-row insert of an event
// ---------------------------------------------------------------------------

fn recurrence_to_storage(recurrence: Option<&RecurrenceSpec>) -> Option<String> {
    recurrence.map(|recurrence| {
        recurrence
            .rrule
            .iter()
            .chain(&recurrence.rdate)
            .chain(&recurrence.exdate)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    })
}

fn recurrence_from_storage(data: &str) -> RecurrenceSpec {
    let mut recurrence = RecurrenceSpec::default();
    for line in data.lines().filter(|line| !line.trim().is_empty()) {
        match line
            .split_once(':')
            .and_then(|(key, _)| key.split(';').next())
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("RRULE") => recurrence.rrule.push(line.to_owned()),
            Some("RDATE") => recurrence.rdate.push(line.to_owned()),
            Some("EXDATE") => recurrence.exdate.push(line.to_owned()),
            _ => {}
        }
    }
    recurrence
}

fn detached_event_from_row(row: &rusqlite::Row) -> rusqlite::Result<(Uuid, DetachedEvent)> {
    let id: Uuid = row.get(0)?;
    let recurrence_kind: String = row.get(1)?;
    let recurrence_date: Option<NaiveDate> = row.get(2)?;
    let recurrence_datetime: Option<DateTime<FixedOffset>> = row.get(3)?;
    let recurrence_timezone: Option<String> = row.get(4)?;
    let cancelled: bool = row.get::<_, i32>(5)? != 0;
    let recurrence_id = match recurrence_kind.as_str() {
        "all_day" => RecurrenceId::AllDay(recurrence_date.ok_or_else(|| {
            rusqlite::Error::InvalidColumnName("missing all-day recurrence date".to_owned())
        })?),
        "timed" => RecurrenceId::Timed {
            date_time: recurrence_datetime.ok_or_else(|| {
                rusqlite::Error::InvalidColumnName("missing timed recurrence date".to_owned())
            })?,
            timezone: recurrence_timezone,
        },
        _ => {
            return Err(rusqlite::Error::InvalidColumnName(
                "unknown detached recurrence kind".to_owned(),
            ));
        }
    };

    if cancelled {
        return Ok((id, DetachedEvent::Cancelled { recurrence_id }));
    }

    let title: String = row.get(6)?;
    let location: String = row.get(7)?;
    let description: String = row.get(8)?;
    let schedule_type: String = row.get::<_, Option<String>>(9)?.ok_or_else(|| {
        rusqlite::Error::InvalidColumnName("missing detached schedule type".to_owned())
    })?;
    let schedule = match schedule_type.as_str() {
        "all_day" => EventSchedule::AllDay {
            start_date: row.get::<_, Option<NaiveDate>>(10)?.ok_or_else(|| {
                rusqlite::Error::InvalidColumnName("missing detached start date".to_owned())
            })?,
            end_date_exclusive: row.get::<_, Option<NaiveDate>>(11)?.ok_or_else(|| {
                rusqlite::Error::InvalidColumnName("missing detached end date".to_owned())
            })?,
        },
        "timed" => EventSchedule::Timed {
            start: row
                .get::<_, Option<DateTime<FixedOffset>>>(12)?
                .ok_or_else(|| {
                    rusqlite::Error::InvalidColumnName("missing detached start datetime".to_owned())
                })?,
            end: row
                .get::<_, Option<DateTime<FixedOffset>>>(13)?
                .ok_or_else(|| {
                    rusqlite::Error::InvalidColumnName("missing detached end datetime".to_owned())
                })?,
            timezone: row.get(14)?,
        },
        _ => {
            return Err(rusqlite::Error::InvalidColumnName(
                "unknown detached schedule type".to_owned(),
            ));
        }
    };

    Ok((
        id,
        DetachedEvent::Modified {
            recurrence_id,
            title,
            location,
            description,
            schedule,
            reminders: Vec::new(),
        },
    ))
}

fn detached_recurrence_sort(recurrence_id: &RecurrenceId) -> String {
    match recurrence_id {
        RecurrenceId::AllDay(date) => format!("{}T00:00:00+00:00", date.format("%Y-%m-%d")),
        RecurrenceId::Timed { date_time, .. } => date_time.with_timezone(&Utc).to_rfc3339(),
    }
}

fn detached_recurrence_identity(detached: &DetachedEvent) -> (String, String, Option<String>) {
    let recurrence_id = match detached {
        DetachedEvent::Modified { recurrence_id, .. }
        | DetachedEvent::Cancelled { recurrence_id } => recurrence_id,
    };
    match recurrence_id {
        RecurrenceId::AllDay(date) => ("all_day".to_owned(), date.to_string(), None),
        RecurrenceId::Timed {
            date_time,
            timezone,
        } => ("timed".to_owned(), date_time.to_rfc3339(), timezone.clone()),
    }
}

fn detached_events_are_valid(exceptions: &[DetachedEvent]) -> bool {
    let mut recurrence_ids = HashSet::new();
    exceptions
        .iter()
        .all(|exception| recurrence_ids.insert(detached_recurrence_identity(exception)))
}

fn replace_detached_events_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    master_event_id: Uuid,
    exceptions: &[DetachedEvent],
) -> Result<(), RepositoryError> {
    tx.execute(
        "DELETE FROM detached_events WHERE master_event_id = ?1",
        params![master_event_id],
    )
    .map_err(|_| RepositoryError)?;
    for exception in exceptions {
        insert_detached_event(tx, master_event_id, exception)?;
    }
    Ok(())
}

fn insert_detached_event(
    tx: &rusqlite::Transaction<'_>,
    master_event_id: Uuid,
    detached: &DetachedEvent,
) -> Result<(), RepositoryError> {
    insert_detached_event_with_id(tx, master_event_id, detached, Uuid::new_v4())
}

fn insert_detached_event_with_id(
    tx: &rusqlite::Transaction<'_>,
    master_event_id: Uuid,
    detached: &DetachedEvent,
    id: Uuid,
) -> Result<(), RepositoryError> {
    let (recurrence_id, cancelled, title, location, description, schedule, reminders) =
        match detached {
            DetachedEvent::Modified {
                recurrence_id,
                title,
                location,
                description,
                schedule,
                reminders,
            } => (
                recurrence_id,
                false,
                title.as_str(),
                location.as_str(),
                description.as_str(),
                Some(schedule),
                reminders.as_slice(),
            ),
            DetachedEvent::Cancelled { recurrence_id } => {
                (recurrence_id, true, "", "", "", None, &[][..])
            }
        };

    let (recurrence_kind, recurrence_date, recurrence_datetime, recurrence_timezone) =
        match recurrence_id {
            RecurrenceId::AllDay(date) => ("all_day", Some(*date), None, None::<String>),
            RecurrenceId::Timed {
                date_time,
                timezone,
            } => ("timed", None, Some(*date_time), timezone.clone()),
        };

    let mut schedule_type = None;
    let mut start_date = None;
    let mut end_date_exclusive = None;
    let mut start_datetime = None;
    let mut end_datetime = None;
    let mut schedule_timezone = None;
    if let Some(schedule) = schedule {
        match schedule {
            EventSchedule::AllDay {
                start_date: start,
                end_date_exclusive: end,
            } => {
                schedule_type = Some("all_day");
                start_date = Some(*start);
                end_date_exclusive = Some(*end);
            }
            EventSchedule::Timed {
                start,
                end,
                timezone,
            } => {
                schedule_type = Some("timed");
                start_datetime = Some(*start);
                end_datetime = Some(*end);
                schedule_timezone = timezone.clone();
            }
        }
    }

    tx.execute(
        "INSERT INTO detached_events
             (id, master_event_id, recurrence_kind, recurrence_date,
              recurrence_datetime, recurrence_timezone, recurrence_sort, cancelled,
              title, location, description, schedule_type, start_date,
              end_date_exclusive, start_datetime, end_datetime, timezone)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        params![
            id,
            master_event_id,
            recurrence_kind,
            recurrence_date,
            recurrence_datetime,
            recurrence_timezone,
            detached_recurrence_sort(recurrence_id),
            cancelled as i32,
            title,
            location,
            description,
            schedule_type,
            start_date,
            end_date_exclusive,
            start_datetime,
            end_datetime,
            schedule_timezone,
        ],
    )
    .map_err(|_| RepositoryError)?;

    for reminder in reminders {
        tx.execute(
            "INSERT INTO detached_event_reminders
                 (id, detached_event_id, seconds_before_start, description)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                Uuid::new_v4(),
                id,
                reminder.seconds_before_start,
                reminder.description,
            ],
        )
        .map_err(|_| RepositoryError)?;
    }
    Ok(())
}

fn reminders_for_detached_event(
    conn: &Connection,
    detached_event_id: Uuid,
) -> rusqlite::Result<Vec<ReminderSpec>> {
    let mut statement = conn.prepare(
        "SELECT seconds_before_start, description
         FROM detached_event_reminders
         WHERE detached_event_id = ?1
         ORDER BY rowid",
    )?;
    statement
        .query_map(params![detached_event_id], |row| {
            Ok(ReminderSpec {
                seconds_before_start: row.get(0)?,
                description: row.get(1)?,
            })
        })?
        .collect()
}

fn event_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    event_id: Uuid,
) -> Result<Event, RepositoryError> {
    tx.query_row(
        "SELECT id, calendar_id, title, location, description,
                schedule_type, start_date, end_date_exclusive,
                start_datetime, end_datetime, timezone,
                start_unix, recurrence_enabled, recurrence_data
         FROM events WHERE id = ?1",
        params![event_id],
        event_from_row,
    )
    .optional()
    .map_err(|_| RepositoryError)?
    .ok_or(RepositoryError)
}

fn event_with_reminders_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    event_id: Uuid,
) -> Result<Event, RepositoryError> {
    let mut event = event_in_transaction(tx, event_id)?;
    event.reminders = reminders_for_event(tx, event_id).map_err(|_| RepositoryError)?;
    Ok(event)
}

fn detached_events_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    master_event_id: Uuid,
) -> Result<Vec<(Uuid, DetachedEvent)>, RepositoryError> {
    let mut statement = tx
        .prepare(
            "SELECT id, recurrence_kind, recurrence_date, recurrence_datetime,
                    recurrence_timezone, cancelled, title, location, description,
                    schedule_type, start_date, end_date_exclusive,
                    start_datetime, end_datetime, timezone
             FROM detached_events
             WHERE master_event_id = ?1
             ORDER BY recurrence_sort ASC, recurrence_kind ASC, id ASC",
        )
        .map_err(|_| RepositoryError)?;
    let rows = statement
        .query_map(params![master_event_id], detached_event_from_row)
        .map_err(|_| RepositoryError)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RepositoryError)?;
    drop(statement);
    rows.into_iter()
        .map(|(id, mut event)| {
            if let DetachedEvent::Modified { reminders, .. } = &mut event {
                *reminders = reminders_for_detached_event(tx, id).map_err(|_| RepositoryError)?;
            }
            Ok((id, event))
        })
        .collect()
}

fn new_future_master_id(
    tx: &rusqlite::Transaction<'_>,
    sync_state: Option<&EventSyncState>,
) -> Result<Uuid, RepositoryError> {
    for _ in 0..8 {
        let id = Uuid::new_v4();
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE id = ?1)",
                params![id],
                |row| row.get(0),
            )
            .map_err(|_| RepositoryError)?;
        if !exists
            && distinct_remote_uid(id, sync_state)
                != sync_state.map(|s| s.remote_uid.clone()).unwrap_or_default()
        {
            return Ok(id);
        }
    }
    Err(RepositoryError)
}

fn distinct_remote_uid(id: Uuid, sync_state: Option<&EventSyncState>) -> String {
    let uid = id.to_string();
    if sync_state.is_some_and(|state| state.remote_uid == uid) {
        format!("{uid}-following")
    } else {
        uid
    }
}

fn edited_schedule_is_supported(
    master: &Event,
    recurrence_id: &RecurrenceId,
    schedule: &EventSchedule,
    allow_kind_change: bool,
) -> bool {
    if allow_kind_change
        && let (
            EventSchedule::AllDay { .. },
            RecurrenceId::AllDay(_),
            EventSchedule::Timed {
                start,
                end,
                timezone,
            },
        ) = (&master.schedule, recurrence_id, schedule)
    {
        return end > start
            && timezone
                .as_deref()
                .map(|timezone| chrono_tz::Tz::from_str(timezone).is_ok())
                .unwrap_or(true);
    }

    match (&master.schedule, recurrence_id, schedule) {
        (
            EventSchedule::AllDay { .. },
            RecurrenceId::AllDay(date),
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            },
        ) => start_date == date && end_date_exclusive > start_date,
        (
            EventSchedule::Timed { timezone, .. },
            RecurrenceId::Timed {
                date_time,
                timezone: identity_timezone,
            },
            EventSchedule::Timed {
                start,
                end,
                timezone: edited_timezone,
            },
        ) => {
            timezone == edited_timezone
                && end > start
                && match timezone.as_deref() {
                    Some(timezone) => chrono_tz::Tz::from_str(timezone)
                        .map(|timezone| {
                            start.with_timezone(&timezone).date_naive()
                                == date_time.with_timezone(&timezone).date_naive()
                        })
                        .unwrap_or(false),
                    None => {
                        identity_timezone.is_none() && start.date_naive() == date_time.date_naive()
                    }
                }
        }
        _ => false,
    }
}

fn future_recurrence_is_supported(schedule: &EventSchedule, recurrence: &RecurrenceSpec) -> bool {
    matches!(
        recurrence_presentation(Some(recurrence), schedule),
        RecurrencePresentation::Editable { .. }
    )
}

fn rebase_detached_event(detached: &DetachedEvent, recurrence_id: RecurrenceId) -> DetachedEvent {
    match detached {
        DetachedEvent::Modified {
            title,
            location,
            description,
            schedule,
            reminders,
            ..
        } => DetachedEvent::Modified {
            recurrence_id,
            title: title.clone(),
            location: location.clone(),
            description: description.clone(),
            schedule: schedule.clone(),
            reminders: reminders.clone(),
        },
        DetachedEvent::Cancelled { .. } => DetachedEvent::Cancelled { recurrence_id },
    }
}

fn rebase_following_recurrence_id(
    master: &Event,
    future: &Event,
    recurrence_id: &RecurrenceId,
) -> Result<RecurrenceId, RepositoryError> {
    let date = detached_recurrence_date(master, recurrence_id).ok_or(RepositoryError)?;
    let rebased = match &future.schedule {
        EventSchedule::AllDay { .. } => RecurrenceId::AllDay(date),
        EventSchedule::Timed { timezone, .. } => {
            future_timed_recurrence_id_for_date(future, date, timezone.as_deref())?
        }
    };
    if occurrence_is_generated(future, &rebased) {
        Ok(rebased)
    } else {
        Err(RepositoryError)
    }
}

fn detached_recurrence_date(master: &Event, recurrence_id: &RecurrenceId) -> Option<NaiveDate> {
    match (&master.schedule, recurrence_id) {
        (EventSchedule::AllDay { .. }, RecurrenceId::AllDay(date)) => Some(*date),
        (
            EventSchedule::Timed { timezone, .. },
            RecurrenceId::Timed {
                date_time,
                timezone: identity_timezone,
            },
        ) if timezone == identity_timezone => match timezone.as_deref() {
            Some(timezone) => chrono_tz::Tz::from_str(timezone)
                .ok()
                .map(|timezone| date_time.with_timezone(&timezone).date_naive()),
            None => Some(date_time.date_naive()),
        },
        _ => None,
    }
}

fn future_timed_recurrence_id_for_date(
    future: &Event,
    date: NaiveDate,
    timezone: Option<&str>,
) -> Result<RecurrenceId, RepositoryError> {
    let recurrence = future.recurrence.as_ref().ok_or(RepositoryError)?;
    let source = recurrence_source_for_timed(
        match &future.schedule {
            EventSchedule::Timed { start, .. } => *start,
            _ => return Err(RepositoryError),
        },
        timezone,
        recurrence,
    )
    .ok_or(RepositoryError)?;
    let set = source.parse::<RRuleSet>().map_err(|_| RepositoryError)?;
    let (lower, upper, rule_timezone) = match timezone {
        Some(timezone) => {
            let timezone = chrono_tz::Tz::from_str(timezone).map_err(|_| RepositoryError)?;
            let lower = timezone
                .from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or(RepositoryError)?)
                .single()
                .ok_or(RepositoryError)?;
            let next_date = date.succ_opt().ok_or(RepositoryError)?;
            let upper = timezone
                .from_local_datetime(&next_date.and_hms_opt(0, 0, 0).ok_or(RepositoryError)?)
                .single()
                .ok_or(RepositoryError)?;
            let rule_timezone = RRuleTz::from(timezone);
            (
                lower.with_timezone(&rule_timezone),
                upper.with_timezone(&rule_timezone),
                rule_timezone,
            )
        }
        None => {
            let utc = FixedOffset::east_opt(0).ok_or(RepositoryError)?;
            let lower = utc
                .from_local_datetime(&date.and_hms_opt(0, 0, 0).ok_or(RepositoryError)?)
                .single()
                .ok_or(RepositoryError)?
                .with_timezone(&RRuleTz::UTC);
            let next_date = date.succ_opt().ok_or(RepositoryError)?;
            let upper = utc
                .from_local_datetime(&next_date.and_hms_opt(0, 0, 0).ok_or(RepositoryError)?)
                .single()
                .ok_or(RepositoryError)?
                .with_timezone(&RRuleTz::UTC);
            (lower, upper, RRuleTz::UTC)
        }
    };
    let mut occurrences = set
        .after(lower - Duration::seconds(1))
        .before(upper + Duration::seconds(1))
        .all(u16::MAX)
        .dates;
    if let EventSchedule::Timed { start, .. } = &future.schedule {
        let first = start.with_timezone(&rule_timezone);
        if first >= lower - Duration::seconds(1) && first <= upper + Duration::seconds(1) {
            occurrences.push(first);
        }
    }
    occurrences.sort();
    occurrences.dedup();
    let occurrence = occurrences
        .into_iter()
        .find(|occurrence| occurrence.with_timezone(&rule_timezone).date_naive() == date)
        .ok_or(RepositoryError)?;
    Ok(RecurrenceId::Timed {
        date_time: occurrence.fixed_offset(),
        timezone: timezone.map(str::to_owned),
    })
}

fn truncate_recurrence_for_delete(
    master: &Event,
    recurrence_id: &RecurrenceId,
) -> Result<RecurrenceSpec, RepositoryError> {
    match split_recurrence_at(master, recurrence_id) {
        Ok(split) => Ok(split.original_recurrence),
        Err(_) => {
            let Some(recurrence) = master.recurrence.as_ref() else {
                return Err(RepositoryError);
            };
            if !matches!(
                recurrence_presentation(Some(recurrence), &master.schedule),
                RecurrencePresentation::Editable { .. }
            ) {
                return Err(RepositoryError);
            }
            let Some(total) = recurrence_rule_count(recurrence.rrule.first().map(String::as_str))
            else {
                return Err(RepositoryError);
            };
            let Some(index) = generated_occurrence_index(master, recurrence_id) else {
                return Err(RepositoryError);
            };
            if index == 0 || index.saturating_add(1) != total as usize {
                return Err(RepositoryError);
            }
            Ok(RecurrenceSpec {
                rrule: vec![rule_with_count(&recurrence.rrule[0], index as u32)],
                rdate: Vec::new(),
                exdate: Vec::new(),
            })
        }
    }
}

fn recurrence_rule_count(rule: Option<&str>) -> Option<u32> {
    rule?.split_once(':')?.1.split(';').find_map(|item| {
        let (key, value) = item.split_once('=')?;
        key.eq_ignore_ascii_case("COUNT")
            .then(|| value.parse().ok())?
    })
}

fn rule_with_count(rule: &str, count: u32) -> String {
    let (property, value) = rule.split_once(':').expect("validated recurrence rule");
    let mut found = false;
    let mut items = Vec::new();
    for item in value.split(';') {
        if let Some((key, _)) = item.split_once('=') {
            if key.eq_ignore_ascii_case("COUNT") {
                items.push(format!("COUNT={count}"));
                found = true;
                continue;
            }
            if key.eq_ignore_ascii_case("UNTIL") {
                continue;
            }
        }
        items.push(item.to_owned());
    }
    if !found {
        items.push(format!("COUNT={count}"));
    }
    format!("{property}:{}", items.join(";"))
}

fn generated_occurrence_index(master: &Event, recurrence_id: &RecurrenceId) -> Option<usize> {
    let recurrence = master.recurrence.as_ref()?;
    let rule = recurrence.rrule.first()?;
    let source = match &master.schedule {
        EventSchedule::AllDay { start_date, .. } => {
            recurrence_source_for_all_day(*start_date, recurrence)
        }
        EventSchedule::Timed {
            start, timezone, ..
        } => recurrence_source_for_timed(*start, timezone.as_deref(), recurrence)?,
    };
    if recurrence.rrule.len() != 1 || rule.is_empty() {
        return None;
    }
    let set = source.parse::<RRuleSet>().ok()?;
    let first = match &master.schedule {
        EventSchedule::AllDay { start_date, .. } => {
            utc_midnight_for_occurrence(*start_date).fixed_offset()
        }
        EventSchedule::Timed { start, .. } => start.with_timezone(&Utc).fixed_offset(),
    };
    let target = match (recurrence_id, &master.schedule) {
        (RecurrenceId::AllDay(date), EventSchedule::AllDay { .. }) => {
            utc_midnight_for_occurrence(*date).fixed_offset()
        }
        (RecurrenceId::Timed { date_time, .. }, EventSchedule::Timed { .. }) => {
            date_time.with_timezone(&Utc).fixed_offset()
        }
        _ => return None,
    };
    if target <= first {
        return Some(0);
    }
    let lower = first.checked_sub_signed(Duration::seconds(1))?;
    let upper = target.checked_add_signed(Duration::seconds(1))?;
    let timezone = match &master.schedule {
        EventSchedule::AllDay { .. } => RRuleTz::UTC,
        EventSchedule::Timed {
            timezone: Some(timezone),
            ..
        } => RRuleTz::from(chrono_tz::Tz::from_str(timezone).ok()?),
        EventSchedule::Timed { timezone: None, .. } => RRuleTz::UTC,
    };
    let mut occurrences = vec![first];
    occurrences.extend(
        set.after(lower.with_timezone(&timezone))
            .before(upper.with_timezone(&timezone))
            .all(u16::MAX)
            .dates
            .into_iter()
            .map(|date| date.fixed_offset()),
    );
    occurrences.sort();
    occurrences.dedup();
    occurrences
        .iter()
        .position(|occurrence| match (recurrence_id, &master.schedule) {
            (RecurrenceId::AllDay(date), EventSchedule::AllDay { .. }) => {
                occurrence.date_naive() == *date
            }
            (
                RecurrenceId::Timed {
                    date_time,
                    timezone: identity_timezone,
                },
                EventSchedule::Timed {
                    timezone: schedule_timezone,
                    ..
                },
            ) => {
                if identity_timezone != schedule_timezone {
                    return false;
                }
                match schedule_timezone.as_deref() {
                    Some(timezone) => chrono_tz::Tz::from_str(timezone)
                        .map(|timezone| {
                            occurrence.with_timezone(&timezone).naive_local()
                                == date_time.with_timezone(&timezone).naive_local()
                        })
                        .unwrap_or(false),
                    None => *occurrence == *date_time,
                }
            }
            _ => false,
        })
}

fn detached_event_for_recurrence_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    master_event_id: Uuid,
    recurrence_id: &RecurrenceId,
) -> Result<Option<(Uuid, DetachedEvent)>, RepositoryError> {
    let mut statement = tx
        .prepare(
            "SELECT id, recurrence_kind, recurrence_date, recurrence_datetime,
                    recurrence_timezone, cancelled, title, location, description,
                    schedule_type, start_date, end_date_exclusive,
                    start_datetime, end_datetime, timezone
             FROM detached_events
             WHERE master_event_id = ?1
             ORDER BY recurrence_sort ASC, recurrence_kind ASC, id ASC",
        )
        .map_err(|_| RepositoryError)?;
    let rows = statement
        .query_map(params![master_event_id], detached_event_from_row)
        .map_err(|_| RepositoryError)?;
    for row in rows {
        let (id, mut event) = row.map_err(|_| RepositoryError)?;
        if !detached_recurrence_ids_match(detached_recurrence_id(&event), recurrence_id) {
            continue;
        }
        if let DetachedEvent::Modified { reminders, .. } = &mut event {
            *reminders = reminders_for_detached_event(tx, id).map_err(|_| RepositoryError)?;
        }
        return Ok(Some((id, event)));
    }
    Ok(None)
}

fn occurrence_pending_operation(
    calendar: &Calendar,
    master_event_id: Uuid,
    prior: Option<PendingSyncOperation>,
    sync_state: Option<EventSyncState>,
) -> Result<Option<PendingSyncOperation>, RepositoryError> {
    if matches!(calendar.source, CalendarSource::Local) {
        return Ok(None);
    }

    if matches!(prior, Some(PendingSyncOperation::Delete { .. })) {
        return Err(RepositoryError);
    }
    if let Some(state) = sync_state {
        if state.calendar_id != calendar.id || state.event_id != master_event_id {
            return Err(RepositoryError);
        }
        return Ok(Some(PendingSyncOperation::Update {
            calendar_id: state.calendar_id,
            event_id: state.event_id,
            remote_href: state.remote_href,
            remote_uid: state.remote_uid,
            base_etag: state.etag,
        }));
    }

    match prior {
        Some(operation @ PendingSyncOperation::Create { .. }) => Ok(Some(operation)),
        Some(PendingSyncOperation::Update { .. }) | None => Err(RepositoryError),
        Some(PendingSyncOperation::Delete { .. }) => Err(RepositoryError),
    }
}

fn detached_recurrence_id(detached: &DetachedEvent) -> &RecurrenceId {
    match detached {
        DetachedEvent::Modified { recurrence_id, .. }
        | DetachedEvent::Cancelled { recurrence_id } => recurrence_id,
    }
}

fn detached_recurrence_ids_match(left: &RecurrenceId, right: &RecurrenceId) -> bool {
    match (left, right) {
        (RecurrenceId::AllDay(left), RecurrenceId::AllDay(right)) => left == right,
        (
            RecurrenceId::Timed {
                date_time: left_time,
                timezone: left_timezone,
            },
            RecurrenceId::Timed {
                date_time: right_time,
                timezone: right_timezone,
            },
        ) if left_timezone == right_timezone => match left_timezone.as_deref() {
            Some(timezone) => chrono_tz::Tz::from_str(timezone)
                .map(|timezone| {
                    left_time.with_timezone(&timezone).naive_local()
                        == right_time.with_timezone(&timezone).naive_local()
                })
                .unwrap_or(false),
            None => left_time == right_time,
        },
        _ => false,
    }
}

fn occurrence_is_generated(master: &Event, recurrence_id: &RecurrenceId) -> bool {
    let Some(recurrence) = master.recurrence.as_ref() else {
        return false;
    };
    match (&master.schedule, recurrence_id) {
        (EventSchedule::AllDay { start_date, .. }, RecurrenceId::AllDay(target_date)) => {
            let Some(offset) = FixedOffset::east_opt(0) else {
                return false;
            };
            let Some(target) = offset
                .from_local_datetime(
                    &target_date
                        .and_hms_opt(0, 0, 0)
                        .expect("valid recurrence date has a midnight"),
                )
                .single()
            else {
                return false;
            };
            let Some(lower) = target.checked_sub_signed(Duration::days(2)) else {
                return false;
            };
            let Some(upper) = target.checked_add_signed(Duration::days(2)) else {
                return false;
            };
            let source = recurrence_source_for_all_day(*start_date, recurrence);
            let Ok(set) = source.parse::<RRuleSet>() else {
                return false;
            };
            let start_is_excluded = set
                .get_exdate()
                .iter()
                .any(|date| date.date_naive() == *start_date);
            let lower = lower.with_timezone(&RRuleTz::UTC);
            let upper = upper.with_timezone(&RRuleTz::UTC);
            let mut dates = set.after(lower).before(upper).all(u16::MAX).dates;
            let recurrence_start = utc_midnight_for_occurrence(*start_date);
            if !start_is_excluded && recurrence_start > lower && recurrence_start < upper {
                dates.push(recurrence_start);
            }
            dates
                .into_iter()
                .any(|date| date.date_naive() == *target_date)
        }
        (
            EventSchedule::Timed {
                start, timezone, ..
            },
            RecurrenceId::Timed {
                date_time: target,
                timezone: target_timezone,
            },
        ) if timezone == target_timezone => {
            let Some(lower) = target.checked_sub_signed(Duration::days(2)) else {
                return false;
            };
            let Some(upper) = target.checked_add_signed(Duration::days(2)) else {
                return false;
            };
            let Some(source) = recurrence_source_for_timed(*start, timezone.as_deref(), recurrence)
            else {
                return false;
            };
            let Ok(set) = source.parse::<RRuleSet>() else {
                return false;
            };
            let recurrence_start = match timezone.as_deref() {
                Some(timezone) => {
                    let Ok(timezone) = chrono_tz::Tz::from_str(timezone) else {
                        return false;
                    };
                    start.with_timezone(&RRuleTz::from(timezone))
                }
                None => start.with_timezone(&RRuleTz::UTC),
            };
            let timezone = match timezone.as_deref() {
                Some(timezone) => {
                    let Ok(timezone) = chrono_tz::Tz::from_str(timezone) else {
                        return false;
                    };
                    RRuleTz::from(timezone)
                }
                None => RRuleTz::UTC,
            };
            let lower = lower.with_timezone(&timezone);
            let upper = upper.with_timezone(&timezone);
            let start_is_excluded = set
                .get_exdate()
                .iter()
                .any(|date| date.timestamp() == recurrence_start.timestamp());
            let mut dates = set.after(lower).before(upper).all(u16::MAX).dates;
            if !start_is_excluded && recurrence_start > lower && recurrence_start < upper {
                dates.push(recurrence_start);
            }
            dates.into_iter().any(|date| {
                let occurrence = date.fixed_offset();
                detached_recurrence_ids_match(
                    &RecurrenceId::Timed {
                        date_time: occurrence,
                        timezone: target_timezone.clone(),
                    },
                    recurrence_id,
                )
            })
        }
        _ => false,
    }
}

fn recurrence_source_for_all_day(start_date: NaiveDate, recurrence: &RecurrenceSpec) -> String {
    let mut source = format!("DTSTART;VALUE=DATE:{}", start_date.format("%Y%m%d"));
    for line in recurrence
        .rrule
        .iter()
        .chain(&recurrence.rdate)
        .chain(&recurrence.exdate)
    {
        source.push('\n');
        source.push_str(line);
    }
    source
}

fn recurrence_source_for_timed(
    start: DateTime<FixedOffset>,
    timezone: Option<&str>,
    recurrence: &RecurrenceSpec,
) -> Option<String> {
    let mut source = match timezone {
        None => format!(
            "DTSTART:{}",
            start.with_timezone(&Utc).format("%Y%m%dT%H%M%SZ")
        ),
        Some(timezone) => {
            let timezone = chrono_tz::Tz::from_str(timezone).ok()?;
            format!(
                "DTSTART;TZID={timezone}:{}",
                start.with_timezone(&timezone).format("%Y%m%dT%H%M%S")
            )
        }
    };
    for line in recurrence
        .rrule
        .iter()
        .chain(&recurrence.rdate)
        .chain(&recurrence.exdate)
    {
        source.push('\n');
        source.push_str(line);
    }
    Some(source)
}

fn utc_midnight_for_occurrence(date: NaiveDate) -> DateTime<RRuleTz> {
    let utc = FixedOffset::east_opt(0).expect("UTC offset is valid");
    utc.from_local_datetime(
        &date
            .and_hms_opt(0, 0, 0)
            .expect("valid recurrence date has a midnight"),
    )
    .single()
    .expect("a fixed offset has no ambiguous local times")
    .with_timezone(&RRuleTz::UTC)
}

fn calendar_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    id: Uuid,
) -> Result<Calendar, RepositoryError> {
    tx.query_row(
        "SELECT id, name, color, visible, read_only, source, account_id
         FROM calendars WHERE id = ?1",
        params![id],
        calendar_from_row,
    )
    .optional()
    .map_err(|_| RepositoryError)?
    .ok_or(RepositoryError)
}

fn event_sync_state_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    event_id: Uuid,
) -> Result<Option<EventSyncState>, RepositoryError> {
    tx.query_row(
        "SELECT calendar_id, event_id, remote_href, remote_uid, etag
         FROM event_sync_metadata WHERE event_id = ?1",
        params![event_id],
        event_sync_state_from_row,
    )
    .optional()
    .map_err(|_| RepositoryError)
}

fn pending_sync_operation_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    event_id: Uuid,
) -> Result<Option<PendingSyncOperation>, RepositoryError> {
    tx.query_row(
        "SELECT event_id, calendar_id, operation_kind, remote_href, remote_uid, base_etag
         FROM pending_sync_operations WHERE event_id = ?1",
        params![event_id],
        pending_sync_operation_from_row,
    )
    .optional()
    .map_err(|_| RepositoryError)
}

fn pending_sync_operations_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    calendar_id: Uuid,
) -> Result<Vec<PendingSyncOperation>, RepositoryError> {
    let mut statement = tx
        .prepare(
            "SELECT event_id, calendar_id, operation_kind, remote_href, remote_uid, base_etag
             FROM pending_sync_operations
             WHERE calendar_id = ?1",
        )
        .map_err(|_| RepositoryError)?;
    statement
        .query_map(params![calendar_id], pending_sync_operation_from_row)
        .map_err(|_| RepositoryError)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| RepositoryError)
}

fn delete_pending_sync_operation_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    event_id: Uuid,
) -> Result<(), RepositoryError> {
    tx.execute(
        "DELETE FROM pending_sync_operations WHERE event_id = ?1",
        params![event_id],
    )
    .map_err(|_| RepositoryError)?;
    Ok(())
}

fn upsert_pending_sync_operation_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    operation: &PendingSyncOperation,
) -> Result<(), RepositoryError> {
    let (calendar_id, event_id, operation_kind, remote_href, remote_uid, base_etag) =
        match operation {
            PendingSyncOperation::Create {
                calendar_id,
                event_id,
                remote_uid,
            } => (*calendar_id, *event_id, "create", None, remote_uid, None),
            PendingSyncOperation::Update {
                calendar_id,
                event_id,
                remote_href,
                remote_uid,
                base_etag,
            } => (
                *calendar_id,
                *event_id,
                "update",
                Some(remote_href),
                remote_uid,
                base_etag.as_ref(),
            ),
            PendingSyncOperation::Delete {
                calendar_id,
                event_id,
                remote_href,
                remote_uid,
                base_etag,
            } => (
                *calendar_id,
                *event_id,
                "delete",
                Some(remote_href),
                remote_uid,
                base_etag.as_ref(),
            ),
        };
    tx.execute(
        "INSERT INTO pending_sync_operations
             (event_id, calendar_id, operation_kind, remote_href, remote_uid, base_etag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(event_id) DO UPDATE SET
             calendar_id = excluded.calendar_id,
             operation_kind = excluded.operation_kind,
             remote_href = excluded.remote_href,
             remote_uid = excluded.remote_uid,
             base_etag = excluded.base_etag",
        params![
            event_id,
            calendar_id,
            operation_kind,
            remote_href,
            remote_uid,
            base_etag,
        ],
    )
    .map_err(|_| RepositoryError)?;
    Ok(())
}

fn update_event_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    event: &Event,
) -> Result<(), RepositoryError> {
    let existing_calendar_id: Option<Uuid> = tx
        .query_row(
            "SELECT calendar_id FROM events WHERE id = ?1",
            params![event.id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| RepositoryError)?;
    let Some(existing_calendar_id) = existing_calendar_id else {
        return Err(RepositoryError);
    };

    let has_sync_metadata: bool = tx
        .query_row(
            "SELECT EXISTS(
                 SELECT 1 FROM event_sync_metadata WHERE event_id = ?1
             )",
            params![event.id],
            |row| row.get::<_, i32>(0).map(|value| value != 0),
        )
        .map_err(|_| RepositoryError)?;
    if has_sync_metadata && existing_calendar_id != event.calendar_id {
        return Err(RepositoryError);
    }

    let recurrence_enabled = if event.recurrence.is_some() { 1 } else { 0 };
    let recurrence_data = recurrence_to_storage(event.recurrence.as_ref());
    let affected = match &event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => tx.execute(
            "UPDATE events SET
                     calendar_id = ?1,
                     title = ?2,
                     location = ?3,
                     description = ?4,
                     schedule_type = 'all_day',
                     start_date = ?5,
                     end_date_exclusive = ?6,
                     start_datetime = NULL,
                     end_datetime = NULL,
                     timezone = NULL,
                     start_unix = 0,
                     end_unix = 0,
                     recurrence_enabled = ?7,
                     recurrence_data = ?8
                 WHERE id = ?9",
            params![
                event.calendar_id,
                event.title,
                event.location,
                event.description,
                start_date,
                end_date_exclusive,
                recurrence_enabled,
                recurrence_data,
                event.id,
            ],
        ),
        EventSchedule::Timed {
            start,
            end,
            timezone,
        } => tx.execute(
            "UPDATE events SET
                     calendar_id = ?1,
                     title = ?2,
                     location = ?3,
                     description = ?4,
                     schedule_type = 'timed',
                     start_date = NULL,
                     end_date_exclusive = NULL,
                     start_datetime = ?5,
                     end_datetime = ?6,
                     timezone = ?7,
                     start_unix = ?8,
                     end_unix = ?9,
                     recurrence_enabled = ?10,
                     recurrence_data = ?11
                 WHERE id = ?12",
            params![
                event.calendar_id,
                event.title,
                event.location,
                event.description,
                start,
                end,
                timezone,
                start.timestamp(),
                end.timestamp(),
                recurrence_enabled,
                recurrence_data,
                event.id,
            ],
        ),
    }
    .map_err(|_| RepositoryError)?;
    if affected == 0 {
        return Err(RepositoryError);
    }
    replace_reminders(tx, event)?;
    Ok(())
}

fn insert_event(conn: &Connection, event: &Event) -> Result<(), RepositoryError> {
    let recurrence_enabled = if event.recurrence.is_some() { 1 } else { 0 };
    let recurrence_data = recurrence_to_storage(event.recurrence.as_ref());

    match &event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => {
            conn.execute(
                "INSERT INTO events (id, calendar_id, title, location, description, \
                 schedule_type, start_date, end_date_exclusive, \
                 start_datetime, end_datetime, timezone, \
                  start_unix, end_unix, recurrence_enabled, recurrence_data) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'all_day', ?6, ?7, \
                         NULL, NULL, NULL, 0, 0, ?8, ?9)",
                params![
                    event.id,
                    event.calendar_id,
                    event.title,
                    event.location,
                    event.description,
                    start_date,
                    end_date_exclusive,
                    recurrence_enabled,
                    recurrence_data,
                ],
            )
            .map_err(|_| RepositoryError)?;
        }
        EventSchedule::Timed {
            start,
            end,
            timezone,
        } => {
            conn.execute(
                "INSERT INTO events (id, calendar_id, title, location, description, \
                 schedule_type, start_date, end_date_exclusive, \
                 start_datetime, end_datetime, timezone, \
                  start_unix, end_unix, recurrence_enabled, recurrence_data) \
                  VALUES (?1, ?2, ?3, ?4, ?5, 'timed', NULL, NULL, \
                          ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    event.id,
                    event.calendar_id,
                    event.title,
                    event.location,
                    event.description,
                    start,
                    end,
                    timezone,
                    start.timestamp(),
                    end.timestamp(),
                    recurrence_enabled,
                    recurrence_data,
                ],
            )
            .map_err(|_| RepositoryError)?;
        }
    }
    replace_reminders(conn, event)?;
    Ok(())
}

fn replace_reminders(conn: &Connection, event: &Event) -> Result<(), RepositoryError> {
    conn.execute(
        "DELETE FROM reminders WHERE event_id = ?1",
        params![event.id],
    )
    .map_err(|_| RepositoryError)?;
    for reminder in &event.reminders {
        conn.execute(
            "INSERT INTO reminders
                 (id, event_id, seconds_before_start, description)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                Uuid::new_v4(),
                event.id,
                reminder.seconds_before_start,
                reminder.description,
            ],
        )
        .map_err(|_| RepositoryError)?;
    }
    Ok(())
}

fn reminders_for_event(conn: &Connection, event_id: Uuid) -> rusqlite::Result<Vec<ReminderSpec>> {
    let mut statement = conn.prepare(
        "SELECT seconds_before_start, description
         FROM reminders WHERE event_id = ?1 ORDER BY rowid",
    )?;
    statement
        .query_map(params![event_id], |row| {
            Ok(ReminderSpec {
                seconds_before_start: row.get(0)?,
                description: row.get(1)?,
            })
        })?
        .collect()
}

fn replace_remote_event(
    tx: &rusqlite::Transaction<'_>,
    event: &Event,
) -> Result<(), RepositoryError> {
    let deleted = tx
        .execute(
            "DELETE FROM events WHERE id = ?1 AND calendar_id = ?2",
            params![event.id, event.calendar_id],
        )
        .map_err(|_| RepositoryError)?;
    if deleted == 0 {
        return Err(RepositoryError);
    }
    insert_event(tx, event)
}

fn delete_remote_event(
    tx: &rusqlite::Transaction<'_>,
    state: &EventSyncState,
) -> Result<(), RepositoryError> {
    let metadata_deleted = tx
        .execute(
            "DELETE FROM event_sync_metadata
             WHERE calendar_id = ?1 AND event_id = ?2",
            params![state.calendar_id, state.event_id],
        )
        .map_err(|_| RepositoryError)?;
    if metadata_deleted == 0 {
        return Err(RepositoryError);
    }

    let event_deleted = tx
        .execute(
            "DELETE FROM events WHERE id = ?1 AND calendar_id = ?2",
            params![state.event_id, state.calendar_id],
        )
        .map_err(|_| RepositoryError)?;
    if event_deleted == 0 {
        return Err(RepositoryError);
    }
    Ok(())
}

fn upsert_event_sync_state_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    state: &EventSyncState,
) -> Result<(), RepositoryError> {
    tx.execute(
        "INSERT INTO event_sync_metadata
             (id, calendar_id, event_id, remote_href, remote_uid, etag)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(event_id) DO UPDATE SET
             calendar_id = excluded.calendar_id,
             remote_href = excluded.remote_href,
             remote_uid = excluded.remote_uid,
             etag = excluded.etag",
        params![
            state.event_id,
            state.calendar_id,
            state.event_id,
            state.remote_href,
            state.remote_uid,
            state.etag.as_deref(),
        ],
    )
    .map_err(|_| RepositoryError)?;
    Ok(())
}
