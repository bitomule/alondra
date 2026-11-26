//! Launches through the SDK on doubled hosts the Claude and Codex host models play, and the backend
//! whose files no process runs. This crate's tests compile it within `model_fixture`, and
//! `daemon-server`'s, which open the whole daemon, as a module of their own: it names this crate's
//! items by the crate's name and the test crate's own only through `crate::test_support`.
use alondra_claude_host_model::{ClaudeModel, NativeStart, Turns};
use alondra_codex_host_model::CodexModel;
use alondra_daemon_engine_model::{LaunchOptions, LaunchOptionsFile as _};
use alondra_domain_model::InteractionId;
use alondra_engine_doubles::agent_host::{InProcessHosts, Serve, Served, Stop};
use alondra_engine_sdk::domain_runtime::DomainRuntime;
use alondra_engine_sdk::sdk_backend::SdkBackend;
use alondra_engine_sdk::sdk_owner::SdkOwners as _;
use serde_json::Value;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// How long a test waits for the turns of a task before it fails: far past what the slowest run
/// takes, so only a start nothing makes reaches it.
const TURNS_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);

/// A domain whose launches start doubled hosts the model plays. Dropping it stops the domain's
/// owners and ends every host left.
pub struct SdkLaunches {
    domain: Arc<DomainRuntime>,
    pub model: ClaudeModel,
    codex: CodexModel,
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
        let codex = CodexModel::default();
        hosts.set_serve(Models { claude: model.clone(), codex: codex.clone() });
        let probe = model.clone();
        hosts.set_probe(move |run| probe.probe(run));
        let state = crate::test_support::Directory::for_unix_socket();
        let backend = model_backend(home, &state);
        let state_root = backend.state_root.clone();
        domain.enable_sdk(backend);
        Self { domain: Arc::clone(domain), model, codex, hosts, state_root, _state: state }
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
        self.model.prompts().into_iter().filter(|prompt| !prompt.starts_with(alondra_domain_model::WAKE_INSTRUCTION)).collect()
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
    /// Past [`TURNS_DEADLINE`] it fails with what the task reads, so a start nothing makes is a
    /// failure that says where the task stopped, never a test that hangs.
    pub async fn until_turns_ended(&self, task: &str, turns: usize) {
        let mut changed = self.domain.subscribe();
        let ended = |domain: &DomainRuntime| domain.read().state().interactions.keys()
            .filter(|card| rests_on(task, card)).count() >= turns;
        let waited = alondra_core::clock::timeout(TURNS_DEADLINE, async {
            while !ended(&self.domain) {
                changed.changed().await.unwrap();
            }
        }).await;
        if waited.is_err() {
            panic!("{task} did not end {turns} turns within {TURNS_DEADLINE:?}: {}", self.where_it_stopped(task));
        }
    }

    fn where_it_stopped(&self, task: &str) -> String {
        let state = self.domain.state().unwrap();
        let id = alondra_foundation::TaskId::from(task);
        let cards: Vec<String> = state.interactions.iter()
            .filter(|(_, card)| matches!(&card.scope, alondra_domain_model::Scope::Task { task_id } if task_id == &id))
            // A problem's text says why the task stopped, such as why its host was lost.
            .map(|(id, card)| match card.card.kind {
                alondra_domain_model::CardKind::Problem => format!("{id}: {}", card.card.content.as_ref().map_or("", |content| content.body.as_str())),
                _ => id.0.clone(),
            }).collect();
        format!("task {:?}, lifecycle {:?}, cards {cards:?}, native starts {:?}", state.tasks.get(&id), state.lifecycle_of(&id), self.starts())
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
        self.codex.close();
        untroubled(&self.codex);
    }
}

/// The models that play the doubled hosts: each host goes to the Claude or the Codex model, by
/// the SDK its bootstrap says it runs.
pub struct Models {
    pub claude: ClaudeModel,
    pub codex: CodexModel,
}

impl Serve for Models {
    fn connect(&self, served: &Served, stream: std::os::unix::net::UnixStream) {
        if alondra_codex_host_model::plays(served.bootstrap()) { self.codex.connect(served, stream) } else { self.claude.connect(served, stream) }
    }

    fn stop(&self, served: &Served, stop: Stop) {
        if alondra_codex_host_model::plays(served.bootstrap()) { self.codex.stop(served, stop) } else { self.claude.stop(served, stop) }
    }
}

/// The command line and environment `start` got, as the fake native Claude records them: the
/// arguments, the names, and the digest of each value.
pub fn capture_of(start: &NativeStart) -> Value {
    use sha2::Digest as _;
    let digests: serde_json::Map<String, Value> = start.environment.iter()
        .map(|(key, value)| (key.clone(), Value::from(sha2::Sha256::digest(value.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>())))
        .collect();
    serde_json::json!({"args": start.arguments, "env_keys": start.environment.keys().collect::<Vec<_>>(), "env_value_sha256": digests})
}

/// Whether `card` is one an ended turn of `task` rests on: the case the orchestrator settles when
/// the node stopped, or the user's card when the turn was interrupted or failed.
fn rests_on(task: &str, card: &InteractionId) -> bool {
    card.0.starts_with(&format!("{}{task}-", alondra_domain_model::STALLED_PREFIX)) || card.0.starts_with(&format!("{}{task}-", alondra_domain_model::TURN_ENDED_PREFIX))
}

/// A backend whose host, Node and native Claude are files no process runs: the doubled hosts
/// check their digests, and the model plays what they would.
pub fn model_backend(home: &Path, state: &Path) -> SdkBackend {
    let folder = home.join("model-host");
    let (node, host, native) = (model_file(&folder, "node"), model_file(&folder, "host.cjs"), model_file(&folder, "claude"));
    let environment = crate::test_support::fixture_environment(home);
    SdkBackend {
        node_sha256: alondra_engine_sdk::sdk_backend::digest(&node).unwrap(),
        host_sha256: alondra_engine_sdk::sdk_backend::digest(&host).unwrap(),
        native_sha256: alondra_engine_sdk::sdk_backend::digest(&native).unwrap(),
        node,
        host,
        native,
        host_environment: environment.clone(),
        native_environment: environment,
        confinement: None,
        state_root: state.join("h"),
        io_timeout_seconds: 30,
        installed_native: false,
        release: alondra_engine_sdk::sdk_backend::this_release(),
        provider: alondra_core::accounts::Provider::Claude,
        origin: None,
    }
}

/// A file in `folder` that stands for `name`, which no process runs: the model plays it.
pub fn model_file(folder: &Path, name: &str) -> PathBuf {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, format!("the model plays {name}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::canonicalize(path).unwrap()
}

/// A test whose Codex host was asked something the model does not play fails, unless it fails
/// already.
pub fn untroubled(codex: &CodexModel) {
    if let Some(trouble) = codex.trouble().filter(|_| !std::thread::panicking()) {
        panic!("the Codex host model: {trouble}");
    }
}
