//! The orchestrator's batches with no process behind them: the domain's hosts are the doubled hosts
//! of `engine-doubles`, the orchestrator model of the Claude host model plays each batch's host and
//! native Claude as the fake orchestrator Claude plays them behind the real host, and the Node,
//! host and native Claude of the backend are files no process runs.
use alondra_claude_host_model::OrchestratorModel;
use alondra_engine_doubles::agent_host::InProcessHosts;
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The doubled hosts of a fixture's home and the model that plays them. Dropping it ends every
/// host; a test whose hosts were asked something the model does not play fails, unless it fails
/// already.
pub(crate) struct Played {
    pub model: OrchestratorModel,
    _hosts: Arc<InProcessHosts>,
}

impl Played {
    /// Every runtime opened at `home` from now on runs its batches on hosts the model plays.
    pub fn serve(home: &Path) -> Self {
        let hosts = Arc::new(InProcessHosts::default());
        let model = OrchestratorModel::default();
        hosts.set_serve(model.clone());
        crate::wiring::serve(home, &hosts);
        Self { model, _hosts: hosts }
    }
}

impl Drop for Played {
    fn drop(&mut self) {
        self.model.close();
        if let Some(trouble) = self.model.trouble().filter(|_| !std::thread::panicking()) {
            panic!("the orchestrator model: {}", trouble.0);
        }
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

/// A file in `folder` that stands for `name`, which no process runs: the model plays it.
fn model_file(folder: &Path, name: &str) -> PathBuf {
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, format!("the model plays {name}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::canonicalize(path).unwrap()
}
