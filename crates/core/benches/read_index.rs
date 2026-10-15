use alondra_core::domain::*;
use std::time::Instant;

const UNITS: usize = 280;
const NOTICES: usize = 5;
const QUESTIONS: usize = 28;

fn id(prefix: &str, index: usize) -> String { format!("{prefix}-{index:05}") }

/// A domain the size of the real one on 15 October (301 tasks, 280 nodes, 1734 cards, 235 PRs),
/// every task open with its PR delivered: heavier than the real one, where most tasks ended.
fn real_size() -> DomainState {
    let mut template = super::inbox_graphs::templates();
    super::commit(&mut template, Actor::System, Command::PublishInformation {
        attachments: vec![], interaction_id: "template-notice".into(), scope: Scope::Task { task_id: "delivery-task".into() },
        recipient: Recipient::User, body: "Synthetic notice".into(), card: CardSpec::derived(CardKind::Know, "Read synthetic notice"),
    });
    let mut state = DomainState::new("synthetic-read-index-epoch");
    state.requests = template.requests.clone();
    let mut order = 0;
    let mut next = || { order += 1; order };
    for unit in 0..UNITS {
        let (agent_id, task_id, pr_id, delivery_id): (AgentId, TaskId, PrId, InteractionId) =
            (id("agent", unit).into(), id("task", unit).into(), id("pr", unit).into(), id("delivery", unit).into());
        let mut agent = template.agents[&"delivery-agent".into()].clone();
        agent.id = agent_id.clone(); agent.name = agent_id.0.clone();
        state.agents.insert(agent_id.clone(), agent);
        let mut task = template.tasks[&"delivery-task".into()].clone();
        task.id = task_id.clone(); task.agent_id = Some(agent_id.clone()); task.title = format!("Task {unit}");
        task.final_presentation = Some(delivery_id.clone()); task.pr_id = Some(pr_id.clone());
        state.tasks.insert(task_id.clone(), task);
        let mut pull_request = template.pull_requests[&"template-pr".into()].clone();
        pull_request.id = pr_id.clone(); pull_request.task_id = task_id.clone(); pull_request.number = unit as u64 + 1;
        state.pull_requests.insert(pr_id.clone(), pull_request);
        let mut delivery = template.interactions[&"template-delivery".into()].clone();
        delivery.id = delivery_id.clone(); delivery.scope = Scope::Task { task_id: task_id.clone() };
        delivery.author = Author::Agent { agent_id: agent_id.clone() };
        if let InteractionPayload::Delivery { targets, .. } = &mut delivery.payload {
            *targets = vec![DeliveryTarget::PullRequest { pr_id: pr_id.clone(), head: "fixture-head".into() }];
        }
        delivery.card = CardSpec::delivery("Review synthetic result", match &delivery.payload {
            InteractionPayload::Delivery { targets, .. } => targets,
            _ => unreachable!(),
        }, true);
        delivery.order = next(); delivery.revision = delivery.order;
        state.interactions.insert(delivery_id, delivery);
        for notice in 0..NOTICES {
            let mut item = template.interactions[&"template-notice".into()].clone();
            item.id = format!("notice-{unit:05}-{notice}").into(); item.scope = Scope::Task { task_id: task_id.clone() };
            if (unit * NOTICES + notice) % 10 != 0
                && let InteractionPayload::Information { read_by } = &mut item.payload {
                read_by.insert("fixture-user".into());
            }
            item.order = next(); item.revision = item.order;
            state.interactions.insert(item.id.clone(), item);
        }
    }
    for question in 0..QUESTIONS {
        let held = question * (UNITS / QUESTIONS);
        let agent_id: AgentId = id("asking-agent", question).into();
        let task_id: TaskId = id("asking-task", question).into();
        let mut agent = template.agents[&"question-agent".into()].clone();
        agent.id = agent_id.clone(); agent.name = agent_id.0.clone();
        state.agents.insert(agent_id.clone(), agent);
        let mut task = template.tasks[&"question-task".into()].clone();
        task.id = task_id.clone(); task.agent_id = Some(agent_id.clone()); task.title = format!("Asking {question}");
        task.dependencies = [TaskId::from(id("task", held))].into();
        state.tasks.insert(task_id.clone(), task);
        let mut item = template.interactions[&"template-question".into()].clone();
        item.id = id("question", question).into(); item.scope = Scope::Task { task_id };
        item.author = Author::Agent { agent_id };
        item.card.blocks = vec![CardTarget::PullRequest { pr_id: id("pr", held).into() }];
        item.withdrawn = question % 2 == 1;
        item.order = next(); item.revision = item.order;
        state.interactions.insert(item.id.clone(), item);
    }
    state.revision = order + 1;
    for task in state.tasks.values_mut() { task.revision = state.revision; }
    state.validate().unwrap();
    state
}

pub(super) fn profile() {
    let state = real_size();
    let index = ReadIndex::build(&state);
    assert_eq!(index.waiting().iter().map(|item| item.id.clone()).collect::<Vec<_>>(),
        state.inbox_projection().into_iter().filter(|item| item.pending).map(|item| item.id).collect::<Vec<_>>(),
        "the index lists what the projection lists");
    let runs = std::env::args().find_map(|argument| argument.strip_prefix("--samples=").and_then(|n| n.parse().ok())).unwrap_or(30);
    let mut samples: Vec<f64> = (0..runs).map(|_| {
        let start = Instant::now();
        std::hint::black_box(ReadIndex::build(&state));
        start.elapsed().as_secs_f64() * 1000.0
    }).collect();
    samples.sort_by(f64::total_cmp);
    let start = Instant::now();
    std::hint::black_box(state.inbox_projection());
    let projection_ms = start.elapsed().as_secs_f64() * 1000.0;
    let median = samples[samples.len() / 2];
    println!("{}", serde_json::json!({"scope": "read-index-build", "tasks": state.tasks.len(), "nodes": state.agents.len(),
        "cards": state.interactions.len(), "pull_requests": state.pull_requests.len(), "waiting": index.waiting().len(),
        "queued": index.queue().len(), "median_ms": median, "max_ms": samples[samples.len() - 1],
        "inbox_projection_ms": projection_ms}));
    if std::env::args().any(|argument| argument == "--check-cost") {
        assert!(median <= 10.0, "building the read index of a real-size domain takes {median:.1} ms, over 10 ms");
    }
}
