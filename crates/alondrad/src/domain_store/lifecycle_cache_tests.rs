//! What the daemon reads of each task's lifecycle is what a full replay of its journal gives,
//! whatever the store went through: a commit SQL refuses, the daemon opening the database again,
//! another connection writing to it.
use super::*;
use crate::domain_runtime::DomainRuntime;
use std::os::unix::fs::DirBuilderExt;

const TASK: &str = "t-cache";
const NODE: &str = "node-cache";

struct Home {
    runtime: DomainRuntime,
    root: crate::test_support::Directory,
}

fn opened() -> Home {
    let root = crate::test_support::Directory::new();
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let runtime = DomainRuntime::open(&root).unwrap();
    let home = Home { runtime, root };
    home.run(&Actor::User { user_id: "device:phone".into() }, Command::CreateRequest {
        request_id: "r-cache".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    }).unwrap();
    home.run(&Actor::System, Command::RegisterAgent { agent_id: NODE.into(), name: "Node".into(), at: 0 }).unwrap();
    home.run(&Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        plan_items: None, task_id: TASK.into(), request_id: "r-cache".into(), title: "Task".into(), agent_id: Some(NODE.into()),
        review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    }).unwrap();
    home.run(&node(), Command::StartTask { task_id: TASK.into() }).unwrap();
    home
}

impl Home {
    fn run(&self, actor: &Actor, command: Command) -> std::result::Result<CommandOutcome, StoreError> {
        run_on(&self.runtime, actor, command)
    }
}

fn run_on(runtime: &DomainRuntime, actor: &Actor, command: Command) -> std::result::Result<CommandOutcome, StoreError> {
    let state = runtime.state().unwrap();
    runtime.execute(actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("cache-{}", state.revision).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(()))
}

fn node() -> Actor {
    Actor::Agent { agent_id: NODE.into() }
}

fn present(id: &str) -> Command {
    let targets = vec![DeliveryTarget::Text { text: "Done".into() }];
    Command::PresentResult {
        attachments: vec![], task_id: TASK.into(), interaction_id: id.into(), body: "Done".into(), is_final: true, nothing_to_review: false,
        targets, checklist: vec![], card: CardSpec::delivery("Review the result", &[], true),
    }
}

fn records(state: &DomainState) -> usize {
    state.lifecycle_journal[&TaskId::from(TASK)].len()
}

/// The state readers get and the one the store goes on from both keep the lifecycles their
/// journals replay to.
fn assert_kept(runtime: &DomainRuntime) {
    let published = runtime.read();
    published.state().kept_lifecycles_match_journals().unwrap_or_else(|divergence| panic!("the published state: {divergence}"));
    let stored = runtime.store.lock().unwrap().load().unwrap();
    stored.kept_lifecycles_match_journals().unwrap_or_else(|divergence| panic!("the store's state: {divergence}"));
}

#[test]
fn a_commit_sql_refuses_leaves_the_kept_lifecycles_of_the_state_before_it() {
    let home = opened();
    assert_kept(&home.runtime);
    let before = home.runtime.read();
    home.runtime.store.lock().unwrap().conn
        .execute_batch("CREATE TRIGGER fail_journal BEFORE INSERT ON lifecycle_events BEGIN SELECT RAISE(ABORT,'injected journal failure'); END;")
        .unwrap();
    let refused = home.run(&node(), present("delivery-refused")).unwrap_err();
    assert!(format!("{refused:?}").contains("injected journal failure"), "the journal write failed: {refused:?}");
    let after = home.runtime.read();
    assert_eq!(after.state().revision, before.state().revision, "nothing was committed");
    assert_eq!(records(after.state()), records(before.state()));
    assert_kept(&home.runtime);
    home.runtime.store.lock().unwrap().conn.execute_batch("DROP TRIGGER fail_journal").unwrap();
    home.run(&node(), present("delivery-kept")).unwrap();
    assert_eq!(records(home.runtime.read().state()), records(before.state()) + 1, "the next commit journals its decision");
    assert_kept(&home.runtime);
}

#[test]
fn the_daemon_opening_the_database_again_keeps_the_lifecycles_its_journals_replay_to() {
    let home = opened();
    home.run(&node(), present("delivery-before-restart")).unwrap();
    let journal = records(home.runtime.read().state());
    let Home { runtime, root } = home;
    drop(runtime);
    let runtime = DomainRuntime::open(&root).unwrap();
    assert_eq!(records(runtime.read().state()), journal);
    assert_kept(&runtime);
    run_on(&runtime, &Actor::User { user_id: "device:phone".into() }, Command::CancelTask { task_id: TASK.into() }).unwrap();
    assert!(records(runtime.read().state()) > journal, "the cancellation is journaled");
    assert_kept(&runtime);
    drop(runtime);
    drop(root);
}

#[test]
fn a_write_from_another_connection_is_read_with_the_lifecycles_its_journals_replay_to() {
    let home = opened();
    let other = DomainRuntime::open(&home.root).unwrap();
    let journal = records(home.runtime.read().state());
    run_on(&other, &node(), present("delivery-elsewhere")).unwrap();
    assert_kept(&other);
    let state = home.runtime.state().unwrap();
    home.runtime.execute_on_current(&CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "cache-after".into(), expected_revision: 0,
        command: Command::RegisterAgent { agent_id: "node-after".into(), name: "After".into(), at: 1 },
    }).unwrap();
    assert_eq!(records(home.runtime.read().state()), journal + 1, "this daemon reads the other connection's decision");
    assert_kept(&home.runtime);
    drop(other);
}
