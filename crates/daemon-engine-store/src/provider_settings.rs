use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alondra_core::domain::{Actor, CommandEnvelope};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::DomainStore;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS settings_projection_receipts(identity BLOB PRIMARY KEY, actor TEXT NOT NULL, command_id TEXT NOT NULL, data_epoch TEXT NOT NULL, command_digest BLOB NOT NULL, target_digest BLOB NOT NULL);";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectionIdentity {
    pub actor: String,
    pub command_id: String,
    pub data_epoch: String,
    pub command_digest: [u8; 32],
    pub previous_digest: Option<[u8; 32]>,
    pub target_digest: [u8; 32],
}

impl ProjectionIdentity {
    pub fn new(actor: &Actor, command: &CommandEnvelope, previous: Option<&str>, target: &str) -> Result<Self> {
        let actor = match actor { Actor::Orchestrator { .. } => serde_json::json!({"kind":"orchestrator"}), _ => serde_json::to_value(actor)? };
        Ok(Self { actor: String::from_utf8(super::canonical::bytes(&actor)?)?, command_id: command.command_id.0.clone(),
            data_epoch: command.data_epoch.0.clone(), command_digest: super::canonical::digest(command)?, previous_digest: previous.map(|text| Sha256::digest(text.as_bytes()).into()), target_digest: Sha256::digest(target.as_bytes()).into() })
    }
    fn key(&self) -> Result<Vec<u8>> { Ok(super::canonical::bytes(self)?) }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PendingProjection {
    pub identity: ProjectionIdentity,
    pub previous: Option<String>,
    pub target: String,
}

pub use alondra_daemon_engine_model::options::{pending, read_options};
pub use alondra_daemon_engine_model::options::read_optional;
use alondra_daemon_engine_model::options::{OPTIONS_LIMIT, PENDING};

fn temporary(path: &Path) -> PathBuf {
    path.with_file_name(format!(".{}.{}.next", path.file_name().expect("settings file name").to_string_lossy(), crate::auth::random_hex(16)))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let next = temporary(path);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&next)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(why) = result { let _ = fs::remove_file(&next); return Err(why.into()); }
    Ok(next)
}

fn sync_parent(path: &Path) -> Result<()> { File::open(path.parent().context("settings parent missing")?)?.sync_all()?; Ok(()) }

pub fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let next = write_private(path, bytes)?;
    if let Err(why) = fs::rename(&next, path) { let _ = fs::remove_file(&next); return Err(why.into()); }
    sync_parent(path)
}

pub fn prepare(home: &Path, projection: &PendingProjection) -> Result<()> {
    let path = home.join(PENDING);
    let bytes = serde_json::to_vec(projection)?;
    let next = write_private(&path, &bytes)?;
    let linked = fs::hard_link(&next, &path);
    let _ = fs::remove_file(&next);
    linked.context("another settings change is awaiting reconciliation")?;
    sync_parent(&path)
}

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
