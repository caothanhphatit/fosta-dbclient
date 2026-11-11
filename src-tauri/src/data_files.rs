//! The app's own data files (config, history, audit log, open tabs, the web edition's keys) are written readable by their
//! owner only: mode 0600 on Unix, where the web edition's data volume may be shared with other processes. On Windows a
//! file takes the ACL of the data folder (the installed app's folder is under the user's profile), which is what the
//! platform offers without touching ACLs.

use std::{
    fs,
    io::{self, Write},
    path::Path,
};

use crate::error::AppResult;

/// Options that create a file readable and writable by its owner only.
pub fn private_options() -> fs::OpenOptions {
    #[allow(unused_mut)]
    let mut options = fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// Restricts a file written by an earlier version to its owner (Unix); a missing file is fine.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        match fs::set_permissions(path, fs::Permissions::from_mode(0o600)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Creates a folder of private files (and its missing parents) usable by its owner only: mode 0700 on Unix, also for a
/// folder an earlier version created with the umask.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new().recursive(true).mode(0o700).create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(path)?;
    Ok(())
}

/// Writes through a temporary file and a rename, readable by the owner only.
pub fn write_private(path: &Path, bytes: &[u8]) -> AppResult<()> {
    let temporary = path.with_extension("tmp");
    let mut file = private_options().write(true).create(true).truncate(true).open(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_files_are_written_whole_and_owner_only() {
        let path = std::env::temp_dir().join(format!("fosta-data-files-{}.txt", std::process::id()));
        let _ = fs::remove_file(&path);
        write_private(&path, b"first").unwrap();
        write_private(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert!(!path.with_extension("tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
            restrict_to_owner(&path).unwrap();
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        restrict_to_owner(&path.with_extension("missing")).unwrap();
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn private_folders_are_owner_only() {
        let dir = std::env::temp_dir()
            .join(format!("fosta-private-dir-{}", std::process::id()))
            .join("inner");
        let _ = fs::remove_dir_all(dir.parent().unwrap());
        create_private_dir(&dir).unwrap();
        create_private_dir(&dir).unwrap();
        assert!(dir.is_dir());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
            create_private_dir(&dir).unwrap();
            assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        }
        fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }
}
