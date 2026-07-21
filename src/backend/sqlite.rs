use std::path::Path;

use chrono::{DateTime, FixedOffset, NaiveDate};
use rusqlite::{Connection, OptionalExtension, params};
use uuid::Uuid;

use crate::model::{Calendar, CalendarSource, DateTimeRange, Event, EventSchedule, RecurrenceSpec};

use super::{CalendarRepository, EventRepository, RepositoryError};

pub struct SqliteRepository {
    conn: Connection,
}

impl SqliteRepository {
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, RepositoryError> {
        let conn = Connection::open(path).map_err(|_| RepositoryError)?;
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
                "CREATE TABLE IF NOT EXISTS calendars (
                    id BLOB PRIMARY KEY,
                    name TEXT NOT NULL,
                    color TEXT NOT NULL,
                    visible INTEGER NOT NULL,
                    read_only INTEGER NOT NULL,
                    source TEXT NOT NULL
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
                    FOREIGN KEY (calendar_id) REFERENCES calendars(id)
                        ON DELETE CASCADE
                );

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
        Ok(())
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
            let source = match calendar.source {
                CalendarSource::Local => "Local",
            };
            tx.execute(
                "INSERT INTO calendars (id, name, color, visible, read_only, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO NOTHING",
                params![
                    calendar.id,
                    calendar.name,
                    calendar.color,
                    calendar.visible as i32,
                    calendar.read_only as i32,
                    source,
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
    let source = match source_str.as_str() {
        "Local" => CalendarSource::Local,
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
        let source = match calendar.source {
            CalendarSource::Local => "Local",
        };
        self.conn
            .execute(
                "INSERT INTO calendars (id, name, color, visible, read_only, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name,
                    color = excluded.color,
                    visible = excluded.visible,
                    read_only = excluded.read_only,
                    source = excluded.source",
                params![
                    calendar.id,
                    calendar.name,
                    calendar.color,
                    calendar.visible as i32,
                    calendar.read_only as i32,
                    source,
                ],
            )
            .map_err(|_| RepositoryError)?;
        Ok(())
    }

    fn list_calendars(&self) -> Vec<Calendar> {
        let mut stmt = match self
            .conn
            .prepare("SELECT id, name, color, visible, read_only, source FROM calendars")
        {
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
                "SELECT id, name, color, visible, read_only, source \
                 FROM calendars WHERE id = ?1",
                params![id],
                calendar_from_row,
            )
            .ok()
    }

    fn delete_calendar(&mut self, id: Uuid) -> bool {
        self.conn
            .execute("DELETE FROM calendars WHERE id = ?1", params![id])
            .map(|n| n > 0)
            .unwrap_or(false)
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
