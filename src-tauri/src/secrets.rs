//! Saved passwords and API keys, behind a [`SecretStore`]: Windows Credential Manager on the desktop
//! ([`KeyringStore`]), an encrypted file in the data directory on the web server ([`file::FileStore`]).
//! The account names below are the same in both.

use std::sync::OnceLock;

use crate::error::{AppError, AppResult};

#[cfg(feature = "web")]
pub mod file;

pub const EMBEDDING_ACCOUNT: &str = "embedding-api-key";
/// Where the single assistant key lived before there were several LLM connections.
const LEGACY_AI_ACCOUNT: &str = "ai-api-key";
/// Id given to the connection migrated from the single legacy assistant setting.
pub const LEGACY_AI_CONNECTION: &str = "default";

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> AppResult<Option<String>>;
    fn set(&self, account: &str, secret: &str) -> AppResult<()>;
    /// Removing a missing secret is not an error.
    fn delete(&self, account: &str) -> AppResult<()>;
}

static STORE: OnceLock<Box<dyn SecretStore>> = OnceLock::new();

/// Chooses the store for this process; call once at startup, before any command runs. The desktop app
/// does not need to: it falls back to Credential Manager.
pub fn install(store: Box<dyn SecretStore>) -> AppResult<()> {
    STORE.set(store).map_err(|_| AppError::new("The secret store is already set up"))
}

fn store() -> AppResult<&'static dyn SecretStore> {
    #[cfg(feature = "desktop")]
    let store = STORE.get_or_init(|| Box::new(KeyringStore));
    #[cfg(not(feature = "desktop"))]
    let store = STORE
        .get()
        .ok_or_else(|| AppError::new("No secret store is set up, so passwords cannot be saved"))?;
    Ok(store.as_ref())
}

pub fn save_password(account: &str, password: &str) -> AppResult<()> {
    store()?.set(account, password)
}

pub fn load_password(account: &str) -> AppResult<Option<String>> {
    store()?.get(account)
}

pub fn forget_password(account: &str) -> AppResult<()> {
    store()?.delete(account)
}

fn ai_account(connection_id: &str) -> String {
    format!("ai-key-{connection_id}")
}

pub fn save_ai_key(connection_id: &str, key: &str) -> AppResult<()> {
    save_password(&ai_account(connection_id), key)
}

/// The key of an LLM connection; the migrated one falls back to the legacy account.
pub fn load_ai_key(connection_id: &str) -> AppResult<Option<String>> {
    match load_password(&ai_account(connection_id))? {
        None if connection_id == LEGACY_AI_CONNECTION => load_password(LEGACY_AI_ACCOUNT),
        found => Ok(found),
    }
}

pub fn forget_ai_key(connection_id: &str) -> AppResult<()> {
    forget_password(&ai_account(connection_id))?;
    if connection_id == LEGACY_AI_CONNECTION {
        forget_password(LEGACY_AI_ACCOUNT)?;
    }
    Ok(())
}

/// Windows Credential Manager (the OS keychain elsewhere), service `fosta-studio`.
#[cfg(feature = "desktop")]
pub struct KeyringStore;

#[cfg(feature = "desktop")]
mod keyring_store {
    use keyring::Entry;

    use super::{KeyringStore, SecretStore};
    use crate::error::AppResult;

    const SERVICE: &str = "fosta-studio";
    /// The Credential Manager service before the rename to Fosta Studio. Secrets found there are moved on first read.
    const LEGACY_SERVICE: &str = "db-studio";

    impl SecretStore for KeyringStore {
        fn get(&self, account: &str) -> AppResult<Option<String>> {
            if let Some(password) = read(SERVICE, account)? {
                return Ok(Some(password));
            }
            // Saved before the rename: move it under the new service name so the old entry disappears.
            let Some(password) = read(LEGACY_SERVICE, account)? else {
                return Ok(None);
            };
            self.set(account, &password)?;
            delete(LEGACY_SERVICE, account)?;
            Ok(Some(password))
        }

        fn set(&self, account: &str, secret: &str) -> AppResult<()> {
            Entry::new(SERVICE, account)?.set_password(secret)?;
            Ok(())
        }

        fn delete(&self, account: &str) -> AppResult<()> {
            delete(SERVICE, account)?;
            delete(LEGACY_SERVICE, account)
        }
    }

    fn read(service: &str, account: &str) -> AppResult<Option<String>> {
        match Entry::new(service, account)?.get_password() {
            Ok(password) => Ok(Some(password)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn delete(service: &str, account: &str) -> AppResult<()> {
        match Entry::new(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}
