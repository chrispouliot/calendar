use chrono::{Datelike, Duration, NaiveDate};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewKind {
    Month,
    Week,
    Agenda,
}

/// Pure shared navigation state for the calendar views.
pub struct ViewState {
    view: ViewKind,
    active_date: NaiveDate,
}

impl ViewState {
    pub fn new(view: ViewKind, active_date: NaiveDate) -> Self {
        Self { view, active_date }
    }

    pub fn view(&self) -> ViewKind {
        self.view
    }

    pub fn set_view(&mut self, view: ViewKind) {
        self.view = view;
    }

    pub fn active_date(&self) -> NaiveDate {
        self.active_date
    }

    pub fn previous(&mut self) {
        self.navigate(-1);
    }

    pub fn next(&mut self) {
        self.navigate(1);
    }

    pub fn set_today(&mut self, today: NaiveDate) {
        self.active_date = today;
    }

    pub fn current_week_dates(&self) -> [NaiveDate; 7] {
        let monday = self.active_date
            - Duration::days(i64::from(self.active_date.weekday().num_days_from_monday()));
        std::array::from_fn(|day| monday + Duration::days(day as i64))
    }

    fn navigate(&mut self, direction: i64) {
        match self.view {
            ViewKind::Month => self.active_date = shift_month(self.active_date, direction),
            ViewKind::Week | ViewKind::Agenda => {
                self.active_date += Duration::days(direction * 7);
            }
        }
    }
}

fn shift_month(date: NaiveDate, direction: i64) -> NaiveDate {
    let month_index = i64::from(date.year()) * 12 + i64::from(date.month0()) + direction;
    let year = month_index.div_euclid(12);
    let month = month_index.rem_euclid(12) as u32 + 1;
    let year = i32::try_from(year).expect("month navigation exceeded chrono's year range");
    let day = date.day().min(days_in_month(year, month));

    NaiveDate::from_ymd_opt(year, month, day).expect("month navigation produced an invalid date")
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let first_of_next_month = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("month navigation exceeded chrono's year range");

    (first_of_next_month - Duration::days(1)).day()
}
