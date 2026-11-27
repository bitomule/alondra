use super::*;
use alondra_daemon_engine::live::LiveOf as _;
use alondra_daemon_engine::contexts::ContextsOf as _;
use alondra_domain_model::{Actor, AgentId, Command, CommandEnvelope, LaunchPlan, ReadSnapshot, PROTOCOL_VERSION};
use serde_json::json;
use std::sync::Arc;

struct Fixture {
    client: Arc<crate::client_routes::Client>,
    home: crate::test_support::Directory,
}

impl Fixture {
    fn new() -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(crate::engine_tests::open(&home).unwrap());
        let client = crate::client_routes::Client::open(domain, &home, Default::default());
        Self { client, home }
    }

    fn command(&self, actor: Actor, id: &str, command: Command) {
        let read = self.client.domain.read();
        self.client.domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: read.state().data_epoch.clone(),
            command_id: id.into(), expected_revision: read.state().revision, command,
        }, None, || Ok(())).unwrap();
    }

    fn agent(&self, id: &str, name: &str) {
        self.command(Actor::System, &format!("register-{id}"), Command::RegisterAgent {
            agent_id: id.into(), name: name.into(), at: 0,
        });
    }

    fn task(&self, id: &str) {
        self.command(Actor::User { user_id: "fixture-user".into() }, &format!("request-{id}"), Command::CreateRequest {
            request_id: id.into(), body: "Review the synthetic project".into(),
            auto_merge: false, maintenance: false, message: false,
        });
        self.command(Actor::Orchestrator { generation: 0 }, &format!("create-{id}"), Command::CreateTask { priority: Default::default(),
            task_id: id.into(), request_id: id.into(), title: format!("Task {id}"),
            agent_id: None, review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(LaunchPlan::new("node", self.home.to_str().unwrap())),
            plan_items: None, continues: None,
        });
    }

    fn report(&self, agent: &str, mut report: Value, at: Millis) {
        report["session_id"] = json!(agent);
        self.client.domain.live().mods.apply(serde_json::from_value(report).unwrap(), at);
    }

    fn context(&self, task: &str, tokens: u64) {
        use alondra_daemon_engine::contexts::ContextEvent;
        let contexts = self.client.domain.contexts();
        let _ = contexts.observed(&task.into(), ContextEvent::Answered {
            tokens, cache: None, model: Some("fixture-model".into()), cached: Default::default(),
        }, 1_000);
        let _ = contexts.observed(&task.into(), ContextEvent::Windows(vec![("fixture-model".into(), 200_000)]), 1_001);
    }

    fn equivalent(&self, state: DomainState, agent: &str,
        launches: &BTreeMap<alondra_domain_model::RequestId, (Option<String>, Option<String>)>,
        exhausted: &Option<alondra_engine_ports::orchestrator::Exhausted>,
    ) -> Vec<Option<LiveNode>> {
        let read = ReadSnapshot::build(Arc::new(state));
        let id: AgentId = agent.into();
        [Lang::En, Lang::Es].into_iter().map(|lang| {
            let expected = reference_node_row(&self.client, read.state(), &read.state().agents[&id], lang, launches, exhausted, 61_000);
            let actual = node_row(&self.client, &read, &id, lang, launches, exhausted, 61_000);
            assert_eq!(serde_json::to_value(&actual).unwrap(), serde_json::to_value(&expected).unwrap(),
                "every wire field must match for {agent} in {lang:?}");
            actual
        }).collect()
    }
}

#[test]
fn indexed_node_rows_preserve_history_context_project_and_closed_visibility() {
    let fixture = Fixture::new();
    fixture.agent("node", "Name fallback/Worker");
    for id in ["a-archived", "b-closing", "c-current", "d-finished"] { fixture.task(id); }
    let mut state = (*fixture.client.domain.read().state()).as_ref().clone();
    for task in state.tasks.values_mut() { task.agent_id = Some("node".into()); }
    state.tasks.get_mut(&"a-archived".into()).unwrap().archived = true;
    state.tasks.get_mut(&"a-archived".into()).unwrap().state = TaskState::Done;
    state.tasks.get_mut(&"b-closing".into()).unwrap().state = TaskState::Closing;
    state.tasks.get_mut(&"c-current".into()).unwrap().state = TaskState::Running;
    state.tasks.get_mut(&"d-finished".into()).unwrap().state = TaskState::Done;
    state.agents.get_mut(&"node".into()).unwrap().phase = NodePhase::Closed;
    fixture.context("a-archived", 40_000);
    fixture.context("c-current", 160_000);
    let launches = BTreeMap::from([
        ("a-archived".into(), (Some("Archived project".into()), None)),
        ("b-closing".into(), (Some("First retained project".into()), None)),
        ("c-current".into(), (Some("Current project".into()), None)),
    ]);
    for row in fixture.equivalent(state.clone(), "node", &launches, &None).into_iter().flatten() {
        assert_eq!(row.context_percent, Some(20));
        assert_eq!(row.context_window, Some(200_000));
        assert_eq!(row.project.as_deref(), Some("First retained project"));
        assert!(row.disconnected);
        assert_eq!(row.state_since, None);
    }
    for row in fixture.equivalent(state.clone(), "node", &BTreeMap::new(), &None).into_iter().flatten() {
        assert_eq!(row.project.as_deref(), Some("Name fallback"));
    }
    state.tasks.get_mut(&"c-current".into()).unwrap().state = TaskState::Cancelled;
    assert!(fixture.equivalent(state.clone(), "node", &launches, &None).iter().all(Option::is_none),
        "closing, archived and terminal tasks do not keep a closed node visible");
    state.agents.get_mut(&"node".into()).unwrap().phase = NodePhase::ThroughSdk;
    assert!(fixture.equivalent(state.clone(), "node", &launches, &None).iter().all(Option::is_some));
    state.tasks.clear();
    assert!(fixture.equivalent(state.clone(), "node", &launches, &None).iter().all(Option::is_some));
    state.agents.get_mut(&"node".into()).unwrap().phase = NodePhase::Closed;
    assert!(fixture.equivalent(state, "node", &launches, &None).iter().all(Option::is_none));
}

#[test]
fn indexed_node_rows_preserve_reported_work_and_localized_clocks() {
    let fixture = Fixture::new();
    fixture.agent("reported", "Reporter");
    let reports = [
        json!({"event":"session.start", "version":"fixture", "cwd":"/workspace/row-project"}),
        json!({"event":"model", "model":"fixture-model"}),
        json!({"event":"turn.complete", "answer":"Previous answer", "aborted":true}),
        json!({"event":"background", "tasks":[{"id":"ended-shell", "type":"shell", "command":"old check"}]}),
        json!({"event":"background", "tasks":[
            {"id":"shell-one", "type":"shell", "command":"check one"},
            {"id":"shell-two", "type":"shell", "command":"check two"},
            {"id":"other", "type":"other", "description":"Other work"}]}),
        json!({"event":"subagent.start", "id":"finished-child", "description":"Completed review", "type":"reviewer", "model":"fixture-model"}),
        json!({"event":"subagent.end", "id":"finished-child"}),
        json!({"event":"subagent.start", "id":"child", "description":"Review links", "type":"reviewer", "model":"fixture-model"}),
        json!({"event":"workflow.start", "id":"workflow", "name":"Review workflow"}),
        json!({"event":"steps", "steps":[
            {"id":"pending", "subject":"Prepare", "status":"pending"},
            {"id":"active", "subject":"Review", "status":"in_progress", "active_form":"Reviewing"},
            {"id":"done", "subject":"Read", "status":"completed"}]}),
        json!({"event":"tool.start", "tool":"Bash", "input":{"command":format!("check {}\nsecond line", "é".repeat(150))}}),
    ];
    for (n, report) in reports.into_iter().enumerate() { fixture.report("reported", report, 1_000 + n as i64); }
    let state = fixture.client.domain.read().state().as_ref().clone();
    let rows = fixture.equivalent(state.clone(), "reported", &BTreeMap::new(), &None);
    assert_ne!(rows[0].as_ref().unwrap().background_badge, rows[1].as_ref().unwrap().background_badge);
    for row in rows.into_iter().flatten() {
        assert_eq!(row.project.as_deref(), Some("others"));
        assert_eq!(row.subagents.len(), 2);
        assert_eq!(row.workflows.len(), 1);
        assert_eq!(row.steps.len(), 3);
        assert!(row.background.len() >= 4, "running background tasks and ended shells are visible");
        assert_eq!(row.last_message.as_deref(), Some("Previous answer"));
        assert!(row.last_turn_aborted);
        assert_eq!(row.tool.as_ref().unwrap().chars().count(), 141);
        assert!(row.tool_since.is_some());
        assert_eq!(row.model.as_deref(), Some("fixture-model"));
    }
    fixture.client.domain.live().mods.apply_sdk("reported", &json!({
        "type":"result", "subtype":"success", "result":"SDK answer", "total_cost_usd":0.42,
    }), 20_000);
    for row in fixture.equivalent(state.clone(), "reported", &BTreeMap::new(), &None).into_iter().flatten() {
        assert_eq!(row.cost_cents, Some(42));
        assert_eq!(row.last_message.as_deref(), Some("SDK answer"));
    }
    fixture.report("reported", json!({"event":"session.end"}), 30_000);
    for row in fixture.equivalent(state, "reported", &BTreeMap::new(), &None).into_iter().flatten() {
        assert_eq!(row.background_badge, None);
        assert_eq!(row.tool, None);
    }
}

#[test]
fn indexed_node_rows_preserve_sdk_turn_and_pause_states() {
    use alondra_domain_model::{NodeEvent, Pause, PauseCleanup, PauseReason};
    let fixture = Fixture::new();
    fixture.agent("sdk-node", "SDK worker");
    fixture.task("current");
    alondra_daemon_engine::domain_transport::fixtures::admitted_sdk_execution(&(fixture.client.domain.clone() as std::sync::Arc<dyn alondra_engine_ports::Runtime>), "current", "sdk-node", 0);
    let state = fixture.client.domain.read().state().as_ref().clone();
    for row in fixture.equivalent(state, "sdk-node", &BTreeMap::new(), &None).into_iter().flatten() {
        assert_eq!(row.state_key.as_deref(), Some("busy"));
        assert!(row.working);
        assert!(!row.waiting);
    }
    for (n, reason, key) in [(1, PauseReason::ByUser, "paused"), (2, PauseReason::Memory, "waiting_machine")] {
        fixture.command(Actor::System, &format!("pause-{n}"), Command::MoveNode {
            agent_id: "sdk-node".into(), event: NodeEvent::Paused { pause: Pause {
                reason, since: n, cut: None, turn_since: None, cleanup: PauseCleanup::Done, released: false,
            } },
        });
        let state = fixture.client.domain.read().state().as_ref().clone();
        for row in fixture.equivalent(state, "sdk-node", &BTreeMap::new(), &None).into_iter().flatten() {
            assert_eq!(row.state_key.as_deref(), Some(key));
            assert!(!row.working);
            assert_eq!(row.waiting, key == "paused");
        }
        fixture.command(Actor::System, &format!("resume-{n}"), Command::MoveNode {
            agent_id: "sdk-node".into(), event: NodeEvent::Resumed,
        });
        fixture.equivalent(fixture.client.domain.read().state().as_ref().clone(), "sdk-node", &BTreeMap::new(), &None);
    }
}

#[cfg(test)]
fn reference_node_row(client: &crate::client_routes::Client, state: &DomainState,
    agent: &alondra_domain_model::Agent, lang: Lang,
    launches: &BTreeMap<alondra_domain_model::RequestId, (Option<String>, Option<String>)>,
    exhausted: &Option<alondra_engine_ports::orchestrator::Exhausted>, now: Millis) -> Option<LiveNode> {
    let node_tasks = alondra_domain_model::NodeTasks::new(state);
    let session_id = agent.id.0.clone();
    let reported = client.domain.live().mods.session(&session_id).unwrap_or_default();
    let mine = node_tasks.of(&agent.id);
    let open_task = mine.iter().any(|task| !task.archived
        && !matches!(task.state, TaskState::Done | TaskState::Closing | TaskState::Cancelled));
    let (node, state_since) = match client.domain.live().node(&session_id) {
        Some(node) => {
            let state_since = Some(node.state_since);
            (node, state_since)
        }
        None if reported.started_at.is_some() || agent.phase.live() || open_task => {
            let mut node = alondra_core::Node::new(&session_id, 0);
            node.cwd = reported.cwd.clone();
            (node, None)
        }
        None => return None,
    };
    let project = if node.cwd.is_some() || node.is_orchestrator {
        node.project()
    } else {
        launched_project(mine, agent, &launches).unwrap_or_else(|| node.project())
    };
    let sdk = (agent.phase == NodePhase::ThroughSdk).then(|| state.sdk_live_of(&agent.id, node_tasks.current(&agent.id)));
    let state_key = match (&agent.phase, reported.activity()) {
        (NodePhase::ThroughSdk, _) => match sdk.as_ref().and_then(Option::as_ref) {
            Some(live) if matches!(live, alondra_domain::LiveState::Working(alondra_domain::Worker::Turn | alondra_domain::Worker::OwnWork | alondra_domain::Worker::AfterMerge
                | alondra_domain::Worker::Alondra(alondra_domain::Step::Starting | alondra_domain::Step::Delivering | alondra_domain::Step::Waking))) => "busy",
            Some(alondra_domain::LiveState::WaitingUser(cards)) if cards.iter().all(|card| state.node_pause_card(card)) => "paused",
            Some(alondra_domain::LiveState::WaitingCapacity(_)) => "waiting_machine",
            _ => "free",
        },
        _ => "free",
    };
    let state_label = node_label(state_key, lang);
    let tool = reported.tool.as_ref().map(|tool| match &tool.summary {
        Some(summary) => format!("{}: {summary}", tool.name),
        None => tool.name.clone(),
    });
    let answer = reported.answer.as_ref().filter(|answer| !answer.text.trim().is_empty());
    let last_message = reported.last_message();
    let failure = reported.job.as_ref().filter(|job| job.phase == crate::job_state::Phase::Failed).map(|job| job.detail.clone()).filter(|detail| !detail.is_empty());
    let summary = failure.or_else(|| tool.clone())
        .or_else(|| node.daemon_needs.clone())
        .or_else(|| node.daemon_detail.clone())
        .map(|summary| summary.lines().next().unwrap_or("").chars().take(141).collect());
    let running: Vec<(&str, &alondra_core::BackgroundItem)> = reported.background.iter()
        .filter(|(_, item)| reported.shell_end(item).is_none()).map(|(id, item)| (id.as_str(), item)).collect();
    let counts = [alondra_core::BackgroundKind::Agent, alondra_core::BackgroundKind::Shell,
        alondra_core::BackgroundKind::Other].into_iter().filter_map(|kind| {
            let count = running.iter().filter(|(_, item)| item.kind == kind).count();
            (count > 0).then(|| {
                let noun = match (lang, kind) {
                    (Lang::En, alondra_core::BackgroundKind::Agent) => "subagent",
                    (Lang::En, alondra_core::BackgroundKind::Shell) => "shell",
                    (Lang::En, alondra_core::BackgroundKind::Other) => "task",
                    (Lang::Es, alondra_core::BackgroundKind::Agent) => "subagente",
                    (Lang::Es, alondra_core::BackgroundKind::Shell) => "shell",
                    (Lang::Es, alondra_core::BackgroundKind::Other) => "tarea",
                };
                format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
            })
        }).collect::<Vec<_>>();
    let background_badge = (!counts.is_empty()).then(|| counts.join(" · "));
    let context = mine.iter()
        .find_map(|task| client.domain.contexts().used(&task.id))
        .map(|used| (used.percent(), used.window));
    let reported_wait = mine.first()
        .and_then(|task| {
            let lifecycle = state.lifecycle_of(&task.id).ok()?;
            alondra_daemon_engine::sdk_live::wait_file(&*client.domain, task.account_key().provider, lifecycle.execution()?.instance().as_str())
        })
        .and_then(|path| alondra_engine_ports::live_view::read_wait_file(&path))
        .map(|wait| alondra_view_model::LiveReportedWait { what: wait.what, progress: wait.progress, detail: wait.detail, since: timestamp(wait.since_ms) });
    Some(LiveNode {
        agent_id: node.session_id,
        short_id: Some(node.short_id),
        cwd: node.cwd.clone(),
        name: agent.name.clone(),
        project: Some(project),
        state_key: Some(state_key.to_owned()),
        state_label: Some(state_label),
        summary,
        needs: node.daemon_needs.clone(),
        subagents: reported.work.agents.iter().map(|agent| LiveRun {
            label: agent.description.clone(),
            kind: agent.kind.clone(),
            model: agent.model.clone(),
            agents: None,
            since: timestamp(agent.started_at),
            state: run_state(agent.started_at, agent.end),
            workflow: None,
        }).collect(),
        workflows: reported.work.workflows.iter().map(|workflow| {
            let state = workflow.transcript_dir.as_deref()
                .and_then(|dir| crate::workflow_state::read(dir, workflow.end.is_some()));
            LiveRun {
                label: workflow.name.clone(),
                kind: None,
                model: None,
                agents: state.as_ref().map(|state| state.running + state.done)
                    .or_else(|| workflow.transcript_dir.as_deref().and_then(crate::node_work::workflow_agents)),
                since: timestamp(workflow.started_at),
                state: run_state(workflow.started_at, workflow.end),
                workflow: state,
            }
        }).collect(),
        steps: reported.work.steps.iter().map(|step| LiveStep {
            subject: step.subject.clone(),
            state: match step.status {
                StepStatus::InProgress => StepState::InProgress,
                StepStatus::Completed => StepState::Completed,
                StepStatus::Pending | StepStatus::Deleted | StepStatus::Unknown => StepState::Pending,
            },
            active_form: step.active_form.clone(),
        }).collect(),
        background: reported.background.iter().filter(|(id, _)| !reported.work.runs(id))
            .map(|(_, item)| (item, reported.shell_end(item)))
            .chain(reported.work.ended_tasks.iter().map(|task| (&task.item, Some(task.end))))
            .map(|(item, end)| LiveBackground {
                kind: serde_json::to_value(item.kind).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_else(|| "other".into()),
                label: item.label.clone(),
                since: timestamp(item.started_at),
                state: run_state(item.started_at, end),
            }).collect(),
        background_badge,
        reported_wait,
        working: matches!(&sdk, Some(Some(alondra_domain::LiveState::Working(alondra_domain::Worker::Turn)))),
        waiting: matches!(&sdk, Some(Some(alondra_domain::LiveState::WaitingUser(_)))),
        state_since: state_since.map(timestamp),
        last_message: last_message.map(|(text, _)| text.to_owned()),
        last_message_at: last_message.map(|(_, at)| timestamp(at)),
        last_turn_aborted: answer.is_some_and(|answer| answer.aborted),
        tool: tool.map(|tool| tool.chars().take(141).collect()),
        tool_since: reported.tool.as_ref().map(|tool| alondra_engine_ports::usage::clock_text(tool.since, now, lang == Lang::Es)),
        context_percent: context.and_then(|(percent, _)| percent).map(|percent| percent.round().clamp(0.0, 999.0) as u32),
        context_window: context.and_then(|(_, window)| window),
        cost_cents: reported.sdk.as_ref().and_then(|readings| readings.cost_cents),
        model: reported.model.clone(),
        disconnected: agent.phase == NodePhase::Closed && open_task,
        waiting_until: exhausted.as_ref().filter(|_| node.usage_limited).map(|exhausted| timestamp(exhausted.until)),
    })
}
