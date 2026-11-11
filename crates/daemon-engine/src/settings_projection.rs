use crate::domain_store::provider_settings::{PendingProjection, ProjectionIdentity, committed, prepare, read_optional, recover_store, replace};
use crate::domain_store::{Attached, StoreError};
use crate::domain_runtime::DomainRuntime;
use crate::locks::Poisonable as _;
use alondra_core::domain::{Actor, Command, CommandEnvelope, DomainError, PROTOCOL_VERSION};
use anyhow::Result;
use std::path::Path;

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
    use std::{fs, path::PathBuf};
    use alondra_daemon_engine_model::options::{PENDING, pending, read_options};
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
            native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None,
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
