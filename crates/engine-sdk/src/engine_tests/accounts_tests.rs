//! The accounts' tests (`accounts`) that start nodes through the SDK.
use alondra_core::clock;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountStatus, AccountsConfig, Policy, Provider};
use alondra_core::claude_account;
use alondra_domain_rules::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use alondra_core::usage::Share;
use alondra_view_model::Lang;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;
use alondra_daemon_engine::reply::Reply;
use crate::domain_runtime::DomainRuntime;
use alondra_daemon_engine::accounts::*;
use alondra_daemon_engine::accounts::tests::*;
use crate::test_support::Directory;
use alondra_core::accounts::{Distribution, ProviderAccount};
use std::os::unix::fs::PermissionsExt as _;

/// A Settings button writes `options.toml`, then the domain records the change, and it survives a
/// restart; one its rules refuse leaves the file as it was.
#[test]
fn a_settings_change_writes_the_file_and_the_domain_and_survives_a_restart() {
    super::wired();
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    let user = Actor::User { user_id: "device:test".into() };
    let order = |domain: &DomainRuntime| domain.read().state().accounts.iter().map(|a| a.id.to_string()).collect::<Vec<_>>();
    {
        let domain = crate::wiring::open(&f.home).unwrap();
        check(&domain, &f.home);
        assert_eq!(order(&domain), ["default", "work"]);

        change(&domain, &f.home, &user, &AccountChange::MoveUp { account: AccountId::parse("work").unwrap() }).unwrap();
        let file = config(&f.home).unwrap();
        assert_eq!(file.accounts(Provider::Claude).iter().map(|a| a.id.to_string()).collect::<Vec<_>>(), ["work", "default"]);
        assert_eq!(order(&domain), ["work", "default"]);

        change(&domain, &f.home, &user, &AccountChange::SetPolicy { policy: Policy::Chain }).unwrap();
        assert_eq!(config(&f.home).unwrap().distribution.policy, Policy::Chain);
        change(&domain, &f.home, &user, &AccountChange::SetMoveWhenExhausted { on: true }).unwrap();
        assert!(config(&f.home).unwrap().distribution.move_when_exhausted, "the file the move reads says so");

        let before = std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap();
        let refused = change(&domain, &f.home, &user, &AccountChange::Remove { account: AccountId::default_account() });
        assert!(matches!(refused, Err(ChangeRefused::Domain(ref e)) if e.why() == alondra_domain_rules::Refusal::DefaultAccountStays { name: "Default".into() }), "{refused:?}");
        assert_eq!(std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap(), before, "a refused change writes nothing");
        let refused = change(&domain, &f.home, &Actor::System, &AccountChange::SetPolicy { policy: Policy::Margin });
        assert!(matches!(refused, Err(ChangeRefused::Domain(ref e)) if e.code == "forbidden"), "only the user changes the accounts: {refused:?}");
    }
    let restarted = crate::wiring::open(&f.home).unwrap();
    assert_eq!(restarted.read().state().account_policy, Policy::Chain, "the policy survives a restart");
    assert!(restarted.read().state().account_moves, "so does moving nodes");
    assert_eq!(order(&restarted), ["work", "default"]);
}

#[test]
fn the_actual_codex_metadata_check_keeps_external_auth_unavailable_and_api_keys_out_of_automatic_work() {
    super::wired();
    let f = fixture(); let domain = Arc::new(crate::wiring::open(&f.home).unwrap());
    let _hosts = crate::sdk_owner::tests::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    std::fs::copy(std::env::var("FIXTURE_CODEX_NATIVE").unwrap(), &native).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone(); backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_backend::digest(&native).unwrap();
    std::fs::write(f.home.join("codex-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let sidecar = PathBuf::from(format!("{}.account.json", native.display()));
    for (method, expected_state) in [("chatgptAuthTokens", AccountState::Unavailable), ("agentIdentity", AccountState::Unavailable), ("chatgpt", AccountState::SignedIn), ("apikey", AccountState::Unavailable)] {
        std::fs::write(&sidecar, serde_json::to_vec(&json!({"trace":true,"authStatus":{"authMethod":method,"authToken":null,"requiresOpenaiAuth":true}})).unwrap()).unwrap();
        check(&domain, &f.home);
        let read = domain.read(); let account = &read.state().accounts[0];
        assert_eq!(account.provider, Provider::Codex); assert_eq!(account.state, expected_state);
        assert_eq!(account.email.as_deref(), Some("first@example.test"));
        if method == "chatgpt" { assert_eq!(account.native_identity.as_deref(), Some("fixture-workspace")); }
        if matches!(method, "chatgptAuthTokens" | "agentIdentity") { assert!(account.native_identity.is_none(), "external authentication cannot prove a managed workspace identity"); }
        let eligible = alondra_core::usage::eligible_account_keys(&read.state().account_order, &read.state().accounts, &read.state().account_usage, alondra_core::now_millis());
        assert_eq!(eligible.contains(&AccountKey::native(Provider::Codex)), method == "chatgpt");
        if method == "apikey" { assert_eq!(account.billing, AccountBilling::PayPerUse, "confirmed API-key mode wins over cached ChatGPT account/read shape"); }
    }
    std::fs::write(&sidecar, serde_json::to_vec(&json!({"trace":true,"accountRead":{"account":{"type":"chatgpt","email":"second@example.test","planType":"plus"},"requiresOpenaiAuth":true},"rateLimits":{"accountId":"replacement-workspace","ordinaryUsageAllowed":true,"rateLimits":null},"models":{"data":"invalid-native-shape"}})).unwrap()).unwrap();
    check(&domain, &f.home);
    {
        let read = domain.read(); let account = &read.state().accounts[0];
        assert_eq!(account.native_identity.as_deref(), Some("replacement-workspace"), "a failed catalog read never loses a proven native identity replacement");
        assert_eq!(account.email.as_deref(), Some("second@example.test"));
        assert_eq!(account.state, AccountState::SignedIn);
        assert!(read.state().model_catalogs.iter().all(|catalog| catalog.account != AccountKey::native(Provider::Codex)), "unknown metadata never republishes the previous identity's catalog");
    }
    let calls = std::fs::read_to_string(format!("{}.calls", sidecar.display())).unwrap();
    assert!(calls.lines().map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap()).all(|call| !matches!(call["method"].as_str(), Some("thread/start" | "thread/resume" | "turn/start"))));
}

#[tokio::test]
async fn a_stale_startup_account_check_retries_and_releases_account_selection() {
    super::wired();
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = fixture();
    let domain = Arc::new(crate::wiring::open(&f.home).unwrap());
    let _hosts = crate::sdk_owner::tests::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    let original = std::fs::read_to_string(std::env::var("FIXTURE_CODEX_NATIVE").unwrap()).unwrap();
    let options = alondra_core::options::path_in(&f.home);
    let edit = format!("\nimport pathlib\noptions = pathlib.Path({:?})\nmarker = pathlib.Path(__file__ + '.attempts')\nif not marker.exists():\n    with options.open('a') as output: output.write('\\n# concurrent settings edit\\n')\nwith marker.open('a') as output: output.write('x')\n", options.display().to_string());
    std::fs::write(&native, original.replacen("import json, os, sys, subprocess, threading", &format!("import json, os, sys, subprocess, threading{edit}"), 1)).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone();
    backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_backend::digest(&native).unwrap();
    std::fs::write(f.home.join("codex-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Codex);
    std::fs::write(options, config.to_toml()).unwrap();
    begin_initial_check(&domain);
    let attempts = std::cell::Cell::new(0);
    crate::native_probe::retry_with(|| {
        attempts.set(attempts.get() + 1);
        let outcome = check_current(&domain, &f.home);
        if attempts.get() == 1 {
            assert_eq!(outcome, crate::native_probe::Outcome::Stale);
            assert!(require_initial_check(&domain).is_err());
            assert!(domain.read().state().accounts.is_empty());
        }
        std::future::ready(outcome)
    }, |delay| {
        assert_eq!(delay, Duration::ZERO);
        std::future::ready(())
    }).await;
    assert_eq!(attempts.get(), 2);
    assert!(require_initial_check(&domain).is_ok());
    let intent = TaskSelection::automatic(ModelProfile::Implementation);
    assert_eq!(select_for_launch(&domain, &f.home, &intent, alondra_core::now_millis()).unwrap().account, AccountKey::native(Provider::Codex));
}
