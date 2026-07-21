use std::path::Path;

use chrono::{DateTime, FixedOffset, NaiveDate};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::model::{
    Account, Calendar, CalendarSource, CalendarSyncState, DateTimeRange, Event, EventSchedule,
    EventSyncState, RecurrenceSpec,
};

use super::{
    AccountRepository, CalendarRepository, EventDeletionUndo, EventRepository, RepositoryError,
    SyncStateRepository,
};

pub struct SqliteRepository {
    conn: Connection,
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
                    FOREIGN KEY (calendar_id) REFERENCES calendars(id)
                        ON DELETE CASCADE
                );

                CREATE TABLE IF NOT EXISTS reminders (
                    id BLOB PRIMARY KEY,
                    event_id BLOB NOT NULL,
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

                CREATE TABLE IF NOT EXISTS calendar_seed_state (
                    id INTEGER PRIMARY KEY CHECK (id = 1)
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
}

fn calendar_source_values(source: &CalendarSource) -> (&'static str, Option<Uuid>) {
    match source {
        CalendarSource::Local => ("Local", None),
        CalendarSource::CalDav { account_id } => ("CalDav", Some(*account_id)),
    }
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
        if enabled != 0 {
            Some(RecurrenceSpec)
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

        let affected = tx
            .execute("DELETE FROM events WHERE id = ?1", params![event.id])
            .map_err(|_| RepositoryError)?;
        if affected == 0 {
            return Err(RepositoryError);
        }

        insert_event(&tx, event)?;

        tx.commit().map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn get_event(&self, id: Uuid) -> Option<Event> {
        self.conn
            .query_row(
                "SELECT id, calendar_id, title, location, description, \
                 schedule_type, start_date, end_date_exclusive, \
                 start_datetime, end_datetime, timezone, \
                 start_unix, recurrence_enabled \
                 FROM events WHERE id = ?1",
                params![id],
                event_from_row,
            )
            .ok()
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
             start_unix, recurrence_enabled \
             FROM events WHERE calendar_id = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![calendar_id], event_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|r| r.ok()))
            .collect()
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
             start_unix, recurrence_enabled \
             FROM events \
             WHERE schedule_type = 'timed' \
               AND start_unix < ?2 \
               AND end_unix > ?1 \
             ORDER BY start_unix ASC, id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        stmt.query_map(params![range_start_unix, range_end_unix], event_from_row)
            .into_iter()
            .flat_map(|rows| rows.filter_map(|r| r.ok()))
            .collect()
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
// Internal helper – single-row insert of an event
// ---------------------------------------------------------------------------

fn insert_event(conn: &Connection, event: &Event) -> Result<(), RepositoryError> {
    let recurrence_enabled = if event.recurrence.is_some() { 1 } else { 0 };

    match &event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => {
            conn.execute(
                "INSERT INTO events (id, calendar_id, title, location, description, \
                 schedule_type, start_date, end_date_exclusive, \
                 start_datetime, end_datetime, timezone, \
                 start_unix, end_unix, recurrence_enabled) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'all_day', ?6, ?7, \
                         NULL, NULL, NULL, 0, 0, ?8)",
                params![
                    event.id,
                    event.calendar_id,
                    event.title,
                    event.location,
                    event.description,
                    start_date,
                    end_date_exclusive,
                    recurrence_enabled,
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
                 start_unix, end_unix, recurrence_enabled) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 'timed', NULL, NULL, \
                         ?6, ?7, ?8, ?9, ?10, ?11)",
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
                ],
            )
            .map_err(|_| RepositoryError)?;
        }
    }
    Ok(())
}
