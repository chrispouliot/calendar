// Library target that exposes pure calendar-grid helpers to integration tests.
//
// This file intentionally only declares the module; the actual implementation
// of `calendar_grid::month_grid` is delivered by the worker against the
// contract pinned in `tests/month_grid.rs`.

pub mod calendar_grid;
