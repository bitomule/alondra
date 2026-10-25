use super::{Gauge, Notice};
use crate::{
    issues::Issue,
    options::{ProjectEntry, Spec},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LiveSnapshot {
    #[serde(default)]
    pub closed_nodes: Vec<String>,
    /// Tasks whose node the user closed, waiting for a relaunch.
    #[serde(default)]
    pub closed_tasks: Vec<String>,
    #[serde(default)]
    pub nodes: Vec<LiveNode>,
    #[serde(default)]
    pub pull_requests: Vec<LivePullRequest>,
    #[serde(default)]
    pub orchestrator: Option<LiveOrchestrator>,
    #[serde(default)]
    pub update: Option<Notice>,
    #[serde(default)]
    pub gauges: Vec<Gauge>,
    #[serde(default)]
    pub inbox_times: Vec<InboxTime>,
    #[serde(default)]
    pub request_times: BTreeMap<String, String>,
    #[serde(default)]
    pub projects: Vec<ProjectEntry>,
    #[serde(default)]
    pub options: Vec<Spec>,
    #[serde(default)]
    pub project_options: BTreeMap<String, Vec<Spec>>,
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub issue_error: Option<String>,
    #[serde(default)]
    pub activity: BTreeMap<String, Vec<LiveActivity>>,
    #[serde(default)]
    pub task_history: BTreeMap<String, Vec<LiveHistoryStep>>,
    #[serde(default)]
    pub task_options: BTreeMap<String, LiveTaskOptions>,
    /// When each open task last changed state, by task id: the Tasks list says since when its fact holds.
    #[serde(default)]
    pub task_since: BTreeMap<String, String>,
    /// When each open or finished task first started running and, once it is over, when it ended.
    #[serde(default)]
    pub task_started: BTreeMap<String, LiveTaskStart>,
    /// Why each cancelled task was cancelled, when someone said so.
    #[serde(default)]
    pub cancel_reasons: BTreeMap<String, String>,
    #[serde(default)]
    pub search_matches: Option<Vec<String>>,
    #[serde(default)]
    pub usage_exhausted: Option<LiveUsageExhausted>,
    /// New work waits because no Claude account has a session; no time ends it.
    #[serde(default)]
    pub waits_for_session: bool,
    /// Why the last launch of a task that has not started was refused, by task id.
    #[serde(default)]
    pub launch_refusals: BTreeMap<String, LiveLaunchRefusal>,
    /// What a task never started waits for before Alondra starts it by itself, by task id.
    #[serde(default)]
    pub start_waits: BTreeMap<String, LiveStartWait>,
    #[serde(default)]
    pub pacing: Option<LivePacing>,
    /// The rules «allow always» added on this Mac that its repos' settings still have.
    #[serde(default)]
    pub allowed_rules: Vec<LiveAllowedRule>,
    /// Permission cards whose «allow always» Claude Code never wrote.
    #[serde(default)]
    pub allow_always_missing: Vec<String>,
    /// What the last check of each project without a live node found of its MCP servers, by
    /// project name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mcp_probes: BTreeMap<String, LiveMcpProbe>,
    /// The MCP servers the user deleted while this daemon runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_removed: Vec<LiveMcpRemoval>,
}

/// An MCP server the user deleted from the agent's settings. The nodes that had it loaded keep it
/// until their agent starts again: `loaded` holds each one's execution then, by agent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveMcpRemoval {
    pub key: crate::domain::McpServerKey,
    pub loaded: BTreeMap<String, u64>,
}

impl LiveMcpRemoval {
    /// Whether the node `agent` reported `server` at `generation` from before it was deleted.
    pub fn hides(&self, agent: &str, generation: u64, server: &crate::domain::McpServerState) -> bool {
        self.loaded.get(agent) == Some(&generation) && self.key.names(server)
    }
}

/// A check of a project's MCP servers: Claude started in the project, asked for nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LiveMcpProbe {
    Checking,
    /// As Claude listed them, at this local ISO time.
    Checked { at: String, servers: Vec<crate::domain::McpServerState> },
    Failed { at: String, error: String },
}

/// A rule «allow always» added on this Mac: what it lets through, in which repo, and from where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveAllowedRule {
    pub id: i64,
    pub repo: String,
    /// As Claude Code matches it: `make build *`.
    pub rule: String,
    /// When it was added, as local ISO time.
    pub added: String,
    pub task_id: Option<String>,
}

/// What a task never started waits for before Alondra starts it by itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveStartWait {
    /// It starts when the gate has room.
    Room,
    /// Its last start failed; Alondra tries again at this time of day.
    Retry { at: String },
}

/// How many nodes work against the limit, how the Mac is, and the place of every queued task.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePacing {
    pub working: usize,
    pub limit: u32,
    /// What `max_working_nodes` in `options.toml` says: a number, or `None` for automatic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed: Option<u32>,
    /// The limit automatic gives on this Mac.
    #[serde(default)]
    pub auto: u32,
    /// The highest number this Mac takes.
    #[serde(default)]
    pub most: u32,
    pub memory_free: Option<u32>,
    /// Memory has no room for one more node to start; the nodes at work go on.
    pub tight: bool,
    pub queued: Vec<LiveQueued>,
    /// Nodes Alondra or the user paused, in the order they come back.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paused: Vec<LivePaused>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePaused {
    pub task_id: String,
    /// The paused node; `None` for a queued task the user took out of the queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Local clock time it was paused at, "14:05".
    pub at: String,
    pub reason: LivePauseReason,
    /// Always `None` since memory stopped pausing nodes; kept for apps that decode it.
    #[serde(default)]
    pub fine_minutes: Option<u32>,
    /// Nothing but the minute between two admissions holds it back.
    #[serde(default)]
    pub next: bool,
    /// What the node said it waits for, when it declared the wait itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waits_for: Option<String>,
    /// For an API error: the local clock time Alondra sends it back, "14:07", and the attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<LiveRetry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRetry {
    pub at: String,
    pub attempt: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LivePauseReason {
    Memory,
    /// The node said it waits for the machine.
    Machine,
    Usage,
    User,
    /// A Claude API error ended its turn; Alondra sends it back by itself.
    ApiError,
    /// The Claude API looks down for every node: it waits for it to come back.
    ApiOutage,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveQueued {
    pub task_id: String,
    pub position: usize,
    pub of: usize,
    pub hold: LiveHold,
    /// The user can take it out of the queue: a task waiting to start, not a scheduled run or a
    /// node coming back.
    #[serde(default)]
    pub pausable: bool,
}

/// What the queue waits for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LiveHold {
    Slots { working: usize, limit: u32 },
    Memory { free: Option<u32> },
    Cpu { load: u32, cpus: usize },
    /// Nothing holds it but the minute between two starts.
    Next,
    /// No room on the disk for one more build: the free space and what one more needs, in bytes.
    Disk {
        #[serde(default)]
        free: u64,
        #[serde(default)]
        needed: u64,
    },
}
/// `reason` is `usage_exhausted`, `disk_low`, `workspace_untrusted` (with the folder as `detail`)
/// or `launch_failed` (with the error).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveLaunchRefusal {
    pub reason: String,
    #[serde(default)]
    pub detail: Option<String>,
}
/// Since when no Claude account of this Mac can work and when the soonest one can again, as local
/// ISO times.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveUsageExhausted {
    pub since: String,
    pub until: String,
    #[serde(default)]
    pub paused_by_user: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveNode {
    pub agent_id: String,
    #[serde(default)]
    pub short_id: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    pub name: String,
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub state_key: Option<String>,
    #[serde(default)]
    pub state_label: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub needs: Option<String>,
    #[serde(default)]
    pub background: Vec<LiveBackground>,
    #[serde(default)]
    pub background_badge: Option<String>,
    pub working: bool,
    #[serde(default)]
    pub waiting: bool,
    #[serde(default)]
    pub state_since: Option<String>,
    pub last_message: Option<String>,
    pub last_message_at: Option<String>,
    /// The rest is what the node mod reports.
    #[serde(default)]
    pub last_turn_aborted: bool,
    #[serde(default)]
    pub tool: Option<String>,
    /// When that call started, as local clock text: a long call runs until it, its own timeout or
    /// Claude ends it, and says since when.
    #[serde(default)]
    pub tool_since: Option<String>,
    #[serde(default)]
    pub context_percent: Option<u32>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub cost_cents: Option<u64>,
    /// The model id the node's main thread runs on.
    #[serde(default)]
    pub model: Option<String>,
    /// Its connection to Alondra has been gone for longer than it takes to come back by itself, and
    /// no restart runs: whatever it showed last is stale.
    #[serde(default)]
    pub disconnected: bool,
    /// A node stopped by the usage limit waits for the machine until this local ISO time.
    #[serde(default)]
    pub waiting_until: Option<String>,
    /// The subagents it runs and the last few it ran.
    #[serde(default)]
    pub subagents: Vec<LiveRun>,
    #[serde(default)]
    pub workflows: Vec<LiveRun>,
    /// Its Claude Code task list, in order.
    #[serde(default)]
    pub steps: Vec<LiveStep>,
}
/// Whether something a node started in the background runs, or how it ended. Only a fact moves
/// it: Claude's notice that the work ended (with its status), the work leaving Claude's list, its
/// shell seen gone, or the session ending.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Running,
    Ended { outcome: RunOutcome, ran_secs: u64 },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    /// It ended by itself, or left Claude's list of running work.
    Finished,
    Failed,
    /// Claude or the user stopped it, or the session it ran in ended.
    Stopped,
}
impl RunOutcome {
    pub const ALL: [RunOutcome; 3] = [RunOutcome::Finished, RunOutcome::Failed, RunOutcome::Stopped];
}
/// A subagent or a workflow a node started.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRun {
    pub label: String,
    /// The subagent's type (`general-purpose`, `Explore`).
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// How many agents a workflow has started, when known.
    #[serde(default)]
    pub agents: Option<u32>,
    pub since: String,
    #[serde(default)]
    pub state: RunState,
    /// What a workflow's own files say about it: its phase and the agents it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<LiveWorkflow>,
}
/// The most agent rows a workflow carries; the rest are only counted.
pub const WORKFLOW_AGENT_ROWS: usize = 8;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveWorkflow {
    #[serde(default)]
    pub phase: Option<String>,
    /// The phase's place in the script's phase list, from 1, and how many phases it lists.
    #[serde(default)]
    pub phase_index: Option<u32>,
    #[serde(default)]
    pub phase_count: Option<u32>,
    #[serde(default)]
    pub running: u32,
    #[serde(default)]
    pub done: u32,
    /// The most recently started agents first, at most `WORKFLOW_AGENT_ROWS`.
    #[serde(default)]
    pub agents: Vec<LiveWorkflowAgent>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub total_tokens: Option<u64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveWorkflowAgent {
    pub label: String,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub running: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    #[default]
    Pending,
    InProgress,
    Completed,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveStep {
    pub subject: String,
    pub state: StepState,
    /// What the spinner says while it runs ("Running tests").
    #[serde(default)]
    pub active_form: Option<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePullRequest {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub head_ref: Option<String>,
    #[serde(default)]
    pub base_ref: Option<String>,
    #[serde(default)]
    pub checks: Vec<LiveCheck>,
    pub checks_passed: Option<bool>,
    #[serde(default)]
    pub additions: u64,
    #[serde(default)]
    pub deletions: u64,
    #[serde(default)]
    pub changed_files: u64,
    pub conflicts: bool,
    pub stacked_on: Option<String>,
    pub url: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveCheck {
    pub name: String,
    pub state: super::CheckState,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveOrchestrator {
    pub working: bool,
    /// The last run's error as Alondra recorded it; present while that failure stands.
    pub summary: Option<String>,
    /// Its kind: launch, interrupted, denied, cancelled or session_missing.
    #[serde(default)]
    pub failure: Option<String>,
    #[serde(default)]
    pub queue_depth: u32,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub can_retry: bool,
    #[serde(default)]
    pub can_cancel: bool,
    /// No batch starts because no Claude account has a session.
    #[serde(default)]
    pub waits_for_sign_in: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveBackground {
    pub kind: String,
    pub label: String,
    pub since: String,
    #[serde(default)]
    pub state: RunState,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxTime {
    pub id: String,
    pub at: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveActivity {
    pub agent_id: Option<String>,
    pub agent_type: Option<String>,
    pub task: Option<String>,
    pub tool: Option<String>,
    pub summary: Option<String>,
    pub since: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveHistoryStep {
    pub at: String,
    pub state: String,
    pub summary: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveTaskStart {
    pub since: String,
    pub until: Option<String>,
    pub ran_secs: Option<u64>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveTaskOptions {
    pub effort: Option<String>,
}
impl LiveSnapshot {
    pub fn time(&self, id: &str) -> Option<&str> {
        self.inbox_times
            .iter()
            .find(|time| time.id == id)
            .map(|time| time.at.as_str())
    }
}
