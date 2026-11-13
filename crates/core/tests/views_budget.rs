//! Every family of views renders a domain the size of the real one in under 50 ms, cold, right
//! after a commit: what a row needs is read from the committed index, never by walking the domain
//! once per row.
use alondra_core::domain::{
    Actor, CardKind, CardSpec, Command, replay, CommandEnvelope, DeliveryTarget, DomainState, InteractionId, LaunchPlan, LifecycleRecord,
    McpServerState, McpStatus, MergeMethod, PROTOCOL_VERSION, PrState, QuestionOption, ReadSnapshot, Recipient,
    RuntimeObservation, Schedule, ScheduleFrequency, Scope,
};
use alondra_core::accounts::{AccountBilling, AccountId, AccountState, AccountStatus, Provider};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use alondra_core::views::{Client, Family, Lang, LiveNode, LivePullRequest, LiveSnapshot, LiveTaskStart, RenderContext, Transport, build};
use alondra_core::metrics::fold::Outcome;
use alondra_core::metrics::report::{DayRow, MetricsInputs, Release, Spend, Sums, TaskRow, Tokens, WeekRow};
use std::time::Instant;

const BUDGET_MS: f64 = 50.0;
const SAMPLES: usize = 5;
/// What every id of a unit carries, replaced by the unit's own number in each copy.
const TOKEN: &str = "u000";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// Delivered a PR the user accepted and Alondra merged; its node closed.
    Done,
    /// Delivered a document the user accepted.
    Document,
    /// Its PR waits for the user's review.
    Review,
    /// Its node waits on an open question.
    Asking,
    /// Its node works; every other one has an open PR.
    Running,
    Cancelled,
    /// A request the orchestrator has not split into tasks yet.
    Defining,
    /// Its node runs through the Agent SDK and waits for the user to sign in to an MCP server.
    WaitingMcp,
    /// Its node ran through the Agent SDK and its session ended on its own: it offers a crash report.
    SessionEnded,
}

impl Shape {
    fn name(self) -> &'static str {
        match self {
            Shape::Done => "done",
            Shape::Document => "document",
            Shape::Review => "review",
            Shape::Asking => "asking",
            Shape::Running => "running",
            Shape::Cancelled => "cancelled",
            Shape::Defining => "defining",
            Shape::WaitingMcp => "waiting-mcp",
            Shape::SessionEnded => "session-ended",
        }
    }
}

/// The real domain on 15 October: ~300 tasks, ~280 nodes, ~300 requests, ~1700 cards, ~250
/// deliveries with a PR and 12 cards waiting for the user.
const UNITS: [(Shape, usize); 9] = [
    (Shape::Done, 230),
    (Shape::Document, 10),
    (Shape::Review, 6),
    (Shape::Asking, 6),
    (Shape::Running, 24),
    (Shape::Cancelled, 10),
    (Shape::Defining, 14),
    (Shape::WaitingMcp, 4),
    (Shape::SessionEnded, 4),
];

fn user() -> Actor {
    Actor::User { user_id: "budget-user".into() }
}

fn id(kind: &str, shape: Shape) -> String {
    format!("{kind}-{}-{TOKEN}", shape.name())
}

struct Seed(DomainState);

impl Seed {
    fn run(&mut self, actor: Actor, command: Command) {
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: self.0.data_epoch.clone(),
            command_id: format!("seed-{}", self.0.revision).into(),
            expected_revision: self.0.revision,
            command,
        };
        if let Err(error) = self.0.execute(&actor, &envelope) {
            panic!("seeding the synthetic domain: {error:?}");
        }
    }
}

/// One unit of a shape, built through the domain's own commands, its ids carrying `TOKEN`.
fn unit(shape: Shape) -> DomainState {
    let mut seed = Seed(DomainState::new("budget-epoch"));
    let (request, agent, task, pr, delivery) = (id("r", shape), id("a", shape), id("t", shape), id("pr", shape), id("d", shape));
    let node = Actor::Agent { agent_id: agent.as_str().into() };
    seed.run(user(), Command::CreateRequest {
        request_id: request.as_str().into(),
        body: format!("Synthetic {} work {TOKEN}. Every detail the user wrote, on a second sentence.", shape.name()),
        auto_merge: false, maintenance: false, message: false,
    });
    if shape == Shape::Defining {
        return seed.0;
    }
    seed.run(user(), Command::TitleRequest { request_id: request.as_str().into(), title: format!("Synthetic {} {TOKEN}", shape.name()) });
    seed.run(Actor::System, Command::RegisterAgent { agent_id: agent.as_str().into(), name: format!("{} node {TOKEN}", shape.name()), at: 0 });
    let through_sdk = matches!(shape, Shape::WaitingMcp | Shape::SessionEnded);
    seed.run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: task.as_str().into(), request_id: request.as_str().into(), title: format!("Synthetic {} task {TOKEN}", shape.name()),
        agent_id: (!through_sdk).then(|| agent.as_str().into()), review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/synthetic/project")),
    });
    if through_sdk {
        seed.run(Actor::System, Command::AdmitExecution { provider: alondra_core::accounts::Provider::Claude, task_id: task.as_str().into(), agent_id: agent.as_str().into(),
            conversation: format!("conversation-{TOKEN}"), instance: format!("instance-{TOKEN}"), host: "sdk-host".into(), model: None, account: None, leaves_limit: None });
        let Some(LifecycleRecord::Planned { input }) = seed.0.lifecycle_journal[&task.as_str().into()].first().cloned() else { panic!("planned first") };
        seed.run(Actor::System, Command::RecordRuntimeFacts { task_id: task.as_str().into(), generation: 1, facts: vec![
            RuntimeObservation::ResourceAvailable, RuntimeObservation::TurnStarted { turn: "turn-1".into(), input },
        ] });
        if shape == Shape::SessionEnded {
            seed.run(Actor::System, Command::RecordHostEnded { task_id: task.as_str().into(), generation: 1, resumes: false, ended_work: vec![], by_alondra: false });
            return seed.0;
        }
        seed.run(Actor::System, Command::RecordMcpServers { task_id: task.as_str().into(), generation: 1, servers: vec![McpServerState {
            name: "linear".into(), status: McpStatus::NeedsAuth, error: None, source: Some("user".into()),
            transport: Some("http".into()), url: Some("https://mcp.linear.app/mcp".into()), traits: alondra_core::domain::McpTraits { scope: alondra_core::domain::McpScope::User, origin: alondra_core::domain::McpOrigin::UserSettings, connect: alondra_core::domain::McpConnect::SignIn, reconnect: true, sign_out: true, remove: true },
        }] });
        seed.run(node, Command::WaitForMcp { task_id: task.as_str().into(), server: "linear".into() });
        return seed.0;
    }
    seed.run(node.clone(), Command::StartTask { task_id: task.as_str().into() });
    let notices = match shape {
        Shape::Done => 5,
        Shape::Running => 4,
        _ => 3,
    };
    for notice in 0..notices {
        let interaction_id: InteractionId = format!("n{notice}-{}-{TOKEN}", shape.name()).into();
        seed.run(Actor::System, Command::PublishInformation {
            attachments: vec![], interaction_id: interaction_id.clone(), scope: Scope::Task { task_id: task.as_str().into() },
            recipient: Recipient::User, body: format!("Synthetic notice {notice}"),
            card: CardSpec::derived(CardKind::Know, &format!("Read synthetic notice {notice}")),
        });
        seed.run(user(), Command::MarkRead { interaction_id });
    }
    let options = vec![
        QuestionOption { id: "yes".into(), label: "Yes".into(), description: None },
        QuestionOption { id: "no".into(), label: "No".into(), description: None },
    ];
    if matches!(shape, Shape::Done | Shape::Running | Shape::Asking) {
        let question = id("q", shape);
        seed.run(node.clone(), Command::AskQuestion {
            attachments: vec![], interaction_id: question.as_str().into(), scope: Scope::Task { task_id: task.as_str().into() },
            recipient: Recipient::User, body: "Which way?".into(), blocking: true, options: options.clone(),
            card: CardSpec::question("Choose the synthetic way", &options),
        });
        if shape != Shape::Asking {
            seed.run(user(), Command::AnswerQuestion {
                attachments: vec![], interaction_id: question.as_str().into(), option_id: "yes".into(), inputs: BTreeMap::new(),
            });
        }
    }
    let delivers_pr = matches!(shape, Shape::Done | Shape::Review | Shape::Cancelled);
    if delivers_pr || shape == Shape::Running {
        seed.run(node.clone(), Command::RegisterPr {
            task_id: task.as_str().into(), pr_id: pr.as_str().into(), host: "github.com".into(),
            repository: "synthetic/project".into(), number: 1, head: "head-1".into(), base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        });
    }
    let targets = match shape {
        _ if delivers_pr => vec![DeliveryTarget::PullRequest { pr_id: pr.as_str().into(), head: "head-1".into() }],
        Shape::Document => {
            let revision = id("rev", shape);
            seed.run(node.clone(), Command::CreateDocument {
                task_id: task.as_str().into(), document_id: id("doc", shape).into(), title: format!("Synthetic plan {TOKEN}"),
                revision_id: revision.as_str().into(), content_digest: "a".repeat(64), content_reference: "https://example.org/plan".into(),
                role: "result".into(),
            });
            vec![DeliveryTarget::DocumentRevision { document_id: id("doc", shape).into(), revision_id: revision.into() }]
        }
        _ => vec![],
    };
    if !targets.is_empty() {
        seed.run(node.clone(), Command::PresentResult {
            checklist: vec![], attachments: vec![], task_id: task.as_str().into(), interaction_id: delivery.as_str().into(),
            body: "Synthetic result".into(), targets: targets.clone(), is_final: true, nothing_to_review: false,
            card: CardSpec::delivery("Review the synthetic result", &targets, true),
        });
    }
    match shape {
        Shape::Done | Shape::Document => {
            seed.run(user(), Command::AcceptDelivery { presentation_id: delivery.as_str().into(), targets });
            if shape == Shape::Done {
                seed.run(Actor::System, Command::ObservePrState {
                    pr_id: pr.as_str().into(), head: "head-1".into(), base: "main".into(), state: PrState::Merged,
                    merged_head: Some("head-1".into()),
                });
            }
            seed.run(Actor::System, Command::CompleteTask { task_id: task.as_str().into(), presentation_id: delivery.as_str().into() });
            seed.run(Actor::System, Command::RecordNodeEnd { task_id: task.as_str().into(), end: alondra_core::domain::NodeEnd::Closed, agent_id: None });
        }
        Shape::Cancelled => seed.run(user(), Command::CancelTask { task_id: task.as_str().into(), reason: None }),
        _ => {}
    }
    seed.0
}

/// Adds a copy of `unit` whose ids carry `number` instead of `TOKEN`, its cards after every card
/// already there.
fn absorb(into: &mut DomainState, unit: &DomainState, number: usize) {
    let json = serde_json::to_string(unit).expect("unit serialization").replace(TOKEN, &format!("u{number:03}"));
    let copy: DomainState = serde_json::from_str(&json).expect("unit copy");
    let DomainState {
        data_epoch: _, revision, orchestrator_generation: _, orchestrator_session: _, orchestrator_batch_base: _, requests, agents, tasks, documents,
        document_revisions, document_reviews, pull_requests, mut interactions, merge_authorizations, stack_readiness, schedule,
        holds, checklists, checklist_marks, last_words, request_plans, plan_reviews, lifecycle_journal, mcp, ..
    } = copy;
    let next = into.interactions.values().map(|i| i.order).max().unwrap_or(0);
    for interaction in interactions.values_mut() {
        interaction.order += next;
    }
    into.revision = into.revision.max(revision);
    into.requests.extend(requests);
    into.agents.extend(agents);
    into.tasks.extend(tasks);
    into.documents.extend(documents);
    into.document_revisions.extend(document_revisions);
    into.document_reviews.extend(document_reviews);
    into.pull_requests.extend(pull_requests);
    into.interactions.extend(interactions);
    into.merge_authorizations.extend(merge_authorizations);
    into.stack_readiness.extend(stack_readiness);
    into.schedule.schedules.extend(schedule.schedules);
    into.schedule.runs.extend(schedule.runs);
    into.holds.extend(holds);
    into.checklists.extend(checklists);
    into.checklist_marks.extend(checklist_marks);
    into.last_words.extend(last_words);
    into.request_plans.extend(request_plans);
    into.plan_reviews.extend(plan_reviews);
    into.lifecycle_journal.extend(lifecycle_journal);
    into.mcp.extend(mcp);
}

fn real_size() -> DomainState {
    let mut state = DomainState::new("budget-epoch");
    for (shape, count) in UNITS {
        let template = unit(shape);
        for number in 0..count {
            absorb(&mut state, &template, number);
        }
    }
    let numbered: Vec<_> = state.pull_requests.keys().cloned().collect();
    for (number, pr) in numbered.into_iter().enumerate() {
        state.pull_requests.get_mut(&pr).expect("listed").number = number as u64 + 1;
    }
    for index in 0..3 {
        let schedule = Schedule {
            paid_account_grant: None,
            id: format!("schedule-{index}").into(), title: format!("Synthetic schedule {index}"), text: "Review the project".into(),
            project: Some("project".into()), options: BTreeMap::new(), disallowed_tools: vec![], frequency: ScheduleFrequency::Daily,
            hour: 9, minute: 0, weekday: None, interval: None, start_at: None, enabled: true, next_run: Some(100), revision: 1,
            deleted_at: None,
        };
        state.schedule.schedules.insert(schedule.id.clone(), schedule);
    }
    state.accounts = ["default", "work", "team"].into_iter().map(|id| AccountStatus {
        provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: format!("Synthetic {id}"), state: AccountState::SignedIn,
        native_identity: None, email: Some(format!("{id}@example.org")), plan: Some("max".into()), billing: AccountBilling::default(), sign_ins: 0, profile: None,
    }).collect();
    let accounts: Vec<AccountId> = state.accounts.iter().map(|account| account.id.clone()).collect();
    for (number, task) in state.tasks.values_mut().enumerate() {
        task.account = Some(accounts[number % accounts.len()].clone());
    }
    state.revision = state.revision.max(state.interactions.values().map(|i| i.order).max().unwrap_or(0)) + 1;
    state.validate().expect("the synthetic domain is valid");
    let mut state = loaded(state);
    model_controls_fixture(&mut state);
    state.validate().expect("the model controls fixture is valid");
    state
}

fn model_controls_fixture(state: &mut DomainState) {
    use alondra_core::accounts::AccountKey;
    use alondra_core::model_selection::{AccountModelCatalog, CatalogModel, ModelCandidate, ModelKey, ModelProfile, ModelSelection, RoleModelPolicy, TaskSelection};
    state.accounts.push(AccountStatus { provider: Provider::Codex, id: AccountId::default_account(), name: "Synthetic Codex".into(), state: AccountState::SignedIn,
        native_identity: None, email: Some("codex@example.invalid".into()), plan: Some("plus".into()), billing: AccountBilling::default(), sign_ins: 0, profile: None });
    state.account_order = state.accounts.iter().map(|account| AccountKey::new(account.provider, account.id.clone())).collect();
    state.account_config_version = "budget-account-order".into();
    state.model_profiles_version = "budget-model-profiles".into();
    for account in &state.accounts {
        let provider = account.provider;
        let models: Vec<_> = ["standard", "design", "review"].into_iter().map(|role| CatalogModel {
            model: format!("budget-{}-{role}", provider.as_str()), display_name: format!("Synthetic {} {role}", provider.as_str()),
            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()], default_reasoning_effort: Some("medium".into()),
            context_window: Some(200_000), input_modalities: vec!["text".into(), "image".into()], is_default: provider == Provider::Codex && role == "standard", hidden: false,
        }).collect();
        state.model_catalogs.push(AccountModelCatalog { account: AccountKey::new(provider, account.id.clone()), revision: format!("budget-{}-{}", provider.as_str(), account.id),
            native_default: (provider == Provider::Codex).then(|| models[0].model.clone()), models, measured_at: 1 });
    }
    for provider in [Provider::Claude, Provider::Codex] {
        state.model_profiles.insert(provider, [ModelProfile::Implementation, ModelProfile::General, ModelProfile::Design, ModelProfile::Review, ModelProfile::Orchestrator].into_iter().map(|profile| (profile,
            RoleModelPolicy::OrderedCandidates { candidates: vec![ModelCandidate { model: format!("budget-{}-standard", provider.as_str()), reasoning_effort: Some("high".into()) },
                ModelCandidate { model: format!("budget-{}-review", provider.as_str()), reasoning_effort: Some("medium".into()) }] })).collect());
    }
    let task_id = id("t", Shape::Running).replace(TOKEN, "u001");
    let task = state.tasks.get_mut(&task_id.as_str().into()).expect("the selected running task");
    let account = task.account_key();
    let model = ModelKey { provider: account.provider, model: format!("budget-{}-standard", account.provider.as_str()) };
    let mut selection = TaskSelection::automatic(ModelProfile::Implementation);
    selection.request_model(Some(model.clone())); selection.requested_reasoning_effort = Some("high".into());
    selection.effective = Some(ModelSelection { model, reasoning_effort: Some("high".into()), catalog_revision: state.model_catalogs.iter().find(|catalog| catalog.account == account).expect("the active account catalog").revision.clone() });
    task.selection = Some(selection);
}

/// The domain as the daemon opens it: each journal replayed once and handed to the domain with it.
fn loaded(mut state: DomainState) -> DomainState {
    let journals: Vec<_> = state.lifecycle_journal.iter()
        .map(|(task, records)| (task.clone(), records.clone(), replay(task, records).expect("the journal replays")))
        .collect();
    state.load_journal(journals).expect("the synthetic domain loads");
    state
}

/// What the daemon reads live for such a domain: every node at work, every PR's details.
fn live(state: &DomainState) -> LiveSnapshot {
    let projects = ["alondra", "quill", "lumo"];
    let nodes = state.agents.values().filter(|agent| agent.phase.live()).enumerate().map(|(index, agent)| LiveNode {
        agent_id: agent.id.0.clone(),
        short_id: Some(format!("s{index}")),
        name: agent.name.clone(),
        project: Some(projects[index % projects.len()].into()),
        summary: Some("Running the synthetic tests".into()),
        working: true,
        state_since: Some("2026-10-15T10:00:00".into()),
        ..LiveNode::default()
    }).collect();
    let pull_requests = state.pull_requests.values().map(|pr| LivePullRequest {
        id: pr.id.0.clone(),
        title: Some(format!("Synthetic PR #{}", pr.number)),
        body: Some("What it changes and why.".into()),
        head_ref: Some("branch".into()),
        fetched: true,
        additions: 40,
        deletions: 12,
        changed_files: 3,
        ..LivePullRequest::default()
    }).collect();
    LiveSnapshot {
        nodes,
        pull_requests,
        request_times: state.requests.keys().map(|id| (id.0.clone(), "2026-10-15T09:00:00".into())).collect(),
        task_since: state.tasks.keys().map(|id| (id.0.clone(), "2026-10-15T09:30:00".into())).collect(),
        task_started: state.tasks.iter().map(|(id, task)| (id.0.clone(), LiveTaskStart {
            since: "2026-10-15T09:10:00".into(),
            until: task.state.terminal().then(|| "2026-10-15T09:30:00".into()),
            ran_secs: task.state.terminal().then_some(1_200),
        })).collect(),
        metrics: Some(metrics_inputs()),
        ..LiveSnapshot::default()
    }
}

/// 90 days of ended tasks at 250 a week, on three projects and four models, two years of weeks
/// before them, and a day of the orchestrator's spend per model for each of those days.
fn metrics_inputs() -> MetricsInputs {
    const DAY: i64 = 86_400_000;
    let now = 20_752 * DAY;
    let (projects, models) = (["alondra", "quill", "lumo"], ["sonnet", "opus", "haiku", "codex"]);
    let tasks = (0..3_250).map(|n: i64| {
        let finished_at = now - n * 90 * DAY / 3_250;
        TaskRow {
            project: Some(projects[n as usize % 3].into()), model: Some(models[n as usize % 4].into()), effort: Some("medium".into()),
            approach: Some("solo".into()), provider: None, kind: None, outcome: if n % 20 == 0 { Outcome::Cancelled } else { Outcome::Done },
            started_at: Some(finished_at - 3_600_000), finished_at, clean: n % 3 != 0, change_rounds: n % 3, reviewer_rounds: n % 2,
            interventions: n % 4, reopened: n % 17 == 0, relaunched: n % 23 == 0, work_ms: 1_800_000, waiting_user_ms: 600_000,
            queued_ms: 60_000, blocked_ms: 0, sessions: 2, steps: 150, subagent_steps: 20, looks: 60, polls: 7, pr_merged: n % 5 != 0,
            pr_lines: (n % 7 != 0).then_some(200), spend: Some(Spend { cost_micros: (n % 4 != 3).then_some(2_000_000 + n),
                tokens: Tokens { input: 10_000, output: 90_000, cache_read: 30_000_000, cache_write: 400_000 } }),
        }
    }).collect();
    let (mut weeks, mut days) = (vec![], vec![]);
    for model in models {
        for week in 13..117 {
            for project in projects {
                weeks.push(WeekRow {
                    week: now - week * 7 * DAY, project: project.into(), model: model.into(), effort: "medium".into(), approach: "solo".into(),
                    sums: Sums { tasks: 20, done: 19, clean: 12, measured: 20, costed: 15, cost_micros: 40_000_000, sessions: 40, steps: 3_000, ..Sums::default() },
                });
            }
        }
        for day in 0..180 {
            days.push(DayRow { day: now - day * DAY, model: model.into(), cost_micros: 3_000_000, costed: 5, sessions: 5, tokens: Tokens::default() });
        }
    }
    let releases = (0..30).map(|n: i64| Release { version: format!("0.3.{n}"), first_seen_at: now - (30 - n) * 6 * DAY }).collect();
    MetricsInputs { now, tasks, weeks, days, releases }
}

/// The card of the tasks that wait for the MCP server: one for all of them.
fn mcp_card() -> String {
    let card = unit(Shape::WaitingMcp).mcp_waiting().first().expect("the unit waits").card.0.clone();
    card.replace(TOKEN, "u000")
}

fn keys() -> Vec<(String, Client)> {
    let one = |kind: &str, shape: Shape| id(kind, shape).replace(TOKEN, "u001");
    [
        ("home".to_owned(), Client::Ios),
        ("home".to_owned(), Client::Tui),
        (format!("home?focus={}", one("d", Shape::Review)), Client::Ios),
        (format!("card:{}", one("d", Shape::Review)), Client::Ios),
        (format!("card:{}", one("q", Shape::Asking)), Client::Tui),
        (format!("card:{}", one("d", Shape::Done)), Client::Ios),
        (format!("card:{}", one("pr", Shape::Review)), Client::Mac),
        (format!("card:{}", mcp_card()), Client::Tui),
        (format!("task:{}", one("t", Shape::WaitingMcp)), Client::Tui),
        (format!("task:{}", one("t", Shape::SessionEnded)), Client::Tui),
        (format!("document:{}", one("rev", Shape::Document)), Client::Ios),
        (format!("pull_request:{}", one("pr", Shape::Review)), Client::Ios),
        (format!("pull_request:{}", one("pr", Shape::Done)), Client::Mac),
        ("sidebar".to_owned(), Client::Mac),
        ("tasks".to_owned(), Client::Ios),
        ("tasks".to_owned(), Client::Mac),
        ("tasks".to_owned(), Client::Tui),
        ("tasks?section=finished&after=0".to_owned(), Client::Ios),
        (format!("task:{}", one("t", Shape::Running)), Client::Ios),
        (format!("task:{}", one("t", Shape::Review)), Client::Tui),
        (format!("task:{}", one("t", Shape::Done)), Client::Mac),
        (format!("request:{}", one("r", Shape::Asking)), Client::Ios),
        ("orchestrator".to_owned(), Client::Tui),
        ("schedules".to_owned(), Client::Ios),
        ("schedule:schedule-1".to_owned(), Client::Ios),
        ("new_request".to_owned(), Client::Ios),
        ("model_profile:codex-orchestrator".to_owned(), Client::Mac),
        ("model_profile:claude-implementation".to_owned(), Client::Ios),
        (format!("task_selection:{}", one("t", Shape::Running)), Client::Tui),
        ("issue_picker".to_owned(), Client::Ios),
        ("search?q=node".to_owned(), Client::Ios),
        ("machine".to_owned(), Client::Mac),
        ("mcp_servers".to_owned(), Client::Ios),
        ("mcp_servers".to_owned(), Client::Tui),
        ("accounts".to_owned(), Client::Tui),
        ("metrics?period=90d&compare=previous".to_owned(), Client::Ios),
        ("metrics?period=all&compare=release:0.3.29&project=alondra".to_owned(), Client::Mac),
    ]
    .into_iter()
    .collect()
}

#[test]
fn every_family_of_views_is_measured() {
    let measured: HashSet<Family> = keys().iter().map(|(key, _)| Family::of(key).expect("a key a view answers")).collect();
    let missing: Vec<_> = Family::ALL.iter().filter(|family| !measured.contains(family)).collect();
    assert!(missing.is_empty(), "views::build renders families the budget does not measure: {missing:?}");
}

#[test]
fn every_view_renders_a_real_size_domain_cold_within_the_budget() {
    let mut state = real_size();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "budget-commit".into(),
        expected_revision: state.revision,
        command: Command::RegisterAgent { agent_id: "budget-agent".into(), name: "Budget".into(), at: 2 },
    };
    state.execute(&Actor::System, &envelope).expect("one more commit");
    let start = Instant::now();
    let committed = ReadSnapshot::build(Arc::new(state));
    let index_ms = start.elapsed().as_secs_f64() * 1000.0;
    let (state, index) = (committed.state(), committed.index());
    let live = live(state);
    println!("{}", serde_json::json!({"tasks": state.tasks.len(), "nodes": state.agents.len(), "requests": state.requests.len(),
        "cards": state.interactions.len(), "pull_requests": state.pull_requests.len(), "waiting": index.waiting().len(),
        "index_ms": index_ms}));
    let mut over = vec![];
    for (key, client) in keys() {
        let mut samples: Vec<f64> = (0..SAMPLES).map(|_| {
            let start = Instant::now();
            let view = build(&key, RenderContext {
                domain: state, index, live: &live, client, lang: Lang::Es, transport: Transport::Loopback,
                cursor: "budget", rendered_at: "",
            });
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            if let Err(error) = view {
                panic!("{key} ({client:?}) does not render: {error:?}");
            }
            elapsed
        }).collect();
        samples.sort_by(f64::total_cmp);
        let median = samples[SAMPLES / 2];
        println!("{}", serde_json::json!({"view": key, "client": format!("{client:?}"), "median_ms": median, "max_ms": samples[SAMPLES - 1]}));
        if median > BUDGET_MS {
            over.push(format!("{key} ({client:?}): {median:.1} ms"));
        }
    }
    assert!(over.is_empty(), "views over {BUDGET_MS} ms: {over:#?}");
}

/// What each app waits for before it can draw its first card, right after a commit: the iPhone's
/// `home` and the Mac's `sidebar`, then the first card they list unless it came inside them.
#[test]
fn the_first_card_comes_with_the_first_answer() {
    let mut state = real_size();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "first-card-commit".into(),
        expected_revision: state.revision,
        command: Command::RegisterAgent { agent_id: "first-card-agent".into(), name: "First card".into(), at: 2 },
    };
    state.execute(&Actor::System, &envelope).expect("one more commit");
    let committed = ReadSnapshot::build(Arc::new(state));
    let (state, index) = (committed.state(), committed.index());
    let live = live(state);
    let render = |key: &str, client: Client| {
        let mut samples: Vec<(f64, usize, serde_json::Value)> = (0..SAMPLES).map(|_| {
            let start = Instant::now();
            let view = build(key, RenderContext {
                domain: state, index, live: &live, client, lang: Lang::Es, transport: Transport::Loopback,
                cursor: "budget", rendered_at: "",
            }).unwrap_or_else(|error| panic!("{key} ({client:?}) does not render: {error:?}"));
            let bytes = serde_json::to_vec(&view).unwrap();
            (start.elapsed().as_secs_f64() * 1000.0, bytes.len(), serde_json::from_slice(&bytes).unwrap())
        }).collect();
        samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        samples.swap_remove(SAMPLES / 2)
    };
    for (root, client) in [("home", Client::Ios), ("sidebar", Client::Mac)] {
        let (root_ms, root_bytes, view) = render(root, client);
        let first = match client {
            Client::Ios => view["sections"][0]["card_ids"][0].as_str().map(str::to_owned),
            _ => view["sections"][0]["items"][0]["link"]["key"].as_str().map(str::to_owned),
        }
        .map(|id| if id.starts_with("card:") { id } else { format!("card:{id}") })
        .expect("cards wait for the user");
        let embedded: Vec<&str> = view["first_cards"].as_array().into_iter().flatten()
            .filter_map(|card| card["key"].as_str()).collect();
        let (card_ms, card_bytes, _) = render(&first, client);
        let trips = if embedded.first() == Some(&first.as_str()) { 1 } else { 2 };
        println!("{}", serde_json::json!({"app": format!("{client:?}"), "root": root, "root_ms": root_ms,
            "root_bytes": root_bytes, "embedded": embedded.len(), "first_card": first, "card_ms": card_ms,
            "card_bytes": card_bytes, "trips_to_first_card": trips,
            "daemon_ms_to_first_card": if trips == 1 { root_ms } else { root_ms + card_ms }}));
        assert_eq!(trips, 1, "{root} ({client:?}) brings its first card: {embedded:?}");
        assert_eq!(embedded.len(), 3, "{root} ({client:?}) brings the front card and the two behind it");
    }
}

/// The number a unit whose task keeps a long journal carries.
const LONG: usize = 900;

/// The views of a domain with a task whose journal is as long as the longest real one read every
/// lifecycle as the domain keeps it: building the index and rendering every view applies no
/// event, however long a journal is.
#[test]
fn reading_every_view_replays_no_journal() {
    const TOLD: usize = 2000;
    let mut long = Seed(unit(Shape::WaitingMcp));
    let task = id("t", Shape::WaitingMcp);
    for notice in 0..TOLD {
        long.run(Actor::System, Command::TellNode { task_id: task.as_str().into(), text: format!("Synthetic notice {notice}"), from_user: false, answers: None });
    }
    let mut state = real_size();
    absorb(&mut state, &long.0, LONG);
    state.revision = state.revision.max(state.interactions.values().map(|i| i.order).max().unwrap_or(0)) + 1;
    let mut state = loaded(state);
    let long_task = id("t", Shape::WaitingMcp).replace(TOKEN, &format!("u{LONG:03}"));
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: "long-journal-commit".into(),
        expected_revision: state.revision,
        command: Command::TellNode { task_id: long_task.as_str().into(), text: "One more notice".into(), from_user: false, answers: None },
    };
    state.execute(&Actor::System, &envelope).expect("one more commit");
    let journal = state.lifecycle_journal[&long_task.as_str().into()].len();
    let state = Arc::new(state);
    let live = live(&state);
    let mut keys = keys();
    keys.extend([
        (format!("task:{long_task}"), Client::Ios),
        (format!("task:{long_task}"), Client::Tui),
    ]);
    let read = || {
        let committed = ReadSnapshot::build(Arc::clone(&state));
        for (key, client) in &keys {
            build(key, RenderContext {
                domain: committed.state(), index: committed.index(), live: &live, client: *client, lang: Lang::Es,
                transport: Transport::Loopback, cursor: "budget", rendered_at: "",
            }).unwrap_or_else(|error| panic!("{key} ({client:?}) does not render: {error:?}"));
        }
    };
    let before = alondra_domain::measure::applied();
    read();
    let applied = alondra_domain::measure::applied() - before;
    let mut samples: Vec<f64> = (0..SAMPLES).map(|_| {
        let start = Instant::now();
        read();
        start.elapsed().as_secs_f64() * 1000.0
    }).collect();
    samples.sort_by(f64::total_cmp);
    println!("{}", serde_json::json!({"journal": journal, "views": keys.len(), "events_applied": applied,
        "median_ms": samples[SAMPLES / 2], "max_ms": samples[SAMPLES - 1]}));
    assert_eq!(applied, 0, "reading the index and {} views over a journal of {journal} records applied {applied} events", keys.len());
}

/// What a task's view says of a crash report: the failure its button sends the report as, if it
/// offers one.
fn crash_report_failure(state: DomainState, shape: Shape, client: Client) -> Option<String> {
    let committed = ReadSnapshot::build(Arc::new(state));
    let (state, index) = (committed.state(), committed.index());
    let live = live(state);
    let view = build(&format!("task:{}", id("t", shape)), RenderContext {
        domain: state, index, live: &live, client, lang: Lang::En, transport: Transport::Loopback, cursor: "budget", rendered_at: "",
    }).expect("the task renders");
    let text = serde_json::to_string(&view).unwrap();
    let at = text.find(r#""local":{"kind":"crash_report","failure":""#)?;
    let rest = &text[at + r#""local":{"kind":"crash_report","failure":""#.len()..];
    Some(rest[..rest.find('"')?].to_owned())
}

#[test]
fn the_terminal_offers_a_crash_report_only_on_a_task_whose_session_ended_on_its_own() {
    assert_eq!(crash_report_failure(unit(Shape::SessionEnded), Shape::SessionEnded, Client::Tui).as_deref(), Some("session-ended"));
    for client in [Client::Ios, Client::Mac] {
        assert_eq!(crash_report_failure(unit(Shape::SessionEnded), Shape::SessionEnded, client), None, "{client:?} has no preview yet");
    }
    for shape in [Shape::WaitingMcp, Shape::Running, Shape::Done] {
        assert_eq!(crash_report_failure(unit(shape), shape, Client::Tui), None, "{shape:?}");
    }
}
