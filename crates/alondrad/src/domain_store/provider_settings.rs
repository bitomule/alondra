use crate::locks::Poisonable as _;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use alondra_core::domain::{Actor, Command, CommandEnvelope, DomainError, PROTOCOL_VERSION};
use anyhow::{Context, Result, ensure};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Attached, DomainStore, StoreError};
use crate::domain_runtime::DomainRuntime;

pub(super) const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS settings_projection_receipts(identity BLOB PRIMARY KEY, actor TEXT NOT NULL, command_id TEXT NOT NULL, data_epoch TEXT NOT NULL, command_digest BLOB NOT NULL, target_digest BLOB NOT NULL);";
const PENDING: &str = "options.pending";
const OPTIONS_LIMIT: u64 = 4 * 1024 * 1024;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct ProjectionIdentity {
    actor: String,
    command_id: String,
    data_epoch: String,
    command_digest: [u8; 32],
    previous_digest: Option<[u8; 32]>,
    target_digest: [u8; 32],
}

impl ProjectionIdentity {
    fn new(actor: &Actor, command: &CommandEnvelope, previous: Option<&str>, target: &str) -> Result<Self> {
        let actor = match actor { Actor::Orchestrator { .. } => serde_json::json!({"kind":"orchestrator"}), _ => serde_json::to_value(actor)? };
        Ok(Self { actor: String::from_utf8(super::canonical::bytes(&actor)?)?, command_id: command.command_id.0.clone(),
            data_epoch: command.data_epoch.0.clone(), command_digest: super::canonical::digest(command)?, previous_digest: previous.map(|text| Sha256::digest(text.as_bytes()).into()), target_digest: Sha256::digest(target.as_bytes()).into() })
    }
    fn key(&self) -> Result<Vec<u8>> { Ok(super::canonical::bytes(self)?) }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingProjection {
    identity: ProjectionIdentity,
    previous: Option<String>,
    target: String,
}

pub(crate) fn pending(home: &Path) -> bool { home.join(PENDING).symlink_metadata().is_ok() }

pub(crate) fn read_options(home: &Path) -> Result<Option<String>, String> {
    if pending(home) { return Err("An accounts or model settings change is being reconciled; try again when it finishes".into()); }
    let text = read_optional(&alondra_core::options::path_in(home)).map_err(|why| why.to_string())?;
    if pending(home) { return Err("The settings changed while they were being read; refresh and try again".into()); }
    Ok(text)
}

fn read_optional(path: &Path) -> Result<Option<String>> {
    match File::open(path) {
        Ok(file) => {
            let mut text = String::new();
            file.take(OPTIONS_LIMIT + 1).read_to_string(&mut text)?;
            ensure!(text.len() as u64 <= OPTIONS_LIMIT, "the options file exceeds its size limit");
            Ok(Some(text))
        }
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(why) => Err(why.into()),
    }
}

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

fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let next = write_private(path, bytes)?;
    if let Err(why) = fs::rename(&next, path) { let _ = fs::remove_file(&next); return Err(why.into()); }
    sync_parent(path)
}

fn prepare(home: &Path, projection: &PendingProjection) -> Result<()> {
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

fn committed(store: &DomainStore, identity: &ProjectionIdentity) -> Result<bool> {
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

pub(crate) fn recover_store(store: &mut DomainStore, home: &Path) -> Result<()> {
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

pub(crate) fn recover(domain: &DomainRuntime, home: &Path) -> Result<()> {
    let mut store = domain.store.lock_or_exit("the domain store");
    recover_store(&mut store, home)
}

pub(crate) fn apply(domain: &DomainRuntime, home: &Path, actor: &Actor, command: CommandEnvelope, previous: Option<String>, target: String, recorded: Vec<Command>) -> Result<(), StoreError> {
    apply_authorized(domain, home, actor, command, previous, target, recorded, || Ok(()), |_| Ok(()))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, command: CommandEnvelope, previous: Option<String>, target: String, recorded: Vec<Command>,
    authorize: impl Fn() -> anyhow::Result<()>, prepare_accounts: impl FnOnce(&alondra_core::domain::DomainState) -> Result<(), StoreError>) -> Result<(), StoreError>
{
    recover(domain, home)?;
    let identity = ProjectionIdentity::new(actor, &command, previous.as_deref(), &target)?;
    let scope = alondra_store::AuthorityScope::new(command.data_epoch.clone())?;
    let projection = PendingProjection { identity: identity.clone(), previous, target };
    let result = domain.own_projected(scope, |transaction| {
        authorize().map_err(|_| DomainError::new("forbidden", "Device approval was revoked before the settings change"))?;
        transaction.stage_command(actor, &command, Attached::None)?;
        for (index, recorded) in recorded.into_iter().enumerate() {
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: command.data_epoch.clone(),
                command_id: format!("{}-projection-{index}", command.command_id).into(), expected_revision: 0, command: recorded };
            transaction.stage_command(&Actor::System, &envelope, Attached::None)?;
        }
        if read_optional(&alondra_core::options::path_in(home))? != projection.previous {
            return Err(DomainError::new("stale_config", "The options file changed; refresh Settings before saving").into());
        }
        transaction.record_settings_projection(&projection.identity)?;
        prepare(home, &projection)?;
        prepare_accounts(transaction.domain_state())?;
        if read_optional(&alondra_core::options::path_in(home))? != projection.previous {
            return Err(DomainError::new("stale_config", "The options file changed during account preparation; preserve the external edit and reconcile Settings").into());
        }
        replace(&alondra_core::options::path_in(home), projection.target.as_bytes())?;
        Ok(())
    }, |store| recover_store(store, home));
    match result {
        Ok(_) => Ok(()),
        Err(why) => {
            let was_committed = { let store = domain.store.lock_or_exit("the domain store"); committed(&store, &identity)? };
            recover(domain, home)?;
            if was_committed { Ok(()) } else { Err(why) }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::accounts::{AccountBilling, AccountKey, AccountState, AccountStatus, AccountsConfig, Provider};

    struct Fixture { runtime: DomainRuntime, home: PathBuf, _scratch: crate::test_support::Directory }

    fn seed(runtime: &DomainRuntime, command: Command) {
        let envelope = envelope(runtime, command);
        runtime.execute_as_system(envelope).unwrap();
    }
    fn user() -> Actor { Actor::User { user_id: "settings-user".into() } }
    fn envelope(runtime: &DomainRuntime, command: Command) -> CommandEnvelope {
        CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: runtime.read().state().data_epoch.clone(),
            command_id: format!("settings-test-{}", crate::auth::random_hex(8)).into(), expected_revision: 0, command }
    }
    fn fixture() -> Fixture {
        let scratch = crate::test_support::Directory::new();
        let home = scratch.join("home");
        fs::create_dir_all(&home).unwrap();
        let mut config = AccountsConfig::fresh();
        config.connect(Provider::Claude); config.connect(Provider::Codex);
        let keys = vec![AccountKey::native(Provider::Claude), AccountKey::native(Provider::Codex)];
        config.set_order(keys.clone()).unwrap();
        fs::write(alondra_core::options::path_in(&home), config.to_toml()).unwrap();
        let runtime = DomainRuntime::open(&home).unwrap();
        seed(&runtime, Command::RecordAccounts { accounts: keys.iter().map(|key| AccountStatus {
            provider: key.provider, id: key.account.clone(), name: key.provider.as_str().into(), state: AccountState::SignedIn,
            native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0,
        }).collect(), reserve: alondra_core::usage::default_reserve() });
        seed(&runtime, Command::RecordAccountConfiguration { order: keys, version: "old-order".into() });
        seed(&runtime, Command::RecordModelProfiles { profiles: Default::default(), version: alondra_core::model_selection::profiles_version(&Default::default()) });
        Fixture { runtime, home, _scratch: scratch }
    }
    fn change(f: &Fixture) -> (CommandEnvelope, PendingProjection) {
        let previous = fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap();
        let mut next = alondra_core::options::OptionsFile::parse(&previous).unwrap().accounts;
        let order = vec![AccountKey::native(Provider::Codex), AccountKey::native(Provider::Claude)];
        next.set_order(order.clone()).unwrap();
        let command = envelope(&f.runtime, Command::SetAccountOrder { order, expected_config_version: "old-order".into() });
        let target = alondra_core::accounts::with_accounts(&previous, &next).unwrap();
        let identity = ProjectionIdentity::new(&user(), &command, Some(&previous), &target).unwrap();
        (command, PendingProjection { identity, previous: Some(previous), target })
    }
    fn stage_file(f: &Fixture, command: &CommandEnvelope, projection: &PendingProjection, commit: bool) -> Result<(), StoreError> {
        let scope = alondra_store::AuthorityScope::new(command.data_epoch.clone()).unwrap();
        f.runtime.own(scope, |transaction| {
            transaction.stage_command(&user(), command, Attached::None)?;
            transaction.record_settings_projection(&projection.identity)?;
            prepare(&f.home, projection)?;
            replace(&alondra_core::options::path_in(&f.home), projection.target.as_bytes())?;
            if !commit { return Err(StoreError::Storage(anyhow::anyhow!("simulated crash after rename before SQL commit"))); }
            Ok(())
        }).map(|_| ())
    }

    #[test]
    fn a_sql_refusal_does_not_change_the_file_or_leave_a_pending_write() {
        let f = fixture();
        let (command, projection) = change(&f);
        let refused = apply(&f.runtime, &f.home, &Actor::Agent { agent_id: "untrusted-node".into() }, command,
            projection.previous.clone(), projection.target, vec![]);
        assert!(matches!(refused, Err(StoreError::Domain(why)) if why.code == "forbidden"));
        assert_eq!(read_options(&f.home).unwrap(), projection.previous);
        assert!(!pending(&f.home));
        assert_eq!(f.runtime.read().state().account_order[0].provider, Provider::Claude);
    }

    #[test]
    fn a_crash_after_rename_before_sql_restores_the_original_before_startup_reads() {
        let f = fixture();
        let (command, projection) = change(&f);
        assert!(stage_file(&f, &command, &projection, false).is_err());
        assert_eq!(fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap(), projection.target);
        assert!(read_options(&f.home).is_err(), "uncommitted projection is never imported");
        drop(f.runtime);
        let reopened = DomainRuntime::open(&f.home).unwrap();
        assert_eq!(read_options(&f.home).unwrap(), projection.previous);
        assert!(!pending(&f.home));
        assert_eq!(reopened.read().state().account_order[0].provider, Provider::Claude);
        drop(reopened);
    }

    #[test]
    fn committed_projection_proof_survives_semantic_receipt_retention_until_cleanup() {
        let f = fixture();
        let (command, projection) = change(&f);
        stage_file(&f, &command, &projection, true).unwrap();
        {
            let store = f.runtime.store.lock().unwrap();
            store.conn.execute("DELETE FROM command_bodies WHERE revision IN (SELECT revision FROM command_audit WHERE command_id=?1)", [&projection.identity.command_id]).unwrap();
            store.conn.execute("DELETE FROM command_audit WHERE command_id=?1", [&projection.identity.command_id]).unwrap();
            store.conn.execute("DELETE FROM semantic_receipts WHERE command_id=?1", [&projection.identity.command_id]).unwrap();
        }
        drop(f.runtime);
        let reopened = DomainRuntime::open(&f.home).unwrap();
        assert_eq!(read_options(&f.home).unwrap().as_deref(), Some(projection.target.as_str()));
        assert_eq!(reopened.read().state().account_order[0].provider, Provider::Codex);
        assert!(!pending(&f.home));
        drop(reopened);
    }

    #[test]
    fn settings_are_reauthorized_under_the_writer_before_any_file_or_domain_change() {
        let f = fixture(); let (command, projection) = change(&f);
        let revision = f.runtime.read().state().revision;
        let result = apply_authorized(&f.runtime, &f.home, &user(), command, projection.previous.clone(), projection.target, vec![],
            || Err(anyhow::anyhow!("approval revoked")), |_| panic!("no account preparation after revocation"));
        assert!(matches!(result, Err(StoreError::Domain(error)) if error.code == "forbidden"));
        assert_eq!(read_options(&f.home).unwrap(), projection.previous);
        assert_eq!(f.runtime.read().state().revision, revision);
        assert!(!f.home.join(PENDING).exists());
    }

    #[test]
    fn the_committed_file_is_reconciled_before_the_owner_state_is_published() {
        let f = fixture(); let (command, projection) = change(&f);
        let home = f.home.clone(); let target = projection.target.clone();
        let database = f.runtime.store.lock().unwrap().conn.path().unwrap().to_owned();
        let (sent, received) = std::sync::mpsc::channel();
        f.runtime.before_owner_publish(move || {
            assert_eq!(read_options(&home).unwrap().as_deref(), Some(target.as_str()));
            assert!(!home.join(PENDING).exists());
            let db = rusqlite::Connection::open_with_flags(&database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            assert_eq!(db.query_row("SELECT count(*) FROM settings_projection_receipts", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
            sent.send(()).unwrap();
        });
        apply(&f.runtime, &f.home, &user(), command, projection.previous, projection.target, vec![]).unwrap();
        received.try_recv().unwrap();
    }

    #[test]
    fn an_external_edit_is_preserved_and_reported_instead_of_overwritten() {
        let f = fixture();
        let (command, projection) = change(&f);
        assert!(stage_file(&f, &command, &projection, false).is_err());
        fs::write(alondra_core::options::path_in(&f.home), "# an external edit\n").unwrap();
        assert!(recover(&f.runtime, &f.home).unwrap_err().to_string().contains("edited outside"));
        assert_eq!(fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap(), "# an external edit\n");
        assert!(pending(&f.home));
    }

    #[test]
    fn a_changed_semantic_digest_cannot_reuse_an_unrelated_committed_receipt() {
        let f = fixture();
        let (command, mut projection) = change(&f);
        stage_file(&f, &command, &projection, true).unwrap();
        projection.identity.command_digest[0] ^= 1;
        assert!(!committed(&f.runtime.store.lock().unwrap(), &projection.identity).unwrap());
        recover(&f.runtime, &f.home).unwrap();
        assert_eq!(read_options(&f.home).unwrap().as_deref(), Some(projection.target.as_str()));
    }
}
