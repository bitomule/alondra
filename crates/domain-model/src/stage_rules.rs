use super::*;

/// The code of a call an agent's role does not allow.
pub const STAGE_REFUSED: &str = "role_forbids_tool";
pub use alondra_domain::HANDED_OVER_PLAN;

impl DomainState {








    /// Whether `agent` waits on a question it asked in `task` that nobody answered yet: a change of
    /// role never ends an execution that waits for an answer.
    pub fn asks_open(&self, task_id: &TaskId, agent: &AgentId) -> bool {
        self.interactions.values().any(|i| matches!(&i.scope, Scope::Task { task_id: t } if t == task_id)
            && i.author == Author::Agent { agent_id: agent.clone() }
            && !i.withdrawn
            && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. }))
    }

}
