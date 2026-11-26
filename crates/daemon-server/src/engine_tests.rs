//! Tests of the engine that go through alondrad's routes, views or delivery. The engine crate
//! cannot see those, so these run here, on the fixtures the engine's crates expose: on the doubled
//! hosts the host models play, or with the feature real-hosts on the real hosts, for the few that
//! need a real Agent SDK host or a fake native Claude.
#[cfg(not(feature = "real-hosts"))]
mod acceptance_catalog_tests;
#[cfg(not(feature = "real-hosts"))]
mod admission_tests;
#[cfg(not(feature = "real-hosts"))]
mod approval_revocation_tests;
#[cfg(not(feature = "real-hosts"))]
mod domain_github_tests;
#[cfg(not(feature = "real-hosts"))]
mod domain_orchestrator_tools_tests;
#[cfg(not(feature = "real-hosts"))]
mod domain_transport_tests;
#[cfg(not(feature = "real-hosts"))]
mod launch_plans_tests;
#[cfg(not(feature = "real-hosts"))]
mod load_bench_tests;
#[cfg(not(feature = "real-hosts"))]
mod orchestrator_accounts_tests;
#[cfg(not(feature = "real-hosts"))]
mod orchestrator_inbox_tests;
#[cfg(not(feature = "real-hosts"))]
mod pauses_tests;
#[cfg(not(feature = "real-hosts"))]
mod retention_tests;
mod sdk_owner_tests;
#[cfg(not(feature = "real-hosts"))]
mod task_list_tests;
#[cfg(not(feature = "real-hosts"))]
mod usage_pause_tests;

/// The SQLite-backed fixtures of an SDK node's owner, which the engine's tests share.
#[path = "../../daemon-engine/src/wiring/owner_fixtures.rs"]
pub(crate) mod owner_fixtures;

/// The engine as `alondrad` puts it together, which this crate cannot see.
#[path = "../../engine-sdk/src/engine_assembly.rs"]
pub(crate) mod engine_assembly;

/// The fixtures of the SDK's nodes, launches and orchestrator tools.
#[path = "../../engine-sdk/src/engine_fixtures.rs"]
pub(crate) mod engine_fixtures;

/// What plays the hosts of `engine_fixtures::owner::Slice` and the launches the fixtures record:
/// the models, or real processes in the target that runs them.
#[cfg(not(feature = "real-hosts"))]
pub(crate) use engine_fixtures::model_hosts::ModelHosts as SliceHosts;
#[cfg(feature = "real-hosts")]
pub(crate) use engine_fixtures::owner::ProcessHosts as SliceHosts;

#[cfg(not(feature = "real-hosts"))]
pub(crate) use crate::model_fixture::SdkLaunches;
#[cfg(feature = "real-hosts")]
pub(crate) use engine_fixtures::owner::SdkLaunches;

/// Opens every runtime the engine's fixtures open on this thread as the daemon does, with the gate
/// of a test.
pub(crate) fn wired() {
    alondra_foundation::durability::relax();
    alondra_engine_kernel::TEST_JOURNAL.get_or_init(|| &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules));
    alondra_daemon_engine::wiring::open_with(open_at);
}

/// The runtime as the daemon opens it, with the doubles of the Mac and of GitHub, the agent hosts of this target
/// and a gate that is unpaced and launches on the disk the machine watch measured; every command it
/// accepts keeps the domain's invariants or fails the test.
pub(crate) fn open_at(home: &std::path::Path, database: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    alondra_domain_model::invariants::check_after_every_command();
    let runtime = engine_assembly::open_engine_at(home, database, files)?;
    alondra_engine_orchestrator::wire(runtime.ports());
    alondra_engine_doubles::machine::wire(runtime.ports(), alondra_engine_ports::watcher::claude_home);
    alondra_engine_doubles::github::wire(runtime.ports());
    #[cfg(feature = "real-hosts")]
    alondra_agent_runtime::wire(runtime.ports(), Default::default());
    #[cfg(not(feature = "real-hosts"))]
    alondra_engine_doubles::agent_host::wire(runtime.ports());
    alondra_engine_admission::for_tests(&runtime);
    Ok(runtime)
}

/// The kernel's runtime behind the server's `dyn Runtime`, for a test that drives what only the
/// kernel offers: capabilities, fixture commands, its lanes of work.
pub(crate) trait Kernel {
    fn kernel(&self) -> &alondra_engine_kernel::domain_runtime::DomainRuntime;
}

impl Kernel for dyn alondra_engine_ports::Runtime {
    fn kernel(&self) -> &alondra_engine_kernel::domain_runtime::DomainRuntime {
        alondra_engine_kernel::domain_runtime::DomainRuntime::of(self)
    }
}

impl crate::domain_bootstrap::DomainDaemon {
    /// The daemon over the engine as a test opens it on its thread, on `files`.
    pub(crate) fn open(home: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<std::sync::Arc<Self>> {
        crate::domain_bootstrap::require_database(home)?;
        wired();
        Self::over(home, alondra_daemon_engine::wiring::open_on(home, &alondra_engine_ports::journal::active_path(home), files)?)
    }
}

/// `open_at` at the home's active database; the engine's fixtures this thread opens open the same.
pub(crate) fn open(home: &std::path::Path) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    open_on(home, &alondra_store_sqlite::Sqlite(&alondra_domain_rules::Rules))
}

/// `open_at` at the home's active database, on `files`, as the root opens the engine.
pub(crate) fn open_on(home: &std::path::Path, files: &dyn alondra_engine_ports::journal::JournalFiles) -> anyhow::Result<alondra_engine_sdk::domain_runtime::DomainRuntime> {
    wired();
    open_at(home, &alondra_engine_ports::journal::active_path(home), files)
}

/// The orchestrator of a daemon home through the SDK, on the doubled hosts of its runtime, which the
/// orchestrator model of the Claude host model plays as the fake orchestrator Claude plays them.
/// What a test reads or sets of Claude, it reads or sets on the model; the rest is the fixture's.
#[cfg(not(feature = "real-hosts"))]
pub(crate) struct PlayedOrchestrator {
    fixture: alondra_engine_orchestrator::domain_orchestrator::testing::Fixture,
    model: alondra_claude_host_model::OrchestratorModel,
}

#[cfg(not(feature = "real-hosts"))]
impl PlayedOrchestrator {
    /// The orchestrator of `domain`, opened by this crate's `open_at` at the daemon home `home`;
    /// its backend and plugins live in `root`, which it keeps until it is dropped. No doubled host
    /// is a process of the kernel: each is born at the same moment for the batch that records it.
    pub(crate) fn over(root: crate::test_support::Directory, domain: std::sync::Arc<alondra_engine_sdk::domain_runtime::DomainRuntime>, home: std::path::PathBuf) -> Self {
        use alondra_engine_orchestrator::engine::OrchestratorOf as _;
        let model = alondra_claude_host_model::OrchestratorModel::default();
        alondra_engine_doubles::agent_host::of(domain.ports()).set_serve(model.clone());
        domain.orchestrator().injected.date_hosts(|_| Some(1));
        let backend = crate::model_fixture::model_backend(&root, &root);
        let fixture = alondra_engine_orchestrator::domain_orchestrator::testing::Fixture::serving(root, domain, home, backend, Box::new(()));
        Self { fixture, model }
    }

    /// The capability each start of the native Claude got, one per start.
    pub(crate) fn starts(&self) -> Vec<String> {
        self.model.starts()
    }

    /// Every prompt the native Claudes took, in order.
    pub(crate) fn prompts(&self) -> Vec<String> {
        self.model.prompts()
    }

    /// Only the next start's prompts start with `marker`.
    pub(crate) fn mark_once(&self, marker: &str) {
        self.model.mark_once(marker);
    }
}

#[cfg(not(feature = "real-hosts"))]
impl std::ops::Deref for PlayedOrchestrator {
    type Target = alondra_engine_orchestrator::domain_orchestrator::testing::Fixture;

    fn deref(&self) -> &Self::Target {
        &self.fixture
    }
}

/// Ends every host; a test whose hosts were asked something the model does not play fails, unless
/// it fails already.
#[cfg(not(feature = "real-hosts"))]
impl Drop for PlayedOrchestrator {
    fn drop(&mut self) {
        self.model.close();
        if let Some(trouble) = self.model.trouble().filter(|_| !std::thread::panicking()) {
            panic!("the orchestrator model: {}", trouble.0);
        }
    }
}
