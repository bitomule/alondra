//! Local hook transport. Claude Code starts `alondra hook`; only this daemon writes state.

use std::io::{Read, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::net::UnixListener;

const OWNER_ANSWERS_WITHIN: Duration = Duration::from_secs(5);
const ACK_PREFIX: &[u8] = b"alondrad:";

pub(crate) fn probe_ack() -> Vec<u8> {
    format!("alondrad:{}\n", std::process::id()).into_bytes()
}

/// A listening socket outlives its daemon when a child forked at the wrong moment inherited the
/// descriptor (macOS cannot create sockets close-on-exec atomically). The kernel keeps queueing
/// connections on it, and when that child exits the queued connections are hung up on, so neither a
/// successful connect nor an EOF proves a daemon is serving. Only the daemon's explicit ack does.
fn owner_is_serving(stream: &mut UnixStream) -> bool {
    let probe = br#"{"probe":true}"#;
    let sent = stream.set_read_timeout(Some(OWNER_ANSWERS_WITHIN)).is_ok()
        && stream.set_write_timeout(Some(OWNER_ANSWERS_WITHIN)).is_ok()
        && stream.write_all(&(probe.len() as u32).to_be_bytes()).is_ok()
        && stream.write_all(probe).is_ok();
    if !sent {
        return false;
    }
    let mut ack = [0u8; ACK_PREFIX.len()];
    stream.read_exact(&mut ack).is_ok() && ack == ACK_PREFIX
}

pub async fn bind(home: &Path) -> Result<UnixListener> {
    let path = home.join("hooks.sock");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() {
            bail!("{} exists but is not a Unix socket", path.display());
        }
        match UnixStream::connect(&path) {
            Ok(mut stream) => {
                if tokio::task::spawn_blocking(move || owner_is_serving(&mut stream)).await? {
                    bail!("another daemon already owns {}", path.display());
                }
            }
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream as AsyncStream;

    fn scratch(_name: &str) -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn answer_probes(listener: UnixListener) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut len = [0u8; 4];
                let _ = stream.read_exact(&mut len).await;
                let mut frame = vec![0; u32::from_be_bytes(len) as usize];
                let _ = stream.read_exact(&mut frame).await;
                let _ = stream.write_all(&probe_ack()).await;
                let _ = stream.shutdown().await;
            }
        })
    }

    #[tokio::test]
    async fn second_daemon_cannot_replace_a_live_hook_socket() {
        let home = scratch("hook-owner");
        let owner = answer_probes(bind(&home).await.unwrap());
        assert!(bind(&home).await.unwrap_err().to_string().contains("already owns"));
        assert!(AsyncStream::connect(home.join("hooks.sock")).await.is_ok());
        owner.abort();
        let _ = owner.await;
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_listener_that_hangs_up_without_the_ack_is_not_an_owner() {
        let home = scratch("hook-hangup");
        let stray = bind(&home).await.unwrap();
        let hanging_up = tokio::spawn(async move {
            while let Ok((mut stream, _)) = stray.accept().await {
                let _ = stream.shutdown().await;
            }
        });
        let started = std::time::Instant::now();
        let replacement = bind(&home).await.unwrap();
        assert!(started.elapsed() < OWNER_ANSWERS_WITHIN);
        hanging_up.abort();
        drop(replacement);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[tokio::test]
    async fn a_socket_kept_open_by_a_stray_child_without_a_daemon_is_replaced() {
        let home = scratch("hook-stray");
        let stray = bind(&home).await.unwrap();
        let started = std::time::Instant::now();
        let replacement = bind(&home).await.unwrap();
        assert!(started.elapsed() >= OWNER_ANSWERS_WITHIN);
        let owner = answer_probes(replacement);
        assert!(bind(&home).await.unwrap_err().to_string().contains("already owns"));
        owner.abort();
        let _ = owner.await;
        drop(stray);
        std::fs::remove_dir_all(home).unwrap();
    }
}
