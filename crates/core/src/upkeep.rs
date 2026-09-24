//! What Alondra does to a node's process on its own: restart one that ended while the node still
//! had work, and close a node that has been free for long when the user asked for that. Both read
//! stored events only, so a restart of alondrad neither repeats an attempt nor forgets one.

use crate::model::{Message, MessageFrom, Millis, Node, TurnSignal};
use crate::states;
use crate::tasks::{Task, TaskState};
use crate::view::View;

const MINUTE: Millis = 60 * 1000;
/// The Claude daemon restarts a process that exits unexpectedly by itself, and records where it
/// left the session right after the exit (code.claude.com/docs/en/agent-view, "The supervisor
/// process"): Alondra steps in only once this has passed.
pub const RESUME_GRACE_MS: Millis = MINUTE;
/// A node restarted this recently is not restarted again, even when its process ended again.
pub const RESUME_BACKOFF_MS: Millis = 15 * MINUTE;
/// Where the daemon leaves a session whose process it stopped because it sat idle: finished, or
/// waiting for its next message. A reply brings it back, so nothing died.
const IDLE_STOP_STATES: &[&str] = &["done", "blocked"];

pub const RESUMED_PREFIX: &str = "[Alondra] Your process stopped";
pub const RESUME_FAILED_PREFIX: &str = "[Alondra] Could not restart a node";

/// The node's process ended while it was working, not because the daemon let an idle session go.
pub fn died_working(node: &Node) -> bool {
    node.watched() && !node.alive && node.exited_at.is_some() && node.daemon_state.as_deref().is_some_and(|s| !IDLE_STOP_STATES.contains(&s))
}

/// The task a dead process leaves open: not finished, and not closed by the node's own final report.
fn open_task<'a>(view: &'a View, node: &Node) -> Option<&'a Task> {
    view.node_tasks(&node.session_id).find(|t| !t.state.finished() && t.final_report.is_none())
}

/// The open task of a node whose process died, when it is time to restart it: once per exit, and
/// not twice within the backoff.
pub fn resume_due<'a>(view: &'a View, node: &Node, now: Millis) -> Option<&'a Task> {
    let exited = node.exited_at?;
    if !died_working(node) || node.attached || now - exited < RESUME_GRACE_MS || node.turn_signal == Some(TurnSignal::Finished) {
        return None;
    }
    if node.resumed.as_ref().is_some_and(|r| r.exit == Some(exited) || now - r.at < RESUME_BACKOFF_MS) {
        return None;
    }
    open_task(view, node)
}

pub fn resumed_text(task: &Task) -> String {
    format!(
        "{RESUMED_PREFIX} while your task \"{}\" was still open, and Alondra restarted it. Carry on where you left off. \
         If you had already finished, say so through Alondra (`report` with kind final).",
        task.title
    )
}

/// What the node is told once its process is back.
pub fn resumed_message(view: &View, session_id: &str, at: Millis) -> Option<Message> {
    let task = open_task(view, view.nodes.get(session_id)?)?;
    view.alondra_message(session_id, resumed_text(task), at)
}

fn resume_tag(node: &Node, at: Millis) -> String {
    format!("({} resume@{at})", node.short_id)
}

/// The orchestrator hears about a restart that failed, once per attempt, unless the node came
/// back anyway.
pub fn resume_failed_message(view: &View, node: &Node, user: &str, at: Millis) -> Option<Message> {
    let attempt = node.resumed.as_ref().filter(|r| !r.ok && !node.alive)?;
    let orchestrator = view.nodes.values().find(|n| n.is_orchestrator && n.alive)?;
    let tag = resume_tag(node, attempt.at);
    let told = view.messages.values().any(|m| {
        m.session_id == orchestrator.session_id && m.from == MessageFrom::Alondra && m.text.starts_with(RESUME_FAILED_PREFIX) && m.text.contains(&tag)
    });
    if told {
        return None;
    }
    let task = open_task(view, node)?;
    let text = format!(
        "{RESUME_FAILED_PREFIX}: {} {tag} stopped while its task \"{}\" was open, and `claude respawn` failed: {}. \
         Writing to it with `send` restarts it too; if that fails as well, it is a question for {user}.",
        node.display_name(),
        task.title,
        attempt.error.as_deref().unwrap_or("no reason given")
    );
    view.alondra_message(&orchestrator.session_id, text, at)
}

/// A node Alondra closes by itself once it has been free for `after` (`close_free_nodes_after`):
/// nothing runs in it, nobody is at its terminal, nothing is on its way to it, and none of its
/// tasks is running, waiting for the user, in review or closing.
pub fn free_to_close(view: &View, node: &Node, now: Millis, after: Millis) -> bool {
    let sid = node.session_id.as_str();
    let quiet_since = [node.last_stop_at, node.handed_at, node.exited_at].into_iter().flatten().fold(node.last_seen, Millis::max);
    let task_open = view.node_tasks(sid).any(|t| matches!(t.state, TaskState::Running | TaskState::NeedsYou | TaskState::InReview | TaskState::Closing));
    let asks = view.questions.values().any(|q| q.session_id == sid && q.status.is_open());
    node.watched()
        && !node.busy
        && !node.waiting_permission
        && node.menu_waiting.is_none()
        && now - quiet_since >= after
        && !task_open
        && !asks
        && !states::watching(view, node, now)
        && !states::works(view, node, now)
        && view.deliverable_messages(sid).next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{DaemonObservation, Event, HookEvent};
    use crate::model::{Question, QuestionOrigin, QuestionStatus, Report, ReportKind};

    const T0: Millis = 1_000_000;
    const SEC: Millis = 1000;

    fn seen(state: &str, pid: Option<i32>) -> Event {
        Event::DaemonSeen(DaemonObservation { session_id: "s".into(), short_id: "s".into(), name: Some("n".into()), pid, state: Some(state.into()), tempo: Some("idle".into()), ..Default::default() })
    }

    fn hook(session: &str, event: &str) -> Event {
        Event::Hook(HookEvent { session_id: session.into(), event: event.into(), ..Default::default() })
    }

    fn exited(pid: i32) -> Event {
        Event::ProcessExited { session_id: "s".into(), pid }
    }

    fn resumed(ok: bool) -> Event {
        Event::Resumed { session_id: "s".into(), ok, error: (!ok).then(|| "no job matching s".to_string()) }
    }

    fn final_report() -> Event {
        Event::ReportPosted(Report {
            id: 1, session_id: "s".into(), node_name: None, title: "Done".into(), body: None, links: vec![], attachments: vec![], kind: ReportKind::Final,
            created_at: T0, read: false, to_orchestrator: false, mission: None, looks_like_question: false, related: vec![],
        })
    }

    fn question() -> Event {
        Event::QuestionAsked(Question {
            id: 1, session_id: "s".into(), node_name: None, title: "Deploy?".into(), detail: None, options: vec![], links: vec![], attachments: vec![],
            blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None, status: QuestionStatus::Open,
            answer: None, asked_at: T0, action: false, steps: vec![], mission: None, related: vec![],
        })
    }

    fn orchestrator() -> [Event; 2] {
        [Event::MarkedOrchestrator { session_id: "o".into() }, hook("o", "SessionStart")]
    }

    /// Events one second apart from T0.
    fn run(events: &[Event]) -> View {
        let mut v = View::default();
        for (i, e) in events.iter().enumerate() {
            v.apply(i as u64 + 1, T0 + i as Millis * SEC, e);
        }
        v
    }

    fn died(after: &[Event]) -> Vec<Event> {
        [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("stopped", None)], after].concat()
    }

    #[test]
    fn a_process_that_died_while_its_node_had_work_is_restarted_once_per_exit() {
        let exit = T0 + 2 * SEC;
        let later = exit + RESUME_GRACE_MS;
        let again = [seen("working", Some(8)), exited(8), seen("stopped", None)];
        let cases: Vec<(&str, Vec<Event>, Millis, bool)> = vec![
            ("died while working, past the grace", died(&[]), later, true),
            ("within the grace the daemon may still restart it", died(&[]), exit + 30 * SEC, false),
            ("the daemon let it go after its turn", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("done", None)], later, false),
            ("the daemon let it go while it waits for an answer", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("blocked", None)], later, false),
            ("it reported final before", [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), final_report()][..], &[exited(7), seen("stopped", None)]].concat(), later, false),
            ("its process came back", died(&[seen("working", Some(8))]), later, false),
            ("the user attached to it", died(&[Event::AttachChanged { session_id: "s".into(), attached: true }]), later, false),
            ("already restarted after this exit", died(&[resumed(true)]), exit + 3600 * SEC, false),
            ("a failed restart is not retried for the same exit", died(&[resumed(false)]), exit + 3600 * SEC, false),
            ("died again right after a restart", died(&[&[resumed(true)][..], &again].concat()), later + 60 * SEC, false),
            ("died again, past the backoff", died(&[&[resumed(true)][..], &again].concat()), T0 + 4 * SEC + RESUME_BACKOFF_MS, true),
        ];
        for (what, events, now, due) in cases {
            let v = run(&events);
            assert_eq!(resume_due(&v, &v.nodes["s"], now).map(|t| t.id.as_str()), due.then_some("t-s"), "{what}");
        }
        let mut orchestrator = run(&died(&[]));
        orchestrator.apply(9, T0, &Event::MarkedOrchestrator { session_id: "s".into() });
        assert!(resume_due(&orchestrator, &orchestrator.nodes["s"], later).is_none(), "the keeper looks after the orchestrator");
    }

    #[test]
    fn a_restarted_node_is_told_to_carry_on_and_a_failed_restart_reaches_the_orchestrator_once() {
        let v = run(&died(&[resumed(true)]));
        let m = resumed_message(&v, "s", T0 + 90 * SEC).unwrap();
        assert!(m.session_id == "s" && m.text.starts_with(RESUMED_PREFIX) && m.text.contains("\"n\""), "{}", m.text);

        let mut v = run(&[&orchestrator()[..], &died(&[resumed(false)])].concat());
        let at = T0 + 90 * SEC;
        let told = resume_failed_message(&v, &v.nodes["s"], "Ana", at).unwrap();
        assert_eq!(told.session_id, "o");
        assert!(told.text.starts_with(RESUME_FAILED_PREFIX) && told.text.contains("no job matching s") && told.text.contains("Ana"), "{}", told.text);
        v.apply(20, at, &Event::MessageQueued(told));
        assert!(resume_failed_message(&v, &v.nodes["s"], "Ana", at).is_none(), "once per attempt");

        let back = run(&[&orchestrator()[..], &died(&[resumed(false), seen("working", Some(9))])].concat());
        assert!(resume_failed_message(&back, &back.nodes["s"], "Ana", at).is_none(), "it came back anyway");
        let alone = run(&died(&[resumed(false)]));
        assert!(resume_failed_message(&alone, &alone.nodes["s"], "Ana", at).is_none(), "no orchestrator to tell yet");
    }

    #[test]
    fn a_node_is_closed_by_itself_only_after_being_free_that_long_with_no_task_on_it() {
        const H: Millis = 3600 * SEC;
        let done = |extra: &[Event]| [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), final_report(), hook("s", "Stop")][..], extra].concat();
        let from_user = crate::model::Message {
            id: 1, session_id: "s".into(), from: MessageFrom::User, text: "one more thing".into(), answers_question: None, created_at: T0,
            handed_at: None, delivered_at: None, about: None, attachments: vec![],
        };
        let pr = Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into() };
        let cases: Vec<(&str, Vec<Event>, Millis, bool)> = vec![
            ("done and free for two hours", done(&[]), 2 * H, true),
            ("done an hour ago", done(&[]), H, false),
            ("its task still running", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), hook("s", "Stop")], 2 * H, false),
            ("its PR waits for review", [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), pr][..], &[final_report(), hook("s", "Stop")]].concat(), 2 * H, false),
            ("its question waits for the user", [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), question()][..], &[hook("s", "Stop")]].concat(), 2 * H, false),
            ("still inside a turn", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), hook("s", "PreToolUse")], 2 * H, false),
            ("a message of the user on its way", done(&[Event::MessageQueued(from_user)]), 2 * H, false),
            ("the user attached to it", done(&[Event::AttachChanged { session_id: "s".into(), attached: true }]), 2 * H, false),
            ("a foreground session", vec![hook("s", "UserPromptSubmit"), final_report(), hook("s", "Stop")], 2 * H, false),
        ];
        for (what, events, after_last, closes) in cases {
            let v = run(&events);
            let now = T0 + (events.len() as Millis - 1) * SEC + after_last;
            assert_eq!(free_to_close(&v, &v.nodes["s"], now, 2 * H), closes, "{what}");
        }
        let mut orchestrator = run(&done(&[]));
        orchestrator.apply(9, T0, &Event::MarkedOrchestrator { session_id: "s".into() });
        assert!(!free_to_close(&orchestrator, &orchestrator.nodes["s"], T0 + 3 * H, 2 * H), "the orchestrator is never closed");
    }
}
