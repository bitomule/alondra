use super::lifecycle_tests::{exec, last, must, node, refused_unchanged, sdk_turn, user, world};
use super::*;
use std::collections::BTreeMap;

fn server(name: &str, status: McpStatus) -> McpServerState {
    McpServerState { name: name.into(), status, error: None, source: None, transport: Some("http".into()), url: Some(format!("https://mcp.{name}.app/mcp")), traits: Default::default() }
}
fn report(task: &str, servers: Vec<McpServerState>) -> Command {
    Command::RecordMcpServers { task_id: task.into(), generation: 1, servers }
}
fn wait(task: &str, name: &str) -> Command {
    Command::WaitForMcp { task_id: task.into(), server: name.into() }
}
fn notices(outcome: &CommandOutcome) -> usize {
    outcome.effects.iter().filter(|e| matches!(e, DomainEffect::NotifyUser { .. })).count()
}
fn waiting(s: &DomainState) -> Vec<(String, String)> {
    s.mcp_waiting().into_iter().map(|w| (w.task.0, w.server.name)).collect()
}
/// The MCP waits' cards Waiting for You lists.
fn listed(s: &DomainState) -> Vec<String> {
    super::read_index::inbox_items(s).into_iter().filter(|item| item.pending).filter_map(|item| {
        let InboxSubject::Interaction { interaction_id, .. } = item.subject;
        interaction_id.0.starts_with(MCP_WAIT_PREFIX).then_some(interaction_id.0)
    }).collect()
}
fn resumed_with(s: &DomainState, task: &str) -> Option<String> {
    match last(s, task) {
        LifecycleRecord::Resumed { text, .. } => text,
        _ => None,
    }
}
/// Two SDK tasks whose nodes both report Linear signed out, and a third node with it connected.
fn signed_out() -> DomainState {
    let mut s = world();
    must(&mut s, Actor::System, Command::RegisterAgent { agent_id: "c".into(), name: "c".into(), at: 0 });
    for (task, agent) in [("t", "a"), ("u", "b"), ("v", "c")] {
        sdk_turn(&mut s, task, agent);
    }
    for task in ["t", "u"] {
        must(&mut s, Actor::System, report(task, vec![server("linear", McpStatus::NeedsAuth), server("files", McpStatus::Connected)]));
    }
    must(&mut s, Actor::System, report("v", vec![server("linear", McpStatus::NeedsAuth), server("off", McpStatus::Disabled)]));
    s
}

/// Tasks t and u wait for Linear; Waiting for You shows one card for both.
pub(crate) fn waiting_for_linear() -> DomainState {
    let mut s = signed_out();
    must(&mut s, node("a"), wait("t", "linear"));
    must(&mut s, node("b"), wait("u", "linear"));
    s
}

#[test]
fn only_a_task_s_own_node_waits_for_a_server_its_claude_reported_and_not_connected() {
    let mut s = signed_out();
    assert_eq!(refused_unchanged(&mut s, user(), wait("t", "linear")), "not_assigned");
    assert_eq!(refused_unchanged(&mut s, node("b"), wait("t", "linear")), "not_assigned", "another task's node");
    assert_eq!(refused_unchanged(&mut s, Actor::System, wait("t", "linear")), "not_assigned");
    assert_eq!(refused_unchanged(&mut s, node("a"), wait("t", "missing")), "not_found", "a server Claude never reported");
    assert_eq!(refused_unchanged(&mut s, node("a"), wait("t", "files")), "conflict", "a connected server is used, not waited for");
    let first = must(&mut s, node("a"), wait("t", "linear"));
    assert_eq!(notices(&first), 1, "the user hears of the server once");
    assert!(matches!(last(&s, "t"), LifecycleRecord::Paused { card, .. } if card.0.starts_with(MCP_WAIT_PREFIX)), "the task is paused on its card");
    must(&mut s, Actor::System, Command::RecordTurnBoundary { task_id: "t".into(), generation: 1, turn: "turn-1".into(), failure: None, failed: None });
    let generic = InteractionId::from(format!("{TURN_ENDED_PREFIX}t-1-turn-1").as_str());
    assert!(!s.interactions.contains_key(&generic), "the wait explains why its turn ended");
    assert_eq!(refused_unchanged(&mut s, node("a"), wait("t", "linear")), "conflict", "one wait at a time");
    let second = must(&mut s, node("b"), wait("u", "linear"));
    assert_eq!(notices(&second), 0, "another task that waits for it is not announced");
    must(&mut s, node("c"), wait("v", "off"));
    assert_eq!(waiting(&s), [("t".into(), "linear".into()), ("u".into(), "linear".into()), ("v".into(), "off".into())]);
    let shown = listed(&s);
    assert_eq!(shown.len(), 2, "one card per server: {shown:?}");
    assert_eq!(shown[0], s.mcp_waiting()[0].card.0, "the oldest card stands for its server");
    let card = &s.interactions[&s.mcp_waiting()[0].card];
    assert_eq!(McpServerKey::of_card(card), Some(McpServerKey { name: "linear".into(), url: Some("https://mcp.linear.app/mcp".into()) }));
}

#[test]
fn a_wait_ends_only_when_its_own_node_reports_the_server_connected() {
    let mut s = signed_out();
    must(&mut s, node("a"), wait("t", "linear"));
    must(&mut s, node("b"), wait("u", "linear"));
    let (t_card, u_card) = (s.mcp_waiting()[0].card.clone(), s.mcp_waiting()[1].card.clone());
    must(&mut s, Actor::System, report("t", vec![server("linear", McpStatus::NeedsAuth)]));
    assert_eq!(waiting(&s).len(), 2, "still signed out");
    must(&mut s, Actor::System, report("v", vec![server("linear", McpStatus::Connected)]));
    assert_eq!(waiting(&s).len(), 2, "another node connecting it is no fact about these nodes");
    must(&mut s, Actor::System, report("t", vec![server("linear", McpStatus::Connected)]));
    assert_eq!(waiting(&s), [("u".into(), "linear".into())]);
    assert!(s.interactions[&t_card].withdrawn, "its card is withdrawn in the same step");
    assert_eq!(resumed_with(&s, "t").as_deref(), Some("linear is connected: go on with your task from where it stopped."));
    assert_eq!(listed(&s), [u_card.0.clone()], "the next task's card stands for the server now");
    must(&mut s, Actor::System, report("u", vec![server("linear", McpStatus::Connected)]));
    assert!(waiting(&s).is_empty());
    assert!(listed(&s).is_empty(), "no card once nothing waits");
}

#[test]
fn going_on_without_the_server_sends_every_task_that_waits_for_it_on() {
    let mut s = signed_out();
    must(&mut s, node("a"), wait("t", "linear"));
    must(&mut s, node("b"), wait("u", "linear"));
    must(&mut s, node("c"), wait("v", "off"));
    let card = s.mcp_waiting()[0].card.clone();
    let answer = |option: &str, text: Option<&str>| Command::AnswerQuestion {
        attachments: vec![], interaction_id: card.clone(), option_id: option.into(),
        inputs: text.map(|text| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(text.into()))])).unwrap_or_default(),
    };
    assert_eq!(refused_unchanged(&mut s, user(), answer("connect", None)), "invalid_input", "Connect opens the sign-in, it answers nothing");
    assert_eq!(refused_unchanged(&mut s, user(), answer("", Some("later"))), "invalid_input", "words alone would leave the tasks waiting");
    must(&mut s, user(), answer(MCP_GO_ON_WITHOUT, None));
    assert_eq!(waiting(&s), [("v".into(), "off".into())], "only the tasks that waited for Linear go on");
    for task in ["t", "u"] {
        assert_eq!(resumed_with(&s, task).as_deref(), Some("linear is not available: go on with your task without it."), "{task}");
    }
    assert_eq!(exec(&mut s, user(), answer(MCP_GO_ON_WITHOUT, None)).unwrap_err().code, "question_already_answered");
}

#[test]
fn a_waiting_task_that_ends_leaves_no_card_behind() {
    let mut s = signed_out();
    must(&mut s, node("a"), wait("t", "linear"));
    must(&mut s, node("b"), wait("u", "linear"));
    must(&mut s, user(), Command::CancelTask { task_id: "t".into(), reason: None });
    assert_eq!(waiting(&s), [("u".into(), "linear".into())]);
    assert_eq!(listed(&s), [s.mcp_waiting()[0].card.0.clone()], "the task still waiting stands for the server");
}
