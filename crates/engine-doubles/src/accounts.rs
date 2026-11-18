//! The provider accounts of a test's Mac. Account folders are made and deleted in the test's own
//! folders, as the Mac makes them; no Codex runs, so a test scripts what each Codex profile's probe
//! answers and which threads it keeps; and Claude Code's trust lives in memory, one settings file
//! per profile, which a test fills or makes refuse to save. It keeps the contract the Mac's accounts
//! keep (`crate::contract::accounts`), which `crates/system` passes on the real files. A native
//! session's account identity is read from the file the test's host wrote, without the Mac's checks.
use alondra_core::accounts::{self, AccountId, AccountKey, Provider};
use alondra_core::claude_account::{SHARED, config_folder, profile};
use alondra_core::codex_account::ProbeReport;
use alondra_engine_ports::Registry;
use alondra_engine_ports::accounts::{Accept, Accounts as Port};
use alondra_core::Millis;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The settings file each profile keeps trust in: none for the user's own, else the account folder.
type Settings = BTreeMap<Option<PathBuf>, BTreeMap<PathBuf, bool>>;

#[derive(Default)]
pub struct Accounts {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    probes: BTreeMap<Option<PathBuf>, Result<ProbeReport, String>>,
    every_probe: Option<Result<ProbeReport, String>>,
    during_probe: Option<Box<dyn FnOnce() + Send>>,
    probed: Vec<Option<PathBuf>>,
    threads_lost: bool,
    trust: Settings,
    refuse_trust: bool,
}

/// Gives `registry` these accounts, both as the port and as themselves for the test to script.
pub fn wire(registry: &Registry) -> Arc<Accounts> {
    let accounts = Arc::new(Accounts::default());
    registry.provide::<dyn Port>(accounts.clone());
    registry.provide::<Accounts>(accounts.clone());
    accounts
}

/// The doubled accounts `registry` was given.
pub fn of(registry: &Registry) -> Arc<Accounts> {
    registry.port::<Accounts>().expect("the test's runtime has the doubled accounts")
}

impl Accounts {
    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// What a probe of the Codex profile `profile` (none: the native one) answers from now on.
    pub fn answer_codex(&self, profile: Option<&Path>, answer: Result<ProbeReport, String>) {
        self.state().probes.insert(profile.map(Path::to_path_buf), answer);
    }

    /// What a probe of any Codex profile the test scripted no answer for answers from now on.
    pub fn answer_every_codex(&self, answer: Result<ProbeReport, String>) {
        self.state().every_probe = Some(answer);
    }

    /// Runs `during` while the next Codex probe is under way, as a change made meanwhile.
    pub fn during_next_codex_probe(&self, during: impl FnOnce() + Send + 'static) {
        self.state().during_probe = Some(Box::new(during));
    }

    /// The Codex profiles probed so far, in order.
    pub fn codex_probes(&self) -> Vec<Option<PathBuf>> {
        self.state().probed.clone()
    }

    /// From now on Codex keeps none of its threads, as when their rollouts were deleted. Until
    /// then it keeps every thread it ran, as the repository's fake Codex does.
    pub fn lose_codex_threads(&self) {
        self.state().threads_lost = true;
    }

    /// Claude Code's settings for `profile` (none: the user's own) say whether each repository is trusted.
    pub fn set_trust(&self, profile: Option<&Path>, repositories: &[(&Path, bool)]) {
        let projects = repositories.iter().map(|(repository, accepted)| (repository.to_path_buf(), *accepted)).collect();
        self.state().trust.insert(profile.map(Path::to_path_buf), projects);
    }

    /// Whether Claude Code's settings for `profile` trust `repository`.
    pub fn trusts(&self, profile: Option<&Path>, repository: &Path) -> bool {
        self.state().trust.get(&profile.map(Path::to_path_buf)).and_then(|projects| projects.get(repository)).copied().unwrap_or(false)
    }

    /// From now on, writing trust fails, as when another process holds Claude Code's settings.
    pub fn refuse_trust_saves(&self, refuse: bool) {
        self.state().refuse_trust = refuse;
    }

    fn grant_in(state: &mut State, profile: Option<&Path>, repository: &Path) -> Result<(), String> {
        if state.refuse_trust {
            return Err("Claude Code's settings are locked by another process".into());
        }
        state.trust.entry(profile.map(Path::to_path_buf)).or_default().insert(repository.to_path_buf(), true);
        Ok(())
    }
}

/// What the repository's fake Codex (`agent-host/fake-codex.py`) answers when a test does not say
/// otherwise: a ChatGPT sign-in of `first@example.test` on the `fixture-workspace` workspace, with
/// usage left and one model, `fixture-model`.
pub fn codex_fixture() -> ProbeReport {
    let models = alondra_core::codex_account::parse_models(&serde_json::json!({"data": [{"id": "fixture-model", "model": "fixture-model", "displayName": "fixture-model",
        "isDefault": true, "hidden": false, "supportedReasoningEfforts": [{"reasoningEffort": "high"}], "defaultReasoningEffort": "high", "inputModalities": ["text"]}]}))
        .expect("the fixture's models read");
    ProbeReport {
        requires_auth: true, auth_method: Some("chatgpt".into()), account_type: Some("chatgpt".into()), workspace_id: Some("fixture-workspace".into()),
        signed_in: true, email: Some("first@example.test".into()), plan: Some("plus".into()), pay_per_use: false,
        rate_limits: Some(serde_json::json!({"accountId": "fixture-workspace", "ordinaryUsageAllowed": true, "rateLimits": {
            "primary": {"usedPercent": 10, "windowDurationMins": 300, "resetsAt": 4102444800_i64},
            "secondary": {"usedPercent": 20, "windowDurationMins": 10080, "resetsAt": 4102444800_i64}}, "rateLimitsByLimitId": null})),
        catalog: alondra_core::model_selection::AccountModelCatalog { account: AccountKey::native(Provider::Codex), revision: "fixture-revision".into(),
            native_default: Some("fixture-model".into()), models, measured_at: 0 },
        catalog_error: None,
    }
}

/// The repository Claude Code keeps trust under for `cwd`: its root, or a linked worktree's main
/// checkout.
fn repository_of(cwd: &Path) -> Option<(PathBuf, Option<PathBuf>)> {
    let cwd = cwd.canonicalize().ok()?;
    let repository = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(&cwd).to_path_buf();
    let main = std::fs::read_to_string(repository.join(".git")).ok().and_then(|link| {
        let admin = repository.join(link.trim().strip_prefix("gitdir:")?.trim()).canonicalize().ok()?;
        let common = admin.join(std::fs::read_to_string(admin.join("commondir")).ok()?.trim()).canonicalize().ok()?;
        (admin.parent()? == common.join("worktrees") && common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
    });
    Some((repository, main))
}

impl Port for Accounts {
    fn prepare_claude(&self, home: &Path, account: &AccountId, shared_from: &Path) -> std::io::Result<PathBuf> {
        use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
        let folder = accounts::folder(home, account).ok_or_else(|| std::io::Error::other("the default account has no folder of Alondra's"))?;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder)?;
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))?;
        for name in SHARED {
            let (target, link) = (shared_from.join(name), folder.join(name));
            if std::fs::symlink_metadata(&target).is_ok() && std::fs::symlink_metadata(&link).is_err() {
                std::os::unix::fs::symlink(&target, &link)?;
            }
        }
        Ok(folder)
    }

    fn remove_claude(&self, home: &Path, account: &AccountId) -> std::io::Result<()> {
        let folder = accounts::folder(home, account).ok_or_else(|| std::io::Error::other("the default account is the user's own and is never deleted"))?;
        match std::fs::remove_dir_all(&folder) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    fn prepare_codex(&self, home: &Path, key: &AccountKey) -> Result<Option<PathBuf>, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        if key.provider != Provider::Codex {
            return Err("The account is not a Codex account".into());
        }
        let Some(folder) = accounts::provider_folder(home, key) else { return Ok(None) };
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder).map_err(|error| error.to_string())?;
        let config = folder.join("config.toml");
        if !config.exists() {
            std::fs::write(&config, "cli_auth_credentials_store = \"file\"\n").map_err(|error| error.to_string())?;
        }
        Ok(Some(folder))
    }

    fn remove_codex(&self, home: &Path, key: &AccountKey) -> std::io::Result<()> {
        accounts::provider_folder(home, key).filter(|folder| folder.exists()).map_or(Ok(()), std::fs::remove_dir_all)
    }

    fn shares_sessions(&self, home: &Path, user_home: &Path, default_profile: Option<&Path>, one: &AccountId, other: &AccountId) -> bool {
        let projects = |id: &AccountId| config_folder(profile(home, id, default_profile).as_deref(), user_home).join("projects").canonicalize().ok();
        projects(one).is_some_and(|one| Some(one) == projects(other))
    }

    fn probe_codex(&self, command: Command, account: AccountKey, now: Millis, accept: Accept<'_>) -> Result<ProbeReport, String> {
        let profile = command.get_envs().find(|(key, _)| *key == "CODEX_HOME").and_then(|(_, value)| value).map(PathBuf::from);
        let mut state = self.state();
        state.probed.push(profile.clone());
        let answer = state.probes.get(&profile).or(state.every_probe.as_ref()).cloned().unwrap_or_else(|| Ok(codex_fixture()));
        let during = state.during_probe.take();
        drop(state);
        if let Some(during) = during {
            during();
        }
        let mut report = answer?;
        report.catalog.account = account;
        report.catalog.measured_at = now;
        if !accept(&report) {
            report.catalog.models.clear();
            report.catalog.revision.clear();
            report.catalog.native_default = None;
            report.rate_limits = None;
        }
        Ok(report)
    }

    fn retained_codex_thread(&self, _: &Path, _: &BTreeMap<String, String>, _: &str, _: &Path) -> Result<bool, String> {
        Ok(!self.state().threads_lost)
    }

    fn recover_codex_sign_in(&self, profile: &Path, attempt: &str) {
        let _ = std::fs::remove_file(profile.join(format!(".alondra-auth-{attempt}")));
    }

    fn clear_sign_in_staging(&self, staging: &Path) {
        let _ = std::fs::remove_dir_all(staging);
    }

    fn native_session_identity(&self, directory: &Path) -> Result<Vec<u8>, String> {
        std::fs::read(directory.join("native-auth-account.json"))
            .map_err(|_| "This native session has no verified account identity; keep it paused and create a new node after signing in".to_owned())
    }

    fn untrusted(&self, profile: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
        let (repository, main) = repository_of(cwd)?;
        let state = self.state();
        let projects = state.trust.get(&profile.map(Path::to_path_buf))?;
        let accepted = |dir: &Path| projects.get(dir).copied().unwrap_or(false);
        let trusted = accepted(&repository) || main.as_deref().is_some_and(accepted);
        (!trusted).then(|| main.unwrap_or(repository))
    }

    fn ensure_trusted(&self, profile: Option<&Path>, cwd: &Path, projects_root: Option<&Path>) -> Option<PathBuf> {
        let repository = self.untrusted(profile, cwd)?;
        let home = alondra_core::config::home_dir(&|key: &str| std::env::var(key).ok());
        let home = home.canonicalize().unwrap_or(home);
        let inside = projects_root.and_then(|root| root.canonicalize().ok()).is_some_and(|root| !home.starts_with(&root) && repository.starts_with(&root));
        if inside && Self::grant_in(&mut self.state(), profile, &repository).is_ok() { None } else { Some(repository) }
    }

    fn grant_trust(&self, repository: &Path, profiles: &[PathBuf]) -> Result<(), String> {
        let mut state = self.state();
        Self::grant_in(&mut state, None, repository)?;
        for profile in profiles.iter().filter(|profile| profile.is_dir()) {
            Self::grant_in(&mut state, Some(profile), repository)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_doubled_accounts_keep_the_contract_the_macs_keep() {
        let accounts = Accounts::default();
        crate::contract::accounts(&accounts, &|profile, repositories| accounts.set_trust(Some(profile), repositories));
    }

    #[test]
    fn a_codex_probe_answers_what_the_test_scripted_for_its_profile_and_is_recorded() {
        let accounts = Accounts::default();
        let profile = Path::new("/synthetic/codex-work");
        accounts.answer_codex(Some(profile), Err("signed out".into()));
        let mut command = Command::new("/synthetic/codex");
        command.env("CODEX_HOME", profile);
        let key = AccountKey::native(Provider::Codex);
        assert_eq!(accounts.probe_codex(command, key.clone(), 1, Box::new(|_| true)), Err("signed out".into()));
        let native = accounts.probe_codex(Command::new("/synthetic/codex"), key, 2, Box::new(|_| false)).unwrap();
        assert!(native.signed_in && native.catalog.models.is_empty() && native.rate_limits.is_none(), "a refused account gets neither models nor usage");
        assert_eq!(accounts.codex_probes(), [Some(profile.to_path_buf()), None]);
    }
}
