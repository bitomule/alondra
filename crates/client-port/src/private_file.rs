//! Owner-only files this terminal keeps, written so a crash never leaves half of one.
use std::fs;
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD as B64};

/// Replaces `path` with `bytes`, readable only by its owner; refuses a symlink or a non-file.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path) {
        if !meta.is_file() || meta.file_type().is_symlink() {
            bail!("unsafe terminal profile path");
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", B64.encode(crate::client::random::<12>()?)));
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&tmp, path)?;
    fs::File::open(path.parent().context("profile parent")?)?.sync_all()?;
    Ok(())
}
