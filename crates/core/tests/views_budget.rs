//! Every family of views renders a domain the size of the real one in under 50 ms, cold, right
//! after a commit: what a row needs is read from the committed index, never by walking the domain
//! once per row.
use alondra_core::domain::{
    Actor, CardKind, CardSpec, Command, CommandEnvelope, DeliveryTarget, DomainState, InteractionId, LaunchPlan,
    MergeMethod, NodeEvent, PROTOCOL_VERSION, PrState, QuestionOption, ReadSnapshot, Recipient, Schedule, ScheduleFrequency,
    Scope,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use alondra_core::views::{Client, Family, Lang, LiveNode, LivePullRequest, LiveSnapshot, RenderContext, Transport, build};
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
        }
    }
}

/// The real domain on 15 October: ~300 tasks, ~280 nodes, ~300 requests, ~1700 cards, ~250
/// deliveries with a PR and 12 cards waiting for the user.
const UNITS: [(Shape, usize); 7] = [
    (Shape::Done, 230),
    (Shape::Document, 10),
    (Shape::Review, 6),
    (Shape::Asking, 6),
    (Shape::Running, 24),
    (Shape::Cancelled, 10),
    (Shape::Defining, 14),
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
    seed.run(Actor::Orchestrator { generation: 0 }, Command::CreateTask {
        task_id: task.as_str().into(), request_id: request.as_str().into(), title: format!("Synthetic {} task {TOKEN}", shape.name()),
        agent_id: Some(agent.as_str().into()), review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/synthetic/project")),
    });
    seed.run(node.clone(), Command::StartTask { task_id: task.as_str().into() });
    seed.run(Actor::System, Command::MoveNode { agent_id: agent.as_str().into(), event: NodeEvent::TurnStarted, at: 1 });
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
        Shape::Cancelled => seed.run(user(), Command::CancelTask { task_id: task.as_str().into() }),
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
        data_epoch: _, revision, orchestrator_generation: _, orchestrator_session: _, requests, agents, tasks, documents,
        document_revisions, document_reviews, pull_requests, mut interactions, merge_authorizations, stack_readiness, schedule,
        holds, checklists, checklist_marks, last_words, request_plans,
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
            id: format!("schedule-{index}").into(), title: format!("Synthetic schedule {index}"), text: "Review the project".into(),
            project: Some("project".into()), options: BTreeMap::new(), disallowed_tools: vec![], frequency: ScheduleFrequency::Daily,
            hour: 9, minute: 0, weekday: None, interval: None, start_at: None, enabled: true, next_run: Some(100), revision: 1,
            deleted_at: None,
        };
        state.schedule.schedules.insert(schedule.id.clone(), schedule);
    }
    state.revision = state.revision.max(state.interactions.values().map(|i| i.order).max().unwrap_or(0)) + 1;
    state.validate().expect("the synthetic domain is valid");
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
        checks_passed: Some(true),
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
        ..LiveSnapshot::default()
    }
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
        ("fleet".to_owned(), Client::Ios),
        ("fleet".to_owned(), Client::Tui),
        (format!("node:{}", one("a", Shape::Running)), Client::Ios),
        (format!("node:{}", one("a", Shape::Done)), Client::Tui),
        ("schedules".to_owned(), Client::Ios),
        ("schedule:schedule-1".to_owned(), Client::Ios),
        ("new_request".to_owned(), Client::Ios),
        ("issue_picker".to_owned(), Client::Ios),
        ("search?q=node".to_owned(), Client::Ios),
        ("machine".to_owned(), Client::Mac),
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
