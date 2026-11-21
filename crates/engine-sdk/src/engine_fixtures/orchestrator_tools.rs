//! The orchestrator's tools as the tests here and in the crates above call them.
//! The launch tools' tests (`domain_orchestrator_tools`), which start nodes through the SDK.
use alondra_core::clock;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use alondra_engine_ports::worktrees::Workplace;
use alondra_domain_rules::{Actor, AgentId, DomainError, DomainState, RequestId, TaskId};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::Path;
use crate::sdk_owner::SdkOwners as _;
use alondra_daemon_engine::gate::GateOf as _;
use alondra_daemon_engine::live::LiveOf as _;
use alondra_daemon_engine::sdk_handover::HandoversOf as _;
use alondra_daemon_engine::domain_orchestrator_tools::*;
use alondra_engine_admission::AdmissionOf as _;

use crate::domain_store::HandoverProcess;
use alondra_daemon_engine::node_close::Answer;
use alondra_daemon_engine::node_push::testing::Node;
use alondra_daemon_engine::sdk_handover::BgSessions;
use alondra_daemon_engine_model::LaunchOptions;
use crate::sdk_owner::fixtures::SdkLaunches;
use crate::test_support::Directory;
use alondra_domain_rules::{AttachmentRef, Command, CommandEnvelope, ModelChooser, PROTOCOL_VERSION, TaskModel, TaskState};
use alondra_core::options::OptionsFile;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};


/// A task «Editor colours» of a request, a `claude --bg` node agent-b of the previous release, and
/// the project folder App. Its launches start through the SDK fixture when it has one.
pub struct Fixture {
    pub sdk: Option<SdkLaunches>,
    pub sessions: Arc<FakeSessions>,
    pub root: Directory,
    pub home: PathBuf,
    pub project: PathBuf,
    pub domain: Arc<DomainRuntime>,
}

pub fn command(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("test-{}", crate::auth::random_hex(8)).into(), expected_revision: state.revision, command,
    };
    domain.execute(&actor, &envelope, None, || Ok(())).unwrap();
}

pub fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}

/// A world whose launches start through the SDK fixture.
pub fn world() -> Fixture {
    let mut f = bare_world();
    f.sdk = Some(SdkLaunches::enable(&f.domain, &f.home));
    f
}

/// A world without an SDK backend, or with the one a test gives it.
pub fn bare_world() -> Fixture {
    let root = Directory::new();
    let home = root.join("home");
    let project = root.join("App");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&project).unwrap();
    let domain = Arc::new(crate::engine_fixtures::open(&home).unwrap());
    alondra_engine_admission::measure_machine(&domain, &alondra_core::Machine { at: 1, memory_total: 16 << 30,
        memory_available: 12 << 30, memory_free_percent: Some(75), cpus: 11,
        disk_total: 500_000_000_000, disk_available: 400_000_000_000, ..Default::default() }, alondra_engine_ports::disk::Space { total: 500_000_000_000, free: 400_000_000_000, purgeable: 0 });
    let sessions = Arc::new(FakeSessions::default());
    let _ = sessions.domain.set(Arc::downgrade(&domain));
    domain.handovers().use_sessions(sessions.clone() as Arc<dyn BgSessions>);
    *domain.handovers().turn_deadline.lock().unwrap() = Some(std::time::Duration::from_millis(20));
    command(&domain, Actor::System, Command::RegisterAgent { agent_id: "agent-b".into(), name: "Instance B".into(), at: 0 });
    command(&domain, user(), Command::CreateRequest { request_id: "r".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false });
    command(&domain, user(), Command::TitleRequest { request_id: "r".into(), title: "Readable editor".into() });
    command(&domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t".into(), request_id: "r".into(), title: "Editor colours".into(),
        agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")), });
    Fixture { sdk: None, sessions, root, home, project, domain }
}

impl Fixture {
    pub fn sdk(&self) -> &SdkLaunches {
        self.sdk.as_ref().expect("this world launches through the SDK")
    }

    /// Waits on the domain until the native Claudes got `prompts` prompts: the folder each started
    /// in, and every prompt.
    pub async fn launched(&self, prompts: usize) -> (Vec<PathBuf>, Vec<String>) {
        let mut changed = self.domain.subscribe();
        while self.sdk().prompts().len() < prompts {
            changed.changed().await.unwrap();
        }
        let folders = self.sdk().starts().iter().map(|start| PathBuf::from(start["cwd"].as_str().unwrap())).collect();
        (folders, self.sdk().prompts())
    }

    /// The prompt of the only launch.
    pub async fn prompt(&self) -> String {
        self.launched(1).await.1.remove(0)
    }

    pub fn task_node(&self) -> Option<AgentId> {
        self.domain.state().unwrap().tasks[&TaskId::from("t")].agent_id.clone()
    }
}

/// Claude Code's `claude stop` and `claude agents` for the `claude --bg` nodes being replaced.
#[derive(Default)]
pub struct FakeSessions {
    pub domain: OnceLock<Weak<DomainRuntime>>,
    pub stopped: Mutex<Vec<String>>,
    /// Why `claude stop` fails, when it does.
    pub stop_fails: Mutex<Option<String>>,
    pub stop_panics: AtomicBool,
    /// Whether the node was on a turn at each stop.
    pub busy_at_stop: Mutex<Vec<bool>>,
}

impl BgSessions for FakeSessions {
    fn listed<'a>(&'a self, _: &'a str) -> Answer<'a, Result<Option<HandoverProcess>, String>> {
        Box::pin(async { Ok(None) })
    }

    fn stop<'a>(&'a self, session: &'a str) -> Answer<'a, Result<(), String>> {
        Box::pin(async move {
            assert!(!self.stop_panics.load(Ordering::SeqCst), "claude stop panicked");
            if let Some(reason) = self.stop_fails.lock().unwrap().take() {
                return Err(reason);
            }
            self.stopped.lock().unwrap().push(session.into());
            let domain = self.domain.get().and_then(Weak::upgrade);
            if let Some(domain) = &domain {
                self.busy_at_stop.lock().unwrap().push(domain.live().node(session).is_some_and(|node| node.busy));
            }
            Ok(())
        })
    }

    fn alive(&self, _: &HandoverProcess) -> bool {
        false
    }
}

/// Claude and Codex connected, both signed in, each advertising its models; `twin` is in both catalogs.
pub fn connect_claude_and_codex(f: &Fixture) {
    use alondra_core::accounts::{AccountBilling, AccountKey, AccountState, AccountStatus, AccountsConfig, Provider};
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel};
    let mut config = AccountsConfig::fresh();
    config.connect(Provider::Claude);
    config.connect(Provider::Codex);
    let keys = vec![AccountKey::native(Provider::Claude), AccountKey::native(Provider::Codex)];
    config.set_order(keys.clone()).unwrap();
    std::fs::write(alondra_core::options::path_in(&f.home), config.to_toml()).unwrap();
    let statuses = keys.iter().map(|key| AccountStatus { provider: key.provider, id: key.account.clone(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None }).collect();
    command(&f.domain, Actor::System, Command::RecordAccounts { accounts: statuses, reserve: alondra_core::usage::Share::of(config.distribution.reserve) });
    command(&f.domain, Actor::System, Command::RecordAccountConfiguration { order: keys.clone(), version: alondra_core::accounts::config_version(&config) });
    let model = |name: &str| CatalogModel { model: name.into(), display_name: name.into(), reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
        default_reasoning_effort: Some("medium".into()), context_window: None, input_modalities: vec!["text".into()], is_default: false, hidden: false };
    for key in keys {
        let models = if key.provider == Provider::Claude { vec![model("opus"), model("sonnet"), model("twin")] } else { vec![model("gpt-6"), model("twin")] };
        command(&f.domain, Actor::System, Command::RecordModelCatalog { catalog: AccountModelCatalog { account: key, revision: "fixture".into(), native_default: None, measured_at: 100, models } });
    }
}

/// A plan an older daemon stored as the one that stopped “Triage legible en AX3”. Its start is
/// refused the same way every time, so it is never retried. The orchestrator gets it at once, the
/// user's inbox gets nothing, and the plan the orchestrator fixes starts the task. The test stores
/// it as task `t`'s straight in the journal, past the validation a new plan goes through.
pub fn refused_plan(f: &Fixture) -> alondra_daemon_engine_model::LaunchPlan {
    connect_claude_and_codex(f);
    alondra_daemon_engine_model::LaunchPlan { effort: Some("medium".into()), reason: Some("Layout fix".into()),
        ..alondra_daemon_engine_model::LaunchPlan::new("app/editor", f.project.to_str().unwrap()) }
}

pub async fn refuse_its_start(f: &Fixture) -> alondra_domain_rules::Interaction {
    crate::auto_launch::pass(&f.domain, &f.home).await.unwrap();
    assert!(f.sdk().starts().is_empty());
    assert!(f.domain.store.lock().unwrap().start_retries().unwrap().is_empty(), "no retry is promised for a start that cannot work");
    let state = f.domain.state().unwrap();
    let card = state.open_plan_refusal(&"t".into()).cloned().expect("the orchestrator is handed the refused plan at once");
    assert_eq!(card.recipient, alondra_domain_rules::Recipient::Orchestrator);
    assert!(card.body.contains("Choose a model before its native reasoning effort"), "{}", card.body);
    assert!(state.interactions.values().all(|i| i.recipient != alondra_domain_rules::Recipient::User
        || !matches!(&i.scope, alondra_domain_rules::Scope::Task { task_id } if task_id.0 == "t")), "nothing reaches the user's inbox");
    let messages = f.domain.store.lock().unwrap().pending_messages().unwrap();
    assert!(messages.iter().any(|m| m.recipient_kind == "orchestrator" && m.interaction_id.as_deref() == Some(card.id.0.as_str())),
        "the orchestrator hears of it in its next batch: {messages:?}");
    assert!(messages.iter().filter(|m| m.interaction_id.as_deref() == Some(card.id.0.as_str())).all(|m| m.recipient_kind == "orchestrator"), "{messages:?}");
    card
}
