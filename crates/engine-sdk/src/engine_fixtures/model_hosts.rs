//! The hosts of `owner::Slice` played by the Claude and Codex host models, which every test crate
//! that compiles these fixtures without the feature real-hosts names in
//! `crate::engine_tests::SliceHosts`. It builds on the launches of `crate::model_fixture`.
use crate::model_fixture::{Models, capture_of, model_backend, model_file, untroubled};
use alondra_claude_host_model::{ClaudeModel, NativeStart, Turns};
use alondra_codex_host_model::CodexModel;
use alondra_engine_doubles::agent_host::{InProcessHosts, Stop};
use alondra_engine_sdk::domain_runtime::DomainRuntime;
use alondra_engine_sdk::sdk_backend::SdkBackend;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The same doubled hosts serve every daemon the slice opens, so a restarted one finds the hosts
/// the one before left.
pub struct ModelHosts {
    pub hosts: Arc<InProcessHosts>,
    model: std::sync::OnceLock<ClaudeModel>,
    pub codex: CodexModel,
    state: crate::test_support::Directory,
    /// Beside this path Claude writes the plans it asks about, as the fake native Claude does
    /// beside its capture.
    pub capture: PathBuf,
}

impl Default for ModelHosts {
    fn default() -> Self {
        let state = crate::test_support::Directory::for_unix_socket();
        let capture = state.join("capture");
        Self { hosts: Arc::default(), model: std::sync::OnceLock::new(), codex: CodexModel::default(), state, capture }
    }
}

impl ModelHosts {
    pub fn model(&self) -> &ClaudeModel {
        self.model.get().expect("the slice made its backend")
    }
}

impl super::owner::Hosts for ModelHosts {
    fn backend(&mut self, home: &Path) -> SdkBackend {
        let model = ClaudeModel::new(home, Arc::default());
        model.set_turns(Turns::Answered);
        model.set_capture(&self.capture);
        self.hosts.set_serve(Models { claude: model.clone(), codex: self.codex.clone() });
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
        let host = u32::try_from(group).expect("a host's number is positive");
        self.model().kill_process(host);
        self.codex.kill_process(host);
    }

    fn watch(&self, group: i32) -> super::owner::HostWatch {
        let (model, codex, host) = (self.model().clone(), self.codex.clone(), u32::try_from(group).expect("a host's number is positive"));
        super::owner::HostWatch::new(move || model.runs(host) || codex.runs(host))
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

    fn strand(&self, folder: &Path) {
        self.hosts.strand(folder);
    }

    fn native_runs(&self, pid: i32) -> bool {
        self.model().runs(u32::try_from(pid).expect("a host's number is positive"))
    }

    fn background(&self) -> Vec<alondra_process_group::Member> {
        self.model().background()
    }

    fn background_runs(&self, process: &alondra_process_group::Member) -> bool {
        self.hosts.descendant_runs(process)
    }

    fn release_background_command(&self) {
        use std::os::unix::process::ExitStatusExt as _;
        let background = self.model().background();
        let [command] = background.as_slice() else { panic!("one command runs in the background: {background:?}") };
        self.hosts.end_descendant(command, std::process::ExitStatus::from_raw(0));
    }

    fn end_background(&self, processes: &[alondra_process_group::Member]) {
        for process in processes {
            self.hosts.end_descendant(process, Stop::Kill.status());
        }
    }

    fn codex_executables(&self, home: &Path) -> (PathBuf, PathBuf) {
        (model_file(&home.join("model-host"), "codex-host.cjs"), model_file(home, "codex-native"))
    }

    fn codex_legacy_host(&self, home: &Path) -> PathBuf {
        model_file(&home.join("model-host"), alondra_codex_host_model::LEGACY_HOST)
    }

    fn kill_native(&self, native: &alondra_process_group::Member) {
        self.codex.kill_native(u32::try_from(native.pid).expect("a host's number is positive"));
    }

    fn freeze(&self, group: i32) {
        let process = u32::try_from(group).expect("a host's number is positive");
        self.codex.freeze(process);
        let served = self.hosts.live().iter().filter_map(|folder| self.hosts.served(folder)).find(|served| served.process_id() == process);
        served.expect("a doubled host runs under that number").freeze();
    }

    fn runs_in(&self, folder: &Path) -> bool {
        self.hosts.live().iter().any(|live| live == folder)
    }

    fn release_codex_commands(&self, _: &Path) {
        use std::os::unix::process::ExitStatusExt as _;
        for command in self.codex.commands() {
            self.hosts.end_descendant(&command, std::process::ExitStatus::from_raw(0));
        }
    }
}

impl Drop for ModelHosts {
    fn drop(&mut self) {
        if let Some(model) = self.model.get() {
            model.close();
        }
        self.codex.close();
        untroubled(&self.codex);
    }
}
