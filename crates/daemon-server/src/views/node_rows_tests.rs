use super::*;
use crate::views::observation::{Serialization, Totals};
use alondra_domain_rules::{Actor, Agent, Command, CommandEnvelope, NodePhase, Task, PROTOCOL_VERSION};
use serde_json::json;

const NOW: i64 = 1_800_000_000_000;

struct Fixture {
    client: Arc<Client>,
    read: Arc<ReadSnapshot>,
    observation: Observation,
    // Keep the fixture directory until the client, its read workers and database have closed.
    _home: crate::test_support::Directory,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(alondra_engine_sdk::engine_tests::open(&home).unwrap());
        let state = domain.read();
        domain.execute(&Actor::Orchestrator { generation: 0 }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.state().data_epoch.clone(),
            command_id: "node-row-request".into(), expected_revision: state.state().revision,
            command: Command::CreateRequest { request_id: "row-request".into(), body: "View fixture".into(),
                auto_merge: false, maintenance: false, message: false },
        }, None, || Ok(())).unwrap();
        let observation = Observation::armed();
        observation.start();
        // No Tokio runtime on this thread: the client cannot spawn its periodic sampler.
        let client = Client::open_observed(domain.clone(), &home, Arc::new(Mutex::new(None)), observation.clone());
        let mut state = (**domain.read().state()).clone();
        state.agents.clear();
        for index in 0..count {
            let id = format!("node-{index:05}");
            state.agents.insert(id.as_str().into(), Agent {
                id: id.as_str().into(), name: format!("Historical node {index}"), phase: NodePhase::Closed, since: 0,
            });
            domain.live().mods.apply(serde_json::from_value(json!({
                "session_id": id, "event": "session.start", "version": "fixture"
            })).unwrap(), NOW - 10_000);
            domain.live().mods.apply(serde_json::from_value(json!({
                "session_id": id, "event": "session.end"
            })).unwrap(), NOW - 9_000);
        }
        if count > 0 {
            let task: Task = serde_json::from_value(json!({
                "id": "row-task", "request_id": "row-request", "agent_id": "node-00000",
                "title": "Finished work", "state": "done", "revision": 1, "started": true,
                "review_prs": false, "final_presentation": null, "pr_id": null,
                "dependencies": [], "stack": null, "documents": {}, "archived": false
            })).unwrap();
            state.tasks.insert(task.id.clone(), task);
        }
        Self { client, read: Arc::new(ReadSnapshot::build(Arc::new(state))), observation, _home: home }
    }

    fn inputs(&self) -> (Launches, Option<crate::domain_store::ReadCursor>) {
        let store = self.client.domain.live_read();
        let boundary = Some(store.read_cursor().unwrap());
        let launches = store.request_launch_summaries().unwrap();
        (launches, boundary)
    }

    fn gather(&self, cache: &Cache, lang: Lang) -> Nodes {
        let (launches, boundary) = self.inputs();
        cache.gather(&self.client, &self.read, lang, &launches, boundary, true, &None, NOW)
    }

    fn delta(&self, before: &Totals, metric: Metric) -> u64 {
        self.observation.totals().metrics[metric as usize] - before.metrics[metric as usize]
    }

    fn result(&self, id: &str, text: &str) {
        self.client.domain.live().mods.apply_sdk(id, &json!({"type": "result", "result": text}), NOW - 1_000);
    }
}

#[test]
fn stable_sources_reuse_all_historical_rows_and_one_report_changes_one_row() {
    for count in [32, 4096] {
        let fixture = Fixture::new(count);
        let cache = Cache::default();
        let initial = fixture.gather(&cache, Lang::En);
        assert_eq!(initial.len(), count, "ended reports retain historical rows");
        let digest = initial.digest(&fixture.observation, Serialization::Digest);
        let before = fixture.observation.totals();
        for _ in 0..3 {
            let unchanged = fixture.gather(&cache, Lang::En);
            assert_eq!(unchanged.digest(&fixture.observation, Serialization::Digest), digest);
        }
        for metric in [Metric::AgentRows, Metric::NodeOutputRows, Metric::NodeLeafSerializations,
            Metric::HashOperations, Metric::HashBytes, Metric::NodeTreeVisits, Metric::NodeTreeCopies,
            Metric::NodeLeafComparisons, Metric::MaterializedNodeRows] {
            assert_eq!(fixture.delta(&before, metric), 0, "stable sources must do no row work");
        }
        let id = format!("node-{:05}", count / 2);
        fixture.result(&id, "The final report changed.");
        let before = fixture.observation.totals();
        let changed = fixture.gather(&cache, Lang::En);
        assert_ne!(changed.digest(&fixture.observation, Serialization::Digest), digest);
        assert_eq!(fixture.delta(&before, Metric::AgentRows), 1);
        assert_eq!(fixture.delta(&before, Metric::NodeOutputRows), 1);
        assert_eq!(fixture.delta(&before, Metric::NodeLeafSerializations), 1);
        assert!(fixture.delta(&before, Metric::HashOperations) <= 65);
        assert!(fixture.delta(&before, Metric::NodeTreeCopies) <= 64);
        assert_eq!(changed.get(&id).unwrap().last_message.as_deref(), Some("The final report changed."));
        assert_eq!(initial.get(&id).unwrap().last_message, None, "the published snapshot is immutable");
    }
}

#[test]
fn coalesced_reports_reach_both_language_caches_without_crossing_languages() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    let english = fixture.gather(&cache, Lang::En);
    let spanish = fixture.gather(&cache, Lang::Es);
    assert_eq!(english.get("node-00000").unwrap().state_label.as_deref(), Some("idle"));
    assert_eq!(spanish.get("node-00000").unwrap().state_label.as_deref(), Some("inactivo"));
    fixture.result("node-00001", "First changed report.");
    fixture.result("node-00002", "Second changed report.");
    for lang in [Lang::Es, Lang::En] {
        let before = fixture.observation.totals();
        let changed = fixture.gather(&cache, lang);
        assert_eq!(fixture.delta(&before, Metric::AgentRows), 2);
        assert_eq!(changed.get("node-00001").unwrap().last_message.as_deref(), Some("First changed report."));
        assert_eq!(changed.get("node-00002").unwrap().last_message.as_deref(), Some("Second changed report."));
        assert_eq!(changed.get("node-00000").unwrap().state_label.as_deref(), Some(if lang == Lang::Es { "inactivo" } else { "idle" }));
    }
}

#[test]
fn a_context_change_invalidates_its_historical_task_owner_only() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    let initial = fixture.gather(&cache, Lang::En);
    let digest = initial.digest(&fixture.observation, Serialization::Digest);
    fixture.client.domain.contexts().handed(&"row-task".into(), NOW);
    let before = fixture.observation.totals();
    let changed = fixture.gather(&cache, Lang::En);
    assert_eq!(fixture.delta(&before, Metric::AgentRows), 1);
    assert_eq!(changed.digest(&fixture.observation, Serialization::Digest), digest,
        "handing an input changes the context source but has no new visible token count");
    assert_eq!(fixture.delta(&before, Metric::NodeLeafSerializations), 0);
    fixture.client.domain.contexts().forget(&"row-task".into());
    let before = fixture.observation.totals();
    fixture.gather(&cache, Lang::En);
    assert_eq!(fixture.delta(&before, Metric::AgentRows), 1);
}

#[test]
fn a_committed_launch_project_invalidates_rows_without_a_domain_revision_change() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    let initial = fixture.gather(&cache, Lang::En);
    let revision = fixture.client.domain.read().state().revision;
    // Insert the previously absent launch in a real SQLite commit. This table is immutable;
    // the test neither drops its triggers nor pretends a setter announces affected nodes.
    fixture.client.domain.store.lock().unwrap().conn.execute(
        "INSERT INTO request_launch(request_id,project,options,attachments) VALUES(?1,?2,'{}','[]')",
        ["row-request", "Project Cedar"],
    ).unwrap();
    assert_eq!(fixture.client.domain.read().state().revision, revision);
    let before = fixture.observation.totals();
    let changed = fixture.gather(&cache, Lang::En);
    assert_eq!(fixture.delta(&before, Metric::AgentRows), 32, "changed SQL dependencies use the complete fallback");
    assert_eq!(changed.get("node-00000").unwrap().project.as_deref(), Some("Project Cedar"));
    assert_ne!(initial.get("node-00000").unwrap().project, changed.get("node-00000").unwrap().project);
}

#[test]
fn a_hook_between_construction_and_retention_cannot_advance_the_cached_boundary() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    fixture.gather(&cache, Lang::En);
    let (launches, boundary) = fixture.inputs();
    let before = fixture.observation.totals();
    let raced = cache.gather_checked(&fixture.client, &fixture.read, Lang::En, &launches, boundary,
        true, &None, NOW, || fixture.client.domain.live().apply(&alondra_core::Event::Joined {
            session_id: "node-00003".into(),
        }));
    assert_eq!(raced.get("node-00003").unwrap().state_since, None);
    assert_eq!(fixture.delta(&before, Metric::NodeSourceRaces), 1);
    assert!(cache.0.lock().unwrap().is_empty());
    let before = fixture.observation.totals();
    let current = fixture.gather(&cache, Lang::En);
    assert_eq!(fixture.delta(&before, Metric::AgentRows), 32);
    assert!(current.get("node-00003").unwrap().state_since.is_some());
}

#[test]
fn missing_store_boundary_or_failed_launch_read_is_never_retained() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    for missing_boundary in [true, false] {
        fixture.gather(&cache, Lang::En);
        let (launches, boundary) = fixture.inputs();
        let before = fixture.observation.totals();
        cache.gather(&fixture.client, &fixture.read, Lang::En, &launches,
            if missing_boundary { None } else { boundary }, missing_boundary, &None, NOW);
        assert_eq!(fixture.delta(&before, Metric::AgentRows), 32);
        assert_eq!(fixture.delta(&before, Metric::NodeSourceRaces), 1);
        assert!(cache.0.lock().unwrap().is_empty());
    }
}

#[test]
fn a_gap_in_report_history_uses_full_rows_instead_of_losing_unread_changes() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    fixture.gather(&cache, Lang::En);
    fixture.result("node-00007", "This unread report must survive a history gap.");
    for peer in 1..=4097 { fixture.client.domain.live().mods.set_peer("node-00000", peer); }
    let before = fixture.observation.totals();
    let changed = fixture.gather(&cache, Lang::En);
    assert_eq!(fixture.delta(&before, Metric::NodeFullBuilds), 1);
    assert_eq!(fixture.delta(&before, Metric::AgentRows), 32);
    assert_eq!(changed.get("node-00007").unwrap().last_message.as_deref(), Some("This unread report must survive a history gap."));
}

/// What building the view of the nodes costs.
struct Cost { rows: u64, leaves: u64, hashed_bytes: u64 }

impl Cost {
    fn of(fixture: &Fixture, before: &Totals) -> Self {
        Self {
            rows: fixture.delta(before, Metric::AgentRows),
            leaves: fixture.delta(before, Metric::NodeLeafSerializations),
            hashed_bytes: fixture.delta(before, Metric::HashBytes),
        }
    }

    /// One changed report costs one row, one serialized component, and a share of the bytes the
    /// full view hashes that is far below an eighth of them: the other 31 rows hash nothing.
    fn within_one_row_budget(&self, full_view: &Cost) -> bool {
        self.rows == 1 && self.leaves == 1 && self.hashed_bytes * 8 < full_view.hashed_bytes
    }
}

fn changing_one_report(fixture: &Fixture, cache: &Cache) -> Cost {
    fixture.result("node-00016", "The final report changed.");
    let before = fixture.observation.totals();
    fixture.gather(cache, Lang::En).digest(&fixture.observation, Serialization::Digest);
    Cost::of(fixture, &before)
}

fn the_full_view_of_32_nodes(fixture: &Fixture) -> Cost {
    let before = fixture.observation.totals();
    fixture.gather(&Cache::default(), Lang::En).digest(&fixture.observation, Serialization::Digest);
    Cost::of(fixture, &before)
}

#[test]
fn one_changed_report_among_32_nodes_costs_one_row_and_none_of_the_other_bytes() {
    let fixture = Fixture::new(32);
    let cache = Cache::default();
    let full_view = the_full_view_of_32_nodes(&fixture);
    fixture.gather(&cache, Lang::En).digest(&fixture.observation, Serialization::Digest);
    let changed = changing_one_report(&fixture, &cache);
    assert_eq!(full_view.rows, 32);
    assert!(changed.within_one_row_budget(&full_view));
}

#[test]
fn rebuilding_every_row_for_one_changed_report_breaks_the_budget() {
    let fixture = Fixture::new(32);
    let full_view = the_full_view_of_32_nodes(&fixture);
    let rebuilt_everything = changing_one_report(&fixture, &Cache::default());
    assert_eq!(rebuilt_everything.rows, 32);
    assert!(!rebuilt_everything.within_one_row_budget(&full_view));
}
