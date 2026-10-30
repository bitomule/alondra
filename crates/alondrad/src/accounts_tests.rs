use super::*;
use crate::test_support::Directory;
use alondra_core::accounts::{Distribution, Policy, ProviderAccount};
use std::os::unix::fs::PermissionsExt as _;

/// A Claude Code that keeps each folder's sign-in as a file in it, and logs every sign-out. An API
/// key in its environment signs every folder in.
const FAKE: &str = r#"#!/bin/sh
folder="${CLAUDE_CONFIG_DIR:-$HOME/.claude}"
case "$1 $2" in
  "auth status")
    if [ -n "$ANTHROPIC_API_KEY" ]; then printf '{"loggedIn":true,"authMethod":"api_key"}';
    elif [ -f "$folder/signed-in" ]; then printf '{"loggedIn":true,"email":"%s","subscriptionType":"max"}' "$(cat "$folder/signed-in")"; else printf '{"loggedIn":false}'; exit 1; fi ;;
  "auth logout")
    [ -f "$HOME/refuse-logout" ] && exit 2
    echo "$folder" >> "$HOME/logouts"; rm -f "$folder/signed-in" ;;
  *) exit 64 ;;
esac
"#;

struct Fixture {
    _scratch: Directory,
    home: PathBuf,
    user_home: PathBuf,
    claude: Claude,
}

fn fixture() -> Fixture {
    let scratch = Directory::new();
    let user_home = scratch.join("user");
    let bin = scratch.join("bin");
    std::fs::create_dir_all(user_home.join(".claude/skills/mine")).unwrap();
    std::fs::create_dir_all(user_home.join(".claude/projects")).unwrap();
    std::fs::write(user_home.join(".claude/settings.json"), "{}").unwrap();
    std::fs::write(user_home.join(".claude/signed-in"), "me@example.org").unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), FAKE).unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = user_home.join(".alondra");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(alondra_core::options::path_in(&home), AccountsConfig::default().to_toml()).unwrap();
    let claude = Claude {
        executable: bin.join("claude"),
        user_home: user_home.clone(),
        path: format!("{}:/usr/bin:/bin", bin.display()),
        username: claude_account::effective_username().unwrap(),
        environment: BTreeMap::new(),
    };
    Fixture { _scratch: scratch, home, user_home, claude }
}

fn work() -> ProviderAccount {
    ProviderAccount { id: AccountId::parse("work").unwrap(), name: "Work".into(), enabled: true, billing: AccountBilling::default() }
}

fn with(accounts: Vec<ProviderAccount>) -> AccountsConfig {
    AccountsConfig { claude: accounts, ..AccountsConfig::default() }
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
    let runtime = DomainRuntime::open(&f.home).unwrap();
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
    let runtime = DomainRuntime::open(&f.home).unwrap();
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

impl Fixture {
    fn store(&self, next: &AccountsConfig) -> Result<(), Refused> {
        let seen = version(&config(&self.home).unwrap());
        store(&self.home, &self.user_home, Some(&self.claude), &seen, next, &[])
    }
    fn claude_tree(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.user_home.join(".claude")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
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
    assert_eq!(store(&f.home, &f.user_home, Some(&f.claude), &stale, &next, &[]), Err(Refused::Stale));
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
    assert!(matches!(store(&f.home, &f.user_home, None, &seen, &AccountsConfig::default(), &[]), Err(Refused::SignOut(_))), "without Claude Code nothing signs it out");
}

#[test]
fn an_account_open_tasks_run_on_is_not_removed() {
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    std::fs::write(f.home.join("accounts/work/signed-in"), "work@example.org").unwrap();
    let seen = version(&config(&f.home).unwrap());
    let refused = store(&f.home, &f.user_home, Some(&f.claude), &seen, &with(vec![]), &[AccountId::parse("work").unwrap()]);
    assert_eq!(refused, Err(Refused::InUse("Work".into())));
    assert_eq!(config(&f.home).unwrap(), with(vec![work()]), "the section is as it was");
    assert!(f.home.join("accounts/work/signed-in").exists(), "still signed in, its folder kept");
    assert!(!f.user_home.join("logouts").exists(), "never signed out");
}

/// A Settings button writes `options.toml`, then the domain records the change, and it survives a
/// restart; one its rules refuse leaves the file as it was.
#[test]
fn a_settings_change_writes_the_file_and_the_domain_and_survives_a_restart() {
    let f = fixture();
    f.store(&with(vec![work()])).unwrap();
    let user = Actor::User { user_id: "device:test".into() };
    let order = |domain: &DomainRuntime| domain.read().state().accounts.iter().map(|a| a.id.to_string()).collect::<Vec<_>>();
    {
        let domain = DomainRuntime::open(&f.home).unwrap();
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
        assert!(matches!(refused, Err(ChangeRefused::Domain(ref e)) if e.why() == alondra_core::domain::Refusal::DefaultAccountStays { name: "Default".into() }), "{refused:?}");
        assert_eq!(std::fs::read_to_string(alondra_core::options::path_in(&f.home)).unwrap(), before, "a refused change writes nothing");
        let refused = change(&domain, &f.home, &Actor::System, &AccountChange::SetPolicy { policy: Policy::Margin });
        assert!(matches!(refused, Err(ChangeRefused::Domain(ref e)) if e.code == "forbidden"), "only the user changes the accounts: {refused:?}");
    }
    let restarted = DomainRuntime::open(&f.home).unwrap();
    assert_eq!(restarted.read().state().account_policy, Policy::Chain, "the policy survives a restart");
    assert!(restarted.read().state().account_moves, "so does moving nodes");
    assert_eq!(order(&restarted), ["work", "default"]);
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
fn the_orchestrator_waits_for_a_sign_in_only_when_no_account_may_have_a_session() {
    let seen = |states: &[(&str, AccountState)]| none_signed_in(&states.iter().map(|(id, state)| AccountStatus {
        provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: (*id).into(), state: *state,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0,
    }).collect::<Vec<_>>());
    assert_eq!(seen(&[("default", AccountState::SignedOut), ("work", AccountState::Disabled)]).as_deref(), Some("default (signed out), work (off)"));
    assert_eq!(seen(&[("default", AccountState::SignedOut), ("work", AccountState::SignedIn)]), None);
    assert_eq!(seen(&[("default", AccountState::SignedOut), ("work", AccountState::Unavailable)]), None, "an account that could not be checked may still work");
    assert_eq!(seen(&[("default", AccountState::Unchecked)]), None);
    assert_eq!(seen(&[]), None, "no account recorded yet is the implicit default, not a missing sign-in");
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
    f.store(&config).unwrap(); let domain = DomainRuntime::open(&f.home).unwrap();
    let statuses = keys.iter().map(|key| AccountStatus { provider:key.provider,id:key.account.clone(),name:"Personal".into(),state:AccountState::SignedIn,native_identity: None, email:None,plan:None,billing:AccountBilling::default(),sign_ins:0 }).collect();
    record_account_command(&domain,Command::RecordAccounts {accounts:statuses,reserve:alondra_core::usage::Share::of(config.distribution.reserve)});
    record_account_command(&domain,Command::RecordAccountConfiguration {order:keys.clone(),version:version(&config)});
    for key in keys {
        record_account_command(&domain,Command::RecordModelCatalog {catalog:AccountModelCatalog {account:key,revision:"native-fixture".into(),native_default:Some("fixture-model".into()),measured_at:100,
            models:vec![CatalogModel {model:"fixture-model".into(),display_name:"Fixture".into(),reasoning_efforts:vec![],default_reasoning_effort:None,context_window:None,input_modalities:vec!["text".into()],is_default:true,hidden:false}]}});
    }
    let mut intent=TaskSelection::automatic(ModelProfile::Implementation);
    assert_eq!(select_for_launch(&domain,&f.home,&intent,100).unwrap().account.provider,Provider::Codex,"unknown initial policy resolves before admission");
    intent.native_tool_denials=Some(vec!["Write".into()]);
    assert_eq!(select_for_launch(&domain,&f.home,&intent,100).unwrap().account.provider,Provider::Claude,"known unsupported policy filters the first account instead of failing after choosing it");
    intent.native_tool_denials=Some(vec![]);
    assert_eq!(select_for_launch(&domain,&f.home,&intent,100).unwrap().account.provider,Provider::Codex);
}


fn probe_account(email: &str) -> AccountStatus {
    AccountStatus { provider: Provider::Claude, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: Some(email.into()), plan: None, billing: AccountBilling::default(), sign_ins: 0 }
}

fn record_probe_fixture(domain: &DomainRuntime, accounts: Vec<AccountStatus>) {
    record_account_command(domain, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
}

fn record_account_command(domain: &DomainRuntime, command: Command) {
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: domain.read().state().data_epoch.clone(),
        command_id: format!("fixture-probe-{}", crate::auth::random_hex(16)).into(), expected_revision: 0, command };
    domain.execute_as_system(envelope).unwrap();
}

fn probe_catalog() -> Command {
    Command::RecordModelCatalog { catalog: alondra_core::model_selection::AccountModelCatalog { account: AccountKey::native(Provider::Claude),
        revision: "native-advertised".into(), models: vec![], native_default: None, measured_at: 2 } }
}

#[test]
fn a_delayed_native_probe_cannot_replace_a_new_credential_identity_or_its_metadata() {
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
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
    let reopened = DomainRuntime::open(&f.home).unwrap();
    assert_eq!(reopened.read().state().accounts[0].email.as_deref(), Some("second@example.invalid"));
    assert!(reopened.read().state().account_usage.is_empty()); assert!(reopened.read().state().model_catalogs.is_empty());
}

#[test]
fn a_native_metadata_bundle_rolls_back_all_results_when_a_later_command_is_refused() {
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap(); let account = probe_account("first@example.invalid");
    record_probe_fixture(&domain, vec![account.clone()]);
    let (fence, _) = probe_fence(&domain, &f.home).unwrap();
    let before = domain.read().state().clone();
    let error = publish_probe(&domain, &f.home, fence, vec![probe_catalog(), Command::RecordAccounts { accounts: vec![account.clone(), account], reserve: alondra_core::usage::default_reserve() }]).unwrap_err();
    assert!(error.contains("twice"), "{error}");
    assert_eq!(domain.read().state().revision, before.revision);
    assert!(domain.read().state().model_catalogs.is_empty());
    drop(domain); assert!(DomainRuntime::open(&f.home).unwrap().read().state().model_catalogs.is_empty());
}

#[test]
fn a_native_probe_cannot_publish_across_an_unimported_options_file_edit() {
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
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
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
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
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
    record_probe_fixture(&domain, vec![probe_account("first@example.invalid")]);
    let (first, _) = probe_fence(&domain, &f.home).unwrap();
    let (other, _) = probe_fence(&domain, &f.home).unwrap();
    publish_probe(&domain, &f.home, other, vec![probe_usage(Provider::Codex, 20, true)]).unwrap();
    publish_probe(&domain, &f.home, first, vec![probe_catalog(), probe_usage(Provider::Claude, 2, false)]).unwrap();
    assert_eq!(domain.read().state().model_catalogs.len(), 1);
    assert_eq!(domain.read().state().account_usage.len(), 2);
    drop(domain);
    let reopened = DomainRuntime::open(&f.home).unwrap();
    assert_eq!(reopened.read().state().model_catalogs.len(), 1);
    assert_eq!(reopened.read().state().account_usage.len(), 2);
}

#[test]
fn cached_authentication_cannot_launch_new_work_until_a_current_roster_is_committed() {
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelProfile, TaskSelection};
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
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
    assert_eq!(crate::orchestrator_provider::selection(&domain, &f.home, 100).unwrap_err().code, "account_check_in_progress");
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
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
    let mut account = probe_account("first@example.invalid"); account.provider = Provider::Codex; account.native_identity = Some("native-workspace".into());
    record_probe_fixture(&domain, vec![account.clone()]);
    drop(domain);
    let reopened = DomainRuntime::open(&f.home).unwrap();
    assert_eq!(reopened.read().state().accounts, [account]);
}

#[test]
fn the_actual_codex_metadata_check_keeps_external_auth_unavailable_and_api_keys_out_of_automatic_work() {
    let f = fixture(); let domain = Arc::new(DomainRuntime::open(&f.home).unwrap());
    let _hosts = crate::sdk_owner::tests::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    std::fs::copy(std::env::var("FIXTURE_CODEX_NATIVE").unwrap(), &native).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone(); backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_owner::backend::digest(&native).unwrap();
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

#[test]
fn absent_native_workspace_never_proves_existing_session_identity() {
    let native = NativeAuthAccount { auth_method: "chatgpt".into(), email: Some("first@example.test".into()), workspace_id: None };
    let report = alondra_core::codex_account::ProbeReport { requires_auth: true, auth_method: Some("chatgpt".into()), account_type: Some("chatgpt".into()), workspace_id: None, signed_in: true, email: Some("first@example.test".into()), plan: Some("plus".into()), pay_per_use: false, rate_limits: None, catalog_error: None,
        catalog: alondra_core::model_selection::AccountModelCatalog { account: AccountKey::native(Provider::Codex), revision: String::new(), models: vec![], native_default: None, measured_at: 0 } };
    assert!(!native.matches(&report), "matching absence is not positive workspace proof");
    assert!(report.signed_in && report.supported_authentication(), "optional identity is independent from first-node authentication");
}

#[tokio::test]
async fn a_stale_startup_account_check_retries_and_releases_account_selection() {
    use alondra_core::model_selection::{ModelProfile, TaskSelection};
    let f = fixture();
    let domain = Arc::new(DomainRuntime::open(&f.home).unwrap());
    let _hosts = crate::sdk_owner::tests::SdkLaunches::enable(&domain, &f.home);
    let native = f._scratch.join("codex-native");
    let original = std::fs::read_to_string(std::env::var("FIXTURE_CODEX_NATIVE").unwrap()).unwrap();
    let options = alondra_core::options::path_in(&f.home);
    let edit = format!("\nimport pathlib\noptions = pathlib.Path({:?})\nmarker = pathlib.Path(__file__ + '.attempts')\nif not marker.exists():\n    with options.open('a') as output: output.write('\\n# concurrent settings edit\\n')\nwith marker.open('a') as output: output.write('x')\n", options.display().to_string());
    std::fs::write(&native, original.replacen("import json, os, sys, subprocess, threading", &format!("import json, os, sys, subprocess, threading{edit}"), 1)).unwrap();
    std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
    let mut backend = domain.sdk().unwrap().0.clone();
    backend.provider = Provider::Codex; backend.origin = None;
    backend.native = native.clone(); backend.native_sha256 = crate::sdk_owner::backend::digest(&native).unwrap();
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

#[tokio::test]
async fn a_stale_model_catalog_retries_the_account_check_until_models_are_ready() {
    let f = fixture();
    let domain = Arc::new(DomainRuntime::open(&f.home).unwrap());
    let state = crate::sdk_owner::tests::HostRoot::new();
    let (mut backend, _) = crate::sdk_owner::tests::fixture_backend(&f.home, &state);
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
    backend.native_sha256 = crate::sdk_owner::backend::digest(&backend.native).unwrap();
    backend.native_environment.insert("HOME".into(), f.user_home.display().to_string());
    backend.native_environment.insert("STALE_MODEL_OPTIONS".into(), alondra_core::options::path_in(&f.home).display().to_string());
    domain.enable_sdk(backend);
    let mut config = AccountsConfig::fresh(); config.connect(Provider::Claude);
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let attempts = std::cell::Cell::new(0);
    crate::native_probe::retry_with(|| {
        attempts.set(attempts.get() + 1);
        let outcome = check_current(&domain, &f.home);
        if attempts.get() == 1 {
            assert_eq!(outcome, crate::native_probe::Outcome::Stale);
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
    let f = fixture(); let domain = DomainRuntime::open(&f.home).unwrap();
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
    let selection = crate::orchestrator_provider::selection(&domain, &f.home, 100).expect("the orchestrator has a model");
    assert_eq!(selection.model.model.model, "opus");

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
