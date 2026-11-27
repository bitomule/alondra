//! The accounts' tests (`accounts`) that start nodes through the SDK.
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountsConfig, Policy, Provider};
use alondra_domain_model::Actor;
use serde_json::json;
use crate::domain_runtime::DomainRuntime;
use alondra_daemon_engine::accounts::*;
use alondra_daemon_engine::accounts::fixtures::*;
use std::os::unix::fs::PermissionsExt as _;

fn fixture() -> Fixture {
    fixture_with(hosts(), std::sync::Arc::new(alondra_engine_doubles::accounts::Accounts::default()))
}

/// Doubled hosts whose Claude Code answers its sign-in commands as the fixture's fake `claude` does.
fn hosts() -> Arc<dyn alondra_engine_ports::agent_host::AgentHost> {
    use alondra_engine_ports::agent_host::ProbeOutput;
    let hosts = alondra_engine_doubles::agent_host::InProcessHosts::default();
    hosts.set_probe(|run| {
        let args: Vec<String> = run.command.get_args().map(|arg| arg.to_string_lossy().into_owned()).collect();
        let env: BTreeMap<String, String> = run.command.get_envs().filter_map(|(key, value)| Some((key.to_string_lossy().into_owned(), value?.to_string_lossy().into_owned()))).collect();
        let home = PathBuf::from(env.get("HOME").cloned().unwrap_or_default());
        let folder = env.get("CLAUDE_CONFIG_DIR").map_or_else(|| home.join(".claude"), PathBuf::from);
        let answer = |success: bool, stdout: String| Ok(ProbeOutput { success, stdout: stdout.into_bytes(), stderr: Vec::new() });
        match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
            ["auth", "status", ..] if env.contains_key("ANTHROPIC_API_KEY") => answer(true, json!({"loggedIn": true, "authMethod": "api_key"}).to_string()),
            ["auth", "status", ..] => match std::fs::read_to_string(folder.join("signed-in")) {
                Ok(email) => answer(true, json!({"loggedIn": true, "email": email, "subscriptionType": "max"}).to_string()),
                Err(_) => answer(false, json!({"loggedIn": false}).to_string()),
            },
            ["auth", "logout", ..] if home.join("refuse-logout").exists() => answer(false, String::new()),
            ["auth", "logout", ..] => {
                use std::io::Write as _;
                std::fs::OpenOptions::new().create(true).append(true).open(home.join("logouts"))?.write_all(format!("{}\n", folder.display()).as_bytes())?;
                let _ = std::fs::remove_file(folder.join("signed-in"));
                answer(true, String::new())
            }
            _ => answer(false, String::new()),
        }
    });
    Arc::new(hosts)
}

/// A native Codex whose digest the Codex backend checks: no test here runs it.
fn codex_native(path: &Path) {
    std::fs::write(path, "a native Codex no test here runs\n").unwrap();
}

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
        let domain = crate::engine_fixtures::open(&f.home).unwrap();
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
        assert!(matches!(refused, Err(ChangeRefused::Domain(ref e)) if e.why() == alondra_domain_model::Refusal::DefaultAccountStays { name: "Default".into() }), "{refused:?}");
        assert_eq!(std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap(), before, "a refused change writes nothing");
        let refused = change(&domain, &f.home, &Actor::System, &AccountChange::SetPolicy { policy: Policy::Margin });
        assert!(matches!(refused, Err(ChangeRefused::Domain(ref e)) if e.code == "forbidden"), "only the user changes the accounts: {refused:?}");
    }
    let restarted = crate::engine_fixtures::open(&f.home).unwrap();
    assert_eq!(restarted.read().state().account_policy, Policy::Chain, "the policy survives a restart");
    assert!(restarted.read().state().account_moves, "so does moving nodes");
    assert_eq!(order(&restarted), ["work", "default"]);
}

#[test]
fn the_actual_codex_metadata_check_keeps_external_auth_unavailable_and_api_keys_out_of_automatic_work() {
    super::wired();
    let f = fixture(); let domain = Arc::new(crate::engine_fixtures::open(&f.home).unwrap());
    let _hosts = super::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    codex_native(&native);
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone(); backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_backend::digest(&native).unwrap();
    std::fs::write(f.home.join("codex-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let accounts = alondra_engine_doubles::accounts::of(domain.ports());
    for (method, expected_state) in [("chatgptAuthTokens", AccountState::Unavailable), ("agentIdentity", AccountState::Unavailable), ("chatgpt", AccountState::SignedIn), ("apikey", AccountState::Unavailable)] {
        let mut report = alondra_engine_doubles::accounts::codex_fixture();
        report.auth_method = Some(method.into());
        report.pay_per_use = method == "apikey";
        if method != "chatgpt" { report.workspace_id = None; }
        accounts.answer_every_codex(Ok(report));
        check(&*domain, &f.home);
        let read = domain.read(); let account = &read.state().accounts[0];
        assert_eq!(account.provider, Provider::Codex); assert_eq!(account.state, expected_state);
        assert_eq!(account.email.as_deref(), Some("first@example.test"));
        if method == "chatgpt" { assert_eq!(account.native_identity.as_deref(), Some("fixture-workspace")); }
        if matches!(method, "chatgptAuthTokens" | "agentIdentity") { assert!(account.native_identity.is_none(), "external authentication cannot prove a managed workspace identity"); }
        let eligible = alondra_core::usage::eligible_account_keys(&read.state().account_order, &read.state().accounts, &read.state().account_usage, alondra_core::now_millis());
        assert_eq!(eligible.contains(&AccountKey::native(Provider::Codex)), method == "chatgpt");
        if method == "apikey" { assert_eq!(account.billing, AccountBilling::PayPerUse, "confirmed API-key mode wins over cached ChatGPT account/read shape"); }
    }
    let mut replaced = alondra_engine_doubles::accounts::codex_fixture();
    replaced.email = Some("second@example.test".into());
    replaced.workspace_id = Some("replacement-workspace".into());
    replaced.rate_limits = Some(json!({"accountId":"replacement-workspace","ordinaryUsageAllowed":true,"rateLimits":null}));
    replaced.catalog.models.clear();
    replaced.catalog.revision.clear();
    replaced.catalog.native_default = None;
    replaced.catalog_error = Some("Codex model list has no data array".into());
    accounts.answer_every_codex(Ok(replaced));
    check(&*domain, &f.home);
    {
        let read = domain.read(); let account = &read.state().accounts[0];
        assert_eq!(account.native_identity.as_deref(), Some("replacement-workspace"), "a failed catalog read never loses a proven native identity replacement");
        assert_eq!(account.email.as_deref(), Some("second@example.test"));
        assert_eq!(account.state, AccountState::SignedIn);
        assert!(read.state().model_catalogs.iter().all(|catalog| catalog.account != AccountKey::native(Provider::Codex)), "unknown metadata never republishes the previous identity's catalog");
    }
}

/// Codex refusing ordinary usage is what it reports while the account is out: each check records
/// that reading, so the accounts show its current percentage and the hold lasts only until Codex
/// allows usage again.
#[test]
fn a_codex_check_that_refuses_ordinary_usage_records_the_current_reading_and_its_limit() {
    super::wired();
    let f = fixture(); let domain = Arc::new(crate::engine_fixtures::open(&f.home).unwrap());
    let _hosts = super::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    codex_native(&native);
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone(); backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_backend::digest(&native).unwrap();
    std::fs::write(f.home.join("codex-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let accounts = alondra_engine_doubles::accounts::of(domain.ports());
    let answer = |allowed: serde_json::Value, used: u64| {
        let mut report = alondra_engine_doubles::accounts::codex_fixture();
        report.rate_limits = Some(json!({"accountId": "fixture-workspace", "ordinaryUsageAllowed": allowed, "rateLimits": {
            "primary": {"usedPercent": used, "windowDurationMins": 10080, "resetsAt": 4102444800_i64}, "secondary": null}, "rateLimitsByLimitId": null}));
        accounts.answer_every_codex(Ok(report));
        check(&*domain, &f.home);
        let read = domain.read();
        let usage = read.state().account_usage.iter().find(|usage| usage.provider == Provider::Codex).cloned().expect("Codex usage recorded");
        let eligible = alondra_core::usage::eligible_account_keys(&read.state().account_order, &read.state().accounts, &read.state().account_usage, alondra_core::now_millis())
            .contains(&AccountKey::native(Provider::Codex));
        let week = usage.windows.iter().find(|window| window.reading.kind.duration_minutes() == Some(10080)).expect("the week is read").reading.used;
        (week, alondra_view_model::accounts::native_limit_reported(Some(&usage)), eligible)
    };
    assert_eq!(answer(json!(true), 99), (alondra_core::usage::Share::of(0.99), false, true));
    assert_eq!(answer(json!(false), 100), (alondra_core::usage::Share::FULL, true, false), "a refusal replaces the older percentage and holds the account");
    assert_eq!(answer(serde_json::Value::Null, 3), (alondra_core::usage::Share::FULL, true, false), "an unknown permission leaves the last reading and its limit");
    assert_eq!(answer(json!(true), 2), (alondra_core::usage::Share::of(0.02), false, true), "Codex allowing usage again ends the limit");
}

#[tokio::test]
async fn a_stale_startup_account_check_retries_and_releases_account_selection() {
    super::wired();
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = fixture();
    let domain = Arc::new(crate::engine_fixtures::open(&f.home).unwrap());
    let _hosts = super::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    codex_native(&native);
    let options = alondra_core::options::path_in(&f.home);
    let edited = options.clone();
    alondra_engine_doubles::accounts::of(domain.ports()).during_next_codex_probe(move || {
        use std::io::Write as _;
        std::fs::OpenOptions::new().append(true).open(&edited).unwrap().write_all(b"\n# concurrent settings edit\n").unwrap();
    });
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone();
    backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_backend::digest(&native).unwrap();
    std::fs::write(f.home.join("codex-backend.json"), serde_json::to_vec(&backend).unwrap()).unwrap();
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Codex);
    std::fs::write(options, config.to_toml()).unwrap();
    begin_initial_check(&*domain);
    let attempts = std::cell::Cell::new(0);
    alondra_engine_ports::native_probe::retry_with(|| {
        attempts.set(attempts.get() + 1);
        let outcome = check_current(&*domain, &f.home);
        if attempts.get() == 1 {
            assert_eq!(outcome, alondra_engine_ports::native_probe::Outcome::Stale);
            assert!(require_initial_check(&*domain).is_err());
            assert!(domain.read().state().accounts.is_empty());
        }
        std::future::ready(outcome)
    }, |delay| {
        assert_eq!(delay, Duration::ZERO);
        std::future::ready(())
    }).await;
    assert_eq!(attempts.get(), 2);
    assert!(require_initial_check(&*domain).is_ok());
    let intent = TaskSelection::automatic(ModelProfile::Implementation);
    assert_eq!(select_for_launch(&*domain, &f.home, &intent, alondra_core::now_millis()).unwrap().account, AccountKey::native(Provider::Codex));
}
