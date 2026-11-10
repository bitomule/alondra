//! The lock every writer of the daemon's settings files holds, and how one file is replaced whole.
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::sync::Mutex;

/// A holder that panics leaves it whole, so it is recovered: it holds nothing, and every file it
/// guards is replaced whole.
pub static WRITER: Mutex<()> = Mutex::new(());

pub fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let next = path.with_extension(format!("toml.{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&next)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&next, path)
}
