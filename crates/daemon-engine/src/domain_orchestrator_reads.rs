//! The orchestrator's reads, as the `tasks_in_progress` and `node` tools give them: the live
//! sessions the daemon observes next to the domain's Requests, Tasks, questions, results and
//! undelivered messages. Each answer has the shape the MCP's summaries already read.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{body_title, is_stall_case, Agent, AgentId, Author, DomainError, DomainState, Interaction, InteractionPayload, NodePhase, PauseReason, Recipient, Scope, TaskActivity, TaskId, NODE_PAUSE_PREFIX};
use alondra_domain::{Capacity, LiveState, Step, Worker};
use alondra_core::Node;
use serde_json::{Value, json};

fn state(domain: &DomainRuntime) -> Result<std::sync::Arc<DomainState>, DomainError> {
    domain.state().map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))
}

fn wire<T: serde::Serialize>(value: &T) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// A live session as the legacy overview listed it, named after its Agent.
fn model_of(domain: &DomainRuntime, session_id: &str) -> Value {
    json!(domain.live.mods.session(session_id).and_then(|reported| reported.model))
}

/// A node's state as the node machine holds it, the one every screen reads: its phase and why.
fn phase_fields(row: &mut Value, phase: &NodePhase) {
    let since = |at: alondra_core::Millis| crate::log::local_iso(at.div_euclid(1000) as libc::time_t, at.rem_euclid(1000) as u32);
    row["state"] = json!(match phase {
        NodePhase::Closing | NodePhase::Closed => "done",
        phase => phase.key(),
    });
    row["alive"] = json!(phase.live() && *phase != NodePhase::Lost);
    row["evidence"] = json!(match phase {
        NodePhase::Starting { reviving: false, .. } => "admitted by the gate; no turn yet".to_owned(),
        NodePhase::Starting { reviving: true, .. } => "its lost session is being restarted".to_owned(),
        NodePhase::Lost => "its session is gone; it waits for the gate to be restarted".to_owned(),
        NodePhase::Working => "a turn is running".to_owned(),
        NodePhase::WaitingBackground(work) => format!("its turn ended behind its own background work (`{}`, since {}{}); it wakes when that ends",
            work.label, since(work.since), if work.others > 0 { format!(", {} more", work.others) } else { String::new() }),
        NodePhase::WaitingUser { .. } => "a card of its task waits for an answer: the user's, or yours to a question it asked you".to_owned(),
        NodePhase::Paused(pause) => format!("paused ({}) since {}", match &pause.reason {
            PauseReason::Memory => "memory".to_owned(),
            PauseReason::Declared { reason } => format!("it waits for the machine: {reason}"),
            PauseReason::Usage => "the usage limit".to_owned(),
            PauseReason::Disk => "the disk fell under its floor; it stopped until there is room".to_owned(),
            PauseReason::ByUser => "by the user".to_owned(),
            PauseReason::ApiError(retry) => format!("a Claude API error, {}", retry.error),
        }, since(pause.since)),
        NodePhase::Delivered { .. } => "its result waits for the user's review".to_owned(),
        NodePhase::Resuming { .. } => "the user's answer is on its way to it and starts its next turn".to_owned(),
        NodePhase::WaitingDependency => "its task waits for another task to be done".to_owned(),
        NodePhase::NoTask => "its turn ended and it has no open task".to_owned(),
        NodePhase::Closing => "its task is over; it cleans up".to_owned(),
        NodePhase::Closed => "its session is closed".to_owned(),
        NodePhase::ThroughSdk => "it runs through the Agent SDK".to_owned(),
    });
}

/// A node of the Agent SDK as its task's lifecycle holds it: what its task does or who it waits on,
/// and whether its session runs. Its phase says nothing of it.
fn live_fields(row: &mut Value, state: &DomainState, agent: &AgentId) {
    let live = state.sdk_live(agent);
    row["state"] = json!(match &live {
        Some(live) => TaskActivity::Live(live.clone()).key(),
        None if state.node_task(agent).is_some_and(|task| task.over()) => "done",
        None => "no_task",
    });
    row["alive"] = json!(live.is_some() && state.sdk_session_runs(agent));
    row["evidence"] = json!(match &live {
        Some(LiveState::Working(Worker::Turn)) => "a turn is running",
        Some(LiveState::Working(Worker::OwnWork)) => "its turn ended behind work of its own; it wakes when that ends",
        Some(LiveState::Working(Worker::AfterMerge)) => "its PR merged and it runs the work it declared for after the merge",
        Some(LiveState::Working(Worker::Alondra(Step::Starting))) => "Alondra is starting its session",
        Some(LiveState::Working(Worker::Alondra(Step::Delivering))) => "Alondra is handing it an input it has not read yet",
        Some(LiveState::Working(Worker::Alondra(Step::Waking))) => "Alondra is waking it: work it waited on ended",
        Some(LiveState::Working(Worker::Alondra(Step::Answering))) if state.node_task(agent).is_some_and(|task| state.interactions.values()
            .any(|i| is_stall_case(i) && state.open(i) && i.scope == (Scope::Task { task_id: task.id.clone() }))) =>
            "it stopped with nothing explaining its next step: settle its case with settle_stall",
        Some(LiveState::Working(Worker::Alondra(Step::Answering))) => "it waits for your answer to what it asked you",
        Some(LiveState::Working(Worker::Alondra(Step::Merging))) => "Alondra merges its PR",
        Some(LiveState::Working(Worker::Alondra(Step::Finishing))) => "its task is over; Alondra closes it",
        Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| state.node_pause_card(card)) =>
            "the user paused it and only the user resumes it",
        Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| state.node_task(agent).and_then(|task| task.final_presentation.as_ref()) == Some(card)) =>
            "its result waits for the user's review",
        Some(LiveState::WaitingUser(_)) => "a card of its task waits for the user's answer",
        Some(LiveState::WaitingCapacity(Capacity::Gate)) => "it waits for a slot in the gate",
        Some(LiveState::WaitingCapacity(Capacity::Machine)) => "it said it waits for the machine; the gate resumes it once the Mac has room",
        Some(LiveState::WaitingCapacity(Capacity::Disk)) => "it waits for room on the disk for one more build",
        Some(LiveState::WaitingCapacity(Capacity::Usage)) => "it waits for the usage limit to renew",
        Some(LiveState::WaitingCapacity(Capacity::ClaudeApi)) => "it waits for Claude's API to answer again",
        Some(LiveState::WaitingTask(_)) => "its task waits for another task to be done",
        None => "it has no open task",
    });
}

fn node_row(node: &Node, state: &DomainState, domain: &DomainRuntime) -> Value {
    let mut row = wire(node);
    let agent = AgentId(node.session_id.clone());
    if state.node_phase(&agent) == Some(&NodePhase::ThroughSdk) || state.through_sdk(&agent) {
        live_fields(&mut row, state, &agent);
    } else if let Some(phase) = state.node_phase(&agent) {
        phase_fields(&mut row, phase);
    }
    row["model"] = model_of(domain, &node.session_id);
    if node.name.is_none() {
        row["name"] = json!(state.agents.get(&AgentId(node.session_id.clone())).map(|a| a.name.clone()));
    }
    row["project"] = json!(node.project());
    row
}

/// A node of the Agent SDK, which no hook session describes: the domain's phase of it, its task
/// as the domain holds it, whether this daemon runs its host, and what its host's events said last.
fn sdk_node_row(agent: &Agent, state: &DomainState, domain: &DomainRuntime) -> Value {
    let reported = domain.live.mods.session(&agent.id.0).unwrap_or_default();
    let mut node = Node::new(&agent.id.0, agent.since);
    node.alive = false;
    node.name = Some(agent.name.clone());
    node.cwd = reported.cwd.clone();
    node.tool = reported.tool.clone();
    node.busy = matches!(state.sdk_live(&agent.id), Some(LiveState::Working(Worker::Turn)));
    node.last_message = reported.answer.as_ref().map(|answer| answer.text.clone());
    node.last_message_at = reported.answer.as_ref().map(|answer| answer.at);
    node.background = reported.background.iter().map(|(_, item)| item.clone()).collect();
    let mut row = node_row(&node, state, domain);
    row["sdk_host"] = json!(domain.owns_sdk_host(&agent.id));
    row["last_event_at"] = json!(reported.last_event_at);
    row["task"] = json!(state.node_task(&agent.id).map(|task| json!({
        "id": task.id, "state": wire(&task.state), "activity": state.task_activity(task).map(|activity| activity.key()),
    })));
    row
}

/// Every node the domain knows: a hook session's, or one of the Agent SDK, which has none.
fn node_rows(domain: &DomainRuntime, state: &DomainState) -> Vec<Value> {
    let live = domain.live.nodes();
    let mut rows: Vec<Value> = live.iter()
        .filter(|n| state.agents.contains_key(&AgentId(n.session_id.clone())))
        .map(|n| node_row(n, state, domain)).collect();
    rows.extend(state.agents.values()
        .filter(|agent| !live.iter().any(|n| n.session_id == agent.id.0) && crate::sdk_owner::runs_through_sdk(state, &agent.id))
        .map(|agent| sdk_node_row(agent, state, domain)));
    rows
}

fn author_session(i: &Interaction) -> Option<&str> {
    match &i.author {
        Author::Agent { agent_id } => Some(agent_id.0.as_str()),
        _ => None,
    }
}

fn author_name(i: &Interaction, state: &DomainState) -> String {
    match &i.author {
        Author::Agent { agent_id } => state.agents.get(agent_id).map_or_else(|| agent_id.0.clone(), |a| a.name.clone()),
        Author::Orchestrator { .. } => "orchestrator".into(),
        _ => "Alondra".into(),
    }
}

/// A question or action request, open until answered or withdrawn.
fn question(i: &Interaction, state: &DomainState) -> Option<Value> {
    let (options, answer, action) = match &i.payload {
        InteractionPayload::Question { options, answer } => (options, answer, false),
        InteractionPayload::ActionRequest { options, answer } => (options, answer, true),
        _ => return None,
    };
    let status = if i.withdrawn { "withdrawn" } else if answer.is_some() { "answered" } else { "open" };
    Some(json!({"id": i.id, "title": body_title(&i.body), "detail": i.body, "blocking": i.blocking,
        "options": options.iter().map(|o| json!({"label": o.label})).collect::<Vec<_>>(),
        "to_orchestrator": i.recipient == Recipient::Orchestrator, "action": action, "status": status,
        "answer": answer.as_ref().map(wire), "node_name": author_name(i, state), "session_id": author_session(i)}))
}

/// Information and presentations: what 8ccd131 called reports, read once acted on.
fn report(i: &Interaction, state: &DomainState) -> Option<Value> {
    let (kind, read) = match &i.payload {
        InteractionPayload::Information { read_by } => ("notice", !read_by.is_empty()),
        InteractionPayload::Delivery { is_final, acceptance, feedback, .. } =>
            (if *is_final { "final" } else { "partial" }, acceptance.is_some() || !feedback.is_empty()),
        _ => return None,
    };
    Some(json!({"id": i.id, "title": body_title(&i.body), "kind": kind, "body": i.body, "read": read,
        "node_name": author_name(i, state), "session_id": author_session(i)}))
}

fn tasks(state: &DomainState) -> Vec<Value> {
    state.tasks.values().filter(|t| !t.archived).map(|t| json!({
        "id": t.id, "title": t.title, "state": wire(&t.state), "activity": state.task_activity(t).map(|activity| activity.key()), "request": t.request_id,
        "depends_on": t.dependencies, "assignee": t.agent_id.as_ref().map(|a| json!({"session_id": a})),
        "delivered_dependencies": t.delivered_dependencies, "blocks": t.blocks,
        "blocked_by": state.open_blockers(&t.id), "verdict": t.verdict, "review_rounds": t.review_rounds,
    })).collect()
}

fn requests(state: &DomainState) -> Vec<Value> {
    state.requests.values().map(|r| {
        let has_tasks = state.tasks.values().any(|t| t.request_id == r.id);
        let phase = if r.closed { "done" } else if has_tasks { "working" } else { "defining" };
        json!({"id": r.id, "state": phase, "text": r.body, "title": r.title})
    }).collect()
}

/// The questions that wait on the user or the orchestrator.
fn open_questions(state: &DomainState) -> Vec<Value> {
    state.interactions.values()
        .filter(|i| matches!(i.recipient, Recipient::User | Recipient::Orchestrator) && !state.out_of_sight(i))
        .filter_map(|i| question(i, state))
        .filter(|q| q["status"] == "open").collect()
}

/// `tasks_in_progress`: the live Agents and everything that waits on someone, in the legacy overview's shape.
pub fn tasks_in_progress(domain: &DomainRuntime, machine: Value, usage: Value) -> Result<Value, DomainError> {
    let state = state(domain)?;
    let nodes = node_rows(domain, &state);
    let reports: Vec<Value> = state.interactions.values().filter(|i| i.recipient == Recipient::User).filter_map(|i| report(i, &state)).collect();
    let pending = domain.store.lock_or_exit("the domain store").pending_messages()
        .map_err(|_| DomainError::new("storage_unavailable", "outbox unavailable"))?;
    let orchestrator_messages: Vec<Value> = pending.iter().filter(|m| m.recipient_kind == "orchestrator")
        .map(|m| json!({"id": m.id, "text": m.message, "handed_at": null})).collect();
    Ok(json!({"nodes": nodes, "open_questions": open_questions(&state), "reports": reports, "orchestrator_messages": orchestrator_messages,
        "requests": requests(&state), "tasks": tasks(&state), "machine": machine, "usage": usage}))
}

/// The Agent a name, short id or session id names.
fn agent_named(domain: &DomainRuntime, state: &DomainState, key: &str) -> Result<AgentId, DomainError> {
    let live = domain.live.nodes();
    let short = |id: &AgentId| live.iter().find(|n| n.session_id == id.0).map(|n| n.short_id.clone());
    let found: Vec<&AgentId> = state.agents.values()
        .filter(|a| a.id.0 == key || a.name == key || short(&a.id).is_some_and(|s| !s.is_empty() && s == key))
        .map(|a| &a.id).collect();
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(DomainError::new("not_found", format!("no agent is called {key}"))),
        _ => Err(DomainError::new("conflict", format!("{key} names several agents; use its session id"))),
    }
}

/// `node`: one Agent's live state, its questions and results and its undelivered messages.
pub fn node(domain: &DomainRuntime, key: &str) -> Result<Value, DomainError> {
    let state = state(domain)?;
    let agent = agent_named(domain, &state, key)?;
    let live = domain.live.node(&agent.0);
    let mut node = match live {
        Some(live) => node_row(&live, &state, domain),
        None if crate::sdk_owner::runs_through_sdk(&state, &agent) || state.agents[&agent].phase == NodePhase::ThroughSdk =>
            sdk_node_row(&state.agents[&agent], &state, domain),
        None => json!({"session_id": agent.0, "alive": false}),
    };
    if node["name"].is_null() {
        node["name"] = json!(state.agents[&agent].name);
    }
    let mine = || state.interactions.values().filter(|i| author_session(i) == Some(agent.0.as_str()));
    let pending = domain.store.lock_or_exit("the domain store").pending_messages()
        .map_err(|_| DomainError::new("storage_unavailable", "outbox unavailable"))?;
    let messages: Vec<Value> = pending.iter().filter(|m| m.agent_id.as_deref() == Some(agent.0.as_str()))
        .map(|m| json!({"id": m.id, "from": "alondra", "text": m.message, "handed_at": null})).collect();
    Ok(json!({"node": node, "questions": mine().filter_map(|i| question(i, &state)).collect::<Vec<_>>(),
        "reports": mine().filter_map(|i| report(i, &state)).collect::<Vec<_>>(), "messages": messages,
        "messages_held_for_turn_end": domain.held.lock_or_recover("the held messages").get(&agent).map_or(0, Vec::len),
        "timeline": []}))
}

#[cfg(test)]
mod tests;
