//! The orchestrator's batches with no process behind them: the domain's hosts are the doubled hosts
//! of `engine-doubles`, the orchestrator model of the Claude host model plays each Claude batch's
//! host and native Claude as the fake orchestrator Claude plays them behind the real host, the Codex
//! host model plays each Codex batch's host and app-server as the fake Codex plays them, and the
//! Node, host and native programs of the backend are files no process runs.
use alondra_claude_host_model::OrchestratorModel;
use alondra_codex_host_model::CodexModel;
use alondra_engine_doubles::agent_host::{InProcessHosts, Serve, Served, Stop};
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use crate::engine::OrchestratorOf as _;

/// The doubled hosts of a fixture's home and the model that plays them. Dropping it ends every
/// host; a test whose hosts were asked something the model does not play fails, unless it fails
/// already.
pub(crate) struct Played {
    pub model: OrchestratorModel,
    codex: CodexModel,
    pub hosts: Arc<InProcessHosts>,
    /// How many batches' hosts the executor recorded as started, the injected spawn hook run.
    pub recorded: Arc<AtomicUsize>,
}

impl Played {
    /// Every runtime opened at `home` from now on runs its batches on hosts the models play.
    pub fn serve(home: &Path) -> Self {
        let hosts = Arc::new(InProcessHosts::default());
        let (model, codex) = (OrchestratorModel::default(), CodexModel::default());
        hosts.set_serve(Models { claude: model.clone(), codex: codex.clone() });
        crate::wiring::serve(home, &hosts);
        Self { model, codex, hosts, recorded: Arc::default() }
    }
}

impl Played {
    /// Counts each host the executor of `domain`, whose home is `home`, records as started.
    pub fn count_recorded(&self, domain: &crate::DomainRuntime, home: &Path) {
        let (at, recorded) = (home.to_owned(), Arc::clone(&self.recorded));
        domain.orchestrator().injected.on_spawn(move |capability| {
            alondra_daemon_engine_model::faults::spawned(&at, capability);
            recorded.fetch_add(1, Ordering::SeqCst);
        });
    }

    /// Waits until the executor recorded every host the model started, or the batch of the last
    /// one ended without recording it. A native Claude plays its prompt as soon as its host takes
    /// it, inside the executor's start, before the executor records the host: a real host's
    /// process only gets the prompt later, so a test that acts on a held turn acts after that.
    pub fn wait_recorded(model: &OrchestratorModel, recorded: &AtomicUsize, home: &Path) {
        let started = model.starts().len();
        while recorded.load(Ordering::SeqCst) < started
            && crate::domain_orchestrator::load(home).is_ok_and(|runtime| runtime.active_run.is_some_and(|run| !run.spawned))
        {
            alondra_core::clock::blocking_sleep(std::time::Duration::from_millis(2));
        }
    }
}

impl Drop for Played {
    fn drop(&mut self) {
        self.model.close();
        self.codex.close();
        if std::thread::panicking() {
            return;
        }
        if let Some(trouble) = self.model.trouble() {
            panic!("the orchestrator model: {}", trouble.0);
        }
        if let Some(trouble) = self.codex.trouble() {
            panic!("the Codex host model: {trouble}");
        }
    }
}

/// Each doubled host goes to the Claude or the Codex model, by the SDK its bootstrap says it runs.
struct Models {
    claude: OrchestratorModel,
    codex: CodexModel,
}

impl Serve for Models {
    fn connect(&self, served: &Served, stream: std::os::unix::net::UnixStream) {
        if alondra_codex_host_model::plays(served.bootstrap()) { self.codex.connect(served, stream) } else { self.claude.connect(served, stream) }
    }

    fn stop(&self, served: &Served, stop: Stop) {
        if alondra_codex_host_model::plays(served.bootstrap()) { self.codex.stop(served, stop) } else { self.claude.stop(served, stop) }
    }
}

/// A backend whose host, Node and native Claude are files in `root` no process runs: the doubled
/// hosts check their digests, and the model plays what they would. Its hosts live in `root` too.
pub(crate) fn backend(root: &Path, environment: BTreeMap<String, String>) -> crate::sdk_backend::SdkBackend {
    let folder = root.join("model-host");
    let (node, host, native) = (model_file(&folder, "node"), model_file(&folder, "host.cjs"), model_file(&folder, "claude"));
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
        state_root: root.join("hosts").join("h"),
        io_timeout_seconds: 30,
        installed_native: false,
        release: crate::sdk_backend::this_release(),
        provider: alondra_core::accounts::Provider::Claude,
        origin: None,
    }
}

/// `backend` on Codex: its host and native app-server are files in `root` no process runs, which the
/// Codex host model plays, and the app-server gets `environment` too.
pub(crate) fn on_codex(root: &Path, mut backend: crate::sdk_backend::SdkBackend, environment: BTreeMap<String, String>) -> crate::sdk_backend::SdkBackend {
    let host = model_file(&root.join("model-host"), "codex-host.cjs");
    backend.native = model_file(root, "codex-native");
    backend.native_sha256 = crate::sdk_backend::digest(&backend.native).unwrap();
    backend.host_sha256 = crate::sdk_backend::digest(&host).unwrap();
    backend.node_sha256 = backend.host_sha256.clone();
    backend.node = host.clone();
    backend.host = host;
    backend.native_environment.extend(environment);
    backend.provider = alondra_core::accounts::Provider::Codex;
    backend.origin = None;
    backend
}

/// A file in `folder` that stands for `name`, which no process runs: the model plays it.
fn model_file(folder: &Path, name: &str) -> PathBuf {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, format!("the model plays {name}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::canonicalize(path).unwrap()
}
