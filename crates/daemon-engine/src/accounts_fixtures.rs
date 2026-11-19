//! Accounts as the account tests here and in the crates above set them up.
use super::*;
use crate::test_support::Directory;
use alondra_core::accounts::{Distribution, Policy, ProviderAccount};
use std::os::unix::fs::PermissionsExt as _;


/// A Claude Code that keeps each folder's sign-in as a file in it, and logs every sign-out. An API
/// key in its environment signs every folder in.
pub const FAKE: &str = r#"#!/bin/sh
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

pub struct Fixture {
    pub _scratch: Directory,
    pub home: PathBuf,
    pub user_home: PathBuf,
    pub claude: Claude,
    pub accounts: std::sync::Arc<dyn alondra_engine_ports::accounts::Accounts>,
}

/// A Mac with a Claude whose sessions start on `hosts`, and whose accounts are `accounts`.
pub fn fixture_with(hosts: std::sync::Arc<dyn alondra_engine_ports::agent_host::AgentHost>, accounts: std::sync::Arc<dyn alondra_engine_ports::accounts::Accounts>) -> Fixture {
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
        username: alondra_host::user::effective_username().unwrap(),
        environment: BTreeMap::new(),
        timeout: None,
        hosts,
    };
    Fixture { _scratch: scratch, home, user_home, claude, accounts }
}

pub fn work() -> ProviderAccount {
    ProviderAccount { id: AccountId::parse("work").unwrap(), name: "Work".into(), enabled: true, billing: AccountBilling::default() }
}

pub fn with(accounts: Vec<ProviderAccount>) -> AccountsConfig {
    AccountsConfig { claude: accounts, ..AccountsConfig::default() }
}

impl Fixture {
    pub fn store(&self, next: &AccountsConfig) -> Result<(), Refused> {
        let seen = version(&config(&self.home).unwrap());
        store(&*self.accounts, &self.home, &self.user_home, Some(&self.claude), &seen, next, &[])
    }
    pub fn claude_tree(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.user_home.join(".claude")).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }
}
