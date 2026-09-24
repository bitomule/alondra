//! The activity ledger: what each thread of a node (its main conversation and each subagent
//! running now) is doing, since when, and for which task. Hooks carry `agent_id` and
//! `agent_type` when they fire inside a subagent (code.claude.com/docs/en/hooks), so the ledger
//! is built from stored events alone and a replay builds the same one.

use serde::{Deserialize, Serialize};

use crate::event::HookEvent;
use crate::model::{Ledger, Millis, Node, Thread, ToolInFlight};
use crate::tasks::Task;
use crate::view::View;

/// Alondra's own MCP tools, the ones that can name a task.
pub const ALONDRA_TOOLS: &str = "mcp__plugin_alondra";

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

    pub fn bound(&self, agent_id: Option<&str>) -> Option<&str> {
        self.bound.iter().find(|(a, _)| a.as_deref() == agent_id).map(|(_, t)| t.as_str())
    }

    /// What a hook says about the thread it fired in. `binds` tells whether the task the hook
    /// names is one this node may bind to.
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
            "PostToolUse" | "PostToolUseFailure" => {
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
            && self.bound(agent).is_none()
            && let Some(task) = &h.task
        {
            self.bound.push((agent.map(str::to_string), task.clone()));
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

impl View {
    pub(crate) fn note_activity(&mut self, at: Millis, h: &HookEvent) {
        let binds = h.task.as_ref().and_then(|t| self.board.tasks.get(t)).is_some_and(|t| t.is_assigned_to(&h.session_id));
        if let Some(node) = self.nodes.get_mut(&h.session_id) {
            node.ledger.on_hook(h, at, binds);
        }
    }

    /// Which thread of the node is calling one of Alondra's tools right now: its `PreToolUse`
    /// reached Alondra before the call did. `Some(None)` is the main thread; none when no thread,
    /// or more than one, is inside such a call.
    pub fn alondra_caller(&self, session_id: &str) -> Option<Option<&str>> {
        let node = self.nodes.get(session_id)?;
        let mut calling = node.ledger.threads.iter().filter(|t| t.tool.as_ref().is_some_and(|x| x.name.starts_with(ALONDRA_TOOLS)));
        match (calling.next(), calling.next()) {
            (Some(t), None) => Some(t.agent_id.as_deref()),
            _ => None,
        }
    }

    /// The node's only open task, or its only task: everything the node does is that task's.
    fn single_task(&self, session_id: &str) -> Option<&str> {
        let mine: Vec<&Task> = self.tasks_of(session_id).collect();
        let open: Vec<&Task> = mine.iter().copied().filter(|t| !t.state.finished()).collect();
        match (open.as_slice(), mine.as_slice()) {
            ([one], _) | ([], [one]) => Some(one.id.as_str()),
            _ => None,
        }
    }

    /// Each thread of the node with the task it works on: the one it bound itself to, the one
    /// assigned to it, or the node's single task.
    pub fn activity(&self, node: &Node) -> Vec<Activity> {
        let sid = node.session_id.as_str();
        let single = self.single_task(sid);
        node.ledger
            .threads
            .iter()
            .map(|t| {
                let agent = t.agent_id.as_deref();
                let assigned = agent.and_then(|a| self.tasks_of(sid).find(|task| task.assignee.as_ref().is_some_and(|x| x.agent_id.as_deref() == Some(a))).map(|task| task.id.as_str()));
                let task = node.ledger.bound(agent).or(assigned).or(single);
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

    struct Run {
        view: View,
        seq: u64,
    }

    impl Run {
        fn new() -> Self {
            Run { view: View::default(), seq: 0 }
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
            self.view.apply(self.seq, self.seq as Millis * 1000, &e);
            self
        }

        fn activity(&self) -> Vec<Activity> {
            self.view.activity(&self.view.nodes["s"])
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
            self
        }
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
        assert_eq!(restored.activity(&restored.nodes["s"]), run.activity());
        assert_eq!(run.activity()[1].task.as_deref(), Some("t-other"));
    }
}
