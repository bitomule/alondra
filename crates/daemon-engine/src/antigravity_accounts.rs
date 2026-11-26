//! Antigravity accounts as the daemon checks them. Each check starts the account's `agy` (never a
//! prompt): when the account is added, when its key or access changes, when the user asks for it,
//! and once a day; in between, what the last check saw stands, so no `agy` starts every round.
//! A Google or Enterprise account's quota is read with `agy -p /usage` right after its check. A
//! key goes from the client's action to the account's `0600` file and, from there, only to that
//! account's `agy`: the domain records that the credential changed, never the key.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use alondra_core::accounts::{AccountKey, AccountState, AccountStatus, AccountsConfig, Provider};
use alondra_core::antigravity_account::{self as antigravity, Check, CheckReport, Probe, Secret, Settings, Unavailable, UsageOutcome};
use alondra_core::model_selection::AccountModelCatalog;
use alondra_domain_model::Command;
use alondra_engine_ports::accounts::Accounts;
use sha2::{Digest, Sha256};

use alondra_engine_ports::Runtime;
use alondra_engine_kernel::sdk_backend::SdkOf as _;

const DAY: alondra_core::Millis = 24 * 60 * 60 * 1000;
/// How long a check that found no sign-in, or could not tell, stands before the next one.
const HOUR: alondra_core::Millis = 60 * 60 * 1000;

/// How the daemon starts `agy`: found on the `PATH` its nodes get, in the HOME they run in.
pub(crate) struct Antigravity {
    accounts: Arc<dyn Accounts>,
    executable: Option<PathBuf>,
    environment: BTreeMap<String, String>,
    pub(crate) user_home: PathBuf,
}

impl Antigravity {
    pub(crate) fn of(domain: &dyn Runtime) -> Option<Self> {
        let accounts = domain.ports().port::<dyn Accounts>()?;
        let (backend, _) = domain.sdk()?;
        let environment = backend.native_environment.clone();
        let path = environment.get("PATH").cloned().unwrap_or_default();
        let user_home = environment.get("HOME").map(PathBuf::from).unwrap_or_else(|| alondra_engine_kernel::accounts_file::claude_user_home(domain));
        Some(Self { accounts, executable: alondra_host::user::executable(&path, "agy"), environment, user_home })
    }

    /// Which `agy` checks run with, and when it was installed or updated: a new one is checked again.
    fn installed(&self) -> String {
        let changed = self.executable.as_ref().and_then(|executable| std::fs::metadata(executable).and_then(|meta| meta.modified()).ok())
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |at| at.as_secs());
        format!("{}@{changed}", self.executable.as_ref().map_or_else(String::new, |executable| executable.display().to_string()))
    }

    /// The user's own `agy` configuration, which every named account's folder links.
    pub(crate) fn shared(user_home: &Path) -> PathBuf {
        user_home.join(".gemini")
    }

    fn probe(&self, home: &Path, key: &AccountKey, settings: &Settings, scratch: &Path, acp: bool) -> Result<Option<Probe>, String> {
        let Some(executable) = self.executable.clone() else { return Ok(None) };
        let folder = alondra_core::accounts::provider_folder(home, key);
        let secret = if settings.access.takes_key() { self.accounts.antigravity_key(home, key)? } else { None };
        let mut launch = antigravity::launch(&self.environment, &self.user_home, settings, folder.as_deref(), secret.as_ref().map(Secret::expose), acp);
        launch.environment.insert("TMPDIR".into(), scratch.join("tmp").display().to_string());
        Ok(Some(Probe { executable, launch, directory: scratch.join("work"), access: settings.access }))
    }

    /// Checks the account, with its models when it is signed in.
    pub(crate) fn check(&self, home: &Path, key: &AccountKey, settings: &Settings) -> CheckReport {
        let unavailable = |why| CheckReport { version: None, check: Check::Unavailable(why) };
        let Ok(scratch) = Scratch::new(home) else { return unavailable(Unavailable::Failed) };
        match self.probe(home, key, settings, &scratch.0, true) {
            Ok(Some(probe)) => self.accounts.check_antigravity(&probe),
            Ok(None) => unavailable(Unavailable::NotInstalled),
            Err(why) => { log!("Antigravity account {} could not read its key: {why}", key.account); unavailable(Unavailable::Failed) }
        }
    }

    /// Reads the account's quota.
    pub(crate) fn usage(&self, home: &Path, key: &AccountKey, settings: &Settings) -> Result<UsageOutcome, String> {
        let scratch = Scratch::new(home)?;
        let probe = self.probe(home, key, settings, &scratch.0, false)?.ok_or("Antigravity CLI is not installed")?;
        self.accounts.antigravity_usage(&probe)
    }
}

/// An empty folder of Alondra's an `agy` probe runs and unpacks its runtime in, removed after it.
struct Scratch(PathBuf);

impl Scratch {
    fn new(home: &Path) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        let root = home.join(alondra_core::accounts::MANAGED_PROVIDER_FOLDER).join("antigravity-probes");
        let folder = root.join(alondra_foundation::random_hex(8));
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder.join("tmp")).map_err(|error| error.to_string())?;
        std::fs::DirBuilder::new().mode(0o700).create(folder.join("work")).map_err(|error| error.to_string())?;
        Ok(Self(folder))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What each account was last checked with and when, by the daemon home it belongs to.
static CHECKED: Mutex<BTreeMap<(PathBuf, AccountKey), (String, alondra_core::Millis)>> = Mutex::new(BTreeMap::new());

/// The next round checks every Antigravity account of `home` again, as when the user asks.
pub fn forget_checks(home: &Path) {
    CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|(of, _), _| of != home);
}

pub(crate) fn forget(home: &Path, key: &AccountKey) {
    CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(home.to_path_buf(), key.clone()));
}

/// The accounts a round checked, each with what it saw, so the next round skips them only once the
/// round that checked them is recorded.
pub(crate) type Checked = Vec<(AccountKey, String, alondra_core::Millis)>;

/// Keeps the checks of a round the domain recorded, so the next round runs no `agy` for them.
pub(crate) fn remember(home: &Path, checked: Checked) {
    let mut cache = CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for (key, fingerprint, at) in checked { cache.insert((home.to_path_buf(), key), (fingerprint, at)); }
}

/// The Antigravity accounts as this round sees them, what their checks found to record after
/// them (catalogs and usage readings), and the checks to `remember` once they are recorded. A
/// round that may not `probe` (the one after startup, which every launch waits for, while no node
/// runs on Antigravity) runs no `agy` and keeps what each account stood at.
pub(crate) fn observe(domain: &dyn Runtime, home: &Path, config: &AccountsConfig, known: &[AccountStatus], now: alondra_core::Millis, probe: bool) -> (Vec<AccountStatus>, Vec<Command>, Checked) {
    let accounts = config.accounts(Provider::Antigravity);
    if accounts.is_empty() { return (vec![], vec![], vec![]); }
    let agy = Antigravity::of(domain);
    let port = domain.ports().port::<dyn Accounts>();
    let (mut statuses, mut found, mut checked_now) = (vec![], vec![], vec![]);
    for account in accounts {
        let key = AccountKey::new(Provider::Antigravity, account.id.clone());
        let settings = config.antigravity_settings(&account.id);
        let key_set = port.as_ref().is_some_and(|port| port.antigravity_key_set(home, &key));
        let prior = known.iter().find(|status| status.provider == key.provider && status.id == key.account);
        let mut status = AccountStatus { provider: key.provider, id: key.account.clone(), name: account.name.clone(), state: AccountState::Disabled,
            native_identity: None, email: None, plan: prior.and_then(|prior| prior.plan.clone()), billing: account.billing.clone(), sign_ins: 0, profile: None,
            antigravity: Some(alondra_core::accounts::AntigravityStatus::of(&settings, key_set)) };
        if !account.enabled { statuses.push(status); continue; }
        let installed = agy.as_ref().map(Antigravity::installed).unwrap_or_default();
        let fingerprint = format!("{}|{key_set}|{installed}", serde_json::to_string(&settings).unwrap_or_default());
        let checked = CHECKED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&(home.to_path_buf(), key.clone())).cloned();
        let stands = |state: AccountState| if state == AccountState::SignedIn { DAY } else { HOUR };
        let fresh = checked.is_some_and(|(seen, at)| seen == fingerprint && prior.is_some_and(|prior| now - at < stands(prior.state)))
            && prior.is_some_and(|prior| !matches!(prior.state, AccountState::Unchecked | AccountState::Disabled));
        if fresh || !probe {
            match prior.filter(|prior| prior.state != AccountState::Disabled) {
                Some(prior) => {
                    status.state = prior.state;
                    if let (Some(now), Some(before)) = (status.antigravity.as_mut(), prior.antigravity.as_ref()) { now.unavailable = before.unavailable; }
                }
                None => status.state = AccountState::Unchecked,
            }
            statuses.push(status);
            continue;
        }
        let folder = alondra_core::accounts::provider_folder(home, &key);
        let report = match &agy {
            _ if folder.as_ref().is_some_and(|folder| !folder.is_dir()) => CheckReport { version: None, check: Check::SignedOut },
            _ if settings.access == alondra_core::antigravity_account::Access::ApiKey && !key_set => CheckReport { version: None, check: Check::SignedOut },
            Some(agy) => agy.check(home, &key, &settings),
            None => CheckReport { version: None, check: Check::Unavailable(Unavailable::NotInstalled) },
        };
        checked_now.push((key.clone(), fingerprint, now));
        match report.check {
            Check::SignedIn { models, native_default } => {
                status.state = AccountState::SignedIn;
                if let Some(catalog) = catalog(&key, models, native_default, now) { found.push(Command::RecordModelCatalog { catalog }); }
                if settings.access.reads_usage() && let Some(agy) = &agy {
                    match agy.usage(home, &key, &settings) {
                        Ok(UsageOutcome::Read(reading)) if !reading.windows.is_empty() => found.push(Command::RecordAccountUsage {
                            provider: key.provider, account: key.account.clone(), reading, at: now, day: alondra_daemon_engine_model::local_day(now) }),
                        Ok(UsageOutcome::Read(_)) => {}
                        Ok(UsageOutcome::Refused) => log!("Antigravity account {} refused its usage reading; it is read again with its next check", key.account),
                        Err(why) => log!("Antigravity account {} usage could not be read: {why}", key.account),
                    }
                }
            }
            Check::SignedOut => status.state = AccountState::SignedOut,
            Check::Unavailable(why) => {
                status.state = AccountState::Unavailable;
                if let Some(antigravity) = status.antigravity.as_mut() { antigravity.unavailable = Some(why); }
                log!("Antigravity account {} could not be checked: {why:?} (agy {})", key.account, report.version.map_or_else(|| "unknown".to_owned(), |version| version.to_string()));
            }
        }
        statuses.push(status);
    }
    (statuses, found, checked_now)
}

fn catalog(key: &AccountKey, models: Vec<alondra_core::model_selection::CatalogModel>, native_default: Option<String>, now: alondra_core::Millis) -> Option<AccountModelCatalog> {
    if models.is_empty() { return None; }
    let revision: String = Sha256::digest(serde_json::to_vec(&models).ok()?).iter().map(|byte| format!("{byte:02x}")).collect();
    Some(AccountModelCatalog { account: key.clone(), revision, models, native_default, measured_at: now })
}

/// The models of the Antigravity account `key`, read now.
pub(crate) fn read_models(domain: &dyn Runtime, home: &Path, config: &AccountsConfig, key: &AccountKey) -> Result<AccountModelCatalog, String> {
    forget(home, key);
    let agy = Antigravity::of(domain).ok_or("Antigravity CLI is not available on this Mac")?;
    let now = alondra_core::now_millis();
    match agy.check(home, key, &config.antigravity_settings(&key.account)).check {
        Check::SignedIn { models, native_default } => catalog(key, models, native_default, now).ok_or_else(|| "This account did not advertise any models".into()),
        Check::SignedOut => Err("Sign in to this Antigravity account before refreshing its models".into()),
        Check::Unavailable(why) => Err(format!("Antigravity CLI could not be checked: {why:?}")),
    }
}
