//! Host key verification against known_hosts files.
//!
//! Two files are consulted: the app's own file (read and written) and, read only, the user's
//! `~/.ssh/known_hosts`, so hosts the user already trusts with OpenSSH work without a prompt. The app file
//! is authoritative: when it has an entry for the host, the user file is not looked at. That way a key the
//! user re-confirmed in Fosta wins over a stale OpenSSH entry, and nothing is ever written to `~/.ssh`.

use std::borrow::Cow;
use std::io::Write;
use std::path::{Path, PathBuf};

use russh::keys::{HashAlg, PublicKey};
use serde::{Deserialize, Serialize};

use crate::error::{AppError, AppResult};

/// A server's host key as shown to the user, e.g. in a "trust this host?" prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostKeyInfo {
    pub host: String,
    pub port: u16,
    /// e.g. `ssh-ed25519`.
    pub algorithm: String,
    /// OpenSSH format: `SHA256:` + unpadded base64, as `ssh-keygen -lf` prints it.
    pub fingerprint_sha256: String,
    /// The key in OpenSSH public key format (`ssh-ed25519 AAAA…`); pass it to [`trust_host_key`].
    pub public_key: String,
}

impl HostKeyInfo {
    pub fn new(host: &str, port: u16, key: &PublicKey) -> Self {
        Self {
            host: host.to_string(),
            port,
            algorithm: key.algorithm().as_str().to_string(),
            fingerprint_sha256: key.fingerprint(HashAlg::Sha256).to_string(),
            public_key: key.to_openssh().unwrap_or_default().trim().to_string(),
        }
    }
}

/// Where host keys are looked up and recorded.
#[derive(Debug, Clone)]
pub struct KnownHosts {
    /// The app's file: read, and written when a key is accepted.
    pub app_file: PathBuf,
    /// Usually `~/.ssh/known_hosts`; read only.
    pub user_file: Option<PathBuf>,
}

/// The outcome of checking one server key.
#[derive(Debug, Clone)]
pub(crate) enum Verdict {
    /// A recorded key matches.
    Trusted,
    /// The host was unknown and the key was recorded (`acceptNew`).
    Learned(HostKeyInfo),
    /// Checking is off.
    Unchecked,
    /// The host is unknown. `other_algorithms` is set when keys of other types are recorded for it, which is
    /// never accepted silently.
    Unknown { info: HostKeyInfo, other_algorithms: bool },
    /// A different key of the same type is recorded: possible man-in-the-middle.
    Changed { info: HostKeyInfo, file: PathBuf, line: usize },
    /// A known_hosts file could not be read or written.
    Failed(String),
}

impl Verdict {
    pub(crate) fn accepted(&self) -> bool {
        matches!(self, Verdict::Trusted | Verdict::Learned(_) | Verdict::Unchecked)
    }
}

enum FileResult {
    Match,
    Changed(usize),
    OtherAlgorithms,
    Nothing,
}

fn check_file(host: &str, port: u16, key: &PublicKey, file: &Path) -> Result<FileResult, String> {
    let entries = russh::keys::known_hosts::known_host_keys_path(host, port, file)
        .map_err(|error| format!("Could not read the known hosts file {}: {error}", file.display()))?;
    if entries.iter().any(|(_, recorded)| recorded == key) {
        return Ok(FileResult::Match);
    }
    if let Some((line, _)) = entries.iter().find(|(_, recorded)| recorded.algorithm() == key.algorithm()) {
        return Ok(FileResult::Changed(*line));
    }
    Ok(if entries.is_empty() {
        FileResult::Nothing
    } else {
        FileResult::OtherAlgorithms
    })
}

/// known_hosts entries are matched on the lower-cased host name, like OpenSSH does.
fn normalize(host: &str) -> Cow<'_, str> {
    if host.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(host.to_ascii_lowercase())
    } else {
        Cow::Borrowed(host)
    }
}

/// Checks `key` for `host:port`. With `accept_new`, an unknown host (with no keys recorded at all) is
/// recorded in the app file.
pub(crate) fn verify(known: &KnownHosts, host: &str, port: u16, key: &PublicKey, accept_new: bool) -> Verdict {
    let host = normalize(host);
    let info = || HostKeyInfo::new(&host, port, key);
    let files = std::iter::once(known.app_file.as_path()).chain(known.user_file.as_deref());
    let mut other_algorithms = false;
    for file in files {
        match check_file(&host, port, key, file) {
            Ok(FileResult::Match) => return Verdict::Trusted,
            Ok(FileResult::Changed(line)) => {
                return Verdict::Changed {
                    info: info(),
                    file: file.to_path_buf(),
                    line,
                }
            }
            Ok(FileResult::OtherAlgorithms) => {
                other_algorithms = true;
                // The app file is authoritative once it knows the host.
                if file == known.app_file {
                    break;
                }
            }
            Ok(FileResult::Nothing) => {}
            Err(message) => return Verdict::Failed(message),
        }
    }
    if accept_new && !other_algorithms {
        return match russh::keys::known_hosts::learn_known_hosts_path(&host, port, key, &known.app_file) {
            Ok(()) => Verdict::Learned(info()),
            Err(error) => Verdict::Failed(format!("Could not record the host key in {}: {error}", known.app_file.display())),
        };
    }
    Verdict::Unknown {
        info: info(),
        other_algorithms,
    }
}

fn host_pattern(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

/// Records `public_key` (OpenSSH format, as in [`HostKeyInfo::public_key`]) as the key of `host:port` in
/// the app's known_hosts file, replacing any key recorded there for that host. Call it only after the user
/// confirmed the fingerprint, e.g. after an `SSH_HOST_KEY_UNKNOWN` or `SSH_HOST_KEY_MISMATCH` error.
/// `~/.ssh/known_hosts` is never modified; a stale entry there is overridden by the app file.
pub fn trust_host_key(known_hosts_file: &Path, host: &str, port: u16, public_key: &str) -> AppResult<()> {
    let key = PublicKey::from_openssh(public_key.trim()).map_err(|error| AppError::new(format!("Not an OpenSSH public key: {error}")))?;
    forget_host_key(known_hosts_file, host, port)?;
    russh::keys::known_hosts::learn_known_hosts_path(&normalize(host), port, &key, known_hosts_file)
        .map_err(|error| AppError::new(format!("Could not write {}: {error}", known_hosts_file.display())))
}

/// Removes every key recorded for `host:port` from the app's known_hosts file (plain entries; the app never
/// writes hashed ones).
pub fn forget_host_key(known_hosts_file: &Path, host: &str, port: u16) -> AppResult<()> {
    let text = match std::fs::read_to_string(known_hosts_file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(AppError::new(format!("Could not read {}: {error}", known_hosts_file.display()))),
    };
    let pattern = host_pattern(&normalize(host), port);
    let kept: Vec<&str> = text
        .lines()
        .filter(|line| {
            let hosts = line.split_whitespace().next().unwrap_or("");
            !line.trim().is_empty() && (line.starts_with('#') || !hosts.split(',').any(|entry| entry == pattern))
        })
        .collect();
    if kept.len() == text.lines().count() {
        return Ok(());
    }
    let mut body = kept.join("\n");
    if !body.is_empty() {
        body.push('\n');
    }
    let temp = known_hosts_file.with_extension("tmp");
    let write = || -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(body.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temp, known_hosts_file)
    };
    write().map_err(|error| AppError::new(format!("Could not write {}: {error}", known_hosts_file.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_A: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIJdD7y3aLq454yWBdwLWbieU1ebz9/cu7/QEXn9OIeZJ";
    const KEY_B: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIA6rWI3G2sz07DnfFlrouTcysQlj2P+jpNSOEWD9OJ3X";
    const KEY_RSA: &str = "ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAAAgQDc7UftuICOKGUH1fRsyHFVpXDShTOLvsR4DN0e85qTHJ4mbSO61Nyztjj2azHhDUK0QOwfaJtkXrEqMsqc5/XX1gBl5Y2KEwWr32T11KhHLxX/Q0Bsu3eBYfl5bILplnPJxZEjg2QfSafOElqQ7oPJ7EJlOWNOwbxFp9hGPkU9mQ==";

    struct Files {
        dir: PathBuf,
    }

    impl Files {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("fosta-net-known-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn known(&self, user: Option<&str>) -> KnownHosts {
            let user_file = user.map(|text| {
                let path = self.dir.join("user_known_hosts");
                std::fs::write(&path, text).unwrap();
                path
            });
            KnownHosts {
                app_file: self.dir.join("app").join("known_hosts"),
                user_file,
            }
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn key(text: &str) -> PublicKey {
        PublicKey::from_openssh(text).unwrap()
    }

    #[test]
    fn accept_new_records_an_unknown_host_once() {
        let files = Files::new("tofu");
        let known = files.known(None);
        let verdict = verify(&known, "DB.example.com", 2222, &key(KEY_A), true);
        let Verdict::Learned(info) = verdict else {
            panic!("expected Learned, got {verdict:?}")
        };
        assert_eq!(info.host, "db.example.com");
        assert!(info.fingerprint_sha256.starts_with("SHA256:"), "{}", info.fingerprint_sha256);
        assert_eq!(info.algorithm, "ssh-ed25519");
        let text = std::fs::read_to_string(&known.app_file).unwrap();
        assert!(text.trim_start().starts_with("[db.example.com]:2222 ssh-ed25519 "), "{text}");
        assert!(matches!(
            verify(&known, "db.example.com", 2222, &key(KEY_A), true),
            Verdict::Trusted
        ));
    }

    #[test]
    fn strict_mode_reports_unknown_hosts_with_their_fingerprint() {
        let files = Files::new("strict");
        let known = files.known(None);
        let verdict = verify(&known, "db", 22, &key(KEY_A), false);
        let Verdict::Unknown { info, other_algorithms } = verdict else {
            panic!("expected Unknown, got {verdict:?}")
        };
        assert!(!other_algorithms);
        assert_eq!(info.fingerprint_sha256, key(KEY_A).fingerprint(HashAlg::Sha256).to_string());
        assert!(!known.app_file.exists());
        // After the user confirms, the key is trusted.
        trust_host_key(&known.app_file, "db", 22, &info.public_key).unwrap();
        assert!(matches!(verify(&known, "db", 22, &key(KEY_A), false), Verdict::Trusted));
    }

    #[test]
    fn a_changed_key_is_never_accepted() {
        let files = Files::new("changed");
        let known = files.known(Some(&format!("bastion {KEY_A}\n")));
        let verdict = verify(&known, "bastion", 22, &key(KEY_B), true);
        let Verdict::Changed { line, file, .. } = verdict else {
            panic!("expected Changed, got {verdict:?}")
        };
        assert_eq!(line, 1);
        assert_eq!(file, known.user_file.clone().unwrap());
        assert!(!known.app_file.exists(), "nothing may be recorded for a changed key");
        // An explicit re-trust in the app file overrides the stale user entry.
        trust_host_key(&known.app_file, "bastion", 22, KEY_B).unwrap();
        assert!(matches!(verify(&known, "bastion", 22, &key(KEY_B), true), Verdict::Trusted));
        assert!(matches!(verify(&known, "bastion", 22, &key(KEY_A), true), Verdict::Changed { .. }));
    }

    #[test]
    fn hosts_known_with_other_key_types_need_a_confirmation() {
        let files = Files::new("other");
        let known = files.known(Some(&format!("bastion {KEY_RSA}\n")));
        let verdict = verify(&known, "bastion", 22, &key(KEY_A), true);
        assert!(
            matches!(
                verdict,
                Verdict::Unknown {
                    other_algorithms: true,
                    ..
                }
            ),
            "{verdict:?}"
        );
        assert!(!verdict.accepted());
    }

    #[test]
    fn user_known_hosts_are_honoured_read_only() {
        let files = Files::new("user");
        let known = files.known(Some(&format!("# comment\nother {KEY_B}\n[bastion]:2200 {KEY_A}\n")));
        assert!(matches!(verify(&known, "bastion", 2200, &key(KEY_A), true), Verdict::Trusted));
        assert!(!known.app_file.exists());
    }

    #[test]
    fn trusting_replaces_only_the_hosts_own_entries() {
        let files = Files::new("replace");
        let known = files.known(None);
        trust_host_key(&known.app_file, "a", 22, KEY_A).unwrap();
        trust_host_key(&known.app_file, "b", 2222, KEY_A).unwrap();
        trust_host_key(&known.app_file, "a", 22, KEY_B).unwrap();
        let text = std::fs::read_to_string(&known.app_file).unwrap();
        assert_eq!(text.lines().filter(|line| !line.is_empty()).count(), 2, "{text}");
        assert!(text.contains("[b]:2222 ssh-ed25519"));
        forget_host_key(&known.app_file, "b", 2222).unwrap();
        let text = std::fs::read_to_string(&known.app_file).unwrap();
        assert_eq!(text.lines().filter(|line| !line.is_empty()).count(), 1, "{text}");
        assert!(trust_host_key(&known.app_file, "a", 22, "not a key").is_err());
    }
}
