use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use alondra_core::pr::PrState;
use alondra_core::tasks::TaskEvent;
use alondra_core::{states, upkeep};
use alondra_core::{
    Event, MessageFrom, Node, NodeState, Question, QuestionStatus, Report, ReportKind, View, now_millis,
};

use crate::http::Agents;
use crate::hub::Hub;
use crate::markdown;
use crate::worktree::Worktrees;

const EVERY: Duration = Duration::from_secs(15);
const DEBOUNCE: Duration = Duration::from_secs(1);
const TITLE_MAX: usize = 110;
const MARK: &str = "sentinel:";
const GRACE: Duration = Duration::from_secs(180);
const NUDGE_COOLDOWN_MS: i64 = 6 * 3600 * 1000;
/// A node without the plugin may be parked on a terminal menu; the menu watcher gets this long
/// to find it before the sentinel reads its last message.
const MENU_FIRST_MS: i64 = 90 * 1000;

/// A free node whose close was refused (work not saved elsewhere, a prompt on screen) is tried
/// again after this long.
const CLOSE_RETRY: Duration = Duration::from_secs(3600);

#[derive(Default)]
struct Memory {
    settled: HashSet<String>,
    close_refused: HashMap<String, std::time::Instant>,
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
    follow_merges(hub, &user, now_millis())?;
    let now = now_millis();
    let stalled: Vec<String> = hub.read(|v| {
        v.nodes.values().filter(|n| n.state == NodeState::Stalled || states::idle_task(v, n, now).is_some()).map(|n| n.session_id.clone()).collect()
    });
    for session in stalled {
        hub.append_with(|v| v.nodes.get(&session).and_then(|n| states::stall_message(v, n, &user, now_millis())).map(Event::MessageQueued))?;
    }
    nudge_defining_requests(hub, now_millis())?;
    let running: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.watched() && !n.background.is_empty()).map(|n| n.session_id.clone()).collect());
    for session in running {
        while hub
            .append_with(|v| v.nodes.get(&session).and_then(|n| states::hung_message(v, n, &user, now_millis())).map(Event::MessageQueued))?
            .is_some()
        {}
    }
    if let Some(after) = hub.config.close_free_nodes_after {
        close_idle_nodes(hub, &mut memory.close_refused, after, &crate::http::ClaudeAgents, &crate::worktree::Git).await;
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
        hub.append_with(|v| {
            let node = v.nodes.get(&session)?;
            if needs_attention(v, node, now_millis()) && key(node) == due_key { settle(v, node) } else { None }
        })?;
        memory.settled.insert(due_key);
    }
    Ok(())
}

/// Keeps what nodes wait on in step with the merges: every merge reaches the nodes of the tasks
/// it concerns; a running task with nothing left but a dependency's merge waits for it; a quiet
/// node hears about the PRs its last words wait on that were merged since, or have no card.
pub(crate) fn follow_merges(hub: &Hub, user: &str, now: i64) -> anyhow::Result<()> {
    let batch = |due: Vec<Event>| if due.is_empty() { Err(()) } else { Ok(due) };
    hub.append_all(|v| batch(v.merge_notices_due(now).into_iter().map(Event::MessageQueued).collect()))?.ok();
    let waiting: Vec<(String, u64)> = hub.read(|v| v.nodes.values().filter_map(|n| states::awaits_merge(v, n, now)).map(|(t, p)| (t.id.clone(), p.id)).collect());
    for (task, pr) in waiting {
        hub.append_all(|v| {
            let still = v.board.tasks.get(&task).and_then(|t| v.awaitable_merge(t)).is_some_and(|p| p.id == pr);
            batch(if still { vec![Event::TaskMoved { task: task.clone(), event: TaskEvent::AwaitsMerge { pr } }] } else { vec![] })
        })?
        .ok();
    }
    let quiet: Vec<String> = hub.read(|v| v.nodes.values().filter(|n| n.watched()).map(|n| n.session_id.clone()).collect());
    for session in quiet {
        hub.append_all(|v| batch(v.nodes.get(&session).map(|n| states::stale_wait_messages(v, n, user, now)).unwrap_or_default().into_iter().map(Event::MessageQueued).collect()))?
            .ok();
    }
    Ok(())
}

/// Tells the orchestrator about each request its last turn left defining, once.
pub(crate) fn nudge_defining_requests(hub: &Hub, now: i64) -> anyhow::Result<()> {
    let user = hub.config.name.clone();
    let nudges = |v: &alondra_core::View| {
        let due: Vec<Event> = v.defining_nudges(&user, now).into_iter().map(Event::MessageQueued).collect();
        if due.is_empty() { Err(()) } else { Ok(due) }
    };
    hub.append_all(nudges)?.ok();
    Ok(())
}

/// Closes the nodes idle for `after` with nothing unfinished, through the fleet's close, and asks
/// the user about those that have something unfinished. A node refused or asked about is left
/// alone for a while instead of being looked at on every sweep.
pub(crate) async fn close_idle_nodes(hub: &Hub, refused: &mut HashMap<String, std::time::Instant>, after: Duration, claude: &impl Agents, trees: &impl Worktrees) {
    refused.retain(|_, at| at.elapsed() < CLOSE_RETRY);
    let now = now_millis();
    let ms = after.as_millis() as i64;
    let idle: Vec<(String, String, Vec<String>, bool)> = hub.read(|v| {
        v.nodes
            .values()
            .filter(|n| !refused.contains_key(&n.session_id) && upkeep::idle_for(v, n, now, ms))
            .map(|n| {
                let prs: Vec<_> = v.prs.values().filter(|p| p.session_id == n.session_id).collect();
                let merged = !prs.is_empty() && prs.iter().all(|p| p.state == PrState::Merged);
                (n.session_id.clone(), n.display_name().to_string(), upkeep::pending_work(v, n), merged)
            })
            .collect()
    });
    for (session, name, mut pending, merged_by_pr) in idle {
        pending.extend(trees.unfinished(&session, merged_by_pr).await);
        if pending.is_empty() {
            let response = crate::http::close_with(hub, &session, false, Some(after), claude).await;
            if response.status().is_success() {
                log!("closed {name}: idle for {} h with nothing unfinished", after.as_secs() / 3600);
                continue;
            }
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024).await.unwrap_or_default();
            log!("did not close the idle node {name}: {}", String::from_utf8_lossy(&body));
            refused.insert(session, std::time::Instant::now());
            continue;
        }
        refused.insert(session.clone(), std::time::Instant::now());
        let asked = hub.append_with(|v| v.nodes.get(&session).and_then(|n| upkeep::idle_close_question(v, n, &pending, ms, now_millis())).map(Event::QuestionAsked));
        match asked {
            Ok(Some(_)) => log!("asked about {name}: idle for {} h with unfinished work", after.as_secs() / 3600),
            Ok(None) => {}
            Err(e) => log!("could not ask about the idle node {name}: {e}"),
        }
    }
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

/// Something David said is on its way to the node: an answer not yet delivered, or a message from
/// him or the orchestrator not yet typed in. What the node shows now is from before it.
fn reply_on_its_way(v: &View, node: &Node) -> bool {
    let sid = &node.session_id;
    v.questions.values().any(|q| &q.session_id == sid && q.status == QuestionStatus::Answered)
        || v.pending_messages(sid).any(|m| m.from != MessageFrom::Alondra)
}

/// The node already reported since the last message reached it or its current turn began,
/// whichever is later: its own words stand, and a report Alondra builds from its terminal would
/// only repeat or contradict them. An older report is about something else.
fn reported_this_episode(v: &View, node: &Node) -> bool {
    let since = node.episode_since.max(node.turn_since);
    v.reports.values().any(|r| r.session_id == node.session_id && r.created_at >= since)
}

/// A node that stopped without reporting gets a notice with its last words, never a question:
/// a node that wants a decision asks for it with `ask`.
fn settle(v: &View, node: &Node) -> Option<Event> {
    if reported_this_episode(v, node) {
        return None;
    }
    let report = terminal_report(v, node);
    let repeated = v.reports.values().filter(|r| r.session_id == node.session_id).max_by_key(|r| r.id).is_some_and(|r| r.body == report.body);
    (!repeated).then_some(Event::ReportPosted(report))
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
/// no reply of David's on its way. A node with fresh evidence of work never reaches the terminal check.
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

fn terminal_report(v: &View, node: &Node) -> Report {
    let said = what_it_said(node).unwrap_or_default();
    let first = first_line(&said);
    let first_sentence = first.split_inclusive(". ").next().unwrap_or(first).trim();
    let (title, body) = if first_sentence.is_empty() {
        (node.name.clone().unwrap_or_else(|| node.short_id.clone()), None)
    } else {
        headline(&said, first_sentence)
    };
    Report {
        id: v.next_report_id(),
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
        task: v.fact_task(&node.session_id, None),
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

fn first_line(message: &str) -> &str {
    markdown::prose_lines(message).into_iter().find(|l| !l.is_empty()).unwrap_or_default()
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
    use alondra_core::{DaemonObservation, HookEvent, Message, Question, QuestionOrigin};

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

    /// Whatever the sweeps, the orchestrator hears once about a request its turn left defining.
    #[test]
    fn the_sweep_nudges_the_orchestrator_once_about_a_request_left_defining() {
        let home = std::env::temp_dir().join(format!("alondra-sentinel-defining-{}-{}", std::process::id(), now_millis()));
        let config = alondra_core::config::Config::resolve(home, &Default::default(), |_| None, || Some("Ana".into()));
        let hub = Hub::open(config).unwrap();
        let hook = |event: &str| Event::Hook(HookEvent { session_id: "o".into(), event: event.into(), prompt: Some("go".into()), ..Default::default() });
        hub.append(Event::MarkedOrchestrator { session_id: "o".into() }).unwrap();
        hub.append(hook("SessionStart")).unwrap();
        let options = alondra_core::options::OptionsFile::default().orchestrator_defaults(None, None);
        hub.append(Event::RequestSent { request_id: "r-1".into(), options, text: Some("¿Cómo va BlurCamera?".into()), attachments: vec![] }).unwrap();
        std::thread::sleep(Duration::from_millis(5));
        hub.append(hook("UserPromptSubmit")).unwrap();
        hub.append(hook("Stop")).unwrap();
        for _ in 0..3 {
            nudge_defining_requests(&hub, now_millis()).unwrap();
        }
        let nudges = || hub.read(|v| v.messages.values().filter(|m| m.session_id == "o" && m.text.starts_with("[Alondra] Request r-1 (")).map(|m| m.id).collect::<Vec<_>>());
        assert_eq!(nudges().len(), 1);
        hub.append(Event::MessageHanded { id: nudges()[0] }).unwrap();
        hub.append(hook("UserPromptSubmit")).unwrap();
        hub.append(hook("Stop")).unwrap();
        nudge_defining_requests(&hub, now_millis()).unwrap();
        assert_eq!(nudges().len(), 1, "not again once it reached the orchestrator and its turn ended");
    }

    #[test]
    fn titles_skip_table_rows_and_code() {
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

    /// R12: a node's terminal line is never turned into a question, whatever it says. A node
    /// that wants a decision uses `ask`.
    #[test]
    fn a_terminal_line_becomes_a_notice_never_a_question() {
        for last in [ASKS, "Task complete.", "Please test it on your iPhone.", "¿Lo apruebas?", "Informe enviado al orchestrator."] {
            let v = waiting_view(last);
            assert!(needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen), "{last}");
            let Some(Event::ReportPosted(r)) = settle(&v, &v.nodes["s"]) else { panic!("{last}: not a report") };
            assert_eq!(r.kind, ReportKind::Notice, "{last}");
            assert!(!r.looks_like_question, "{last}");
        }
    }

    #[test]
    fn cards_built_from_the_terminal_carry_the_node_mission() {
        let mut v = waiting_view(ASKS);
        v.nodes.get_mut("s").unwrap().mission = Some("Ship Lumo 4.5".into());
        let Some(Event::ReportPosted(r)) = settle(&v, &v.nodes["s"]) else { panic!() };
        assert_eq!(r.mission.as_deref(), Some("Ship Lumo 4.5"));
    }

    #[test]
    fn a_node_that_is_done_gets_a_notice_not_a_question() {
        let mut v = waiting_view("Task complete and reported to hive/corpus-router.\nEverything committed locally.");
        let event = settle(&v, &v.nodes["s"]).unwrap();
        let Event::ReportPosted(r) = &event else { panic!() };
        assert_eq!(r.kind, ReportKind::Notice);
        assert_eq!(r.title, "Task complete and reported to hive/corpus-router.");
        v.apply(4, 4000, &event);
        assert!(v.questions.is_empty());
        assert!(settle(&v, &v.nodes["s"]).is_none(), "its report stands for this turn");
    }

    #[test]
    fn a_dormant_node_is_watched_like_a_live_one() {
        let mut v = waiting_view(ASKS);
        v.nodes.get_mut("s").unwrap().alive = false;
        assert!(needs_attention(&v, &v.nodes["s"], v.nodes["s"].last_seen));
    }

    #[test]
    fn a_node_with_a_task_still_running_never_reaches_terminal_check() {
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
    fn the_same_terminal_report_is_not_posted_twice() {
        let mut v = waiting_view("Task complete.");
        let first = settle(&v, &v.nodes["s"]).unwrap();
        v.apply(4, 4000, &first);
        assert!(settle(&v, &v.nodes["s"]).is_none());
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
        let Event::ReportPosted(r) = settle(&v, &v.nodes["s"]).unwrap() else { panic!() };
        assert_eq!(r.title, "Ya no.");
        assert_eq!(r.body.as_deref(), Some("`d0b8007a` **es exactamente el fix**. Main ya lo tiene."));
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

    /// Sentinel questions asked before R12 are closed once the node carries on.
    #[test]
    fn an_earlier_sentinel_question_is_withdrawn_when_the_node_carries_on() {
        let mut v = waiting_view(ASKS);
        let q = Question { origin: QuestionOrigin::Classifier, tool_use_id: Some(key(&v.nodes["s"])), blocking: true, ..asked(1) };
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
            replaces: None, about_pr: None,
            task: None,
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
                id: 1, session_id: "s".into(), node_name: None, title: "Auditoría rehecha".into(), body: Some("Detalle".into()),
                links: vec![], attachments: vec![], kind, created_at: at, read: false, to_orchestrator: false, mission: None, looks_like_question: false,
                task: None,
            })
        };
        let handed = |at: i64| vec![message(1, MessageFrom::User, at), Event::MessageHanded { id: 1 }];
        let turn = |event: &str| Event::Hook(HookEvent { session_id: "s".into(), event: event.into(), last_message: Some("Task complete.".into()), ..Default::default() });
        let new_turn = || vec![turn("UserPromptSubmit"), turn("Stop")];
        let cases: Vec<(&str, Vec<Event>, bool)> = vec![
            ("the node reported nothing", vec![], true),
            ("the node reported final", vec![own(ReportKind::Final, 2200)], false),
            ("the node sent a notice", vec![own(ReportKind::Notice, 2200)], false),
            ("the node sent progress", vec![own(ReportKind::Progress, 2200)], false),
            ("its report was before the last message", [vec![own(ReportKind::Final, 2200)], handed(2600)].concat(), true),
            ("never messaged, its report was in an earlier turn", [vec![own(ReportKind::Final, 2200)], new_turn()].concat(), true),
            ("never messaged, a report in an earlier turn and one in this one", [vec![own(ReportKind::Final, 2200)], new_turn(), vec![own(ReportKind::Notice, 3400)]].concat(), false),
            ("messaged, then a new turn after its report", [vec![own(ReportKind::Final, 2200)], handed(2600), new_turn()].concat(), true),
        ];
        for (what, events, posts) in cases {
            let mut v = waiting_view("Task complete.");
            for (i, e) in events.iter().enumerate() {
                v.apply(10 + i as u64, 2200 + i as i64 * 400, e);
            }
            let settled = settle(&v, &v.nodes["s"]);
            assert!(!matches!(settled, Some(Event::QuestionAsked(_))), "{what}");
            assert_eq!(matches!(settled, Some(Event::ReportPosted(_))), posts, "{what}");
        }
        let mut v = waiting_view(ASKS);
        v.apply(10, 2200, &own(ReportKind::Final, 2200));
        assert!(settle(&v, &v.nodes["s"]).is_none(), "a question in the terminal after its own final report adds nothing");
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

    /// `claude rm` as the test sees it: it records every call and every close goes through.
    #[derive(Default)]
    struct FakeAgents(std::sync::Mutex<Vec<String>>);

    impl Agents for FakeAgents {
        async fn run(&self, args: &[&str]) -> Result<String, String> {
            self.0.lock().unwrap().push(args.join(" "));
            Ok(if args[0] == "rm" { format!("removed {}", args[1]) } else { "[]".into() })
        }
    }

    /// Worktrees as they would look: what each session left, and whether every PR it opened was
    /// merged when Alondra asked.
    #[derive(Default)]
    struct FakeTrees(std::sync::Mutex<Vec<(String, bool)>>, HashMap<String, Vec<String>>);

    impl Worktrees for FakeTrees {
        async fn unfinished(&self, session_id: &str, merged_by_pr: bool) -> Vec<String> {
            self.0.lock().unwrap().push((session_id.to_string(), merged_by_pr));
            self.1.get(session_id).cloned().unwrap_or_default()
        }
    }

    fn seen(sid: &str) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: sid.into(), short_id: sid.into(), name: Some(format!("app/{sid}")), pid: Some(1), ..Default::default() })
    }

    /// Idle for the set time: a node with all its work on the default branch is closed; one with
    /// work left in its worktree or an unfinished task is not, and the user is asked once what to
    /// do with it; the orchestrator is never touched.
    #[tokio::test]
    async fn a_node_idle_for_the_set_time_is_closed_only_with_its_work_merged_and_otherwise_the_user_is_asked() {
        let home = std::env::temp_dir().join(format!("alondra-sentinel-idle-{}-{}", std::process::id(), now_millis()));
        let config = alondra_core::config::Config::resolve(home, &alondra_core::config::FileConfig::default(), |_| None, || Some("Ana".into()));
        assert_eq!(config.close_free_nodes_after, Some(Duration::from_secs(30 * 60)), "30 minutes unless set");
        let hub = Hub::open(config).unwrap();
        for sid in ["clean", "unpushed", "tasked", "orch"] {
            hub.append(seen(sid)).unwrap();
        }
        hub.append(Event::MarkedOrchestrator { session_id: "orch".into() }).unwrap();
        hub.append(Event::TaskCreated(alondra_core::tasks::NewTask {
            id: "t-1".into(), request: "r-1".into(), title: "Menu model".into(), brief: None, depends_on: vec![], options: Default::default(),
            session_id: Some("tasked".into()), launched: false, launched_as: None, start: None,
        }))
        .unwrap();
        hub.append(Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "clean".into(), task: None }).unwrap();
        let pr = hub.read(|v| *v.prs.keys().next().unwrap());
        hub.append(Event::PrMerged { id: pr, method: alondra_core::pr::MergeMethod::Squash }).unwrap();
        let trees = FakeTrees(Default::default(), HashMap::from([("unpushed".to_string(), vec!["2 commit(s) not pushed in /w/unpushed".to_string()])]));
        let claude = FakeAgents::default();
        let after = Duration::from_millis(1);
        tokio::time::sleep(Duration::from_millis(5)).await;

        hub.append(Event::ReportPosted(alondra_core::Report {
            id: 1, session_id: "clean".into(), node_name: None, title: "Merged".into(), body: None, links: vec![], attachments: vec![],
            kind: alondra_core::ReportKind::Final, created_at: now_millis(), read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None,
        }))
        .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(hub.read(|v| upkeep::pending_work(v, &v.nodes["clean"])), Vec::<String>::new(), "its task is done");
        let mut refused = HashMap::new();
        close_idle_nodes(&hub, &mut refused, after, &claude, &trees).await;
        assert_eq!(*claude.0.lock().unwrap(), ["rm clean", "agents --json --all"], "only the node with nothing unfinished is closed");
        let looked: Vec<(String, bool)> = trees.0.lock().unwrap().clone();
        assert!(looked.contains(&("clean".to_string(), true)), "its merged PR stands for a squash merge: {looked:?}");
        assert!(!looked.iter().any(|(s, _)| s == "orch"), "the orchestrator is never looked at");

        let asked: Vec<Question> = hub.read(|v| v.questions.values().filter(|q| upkeep::is_idle_close(q)).cloned().collect());
        let about = |sid: &str| asked.iter().find(|q| q.session_id == sid).unwrap_or_else(|| panic!("no question about {sid}")).clone();
        assert!(about("unpushed").detail.unwrap().contains("2 commit(s) not pushed"));
        assert!(about("tasked").detail.unwrap().contains("task t-1 “Menu model” is running"));
        let options: Vec<String> = about("tasked").options.into_iter().map(|o| o.label).collect();
        assert_eq!(options, [upkeep::KEEP, upkeep::DISCARD, upkeep::RESUME]);
        assert_eq!(asked.len(), 2);

        refused.clear();
        close_idle_nodes(&hub, &mut refused, after, &claude, &trees).await;
        assert_eq!(hub.read(|v| v.questions.values().filter(|q| upkeep::is_idle_close(q)).count()), 2, "asked once per idle stretch");
        assert_eq!(claude.0.lock().unwrap().len(), 2, "nothing more is closed");
    }

    /// Request r-1790284418392 (t-48599): the beta t-44137 depended on PR #264 of t-44136, both
    /// on one node. The sweep shows the beta waiting for that merge, and the merge reaches the
    /// node once, naming the beta.
    #[test]
    fn a_sweep_follows_the_merge_a_dependent_task_waits_for() {
        use alondra_core::tasks::{NewTask, Start, TaskOptions, TaskState};
        use alondra_core::{MessageAbout, Report, ReportKind};
        let home = std::env::temp_dir().join(format!("alondra-sentinel-merges-{}-{}", std::process::id(), now_millis()));
        let config = alondra_core::config::Config::resolve(home, &Default::default(), |_| None, || Some("David".into()));
        let hub = Hub::open(config).unwrap();
        let u = "a7e1d69b";
        let hook = |event: &str| Event::Hook(HookEvent { session_id: u.into(), event: event.into(), ..Default::default() });
        let task = |id: &str, depends_on: &[&str]| {
            Event::TaskCreated(NewTask {
                id: id.into(),
                request: "r-1790284418392".into(),
                title: id.into(),
                brief: None,
                depends_on: depends_on.iter().map(|d| d.to_string()).collect(),
                options: TaskOptions::default(),
                session_id: None,
                launched: false,
                launched_as: None,
                start: Some(Start::Node { session_id: u.into() }),
            })
        };
        let assigned = |id: &str| Event::TaskAssigned { task: id.into(), session_id: u.into(), agent_id: None };
        let report = Report {
            id: 243,
            session_id: u.into(),
            node_name: None,
            title: "PR #264".into(),
            body: None,
            links: vec![],
            attachments: vec![],
            kind: ReportKind::Final,
            created_at: now_millis(),
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: Some("t-44136".into()),
        };
        for e in [
            Event::TasksRecorded,
            task("t-44136", &[]),
            task("t-44137", &["t-44136"]),
            Event::DaemonSeen(DaemonObservation { session_id: u.into(), short_id: u.into(), pid: Some(1), state: Some("working".into()), ..Default::default() }),
            assigned("t-44136"),
            hook("UserPromptSubmit"),
            Event::PrOpened { url: "https://github.com/acmecorp-private/Redraft/pull/264".into(), session_id: u.into(), task: Some("t-44136".into()) },
            assigned("t-44137"),
            Event::ReportPosted(report),
            hook("Stop"),
        ] {
            hub.append(e).unwrap();
        }
        let later = now_millis() + 10 * 60_000;
        let state = |id: &str| hub.read(|v| v.board.tasks[id].state);
        follow_merges(&hub, "David", later).unwrap();
        assert_eq!((state("t-44136"), state("t-44137")), (TaskState::InReview, TaskState::WaitingDependency));

        hub.append(Event::PrMerged { id: 1, method: alondra_core::pr::MergeMethod::Merge }).unwrap();
        assert_eq!((state("t-44136"), state("t-44137")), (TaskState::Done, TaskState::Running));
        let notices = || hub.read(|v| v.messages.values().filter(|m| m.session_id == u && m.about == Some(MessageAbout::PrMerged { pr_id: 1, task: Some("t-44137".into()) })).count());
        for _ in 0..2 {
            follow_merges(&hub, "David", later + 60_000).unwrap();
        }
        assert_eq!(notices(), 1);
    }
}
