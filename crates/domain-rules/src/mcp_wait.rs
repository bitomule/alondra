//! A task whose node needs an MCP server that is not connected waits for it on a card of its own,
//! as every wait does. Waiting for You shows one card per server however many tasks wait for it:
//! the oldest. Connect opens the sign-in on a node that waits; Go on without it sends every
//! waiting task on. Only facts release a task otherwise: its own node reporting the server
//! connected.
use super::*;

pub(crate) fn connected_text(name: &str) -> String {
    format!("{name} is connected: go on with your task from where it stopped.")
}
pub(crate) fn without_text(name: &str) -> String {
    format!("{name} is not available: go on with your task without it.")
}
/// The user hears of a server only when no other task waits for it.
pub(crate) fn wait_for_mcp(domain: &mut DomainState, actor: &Actor, task_id: &TaskId, server: &str, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
    let (key, record) = domain.mcp_wait_guard(actor, task_id, server)?;
    let LifecycleRecord::Paused { card, .. } = &record else { unreachable!("the guard pauses the task") };
    let card = card.clone();
    let announce = domain.mcp_waiting_for(&key).is_empty();
    let revision = domain.revision + 1;
    domain.insert_system_card(mcp_card(card.clone(), task_id, &key, revision))?;
    if announce {
        effects.push(DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: card });
    }
    domain.decide(lifecycles, task_id, record)
}
/// The task's node reported its servers: each of its waits for a server now connected ends,
/// and the task goes on.
pub(crate) fn mcp_reported(domain: &mut DomainState, task_id: &TaskId, lifecycles: &mut Lifecycles) -> Result<()> {
    let released: Vec<McpWaiting> = domain
        .mcp_waiting()
        .into_iter()
        .filter(|waiting| waiting.task == *task_id)
        .filter(|waiting| domain.mcp_server_of(task_id, &waiting.server).is_some_and(|server| server.status == McpStatus::Connected))
        .collect();
    for waiting in released {
        domain.retire(&waiting.card);
        resume_mcp(domain, &waiting, connected_text(&waiting.server.name), lifecycles)?;
    }
    Ok(())
}
/// Only Go on without it answers an MCP wait's card: Connect is the sign-in itself.
pub(crate) fn mcp_answer_guard(card: &Interaction, option_id: &str) -> Result<()> {
    if McpServerKey::of_card(card).is_some() && option_id != MCP_GO_ON_WITHOUT {
        return refuse("invalid_input", "option_id", &format!("This card is answered only with {MCP_GO_ON_WITHOUT}: Connect opens the sign-in"));
    }
    Ok(())
}
/// The user went on without the server: every task that waits for it goes on, and the other
/// cards of the server are withdrawn. False for any other card.
pub(crate) fn settle_mcp_card(domain: &mut DomainState, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<bool> {
    let Some(key) = domain.interactions.get(card).and_then(McpServerKey::of_card) else { return Ok(false) };
    let Some(Scope::Task { task_id }) = domain.interactions.get(card).map(|i| i.scope.clone()) else { return Ok(true) };
    let order = domain.interactions[card].order;
    let answered = McpWaiting { task: task_id, card: card.clone(), server: key.clone(), order };
    let waiting: Vec<McpWaiting> = std::iter::once(answered).chain(domain.mcp_waiting_for(&key)).collect();
    for waiting in waiting {
        if waiting.card != *card {
            domain.retire(&waiting.card);
        }
        if domain.tasks.get(&waiting.task).is_some_and(|task| !task.over()) {
            resume_mcp(domain, &waiting, without_text(&key.name), lifecycles)?;
        }
    }
    Ok(true)
}
pub(crate) fn resume_mcp(domain: &mut DomainState, waiting: &McpWaiting, text: String, lifecycles: &mut Lifecycles) -> Result<()> {
    let record = LifecycleRecord::Resumed { card: waiting.card.clone(), input: domain.next_input(&waiting.task), text: Some(text) };
    domain.decide(lifecycles, &waiting.task, record)
}
pub(crate) fn mcp_card(id: InteractionId, task: &TaskId, server: &McpServerKey, revision: u64) -> Interaction {
    let need = clip(&format!("Sign in to {}: a task waits for it", server.name), CardSpec::NEED_MAX);
    let why = format!("The task needs the MCP server {} and Claude cannot use it until you sign in.", server.name);
    let card = CardSpec::new(CardKind::Problem, need.clone()).why(clip(&why, CardSpec::WHY_MAX));
    let option = QuestionOption {
        id: MCP_GO_ON_WITHOUT.into(),
        label: format!("Go on without {}", server.name),
        description: Some("Every task that waits for it goes on without it".into()),
    };
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: serde_json::to_string(server).expect("a server key serializes"),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question { options: vec![option], answer: None },
        revision,
        order: revision,
        withdrawn: false,
        card,
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::McpWait), shortened: false,
    }
}
