//! The accounts the user registered, as `options.toml` keeps them, and what the engine reads of
//! them without asking Claude Code.
use crate::domain_runtime::DomainRuntime;
use alondra_core::accounts::{AccountId, AccountsConfig, Provider};
use alondra_core::claude_account;
use std::path::{Path, PathBuf};

/// Whether a session one account started resumes with the other: both read the same conversations.
pub fn share_sessions(domain: &DomainRuntime, one: &AccountId, other: &AccountId) -> bool {
    let default_profile = domain.sdk().and_then(|(backend, _)| claude_account::configured_profile(&|key: &str| backend.native_environment.get(key).cloned()));
    domain.ports().accounts().shares_sessions(domain.home(), &claude_user_home(domain), default_profile.as_deref(), one, other)
}

pub fn config(home: &Path) -> Result<AccountsConfig, String> {
    options_file(home).map(|file| file.accounts)
}

pub fn options_file(home: &Path) -> Result<alondra_core::options::OptionsFile, String> {
    let path = alondra_core::options::path_in(home);
    match crate::domain_store::provider_settings::read_options(home)? {
        Some(text) => alondra_core::options::OptionsFile::parse(&text).map_err(|why| format!("{}: {why}", path.display())),
        None => Ok(alondra_core::options::OptionsFile::fresh()),
    }
}

/// Every account the user registered, on or off: the folders an execution may run Claude with.
pub fn registered(home: &Path) -> Vec<AccountId> {
    registered_provider(home, Provider::Claude)
}

pub fn registered_provider(home: &Path, provider: Provider) -> Vec<AccountId> {
    config(home).map_or_else(|_| vec![], |config| config.accounts(provider).into_iter().map(|account| account.id).collect())
}

/// The folders of every account but the default one: where each keeps its Claude settings.
pub fn profiles(home: &Path) -> Vec<PathBuf> {
    registered(home).iter().filter_map(|account| alondra_core::accounts::folder(home, account)).collect()
}

/// The HOME the nodes' Claude runs in, whose `~/.claude` the default account is.
pub fn claude_user_home(domain: &DomainRuntime) -> PathBuf {
    domain.sdk().and_then(|(backend, _)| backend.native_environment.get("HOME").map(PathBuf::from)).unwrap_or_else(crate::options::user_home)
}
