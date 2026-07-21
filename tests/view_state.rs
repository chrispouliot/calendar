// Public contract pinned by this acceptance test:
//
//     pub mod view_state {
//         use chrono::NaiveDate;
//
//         #[derive(Debug, Clone, Copy, PartialEq, Eq)]
//         pub enum ViewKind { Month, Week, Agenda }
//
//         /// Pure shared navigation state. Construction and `set_today`
//         /// receive dates from the caller and never read the clock.
//         pub struct ViewState { /* private fields */ }
//
//         impl ViewState {
//             pub fn new(view: ViewKind, active_date: NaiveDate) -> Self;
//             pub fn view(&self) -> ViewKind;
//             pub fn set_view(&mut self, view: ViewKind);
//             pub fn active_date(&self) -> NaiveDate;
//             pub fn previous(&mut self);
//             pub fn next(&mut self);
//             pub fn set_today(&mut self, today: NaiveDate);
//             pub fn current_week_dates(&self) -> [NaiveDate; 7];
//         }
//     }
//
// This test uses only fixed chrono dates. It is deterministic and has no GTK,
// filesystem, locale, or clock dependency; every ViewState is local to it.

use calendar::view_state::{ViewKind, ViewState};
use chrono::NaiveDate;

fn date(year: i32, month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(year, month, day).expect("test fixture must be a valid date")
}

#[test]
fn phase8_shared_active_date_navigation_state() {
    let active = date(2026, 12, 30);
    let mut state = ViewState::new(ViewKind::Month, active);

    // Changing presentation never changes the shared active date.
    assert_eq!(state.view(), ViewKind::Month);
    for view in [ViewKind::Week, ViewKind::Agenda, ViewKind::Month] {
        state.set_view(view);
        assert_eq!(state.view(), view);
        assert_eq!(
            state.active_date(),
            active,
            "switching to {view:?} lost active date"
        );
    }

    // Week and agenda navigation advance in whole weeks, including year rollover.
    state.set_view(ViewKind::Week);
    state.next();
    assert_eq!(state.active_date(), date(2027, 1, 6));
    state.previous();
    assert_eq!(state.active_date(), active);

    state.set_view(ViewKind::Agenda);
    state.next();
    assert_eq!(state.active_date(), date(2027, 1, 6));
    state.previous();
    assert_eq!(state.active_date(), active);

    // Month navigation changes the calendar month and clamps an invalid day.
    let mut month_state = ViewState::new(ViewKind::Month, date(2027, 1, 31));
    month_state.next();
    assert_eq!(month_state.active_date(), date(2027, 2, 28));
    let mut previous_month_state = ViewState::new(ViewKind::Month, date(2027, 3, 31));
    previous_month_state.previous();
    assert_eq!(previous_month_state.active_date(), date(2027, 2, 28));

    // Today is supplied by the caller, rather than obtained from a clock.
    state.set_today(date(2024, 2, 29));
    assert_eq!(state.active_date(), date(2024, 2, 29));

    // The current week always spans Monday through Sunday around active_date.
    let week = state.current_week_dates();
    assert_eq!(
        week,
        [
            date(2024, 2, 26),
            date(2024, 2, 27),
            date(2024, 2, 28),
            date(2024, 2, 29),
            date(2024, 3, 1),
            date(2024, 3, 2),
            date(2024, 3, 3),
        ]
    );
}
