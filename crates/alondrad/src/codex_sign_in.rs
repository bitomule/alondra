use crate::locks::Poisonable as _;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use alondra_core::accounts::{AccountKey, Provider};
use alondra_core::codex_login::{DeviceLogin, Staging};
use alondra_core::domain::{AccountLogin, AccountLoginPhase, Actor, Command, CommandEnvelope, DomainError, PROTOCOL_VERSION};
use crate::domain_runtime::DomainRuntime;

pub(crate) type Authorization = Arc<dyn Fn() -> bool + Send + Sync>;

pub(crate) struct SignIns {
    domain: Arc<DomainRuntime>,
    // The gate covers admission, cancellation and publication, never the user's browser ceremony.
    workers: Mutex<BTreeMap<AccountKey, Arc<AtomicBool>>>,
    #[cfg(test)]
    settled: std::sync::Condvar,
    #[cfg(test)]
    threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
}

fn envelope(domain: &DomainRuntime, command: Command) -> CommandEnvelope {
    let read = domain.read();
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: read.state().data_epoch.clone(),
        command_id: format!("account-login-{}", crate::auth::random_hex(16)).into(), expected_revision: read.state().revision, command }
}

impl SignIns {
    pub(crate) fn new(domain: Arc<DomainRuntime>) -> Arc<Self> {
        let manager = Arc::new(Self { domain, workers: Mutex::new(BTreeMap::new()), #[cfg(test)] settled: Default::default(), #[cfg(test)] threads: Default::default() });
        let abandoned = manager.domain.read().state().account_logins.iter().filter(|login| matches!(login.phase, AccountLoginPhase::Starting | AccountLoginPhase::Pending)).cloned().collect::<Vec<_>>();
        for login in abandoned { let _ = manager.record(login, AccountLoginPhase::Failed, None, Some("Sign-in was interrupted; the previous session was preserved".into())); }
        manager.recover_staging();
        manager
    }

    pub(crate) fn recover_after_backend(self: &Arc<Self>) {
        let abandoned = self.domain.read().state().account_logins.clone();
        for login in abandoned {
            let mut workers = self.workers.lock_or_recover("Codex sign-ins");
            workers.insert(login.account.clone(), Arc::new(AtomicBool::new(false)));
            let manager = self.clone();
            let account = login.account.clone();
            match std::thread::Builder::new().name("codex-sign-in-recovery".into()).spawn(move || {
                if let Some(native) = crate::accounts::Codex::of(&manager.domain) {
                    crate::codex_credentials::recover_pending(&native.profile(manager.domain.home(), &login.account), &login.attempt);
                }
                while login.phase == AccountLoginPhase::Publishing && manager.active(&login) {
                    let status = crate::accounts::Codex::of(&manager.domain).and_then(|native| {
                        let profile = alondra_core::accounts::provider_folder(manager.domain.home(), &login.account);
                        alondra_core::codex_login::authentication(native.command(profile.as_deref())).ok()
                    });
                    if let Some(status) = status {
                        manager.finish(&login, if status.signed_in { AccountLoginPhase::Connected } else { AccountLoginPhase::Failed },
                            (!status.signed_in).then(|| "Codex could not confirm a completed sign-in".into()));
                        break;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                }
                manager.workers.lock_or_recover("Codex sign-ins").remove(&login.account);
                #[cfg(test)] manager.settled.notify_all();
            }) {
                Ok(thread) => { #[cfg(test)] self.threads.lock().unwrap().push(thread); #[cfg(not(test))] let _ = thread; }
                Err(_) => { workers.remove(&account); }
            }
        }
    }

    fn recover_staging(&self) {
        let root = self.domain.home().join("account-sign-ins");
        if !root.symlink_metadata().is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) { return; }
        if let Ok(entries) = std::fs::read_dir(root) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                if name.to_str().is_some_and(|name| name.len() == 32 && name.bytes().all(|byte| byte.is_ascii_hexdigit()))
                    && entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }

    pub(crate) fn start(self: &Arc<Self>, actor: &Actor, account: AccountKey, authorized: Authorization, allow_login: bool) -> Result<AccountLogin, DomainError> {
        let mut workers = self.workers.lock_or_recover("Codex sign-ins");
        if workers.contains_key(&account) { return Err(DomainError::new("account_login_in_progress", "A sign-in is already open for this account")); }
        let attempt = crate::auth::random_hex(16);
        let command = Command::BeginAccountLogin { account: account.clone(), attempt: attempt.clone(), started_at: alondra_core::now_millis() };
        self.domain.execute_with(actor, &envelope(&self.domain, command), None, None, || if authorized() { Ok(()) } else { Err(DomainError::new("forbidden", "Device approval was revoked").into()) }).map_err(|why| DomainError::new("account_login_refused", why.to_string()))?;
        let login = self.current(&account, &attempt).ok_or_else(|| DomainError::new("account_login_ended", "This sign-in is no longer open"))?;
        let cancelled = Arc::new(AtomicBool::new(false));
        workers.insert(account.clone(), cancelled.clone());
        let manager = self.clone();
        let worker_login = login.clone();
        let actor = actor.clone();
        match std::thread::Builder::new().name("codex-sign-in".into()).spawn(move || {
            let result = manager.authenticate(&actor, &worker_login, cancelled, authorized, allow_login);
            if let Err(error) = result { manager.finish(&worker_login, AccountLoginPhase::Failed, Some(error)); }
            manager.workers.lock_or_recover("Codex sign-ins").remove(&worker_login.account);
            #[cfg(test)] manager.settled.notify_all();
        }) {
            Ok(thread) => {
                #[cfg(test)] self.threads.lock().unwrap().push(thread);
                #[cfg(not(test))] let _ = thread;
                Ok(login)
            },
            Err(_) => {
                workers.remove(&account);
                let _ = self.record(login, AccountLoginPhase::Failed, None, Some("Could not start Codex sign-in".into()));
                Err(DomainError::new("unavailable", "Could not start Codex sign-in"))
            }
        }
    }

    pub(crate) fn cancel(&self, actor: &Actor, account: AccountKey, attempt: String) -> Result<(), DomainError> {
        let workers = self.workers.lock_or_recover("Codex sign-ins");
        self.domain.execute_with(actor, &envelope(&self.domain, Command::CancelAccountLogin { account: account.clone(), attempt }), None, None, || Ok(()))
            .map_err(|why| DomainError::new("account_login_ended", why.to_string()))?;
        if let Some(cancelled) = workers.get(&account) { cancelled.store(true, Ordering::Release); }
        Ok(())
    }

    fn current(&self, account: &AccountKey, attempt: &str) -> Option<AccountLogin> {
        self.domain.read().state().account_logins.iter().find(|login| login.account == *account && login.attempt == attempt).cloned()
    }

    fn active(&self, login: &AccountLogin) -> bool { self.current(&login.account, &login.attempt).is_some_and(|current| current.phase.active()) }

    fn record(&self, mut login: AccountLogin, phase: AccountLoginPhase, challenge: Option<alondra_core::codex_login::Challenge>, error: Option<String>) -> Result<(), String> {
        login.phase = phase; login.challenge = challenge; login.error = error;
        match self.domain.execute_as_system(envelope(&self.domain, Command::RecordAccountLogin { login: login.clone() })) {
            Ok(_) => Ok(()),
            Err(crate::domain_store::StoreError::Domain(error)) if error.code == "account_login_ended"
                && self.current(&login.account, &login.attempt).as_ref() == Some(&login) => Ok(()),
            Err(_) => Err("Could not save the Codex sign-in state".into()),
        }
    }

    fn finish(&self, login: &AccountLogin, phase: AccountLoginPhase, error: Option<String>) {
        let mut reported = false;
        while self.active(login) {
            if self.record(login.clone(), phase, None, error.clone()).is_ok() { return; }
            if !reported { log!("Waiting to save the Codex sign-in result"); reported = true; }
            std::thread::sleep(Duration::from_secs(1));
        }
    }

    fn connect(&self, actor: &Actor, authorized: &dyn Fn() -> bool) -> Result<(), String> {
        if !authorized() { return Err("Device approval was revoked; the previous session was preserved".into()); }
        let mut current = crate::accounts::config(self.domain.home())?;
        if current.is_connected(Provider::Codex) { return Ok(()); }
        if self.domain.read().state().account_config_version != alondra_core::accounts::config_version(&current) {
            crate::accounts::check(&self.domain, self.domain.home());
            current = crate::accounts::config(self.domain.home())?;
            if current.is_connected(Provider::Codex) { return Ok(()); }
        }
        let mut next = current.clone(); next.connect(Provider::Codex);
        let order = next.ordered_accounts().into_iter().map(|(key, _)| key).collect();
        next.set_order(order)?;
        crate::accounts::replace_configuration_authorized(&self.domain, self.domain.home(), actor, &alondra_core::accounts::config_version(&current), &next, || if authorized() { Ok(()) } else { Err(anyhow::anyhow!("Device approval was revoked")) })
            .map_err(|_| "The account configuration changed; connect Codex again".into())
    }

    fn authenticate(&self, actor: &Actor, login: &AccountLogin, cancelled: Arc<AtomicBool>, authorized: Authorization, allow_login: bool) -> Result<(), String> {
        let native = crate::accounts::Codex::of(&self.domain).ok_or("Codex is not installed for this service")?;
        let managed = alondra_core::codex_account::prepare(self.domain.home(), &login.account)?;
        let authentication = alondra_core::codex_login::authentication_with_cancel(native.command(managed.as_deref()), Some(cancelled.clone()))?;
        if !self.active(login) { return Ok(()); }
        self.connect(actor, &*authorized)?;
        if authentication.signed_in {
            crate::accounts::check(&self.domain, self.domain.home());
            self.record(login.clone(), AccountLoginPhase::Connected, None, None)?;
            return Ok(());
        }
        if !allow_login { return Err("Codex needs sign-in; run this command without --no-input".into()); }
        let destination = crate::codex_credentials::Destination::inspect(native.profile(self.domain.home(), &login.account), &authentication.storage, &crate::codex_credentials::NativeKeychain)?;
        let again = alondra_core::codex_login::authentication_with_cancel(native.command(managed.as_deref()), Some(cancelled.clone()))?;
        if again.signed_in {
            crate::accounts::check(&self.domain, self.domain.home());
            self.record(login.clone(), AccountLoginPhase::Connected, None, None)?;
            return Ok(());
        }
        if again.storage != authentication.storage || again.restrictions != authentication.restrictions {
            return Err("The native Codex configuration changed; the previous session was preserved".into());
        }
        let root = self.domain.home().join("account-sign-ins");
        private_root(&root)?;
        let staging = Staging::create(&root.join(&login.attempt))?;
        let mut command = native.command(Some(staging.path()));
        for (name, value) in &authentication.restrictions { command.args(["-c", &format!("{name}={value}")]); }
        let mut device = DeviceLogin::start_with_cancel(command, Some(cancelled.clone()))?;
        self.record(login.clone(), AccountLoginPhase::Pending, Some(device.challenge.clone()), None)?;
        loop {
            if !self.active(login) { device.cancel(); return Ok(()); }
            if !authorized() { device.cancel(); return Err("Device approval was revoked; the previous session was preserved".into()); }
            if device.poll(Duration::from_millis(100))?.is_some() { break; }
        }
        let verified = alondra_core::codex_login::authentication_with_cancel(native.command(Some(staging.path())), Some(cancelled.clone()))?;
        if !verified.signed_in { return Err("Codex did not confirm the new session; the previous session was preserved".into()); }
        let current = alondra_core::codex_login::authentication_with_cancel(native.command(managed.as_deref()), Some(cancelled))?;
        if current.signed_in {
            crate::accounts::check(&self.domain, self.domain.home());
            self.record(login.clone(), AccountLoginPhase::Connected, None, None)?;
            return Ok(());
        }
        if current.storage != authentication.storage || current.restrictions != authentication.restrictions {
            return Err("The native Codex configuration changed; the previous session was preserved".into());
        }
        let bytes = staging.credential()?;
        {
            let _workers = self.workers.lock_or_recover("Codex sign-ins");
            if !self.active(login) { device.cancel(); return Ok(()); }
            let configured = crate::accounts::config(self.domain.home())?;
            if !configured.accounts(Provider::Codex).iter().any(|account| account.id == login.account.account && account.enabled) {
                return Err("The Codex account changed during sign-in; the previous session was preserved".into());
            }
            if !authorized() { return Err("Device approval was revoked; the previous session was preserved".into()); }
            self.record(login.clone(), AccountLoginPhase::Publishing, None, None)?;
            destination.publish(&bytes, &login.attempt, &crate::codex_credentials::NativeKeychain)?;
        }
        self.finish(login, AccountLoginPhase::Connected, None);
        crate::accounts::check(&self.domain, self.domain.home());
        Ok(())
    }
}

fn private_root(root: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt as _;
    match std::fs::DirBuilder::new().mode(0o700).create(root) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && root.symlink_metadata().is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) => Ok(()),
        Err(_) => Err("Could not create the private Codex sign-in folder".into()),
    }
}

#[cfg(test)]
#[path = "codex_sign_in_tests.rs"]
mod tests;
