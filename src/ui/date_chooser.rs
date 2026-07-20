use adw::prelude::*;
use adw::subclass::prelude::*;
use calendar::calendar_grid::{MonthCell, month_grid};
use gtk::glib;
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(gtk::CompositeTemplate, Default)]
    #[template(resource = "/dev/chris/calendar/ui/common/date-chooser.ui")]
    pub struct DateChooser {
        #[template_child]
        pub month_label: TemplateChild<gtk::Label>,
        #[template_child]
        pub prev_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub headers_grid: TemplateChild<gtk::Grid>,
        #[template_child]
        pub day_grid: TemplateChild<gtk::Grid>,

        // Display month/year shown in the grid.
        pub display_year: Cell<i32>,
        pub display_month: Cell<u32>,

        // Currently selected date.
        pub selected_year: Cell<i32>,
        pub selected_month: Cell<u32>,
        pub selected_day: Cell<u32>,

        // Today's date (read once at construction).
        pub today_year: Cell<i32>,
        pub today_month: Cell<u32>,
        pub today_day: Cell<u32>,

        // Grid cells for the current display month.
        pub grid_data: RefCell<Vec<MonthCell>>,

        // Day buttons created once at construction.
        pub day_buttons: RefCell<Vec<gtk::Button>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DateChooser {
        const NAME: &'static str = "DateChooser";
        type Type = super::DateChooser;
        type ParentType = adw::Bin;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DateChooser {
        fn constructed(&self) {
            self.parent_constructed();

            // Weekday header labels.
            let day_names = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
            for (i, name) in day_names.iter().enumerate() {
                let label = gtk::Label::builder()
                    .label(*name)
                    .halign(gtk::Align::Center)
                    .css_classes(["weekday"])
                    .build();
                self.headers_grid.attach(&label, i as i32, 0, 1, 1);
            }

            // Create 42 day buttons with click handlers that use their grid index.
            let obj = self.obj();
            let buttons: Vec<gtk::Button> = (0..42)
                .map(|idx| {
                    let btn = gtk::Button::builder()
                        .css_classes(["circular", "flat"])
                        .halign(gtk::Align::Center)
                        .valign(gtk::Align::Center)
                        .hexpand(true)
                        .vexpand(true)
                        .build();

                    let obj_clone = obj.clone();
                    btn.connect_clicked(move |_| {
                        let imp = obj_clone.imp();
                        let cells = imp.grid_data.borrow();
                        if let Some(&cell) = cells.get(idx) {
                            drop(cells);
                            imp.selected_year.set(cell.year);
                            imp.selected_month.set(cell.month);
                            imp.selected_day.set(cell.day);

                            // Navigate to the cell's month.
                            imp.display_year.set(cell.year);
                            imp.display_month.set(cell.month);

                            imp.populate_grid();
                        }
                    });

                    btn
                })
                .collect();

            for (i, btn) in buttons.iter().enumerate() {
                let row = (i / 7) as i32;
                let col = (i % 7) as i32;
                self.day_grid.attach(btn, col, row, 1, 1);
            }
            *self.day_buttons.borrow_mut() = buttons;

            // Wire up month navigation.
            let obj_prev = obj.clone();
            self.prev_button.connect_clicked(move |_| {
                obj_prev.previous_month();
            });

            let obj_next = obj.clone();
            self.next_button.connect_clicked(move |_| {
                obj_next.next_month();
            });

            // Initialise date state from the local clock.
            let now = glib::DateTime::now_local().unwrap();
            let (ty, tm, td) = (now.year(), now.month() as u32, now.day_of_month() as u32);
            self.today_year.set(ty);
            self.today_month.set(tm);
            self.today_day.set(td);
            self.display_year.set(ty);
            self.display_month.set(tm);
            self.selected_year.set(ty);
            self.selected_month.set(tm);
            self.selected_day.set(td);

            self.populate_grid();
        }
    }

    impl WidgetImpl for DateChooser {}

    impl BinImpl for DateChooser {}
}

glib::wrapper! {
    pub struct DateChooser(ObjectSubclass<imp::DateChooser>)
        @extends adw::Bin, gtk::Widget,
        @implements gtk::Buildable, gtk::ConstraintTarget;
}

impl DateChooser {
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn previous_month(&self) {
        let imp = self.imp();
        let (y, m) = if imp.display_month.get() == 1 {
            (imp.display_year.get() - 1, 12)
        } else {
            (imp.display_year.get(), imp.display_month.get() - 1)
        };
        imp.display_year.set(y);
        imp.display_month.set(m);
        imp.populate_grid();
    }

    fn next_month(&self) {
        let imp = self.imp();
        let (y, m) = if imp.display_month.get() == 12 {
            (imp.display_year.get() + 1, 1)
        } else {
            (imp.display_year.get(), imp.display_month.get() + 1)
        };
        imp.display_year.set(y);
        imp.display_month.set(m);
        imp.populate_grid();
    }
}

/// Private helpers on the implementation struct.
impl imp::DateChooser {
    fn populate_grid(&self) {
        let (dy, dm) = (self.display_year.get(), self.display_month.get());
        let (ty, tm, td) = (
            self.today_year.get(),
            self.today_month.get(),
            self.today_day.get(),
        );
        let (sy, sm, sd) = (
            self.selected_year.get(),
            self.selected_month.get(),
            self.selected_day.get(),
        );

        // Update the heading label.
        let month_name = match dm {
            1 => "January",
            2 => "February",
            3 => "March",
            4 => "April",
            5 => "May",
            6 => "June",
            7 => "July",
            8 => "August",
            9 => "September",
            10 => "October",
            11 => "November",
            12 => "December",
            _ => unreachable!(),
        };
        self.month_label
            .set_label(&format!("{} {}", month_name, dy));

        // Generate grid and update buttons.
        let grid = month_grid(dy, dm);
        *self.grid_data.borrow_mut() = grid.to_vec();

        for (i, cell) in grid.iter().enumerate() {
            if let Some(btn) = self.day_buttons.borrow().get(i) {
                btn.set_label(&cell.day.to_string());

                let mut classes = vec!["circular", "flat"];
                if !cell.in_displayed_month {
                    classes.push("other-month");
                }
                if cell.year == ty && cell.month == tm && cell.day == td {
                    classes.push("today");
                }
                if cell.year == sy && cell.month == sm && cell.day == sd {
                    classes.push("selected");
                }
                btn.set_css_classes(&classes);
            }
        }
    }
}
