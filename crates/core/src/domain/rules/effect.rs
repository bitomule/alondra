//! A command that answers Ok changed something, and says what. One that would change nothing is
//! refused with `no_effect`, so a caller never reads a confirmation that nothing backs. A repeated
//! command (same command id) is answered by the store with its first outcome before it gets here.
use super::*;

impl DomainState {
    /// The refusal of `command`, which left the state as it was.
    pub(in crate::domain) fn no_effect(&self, command: &Command) -> DomainError {
        DomainError::new("no_effect", format!(
            "{} changed nothing: {} Nothing was recorded.", command_name(command), self.why_unchanged(command)))
    }

    fn why_unchanged(&self, command: &Command) -> String {
        let state = |task: &TaskId| self.tasks.get(task).map_or_else(String::new, |t| state_words(t.state));
        match command {
            Command::StartTask { task_id } => format!("task {task_id} is already started ({}).", state(task_id)),
            Command::AssignTask { task_id, agent_id, .. } => format!("task {task_id} is already assigned to {agent_id}."),
            Command::CancelTask { task_id } | Command::ReopenTask { task_id } => format!("task {task_id} is already {}.", state(task_id)),
            Command::RegisterPr { pr_id, task_id, .. } => format!("PR {pr_id} is already registered to task {task_id} at this head and base."),
            Command::RecordVerdict { presentation_id, .. } =>
                format!("the verdict on delivery {presentation_id} is already recorded; a new verdict counts once the task delivers again."),
            Command::MarkRead { interaction_id } => format!("card {interaction_id} is already read."),
            Command::Withdraw { interaction_id } => format!("card {interaction_id} is already withdrawn or settled."),
            Command::CloseRequest { request_id, .. } => format!("request {request_id} is already closed this way."),
            Command::WaitForTasks { task_id, .. } => format!("task {task_id} already waits for exactly those tasks."),
            Command::SetDependencies { task_id, .. } | Command::SetRelations { task_id, .. } => format!("task {task_id} already has those dependencies."),
            Command::RetitleTask { task_id, .. } => format!("task {task_id} already has this title."),
            Command::TitleRequest { request_id, .. } => format!("request {request_id} already has this title."),
            _ => "everything it asks for is already so.".into(),
        }
    }

    /// What a command changed, in the words a caller reads instead of a bare "Recorded".
    pub(in crate::domain) fn describe_effect(&self, next: &DomainState, touched: &BTreeSet<TaskId>) -> Vec<String> {
        let mut lines = vec![];
        for id in next.requests.keys().filter(|id| !self.requests.contains_key(*id)) {
            lines.push(format!("Request {id} created."));
        }
        for (id, request) in &next.requests {
            if request.closed && self.requests.get(id).is_some_and(|before| !before.closed) {
                lines.push(format!("Request {id} closed."));
            }
        }
        for id in touched {
            let after = &next.tasks[id];
            match self.tasks.get(id) {
                None => lines.push(format!("Task {id} created, {}.", state_words(after.state))),
                Some(before) if before.state != after.state =>
                    lines.push(format!("Task {id} went from {} to {}.", state_words(before.state), state_words(after.state))),
                Some(_) => {}
            }
        }
        for (id, card) in &next.interactions {
            match self.interactions.get(id) {
                None => lines.push(format!("Card {id} created{}.", if next.open(card) { "" } else { " and already settled" })),
                Some(before) if self.open(before) && !next.open(card) => lines.push(format!("Card {id} settled.")),
                Some(_) => {}
            }
        }
        for id in next.pull_requests.keys().filter(|id| !self.pull_requests.contains_key(*id)) {
            lines.push(format!("PR {id} registered."));
        }
        if lines.is_empty() {
            let tasks: Vec<String> = touched.iter().map(|id| id.0.clone()).collect();
            lines.push(if tasks.is_empty() { "The state changed.".to_owned() } else { format!("Task {} updated.", tasks.join(", ")) });
        }
        lines
    }
}

/// What a command the domain only authorizes does outside it: the daemon runs it once the domain
/// accepts, so the state stays as it was and the caller still reads what happens.
pub(in crate::domain) fn effect_outside_the_domain(command: &Command) -> Option<&'static str> {
    match command {
        Command::ChangeAccounts { change: crate::accounts::AccountChange::Connect, .. } => Some("Alondra connects the provider on this Mac."),
        Command::ChangeAccounts { change: crate::accounts::AccountChange::Add { .. }, .. } => Some("Alondra adds the account and opens its sign-in."),
        Command::ReportFailure { .. } => Some("Alondra prepares the crash report of the task's last failure."),
        _ => None,
    }
}

/// Whether `actor` reads the answer as a confirmation, so an unchanged state is refused. Users,
/// the orchestrator and nodes do. Alondra's own commands are observations and sweeps: they report
/// what it saw (a worktree, an account check, a host's facts) and may find the state already so;
/// several share one transaction, and the daemon reads `changed` instead of a confirmation.
pub(in crate::domain) fn answers_with_its_effect(actor: &Actor) -> bool {
    !matches!(actor, Actor::System)
}

/// The command's name as its callers send it.
pub fn command_name(command: &Command) -> String {
    serde_json::to_value(command).ok()
        .and_then(|value| value.get("kind").and_then(serde_json::Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "command".into())
}

fn state_words(state: TaskState) -> String {
    serde_json::to_value(state).ok().and_then(|value| value.as_str().map(|s| s.replace('_', " "))).unwrap_or_default()
}
