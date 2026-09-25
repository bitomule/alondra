//! What Alondra does to a node's process on its own: restart one that ended while the node still
//! had work, and close a node that has been free for long when the user asked for that. Both read
//! stored events only, so a restart of alondrad neither repeats an attempt nor forgets one.

use crate::model::{Message, MessageFrom, Millis, Node, Question, QuestionOrigin, QuestionStatus, TurnSignal};
use crate::states;
use crate::tasks::Task;
use crate::view::View;

const MINUTE: Millis = 60 * 1000;
/// The Claude daemon restarts a process that exits unexpectedly by itself, and records where it
/// left the session right after the exit (code.claude.com/docs/en/agent-view, "The supervisor
/// process"): Alondra steps in only once this has passed.
pub const RESUME_GRACE_MS: Millis = MINUTE;
/// A node restarted this recently is not restarted again, even when its process ended again.
pub const RESUME_BACKOFF_MS: Millis = 15 * MINUTE;
/// Where the daemon leaves a session whose process ended without dying: `done` or `blocked` when
/// it stopped an idle one (a reply brings it back), `stopped` when someone stopped it on purpose.
const NOT_DEAD_STATES: &[&str] = &["done", "blocked", STOPPED];
const STOPPED: &str = "stopped";

/// Restarts in a row for one task, with no turn completed in between, before Alondra leaves the
/// node to the orchestrator.
pub const MAX_RESUMES: u32 = 3;

pub const RESUMED_PREFIX: &str = "[Alondra] Your process stopped";
pub const RESUME_FAILED_PREFIX: &str = "[Alondra] Could not restart a node";
pub const RESUMES_GIVEN_UP_PREFIX: &str = "[Alondra] Stopped restarting a node";

/// The node's process ended while it was working: neither the daemon letting an idle session go
/// nor someone stopping it.
pub fn died_working(node: &Node) -> bool {
    node.watched() && !node.alive && node.exited_at.is_some() && node.daemon_state.as_deref().is_some_and(|s| !NOT_DEAD_STATES.contains(&s))
}

/// Someone stopped the node's session on purpose: Alondra neither restarts it nor types into it.
pub fn stopped_on_purpose(node: &Node) -> bool {
    node.watched() && !node.alive && node.daemon_state.as_deref() == Some(STOPPED)
}

/// The task a dead process leaves open: not finished, and not closed by the node's own final report.
pub(crate) fn open_task<'a>(view: &'a View, node: &Node) -> Option<&'a Task> {
    view.tasks_of(&node.session_id).find(|t| !t.state.finished() && t.final_report.is_none())
}

/// The node died again after `MAX_RESUMES` restarts in a row for the same task, none of which
/// got a turn done: another restart would only repeat them.
fn restarts_spent(node: &Node, task: &Task) -> bool {
    node.resumed.as_ref().is_some_and(|r| r.streak >= MAX_RESUMES && r.task.as_deref() == Some(task.id.as_str()) && node.last_stop_at.is_none_or(|stop| stop < r.at))
}

/// The open task of a node whose process died, when it is time to restart it: once per exit, not
/// twice within the backoff, and not after `MAX_RESUMES` restarts that got nothing done.
pub fn resume_due<'a>(view: &'a View, node: &Node, now: Millis) -> Option<&'a Task> {
    let exited = node.exited_at?;
    if !died_working(node) || node.attached || now - exited < RESUME_GRACE_MS || node.turn_signal == Some(TurnSignal::Finished) {
        return None;
    }
    if node.resumed.as_ref().is_some_and(|r| r.exit == Some(exited) || now - r.at < RESUME_BACKOFF_MS) {
        return None;
    }
    open_task(view, node).filter(|t| !restarts_spent(node, t))
}

/// The orchestrator hears once that Alondra stopped restarting a node that keeps dying.
pub fn resumes_given_up_message(view: &View, node: &Node, at: Millis) -> Option<Message> {
    let attempt = node.resumed.as_ref()?;
    let task = open_task(view, node).filter(|t| restarts_spent(node, t))?;
    if !died_working(node) || node.exited_at.is_none_or(|exit| exit <= attempt.at) {
        return None;
    }
    let orchestrator = view.nodes.values().find(|n| n.is_orchestrator && n.alive)?;
    let tag = resume_tag(node, attempt.at);
    let told = view.messages.values().any(|m| m.session_id == orchestrator.session_id && m.text.starts_with(RESUMES_GIVEN_UP_PREFIX) && m.text.contains(&tag));
    if told {
        return None;
    }
    let text = format!(
        "{RESUMES_GIVEN_UP_PREFIX}: {} {tag} stopped again after {} restarts in a row while its task “{}” was open, without finishing a turn in between. \
         Alondra no longer restarts it: look at it (`node`), then `send` it what to do, or give the task to another node.",
        node.display_name(),
        attempt.streak,
        task.title
    );
    view.alondra_message(&orchestrator.session_id, text, at)
}

pub fn resumed_text(task: &Task) -> String {
    format!(
        "{RESUMED_PREFIX} while your task “{}” was still open, and Alondra restarted it. Carry on where you left off. \
         If you had already finished, say so through Alondra (`report` with kind final).",
        task.title
    )
}

/// What the node is told once its process is back, unless the same word from an earlier restart
/// has not reached it yet.
pub fn resumed_message(view: &View, session_id: &str, at: Millis) -> Option<Message> {
    let task = open_task(view, view.nodes.get(session_id)?)?;
    if view.pending_messages(session_id).any(|m| m.from == MessageFrom::Alondra && m.text.starts_with(RESUMED_PREFIX)) {
        return None;
    }
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
        "{RESUME_FAILED_PREFIX}: {} {tag} stopped while its task “{}” was open, and `claude respawn` failed: {}. \
         Writing to it with `send` restarts it too; if that fails as well, it is a question for {user}.",
        node.display_name(),
        task.title,
        attempt.error.as_deref().unwrap_or("no reason given")
    );
    view.alondra_message(&orchestrator.session_id, text, at)
}

/// No turn runs in the node and nothing works for it in the background: typing into it starts a
/// turn rather than joining one.
pub fn idle(view: &View, node: &Node, now: Millis) -> bool {
    node.watched()
        && !node.busy
        && !node.waiting_permission
        && node.menu_waiting.is_none()
        && node.background.is_empty()
        && !states::works(view, node, now)
}

pub const COMPACT_COMMAND: &str = "/compact";
/// A `/compact` whose `PostCompact` has not come by then is given up: what waits for it goes in.
/// Claude Code sends no `PostCompact` when there is too little to compact.
pub const COMPACTION_TIMEOUT_MS: Millis = 10 * MINUTE;

/// The node is about to get a task and is idle with its context at least `at_percent` full: it
/// is compacted before the task is typed, so the new work starts from a summary of the old.
pub fn compaction_due(view: &View, node: &Node, now: Millis, at_percent: Option<f64>) -> bool {
    let full = at_percent.zip(node.context).is_some_and(|(limit, c)| c.percent >= limit);
    full && !node.is_orchestrator && node.compaction.is_none() && idle(view, node, now)
}

pub fn compaction_overdue(node: &Node, now: Millis) -> bool {
    node.compaction.is_some_and(|c| now - c.typed_at.unwrap_or(c.requested_at) >= COMPACTION_TIMEOUT_MS)
}

fn quiet_since(node: &Node) -> Millis {
    [node.last_stop_at, node.handed_at, node.exited_at].into_iter().flatten().fold(node.last_seen, Millis::max)
}

/// The node has been idle for `after`: nobody is at its terminal, nothing is on its way to it,
/// and it is not the orchestrator, which Alondra keeps alive.
pub fn idle_for(view: &View, node: &Node, now: Millis, after: Millis) -> bool {
    !node.is_orchestrator
        && idle(view, node, now)
        && node.compaction.is_none()
        && now - quiet_since(node) >= after
        && !states::watching(view, node, now)
        && view.deliverable_messages(&node.session_id).next().is_none()
}

/// What the node leaves unfinished, as far as the view knows: its tasks not done or cancelled and
/// its open questions. Its worktree is checked apart (`worktree::leftovers`).
pub fn pending_work(view: &View, node: &Node) -> Vec<String> {
    let sid = node.session_id.as_str();
    let tasks = view.tasks_of(sid).filter(|t| !t.state.finished()).map(|t| format!("task {} “{}” is {}", t.id, t.title, t.state.name().replace('_', " ")));
    let asks = view.questions.values().filter(|q| q.session_id == sid && q.status.is_open()).map(|q| format!("question #{} “{}” waits for an answer", q.id, q.title));
    tasks.chain(asks).collect()
}

/// A node Alondra closes by itself once it has been idle for `after` (`close_free_nodes_after`)
/// with nothing unfinished. Its worktree must hold nothing the remote's default branch lacks,
/// which alondrad checks before closing it.
pub fn free_to_close(view: &View, node: &Node, now: Millis, after: Millis) -> bool {
    idle_for(view, node, now, after) && pending_work(view, node).is_empty()
}

pub const IDLE_CLOSE_KEY: &str = "idle-close@";
pub const KEEP: &str = "Keep it";
pub const DISCARD: &str = "Close it and discard its work";
pub const RESUME: &str = "Resume it";

pub fn is_idle_close(q: &Question) -> bool {
    q.tool_use_id.as_deref().is_some_and(|k| k.starts_with(IDLE_CLOSE_KEY))
}

/// The question for a node idle for `after` that Alondra may not close: keep it, close it
/// discarding what it left, or have it resume. Once per idle stretch, and not while the node
/// already has a question in front of the user.
pub fn idle_close_question(view: &View, node: &Node, pending: &[String], after: Millis, at: Millis) -> Option<Question> {
    let key = format!("{IDLE_CLOSE_KEY}{}", quiet_since(node));
    let asked = view.questions.values().any(|q| q.session_id == node.session_id && (q.status.is_open() || q.tool_use_id.as_deref() == Some(key.as_str())));
    if pending.is_empty() || asked {
        return None;
    }
    let days = (after as f64 / (24.0 * 60.0 * MINUTE as f64)).round().max(1.0);
    let span = if days == 1.0 { "1 day".to_string() } else { format!("{days} days") };
    let name = node.display_name();
    let detail = format!(
        "{name} has done nothing for {span}, but Alondra does not close it because it has unfinished work:\n\n{}\n\n\
         - {KEEP}: Alondra leaves it as it is and does not ask again until it works and goes idle again.\n\
         - {DISCARD}: Alondra deletes its worktree and closes it: uncommitted changes are lost, its commits stay on its local branch.\n\
         - {RESUME}: Alondra tells {name} to pick the work up again.",
        pending.iter().map(|p| format!("- {p}")).collect::<Vec<_>>().join("\n")
    );
    let option = |label: &str| crate::model::QuestionOption { label: label.into(), description: None };
    Some(Question {
        id: view.questions.keys().next_back().copied().unwrap_or(0) + 1,
        session_id: node.session_id.clone(),
        node_name: Some(name.to_string()),
        title: format!("{name} has been idle for {span} with unfinished work: keep it, close it or resume it?"),
        detail: Some(detail),
        options: vec![option(KEEP), option(DISCARD), option(RESUME)],
        links: vec![],
        attachments: vec![],
        blocking: false,
        origin: QuestionOrigin::Ask,
        tool_use_id: Some(key),
        to_orchestrator: false,
        node_context: None,
        status: QuestionStatus::Open,
        answer: None,
        asked_at: at,
        action: false,
        steps: vec![],
        mission: node.mission.clone(),
        replaces: None,
        task: view.fact_task(&node.session_id, None),
    })
}

pub fn resume_idle_text(user: &str, question: &Question) -> String {
    format!(
        "[Alondra] You have been idle for days with unfinished work, and {user} wants you to pick it up: finish it, or say through Alondra what stops you.\n\n{}",
        question.detail.as_deref().unwrap_or_default()
    )
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
            created_at: T0, read: false, to_orchestrator: false, mission: None, looks_like_question: false, task: None,
        })
    }

    fn question() -> Event {
        Event::QuestionAsked(Question {
            id: 1, session_id: "s".into(), node_name: None, title: "Deploy?".into(), detail: None, options: vec![], links: vec![], attachments: vec![],
            blocking: true, origin: QuestionOrigin::Ask, tool_use_id: None, to_orchestrator: false, node_context: None, status: QuestionStatus::Open,
            answer: None, asked_at: T0, action: false, steps: vec![], mission: None, replaces: None, task: None,
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
        [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("working", None)], after].concat()
    }

    #[test]
    fn a_process_that_died_while_its_node_had_work_is_restarted_once_per_exit() {
        let exit = T0 + 2 * SEC;
        let later = exit + RESUME_GRACE_MS;
        let again = [seen("working", Some(8)), exited(8), seen("failed", None)];
        let cases: Vec<(&str, Vec<Event>, Millis, bool)> = vec![
            ("died while working, past the grace", died(&[]), later, true),
            ("the daemon marked it failed", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("failed", None)], later, true),
            ("someone stopped it on purpose", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("stopped", None)], later, false),
            ("within the grace the daemon may still restart it", died(&[]), exit + 30 * SEC, false),
            ("the daemon let it go after its turn", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("done", None)], later, false),
            ("the daemon let it go while it waits for an answer", vec![seen("working", Some(7)), hook("s", "UserPromptSubmit"), exited(7), seen("blocked", None)], later, false),
            ("it reported final before", [&[seen("working", Some(7)), hook("s", "UserPromptSubmit"), final_report()][..], &[exited(7), seen("working", None)]].concat(), later, false),
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
        assert!(m.session_id == "s" && m.text.starts_with(RESUMED_PREFIX) && m.text.contains("“n”"), "{}", m.text);

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
        let pr = Event::PrOpened { url: "https://github.com/a/b/pull/7".into(), session_id: "s".into(), task: None };
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
        assert!(!idle_for(&orchestrator, &orchestrator.nodes["s"], T0 + 3 * H, 2 * H), "nor asked about");
    }

    #[test]
    fn a_compaction_holds_what_is_queued_until_it_ends_or_is_given_up() {
        use crate::event::CompactionStep;
        let idle = [seen("working", Some(7)), hook("s", "UserPromptSubmit"), final_report(), hook("s", "Stop")];
        let measured = |percent: f64| Event::ContextMeasured { session_id: "s".into(), percent, window: 200_000 };
        let v = run(&[&idle[..], &[measured(29.0)]].concat());
        let now = T0 + 60 * SEC;
        assert!(!compaction_due(&v, &v.nodes["s"], now, Some(30.0)), "below the threshold");
        let v = run(&[&idle[..], &[measured(30.0)]].concat());
        assert!(compaction_due(&v, &v.nodes["s"], now, Some(30.0)), "at the threshold");
        assert!(!compaction_due(&v, &v.nodes["s"], now, None), "the setting off");
        let busy = run(&[&idle[..], &[measured(90.0), hook("s", "UserPromptSubmit")]].concat());
        assert!(!compaction_due(&busy, &busy.nodes["s"], now, Some(30.0)), "in a turn");

        let mut v = run(&[&idle[..], &[measured(60.0), Event::Compaction { session_id: "s".into(), step: CompactionStep::Requested }]].concat());
        let task = Message {
            id: 1, session_id: "s".into(), from: MessageFrom::Orchestrator, text: "[Task t-2] Next".into(), answers_question: None, created_at: T0,
            handed_at: None, delivered_at: None, about: None, attachments: vec![],
        };
        v.apply(20, T0 + 7 * SEC, &Event::MessageQueued(task));
        assert_eq!(v.deliverable_messages("s").count(), 0);
        let requested = v.nodes["s"].compaction.unwrap().requested_at;
        assert!(!compaction_overdue(&v.nodes["s"], requested + COMPACTION_TIMEOUT_MS - 1));
        assert!(compaction_overdue(&v.nodes["s"], requested + COMPACTION_TIMEOUT_MS), "no PostCompact came: Claude Code sends none when there is too little to compact");
        v.apply(21, T0 + 8 * SEC, &Event::Compaction { session_id: "s".into(), step: CompactionStep::GaveUp });
        assert_eq!(v.deliverable_messages("s").count(), 1, "given up, the task goes in");
    }
}
