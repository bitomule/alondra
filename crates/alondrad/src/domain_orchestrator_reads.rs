//! The orchestrator's reads, as 8ccd131's `fleet` and `node` gave them: the live
//! sessions the daemon observes next to the domain's Requests, Tasks, questions, results and
//! undelivered messages. Each answer has the shape the MCP's summaries already read.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{AgentId, Author, DomainError, DomainState, Interaction, InteractionPayload, NodePhase, PauseReason, Recipient, TaskId};
use alondra_core::Node;
use serde_json::{Value, json};

fn state(domain: &DomainRuntime) -> Result<std::sync::Arc<DomainState>, DomainError> {
    domain.state().map_err(|_| DomainError::new("storage_unavailable", "domain state unavailable"))
}

fn title(body: &str) -> String {
    let line = body.lines().find(|line| !line.trim().is_empty()).unwrap_or_default().trim();
    let mut short: String = line.chars().take(120).collect();
    if line.chars().count() > 120 {
        short.push('…');
    }
    short
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
        NodePhase::WaitingUser { .. } => "a card of its task waits for the user".to_owned(),
        NodePhase::Paused(pause) => format!("paused ({}) since {}", match &pause.reason {
            PauseReason::Memory => "memory".to_owned(),
            PauseReason::Declared { reason } => format!("it waits for the machine: {reason}"),
            PauseReason::Usage => "the usage limit".to_owned(),
            PauseReason::ByUser => "by the user".to_owned(),
            PauseReason::ApiError(retry) => format!("a Claude API error, {}", retry.error),
        }, since(pause.since)),
        NodePhase::Delivered { .. } => "its result waits for the user's review".to_owned(),
        NodePhase::Resuming { .. } => "the user's answer is on its way to it and starts its next turn".to_owned(),
        NodePhase::WaitingDependency => "its task waits for another task to be done".to_owned(),
        NodePhase::NoTask => "its turn ended and it has no open task".to_owned(),
        NodePhase::Closing => "its task is over; it cleans up".to_owned(),
        NodePhase::Closed => "its session is closed".to_owned(),
    });
}

fn node_row(node: &Node, state: &DomainState, domain: &DomainRuntime) -> Value {
    let mut row = wire(node);
    if let Some(phase) = state.node_phase(&AgentId(node.session_id.clone())) {
        phase_fields(&mut row, phase);
    }
    row["model"] = model_of(domain, &node.session_id);
    if node.name.is_none() {
        row["name"] = json!(state.agents.get(&AgentId(node.session_id.clone())).map(|a| a.name.clone()));
    }
    row["project"] = json!(node.project());
    row
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
    Some(json!({"id": i.id, "title": title(&i.body), "detail": i.body, "blocking": i.blocking,
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
    Some(json!({"id": i.id, "title": title(&i.body), "kind": kind, "body": i.body, "read": read,
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

/// `fleet`: the live Agents and everything that waits on someone, in the legacy overview's shape.
pub(crate) fn fleet(domain: &DomainRuntime, machine: Value, usage: Value) -> Result<Value, DomainError> {
    let state = state(domain)?;
    let nodes: Vec<Value> = domain.live.nodes().iter()
        .filter(|n| state.agents.contains_key(&AgentId(n.session_id.clone())))
        .map(|n| node_row(n, &state, domain)).collect();
    let interactions = || state.interactions.values().filter(|i| matches!(i.recipient, Recipient::User | Recipient::Orchestrator));
    let open_questions: Vec<Value> = interactions().filter_map(|i| question(i, &state))
        .filter(|q| q["status"] == "open").collect();
    let reports: Vec<Value> = interactions().filter(|i| i.recipient == Recipient::User).filter_map(|i| report(i, &state)).collect();
    let pending = domain.store.lock().expect("domain store poisoned").pending_messages()
        .map_err(|_| DomainError::new("storage_unavailable", "outbox unavailable"))?;
    let orchestrator_messages: Vec<Value> = pending.iter().filter(|m| m.recipient_kind == "orchestrator")
        .map(|m| json!({"id": m.id, "text": m.message, "handed_at": null})).collect();
    Ok(json!({"nodes": nodes, "open_questions": open_questions, "reports": reports, "orchestrator_messages": orchestrator_messages,
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
pub(crate) fn node(domain: &DomainRuntime, key: &str) -> Result<Value, DomainError> {
    let state = state(domain)?;
    let agent = agent_named(domain, &state, key)?;
    let live = domain.live.node(&agent.0);
    let mut node = live.as_ref().map_or_else(|| json!({"session_id": agent.0, "alive": false}), |n| node_row(n, &state, domain));
    if node["name"].is_null() {
        node["name"] = json!(state.agents[&agent].name);
    }
    let mine = || state.interactions.values().filter(|i| author_session(i) == Some(agent.0.as_str()));
    let pending = domain.store.lock().expect("domain store poisoned").pending_messages()
        .map_err(|_| DomainError::new("storage_unavailable", "outbox unavailable"))?;
    let messages: Vec<Value> = pending.iter().filter(|m| m.agent_id.as_deref() == Some(agent.0.as_str()))
        .map(|m| json!({"id": m.id, "from": "alondra", "text": m.message, "handed_at": null})).collect();
    Ok(json!({"node": node, "questions": mine().filter_map(|i| question(i, &state)).collect::<Vec<_>>(),
        "reports": mine().filter_map(|i| report(i, &state)).collect::<Vec<_>>(), "messages": messages,
        "messages_held_for_turn_end": domain.held.lock().expect("held messages poisoned").get(&agent).map_or(0, Vec::len),
        "timeline": []}))
}

#[cfg(test)]
mod tests;
