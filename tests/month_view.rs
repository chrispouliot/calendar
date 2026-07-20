// Public contract pinned by this acceptance test:
//
//     pub mod month_view {
//         use crate::model::{Calendar, Event};
//         use chrono::NaiveDate;
//         use uuid::Uuid;
//
//         #[derive(Debug, Clone, PartialEq, Eq)]
//         pub struct EventChip {
//             pub event_id: Uuid,
//             pub title: String,
//             pub calendar_id: Uuid,
//             pub color: String,
//             pub is_all_day: bool,
//         }
//
//         #[derive(Debug, Clone, PartialEq, Eq)]
//         pub struct DayProjection {
//             pub date: NaiveDate,
//             pub in_displayed_month: bool,
//             pub all_day: Vec<EventChip>,
//             pub timed: Vec<EventChip>,
//         }
//
//         /// Pure, deterministic projection of `events` onto the fixed
//         /// 42-cell Monday-first grid for the displayed year/month.
//         /// Cells align with `calendar_grid::month_grid(year, month)`.
//         ///
//         /// Behavior:
//         ///   - Events whose calendar is missing from `calendars` or
//         ///     has `Calendar::visible == false` are excluded.
//         ///   - Visible events carry their calendar's color.
//         ///   - All-day events appear on every local date in
//         ///     `[start_date, end_date_exclusive)`; the exclusive end
//         ///     date is NOT occupied.
//         ///   - Timed events appear on every local fixed-offset
//         ///     calendar date in `[start, end)`. An event whose `end`
//         ///     is exactly local midnight does NOT appear on the
//         ///     ending date.
//         ///   - Within a day, all-day chips come before timed chips;
//         ///     timed chips are ordered by start time ascending;
//         ///     ties broken deterministically (e.g. by event id /
//         ///     title).
//         ///   - Spillover days are part of the 42-cell grid, so an
//         ///     event on an adjacent-month date can appear.
//         ///   - Recurrence expansion is out of scope.
//         pub fn project_month(
//             year: i32,
//             month: u32,
//             calendars: &[Calendar],
//             events: &[Event],
//         ) -> [DayProjection; 42];
//     }
//
// The test is pure: deterministic UUIDs, fixed chrono datetimes at a
// +02:00 fixed offset, and no clock / locale / filesystem / GTK
// reads.

use calendar::calendar_grid::month_grid;
use calendar::model::{Calendar, CalendarSource, Event, EventSchedule};
use calendar::month_view::{DayProjection, project_month};
use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone};
use uuid::Uuid;

const TWO_HOURS_SECS: i32 = 2 * 3600;

fn at(year: i32, month: u32, day: u32, hour: u32, min: u32) -> DateTime<FixedOffset> {
    let naive = NaiveDate::from_ymd_opt(year, month, day)
        .unwrap()
        .and_hms_opt(hour, min, 0)
        .unwrap();
    // Treat the supplied wall-clock components as local time in the
    // +02:00 fixed offset. For a FixedOffset this is a deterministic
    // single result (no DST ambiguity / gap), so unwrapping is sound.
    FixedOffset::east_opt(TWO_HOURS_SECS)
        .unwrap()
        .from_local_datetime(&naive)
        .single()
        .expect("+02:00 is a fixed offset: local lookup is unambiguous")
}

#[test]
fn phase5_month_view_event_projection() {
    // ----- Calendars: one visible Personal, one visible Work, one hidden.
    let personal_id = Uuid::parse_str("11111111-1111-1111-1111-111111111111").unwrap();
    let work_id = Uuid::parse_str("22222222-2222-2222-2222-222222222222").unwrap();
    let hidden_id = Uuid::parse_str("33333333-3333-3333-3333-333333333333").unwrap();
    let orphan_id = Uuid::parse_str("44444444-4444-4444-4444-444444444444").unwrap();

    let personal = Calendar {
        id: personal_id,
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let work = Calendar {
        id: work_id,
        name: "Work".to_string(),
        color: "#cc3333".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let hidden = Calendar {
        id: hidden_id,
        name: "Hidden".to_string(),
        color: "#999999".to_string(),
        visible: false,
        read_only: false,
        source: CalendarSource::Local,
    };
    let calendars = vec![personal, work, hidden];

    // ----- Events.
    // All-day Personal event spanning [May 14, May 16): May 14, 15, NOT 16.
    let all_day_long = Event {
        id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa01").unwrap(),
        calendar_id: personal_id,
        title: "Conference".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Single-day all-day on May 15 in Work.
    let all_day_workshop = Event {
        id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa02").unwrap(),
        calendar_id: work_id,
        title: "Workshop".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // All-day Personal event on June 3 — spillover into the next-month grid.
    let spillover = Event {
        id: Uuid::parse_str("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaa03").unwrap(),
        calendar_id: personal_id,
        title: "Spillover Day".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::AllDay {
            start_date: NaiveDate::from_ymd_opt(2026, 6, 3).unwrap(),
            end_date_exclusive: NaiveDate::from_ymd_opt(2026, 6, 4).unwrap(),
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Timed in Work on May 15 09:00-10:00.
    let standup = Event {
        id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb01").unwrap(),
        calendar_id: work_id,
        title: "Standup".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 15, 9, 0),
            end: at(2026, 5, 15, 10, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Cross-midnight timed in Work: May 15 23:00 - May 16 01:00. Both dates.
    let night_shift = Event {
        id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb02").unwrap(),
        calendar_id: work_id,
        title: "Night Shift".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 15, 23, 0),
            end: at(2026, 5, 16, 1, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Timed ending exactly at midnight: May 10 22:00 - May 11 00:00.
    // Occupies only the local date May 10.
    let late_dinner = Event {
        id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb03").unwrap(),
        calendar_id: personal_id,
        title: "Late Dinner".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 10, 22, 0),
            end: at(2026, 5, 11, 0, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Hidden calendar event — must NOT appear anywhere.
    let hidden_event = Event {
        id: Uuid::parse_str("cccccccc-cccc-cccc-cccc-ccccccccc001").unwrap(),
        calendar_id: hidden_id,
        title: "Should Not Appear".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 15, 10, 0),
            end: at(2026, 5, 15, 11, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Event whose calendar is not in `calendars` — must NOT appear.
    let orphan_event = Event {
        id: Uuid::parse_str("dddddddd-dddd-dddd-dddd-dddddddddd01").unwrap(),
        calendar_id: orphan_id,
        title: "Orphan".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 15, 12, 0),
            end: at(2026, 5, 15, 13, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    // Extra timed on May 15 to test start-time ordering of timed chips.
    let early = Event {
        id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb04").unwrap(),
        calendar_id: work_id,
        title: "Early".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 15, 8, 0),
            end: at(2026, 5, 15, 9, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };
    let afternoon = Event {
        id: Uuid::parse_str("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbb05").unwrap(),
        calendar_id: work_id,
        title: "Afternoon".to_string(),
        location: String::new(),
        description: String::new(),
        schedule: EventSchedule::Timed {
            start: at(2026, 5, 15, 14, 0),
            end: at(2026, 5, 15, 15, 0),
            timezone: None,
        },
        recurrence: None,
        reminders: Vec::new(),
    };

    let events = vec![
        all_day_long.clone(),
        all_day_workshop.clone(),
        spillover.clone(),
        standup.clone(),
        night_shift.clone(),
        late_dinner.clone(),
        hidden_event,
        orphan_event,
        early,
        afternoon,
    ];

    // ----- Project May 2026.
    let projection = project_month(2026, 5, &calendars, &events);
    let grid = month_grid(2026, 5);

    // 1) Length and date alignment with month_grid.
    assert_eq!(projection.len(), 42);
    for (i, day) in projection.iter().enumerate() {
        let cell = grid[i];
        let expected_date = NaiveDate::from_ymd_opt(cell.year, cell.month, cell.day).unwrap();
        assert_eq!(
            day.date, expected_date,
            "cell {i} date must match month_grid"
        );
        assert_eq!(
            day.in_displayed_month, cell.in_displayed_month,
            "cell {i} in_displayed_month must match month_grid"
        );
    }

    let find_day = |y: i32, m: u32, d: u32| -> &DayProjection {
        let target = NaiveDate::from_ymd_opt(y, m, d).unwrap();
        projection
            .iter()
            .find(|p| p.date == target)
            .unwrap_or_else(|| panic!("grid must contain {y:04}-{m:02}-{d:02}"))
    };

    let titles_of = |day: &DayProjection| -> Vec<String> {
        day.all_day
            .iter()
            .chain(day.timed.iter())
            .map(|c| c.title.clone())
            .collect()
    };

    // 2) All-day multi-day occupies start and middle, NOT exclusive end.
    let may_14 = find_day(2026, 5, 14);
    assert_eq!(may_14.all_day.len(), 1, "May 14 should host Conference");
    assert_eq!(may_14.all_day[0].title, "Conference");
    assert_eq!(may_14.all_day[0].event_id, all_day_long.id);
    assert_eq!(may_14.all_day[0].calendar_id, personal_id);
    assert_eq!(may_14.all_day[0].color, "#3366cc");
    assert!(may_14.all_day[0].is_all_day);

    let may_15 = find_day(2026, 5, 15);
    let may_15_all_day_titles: Vec<&str> =
        may_15.all_day.iter().map(|c| c.title.as_str()).collect();
    assert!(
        may_15_all_day_titles.contains(&"Conference"),
        "May 15 all-day must include Conference: got {may_15_all_day_titles:?}"
    );
    assert!(
        may_15_all_day_titles.contains(&"Workshop"),
        "May 15 all-day must include Workshop: got {may_15_all_day_titles:?}"
    );

    let may_16 = find_day(2026, 5, 16);
    let may_16_titles = titles_of(may_16);
    assert!(
        !may_16_titles.contains(&"Conference".to_string()),
        "May 16 is the exclusive end and must NOT host Conference: got {may_16_titles:?}"
    );
    assert!(
        !may_16_titles.contains(&"Workshop".to_string()),
        "May 16 is the exclusive end and must NOT host Workshop: got {may_16_titles:?}"
    );

    // 3) Cross-midnight event appears on both May 15 and May 16.
    assert!(
        may_15.timed.iter().any(|c| c.title == "Night Shift"),
        "May 15 timed must include Night Shift"
    );
    assert!(
        may_16.timed.iter().any(|c| c.title == "Night Shift"),
        "May 16 timed must include Night Shift (cross-midnight)"
    );

    // 4) Event ending exactly at midnight: appears on May 10 only, NOT May 11.
    let may_10 = find_day(2026, 5, 10);
    assert!(
        may_10.timed.iter().any(|c| c.title == "Late Dinner"),
        "May 10 must host Late Dinner"
    );
    let may_11 = find_day(2026, 5, 11);
    let may_11_titles = titles_of(may_11);
    assert!(
        !may_11_titles.contains(&"Late Dinner".to_string()),
        "May 11 is the local ending date at midnight and must NOT host Late Dinner: got {may_11_titles:?}"
    );

    // 5) Hidden / orphan events are excluded everywhere in the grid.
    for (i, day) in projection.iter().enumerate() {
        for chip in day.all_day.iter().chain(day.timed.iter()) {
            assert_ne!(
                chip.calendar_id, hidden_id,
                "cell {i} on {} must not contain an event from the hidden calendar",
                day.date
            );
            assert_ne!(
                chip.calendar_id, orphan_id,
                "cell {i} on {} must not contain an event from a missing calendar",
                day.date
            );
        }
    }
    let may_15_titles = titles_of(may_15);
    assert!(
        !may_15_titles.contains(&"Should Not Appear".to_string()),
        "hidden calendar event must not appear on May 15"
    );
    assert!(
        !may_15_titles.contains(&"Orphan".to_string()),
        "orphan (missing-calendar) event must not appear on May 15"
    );

    // 6) May 15 ordering: all-day first, then timed by start time.
    let may_15_timed_titles: Vec<&str> = may_15.timed.iter().map(|c| c.title.as_str()).collect();
    assert_eq!(
        may_15_timed_titles,
        vec!["Early", "Standup", "Afternoon", "Night Shift"],
        "timed chips on May 15 must be ordered by start time ascending"
    );
    for chip in &may_15.timed {
        assert_eq!(
            chip.color, "#cc3333",
            "visible calendar color must be carried onto timed chip"
        );
        assert!(!chip.is_all_day, "timed chip must have is_all_day == false");
    }
    for chip in &may_15.all_day {
        assert!(chip.is_all_day, "all-day chip must have is_all_day == true");
    }

    // 7) Spillover day hosts its event.
    let jun_03 = find_day(2026, 6, 3);
    let jun_03_titles = titles_of(jun_03);
    assert!(
        jun_03_titles.contains(&"Spillover Day".to_string()),
        "spillover all-day on June 3 must appear in the grid: got {jun_03_titles:?}"
    );
    assert!(!jun_03.in_displayed_month, "June 3 is spillover");
    assert_eq!(jun_03.all_day[0].color, "#3366cc");

    // 8) Other days are empty.
    let occupied: &[NaiveDate] = &[
        NaiveDate::from_ymd_opt(2026, 5, 10).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 15).unwrap(),
        NaiveDate::from_ymd_opt(2026, 5, 16).unwrap(),
        NaiveDate::from_ymd_opt(2026, 6, 3).unwrap(),
    ];
    for (i, day) in projection.iter().enumerate() {
        if occupied.contains(&day.date) {
            continue;
        }
        assert!(
            day.all_day.is_empty() && day.timed.is_empty(),
            "cell {i} on {} unexpectedly has chips",
            day.date
        );
    }
}
