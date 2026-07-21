// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         /// Insert `defaults` exactly once for this database. The returned
//         /// value is true only when this call performed that initialization.
//         /// Once initialization has completed, later calls do not add,
//         /// replace, or restore calendar rows, including after reopening.
//         pub fn seed_default_calendars(
//             &mut self,
//             defaults: &[Calendar],
//         ) -> Result<bool, RepositoryError>;
//     }

use calendar::backend::{CalendarRepository, SqliteRepository};
use calendar::model::{Calendar, CalendarSource};
use std::path::PathBuf;
use uuid::Uuid;

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

struct TempDb(PathBuf);

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(format!("{}-wal", self.0.display()));
        let _ = std::fs::remove_file(format!("{}-shm", self.0.display()));
    }
}

#[test]
fn phase7_sqlite_default_calendar_initialization_is_durable_and_non_destructive() {
    let db_path = unique_temp_db_path("default_seed");
    let _cleanup = TempDb(db_path.clone());

    let personal = Calendar {
        id: Uuid::parse_str("ca1e0001-0000-0000-0000-000000000001").unwrap(),
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let work = Calendar {
        id: Uuid::parse_str("ca1e0002-0000-0000-0000-000000000002").unwrap(),
        name: "Work".to_string(),
        color: "#cc3333".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let defaults = [personal.clone(), work.clone()];

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("opening a fresh sqlite database must succeed");

        assert!(
            repo.seed_default_calendars(&defaults)
                .expect("initial default seeding must succeed"),
            "a fresh database must report that defaults were seeded"
        );
        assert_eq!(
            repo.list_calendars().len(),
            defaults.len(),
            "the first seed must create every default calendar"
        );
        for calendar in &defaults {
            assert_eq!(
                repo.get_calendar(calendar.id),
                Some(calendar.clone()),
                "the first seed must persist each default exactly"
            );
        }

        assert!(
            !repo
                .seed_default_calendars(&defaults)
                .expect("repeated default seeding must succeed"),
            "a completed initialization must report a no-op"
        );
        assert_eq!(
            repo.list_calendars().len(),
            defaults.len(),
            "a repeated seed must not duplicate defaults"
        );

        let renamed_personal = Calendar {
            name: "Personal (renamed)".to_string(),
            ..personal.clone()
        };
        repo.save_calendar(&renamed_personal)
            .expect("editing a seeded calendar must succeed");
        assert!(
            !repo
                .seed_default_calendars(&defaults)
                .expect("seeding after an edit must succeed"),
            "a completed initialization must remain a no-op after an edit"
        );
        assert_eq!(
            repo.get_calendar(personal.id),
            Some(renamed_personal),
            "a later seed must not overwrite an edited default"
        );

        assert!(
            repo.delete_calendar(work.id),
            "deleting a seeded default must succeed"
        );
    }

    let mut repo =
        SqliteRepository::open(&db_path).expect("reopening the sqlite database must succeed");
    assert!(
        !repo
            .seed_default_calendars(&defaults)
            .expect("seeding after reopen must succeed"),
        "completed initialization must remain durable after reopening"
    );
    assert!(
        repo.get_calendar(work.id).is_none(),
        "a deleted default must not be resurrected after reopening"
    );
    assert_eq!(
        repo.list_calendars().len(),
        1,
        "the deleted default must remain absent after the durable no-op"
    );
}
