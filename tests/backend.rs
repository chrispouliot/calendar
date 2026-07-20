// Public contract pinned by this acceptance test:
//
//     pub mod backend {
//         use crate::model::{Calendar, DateTimeRange, Event};
//         use uuid::Uuid;
//
//         /// Storage abstraction for calendars. Methods take `&mut self`
//         /// so a single in-memory repository can be updated in place.
//         pub trait CalendarRepository {
//             /// Save a calendar. Saving a calendar whose UUID already
//             /// exists replaces the prior value rather than duplicating
//             /// it.
//             fn save_calendar(
//                 &mut self,
//                 calendar: &Calendar,
//             ) -> Result<(), RepositoryError>;
//
//             fn list_calendars(&self) -> Vec<Calendar>;
//             fn get_calendar(&self, id: Uuid) -> Option<Calendar>;
//
//             /// Returns true iff a calendar with that ID was present.
//             fn delete_calendar(&mut self, id: Uuid) -> bool;
//         }
//
//         /// Storage abstraction for events.
//         pub trait EventRepository {
//             /// Create a new event. Returns Err if the UUID already
//             /// exists; use `update_event` to replace an existing event.
//             fn save_event(&mut self, event: &Event)
//                 -> Result<(), RepositoryError>;
//
//             /// Replace an existing event. Returns Err if the UUID
//             /// does not already exist.
//             fn update_event(&mut self, event: &Event)
//                 -> Result<(), RepositoryError>;
//
//             fn get_event(&self, id: Uuid) -> Option<Event>;
//
//             /// Returns true iff an event with that ID was present.
//             fn delete_event(&mut self, id: Uuid) -> bool;
//
//             /// All events for the given calendar UUID. Order is not
//             /// pinned by this test.
//             fn list_events_for_calendar(
//                 &self,
//                 calendar_id: Uuid,
//             ) -> Vec<Event>;
//
//             /// Timed events whose [start, end) interval genuinely
//             /// overlaps `range` (start-inclusive, end-exclusive on
//             /// both sides). Returned in deterministic chronological
//             /// order — start time ascending. All-day events are out
//             of scope for this query in Phase 4.
// (The line above is intentionally a doc comment for the
// implementation; the contract summary is repeated below.)
//
//             fn timed_events_in_range(
//                 &self,
//                 range: &DateTimeRange,
//             ) -> Vec<Event>;
//         }
//
//         // Combined in-memory implementation usable by tests and by
//         application startup. Constructed with `InMemoryRepository::new()`.
// (See line above for the summary.)
//         pub struct InMemoryRepository { /* private fields */ }
//
//         impl InMemoryRepository {
//             pub fn new() -> Self;
//         }
//
//         impl CalendarRepository for InMemoryRepository { /* ... */ }
//         impl EventRepository for InMemoryRepository { /* ... */ }
//
//         #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//         pub struct RepositoryError;
//     }
//
// Every value in the test is constructed from deterministic literals
// (fixed UUIDs and fixed chrono datetimes at a +02:00 fixed offset).
// The test does not read the clock, the locale, the filesystem, or any
// GTK/Adwaita state. The repository is freshly constructed for this
// test and is not shared with any other test.

use calendar::backend::{CalendarRepository, EventRepository, InMemoryRepository};
use calendar::model::{Calendar, CalendarSource, DateTimeRange, Event, EventSchedule};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

const TWO_HOURS_SECS: i32 = 2 * 3600;

fn at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap();
    FixedOffset::east_opt(TWO_HOURS_SECS)
        .unwrap()
        .from_utc_datetime(&naive)
}

#[test]
fn phase4_in_memory_repository_crud_and_range_query() {
    // ----- Calendar save / list / get / replace / delete
    let mut repo = InMemoryRepository::new();

    let cal_a_id = Uuid::parse_str("aaaa1111-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let cal_b_id = Uuid::parse_str("bbbb2222-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let cal_a = Calendar {
        id: cal_a_id,
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let cal_b = Calendar {
        id: cal_b_id,
        name: "Work".to_string(),
        color: "#cc3333".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };

    repo.save_calendar(&cal_a)
        .expect("first calendar save must succeed");
    repo.save_calendar(&cal_b)
        .expect("second calendar save must succeed");

    let listed = repo.list_calendars();
    assert_eq!(listed.len(), 2, "two saved calendars must be listable");
    assert!(
        repo.get_calendar(cal_a_id).is_some(),
        "cal_a must be gettable by id"
    );
    assert!(
        repo.get_calendar(cal_b_id).is_some(),
        "cal_b must be gettable by id"
    );

    // Saving with an existing UUID replaces the prior value.
    let cal_a_v2 = Calendar {
        name: "Personal (renamed)".to_string(),
        ..cal_a.clone()
    };
    repo.save_calendar(&cal_a_v2)
        .expect("replacement calendar save must succeed");
    assert_eq!(
        repo.list_calendars().len(),
        2,
        "saving a calendar with an existing UUID must replace, not duplicate"
    );
    let cal_a_back = repo
        .get_calendar(cal_a_id)
        .expect("cal_a must still exist after replace");
    assert_eq!(
        cal_a_back.name, "Personal (renamed)",
        "replacement must overwrite the stored value"
    );

    // Delete removes it; deleting again is a no-op.
    assert!(
        repo.delete_calendar(cal_a_id),
        "delete of an existing calendar must return true"
    );
    assert!(
        repo.get_calendar(cal_a_id).is_none(),
        "deleted calendar must not be retrievable"
    );
    assert_eq!(
        repo.list_calendars().len(),
        1,
        "only the un-deleted calendar must remain"
    );
    assert!(
        !repo.delete_calendar(cal_a_id),
        "deleting a missing calendar must return false"
    );

    // ----- Event create / get / update / delete + filter by calendar
    let ev1_id = Uuid::parse_str("eeee1111-1111-1111-1111-111111111111").unwrap();
    let ev2_id = Uuid::parse_str("eeee2222-2222-2222-2222-222222222222").unwrap();

    let ev1 = Event {
        id: ev1_id,
        calendar_id: cal_b_id,
        title: "Standup".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 9, 0),
            end: at(2026, 7, 1, 10, 0),
            timezone: Some("Europe/Berlin".to_string()),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let ev2 = Event {
        id: ev2_id,
        calendar_id: cal_b_id,
        title: "Lunch".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 12, 0),
            end: at(2026, 7, 1, 13, 0),
            timezone: Some("Europe/Berlin".to_string()),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    repo.save_event(&ev1).expect("save_event ev1 must succeed");
    repo.save_event(&ev2).expect("save_event ev2 must succeed");

    // save with an existing UUID is rejected; use update_event instead.
    let dup = ev1.clone();
    assert!(
        repo.save_event(&dup).is_err(),
        "saving an event with an existing UUID must not silently overwrite"
    );

    // get_event round-trip.
    let ev1_back = repo
        .get_event(ev1_id)
        .expect("ev1 must be retrievable by id");
    assert_eq!(ev1_back.title, "Standup");

    // update_event replaces the stored event.
    let ev1_v2 = Event {
        title: "Daily Standup".to_string(),
        ..ev1.clone()
    };
    repo.update_event(&ev1_v2)
        .expect("update_event ev1 must succeed");
    let ev1_back = repo
        .get_event(ev1_id)
        .expect("ev1 must still exist after update");
    assert_eq!(ev1_back.title, "Daily Standup");

    // update_event on a missing event must error.
    let phantom_id = Uuid::parse_str("eeeeffff-eeee-eeee-eeee-eeeeeeeeeeee").unwrap();
    let phantom = Event {
        id: phantom_id,
        calendar_id: cal_b_id,
        title: "Phantom".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 9, 0),
            end: at(2026, 7, 1, 10, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    assert!(
        repo.update_event(&phantom).is_err(),
        "updating a non-existent event must error"
    );

    // list_events_for_calendar — sort to be order-independent.
    let mut in_b = repo.list_events_for_calendar(cal_b_id);
    in_b.sort_by(|a, b| a.title.cmp(&b.title));
    assert_eq!(in_b.len(), 2, "two events belong to cal_b");
    let titles: Vec<&str> = in_b.iter().map(|e| e.title.as_str()).collect();
    assert_eq!(titles, vec!["Daily Standup", "Lunch"]);

    let in_a = repo.list_events_for_calendar(cal_a_id);
    assert!(
        in_a.is_empty(),
        "no events belong to cal_a (no event was ever saved to it)"
    );

    // delete_event
    assert!(
        repo.delete_event(ev2_id),
        "delete of an existing event must return true"
    );
    assert!(
        repo.get_event(ev2_id).is_none(),
        "deleted event must not be retrievable"
    );
    assert!(
        !repo.delete_event(ev2_id),
        "deleting a missing event must return false"
    );

    // ----- Timed event range query
    // Query window: [9:00, 12:00) on 2026-07-01 (+02:00).
    let q_start = at(2026, 7, 1, 9, 0);
    let q_end = at(2026, 7, 1, 12, 0);
    let range = DateTimeRange::new(q_start, q_end).expect("forward range must build");

    // Boundary cases — must be EXCLUDED (start-inclusive, end-exclusive).
    let boundary_before = Event {
        id: Uuid::parse_str("ee000001-0000-0000-0000-000000000001").unwrap(),
        calendar_id: cal_b_id,
        title: "boundary before".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 8, 0),
            end: q_start, // ends exactly when query starts
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let boundary_after = Event {
        id: Uuid::parse_str("ee000002-0000-0000-0000-000000000002").unwrap(),
        calendar_id: cal_b_id,
        title: "boundary after".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: q_end, // starts exactly when query ends
            end: at(2026, 7, 1, 13, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    repo.save_event(&boundary_before)
        .expect("save boundary_before must succeed");
    repo.save_event(&boundary_after)
        .expect("save boundary_after must succeed");

    // Genuine overlaps — must be INCLUDED.
    let overlap_start = Event {
        id: Uuid::parse_str("ee000003-0000-0000-0000-000000000003").unwrap(),
        calendar_id: cal_b_id,
        title: "overlap start".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 8, 30),
            end: at(2026, 7, 1, 9, 30), // ends 30 min into the query
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let inside = Event {
        id: Uuid::parse_str("ee000004-0000-0000-0000-000000000004").unwrap(),
        calendar_id: cal_b_id,
        title: "inside".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 10, 0),
            end: at(2026, 7, 1, 11, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let overlap_end = Event {
        id: Uuid::parse_str("ee000005-0000-0000-0000-000000000005").unwrap(),
        calendar_id: cal_b_id,
        title: "overlap end".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 11, 30),
            end: at(2026, 7, 1, 12, 30), // starts 30 min before query ends
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Event that fully contains the query — must be INCLUDED.
    let contains = Event {
        id: Uuid::parse_str("ee000006-0000-0000-0000-000000000006").unwrap(),
        calendar_id: cal_b_id,
        title: "contains".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 8, 0),
            end: at(2026, 7, 1, 13, 0), // spans the entire query
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Far before / far after — must be EXCLUDED.
    let far_before = Event {
        id: Uuid::parse_str("ee000007-0000-0000-0000-000000000007").unwrap(),
        calendar_id: cal_b_id,
        title: "far before".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 6, 0),
            end: at(2026, 7, 1, 7, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let far_after = Event {
        id: Uuid::parse_str("ee000008-0000-0000-0000-000000000008").unwrap(),
        calendar_id: cal_b_id,
        title: "far after".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 1, 14, 0),
            end: at(2026, 7, 1, 15, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // All-day event spanning the query date — must be EXCLUDED from a
    // timed-event range query (mixed all-day/timed policy is deferred).
    let all_day = Event {
        id: Uuid::parse_str("ee000009-0000-0000-0000-000000000009").unwrap(),
        calendar_id: cal_b_id,
        title: "all day".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 7, 2).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    for ev in [
        &overlap_start,
        &inside,
        &overlap_end,
        &contains,
        &far_before,
        &far_after,
        &all_day,
    ] {
        repo.save_event(ev)
            .expect("save must succeed for range-query fixtures");
    }

    let in_range = repo.timed_events_in_range(&range);
    let titles: Vec<&str> = in_range.iter().map(|e| e.title.as_str()).collect();

    // Pin inclusions.
    assert!(
        titles.contains(&"contains"),
        "event fully containing the query must be returned: got {titles:?}"
    );
    assert!(
        titles.contains(&"overlap start"),
        "event overlapping the query start must be returned: got {titles:?}"
    );
    assert!(
        titles.contains(&"overlap end"),
        "event overlapping the query end must be returned: got {titles:?}"
    );
    assert!(
        titles.contains(&"inside"),
        "fully-contained event must be returned: got {titles:?}"
    );
    // The pre-existing ev1 (renamed to "Daily Standup") lives at
    // [9:00, 10:00), entirely inside [9:00, 12:00).
    assert!(
        titles.contains(&"Daily Standup"),
        "the pre-existing event inside the window must be returned: got {titles:?}"
    );

    // Pin exclusions.
    assert!(
        !titles.contains(&"boundary before"),
        "event ending exactly at the query start must be excluded: got {titles:?}"
    );
    assert!(
        !titles.contains(&"boundary after"),
        "event starting exactly at the query end must be excluded: got {titles:?}"
    );
    assert!(
        !titles.contains(&"far before"),
        "event entirely before the query must be excluded: got {titles:?}"
    );
    assert!(
        !titles.contains(&"far after"),
        "event entirely after the query must be excluded: got {titles:?}"
    );
    assert!(
        !titles.contains(&"all day"),
        "all-day events must be excluded from the timed range query: got {titles:?}"
    );

    // Pin deterministic chronological ordering: start time ascending.
    //   contains       8:00
    //   overlap start  8:30
    //   Daily Standup  9:00
    //   inside        10:00
    //   overlap end   11:30
    let expected_order = [
        "contains",
        "overlap start",
        "Daily Standup",
        "inside",
        "overlap end",
    ];
    assert_eq!(
        titles, expected_order,
        "range query must be ordered by event start time ascending"
    );
}
