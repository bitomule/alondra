//! Local hook transport. Claude Code starts `alondra hook`; only this daemon writes state.

use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};
use tokio::net::UnixListener;

pub async fn bind(home: &Path) -> Result<UnixListener> {
    let path = home.join("hooks.sock");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            bail!("{} exists but is not a Unix socket", path.display());
        }
        match std::os::unix::net::UnixStream::connect(&path) {
            Ok(_) => bail!("another daemon already owns {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {}
            Err(e) => return Err(e).with_context(|| format!("checking {}", path.display())),
        }
        std::fs::remove_file(&path).with_context(|| format!("removing stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixStream;

    #[tokio::test]
    async fn second_daemon_cannot_replace_a_live_hook_socket() {
        let home = std::env::temp_dir().join(format!("alondra-hook-owner-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        std::fs::create_dir_all(&home).unwrap();
        let first = bind(&home).await.unwrap();
        assert!(bind(&home).await.unwrap_err().to_string().contains("already owns"));
        assert!(UnixStream::connect(home.join("hooks.sock")).await.is_ok());
        drop(first);
        std::fs::remove_dir_all(home).unwrap();
    }
}
