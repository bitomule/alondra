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
}
/// When Claude usage ran out on this Mac and when Claude said it comes back, as local ISO times.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveUsageExhausted {
    pub since: String,
    pub until: String,
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
    #[serde(default)]
    pub reconnecting: bool,
    /// A node stopped by the usage limit waits for the machine until this local ISO time.
    #[serde(default)]
    pub waiting_until: Option<String>,
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
