use calendar::backend::{AccountRepository, CalendarRepository, SqliteRepository};
use calendar::model::{Account, Calendar, CalendarSource};
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_db_path(label: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    path.push(format!("calendar_phase11_{label}_{pid}_{nanos}.sqlite"));
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
fn phase11_account_and_remote_calendar_lifecycle_persists() {
    let db_path = unique_temp_db_path("account_source");
    let _cleanup = TempDb(db_path.clone());

    let account = Account {
        id: Uuid::parse_str("ac110001-0000-0000-0000-000000000001").unwrap(),
        name: "Work CalDAV".to_string(),
        server_url: "https://caldav.example.test/dav/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    };
    let updated_account = Account {
        name: "Work Calendar".to_string(),
        server_url: "https://calendar.example.test/caldav/".to_string(),
        username: "ada.lovelace".to_string(),
        enabled: false,
        ..account.clone()
    };
    let unknown_account = Account {
        id: Uuid::parse_str("ac110002-0000-0000-0000-000000000002").unwrap(),
        ..updated_account.clone()
    };
    let local_calendar = Calendar {
        id: Uuid::parse_str("ca110001-0000-0000-0000-000000000001").unwrap(),
        name: "Personal".to_string(),
        color: "#3366cc".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::Local,
    };
    let remote_calendar = Calendar {
        id: Uuid::parse_str("ca110002-0000-0000-0000-000000000002").unwrap(),
        name: "Work".to_string(),
        color: "#d946ef".to_string(),
        visible: true,
        read_only: false,
        source: CalendarSource::CalDav {
            account_id: account.id,
        },
    };

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("opening a fresh sqlite database must succeed");
        repo.save_account(&account)
            .expect("saving the account configuration must succeed");
        repo.save_calendar(&local_calendar)
            .expect("saving the local calendar must succeed");
        repo.save_calendar(&remote_calendar)
            .expect("saving the remote calendar must succeed");

        assert_eq!(repo.get_account(account.id), Some(account.clone()));
        assert_eq!(repo.list_accounts(), vec![account.clone()]);
        repo.update_account(&updated_account)
            .expect("updating a saved account must succeed");
        assert!(
            repo.update_account(&unknown_account).is_err(),
            "updating an unknown account must fail rather than insert it"
        );
        assert!(repo.get_account(unknown_account.id).is_none());
        assert!(
            !repo.delete_account(unknown_account.id),
            "deleting an unknown account must report failure"
        );
    }

    {
        let mut repo =
            SqliteRepository::open(&db_path).expect("reopening the sqlite database must succeed");
        assert_eq!(repo.get_account(account.id), Some(updated_account.clone()));
        assert_eq!(repo.list_accounts(), vec![updated_account.clone()]);
        assert_eq!(
            repo.get_calendar(local_calendar.id),
            Some(local_calendar.clone())
        );
        assert_eq!(
            repo.get_calendar(remote_calendar.id),
            Some(remote_calendar.clone()),
            "the remote calendar must retain its exact CalDAV account association"
        );

        assert!(
            repo.delete_calendar(remote_calendar.id),
            "deleting the saved remote calendar must report success"
        );
        assert!(
            repo.delete_account(account.id),
            "deleting the account after its remote calendar must report success"
        );
    }

    let repo = SqliteRepository::open(&db_path).expect("reopening after deletion must succeed");
    assert!(repo.get_calendar(remote_calendar.id).is_none());
    assert!(repo.get_account(account.id).is_none());
    assert_eq!(repo.list_accounts(), Vec::<Account>::new());
    assert_eq!(repo.get_calendar(local_calendar.id), Some(local_calendar));
}
