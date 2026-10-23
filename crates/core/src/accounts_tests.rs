use super::*;
use crate::options::OptionsFile;

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
    assert_eq!(file.accounts.distribution, Distribution { policy: Policy::Margin, reserve: 0.90, move_when_exhausted: false });
    assert_eq!(AccountsConfig::default().to_toml(), "");
}

#[test]
fn the_section_goes_to_the_file_and_back_unchanged() {
    let config = AccountsConfig {
        distribution: Distribution { policy: Policy::Chain, reserve: 0.8, move_when_exhausted: true },
        claude: vec![
            work(),
            ProviderAccount { id: AccountId::default_account(), name: "Personal".into(), enabled: false, billing: AccountBilling::default() },
            ProviderAccount { id: AccountId::parse("api").unwrap(), name: "API".into(), enabled: true, billing: AccountBilling::PayPerUse },
        ],
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
