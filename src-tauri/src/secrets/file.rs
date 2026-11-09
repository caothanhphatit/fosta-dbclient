//! The web server's secret store: one file, `fosta-studio-secrets.enc` in the data directory, holding every
//! saved password and API key as a JSON map encrypted with XChaCha20-Poly1305.
//!
//! The 32-byte key comes from `FOSTA_STUDIO_SECRET_KEY` (64 hex characters) or, when that is not set, from
//! `fosta-studio-secret.key` next to the store, generated on first start with owner-only permissions.
//! Losing the key loses the saved passwords: the store cannot be decrypted without it.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use serde::{Deserialize, Serialize};

use super::SecretStore;
use crate::{
    error::{AppError, AppResult},
    lock::lock,
};

pub const STORE_FILE: &str = "fosta-studio-secrets.enc";
pub const KEY_FILE: &str = "fosta-studio-secret.key";
pub const KEY_ENV: &str = "FOSTA_STUDIO_SECRET_KEY";
const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
/// Binds the ciphertext to its purpose and format version.
const AAD: &[u8] = b"fosta-studio-secrets-v1";

#[derive(Serialize, Deserialize)]
struct Envelope {
    version: u32,
    nonce: String,
    data: String,
}

pub struct FileStore {
    path: PathBuf,
    cipher: XChaCha20Poly1305,
    secrets: Mutex<BTreeMap<String, String>>,
}

/// Where the key came from, for the startup log.
#[derive(Debug, PartialEq, Eq)]
pub enum KeySource {
    Environment,
    ExistingFile(PathBuf),
    GeneratedFile(PathBuf),
}

impl FileStore {
    /// Opens (or starts) the store in `directory`, with the key from `env_key` or the key file there.
    pub fn open(directory: &Path, env_key: Option<&str>) -> AppResult<(Self, KeySource)> {
        let (key, source) = match env_key.map(str::trim).filter(|key| !key.is_empty()) {
            Some(text) => (
                parse_key(text).map_err(|error| AppError::new(format!("{KEY_ENV}: {error}")))?,
                KeySource::Environment,
            ),
            None => load_or_create_key(&directory.join(KEY_FILE))?,
        };
        let store = Self::with_key(directory.join(STORE_FILE), &key)?;
        Ok((store, source))
    }

    fn with_key(path: PathBuf, key: &[u8; KEY_LEN]) -> AppResult<Self> {
        let cipher = XChaCha20Poly1305::new(key.into());
        let secrets = if path.exists() {
            decrypt(&cipher, &fs::read_to_string(&path)?).map_err(|error| {
                AppError::new(format!(
                    "Cannot read the saved passwords in {}: {error}. Restore the key it was written with ({KEY_ENV} or {KEY_FILE}), \
                     or delete the file to start over without saved passwords.",
                    path.display()
                ))
            })?
        } else {
            BTreeMap::new()
        };
        Ok(Self {
            path,
            cipher,
            secrets: Mutex::new(secrets),
        })
    }

    fn persist(&self, secrets: &BTreeMap<String, String>) -> AppResult<()> {
        let plain = serde_json::to_vec(secrets)?;
        let nonce = random::<NONCE_LEN>()?;
        let data = self
            .cipher
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: &plain, aad: AAD })
            .map_err(|_| AppError::new("Encrypting the secret store failed"))?;
        let envelope = Envelope {
            version: 1,
            nonce: to_hex(&nonce),
            data: to_hex(&data),
        };
        write_private(&self.path, serde_json::to_string(&envelope)?.as_bytes())
    }
}

impl SecretStore for FileStore {
    fn get(&self, account: &str) -> AppResult<Option<String>> {
        Ok(lock(&self.secrets).get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> AppResult<()> {
        let mut secrets = lock(&self.secrets);
        secrets.insert(account.to_owned(), secret.to_owned());
        self.persist(&secrets)
    }

    fn delete(&self, account: &str) -> AppResult<()> {
        let mut secrets = lock(&self.secrets);
        if secrets.remove(account).is_some() {
            self.persist(&secrets)?;
        }
        Ok(())
    }
}

fn decrypt(cipher: &XChaCha20Poly1305, text: &str) -> Result<BTreeMap<String, String>, String> {
    let envelope: Envelope = serde_json::from_str(text).map_err(|error| format!("not a secret store ({error})"))?;
    if envelope.version != 1 {
        return Err(format!("unknown format version {}", envelope.version));
    }
    let nonce = from_hex(&envelope.nonce)
        .filter(|nonce| nonce.len() == NONCE_LEN)
        .ok_or("damaged nonce")?;
    let data = from_hex(&envelope.data).ok_or("damaged data")?;
    let plain = cipher
        .decrypt(XNonce::from_slice(&nonce), Payload { msg: &data, aad: AAD })
        .map_err(|_| String::from("wrong key or damaged file"))?;
    serde_json::from_slice(&plain).map_err(|error| error.to_string())
}

fn load_or_create_key(path: &Path) -> AppResult<([u8; KEY_LEN], KeySource)> {
    if path.exists() {
        let key = parse_key(&fs::read_to_string(path)?).map_err(|error| AppError::new(format!("{}: {error}", path.display())))?;
        return Ok((key, KeySource::ExistingFile(path.to_owned())));
    }
    let key = random::<KEY_LEN>()?;
    write_private(path, format!("{}\n", to_hex(&key)).as_bytes())?;
    Ok((key, KeySource::GeneratedFile(path.to_owned())))
}

fn parse_key(text: &str) -> Result<[u8; KEY_LEN], String> {
    from_hex(text.trim())
        .and_then(|bytes| <[u8; KEY_LEN]>::try_from(bytes).ok())
        .ok_or_else(|| String::from("the key must be 64 hex characters (32 random bytes, e.g. from `openssl rand -hex 32`)"))
}

pub fn random<const N: usize>() -> AppResult<[u8; N]> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|error| AppError::new(format!("No secure random numbers: {error}")))?;
    Ok(bytes)
}

pub use crate::data_files::write_private;

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("fosta-secrets-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn round_trips_through_the_encrypted_file() {
        let directory = scratch("roundtrip");
        let (store, source) = FileStore::open(&directory, None).unwrap();
        assert!(matches!(source, KeySource::GeneratedFile(_)));
        store.set("conn-1", "pa55 word").unwrap();
        store.set("ai-key-x", "sk-123").unwrap();
        store.delete("ai-key-x").unwrap();
        store.delete("missing").unwrap();

        let on_disk = fs::read_to_string(directory.join(STORE_FILE)).unwrap();
        assert!(!on_disk.contains("pa55"), "the password must not be stored in clear text");

        let (reopened, source) = FileStore::open(&directory, None).unwrap();
        assert!(matches!(source, KeySource::ExistingFile(_)));
        assert_eq!(reopened.get("conn-1").unwrap().as_deref(), Some("pa55 word"));
        assert_eq!(reopened.get("ai-key-x").unwrap(), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn a_wrong_key_is_refused_instead_of_overwriting() {
        let directory = scratch("wrongkey");
        let key = to_hex(&[7u8; KEY_LEN]);
        let (store, source) = FileStore::open(&directory, Some(&key)).unwrap();
        assert_eq!(source, KeySource::Environment);
        store.set("conn-1", "secret").unwrap();
        assert!(FileStore::open(&directory, Some(&to_hex(&[8u8; KEY_LEN]))).is_err());
        assert!(FileStore::open(&directory, Some("too-short")).is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(from_hex(&to_hex(&[0, 15, 255])).unwrap(), vec![0, 15, 255]);
        assert!(from_hex("abc").is_none());
        assert!(from_hex("zz").is_none());
    }
}
