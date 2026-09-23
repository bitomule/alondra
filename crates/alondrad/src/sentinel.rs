use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use alondra_core::view::derive;
use alondra_core::{
    Event, Message, MessageFrom, Node, NodeState, Question, QuestionOrigin, QuestionStatus, Report, ReportKind, View, now_millis,
};

use crate::classify::{Expects, what_node_expects};
use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(15);
const TITLE_MAX: usize = 110;
const MARK: &str = "sentinel:";
const GRACE: Duration = Duration::from_secs(180);

#[derive(Default)]
struct Memory {
    nudged: HashMap<String, (String, Instant)>,
    settled: HashSet<String>,
}

pub async fn run(hub: Arc<Hub>) {
    let mut memory = Memory::default();
    loop {
        hub.wait_event(EVERY).await;
        if let Err(e) = sweep(&hub, &mut memory).await {
            eprintln!("alondrad: sentinel: {e}");
        }
    }
}

fn nudge_text(user: &str) -> String {
    format!(
        "[Alondra] Your turn ended without telling {user} anything through Alondra, so they cannot tell whether you are \
         waiting on them. What you write in your terminal does not reach them. Declare your state with an Alondra tool: \
         `ask` if you need a decision (one question per decision, with options), `report` with kind final if you are \
         done, or `wait_for_machine` if you are waiting for resources. If you have not done it in 3 minutes, Alondra will \
         pass your last message on to {user}."
    )
}

async fn sweep(hub: &Hub, memory: &mut Memory) -> anyhow::Result<()> {
    let stale: Vec<u64> = hub.read(|v| {
        v.questions
            .values()
            .filter(|q| q.status.is_open() && is_sentinel(q))
            .filter(|q| v.nodes.get(&q.session_id).is_none_or(|n| moved_on(n, q)))
            .map(|q| q.id)
            .collect()
    });
    for id in stale {
        hub.append(Event::QuestionClosed { id, status: QuestionStatus::AnsweredInTerminal })?;
    }

    let pending: Vec<(String, String, bool)> =
        hub.read(|v| v.nodes.values().filter(|n| needs_attention(v, n)).map(|n| (n.session_id.clone(), key(n), n.has_plugin)).collect());
    memory.nudged.retain(|session, (k, _)| pending.iter().any(|(s, key, _)| s == session && key == k));
    memory.settled.retain(|k| pending.iter().any(|(_, key, _)| key == k));

    let mut due = vec![];
    for (session, key, has_plugin) in pending {
        if memory.settled.contains(&key) {
            continue;
        }
        if !has_plugin {
            due.push((session, key));
            continue;
        }
        match memory.nudged.get(&session) {
            Some((_, at)) if at.elapsed() >= GRACE => due.push((session, key)),
            Some(_) => {}
            None => {
                let text = nudge_text(&hub.config.name);
                hub.append_with(|v| {
                    Some(Event::MessageQueued(Message {
                        id: v.messages.keys().next_back().copied().unwrap_or(0) + 1,
                        session_id: session.clone(),
                        from: MessageFrom::Alondra,
                        text,
                        answers_question: None,
                        created_at: now_millis(),
                        handed_at: None,
                        delivered_at: None,
                    }))
                })?;
                memory.nudged.insert(session, (key, Instant::now()));
            }
        }
    }

    for (session, due_key) in due {
        let Some(said) = hub.read(|v| v.nodes.get(&session).map(what_it_said)) else { continue };
        let expects = match &said {
            Some(text) => what_node_expects(text).await,
            None => Expects::Unsure,
        };
        hub.append_with(|v| {
            let node = v.nodes.get(&session)?;
            (needs_attention(v, node) && key(node) == due_key).then(|| settle(v, node, expects))
        })?;
        memory.settled.insert(due_key);
    }
    Ok(())
}

fn what_it_said(node: &Node) -> Option<String> {
    let message = node.last_message.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let needs = node.daemon_needs.as_deref().map(str::trim).filter(|s| !s.is_empty());
    message.or(needs).map(str::to_string)
}

fn settle(v: &View, node: &Node, expects: Expects) -> Event {
    match expects {
        Expects::Nothing => Event::ReportPosted(finished_report(v, node)),
        other => Event::QuestionAsked(question_for(v, node, other)),
    }
}

fn is_sentinel(q: &Question) -> bool {
    q.tool_use_id.as_deref().is_some_and(|t| t.starts_with(MARK))
}

fn key(node: &Node) -> String {
    format!("{MARK}{}:{}:{}", node.session_id, node.turn, node.daemon_needs.as_deref().unwrap_or("").trim())
}

fn moved_on(node: &Node, q: &Question) -> bool {
    !node.in_daemon || derive(node, false).0 == NodeState::Working || q.tool_use_id.as_deref() != Some(key(node).as_str())
}

fn needs_attention(v: &View, node: &Node) -> bool {
    let waiting = matches!(node.state, NodeState::WaitingAnswer | NodeState::Unclear);
    if !node.in_daemon || node.is_orchestrator || !waiting || node.waiting_permission {
        return false;
    }
    let key = key(node);
    !v.questions.values().any(|q| {
        q.session_id == node.session_id
            && (q.status.is_open() || (q.tool_use_id.as_deref() == Some(key.as_str()) && q.status != QuestionStatus::Withdrawn))
    })
}

fn question_for(v: &View, node: &Node, expects: Expects) -> Question {
    let said = what_it_said(node);
    let title = match (expects, said.as_deref()) {
        (Expects::Act, Some(text)) => format!("Your turn: {}", cap(last_line(text))),
        (_, Some(text)) => cap(last_question(text).unwrap_or_else(|| last_line(text))),
        (_, None) => "The node is waiting for something and did not say what".to_string(),
    };
    Question {
        id: v.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: node.name.clone(),
        title,
        detail: said,
        options: vec![],
        links: vec![],
        attachments: vec![],
        blocking: true,
        origin: QuestionOrigin::Classifier,
        tool_use_id: Some(key(node)),
        to_orchestrator: false,
        node_context: Some("The node asked in its terminal, not with an Alondra question".into()),
        status: QuestionStatus::Open,
        answer: None,
        asked_at: now_millis(),
    }
}

fn finished_report(v: &View, node: &Node) -> Report {
    let said = what_it_said(node).unwrap_or_default();
    Report {
        id: v.reports.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: node.name.clone(),
        title: format!("Finished: {}", cap(said.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no summary"))),
        body: Some(said),
        links: vec![],
        attachments: vec![],
        kind: ReportKind::Final,
        created_at: now_millis(),
        read: false,
    }
}

fn last_question(message: &str) -> Option<&str> {
    message.lines().rev().map(str::trim).find(|l| l.ends_with('?'))
}

fn last_line(message: &str) -> &str {
    message.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or(message)
}

fn cap(text: &str) -> String {
    let text = text.trim().trim_start_matches(['-', '*', ' ']);
    if text.chars().count() <= TITLE_MAX {
        return text.to_string();
    }
    let cut: String = text.chars().take(TITLE_MAX).collect();
    format!("{}…", cut.rsplit_once(' ').map_or(cut.as_str(), |(a, _)| a))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::{DaemonObservation, HookEvent};

    fn waiting_view(last: &str) -> View {
        let mut v = View::default();
        let hook = |event: &str, last: Option<&str>| {
            Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), last_message: last.map(str::to_string), ..Default::default() })
        };
        v.apply(0, 500, &Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), pid: Some(42), ..Default::default() }));
        v.apply(1, 1000, &hook("UserPromptSubmit", None));
        v.apply(2, 2000, &hook("Stop", Some(last)));
        let turn = v.nodes["s"].turn;
        v.apply(3, 3000, &Event::Classified { session_id: "s".into(), turn, state: NodeState::WaitingAnswer, confidence: None });
        v
    }

    const ASKS: &str = "I prepared the release.\nShould I ship 4.5.0 now or merge #256 first?";

    #[test]
    fn a_decision_becomes_a_question_with_its_real_question() {
        let v = waiting_view(ASKS);
        assert!(needs_attention(&v, &v.nodes["s"]));
        let Event::QuestionAsked(q) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        assert_eq!(q.title, "Should I ship 4.5.0 now or merge #256 first?");
    }

    #[test]
    fn a_node_that_is_done_gets_a_final_report_not_a_question() {
        let mut v = waiting_view("Task complete and reported to hive/corpus-router.\nEverything committed locally.");
        let event = settle(&v, &v.nodes["s"], Expects::Nothing);
        let Event::ReportPosted(r) = &event else { panic!() };
        assert_eq!(r.kind, ReportKind::Final);
        assert_eq!(r.title, "Finished: Task complete and reported to hive/corpus-router.");
        v.apply(4, 4000, &event);
        assert!(!needs_attention(&v, &v.nodes["s"]));
    }

    #[test]
    fn a_dormant_node_is_watched_like_a_live_one() {
        let mut v = waiting_view(ASKS);
        v.nodes.get_mut("s").unwrap().alive = false;
        assert!(needs_attention(&v, &v.nodes["s"]));
    }

    #[test]
    fn something_to_do_is_marked_as_your_turn() {
        let v = waiting_view("PR merged. Remove the camera permission on a real iPhone and open the scanner.");
        let Event::QuestionAsked(q) = settle(&v, &v.nodes["s"], Expects::Act) else { panic!() };
        assert!(q.title.starts_with("Your turn: "));
    }

    #[test]
    fn it_is_asked_once_and_withdrawn_when_the_node_carries_on() {
        let mut v = waiting_view(ASKS);
        let Event::QuestionAsked(q) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        v.apply(4, 4000, &Event::QuestionAsked(q.clone()));
        assert!(!needs_attention(&v, &v.nodes["s"]));
        assert!(!moved_on(&v.nodes["s"], &q));
        v.apply(5, 5000, &Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(moved_on(&v.nodes["s"], &q));
    }
}
