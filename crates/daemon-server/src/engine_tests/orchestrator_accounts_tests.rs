//! The orchestrator over the whole daemon: the accounts it waits for and the usage it runs out of
//! are the engine's, read and measured as the daemon does.
use alondra_daemon_engine::live::LiveOf as _;
use alondra_daemon_engine::{accounts, usage_probe};
use alondra_domain_rules::orchestrator_batch::BATCH_PROBLEM_PREFIX;
use alondra_domain_rules::*;
use alondra_engine_orchestrator::domain_orchestrator::{self, testing::Fixture};
use alondra_engine_orchestrator::engine::OrchestratorOf as _;
use crate::domain_runtime::DomainRuntime;
use std::os::unix::fs::DirBuilderExt;
use std::sync::Arc;

/// A daemon home of its own, with every piece of the daemon, whose orchestrator runs through the SDK.
fn fixture() -> Fixture {
    super::wired();
    let root = crate::test_support::Directory::new();
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let domain = Arc::new(alondra_daemon_engine::wiring::open(&home).unwrap());
    alondra_engine_orchestrator::wire(domain.ports());
    Fixture::over(root, domain, home, alondra_engine_sdk::sdk_owner::fixtures::HostRoot::new())
}

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: id.into(), expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

/// A question from an Agent to the orchestrator: one orchestrator inbox message.
fn ask_orchestrator(f: &Fixture, n: u32) {
    f.ask(n, &format!("Where do I put change {n}?"));
}

fn outbox(domain: &DomainRuntime) -> Vec<(i64, String)> {
    domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|m| m.recipient_kind == "orchestrator").map(|m| (m.id, m.state)).collect()
}

/// The orchestrator's inbox without the reminders `settle` sent back to it.
fn unhandled(domain: &DomainRuntime) -> Vec<(i64, String)> {
    use alondra_domain_rules::orchestrator_cards::{UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX};
    domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|m| m.recipient_kind == "orchestrator" && m.interaction_id.as_ref()
            .is_none_or(|id| ![UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX].iter().any(|prefix| id.starts_with(prefix))))
        .map(|m| (m.id, m.state)).collect()
}

fn attempts(domain: &DomainRuntime) -> Vec<(i64, String, Option<String>)> {
    domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
        .filter(|i| i.recipient_kind == "orchestrator").map(|i| (i.message_id, i.state, i.reason)).collect()
}

fn accounts_seen(f: &Fixture, id: &str, state: alondra_core::accounts::AccountState) {
    use alondra_core::accounts::{AccountBilling, AccountId, AccountStatus, Provider};
    let accounts = vec![AccountStatus {
        provider: Provider::Claude, id: AccountId::default_account(), name: "default".into(), state,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None,
    }];
    command(&f.executor.domain, Actor::System, id, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
    if state == alondra_core::accounts::AccountState::SignedIn {
        let catalog = alondra_core::model_selection::AccountModelCatalog { account: alondra_core::accounts::AccountKey::native(Provider::Claude), revision: "fixture-models".into(), native_default: None, measured_at: alondra_core::now_millis(),
            models: vec![alondra_core::model_selection::CatalogModel { model: "opus".into(), display_name: "Native fixture model".into(), reasoning_efforts: vec![], default_reasoning_effort: None, context_window: Some(200000), input_modalities: vec!["text".into()], is_default: false, hidden: false }] };
        command(&f.executor.domain, Actor::System, &format!("{id}-models"), Command::RecordModelCatalog { catalog });
    }
}

#[tokio::test]
async fn without_a_signed_in_account_launches_wait_for_a_session_not_for_usage() {
    use alondra_core::accounts::AccountState;
    let f = fixture();
    let domain = &f.executor.domain;
    accounts_seen(&f, "accounts-out", AccountState::SignedOut);
    ask_orchestrator(&f, 1);
    f.tick().await;
    assert_eq!(f.starts().len(), 0, "no account can run the batch");
    assert!(domain.live().usage_pause.waits_for_session(), "launches wait for a session, not for usage: {:?}", domain.engine().usage_pause());
    f.tick().await;
    assert!(domain.live().usage_pause.waits_for_session(), "nothing but a session ends the wait");

    accounts_seen(&f, "accounts-in", AccountState::SignedIn);
    f.tick().await;
    assert_eq!(f.starts().len(), 1, "the account check seeing a session runs the batch at once");
    assert!(domain.engine().usage_pause().is_none(), "and nothing holds new work any more");
}

#[tokio::test]
async fn a_request_on_a_fresh_install_says_each_reason_it_waits_and_runs_once_claude_is_connected() {
    use alondra_core::accounts::{AccountKey, AccountsConfig, Provider};
    let f = fixture();
    let domain = &f.executor.domain;
    let options = alondra_core::options::path_in(&f.executor.home);
    let connected = std::fs::read_to_string(&options).unwrap();
    std::fs::write(&options, AccountsConfig::fresh().to_toml()).unwrap();
    accounts::begin_initial_check(domain);
    ask_orchestrator(&f, 1);

    f.tick().await;
    assert_eq!(f.starts().len(), 0);
    let checking = domain.engine().account_wait().reason().expect("the request waits");
    assert!(checking.contains("Accounts are being checked"), "{checking}");

    accounts::check(domain, &f.executor.home);
    f.tick().await;
    assert_eq!(f.starts().len(), 0, "no provider is connected");
    let unconnected = domain.engine().account_wait().reason().expect("the request still waits");
    assert_ne!(unconnected, checking, "the wait says why it waits now, not why it waited at startup");
    assert!(unconnected.contains("open Accounts to connect"), "{unconnected}");

    std::fs::write(&options, connected).unwrap();
    let profiles = alondra_core::options::OptionsFile::parse(&std::fs::read_to_string(&options).unwrap()).unwrap().model_profiles;
    let version = alondra_core::model_selection::profiles_version(&profiles);
    command(domain, Actor::System, "connected-profiles", Command::RecordModelProfiles { profiles, version });
    accounts_seen(&f, "connected", alondra_core::accounts::AccountState::SignedIn);
    command(domain, Actor::System, "connected-order", Command::RecordAccountConfiguration { order: vec![AccountKey::native(Provider::Claude)], version: "connected".into() });
    f.tick().await;
    assert_eq!(f.starts().len(), 1, "connecting Claude runs the request that waited");
    assert_eq!(domain.engine().account_wait().reason(), None);
}

#[tokio::test]
async fn claude_answering_while_no_account_has_a_session_checks_the_accounts_at_once() {
    use alondra_core::accounts::AccountState;
    let f = fixture();
    let domain = &f.executor.domain;
    let checked = || async { tokio::time::timeout(std::time::Duration::ZERO, domain.live().account_check.notified()).await.is_ok() };
    accounts::answered(domain);
    assert!(!checked().await, "with sessions to spare an answer asks for nothing");

    accounts_seen(&f, "accounts-out", AccountState::SignedOut);
    assert!(accounts::choose_for_launch(domain, &f.executor.home, alondra_core::now_millis()).is_none());
    accounts::answered(domain);
    assert!(checked().await, "an answer while waiting for a session checks the accounts now");
}

#[tokio::test]
async fn a_run_that_hits_the_limit_before_working_pauses_everything_and_is_retried_after_the_reset() {
    let f = fixture();
    f.mark_once("fixture:limited");
    let domain = &f.executor.domain;
    ask_orchestrator(&f, 1);
    let before = alondra_core::now_millis() / 1000;
    f.tick().await;
    let after = alondra_core::now_millis() / 1000;
    assert_eq!(f.starts().len(), 1);
    let until = domain.engine().usage_pause().map(|e| e.until).expect("usage pauses everything");
    assert!(until % 1000 == 0 && ((before + 3_600) * 1000..=(after + 3_600) * 1000).contains(&until),
        "until is the reset Claude reported, an hour after the run: {until}");
    let runtime = domain_orchestrator::load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none() && runtime.consecutive_failures == 0 && runtime.retry_at.is_none(), "{runtime:?}");
    assert_eq!(outbox(domain).iter().map(|(_, state)| state.as_str()).collect::<Vec<_>>(), ["retryable"]);
    assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(BATCH_PROBLEM_PREFIX)), "a limit is not a Problem for the user to decide");

    f.tick().await;
    assert_eq!(f.starts().len(), 1, "nothing runs until the reset");
    let usage = domain.read().state().account_usage.clone();
    assert!(usage.iter().any(|usage| usage.account.is_default() && usage.exhausted_until(alondra_core::now_millis()) == Some(until)),
        "the orchestrator's account ran out until the reset: {usage:?}");

    usage_probe::record(domain, &alondra_core::accounts::AccountId::default_account(), alondra_core::usage::UsageReading {
        windows: vec![alondra_core::usage::WindowReading { kind: alondra_core::usage::WindowKind::Session, used: alondra_core::usage::Share::of(0.1), resets_at: None, exhausted: false }],
        plan: None, complete: true,
    }, alondra_core::now_millis());
    domain.live().usage_pause.resume(alondra_core::now_millis());
    f.tick().await;
    assert_eq!(f.starts().len(), 2, "after the reset the probe finds room: the same batch is retried");
    assert!(unhandled(domain).is_empty());
    assert!(attempts(domain).iter().any(|(_, state, _)| state == "handed"));
}
