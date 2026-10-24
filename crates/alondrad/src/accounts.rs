//! The provider accounts nodes may run on: `GET/PUT /settings/accounts` read and change the
//! `[accounts]` section of `options.toml`, of which the daemon is the only writer, and the daemon
//! checks each account's sign-in and records what it saw in the domain, where every screen reads
//! it. An added account gets its folder, linked to the user's `~/.claude`; a removed one is signed
//! out and its folder deleted. `~/.claude` itself is never changed. Each launch starts on the
//! account the user's distribution picks (`choose_for_launch`).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountState, AccountStatus, AccountsConfig, Policy, Provider};
use alondra_core::claude_account;
use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use alondra_core::usage::Share;
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
#[derive(Clone)]
pub(crate) struct Claude {
    pub executable: PathBuf,
    pub environment: BTreeMap<String, String>,
    pub user_home: PathBuf,
    pub path: String,
    pub username: String,
}

impl Claude {
    fn of(domain: &DomainRuntime) -> Option<Self> {
        let (backend, _) = domain.sdk()?;
        let path = backend.native_environment.get("PATH").cloned().unwrap_or_default();
        let executable = if backend.installed_native { claude_account::executable(&path)? } else { backend.native.clone() };
        let user_home = Self::user_home(domain);
        Some(Claude { executable, environment: backend.native_environment.clone(), user_home, path, username: claude_account::effective_username()? })
    }

    /// The profile the default account runs with: the one the daemon was set up with.
    fn default_profile(&self) -> Option<PathBuf> {
        claude_account::configured_profile(&|key: &str| self.environment.get(key).cloned())
    }

    /// The HOME the nodes' Claude runs in, whose `~/.claude` the default account is.
    pub(crate) fn user_home(domain: &DomainRuntime) -> PathBuf {
        domain.sdk().and_then(|(backend, _)| backend.native_environment.get("HOME").map(PathBuf::from)).unwrap_or_else(crate::options::user_home)
    }

    fn status(&self, folder: Option<&Path>) -> Result<claude_account::AuthStatus, String> {
        claude_account::status(&self.executable, &self.environment, &self.user_home, &self.path, &self.username, folder)
    }

    fn logout(&self, folder: &Path) -> Result<(), String> {
        claude_account::logout(&self.executable, &self.environment, &self.user_home, &self.path, &self.username, folder)
    }
}

/// Whether a session one account started resumes with the other: both read the same conversations.
pub(crate) fn share_sessions(domain: &DomainRuntime, one: &AccountId, other: &AccountId) -> bool {
    let default_profile = domain.sdk().and_then(|(backend, _)| claude_account::configured_profile(&|key: &str| backend.native_environment.get(key).cloned()));
    claude_account::shares_sessions(domain.home(), &Claude::user_home(domain), default_profile.as_deref(), one, other)
}

pub(crate) fn config(home: &Path) -> Result<AccountsConfig, String> {
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
                email: None, plan: configured_plan.clone(), billing: account.billing.clone(), sign_ins: 0,
            };
            if !account.enabled {
                return status;
            }
            let folder = alondra_core::accounts::folder(home, &account.id);
            let seen = match (claude, &folder) {
                (_, Some(folder)) if !folder.is_dir() => Ok(claude_account::AuthStatus::default()),
                (Some(claude), _) => claude.status(claude_account::profile(home, &account.id, claude.default_profile().as_deref()).as_deref()),
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

/// The account a new launch, batch or scheduled run starts on, as the user's distribution picks it
/// from what the domain knows at `now`. When none can work, the Mac waits until the soonest one can
/// again: every launch, batch and schedule holds until then, as for a usage limit.
pub(crate) fn choose_for_launch(domain: &DomainRuntime, home: &Path, now: alondra_core::Millis) -> Option<AccountId> {
    let policy = config(home).map(|config| config.distribution.policy).unwrap_or_default();
    let read = domain.read();
    let state = read.state();
    let day = crate::node_reports::local_day(now);
    let chosen = alondra_core::usage::choose_account(policy, Provider::Claude, &state.accounts, &state.account_usage, state.account_reserve, now, &day);
    if chosen.is_none() {
        let until = alondra_core::usage::margin_returns_at(Provider::Claude, &state.accounts, &state.account_usage, now);
        drop(read);
        domain.live.usage_pause.exhaust_for_margin(until, now);
    }
    chosen
}

/// The Claude accounts as the domain last checked them, when none has a session to start work
/// with: `default (signed out), work (off)`. None while one may have it: signed in, not checked
/// yet, or not checkable.
pub(crate) fn none_signed_in(accounts: &[AccountStatus]) -> Option<String> {
    let claude: Vec<&AccountStatus> = accounts.iter().filter(|account| account.provider == Provider::Claude).collect();
    if claude.is_empty() || !claude.iter().all(|account| matches!(account.state, AccountState::SignedOut | AccountState::Disabled)) {
        return None;
    }
    let state = |state: AccountState| if state == AccountState::Disabled { "off" } else { "signed out" };
    Some(claude.iter().map(|account| format!("{} ({})", account.id, state(account.state))).collect::<Vec<_>>().join(", "))
}

/// Whether the orchestrator waits because no Claude account has a session: set and cleared once
/// per change, so the log and the screens say it once.
#[derive(Default)]
pub(crate) struct AccountWait(std::sync::Mutex<bool>);

impl AccountWait {
    pub(crate) fn is_waiting(&self) -> bool {
        *self.0.lock().expect("account wait poisoned")
    }

    /// True when the wait starts now.
    pub(crate) fn start(&self) -> bool {
        !std::mem::replace(&mut *self.0.lock().expect("account wait poisoned"), true)
    }

    /// True when a standing wait ends now.
    pub(crate) fn end(&self) -> bool {
        std::mem::replace(&mut *self.0.lock().expect("account wait poisoned"), false)
    }
}

/// Every account the user registered, on or off: the folders an execution may run Claude with.
pub(crate) fn registered(home: &Path) -> Vec<AccountId> {
    config(home).map_or_else(|_| vec![AccountId::default_account()], |config| config.accounts(Provider::Claude).into_iter().map(|account| account.id).collect())
}

/// The folders of every account but the default one: where each keeps its Claude settings.
pub(crate) fn profiles(home: &Path) -> Vec<PathBuf> {
    registered(home).iter().filter_map(|account| alondra_core::accounts::folder(home, account)).collect()
}

/// The generation of the credential new executions on `account` run with: one more each time it
/// was signed in again after its sign-in expired.
pub(crate) fn generation(state: &alondra_core::domain::DomainState, account: &AccountId) -> u64 {
    state.accounts.iter().find(|status| status.provider == Provider::Claude && status.id == *account).map_or(0, |status| status.sign_ins) + 1
}

/// What the domain keeps of the accounts but a check never sees: the generations of their credentials.
fn unchecked(accounts: &[AccountStatus]) -> Vec<AccountStatus> {
    accounts.iter().cloned().map(|status| AccountStatus { sign_ins: 0, ..status }).collect()
}

/// Records `accounts` and their `reserve` in the domain when it holds something else.
fn record(domain: &DomainRuntime, accounts: Vec<AccountStatus>, reserve: Share) {
    let state = domain.read();
    if unchecked(&state.state().accounts) == accounts && state.state().account_reserve == reserve {
        return;
    }
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.state().data_epoch.clone(),
        command_id: format!("accounts-{}", crate::auth::random_hex(16)).into(),
        expected_revision: 0,
        command: Command::RecordAccounts { accounts, reserve },
    };
    drop(state);
    if let Err(error) = domain.execute_as_system(envelope) {
        log!("could not record the accounts: {error}");
    }
}

/// Records how new work is spread, and whether nodes move between accounts, when the domain
/// holds another way.
pub(crate) fn record_distribution(domain: &DomainRuntime, policy: Policy, move_when_exhausted: bool) {
    let state = domain.read();
    if state.state().account_policy == policy && state.state().account_moves == move_when_exhausted {
        return;
    }
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.state().data_epoch.clone(),
        command_id: format!("account-policy-{}", crate::auth::random_hex(16)).into(),
        expected_revision: 0,
        command: Command::RecordDistribution { policy, move_when_exhausted },
    };
    drop(state);
    if let Err(error) = domain.execute_as_system(envelope) {
        log!("could not record how work is spread over the accounts: {error}");
    }
}

/// Checks every account's sign-in and records what it saw.
pub(crate) fn check(domain: &DomainRuntime, home: &Path) {
    match config(home) {
        Ok(config) => {
            record_distribution(domain, config.distribution.policy, config.distribution.move_when_exhausted);
            record(domain, observe(home, Claude::of(domain).as_ref(), &config), Share::of(config.distribution.reserve));
        }
        Err(why) => log!("accounts not checked: {why}"),
    }
}

/// Why a change from Settings was not made.
#[derive(Debug)]
pub(crate) enum ChangeRefused {
    /// The change's own rules refuse it.
    Domain(alondra_core::domain::DomainError),
    Store(Refused),
}

/// Makes a change the user asked for from Settings: judged by the guard of its command, written
/// to `options.toml`, recorded in the domain, and the accounts checked again.
pub(crate) fn change(domain: &DomainRuntime, home: &Path, actor: &Actor, change: &AccountChange) -> Result<(), ChangeRefused> {
    let _writing = crate::settings::WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let envelope = |state: &alondra_core::domain::DomainState| CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("accounts-change-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision,
        command: Command::ChangeAccounts { provider: Provider::Claude, change: change.clone() },
    };
    let working = {
        let read = domain.read();
        read.state().guard(actor, &envelope(read.state())).map_err(ChangeRefused::Domain)?;
        working(read.state())
    };
    let current = config(home).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let next = current.changed(Provider::Claude, change).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let claude = Claude::of(domain);
    let user_home = claude.as_ref().map_or_else(crate::options::user_home, |claude| claude.user_home.clone());
    store(home, &user_home, claude.as_ref(), &version(&current), &next, &working).map_err(ChangeRefused::Store)?;
    for _ in 0..5 {
        let read = domain.read();
        let envelope = envelope(read.state());
        drop(read);
        match domain.execute(actor, &envelope, None, || Ok(())) {
            Ok(_) => break,
            Err(crate::domain_store::StoreError::Domain(error)) if error.code == "stale_revision" => continue,
            Err(error) => {
                log!("the accounts changed in options.toml, but the domain did not record it: {error}");
                break;
            }
        }
    }
    check(domain, home);
    Ok(())
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

/// What the user is told about moving a node to another account when its own reaches its limit.
pub(crate) fn move_risk(lang: Lang) -> &'static str {
    match lang {
        Lang::Es => "Anthropic no dice si cambiar de cuenta al llegar al límite está permitido.",
        Lang::En => "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed.",
    }
}

pub(crate) fn read(domain: &DomainRuntime, home: &Path, lang: Lang) -> Reply {
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
    let moving = json!({"on": config.distribution.move_when_exhausted, "risk": move_risk(lang)});
    Reply { status: StatusCode::OK, body: json!({"version": version(&config), "config": config, "accounts": accounts, "move_when_exhausted": moving}) }
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
    let working = working(domain.read().state());
    if let Err(refused) = store(home, &user_home, claude.as_ref(), &change.version, &change.accounts, &working) {
        return refused.reply(lang);
    }
    check(domain, home);
    read(domain, home, lang)
}

/// The accounts the open tasks' executions run on.
fn working(state: &alondra_core::domain::DomainState) -> Vec<AccountId> {
    state.tasks.values().filter(|task| !task.over()).filter_map(|task| task.account.clone()).collect()
}

#[derive(Debug, PartialEq)]
pub(crate) enum Refused {
    Invalid(String),
    Stale,
    /// Open tasks still run on the account, by its name.
    InUse(String),
    Folder(String),
    SignOut(String),
}

impl Refused {
    pub(crate) fn message(self, lang: Lang) -> String {
        self.reply(lang).body["message"].as_str().unwrap_or_default().to_owned()
    }

    fn reply(self, lang: Lang) -> Reply {
        let es = lang == Lang::Es;
        let (status, code, message) = match self {
            Refused::Invalid(why) => (StatusCode::UNPROCESSABLE_ENTITY, "invalid_input", why),
            Refused::Stale => (StatusCode::CONFLICT, "stale", if es {
                "Las cuentas cambiaron mientras tanto: vuelve a intentarlo.".to_owned()
            } else {
                "The accounts changed in the meantime: try again.".to_owned()
            }),
            Refused::InUse(name) => (StatusCode::CONFLICT, "account_in_use", if es {
                format!("Aún hay tareas abiertas que trabajan con {name}: quítala cuando terminen.")
            } else {
                format!("Open tasks still run on {name}: remove it once they are done.")
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
/// signs out each one it takes away and deletes its folder once the file no longer names it. An
/// account open tasks still run on (`working`) is not taken away.
pub(crate) fn store(home: &Path, user_home: &Path, claude: Option<&Claude>, seen: &str, next: &AccountsConfig, working: &[AccountId]) -> Result<(), Refused> {
    next.check().map_err(Refused::Invalid)?;
    if !next.any_enabled(Provider::Claude) {
        return Err(Refused::Invalid("at least one Claude account stays on, or no node could start".into()));
    }
    let current = config(home).map_err(Refused::Invalid)?;
    if version(&current) != seen {
        return Err(Refused::Stale);
    }
    let ids = |config: &AccountsConfig| -> Vec<AccountId> { config.listed(Provider::Claude).iter().map(|a| a.id.clone()).filter(|id| !id.is_default()).collect() };
    let (before, after) = (ids(&current), ids(next));
    let shared = claude_account::config_folder(claude.and_then(Claude::default_profile).as_deref(), user_home);
    for id in after.iter().filter(|id| !before.contains(id)) {
        claude_account::prepare(home, id, &shared).map_err(|e| Refused::Folder(e.to_string()))?;
    }
    let removed: Vec<&AccountId> = before.iter().filter(|id| !after.contains(id)).collect();
    if let Some(busy) = removed.iter().find(|id| working.contains(id)) {
        let name = current.find(Provider::Claude, busy.as_str()).map_or_else(|| busy.to_string(), |account| account.name);
        return Err(Refused::InUse(name));
    }
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
