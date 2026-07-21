// Public contract pinned by this acceptance test:
//
//     impl SqliteRepository {
//         pub fn provision_caldav_account(
//             &mut self,
//             account: &Account,
//             discovery: &CaldavDiscovery,
//         ) -> Result<Vec<Calendar>, RepositoryError>;
//     }
//
// Provisioning atomically upserts the password-free account, its discovered
// CalDAV calendars, and their sync state. Calendars are identified by
// account plus absolute HTTP(S) href; results are sorted by href.

use calendar::backend::caldav::{CaldavDiscovery, DiscoveredCalendar};
use calendar::backend::{
    AccountRepository, CalendarRepository, SqliteRepository, SyncStateRepository,
};
use calendar::model::{Account, CalendarSource};
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

fn account(id: &str, name: &str) -> Account {
    Account {
        id: Uuid::parse_str(id).unwrap(),
        name: name.to_string(),
        server_url: "https://dav.example.test/".to_string(),
        username: "ada".to_string(),
        enabled: true,
    }
}

fn discovery(calendars: Vec<DiscoveredCalendar>) -> CaldavDiscovery {
    CaldavDiscovery {
        principal_url: "https://dav.example.test/principals/ada/".to_string(),
        calendar_home_url: "https://dav.example.test/calendars/ada/".to_string(),
        calendars,
    }
}

#[test]
fn phase11_provisions_discovered_caldav_calendars_atomically_and_durably() {
    let db_path = unique_temp_db_path("account_provisioning");
    let _cleanup = TempDb(db_path.clone());
    let work_url = "https://dav.example.test/calendars/ada/work/";
    let personal_url = "https://dav.example.test/calendars/ada/personal/";
    let primary_account = account("ac110101-0000-0000-0000-000000000001", "Work CalDAV");
    let initial_discovery = discovery(vec![
        DiscoveredCalendar {
            href: work_url.to_string(),
            display_name: Some("Work".to_string()),
            sync_token: Some("work-v1".to_string()),
            color: Some("#A1B2C3DD".to_string()),
            writable: true,
        },
        DiscoveredCalendar {
            href: personal_url.to_string(),
            display_name: None,
            sync_token: None,
            color: Some("not-a-colour".to_string()),
            writable: false,
        },
    ]);

    let (work_id, personal_id, fallback_color) = {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        let provisioned = repo
            .provision_caldav_account(&primary_account, &initial_discovery)
            .expect("a valid discovery must provision account, calendars, and sync state");

        assert_eq!(provisioned.len(), 2);
        let remote_urls: Vec<_> = provisioned
            .iter()
            .map(|calendar| {
                repo.get_calendar_sync_state(calendar.id)
                    .expect("every provisioned calendar must have sync state")
                    .remote_url
            })
            .collect();
        assert_eq!(remote_urls, vec![personal_url, work_url]);

        let personal = &provisioned[0];
        assert!(
            !personal.name.trim().is_empty(),
            "missing display names need a usable fallback"
        );
        assert_eq!(personal.color.len(), 7);
        assert!(personal.color.starts_with('#'));
        assert!(
            personal.color[1..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
        assert!(
            personal.read_only,
            "non-writable discovery must be read-only"
        );
        assert!(personal.visible);
        assert_eq!(
            personal.source,
            CalendarSource::CalDav {
                account_id: primary_account.id
            }
        );

        let work = &provisioned[1];
        assert_eq!(work.name, "Work");
        assert_eq!(work.color, "#a1b2c3");
        assert!(!work.read_only);
        assert!(work.visible);
        assert_eq!(
            work.source,
            CalendarSource::CalDav {
                account_id: primary_account.id
            }
        );
        assert_eq!(
            repo.get_calendar_sync_state(work.id)
                .unwrap()
                .sync_token
                .as_deref(),
            Some("work-v1")
        );
        assert_eq!(
            repo.get_account(primary_account.id),
            Some(primary_account.clone())
        );

        (work.id, personal.id, personal.color.clone())
    };

    {
        let mut repo = SqliteRepository::open(&db_path).unwrap();
        assert_eq!(repo.get_calendar(work_id).unwrap().color, "#a1b2c3");
        assert_eq!(
            repo.get_calendar_sync_state(work_id).unwrap().remote_url,
            work_url
        );

        let mut personal = repo.get_calendar(personal_id).unwrap();
        personal.visible = false;
        repo.update_calendar(&personal).unwrap();
        let updated_account = Account {
            name: "Updated Work CalDAV".to_string(),
            server_url: "https://updated.example.test/dav/".to_string(),
            ..primary_account.clone()
        };
        let reprovisioned = repo
            .provision_caldav_account(
                &updated_account,
                &discovery(vec![
                    DiscoveredCalendar {
                        href: work_url.to_string(),
                        display_name: Some("Renamed Work".to_string()),
                        sync_token: Some("work-v2".to_string()),
                        color: Some("#112233".to_string()),
                        writable: false,
                    },
                    DiscoveredCalendar {
                        href: personal_url.to_string(),
                        display_name: None,
                        sync_token: Some("personal-v2".to_string()),
                        color: Some("still invalid".to_string()),
                        writable: true,
                    },
                ]),
            )
            .unwrap();

        assert_eq!(
            reprovisioned
                .iter()
                .map(|calendar| calendar.id)
                .collect::<Vec<_>>(),
            vec![personal_id, work_id]
        );
        assert_eq!(
            repo.list_calendars().len(),
            2,
            "reprovisioning must not duplicate calendars"
        );
        let personal = repo.get_calendar(personal_id).unwrap();
        assert!(
            !personal.visible,
            "local visibility must survive rediscovery"
        );
        assert_eq!(
            personal.color, fallback_color,
            "invalid colors use a stable fallback"
        );
        let work = repo.get_calendar(work_id).unwrap();
        assert_eq!(work.name, "Renamed Work");
        assert_eq!(work.color, "#112233");
        assert!(work.read_only);
        assert_eq!(
            repo.get_calendar_sync_state(work_id)
                .unwrap()
                .sync_token
                .as_deref(),
            Some("work-v2")
        );
        assert_eq!(repo.get_account(primary_account.id), Some(updated_account));

        let other_account = account("ac110102-0000-0000-0000-000000000002", "Other CalDAV");
        let other = repo
            .provision_caldav_account(
                &other_account,
                &discovery(vec![DiscoveredCalendar {
                    href: work_url.to_string(),
                    display_name: Some("Other Work".to_string()),
                    sync_token: None,
                    color: None,
                    writable: true,
                }]),
            )
            .unwrap();
        assert_ne!(
            other[0].id, work_id,
            "remote URLs are only unique within an account"
        );
        assert_eq!(repo.list_calendars().len(), 3);
    }

    let rollback_path = unique_temp_db_path("account_provisioning_rollback");
    let _rollback_cleanup = TempDb(rollback_path.clone());
    let rollback_account = account("ac110103-0000-0000-0000-000000000003", "Rollback");
    let mut repo = SqliteRepository::open(&rollback_path).unwrap();
    for invalid_discovery in [
        discovery(vec![
            DiscoveredCalendar {
                href: work_url.to_string(),
                display_name: Some("Valid".to_string()),
                sync_token: None,
                color: None,
                writable: true,
            },
            DiscoveredCalendar {
                href: work_url.to_string(),
                display_name: Some("Duplicate".to_string()),
                sync_token: None,
                color: None,
                writable: true,
            },
        ]),
        discovery(vec![
            DiscoveredCalendar {
                href: personal_url.to_string(),
                display_name: Some("Valid".to_string()),
                sync_token: None,
                color: None,
                writable: true,
            },
            DiscoveredCalendar {
                href: "ftp://dav.example.test/not-http/".to_string(),
                display_name: Some("Invalid".to_string()),
                sync_token: None,
                color: None,
                writable: true,
            },
        ]),
    ] {
        assert!(
            repo.provision_caldav_account(&rollback_account, &invalid_discovery)
                .is_err()
        );
        assert!(repo.get_account(rollback_account.id).is_none());
        assert!(repo.list_calendars().is_empty());
    }
}
