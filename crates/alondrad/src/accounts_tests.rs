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
    let config = AccountsConfig { distribution: Distribution { policy: Policy::Chain, ..Distribution::default() }, claude: vec![work(), team, disabled] };
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
