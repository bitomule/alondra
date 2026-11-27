use crate::domain_runtime::DomainRuntime;
use super::*;
use alondra_engine_kernel::accounts_file::options_file;
use crate::test_support::Directory;
use alondra_core::accounts::{Distribution, Policy, ProviderAccount};
use std::os::unix::fs::PermissionsExt as _;
use super::fixtures::*;

fn fixture() -> Fixture {
    fixture_with(
        std::sync::Arc::new(alondra_agent_runtime::owned_host::ProcessHosts::new(Default::default())),
        std::sync::Arc::new(alondra_engine_doubles::accounts::Accounts::default()),
    )
}

#[test]
fn codex_login_credential_destination_matches_the_native_working_directory() {
    let user_home = PathBuf::from("/fixture-user");
    let mut native = Codex { accounts: std::sync::Arc::new(alondra_engine_doubles::accounts::Accounts::default()),
        hosts: std::sync::Arc::new(alondra_agent_runtime::owned_host::ProcessHosts::new(Default::default())), executable: "/fixture-codex".into(), environment: BTreeMap::new(),
        user_home: user_home.clone(), path: "/usr/bin:/bin".into(), username: "fixture".into() };
    let account = AccountKey::native(Provider::Codex);
    for (configured, expected) in [(None, user_home.join(".codex")), (Some(""), user_home.join(".codex")), (Some("/absolute-codex"), PathBuf::from("/absolute-codex")),
        (Some("relative-codex"), user_home.join("relative-codex"))] {
        native.environment.remove("CODEX_HOME");
        if let Some(profile) = configured { native.environment.insert("CODEX_HOME".into(), profile.into()); }
        assert_eq!(native.command(None).get_current_dir(), Some(user_home.as_path()));
        assert_eq!(native.profile(Path::new("/fixture-service"), &account), expected);
    }
}

#[test]
fn claude_account_probes_keep_the_native_deadline_for_installed_and_pinned_backends() {
    let f = fixture();
    assert_eq!(f.claude.timeout, None, "the finite shell fixture waits for completion");
    for installed_native in [false, true] {
        let domain = Arc::new(crate::wiring::open(&f.home).unwrap());
        let native = f.claude.executable.clone();
        domain.enable_sdk(crate::sdk_backend::SdkBackend {
            provider: Provider::Claude, origin: None,
            node: native.clone(), node_sha256: String::new(),
            host: native.clone(), host_sha256: String::new(),
            native, native_sha256: String::new(),
            host_environment: BTreeMap::new(),
            native_environment: BTreeMap::from([("HOME".into(), f.user_home.display().to_string()), ("PATH".into(), f.claude.path.clone())]),
            confinement: None, state_root: f.home.join("hosts"), io_timeout_seconds: 30, installed_native, release: crate::sdk_backend::this_release(),
        });
        let claude = Claude::of(&*domain).unwrap();
        assert_eq!(claude.executable, f.claude.executable);
        assert_eq!(claude.timeout, Some(Duration::from_secs(10)), "native probes stay bounded, including pinned custom binaries");
    }
}

#[test]
fn a_fresh_home_has_no_connected_provider_even_when_native_claude_is_signed_in() {
    let f = fixture();
    std::fs::remove_file(alondra_core::options::path_in(&f.home)).unwrap();
    let fresh = config(&f.home).unwrap();
    assert_eq!(fresh.connected, Some(vec![]));
    assert!(fresh.ordered_accounts().is_empty());
    assert!(f.user_home.join(".claude/signed-in").is_file());
    let mut codex = fresh;
    codex.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&f.home), codex.to_toml()).unwrap();
    let configured = config(&f.home).unwrap();
    assert!(configured.accounts(Provider::Claude).is_empty());
    assert_eq!(configured.accounts(Provider::Codex).len(), 1);
}

#[test]
fn legacy_migration_preserves_policy_and_custom_profiles_and_seeds_only_the_exact_old_orchestrator_model_once() {
    use alondra_core::model_selection::{ModelCandidate, ModelProfile, RoleModelPolicy};
    let f = fixture();
    let mut legacy = with(vec![work()]);
    legacy.distribution.reserve = 0.73;
    legacy.distribution.move_when_exhausted = true;
    legacy.set_order(vec![AccountKey::new(Provider::Claude, work().id), AccountKey::native(Provider::Claude)]).unwrap();
    let text = alondra_core::accounts::with_accounts("# user configuration\n[models]\nopus=\"user-mapped-opus\"\n", &legacy).unwrap();
    let review = RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: "user-review-model".into(), reasoning_effort: None }] };
    let profiles = std::collections::BTreeMap::from([(Provider::Claude, std::collections::BTreeMap::from([(ModelProfile::Review, review.clone())]))]);
    std::fs::write(alondra_core::options::path_in(&f.home), alondra_core::options::with_model_profiles(&text, &profiles).unwrap()).unwrap();
    let runtime = crate::wiring::open(&f.home).unwrap();
    migrate_legacy_settings(&runtime, &f.home).unwrap();
    let after = options_file(&f.home).unwrap();
    assert_eq!(after.accounts.connected, Some(vec![Provider::Claude]));
    assert_eq!(after.accounts.distribution, legacy.distribution);
    assert_eq!(after.model_profiles[&Provider::Claude][&ModelProfile::Review], review);
    assert_eq!(after.model_profiles[&Provider::Claude][&ModelProfile::Orchestrator], RoleModelPolicy::OrderedCandidates {
        candidates: vec![ModelCandidate { model: "opus".into(), reasoning_effort: None }] });
    assert_eq!(after.models["opus"], "user-mapped-opus");
    let revision = runtime.read().state().revision;
    let bytes = std::fs::read(alondra_core::options::path_in(&f.home)).unwrap();
    migrate_legacy_settings(&runtime, &f.home).unwrap();
    assert_eq!(std::fs::read(alondra_core::options::path_in(&f.home)).unwrap(), bytes);
    assert_eq!(runtime.read().state().revision, revision);
}

#[test]
fn first_install_and_explicit_codex_only_configuration_never_seed_a_claude_account_or_profile() {
    let f = fixture();
    std::fs::remove_file(alondra_core::options::path_in(&f.home)).unwrap();
    let runtime = crate::wiring::open(&f.home).unwrap();
    migrate_legacy_settings(&runtime, &f.home).unwrap();
    assert!(!alondra_core::options::path_in(&f.home).exists());
    let mut codex = alondra_core::options::OptionsFile::fresh(); codex.accounts.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&f.home), codex.accounts.to_toml()).unwrap();
    let bytes = std::fs::read(alondra_core::options::path_in(&f.home)).unwrap();
    migrate_legacy_settings(&runtime, &f.home).unwrap();
    assert_eq!(std::fs::read(alondra_core::options::path_in(&f.home)).unwrap(), bytes);
    assert_eq!(options_file(&f.home).unwrap().accounts.connected, Some(vec![Provider::Codex]));
    assert!(options_file(&f.home).unwrap().model_profiles.is_empty());
}

#[test]
fn adding_an_account_writes_the_section_and_prepares_its_folder_linked_to_claude() {
    let f = fixture();
    std::fs::write(alondra_core::options::path_in(&f.home), "# mine\nmax_working_nodes = 2\n").unwrap();
    let before = f.claude_tree();
    f.store(&with(vec![work()])).unwrap();
    let text = std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap();
    assert!(text.starts_with("# mine\nmax_working_nodes = 2\n"), "{text}");
    assert_eq!(config(&f.home).unwrap(), with(vec![work()]));
    let folder = f.home.join("accounts/work");
    assert_eq!(std::fs::read_link(folder.join("skills")).unwrap(), f.user_home.join(".claude/skills"));
    assert_eq!(std::fs::read_link(folder.join("settings.json")).unwrap(), f.user_home.join(".claude/settings.json"));
    assert_eq!(f.claude_tree(), before, "~/.claude is never changed");
}

#[test]
fn a_change_made_against_an_older_section_is_refused_and_changes_nothing() {
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    let text = std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap();
    let stale = version(&AccountsConfig::default());
    let next = with(vec![work(), ProviderAccount { id: AccountId::parse("other").unwrap(), name: "Other".into(), ..work() }]);
    assert_eq!(store(&*f.accounts, &f.home, &f.user_home, Some(&f.claude), &stale, &next, &[]), Err(Refused::Stale));
    assert_eq!(std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap(), text);
    assert!(!f.home.join("accounts/other").exists());
    let broken = with(vec![work(), work()]);
    assert!(matches!(f.store(&broken), Err(Refused::Invalid(_))));
}

#[test]
fn each_account_is_seen_with_its_own_sign_in_email_and_plan() {
    let f = fixture();
    let disabled = ProviderAccount { id: AccountId::parse("off").unwrap(), name: "Off".into(), enabled: false, billing: AccountBilling::default() };
    let team = ProviderAccount {
        id: AccountId::parse("team").unwrap(), name: "Team".into(), enabled: true,
        billing: AccountBilling::Subscription { plan: Some("team".into()), weight: Some(5) },
    };
    let config = AccountsConfig { distribution: Distribution { policy: Policy::Chain, ..Distribution::default() }, claude: vec![work(), team, disabled], ..Default::default() };
    f.store(&config).unwrap();
    let state = |seen: &[AccountStatus]| seen.iter().map(|s| (s.id.to_string(), s.state, s.email.clone(), s.plan.clone())).collect::<Vec<_>>();
    let seen = observe(&f.home, Some(&f.claude), &config);
    assert_eq!(state(&seen), [
        ("default".into(), AccountState::SignedIn, Some("me@example.org".into()), Some("max".into())),
        ("work".into(), AccountState::SignedOut, None, None),
        ("team".into(), AccountState::SignedOut, None, Some("team".into())),
        ("off".into(), AccountState::Disabled, None, None),
    ]);
    std::fs::write(f.home.join("accounts/work/signed-in"), "work@example.org").unwrap();
    std::fs::write(f.home.join("accounts/team/signed-in"), "team@example.org").unwrap();
    std::fs::write(f.home.join("accounts/off/signed-in"), "off@example.org").unwrap();
    let seen = observe(&f.home, Some(&f.claude), &config);
    assert_eq!(state(&seen)[1..], [
        ("work".into(), AccountState::SignedIn, Some("work@example.org".into()), Some("max".into())),
        ("team".into(), AccountState::SignedIn, Some("team@example.org".into()), Some("team".into()), ),
        ("off".into(), AccountState::Disabled, None, None),
    ], "a plan the user set wins over the reported one, and a disabled account is not checked");
    let unavailable = observe(&f.home, None, &config);
    assert!(unavailable[..3].iter().all(|s| s.state == AccountState::Unavailable));
}

#[test]
fn the_default_account_is_the_profile_the_daemon_was_set_up_with() {
    let mut f = fixture();
    let profile = f.user_home.join(".claude-work");
    std::fs::create_dir_all(profile.join("skills")).unwrap();
    f.claude.environment = BTreeMap::from([("CLAUDE_CONFIG_DIR".into(), profile.display().to_string())]);
    let config = with(vec![work()]);
    f.store(&config).unwrap();
    let default = |f: &Fixture| observe(&f.home, Some(&f.claude), &config)[0].clone();
    assert_eq!(default(&f).state, AccountState::SignedOut, "~/.claude's sign-in is not the one nodes run with");
    std::fs::write(profile.join("signed-in"), "work-profile@example.org").unwrap();
    assert_eq!(default(&f).email.as_deref(), Some("work-profile@example.org"));
    assert_eq!(std::fs::read_link(f.home.join("accounts/work/skills")).unwrap(), profile.join("skills"), "an added account shares what that profile has");
}

#[test]
fn a_daemon_environment_naming_an_added_account_leaves_each_account_on_its_own_profile() {
    let mut f = fixture();
    let config = with(vec![work()]);
    f.store(&config).unwrap();
    let added = f.home.join("accounts/work");
    std::fs::write(added.join("signed-in"), "work@example.org").unwrap();
    f.claude.environment = BTreeMap::from([("HOME".into(), f.user_home.display().to_string()),
        ("CLAUDE_CONFIG_DIR".into(), added.display().to_string()), ("CLAUDE_HOME".into(), added.display().to_string())]);
    let seen = observe(&f.home, Some(&f.claude), &config);
    assert_eq!(seen.iter().map(|s| s.email.as_deref()).collect::<Vec<_>>(), [Some("me@example.org"), Some("work@example.org")]);
}

#[test]
fn accounts_sharing_one_email_are_checked_before_their_sessions_are_taken_over() {
    let f = fixture();
    let config = with(vec![work()]);
    f.store(&config).unwrap();
    std::fs::write(f.home.join("accounts/work/signed-in"), "work@example.org").unwrap();
    let domain = Arc::new(crate::wiring::open(&f.home).unwrap());
    domain.live().unbounded_claude_status.store(true, std::sync::atomic::Ordering::Relaxed);
    let native = f.claude.executable.clone();
    domain.enable_sdk(crate::sdk_backend::SdkBackend {
        provider: Provider::Claude, origin: None,
        node: native.clone(), node_sha256: String::new(),
        host: native.clone(), host_sha256: String::new(),
        native, native_sha256: String::new(),
        host_environment: BTreeMap::new(),
        native_environment: BTreeMap::from([("HOME".into(), f.user_home.display().to_string()), ("PATH".into(), f.claude.path.clone())]),
        confinement: None, state_root: f.home.join("hosts"), io_timeout_seconds: 30, installed_native: false, release: crate::sdk_backend::this_release(),
    });
    let stored = |email: &str| vec![probe_account(email), AccountStatus { id: AccountId::parse("work").unwrap(), ..probe_account("work@example.org") }];
    let default = || domain.read().state().accounts[0].email.clone();
    record_probe_fixture(&domain, stored("stale@example.org"));
    check_if_crossed(&*domain);
    assert_eq!(default().as_deref(), Some("stale@example.org"), "accounts with their own emails wait for the usual check");
    record_probe_fixture(&domain, stored("work@example.org"));
    check_if_crossed(&*domain);
    assert_eq!(default().as_deref(), Some("me@example.org"), "the default account answers for its own profile");
}

#[test]
fn only_the_account_that_wore_anothers_email_is_uncrossed() {
    let account = |id: &str, email: Option<&str>| AccountStatus { id: AccountId::parse(id).unwrap(), email: email.map(str::to_owned), ..probe_account("") };
    let key = |id: &str| AccountKey::new(Provider::Claude, AccountId::parse(id).unwrap());
    let shared = "work@example.org";
    assert_eq!(uncrossed(&[account("default", Some(shared)), account("work", Some(" WORK@example.org "))], &[account("default", Some("me@example.org")), account("work", Some(shared))]),
        [(key("default"), shared.to_owned())]);
    let same = [account("default", Some(shared)), account("work", Some(shared))];
    assert!(uncrossed(&same, &same).is_empty(), "two folders signed in to one account stay as they are");
    assert!(uncrossed(&same, &[account("default", None), account("work", Some(shared))]).is_empty(), "a check that saw no email proves nothing");
    assert!(uncrossed(&same, &[account("default", Some("a@example.org")), account("work", Some("b@example.org"))]).is_empty(), "with no account keeping it, it is nobody's identity");
    assert!(uncrossed(&[account("default", Some("a@example.org")), account("work", Some(shared))], &[account("default", Some("b@example.org")), account("work", Some(shared))]).is_empty(),
        "signing in to another identity is no crossing: its sessions stay refused");
}

#[test]
fn an_account_is_checked_with_the_environment_its_nodes_run_claude_with() {
    let mut f = fixture();
    std::fs::remove_file(f.user_home.join(".claude/signed-in")).unwrap();
    let config = AccountsConfig::default();
    assert_eq!(observe(&f.home, Some(&f.claude), &config)[0].state, AccountState::SignedOut);
    f.claude.environment = BTreeMap::from([("ANTHROPIC_API_KEY".into(), "sk-ant-fixture".into())]);
    assert_eq!(observe(&f.home, Some(&f.claude), &config)[0].state, AccountState::SignedIn,
        "nodes that sign in with their environment's key run on a signed-in account");
}

#[test]
fn removing_an_account_signs_it_out_and_deletes_its_folder_but_never_claudes() {
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    let folder = f.home.join("accounts/work");
    std::fs::write(folder.join("signed-in"), "work@example.org").unwrap();
    let before = f.claude_tree();
    f.store(&AccountsConfig::default()).unwrap();
    assert!(!folder.exists());
    assert_eq!(std::fs::read_to_string(f.user_home.join("logouts")).unwrap().trim(), folder.display().to_string());
    assert_eq!(f.claude_tree(), before);
    assert!(f.user_home.join(".claude/skills/mine").is_dir() && f.user_home.join(".claude/signed-in").is_file());
    assert_eq!(config(&f.home).unwrap(), AccountsConfig::default());
}

#[test]
fn an_account_that_cannot_be_signed_out_stays() {
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    let folder = f.home.join("accounts/work");
    std::fs::write(folder.join("signed-in"), "work@example.org").unwrap();
    std::fs::write(f.user_home.join("refuse-logout"), "").unwrap();
    assert!(matches!(f.store(&AccountsConfig::default()), Err(Refused::SignOut(_))));
    assert!(folder.join("signed-in").is_file());
    assert_eq!(config(&f.home).unwrap(), with(vec![work()]));
    let seen = version(&config(&f.home).unwrap());
    assert!(matches!(store(&*f.accounts, &f.home, &f.user_home, None, &seen, &AccountsConfig::default(), &[]), Err(Refused::SignOut(_))), "without Claude Code nothing signs it out");
}

#[test]
fn an_account_open_tasks_run_on_is_not_removed() {
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    std::fs::write(f.home.join("accounts/work/signed-in"), "work@example.org").unwrap();
    let seen = version(&config(&f.home).unwrap());
    let refused = store(&*f.accounts, &f.home, &f.user_home, Some(&f.claude), &seen, &with(vec![]), &[AccountId::parse("work").unwrap()]);
    assert_eq!(refused, Err(Refused::InUse("Work".into())));
    assert_eq!(config(&f.home).unwrap(), with(vec![work()]), "the section is as it was");
    assert!(f.home.join("accounts/work/signed-in").exists(), "still signed in, its folder kept");
    assert!(!f.user_home.join("logouts").exists(), "never signed out");
}


#[test]
fn the_default_account_can_be_disabled_while_another_stays_on_but_never_the_last_one() {
    let f = fixture();
    let default_off = ProviderAccount { enabled: false, ..ProviderAccount::implicit_default() };
    let only_work = with(vec![default_off.clone(), work()]);
    f.store(&only_work).unwrap();
    assert_eq!(config(&f.home).unwrap(), only_work);
    let none_on = with(vec![default_off, ProviderAccount { enabled: false, ..work() }]);
    assert_eq!(f.store(&none_on), Err(Refused::Invalid("At least one account stays on".into())));
    assert_eq!(config(&f.home).unwrap(), only_work, "the section is as it was");
}

#[test]
fn native_claude_api_key_auth_overrides_a_configured_subscription_before_automatic_selection() {
    let mut f = fixture();
    f.claude.environment.insert("ANTHROPIC_API_KEY".into(),"fake-api-key".into());
    let config = AccountsConfig::default();
    let seen = observe(&f.home,Some(&f.claude),&config);
    assert_eq!(seen[0].state,AccountState::SignedIn);
    assert_eq!(seen[0].billing,AccountBilling::PayPerUse);
    assert!(seen[0].plan.is_none());
}

#[test]
fn known_native_denials_filter_automatic_codex_before_choosing_a_compatible_claude_account() {
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelProfile, TaskSelection};
    let f = fixture(); let mut config = AccountsConfig::fresh(); config.connect(Provider::Codex); config.connect(Provider::Claude);
    let keys = vec![AccountKey::native(Provider::Codex), AccountKey::native(Provider::Claude)]; config.set_order(keys.clone()).unwrap();
    f.store(&config).unwrap(); let domain = crate::wiring::open(&f.home).unwrap();
    let statuses = keys.iter().map(|key| AccountStatus { antigravity: None, provider:key.provider,id:key.account.clone(),name:"Personal".into(),state:AccountState::SignedIn,native_identity: None, email:None,plan:None,billing:AccountBilling::default(),sign_ins: 0, profile: None }).collect();
    record_account_command(&domain,Command::RecordAccounts {accounts:statuses,reserve:alondra_core::usage::Share::of(config.distribution.reserve)});
    record_account_command(&domain,Command::RecordAccountConfiguration {order:keys.clone(),version:version(&config)});
    for key in keys {
        record_account_command(&domain,Command::RecordModelCatalog {catalog:AccountModelCatalog {account:key,revision:"native-fixture".into(),native_default:Some("fixture-model".into()),measured_at:100,
            models:vec![CatalogModel {model:"fixture-model".into(),display_name:"Fixture".into(),reasoning_efforts:vec![],default_reasoning_effort:None,context_window:None,input_modalities:vec!["text".into()],is_default:true,hidden:false}]}});
    }
    for (provider, used) in [(Provider::Codex, 0.1), (Provider::Claude, 0.5)] {
        record_account_command(&domain, Command::RecordAccountUsage { provider, account: AccountId::default_account(), at: 100, day: "2026-01-01".into(),
            reading: alondra_core::usage::UsageReading { plan: None, complete: true, windows: vec![alondra_core::usage::WindowReading {
                kind: alondra_core::usage::WindowKind::Session, used: Share::of(used), resets_at: Some(1_000), exhausted: false }] } });
    }
    let mut intent=TaskSelection::automatic(ModelProfile::Implementation);
    assert_eq!(select_for_launch(&domain,&f.home,&intent,100).unwrap().account.provider,Provider::Codex,"unknown initial policy resolves before admission");
    intent.native_tool_denials=Some(vec!["Write".into()]);
    assert_eq!(select_for_launch(&domain,&f.home,&intent,100).unwrap().account.provider,Provider::Claude,"known unsupported policy filters the first account instead of failing after choosing it");
    intent.native_tool_denials=Some(vec![]);
    assert_eq!(select_for_launch(&domain,&f.home,&intent,100).unwrap().account.provider,Provider::Codex);
}


fn probe_account(email: &str) -> AccountStatus {
    AccountStatus { antigravity: None, provider: Provider::Claude, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: Some(email.into()), plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None }
}

fn record_probe_fixture(domain: &DomainRuntime, accounts: Vec<AccountStatus>) {
    record_account_command(domain, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
}

fn record_account_command(domain: &DomainRuntime, command: Command) {
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: domain.read().state().data_epoch.clone(),
        command_id: format!("fixture-probe-{}", alondra_foundation::random_hex(16)).into(), expected_revision: 0, command };
    domain.execute_as_system(envelope).unwrap();
}

fn probe_catalog() -> Command {
    Command::RecordModelCatalog { catalog: alondra_core::model_selection::AccountModelCatalog { account: AccountKey::native(Provider::Claude),
        revision: "native-advertised".into(), models: vec![], native_default: None, measured_at: 2 } }
}

#[test]
fn a_delayed_native_probe_cannot_replace_a_new_credential_identity_or_its_metadata() {
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    let original = probe_account("first@example.invalid");
    record_probe_fixture(&domain, vec![original.clone()]);
    let (fence, _) = probe_fence(&domain, &f.home).unwrap();
    record_probe_fixture(&domain, vec![probe_account("second@example.invalid")]);
    let usage = Command::RecordAccountUsage { provider: Provider::Claude, account: AccountId::default_account(),
        reading: alondra_core::usage::UsageReading { windows: vec![alondra_core::usage::WindowReading { kind: alondra_core::usage::WindowKind::Weekly,
            used: Share::of(0.5), exhausted: false, resets_at: None }], plan: None, complete: false }, at: 2, day: "2026-10-26".into() };
    let error = publish_probe(&domain, &f.home, fence, vec![Command::RecordAccounts { accounts: vec![original], reserve: alondra_core::usage::default_reserve() }, probe_catalog(), usage]).unwrap_err();
    assert!(error.contains("changed"), "{error}");
    assert_eq!(domain.read().state().accounts[0].email.as_deref(), Some("second@example.invalid"));
    assert!(domain.read().state().account_usage.is_empty()); assert!(domain.read().state().model_catalogs.is_empty());
    drop(domain);
    let reopened = crate::wiring::open(&f.home).unwrap();
    assert_eq!(reopened.read().state().accounts[0].email.as_deref(), Some("second@example.invalid"));
    assert!(reopened.read().state().account_usage.is_empty()); assert!(reopened.read().state().model_catalogs.is_empty());
}

#[test]
fn a_native_metadata_bundle_rolls_back_all_results_when_a_later_command_is_refused() {
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap(); let account = probe_account("first@example.invalid");
    record_probe_fixture(&domain, vec![account.clone()]);
    let (fence, _) = probe_fence(&domain, &f.home).unwrap();
    let before = domain.read().state().clone();
    let error = publish_probe(&domain, &f.home, fence, vec![probe_catalog(), Command::RecordAccounts { accounts: vec![account.clone(), account], reserve: alondra_core::usage::default_reserve() }]).unwrap_err();
    assert!(error.contains("twice"), "{error}");
    assert_eq!(domain.read().state().revision, before.revision);
    assert!(domain.read().state().model_catalogs.is_empty());
    drop(domain); assert!(crate::wiring::open(&f.home).unwrap().read().state().model_catalogs.is_empty());
}

#[test]
fn a_native_probe_cannot_publish_across_an_unimported_options_file_edit() {
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    record_probe_fixture(&domain, vec![probe_account("first@example.invalid")]);
    let (fence, _) = probe_fence(&domain, &f.home).unwrap();
    let options = alondra_core::options::path_in(&f.home);
    let mut text = std::fs::read_to_string(&options).unwrap(); text.push_str("\n# external settings edit\n"); std::fs::write(options, text).unwrap();
    assert!(publish_probe(&domain, &f.home, fence, vec![probe_catalog()]).is_err());
    assert!(domain.read().state().model_catalogs.is_empty());
}


fn probe_usage(provider: Provider, at: i64, exhausted: bool) -> Command {
    Command::RecordAccountUsage { provider, account: AccountId::default_account(), reading: alondra_core::usage::UsageReading {
        windows: vec![alondra_core::usage::WindowReading { kind: alondra_core::usage::WindowKind::Weekly, used: Share::of(if exhausted { 1.0 } else { 0.2 }), exhausted, resets_at: None }],
        plan: None, complete: true }, at, day: "2026-10-26".into() }
}

#[test]
fn native_probe_publication_preserves_newer_quota_and_rolls_back_its_other_metadata() {
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    record_probe_fixture(&domain, vec![probe_account("first@example.invalid")]);
    let (newer, _) = probe_fence(&domain, &f.home).unwrap();
    publish_probe(&domain, &f.home, newer, vec![probe_usage(Provider::Claude, 10, true)]).unwrap();
    let (old_reading, _) = probe_fence(&domain, &f.home).unwrap();
    assert!(publish_probe(&domain, &f.home, old_reading, vec![probe_catalog(), probe_usage(Provider::Claude, 9, false)]).is_err());
    let read = domain.read();
    assert!(read.state().model_catalogs.is_empty(), "the bundle is atomic even when a later usage result is stale");
    assert_eq!(read.state().account_usage[0].measured_at(), Some(10));
    assert!(read.state().account_usage[0].windows[0].reading.exhausted);
}

#[test]
fn a_current_native_metadata_bundle_survives_unrelated_quota_progress_and_restart() {
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    record_probe_fixture(&domain, vec![probe_account("first@example.invalid")]);
    let (first, _) = probe_fence(&domain, &f.home).unwrap();
    let (other, _) = probe_fence(&domain, &f.home).unwrap();
    publish_probe(&domain, &f.home, other, vec![probe_usage(Provider::Codex, 20, true)]).unwrap();
    publish_probe(&domain, &f.home, first, vec![probe_catalog(), probe_usage(Provider::Claude, 2, false)]).unwrap();
    assert_eq!(domain.read().state().model_catalogs.len(), 1);
    assert_eq!(domain.read().state().account_usage.len(), 2);
    drop(domain);
    let reopened = crate::wiring::open(&f.home).unwrap();
    assert_eq!(reopened.read().state().model_catalogs.len(), 1);
    assert_eq!(reopened.read().state().account_usage.len(), 2);
}

#[test]
fn cached_authentication_cannot_launch_new_work_until_a_current_roster_is_committed() {
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelProfile, TaskSelection};
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    record_probe_fixture(&domain, vec![probe_account("cached@example.invalid")]);
    record_account_command(&domain, Command::RecordAccountConfiguration { order: vec![AccountKey::native(Provider::Claude)], version: version(&config(&f.home).unwrap()) });
    record_account_command(&domain, Command::RecordModelCatalog { catalog: AccountModelCatalog {
        account: AccountKey::native(Provider::Claude), revision: "cached-catalog".into(), native_default: Some("cached-model".into()), measured_at: 100,
        models: vec![CatalogModel { model: "cached-model".into(), display_name: "Cached model".into(), reasoning_efforts: vec![], default_reasoning_effort: None,
            context_window: None, input_modalities: vec!["text".into()], is_default: true, hidden: false }],
    } });
    let intent = TaskSelection::automatic(ModelProfile::Implementation);
    assert!(select_for_launch(&domain, &f.home, &intent, 100).is_ok());
    begin_initial_check(&domain);
    assert_eq!(select_for_launch(&domain, &f.home, &intent, 100).unwrap_err().code, "account_check_in_progress");
    assert_eq!(select_for_launch(&domain, &f.home, &alondra_core::model_selection::TaskSelection::automatic(alondra_core::model_selection::ModelProfile::Orchestrator), 100).unwrap_err().code, "account_check_in_progress");
    assert_eq!(domain.read().state().accounts[0].state, AccountState::SignedIn, "cached presentation remains readable while work waits");
    let (stale, _) = probe_fence(&domain, &f.home).unwrap();
    let path = alondra_core::options::path_in(&f.home);
    let mut text = std::fs::read_to_string(&path).unwrap(); text.push_str("\n# Concurrent settings edit\n"); std::fs::write(path, text).unwrap();
    assert!(publish_probe(&domain, &f.home, stale, vec![]).is_err());
    assert!(require_initial_check(&domain).is_err(), "discarded metadata cannot unlock cached credentials");
    check(&domain, &f.home);
    assert!(require_initial_check(&domain).is_ok(), "the complete fresh roster was committed");
    assert_eq!(domain.read().state().accounts[0].state, AccountState::Unavailable, "without a current native runtime, cached sign-in is not authority to launch");
    assert_eq!(select_for_launch(&domain, &f.home, &intent, 101).unwrap_err().code, "account_unavailable");
}

#[test]
fn native_workspace_identity_survives_the_existing_account_store_without_a_parallel_registry() {
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    let mut account = probe_account("first@example.invalid"); account.provider = Provider::Codex; account.native_identity = Some("native-workspace".into());
    record_probe_fixture(&domain, vec![account.clone()]);
    drop(domain);
    let reopened = crate::wiring::open(&f.home).unwrap();
    assert_eq!(reopened.read().state().accounts, [account]);
}


#[test]
fn absent_native_workspace_never_proves_existing_session_identity() {
    let native = NativeAuthAccount { auth_method: "chatgpt".into(), email: Some("first@example.test".into()), workspace_id: None };
    let report = alondra_core::codex_account::ProbeReport { requires_auth: true, auth_method: Some("chatgpt".into()), account_type: Some("chatgpt".into()), workspace_id: None, signed_in: true, email: Some("first@example.test".into()), plan: Some("plus".into()), pay_per_use: false, rate_limits: None, catalog_error: None,
        catalog: alondra_core::model_selection::AccountModelCatalog { account: AccountKey::native(Provider::Codex), revision: String::new(), models: vec![], native_default: None, measured_at: 0 } };
    assert!(!native.matches(&report), "matching absence is not positive workspace proof");
    assert!(report.signed_in && report.supported_authentication(), "optional identity is independent from first-node authentication");
}


#[tokio::test]
async fn a_stale_model_catalog_retries_the_account_check_until_models_are_ready() {
    let f = fixture();
    let domain = Arc::new(crate::wiring::open(&f.home).unwrap());
    domain.live().unbounded_claude_status.store(true, std::sync::atomic::Ordering::Relaxed);
    let state = crate::host_fixtures::HostRoot::new();
    let (mut backend, _) = crate::host_fixtures::fixture_backend(&f.home, &state);
    std::fs::write(&backend.native, r#"#!/usr/bin/python3
import json, os, pathlib, sys
if sys.argv[1:3] == ['auth', 'status']:
    print(json.dumps(dict(loggedIn=True, email='me@example.org', subscriptionType='max')))
    sys.exit(0)
for raw in sys.stdin:
    message = json.loads(raw)
    assert message['request']['subtype'] == 'initialize'
    marker = pathlib.Path(os.environ['HOME']) / 'model-probed'
    if not marker.exists():
        with open(os.environ['STALE_MODEL_OPTIONS'], 'a') as options:
            options.write('\n# settings changed during model read\n')
        marker.write_text('')
    response = dict(commands=[], models=[dict(value='opus', displayName='Opus', description='Fixture model')], output_style='default', account={})
    print(json.dumps(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=response))), flush=True)
"#).unwrap();
    backend.native_sha256 = crate::sdk_backend::digest(&backend.native).unwrap();
    backend.native_environment.insert("HOME".into(), f.user_home.display().to_string());
    backend.native_environment.insert("STALE_MODEL_OPTIONS".into(), alondra_core::options::path_in(&f.home).display().to_string());
    domain.enable_sdk(backend);
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Claude);
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let attempts = std::cell::Cell::new(0);
    alondra_engine_ports::native_probe::retry_with(|| {
        attempts.set(attempts.get() + 1);
        let outcome = check_current(&*domain, &f.home);
        if attempts.get() == 1 {
            assert_eq!(outcome, alondra_engine_ports::native_probe::Outcome::Stale);
            assert!(domain.read().state().model_catalogs.is_empty());
        }
        std::future::ready(outcome)
    }, |delay| {
        assert_eq!(delay, Duration::ZERO);
        std::future::ready(())
    }).await;
    assert_eq!(attempts.get(), 2);
    let read = domain.read();
    assert_eq!(read.state().model_catalogs.len(), 1);
    assert_eq!(read.state().model_catalogs[0].models[0].model, "opus");
}

#[test]
fn connecting_claude_on_a_fresh_install_gives_the_orchestrator_a_model_claude_advertises() {
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelCandidate, ModelProfile, RoleModelPolicy};
    let f = fixture(); let domain = crate::wiring::open(&f.home).unwrap();
    let options = alondra_core::options::path_in(&f.home);
    std::fs::remove_file(&options).unwrap();
    check(&domain, &f.home);
    let user = Actor::User { user_id: "device:test".into() };
    let fresh = config(&f.home).unwrap();
    let mut connected = fresh.clone(); connected.connect(Provider::Claude);
    replace_configuration_authorized(&domain, &f.home, &user, &version(&fresh), &connected, || Ok(())).unwrap();
    let key = AccountKey::native(Provider::Claude);
    record_probe_fixture(&domain, vec![probe_account("first@example.invalid")]);
    record_account_command(&domain, Command::RecordAccountConfiguration { order: vec![key.clone()], version: version(&config(&f.home).unwrap()) });
    let model = |name: &str| CatalogModel { model: name.into(), display_name: name.into(), reasoning_efforts: vec![], default_reasoning_effort: None,
        context_window: None, input_modalities: vec![], is_default: false, hidden: false };
    record_account_command(&domain, Command::RecordModelCatalog { catalog: AccountModelCatalog { account: key, revision: "no-native-default".into(),
        models: vec![model("sonnet"), model("opus")], native_default: None, measured_at: 2 } });
    let selection = select_for_launch(&domain, &f.home, &alondra_core::model_selection::TaskSelection::automatic(alondra_core::model_selection::ModelProfile::Orchestrator), 100).expect("the orchestrator has a model");
    assert_eq!(selection.selection.model.model, "opus");

    let text = std::fs::read_to_string(&options).unwrap();
    let mut profiles = alondra_core::options::OptionsFile::parse(&text).unwrap().model_profiles;
    let chosen = RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: "sonnet".into(), reasoning_effort: None }] };
    profiles.entry(Provider::Claude).or_default().insert(ModelProfile::Orchestrator, chosen.clone());
    std::fs::write(&options, alondra_core::options::with_model_profiles(&text, &profiles).unwrap()).unwrap();
    let current = config(&f.home).unwrap();
    let mut both = current.clone(); both.connect(Provider::Codex);
    replace_configuration_authorized(&domain, &f.home, &user, &version(&current), &both, || Ok(())).unwrap();
    let kept = alondra_core::options::OptionsFile::parse(&std::fs::read_to_string(&options).unwrap()).unwrap().model_profiles;
    assert_eq!(kept[&Provider::Claude][&ModelProfile::Orchestrator], chosen, "a later connection keeps the user's choice");
}

#[test]
fn antigravity_accounts_are_checked_with_their_models_and_usage_only_when_due_and_run_no_node() {
    use alondra_core::antigravity_account::{Access, Check, CheckReport, Settings, Unavailable, UsageOutcome, Version};
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = fixture();
    let bin = f.user_home.parent().unwrap().join("agy-bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("agy"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(bin.join("agy"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let domain = Arc::new(crate::wiring::open(&f.home).unwrap());
    domain.enable_sdk(crate::sdk_backend::SdkBackend { provider: Provider::Claude, origin: None,
        node: f.claude.executable.clone(), node_sha256: String::new(), host: f.claude.executable.clone(), host_sha256: String::new(),
        native: f.claude.executable.clone(), native_sha256: String::new(), host_environment: BTreeMap::new(),
        native_environment: BTreeMap::from([("HOME".into(), f.user_home.display().to_string()), ("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()))]),
        confinement: None, state_root: f.home.join("hosts"), io_timeout_seconds: 30, installed_native: false, release: crate::sdk_backend::this_release() });
    let doubles = alondra_engine_doubles::accounts::of(domain.ports());
    let mut config = AccountsConfig::fresh();
    config.connect(Provider::Antigravity);
    let config = config.changed(Provider::Antigravity, &AccountChange::Add { name: "Studio".into(), antigravity: Some(Settings::of(Access::ApiKey)) }).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let studio = AccountKey::new(Provider::Antigravity, AccountId::parse("studio").unwrap());
    let folder = doubles.prepare_antigravity(&f.home, &studio, &f.user_home.join(".gemini")).unwrap().unwrap();
    let usage = alondra_core::antigravity_account::parse_usage(&serde_json::json!({"groups": [{"name": "All Models", "buckets": [
        {"id": "gemini-4-neon-high", "remaining_fraction": 0.4, "reset_time": "2030-01-01T00:00:00Z"},
        {"id": "claude-opus-max", "remaining_fraction": 0, "reset_time": "2030-01-02T00:00:00Z"}]}]})).unwrap();
    doubles.answer_antigravity_usage(None, Ok(UsageOutcome::Read(usage)));

    let status = |state: &alondra_domain_model::DomainState, key: &AccountKey| state.accounts.iter().find(|status| status.provider == key.provider && status.id == key.account).cloned().unwrap();
    let studio_probes = |probes: &[alondra_core::antigravity_account::Probe]| probes.iter().filter(|probe| probe.launch.environment.get("GEMINI_HOME").map(PathBuf::from) == Some(folder.clone())).count();
    begin_initial_check(&*domain);
    assert_eq!(check_current(&*domain, &f.home), alondra_engine_ports::native_probe::Outcome::Finished);
    assert!(doubles.antigravity_probes().is_empty(), "the check after startup, which every launch waits for, runs no agy");
    assert!(require_initial_check(&*domain).is_ok(), "and lets launches go once Claude and Codex are known");
    assert_eq!(status(&domain.read().state().clone(), &studio).state, AccountState::Unchecked);
    assert_eq!(check_current(&*domain, &f.home), alondra_engine_ports::native_probe::Outcome::Finished);
    assert_eq!(status(&domain.read().state().clone(), &studio).state, AccountState::SignedOut, "an API key account with no key is signed out");
    assert_eq!(studio_probes(&doubles.antigravity_probes()), 0, "and no agy starts to learn it");
    doubles.set_antigravity_key(&f.home, &studio, Some(&alondra_core::antigravity_account::Secret::new("AIza-fixture".into()))).unwrap();
    assert_eq!(check_current(&*domain, &f.home), alondra_engine_ports::native_probe::Outcome::Finished);
    let state = domain.read().state().clone();
    let native = AccountKey::native(Provider::Antigravity);
    assert_eq!(status(&state, &native).state, AccountState::SignedIn);
    assert_eq!(status(&state, &studio).billing, AccountBilling::PayPerUse, "an API key is billed per use");
    assert!(status(&state, &studio).antigravity.unwrap().key_set, "the screens learn that a key is set");
    assert!(state.model_catalogs.iter().any(|catalog| catalog.account == native && catalog.models[0].model == "fixture-gemini"));
    let measured = state.account_usage.iter().find(|usage| usage.provider == Provider::Antigravity && usage.account.is_default()).unwrap();
    assert_eq!(measured.windows.len(), 2, "one window per pool");
    assert!(!measured.plan_exhausted(0), "a pool that ran out leaves the others working");
    assert!(!state.account_usage.iter().any(|usage| usage.provider == Provider::Antigravity && usage.account == studio.account), "an API key has no usage reading");
    assert_eq!(studio_probes(&doubles.antigravity_probes()), 1, "a new key checks the account again, which is never read for usage");

    let started = doubles.antigravity_probes().len();
    assert_eq!(check_current(&*domain, &f.home), alondra_engine_ports::native_probe::Outcome::Finished);
    assert_eq!(doubles.antigravity_probes().len(), started, "an account checked today starts no agy again");

    crate::antigravity_accounts::forget(&f.home, &native);
    let (file, known) = (config.clone(), domain.read().state().accounts.clone());
    let round = || {
        let before = doubles.antigravity_probes().len();
        let (_, _, checked) = crate::antigravity_accounts::observe(&*domain, &f.home, &file, &known, alondra_core::now_millis(), true);
        (doubles.antigravity_probes().len() - before, checked)
    };
    let (discarded, _) = round();
    assert!(discarded > 0);
    let (again, checked) = round();
    assert_eq!(again, discarded, "a round the domain never recorded leaves its accounts to check again");
    crate::antigravity_accounts::remember(&f.home, checked);
    assert_eq!(round().0, 0, "a recorded round is not run again");
    doubles.answer_antigravity(Some(&folder), CheckReport { version: Some(Version(1, 1, 22)), check: Check::Unavailable(Unavailable::TooOld) });
    check_now(&*domain, &f.home);
    assert!(doubles.antigravity_probes().len() > started, "asking for a check checks again");
    let state = domain.read().state().clone();
    let studio_status = status(&state, &studio);
    assert_eq!(studio_status.state, AccountState::Unavailable);
    assert_eq!(studio_status.antigravity.unwrap().unavailable, Some(Unavailable::TooOld));

    let mut pinned = TaskSelection::automatic(ModelProfile::Implementation);
    pinned.account = Some(native.clone());
    let chosen = select_for_launch(&*domain, &f.home, &pinned, 100).unwrap();
    assert_eq!((&chosen.account, chosen.selection.model.model.as_str()), (&native, "fixture-gemini"), "a node runs on the Antigravity account it names");
    let orchestrator = select_for_launch(&*domain, &f.home, &TaskSelection::automatic(ModelProfile::Orchestrator), 100).unwrap_err();
    assert_eq!(orchestrator.code, "account_unavailable", "the orchestrator never runs on Antigravity");
}

#[test]
fn an_antigravity_key_goes_only_with_the_change_that_drops_it_and_a_re_added_account_starts_without_one() {
    use alondra_core::antigravity_account::{Access, Secret, Settings};
    use alondra_engine_ports::accounts::Accounts as _;
    let f = fixture();
    let accounts = alondra_engine_doubles::accounts::Accounts::default();
    let shared = f.user_home.join(".gemini");
    let mut fresh = AccountsConfig::fresh();
    fresh.connect(Provider::Antigravity);
    let current = fresh.changed(Provider::Antigravity, &AccountChange::Add { name: "Studio".into(), antigravity: Some(Settings::of(Access::ApiKey)) }).unwrap()
        .changed(Provider::Antigravity, &AccountChange::Add { name: "Lab".into(), antigravity: None }).unwrap();
    let (studio, lab) = (AccountKey::new(Provider::Antigravity, AccountId::parse("studio").unwrap()), AccountKey::new(Provider::Antigravity, AccountId::parse("lab").unwrap()));
    for key in [&studio, &lab] { accounts.prepare_antigravity(&f.home, key, &shared).unwrap(); }
    accounts.set_antigravity_key(&f.home, &studio, Some(&Secret::new("AIza-fixture".into()))).unwrap();
    let mut next = current.changed(Provider::Antigravity, &AccountChange::Remove { account: lab.account.clone() }).unwrap();
    next.antigravity.iter_mut().find(|account| account.id == studio.account).unwrap().access = Access::Google;

    let refused = prepare_account_changes(&accounts, &f.home, &f.user_home, None, None, &current, &next, std::slice::from_ref(&lab));
    assert!(matches!(refused, Err(Refused::InUse(_))), "an account open tasks run on stays");
    assert!(accounts.antigravity_key_set(&f.home, &studio), "a refused change keeps the key of the account it would have changed");

    let leftovers = prepare_account_changes(&accounts, &f.home, &f.user_home, None, None, &current, &next, &[]).unwrap();
    assert!(accounts.antigravity_key_set(&f.home, &studio), "the key stays until the file holds the change");
    cleanup_account_folders(&accounts, &f.home, &leftovers);
    assert!(!accounts.antigravity_key_set(&f.home, &studio), "an access that keeps no key leaves none behind");

    let without = current.changed(Provider::Antigravity, &AccountChange::Remove { account: studio.account.clone() }).unwrap();
    accounts.set_antigravity_key(&f.home, &studio, Some(&Secret::new("AIza-fixture".into()))).unwrap();
    let again = without.changed(Provider::Antigravity, &AccountChange::Add { name: "Studio".into(), antigravity: Some(Settings::of(Access::ApiKey)) }).unwrap();
    assert!(again.antigravity.iter().any(|account| account.id == studio.account), "the same name takes the same id back");
    prepare_account_changes(&accounts, &f.home, &f.user_home, None, None, &without, &again, &[]).unwrap();
    assert!(!accounts.antigravity_key_set(&f.home, &studio), "an account added again starts from an empty folder, never with the key a failed removal left");
}
