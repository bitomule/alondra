//! The provider accounts nodes may run on: `GET/PUT /settings/accounts` read and change the
//! `[accounts]` section of `options.toml`, of which the daemon is the only writer, and the daemon
//! checks each account's sign-in and records what it saw in the domain, where every screen reads
//! it. An added account gets its folder, linked to the user's `~/.claude`; a removed one is signed
//! out and its folder deleted. `~/.claude` itself is never changed. Nodes still run on the default
//! account.
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, AccountsConfig, Provider};
use alondra_core::claude_account;
use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use alondra_core::views::Lang;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::client_routes::Reply;
use crate::domain_runtime::DomainRuntime;

pub(crate) const PATH: &str = "/settings/accounts";
/// Checks every account's sign-in now, as after the user signs one in.
pub(crate) const CHECK: &str = "/settings/accounts/check";
const CHECK_EVERY: Duration = Duration::from_secs(10 * 60);

/// How Claude Code runs to read or end an account's sign-in: the executable and environment the
/// daemon's nodes get.
#[derive(Clone, Debug)]
pub(crate) struct Claude {
    pub executable: PathBuf,
    pub user_home: PathBuf,
    pub path: String,
    pub username: String,
}

impl Claude {
    fn of(domain: &DomainRuntime) -> Option<Self> {
        let (backend, _) = domain.sdk()?;
        let path = backend.native_environment.get("PATH").cloned().unwrap_or_default();
        let executable = if backend.installed_native { claude_account::executable(&path)? } else { backend.native.clone() };
        let user_home = backend.native_environment.get("HOME").map(PathBuf::from).unwrap_or_else(crate::options::user_home);
        Some(Claude { executable, user_home, path, username: claude_account::effective_username()? })
    }

    fn status(&self, folder: Option<&Path>) -> Result<claude_account::AuthStatus, String> {
        claude_account::status(&self.executable, &self.user_home, &self.path, &self.username, folder)
    }

    fn logout(&self, folder: &Path) -> Result<(), String> {
        claude_account::logout(&self.executable, &self.user_home, &self.path, &self.username, folder)
    }
}

fn config(home: &Path) -> Result<AccountsConfig, String> {
    let path = alondra_core::options::path_in(home);
    match std::fs::read_to_string(&path) {
        Ok(text) => alondra_core::options::OptionsFile::parse(&text).map(|file| file.accounts).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(AccountsConfig::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// What a change must name so it never undoes another one it did not see.
fn version(config: &AccountsConfig) -> String {
    use sha2::Digest as _;
    sha2::Sha256::digest(config.to_toml().as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// Every account in the user's order with what `claude auth status` says of it now. A disabled
/// account is not checked.
pub(crate) fn observe(home: &Path, claude: Option<&Claude>, config: &AccountsConfig) -> Vec<AccountStatus> {
    config
        .accounts(Provider::Claude)
        .into_iter()
        .map(|account| {
            let configured_plan = match &account.billing {
                AccountBilling::Subscription { plan, .. } => plan.clone(),
                AccountBilling::PayPerUse => None,
            };
            let mut status = AccountStatus {
                provider: Provider::Claude, id: account.id.clone(), name: account.name.clone(), state: AccountState::Disabled,
                email: None, plan: configured_plan.clone(), billing: account.billing.clone(),
            };
            if !account.enabled {
                return status;
            }
            let folder = alondra_core::accounts::folder(home, &account.id);
            let seen = match (claude, &folder) {
                (_, Some(folder)) if !folder.is_dir() => Ok(claude_account::AuthStatus::default()),
                (Some(claude), folder) => claude.status(folder.as_deref()),
                (None, _) => Err("no Claude Code to ask".to_owned()),
            };
            match seen {
                Ok(seen) if seen.logged_in => {
                    status.state = AccountState::SignedIn;
                    status.email = seen.email;
                    status.plan = configured_plan.or(seen.subscription);
                }
                Ok(_) => status.state = AccountState::SignedOut,
                Err(why) => {
                    log!("could not check the sign-in of account {}: {why}", account.id);
                    status.state = AccountState::Unavailable;
                }
            }
            status
        })
        .collect()
}

/// Records `accounts` in the domain when it holds something else.
fn record(domain: &DomainRuntime, accounts: Vec<AccountStatus>) {
    for _ in 0..5 {
        let state = domain.read();
        if state.state().accounts == accounts {
            return;
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.state().data_epoch.clone(),
            command_id: format!("accounts-{}", crate::auth::random_hex(16)).into(),
            expected_revision: state.state().revision,
            command: Command::RecordAccounts { accounts: accounts.clone() },
        };
        match domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            Ok(_) => return,
            Err(error) => log!("could not record the accounts: {error}"),
        }
    }
}

/// Checks every account's sign-in and records what it saw.
pub(crate) fn check(domain: &DomainRuntime, home: &Path) {
    match config(home) {
        Ok(config) => record(domain, observe(home, Claude::of(domain).as_ref(), &config)),
        Err(why) => log!("accounts not checked: {why}"),
    }
}

/// Checks the accounts when the daemon starts, then every ten minutes.
pub(crate) async fn run(domain: Arc<DomainRuntime>, home: PathBuf) {
    loop {
        let (checking, at) = (domain.clone(), home.clone());
        if tokio::task::spawn_blocking(move || check(&checking, &at)).await.is_err() {
            log!("the accounts check stopped");
        }
        tokio::time::sleep(CHECK_EVERY).await;
    }
}

pub(crate) fn read(domain: &DomainRuntime, home: &Path) -> Reply {
    let config = match config(home) {
        Ok(config) => config,
        Err(why) => return Reply { status: StatusCode::INTERNAL_SERVER_ERROR, body: json!({"error": "file", "message": why}) },
    };
    let seen = domain.read().state().accounts.clone();
    let accounts: Vec<_> = config
        .accounts(Provider::Claude)
        .into_iter()
        .map(|account| {
            let status = seen.iter().find(|s| s.provider == Provider::Claude && s.id == account.id);
            let state = match status {
                Some(status) if account.enabled && status.state == AccountState::Disabled => AccountState::Unchecked,
                Some(status) => status.state,
                None if account.enabled => AccountState::Unchecked,
                None => AccountState::Disabled,
            };
            json!({
                "provider": Provider::Claude, "id": account.id, "name": account.name, "enabled": account.enabled,
                "default": account.id.is_default(), "billing": account.billing, "state": state,
                "email": status.and_then(|s| s.email.clone()), "plan": status.and_then(|s| s.plan.clone()),
            })
        })
        .collect();
    Reply { status: StatusCode::OK, body: json!({"version": version(&config), "config": config, "accounts": accounts}) }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    version: String,
    accounts: AccountsConfig,
}

/// Replaces the `[accounts]` section with the one a client sent, if it saw the current one. A new
/// account links the `~/.claude` of the HOME the nodes' Claude runs in.
pub(crate) fn write(domain: &DomainRuntime, home: &Path, body: &serde_json::Value, lang: Lang) -> Reply {
    let change = match serde_json::from_value::<Change>(body.clone()) {
        Ok(change) => change,
        Err(e) => return Refused::Invalid(e.to_string()).reply(lang),
    };
    let _writing = crate::settings::WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let claude = Claude::of(domain);
    let user_home = claude.as_ref().map_or_else(crate::options::user_home, |claude| claude.user_home.clone());
    if let Err(refused) = store(home, &user_home, claude.as_ref(), &change.version, &change.accounts) {
        return refused.reply(lang);
    }
    check(domain, home);
    read(domain, home)
}

#[derive(Debug, PartialEq)]
pub(crate) enum Refused {
    Invalid(String),
    Stale,
    Folder(String),
    SignOut(String),
}

impl Refused {
    fn reply(self, lang: Lang) -> Reply {
        let es = lang == Lang::Es;
        let (status, code, message) = match self {
            Refused::Invalid(why) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", why),
            Refused::Stale => (StatusCode::CONFLICT, "stale", if es {
                "Las cuentas cambiaron mientras tanto: vuelve a intentarlo.".to_owned()
            } else {
                "The accounts changed in the meantime: try again.".to_owned()
            }),
            Refused::Folder(why) => (StatusCode::INTERNAL_SERVER_ERROR, "cannot_create", if es {
                format!("No se pudo preparar la cuenta: {why}")
            } else {
                format!("Couldn't prepare the account: {why}")
            }),
            Refused::SignOut(why) => (StatusCode::UNPROCESSABLE_ENTITY, "sign_out_failed", if es {
                format!("No se pudo cerrar la sesión de la cuenta, así que sigue: {why}")
            } else {
                format!("Couldn't sign the account out, so it stays: {why}")
            }),
        };
        Reply { status, body: json!({"error": code, "message": message}) }
    }
}

/// Writes `next` over the section `seen` names: prepares the folder of each account it adds,
/// signs out each one it takes away and deletes its folder once the file no longer names it.
pub(crate) fn store(home: &Path, user_home: &Path, claude: Option<&Claude>, seen: &str, next: &AccountsConfig) -> Result<(), Refused> {
    next.check().map_err(Refused::Invalid)?;
    let current = config(home).map_err(Refused::Invalid)?;
    if version(&current) != seen {
        return Err(Refused::Stale);
    }
    let ids = |config: &AccountsConfig| -> Vec<AccountId> { config.listed(Provider::Claude).iter().map(|a| a.id.clone()).filter(|id| !id.is_default()).collect() };
    let (before, after) = (ids(&current), ids(next));
    let shared = claude_account::default_folder(user_home);
    for id in after.iter().filter(|id| !before.contains(id)) {
        claude_account::prepare(home, id, &shared).map_err(|e| Refused::Folder(e.to_string()))?;
    }
    let removed: Vec<&AccountId> = before.iter().filter(|id| !after.contains(id)).collect();
    for id in &removed {
        sign_out(home, claude, id)?;
    }
    let file = alondra_core::options::path_in(home);
    let original = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Refused::Folder(format!("{}: {e}", file.display()))),
    };
    let text = alondra_core::accounts::with_accounts(&original, next).map_err(Refused::Invalid)?;
    crate::settings::replace(&file, text.as_bytes()).map_err(|e| Refused::Folder(format!("{}: {e}", file.display())))?;
    for id in removed {
        if let Err(e) = claude_account::remove(home, id) {
            log!("account {id} is gone, but its folder could not be deleted: {e}");
        }
    }
    Ok(())
}

/// Ends the sign-in of an account about to be removed. One never signed in needs nothing.
fn sign_out(home: &Path, claude: Option<&Claude>, id: &AccountId) -> Result<(), Refused> {
    let Some(folder) = alondra_core::accounts::folder(home, id).filter(|folder| folder.is_dir()) else { return Ok(()) };
    let claude = claude.ok_or_else(|| Refused::SignOut("Claude Code is not available on this Mac".into()))?;
    let seen = claude.status(Some(&folder)).map_err(Refused::SignOut)?;
    if seen.logged_in {
        claude.logout(&folder).map_err(Refused::SignOut)?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "accounts_tests.rs"]
mod tests;
