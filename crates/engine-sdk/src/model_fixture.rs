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
        use sha2::Digest as _;
        let environment = self.last().environment;
        let digests: serde_json::Map<String, Value> = environment.iter()
            .map(|(key, value)| (key.clone(), Value::from(sha2::Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>())))
            .collect();
        serde_json::json!({"args": self.args(), "env_keys": environment.keys().collect::<Vec<_>>(), "env_value_sha256": digests})
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
