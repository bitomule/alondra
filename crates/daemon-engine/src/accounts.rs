//! The provider accounts nodes may run on: `GET/PUT /settings/accounts` read and change the
//! `[accounts]` section of `options.toml`, of which the daemon is the only writer, and the daemon
//! checks each account's sign-in and records what it saw in the domain, where every screen reads
//! it. An added account gets its folder, linked to the user's `~/.claude`; a removed one is signed
//! out and its folder deleted. `~/.claude` itself is never changed. Each launch starts on the
//! account the user's distribution picks (`choose_for_launch`).
use crate::locks::Poisonable as _;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::accounts::{AccountBilling, AccountChange, AccountId, AccountKey, AccountState, AccountStatus, AccountsConfig, Policy, Provider};
use alondra_core::claude_account;
use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
use alondra_core::usage::Share;
use alondra_core::views::Lang;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::reply::Reply;
use crate::domain_runtime::DomainRuntime;

pub const PATH: &str = "/settings/accounts";
/// Checks every account's sign-in now, as after the user signs one in.
pub const CHECK: &str = "/settings/accounts/check";
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
    pub timeout: Option<Duration>,
}

impl Claude {
    fn of(domain: &DomainRuntime) -> Option<Self> {
        let (backend, _) = domain.sdk()?;
        let backend = backend.for_provider(Provider::Claude, domain.home()).ok()?;
        let path = backend.native_environment.get("PATH").cloned().unwrap_or_default();
        let executable = if backend.installed_native { claude_account::executable(&path)? } else { backend.native.clone() };
        let user_home = Self::user_home(domain);
        Some(Claude { executable, environment: backend.native_environment.clone(), user_home, path, username: claude_account::effective_username()?, timeout: (!domain.live.unbounded_claude_status.load(std::sync::atomic::Ordering::Relaxed)).then_some(claude_account::STATUS_TIMEOUT) })
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
        claude_account::status(&self.executable, &self.environment, &self.user_home, &self.path, &self.username, folder, self.timeout)
    }

    fn logout(&self, folder: &Path) -> Result<(), String> {
        claude_account::logout(&self.executable, &self.environment, &self.user_home, &self.path, &self.username, folder, self.timeout)
    }
}

#[derive(Clone)]
pub(crate) struct Codex {
    executable: PathBuf,
    environment: BTreeMap<String, String>,
    user_home: PathBuf,
    path: String,
    username: String,
}

impl Codex {
    pub(crate) fn profile(&self, home: &Path, key: &AccountKey) -> PathBuf {
        let profile = alondra_core::accounts::provider_folder(home, key)
            .or_else(|| self.environment.get("CODEX_HOME").filter(|value| !value.is_empty()).map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from(".codex"));
        self.user_home.join(profile)
    }
    pub(crate) fn of(domain: &DomainRuntime) -> Option<Self> {
        let (backend, _) = domain.sdk()?;
        let backend = backend.for_provider(Provider::Codex, domain.home()).ok()?;
        let path = backend.native_environment.get("PATH").cloned().unwrap_or_default();
        let executable = if backend.installed_native { alondra_core::codex_account::executable(&path)? } else { backend.native.clone() };
        let user_home = backend.native_environment.get("HOME").map(PathBuf::from).unwrap_or_else(|| Claude::user_home(domain));
        Some(Self { executable, environment: backend.native_environment.clone(), user_home, path, username: claude_account::effective_username()? })
    }

    pub(crate) fn command(&self, profile: Option<&Path>) -> std::process::Command {
        alondra_core::codex_account::command(&self.executable, &self.environment, &self.user_home, &self.path, &self.username, profile)
    }

    fn probe(&self, home: &Path, key: &AccountKey, now: alondra_core::Millis) -> Result<alondra_core::codex_account::ProbeReport, String> {
        let folder = alondra_core::accounts::provider_folder(home, key);
        alondra_core::codex_account::probe(self.command(folder.as_deref()), key.clone(), now)
    }

    fn probe_matching(&self, home: &Path, key: &AccountKey, now: alondra_core::Millis, accept: impl FnOnce(&alondra_core::codex_account::ProbeReport) -> bool) -> Result<alondra_core::codex_account::ProbeReport, String> {
        let folder = alondra_core::accounts::provider_folder(home, key);
        alondra_core::codex_account::probe_matching(self.command(folder.as_deref()), key.clone(), now, accept)
    }
}

/// Whether a session one account started resumes with the other: both read the same conversations.
pub(crate) fn share_sessions(domain: &DomainRuntime, one: &AccountId, other: &AccountId) -> bool {
    let default_profile = domain.sdk().and_then(|(backend, _)| claude_account::configured_profile(&|key: &str| backend.native_environment.get(key).cloned()));
    claude_account::shares_sessions(domain.home(), &Claude::user_home(domain), default_profile.as_deref(), one, other)
}

pub fn config(home: &Path) -> Result<AccountsConfig, String> {
    options_file(home).map(|file| file.accounts)
}

fn options_file(home: &Path) -> Result<alondra_core::options::OptionsFile, String> {
    let path = alondra_core::options::path_in(home);
    match crate::domain_store::provider_settings::read_options(home)? {
        Some(text) => alondra_core::options::OptionsFile::parse(&text).map_err(|why| format!("{}: {why}", path.display())),
        None => Ok(alondra_core::options::OptionsFile::fresh()),
    }
}

/// What a change must name so it never undoes another one it did not see.
fn version(config: &AccountsConfig) -> String {
    alondra_core::accounts::config_version(config)
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
                native_identity: None, email: None, plan: configured_plan.clone(), billing: account.billing.clone(), sign_ins: 0, profile: None,
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
                    if seen.pay_per_use { status.billing = AccountBilling::PayPerUse; status.plan = None; }
                    else { status.plan = configured_plan.or(seen.subscription); }
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
/// again, or until one has a session when none has: every launch, batch and schedule holds until
/// then, as for a usage limit. One that can work ends a wait for a session.
pub(crate) fn choose_for_launch(domain: &DomainRuntime, home: &Path, now: alondra_core::Millis) -> Option<AccountId> {
    let policy = config(home).map(|config| config.distribution.policy).unwrap_or_default();
    let read = domain.read();
    let state = read.state();
    let day = crate::node_reports::local_day(now);
    let chosen = alondra_core::usage::choose_account(policy, Provider::Claude, &state.accounts, &state.account_usage, state.account_reserve, now, &day);
    drop(read);
    match chosen {
        Some(_) => crate::usage_pause::session_is_back(domain),
        None => crate::usage_probe::hold(domain, now),
    }
    chosen
}

pub(crate) fn select_for_launch(domain: &DomainRuntime, home: &Path, intent: &alondra_core::model_selection::TaskSelection, now: alondra_core::Millis)
    -> Result<alondra_core::model_selection::SelectionDecision, alondra_core::domain::DomainError>
{
    select_for_node(domain, home, intent, None, None, now)
}

pub fn select_for_active_model(domain: &DomainRuntime, task_id: &alondra_core::domain::TaskId,
    intent: &alondra_core::model_selection::TaskSelection) -> Result<alondra_core::model_selection::SelectionDecision, alondra_core::domain::DomainError>
{
    use alondra_core::domain::DomainError;
    let read=domain.read(); let state=read.state();
    let task=state.tasks.get(task_id).ok_or_else(||DomainError::new("not_found","Task missing"))?;
    let account=task.account_key();
    if task.over() { return Err(DomainError::new("invalid_input","An ended node cannot change its model")); }
    let mut decision=alondra_core::model_selection::resolve_selection(intent,&state.model_profiles,&state.model_catalogs,std::slice::from_ref(&account))
        .map_err(|reasons|DomainError::new("model_unavailable",format!("Choose an advertised model for the node's current account: {}",reasons.join("; "))))?;
    decision.selection.reasoning_effort=task.selection.as_ref().and_then(|selection|selection.effective.as_ref()).and_then(|effective|effective.reasoning_effort.clone());
    let catalog=state.model_catalogs.iter().find(|catalog|catalog.account==account).ok_or_else(||DomainError::new("model_unavailable","Refresh the current account's model catalog"))?;
    catalog.validate(&decision.selection).map_err(|reason|DomainError::new("model_unavailable",reason))?;
    if decision.account!=account || decision.selection.model.provider!=task.provider { return Err(DomainError::new("invalid_input","A running node keeps its original provider and account")); }
    Ok(decision)
}

pub(crate) fn select_for_launch_with_tools(domain: &DomainRuntime, home: &Path, intent: &alondra_core::model_selection::TaskSelection,
    denied_tools: &[String], now: alondra_core::Millis) -> Result<alondra_core::model_selection::SelectionDecision, alondra_core::domain::DomainError>
{
    select_for_node_with_tools(domain, home, intent, None, None, denied_tools, now)
}

pub(crate) fn select_for_node(domain: &DomainRuntime, home: &Path, intent: &alondra_core::model_selection::TaskSelection,
    preferred_account: Option<&AccountKey>, provider: Option<Provider>, now: alondra_core::Millis) -> Result<alondra_core::model_selection::SelectionDecision, alondra_core::domain::DomainError>
{
    select_for_node_with_tools(domain, home, intent, preferred_account, provider, &[], now)
}

pub fn begin_initial_check(domain: &DomainRuntime) {
    domain.live.initial_account_check_pending.store(true, std::sync::atomic::Ordering::Release);
}

pub fn require_initial_check(domain: &DomainRuntime) -> Result<(), alondra_core::domain::DomainError> {
    if domain.live.initial_account_check_pending.load(std::sync::atomic::Ordering::Acquire) {
        return Err(alondra_core::domain::DomainError::new("account_check_in_progress", "Accounts are being checked after startup. New nodes will wait for current authentication and billing information."));
    }
    Ok(())
}

pub(crate) fn select_for_node_with_tools(domain: &DomainRuntime, home: &Path, intent: &alondra_core::model_selection::TaskSelection,
    preferred_account: Option<&AccountKey>, provider: Option<Provider>, denied_tools: &[String], now: alondra_core::Millis) -> Result<alondra_core::model_selection::SelectionDecision, alondra_core::domain::DomainError>
{
    use alondra_core::domain::DomainError;
    require_initial_check(domain)?;
    let config = config(home).map_err(|why| DomainError::new("settings_unavailable", why))?;
    let read = domain.read();
    let state = read.state();
    if let Some(pinned) = &intent.account
        && intent.paid_account_grant.as_ref() != Some(pinned)
        && state.accounts.iter().any(|account| account.provider == pinned.provider && account.id == pinned.account && account.billing == AccountBilling::PayPerUse)
    { return Err(DomainError::new("paid_authorization_required", "Select this pay-per-use account again in the task or schedule form to authorize billable usage; a legacy or model-selected pin does not grant permission")); }
    let mut eligible = alondra_core::usage::eligible_account_keys(&state.account_order, &state.accounts, &state.account_usage, now);
    if let Some(pinned) = &intent.account {
        let configured = config.accounts(pinned.provider).iter().any(|account| account.id == pinned.account && account.enabled);
        let explicit_paid = intent.paid_account_grant.as_ref() == Some(pinned) && state.accounts.iter().any(|account| account.provider == pinned.provider && account.id == pinned.account && account.state == AccountState::SignedIn && account.billing == AccountBilling::PayPerUse);
        if configured && explicit_paid && !eligible.contains(pinned) { eligible.push(pinned.clone()); }
    }
    let effective_denied = !denied_tools.is_empty() || intent.native_tool_denials.as_ref().is_some_and(|tools| !tools.is_empty());
    eligible.retain(|key| config.accounts(key.provider).iter().any(|account| account.id == key.account && account.enabled)
        && provider.is_none_or(|provider| key.provider == provider) && (!effective_denied || key.provider != Provider::Codex));
    let mut eligible = alondra_core::usage::ranked_for_launch(state.account_policy, &eligible, &state.accounts, &state.account_usage, state.account_reserve, now, &crate::node_reports::local_day(now));
    if let Some(preferred) = preferred_account
        && let Some(at) = eligible.iter().position(|key| key == preferred) { let first = eligible.remove(at); eligible.insert(0, first); }
    if eligible.is_empty() { return Err(DomainError::new("account_unavailable", "No enabled compatible account is signed in with available quota; open Accounts to connect or refresh it")); }
    alondra_core::model_selection::resolve_selection(intent, &state.model_profiles, &state.model_catalogs, &eligible)
        .map_err(|reasons| DomainError::new("model_unavailable", reasons.join("; ")))
}

/// Claude answered while no account had a session: the accounts are checked now, not at the next
/// round, so the wait ends as soon as the check sees the session.
pub(crate) fn answered(domain: &DomainRuntime) {
    if domain.live.usage_pause.waits_for_session() {
        domain.live.account_check.notify_one();
    }
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

/// Whether the orchestrator waits for an account or a model, and why: set and cleared once per
/// change, so the log and the screens say each reason once.
/// A holder that panics leaves it whole, so it is recovered: one assignment or take.
#[derive(Default)]
pub(crate) struct AccountWait(std::sync::Mutex<Option<String>>);

impl AccountWait {
    pub(crate) fn is_waiting(&self) -> bool {
        self.0.lock_or_recover("the account wait").is_some()
    }

    /// True when the wait starts now.
    pub(crate) fn start(&self) -> bool {
        let mut wait = self.0.lock_or_recover("the account wait");
        if wait.is_some() { return false; }
        *wait = Some(String::new());
        true
    }

    /// True when the wait starts now or its reason changed.
    pub(crate) fn wait_for(&self, reason: &str) -> bool {
        let mut wait = self.0.lock_or_recover("the account wait");
        if wait.as_deref() == Some(reason) { return false; }
        *wait = Some(reason.to_owned());
        true
    }

    pub(crate) fn reason(&self) -> Option<String> {
        self.0.lock_or_recover("the account wait").clone()
    }

    /// True when a standing wait ends now.
    pub(crate) fn end(&self) -> bool {
        self.0.lock_or_recover("the account wait").take().is_some()
    }
}

/// Every account the user registered, on or off: the folders an execution may run Claude with.
pub(crate) fn registered(home: &Path) -> Vec<AccountId> {
    registered_provider(home, Provider::Claude)
}

pub(crate) fn registered_provider(home: &Path, provider: Provider) -> Vec<AccountId> {
    config(home).map_or_else(|_| vec![], |config| config.accounts(provider).into_iter().map(|account| account.id).collect())
}

/// The folders of every account but the default one: where each keeps its Claude settings.
pub(crate) fn profiles(home: &Path) -> Vec<PathBuf> {
    registered(home).iter().filter_map(|account| alondra_core::accounts::folder(home, account)).collect()
}

/// The generation of the credential new executions on `account` run with: one more each time it
/// was signed in again after its sign-in expired.
pub(crate) fn generation(state: &alondra_core::domain::DomainState, account: &AccountId) -> u64 {
    generation_provider(state, &AccountKey::new(Provider::Claude, account.clone()))
}

pub(crate) fn generation_provider(state: &alondra_core::domain::DomainState, account: &AccountKey) -> u64 {
    state.accounts.iter().find(|status| status.provider == account.provider && status.id == account.account).map_or(0, |status| status.sign_ins) + 1
}

/// What the domain keeps of the accounts but a check never sees: the generations of their credentials.
fn unchecked(accounts: &[AccountStatus]) -> Vec<AccountStatus> {
    accounts.iter().cloned().map(|status| AccountStatus { sign_ins: 0, ..status }).collect()
}

pub(crate) struct ProbeFence {
    state: Arc<alondra_core::domain::DomainState>,
    options: Option<String>,
}

pub(crate) fn probe_fence(domain: &DomainRuntime, home: &Path) -> Result<(ProbeFence, alondra_core::options::OptionsFile), String> {
    let options = crate::domain_store::provider_settings::read_options(home)?;
    let file = match options.as_deref() {
        Some(text) => alondra_core::options::OptionsFile::parse(text)?,
        None => alondra_core::options::OptionsFile::fresh(),
    };
    Ok((ProbeFence { state: Arc::clone(domain.read().state()), options }, file))
}

pub(crate) fn publish_probe(domain: &DomainRuntime, home: &Path, fence: ProbeFence, commands: Vec<Command>) -> Result<(), String> {
    publish_probe_with(domain, home, fence, commands, |_| Ok(()))
}

fn publish_probe_with<T>(domain: &DomainRuntime, home: &Path, fence: ProbeFence, commands: Vec<Command>, finish: impl FnOnce(&mut crate::domain_store::OwnerTransaction<'_>) -> Result<T, crate::domain_store::StoreError>) -> Result<T, String> {
    commit_probe(domain, home, fence, commands, finish).map_err(|why| why.to_string())
}

pub(crate) fn publish_current_probe(domain: &DomainRuntime, home: &Path, fence: ProbeFence, commands: Vec<Command>) -> Result<(), crate::domain_store::StoreError> {
    commit_probe(domain, home, fence, commands, |_| Ok(()))
}

fn commit_probe<T>(domain: &DomainRuntime, home: &Path, fence: ProbeFence, commands: Vec<Command>, finish: impl FnOnce(&mut crate::domain_store::OwnerTransaction<'_>) -> Result<T, crate::domain_store::StoreError>) -> Result<T, crate::domain_store::StoreError> {
    use crate::domain_store::{Attached, StoreError};
    use alondra_core::domain::DomainError;
    let scope = alondra_store::AuthorityScope::new(fence.state.data_epoch.clone()).map_err(StoreError::from)?;
    domain.own(scope, |transaction| {
        let current = transaction.domain_state();
        if current.data_epoch != fence.state.data_epoch
            || current.account_config_version != fence.state.account_config_version
            || current.model_profiles_version != fence.state.model_profiles_version
            || current.accounts != fence.state.accounts
            || crate::domain_store::provider_settings::read_options(home).map_err(|why| StoreError::Domain(DomainError::new("stale_probe", why)))? != fence.options
        {
            return Err(DomainError::new("stale_probe", "The account or settings changed while native metadata was being read; refresh the account again").into());
        }
        for command in commands {
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: fence.state.data_epoch.clone(),
                command_id: format!("provider-probe-{}", crate::auth::random_hex(16)).into(), expected_revision: 0, command };
            let stale = match &envelope.command {
                Command::RecordAccountUsage { provider, account, at, .. } => transaction.domain_state().account_usage.iter()
                    .filter(|usage| usage.provider == *provider && usage.account == *account).filter_map(|usage| usage.measured_at()).any(|measured| measured > *at),
                Command::RecordModelCatalog { catalog } => transaction.domain_state().model_catalogs.iter()
                    .any(|known| known.account == catalog.account && known.measured_at > catalog.measured_at),
                _ => false,
            };
            if stale {
                return Err(DomainError::new("stale_probe", "Newer metadata for this account was already recorded; refresh again").into());
            }
            transaction.stage_command(&Actor::System, &envelope, Attached::None)?;
        }
        finish(transaction)
    }).map(|(value, _)| value)
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct NativeAuthAccount {
    auth_method: String,
    email: Option<String>,
    workspace_id: Option<String>,
}

impl NativeAuthAccount {
    fn load(directory: &Path) -> Result<Self, String> {
        use std::io::Read;
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let path = directory.join("native-auth-account.json");
        let expected = std::fs::symlink_metadata(&path).map_err(|_| "This native session has no verified account identity; keep it paused and create a new node after signing in".to_owned())?;
        let owner = std::fs::metadata(directory).map_err(|why| why.to_string())?;
        if !expected.is_file() || expected.file_type().is_symlink() || expected.nlink() != 1 || expected.uid() != owner.uid() || expected.mode() & 0o777 != 0o600 || expected.len() > 4096 {
            return Err("The native session account identity is not a private file".into());
        }
        let mut file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path).map_err(|why| why.to_string())?;
        let actual = file.metadata().map_err(|why| why.to_string())?;
        if expected.ino() != actual.ino() || expected.dev() != actual.dev() { return Err("The native session account identity changed while opening it".into()); }
        let mut bytes = vec![];
        file.by_ref().take(4097).read_to_end(&mut bytes).map_err(|why| why.to_string())?;
        if bytes.len() > 4096 { return Err("The native session account identity is too large".into()); }
        serde_json::from_slice(&bytes).map_err(|_| "The native session account identity is invalid".into())
    }

    fn matches(&self, report: &alondra_core::codex_account::ProbeReport) -> bool {
        self.auth_method == "chatgpt" && report.auth_method.as_deref() == Some("chatgpt")
            && self.workspace_id.as_deref().zip(report.workspace_id.as_deref()).is_some_and(|(before, after)| !before.is_empty() && before == after)
            && self.email.as_deref().zip(report.email.as_deref()).is_some_and(|(before, after)| !before.trim().is_empty() && before.trim().eq_ignore_ascii_case(after.trim()))
    }
}

/// Refreshes only the account of this existing native session, including when disabled for new work.
pub(crate) fn probe_current_codex(domain: &DomainRuntime, task_id: &alondra_core::domain::TaskId, hold: Option<alondra_core::domain::ClaudeHold>) -> Result<bool, String> {
    use alondra_core::domain::{ClaudeHold, DomainError};
    use crate::domain_store::{Attached, StoreError};
    if hold == Some(ClaudeHold::SignedOut) { return Err("Only the user's answer checks a signed-out native session".into()); }
    let (fence, file) = probe_fence(domain, domain.home())?;
    let task = fence.state.tasks.get(task_id).filter(|task| !task.over() && task.provider == Provider::Codex && task.selection.is_some())
        .ok_or("The current Codex task is no longer active")?.clone();
    let key = task.account_key();
    let configured = file.accounts.accounts(Provider::Codex).into_iter().find(|account| account.id == key.account)
        .ok_or("This existing task's account is no longer configured")?;
    let stamp = fence.state.lifecycle_of(task_id).map_err(|why| why.to_string())?.execution().cloned().ok_or("This task has no existing native execution")?;
    let (backend, _) = domain.sdk().ok_or("The native SDK host is unavailable")?;
    let backend = backend.for_provider(Provider::Codex, domain.home()).map_err(|why| why.to_string())?;
    let directory = backend.state_root.join(stamp.instance().as_str());
    let expected = crate::sdk_owner::NativeAccountIdentity::load(&directory).map_err(|why| why.to_string())?
        .ok_or("This task has no original native account identity")?;
    let native = NativeAuthAccount::load(&directory)?;
    let original = fence.state.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account)
        .ok_or("This task's original account metadata is unavailable")?;
    if expected.change(original).is_some() { return Err("This account's known identity or original billing policy changed; its existing session will remain paused".into()); }
    let now = alondra_core::now_millis();
    let report = Codex::of(domain).ok_or("Codex is unavailable on this Mac")?.probe_matching(domain.home(), &key, now, |report|
        report.signed_in && report.supported_authentication() && native.matches(report))?;
    let mut observed = original.clone();
    observed.email.clone_from(&report.email);
    observed.native_identity = report.native_identity();
    observed.plan.clone_from(&report.plan);
    observed.billing = if report.pay_per_use { AccountBilling::PayPerUse } else { configured.billing };
    observed.state = if !configured.enabled { AccountState::Disabled }
        else if !report.signed_in { AccountState::SignedOut }
        else if report.supported_authentication() { AccountState::SignedIn } else { AccountState::Unavailable };
    let selected = task.selection.as_ref().expect("typed selection checked");
    let paid_authorized = selected.account.as_ref() == Some(&key) && selected.paid_account_grant.as_ref() == Some(&key);
    let refusal = if !report.signed_in { Some("Sign in to this task's original Codex account before continuing".to_owned()) }
        else if !report.supported_authentication() { Some("This native authentication method is unsupported; the existing task remains paused".to_owned()) }
        else if !native.matches(&report) { Some("The current native account does not prove the original managed account and workspace; the existing task remains paused".to_owned()) }
        else if report.ordinary_usage_allowed() != Some(true) { Some("The native backend has not confirmed ordinary included usage is available; this existing task remains paused".to_owned()) }
        else if expected.change(&observed).is_some() { Some("This account's known identity or original billing policy changed; the existing task remains paused".to_owned()) }
        else if matches!(observed.billing, AccountBilling::PayPerUse) && !paid_authorized { Some("This task has no original authorization for this paid account".to_owned()) }
        else { None };
    let mut statuses = unchecked(&fence.state.accounts);
    *statuses.iter_mut().find(|account| account.provider == key.provider && account.id == key.account).expect("original account checked") = observed;
    let mut commands = vec![Command::RecordAccounts { accounts: statuses, reserve: fence.state.account_reserve }];
    if refusal.is_none() {
        if report.catalog_error.is_none() { commands.push(Command::RecordModelCatalog { catalog: report.catalog }); }
        if let Some(limits) = report.rate_limits {
            let reading = alondra_core::codex_account::parse_rate_limits(&limits, report.plan)?;
            if !reading.windows.is_empty() { commands.push(Command::RecordAccountUsage { provider: key.provider, account: key.account.clone(), reading, at: now, day: crate::node_reports::local_day(now) }); }
        }
    }
    publish_probe_with(domain, domain.home(), fence, commands, |transaction| {
        let current = transaction.state().tasks.get(task_id).ok_or_else(|| DomainError::new("stale_probe", "The task no longer exists"))?;
        if current.over() || current.account_key() != key || current.selection != task.selection
            || transaction.state().lifecycle_of(task_id).map_err(StoreError::Domain)?.execution() != Some(&stamp)
            || NativeAuthAccount::load(&directory).map_err(|why| StoreError::Domain(DomainError::new("stale_probe", why)))? != native {
            return Err(DomainError::new("stale_probe", "The existing native session changed during its metadata check").into());
        }
        let binding = transaction.execution_binding(&stamp)?;
        if Provider::from_driver(binding.driver()) != Some(Provider::Codex) || crate::usage_probe::account_of(binding.credential()) != key.account {
            return Err(DomainError::new("stale_probe", "The task's native account binding changed").into());
        }
        if let Some(why) = refusal { return Ok(Err(why)); }
        transaction.require_native_identity(&binding, Some(&expected))?;
        if !crate::claude_access::native_capacity_for_existing(transaction.state(), &key, now) { return Ok(Ok(false)); }
        let Some(hold) = hold else { return Ok(Ok(true)); };
        let candidate = transaction.state().claude_held().into_iter().filter(|held| held.provider == key.provider && held.hold == hold && held.account == key.account)
            .min_by_key(|held| transaction.state().interactions.get(&held.card).map(|card| card.order));
        if candidate.as_ref().is_none_or(|candidate| candidate.task != *task_id) { return Ok(Ok(false)); }
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: transaction.state().data_epoch.clone(),
            command_id: format!("current-codex-probe-{}", crate::auth::random_hex(16)).into(), expected_revision: 0,
            command: Command::ProbeClaudeAccess { provider: key.provider, hold, account: Some(key.account.clone()) } };
        transaction.stage_command(&Actor::System, &envelope, Attached::None)?;
        Ok(Ok(true))
    })?
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

pub fn refresh_models(domain: &DomainRuntime, home: &Path, key: &AccountKey) -> Result<(), String> {
    refresh_models_current(domain, home, key).map_err(|why| why.to_string())
}

fn refresh_models_current(domain: &DomainRuntime, home: &Path, key: &AccountKey) -> Result<(), crate::domain_store::StoreError> {
    let (fence, catalog) = read_models(domain, home, key).map_err(|why| crate::domain_store::StoreError::Storage(anyhow::anyhow!(why)))?;
    publish_current_probe(domain, home, fence, vec![Command::RecordModelCatalog { catalog }])
}

fn read_models(domain: &DomainRuntime, home: &Path, key: &AccountKey) -> Result<(ProbeFence, alondra_core::model_selection::AccountModelCatalog), String> {
    let (fence, file) = probe_fence(domain, home)?;
    if !file.accounts.accounts(key.provider).iter().any(|account| account.id == key.account && account.enabled) {
        return Err("The account is not connected and enabled".into());
    }
    let catalog = match key.provider {
        Provider::Codex => {
            let report = Codex::of(domain).ok_or("Codex is not available on this Mac")?.probe(home, key, alondra_core::now_millis())?;
            if !report.signed_in { return Err("Sign in to this Codex account before refreshing its models".into()); }
            if let Some(why) = report.catalog_error { return Err(format!("Native model metadata is unavailable: {why}")); }
            report.catalog
        }
        Provider::Claude => {
            let (backend, _) = domain.sdk().ok_or("The Claude SDK host is not available on this Mac")?;
            crate::usage_probe::models(backend, home, key)?
        }
    };
    if catalog.models.is_empty() { return Err("This account did not advertise any models".into()); }
    Ok((fence, catalog))
}

/// Checks every account's sign-in and records what it saw.
pub fn check(domain: &DomainRuntime, home: &Path) {
    if check_current(domain, home) == crate::native_probe::Outcome::Stale {
        domain.live.account_check.notify_one();
    }
}

fn check_current(domain: &DomainRuntime, home: &Path) -> crate::native_probe::Outcome {
    if let Err(why) = migrate_legacy_settings(domain, home) { log!("Provider settings migration needs repair: {why}"); return crate::native_probe::Outcome::Finished; }
    match probe_fence(domain, home) {
        Ok((fence, file)) => {
            let prior_generations: Vec<_> = fence.state.accounts.iter().map(|account| (AccountKey::new(account.provider, account.id.clone()), account.sign_ins)).collect();
            let mut commands = vec![];
            let profiles_version = alondra_core::model_selection::profiles_version(&file.model_profiles);
            if domain.read().state().model_profiles_version != profiles_version {
                commands.push(Command::RecordModelProfiles { profiles: file.model_profiles, version: profiles_version });
            }
            let config = file.accounts;
            commands.push(Command::RecordDistribution { policy: config.distribution.policy, move_when_exhausted: config.distribution.move_when_exhausted });
            let claude = config.is_connected(Provider::Claude).then(|| Claude::of(domain)).flatten();
            let mut statuses = observe(home, claude.as_ref(), &config);
            let codex = config.is_connected(Provider::Codex).then(|| Codex::of(domain)).flatten();
            let now = alondra_core::now_millis();
            let mut reports = vec![];
            for account in config.accounts(Provider::Codex) {
                let key = AccountKey::new(Provider::Codex, account.id.clone());
                let mut status = AccountStatus { provider: Provider::Codex, id: account.id.clone(), name: account.name,
                    state: AccountState::Disabled, native_identity: None, email: None, plan: None, billing: account.billing, sign_ins: 0, profile: None };
                if account.enabled {
                    let folder = alondra_core::accounts::provider_folder(home, &key);
                    let result = if folder.as_ref().is_some_and(|folder| !folder.is_dir()) { Ok(None) }
                        else { codex.as_ref().ok_or_else(|| "Codex is not installed or its account probe is unavailable".to_owned()).and_then(|codex| codex.probe(home, &key, now)).map(Some) };
                    match result {
                        Ok(Some(report)) => {
                            status.state = if report.signed_in && !report.supported_authentication() { AccountState::Unavailable }
                                else if report.signed_in { AccountState::SignedIn } else { AccountState::SignedOut };
                            status.native_identity = report.native_identity();
                            status.email.clone_from(&report.email);
                            status.plan.clone_from(&report.plan);
                            if report.pay_per_use { status.billing = AccountBilling::PayPerUse; }
                            reports.push(report);
                        }
                        Ok(None) => status.state = AccountState::SignedOut,
                        Err(why) => { status.state = AccountState::Unavailable; log!("Codex account {} could not be checked: {why}", key.account); }
                    }
                }
                statuses.push(status);
            }
            let order: Vec<_> = config.ordered_accounts().into_iter().map(|(key, _)| key).collect();
            statuses.sort_by_key(|status| order.iter().position(|key| key.provider == status.provider && key.account == status.id));
            let user_home = Claude::user_home(domain);
            for status in &mut statuses { status.profile = alondra_core::accounts::shown_profile(home, &user_home, &AccountKey::new(status.provider, status.id.clone())); }
            commands.push(Command::RecordAccounts { accounts: statuses, reserve: Share::of(config.distribution.reserve) });
            commands.push(Command::RecordAccountConfiguration { order, version: version(&config) });
            for report in reports {
                if !report.signed_in { continue; }
                let key = report.catalog.account.clone();
                let ordinary_allowed = report.ordinary_usage_allowed() == Some(true);
                if !ordinary_allowed { log!("Codex account {} has no confirmed ordinary-usage permission; previous quota evidence is retained", key.account); }
                if let Some(why) = report.catalog_error { log!("Codex model metadata could not be refreshed: {why}"); }
                else { commands.push(Command::RecordModelCatalog { catalog: report.catalog }); }
                if let Some(limits) = report.rate_limits.filter(|_| ordinary_allowed) {
                    match alondra_core::codex_account::parse_rate_limits(&limits, report.plan) {
                        Ok(reading) if !reading.windows.is_empty() => commands.push(Command::RecordAccountUsage { provider: key.provider, account: key.account, reading, at: now, day: crate::node_reports::local_day(now) }),
                        Ok(_) => {}
                        Err(why) => log!("Codex quota could not be read: {why}"),
                    }
                }
            }
            if let Err(why) = publish_current_probe(domain, home, fence, commands) {
                log!("Discarded native account metadata: {why}");
                return crate::native_probe::Outcome::of(&why);
            }
            if domain.live.initial_account_check_pending.swap(false, std::sync::atomic::Ordering::AcqRel) {
                domain.live.gate.poke();
            }
            let refresh: Vec<_> = domain.read().state().accounts.iter().filter(|account| account.provider == Provider::Claude && account.state == AccountState::SignedIn)
                .filter(|account| {
                    let key = AccountKey::new(account.provider, account.id.clone());
                    prior_generations.iter().find(|(prior, _)| prior == &key).is_none_or(|(_, generation)| *generation != account.sign_ins)
                        || !domain.read().state().model_catalogs.iter().any(|catalog| catalog.account == key)
                }).map(|account| AccountKey::new(account.provider, account.id.clone())).collect();
            for key in refresh {
                if let Err(why) = refresh_models_current(domain, home, &key) {
                    log!("The connected Claude account's models are not ready: {why}");
                    if crate::native_probe::Outcome::of(&why) == crate::native_probe::Outcome::Stale {
                        return crate::native_probe::Outcome::Stale;
                    }
                }
            }
        }
        Err(why) => log!("accounts not checked: {why}"),
    }
    crate::native_probe::Outcome::Finished
}

/// Claude never names its native default model, so the orchestrator starts with opus until the
/// user picks another. True when the profile was missing.
fn seed_claude_orchestrator(profiles: &mut alondra_core::model_selection::ModelProfiles) -> bool {
    use alondra_core::model_selection::{ModelCandidate, ModelProfile, RoleModelPolicy};
    let claude = profiles.entry(Provider::Claude).or_default();
    if claude.contains_key(&ModelProfile::Orchestrator) { return false; }
    claude.insert(ModelProfile::Orchestrator, RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: "opus".into(), reasoning_effort: None }] });
    true
}

pub(crate) fn migrate_legacy_settings(domain: &DomainRuntime, home: &Path) -> Result<(), String> {
    let _writing = crate::settings_file::WRITER.lock_or_recover("the settings writer");
    recover_settings(domain, home)?;
    let Some(previous) = crate::domain_store::provider_settings::read_options(home)? else { return Ok(()) };
    let mut file = alondra_core::options::OptionsFile::parse(&previous)?;
    if file.accounts.connected.is_some() { return Ok(()) }
    file.accounts.migrate_legacy();
    seed_claude_orchestrator(&mut file.model_profiles);
    let target = alondra_core::accounts::with_accounts(&previous, &file.accounts)?;
    let target = alondra_core::options::with_model_profiles(&target, &file.model_profiles)?;
    let read = domain.read();
    let statuses = file.accounts.ordered_accounts().into_iter().map(|(key, account)| {
        read.state().accounts.iter().find(|old| old.provider == key.provider && old.id == key.account).cloned()
            .unwrap_or(AccountStatus { provider: key.provider, id: key.account, name: account.name, state: if account.enabled { AccountState::Unchecked } else { AccountState::Disabled },
                native_identity: None, email: None, plan: None, billing: account.billing, sign_ins: 0, profile: None })
    }).collect();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(), command_id: format!("provider-settings-migration-{}", crate::auth::random_hex(16)).into(),
        expected_revision: 0, command: Command::RecordAccounts { accounts: statuses, reserve: alondra_core::usage::Share::of(file.accounts.distribution.reserve) } };
    drop(read);
    let recorded = vec![Command::RecordAccountConfiguration { order: file.accounts.ordered_accounts().into_iter().map(|(key, _)| key).collect(), version: version(&file.accounts) },
        Command::RecordDistribution { policy: file.accounts.distribution.policy, move_when_exhausted: file.accounts.distribution.move_when_exhausted },
        Command::RecordModelProfiles { profiles: file.model_profiles.clone(), version: alondra_core::model_selection::profiles_version(&file.model_profiles) }];
    crate::domain_store::provider_settings::apply(domain, home, &Actor::System, envelope, Some(previous), target, recorded).map_err(|why| why.to_string())
}

/// Why a change from Settings was not made.
#[derive(Debug)]
pub enum ChangeRefused {
    /// The change's own rules refuse it.
    Domain(alondra_core::domain::DomainError),
    Store(Refused),
}

/// Makes a change the user asked for from Settings: judged by the guard of its command, written
/// to `options.toml`, recorded in the domain, and the accounts checked again.
pub(crate) fn change(domain: &DomainRuntime, home: &Path, actor: &Actor, change: &AccountChange) -> Result<(), ChangeRefused> {
    change_for_provider(domain, home, actor, Provider::Claude, change)
}

pub(crate) fn change_for_provider(domain: &DomainRuntime, home: &Path, actor: &Actor, provider: Provider, change: &AccountChange) -> Result<(), ChangeRefused> {
    change_for_provider_authorized(domain, home, actor, provider, change, || Ok(()))
}

pub fn change_for_provider_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, provider: Provider, change: &AccountChange,
    authorize: impl Fn() -> anyhow::Result<()>) -> Result<(), ChangeRefused>
{
    let current = config(home).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let state = domain.read();
    state.state().guard(actor, &CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.state().data_epoch.clone(),
        command_id: "account-change-preflight".into(), expected_revision: state.state().revision, command: Command::ChangeAccounts { provider, change: change.clone() } }).map_err(ChangeRefused::Domain)?;
    drop(state);
    let next = current.changed(provider, change).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    replace_configuration_authorized(domain, home, actor, &version(&current), &next, authorize)?;
    check(domain, home);
    Ok(())
}

/// Connects `provider` with the session the user signed in to on this Mac with the provider's own
/// tool: only the settings change, then every account is checked. Alondra never signs in.
pub fn connect(domain: &DomainRuntime, home: &Path, actor: &Actor, provider: Provider, authorize: impl Fn() -> anyhow::Result<()>) -> Result<(), ChangeRefused> {
    let current = config(home).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    if domain.read().state().account_config_version != version(&current) { check(domain, home); }
    change_for_provider_authorized(domain, home, actor, provider, &AccountChange::Connect, authorize)
}

pub(crate) fn replace_configuration_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, seen: &str, proposed: &AccountsConfig,
    authorize: impl Fn() -> anyhow::Result<()>) -> Result<(), ChangeRefused>
{
    let _writing = crate::settings_file::WRITER.lock_or_recover("the settings writer");
    recover_settings(domain, home).map_err(|why| ChangeRefused::Store(Refused::Folder(why)))?;
    let previous = crate::domain_store::provider_settings::read_options(home).map_err(|why| ChangeRefused::Store(Refused::Folder(why)))?;
    let file = previous.as_deref().map(alondra_core::options::OptionsFile::parse).transpose().map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?.unwrap_or_else(alondra_core::options::OptionsFile::fresh);
    let current = file.accounts;
    if version(&current) != seen { return Err(ChangeRefused::Store(Refused::Stale)); }
    let mut next = proposed.clone();
    if next.connected.is_none() && current.connected.is_some() && !current.is_connected(Provider::Claude) && next.claude != current.claude {
        if current.connected_providers().is_empty() && !next.claude.is_empty() { next.connected = Some(vec![Provider::Claude]); }
        else { return Err(ChangeRefused::Store(Refused::Invalid("Update this client before connecting another provider; its legacy configuration cannot change connected providers".into()))); }
    }
    next.preserve_connected_from(&current);
    next.check().map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let mut target = alondra_core::accounts::with_accounts(previous.as_deref().unwrap_or_default(), &next).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let mut recorded = vec![];
    let mut profiles = file.model_profiles;
    if !current.is_connected(Provider::Claude) && next.is_connected(Provider::Claude) && seed_claude_orchestrator(&mut profiles) {
        target = alondra_core::options::with_model_profiles(&target, &profiles).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
        recorded.push(Command::RecordModelProfiles { version: alondra_core::model_selection::profiles_version(&profiles), profiles });
    }
    let state = domain.read();
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.state().data_epoch.clone(),
        command_id: format!("account-configuration-{}", crate::auth::random_hex(16)).into(), expected_revision: 0,
        command: Command::ReplaceAccountConfiguration { accounts: next.to_toml(), expected_config_version: seen.into() } };
    drop(state);
    let claude = (current.is_connected(Provider::Claude) || next.is_connected(Provider::Claude)).then(|| Claude::of(domain)).flatten();
    let codex = (current.is_connected(Provider::Codex) || next.is_connected(Provider::Codex)).then(|| Codex::of(domain)).flatten();
    let user_home = claude.as_ref().map(|claude| claude.user_home.clone()).or_else(|| codex.as_ref().map(|codex| codex.user_home.clone())).unwrap_or_else(|| home.join("unavailable-native-home"));
    let refused = std::cell::RefCell::new(None);
    let removed = std::cell::RefCell::new(Vec::new());
    let result = crate::domain_store::provider_settings::apply_authorized(domain, home, actor, envelope, previous, target, recorded, authorize, |state| {
        match prepare_account_changes(home, &user_home, claude.as_ref(), codex.as_ref(), &current, &next, &working_keys(state)) {
            Ok(keys) => { *removed.borrow_mut() = keys; Ok(()) }
            Err(why) => { let message = format!("{why:?}"); *refused.borrow_mut() = Some(why); Err(crate::domain_store::StoreError::Storage(anyhow::anyhow!(message))) }
        }
    });
    if let Some(refused) = refused.into_inner() { return Err(ChangeRefused::Store(refused)); }
    match result {
        Ok(()) => { cleanup_account_folders(home, &removed.into_inner()); Ok(()) }
        Err(crate::domain_store::StoreError::Domain(why)) => Err(ChangeRefused::Domain(why)),
        Err(crate::domain_store::StoreError::Storage(why)) => Err(ChangeRefused::Store(Refused::Folder(format!("{why:#}")))),
    }
}

pub(crate) fn set_order(domain: &DomainRuntime, home: &Path, actor: &Actor, order: &[AccountKey], expected_config_version: &str) -> Result<(), ChangeRefused> {
    set_order_authorized(domain, home, actor, order, expected_config_version, || Ok(()))
}

pub fn set_order_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, order: &[AccountKey], expected_config_version: &str, authorize: impl Fn() -> anyhow::Result<()>) -> Result<(), ChangeRefused> {
    let _writing = crate::settings_file::WRITER.lock_or_recover("the settings writer");
    recover_settings(domain, home).map_err(|why| ChangeRefused::Store(Refused::Folder(why)))?;
    let previous = crate::domain_store::provider_settings::read_options(home).map_err(|why| ChangeRefused::Store(Refused::Folder(why)))?;
    let file = previous.as_deref().map(alondra_core::options::OptionsFile::parse).transpose().map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?.unwrap_or_else(alondra_core::options::OptionsFile::fresh);
    let current = file.accounts;
    if version(&current) != expected_config_version { return Err(ChangeRefused::Store(Refused::Stale)); }
    let mut next = current.clone();
    next.set_order(order.to_vec()).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let target = alondra_core::accounts::with_accounts(previous.as_deref().unwrap_or_default(), &next).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let command = Command::SetAccountOrder { order: order.to_vec(), expected_config_version: expected_config_version.into() };
    apply_settings_authorized(domain, home, actor, command, previous, target, vec![Command::RecordAccountConfiguration { order: order.to_vec(), version: version(&next) }], authorize)
}

pub(crate) fn set_model_profile(domain: &DomainRuntime, home: &Path, actor: &Actor, provider: Provider, profile: alondra_core::model_selection::ModelProfile,
    policy: &alondra_core::model_selection::RoleModelPolicy, expected_config_version: &str) -> Result<(), ChangeRefused>
{
    set_model_profile_authorized(domain, home, actor, provider, profile, policy, expected_config_version, || Ok(()))
}

#[allow(clippy::too_many_arguments)]
pub fn set_model_profile_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, provider: Provider, profile: alondra_core::model_selection::ModelProfile,
    policy: &alondra_core::model_selection::RoleModelPolicy, expected_config_version: &str, authorize: impl Fn() -> anyhow::Result<()>) -> Result<(), ChangeRefused>
{
    let _writing = crate::settings_file::WRITER.lock_or_recover("the settings writer");
    recover_settings(domain, home).map_err(|why| ChangeRefused::Store(Refused::Folder(why)))?;
    let previous = crate::domain_store::provider_settings::read_options(home).map_err(|why| ChangeRefused::Store(Refused::Folder(why)))?;
    let mut file = previous.as_deref().map(alondra_core::options::OptionsFile::parse).transpose().map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?.unwrap_or_else(alondra_core::options::OptionsFile::fresh);
    if alondra_core::model_selection::profiles_version(&file.model_profiles) != expected_config_version { return Err(ChangeRefused::Store(Refused::Stale)); }
    file.model_profiles.entry(provider).or_default().insert(profile, policy.clone());
    let target = alondra_core::options::with_model_profiles(previous.as_deref().unwrap_or_default(), &file.model_profiles).map_err(|why| ChangeRefused::Store(Refused::Invalid(why)))?;
    let command = Command::SetModelProfile { provider, profile, policy: policy.clone(), expected_config_version: expected_config_version.into() };
    apply_settings_authorized(domain, home, actor, command, previous, target, vec![], authorize)
}

#[allow(clippy::too_many_arguments)]
fn apply_settings_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, command: Command, previous: Option<String>, target: String, recorded: Vec<Command>, authorize: impl Fn() -> anyhow::Result<()>) -> Result<(), ChangeRefused> {
    let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: domain.read().state().data_epoch.clone(),
        command_id: format!("provider-settings-{}", crate::auth::random_hex(16)).into(), expected_revision: 0, command };
    crate::domain_store::provider_settings::apply_authorized(domain, home, actor, envelope, previous, target, recorded, authorize, |_| Ok(())).map_err(|why| match why {
        crate::domain_store::StoreError::Domain(why) => ChangeRefused::Domain(why),
        why => ChangeRefused::Store(Refused::Folder(why.to_string())),
    })
}

pub(crate) fn recover_settings(domain: &DomainRuntime, home: &Path) -> Result<(), String> {
    crate::domain_store::provider_settings::recover(domain, home).map_err(|why| format!("Could not reconcile the settings change: {why:#}"))
}

/// Checks the accounts when the daemon starts, then every ten minutes, and at once when Claude
/// answered or refused a sign-in while no account had a session.
pub async fn run(domain: Arc<DomainRuntime>, home: PathBuf) {
    loop {
        crate::native_probe::retry(|| {
            let (checking, at) = (domain.clone(), home.clone());
            async move {
                tokio::task::spawn_blocking(move || check_current(&checking, &at)).await.unwrap_or_else(|_| {
                    log!("the accounts check stopped");
                    crate::native_probe::Outcome::Finished
                })
            }
        }).await;
        tokio::select! {
            () = tokio::time::sleep(CHECK_EVERY) => {}
            () = domain.live.account_check.notified() => {}
        }
    }
}

/// Ends what older daemons left of a Codex device sign-in: Alondra no longer signs in, so an
/// unfinished one is recorded as failed, and a credential it was publishing, or its staging
/// folder, is removed. The provider's own session stays as it is.
pub fn end_abandoned_sign_ins(domain: &DomainRuntime) {
    let logins = domain.read().state().account_logins.clone();
    let codex = (!logins.is_empty()).then(|| Codex::of(domain)).flatten();
    for login in logins {
        if let Some(codex) = &codex { crate::codex_credentials::recover_pending(&codex.profile(domain.home(), &login.account), &login.attempt); }
        if !login.phase.active() { continue; }
        let ended = alondra_core::domain::AccountLogin { phase: alondra_core::domain::AccountLoginPhase::Failed, challenge: None,
            error: Some("Sign-in was interrupted; the previous session was preserved".into()), ..login };
        let read = domain.read();
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(),
            command_id: format!("account-login-{}", crate::auth::random_hex(16)).into(), expected_revision: read.state().revision, command: Command::RecordAccountLogin { login: ended } };
        drop(read);
        if let Err(why) = domain.execute_as_system(envelope) { log!("could not close an unfinished Codex sign-in: {why}"); }
    }
    let staging = domain.home().join("account-sign-ins");
    if !staging.symlink_metadata().is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) { return; }
    for entry in std::fs::read_dir(staging).into_iter().flatten().flatten() {
        if entry.file_name().to_str().is_some_and(|name| name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
            && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// What the user is told about moving a node to another account when its own reaches its limit.
pub(crate) fn move_risk(lang: Lang) -> &'static str {
    match lang {
        Lang::Es => "Anthropic no dice si cambiar de cuenta al llegar al límite está permitido.",
        Lang::En => "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed.",
    }
}

pub fn read(domain: &DomainRuntime, home: &Path, lang: Lang) -> Reply {
    let config = match config(home) {
        Ok(config) => config,
        Err(why) => return Reply { status: StatusCode::INTERNAL_SERVER_ERROR, body: json!({"error": "file", "message": why}) },
    };
    let seen = domain.read().state().accounts.clone();
    let user_home = Claude::user_home(domain);
    let accounts: Vec<_> = config
        .ordered_accounts()
        .into_iter()
        .map(|(key, account)| {
            let status = seen.iter().find(|s| s.provider == key.provider && s.id == account.id);
            let state = match status {
                Some(status) if account.enabled && status.state == AccountState::Disabled => AccountState::Unchecked,
                Some(status) => status.state,
                None if account.enabled => AccountState::Unchecked,
                None => AccountState::Disabled,
            };
            json!({
                "provider": key.provider, "id": account.id, "name": account.name, "enabled": account.enabled,
                "default": account.id.is_default(), "billing": account.billing, "state": state,
                "email": status.and_then(|s| s.email.clone()), "plan": status.and_then(|s| s.plan.clone()),
                "profile": alondra_core::accounts::shown_profile(home, &user_home, &key),
            })
        })
        .collect();
    let moving = json!({"on": config.distribution.move_when_exhausted, "risk": move_risk(lang)});
    let read = domain.read();
    Reply { status: StatusCode::OK, body: json!({"version": version(&config), "config": config, "accounts": accounts, "move_when_exhausted": moving, "supported_providers": Provider::ALL, "data_epoch": read.state().data_epoch, "revision": read.state().revision}) }
}

pub fn for_client(mut reply: Reply, provider_accounts: bool) -> Reply {
    if provider_accounts || reply.status != StatusCode::OK { return reply; }
    if let Some(config) = reply.body.get_mut("config").and_then(serde_json::Value::as_object_mut) {
        config.remove("connected");
        config.remove("codex");
        if let Some(distribution) = config.get_mut("distribution").and_then(serde_json::Value::as_object_mut) { distribution.remove("order"); }
    }
    if let Some(rows) = reply.body.get_mut("accounts").and_then(serde_json::Value::as_array_mut) {
        rows.retain(|row| row["provider"] == "claude");
    }
    if let Some(object) = reply.body.as_object_mut() { object.remove("supported_providers"); }
    reply
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    version: String,
    accounts: AccountsConfig,
}

/// Replaces the `[accounts]` section with the one a client sent, if it saw the current one. A new
/// account links the `~/.claude` of the HOME the nodes' Claude runs in.
pub fn write(domain: &DomainRuntime, home: &Path, body: &serde_json::Value, lang: Lang) -> Reply {
    write_authorized(domain, home, &Actor::User { user_id: "local-user".into() }, body, lang, || Ok(()))
}

pub(crate) fn write_authorized(domain: &DomainRuntime, home: &Path, actor: &Actor, body: &serde_json::Value, lang: Lang, authorize: impl Fn() -> anyhow::Result<()>) -> Reply {
    let change = match serde_json::from_value::<Change>(body.clone()) {
        Ok(change) => change,
        Err(e) => return Refused::Invalid(e.to_string()).reply(lang),
    };
    let original = match config(home) { Ok(config) => config, Err(why) => return Refused::Invalid(why).reply(lang) };
    if version(&original) != change.version { return Refused::Stale.reply(lang); }
    check(domain, home);
    let current = match config(home) { Ok(config) => config, Err(why) => return Refused::Invalid(why).reply(lang) };
    let mut expected_migration = original.clone(); if original.connected.is_none() { expected_migration.migrate_legacy(); }
    if current != original && current != expected_migration { return Refused::Stale.reply(lang); }
    if let Err(refused) = replace_configuration_authorized(domain, home, actor, &version(&current), &change.accounts, authorize) {
        return match refused {
            ChangeRefused::Store(refused) => refused.reply(lang),
            ChangeRefused::Domain(error) => Reply { status: StatusCode::CONFLICT, body: json!({"error": error.code, "message": error.message}) },
        };
    }
    check(domain, home);
    read(domain, home, lang)
}

/// The accounts the open tasks' executions run on.
fn working_keys(state: &alondra_core::domain::DomainState) -> Vec<AccountKey> {
    state.tasks.values().filter(|task| !task.over()).map(|task| task.account_key()).collect()
}

#[derive(Debug, PartialEq)]
pub enum Refused {
    Invalid(String),
    Stale,
    /// Open tasks still run on the account, by its name.
    InUse(String),
    Folder(String),
    SignOut(String),
}

impl Refused {
    pub fn message(self, lang: Lang) -> String {
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
#[cfg(any(test, feature = "test-hooks"))]
pub(crate) fn store(home: &Path, user_home: &Path, claude: Option<&Claude>, seen: &str, next: &AccountsConfig, working: &[AccountId]) -> Result<(), Refused> {
    let working: Vec<_> = working.iter().cloned().map(|account| AccountKey::new(Provider::Claude, account)).collect();
    store_provider(home, user_home, claude, None, seen, next, &working)
}

fn prepare_account_changes(home: &Path, user_home: &Path, claude: Option<&Claude>, codex: Option<&Codex>, current: &AccountsConfig, next: &AccountsConfig, working: &[AccountKey]) -> Result<Vec<AccountKey>, Refused> {
    let ids = |config: &AccountsConfig| -> Vec<AccountKey> { Provider::ALL.into_iter().flat_map(|provider| config.listed(provider).iter().filter(|account| !account.id.is_default()).map(move |account| AccountKey::new(provider, account.id.clone()))).collect() };
    let (before, after) = (ids(&current), ids(&next));
    let shared = claude_account::config_folder(claude.and_then(Claude::default_profile).as_deref(), user_home);
    for id in after.iter().filter(|id| !before.contains(id)) {
        match id.provider {
            Provider::Claude => { claude_account::prepare(home, &id.account, &shared).map_err(|e| Refused::Folder(e.to_string()))?; }
            Provider::Codex => { alondra_core::codex_account::prepare(home, id).map_err(Refused::Folder)?; }
        }
    }
    let removed: Vec<AccountKey> = before.iter().filter(|id| !after.contains(id)).cloned().collect();
    if let Some(busy) = removed.iter().find(|id| working.contains(*id)) {
        let name = current.find(busy.provider, busy.account.as_str()).map_or_else(|| busy.wire_id(), |account| account.name);
        return Err(Refused::InUse(name));
    }
    for id in &removed {
        match id.provider {
            Provider::Claude => sign_out(home, claude, &id.account)?,
            Provider::Codex => sign_out_codex(home, codex, id)?,
        }
    }
    Ok(removed)
}

fn cleanup_account_folders(home: &Path, removed: &[AccountKey]) {
    for id in removed {
        let deleted = match id.provider {
            Provider::Claude => claude_account::remove(home, &id.account),
            Provider::Codex => alondra_core::accounts::provider_folder(home, id).filter(|folder| folder.exists()).map_or(Ok(()), std::fs::remove_dir_all),
        };
        if let Err(why) = deleted { log!("Account {} is no longer registered, but its private folder needs cleanup: {why}", id.wire_id()); }
    }
}

#[cfg(any(test, feature = "test-hooks"))]
fn store_provider(home: &Path, user_home: &Path, claude: Option<&Claude>, codex: Option<&Codex>, seen: &str, next: &AccountsConfig, working: &[AccountKey]) -> Result<(), Refused> {
    let current = config(home).map_err(Refused::Invalid)?;
    if version(&current) != seen {
        return Err(Refused::Stale);
    }
    let mut next = next.clone();
    next.preserve_connected_from(&current);
    next.check().map_err(Refused::Invalid)?;
    if !next.any_enabled_globally() {
        return Err(Refused::Invalid("At least one account stays on".into()));
    }
    let removed = prepare_account_changes(home, user_home, claude, codex, &current, &next, working)?;
    let file = alondra_core::options::path_in(home);
    let original = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Refused::Folder(format!("{}: {e}", file.display()))),
    };
    let text = alondra_core::accounts::with_accounts(&original, &next).map_err(Refused::Invalid)?;
    crate::settings_file::replace(&file, text.as_bytes()).map_err(|e| Refused::Folder(format!("{}: {e}", file.display())))?;
    cleanup_account_folders(home, &removed);
    Ok(())
}

fn sign_out_codex(home: &Path, codex: Option<&Codex>, key: &AccountKey) -> Result<(), Refused> {
    let Some(folder) = alondra_core::accounts::provider_folder(home, key).filter(|folder| folder.is_dir()) else { return Ok(()) };
    if !folder.join("auth.json").is_file() { return Ok(()); }
    let codex = codex.ok_or_else(|| Refused::SignOut("Codex is not available on this Mac".into()))?;
    let output = codex.command(Some(&folder)).arg("logout").output().map_err(|why| Refused::SignOut(why.to_string()))?;
    if !output.status.success() { return Err(Refused::SignOut("Codex could not sign out the selected account".into())); }
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
