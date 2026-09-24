use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::states;
use alondra_core::{
    Event, MessageFrom, Node, NodeState, Question, QuestionOrigin, QuestionStatus, Report, ReportKind, View, answer_option, now_millis,
};

use crate::classify::{Expects, what_node_expects};
use crate::hub::Hub;
use crate::markdown;

const EVERY: Duration = Duration::from_secs(15);
const DEBOUNCE: Duration = Duration::from_secs(1);
const TITLE_MAX: usize = 110;
const MARK: &str = "sentinel:";
const GRACE: Duration = Duration::from_secs(180);
const NUDGE_COOLDOWN_MS: i64 = 6 * 3600 * 1000;
/// A node without the plugin may be parked on a terminal menu; the menu watcher gets this long
/// to find it before jev reads its last message.
const MENU_FIRST_MS: i64 = 90 * 1000;

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
            log!("sentinel: {e}");
        }
    }
}

async fn sweep(hub: &Hub, memory: &mut Memory) -> anyhow::Result<()> {
    let strained = hub.machine().strained();
    if hub.read(|v| v.due(now_millis(), strained)) {
        hub.append(Event::Clock { machine_strained: strained })?;
    }
    let user = hub.config.name.clone();
    let stalled: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.state == NodeState::Stalled).map(|n| n.session_id.clone()).collect());
    for session in stalled {
        hub.append_with(|v| v.nodes.get(&session).and_then(|n| states::stall_message(v, n, &user, now_millis())).map(Event::MessageQueued))?;
    }
    let running: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.watched() && !n.background.is_empty()).map(|n| n.session_id.clone()).collect());
    for session in running {
        while hub
            .append_with(|v| v.nodes.get(&session).and_then(|n| states::hung_message(v, n, &user, now_millis())).map(Event::MessageQueued))?
            .is_some()
        {}
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
        hub.append(Event::QuestionClosed { id, status: QuestionStatus::AnsweredInTerminal, note: None })?;
    }

    let pending: Vec<(String, String, bool)> =
        hub.read(|v| v.nodes.values().filter(|n| needs_attention(v, n, now_millis())).map(|n| (n.session_id.clone(), key(n), n.has_plugin)).collect());
    memory.settled.retain(|k| pending.iter().any(|(_, key, _)| key == k));

    let mut due = vec![];
    for (session, key, has_plugin) in pending {
        if memory.settled.contains(&key) {
            continue;
        }
        if !has_plugin {
            let since = hub.read(|v| v.nodes.get(&session).map(|n| n.state_since)).unwrap_or(0);
            if now_millis() - since >= MENU_FIRST_MS {
                due.push((session, key));
            }
            continue;
        }
        let last_nudge = hub.read(|v| last_nudge_at(v, &session));
        match last_nudge {
            Some(at) if now_millis() - at < GRACE.as_millis() as i64 => {}
            Some(at) if now_millis() - at < NUDGE_COOLDOWN_MS => due.push((session, key)),
            _ => {
                let text = states::nudge_text(&hub.config.name);
                hub.append_with(|v| v.alondra_message(&session, text, now_millis()).map(Event::MessageQueued))?;
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
            if needs_attention(v, node, now_millis()) && key(node) == due_key { settle(v, node, expects) } else { None }
        })?;
        memory.settled.insert(due_key);
    }
    Ok(())
}

fn last_nudge_at(v: &View, session: &str) -> Option<i64> {
    v.messages
        .values()
        .filter(|m| m.session_id == session && m.from == MessageFrom::Alondra && m.text.starts_with(states::NUDGE_PREFIX))
        .map(|m| m.created_at)
        .max()
}

fn what_it_said(node: &Node) -> Option<String> {
    let message = node.last_message.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let needs = node.daemon_needs.as_deref().map(str::trim).filter(|s| !s.is_empty());
    message.or(needs).map(str::to_string)
}

/// The node already reported since the last message reached it or its current turn began,
/// whichever is later: its own words stand, and a report Alondra builds from its terminal would
/// only repeat or contradict them. An older report is about something else.
fn reported_this_episode(v: &View, node: &Node) -> bool {
    let since = node.episode_since.max(node.turn_since);
    v.reports.values().any(|r| r.session_id == node.session_id && r.created_at >= since)
}

/// Something David said is on its way to the node: an answer not yet delivered, or a message from
/// him or the orchestrator not yet typed in. What the node shows now is from before it.
fn reply_on_its_way(v: &View, node: &Node) -> bool {
    let sid = &node.session_id;
    v.questions.values().any(|q| &q.session_id == sid && q.status == QuestionStatus::Answered)
        || v.pending_messages(sid).any(|m| m.from != MessageFrom::Alondra)
}

fn settle(v: &View, node: &Node, expects: Expects) -> Option<Event> {
    match expects {
        Expects::Finished | Expects::HandedOff if reported_this_episode(v, node) => None,
        Expects::Finished => {
            let report = finished_report(v, node);
            let repeated = v.reports.values().filter(|r| r.session_id == node.session_id).max_by_key(|r| r.id).is_some_and(|r| r.body == report.body);
            (!repeated).then_some(Event::ReportPosted(report))
        }
        Expects::HandedOff => {
            let mut report = finished_report(v, node);
            report.read = true;
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

/// Only nodes the state machine handed over: idle, with no work in sight, nothing in the inbox and
/// no reply of David's on its way. A node with fresh evidence of work never reaches jev.
fn needs_attention(v: &View, node: &Node, now: i64) -> bool {
    let asked_this_turn = node.turn_signal == Some(alondra_core::TurnSignal::Asked);
    let hung_task = node.evidence.as_ref().is_some_and(|e| e.signal == alondra_core::Signal::BackgroundSuspect);
    if !node.watched()
        || node.state != NodeState::Unclear
        || hung_task
        || node.waiting_permission
        || asked_this_turn
        || states::watching(v, node, now)
        || reply_on_its_way(v, node)
    {
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
    let (title, detail) = match (expects, said.as_deref()) {
        (Expects::Act, Some(text)) => headline(text, last_line(text)),
        (_, Some(text)) => headline(text, last_question(text).unwrap_or_else(|| last_line(text))),
        (_, None) => (node.name.clone().unwrap_or_else(|| node.short_id.clone()), None),
    };
    Question {
        related: vec![],
        id: v.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: node.name.clone().or_else(|| Some(node.short_id.clone())),
        title,
        detail,
        options: if expects == Expects::Act { answer_option::for_action() } else { vec![] },
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
        steps: vec![],
        mission: node.mission.clone(),
    }
}

fn finished_report(v: &View, node: &Node) -> Report {
    let said = what_it_said(node).unwrap_or_default();
    let first = first_line(&said);
    let first_sentence = first.split_inclusive(". ").next().unwrap_or(first).trim();
    let (title, body) = if first_sentence.is_empty() {
        (node.name.clone().unwrap_or_else(|| node.short_id.clone()), None)
    } else {
        headline(&said, first_sentence)
    };
    Report {
        related: vec![],
        id: v.reports.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: node.name.clone().or_else(|| Some(node.short_id.clone())),
        title,
        body,
        links: vec![],
        attachments: vec![],
        kind: ReportKind::Notice,
        created_at: now_millis(),
        read: false,
        to_orchestrator: false,
        mission: node.mission.clone(),
        looks_like_question: false,
    }
}

/// The chosen line becomes a plain-text title and leaves the body, so nothing is said twice.
fn headline(message: &str, chosen: &str) -> (String, Option<String>) {
    let title = cap(&plain(chosen));
    let rest = message.replacen(chosen, "", 1);
    let rest = rest.trim();
    (title, (!rest.is_empty()).then(|| rest.to_string()))
}

fn plain(text: &str) -> String {
    text.replace("**", "").replace("__", "").replace('`', "").trim_start_matches(['#', '-', '*', '>', ' ']).trim().to_string()
}

fn last_question(message: &str) -> Option<&str> {
    markdown::prose_lines(message).into_iter().rev().find(|l| l.ends_with('?'))
}

fn first_line(message: &str) -> &str {
    markdown::prose_lines(message).into_iter().find(|l| !l.is_empty()).unwrap_or_default()
}

fn last_line(message: &str) -> &str {
    markdown::prose_lines(message).into_iter().rev().find(|l| !l.is_empty()).unwrap_or(message)
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
    use alondra_core::{DaemonObservation, HookEvent, Message};

    fn waiting_view(last: &str) -> View {
        let mut v = View::default();
        let hook = |event: &str, last: Option<&str>| {
            Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), last_message: last.map(str::to_string), ..Default::default() })
        };
        v.apply(0, 500, &Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), pid: Some(42), ..Default::default() }));
        v.apply(1, 1000, &hook("UserPromptSubmit", None));
        v.apply(2, 2000, &hook("Stop", Some(last)));
        v.nodes.get_mut("s").unwrap().has_plugin = false;
        v.apply(3, 2500, &Event::Clock { machine_strained: false });
        v
    }

    const ASKS: &str = "I prepared the release.\nShould I ship 4.5.0 now or merge #256 first?";

    #[test]
    fn titles_skip_table_rows_and_code() {
        let cases = [
            ("plain last line", "Done.\nShipped 4.5.0", "Shipped 4.5.0", None),
            ("table row is not a title", "Results:\n\n| app | size |\n|---|---|\n| Lumo | 12 |", "Results:", None),
            ("code is not a title", "Run this:\n```sh\nmake test\n```", "Run this:", None),
            ("question inside code is ignored", "Is this right?\n```\nwhy?\n```", "Is this right?", Some("Is this right?")),
            ("question in a table is ignored", "Pick one?\n| a? | b? |", "Pick one?", Some("Pick one?")),
            ("pipe prose still counts", "|x is the absolute value?", "|x is the absolute value?", Some("|x is the absolute value?")),
        ];
        for (name, message, line, question) in cases {
            assert_eq!(last_line(message), line, "{name}");
            assert_eq!(last_question(message), question, "{name}");
        }
        let firsts = [
            ("plain first", "Shipped 4.5.0.\nAll green.", "Shipped 4.5.0."),
            ("table row first", "| app | size |\n|---|---|\n| Lumo | 12 |\nSizes are stable.", "Sizes are stable."),
            ("code first", "```sh\nmake test\n```\nTests pass.", "Tests pass."),
        ];
        for (name, message, line) in firsts {
            assert_eq!(first_line(message), line, "{name}");
        }
    }

    #[test]
    fn a_node_left_with_only_a_hung_task_gets_no_card_from_its_terminal() {
        let mut v = View::default();
        let hook = |event: &str| Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), last_message: Some(ASKS.into()), ..Default::default() });
        let loop_left = alondra_core::BackgroundItem { kind: alondra_core::BackgroundKind::Shell, label: "until pgrep …".into(), started_at: 1000, suspect: false };
        v.apply(1, 1000, &hook("UserPromptSubmit"));
        v.apply(2, 2000, &hook("Stop"));
        v.apply(3, 2500, &Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), pid: Some(42), tasks: Some(1), background: vec![loop_left], ..Default::default() }));
        let later = 2500 + 40 * 60_000;
        v.apply(4, later, &Event::Clock { machine_strained: false });
        assert_eq!(v.nodes["s"].state, NodeState::Unclear);
        assert!(!needs_attention(&v, &v.nodes["s"], later), "the hung-task message handles it");
        assert!(states::hung_message(&v, &v.nodes["s"], "Ana", later).is_some_and(|m| m.session_id == "s"));
    }

    #[test]
    fn a_decision_becomes_a_question_with_its_real_question() {
        let v = waiting_view(ASKS);
        assert!(needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        assert_eq!(q.title, "Should I ship 4.5.0 now or merge #256 first?");
    }

    #[test]
    fn cards_built_from_the_terminal_carry_the_node_mission() {
        let mut v = waiting_view(ASKS);
        v.nodes.get_mut("s").unwrap().mission = Some("Ship Lumo 4.5".into());
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        assert_eq!(q.mission.as_deref(), Some("Ship Lumo 4.5"));
        let Some(Event::ReportPosted(r)) = settle(&v, &v.nodes["s"], Expects::Finished) else { panic!() };
        assert_eq!(r.mission.as_deref(), Some("Ship Lumo 4.5"));
    }

    #[test]
    fn a_node_that_is_done_gets_a_final_report_not_a_question() {
        let mut v = waiting_view("Task complete and reported to hive/corpus-router.\nEverything committed locally.");
        let event = settle(&v, &v.nodes["s"], Expects::Finished).unwrap();
        let Event::ReportPosted(r) = &event else { panic!() };
        assert_eq!(r.kind, ReportKind::Notice);
        assert_eq!(r.title, "Task complete and reported to hive/corpus-router.");
        v.apply(4, 4000, &event);
        assert!(!needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
    }

    #[test]
    fn a_dormant_node_is_watched_like_a_live_one() {
        let mut v = waiting_view(ASKS);
        v.nodes.get_mut("s").unwrap().alive = false;
        assert!(needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
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
        assert!(!needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
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
            text: states::nudge_text("Ana"),
            answers_question: None,
            created_at: 4000,
            handed_at: None,
            delivered_at: None,
            about: None,
            attachments: vec![],
        }));
        assert_eq!(last_nudge_at(&v, "s"), Some(4000));
    }

    #[test]
    fn the_title_is_not_repeated_in_the_body_and_has_no_markdown() {
        let v = waiting_view("Ya no. `d0b8007a` **es exactamente el fix**. Main ya lo tiene.");
        let Event::ReportPosted(r) = settle(&v, &v.nodes["s"], Expects::Finished).unwrap() else { panic!() };
        assert_eq!(r.title, "Ya no.");
        assert_eq!(r.body.as_deref(), Some("`d0b8007a` **es exactamente el fix**. Main ya lo tiene."));
    }

    #[test]
    fn a_result_handed_to_another_agent_stays_out_of_the_users_inbox() {
        let mut v = waiting_view("Informe enviado al orchestrator. 4 de las 5 ramas ya están en main.");
        let event = settle(&v, &v.nodes["s"], Expects::HandedOff).unwrap();
        let Event::ReportPosted(r) = &event else { panic!() };
        assert!(r.read);
        v.apply(4, 4000, &event);
        assert!(!needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
    }

    #[test]
    fn a_node_the_user_is_talking_with_directly_is_left_alone() {
        let mut v = waiting_view(ASKS);
        let node = v.nodes.get_mut("s").unwrap();
        node.last_prompt = Some("the TUI shift key does not work".into());
        node.last_prompt_at = Some(1000);
        assert!(!needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
        assert!(needs_attention(&v, &v.nodes["s"], 1000 + states::WATCH_WINDOW_MS + 1), "the user left 10 minutes ago");
        v.nodes.get_mut("s").unwrap().last_prompt = Some(states::nudge_text("Ana"));
        v.apply(9, 9000, &Event::MessageQueued(Message {
            id: 7,
            session_id: "s".into(),
            from: MessageFrom::Alondra,
            text: states::nudge_text("Ana"),
            answers_question: None,
            created_at: 9000,
            handed_at: None,
            delivered_at: None,
            about: None,
            attachments: vec![],
        }));
        assert!(needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
    }

    #[test]
    fn something_to_do_is_marked_as_your_turn() {
        let v = waiting_view("PR merged. Remove the camera permission on a real iPhone and open the scanner.");
        let mut v = v;
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Act) else { panic!() };
        assert_eq!(q.title, "PR merged. Remove the camera permission on a real iPhone and open the scanner.");
        assert!(q.action);
        assert_eq!(q.options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), [answer_option::DONE, answer_option::CANNOT]);
        v.apply(4, 4000, &Event::QuestionAsked(q));
        assert_eq!(v.nodes["s"].state, NodeState::YourTurn);
    }

    #[test]
    fn it_is_asked_once_and_withdrawn_when_the_node_carries_on() {
        let mut v = waiting_view(ASKS);
        let Some(Event::QuestionAsked(q)) = settle(&v, &v.nodes["s"], Expects::Decide) else { panic!() };
        v.apply(4, 4000, &Event::QuestionAsked(q.clone()));
        assert!(!needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
        assert!(!moved_on(&v, &v.nodes["s"], &q, 4000));
        v.apply(5, 5000, &Event::Hook(HookEvent { session_id: "s".into(), event: "UserPromptSubmit".into(), ..Default::default() }));
        assert!(moved_on(&v, &v.nodes["s"], &q, 5000));
    }

    fn message(id: u64, from: MessageFrom, at: i64) -> Event {
        Event::MessageQueued(Message { id, session_id: "s".into(), from, text: "Sí".into(), answers_question: None, created_at: at, handed_at: None, delivered_at: None, about: None, attachments: vec![] })
    }

    fn asked(id: u64) -> Question {
        Question {
            related: vec![],
            id,
            session_id: "s".into(),
            node_name: None,
            title: "¿Recupero las dos notas?".into(),
            detail: None,
            options: vec![],
            links: vec![],
            attachments: vec![],
            blocking: false,
            origin: QuestionOrigin::Ask,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: 2100,
            action: false,
            steps: vec![],
            mission: None,
        }
    }

    /// Q#94 (mav/find-goto, 23 Sep): David answered Q#91 and, before the answer reached the
    /// node, the sentinel built a card from the node's "waiting for David" line.
    #[test]
    fn no_card_while_a_reply_of_the_user_is_on_its_way() {
        let answer = || Event::QuestionAnswered { id: 1, answer: alondra_core::Answer { text: "Sí".into(), option: None, by: None, device: None, at: 2200 } };
        let cases: Vec<(&str, Vec<Event>, bool)> = vec![
            ("nothing pending", vec![], true),
            ("an answer not yet delivered", vec![Event::QuestionAsked(asked(1)), answer()], false),
            ("an answer already delivered", vec![Event::QuestionAsked(asked(1)), answer(), Event::QuestionDelivered { id: 1 }], true),
            ("a message from the user not yet typed in", vec![message(1, MessageFrom::User, 2200)], false),
            ("a message from the orchestrator not yet typed in", vec![message(1, MessageFrom::Orchestrator, 2200)], false),
            ("only Alondra's own nudge pending", vec![message(1, MessageFrom::Alondra, 2200)], true),
        ];
        for (what, events, attends) in cases {
            let mut v = waiting_view(ASKS);
            for (i, e) in events.iter().enumerate() {
                v.apply(10 + i as u64, 2200, e);
            }
            assert_eq!(v.nodes["s"].state, NodeState::Unclear, "{what}");
            assert_eq!(needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen), attends, "{what}");
        }
    }

    /// R#31 (hive/auditoria-jev-alondra, 23 Sep): "Looks finished" three minutes after the node's
    /// own final reports R#29 and R#30.
    #[test]
    fn no_terminal_report_over_the_nodes_own() {
        let own = |kind: ReportKind, at: i64| {
            Event::ReportPosted(Report {
                related: vec![],
                id: 1, session_id: "s".into(), node_name: None, title: "Auditoría rehecha".into(), body: Some("Detalle".into()),
                links: vec![], attachments: vec![], kind, created_at: at, read: false, to_orchestrator: false, mission: None, looks_like_question: false,
            })
        };
        let handed = |at: i64| vec![message(1, MessageFrom::User, at), Event::MessageHanded { id: 1 }];
        let turn = |event: &str| Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), last_message: Some("Task complete.".into()), ..Default::default() });
        let new_turn = || vec![turn("UserPromptSubmit"), turn("Stop")];
        let cases: Vec<(&str, Vec<Event>, Expects, bool)> = vec![
            ("the node reported nothing", vec![], Expects::Finished, true),
            ("the node reported final", vec![own(ReportKind::Final, 2200)], Expects::Finished, false),
            ("the node sent a notice", vec![own(ReportKind::Notice, 2200)], Expects::Finished, false),
            ("handed off after its own report", vec![own(ReportKind::Final, 2200)], Expects::HandedOff, false),
            ("its report was before the last message", [vec![own(ReportKind::Final, 2200)], handed(2600)].concat(), Expects::Finished, true),
            ("never messaged, its report was in an earlier turn", [vec![own(ReportKind::Final, 2200)], new_turn()].concat(), Expects::Finished, true),
            ("never messaged, a report in an earlier turn and one in this one", [vec![own(ReportKind::Final, 2200)], new_turn(), vec![own(ReportKind::Notice, 3400)]].concat(), Expects::Finished, false),
            ("messaged, then a new turn after its report", [vec![own(ReportKind::Final, 2200)], handed(2600), new_turn()].concat(), Expects::Finished, true),
        ];
        for (what, events, expects, posts) in cases {
            let mut v = waiting_view("Task complete.");
            for (i, e) in events.iter().enumerate() {
                v.apply(10 + i as u64, 2200 + i as i64 * 400, e);
            }
            assert_eq!(matches!(settle(&v, &v.nodes["s"], expects), Some(Event::ReportPosted(_))), posts, "{what}");
        }
        let mut v = waiting_view(ASKS);
        v.apply(10, 2200, &own(ReportKind::Final, 2200));
        assert!(matches!(settle(&v, &v.nodes["s"], Expects::Decide), Some(Event::QuestionAsked(_))), "a question is still built from the terminal");
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
                wanted.push((seq, needs_attention(v, n, n.last_seen)));
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
            if [3429, 3436, 3442, 3455].contains(&seq) && needs_attention(v, &v.nodes[sid], v.nodes[sid].last_seen) {
                nudged.push(seq);
            }
        });
        assert!(nudged.is_empty(), "handed to the sentinel after {nudged:?}");
    }
}
