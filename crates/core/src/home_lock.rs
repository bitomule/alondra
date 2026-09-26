//! Admission barrier for service identity recovery. Locks live for the process /
//! setup operation; never remove the lock file (that would split the lock domain).
use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::fs::OpenOptionsExt,
    path::Path,
};
pub const RECOVERY: &str = "service-recovery.json";
pub struct HomeLock {
    _file: File,
}
impl HomeLock {
    pub fn shared(home: &Path) -> io::Result<Self> {
        Self::acquire(home, false)
    }
    pub fn recovery(home: &Path) -> io::Result<Self> {
        Self::acquire(home, true)
    }
    fn acquire(home: &Path, exclusive: bool) -> io::Result<Self> {
        fs::create_dir_all(home)?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home.join("service-home.lock"))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("HOME lock must be a regular file"));
        }
        let result = if exclusive {
            file.try_lock()
        } else {
            file.try_lock_shared()
        };
        result.map_err(|_| {
            io::Error::other("Alondra HOME is busy; stop its daemon before service recovery")
        })?;
        Ok(Self { _file: file })
    }
}
/// Call only after holding a home lock and before loading any local trust.
pub fn ensure_ready(home: &Path) -> io::Result<()> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(home.join(RECOVERY))
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("recovery state must be a regular file"));
    }
    let value: serde_json::Value = serde_json::from_reader(file).map_err(io::Error::other)?;
    if value["version"] != 1 || value["phase"] != "committed" {
        return Err(io::Error::other(
            "service recovery is pending; resume alondra setup --recover-service before starting the daemon",
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exclusive_recovery_blocks_daemon_and_shared_readers_block_recovery() {
        let home = std::env::temp_dir().join(format!("alondra-home-lock-{}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        let a = HomeLock::shared(&home).unwrap();
        let b = HomeLock::shared(&home).unwrap();
        assert!(HomeLock::recovery(&home).is_err());
        drop(a);
        drop(b);
        let recovery = HomeLock::recovery(&home).unwrap();
        assert!(HomeLock::shared(&home).is_err());
        fs::write(home.join(RECOVERY), br#"{"version":1,"phase":"prepared"}"#).unwrap();
        assert!(ensure_ready(&home).is_err());
        fs::write(home.join(RECOVERY), br#"{"version":1,"phase":"committed"}"#).unwrap();
        ensure_ready(&home).unwrap();
        drop(recovery);
        fs::remove_dir_all(home).unwrap();
    }
}

/// Public identity follows the same config pointer as the signing key. Legacy
/// no-config callers retain their original path; service startup requires config.
pub fn public_identity_path(home: &Path) -> io::Result<std::path::PathBuf> {
    #[derive(serde::Deserialize)]
    struct Config {
        signing_key_path: std::path::PathBuf,
    }
    let path = home.join("service.toml");
    let config = match fs::read_to_string(path) {
        Ok(value) => toml::from_str::<Config>(&value).map_err(io::Error::other)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(home.join("service-keys/public.json"));
        }
        Err(error) => return Err(error),
    };
    if config.signing_key_path.is_absolute()
        || !config
            .signing_key_path
            .components()
            .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return Err(io::Error::other(
            "service signing key must be relative to HOME",
        ));
    }
    let parent = home
        .join(config.signing_key_path)
        .parent()
        .ok_or_else(|| io::Error::other("missing key parent"))?
        .to_owned();
    if !parent.canonicalize()?.starts_with(home.canonicalize()?) {
        return Err(io::Error::other("service signing key escapes HOME"));
    }
    Ok(parent.join("public.json"))
}
