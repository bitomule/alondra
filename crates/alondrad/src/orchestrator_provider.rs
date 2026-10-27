use crate::domain_store::BatchOwner;
use alondra_core::accounts::AccountKey;
use alondra_core::model_selection::ModelSelection;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchSelection {
    pub account: AccountKey,
    #[serde(default)]
    pub account_status: Option<alondra_core::accounts::AccountStatus>,
    pub model: ModelSelection,
    pub policy_revision: String,
    pub reason: String,
    pub rejected_candidates: Vec<String>,
}

impl alondra_provider_claude::HostOwner for BatchOwner {
    fn owner_generation(&self) -> u64 { 1 }
    fn epoch(&self) -> &alondra_core::domain::DataEpoch { self.epoch() }
    fn host(&self) -> &str { self.host_name() }
    fn wire_execution(&self) -> alondra_provider_claude::Execution {
        alondra_provider_claude::Execution::Orchestrator(alondra_provider_claude::OrchestratorExecution {
            lane: alondra_provider_claude::OrchestratorLane::Orchestrator,
            run_id: self.run().to_owned(), conversation_id: self.session().to_owned(), generation: 1,
        })
    }
}

impl alondra_provider_codex::HostOwner for BatchOwner {
    fn owner_generation(&self) -> u64 { 1 }
    fn epoch(&self) -> &alondra_core::domain::DataEpoch { self.epoch() }
    fn host(&self) -> &str { self.host_name() }
    fn wire_execution(&self) -> alondra_provider_codex::Execution {
        alondra_provider_codex::Execution::Orchestrator(alondra_provider_codex::OrchestratorExecution {
            lane: alondra_provider_codex::OrchestratorLane::Orchestrator,
            run_id: self.run().to_owned(), conversation_id: self.session().to_owned(), generation: 1,
        })
    }
}


#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BatchHandoff {
    pub run_id: String,
    pub selection: BatchSelection,
    pub reason: String,
}

pub(crate) struct Batch {
    pub(crate) owner: BatchOwner,
    pub(crate) domain: std::sync::Arc<crate::domain_runtime::DomainRuntime>,
    pub(crate) backend: crate::sdk_owner::SdkBackend,
    pub(crate) home: std::path::PathBuf,
    pub(crate) directory: std::path::PathBuf,
    pub(crate) node_plugin: std::path::PathBuf,
    pub(crate) orchestrator_plugin: std::path::PathBuf,
    pub(crate) projects_root: Option<std::path::PathBuf>,
    pub(crate) capability: String,
    pub(crate) session: String,
    pub(crate) prompt: String,
    pub(crate) selection: BatchSelection,
}

pub(crate) fn selection(domain: &crate::domain_runtime::DomainRuntime, home: &std::path::Path, now: alondra_core::Millis) -> Result<BatchSelection, alondra_core::domain::DomainError> {
    use alondra_core::model_selection::{TaskSelection, ModelProfile};
    let before = domain.read();
    let policy_revision = format!("{}:{}", before.state().account_config_version, before.state().model_profiles_version);
    drop(before);
    let decision = crate::accounts::select_for_launch(domain, home, &TaskSelection::automatic(ModelProfile::Orchestrator), now)?;
    let read = domain.read();
    if policy_revision != format!("{}:{}",read.state().account_config_version,read.state().model_profiles_version) {
        return Err(alondra_core::domain::DomainError::new("stale_configuration","Account/model policy changed during selection"));
    }
    let account_status = read.state().accounts.iter().find(|status| status.provider == decision.account.provider && status.id == decision.account.account).cloned();
    Ok(BatchSelection { account_status, account: decision.account, model: decision.selection, reason: decision.reason, rejected_candidates: decision.rejected_candidates, policy_revision })
}

pub(crate) async fn execute(batch: Batch, spawned: impl FnOnce() -> anyhow::Result<()> + Send + 'static, cancel: impl std::future::Future<Output = ()>) -> crate::orchestrator_process::Output {
    match batch.selection.account.provider {
        alondra_core::accounts::Provider::Claude => crate::orchestrator_sdk::execute(batch, spawned, cancel).await,
        alondra_core::accounts::Provider::Codex => crate::orchestrator_codex::execute(batch, spawned, cancel).await,
    }
}


pub(crate) fn record_limit(domain: &crate::domain_runtime::DomainRuntime, selection: &BatchSelection, until: Option<alondra_core::Millis>, now: alondra_core::Millis) {
    use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
    use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
    let account = &selection.account;
    let Some(source) = selection.account_status.as_ref() else { return };
    let reset = until.or_else(|| (account.provider == alondra_core::accounts::Provider::Claude).then(|| domain.live.measured.reset_after_limit(&account.account, now)).flatten()).filter(|reset| *reset > now);
    let read = domain.read();
    let scope = match alondra_store::AuthorityScope::new(read.state().data_epoch.clone()) { Ok(scope) => scope, Err(_) => return };
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(),
        command_id: format!("native-limit-{}", crate::auth::random_hex(8)).into(), expected_revision: 0,
        command: Command::RecordAccountUsage { provider: account.provider, account: account.account.clone(), at: now,
            day: crate::node_reports::local_day(now), reading: UsageReading { plan: None, complete: false,
                windows: vec![WindowReading { kind: if account.provider == alondra_core::accounts::Provider::Codex { WindowKind::Other("codex:account:limit".into()) } else { WindowKind::Session }, used: if account.provider == alondra_core::accounts::Provider::Codex { Share::NONE } else { Share::FULL }, resets_at: reset, exhausted: true }] } },
    };
    drop(read);
    let result = domain.own(scope, |owner| {
        let current = owner.state().accounts.iter().find(|status| status.provider == account.provider && status.id == account.account);
        if !current.is_some_and(|current| current.sign_ins == source.sign_ins && current.email == source.email && current.billing == source.billing && current.native_identity == source.native_identity) { return Ok(false) }
        owner.stage_command(&Actor::System, &envelope, crate::domain_store::Attached::None)?;
        Ok(true)
    });
    match result {
        Ok((true, _)) => crate::usage_probe::follow(domain, now),
        Ok((false, _)) => {},
        Err(error) => log!("Cannot record the native account limit: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::accounts::{AccountId, AccountState, AccountStatus, Provider};
    use alondra_core::domain::{Command, CommandEnvelope, PROTOCOL_VERSION};
    use alondra_core::model_selection::ModelKey;

    #[test]
    fn a_late_batch_limit_never_marks_replaced_credentials_exhausted() {
        for provider in [Provider::Claude, Provider::Codex] {
            let directory = crate::test_support::Directory::new();
            std::fs::create_dir_all(&directory).unwrap();
            let domain = crate::domain_runtime::DomainRuntime::open(&directory).unwrap();
            let publish = |email: &str| {
                let account = AccountStatus { provider, id: AccountId::default_account(), name: "Native account".into(), state: AccountState::SignedIn,
                    native_identity: None, email: Some(email.into()), plan: None, billing: Default::default(), sign_ins: 0 };
                let state = domain.read();
                let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.state().data_epoch.clone(),
                    command_id: crate::auth::random_hex(16).into(), expected_revision: 0,
                    command: Command::RecordAccounts { accounts: vec![account], reserve: alondra_core::usage::default_reserve() } };
                drop(state); domain.execute_as_system(envelope).unwrap();
            };
            publish("first@example.invalid");
            let mut batch = BatchSelection { account: AccountKey::native(provider), account_status: Some(domain.read().state().accounts[0].clone()),
                model: ModelSelection { model: ModelKey { provider, model: "native-model".into() }, reasoning_effort: None, catalog_revision: "fixture".into() },
                policy_revision: "fixture".into(), reason: "Fixture".into(), rejected_candidates: vec![] };
            publish("replacement@example.invalid");
            record_limit(&domain, &batch, Some(200), 100);
            assert!(domain.read().state().account_usage.is_empty(), "old native errors cannot alter a replacement identity");
            batch.account_status = Some(domain.read().state().accounts[0].clone());
            record_limit(&domain, &batch, Some(200), 101);
            assert!(domain.read().state().account_usage[0].windows.iter().any(|window| window.reading.exhausted), "current native limit evidence remains effective");
            drop(domain);
            let reopened = crate::domain_runtime::DomainRuntime::open(&directory).unwrap();
            assert_eq!(reopened.read().state().accounts[0].email.as_deref(), Some("replacement@example.invalid"));
            assert!(reopened.read().state().account_usage[0].windows.iter().any(|window| window.reading.exhausted));
        }
    }
}
