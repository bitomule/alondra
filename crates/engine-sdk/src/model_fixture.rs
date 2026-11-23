//! Launches through the SDK with no process behind them: the domain's hosts are the doubles of
//! `engine-doubles`, and the Claude host model of `provider-claude` plays each one and every probe.
//! It reads what each native Claude was started with and the prompts it took as the fake native
//! Claude records them, so a test reads the same thing whichever hosts it runs on.
use crate::domain_runtime::DomainRuntime;
use crate::sdk_owner::SdkOwners as _;
use alondra_claude_host_model::{ClaudeModel, NativeStart, Turns};
use alondra_daemon_engine_model::{LaunchOptions, LaunchOptionsFile as _};
use alondra_domain_rules::InteractionId;
use alondra_engine_doubles::agent_host::InProcessHosts;
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// A runtime of this crate's own pieces with doubles of the rest of the engine: the Mac, the agent
/// hosts, a gate that holds nothing back, what the gate and the orchestrator ask of the engine, and
/// the part of `SdkEngine` a launch asks (`alondra_engine_doubles::sdk`).
#[cfg(not(feature = "whole-engine"))]
pub fn open(home: &Path) -> anyhow::Result<DomainRuntime> {
    use alondra_engine_ports::admission::AdmissionGate;
    use alondra_engine_ports::orchestrator::OrchestratorEngine;
    let files = *alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite);
    let observers: Vec<Box<dyn crate::domain_runtime::CommitObserver>> = vec![Box::new(crate::task_closer::Unattended), Box::new(crate::sdk_owner::Retakes)];
    let runtime = DomainRuntime::open_observed(home, &alondra_engine_ports::journal::active_path(home), files, None, observers)?;
    let ports = runtime.ports();
    crate::wiring::wire(ports);
    alondra_engine_doubles::machine::wire(ports, alondra_daemon_support::watcher::claude_home);
    alondra_engine_doubles::agent_host::wire(ports);
    ports.provide::<dyn AdmissionGate>(Arc::new(alondra_engine_doubles::gate::OpenGate::default()));
    alondra_engine_doubles::admission::Engine::wire(ports);
    ports.provide::<dyn OrchestratorEngine>(Arc::new(alondra_engine_doubles::orchestrator::Engine));
    alondra_engine_doubles::sdk::Engine::wire(ports);
    Ok(runtime)
}

/// A domain whose launches start doubled hosts the model plays. Dropping it stops the domain's
/// owners and ends every host left.
pub struct SdkLaunches {
    domain: Arc<DomainRuntime>,
    pub model: ClaudeModel,
    pub hosts: Arc<InProcessHosts>,
    state_root: PathBuf,
    _state: crate::test_support::Directory,
}

impl SdkLaunches {
    /// Each native Claude answers every prompt with a result.
    pub fn enable(domain: &Arc<DomainRuntime>, home: &Path) -> Self {
        Self::with(domain, home, Turns::Answered)
    }

    /// Each native Claude keeps every prompt and never begins a turn, so each node holds its slot
    /// until its task ends.
    pub fn enable_holding(domain: &Arc<DomainRuntime>, home: &Path) -> Self {
        Self::with(domain, home, Turns::Ignored)
    }

    fn with(domain: &Arc<DomainRuntime>, home: &Path, turns: Turns) -> Self {
        let hosts = alondra_engine_doubles::agent_host::wire(domain.ports());
        let model = ClaudeModel::new(home, Arc::default());
        model.set_turns(turns);
        hosts.set_serve(model.clone());
        let probe = model.clone();
        hosts.set_probe(move |run| probe.probe(run));
        let state = crate::test_support::Directory::for_unix_socket();
        let backend = model_backend(home, &state);
        let state_root = backend.state_root.clone();
        domain.enable_sdk(backend);
        Self { domain: Arc::clone(domain), model, hosts, state_root, _state: state }
    }

    fn last(&self) -> NativeStart {
        self.model.starts().pop().expect("a native Claude started")
    }

    /// The arguments the last native Claude was started with.
    pub fn args(&self) -> Vec<String> {
        self.last().arguments
    }

    /// The value the last native Claude got for `flag`.
    pub fn flag(&self, flag: &str) -> Option<String> {
        self.last().flag(flag)
    }

    /// The environment the last native Claude got, as the fake one records it: the names, and the
    /// digest of each value.
    pub fn capture(&self) -> Value {
        capture_of(&self.last())
    }

    /// Every prompt the native Claudes took, in order, besides the wake Alondra sends after a turn
    /// that ended empty.
    pub fn prompts(&self) -> Vec<String> {
        self.model.prompts().into_iter().filter(|prompt| !prompt.starts_with(alondra_domain_rules::WAKE_INSTRUCTION)).collect()
    }

    /// Each native Claude the hosts started: its session, the one it resumed and its folder.
    pub fn starts(&self) -> Vec<Value> {
        self.model.starts().iter().map(NativeStart::record).collect()
    }

    /// What the launch of `agent`'s execution kept beside its host.
    pub fn options(&self, agent: &str) -> LaunchOptions {
        LaunchOptions::load(&self.state_root.join(agent)).unwrap()
    }

    /// Waits on the domain until `task`'s turns have ended on `turns` cards it rested on.
    pub async fn until_turns_ended(&self, task: &str, turns: usize) {
        let mut changed = self.domain.subscribe();
        let ended = |domain: &DomainRuntime| domain.read().state().interactions.keys()
            .filter(|card| rests_on(task, card)).count() >= turns;
        while !ended(&self.domain) {
            changed.changed().await.unwrap();
        }
    }

    /// Waits on the domain until `task`'s first turn ended with Claude's answer.
    pub async fn until_answered(&self, task: &str) {
        self.until_turns_ended(task, 1).await;
    }

    /// While `refused`, no new host gets the folder it needs, so every start fails before the
    /// domain registers a node; hosts that run already go on.
    pub fn refuse_starts(&self, refused: bool) {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.state_root).unwrap();
        std::fs::set_permissions(&self.state_root, std::fs::Permissions::from_mode(if refused { 0o500 } else { 0o700 })).unwrap();
    }
}

impl Drop for SdkLaunches {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
        self.model.close();
    }
}

/// The hosts of `engine_fixtures::owner::Slice` played by the model: the same doubled hosts serve
/// every daemon the slice opens, so a restarted one finds the hosts the one before left.
#[cfg(feature = "whole-engine")]
pub struct ModelHosts {
    pub hosts: Arc<InProcessHosts>,
    model: std::sync::OnceLock<ClaudeModel>,
    state: crate::test_support::Directory,
    /// Beside this path Claude writes the plans it asks about, as the fake native Claude does
    /// beside its capture.
    pub capture: PathBuf,
}

#[cfg(feature = "whole-engine")]
impl Default for ModelHosts {
    fn default() -> Self {
        let state = crate::test_support::Directory::for_unix_socket();
        let capture = state.join("capture");
        Self { hosts: Arc::default(), model: std::sync::OnceLock::new(), state, capture }
    }
}

#[cfg(feature = "whole-engine")]
impl ModelHosts {
    pub fn model(&self) -> &ClaudeModel {
        self.model.get().expect("the slice made its backend")
    }
}

#[cfg(feature = "whole-engine")]
impl crate::engine_fixtures::owner::Hosts for ModelHosts {
    fn backend(&mut self, home: &Path) -> crate::sdk_backend::SdkBackend {
        let model = ClaudeModel::new(home, Arc::default());
        model.set_turns(Turns::Answered);
        model.set_capture(&self.capture);
        self.hosts.set_serve(model.clone());
        let probe = model.clone();
        self.hosts.set_probe(move |run| probe.probe(run));
        assert!(self.model.set(model).is_ok(), "one backend per slice");
        model_backend(home, &self.state)
    }

    fn serve(&self, domain: &DomainRuntime) {
        self.hosts.provide(domain.ports());
    }

    fn starts(&self) -> Vec<Value> {
        self.model().starts().iter().map(NativeStart::record).collect()
    }

    fn prompts(&self) -> Vec<String> {
        self.model().prompts()
    }

    fn controls(&self) -> Vec<Value> {
        self.model().controls()
    }

    fn decisions(&self) -> Vec<Value> {
        self.model().decisions()
    }

    fn native_capture(&self) -> Value {
        capture_of(&self.model().starts().pop().expect("a native Claude started"))
    }

    fn kill(&self, group: i32) {
        self.model().kill_process(u32::try_from(group).expect("a host's number is positive"));
    }

    fn watch(&self, group: i32) -> crate::engine_fixtures::owner::HostWatch {
        let (model, host) = (self.model().clone(), u32::try_from(group).expect("a host's number is positive"));
        crate::engine_fixtures::owner::HostWatch::new(move || model.runs(host))
    }

    fn forget(&self, session: &str) {
        self.model().forget(session);
    }

    fn hold(&self) {
        self.model().hold();
    }

    fn hold_prompts(&self, _: &Path) {
        self.model().set_turns(Turns::Ignored);
    }

    fn release_prompts(&self, _: &Path) {
        self.model().set_turns(Turns::Answered);
    }

    fn recover(&self) {
        self.model().recover();
    }

    fn own_turn(&self) {
        self.model().own_turn();
    }
}

#[cfg(feature = "whole-engine")]
impl Drop for ModelHosts {
    fn drop(&mut self) {
        if let Some(model) = self.model.get() {
            model.close();
        }
    }
}

/// The command line and environment `start` got, as the fake native Claude records them: the
/// arguments, the names, and the digest of each value.
fn capture_of(start: &NativeStart) -> Value {
    use sha2::Digest as _;
    let digests: serde_json::Map<String, Value> = start.environment.iter()
        .map(|(key, value)| (key.clone(), Value::from(sha2::Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>())))
        .collect();
    serde_json::json!({"args": start.arguments, "env_keys": start.environment.keys().collect::<Vec<_>>(), "env_value_sha256": digests})
}

/// Whether `card` is one an ended turn of `task` rests on: the case the orchestrator settles when
/// the node stopped, or the user's card when the turn was interrupted or failed.
fn rests_on(task: &str, card: &InteractionId) -> bool {
    card.0.starts_with(&format!("{}{task}-", alondra_domain_rules::STALLED_PREFIX)) || card.0.starts_with(&format!("{}{task}-", alondra_domain_rules::TURN_ENDED_PREFIX))
}

/// A backend whose host, Node and native Claude are files no process runs: the doubled hosts
/// check their digests, and the model plays what they would.
fn model_backend(home: &Path, state: &Path) -> crate::sdk_backend::SdkBackend {
    let folder = home.join("model-host");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&folder).unwrap();
    let file = |name: &str| {
        let path = folder.join(name);
        std::fs::write(&path, format!("the model plays {name}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::canonicalize(path).unwrap()
    };
    let (node, host, native) = (file("node"), file("host.cjs"), file("claude"));
    let environment = crate::test_support::fixture_environment(home);
    crate::sdk_backend::SdkBackend {
        node_sha256: crate::sdk_backend::digest(&node).unwrap(),
        host_sha256: crate::sdk_backend::digest(&host).unwrap(),
        native_sha256: crate::sdk_backend::digest(&native).unwrap(),
        node,
        host,
        native,
        host_environment: environment.clone(),
        native_environment: environment,
        confinement: None,
        state_root: state.join("h"),
        io_timeout_seconds: 30,
        installed_native: false,
        release: crate::sdk_backend::this_release(),
        provider: alondra_core::accounts::Provider::Claude,
        origin: None,
    }
}
