//! A node of the Agent SDK driven step by step, for the tests of pauses here and of what the
//! screens show of it in `daemon-server`.
use alondra_engine_sdk::domain_runtime::DomainRuntime;
use alondra_domain_model::{DomainState, TaskState};
use alondra_core::Millis;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use alondra_engine_sdk::sdk_owner::SdkOwners as _;
use alondra_daemon_engine::live::LiveOf as _;
use alondra_engine_admission::pauses::*;
use alondra_engine_admission::AdmissionOf as _;
use alondra_engine_admission::admission::{Hold, Key, Limit, SPACING, Tuning, Waiting};
use alondra_daemon_engine::machine_state::NoRoom;
use alondra_daemon_engine::node_push::testing::Node;
use alondra_daemon_engine::node_reports::{Envelope, Report};
use alondra_core::{Machine, MemoryCompressor, MemoryPressure};
use alondra_domain_model::{API_RETRIES, Actor, AgentId, ApiRetry, Command, CommandEnvelope, NodePhase, OUTAGE_PROBE, PROTOCOL_VERSION, Pause, PauseReason, TaskId, TaskPriority};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::Arc;
use super::owner::Hosts;


pub struct Home(pub crate::test_support::Directory);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: alondra_daemon_support::auth::random_hex(8).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

pub fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

pub const SDK_TASK: &str = "t-sdk-paused";

/// Drives an owner until it has nothing to send and its host owes nothing.
pub fn settle(driver: &mut alondra_engine_sdk::sdk_owner::Driver) {
    while driver.pending_permits() > 0 || driver.waits_for_host() {
        if driver.pending_permits() > 0 {
            driver.step(&|| false).unwrap();
        } else {
            driver.observe_written().unwrap();
        }
    }
}

/// A task of the Agent SDK whose turn runs, on the hosts `H` plays. Its owner is driven here step
/// by step, so nothing reaches Claude unless a step hands it over.
pub struct SdkNode<H: Hosts = crate::engine_tests::SliceHosts> {
    pub driver: alondra_engine_sdk::sdk_owner::Driver,
    pub domain: Arc<DomainRuntime>,
    pub backend: alondra_daemon_engine::sdk_backend::SdkBackend,
    /// Before `home`: the hosts write in the HOME they get, so they end before it is removed.
    pub hosts: H,
    pub home: crate::test_support::Directory,
}

impl SdkNode {
    /// A node whose first turn the fake Claude holds open.
    pub fn working() -> Self {
        let mut node = Self::told("consume-and-hold");
        while node.lifecycle().activity() != alondra_domain::ActivityView::Working {
            node.driver.observe_written().unwrap();
        }
        node
    }

    /// A node whose first turn ended: it rests until it is told something.
    pub fn resting() -> Self {
        let mut node = Self::told("Write the notes");
        settle(&mut node.driver);
        node
    }

    pub fn told(prompt: &str) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        let mut hosts = crate::engine_tests::SliceHosts::default();
        let backend = hosts.backend(&home);
        let domain = Arc::new(super::open(&home).unwrap());
        hosts.serve(&domain);
        domain.enable_sdk(backend.clone());
        run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateRequest {
            request_id: "r".into(), body: "Write the release notes".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r".into(), title: "Release notes".into() });
        run(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: SDK_TASK.into(), request_id: "r".into(), title: "Write the release notes".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_domain_model::LaunchPlan::new("node", worktree.to_str().unwrap())) });
        let driver = alondra_engine_sdk::sdk_owner::start(domain.clone(), &backend, alondra_engine_sdk::sdk_owner::Launch {
            task: SDK_TASK.into(), name: "Release notes".into(), directory: worktree.to_str().unwrap().into(),
            prompt: prompt.into(), plugin: home.join("plugin"), options: Default::default(),
            model: None, account: alondra_core::accounts::AccountId::default_account(),
        }).unwrap();
        domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
        domain.admission().ignore_mac();
        domain.machine_watch().measure_disk(alondra_engine_ports::disk::Space { total: 500_000_000_000, free: 100_000_000_000, purgeable: 0 });
        Self { driver, domain, backend, hosts, home }
    }

    /// One more node of the same host root whose first turn the fake Claude holds open.
    pub fn beside(&self, task: &str) -> alondra_engine_sdk::sdk_owner::Driver {
        let mut driver = self.beside_told(task, "consume-and-hold");
        while self.activity(task) != alondra_domain::ActivityView::Working {
            driver.observe_written().unwrap();
        }
        driver
    }

    /// One more node of the same host root, resting after its first turn.
    pub fn beside_resting(&self, task: &str) -> alondra_engine_sdk::sdk_owner::Driver {
        let mut driver = self.beside_told(task, "Write the notes");
        settle(&mut driver);
        driver
    }

    pub fn beside_told(&self, task: &str, prompt: &str) -> alondra_engine_sdk::sdk_owner::Driver {
        let worktree = self.home.join(task);
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        run(&self.domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: task.into(), request_id: "r".into(), title: format!("Task {task}"), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_domain_model::LaunchPlan::new("node", worktree.to_str().unwrap())) });
        let driver = alondra_engine_sdk::sdk_owner::start(self.domain.clone(), &self.backend, alondra_engine_sdk::sdk_owner::Launch {
            task: task.into(), name: format!("Task {task}"), directory: worktree.to_str().unwrap().into(),
            prompt: prompt.into(), plugin: self.home.join("plugin"), options: Default::default(),
            model: None, account: alondra_core::accounts::AccountId::default_account(),
        }).unwrap();
        self.domain.sdk_owners().driven_by_hand.lock().unwrap().insert(driver.agent().clone());
        driver
    }

    pub fn tell(&self, task: &str, text: &str) {
        alondra_daemon_engine::domain_messages::tell_sdk_node(&*self.domain, &task.into(), alondra_daemon_engine::node_push::ALONDRA, text, &format!("tell-{task}"), false).unwrap();
    }

    pub fn set_limit(&self, limit: u32) {
        self.domain.admission().tune(Tuning { spacing: 0, limit: Some(Limit::Fixed(limit)) });
    }

    pub fn gate(&self) -> alondra_engine_admission::admission::Snapshot {
        self.domain.admission().snapshot(&*self.domain, alondra_core::now_millis()).unwrap()
    }

    /// The tasks whose next turn waits for a slot, in the gate's order.
    pub fn waiting_turns(&self) -> Vec<String> {
        self.gate().queue.into_iter().filter_map(|waiting| match waiting.key { Key::Turn(task) => Some(task.0), _ => None }).collect()
    }

    pub fn cards(&self) -> Vec<alondra_domain_model::InteractionId> {
        self.domain.state().unwrap().interactions.keys().cloned().collect()
    }

    pub fn activity(&self, task: &str) -> alondra_domain::ActivityView {
        self.domain.state().unwrap().lifecycle_of(&task.into()).unwrap().activity()
    }

    pub fn lifecycle(&self) -> alondra_domain::TaskLifecycle {
        let read = self.domain.read();
        alondra_domain_model::replay(&SDK_TASK.into(), &read.state().lifecycle_journal[&TaskId::from(SDK_TASK)]).unwrap()
    }

    pub fn agent(&self) -> String {
        self.driver.agent().0.clone()
    }

    /// The prompts Claude got, besides the wake Alondra sends after a turn that ended empty.
    pub fn prompts(&self) -> Vec<String> {
        super::owner::not_wakes(self.hosts.prompts().into_iter())
    }

    pub fn pending_inputs(&self) -> usize {
        self.lifecycle().pending_inputs().len()
    }

    /// Steps the owner as its thread does, until `done` holds: each step sends what may be sent,
    /// or reads what the host has.
    pub fn step_until(&mut self, done: impl Fn(&Self) -> bool) {
        while !done(self) {
            self.driver.step(&|| true).unwrap();
        }
    }

    /// A few steps of the owner: each sends whatever it may send.
    pub fn step(&mut self) {
        for _ in 0..4 {
            self.driver.step(&|| true).unwrap();
        }
    }

    /// How many of the task's inputs the owner claimed to hand to its host.
    pub fn dispatched_inputs(&self) -> i64 {
        rusqlite::Connection::open(alondra_engine_sdk::domain_store::active_path(&self.home)).unwrap()
            .query_row("SELECT count(*) FROM lifecycle_dispatches WHERE task_id=?1 AND kind='input' AND status NOT IN ('prepared','revoked')", [SDK_TASK], |row| row.get(0)).unwrap()
    }

    /// Steps the owner until Claude got a prompt that says `text`.
    pub fn until_told(&mut self, text: &str) {
        self.step_until(|node| node.prompts().iter().any(|prompt| prompt.contains(text)));
    }

    pub fn turn_ended(&self) -> bool {
        self.domain.read().state().lifecycle_journal[&TaskId::from(SDK_TASK)].iter()
            .any(|record| matches!(record, alondra_domain_model::LifecycleRecord::TurnEnded { .. }))
    }

    /// One look of the gate, which lets the node go on once it has a slot.
    pub async fn gate_look(&self) {
        alondra_engine_admission::admission::pass_at(&*self.domain, &self.home, alondra_core::now_millis()).await.unwrap();
    }

    /// The user pauses the node from its task; its turn is interrupted and ends.
    pub fn pause(&mut self) {
        self.domain.admission().pause_by_user(&*self.domain, &self.agent(), alondra_core::now_millis()).unwrap();
        self.step_until(Self::turn_ended);
        let state = self.domain.state().unwrap();
        assert!(matches!(state.node_pause(&AgentId(self.agent())), Some(Pause { reason: PauseReason::ByUser, .. })), "{:?}", state.node_pause(&AgentId(self.agent())));
        assert_eq!(state.node_phase(&AgentId(self.agent())), Some(&NodePhase::ThroughSdk), "its task's lifecycle holds the pause, not its phase");
    }

    /// Each native Claude the hosts started, with the session it runs and the one it resumed.
    pub fn starts(&self) -> Vec<serde_json::Value> {
        self.hosts.starts()
    }

    /// The node's host dies while it is paused and no owner of the daemon is left to replace it.
    pub fn lose_host(&mut self) {
        std::fs::DirBuilder::new().mode(0o700).create(self.home.join("plugin")).unwrap();
        self.hosts.kill(self.driver.worker.host_group().unwrap());
        self.domain.sdk_owners().driven_by_hand.lock().unwrap().remove(self.driver.agent());
        self.driver.let_go();
        assert!(!self.domain.owns_sdk_host(&AgentId(self.agent())));
    }

    pub fn deliver(&self) {
        run(&self.domain, Actor::Agent { agent_id: self.driver.agent().clone() }, Command::PresentResult {
            attachments: vec![], task_id: SDK_TASK.into(), interaction_id: "result".into(), body: "Done".into(), is_final: true, nothing_to_review: false,
            targets: vec![alondra_domain_model::DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
            card: alondra_domain_model::CardSpec::delivery("Review the result", &[], true) });
    }

    /// The user sends the delivery back with changes.
    pub fn get_it_back(&self) {
        run(&self.domain, user(), Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "result".into(),
            targets: vec![alondra_domain_model::DeliveryTarget::Text { text: "Done".into() }], body: "Pass the MCP variables too".into(),
            instruction: alondra_domain_model::Instruction::Changes });
    }

    pub fn host_lost_card(&self) -> Option<alondra_domain_model::Interaction> {
        super::owner::open_card(&self.domain, alondra_domain_model::HOST_LOST_PREFIX)
    }
}

impl<H: Hosts> Drop for SdkNode<H> {
    fn drop(&mut self) {
        let _ = self.driver.stop();
    }
}

pub fn node_pause_card(domain: &DomainRuntime) -> Option<alondra_domain_model::InteractionId> {
    let state = domain.state().unwrap();
    state.interactions.values().find(|card| state.node_pause_card(&card.id) && state.open(card)).map(|card| card.id.clone())
}

pub fn answer_resume(domain: &DomainRuntime, card: &alondra_domain_model::InteractionId) -> Result<(), String> {
    let state = domain.state().unwrap();
    domain.execute(&user(), &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: alondra_daemon_support::auth::random_hex(8).into(),
        expected_revision: state.revision,
        command: Command::AnswerQuestion { interaction_id: card.clone(), option_id: "resume".into(), inputs: Default::default(), attachments: vec![] },
    }, None, || Ok(())).map(drop).map_err(|error| format!("{error:?}"))
}
