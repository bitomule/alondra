//! What the server reads of the live view the engine keeps beside the domain: the nodes its hooks
//! deduced, what each node's mod reported, the context each task used and the wait a node reported.
use alondra_core::{BackgroundItem, Millis, Node};
use alondra_view_model::RunOutcome;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct Answer {
    pub text: String,
    pub at: Millis,
    pub aborted: bool,
}

/// What a node's mod last reported, as the screens read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionReading {
    pub started_at: Option<Millis>,
    pub cwd: Option<String>,
    pub tool: Option<alondra_core::ToolInFlight>,
    pub answer: Option<Answer>,
    /// The newest text the node wrote, from its host's events or its last answer.
    pub last_message: Option<(String, Millis)>,
    /// Why Claude Code's own job state says the session failed.
    pub failure: Option<String>,
    pub model: Option<String>,
    /// The subagents and background shells it has running, by Claude Code's id, with when a shell
    /// was last seen gone.
    pub background: Vec<(String, BackgroundItem, Option<End>)>,
    pub agents: Vec<AgentRun>,
    pub workflows: Vec<WorkflowRun>,
    pub ended_tasks: Vec<EndedTask>,
    pub steps: Vec<Step>,
    /// What the Agent SDK host says the session spent.
    pub cost_cents: Option<u64>,
}

impl SessionReading {
    /// Whether `id` is a subagent or workflow still running.
    pub fn runs(&self, id: &str) -> bool {
        self.agents.iter().any(|agent| agent.id == id && agent.end.is_none())
            || self.workflows.iter().any(|workflow| workflow.id == id && workflow.end.is_none())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Deleted,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AgentRun {
    pub id: String,
    pub description: String,
    pub kind: Option<String>,
    pub model: Option<String>,
    pub started_at: Millis,
    pub end: Option<End>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkflowRun {
    pub id: String,
    pub name: String,
    /// Where the workflow writes one transcript per agent it runs.
    pub transcript_dir: Option<PathBuf>,
    pub started_at: Millis,
    pub end: Option<End>,
}

/// When something the node ran ended, and how, as the fact that ended it says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct End {
    pub at: Millis,
    pub outcome: RunOutcome,
}

/// A shell or other background task that ended; subagents and workflows keep their end in their
/// own run.
#[derive(Clone, Debug, PartialEq)]
pub struct EndedTask {
    pub item: BackgroundItem,
    pub end: End,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: String,
    pub subject: String,
    pub status: StepStatus,
    pub active_form: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Used {
    pub tokens: u64,
    pub window: Option<u64>,
}

impl Used {
    pub fn percent(&self) -> Option<f64> {
        self.window.map(|window| self.tokens as f64 * 100.0 / window as f64)
    }
}

#[derive(Debug, Deserialize)]
pub struct WaitReport {
    pub what: String,
    pub detail: Option<String>,
    pub progress: Option<String>,
    pub since_ms: Millis,
    pid: Option<i32>,
}

/// A report is display text, never an input to admission or lifecycle decisions.
pub fn read_wait_file(path: &std::path::Path) -> Option<WaitReport> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW).open(path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > 4096 { return None; }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).ok()?;
    if bytes.len() > 4096 { return None; }
    let mut report: WaitReport = serde_json::from_slice(&bytes).ok()?;
    if let Some(pid) = report.pid {
        if pid <= 0 { return None; }
        if unsafe { libc::kill(pid, 0) } != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EPERM) { return None; }
    }
    fn single_line(text: &str, limit: usize) -> String {
        text.chars().map(|c| if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') { ' ' } else { c }).take(limit).collect::<String>().trim().into()
    }
    report.what = single_line(&report.what, 60);
    if report.what.is_empty() { return None; }
    report.progress = report.progress.map(|text| single_line(&text, 40)).filter(|text| !text.is_empty());
    report.detail = report.detail.map(|text| single_line(&text, 120)).filter(|text| !text.is_empty());
    Some(report)
}

/// How many agents a workflow has started, by the transcripts it wrote; none when its folder
/// cannot be read or holds none yet.
pub fn workflow_agents(dir: &std::path::Path) -> Option<u32> {
    let count = std::fs::read_dir(dir).ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|extension| extension == "jsonl"))
        .count();
    (count > 0).then(|| count.min(u32::MAX as usize) as u32)
}

/// The nodes `/runtime` lists. A node of the Agent SDK reads its state from its task's lifecycle,
/// never from what the deduction kept for `claude --bg` nodes made of it, and is listed even when
/// that deduction never saw it. `since` is when a task last changed state.
pub fn runtime_nodes(deduced: Vec<Node>, state: &alondra_domain_model::DomainState, since: impl Fn(&alondra_domain_model::TaskId) -> Option<i64>) -> Vec<Value> {
    use alondra_domain_model::AgentId;
    let sdk = |agent: &AgentId| through_sdk(state, agent);
    let mut nodes: Vec<Node> = deduced.into_iter().filter(|node| state.agents.contains_key(&AgentId(node.session_id.clone()))).collect();
    let unseen: Vec<Node> = state.agents.keys()
        .filter(|agent| sdk(agent) && state.sdk_live(agent).is_some() && !nodes.iter().any(|node| node.session_id == agent.0))
        .map(|agent| Node::new(&agent.0, state.node_task(agent).and_then(|task| since(&task.id)).unwrap_or_default()))
        .collect();
    nodes.extend(unseen);
    nodes.iter_mut().filter(|node| sdk(&AgentId(node.session_id.clone()))).for_each(|node| from_lifecycle(node, state));
    nodes.iter().map(|node| client_node(node, state.agents.get(&AgentId(node.session_id.clone())).map(|agent| agent.name.as_str()))).collect()
}

fn from_lifecycle(node: &mut Node, state: &alondra_domain_model::DomainState) {
    use alondra_core::{NodeState, NodeStatus, StateSource};
    use alondra_domain::LiveState;
    let agent = alondra_domain_model::AgentId(node.session_id.clone());
    let result = state.node_task(&agent).and_then(|task| task.final_presentation.as_ref());
    node.state = match state.sdk_live(&agent) {
        Some(LiveState::Working(_)) => NodeState::Working,
        Some(LiveState::WaitingUser(cards)) if cards.iter().all(|card| Some(card) == result) => NodeState::WaitingMerge,
        Some(LiveState::Paused) => NodeState::Idle,
        Some(LiveState::WaitingUser(_) | LiveState::WaitingTask(_)) => NodeState::WaitingAnswer,
        Some(LiveState::WaitingCapacity(_)) => NodeState::WaitingMachine,
        None => NodeState::Done,
    };
    node.status = match node.state {
        NodeState::Working => NodeStatus::Busy,
        NodeState::WaitingMachine => NodeStatus::WaitingMachine,
        _ => NodeStatus::Free,
    };
    node.busy = node.state == NodeState::Working;
    node.state_source = StateSource::Daemon;
    node.evidence = None;
}

pub fn through_sdk(state: &alondra_domain_model::DomainState, agent: &alondra_domain_model::AgentId) -> bool {
    state.through_sdk(agent) || state.node_phase(agent) == Some(&alondra_domain_model::NodePhase::ThroughSdk)
}

/// A node as a client reads it: what the daemon keeps for itself stays out.
pub fn client_node(node: &Node, name: Option<&str>) -> Value {
    let mut value = serde_json::to_value(node).unwrap_or(Value::Null);
    if let Some(fields) = value.as_object_mut() {
        fields.remove("alive");
        fields.remove("ledger");
        if node.name.is_none() {
            fields.insert("name".into(), json!(name));
        }
        fields.insert("project".into(), json!(node.project()));
    }
    value
}

