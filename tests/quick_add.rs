// Public contract pinned by this acceptance test:
//
//     pub mod model {
//         use chrono::NaiveDate;
//         use uuid::Uuid;
//         use crate::model::Event;
//
//         /// Reason quick-add event construction can fail: the supplied
//         /// title was empty after trimming.
//         #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//         pub struct EmptyQuickAddTitle;
//
//         /// Build the base all-day event that the quick-add popover
//         /// would produce for the given date on a writable calendar.
//         ///
//         /// Pure / UI-independent:
//         ///   - Does not read the clock.
//         ///   - Does not generate nondeterministic IDs internally;
//         ///     the caller supplies the event UUID.
//         ///   - Does not touch GTK/Adwaita.
//         ///   - Does not persist to a repository.
//         ///
//         /// The supplied `title` is trimmed; a title that is empty
//         /// after trimming is rejected with `EmptyQuickAddTitle`. The
//         /// resulting `Event` has empty `location` and `description`,
//         /// no `recurrence`, no `reminders`, and an `AllDay` schedule
//         /// spanning exactly `date..date+1 day` as an exclusive end.
//         pub fn new_quick_add_event(
//             event_id: Uuid,
//             calendar_id: Uuid,
//             title: &str,
//             date: NaiveDate,
//         ) -> Result<Event, EmptyQuickAddTitle>;
//     }
//
// Every value in the test is constructed from deterministic literals
// (fixed UUIDs and a fixed NaiveDate). The test does not read the
// clock, the locale, the filesystem, or any GTK/Adwaita state.

use calendar::model::{Event, EventSchedule, new_quick_add_event};
use chrono::NaiveDate;
use uuid::Uuid;

#[test]
fn phase6_quick_add_event_base_construction() {
    // ----- Happy path: well-formed inputs build a base quick-add event
    let event_id = Uuid::parse_str("aaaa1111-aaaa-aaaa-aaaa-aaaaaaaaaaaa").unwrap();
    let calendar_id = Uuid::parse_str("bbbb2222-bbbb-bbbb-bbbb-bbbbbbbbbbbb").unwrap();
    let day = NaiveDate::from_ymd_opt(2026, 7, 20).unwrap();
    let next = NaiveDate::from_ymd_opt(2026, 7, 21).unwrap();

    let event = new_quick_add_event(event_id, calendar_id, "  Team Sync  ", day)
        .expect("well-formed quick-add input must build");

    assert_eq!(event.id, event_id, "supplied event id must be preserved");
    assert_eq!(
        event.calendar_id, calendar_id,
        "supplied calendar id must be preserved"
    );
    assert_eq!(event.title, "Team Sync", "title must be trimmed");
    assert!(event.location.is_empty(), "location must be empty");
    assert!(event.description.is_empty(), "description must be empty");
    assert!(event.recurrence.is_none(), "no recurrence for quick-add");
    assert!(event.reminders.is_empty(), "no reminders for quick-add");
    match event.schedule {
        EventSchedule::AllDay {
            start_date,
            end_date_exclusive,
        } => {
            assert_eq!(start_date, day, "all-day start must be the chosen date");
            assert_eq!(
                end_date_exclusive, next,
                "all-day end must be the next date as an exclusive bound"
            );
        }
        other => panic!("quick-add must produce an AllDay schedule, got {other:?}"),
    }

    // ----- A title that is empty after trimming is rejected
    assert!(
        new_quick_add_event(event_id, calendar_id, "", day).is_err(),
        "empty title must be rejected"
    );
    assert!(
        new_quick_add_event(event_id, calendar_id, "   \t\n  ", day).is_err(),
        "whitespace-only title must be rejected as empty after trim"
    );

    // Reference the `Event` type so the import is used even if the
    // match arm above is the only field-level check; keeps the
    // contract self-evidently about an `Event` value.
    let _shape: Event = event;
}
