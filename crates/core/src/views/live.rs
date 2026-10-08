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
    #[serde(default)]
    pub search_matches: Option<Vec<String>>,
    #[serde(default)]
    pub usage_exhausted: Option<LiveUsageExhausted>,
    #[serde(default)]
    pub pacing: Option<LivePacing>,
}

/// How many nodes work against the limit, how the Mac is, and the place of every queued task.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LivePacing {
    pub working: usize,
    pub limit: u32,
    pub memory_free: Option<u32>,
    /// The Mac is short of memory: Tight or at its limit.
    pub tight: bool,
    pub queued: Vec<LiveQueued>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveQueued {
    pub task_id: String,
    pub position: usize,
    pub of: usize,
    pub hold: LiveHold,
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
}
/// When Claude usage ran out on this Mac and when Claude said it comes back, as local ISO times.
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
    #[serde(default)]
    pub background_suspect: bool,
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
    #[serde(default)]
    pub context_percent: Option<u32>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub cost_cents: Option<u64>,
    /// The model id the node's main thread runs on.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reconnecting: bool,
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
    /// How long it ran, once it finished.
    #[serde(default)]
    pub ran_secs: Option<u64>,
    #[serde(default)]
    pub suspect: bool,
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
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveBackground {
    pub kind: String,
    pub label: String,
    pub since: String,
    pub suspect: bool,
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
pub struct LiveTaskOptions {
    pub project: Option<String>,
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
