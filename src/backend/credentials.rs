use std::{
    fmt,
    path::Path,
    sync::mpsc::{self, Receiver},
};

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

/// Store a credential without making the GTK thread wait for the keyring.
pub fn store_on_worker(account: Uuid, password: Secret) -> Receiver<Result<(), CredentialError>> {
    credential_worker("credential-store", move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| CredentialError)?;
        runtime.block_on(async move {
            let mut store = CredentialStore::system().await?;
            store.store(account, password).await
        })
    })
}

/// Best-effort cleanup for a credential whose account could not be provisioned.
pub fn delete_on_worker(account: Uuid) -> Receiver<Result<(), CredentialError>> {
    credential_worker("credential-delete", move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| CredentialError)?;
        runtime.block_on(async move {
            let mut store = CredentialStore::system().await?;
            store.delete(account).await
        })
    })
}

/// Look up an account password without making the GTK thread wait for the
/// system credential store.
pub fn lookup_on_worker(account: Uuid) -> Receiver<Result<Option<Secret>, CredentialError>> {
    credential_worker("credential-lookup", move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| CredentialError)?;
        runtime.block_on(async move {
            let store = CredentialStore::system().await?;
            store.lookup(account).await
        })
    })
}

fn credential_worker<T, F>(name: &str, operation: F) -> Receiver<Result<T, CredentialError>>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CredentialError> + Send + 'static,
{
    let (sender, receiver) = mpsc::sync_channel(1);
    let worker = move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation))
            .unwrap_or(Err(CredentialError));
        let _ = sender.send(result);
    };
    if std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(worker)
        .is_err()
    {
        let _ = receiver;
    }
    receiver
}

fn attributes(account: Uuid) -> [(String, String); 2] {
    [
        ("app".to_owned(), APPLICATION.to_owned()),
        ("account".to_owned(), account.to_string()),
    ]
}
