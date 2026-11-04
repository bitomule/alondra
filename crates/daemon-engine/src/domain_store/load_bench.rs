//! Seeds the production-shaped domain that `make load-bench` (tools/load_bench.py) measures.
use super::*;
use std::collections::BTreeSet;

pub const REQUESTS: usize = 20;
pub const TASKS: usize = 115;
pub const INTERACTIONS: usize = 669;
pub const AGENTS: usize = 97;

fn run(store: &mut DomainStore, actor: &Actor, id: String, command: Command) {
    let state = store.load().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: state.revision,
        command,
    };
    store.execute_authenticated(actor, &envelope, None).unwrap();
}

fn body(i: usize) -> String {
    "The node reports what it changed, what it measured and what is left to decide. "
        .repeat(15 + (i * 37) % 90)
}

#[test]
#[ignore = "run by make load-bench with ALONDRA_LOAD_BENCH_DB"]
fn seed_load_bench() {
    let path = std::env::var("ALONDRA_LOAD_BENCH_DB").expect("ALONDRA_LOAD_BENCH_DB");
    let nodes: usize = std::env::var("ALONDRA_LOAD_BENCH_NODES").map_or(20, |n| n.parse().unwrap());
    let mut store = DomainStore::open(Path::new(&path)).unwrap();
    seed(&mut store, nodes);
}

pub fn seed(store: &mut DomainStore, nodes: usize) {
    let user = Actor::User { user_id: "user-1".into() };
    for r in 0..REQUESTS {
        run(store, &Actor::Orchestrator { generation: 0 }, format!("request-{r}"), Command::CreateRequest {
            request_id: format!("request-{r}").into(),
            body: body(r),
            auto_merge: false, maintenance: false, message: false,
        });
        run(store, &user, format!("title-{r}"), Command::TitleRequest {
            request_id: format!("request-{r}").into(),
            title: format!("Bench request {r}"),
        });
    }
    for n in 0..nodes.max(AGENTS) {
        run(store, &Actor::System, format!("agent-{n}"), Command::RegisterAgent {
            agent_id: format!("bench-node-{n}").into(),
            name: format!("Bench node {n}"), at: 0
        });
    }
    for t in 0..TASKS {
        run(store, &Actor::Orchestrator { generation: 0 }, format!("task-{t}"), Command::CreateTask { plan_items: None, continues: None,
            task_id: format!("task-{t}").into(),
            request_id: format!("request-{}", t % REQUESTS).into(),
            title: format!("Bench task {t}"),
            agent_id: (t < nodes.max(AGENTS)).then(|| format!("bench-node-{t}").into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
    }
    for i in 0..INTERACTIONS {
        run(store, &Actor::System, format!("info-{i}"), Command::PublishInformation {
            interaction_id: format!("info-{i}").into(),
            scope: Scope::Task { task_id: format!("task-{}", i % TASKS).into() },
            recipient: Recipient::User,
            body: body(i),
            attachments: vec![],
            card: CardSpec::derived(CardKind::Know, "Read the update"),
        });
    }
    store.conn.execute("UPDATE domain_outbox SET state='handed'", []).unwrap();
    let state = store.load().unwrap();
    assert_eq!((state.tasks.len(), state.interactions.len()), (TASKS, INTERACTIONS));
}

pub fn distribution(samples: &[f64]) -> serde_json::Value {
    let mut sorted = samples.to_vec();
    sorted.sort_by(f64::total_cmp);
    if sorted.is_empty() { return serde_json::json!({"n": 0}); }
    let pick = |q: f64| sorted[(q * sorted.len() as f64).ceil() as usize - 1];
    serde_json::json!({"n": sorted.len(), "p50_ms": pick(0.5), "p95_ms": pick(0.95),
        "max_ms": sorted.last(), "samples_ms": samples})
}

pub fn process_resources() -> (f64, u64) {
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) }, 0);
    let millis = |time: libc::timeval| time.tv_sec as f64 * 1000.0 + time.tv_usec as f64 / 1000.0;
    let rss_units = if cfg!(target_os = "macos") { 1 } else { 1024 };
    (millis(usage.ru_utime) + millis(usage.ru_stime), usage.ru_maxrss as u64 * rss_units)
}


