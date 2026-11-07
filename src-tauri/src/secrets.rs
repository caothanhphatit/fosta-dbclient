use keyring::Entry;

use crate::error::AppResult;

const SERVICE: &str = "db-studio";
pub const EMBEDDING_ACCOUNT: &str = "embedding-api-key";

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
