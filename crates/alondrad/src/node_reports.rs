//! What each session's node mod reports (`plugin/hooks/register.ts`): that it loaded, its turns,
//! the tool it runs, how full its context is, what it has cost, and the plan's limits. Kept in
//! memory beside the hooks' live view; the domain never records it. The open connection the mod
//! holds (`alondra node connect`) is the node's liveness: a newer connection from the same
//! session replaces the older one, and only the current one closing makes the node unreachable.
//! A session binds to its launch by the key of the settings file the launch gave it
//! (`ALONDRA_DOMAIN_SETTINGS`), which its mod reports in `session.start`.
use alondra_core::usage::{RateLimit, Usage};
use alondra_core::{BackgroundItem, BackgroundKind, Millis, ToolInFlight};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// How long a launch waits for the mod's `session.start` before it counts the mods as not loaded.
pub(crate) const MODS_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Deserialize, Debug, PartialEq)]
#[serde(tag = "event")]
pub(crate) enum Report {
    #[serde(rename = "session.start")]
    SessionStart { version: String, #[serde(default)] cwd: Option<String>, #[serde(default)] launch_key: Option<String> },
    #[serde(rename = "session.end")]
    SessionEnd,
    #[serde(rename = "session.measure")]
    Measure {
        #[serde(default)]
        context: Option<ContextIn>,
        #[serde(default)]
        rate_limits: Vec<RateLimitIn>,
        #[serde(default)]
        cost_usd: Option<f64>,
    },
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
    id: String,
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    agent_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Deserialize, Debug, PartialEq)]
pub(crate) struct ContextIn {
    #[serde(default)]
    tokens: Option<u64>,
    window: u64,
    #[serde(default)]
    percent: Option<f64>,
}

#[derive(Deserialize, Debug, PartialEq)]
pub(crate) struct RateLimitIn {
    kind: String,
    percent_used: f64,
    #[serde(default)]
    resets_at: Option<String>,
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
pub(crate) struct ContextReading {
    pub percent: Option<f64>,
    pub tokens: Option<u64>,
    pub window: u64,
}

impl ContextReading {
    pub(crate) fn full_percent(&self) -> Option<f64> {
        self.percent.or_else(|| self.tokens.filter(|_| self.window > 0).map(|tokens| tokens as f64 * 100.0 / self.window as f64))
    }
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
    pub context: Option<ContextReading>,
    /// The fullest the context has been while the daemon watched, in percent.
    pub context_peak: Option<f64>,
    pub cost_usd: Option<f64>,
    /// The model id the main thread last sent a request with.
    pub model: Option<String>,
    /// The subagents and background shells the node has running, by the id Claude Code gives them.
    pub background: Vec<(String, BackgroundItem)>,
    pub work: crate::node_work::NodeWork,
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
        let call_overran = self.tool.as_ref().is_some_and(|tool| now - tool.since > alondra_core::states::call_limit(&tool.name));
        if (call_overran && self.shell_alive != Some(true)) || self.silent(now) {
            Activity::Stalled
        } else if self.working_since.is_some() || self.tool.is_some() || self.background_at(now).iter().any(|item| !item.suspect) {
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

    /// What the node has running in the background, each flagged when it has run long while the
    /// node shows no other work, as it may be hung.
    pub(crate) fn background_at(&self, now: Millis) -> Vec<BackgroundItem> {
        let idle = self.working_since.is_none() && self.tool.is_none();
        self.background.iter()
            .map(|(_, item)| BackgroundItem { suspect: idle && !self.shell_running() && now - item.started_at > alondra_core::states::BACKGROUND_SUSPECT_MS, ..item.clone() })
            .collect()
    }

    /// Everything the node has in the background may never end, so it is no reason to wait for
    /// the node: a subagent past `SUBAGENT_CALL_LIMIT_MS`, anything else past
    /// `BACKGROUND_SUSPECT_MS` with no shell call alive.
    pub(crate) fn background_hung(&self, now: Millis) -> bool {
        !self.background.is_empty() && self.background.iter().all(|(_, item)| match item.kind {
            BackgroundKind::Agent => now - item.started_at > alondra_core::states::SUBAGENT_CALL_LIMIT_MS,
            _ => !self.shell_running() && now - item.started_at > alondra_core::states::BACKGROUND_SUSPECT_MS,
        })
    }

    /// The mod loaded and its session has not ended, yet its connection is closed: what brings
    /// the node back (`node_revival`), never something Fleet shows as the node's state.
    pub(crate) fn connection_lost(&self) -> bool {
        self.started_at.is_some() && !self.ended && self.connection.is_none()
    }
}

pub(crate) const USAGE_FILE: &str = "usage.json";

#[derive(Default, Serialize, Deserialize)]
struct Limits {
    windows: Vec<RateLimit>,
    measured_at: Millis,
    /// The local day, the week's reset and the week's percent at that day's first measurement.
    day_start: Option<(String, Millis, f64)>,
}

#[derive(Default)]
pub(crate) struct Reports {
    sessions: Mutex<HashMap<String, Reported>>,
    launches: Mutex<HashMap<String, String>>,
    limits: Mutex<Limits>,
    /// Where the plan's limits are kept so a restart still knows the last reading and the day's start.
    saved_in: Option<PathBuf>,
    connections: std::sync::atomic::AtomicU64,
    started: tokio::sync::Notify,
}

impl Reports {
    /// Reports that keep the plan's limits in `home`'s `usage.json` and start from what it holds.
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(USAGE_FILE);
        let limits = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { limits: Mutex::new(limits), saved_in: Some(file), ..Self::default() }
    }

    fn save(&self, limits: &Limits) {
        let Some(file) = &self.saved_in else { return };
        let next = file.with_extension("next");
        let written = serde_json::to_vec(limits).map_err(std::io::Error::from)
            .and_then(|bytes| std::fs::write(&next, bytes))
            .and_then(|()| std::fs::rename(&next, file));
        if let Err(error) = written {
            log!("could not save {}: {error}", file.display());
        }
    }

    pub(crate) fn apply(&self, envelope: Envelope, now: Millis) {
        self.apply_on(envelope, now, &local_day(now));
    }

    fn apply_on(&self, Envelope { session_id, report }: Envelope, now: Millis, today: &str) {
        if let Report::Measure { rate_limits, .. } = &report
            && !rate_limits.is_empty()
        {
            self.measured(rate_limits, now, today);
        }
        let id = session_id.clone();
        let mut sessions = self.sessions.lock().expect("node reports poisoned");
        let session = sessions.entry(session_id).or_default();
        session.last_event_at = Some(now);
        match report {
            Report::SessionStart { version, cwd, launch_key } => {
                if let Some(key) = launch_key {
                    self.launches.lock().expect("launch keys poisoned").insert(key, id);
                }
                session.version = Some(version);
                session.cwd = cwd;
                session.started_at = Some(now);
                session.ended = false;
                drop(sessions);
                self.started.notify_waiters();
            }
            Report::SessionEnd => {
                session.ended = true;
                session.connection = None;
                session.tool = None;
                session.working_since = None;
                session.background.clear();
                session.work.ended(now);
            }
            Report::Measure { context, cost_usd, .. } => {
                if let Some(context) = context {
                    let reading = ContextReading { percent: context.percent, tokens: context.tokens, window: context.window };
                    if let Some(percent) = reading.full_percent() {
                        session.context_peak = Some(session.context_peak.map_or(percent, |peak| peak.max(percent)));
                    }
                    session.context = Some(reading);
                }
                session.cost_usd = cost_usd.or(session.cost_usd);
            }
            Report::Model { model, agent_id: None } => session.model = Some(model),
            Report::Model { model, agent_id: Some(agent) } => session.work.agent_model(&agent, model),
            Report::TurnStart => session.working_since = Some(now),
            Report::TurnComplete { answer, aborted } => {
                session.working_since = None;
                session.tool = None;
                session.answer = Some(Answer { text: answer, at: now, aborted });
            }
            Report::ToolStart { tool, tool_use_id, input } => {
                let summary = crate::http::tool_summary(Some(&tool), Some(&input));
                session.tool = Some(ToolInFlight { tool_use_id, name: tool, summary, since: now });
            }
            Report::Background { tasks } => {
                let listed: Vec<_> = tasks.iter().map(|task| crate::node_work::Listed {
                    id: &task.id,
                    kind: &task.kind,
                    description: &task.description,
                    agent_type: task.agent_type.as_deref(),
                    name: task.name.as_deref(),
                }).collect();
                session.work.listed(&listed, now);
                let known = std::mem::take(&mut session.background);
                session.background = tasks.into_iter().map(|task| {
                    let started_at = known.iter().find(|(id, _)| *id == task.id).map_or(now, |(_, item)| item.started_at);
                    let (kind, label) = match task.kind.as_str() {
                        "subagent" => (BackgroundKind::Agent, task.description),
                        "shell" => (BackgroundKind::Shell, task.command.unwrap_or(task.description)),
                        "workflow" => (BackgroundKind::Other, task.name.filter(|name| !name.is_empty()).unwrap_or(task.description)),
                        _ => (BackgroundKind::Other, task.description),
                    };
                    (task.id, BackgroundItem { kind, label: short_label(&label), started_at, suspect: false })
                }).collect();
            }
            Report::SubagentStart { id, description, kind, model } => {
                if !session.background.iter().any(|(known, _)| *known == id) {
                    let label = short_label(description.as_deref().unwrap_or("subagent"));
                    session.background.push((id.clone(), BackgroundItem { kind: BackgroundKind::Agent, label, started_at: now, suspect: false }));
                }
                session.work.agent_started(id, description, kind, model, now);
            }
            Report::SubagentEnd { id } => {
                session.background.retain(|(known, _)| *known != id);
                session.work.agent_finished(&id, now);
            }
            Report::WorkflowStart { id, name, transcript_dir } => {
                if !session.background.iter().any(|(known, _)| *known == id) {
                    session.background.push((id.clone(), BackgroundItem { kind: BackgroundKind::Other, label: short_label(&name), started_at: now, suspect: false }));
                }
                session.work.workflow_started(id, name, transcript_dir, now);
            }
            Report::StepCreate(step) => session.work.step_created(step),
            Report::StepUpdate(step) => session.work.step_updated(step),
            Report::Steps { steps } => session.work.steps_listed(steps),
            Report::ShellStart { id, command } => {
                if !session.background.iter().any(|(known, _)| *known == id) {
                    session.background.push((id, BackgroundItem { kind: BackgroundKind::Shell, label: short_label(&command), started_at: now, suspect: false }));
                }
            }
            Report::ToolEnd { tool_use_id } => {
                if session.tool.as_ref().is_some_and(|tool| tool.tool_use_id == tool_use_id) {
                    session.tool = None;
                }
            }
        }
    }

    fn measured(&self, rate_limits: &[RateLimitIn], now: Millis, today: &str) {
        let windows: Vec<RateLimit> = rate_limits.iter().map(|limit| RateLimit {
            kind: limit.kind.clone(),
            percent_used: limit.percent_used,
            resets_at: limit.resets_at.as_deref().and_then(crate::watcher::parse_rfc3339_millis),
        }).collect();
        let mut limits = self.limits.lock().expect("plan limits poisoned");
        if let Some(week) = windows.iter().find(|limit| limit.kind == alondra_core::usage::SEVEN_DAY) {
            let reset = week.resets_at.unwrap_or_default();
            let same_day = limits.day_start.as_ref().is_some_and(|(day, week_reset, _)| day == today && *week_reset == reset);
            if !same_day {
                limits.day_start = Some((today.to_owned(), reset, week.percent_used));
            }
        }
        limits.windows = windows;
        limits.measured_at = now;
        self.save(&limits);
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
        if let Some(session) = self.sessions.lock().expect("node reports poisoned").get_mut(session_id) {
            session.job = job;
            session.job_dir = dir;
        }
    }

    pub(crate) fn set_shell_alive(&self, session_id: &str, alive: Option<bool>) {
        if let Some(session) = self.sessions.lock().expect("node reports poisoned").get_mut(session_id) {
            session.shell_alive = alive;
        }
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

    fn launched(&self, key: &str) -> Option<String> {
        self.launches.lock().expect("launch keys poisoned").get(key).cloned()
    }

    /// The session whose mod reported the launch's key within `deadline`: the id Claude chose
    /// for it, which is how a launch binds to its session.
    pub(crate) async fn wait_launched(&self, key: &str, deadline: Duration) -> Option<String> {
        tokio::time::timeout(deadline, async {
            loop {
                let started = self.started.notified();
                tokio::pin!(started);
                started.as_mut().enable();
                if let Some(session) = self.launched(key) {
                    return session;
                }
                started.await;
            }
        })
        .await
        .ok()
    }

    /// Whether the session holds a connection other than `previous` within `deadline`: what a
    /// restart of a session that still held one has to wait for.
    pub(crate) async fn wait_reconnected(&self, session_id: &str, previous: Option<u64>, deadline: Duration) -> bool {
        tokio::time::timeout(deadline, async {
            loop {
                let started = self.started.notified();
                tokio::pin!(started);
                started.as_mut().enable();
                if self.session(session_id).is_some_and(|session| session.connection.is_some() && session.connection != previous) {
                    return;
                }
                started.await;
            }
        })
        .await
        .is_ok()
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

    /// When a plan window the node mods measured as used up comes back.
    pub(crate) fn exhausted_until(&self, now: Millis) -> Option<Millis> {
        let limits = self.limits.lock().expect("plan limits poisoned");
        limits.windows.iter().filter(|window| window.percent_used >= 100.0)
            .filter_map(|window| window.resets_at.filter(|reset| *reset > now)).max()
    }

    /// When a limit Claude reported without a time ends: the window that ran out, else the
    /// 5-hour window, else the soonest reset the plan has.
    pub(crate) fn reset_after_limit(&self, now: Millis) -> Option<Millis> {
        self.exhausted_until(now).or_else(|| {
            let limits = self.limits.lock().expect("plan limits poisoned");
            let future = |window: &&RateLimit| window.resets_at.is_some_and(|reset| reset > now);
            limits.windows.iter().filter(future).find(|window| window.kind == alondra_core::usage::FIVE_HOUR)
                .or_else(|| limits.windows.iter().filter(future).min_by_key(|window| window.resets_at))
                .and_then(|window| window.resets_at)
        })
    }

    pub(crate) fn usage(&self, now: Millis) -> Option<Usage> {
        let limits = self.limits.lock().expect("plan limits poisoned");
        let start = limits.day_start.as_ref().filter(|(day, _, _)| *day == local_day(now)).map(|(_, _, start)| *start);
        alondra_core::usage::compute(&limits.windows, start, limits.measured_at, now)
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
        reports.apply_on(report(json!({"session_id": "s", "event": "model", "model": "claude-opus-5-5"})), 1, "2026-10-02");
        reports.apply_on(report(json!({"session_id": "s", "event": "model", "model": "claude-sonnet-5-5"})), 2, "2026-10-02");
        assert_eq!(reports.session("s").unwrap().model.as_deref(), Some("claude-sonnet-5-5"));
    }

    #[test]
    fn subagents_workflows_and_the_task_list_reported_by_the_mod_become_the_nodes_work() {
        use crate::node_work::StepStatus;
        let reports = Reports::default();
        let apply = |value: Value, at| reports.apply_on(report(value), at, "2026-10-06");
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
        assert_eq!(session.work.agents[0].finished_at, Some(9));
        assert_eq!(session.work.workflows[0].finished_at, None);
        assert_eq!(session.background[0].1.label, "review-loop");
        assert!(session.work.steps.iter().all(|step| step.status == StepStatus::Completed));

        apply(json!({"session_id": "s", "event": "background", "tasks": []}), 12);
        assert_eq!(reports.session("s").unwrap().work.workflows[0].finished_at, Some(12));
    }

    #[test]
    fn measurements_keep_the_context_the_cost_and_the_plan_limits_with_their_reset() {
        let reports = Reports::default();
        let measure = |week: f64, cost: f64| report(json!({"session_id": "s", "event": "session.measure",
            "context": {"tokens": 54944, "window": 1000000, "percent": 5},
            "rate_limits": [{"kind": "five_hour", "percent_used": 10, "resets_at": "2026-10-02T16:40:00Z"},
                            {"kind": "seven_day", "percent_used": week, "resets_at": "2026-10-05T00:00:00Z"}],
            "cost_usd": cost}));
        let now = 1_790_950_000_000;
        reports.apply_on(measure(20.0, 0.5), now, "2026-10-02");
        reports.apply_on(measure(30.0, 1.25), now + 1, "2026-10-02");
        let session = reports.session("s").unwrap();
        assert_eq!(session.context, Some(ContextReading { percent: Some(5.0), tokens: Some(54944), window: 1_000_000 }));
        assert_eq!(session.cost_usd, Some(1.25));
        let limits = reports.limits.lock().unwrap();
        let usage = alondra_core::usage::compute(&limits.windows, limits.day_start.as_ref().map(|d| d.2), limits.measured_at, now).unwrap();
        assert_eq!((usage.week_percent, usage.today_spent), (30.0, Some(10.0)), "today counts from the day's first measurement");
        assert_eq!(usage.week_resets_at, 1_791_158_400_000);
        assert_eq!((usage.five_hour_percent, usage.five_hour_resets_at), (Some(10.0), Some(1_790_959_200_000)));
        drop(limits);
        reports.apply_on(measure(31.0, 2.0), now + 2, "2026-10-03");
        assert_eq!(reports.limits.lock().unwrap().day_start.as_ref().map(|d| d.2), Some(31.0), "a new day starts from its own first measurement");
    }

    #[test]
    fn the_last_reading_and_the_days_start_survive_a_restart() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-usage-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let measure = |week: f64| report(json!({"session_id": "s", "event": "session.measure",
            "rate_limits": [{"kind": "five_hour", "percent_used": 10, "resets_at": "2026-10-02T16:40:00Z"},
                            {"kind": "seven_day", "percent_used": week, "resets_at": "2026-10-05T00:00:00Z"}]}));
        let now = 1_790_950_000_000;
        let before = Reports::persisted_in(&home);
        assert!(before.usage(now).is_none(), "nothing measured yet");
        before.apply_on(measure(20.0), now, &local_day(now));
        before.apply_on(measure(30.0), now + 1, &local_day(now));
        let expected = before.usage(now + 2).unwrap();
        drop(before);

        let after = Reports::persisted_in(&home);
        let restored = after.usage(now + 2).unwrap();
        assert_eq!(restored, expected);
        assert_eq!((restored.week_percent, restored.today_spent, restored.measured_at), (30.0, Some(10.0), now + 1));
        assert_eq!((restored.five_hour_percent, restored.five_hour_resets_at), (Some(10.0), Some(1_790_959_200_000)));
        after.apply_on(measure(35.0), now + 3, &local_day(now));
        assert_eq!(after.usage(now + 3).unwrap().today_spent, Some(15.0), "the restored day start still anchors today");
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[test]
    fn a_missing_or_broken_usage_file_starts_empty() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-usage-broken-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        assert!(Reports::persisted_in(&home).usage(1).is_none());
        std::fs::write(home.join(USAGE_FILE), "{not json").unwrap();
        assert!(Reports::persisted_in(&home).usage(1).is_none());
        std::fs::remove_dir_all(&home).unwrap();
    }

    #[tokio::test]
    async fn a_launch_binds_to_the_session_whose_mod_reports_its_key_and_gives_up_after_the_deadline() {
        let reports = std::sync::Arc::new(Reports::default());
        assert_eq!(reports.wait_launched("silent", Duration::from_millis(20)).await, None, "no session.start before the deadline");
        let waiting = tokio::spawn({
            let reports = reports.clone();
            async move { reports.wait_launched("launch-1", MODS_DEADLINE).await }
        });
        tokio::task::yield_now().await;
        reports.apply(report(json!({"session_id": "other", "event": "session.start", "version": "2.1.287", "launch_key": "launch-2"})), 1);
        reports.apply(report(json!({"session_id": "real-session-id", "event": "session.start", "version": "2.1.287", "launch_key": "launch-1"})), 2);
        assert_eq!(waiting.await.unwrap().as_deref(), Some("real-session-id"), "the id is Claude's, with no banner and no listing");
        assert_eq!(reports.wait_launched("launch-1", Duration::ZERO).await.as_deref(), Some("real-session-id"), "a session that already reported does not wait");
        let mut reconnected = hello("after-a-restart");
        reconnected.launch_key = Some("launch-3".into());
        reports.connect(reconnected, 3);
        assert_eq!(reports.wait_launched("launch-3", Duration::ZERO).await.as_deref(), Some("after-a-restart"), "a connection carries the key too");
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

    #[tokio::test]
    async fn waiting_for_a_reconnection_ignores_the_connection_it_replaces() {
        let reports = std::sync::Arc::new(Reports::default());
        let old = reports.connect(hello("s"), 1);
        assert!(!reports.wait_reconnected("s", Some(old), Duration::from_millis(20)).await);
        let waiting = tokio::spawn({
            let reports = reports.clone();
            async move { reports.wait_reconnected("s", Some(old), Duration::from_secs(5)).await }
        });
        tokio::task::yield_now().await;
        reports.connect(hello("s"), 2);
        assert!(waiting.await.unwrap());
    }

    const HOUR: Millis = 60 * 60 * 1000;

    #[test]
    fn a_foreground_build_that_stays_silent_for_an_hour_is_working_while_its_shell_lives_and_stalled_when_it_is_gone() {
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "turn.start", "turn_id": "t"})), 1);
        reports.apply(report(json!({"session_id": "s", "event": "tool.start", "tool": "Bash", "tool_use_id": "u", "input": {"command": "bazel test //..."}})), 2);
        reports.set_shell_alive("s", Some(true));
        let activity = |at| reports.session("s").unwrap().activity(at);
        assert_eq!(activity(HOUR), Activity::Working, "past the call limit, but its shell is alive");
        assert!(!reports.session("s").unwrap().silent(HOUR));
        reports.set_shell_alive("s", Some(false));
        assert_eq!(activity(HOUR), Activity::Stalled, "the call never ends: its shell is gone");
        assert!(reports.session("s").unwrap().silent(HOUR), "and Alondra restarts it");
        reports.set_shell_alive("s", None);
        assert_eq!(activity(HOUR), Activity::Stalled, "nothing known: the call limit decides, as before");
    }

    #[test]
    fn a_live_background_shell_with_an_idle_turn_is_working_and_a_dead_one_with_no_turn_is_stalled() {
        let reports = Reports::default();
        reports.connect(hello("s"), 0);
        reports.sample_cpu("s", 100, 0);
        reports.apply(report(json!({"session_id": "s", "event": "shell.start", "id": "b", "command": "musts run bazel-test-root"})), 1);
        let session = || reports.session("s").unwrap();
        reports.set_shell_alive("s", Some(true));
        assert_eq!(session().activity(2 * HOUR), Activity::Working, "an hour-long wait on a live shell is not a hung one");
        assert!(session().background_at(2 * HOUR).iter().all(|item| !item.suspect));
        assert!(!session().silent(2 * HOUR));
        reports.set_shell_alive("s", Some(false));
        assert_eq!(session().activity(2 * HOUR), Activity::Stalled, "its shell is gone, no turn and nothing reported since");
        assert!(session().silent(2 * HOUR));
        assert_eq!(session().activity(alondra_core::states::TURN_QUIET_MS), Activity::Working, "just after it started it is not stalled yet");
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
        reports.set_shell_alive("s", Some(false));
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
        reports.set_shell_alive("s", Some(true));
        let session = || reports.session("s").unwrap();
        assert_eq!(session().activity(HOUR), Activity::Working, "the shell is alive however still the job and the CPU are");
        assert!(!session().silent(HOUR));
        reports.set_shell_alive("s", Some(false));
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
}
