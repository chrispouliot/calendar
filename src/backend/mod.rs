pub mod caldav;

mod sqlite;

pub use sqlite::SqliteRepository;

use std::collections::HashMap;
use uuid::Uuid;

use crate::model::{
    Account, Calendar, CalendarSyncState, DateTimeRange, Event, EventSchedule, EventSyncState,
    RangeOverlap,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepositoryError;

/// The event removed by `delete_event_with_undo`, together with its one-time
/// restoration state.
#[derive(Debug)]
pub struct EventDeletionUndo {
    pub event: Event,
    restored: bool,
}

/// Storage abstraction for calendars. Methods take `&mut self` so a single
/// in-memory repository can be updated in place.
pub trait CalendarRepository {
    /// Save a calendar. Saving a calendar whose UUID already exists replaces
    /// the prior value rather than duplicating it.
    fn save_calendar(&mut self, calendar: &Calendar) -> Result<(), RepositoryError>;

    /// Replace an existing calendar. Returns Err if the UUID does not already
    /// exist.
    fn update_calendar(&mut self, calendar: &Calendar) -> Result<(), RepositoryError>;

    fn list_calendars(&self) -> Vec<Calendar>;
    fn get_calendar(&self, id: Uuid) -> Option<Calendar>;

    /// Returns true iff a calendar with that ID was present.
    fn delete_calendar(&mut self, id: Uuid) -> bool;
}

/// Storage abstraction for non-secret CalDAV account configuration.
pub trait AccountRepository {
    /// Save an account, replacing the prior value when its UUID already exists.
    fn save_account(&mut self, account: &Account) -> Result<(), RepositoryError>;

    /// Replace an existing account. Returns Err if the UUID does not already
    /// exist.
    fn update_account(&mut self, account: &Account) -> Result<(), RepositoryError>;

    fn list_accounts(&self) -> Vec<Account>;
    fn get_account(&self, id: Uuid) -> Option<Account>;

    /// Returns true iff an account with that ID was present.
    fn delete_account(&mut self, id: Uuid) -> bool;
}

/// Storage abstraction for events.
pub trait EventRepository {
    /// Create a new event. Returns Err if the UUID already exists; use
    /// `update_event` to replace an existing event.
    fn save_event(&mut self, event: &Event) -> Result<(), RepositoryError>;

    /// Replace an existing event. Returns Err if the UUID does not already
    /// exist.
    fn update_event(&mut self, event: &Event) -> Result<(), RepositoryError>;

    fn get_event(&self, id: Uuid) -> Option<Event>;

    /// Returns true iff an event with that ID was present.
    fn delete_event(&mut self, id: Uuid) -> bool;

    /// Delete an event and return the complete event needed to undo the
    /// deletion, or `None` when no event with that ID exists.
    fn delete_event_with_undo(&mut self, id: Uuid) -> Option<EventDeletionUndo>;

    /// Restore a deletion exactly once. Existing events are never replaced.
    fn undo_delete_event(&mut self, undo: &mut EventDeletionUndo) -> Result<(), RepositoryError>;

    /// All events for the given calendar UUID. Order is not pinned.
    fn list_events_for_calendar(&self, calendar_id: Uuid) -> Vec<Event>;

    /// Timed events whose [start, end) interval genuinely overlaps `range`
    /// (start-inclusive, end-exclusive on both sides). Returned in
    /// deterministic chronological order — start time ascending. All-day
    /// events are excluded from this query.
    fn timed_events_in_range(&self, range: &DateTimeRange) -> Vec<Event>;
}

/// Storage abstraction for durable CalDAV calendar and event identities.
pub trait SyncStateRepository {
    fn upsert_calendar_sync_state(
        &mut self,
        state: &CalendarSyncState,
    ) -> Result<(), RepositoryError>;

    fn get_calendar_sync_state(&self, calendar_id: Uuid) -> Option<CalendarSyncState>;

    fn upsert_event_sync_state(&mut self, state: &EventSyncState) -> Result<(), RepositoryError>;

    fn get_event_sync_state(&self, event_id: Uuid) -> Option<EventSyncState>;

    fn find_event_sync_state_by_remote_href(
        &self,
        calendar_id: Uuid,
        remote_href: &str,
    ) -> Option<EventSyncState>;

    fn list_event_sync_states(&self, calendar_id: Uuid) -> Vec<EventSyncState>;
}

/// Combined in-memory implementation usable by tests and by application
/// startup. Constructed with `InMemoryRepository::new()`.
#[derive(Default)]
pub struct InMemoryRepository {
    accounts: HashMap<Uuid, Account>,
    calendars: HashMap<Uuid, Calendar>,
    events: HashMap<Uuid, Event>,
}

impl AccountRepository for InMemoryRepository {
    fn save_account(&mut self, account: &Account) -> Result<(), RepositoryError> {
        self.accounts.insert(account.id, account.clone());
        Ok(())
    }

    fn update_account(&mut self, account: &Account) -> Result<(), RepositoryError> {
        if !self.accounts.contains_key(&account.id) {
            return Err(RepositoryError);
        }
        self.accounts.insert(account.id, account.clone());
        Ok(())
    }

    fn list_accounts(&self) -> Vec<Account> {
        let mut accounts: Vec<Account> = self.accounts.values().cloned().collect();
        accounts.sort_by_key(|account| account.id);
        accounts
    }

    fn get_account(&self, id: Uuid) -> Option<Account> {
        self.accounts.get(&id).cloned()
    }

    fn delete_account(&mut self, id: Uuid) -> bool {
        self.accounts.remove(&id).is_some()
    }
}

impl InMemoryRepository {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CalendarRepository for InMemoryRepository {
    fn save_calendar(&mut self, calendar: &Calendar) -> Result<(), RepositoryError> {
        self.calendars.insert(calendar.id, calendar.clone());
        Ok(())
    }

    fn update_calendar(&mut self, calendar: &Calendar) -> Result<(), RepositoryError> {
        if !self.calendars.contains_key(&calendar.id) {
            return Err(RepositoryError);
        }
        self.calendars.insert(calendar.id, calendar.clone());
        Ok(())
    }

    fn list_calendars(&self) -> Vec<Calendar> {
        self.calendars.values().cloned().collect()
    }

    fn get_calendar(&self, id: Uuid) -> Option<Calendar> {
        self.calendars.get(&id).cloned()
    }

    fn delete_calendar(&mut self, id: Uuid) -> bool {
        self.calendars.remove(&id).is_some()
    }
}

impl EventRepository for InMemoryRepository {
    fn save_event(&mut self, event: &Event) -> Result<(), RepositoryError> {
        if self.events.contains_key(&event.id) {
            return Err(RepositoryError);
        }
        self.events.insert(event.id, event.clone());
        Ok(())
    }

    fn update_event(&mut self, event: &Event) -> Result<(), RepositoryError> {
        if !self.events.contains_key(&event.id) {
            return Err(RepositoryError);
        }
        self.events.insert(event.id, event.clone());
        Ok(())
    }

    fn get_event(&self, id: Uuid) -> Option<Event> {
        self.events.get(&id).cloned()
    }

    fn delete_event(&mut self, id: Uuid) -> bool {
        self.events.remove(&id).is_some()
    }

    fn delete_event_with_undo(&mut self, id: Uuid) -> Option<EventDeletionUndo> {
        self.events.remove(&id).map(|event| EventDeletionUndo {
            event,
            restored: false,
        })
    }

    fn undo_delete_event(&mut self, undo: &mut EventDeletionUndo) -> Result<(), RepositoryError> {
        if undo.restored || self.events.contains_key(&undo.event.id) {
            return Err(RepositoryError);
        }
        self.events.insert(undo.event.id, undo.event.clone());
        undo.restored = true;
        Ok(())
    }

    fn list_events_for_calendar(&self, calendar_id: Uuid) -> Vec<Event> {
        self.events
            .values()
            .filter(|e| e.calendar_id == calendar_id)
            .cloned()
            .collect()
    }

    fn timed_events_in_range(&self, range: &DateTimeRange) -> Vec<Event> {
        let mut result: Vec<Event> = self
            .events
            .values()
            .filter(|event| {
                // Only timed events are considered.
                if let EventSchedule::Timed { start, end, .. } = &event.schedule {
                    let event_range = DateTimeRange {
                        start: *start,
                        end: *end,
                    };
                    // Genuine overlap (not None) — boundaries are excluded
                    // by overlap's <= check on both sides.
                    !matches!(range.overlap(&event_range), RangeOverlap::None)
                } else {
                    // All-day events are excluded.
                    false
                }
            })
            .cloned()
            .collect();

        // Deterministic chronological ordering by start time ascending.
        result.sort_by(|a, b| {
            let a_start = match &a.schedule {
                EventSchedule::Timed { start, .. } => start,
                _ => unreachable!(), // filtered above
            };
            let b_start = match &b.schedule {
                EventSchedule::Timed { start, .. } => start,
                _ => unreachable!(),
            };
            a_start.cmp(b_start).then_with(|| a.id.cmp(&b.id))
        });

        result
    }
}
