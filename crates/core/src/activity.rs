//! The activity ledger: what each thread of a node (its main conversation and each subagent
//! running now) is doing, since when, and for which task. Hooks carry `agent_id` and
//! `agent_type` when they fire inside a subagent (code.claude.com/docs/en/hooks), so the ledger
//! is built from stored events alone and a replay builds the same one.
//!
//! Claude Code never sends `SubagentStop` for a subagent's own subagents (a fork, for example):
//! their hooks reach the parent session, but nothing ever tells it they are gone. `activity`
//! drops such a thread once it has been silent past `THREAD_QUIET_MS` with no tool in
//! flight within its call limit, the same clock-and-window rule `states` uses everywhere else.
//! The main thread is never dropped this way; `SubagentStop` still drops a subagent at once.
//!
//! Claude Code gives no parent call ID to SubagentStart. A child therefore binds only after it
//! claims the token injected into its own Agent prompt. The node's focus applies to its main
//! thread; it never assigns an unbound subagent to a task.

use serde::{Deserialize, Serialize};

use crate::event::HookEvent;
use crate::model::{Ledger, Millis, Node, Thread, ToolInFlight};
use crate::states::{THREAD_QUIET_MS, call_limit};
use crate::tasks::Task;
use crate::view::View;

/// Alondra's own MCP tools, the ones that can name a task.
pub const ALONDRA_TOOLS: &str = "mcp__plugin_alondra";

pub const FOCUS_DEFAULT_MINUTES: u32 = 30;
pub const FOCUS_MAX_MINUTES: u32 = 240;

/// A thread as clients get it, in `activity` on each node.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Activity {
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub task: Option<String>,
    pub tool: Option<String>,
    pub summary: Option<String>,
    pub since: Millis,
}

impl Ledger {
    fn thread(&mut self, agent_id: Option<&str>, at: Millis) -> &mut Thread {
        let i = match self.threads.iter().position(|t| t.agent_id.as_deref() == agent_id) {
            Some(i) => i,
            None => {
                let thread = Thread { agent_id: agent_id.map(str::to_string), agent_type: None, tool: None, since: at };
                let i = if agent_id.is_none() { 0 } else { self.threads.len() };
                self.threads.insert(i, thread);
                i
            }
        };
        &mut self.threads[i]
    }

    /// The task a subagent claimed with the token from its own Agent prompt.
    pub fn bound(&self, agent_id: &str) -> Option<&str> {
        self.bound.iter().find(|(a, _)| a.as_deref() == Some(agent_id)).map(|(_, t)| t.as_str())
    }

    /// What a hook says about the thread it fired in. `binds` tells whether the task the hook
    /// names is one this node may bind to. Only a subagent binds, with its first Alondra call that
    /// names a task; the main thread's `task` is for
    /// that call alone. 25 Sep 00:01: the main thread of 7e661a50 asked once for t-44370 and
    /// everything it did after counted as t-44370.
    fn on_hook(&mut self, h: &HookEvent, at: Millis, binds: bool) {
        let agent = h.agent_id.as_deref();
        self.thread(None, at);
        if h.event == "SubagentStop" && agent.is_some() {
            self.threads.retain(|t| t.agent_id.as_deref() != agent);
            return;
        }
        let thread = self.thread(agent, at);
        if h.agent_type.is_some() {
            thread.agent_type = h.agent_type.clone();
        }
        match h.event.as_str() {
            "PreToolUse" => {
                thread.tool = Some(ToolInFlight { tool_use_id: h.tool_use_id.clone(), name: h.tool_name.clone().unwrap_or_default(), summary: h.tool_summary.clone(), since: at });
                thread.since = at;
            }
            "PostToolUse" | "PostToolUseFailure" | "PermissionDenied" => {
                let same = match (&thread.tool, &h.tool_use_id) {
                    (Some(t), Some(id)) => t.tool_use_id.as_deref() == Some(id.as_str()),
                    (Some(_), None) => true,
                    _ => false,
                };
                if same {
                    thread.tool = None;
                    thread.since = at;
                }
            }
            "UserPromptSubmit" | "Stop" | "StopFailure" | "SessionStart" => {
                thread.tool = None;
                thread.since = at;
            }
            _ => {}
        }
        let alondra_call = h.event == "PostToolUse" && h.tool_name.as_deref().is_some_and(|t| t.starts_with(ALONDRA_TOOLS));
        if alondra_call
            && binds
            && let Some(agent) = agent
            && self.bound(agent).is_none()
            && let Some(task) = &h.task
        {
            self.bound.push((Some(agent.to_string()), task.clone()));
        }
    }

    /// The process is gone and so is everything running in it; subagents that carry over to the
    /// next process show up again with their next hook.
    pub(crate) fn on_exit(&mut self, at: Millis) {
        self.threads.retain(|t| t.agent_id.is_none());
        for t in &mut self.threads {
            t.tool = None;
            t.since = at;
        }
    }

    pub(crate) fn drop_main_tool(&mut self, at: Millis) {
        if let Some(main) = self.threads.iter_mut().find(|t| t.agent_id.is_none() && t.tool.is_some()) {
            main.tool = None;
            main.since = at;
        }
    }
}

/// A subagent thread with no tool in flight is live for `THREAD_QUIET_MS` past its last
/// hook; one with a tool in flight is live up to that tool's own call limit, whichever is
/// longer. The main thread is always live.
fn thread_alive(t: &Thread, now: Millis) -> bool {
    if t.agent_id.is_none() {
        return true;
    }
    if let Some(tool) = &t.tool
        && now - tool.since <= call_limit(&tool.name)
    {
        return true;
    }
    now - t.since <= THREAD_QUIET_MS
}

impl View {
    pub(crate) fn note_activity(&mut self, at: Millis, h: &HookEvent) {
        let binds = h.task.as_ref().and_then(|t| self.board.tasks.get(t)).is_some_and(|t| t.is_assigned_to(&h.session_id));
        if let Some(node) = self.nodes.get_mut(&h.session_id) {
            node.ledger.on_hook(h, at, binds);
        }
    }

    /// The task the node said it works on now (`focus`), while it lasts and is still an open task
    /// of the node.
    pub fn focus(&self, session_id: &str, now: Millis) -> Option<&str> {
        let focus = self.nodes.get(session_id)?.ledger.focus.as_ref().filter(|f| now < f.until)?;
        self.board.tasks.get(&focus.task).filter(|t| t.is_assigned_to(session_id) && !t.state.finished()).map(|t| t.id.as_str())
    }

    /// The node's only open task, or its only task: everything the node does is that task's.
    pub(crate) fn single_task(&self, session_id: &str) -> Option<&str> {
        let mine: Vec<&Task> = self.tasks_of(session_id).collect();
        let open: Vec<&Task> = mine.iter().copied().filter(|t| !t.state.finished()).collect();
        match (open.as_slice(), mine.as_slice()) {
            ([one], _) | ([], [one]) => Some(one.id.as_str()),
            _ => None,
        }
    }

    /// Each thread of the node with the task it works on: the one it claimed or was explicitly
    /// assigned to, the main thread's live focus, or the node's single task. A subagent
    /// thread gone silent past its window (`thread_alive`) is left out, as if it had sent
    /// `SubagentStop`.
    pub fn activity(&self, node: &Node, now: Millis) -> Vec<Activity> {
        let sid = node.session_id.as_str();
        let main_unbound = self.focus(sid, now).or(self.single_task(sid));
        let subagent_unbound = self.single_task(sid);
        node.ledger
            .threads
            .iter()
            .filter(|t| thread_alive(t, now))
            .map(|t| {
                let agent = t.agent_id.as_deref();
                let assigned = agent.and_then(|a| self.tasks_of(sid).find(|task| task.assignee.as_ref().is_some_and(|x| x.agent_id.as_deref() == Some(a))).map(|task| task.id.as_str()));
                let task = agent.and_then(|a| node.ledger.bound(a)).or(assigned).or(if agent.is_some() { subagent_unbound } else { main_unbound });
                Activity {
                    agent_id: t.agent_id.clone(),
                    agent_type: t.agent_type.clone(),
                    task: task.map(str::to_string),
                    tool: t.tool.as_ref().map(|x| x.name.clone()),
                    summary: t.tool.as_ref().and_then(|x| x.summary.clone()),
                    since: t.since,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Event;
    use crate::tasks::{Assignee, TaskState};

    #[derive(Clone)]
    enum Step {
        On(Millis, Box<Event>),
        SecondTask,
    }

    struct Run {
        view: View,
        seq: u64,
        waited: Millis,
        log: Vec<Step>,
    }

    impl Run {
        fn new() -> Self {
            Run { view: View::default(), seq: 0, waited: 0, log: vec![] }
        }

        fn now(&self) -> Millis {
            self.seq as Millis * 1000 + self.waited
        }

        /// The same steps into a fresh view, through a snapshot after each one, as alondrad
        /// restarts from its last snapshot and the events after it.
        fn replayed(&self) -> Run {
            let mut run = Run::new();
            for step in &self.log {
                match step {
                    Step::On(at, e) => {
                        run.seq += 1;
                        run.view.apply(run.seq, *at, e);
                        run.waited = at - run.seq as Millis * 1000;
                    }
                    Step::SecondTask => {
                        run.second_task();
                    }
                }
                run.view = serde_json::from_value(serde_json::to_value(&run.view).unwrap()).unwrap();
            }
            run
        }

        fn hook(&mut self, event: &str, agent: Option<(&str, &str)>, tool: Option<(&str, &str)>, task: Option<&str>) -> &mut Self {
            self.on(Event::Hook(HookEvent {
                session_id: "s".into(),
                event: event.into(),
                agent_id: agent.map(|(id, _)| id.into()),
                agent_type: agent.map(|(_, kind)| kind.into()),
                tool_name: tool.map(|(name, _)| name.into()),
                tool_use_id: tool.map(|(_, id)| id.into()),
                task: task.map(str::to_string),
                ..Default::default()
            }))
        }

        fn on(&mut self, e: Event) -> &mut Self {
            self.seq += 1;
            let at = self.now();
            self.view.apply(self.seq, at, &e);
            self.log.push(Step::On(at, Box::new(e)));
            self
        }

        fn activity(&self) -> Vec<Activity> {
            self.activity_at(self.now())
        }

        fn activity_at(&self, now: Millis) -> Vec<Activity> {
            self.view.activity(&self.view.nodes["s"], now)
        }

        fn rows(&self) -> Vec<(Option<String>, Option<String>, Option<String>)> {
            self.activity().into_iter().map(|a| (a.agent_id, a.task, a.tool)).collect()
        }

        fn second_task(&mut self) -> &mut Self {
            let mut other = self.view.board.tasks["t-s"].clone();
            other.id = "t-other".into();
            other.assignee = Some(Assignee { session_id: "s".into(), agent_id: None });
            other.state = TaskState::Running;
            self.view.board.tasks.insert(other.id.clone(), other);
            self.log.push(Step::SecondTask);
            self
        }

        fn tasks(&self) -> Vec<(Option<String>, Option<String>)> {
            self.activity().into_iter().map(|a| (a.agent_id, a.task)).collect()
        }

        fn focus(&mut self, task: &str, minutes: u32) -> &mut Self {
            self.on(Event::Focused { session_id: "s".into(), task: task.into(), minutes })
        }
    }

    fn working(agent: Option<&str>, task: Option<&str>) -> (Option<String>, Option<String>) {
        (agent.map(str::to_string), task.map(str::to_string))
    }

    const EXPLORE: Option<(&str, &str)> = Some(("a1", "Explore"));
    const REPORT: &str = "mcp__plugin_alondra_alondra__report";

    fn row(agent: Option<&str>, task: Option<&str>, tool: Option<&str>) -> (Option<String>, Option<String>, Option<String>) {
        (agent.map(str::to_string), task.map(str::to_string), tool.map(str::to_string))
    }

    #[test]
    fn the_main_thread_and_each_running_subagent_show_their_tool_and_since_when() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).hook("PreToolUse", None, Some(("Agent", "m1")), None);
        run.hook("SubagentStart", EXPLORE, None, None).hook("PreToolUse", EXPLORE, Some(("Bash", "u1")), None);
        assert_eq!(run.rows(), vec![row(None, Some("t-s"), Some("Agent")), row(Some("a1"), Some("t-s"), Some("Bash"))], "a node with one task: all of it is that task's");
        let a = &run.activity()[1];
        assert_eq!((a.agent_type.as_deref(), a.since), (Some("Explore"), 4000), "since the tool began");

        run.hook("PostToolUse", EXPLORE, Some(("Bash", "u1")), None);
        assert_eq!((run.activity()[1].tool.clone(), run.activity()[1].since), (None, 5000), "between tools, since the last one ended");
        run.hook("PostToolUse", EXPLORE, Some(("Read", "other")), None);
        assert_eq!(run.activity()[1].since, 5000, "the end of another call changes nothing");

        run.hook("SubagentStop", EXPLORE, None, None).hook("PostToolUse", None, Some(("Agent", "m1")), None).hook("Stop", None, None, None);
        assert_eq!(run.rows(), vec![row(None, Some("t-s"), None)], "the subagent left and the turn ended");
        assert_eq!(run.activity()[0].since, 9000);
    }

    #[test]
    fn a_thread_binds_to_the_task_of_its_first_alondra_call_and_keeps_it_across_its_stops() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).second_task();
        run.hook("SubagentStart", EXPLORE, None, None).hook("SubagentStart", Some(("a2", "Plan")), None, None);
        assert_eq!(run.rows(), vec![row(None, None, None), row(Some("a1"), None, None), row(Some("a2"), None, None)], "two open tasks: nothing is guessed");

        run.hook("PostToolUse", EXPLORE, Some(("Read", "r")), Some("t-other"));
        assert_eq!(run.activity()[1].task, None, "only Alondra's own tools bind");
        run.hook("PostToolUseFailure", EXPLORE, Some((REPORT, "f")), Some("t-other"));
        assert_eq!(run.activity()[1].task, None, "a refused call binds nothing");
        run.hook("PostToolUse", EXPLORE, Some((REPORT, "x")), Some("t-missing"));
        assert_eq!(run.activity()[1].task, None, "a task that does not exist binds nothing");
        run.hook("PostToolUse", EXPLORE, Some((REPORT, "y")), Some("t-other")).hook("PostToolUse", EXPLORE, Some((REPORT, "z")), Some("t-s"));
        run.hook("PostToolUse", Some(("a2", "Plan")), Some((REPORT, "w")), Some("t-s"));
        assert_eq!(run.rows(), vec![row(None, None, None), row(Some("a1"), Some("t-other"), None), row(Some("a2"), Some("t-s"), None)], "the first call binds, later ones do not move it");

        run.hook("SubagentStop", EXPLORE, None, None).hook("SubagentStart", EXPLORE, None, None);
        assert_eq!(run.activity().last().and_then(|a| a.task.clone()).as_deref(), Some("t-other"), "SendMessage continues it under the same id");
    }

    /// 25 Sep 00:01: the main thread of 7e661a50 asked once for t-44370, and everything it did
    /// after counted as t-44370.
    #[test]
    fn the_main_thread_never_binds_to_the_task_one_of_its_calls_names() {
        const ASK: &str = "mcp__plugin_alondra_alondra__ask";
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).second_task();
        run.hook("PreToolUse", None, Some((ASK, "q")), None).hook("PostToolUse", None, Some((ASK, "q")), Some("t-other"));
        run.hook("PreToolUse", None, Some(("Bash", "b")), None);
        assert_eq!(run.rows(), vec![row(None, None, Some("Bash"))], "it coordinates both tasks");
        assert!(run.view.nodes["s"].ledger.bound.is_empty());

        run.hook("SubagentStart", EXPLORE, None, None).hook("PostToolUse", EXPLORE, Some((ASK, "a")), Some("t-other"));
        assert_eq!(run.activity()[1].task.as_deref(), Some("t-other"), "a subagent still binds");
    }

    #[test]
    fn a_process_that_ends_takes_its_subagents_and_tools_with_it() {
        let mut run = Run::new();
        run.on(Event::DaemonSeen(crate::event::DaemonObservation { session_id: "s".into(), short_id: "s".into(), pid: Some(7), ..Default::default() }));
        run.hook("UserPromptSubmit", None, None, None).hook("PreToolUse", None, Some(("Bash", "b")), None).hook("PreToolUse", EXPLORE, Some(("Grep", "g")), None);
        run.on(Event::ProcessExited { session_id: "s".into(), pid: 7 });
        assert_eq!(run.rows(), vec![row(None, Some("t-s"), None)]);
        assert_eq!(run.view.nodes["s"].exited_at, Some(5000));
    }

    #[test]
    fn the_ledger_survives_a_snapshot() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).second_task();
        run.hook("PreToolUse", EXPLORE, Some(("Bash", "u")), None).hook("PostToolUse", EXPLORE, Some((REPORT, "r")), Some("t-other"));
        let restored: View = serde_json::from_value(serde_json::to_value(&run.view).unwrap()).unwrap();
        let now = run.seq as Millis * 1000;
        assert_eq!(restored.activity(&restored.nodes["s"], now), run.activity());
        assert_eq!(run.activity()[1].task.as_deref(), Some("t-other"));
    }

    const GHOST_A: Option<(&str, &str)> = Some(("g1", "Explore"));
    const GHOST_B: Option<(&str, &str)> = Some(("g2", "Explore"));
    const GHOST_C: Option<(&str, &str)> = Some(("g3", "Explore"));

    /// 25 Sep 00:23-00:30: three of 7e661a50's nested subagents (forks spawned by its own
    /// subagents) each sent one `PreToolUse` and then nothing. Claude Code never sends
    /// `SubagentStop` for a subagent's own subagents, so they stayed in `activity` forever.
    #[test]
    fn three_ghost_subagents_with_one_pre_tool_use_each_drop_after_their_call_limit_and_not_before() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None);
        run.hook("SubagentStart", GHOST_A, None, None).hook("PreToolUse", GHOST_A, Some(("SubagentHandback", "h1")), None);
        run.hook("SubagentStart", GHOST_B, None, None).hook("PreToolUse", GHOST_B, Some(("Bash", "h2")), None);
        run.hook("SubagentStart", GHOST_C, None, None).hook("PreToolUse", GHOST_C, Some(("Edit", "h3")), None);
        let since: Vec<Millis> = run.activity()[1..].iter().map(|a| a.since).collect();
        let limit = call_limit("Edit");

        let alive = |now: Millis| -> Vec<String> { run.activity_at(now).into_iter().filter_map(|a| a.agent_id).collect() };
        let before = alive(since.iter().copied().min().unwrap() + limit);
        for g in ["g1", "g2", "g3"] {
            assert!(before.contains(&g.to_string()), "{g} is still within its call limit: {before:?}");
        }
        let after = alive(since.iter().copied().max().unwrap() + limit + 1);
        for g in ["g1", "g2", "g3"] {
            assert!(!after.contains(&g.to_string()), "{g} went silent past its call limit and should be dropped: {after:?}");
        }
    }

    #[test]
    fn a_live_subagent_with_a_recent_hook_stays_even_long_after_it_started() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None);
        run.hook("SubagentStart", EXPLORE, None, None)
            .hook("PreToolUse", EXPLORE, Some(("Bash", "u1")), None)
            .hook("PostToolUse", EXPLORE, Some(("Bash", "u1")), None);
        let since = run.activity()[1].since;
        let ids: Vec<String> = run.activity_at(since + THREAD_QUIET_MS).into_iter().filter_map(|a| a.agent_id).collect();
        assert!(ids.contains(&"a1".to_string()), "a hook this recent keeps the thread alive: {ids:?}");
    }

    #[test]
    fn subagent_stop_drops_its_thread_at_once_regardless_of_the_window() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None);
        run.hook("SubagentStart", EXPLORE, None, None).hook("PreToolUse", EXPLORE, Some(("Bash", "u1")), None);
        run.hook("SubagentStop", EXPLORE, None, None);
        let now = run.seq as Millis * 1000;
        let ids: Vec<Option<String>> = run.activity_at(now).into_iter().map(|a| a.agent_id).collect();
        assert_eq!(ids, vec![None], "the subagent is gone the instant it stops, not just after the window");
    }

    const AGENT: &str = "Agent";
    const PLAN: Option<(&str, &str)> = Some(("a2", "Plan"));

    /// An Agent marker and the next SubagentStart cannot prove a parent-child relationship.
    /// Only a successful claim_task hook can bind the child.
    #[test]
    fn agent_start_order_never_binds_a_subagent() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).second_task();
        run.hook("PreToolUse", None, Some(("Agent", "m1")), Some("t-s"));
        run.hook("PreToolUse", None, Some(("Agent", "m2")), Some("t-other"));
        run.hook("SubagentStart", EXPLORE, None, None);
        run.hook("SubagentStart", Some(("a2", "Plan")), None, None);
        assert_eq!(run.tasks(), vec![working(None, None), working(Some("a1"), None), working(Some("a2"), None)]);
        assert!(run.view.nodes["s"].ledger.bound.is_empty());
    }

    #[test]
    fn a_focus_applies_only_to_the_main_thread_until_it_expires_or_is_replaced() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).second_task();
        run.hook("PreToolUse", None, Some((AGENT, "m1")), Some("t-s")).hook("SubagentStart", EXPLORE, None, None);
        run.focus("t-other", 30);
        let until = run.now() + 30 * 60 * 1000;
        run.hook("PreToolUse", None, Some(("Bash", "b1")), None).hook("SubagentStart", PLAN, None, None);
        assert_eq!(
            run.tasks(),
            vec![working(None, Some("t-other")), working(Some("a1"), None), working(Some("a2"), None)],
            "a focus cannot assign either unbound subagent"
        );
        run.hook("PostToolUse", EXPLORE, Some((REPORT, "claim")), Some("t-s"));
        assert_eq!(run.tasks()[1], working(Some("a1"), Some("t-s")), "a bound subagent keeps its own task");

        assert_eq!(run.activity_at(until - 1)[0].task.as_deref(), Some("t-other"));
        assert_eq!(run.activity_at(until)[0].task, None, "30 minutes later it is over");

        run.focus("t-s", 30);
        assert_eq!(run.tasks()[0], working(None, Some("t-s")), "a new focus replaces it");
    }

    #[test]
    fn a_replay_through_snapshots_gives_the_same_ledger() {
        let mut run = Run::new();
        run.hook("UserPromptSubmit", None, None, None).second_task();
        run.hook("PreToolUse", None, Some((AGENT, "m1")), Some("t-other")).hook("SubagentStart", EXPLORE, None, None);
        run.focus("t-s", 30).hook("PreToolUse", None, Some(("Bash", "b1")), None);
        run.hook("PreToolUse", None, Some((AGENT, "m2")), Some("t-other"));
        let mut replayed = run.replayed();
        assert_eq!(replayed.view.nodes["s"].ledger, run.view.nodes["s"].ledger);
        assert_eq!(replayed.activity(), run.activity());

        for r in [&mut run, &mut replayed] {
            r.hook("SubagentStart", PLAN, None, None);
        }
        assert_eq!(replayed.tasks(), run.tasks());
        assert_eq!(run.tasks(), vec![working(None, Some("t-s")), working(Some("a1"), None), working(Some("a2"), None)]);
    }
}
