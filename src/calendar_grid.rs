// Pure month-grid logic: deterministic, no clock/locale reads.
//
// Exposes a fixed six-week Monday-first grid for the requested
// displayed year/month.  The public contract is pinned by
// `tests/month_grid.rs`.

#[derive(Debug, PartialEq, Clone, Copy)]
pub struct MonthCell {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub in_displayed_month: bool,
}

/// Returns a fixed six-week (42-cell) grid for the requested displayed
/// year/month.  Weeks start on Monday; `in_displayed_month` is true only
/// for cells whose (year, month) match the display month.
pub fn month_grid(year: i32, month: u32) -> [MonthCell; 42] {
    // Tomohiko Sakamoto's day-of-week: 0 = Sunday … 6 = Saturday.
    fn sakamoto_dow(y: i32, m: u32, d: u32) -> u32 {
        let (adj_y, adj_m) = if m <= 2 { (y - 1, m + 12) } else { (y, m) };
        let y = adj_y as usize;
        let m = adj_m as usize;
        let d = d as usize;
        ((y + y / 4 - y / 100 + y / 400 + (13 * m + 8) / 5 + d) % 7) as u32
    }

    // Convert to Monday-first: 0 = Monday … 6 = Sunday.
    let mon0 = |y: i32, m: u32, d: u32| (sakamoto_dow(y, m, d) + 6) % 7;

    let is_leap = |y: i32| (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;

    let days_in_month = |y: i32, m: u32| -> u32 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap(y) {
                    29
                } else {
                    28
                }
            }
            _ => 0,
        }
    };

    let prev_month =
        |y: i32, m: u32| -> (i32, u32) { if m == 1 { (y - 1, 12) } else { (y, m - 1) } };

    let next_month =
        |y: i32, m: u32| -> (i32, u32) { if m == 12 { (y + 1, 1) } else { (y, m + 1) } };

    // Offset from the 1st of the display month to the Monday that starts
    // the week containing the 1st.
    let first_dow_monday = mon0(year, month, 1); // 0 = Mon … 6 = Sun
    let start_offset = 1i32 - first_dow_monday as i32;

    std::array::from_fn(|i: usize| {
        let mut day_offset = start_offset + i as i32;
        let mut cell_year = year;
        let mut cell_month = month;

        // Clamp backwards into the previous month if needed.
        if day_offset < 1 {
            let (py, pm) = prev_month(cell_year, cell_month);
            cell_year = py;
            cell_month = pm;
            day_offset += days_in_month(cell_year, cell_month) as i32;
        }

        let dim = days_in_month(cell_year, cell_month) as i32;

        // Clamp forward into the next month if needed.
        if day_offset > dim {
            day_offset -= dim;
            let (ny, nm) = next_month(cell_year, cell_month);
            cell_year = ny;
            cell_month = nm;
        }

        let in_displayed = cell_year == year && cell_month == month;

        MonthCell {
            year: cell_year,
            month: cell_month,
            day: day_offset as u32,
            in_displayed_month: in_displayed,
        }
    })
}
