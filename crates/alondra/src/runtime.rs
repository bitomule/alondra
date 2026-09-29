//! What alondrad knows about the Mac and its Claude sessions beyond the domain: machine and
//! usage readings, each session's live state, PR checks and timestamps. Read from
//! `/runtime`, shown next to the projection and never used to decide an action.

use std::collections::HashMap;

use alondra_core::domain_v2::{AgentId, TaskState};
use alondra_core::{Machine, Node};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub const RUNTIME: &str = "/runtime";

/// A Claude session as the daemon sees it; `session_id` is the domain's AgentId.
#[derive(Deserialize, Clone)]
pub struct FleetNode {
    #[serde(default)]
    pub orchestrator_runtime: Option<OrchestratorRuntime>,
    #[serde(flatten)]
    pub node: Node,
    #[serde(default)]
    pub project: Option<String>,
}

impl std::ops::Deref for FleetNode {
    type Target = Node;
    fn deref(&self) -> &Node {
        &self.node
    }
}

impl FleetNode {
    pub fn status_label(&self) -> &'static str {
        match self.orchestrator_runtime.as_ref() {
            Some(runtime) => runtime.status_label(),
            None => crate::i18n::fleet_state(&self.node),
        }
    }
}

/// Public status only; Claude conversation and execution identities stay internal.
#[derive(Deserialize, Clone, Default, Debug, PartialEq)]
pub struct OrchestratorRuntime {
    pub phase: String,
    #[serde(default)]
    pub can_retry: bool,
    #[serde(default)]
    pub can_cancel: bool,
    #[serde(default)]
    pub cancel_requested: bool,
    #[serde(default)]
    pub active_run_id: Option<String>,
    #[serde(default)]
    pub queue_depth: usize,
    #[serde(default)]
    pub last_error: Option<String>,
    #[serde(default)]
    pub retry_at: Option<i64>,
}

impl OrchestratorRuntime {
    pub fn status_label(&self) -> &'static str {
        if self.cancel_requested {
            return tr!("stopping", "deteniendo");
        }
        match self.phase.as_str() {
            "idle" => tr!("available", "disponible"),
            "starting" => tr!("starting", "iniciando"),
            "running" => tr!("handling a request", "atendiendo una petición"),
            "compacting" => tr!("compacting context", "compactando contexto"),
            "failed" if self.retry_at.is_some() => tr!("retrying", "reintentando"),
            "failed" => tr!("needs attention", "requiere atención"),
            _ => tr!("unknown", "desconocido"),
        }
    }
}

/// One thread of a session, the main one (no `agent_id`) or a subagent, and what it does now.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Activity {
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub agent_type: Option<String>,
    #[serde(default)]
    pub task: Option<String>,
    #[serde(default)]
    pub tool: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    pub since: i64,
}

impl Activity {
    pub fn is_main(&self) -> bool {
        self.agent_id.is_none()
    }

    /// `main` for the session's own thread, the subagent's type otherwise.
    pub fn thread(&self) -> String {
        match (&self.agent_id, &self.agent_type) {
            (None, _) => tr!("main", "principal").into(),
            (Some(_), Some(t)) => t.clone(),
            (Some(_), None) => tr!("subagent", "subagente").into(),
        }
    }
}

#[derive(Deserialize, Clone, Default, Debug, PartialEq)]
pub struct TaskRuntime {
    #[serde(default)]
    pub project: Option<String>,
    #[serde(default)]
    pub state_since: Option<i64>,
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub cancel_reason: Option<String>,
}

/// When a request came in and how it closed, which the domain's `closed` flag does not say.
#[derive(Deserialize, Clone, Default, Debug, PartialEq)]
pub struct RequestRuntime {
    #[serde(default)]
    pub created_at: Option<i64>,
    #[serde(default)]
    pub withdrawn: bool,
    #[serde(default)]
    pub answer: Option<String>,
}

#[derive(Deserialize, Clone, Default, Debug, PartialEq)]
pub struct PrRuntime {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub checks: Vec<alondra_core::pr::Check>,
    #[serde(default)]
    pub conflicts: bool,
    #[serde(default)]
    pub mergeability_unknown: bool,
}

/// Every field is optional and an entry this build cannot read costs only that entry.
#[derive(Deserialize, Clone, Default)]
pub struct Runtime {
    #[serde(default, deserialize_with = "lenient_one")]
    pub machine: Option<Machine>,
    #[serde(default, deserialize_with = "lenient_one")]
    pub usage: Option<alondra_core::usage::Usage>,
    #[serde(default, deserialize_with = "lenient_one")]
    pub update: Option<alondra_core::update::Status>,
    #[serde(default, deserialize_with = "lenient")]
    pub nodes: Vec<FleetNode>,
    #[serde(default, deserialize_with = "lenient_one")]
    pub orchestrator: Option<OrchestratorRuntime>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub tasks: HashMap<String, TaskRuntime>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub requests: HashMap<String, RequestRuntime>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub pull_requests: HashMap<String, PrRuntime>,
    #[serde(default, deserialize_with = "lenient_map")]
    pub inbox_times: HashMap<String, i64>,
}

impl Runtime {
    pub fn node(&self, id: &AgentId) -> Option<&FleetNode> {
        self.nodes.iter().find(|n| n.session_id == id.0)
    }

    pub fn orchestrator_node(&self) -> Option<&FleetNode> {
        self.nodes.iter().find(|n| n.is_orchestrator)
    }

    /// The orchestrator's execution state, from its node or from the top-level summary.
    pub fn orchestrator_state(&self) -> Option<&OrchestratorRuntime> {
        self.orchestrator_node().and_then(|n| n.orchestrator_runtime.as_ref()).or(self.orchestrator.as_ref())
    }

    pub fn task(&self, id: &str) -> Option<&TaskRuntime> {
        self.tasks.get(id)
    }

    pub fn pull_request(&self, id: &str) -> Option<&PrRuntime> {
        self.pull_requests.get(id)
    }

    pub fn request(&self, id: &str) -> Option<&RequestRuntime> {
        self.requests.get(id)
    }

    pub fn working(&self) -> usize {
        self.nodes.iter().filter(|n| n.state == alondra_core::NodeState::Working).count()
    }
}

/// A task's history as the daemon recorded it, with the brief it was launched with.
#[derive(Deserialize, Clone, Default, Debug, PartialEq)]
pub struct TaskHistory {
    #[serde(default)]
    pub brief: Option<String>,
    #[serde(default, deserialize_with = "lenient")]
    pub history: Vec<Step>,
}

#[derive(Deserialize, Clone, Debug, PartialEq)]
pub struct Step {
    pub at: i64,
    pub state: TaskState,
    #[serde(default)]
    pub summary: String,
}

fn lenient<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Option::<Vec<Value>>::deserialize(d)?.unwrap_or_default();
    Ok(values.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

fn lenient_one<'de, D, T>(d: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    Ok(serde_json::from_value(Value::deserialize(d)?).ok())
}

fn lenient_map<'de, D, T>(d: D) -> Result<HashMap<String, T>, D::Error>
where
    D: Deserializer<'de>,
    T: DeserializeOwned,
{
    let values = Option::<HashMap<String, Value>>::deserialize(d)?.unwrap_or_default();
    Ok(values.into_iter().filter_map(|(k, v)| serde_json::from_value(v).ok().map(|v| (k, v))).collect())
}

#[cfg(test)]
pub(crate) fn fixture(name: &str) -> Value {
    let path = match std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR")) {
        Ok(root) => std::path::PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into())).join("fixtures/client-v2").join(name),
        Err(_) => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/client-v2").join(name),
    };
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_fixture_reads_whole_and_a_broken_entry_costs_only_itself() {
        let runtime: Runtime = serde_json::from_value(fixture("runtime.json")).unwrap();
        assert!(runtime.machine.is_some() && runtime.usage.is_some() && runtime.update.is_some());
        assert_eq!(runtime.nodes.len(), 3, "the node with no state is dropped, the others stay");
        assert_eq!(runtime.node(&"agent-a".into()).and_then(|n| n.project.as_deref()), Some("App"));
        assert_eq!(runtime.orchestrator_state().and_then(|o| o.active_run_id.as_deref()), Some("run-7"));
        assert_eq!(runtime.task("task-1").and_then(|t| t.effort.as_deref()), Some("hard"));
        assert!(runtime.pull_request("pr-1").is_some_and(|p| p.conflicts && p.checks.len() == 2));
        assert_eq!(runtime.inbox_times.get("interaction:info-1"), Some(&1_000));
        let empty: Runtime = serde_json::from_value(serde_json::json!({"machine": "broken", "nodes": null})).unwrap();
        assert!(empty.machine.is_none() && empty.nodes.is_empty());
    }
}
