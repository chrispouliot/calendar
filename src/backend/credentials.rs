use std::{fmt, path::Path};

use oo7::{Keyring, Secret};
use uuid::Uuid;

const APPLICATION: &str = "dev.chris.calendar";

/// A redacted error from the credential store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialError;

impl fmt::Display for CredentialError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("credential store operation failed")
    }
}

impl std::error::Error for CredentialError {}

/// Secure storage for CalDAV account credentials.
pub struct CredentialStore {
    keyring: Keyring,
}

impl CredentialStore {
    /// Open the platform credential backend, selecting the host or sandboxed
    /// implementation as appropriate.
    pub async fn system() -> Result<Self, CredentialError> {
        Ok(Self {
            keyring: Keyring::new().await.map_err(|_| CredentialError)?,
        })
    }

    /// Open or create an encrypted file-backed credential store.
    pub async fn open_encrypted_file(
        path: impl AsRef<Path>,
        encryption_secret: Secret,
    ) -> Result<Self, CredentialError> {
        Ok(Self {
            keyring: Keyring::sandboxed_with_path(path, encryption_secret)
                .await
                .map_err(|_| CredentialError)?,
        })
    }

    pub async fn store(&mut self, account: Uuid, password: Secret) -> Result<(), CredentialError> {
        self.keyring
            .create_item(
                "CalDAV account credential",
                &attributes(account),
                password,
                true,
            )
            .await
            .map(|_| ())
            .map_err(|_| CredentialError)
    }

    pub async fn lookup(&self, account: Uuid) -> Result<Option<Secret>, CredentialError> {
        let item = self
            .keyring
            .search_items(&attributes(account))
            .await
            .map_err(|_| CredentialError)?
            .into_iter()
            .next();
        match item {
            Some(item) => item.secret().await.map(Some).map_err(|_| CredentialError),
            None => Ok(None),
        }
    }

    pub async fn delete(&mut self, account: Uuid) -> Result<(), CredentialError> {
        self.keyring
            .delete(&attributes(account))
            .await
            .map_err(|_| CredentialError)
    }
}

fn attributes(account: Uuid) -> [(String, String); 2] {
    [
        ("app".to_owned(), APPLICATION.to_owned()),
        ("account".to_owned(), account.to_string()),
    ]
}
