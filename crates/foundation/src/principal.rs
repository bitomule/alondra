//! Who a domain writer's capability names, and the capability itself. The registry that issues and
//! checks them is the daemon's (`domain_authority`); the engine's pieces only pass them along.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Principal {
    Agent {
        session_id: String,
        task_binding: Option<String>,
        provider_agent_id: Option<String>,
    },
    Orchestrator {
        generation: u64,
        execution_id: String,
    },
}

/// Deliberately has no Debug/Serialize implementation: never put bearer credentials in logs.
pub struct Capability(String);
impl Capability {
    pub fn issued(token: String) -> Self {
        Self(token)
    }

    pub fn expose_for_child_environment(&self) -> &str {
        &self.0
    }
}
