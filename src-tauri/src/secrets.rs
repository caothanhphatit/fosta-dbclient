//! Saved passwords and API keys, behind a [`SecretStore`]: Windows Credential Manager on the desktop
//! ([`KeyringStore`]), an encrypted file in the data directory on the web server ([`file::FileStore`]).
//!
//! Every secret has a [`SecretKey`]; its account name is the same in both stores: `conn:<id>:<slot>`,
//! `ai:<id>`, `app:embedding`. Secrets saved under the older names (the bare connection id, `ai-key-<id>`,
//! `embedding-api-key`) are moved to the new name on first read, above the store, so this works for every store.

use std::sync::OnceLock;

use crate::{
    contract::SecretSlot,
    error::{AppError, AppResult},
};

#[cfg(feature = "web")]
pub mod file;

/// Pre-namespacing account of the embedding key; [`load_password`] and friends map it to [`SecretKey::Embedding`].
pub const EMBEDDING_ACCOUNT: &str = "embedding-api-key";
/// Where the single assistant key lived before there were several LLM connections.
const LEGACY_AI_ACCOUNT: &str = "ai-api-key";
/// Id given to the connection migrated from the single legacy assistant setting.
pub const LEGACY_AI_CONNECTION: &str = "default";

/// Every slot a connection can have a secret in.
pub const CONNECTION_SLOTS: [SecretSlot; 6] = [
    SecretSlot::Password,
    SecretSlot::SshPassword,
    SecretSlot::SshKeyPassphrase,
    SecretSlot::TlsKeyPassphrase,
    SecretSlot::ApiKey,
    SecretSlot::Token,
];

/// What a secret belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SecretKey {
    Connection {
        id: String,
        slot: SecretSlot,
    },
    AiKey(String),
    Embedding,
    /// The bearer token MCP clients send to the MCP server (`crate::mcp`), in both editions; never the web access token.
    McpToken,
}

impl SecretKey {
    pub fn connection(id: &str, slot: SecretSlot) -> Self {
        Self::Connection { id: id.to_owned(), slot }
    }

    /// The account name in the store.
    pub fn account(&self) -> String {
        match self {
            Self::Connection { id, slot } => format!("conn:{id}:{}", slot_name(*slot)),
            Self::AiKey(id) => format!("ai:{id}"),
            Self::Embedding => "app:embedding".to_owned(),
            Self::McpToken => "app:mcp-token".to_owned(),
        }
    }

    /// Where older versions saved this secret, in lookup order.
    fn legacy_accounts(&self) -> Vec<String> {
        match self {
            Self::Connection {
                id,
                slot: SecretSlot::Password,
            } => vec![id.clone()],
            Self::Connection { .. } => Vec::new(),
            Self::AiKey(id) if id == LEGACY_AI_CONNECTION => vec![format!("ai-key-{id}"), LEGACY_AI_ACCOUNT.to_owned()],
            Self::AiKey(id) => vec![format!("ai-key-{id}")],
            Self::Embedding => vec![EMBEDDING_ACCOUNT.to_owned()],
            Self::McpToken => Vec::new(),
        }
    }
}

fn slot_name(slot: SecretSlot) -> &'static str {
    match slot {
        SecretSlot::Password => "password",
        SecretSlot::SshPassword => "sshPassword",
        SecretSlot::SshKeyPassphrase => "sshKeyPassphrase",
        SecretSlot::TlsKeyPassphrase => "tlsKeyPassphrase",
        SecretSlot::ApiKey => "apiKey",
        SecretSlot::Token => "token",
    }
}

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

#[cfg(test)]
thread_local! {
    /// The store of the current test thread (see [`testing::memory_store`]); the global one otherwise.
    static TEST_STORE: std::cell::Cell<Option<&'static dyn SecretStore>> = const { std::cell::Cell::new(None) };
}

fn store() -> AppResult<&'static dyn SecretStore> {
    #[cfg(test)]
    if let Some(store) = TEST_STORE.with(std::cell::Cell::get) {
        return Ok(store);
    }
    #[cfg(feature = "desktop")]
    let store = STORE.get_or_init(|| Box::new(KeyringStore));
    #[cfg(not(feature = "desktop"))]
    let store = STORE
        .get()
        .ok_or_else(|| AppError::new("No secret store is set up, so passwords cannot be saved"))?;
    Ok(store.as_ref())
}

/// The secret, from its account or else from the first legacy account that has it (which is then moved).
pub fn get(key: &SecretKey) -> AppResult<Option<String>> {
    get_in(store()?, key)
}

/// Whether the secret is saved, without moving a legacy entry.
pub fn contains(key: &SecretKey) -> AppResult<bool> {
    contains_in(store()?, key)
}

pub fn set(key: &SecretKey, secret: &str) -> AppResult<()> {
    store()?.set(&key.account(), secret)
}

/// Removes the secret under its account and every legacy one.
pub fn delete(key: &SecretKey) -> AppResult<()> {
    delete_in(store()?, key)
}

/// Removes every secret of a connection.
pub fn delete_connection(id: &str) -> AppResult<()> {
    delete_connection_in(store()?, id)
}

fn get_in(store: &dyn SecretStore, key: &SecretKey) -> AppResult<Option<String>> {
    let account = key.account();
    if let Some(secret) = store.get(&account)? {
        return Ok(Some(secret));
    }
    for legacy in key.legacy_accounts() {
        if let Some(secret) = store.get(&legacy)? {
            // The legacy entry goes only after the new one is written; a failed move is retried on the next read.
            if store.set(&account, &secret).is_ok() {
                let _ = store.delete(&legacy);
            }
            return Ok(Some(secret));
        }
    }
    Ok(None)
}

fn contains_in(store: &dyn SecretStore, key: &SecretKey) -> AppResult<bool> {
    for account in std::iter::once(key.account()).chain(key.legacy_accounts()) {
        if store.get(&account)?.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn delete_in(store: &dyn SecretStore, key: &SecretKey) -> AppResult<()> {
    store.delete(&key.account())?;
    for legacy in key.legacy_accounts() {
        store.delete(&legacy)?;
    }
    Ok(())
}

fn delete_connection_in(store: &dyn SecretStore, id: &str) -> AppResult<()> {
    for slot in CONNECTION_SLOTS {
        delete_in(store, &SecretKey::connection(id, slot))?;
    }
    Ok(())
}

/// The key behind a pre-namespacing account name: [`EMBEDDING_ACCOUNT`], or else a connection id (its password).
fn account_key(account: &str) -> SecretKey {
    if account == EMBEDDING_ACCOUNT {
        SecretKey::Embedding
    } else {
        SecretKey::connection(account, SecretSlot::Password)
    }
}

/// Saves by pre-namespacing account name: a connection id (its password) or [`EMBEDDING_ACCOUNT`].
pub fn save_password(account: &str, password: &str) -> AppResult<()> {
    set(&account_key(account), password)
}

/// Reads by pre-namespacing account name; see [`save_password`].
pub fn load_password(account: &str) -> AppResult<Option<String>> {
    get(&account_key(account))
}

/// Removes by pre-namespacing account name; see [`save_password`]. A connection loses every slot.
pub fn forget_password(account: &str) -> AppResult<()> {
    match account_key(account) {
        SecretKey::Connection { id, .. } => delete_connection(&id),
        key => delete(&key),
    }
}

pub fn save_ai_key(connection_id: &str, key: &str) -> AppResult<()> {
    set(&SecretKey::AiKey(connection_id.to_owned()), key)
}

/// The key of an LLM connection.
pub fn load_ai_key(connection_id: &str) -> AppResult<Option<String>> {
    get(&SecretKey::AiKey(connection_id.to_owned()))
}

pub fn forget_ai_key(connection_id: &str) -> AppResult<()> {
    delete(&SecretKey::AiKey(connection_id.to_owned()))
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

/// An in-memory secret store for tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::{collections::BTreeMap, sync::Mutex};

    use super::{SecretStore, TEST_STORE};
    use crate::error::{AppError, AppResult};

    /// An in-memory store; `fail_set` makes every write fail.
    #[derive(Default)]
    pub(crate) struct MemoryStore {
        pub(crate) secrets: Mutex<BTreeMap<String, String>>,
        pub(crate) fail_set: bool,
    }

    impl MemoryStore {
        pub(crate) fn with(entries: &[(&str, &str)]) -> Self {
            let store = Self::default();
            for (account, secret) in entries {
                store.secrets.lock().unwrap().insert((*account).to_owned(), (*secret).to_owned());
            }
            store
        }

        pub(crate) fn accounts(&self) -> Vec<String> {
            self.secrets.lock().unwrap().keys().cloned().collect()
        }
    }

    /// Makes `secrets::get` / `set` / `delete` on this test's thread use a fresh in-memory store, holding `entries`.
    pub(crate) fn memory_store(entries: &[(&str, &str)]) -> &'static MemoryStore {
        let store: &'static MemoryStore = Box::leak(Box::new(MemoryStore::with(entries)));
        TEST_STORE.with(|cell| cell.set(Some(store)));
        store
    }

    impl SecretStore for MemoryStore {
        fn get(&self, account: &str) -> AppResult<Option<String>> {
            Ok(self.secrets.lock().unwrap().get(account).cloned())
        }

        fn set(&self, account: &str, secret: &str) -> AppResult<()> {
            if self.fail_set {
                return Err(AppError::new("store is read-only"));
            }
            self.secrets.lock().unwrap().insert(account.to_owned(), secret.to_owned());
            Ok(())
        }

        fn delete(&self, account: &str) -> AppResult<()> {
            self.secrets.lock().unwrap().remove(account);
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::MemoryStore;
    use super::*;

    #[test]
    fn account_names_are_namespaced() {
        assert_eq!(SecretKey::connection("c1", SecretSlot::Password).account(), "conn:c1:password");
        assert_eq!(
            SecretKey::connection("c1", SecretSlot::SshPassword).account(),
            "conn:c1:sshPassword"
        );
        assert_eq!(
            SecretKey::connection("c1", SecretSlot::SshKeyPassphrase).account(),
            "conn:c1:sshKeyPassphrase"
        );
        assert_eq!(
            SecretKey::connection("c1", SecretSlot::TlsKeyPassphrase).account(),
            "conn:c1:tlsKeyPassphrase"
        );
        assert_eq!(SecretKey::connection("c1", SecretSlot::ApiKey).account(), "conn:c1:apiKey");
        assert_eq!(SecretKey::connection("c1", SecretSlot::Token).account(), "conn:c1:token");
        assert_eq!(SecretKey::AiKey("x".into()).account(), "ai:x");
        assert_eq!(SecretKey::Embedding.account(), "app:embedding");
        assert_eq!(
            SecretKey::AiKey("default".into()).legacy_accounts(),
            ["ai-key-default", "ai-api-key"]
        );
        assert!(SecretKey::connection("c1", SecretSlot::Token).legacy_accounts().is_empty());
    }

    #[test]
    fn legacy_secrets_move_on_first_read() {
        let store = MemoryStore::with(&[("c1", "pw"), ("embedding-api-key", "emb"), ("ai-api-key", "sk")]);
        let password = SecretKey::connection("c1", SecretSlot::Password);
        assert_eq!(get_in(&store, &password).unwrap().as_deref(), Some("pw"));
        assert_eq!(get_in(&store, &SecretKey::Embedding).unwrap().as_deref(), Some("emb"));
        assert_eq!(get_in(&store, &SecretKey::AiKey("default".into())).unwrap().as_deref(), Some("sk"));
        assert_eq!(store.accounts(), ["ai:default", "app:embedding", "conn:c1:password"]);
        assert_eq!(
            get_in(&store, &password).unwrap().as_deref(),
            Some("pw"),
            "second read uses the new account"
        );
        assert_eq!(get_in(&store, &SecretKey::connection("c1", SecretSlot::SshPassword)).unwrap(), None);
    }

    #[test]
    fn the_new_account_wins_over_a_legacy_one() {
        let store = MemoryStore::with(&[("c1", "old"), ("conn:c1:password", "new")]);
        assert_eq!(
            get_in(&store, &SecretKey::connection("c1", SecretSlot::Password))
                .unwrap()
                .as_deref(),
            Some("new")
        );
        assert!(contains_in(&store, &SecretKey::connection("c1", SecretSlot::Password)).unwrap());
    }

    #[test]
    fn a_failed_move_keeps_the_legacy_entry() {
        let store = MemoryStore {
            fail_set: true,
            ..MemoryStore::with(&[("ai-key-x", "sk")])
        };
        assert_eq!(get_in(&store, &SecretKey::AiKey("x".into())).unwrap().as_deref(), Some("sk"));
        assert_eq!(store.accounts(), ["ai-key-x"], "never deleted before a successful write");
    }

    #[test]
    fn contains_does_not_move() {
        let store = MemoryStore::with(&[("c1", "pw")]);
        assert!(contains_in(&store, &SecretKey::connection("c1", SecretSlot::Password)).unwrap());
        assert!(!contains_in(&store, &SecretKey::connection("c2", SecretSlot::Password)).unwrap());
        assert_eq!(store.accounts(), ["c1"]);
    }

    #[test]
    fn deleting_a_connection_removes_every_slot_and_the_legacy_entry() {
        let store = MemoryStore::with(&[
            ("c1", "legacy"),
            ("conn:c1:password", "pw"),
            ("conn:c1:sshPassword", "ssh"),
            ("conn:c1:token", "t"),
            ("conn:c2:password", "other"),
        ]);
        delete_connection_in(&store, "c1").unwrap();
        assert_eq!(store.accounts(), ["conn:c2:password"]);
        delete_in(&store, &SecretKey::AiKey("missing".into())).unwrap();
    }

    /// The saved passwords of the local dev connections are readable from the OS keyring (this machine's setup).
    #[test]
    #[ignore = "reads this machine's keyring"]
    fn live_saved_passwords_are_readable() {
        for id in ["local-mariadb", "local-mssql", "local-mongo"] {
            assert!(
                contains(&SecretKey::connection(id, crate::contract::SecretSlot::Password)).expect("keyring"),
                "no saved password for {id}"
            );
        }
    }
}
