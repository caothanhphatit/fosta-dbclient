use keyring::Entry;

use crate::error::AppResult;

const SERVICE: &str = "db-studio";
pub const EMBEDDING_ACCOUNT: &str = "embedding-api-key";
/// Where the single assistant key lived before there were several LLM connections.
const LEGACY_AI_ACCOUNT: &str = "ai-api-key";
/// Id given to the connection migrated from the single legacy assistant setting.
pub const LEGACY_AI_CONNECTION: &str = "default";

pub fn save_password(account: &str, password: &str) -> AppResult<()> {
    Entry::new(SERVICE, account)?.set_password(password)?;
    Ok(())
}

pub fn load_password(account: &str) -> AppResult<Option<String>> {
    match Entry::new(SERVICE, account)?.get_password() {
        Ok(password) => Ok(Some(password)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub fn forget_password(account: &str) -> AppResult<()> {
    match Entry::new(SERVICE, account)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(error.into()),
    }
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
