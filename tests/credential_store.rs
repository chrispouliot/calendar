// Public contract pinned by this acceptance test:
//
//     impl CredentialStore {
//         pub async fn system() -> Result<Self, CredentialError>;
//         pub async fn open_encrypted_file(
//             path: impl AsRef<Path>,
//             encryption_secret: oo7::Secret,
//         ) -> Result<Self, CredentialError>;
//         pub async fn store(
//             &mut self,
//             account: Uuid,
//             password: oo7::Secret,
//         ) -> Result<(), CredentialError>;
//         pub async fn lookup(
//             &self,
//             account: Uuid,
//         ) -> Result<Option<oo7::Secret>, CredentialError>;
//         pub async fn delete(&mut self, account: Uuid) -> Result<(), CredentialError>;
//     }
//
// The encrypted-file adapter is the sandboxed, no-D-Bus implementation. It
// scopes each item to `dev.chris.calendar` and its account UUID.

use calendar::backend::credentials::CredentialStore;
use oo7::Secret;
use std::path::PathBuf;
use uuid::Uuid;

fn unique_temp_dir() -> PathBuf {
    let mut path = std::env::temp_dir();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    path.push(format!(
        "calendar_credential_store_{}_{}",
        std::process::id(),
        nanos
    ));
    std::fs::create_dir(&path).expect("unique credential-store temp directory");
    path
}

struct TempDir(PathBuf);

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn encrypted_file_credentials_are_account_scoped_durable_and_redacted() {
    let temp_dir = unique_temp_dir();
    let _cleanup = TempDir(temp_dir.clone());
    let backing_file = temp_dir.join("credentials.keyring");
    let primary = Uuid::parse_str("ce000001-0000-0000-0000-000000000001").unwrap();
    let secondary = Uuid::parse_str("ce000002-0000-0000-0000-000000000002").unwrap();
    let first_password = "credential-first-password-not-on-disk";
    let replacement_password = "credential-replacement-password-not-on-disk";
    let secondary_password = "credential-secondary-password-not-on-disk";

    let mut store = CredentialStore::open_encrypted_file(
        &backing_file,
        Secret::text("deterministic-test-encryption-secret"),
    )
    .await
    .expect("sandboxed encrypted credential store opens without D-Bus");

    store
        .store(primary, Secret::text(first_password))
        .await
        .expect("stores a primary account secret");
    let initial: Secret = store
        .lookup(primary)
        .await
        .expect("looks up the primary account")
        .expect("primary secret exists");
    assert_eq!(initial, Secret::text(first_password));

    store
        .store(primary, Secret::text(replacement_password))
        .await
        .expect("replaces an existing account secret");
    store
        .store(secondary, Secret::text(secondary_password))
        .await
        .expect("stores an independent account secret");
    assert_eq!(
        store.lookup(primary).await.unwrap(),
        Some(Secret::text(replacement_password))
    );
    assert_eq!(
        store.lookup(secondary).await.unwrap(),
        Some(Secret::text(secondary_password))
    );

    let backing_bytes = std::fs::read(&backing_file).expect("encrypted backing file exists");
    for plaintext in [first_password, replacement_password, secondary_password] {
        assert!(
            !backing_bytes
                .windows(plaintext.len())
                .any(|window| window == plaintext.as_bytes()),
            "encrypted backing file must not contain password plaintext"
        );
    }
    assert!(
        !format!("{initial:?}").contains(first_password),
        "returned oo7 secrets redact their value in Debug output"
    );

    store
        .delete(primary)
        .await
        .expect("deletes the primary account secret");
    store
        .delete(primary)
        .await
        .expect("deleting a missing account is idempotent");
    assert_eq!(store.lookup(primary).await.unwrap(), None);
    assert_eq!(
        store.lookup(secondary).await.unwrap(),
        Some(Secret::text(secondary_password))
    );
    drop(store);

    let reopened = CredentialStore::open_encrypted_file(
        &backing_file,
        Secret::text("deterministic-test-encryption-secret"),
    )
    .await
    .expect("reopens the same encrypted credential file");
    assert_eq!(reopened.lookup(primary).await.unwrap(), None);
    assert_eq!(
        reopened.lookup(secondary).await.unwrap(),
        Some(Secret::text(secondary_password)),
        "other account credentials survive reopening"
    );
}
