pub use alondra_engine_ports::journal::settings::{PendingProjection, ProjectionIdentity};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alondra_domain_model::{Actor, CommandEnvelope};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::DomainStore;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS settings_projection_receipts(identity BLOB PRIMARY KEY, actor TEXT NOT NULL, command_id TEXT NOT NULL, data_epoch TEXT NOT NULL, command_digest BLOB NOT NULL, target_digest BLOB NOT NULL);";







pub use alondra_daemon_engine_model::options::{pending, read_options};
pub use alondra_daemon_engine_model::options::read_optional;
use alondra_daemon_engine_model::options::{OPTIONS_LIMIT, PENDING, replace, sync_parent};

pub(super) fn record(c: &rusqlite::Connection, identity: &ProjectionIdentity) -> Result<()> {
    ensure!(!c.is_autocommit(), "a settings projection receipt requires the authority transaction");
    let key = identity.key()?;
    super::repository::exec(c, "INSERT INTO settings_projection_receipts(identity,actor,command_id,data_epoch,command_digest,target_digest) VALUES(?1,?2,?3,?4,?5,?6)",
        params![key, identity.actor, identity.command_id, identity.data_epoch, identity.command_digest.as_slice(), identity.target_digest.as_slice()])?;
    Ok(())
}

pub fn committed(store: &DomainStore, identity: &ProjectionIdentity) -> Result<bool> {
    let epoch: String = store.conn.query_row("SELECT data_epoch FROM domain_meta WHERE singleton=1", [], |row| row.get(0))?;
    ensure!(epoch == identity.data_epoch, "the domain database changed during a settings write; preserve options.toml and options.pending for reconciliation");
    let stored = store.conn.query_row("SELECT actor,command_id,data_epoch,command_digest,target_digest FROM settings_projection_receipts WHERE identity=?1", [identity.key()?], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, Vec<u8>>(3)?, row.get::<_, Vec<u8>>(4)?))
    }).optional()?;
    if let Some((actor, command_id, epoch, digest, target)) = stored {
        ensure!(actor == identity.actor && command_id == identity.command_id && epoch == identity.data_epoch && digest == identity.command_digest && target == identity.target_digest,
            "the settings receipt does not match its complete semantic identity");
        return Ok(true);
    }
    Ok(false)
}

pub fn recover_store(store: &mut DomainStore, home: &Path) -> Result<()> {
    let path = home.join(PENDING);
    if !pending(home) {
        store.conn.execute("DELETE FROM settings_projection_receipts", [])?;
        return Ok(());
    }
    let meta = fs::symlink_metadata(&path)?;
    ensure!(meta.is_file() && !meta.file_type().is_symlink(), "options.pending must be a private regular file");
    let file = File::open(&path)?;
    let projection: PendingProjection = serde_json::from_reader(file.take(OPTIONS_LIMIT * 3))
        .context("options.pending is incomplete or invalid; preserve it and options.toml for reconciliation")?;
    ensure!(Sha256::digest(projection.target.as_bytes()).as_slice() == projection.identity.target_digest, "the pending settings target digest does not match");
    ensure!(projection.previous.as_deref().map(|text| <[u8; 32]>::from(Sha256::digest(text.as_bytes()))) == projection.identity.previous_digest, "the pending settings backup digest does not match");
    let is_committed = committed(store, &projection.identity)?;
    let current = read_optional(&alondra_core::options::path_in(home))?;
    ensure!(current == projection.previous || current.as_deref() == Some(projection.target.as_str()),
        "options.toml was edited outside the pending settings transaction; preserve both files and reconcile the external change");
    let expected = if is_committed { Some(projection.target.as_str()) } else { projection.previous.as_deref() };
    let options = alondra_core::options::path_in(home);
    if current.as_deref() != expected {
        match expected {
            Some(text) => replace(&options, text.as_bytes())?,
            None => { if options.exists() { fs::remove_file(&options)?; sync_parent(&options)?; } }
        }
    }
    fs::remove_file(&path)?;
    sync_parent(&path)?;
    store.conn.execute("DELETE FROM settings_projection_receipts WHERE identity=?1", [projection.identity.key()?])?;
    Ok(())
}
