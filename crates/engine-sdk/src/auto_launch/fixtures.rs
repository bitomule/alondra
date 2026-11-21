//! Automatic launches as the tests here and in the crates above set them up.
use alondra_daemon_engine::gate::GateOf as _;
use crate::sdk_owner::SdkOwners as _;
use alondra_daemon_engine::live::LiveOf as _;
use super::*;
use alondra_engine_admission::AdmissionOf as _;
use alondra_daemon_engine_model::LaunchPlan;
#[cfg(all(test, not(feature = "real-hosts")))]
use crate::model_fixture::SdkLaunches;
#[cfg(any(not(test), feature = "real-hosts"))]
use crate::sdk_owner::fixtures::SdkLaunches;
use alondra_domain_rules::{
    Actor, StartProblem,
    Command, CommandEnvelope, DeliveryTarget, InteractionId, MergeMethod, NodeEnd, PROTOCOL_VERSION, PrState, TaskId,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;


pub struct Fixture {
    pub sdk: SdkLaunches,
    pub root: crate::test_support::Directory,
    pub home: PathBuf,
    pub project: PathBuf,
    pub domain: Arc<DomainRuntime>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
        self.sdk.refuse_starts(false);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

pub fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("c-{}", crate::auth::random_hex(8)).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

pub fn world() -> Fixture {
    world_using_sdk(SdkLaunches::enable_holding)
}

pub fn world_using_sdk(enable: fn(&Arc<DomainRuntime>, &Path) -> SdkLaunches) -> Fixture {
    let root = crate::test_support::Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(crate::engine_fixtures::open(&home).unwrap());
    alondra_engine_admission::measure_machine(&domain, &alondra_core::Machine { at: 1, memory_total: 16 << 30,
        memory_available: 12 << 30, memory_free_percent: Some(75), cpus: 11,
        disk_total: 500_000_000_000, disk_available: 400_000_000_000, ..Default::default() }, alondra_engine_ports::disk::Space { total: 500_000_000_000, free: 400_000_000_000, purgeable: 0 });
    let sdk = enable(&domain, &home);
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Rebuild the editor".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Editor".into() });
    Fixture { sdk, root, home, project, domain }
}

pub fn plan(f: &Fixture, name: &str) -> LaunchPlan {
    LaunchPlan {
        name: name.into(), cwd: f.project.to_str().unwrap().into(), mission: None, model: Some("sonnet".into()), effort: None,
        approach: None, reason: Some("A clear, mechanical change".into()), role: None, tags: None, project: None,
    }
}

/// A task created the way the orchestrator creates one: with its plan, through its capability.
pub fn create(f: &Fixture, id: &str, after: &[&str], plan: Option<&LaunchPlan>) {
    let bearer = f.domain.bind_orchestrator(&format!("execution-{id}")).unwrap().expose_for_child_environment().to_owned();
    let state = f.domain.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("create-{id}").into(), expected_revision: 0,
        command: Command::CreateTask { plan_items: None, continues: None,
            task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: after.iter().map(|dep| TaskId::from(*dep)).collect(), stack: None,
            launch: plan.cloned(),
        },
    };
    f.domain.execute_bearer(&bearer, &envelope).unwrap();
}

/// A task an earlier node works on, which was never launched by Alondra.
pub fn open_task(f: &Fixture, id: &str) {
    command(&f.domain, Actor::System, Command::RegisterAgent { agent_id: format!("seed-{id}").into(), name: format!("Seed {id}").into(), at: 0 });
    command(&f.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"),
        agent_id: Some(format!("seed-{id}").into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")), });
}

/// How many nodes Alondra started through the SDK.
pub fn launches(f: &Fixture) -> usize {
    f.domain.state().unwrap().agents.keys().filter(|agent| agent.0.starts_with("sdk-")).count()
}

pub fn told(f: &Fixture) -> Vec<String> {
    f.domain.state().unwrap().interactions.keys().map(|id| id.0.clone()).collect()
}


pub fn problem(f: &Fixture, task: &str) -> Option<alondra_domain_rules::Interaction> {
    f.domain.state().unwrap().open_start_problem(&task.into()).cloned()
}
