use alondra_core::domain::*;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Fixture {
    pub state: DomainState,
    pub blockers: BTreeMap<InteractionId, Vec<InteractionId>>,
    pub order: Vec<InteractionId>,
}

fn delivery(index: usize) -> InteractionId { format!("delivery-{index:05}").into() }
fn question(index: usize) -> InteractionId { format!("question-{index:05}").into() }
fn delivery_task(index: usize) -> TaskId { format!("task-delivery-{index:05}").into() }
fn question_task(index: usize) -> TaskId { format!("task-question-{index:05}").into() }
fn pr(index: usize) -> PrId { format!("pr-{index:05}").into() }

pub(super) fn templates() -> DomainState {
    let mut state = DomainState::new("synthetic-inbox-graph-epoch");
    for id in ["delivery-agent", "question-agent"] {
        super::commit(&mut state, Actor::System, Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: 0 });
    }
    super::commit(&mut state, super::user(), Command::CreateRequest {
        request_id: "request".into(), body: "Synthetic graph".into(), auto_merge: false, maintenance: false, message: false,
    });
    super::commit(&mut state, super::user(), Command::TitleRequest { request_id: "request".into(), title: "Synthetic graph".into() });
    super::commit(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "delivery-task".into(), request_id: "request".into(), title: "Synthetic delivery".into(),
        agent_id: Some("delivery-agent".into()), review_prs: true, dependencies: BTreeSet::new(), stack: None,
        launch: Some(LaunchPlan::new("node", "/synthetic")),
    });
    super::commit(&mut state, Actor::Agent { agent_id: "delivery-agent".into() }, Command::RegisterPr {
        task_id: "delivery-task".into(), pr_id: "template-pr".into(), host: "github.com".into(), repository: "synthetic/fixture".into(),
        number: 1, head: "fixture-head".into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
    });
    let targets = vec![DeliveryTarget::PullRequest { pr_id: "template-pr".into(), head: "fixture-head".into() }];
    let card = CardSpec::delivery("Review synthetic result", &targets, true);
    super::commit(&mut state, Actor::Agent { agent_id: "delivery-agent".into() }, Command::PresentResult {
        checklist: Vec::new(),
        attachments: vec![], task_id: "delivery-task".into(), interaction_id: "template-delivery".into(), body: "Synthetic result".into(),
        targets, is_final: true, nothing_to_review: false, card,
    });
    super::commit(&mut state, Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None,
        task_id: "question-task".into(), request_id: "request".into(), title: "Synthetic question".into(),
        agent_id: Some("question-agent".into()), review_prs: true, dependencies: BTreeSet::from(["delivery-task".into()]), stack: None,
        launch: Some(LaunchPlan::new("node", "/synthetic")),
    });
    let options = vec![QuestionOption { id: "yes".into(), label: "Yes".into(), description: None },
        QuestionOption { id: "no".into(), label: "No".into(), description: None }];
    let mut card = CardSpec::question("Choose a synthetic option", &options);
    card.blocks = vec![CardTarget::Delivery { interaction_id: "template-delivery".into() }];
    super::commit(&mut state, Actor::Agent { agent_id: "question-agent".into() }, Command::AskQuestion {
        attachments: vec![], interaction_id: "template-question".into(), scope: Scope::Task { task_id: "question-task".into() },
        recipient: Recipient::User, body: "Choose a synthetic option".into(), blocking: true, options, card,
    });
    state.validate().unwrap();
    state
}

pub(super) fn fixture(shape: &str, count: usize, body_size: usize) -> Fixture {
    assert!(count >= 2 && body_size > 0);
    if shape == "unblocked" {
        let state = super::inbox_fixture(count, body_size, 0);
        let order: Vec<InteractionId> = (0..count).map(|index| format!("information-{index:05}").into()).collect();
        let blockers = order.iter().cloned().map(|id| (id, vec![])).collect();
        return Fixture { state, blockers, order };
    }
    let (deliveries, targets, stacks): (usize, Vec<Vec<usize>>, Vec<Option<usize>>) = match shape {
        "stack-chain" => (count, vec![], (0..count).map(|index| index.checked_sub(1)).collect()),
        "fanout" => {
            let questions = count.div_ceil(17);
            let deliveries = count - questions;
            let mut next = 0;
            let targets = (0..questions).map(|index| {
                let size = 16.min(deliveries - next - (questions - index - 1));
                let held = (next..next + size).collect(); next += size; held
            }).collect();
            (deliveries, targets, vec![None; deliveries])
        }
        "fanin" => (1, vec![vec![0]; count - 1], vec![None]),
        "mixed" => {
            assert_eq!(count, 5);
            (3, vec![vec![0, 1], vec![1, 2]], vec![None, Some(0), Some(1)])
        }
        _ => panic!("unknown graph shape {shape}"),
    };
    let template = templates();
    let questions = targets.len();
    let mut state = DomainState::new("synthetic-inbox-graph-epoch");
    state.requests = template.requests.clone();
    state.revision = template.revision + count as u64;
    let mut blockers: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for index in 0..deliveries {
        let agent_id: AgentId = format!("agent-delivery-{index:05}").into();
        let mut agent = template.agents[&"delivery-agent".into()].clone();
        agent.id = agent_id.clone(); agent.name = agent_id.0.clone();
        state.agents.insert(agent.id.clone(), agent);
        let mut task = template.tasks[&"delivery-task".into()].clone();
        task.id = delivery_task(index); task.agent_id = Some(agent_id.clone()); task.title = format!("Delivery {index}");
        task.revision = state.revision; task.final_presentation = Some(delivery(index)); task.pr_id = Some(pr(index));
        task.stack = stacks[index].map(|upstream| StackDependency { upstream_task: delivery_task(upstream),
            presentation_id: delivery(upstream), pr_id: pr(upstream), head: "fixture-head".into() });
        state.tasks.insert(task.id.clone(), task);
        let mut pull_request = template.pull_requests[&"template-pr".into()].clone();
        pull_request.id = pr(index); pull_request.task_id = delivery_task(index); pull_request.number = index as u64 + 1;
        state.pull_requests.insert(pull_request.id.clone(), pull_request);
        let mut item = template.interactions[&"template-delivery".into()].clone();
        item.id = delivery(index); item.scope = Scope::Task { task_id: delivery_task(index) }; item.author = Author::Agent { agent_id };
        item.body = "x".repeat(body_size);
        let position = if matches!(shape, "stack-chain" | "mixed") { deliveries - index } else { index + 1 };
        item.order = (questions + position) as u64; item.revision = item.order;
        if let InteractionPayload::Delivery { targets, .. } = &mut item.payload {
            *targets = vec![DeliveryTarget::PullRequest { pr_id: pr(index), head: "fixture-head".into() }];
        }
        blockers.insert(item.id.clone(), vec![]);
        state.interactions.insert(item.id.clone(), item);
    }
    for (index, held) in targets.iter().enumerate() {
        assert!(!held.is_empty() && held.len() <= 16);
        let agent_id: AgentId = format!("agent-question-{index:05}").into();
        let mut agent = template.agents[&"question-agent".into()].clone();
        agent.id = agent_id.clone(); agent.name = agent_id.0.clone();
        state.agents.insert(agent.id.clone(), agent);
        let mut task = template.tasks[&"question-task".into()].clone();
        task.id = question_task(index); task.agent_id = Some(agent_id.clone()); task.title = format!("Question {index}");
        task.revision = state.revision; task.dependencies = held.iter().map(|index| delivery_task(*index)).collect();
        state.tasks.insert(task.id.clone(), task);
        let mut item = template.interactions[&"template-question".into()].clone();
        item.id = question(index); item.scope = Scope::Task { task_id: question_task(index) }; item.author = Author::Agent { agent_id };
        item.body = "x".repeat(body_size); item.order = index as u64 + 1; item.revision = item.order;
        item.card.blocks = held.iter().map(|index| CardTarget::Delivery { interaction_id: delivery(*index) }).collect();
        for &at in held { blockers.get_mut(&delivery(at)).unwrap().push(item.id.clone()); }
        blockers.insert(item.id.clone(), vec![]);
        state.interactions.insert(item.id.clone(), item);
    }
    for (index, upstream) in stacks.iter().enumerate() {
        if let Some(upstream) = upstream { blockers.get_mut(&delivery(index)).unwrap().push(delivery(*upstream)); }
    }
    let order = match shape {
        "stack-chain" => (0..deliveries).map(delivery).collect(),
        "fanout" => targets.iter().enumerate().flat_map(|(index, held)| std::iter::once(question(index)).chain(held.iter().copied().map(delivery))).collect(),
        "fanin" => (0..questions).map(question).chain(std::iter::once(delivery(0))).collect(),
        "mixed" => vec![question(0), delivery(0), question(1), delivery(1), delivery(2)],
        _ => unreachable!(),
    };
    Fixture { state, blockers, order }
}

pub(super) fn delivery_blockers(state: &DomainState) -> Vec<(InteractionId, Vec<InteractionId>)> {
    state.interactions.values().filter(|item| matches!(item.payload, InteractionPayload::Delivery { .. }))
        .map(|item| (item.id.clone(), state.delivery_blocked_by(item))).collect()
}

// Frozen pre-index rank and recursive scan; this oracle never calls production ordering.
fn rank(item: &Interaction) -> u8 {
    match item.card.kind {
        CardKind::Problem => 0,
        CardKind::Authorize | CardKind::Do | CardKind::Decide | CardKind::Provide if item.blocking => 1,
        CardKind::Authorize | CardKind::Do | CardKind::Decide | CardKind::Review | CardKind::Test => 2,
        CardKind::Provide => 3, CardKind::Propose => 4, CardKind::Know => 5,
    }
}

impl Fixture {
    pub fn orders(&self) -> Vec<(String, Revision)> {
        self.order.iter().map(|id| (format!("interaction:{id}"), self.state.interactions[id].order)).collect()
    }

    pub fn inspect(&self) -> serde_json::Value {
        self.state.validate().unwrap();
        assert_eq!(self.state.interactions.len(), self.order.len());
        let mut owners = BTreeSet::new();
        for task in self.state.tasks.values() {
            assert!(owners.insert(task.agent_id.as_ref().unwrap()));
        }
        for item in self.state.interactions.values() {
            assert!(item.recipient == Recipient::User && self.state.open(item));
            assert!(item.card.blocks.len() <= 16);
            let Scope::Task { task_id } = &item.scope else { panic!("fixture card without task"); };
            let task = &self.state.tasks[task_id];
            for target in &item.card.blocks {
                let CardTarget::Delivery { interaction_id } = target else { panic!("fixture target kind"); };
                let Scope::Task { task_id: target_task } = &self.state.interactions[interaction_id].scope else { unreachable!() };
                assert!(target_task == task_id || task.dependencies.contains(target_task)
                    || task.stack.as_ref().is_some_and(|stack| &stack.upstream_task == target_task));
            }
        }
        let actual = delivery_blockers(&self.state);
        for (id, held) in &actual { assert_eq!(held, &self.blockers[id], "actual adjacency for {id}"); }
        let mut sorted: Vec<_> = self.state.interactions.values().collect();
        sorted.sort_by_key(|item| (rank(item), item.order, format!("interaction:{}", item.id)));
        let positions: BTreeMap<_, _> = sorted.iter().enumerate().map(|(at, item)| (item.id.clone(), at)).collect();
        let blockers: Vec<Vec<usize>> = sorted.iter().map(|item| {
            let mut held: Vec<_> = self.blockers[&item.id].iter().map(|id| positions[id]).collect(); held.sort_unstable(); held
        }).collect();
        let mut counters = [0_u64; 4];
        let mut placed = vec![false; sorted.len()]; let mut sequence = vec![];
        fn place(at: usize, blockers: &[Vec<usize>], placed: &mut [bool], sequence: &mut Vec<usize>, counts: &mut [u64; 4], depth: u64) {
            if placed[at] { return; }
            placed[at] = true; counts[3] = counts[3].max(depth);
            for &blocker in &blockers[at] { place(blocker, blockers, placed, sequence, counts, depth + 1); }
            sequence.push(at);
            for (dependent, held) in blockers.iter().enumerate() {
                counts[0] += 1;
                if placed[dependent] { continue; }
                counts[1] += 1;
                if !held.contains(&at) { continue; }
                let ready = held.iter().all(|blocker| { counts[2] += 1; placed[*blocker] });
                if ready { place(dependent, blockers, placed, sequence, counts, depth + 1); }
            }
        }
        for at in 0..sorted.len() { place(at, &blockers, &mut placed, &mut sequence, &mut counters, 1); }
        let legacy: Vec<_> = sequence.into_iter().map(|at| sorted[at].id.clone()).collect();
        assert_eq!(legacy, self.order, "frozen legacy permutation must match the literal fixture order");
        let edges: usize = self.blockers.values().map(Vec::len).sum();
        let distinct_edges: usize = self.blockers.values().map(|held| held.iter().collect::<BTreeSet<_>>().len()).sum();
        let deliveries = actual.len();
        let targets: usize = self.state.interactions.values().map(|item| item.card.blocks.len()).sum();
        let questions = self.state.interactions.values().filter(|item| matches!(item.payload, InteractionPayload::Question { .. })).count();
        serde_json::json!({"entries": self.order.len(), "deliveries": deliveries, "questions": questions, "information": self.order.len() - deliveries - questions,
            "blocker_positions": edges, "distinct_edges": distinct_edges, "explicit_card_targets": targets,
            "max_blockers": self.blockers.values().map(Vec::len).max().unwrap_or(0),
            "legacy_counted_row_visits": counters[0], "legacy_counted_contains_calls": counters[1],
            "legacy_counted_readiness_positions": counters[2], "legacy_counted_max_depth": counters[3],
            "source_model_index_candidate_visits": distinct_edges,
            "source_model_lookup_outer_visits": deliveries * self.order.len(),
            "source_model_lookup_target_filter_visits": deliveries * targets * self.order.len(),
            "source_model_lookup_delivery_matches": deliveries * targets * deliveries,
            "counter_limits": "Legacy counters are a separate frozen procedure; lookup/index fields are source-derived iteration models, not production instrumentation or timing."})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn graph_fixtures_match_literal_legacy_orders_and_actual_edges() {
        for (shape, count, expected, edges) in [
            ("stack-chain", 4, vec![delivery(0), delivery(1), delivery(2), delivery(3)], 3),
            ("fanout", 5, vec![question(0), delivery(0), delivery(1), delivery(2), delivery(3)], 4),
            ("fanin", 4, vec![question(0), question(1), question(2), delivery(0)], 3),
            ("mixed", 5, vec![question(0), delivery(0), question(1), delivery(1), delivery(2)], 6),
        ] {
            let fixture = fixture(shape, count, 64);
            assert_eq!(fixture.order, expected, "{shape}");
            let stats = fixture.inspect();
            assert_eq!(stats["distinct_edges"], edges);
            assert_eq!(fixture.state.inbox_orders(), fixture.orders(), "{shape}");
        }
    }
    #[test]
    fn fanout_respects_the_target_limit_and_interleaves_ready_groups() {
        let fixture = fixture("fanout", 20, 64);
        assert_eq!(fixture.order, std::iter::once(question(0)).chain((0..16).map(delivery))
            .chain(std::iter::once(question(1))).chain((16..18).map(delivery)).collect::<Vec<_>>());
        let stats = fixture.inspect();
        assert_eq!(stats["entries"], 20); assert_eq!(stats["deliveries"], 18); assert_eq!(stats["distinct_edges"], 18);
        assert_eq!(fixture.state.inbox_orders(), fixture.orders());
        let spill = super::fixture("fanout", 18, 64);
        assert_eq!(spill.order, std::iter::once(question(0)).chain((0..15).map(delivery))
            .chain([question(1), delivery(15)]).collect::<Vec<_>>());
        assert_eq!(spill.inspect()["distinct_edges"], 16);
        assert_eq!(spill.state.inbox_orders(), spill.orders());
    }
}
