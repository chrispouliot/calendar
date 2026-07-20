// Public contract pinned by this acceptance test:
//
//     pub mod calendar_grid {
//         #[derive(Debug, PartialEq, ...)]
//         pub struct MonthCell {
//             pub year: i32,
//             pub month: u32,
//             pub day: u32,
//             pub in_displayed_month: bool,
//         }
//
//         /// Fixed six-week (42-cell) grid for the requested displayed
//         /// year/month, Monday-first. `in_displayed_month` is true only
//         /// for cells whose (year, month) match the display.
//         pub fn month_grid(year: i32, month: u32) -> [MonthCell; 42];
//     }
//
// The helper must be pure: deterministic and free of any clock/locale read.
// The test exercises May 2026 specifically and is fully independent of the
// real current local date.

use calendar::calendar_grid::{MonthCell, month_grid};

#[test]
fn may_2026_grid_is_monday_first_six_weeks() {
    let grid = month_grid(2026, 5);
    assert_eq!(grid.len(), 42, "expected a fixed 42-cell grid");

    // Monday-first: row 0 starts on Monday April 27, 2026.
    assert_eq!(
        grid[0],
        MonthCell {
            year: 2026,
            month: 4,
            day: 27,
            in_displayed_month: false
        },
        "row 0 must start on Monday April 27, 2026",
    );

    // May 1 2026 is a Friday. In a Monday-first grid it sits at index 4;
    // in a Sunday-first grid it would be at index 5.
    assert_eq!(
        grid[4],
        MonthCell {
            year: 2026,
            month: 5,
            day: 1,
            in_displayed_month: true
        },
        "May 1 2026 (Friday) must sit at index 4 in a Monday-first grid",
    );

    // Row 0 ends on Sunday May 3, 2026.
    assert_eq!(
        grid[6],
        MonthCell {
            year: 2026,
            month: 5,
            day: 3,
            in_displayed_month: true
        },
        "row 0 must end on Sunday May 3, 2026",
    );

    // The last in-month cell is Sunday May 31, 2026 at the end of row 4.
    assert_eq!(
        grid[34],
        MonthCell {
            year: 2026,
            month: 5,
            day: 31,
            in_displayed_month: true
        },
        "last in-month cell must be Sunday May 31, 2026",
    );

    // Spillover starts on Monday June 1, 2026 and the grid ends on
    // Sunday June 7, 2026.
    assert_eq!(
        grid[35],
        MonthCell {
            year: 2026,
            month: 6,
            day: 1,
            in_displayed_month: false
        },
        "spillover must start on Monday June 1, 2026",
    );
    assert_eq!(
        grid[41],
        MonthCell {
            year: 2026,
            month: 6,
            day: 7,
            in_displayed_month: false
        },
        "grid must end on Sunday June 7, 2026",
    );

    // in_displayed_month must agree with the cell's (year, month) matching
    // the requested display. Catches spillover mislabeling in both
    // directions across the whole grid.
    for (i, cell) in grid.iter().enumerate() {
        let in_displayed = cell.year == 2026 && cell.month == 5;
        assert_eq!(
            cell.in_displayed_month, in_displayed,
            "cell {i} has wrong in_displayed_month flag",
        );
    }
}
