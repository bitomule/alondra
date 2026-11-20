//! What the tests of the engine's crates build on: narrow access to the store's internals and the
//! domain databases they start from.
use super::*;
use std::collections::BTreeSet;
use std::path::PathBuf;
pub use super::owner::ObservationRefusal;
pub const DAY_MS: i64 = super::metrics_fold::DAY_MS;
pub const WINDOW_MS: i64 = super::retention::WINDOW_MS;
pub const AUDIT_BOUND_TO_RECEIPTS: &str = super::retention::AUDIT_BOUND_TO_RECEIPTS;
pub fn summary(json: &str) -> String { super::retention::summary(json) }
pub fn load(connection: &Connection) -> Result<DomainState> { super::repository::load(connection) }
pub fn save_journal<'a>(journal: &mut alondra_store::LifecycleTransaction<'_>, task: &TaskId,
    records: impl IntoIterator<Item = &'a LifecycleRecord>, known: usize) -> Result<()> {
    super::repository::save_journal(journal, task, records, known)
}
pub fn samples_of_folded_tasks(tx: &rusqlite::Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    super::metrics_fold::samples_of_folded_tasks(tx, now, limit)
}
pub fn idle_sessions_without_task(tx: &rusqlite::Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    super::metrics_fold::idle_sessions_without_task(tx, now, limit)
}
pub fn tasks_into_weeks(tx: &rusqlite::Transaction<'_>, now: i64, limit: i64) -> rusqlite::Result<usize> {
    super::metrics_fold::tasks_into_weeks(tx, now, limit)
}
impl DomainStore {
    pub fn cached_state(&self) -> Option<(&Arc<DomainState>, u64)> {
        self.cached.as_ref().map(|cached| (&cached.state, cached.total_changes))
    }
}

thread_local! {
    pub(crate) static STORE_PORTS: std::cell::Cell<Option<fn(&alondra_engine_ports::Registry)>> = const { std::cell::Cell::new(None) };
}

/// What the stores this thread opens check worktrees with until a runtime protects them: a test of
/// a crate above the store registers its doubles of the Mac.
pub fn wire_stores_with(wire: fn(&alondra_engine_ports::Registry)) {
    STORE_PORTS.with(|ports| ports.set(Some(wire)));
}

/// A domain database of a test's own, in a folder removed with it.
pub struct Fixture(pub PathBuf, pub tempfile::TempDir);
impl Fixture {
    pub fn new() -> Self {
        let owner = tempfile::Builder::new().prefix("at-").tempdir().expect("a folder for the domain database");
        let home = owner.path().canonicalize().expect("the domain database's folder").join("work");
        std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(&home).expect("the domain database's home");
        Self(home, owner)
    }
    pub fn path(&self) -> PathBuf {
        self.0.join("domain.sqlite")
    }
}

pub fn agent() -> Actor {
    Actor::Agent {
        agent_id: "agent-1".into(),
    }
}
pub fn envelope(store: &mut DomainStore, id: &str, command: Command) -> CommandEnvelope {
    let s = store.load().unwrap();
    CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: s.revision,
        command,
    }
}
pub fn execute(store: &mut DomainStore, actor: &Actor, id: &str, command: Command) -> Commit {
    let e = envelope(store, id, command);
    let committed = store.execute_authenticated(actor, &e, None).unwrap();
    assert_eq!(
        store.load().unwrap(),
        committed.state,
        "SQL roundtrip for {id}"
    );
    committed
}
pub fn seeded(store: &mut DomainStore) {
    execute(
        store,
        &Actor::Orchestrator { generation: 0 },
        "request",
        Command::CreateRequest {
            request_id: "request-1".into(),
            body: "Do useful work".into(), auto_merge: false, maintenance: false, message: false,
        },
    );
    execute(
        store,
        &Actor::System,
        "agent",
        Command::RegisterAgent {
            agent_id: "agent-1".into(),
            name: "Fixture".into(), at: 0
        },
    );
    execute(
        store,
        &Actor::Orchestrator { generation: 0 },
        "task",
        Command::CreateTask { plan_items: None, continues: None,
            task_id: "task-1".into(),
            request_id: "request-1".into(),
            title: "Goal".into(),
            agent_id: Some("agent-1".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        },
    );
}
/// A task with an approved checklist and a worktree at `worktree`, and the command that continues it.
pub fn continuation_fixture(worktree: &Path) -> (Fixture, DomainStore, CommandEnvelope, Checklist) {
    let fixture = Fixture::new();
    let mut store = DomainStore::open(&fixture.path()).unwrap();
    seeded(&mut store);
    execute(&mut store, &Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest {
        request_id: "request-1".into(), title: "The answer".into(),
    });
    let path = worktree.to_str().unwrap();
    execute(&mut store, &Actor::System, "worktree", Command::RecordTaskWorktree {
        task_id: "task-1".into(), path: path.into(), origin: WorktreeOrigin::Task, branch: Some("worktree-original".into()),
    });
    let plan = "- [ ] Build the answer\n- [ ] Check the answer\n";
    execute(&mut store, &Actor::System, "plan", Command::HoldHook {
        interaction_id: "plan".into(), task_id: "task-1".into(), agent_id: "agent-1".into(), hold: Hold::Plan,
        body: plan.into(), options: Hold::Plan.options(), card: CardSpec::new(CardKind::Review, "Plan").content(ContentKind::Plan, plan),
    });
    execute(&mut store, &Actor::User { user_id: "user-1".into() }, "approve", Command::AnswerQuestion {
        attachments: vec![], interaction_id: "plan".into(), option_id: Hold::APPROVE.into(), inputs: Default::default(),
    });
    let checklist = store.load().unwrap().checklists[&TaskId::from("task-1")].clone();
    let command = envelope(&mut store, "continue", Command::CreateTask {
        task_id: "continued".into(), request_id: "request-1".into(), title: "Continue the answer".into(), agent_id: None,
        review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", path)),
        plan_items: None, continues: Some("task-1".into()),
    });
    (fixture, store, command, checklist)
}
