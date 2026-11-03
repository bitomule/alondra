use super::*;
use crate::options::OptionsFile;

#[test]
fn connected_providers_do_not_synthesize_unrelated_native_accounts() {
    let mut accounts = AccountsConfig::fresh();
    assert!(accounts.ordered_accounts().is_empty());
    accounts.connect(Provider::Codex);
    assert!(accounts.accounts(Provider::Claude).is_empty());
    assert_eq!(accounts.ordered_accounts(), vec![(AccountKey::native(Provider::Codex), ProviderAccount::implicit_default())]);
    let file = OptionsFile::parse(&accounts.to_toml()).unwrap();
    assert_eq!(file.accounts, accounts);
    assert_eq!(AccountsConfig::default().connected_providers(), &[Provider::Claude]);
}

#[test]
fn global_eligibility_follows_account_order_without_weights_or_phantom_defaults() {
    let keys = [AccountKey::native(Provider::Claude), AccountKey::native(Provider::Codex), AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap())];
    let mut accounts: Vec<_> = keys.iter().map(|key| AccountStatus { provider: key.provider, id: key.account.clone(), name: key.wire_id(),
        state: AccountState::SignedIn, native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None }).collect();
    assert_eq!(crate::usage::eligible_account_keys(&keys, &accounts, &[], 1), keys);
    accounts[0].state = AccountState::SignedOut;
    assert_eq!(crate::usage::eligible_account_keys(&keys, &accounts, &[], 1), keys[1..]);
    let reading = crate::codex_account::parse_rate_limits(&serde_json::json!({"rateLimits":{"primary":{"usedPercent":100,"windowDurationMins":30,"resetsAt":100}}}), None).unwrap();
    let usage = crate::usage::record(None, Provider::Codex, &keys[1].account, &reading, 1, "2026-10-25");
    assert_eq!(crate::usage::eligible_account_keys(&keys, &accounts, &[usage], 2), keys[2..]);
    accounts[2].billing = AccountBilling::PayPerUse;
    assert!(crate::usage::eligible_account_keys(&[], &[], &[], 1).is_empty());
    assert_eq!(crate::usage::eligible_account_keys(&keys, &accounts, &[], 1), [keys[1].clone()]);
}

#[test]
fn request_model_controls_preserve_explicit_intent_and_provider_qualified_account() {
    use crate::options::{ALLOW_MODEL_SUBSTITUTION, Choice, MODEL, REASONING_EFFORT, SUBSTITUTION_WAS_EDITED};
    let mut file = OptionsFile::fresh();
    file.accounts.connect(Provider::Codex);
    let values = std::collections::BTreeMap::from([(MODEL.into(), Choice::Value("codex/native-only".into())),
        (REASONING_EFFORT.into(), Choice::Value("high".into())), (crate::options::ACCOUNT.into(), Choice::Value("codex/default".into())),
        (ALLOW_MODEL_SUBSTITUTION.into(), Choice::Toggle(false)), (SUBSTITUTION_WAS_EDITED.into(), Choice::Toggle(true))]);
    let resolved = file.resolve(None, &values).unwrap();
    assert!(resolved.account().is_none(), "a Codex account is never returned as a legacy Claude account");
    assert_eq!(resolved.account_key(), Some(AccountKey::native(Provider::Codex)));
    let selection = resolved.selection.unwrap();
    assert_eq!(selection.requested_reasoning_effort.as_deref(), Some("high"));
    assert!(!selection.allow_model_substitution);
    assert!(selection.substitution_was_edited);
    assert!(selection.effective.is_none(), "catalog validation and selection happen before launch");
}

#[test]
fn codex_hard_effort_and_delegation_do_not_inherit_claude_model_or_tool_instructions() {
    use crate::options::{Approach, Choice, EFFORT, MODEL};
    let mut file = OptionsFile::fresh();
    file.accounts.connect(Provider::Codex);
    let values = std::collections::BTreeMap::from([(MODEL.into(), Choice::Value("codex/native-model".into())), (EFFORT.into(), Choice::Value("hard".into()))]);
    let resolved = file.resolve(None, &values).unwrap();
    let note = resolved.effort_note("User");
    let delegation = Approach::Subagents.how_to_work_for(resolved.selected_provider()).join("\n");
    for text in [note, delegation] {
        for other in ["Opus", "opus", "sonnet", "Fable", "TaskCreate", "TaskUpdate"] { assert!(!text.contains(other), "{text}"); }
        assert!(text.contains("same available model"));
    }
    assert!(resolved.launch_model().is_none());
    assert!(resolved.launch_flags(None).is_empty());
}

#[test]
fn mixed_provider_order_moves_globally_and_rejects_missing_or_duplicate_accounts() {
    let mut accounts = AccountsConfig::fresh();
    accounts.connect(Provider::Claude);
    accounts.claude.push(work());
    accounts.connect(Provider::Codex);
    let claude = AccountKey::native(Provider::Claude);
    let codex = AccountKey::native(Provider::Codex);
    let work = AccountKey::new(Provider::Claude, AccountId::parse("work").unwrap());
    accounts.set_order(vec![claude.clone(), codex.clone(), work.clone()]).unwrap();
    assert!(accounts.set_order(vec![claude.clone(), claude.clone(), work.clone()]).is_err());
    assert!(accounts.set_order(vec![claude.clone(), codex.clone()]).is_err());
    let moved = accounts.changed(Provider::Claude, &AccountChange::MoveUp { account: work.account.clone() }).unwrap();
    assert_eq!(moved.distribution.order, vec![claude, work, codex]);
    assert_eq!(provider_folder(Path::new("/home/alondra"), &AccountKey::new(Provider::Codex, AccountId::parse("work").unwrap())).unwrap(), Path::new("/home/alondra/provider-accounts/codex/work"));
}

#[test]
fn an_old_settings_writer_cannot_disconnect_codex_or_erase_its_accounts() {
    let mut current = AccountsConfig::fresh();
    current.connect(Provider::Codex);
    current.codex.push(work());
    let mut old_writer = AccountsConfig::default();
    old_writer.preserve_connected_from(&current);
    assert_eq!(old_writer.connected, current.connected);
    assert_eq!(old_writer.codex, current.codex);
}

fn work() -> ProviderAccount {
    ProviderAccount {
        id: AccountId::parse("work").unwrap(),
        name: "Work".into(),
        enabled: true,
        billing: AccountBilling::Subscription { plan: Some("max_20x".into()), weight: Some(10) },
    }
}

#[test]
fn without_the_section_there_is_one_implicit_default_account() {
    let file = OptionsFile::parse("projects_root = \"~/Code\"\n").unwrap();
    assert_eq!(file.accounts, AccountsConfig::default());
    assert_eq!(file.accounts.accounts(Provider::Claude), vec![ProviderAccount::implicit_default()]);
    assert_eq!(file.accounts.distribution, Distribution { policy: Policy::Margin, reserve: 0.90, move_when_exhausted: false, order: vec![] });
    assert_eq!(AccountsConfig::default().to_toml(), "");
}

#[test]
fn the_section_goes_to_the_file_and_back_unchanged() {
    let config = AccountsConfig {
        distribution: Distribution { policy: Policy::Chain, reserve: 0.8, move_when_exhausted: true, order: vec![] },
        claude: vec![
            work(),
            ProviderAccount { id: AccountId::default_account(), name: "Personal".into(), enabled: false, billing: AccountBilling::default() },
            ProviderAccount { id: AccountId::parse("api").unwrap(), name: "API".into(), enabled: true, billing: AccountBilling::PayPerUse },
        ],
        ..Default::default()
    };
    let text = with_accounts("max_working_nodes = 3\n", &config).unwrap();
    let file = OptionsFile::parse(&text).unwrap();
    assert_eq!(file.accounts, config);
    assert_eq!(file.max_working_nodes, crate::options::NodeLimit::Fixed(3));
    let ids: Vec<_> = file.accounts.accounts(Provider::Claude).into_iter().map(|a| a.id.to_string()).collect();
    assert_eq!(ids, ["work", "default", "api"], "a listed default keeps its place");
    assert_eq!(with_accounts(&text, &config).unwrap(), text, "writing the same section again changes nothing");
}

#[test]
fn replacing_the_section_keeps_every_other_line_and_comment() {
    let original = "# mine\nprojects_root = \"~/Code\"\n\n[accounts.distribution]\npolicy = \"chain\"\n\n[[accounts.claude]]\nid = \"old\"\nname = \"Old\"\n\n[issues]\n# keep me\nowners = [\"me\"]\n";
    let config = AccountsConfig { claude: vec![work()], ..AccountsConfig::default() };
    let text = with_accounts(original, &config).unwrap();
    assert!(text.starts_with("# mine\nprojects_root = \"~/Code\"\n\n[issues]\n# keep me\nowners = [\"me\"]\n\n"), "{text}");
    assert!(!text.contains("old"), "{text}");
    let file = OptionsFile::parse(&text).unwrap();
    assert_eq!(file.issues.owners, ["me"]);
    assert_eq!(file.accounts, config);
    let cleared = with_accounts(&text, &AccountsConfig::default()).unwrap();
    assert_eq!(cleared, "# mine\nprojects_root = \"~/Code\"\n\n[issues]\n# keep me\nowners = [\"me\"]\n");
}

#[test]
fn a_broken_section_is_refused_with_the_reason() {
    for (text, reason) in [
        ("[[accounts.claude]]\nid = \"Work\"\nname = \"Work\"\n", "lowercase letters"),
        ("[[accounts.claude]]\nid = \"work\"\nname = \" \"\n", "needs a name"),
        ("[[accounts.claude]]\nid = \"a\"\nname = \"Work\"\n[[accounts.claude]]\nid = \"a\"\nname = \"Other\"\n", "listed twice"),
        ("[[accounts.claude]]\nid = \"a\"\nname = \"Work\"\n[[accounts.claude]]\nid = \"b\"\nname = \"work\"\n", "two accounts are called"),
        ("[accounts.distribution]\nreserve = 1.5\n", "reserve"),
        ("[accounts.distribution]\npolicy = \"round_robin\"\n", "unknown variant"),
        ("[[accounts.claude]]\nid = \"a\"\nname = \"A\"\nbilling = { kind = \"subscription\", weight = 0 }\n", "weight from 1 to 20"),
        ("[[accounts.claude]]\nid = \"a\"\nname = \"A\"\nfolder = \"~/.claude-work\"\n", "unknown field"),
    ] {
        let error = OptionsFile::parse(text).unwrap_err();
        assert!(error.contains(reason), "{text}: {error}");
    }
    assert!(with_accounts("accounts = 1\n", &AccountsConfig::default()).is_err());
}

#[test]
fn an_id_comes_from_the_name_and_never_repeats_or_takes_default() {
    let taken = [AccountId::parse("work").unwrap(), AccountId::parse("work-2").unwrap()];
    assert_eq!(AccountId::for_name("Work", &taken).as_str(), "work-3");
    assert_eq!(AccountId::for_name("  Cuenta de Empresa ñ!", &[]).as_str(), "cuenta-de-empresa-n");
    assert_eq!(AccountId::for_name("Default", &[]).as_str(), "default-2");
    assert_eq!(AccountId::for_name("!!!", &[]).as_str(), "account");
    let long = AccountId::for_name(&"x".repeat(100), &[]);
    assert!(AccountId::parse(long.as_str()).is_ok());
    for bad in ["", "-a", "a-", "A", "a b", "../x", &"a".repeat(33)] {
        assert!(AccountId::parse(bad).is_err(), "{bad}");
    }
}

#[test]
fn an_account_is_found_by_its_name_or_id() {
    let config = AccountsConfig { claude: vec![work()], ..AccountsConfig::default() };
    assert_eq!(config.find(Provider::Claude, "work").unwrap().id.as_str(), "work");
    assert_eq!(config.find(Provider::Claude, " WORK ").unwrap().id.as_str(), "work");
    assert!(config.find(Provider::Claude, "default").unwrap().id.is_default());
    assert!(config.find(Provider::Claude, "nobody").is_none());
}

#[test]
fn only_accounts_other_than_the_default_have_a_folder_inside_alondras_home() {
    let home = Path::new("/h/.alondra");
    assert_eq!(folder(home, &AccountId::parse("work").unwrap()).unwrap(), Path::new("/h/.alondra/accounts/work"));
    assert_eq!(folder(home, &AccountId::default_account()), None);
}

#[test]
fn a_settings_change_moves_disables_weighs_and_removes_accounts_in_the_users_order() {
    let config = AccountsConfig { claude: vec![work()], ..AccountsConfig::default() };
    let id = |text: &str| AccountId::parse(text).unwrap();
    let ids = |config: &AccountsConfig| config.accounts(Provider::Claude).into_iter().map(|a| a.id.to_string()).collect::<Vec<_>>();

    let moved = config.changed(Provider::Claude, &AccountChange::MoveUp { account: id("work") }).unwrap();
    assert_eq!(ids(&moved), ["work", "default"]);
    assert_eq!(moved.claude.len(), 2, "placing the default account writes it into the list");
    assert!(moved.changed(Provider::Claude, &AccountChange::MoveUp { account: id("work") }).is_err(), "the first can't move up");

    let disabled = config.changed(Provider::Claude, &AccountChange::Disable { account: id("work") }).unwrap();
    assert!(!disabled.claude[0].enabled);
    assert_eq!(disabled.claude.len(), 1, "the default account the change leaves alone stays implicit");
    assert!(disabled.changed(Provider::Claude, &AccountChange::Enable { account: id("work") }).unwrap().claude[0].enabled);

    let weighed = config.changed(Provider::Claude, &AccountChange::SetWeight { account: AccountId::default_account(), weight: Some(3) }).unwrap();
    assert_eq!(weighed.claude[0].billing, AccountBilling::Subscription { plan: None, weight: Some(3) });
    assert!(config.changed(Provider::Claude, &AccountChange::SetWeight { account: id("work"), weight: Some(WEIGHT_LIMIT + 1) }).is_err());
    let automatic = config.changed(Provider::Claude, &AccountChange::SetWeight { account: id("work"), weight: None }).unwrap();
    assert_eq!(automatic.claude[0].billing, AccountBilling::Subscription { plan: Some("max_20x".into()), weight: None });

    let removed = config.changed(Provider::Claude, &AccountChange::Remove { account: id("work") }).unwrap();
    assert_eq!(removed, AccountsConfig::default());
    let chain = config.changed(Provider::Claude, &AccountChange::SetPolicy { policy: Policy::Chain }).unwrap();
    assert_eq!(chain.distribution.policy, Policy::Chain);
    assert!(config.changed(Provider::Claude, &AccountChange::Remove { account: id("nobody") }).is_err());
}

#[test]
fn a_request_offers_automatic_or_an_account_paid_per_use_and_only_then() {
    let api = ProviderAccount { id: AccountId::parse("api").unwrap(), name: "API".into(), enabled: true, billing: AccountBilling::PayPerUse };
    let off = ProviderAccount { id: AccountId::parse("old-api").unwrap(), name: "Old API".into(), enabled: false, billing: AccountBilling::PayPerUse };
    let only_subscriptions = AccountsConfig { claude: vec![work()], ..AccountsConfig::default() };
    let file = OptionsFile::parse(&with_accounts("", &only_subscriptions).unwrap()).unwrap();
    assert!(file.catalog(None).iter().any(|spec| spec.id == crate::options::ACCOUNT), "the account picker also offers subscription accounts");

    let config = AccountsConfig { claude: vec![work(), api, off], ..AccountsConfig::default() };
    let file = OptionsFile::parse(&with_accounts("", &config).unwrap()).unwrap();
    let account = file.catalog(None).into_iter().find(|spec| spec.id == crate::options::ACCOUNT).expect("the account option");
    let values: Vec<_> = account.values.iter().map(|value| (value.id.as_str(), value.label.as_str())).collect();
    assert_eq!(values, [("auto", "automatic"), ("claude/default", "Default"), ("claude/work", "Work"), ("claude/api", "API")], "an account that is off is not offered");
    assert_eq!(account.default, crate::options::Choice::Value("auto".into()));

    let automatic = file.resolve(None, &std::collections::BTreeMap::new()).unwrap();
    assert_eq!(automatic.account(), None, "by default Alondra picks the account");
    let chosen = std::collections::BTreeMap::from([(crate::options::ACCOUNT.to_owned(), crate::options::Choice::Value("api".into()))]);
    assert_eq!(file.resolve(None, &chosen).unwrap().account(), Some(AccountId::parse("api").unwrap()));
    let subscription = std::collections::BTreeMap::from([(crate::options::ACCOUNT.to_owned(), crate::options::Choice::Value("work".into()))]);
    assert_eq!(file.resolve(None, &subscription).unwrap().account(), Some(AccountId::parse("work").unwrap()), "subscription accounts can be pinned explicitly");
}

#[test]
fn fresh_accounts_never_enable_node_account_movement_and_existing_preferences_survive() {
    let mut fresh=AccountsConfig::fresh(); assert!(!fresh.distribution.move_when_exhausted);
    fresh.connect(Provider::Codex); fresh.connect(Provider::Claude); assert!(!fresh.distribution.move_when_exhausted);
    let mut existing=AccountsConfig::default(); existing.distribution.move_when_exhausted=true;
    let text=existing.to_toml(); let mut decoded=OptionsFile::parse(&text).unwrap().accounts; decoded.migrate_legacy();
    assert!(decoded.distribution.move_when_exhausted,"an existing saved preference is not silently rewritten");
}

#[test]
fn absent_native_identity_preserves_legacy_account_bytes() {
    let account = AccountStatus { provider: Provider::Claude, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None };
    let value = serde_json::to_value(&account).unwrap();
    assert!(value.get("native_identity").is_none());
    assert_eq!(serde_json::from_value::<AccountStatus>(value).unwrap(), account);
    let native = AccountStatus { native_identity: Some("real-workspace".into()), ..account };
    let value = serde_json::to_value(&native).unwrap();
    assert_eq!(value["native_identity"], "real-workspace");
    assert_eq!(serde_json::from_value::<AccountStatus>(value).unwrap(), native);
}
