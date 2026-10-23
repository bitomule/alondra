//! What a command costs the store when the domain holds many tasks with long journals.
use super::*;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;
use std::time::Instant;

const TASKS: usize = 200;
const DELIVERIES: usize = 10;
const LONG_DELIVERIES: usize = 1000;
const SAMPLES: usize = 21;

fn envelope(state: &DomainState, command_id: String, command: Command) -> CommandEnvelope {
    CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: command_id.into(),
        expected_revision: state.revision,
        command,
    }
}

fn run(store: &mut DomainStore, actor: &Actor, command: Command) {
    let state = store.load().unwrap();
    store.execute_authenticated(actor, &envelope(&state, format!("cost-{}", state.revision), command), None).unwrap();
}

fn task(t: usize) -> TaskId {
    format!("task-{t}").as_str().into()
}

fn node(t: usize) -> Actor {
    Actor::Agent { agent_id: format!("node-{t}").as_str().into() }
}

/// Task `t`'s node delivers again: its previous delivery goes back and this one is presented.
fn delivery(t: usize, n: usize) -> Command {
    Command::PresentResult {
        attachments: vec![], task_id: task(t), interaction_id: format!("delivery-{t}-{n}").as_str().into(), body: "Done".into(),
        is_final: true, nothing_to_review: false, targets: vec![DeliveryTarget::Text { text: "Done".into() }], checklist: vec![],
        card: CardSpec::delivery("Review the result", &[], true),
    }
}

/// `tasks` started tasks whose nodes delivered `deliveries` times each, the last one `long` times more.
fn seeded(root: &Path, tasks: usize, deliveries: usize, long: usize) -> DomainStore {
    std::fs::DirBuilder::new().mode(0o700).create(root).unwrap();
    let mut store = DomainStore::open(&root.join("alondra.db")).unwrap();
    run(&mut store, &Actor::User { user_id: "device:phone".into() }, Command::CreateRequest {
        request_id: "r-cost".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
    });
    run(&mut store, &Actor::Orchestrator { generation: 0 }, Command::TitleRequest { request_id: "r-cost".into(), title: "Cost".into() });
    for t in 0..tasks {
        let agent: AgentId = format!("node-{t}").as_str().into();
        run(&mut store, &Actor::System, Command::RegisterAgent { agent_id: agent.clone(), name: format!("Node {t}"), at: 0 });
        run(&mut store, &Actor::Orchestrator { generation: 0 }, Command::CreateTask {
            plan_items: None, task_id: task(t), request_id: "r-cost".into(), title: format!("Task {t}"), agent_id: Some(agent),
            review_prs: false, dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        run(&mut store, &node(t), Command::StartTask { task_id: task(t) });
        for n in 0..deliveries {
            run(&mut store, &node(t), delivery(t, n));
        }
    }
    for n in 0..long {
        run(&mut store, &node(tasks - 1), delivery(tasks - 1, deliveries + n));
    }
    store
}

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}

fn events_stored(store: &DomainStore, t: usize) -> i64 {
    store.conn.query_row("SELECT COUNT(*) FROM lifecycle_events WHERE task_id=?1", [task(t).0], |row| row.get(0)).unwrap()
}

/// One more delivery of task `t`: the events the domain and the store applied, the ones the
/// store decoded reading the task's journal from its rows, and the events it stored.
fn delivery_cost(store: &mut DomainStore, t: usize, n: usize) -> (u64, u64, i64) {
    let stored = events_stored(store, t);
    let (applied, decoded) = (alondra_domain::measure::applied(), alondra_store::measure::counts().event_decodes);
    run(store, &node(t), delivery(t, n));
    let decoded = alondra_store::measure::counts().event_decodes - decoded;
    (alondra_domain::measure::applied() - applied, decoded, events_stored(store, t) - stored)
}

/// The cost of one more delivery of task `t`, and the median times of the domain's transition
/// alone and of the whole commit.
fn measure(store: &mut DomainStore, t: usize) -> serde_json::Value {
    let journal = store.load().unwrap().lifecycle_journal[&task(t)].len();
    let (applied, decoded, stored) = delivery_cost(store, t, 1_000_000);
    let transition = median((0..SAMPLES).map(|n| {
        let state = store.load().unwrap();
        let envelope = envelope(&state, format!("dry-{n}"), delivery(t, 2_000_000 + n));
        let mut next = (*state).clone();
        let start = Instant::now();
        next.execute(&node(t), &envelope).unwrap();
        start.elapsed().as_secs_f64() * 1000.0
    }).collect());
    let commit = median((0..SAMPLES).map(|n| {
        let start = Instant::now();
        run(store, &node(t), delivery(t, 3_000_000 + n));
        start.elapsed().as_secs_f64() * 1000.0
    }).collect());
    serde_json::json!({"journal": journal, "events_applied": applied, "store_events_decoded": decoded, "events_stored": stored,
        "transition_median_ms": transition, "commit_median_ms": commit})
}

#[test]
#[ignore = "opt-in measurement of a command's cost over many long journals"]
fn measure_command_cost_over_many_journals() {
    let root = crate::test_support::Directory::new();
    let mut store = seeded(&root, TASKS, DELIVERIES, LONG_DELIVERIES);
    let state = store.load().unwrap();
    let records: usize = state.lifecycle_journal.iter().map(|(_, journal)| journal.len()).sum();
    let short = measure(&mut store, 0);
    let long = measure(&mut store, TASKS - 1);
    println!("{}", serde_json::json!({"tasks": state.tasks.len(), "records": records, "short_task": short, "long_task": long}));
}

#[test]
fn a_command_on_a_long_journal_writes_and_replays_only_its_new_records() {
    const LONG: usize = 150;
    let root = crate::test_support::Directory::new();
    let mut store = seeded(&root, 2, 2, LONG);
    let (short_applied, short_decoded, short_stored) = delivery_cost(&mut store, 0, 1_000_000);
    let (long_applied, long_decoded, long_stored) = delivery_cost(&mut store, 1, 1_000_000);
    assert_eq!(long_stored, short_stored, "a delivery stores the same events whatever its task's journal holds");
    assert_eq!(long_decoded, short_decoded, "a delivery decodes the same stored events whatever its task's journal holds");
    assert_eq!(long_applied - long_decoded, short_applied - short_decoded,
        "beyond the store reading the journal from its rows, a delivery to a task with {LONG} more records costs what it costs a short one");
    let state = store.load().unwrap();
    drop(store);
    let reopened = DomainStore::open(&root.join("alondra.db")).unwrap().load().unwrap();
    assert_eq!(reopened.lifecycle_journal, state.lifecycle_journal);
    assert_eq!(reopened.tasks, state.tasks);
}

#[test]
fn a_store_reads_what_another_store_of_the_same_database_committed() {
    let root = crate::test_support::Directory::new();
    let mut first = seeded(&root, 1, 2, 0);
    let mut second = DomainStore::open(&root.join("alondra.db")).unwrap();
    run(&mut second, &node(0), delivery(0, 10));
    run(&mut first, &node(0), delivery(0, 11));
    run(&mut second, &node(0), delivery(0, 12));
    run(&mut first, &node(0), delivery(0, 13));
    drop(first);
    run(&mut second, &node(0), delivery(0, 14));
    let mut reopened = DomainStore::open(&root.join("alondra.db")).unwrap();
    run(&mut reopened, &node(0), delivery(0, 15));
    let state = reopened.load().unwrap();
    drop(reopened);
    drop(second);
    assert_eq!(DomainStore::open(&root.join("alondra.db")).unwrap().load().unwrap().lifecycle_journal, state.lifecycle_journal);
}
