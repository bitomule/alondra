use super::*;

/// Specs exist only where the task's project turns them on.
pub const SPECS_OFF: &str = "This project does not use specs: its alondra.toml does not turn them on, so there is nothing to propose";

impl DomainState {
    /// Whether Alondra still owes the task's worktree approved spec files: no change of role
    /// starts until they are written.
    pub fn specs_due(&self, task_id: &TaskId) -> bool {
        self.tasks.get(task_id).is_some_and(|task| !task.specs.due().is_empty())
    }
    /// The task's plan card the user has still to decide on.
    pub fn open_plan_card(&self, task: &TaskId) -> Option<&Interaction> {
        self.interactions.values().find(|i| self.holds.get(&i.id) == Some(&Hold::Plan) && !i.withdrawn
            && matches!(&i.scope, Scope::Task { task_id } if task_id == task)
            && matches!(i.payload, InteractionPayload::Question { answer: None, .. }))
    }















}

