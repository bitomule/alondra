//! What each session's node mod reports (`plugin/hooks/register.ts`): that it loaded, its turns
//! and the tool it runs. Kept in
//! memory beside the hooks' live view; the domain never records it. The open connection the mod
//! holds (`alondra node connect`) is the node's liveness: a newer connection from the same
//! session replaces the older one, and only the current one closing makes the node unreachable.
//! A session binds to its launch by the key of the settings file the launch gave it
//! (`ALONDRA_DOMAIN_SETTINGS`), which its mod reports in `session.start`.
use crate::node_work::End;
use alondra_core::views::RunOutcome;
use alondra_core::{BackgroundItem, BackgroundKind, Millis, ToolInFlight};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Deserialize, Debug, PartialEq)]
#[serde(tag = "event")]
pub(crate) enum Report {
    #[serde(rename = "session.start")]
    SessionStart { version: String, #[serde(default)] cwd: Option<String>, #[serde(default)] launch_key: Option<String> },
    #[serde(rename = "session.end")]
    SessionEnd,
    /// The model a request was sent with: the main thread's, or with `agent_id` a subagent's.
    #[serde(rename = "model")]
    Model { model: String, #[serde(default)] agent_id: Option<String> },
    #[serde(rename = "turn.start")]
    TurnStart,
    #[serde(rename = "turn.complete")]
    TurnComplete { answer: String, #[serde(default)] aborted: bool },
    #[serde(rename = "tool.start")]
    ToolStart { tool: String, #[serde(default)] tool_use_id: Option<String>, #[serde(default)] input: Value },
    #[serde(rename = "tool.end")]
    ToolEnd { #[serde(default)] tool_use_id: Option<String> },
    /// Every background task Claude Code lists as in flight at the end of a turn (`classic.Stop`'s
    /// `background_tasks`): the whole list, so a task that finished is no longer in it.
    #[serde(rename = "background")]
    Background { #[serde(default)] tasks: Vec<TaskIn> },
    #[serde(rename = "subagent.start")]
    SubagentStart {
        id: String,
        #[serde(default)]
        description: Option<String>,
        #[serde(rename = "type", default)]
        kind: Option<String>,
        #[serde(default)]
        model: Option<String>,
    },
    #[serde(rename = "subagent.end")]
    SubagentEnd { id: String },
    #[serde(rename = "shell.start")]
    ShellStart { id: String, command: String },
    #[serde(rename = "workflow.start")]
    WorkflowStart { id: String, name: String, #[serde(default)] transcript_dir: Option<PathBuf> },
    #[serde(rename = "step.create")]
    StepCreate(crate::node_work::StepIn),
    #[serde(rename = "step.update")]
    StepUpdate(crate::node_work::StepIn),
    /// The whole task list, as `TaskList` or `TodoWrite` gave it.
    #[serde(rename = "steps")]
    Steps { steps: Vec<crate::node_work::StepIn> },
}

#[derive(Deserialize, Debug, PartialEq)]
pub(crate) struct TaskIn {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
}

/// One report as `alondra node event` hands it over.
#[derive(Deserialize, Debug)]
pub(crate) struct Envelope {
    pub session_id: String,
    #[serde(flatten)]
    pub report: Report,
}

/// What `alondra node connect` says first: the same facts as the mod's `session.start`, so a
/// daemon that restarted while the session ran learns them again from the reconnection.
#[derive(Deserialize, Debug)]
pub(crate) struct Hello {
    pub session_id: String,
    pub version: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub launch_key: Option<String>,
    #[serde(default)]
    pub working: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Answer {
    pub text: String,
    pub at: Millis,
    pub aborted: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Reported {
    pub version: Option<String>,
    pub cwd: Option<String>,
    pub started_at: Option<Millis>,
    /// The current connection, by the number it was given; none while the session is unreachable.
    pub connection: Option<u64>,
    /// When the connection it held last closed, while it has none.
    pub disconnected_at: Option<Millis>,
    pub ended: bool,
    pub working_since: Option<Millis>,
    /// When the mod last reported anything or connected: a turn that goes quiet is a session that
    /// stopped processing.
    pub last_event_at: Option<Millis>,
    /// The process of the mod's connection, whose parent is the session's own process.
    pub peer_pid: Option<u32>,
    /// Whether the session has a shell call running, as the last look at its processes saw;
    /// none while nothing is known.
    pub shell_alive: Option<bool>,
    /// When that look was taken: a shell started after it is not yet seen either way.
    pub shell_looked_at: Option<Millis>,
    /// The session process's cumulative CPU time at the last look, in hundredths of a second.
    pub cpu_cs: Option<u64>,
    /// When the session process last used more than idle CPU between two looks, or was first seen.
    pub cpu_busy_at: Option<Millis>,
    /// What Claude Code's own job state says, when its file could be read.
    pub job: Option<crate::job_state::Job>,
    /// Where that file was found, so the folder is not searched for every look.
    pub job_dir: Option<PathBuf>,
    pub tool: Option<ToolInFlight>,
    pub answer: Option<Answer>,
    /// The model id the main thread last sent a request with.
    pub model: Option<String>,
    /// The subagents and background shells the node has running, by the id Claude Code gives them.
    pub background: Vec<(String, BackgroundItem)>,
    /// Whether `background` is Claude Code's whole list (a turn end gave it, or the session ended)
    /// rather than what this daemon heard of since it started.
    pub background_known: bool,
    pub work: crate::node_work::NodeWork,
    /// What only the Agent SDK host's events say, for a node that runs through it.
    pub sdk: Option<crate::sdk_live::Readings>,
}

const LABEL_LIMIT: usize = 80;

pub(crate) fn short_label(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LABEL_LIMIT {
        return flat;
    }
    let cut: String = flat.chars().take(LABEL_LIMIT - 1).collect();
    format!("{}…", cut.trim_end())
}

/// What a node does, as its mod's events and connection say it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Activity {
    /// Inside a turn, or a tool call is running within the time such a call can take.
    Working,
    /// One tool call has run far longer than such a call can.
    Stalled,
    /// Claude Code's job state says the session failed; its detail says why.
    Failed,
    Idle,
}

impl Reported {
    pub(crate) fn activity(&self, now: Millis) -> Activity {
        match self.job.as_ref().map(|job| job.phase) {
            Some(crate::job_state::Phase::Failed) => return Activity::Failed,
            Some(crate::job_state::Phase::Working) => return if self.silent(now) { Activity::Stalled } else { Activity::Working },
            _ => {}
        }
        let call_overran = self.sdk.is_none() && self.tool.as_ref().is_some_and(|tool| now - tool.since > alondra_core::states::call_limit(&tool.name));
        if (call_overran && self.shell_alive != Some(true)) || self.silent(now) {
            Activity::Stalled
        } else if self.working_since.is_some() || self.tool.is_some() || self.background.iter().any(|(_, item)| !self.shell_gone(item)) {
            Activity::Working
        } else {
            Activity::Idle
        }
    }

    /// The session has a shell call running: a build that stays silent for an hour is still work.
    pub(crate) fn shell_running(&self) -> bool {
        self.shell_alive == Some(true)
    }

    fn agent_running(&self, now: Millis) -> bool {
        self.background.iter().any(|(_, item)| item.kind == BackgroundKind::Agent && now - item.started_at <= alondra_core::states::SUBAGENT_CALL_LIMIT_MS)
    }

    /// A tool call, a shell or a subagent is running: a foreground build or a wait on a deploy
    /// leaves the job state and the session's own CPU still for many minutes, so only the call
    /// limits and the dead-shell rule may call it stalled.
    fn busy_with_a_call(&self, now: Millis) -> bool {
        self.tool.is_some() || self.shell_running() || self.agent_running(now)
    }

    /// Connected, yet nothing the session does shows for `TURN_QUIET_MS`: no report from the mod,
    /// no CPU used by the session process, no shell call alive, no subagent. Either it is inside a turn that stopped processing, or
    /// it waits on a call or a background shell that is gone and nothing resumed it.
    pub(crate) fn silent(&self, now: Millis) -> bool {
        match &self.job {
            Some(job) if job.phase == crate::job_state::Phase::Failed => return false,
            Some(job) if job.phase == crate::job_state::Phase::Working && !self.busy_with_a_call(now) => return self.job_stalled(job, now),
            _ => {}
        }
        if self.connection.is_none() || self.shell_running() || self.agent_running(now) {
            return false;
        }
        let quiet_for = |at: Option<Millis>| at.is_some_and(|at| now - at > alondra_core::states::TURN_QUIET_MS);
        let quiet = quiet_for(self.last_event_at.or(self.working_since)) && quiet_for(self.cpu_busy_at);
        let stuck_turn = self.working_since.is_some() && self.tool.is_none();
        let waits_on_dead_shell = self.shell_alive == Some(false)
            && (self.tool.as_ref().is_some_and(|tool| tool.name == "Bash") || self.background.iter().any(|(_, item)| item.kind == BackgroundKind::Shell));
        quiet && (stuck_turn || waits_on_dead_shell)
    }

    /// Claude Code says it works yet nothing is in flight, its state has not moved and its process
    /// has used no CPU for `JOB_STALL_MS`: a long reply being generated shows in the CPU.
    fn job_stalled(&self, job: &crate::job_state::Job, now: Millis) -> bool {
        let still_for = |at: Option<Millis>| at.is_some_and(|at| now - at > alondra_core::states::JOB_STALL_MS);
        self.connection.is_some() && job.tasks == 0 && still_for(Some(job.updated_at)) && still_for(self.cpu_busy_at)
    }

    /// A background command ended when a look taken after it started found its shell gone.
    pub(crate) fn shell_end(&self, item: &BackgroundItem) -> Option<End> {
        let gone = item.kind == BackgroundKind::Shell && self.shell_alive == Some(false);
        self.shell_looked_at.filter(|at| gone && *at > item.started_at).map(|at| End { at, outcome: RunOutcome::Finished })
    }

    fn shell_gone(&self, item: &BackgroundItem) -> bool {
        self.shell_end(item).is_some()
    }

    /// Moves the background tasks `gone` picks out of the running list into the ended ones: a
    /// shell already seen gone ended then, any other with `outcome` now.
    pub(crate) fn end_background(&mut self, gone: impl Fn(&str) -> bool, outcome: RunOutcome, now: Millis) {
        let (ended, kept): (Vec<_>, Vec<_>) = std::mem::take(&mut self.background).into_iter().partition(|(id, _)| gone(id));
        self.background = kept;
        for (id, item) in ended {
            let end = self.shell_end(&item).unwrap_or(End { at: now, outcome });
            self.work.task_ended(&id, item, end);
        }
    }

    /// Everything the node left running is known to be over: Claude Code's whole list is empty, or
    /// holds only commands whose shells are gone.
    pub(crate) fn work_gone(&self) -> bool {
        self.background_known && self.background.iter().all(|(_, item)| self.shell_gone(item))
    }

    /// The mod loaded and its session has not ended, yet its connection is closed: what brings
    /// the node back (`node_revival`), never something Fleet shows as the node's state.
    pub(crate) fn connection_lost(&self) -> bool {
        self.started_at.is_some() && !self.ended && self.connection.is_none()
    }

    /// What one report says of the session, but its start, which only `Reports` records.
    pub(crate) fn record(&mut self, report: Report, now: Millis) {
        match report {
            Report::SessionStart { .. } => {}
            Report::SessionEnd => {
                self.ended = true;
                self.connection = None;
                self.tool = None;
                self.working_since = None;
                self.end_background(|_| true, RunOutcome::Stopped, now);
                self.background_known = true;
                self.work.ended(now);
            }
            Report::Model { model, agent_id: None } => self.model = Some(model),
            Report::Model { model, agent_id: Some(agent) } => self.work.agent_model(&agent, model),
            Report::TurnStart => self.working_since = Some(now),
            Report::TurnComplete { answer, aborted } => {
                self.working_since = None;
                self.tool = None;
                self.answer = Some(Answer { text: answer, at: now, aborted });
            }
            Report::ToolStart { tool, tool_use_id, input } => {
                let summary = crate::hook_text::tool_summary(Some(&tool), Some(&input));
                self.tool = Some(ToolInFlight { tool_use_id, name: tool, summary, since: now });
            }
            Report::Background { tasks } => {
                let listed: Vec<_> = tasks.iter().map(|task| crate::node_work::Listed {
                    id: &task.id,
                    kind: &task.kind,
                    description: &task.description,
                    agent_type: task.agent_type.as_deref(),
                    name: task.name.as_deref(),
                }).collect();
                self.work.listed(&listed, now);
                self.background_known = true;
                self.end_background(|id| !tasks.iter().any(|task| task.id == id), RunOutcome::Finished, now);
                let known = std::mem::take(&mut self.background);
                self.background = tasks.into_iter().map(|task| {
                    let started_at = known.iter().find(|(id, _)| *id == task.id).map_or(now, |(_, item)| item.started_at);
                    let (kind, label) = match task.kind.as_str() {
                        "subagent" => (BackgroundKind::Agent, task.description),
                        "shell" => (BackgroundKind::Shell, task.command.unwrap_or(task.description)),
                        "workflow" => (BackgroundKind::Other, task.name.filter(|name| !name.is_empty()).unwrap_or(task.description)),
                        _ => (BackgroundKind::Other, task.description),
                    };
                    (task.id, BackgroundItem { kind, label: short_label(&label), started_at })
                }).collect();
            }
            Report::SubagentStart { id, description, kind, model } => {
                if !self.background.iter().any(|(known, _)| *known == id) {
                    let label = short_label(description.as_deref().unwrap_or("subagent"));
                    self.background.push((id.clone(), BackgroundItem { kind: BackgroundKind::Agent, label, started_at: now }));
                }
                self.work.agent_started(id, description, kind, model, now);
            }
            Report::SubagentEnd { id } => {
                self.background.retain(|(known, _)| *known != id);
                self.work.agent_finished(&id, RunOutcome::Finished, now);
            }
            Report::WorkflowStart { id, name, transcript_dir } => {
                if !self.background.iter().any(|(known, _)| *known == id) {
                    self.background.push((id.clone(), BackgroundItem { kind: BackgroundKind::Other, label: short_label(&name), started_at: now }));
                }
                self.work.workflow_started(id, name, transcript_dir, now);
            }
            Report::StepCreate(step) => self.work.step_created(step),
            Report::StepUpdate(step) => self.work.step_updated(step),
            Report::Steps { steps } => self.work.steps_listed(steps),
            Report::ShellStart { id, command } => {
                if !self.background.iter().any(|(known, _)| *known == id) {
                    self.background.push((id, BackgroundItem { kind: BackgroundKind::Shell, label: short_label(&command), started_at: now }));
                }
            }
            Report::ToolEnd { tool_use_id } => {
                if self.tool.as_ref().is_some_and(|tool| tool.tool_use_id == tool_use_id) {
                    self.tool = None;
                }
            }
        }
    }
}

pub(crate) struct LiveChanges(tokio::sync::watch::Sender<()>);

impl Default for LiveChanges {
    fn default() -> Self { Self(tokio::sync::watch::Sender::new(())) }
}

impl LiveChanges {
    pub(crate) fn publish(&self) { self.0.send_replace(()); }
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<()> { self.0.subscribe() }
}

#[derive(Default)]
pub(crate) struct Reports {
    sessions: Mutex<HashMap<String, Reported>>,
    launches: Mutex<HashMap<String, String>>,
    connections: std::sync::atomic::AtomicU64,
    started: tokio::sync::Notify,
    pub(crate) changed: LiveChanges,
}

impl Reports {
    pub(crate) fn apply(&self, envelope: Envelope, now: Millis) {
        self.apply_on(envelope, now);
        self.changed.publish();
    }

    fn apply_on(&self, Envelope { session_id, report }: Envelope, now: Millis) {
        let id = session_id.clone();
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        let session = sessions.entry(session_id).or_default();
        session.last_event_at = Some(now);
        if let Report::SessionStart { version, cwd, launch_key } = report {
            if let Some(key) = launch_key {
                self.launches.lock().expect("launch keys poisoned").insert(key, id);
            }
            session.version = Some(version);
            session.cwd = cwd;
            session.started_at = Some(now);
            session.ended = false;
            drop(sessions);
            self.started.notify_waiters();
            return;
        }
        session.record(report, now);
    }

    /// What a node's Agent SDK host observed, as the SDK gave it. It never makes the node
    /// connected: only a mod's connection does.
    pub(crate) fn apply_sdk(&self, session_id: &str, message: &Value, now: Millis) {
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        let session = sessions.entry(session_id.to_owned()).or_default();
        session.last_event_at = Some(now);
        crate::sdk_live::observe(session, message, now);
        drop(sessions);
        self.changed.publish();
    }

    /// The node's Agent SDK host is gone, and with it everything it ran.
    pub(crate) fn sdk_ended(&self, session_id: &str, now: Millis) {
        if let Some(session) = self.sessions.lock().expect("node reports poisoned").get_mut(session_id) {
            crate::sdk_live::ended(session, now);
        }
        self.changed.publish();
    }

    pub(crate) fn connect(&self, hello: Hello, now: Millis) -> u64 {
        let number = self.connections.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        if let Some(key) = hello.launch_key {
            self.launches.lock().expect("launch keys poisoned").insert(key, hello.session_id.clone());
        }
        let session = sessions.entry(hello.session_id).or_default();
        session.connection = Some(number);
        session.disconnected_at = None;
        session.ended = false;
        session.working_since = hello.working.then_some(now);
        session.last_event_at = Some(now);
        session.version = Some(hello.version);
        session.cwd = hello.cwd.or(session.cwd.take());
        session.started_at.get_or_insert(now);
        drop(sessions);
        self.started.notify_waiters();
        self.changed.publish();
        number
    }

    /// Only the connection the session holds now can lose it: one it replaced closing later
    /// changes nothing. True when this was the current connection.
    pub(crate) fn disconnect(&self, session_id: &str, number: u64) -> bool {
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        match sessions.get_mut(session_id) {
            Some(session) if session.connection == Some(number) => {
                session.connection = None;
                session.disconnected_at = Some(alondra_core::now_millis());
                self.changed.publish();
                true
            }
            _ => false,
        }
    }

    pub(crate) fn set_peer(&self, session_id: &str, pid: u32) {
        self.sessions.lock().expect("node reports poisoned").entry(session_id.to_owned()).or_default().peer_pid = Some(pid);
    }

    /// One look at the session process's cumulative CPU time.
    pub(crate) fn sample_cpu(&self, session_id: &str, cpu_cs: u64, now: Millis) {
        if let Some(session) = self.sessions.lock().expect("node reports poisoned").get_mut(session_id) {
            let busy = session.cpu_cs.is_none_or(|before| cpu_cs < before || cpu_cs - before >= alondra_core::states::IDLE_CPU_CS_PER_LOOK);
            if busy || session.cpu_busy_at.is_none() {
                session.cpu_busy_at = Some(now);
            }
            session.cpu_cs = Some(cpu_cs);
        }
    }

    pub(crate) fn set_job(&self, session_id: &str, job: Option<crate::job_state::Job>, dir: Option<PathBuf>) {
        let changed = match self.sessions.lock().expect("node reports poisoned").get_mut(session_id) {
            Some(session) => {
                let changed = session.job != job;
                session.job = job;
                session.job_dir = dir;
                changed
            }
            None => false,
        };
        if changed { self.changed.publish(); }
    }

    pub(crate) fn set_shell_alive(&self, session_id: &str, alive: Option<bool>, now: Millis) {
        let changed = match self.sessions.lock().expect("node reports poisoned").get_mut(session_id) {
            Some(session) => {
                let changed = session.shell_alive != alive;
                session.shell_alive = alive;
                session.shell_looked_at = Some(now);
                changed
            }
            None => false,
        };
        if changed { self.changed.publish(); }
    }

    pub(crate) fn connected(&self, session_id: &str) -> bool {
        self.sessions.lock().expect("node reports poisoned").get(session_id).is_some_and(|session| session.connection.is_some())
    }

    /// What a session whose mod loaded reports first, for tests that launch through a fake Claude.
    #[cfg(test)]
    pub(crate) fn loaded_in(&self, session_id: &str) {
        self.loaded_for(session_id, None);
    }

    /// A fake Claude's launch whose mod loads: the session reports the key of the settings file
    /// the launch passed with `--settings`, as the real mod reports `ALONDRA_DOMAIN_SETTINGS`.
    #[cfg(test)]
    pub(crate) fn loaded_by(&self, session_id: &str, launch_args: &[String]) {
        let settings = launch_args.windows(2).find(|pair| pair[0] == "--settings").expect("a launch passes --settings");
        let key = std::path::Path::new(&settings[1]).file_stem().and_then(|stem| stem.to_str()).expect("a settings file name");
        self.loaded_for(session_id, Some(key));
    }

    #[cfg(test)]
    pub(crate) fn loaded_for(&self, session_id: &str, launch_key: Option<&str>) {
        let report = Report::SessionStart { version: "2.1.287".into(), cwd: None, launch_key: launch_key.map(str::to_owned) };
        self.apply(Envelope { session_id: session_id.into(), report }, alondra_core::now_millis());
    }

    /// The session started with a launch key, which only Alondra's own launches carry.
    pub(crate) fn started_with_a_launch_key(&self, session_id: &str) -> bool {
        self.launches.lock().expect("launch keys poisoned").values().any(|session| session == session_id)
    }

    pub(crate) fn session(&self, session_id: &str) -> Option<Reported> {
        self.sessions.lock().expect("node reports poisoned").get(session_id).cloned()
    }

    /// Whether the session's mod said it loaded: only Alondra's launches carry the node mod.
    pub(crate) fn is_node(&self, session_id: &str) -> bool {
        self.sessions.lock().expect("node reports poisoned").get(session_id).is_some_and(|session| session.started_at.is_some())
    }

    /// Whether the session holds a connection within `deadline`.
    pub(crate) async fn wait_connected(&self, session_id: &str, deadline: Duration) -> bool {
        tokio::time::timeout(deadline, async {
            loop {
                let started = self.started.notified();
                tokio::pin!(started);
                started.as_mut().enable();
                if self.connected(session_id) {
                    return;
                }
                started.await;
            }
        })
        .await
        .is_ok()
    }
}

pub(crate) fn local_day(millis: Millis) -> String {
    let t = millis.div_euclid(1000) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:04}-{:02}-{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn report(value: Value) -> Envelope {
        serde_json::from_value(value).unwrap()
    }

    fn hello(session: &str) -> Hello {
        serde_json::from_value(json!({"session_id": session, "version": "2.1.287", "cwd": "/work"})).unwrap()
    }

    #[test]
    fn a_newer_connection_replaces_the_older_one_and_only_the_current_one_closing_counts() {
        let reports = Reports::default();
        let first = reports.connect(hello("s"), 1);
        let second = reports.connect(hello("s"), 2);
        assert_ne!(first, second);
        reports.disconnect("s", first);
        assert_eq!(reports.session("s").unwrap().connection, Some(second), "the replaced connection closing late changes nothing");
        assert!(!reports.session("s").unwrap().connection_lost());
        reports.disconnect("s", second);
        assert!(reports.session("s").unwrap().connection_lost(), "the current connection closed: the node is unreachable");
        reports.connect(hello("s"), 3);
        assert!(!reports.session("s").unwrap().connection_lost());
        reports.apply(report(json!({"session_id": "s", "event": "session.end"})), 4);
        assert!(!reports.session("s").unwrap().connection_lost(), "an ended session is not unreachable, it is gone");
    }

    #[test]
    fn turns_and_tools_become_what_the_node_does_and_said_last() {
        let reports = Reports::default();
        reports.apply(report(json!({"session_id": "s", "event": "session.start", "version": "2.1.287", "cwd": "/w"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 2);
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "Bash", "tool_use_id": "u1",
            "input": {"command": "cargo test"}})), 3);
        let session = reports.session("s").unwrap();
        assert_eq!((session.version.as_deref(), session.working_since), (Some("2.1.287"), Some(2)));
        let tool = session.tool.unwrap();
        assert_eq!((tool.name.as_str(), tool.summary.as_deref(), tool.since), ("Bash", Some("cargo test"), 3));
        reports.apply(report(json!({"session_id": "s", "event": "tool.end", "tool_use_id": "other"})), 4);
        assert!(reports.session("s").unwrap().tool.is_some(), "another call ending leaves this one running");
        reports.apply(report(json!({"session_id": "s", "event": "tool.end", "tool_use_id": "u1"})), 5);
        assert!(reports.session("s").unwrap().tool.is_none());
        reports.apply(report(json!({"session_id": "s", "event": "turn.complete", "turn_id": "t", "answer": "Stopped.", "aborted": true})), 6);
        let session = reports.session("s").unwrap();
        assert_eq!(session.answer, Some(Answer { text: "Stopped.".into(), at: 6, aborted: true }));
        assert_eq!(session.working_since, None);
    }

    #[test]
    fn the_model_a_session_reports_is_the_one_it_keeps() {
        let reports = Reports::default();
        reports.apply_on(report(json!({"session_id": "s", "event": "model", "model": "claude-opus-5-5"})), 1);
        reports.apply_on(report(json!({"session_id": "s", "event": "model", "model": "claude-sonnet-5-5"})), 2);
        assert_eq!(reports.session("s").unwrap().model.as_deref(), Some("claude-sonnet-5-5"));
    }

    #[test]
    fn subagents_workflows_and_the_task_list_reported_by_the_mod_become_the_nodes_work() {
        use crate::node_work::StepStatus;
        let reports = Reports::default();
        let apply = |value: Value, at| reports.apply_on(report(value), at);
        apply(json!({"session_id": "s", "event": "model", "model": "claude-opus-5-5"}), 1);
        apply(json!({"session_id": "s", "event": "subagent.start", "id": "a1", "description": "Fix the parser",
            "type": "general-purpose", "model": "claude-sonnet-5"}), 2);
        apply(json!({"session_id": "s", "event": "model", "model": "claude-haiku-5", "agent_id": "a1"}), 3);
        apply(json!({"session_id": "s", "event": "workflow.start", "id": "w1", "name": "review-loop", "transcript_dir": "/tmp/w1"}), 4);
        apply(json!({"session_id": "s", "event": "step.create", "id": "1", "subject": "Read", "active_form": "Reading"}), 5);
        apply(json!({"session_id": "s", "event": "step.create", "id": "2", "subject": "Fix"}), 6);
        apply(json!({"session_id": "s", "event": "step.update", "id": "1", "status": "in_progress"}), 7);
        apply(json!({"session_id": "s", "event": "step.update", "id": "2", "status": "blocked_on_review"}), 8);
        let session = reports.session("s").unwrap();
        assert_eq!(session.model.as_deref(), Some("claude-opus-5-5"), "a subagent's model is not the main thread's");
        assert_eq!(session.work.agents[0].model.as_deref(), Some("claude-haiku-5"));
        assert_eq!(session.work.agents[0].kind.as_deref(), Some("general-purpose"));
        assert_eq!(session.work.workflows[0].name, "review-loop");
        assert_eq!(session.work.steps.iter().map(|step| step.status).collect::<Vec<_>>(), [StepStatus::InProgress, StepStatus::Pending]);
        assert_eq!(session.background.len(), 2, "a running workflow keeps the node busy like a subagent");

        apply(json!({"session_id": "s", "event": "subagent.end", "id": "a1"}), 9);
        apply(json!({"session_id": "s", "event": "steps", "steps": [
            {"id": "1", "subject": "Read", "status": "completed"}, {"id": "2", "subject": "Fix", "status": "completed"}]}), 10);
        apply(json!({"session_id": "s", "event": "background", "tasks": [
            {"id": "w1", "type": "workflow", "description": "Review", "name": "review-loop"}]}), 11);
        let session = reports.session("s").unwrap();
        assert_eq!(session.work.agents[0].end.map(|end| end.at), Some(9));
        assert_eq!(session.work.workflows[0].end.map(|end| end.at), None);
        assert_eq!(session.background[0].1.label, "review-loop");
        assert!(session.work.steps.iter().all(|step| step.status == StepStatus::Completed));

        apply(json!({"session_id": "s", "event": "background", "tasks": []}), 12);
        assert_eq!(reports.session("s").unwrap().work.workflows[0].end.map(|end| end.at), Some(12));
    }

    #[test]
    fn a_session_whose_mod_reports_a_launch_key_at_start_or_at_reconnection_is_alondras() {
        let reports = Reports::default();
        reports.apply(report(json!({"session_id": "other", "event": "session.start", "version": "2.1.287"})), 1);
        reports.apply(report(json!({"session_id": "real-session-id", "event": "session.start", "version": "2.1.287", "launch_key": "launch-1"})), 2);
        assert!(reports.started_with_a_launch_key("real-session-id") && !reports.started_with_a_launch_key("other"));
        let mut reconnected = hello("after-a-restart");
        reconnected.launch_key = Some("launch-3".into());
        reports.connect(reconnected, 3);
        assert!(reports.started_with_a_launch_key("after-a-restart"), "a connection carries the key too");
        assert!(reports.is_node("after-a-restart") && !reports.is_node("stranger"));
    }

    #[test]
    fn a_reconnection_says_whether_the_session_is_inside_a_turn() {
        let reports = Reports::default();
        let mut busy = hello("s");
        busy.working = true;
        reports.connect(busy, 5);
        assert_eq!(reports.session("s").unwrap().working_since, Some(5), "a daemon that restarted mid-turn learns it from the reconnection");
        reports.apply(report(json!({"session_id": "s", "event": "turn.complete", "turn_id": "t", "answer": "Done."})), 6);
        reports.connect(hello("s"), 7);
        assert_eq!(reports.session("s").unwrap().working_since, None);
    }

    #[test]
    fn a_turn_that_goes_quiet_is_stalled_unless_a_call_runs_or_anything_is_reported() {
        let quiet = alondra_core::states::TURN_QUIET_MS;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        let activity = |at| reports.session("s").unwrap().activity(at);
        assert_eq!(activity(quiet), Activity::Working, "quiet for less than the limit");
        assert_eq!(activity(quiet + 2), Activity::Stalled, "inside a turn and silent past the limit");
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "Bash", "tool_use_id": "u"})), quiet + 3);
        assert_eq!(activity(quiet * 2), Activity::Working, "a call in flight is governed by its own limit");
        reports.apply(report(json!({"session_id": "s", "event": "tool.end", "tool_use_id": "u"})), quiet * 2);
        assert_eq!(activity(quiet * 2 + 1), Activity::Working, "the last report is recent");
        reports.apply(report(json!({"session_id": "s", "event": "turn.complete", "turn_id": "t", "answer": "Done."})), quiet * 2 + 2);
        assert_eq!(activity(quiet * 5), Activity::Idle, "between turns silence is idleness");
    }

    #[test]
    fn a_session_without_a_connection_is_not_silent_and_a_new_connection_restarts_the_clock() {
        let quiet = alondra_core::states::TURN_QUIET_MS;
        let reports = Reports::default();
        let first = reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        assert!(reports.session("s").unwrap().silent(quiet + 2));
        reports.disconnect("s", first);
        assert!(!reports.session("s").unwrap().silent(quiet + 2), "unreachable is the revival's business, not a silent turn");
        let mut busy = hello("s");
        busy.working = true;
        reports.connect(busy, quiet + 3);
        assert!(!reports.session("s").unwrap().silent(quiet + 4), "a fresh connection is activity");
        reports.connect(hello("s"), quiet + 5);
        assert_eq!(reports.session("s").unwrap().working_since, None, "a restarted session is not mid-turn");
    }

    const HOUR: Millis = 60 * 60 * 1000;

    #[test]
    fn a_foreground_build_that_stays_silent_for_an_hour_is_working_while_its_shell_lives_and_stalled_when_it_is_gone() {
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "Bash", "tool_use_id": "u", "input": {"command": "bazel test //..."}})), 2);
        reports.set_shell_alive("s", Some(true), 3);
        let activity = |at| reports.session("s").unwrap().activity(at);
        assert_eq!(activity(HOUR), Activity::Working, "past the call limit, but its shell is alive");
        assert!(!reports.session("s").unwrap().silent(HOUR));
        reports.set_shell_alive("s", Some(false), 3);
        assert_eq!(activity(HOUR), Activity::Stalled, "the call never ends: its shell is gone");
        assert!(reports.session("s").unwrap().silent(HOUR), "and Alondra restarts it");
        reports.set_shell_alive("s", None, 3);
        assert_eq!(activity(HOUR), Activity::Stalled, "nothing known: the call limit decides, as before");
    }

    #[test]
    fn a_live_background_shell_with_an_idle_turn_is_working_and_a_dead_one_with_no_turn_is_stalled() {
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "shell.start", "id": "b", "command": "musts run bazel-test-root"})), 1);
        let session = || reports.session("s").unwrap();
        reports.set_shell_alive("s", Some(true), 3);
        assert_eq!(session().activity(2 * HOUR), Activity::Working, "an hour-long wait on a live shell is work");
        assert!(session().background.iter().all(|(_, item)| session().shell_end(item).is_none()), "a live shell runs");
        assert!(!session().silent(2 * HOUR));
        reports.set_shell_alive("s", Some(false), 3);
        assert_eq!(session().activity(2 * HOUR), Activity::Stalled, "its shell is gone, no turn and nothing reported since");
        assert!(session().silent(2 * HOUR));
        assert!(session().background.iter().all(|(_, item)| session().shell_end(item) == Some(End { at: 3, outcome: RunOutcome::Finished })),
            "a look after it started found its shell gone: it ended then");
        assert_eq!(session().activity(alondra_core::states::TURN_QUIET_MS), Activity::Idle, "nothing runs, yet it is not quiet long enough to be stalled");
    }

    #[test]
    fn a_running_subagent_keeps_a_quiet_turn_working() {
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "subagent.start", "id": "a", "description": "review"})), 2);
        let quiet = alondra_core::states::TURN_QUIET_MS;
        assert_eq!(reports.session("s").unwrap().activity(quiet * 3), Activity::Working);
        reports.apply(report(json!({"session_id": "s", "event": "subagent.end", "id": "a"})), 3);
        assert_eq!(reports.session("s").unwrap().activity(quiet * 3), Activity::Stalled);
    }

    #[test]
    fn a_quiet_turn_is_working_while_the_session_uses_cpu_and_stalled_once_it_is_flat_for_the_whole_window() {
        let quiet = alondra_core::states::TURN_QUIET_MS;
        let step = alondra_core::states::IDLE_CPU_CS_PER_LOOK;
        let look = 30_000;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        let session = || reports.session("s").unwrap();
        assert!(!session().silent(quiet + 2), "no look at the process yet: idleness is not proven");
        let mut cpu = 0;
        let mut at = 0;
        while at < quiet + 5 * look {
            cpu += step;
            reports.sample_cpu("s", cpu, at);
            at += look;
        }
        assert_eq!(session().activity(at), Activity::Working, "the model is generating: the CPU keeps advancing");
        assert!(!session().silent(at));
        let busy_until = at;
        while at < busy_until + quiet + 2 * look {
            cpu += step - 1;
            reports.sample_cpu("s", cpu, at);
            at += look;
        }
        assert_eq!(session().activity(at), Activity::Stalled, "flat CPU across the whole window");
        assert!(session().silent(at));
        reports.sample_cpu("s", cpu + step, at);
        assert!(!session().silent(at + 1), "it moved again");
    }

    #[test]
    fn a_dead_shell_wait_is_not_revived_while_the_session_still_uses_cpu() {
        let quiet = alondra_core::states::TURN_QUIET_MS;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.apply(report(json!({"session_id": "s", "event": "shell.start", "id": "b", "command": "make"})), 1);
        reports.set_shell_alive("s", Some(false), 3);
        reports.sample_cpu("s", 0, 0);
        reports.sample_cpu("s", 10_000, 2 * quiet);
        assert!(!reports.session("s").unwrap().silent(2 * quiet + 1));
        reports.sample_cpu("s", 10_001, 4 * quiet);
        assert!(reports.session("s").unwrap().silent(4 * quiet));
    }

    fn job(phase: crate::job_state::Phase, tasks: u64, updated_at: Millis) -> crate::job_state::Job {
        crate::job_state::Job { phase, detail: "exit 1 before init".into(), tasks, updated_at, settings: None, worktree: None }
    }

    #[test]
    fn a_failed_job_is_a_failed_node_that_is_never_restarted_for_being_quiet() {
        use crate::job_state::Phase;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.set_job("s", Some(job(Phase::Failed, 0, 0)), None);
        reports.sample_cpu("s", 0, 0);
        let session = reports.session("s").unwrap();
        assert_eq!(session.activity(HOUR), Activity::Failed);
        assert!(!session.silent(HOUR));
    }

    #[test]
    fn a_working_job_is_working_with_tasks_in_flight_or_a_state_that_moves_and_stalled_when_both_stop_and_the_cpu_is_flat() {
        use crate::job_state::Phase;
        let window = alondra_core::states::JOB_STALL_MS;
        let look = 30_000;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 0, 0);
        let activity = |at| reports.session("s").unwrap().activity(at);
        reports.set_job("s", Some(job(Phase::Working, 1, 0)), None);
        assert_eq!(activity(HOUR), Activity::Working, "a build in flight, however long it is silent");
        reports.set_job("s", Some(job(Phase::Working, 0, 0)), None);
        assert_eq!(activity(window), Activity::Working, "the state moved less than the window ago");
        assert_eq!(activity(window + 1), Activity::Stalled, "nothing in flight, the state still, the CPU flat since the first look");
        reports.sample_cpu("s", 10_000, window + 2 * look);
        assert_eq!(activity(window + 2 * look + 1), Activity::Working, "the CPU moved: a long reply being generated");
        assert!(!reports.session("s").unwrap().silent(window + 2 * look + 1));
    }

    #[test]
    fn a_foreground_build_or_a_deploy_wait_with_a_still_job_and_flat_cpu_is_working_while_its_shell_lives() {
        use crate::job_state::Phase;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 0, 0);
        reports.set_job("s", Some(job(Phase::Working, 0, 0)), None);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "Bash", "tool_use_id": "u", "input": {"command": "until curl -sf https://x/health; do sleep 30; done"}})), 2);
        reports.set_shell_alive("s", Some(true), 3);
        let session = || reports.session("s").unwrap();
        assert_eq!(session().activity(HOUR), Activity::Working, "the shell is alive however still the job and the CPU are");
        assert!(!session().silent(HOUR));
        reports.set_shell_alive("s", Some(false), 3);
        assert_eq!(session().activity(HOUR), Activity::Stalled, "the shell it waits on is gone");
    }

    #[test]
    fn a_long_tool_call_or_a_subagent_with_a_still_job_is_not_stalled_after_the_job_window() {
        use crate::job_state::Phase;
        let window = alondra_core::states::JOB_STALL_MS;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 0, 0);
        reports.set_job("s", Some(job(Phase::Working, 0, 0)), None);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "mcp__x__deploy", "tool_use_id": "u", "input": {}})), 2);
        assert_eq!(reports.session("s").unwrap().activity(window * 3), Activity::Working, "a tool call in flight");
        reports.apply(report(json!({"session_id": "s", "event": "tool.end", "tool": "mcp__x__deploy", "tool_use_id": "u"})), 3);
        reports.apply(report(json!({"session_id": "s", "event": "subagent.start", "id": "a", "description": "review"})), 4);
        assert_eq!(reports.session("s").unwrap().activity(window * 3), Activity::Working, "a subagent in flight");
    }

    #[test]
    fn a_reply_generated_for_five_minutes_stays_working_and_three_flat_minutes_stall() {
        use crate::job_state::Phase;
        let window = alondra_core::states::JOB_STALL_MS;
        let step = alondra_core::states::IDLE_CPU_CS_PER_LOOK;
        let look = 30_000;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.set_job("s", Some(job(Phase::Working, 0, 0)), None);
        let (mut at, mut cpu) = (0, 0);
        while at <= 5 * 60_000 {
            cpu += step;
            reports.sample_cpu("s", cpu, at);
            assert_eq!(reports.session("s").unwrap().activity(at), Activity::Working, "generating at {at}");
            at += look;
        }
        let flat_since = at - look;
        while at <= flat_since + window {
            cpu += step - 1;
            reports.sample_cpu("s", cpu, at);
            assert_eq!(reports.session("s").unwrap().activity(at), Activity::Working, "flat for less than the window at {at}");
            at += look;
        }
        cpu += step - 1;
        reports.sample_cpu("s", cpu, at);
        assert_eq!(reports.session("s").unwrap().activity(at), Activity::Stalled, "flat for the whole window");
        assert!(reports.session("s").unwrap().silent(at));
    }

    #[test]
    fn a_working_job_without_a_look_at_the_process_is_not_stalled_and_an_idle_one_leaves_the_decision_to_the_mod() {
        use crate::job_state::Phase;
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.set_job("s", Some(job(Phase::Working, 0, 0)), None);
        assert_eq!(reports.session("s").unwrap().activity(HOUR), Activity::Working, "no CPU look: idleness is not proven");
        reports.set_job("s", Some(job(Phase::Blocked, 0, 0)), None);
        assert_eq!(reports.session("s").unwrap().activity(HOUR), Activity::Idle, "the mod reports no turn");
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        assert_eq!(reports.session("s").unwrap().activity(HOUR), Activity::Working, "the mod's events decide, as without a job state");
        reports.set_job("s", None, None);
        assert_eq!(reports.session("s").unwrap().activity(HOUR), Activity::Working);
    }

    /// David's rule, 15 October: whether a node's own work is over, and which of it is flagged,
    /// follows from what the mod reported and what the looks at its processes saw, in their order,
    /// never from how long any of it has run. The same 256 generated histories on a clock that
    /// moves a millisecond a fact and on one that moves up to two days give the same answers.
    #[test]
    fn the_same_reports_and_looks_on_any_clock_say_the_same_of_a_node_s_own_work() {
        let mut seed: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = move |below: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % below
        };
        let ids = ["b1", "b2", "sub", "w1", "m1"];
        let kind = |id: &str| match id {
            "b1" | "b2" => ("shell", "command"),
            "sub" => ("subagent", "description"),
            "w1" => ("workflow", "name"),
            _ => ("monitor", "description"),
        };
        for run in 0..256 {
            let facts: Vec<Value> = (0..1 + next(40)).map(|_| {
                let id = ids[next(ids.len() as u64) as usize];
                match next(8) {
                    0 => {
                        let listed: Vec<Value> = ids.iter().filter(|_| next(2) == 0).map(|id| {
                            let (kind, field) = kind(id);
                            json!({"id": id, "type": kind, field: format!("{id} work")})
                        }).collect();
                        json!({"event": "background", "tasks": listed})
                    }
                    1 => json!({"event": "shell.start", "id": id, "command": "sleep 99999"}),
                    2 => json!({"event": "subagent.start", "id": id, "description": "explore"}),
                    3 => json!({"event": "subagent.end", "id": id}),
                    4 => json!({"event": "workflow.start", "id": id, "name": "review"}),
                    5 => json!({"event": "session.end"}),
                    _ => {
                        let alive = [Some(true), Some(false), None][next(3) as usize];
                        json!({"look": alive})
                    }
                }
            }).collect();
            let gaps: Vec<Millis> = (0..facts.len()).map(|_| 1 + next(48 * HOUR as u64) as Millis).collect();
            let (fast, slow) = (Reports::default(), Reports::default());
            let (mut fast_now, mut slow_now) = (0, 0);
            fast.connect(hello("s"), fast_now);
            slow.connect(hello("s"), slow_now);
            for (fact, gap) in facts.iter().zip(&gaps) {
                fast_now += 1;
                slow_now += gap;
                for (reports, now) in [(&fast, fast_now), (&slow, slow_now)] {
                    match fact.get("look") {
                        Some(alive) => reports.set_shell_alive("s", alive.as_bool(), now),
                        None => {
                            let mut fact = fact.clone();
                            fact["session_id"] = json!("s");
                            reports.apply(report(fact), now);
                        }
                    }
                }
                let said = |reports: &Reports| {
                    let session = reports.session("s").unwrap();
                    let items: Vec<(String, BackgroundKind, Option<RunOutcome>)> = session.background.iter()
                        .map(|(id, item)| (id.clone(), item.kind, session.shell_end(item).map(|end| end.outcome))).collect();
                    let ended: Vec<(String, RunOutcome)> = session.work.ended_tasks.iter().map(|task| (task.item.label.clone(), task.end.outcome)).collect();
                    (session.work_gone(), items, ended)
                };
                assert_eq!(said(&fast), said(&slow), "run {run}, after {fact}");
            }
        }
    }
}
