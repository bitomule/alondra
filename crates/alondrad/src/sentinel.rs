use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::states;
use alondra_core::{
    Event, Message, MessageFrom, Node, NodeState, Question, QuestionOrigin, QuestionStatus, Report, ReportKind, View, now_millis,
};

use crate::classify::{Expects, what_node_expects};
use crate::hub::Hub;

const EVERY: Duration = Duration::from_secs(15);
const DEBOUNCE: Duration = Duration::from_secs(1);
const TITLE_MAX: usize = 110;
const MARK: &str = "sentinel:";
const GRACE: Duration = Duration::from_secs(180);
const NUDGE_COOLDOWN_MS: i64 = 6 * 3600 * 1000;
const NUDGE_PREFIX: &str = "[Alondra] Your turn ended without telling";

#[derive(Default)]
struct Memory {
    settled: HashSet<String>,
}

pub async fn run(hub: Arc<Hub>) {
    let mut memory = Memory::default();
    loop {
        hub.wait_event(EVERY).await;
        tokio::time::sleep(DEBOUNCE).await;
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
         done, or `wait_for_machine` if you are waiting for resources. If you are still working on your own (a build, an \
         upload, a review, a long command), send nothing: do not report that you do not need {user}, that is noise. \
         If you have not done any of this in 3 minutes, Alondra will turn your last message into an item in {user}'s \
         inbox; it never answers or closes anything for you."
    )
}

async fn sweep(hub: &Hub, memory: &mut Memory) -> anyhow::Result<()> {
    let strained = hub.machine().strained();
    if hub.read(|v| v.due(now_millis(), strained)) {
        hub.append(Event::Clock { machine_strained: strained })?;
    }
    let now = now_millis();
    let stale: Vec<u64> = hub.read(|v| {
        v.questions
            .values()
            .filter(|q| q.status.is_open() && is_sentinel(q))
            .filter(|q| v.nodes.get(&q.session_id).is_none_or(|n| moved_on(v, n, q, now)))
            .map(|q| q.id)
            .collect()
    });
    for id in stale {
        hub.append(Event::QuestionClosed { id, status: QuestionStatus::AnsweredInTerminal })?;
    }

    let pending: Vec<(String, String, bool)> =
        hub.read(|v| v.nodes.values().filter(|n| needs_attention(v, n)).map(|n| (n.session_id.clone(), key(n), n.has_plugin)).collect());
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
        let last_nudge = hub.read(|v| last_nudge_at(v, &session));
        match last_nudge {
            Some(at) if now_millis() - at < GRACE.as_millis() as i64 => {}
            Some(at) if now_millis() - at < NUDGE_COOLDOWN_MS => due.push((session, key)),
            _ => {
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
            if needs_attention(v, node) && key(node) == due_key { settle(v, node, expects) } else { None }
        })?;
        memory.settled.insert(due_key);
    }
    Ok(())
}

fn last_nudge_at(v: &View, session: &str) -> Option<i64> {
    v.messages
        .values()
        .filter(|m| m.session_id == session && m.from == MessageFrom::Alondra && m.text.starts_with(NUDGE_PREFIX))
        .map(|m| m.created_at)
        .max()
}

fn what_it_said(node: &Node) -> Option<String> {
    let message = node.last_message.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let needs = node.daemon_needs.as_deref().map(str::trim).filter(|s| !s.is_empty());
    message.or(needs).map(str::to_string)
}

fn settle(v: &View, node: &Node, expects: Expects) -> Option<Event> {
    match expects {
        Expects::Finished => {
            let report = finished_report(v, node);
            let repeated = v.reports.values().filter(|r| r.session_id == node.session_id).max_by_key(|r| r.id).is_some_and(|r| r.body == report.body);
            (!repeated).then_some(Event::ReportPosted(report))
        }
        other => Some(Event::QuestionAsked(question_for(v, node, other))),
    }
}

fn is_sentinel(q: &Question) -> bool {
    q.tool_use_id.as_deref().is_some_and(|t| t.starts_with(MARK))
}

fn key(node: &Node) -> String {
    format!("{MARK}{}:{}:{}", node.session_id, node.turn, node.daemon_needs.as_deref().unwrap_or("").trim())
}

fn moved_on(v: &View, node: &Node, q: &Question, now: i64) -> bool {
    !node.in_daemon || states::works(v, node, now) || q.tool_use_id.as_deref() != Some(key(node).as_str())
}

/// Only nodes the state machine handed over: idle, with no work in sight and nothing in the
/// inbox. A node with fresh evidence of work never reaches jev.
fn needs_attention(v: &View, node: &Node) -> bool {
    if !node.watched() || node.state != NodeState::Unclear || node.waiting_permission {
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
        (Expects::Act, Some(text)) => cap(last_line(text)),
        (_, Some(text)) => cap(last_question(text).unwrap_or_else(|| last_line(text))),
        (_, None) => node.name.clone().unwrap_or_else(|| node.short_id.clone()),
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
        node_context: None,
        status: QuestionStatus::Open,
        answer: None,
        asked_at: now_millis(),
        action: expects == Expects::Act,
    }
}

fn finished_report(v: &View, node: &Node) -> Report {
    let said = what_it_said(node).unwrap_or_default();
    Report {
        id: v.reports.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: node.name.clone(),
        title: cap(said.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or(node.name.as_deref().unwrap_or(&node.short_id))),
        body: Some(said),
        links: vec![],
        attachments: vec![],
        kind: ReportKind::Notice,
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
        v
    }

    const ASKS: &str = "I prepared the release.\nShould I ship 4.5.0 now or merge #256 first?";

    #[test]
    fn a_decision_becomes_a_question_with_its_real_question() {
        let v = waiting_view(ASKS);
        assert!(needs_attention(&v, &v.nodes["s"]));
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        assert_eq!(q.title, "Should I ship 4.5.0 now or merge #256 first?");
    }

    #[test]
    fn a_node_that_is_done_gets_a_final_report_not_a_question() {
        let mut v = waiting_view("Task complete and reported to hive/corpus-router.\nEverything committed locally.");
        let event = settle(&v, &v.nodes["s"], Expects::Finished).unwrap();
        let Event::ReportPosted(r) = &event else { panic!() };
        assert_eq!(r.kind, ReportKind::Notice);
        assert_eq!(r.title, "Task complete and reported to hive/corpus-router.");
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
    fn a_node_with_a_task_still_running_never_reaches_jev() {
        let mut v = waiting_view("Release build running in the background; I will upload it when it finishes.");
        v.apply(3, 3000, &Event::DaemonSeen(DaemonObservation {
            session_id: "s".into(),
            short_id: "s".into(),
            pid: Some(42),
            state: Some("working".into()),
            tempo: Some("idle".into()),
            tasks: Some(1),
            task_kinds: vec!["local_bash".into()],
            ..Default::default()
        }));
        assert_eq!(v.nodes["s"].state, NodeState::Working);
        assert!(!needs_attention(&v, &v.nodes["s"]));
    }

    #[test]
    fn the_same_finished_report_is_not_posted_twice() {
        let mut v = waiting_view("Task complete.");
        let first = settle(&v, &v.nodes["s"], Expects::Finished).unwrap();
        v.apply(4, 4000, &first);
        assert!(settle(&v, &v.nodes["s"], Expects::Finished).is_none());
    }

    #[test]
    fn a_nudge_is_remembered_from_the_stored_messages() {
        let mut v = waiting_view(ASKS);
        assert_eq!(last_nudge_at(&v, "s"), None);
        v.apply(4, 4000, &Event::MessageQueued(Message {
            id: 1,
            session_id: "s".into(),
            from: MessageFrom::Alondra,
            text: nudge_text("Ana"),
            answers_question: None,
            created_at: 4000,
            handed_at: None,
            delivered_at: None,
        }));
        assert_eq!(last_nudge_at(&v, "s"), Some(4000));
    }

    #[test]
    fn something_to_do_is_marked_as_your_turn() {
        let v = waiting_view("PR merged. Remove the camera permission on a real iPhone and open the scanner.");
        let mut v = v;
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Act) else { panic!() };
        assert_eq!(q.title, "PR merged. Remove the camera permission on a real iPhone and open the scanner.");
        assert!(q.action);
        v.apply(4, 4000, &Event::QuestionAsked(q));
        assert_eq!(v.nodes["s"].state, NodeState::YourTurn);
    }

    #[test]
    fn it_is_asked_once_and_withdrawn_when_the_node_carries_on() {
        let mut v = waiting_view(ASKS);
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        v.apply(4, 4000, &Event::QuestionAsked(q.clone()));
        assert!(!needs_attention(&v, &v.nodes["s"]));
        assert!(!moved_on(&v, &v.nodes["s"], &q, 4000));
        v.apply(5, 5000, &Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(moved_on(&v, &v.nodes["s"], &q, 5000));
    }

    fn replay(fixture: &str, mut each: impl FnMut(u64, &View)) {
        let mut v = View::default();
        for line in fixture.lines() {
            let r: serde_json::Value = serde_json::from_str(line).unwrap();
            let seq = r["seq"].as_u64().unwrap();
            v.apply(seq, r["at"].as_i64().unwrap(), &serde_json::from_value(r["event"].clone()).unwrap());
            each(seq, &v);
        }
    }

    /// hive/corpus-router/ctrl-trivial~2, recorded on 23 Sep: the sentinel posted the same
    /// "Finished" report five times, once after each daemon restart.
    #[test]
    fn recorded_ctrl_trivial_is_settled_by_its_first_report() {
        let sid = "128f3fa5-71f3-4376-8948-6ee1cf147c22";
        let mut wanted = vec![];
        replay(include_str!("../../core/fixtures/ctrl_trivial_2.jsonl"), |seq, v| {
            if let Some(n) = v.nodes.get(sid) {
                wanted.push((seq, needs_attention(v, n)));
            }
        });
        assert!(wanted.iter().filter(|(seq, _)| *seq < 2966).all(|(_, w)| *w));
        assert!(wanted.iter().filter(|(seq, _)| *seq >= 2966).all(|(_, w)| !*w));
    }

    /// ios27/quill, recorded on 23 Sep: after declaring wait_for_machine it was nudged four
    /// times in 40 s.
    #[test]
    fn recorded_quill_waiting_for_the_machine_is_not_nudged() {
        let sid = "11f98ac0-771f-4725-88f5-7e5176ffff92";
        let mut nudged = vec![];
        replay(include_str!("../../core/fixtures/quill.jsonl"), |seq, v| {
            if [3429, 3436, 3442, 3455].contains(&seq) && needs_attention(v, &v.nodes[sid]) {
                nudged.push(seq);
            }
        });
        assert!(nudged.is_empty(), "handed to the sentinel after {nudged:?}");
    }
}
