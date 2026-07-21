// Public contract pinned by this acceptance test:
//
//     pub mod backend {
//         use std::path::Path;
//
//         /// File-backed SQLite repository. Implements both
//         /// `CalendarRepository` and `EventRepository`. Opening a
//         /// path that does not yet exist creates the file; the
//         /// Phase 7 schema (calendars, events, reminders,
//         /// sync_metadata) is applied on open so callers do not
//         /// need to run migrations themselves.
//         pub struct SqliteRepository { /* private fields */ }
//
//         impl SqliteRepository {
//             /// Open (or create) a SQLite database at the given path.
//             pub fn open<P: AsRef<Path>>(path: P)
//                 -> Result<Self, RepositoryError>;
//         }
//
//         impl CalendarRepository for SqliteRepository { /* ... */ }
//         impl EventRepository for SqliteRepository { /* ... */ }
//     }
//
// The Phase 7 schema includes the following tables: calendars,
// events, reminders, sync_metadata.
//
// Every value in the test is constructed from deterministic literals
// (fixed UUIDs and fixed chrono datetimes at a +02:00 fixed offset).
// The test does not read the clock, the locale, GTK/Adwaita, or any
// filesystem location outside of its per-test temp-dir database
// path. The repository is opened on a unique database path under
// temp_dir and is not shared with any other test.

use calendar::backend::{CalendarRepository, EventRepository, SqliteRepository};
use calendar::model::{Calendar, CalendarSource, DateTimeRange, Event, EventSchedule};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use rusqlite::Connection;
use std::path::PathBuf;
use uuid::Uuid;

const TWO_HOURS_SECS: i32 = 2 * 3600;
const FIVE_HOURS_WEST_SECS: i32 = -5 * 3600;

fn at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<FixedOffset> {
    at_with_offset(TWO_HOURS_SECS, year, month, day, hour, min)
}

fn at_with_offset(
    offset_secs: i32,
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    min: u32,
) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap();
    FixedOffset::east_opt(offset_secs)
        .unwrap()
        .from_local_datetime(&naive)
        .single()
        .unwrap()
}

fn unique_temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!("calendar_phase7_{label}_{pid}_{nanos}.sqlite"));
    path
}

/// Best-effort cleanup of the per-test database file. Runs on
/// normal return and on panic, so a failing test never leaves a
/// stray `.sqlite` file in `temp_dir`.
struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[test]
fn phase7_sqlite_repository_persists_calendars_and_events() {
    let db_path = unique_temp_db_path("persistence");
    let _cleanup = TempDb(db_path.clone());

    // ----- Deterministic fixtures (fixed UUIDs, fixed +02:00 offset)
    let cal_id = Uuid::parse_str("cccc7777-cccc-cccc-cccc-cccccccccccc").unwrap();
    let calendar = Calendar {
        id: cal_id,
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };

    let all_day_id = Uuid::parse_str("eeeeaaaa-eeee-aaaa-eeee-aaaaaaaaaaaa").unwrap();
    let day = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
    let next = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();
    let all_day = Event {
        id: all_day_id,
        calendar_id: cal_id,
        title: "Holiday".to_string(),
        location: "Home".to_string(),
        description: "Bank holiday".to_string(),
        schedule: EventSchedule::AllDay {
            start_date: day,
            end_date_exclusive: next,
        },
        recurrence: None,
        reminders: Vec::new(),
    };

    let timed_id = Uuid::parse_str("eeeebbbb-eeee-bbbb-eeee-bbbbbbbbbbbb").unwrap();
    let timed = Event {
        id: timed_id,
        calendar_id: cal_id,
        title: "Standup".to_string(),
        location: "Room 42".to_string(),
        description: "Daily status".to_string(),
        schedule: EventSchedule::Timed {
            start: at(2026, 7, 20, 9, 0),
            end: at(2026, 7, 20, 10, 0),
            timezone: Some("Europe/Berlin".to_string()),
        },
        recurrence: None,
        reminders: Vec::new(),
    };

    // This is [08:00, 09:00) UTC while the query below is [07:00, 09:00)
    // UTC. Its -05:00 RFC3339 text must not be compared lexically with the
    // query's +02:00 text.
    let offset_timed_id = Uuid::parse_str("eeeecccc-eeee-cccc-eeee-cccccccccccc").unwrap();
    let offset_timed = Event {
        id: offset_timed_id,
        calendar_id: cal_id,
        title: "Offset follow-up".to_string(),
        location: "Remote".to_string(),
        description: "Cross-offset overlap".to_string(),
        schedule: EventSchedule::Timed {
            start: at_with_offset(FIVE_HOURS_WEST_SECS, 2026, 7, 20, 3, 0),
            end: at_with_offset(FIVE_HOURS_WEST_SECS, 2026, 7, 20, 4, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };

    // ----- Phase 1: first open, persist, exercise contract, drop
    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("opening a fresh sqlite database must succeed");

        repo.save_calendar(&calendar)
            .expect("save_calendar must succeed");
        repo.save_event(&all_day)
            .expect("save_event (all-day) must succeed");
        repo.save_event(&timed)
            .expect("save_event (timed) must succeed");
        repo.save_event(&offset_timed)
            .expect("save_event (cross-offset timed) must succeed");

        // list_calendars
        let listed_cals = repo.list_calendars();
        assert_eq!(
            listed_cals.len(),
            1,
            "exactly one calendar must be listed after save"
        );
        assert!(
            listed_cals.contains(&calendar),
            "list_calendars must include the saved calendar"
        );

        // get_calendar — exact record
        let cal_back = repo
            .get_calendar(cal_id)
            .expect("calendar must be retrievable by id");
        assert_eq!(cal_back.id, cal_id, "calendar id must round-trip");
        assert_eq!(cal_back.name, "Personal", "calendar name must round-trip");
        assert_eq!(cal_back.color, "#3366cc", "calendar color must round-trip");
        assert!(cal_back.visible, "calendar visibility must round-trip");
        assert!(!cal_back.read_only, "calendar read_only must round-trip");
        assert_eq!(
            cal_back.source,
            CalendarSource::Local,
            "calendar source must round-trip"
        );

        // get_event (all-day) — exact record
        let ad_back = repo
            .get_event(all_day_id)
            .expect("all-day event must be retrievable by id");
        assert_eq!(ad_back.id, all_day_id, "all-day id must round-trip");
        assert_eq!(
            ad_back.calendar_id, cal_id,
            "all-day calendar_id must round-trip"
        );
        assert_eq!(ad_back.title, "Holiday", "all-day title must round-trip");
        assert_eq!(ad_back.location, "Home", "all-day location must round-trip");
        assert_eq!(
            ad_back.description, "Bank holiday",
            "all-day description must round-trip"
        );
        assert!(
            ad_back.recurrence.is_none(),
            "all-day recurrence must be None"
        );
        assert!(
            ad_back.reminders.is_empty(),
            "all-day reminders must be empty"
        );
        match ad_back.schedule {
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            } => {
                assert_eq!(start_date, day, "all-day start_date must round-trip");
                assert_eq!(
                    end_date_exclusive, next,
                    "all-day end_date_exclusive must round-trip"
                );
            }
            other => {
                panic!("all-day event must round-trip as AllDay, got {other:?}")
            }
        }

        // get_event (timed) — exact record
        let timed_back = repo
            .get_event(timed_id)
            .expect("timed event must be retrievable by id");
        assert_eq!(timed_back.id, timed_id, "timed id must round-trip");
        assert_eq!(
            timed_back.calendar_id, cal_id,
            "timed calendar_id must round-trip"
        );
        assert_eq!(timed_back.title, "Standup", "timed title must round-trip");
        assert_eq!(
            timed_back.location, "Room 42",
            "timed location must round-trip"
        );
        assert_eq!(
            timed_back.description, "Daily status",
            "timed description must round-trip"
        );
        assert!(
            timed_back.recurrence.is_none(),
            "timed recurrence must be None"
        );
        assert!(
            timed_back.reminders.is_empty(),
            "timed reminders must be empty"
        );
        match timed_back.schedule {
            EventSchedule::Timed {
                start,
                end,
                timezone,
            } => {
                assert_eq!(start, at(2026, 7, 20, 9, 0), "timed start must round-trip");
                assert_eq!(end, at(2026, 7, 20, 10, 0), "timed end must round-trip");
                assert_eq!(
                    timezone.as_deref(),
                    Some("Europe/Berlin"),
                    "timed timezone must round-trip"
                );
            }
            other => {
                panic!("timed event must round-trip as Timed, got {other:?}")
            }
        }

        // list_events_for_calendar
        let mut listed_events = repo.list_events_for_calendar(cal_id);
        listed_events.sort_by(|a, b| a.title.cmp(&b.title));
        let titles: Vec<&str> = listed_events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Holiday", "Offset follow-up", "Standup"],
            "list_events_for_calendar must return all three events"
        );

        // timed_events_in_range: [9:00, 11:00) on 2026-07-20 must
        // include the timed event and exclude the all-day event.
        let q_start = at(2026, 7, 20, 9, 0);
        let q_end = at(2026, 7, 20, 11, 0);
        let range = DateTimeRange::new(q_start, q_end).expect("forward range must build");
        let in_range = repo.timed_events_in_range(&range);
        let range_titles: Vec<&str> = in_range.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            range_titles,
            vec!["Standup", "Offset follow-up"],
            "timed range query must use absolute times and chronological ordering across offsets"
        );
    } // repo is dropped here, releasing the on-disk file lock

    // ----- Phase 2: reopen the same path, assert exact persistence
    {
        let repo =
            SqliteRepository::open(&db_path).expect("reopening the sqlite database must succeed");

        let listed_cals = repo.list_calendars();
        assert_eq!(
            listed_cals.len(),
            1,
            "exactly one calendar must be listed after reopen"
        );
        assert!(
            listed_cals.contains(&calendar),
            "calendar list must round-trip across reopen"
        );

        let cal_back = repo
            .get_calendar(cal_id)
            .expect("calendar must be retrievable after reopen");
        assert_eq!(cal_back.id, cal_id);
        assert_eq!(cal_back.name, "Personal");
        assert_eq!(cal_back.color, "#3366cc");
        assert!(cal_back.visible);
        assert!(!cal_back.read_only);
        assert_eq!(cal_back.source, CalendarSource::Local);

        let ad_back = repo
            .get_event(all_day_id)
            .expect("all-day event must round-trip across reopen");
        assert_eq!(ad_back.id, all_day_id);
        assert_eq!(ad_back.calendar_id, cal_id);
        assert_eq!(ad_back.title, "Holiday");
        assert_eq!(ad_back.location, "Home");
        assert_eq!(ad_back.description, "Bank holiday");
        assert!(ad_back.recurrence.is_none());
        assert!(ad_back.reminders.is_empty());
        match ad_back.schedule {
            EventSchedule::AllDay {
                start_date,
                end_date_exclusive,
            } => {
                assert_eq!(start_date, day);
                assert_eq!(end_date_exclusive, next);
            }
            other => {
                panic!("all-day event must round-trip as AllDay, got {other:?}")
            }
        }

        let timed_back = repo
            .get_event(timed_id)
            .expect("timed event must round-trip across reopen");
        assert_eq!(timed_back.id, timed_id);
        assert_eq!(timed_back.calendar_id, cal_id);
        assert_eq!(timed_back.title, "Standup");
        assert_eq!(timed_back.location, "Room 42");
        assert_eq!(timed_back.description, "Daily status");
        assert!(timed_back.recurrence.is_none());
        assert!(timed_back.reminders.is_empty());
        match timed_back.schedule {
            EventSchedule::Timed {
                start,
                end,
                timezone,
            } => {
                assert_eq!(start, at(2026, 7, 20, 9, 0));
                assert_eq!(end, at(2026, 7, 20, 10, 0));
                assert_eq!(timezone.as_deref(), Some("Europe/Berlin"));
            }
            other => {
                panic!("timed event must round-trip as Timed, got {other:?}")
            }
        }

        let offset_timed_back = repo
            .get_event(offset_timed_id)
            .expect("cross-offset timed event must round-trip across reopen");
        assert_eq!(offset_timed_back.id, offset_timed_id);
        assert_eq!(offset_timed_back.calendar_id, cal_id);
        assert_eq!(offset_timed_back.title, "Offset follow-up");
        assert_eq!(offset_timed_back.location, "Remote");
        assert_eq!(offset_timed_back.description, "Cross-offset overlap");
        assert!(offset_timed_back.recurrence.is_none());
        assert!(offset_timed_back.reminders.is_empty());
        match offset_timed_back.schedule {
            EventSchedule::Timed {
                start,
                end,
                timezone,
            } => {
                assert_eq!(
                    start,
                    at_with_offset(FIVE_HOURS_WEST_SECS, 2026, 7, 20, 3, 0)
                );
                assert_eq!(end, at_with_offset(FIVE_HOURS_WEST_SECS, 2026, 7, 20, 4, 0));
                assert_eq!(
                    start.offset().local_minus_utc(),
                    FIVE_HOURS_WEST_SECS,
                    "timed start must preserve its -05:00 FixedOffset"
                );
                assert_eq!(
                    end.offset().local_minus_utc(),
                    FIVE_HOURS_WEST_SECS,
                    "timed end must preserve its -05:00 FixedOffset"
                );
                assert!(timezone.is_none(), "None timezone must round-trip as None");
            }
            other => {
                panic!("cross-offset event must round-trip as Timed, got {other:?}")
            }
        }

        let mut listed_events = repo.list_events_for_calendar(cal_id);
        listed_events.sort_by(|a, b| a.title.cmp(&b.title));
        let titles: Vec<&str> = listed_events.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            titles,
            vec!["Holiday", "Offset follow-up", "Standup"],
            "list_events_for_calendar must return all three events after reopen"
        );

        let q_start = at(2026, 7, 20, 9, 0);
        let q_end = at(2026, 7, 20, 11, 0);
        let range = DateTimeRange::new(q_start, q_end).expect("forward range must build");
        let in_range = repo.timed_events_in_range(&range);
        let range_titles: Vec<&str> = in_range.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(
            range_titles,
            vec!["Standup", "Offset follow-up"],
            "timed range query must use absolute times and chronological ordering after reopen"
        );
    } // repo is dropped here, releasing the on-disk file lock

    // ----- Phase 3: schema inspection via raw rusqlite, after all
    // repository handles have been dropped.
    let conn =
        Connection::open(&db_path).expect("rusqlite must be able to open the on-disk database");
    let mut stmt = conn
        .prepare(
            "SELECT name FROM sqlite_master \
             WHERE type = 'table' AND name IN \
             ('calendars', 'events', 'reminders', 'sync_metadata') \
             ORDER BY name",
        )
        .expect("preparing the schema query must succeed");
    let rows: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .expect("querying the schema must succeed")
        .map(|r| r.expect("row read must succeed"))
        .collect();
    let expected = vec![
        "calendars".to_string(),
        "events".to_string(),
        "reminders".to_string(),
        "sync_metadata".to_string(),
    ];
    assert_eq!(
        rows, expected,
        "Phase 7 schema must include the four named tables"
    );
}
