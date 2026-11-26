//! The admission gate together with the engine it asks: the node transport's watch samples the
//! Mac off the async worker, a node the user paused runs no tool, the start facts read while the
//! gate hands old pauses over, a home renamed by an older daemon, Settings, and usage readings.
//! The gate's own decisions are tested in engine-admission, against doubles of the engine.
use alondra_daemon_engine::domain_transport::PAUSED_BY_USER;
use alondra_daemon_engine::domain_transport::fixtures::*;
use alondra_daemon_engine::gate::GateOf as _;
use alondra_daemon_engine::live::LiveOf as _;
use alondra_engine_admission::AdmissionOf as _;
use alondra_engine_admission::admission::{FILE, Key, PREVIOUS_FILE, Tuning};
use alondra_engine_admission::pauses::Reason;
use alondra_engine_admission::machine_watch;
use alondra_core::{Machine, Millis};
use alondra_domain_model::{Actor, AgentId, Command, CommandEnvelope, PROTOCOL_VERSION, RuntimeObservation};
use crate::domain_runtime::DomainRuntime;
use serde_json::json;
use std::sync::Arc;

const MIN: Millis = 60_000;

#[test]
fn runtime_progress_machine_watch_persists_off_the_async_worker() {
    super::wired();
    let fixture = Fixture::new();
    let domain = fixture.open_domain();
    domain.machine_watch().record_sample_threads();
    let driver = std::thread::current().id();
    let gauges = Arc::new(std::sync::Mutex::new(Some((Machine {
        at: 1_700_000_000_000, memory_total: 100, memory_available: 50, memory_free_percent: Some(50),
        disk_total: 500_000_000_000, disk_available: 400_000_000_000, cpus: 8, ..Default::default()
    }, None, vec![]))));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let watcher = tokio::spawn(machine_watch::run(domain.clone(), gauges, Silent));
        domain.machine_watch().sampled.notified().await;
        let threads = domain.machine_watch().sample_threads();
        watcher.abort();
        assert!(watcher.await.unwrap_err().is_cancelled());
        assert!(!threads.is_empty());
        assert!(threads.iter().all(|thread| *thread != driver), "machine file sampling occupied the async worker: {threads:?}");
        assert!(std::fs::read_dir(fixture.0.join(crate::machine_state::SAMPLES_DIR)).unwrap().next().is_some());
    });
}

/// Whatever starts a turn of a node the user paused (a background command ending, a queued
/// message), it may run no tool, Alondra's included, and its turn end is not held back.
#[tokio::test]
async fn a_node_the_user_paused_runs_no_tool_and_may_end_its_turn() {
    super::wired();
    let fixture = Fixture::new();
    let domain = fixture.open_domain();
    node_with_task(&(domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>));
    domain.gate().pause_by_user("agent-a", alondra_core::now_millis()).unwrap();
    for tool in ["Bash", "mcp__plugin_alondra_alondra__present_result"] {
        let (reply, result) = hook(&(domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": tool}), Some(6)).await;
        result.unwrap();
        assert_eq!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{tool}: {reply}");
        assert_eq!(reply["hookSpecificOutput"]["permissionDecisionReason"], PAUSED_BY_USER);
    }
    assert_eq!(hook(&(domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0, json!({}));

    domain.gate().release_by_user_message("agent-a");
    let (reply, _) = hook(&(domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), json!({"session_id": "agent-a", "hook_event_name": "PreToolUse", "tool_name": "Bash"}), Some(6)).await;
    assert_ne!(reply["hookSpecificOutput"]["permissionDecision"], "deny", "{reply}");
    assert_eq!(hook(&(domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), json!({"session_id": "agent-a", "hook_event_name": "Stop"}), Some(6)).await.0["decision"], "block");
}

struct Home(crate::test_support::Directory);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn home() -> Home {
    let home = crate::test_support::Directory::new();
    std::fs::create_dir_all(&home).unwrap();
    Home(home)
}

fn open(home: &std::path::Path) -> Arc<DomainRuntime> {
    super::wired();
    Arc::new(alondra_daemon_engine::wiring::open(home).unwrap())
}

fn run(domain: &DomainRuntime, actor: Actor, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: crate::auth::random_hex(8).into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn world() -> (Home, Arc<DomainRuntime>) {
    let home = home();
    let domain = open(&home.0);
    alondra_engine_admission::measure_machine(&*domain, &Machine { at: 1, memory_total: 16 << 30, memory_available: 12 << 30,
        memory_free_percent: Some(75), cpus: 11, ..Machine::default() }, alondra_engine_ports::disk::Space { total: 500 << 30, free: 100 << 30, purgeable: 0 });
    run(&domain, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    (home, domain)
}

fn create(domain: &DomainRuntime, id: &str) {
    run(domain, Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
        task_id: id.into(), request_id: "r".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None,
        launch: Some(alondra_domain_model::LaunchPlan::new("node", "/tmp")),
    });
}

/// 0.2.64 hung on its first start after an upgrade from 0.2.63 with paused nodes: reading the
/// start facts held the domain store while the gate handed the old pauses to the node machine,
/// which needs that store too.
#[test]
fn reading_start_facts_hands_pauses_an_older_daemon_saved_without_holding_the_store() {
    let home = home();
    std::fs::write(home.0.join(FILE), json!({
        "learned": {"cap": null, "since": null}, "last_admission": null,
        "pauses": {
            "step": {"step": 180_000, "last_resume": null, "saturated_at": null}, "last_pause": 1, "owed": {}, "held": {},
            "paused": {"agent-t1": {"task": "t1", "reason": {"kind": "memory"}, "since": 1, "cleanup": "done", "released": false}},
        },
    }).to_string()).unwrap();
    let domain = open(&home.0);
    run(&domain, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    create(&domain, "t1");
    run(&domain, Actor::System, Command::RegisterAgent { agent_id: AgentId("agent-t1".into()), name: "agent-t1".into(), at: 0 });
    run(&domain, Actor::User { user_id: "u".into() }, Command::AssignTask { task_id: "t1".into(), agent_id: AgentId("agent-t1".into()), model: None });
    assert!(domain.admission().holds_old_pauses(), "the old pause is still waiting to be handed over");
    let (sent, received) = std::sync::mpsc::channel();
    let reading = Arc::clone(&domain);
    std::thread::spawn(move || {
        let state = reading.state().unwrap();
        sent.send(alondra_daemon_engine::start_failures::facts(&*reading, &state).is_ok()).ok();
    });
    assert_eq!(received.recv_timeout(std::time::Duration::from_secs(5)), Ok(true), "reading the start facts deadlocked");
}

#[test]
fn a_home_with_the_previous_gate_file_starts_with_its_pauses_under_the_new_name() {
    let home = home();
    {
        let domain = open(&home.0);
        run(&domain, Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        create(&domain, "t-held");
    }
    std::fs::write(home.0.join(PREVIOUS_FILE), json!({
        "last_admission": null,
        "pauses": {"paused": {}, "owed": {}, "resumed": {}, "held": {"t-held": {"body": {}, "origin": "orchestrator", "since": 1}}},
    }).to_string()).unwrap();
    std::fs::write(home.0.join("fleet-gate.next"), "{").unwrap();

    alondra_daemon_engine::home_renames::apply(&home.0, &[], &alondra_store_sqlite::Sqlite).unwrap();

    assert!(!home.0.join(PREVIOUS_FILE).exists() && !home.0.join("fleet-gate.next").exists());
    assert!(home.0.join(FILE).exists());
    let domain = open(&home.0);
    domain.gate().pauses();
    assert!(domain.gate().held_by_user(&"t-held".into()), "the task the user paused in the queue stays paused");
}

/// What Settings offers is what the gate applies: automatic, from the node cost the Mac measures,
/// and the most this Mac takes, also for a number over it in `options.toml`.
#[test]
fn settings_offer_the_automatic_limit_the_gate_applies_and_the_most_it_takes() {
    use alondra_core::options::NodeLimits;
    let (cores, memory) = (11, 16 << 30);
    let most = NodeLimits::of(cores, memory, 0).most;
    let over_4_gb = memory.saturating_sub(4 << 30);
    for (cost, auto) in [(over_4_gb / 2, 2), (128 << 20, NodeLimits::of(cores, memory, 128 << 20).auto)] {
        let (home, domain) = world();
        domain.admission().tune(Tuning { spacing: 0, limit: None });
        let now = alondra_core::now_millis();
        domain.machine_watch().measure_nodes(now - MIN, vec![cost; 30]);
        let pacing = domain.admission().live(&*domain, now).unwrap();
        assert_eq!((pacing.fixed, pacing.auto, pacing.limit, pacing.most), (None, auto, auto, most), "{cost} bytes a node");

        let state = domain.state().unwrap();
        let index = alondra_domain_model::ReadIndex::build(&state);
        let live = alondra_view_model::LiveSnapshot { pacing: Some(pacing), ..Default::default() };
        let context = alondra_views::RenderContext { domain: &state, index: &index, live: &live, client: alondra_view_model::Client::Ios,
            lang: alondra_view_model::Lang::En, transport: alondra_view_model::Transport::Loopback, cursor: "c", rendered_at: "" };
        let view = alondra_views::build("machine", context).unwrap();
        let menu = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            alondra_view_model::Item::Row(row) => row.actions.iter().find(|action| action.id.starts_with("set_limit:")).cloned(),
            _ => None,
        }).expect("Settings offer the limit");
        let ids: Vec<&str> = menu.options.iter().map(|option| option.id.as_str()).collect();
        let offered: Vec<String> = std::iter::once("auto".to_string()).chain((1..=most).map(|count| count.to_string())).collect();
        assert_eq!(ids, offered);
        assert_eq!(menu.options[0].label, format!("Automatic ({auto}) · now"));

        std::fs::write(alondra_core::options::path_in(&home.0), format!("max_working_nodes = {}\n", most + 5)).unwrap();
        let pacing = domain.admission().live(&*domain, now).unwrap();
        assert_eq!((pacing.fixed, pacing.limit), (Some(most + 5), most), "a number over the most works as the most");
    }
}

#[test]
fn another_providers_capacity_does_not_admit_a_usage_paused_node() {
    use alondra_core::accounts::{AccountId, AccountKey, AccountState, AccountStatus, Provider};
    use alondra_core::usage::{Share, UsageReading, WindowKind, WindowReading};
    let (_home, domain) = world();
    domain.admission().ignore_mac();
    create(&domain, "t1");
    run(&domain, Actor::System, Command::RegisterAgent { agent_id: AgentId("agent-t1".into()), name: "agent-t1".into(), at: 0 });
    crate::domain_store::admit_through_sdk(&domain, &"t1".into(), "agent-t1");
    let input = domain.state().unwrap().lifecycle_of(&"t1".into()).unwrap().pending_inputs()[0].as_str().to_owned();
    run(&domain, Actor::System, Command::RecordRuntimeFacts { task_id: "t1".into(), generation: 1, facts: vec![
        RuntimeObservation::ResourceAvailable, RuntimeObservation::TurnStarted { turn: "turn-t1".into(), input },
    ] });
    domain.report(alondra_daemon_engine::node_reports::Envelope { session_id: "agent-t1".into(), report: alondra_daemon_engine::node_reports::Report::TurnStart }, 1_000);
    let status = |provider| AccountStatus { antigravity: None, provider, id: AccountId::default_account(), name: "Personal".into(), state: AccountState::SignedIn,
        native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0, profile: None };
    run(&domain, Actor::System, Command::RecordAccounts { accounts: vec![status(Provider::Claude), status(Provider::Codex)], reserve: alondra_core::usage::default_reserve() });
    let record = |provider, windows| alondra_daemon_engine::usage_probe::record_for_provider(&*domain, &AccountKey::new(provider, AccountId::default_account()),
        UsageReading { windows, plan: None, complete: true }, 1_000);
    record(Provider::Claude, vec![WindowReading { kind: WindowKind::Session, used: Share::FULL, resets_at: Some(9_000), exhausted: true }]);
    record(Provider::Codex, vec![
        WindowReading { kind: WindowKind::Other("codex:account:primary:300".into()), used: Share::of(0.2), resets_at: Some(9_000), exhausted: false },
        WindowReading { kind: WindowKind::Other("codex:account:secondary:10080".into()), used: Share::of(0.2), resets_at: Some(9_000), exhausted: false },
    ]);
    domain.live().usage_pause.resume(2_000);
    assert!(domain.admission().hold_back(&*domain, "agent-t1", Reason::Usage, 2_000));
    assert!(!domain.admission().snapshot(&*domain, 2_000).unwrap().queue.iter().any(|waiting| waiting.key == Key::Resume("agent-t1".into())),
        "global Codex capacity cannot release this Claude account");
    assert!(domain.admission().snapshot(&*domain, 9_000).unwrap().queue.iter().any(|waiting| waiting.key == Key::Resume("agent-t1".into())),
        "its own declared reset admits the same node");
}
