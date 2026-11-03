use super::*;
use crate::accounts::{AccountBilling, AccountKey, AccountState};
use crate::model_selection::{PendingTaskSelection, TaskSelection};

impl DomainState {
    pub fn task_selection_stamp(&self, task_id: &TaskId) -> Option<String> {
        use sha2::Digest;
        let task = self.tasks.get(task_id)?;
        let execution = self.lifecycle_of(task_id).ok().and_then(|lifecycle| lifecycle.execution().map(|execution| (
            execution.generation(), execution.node().clone(), execution.host().as_str().to_owned(), execution.instance().as_str().to_owned(), execution.conversation().as_str().to_owned(),
        )));
        let bytes = serde_json::to_vec(&(&self.data_epoch, task_id, &task.selection, &task.pending_selection, &task.model, task.provider, &task.account, &task.agent_id, execution)).ok()?;
        Some(sha2::Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub(super) fn task_selection_guard(&self, actor: &Actor, task_id: &TaskId, expected_selection: &str, selection: &TaskSelection, pending: &PendingTaskSelection) -> Result<()> {
        user(actor)?;
        self.not_ending(task_id)?;
        let task = self.active(task_id)?;
        if pending.account != task.account_key() || pending.model.model.provider != task.provider {
            return fail("invalid_input","A running node keeps its original provider and account; choose those only when creating a node");
        }
        if selection.account.as_ref()!=task.selection.as_ref().and_then(|previous|previous.account.as_ref())
            || selection.paid_account_grant.as_ref()!=task.selection.as_ref().and_then(|previous|previous.paid_account_grant.as_ref()) {
            return fail("invalid_input","An active model change preserves the original account choice and payment authorization");
        }
        if task.pending_selection.is_some() { return fail("selection_pending", "A selection change is already pending for this task"); }
        if self.task_selection_stamp(task_id).as_deref() != Some(expected_selection) { return fail("stale_selection", "The task's selection or execution changed; refresh before choosing again"); }
        let current_effort=task.selection.as_ref().and_then(|selection|selection.effective.as_ref()).and_then(|effective|effective.reasoning_effort.as_ref());
        if pending.model.reasoning_effort.as_ref()!=current_effort {
            return fail("invalid_input","This native session supports changing its model while keeping its current reasoning effort; choose effort when creating a node");
        }
        if selection.effective.as_ref() != task.selection.as_ref().and_then(|selection| selection.effective.as_ref()) {
            return fail("invalid_input", "A requested selection cannot claim that its model is already running");
        }
        if task.selection.as_ref().is_some_and(|previous| previous.profile != selection.profile) {
            return fail("invalid_input", "Changing the model does not change the task's work profile");
        }
        if selection.native_tool_denials.as_ref()!=task.selection.as_ref().and_then(|previous|previous.native_tool_denials.as_ref()) {
            return fail("invalid_input","A model choice cannot change the execution's verified tool policy");
        }
        if pending.account.provider==crate::accounts::Provider::Codex {
            match selection.native_tool_denials.as_deref() {
                Some([])=>{},
                Some(_)=>return fail("unsupported_tool_policy","Codex cannot enforce this task's retained tool restrictions"),
                None=>return fail("unsupported_tool_policy","The current execution's tool policy must be verified before choosing Codex"),
            }
        }
        if pending.reason.trim().is_empty() || pending.reason.len() > 2000 { return fail("invalid_input", "A pending selection needs a bounded reason"); }
        if pending.account.provider != pending.model.model.provider
            || !selection.allows(&pending.account, &pending.model.model)
            || (selection.requested.as_ref().is_none_or(|model| model == &pending.model.model)
                && selection.requested_reasoning_effort.as_ref().is_some_and(|effort| pending.model.reasoning_effort.as_ref() != Some(effort)))
        { return fail("invalid_input", "The requested account, model and reasoning effort are inconsistent"); }
        let account = self.accounts.iter().find(|account| AccountKey::new(account.provider, account.id.clone()) == pending.account)
            .ok_or_else(|| DomainError::new("not_found", "The selected provider account is not connected"))?;
        if account.state != AccountState::SignedIn { return fail("account_unavailable", "Sign in to the selected account before changing this task"); }
        if matches!(account.billing, AccountBilling::PayPerUse) && (selection.account.as_ref() != Some(&pending.account) || selection.paid_account_grant.as_ref()!=Some(&pending.account)) {
            return fail("invalid_input", "An account paid per use must be selected explicitly");
        }
        let catalog = self.model_catalogs.iter().find(|catalog| catalog.account == pending.account)
            .ok_or_else(|| DomainError::new("model_unavailable", "Refresh models for the selected account"))?;
        catalog.validate(&pending.model).map_err(|reason| DomainError::new("model_unavailable", reason))
    }
}


/// Why a task moves to another account, as its pending selection and the move's journal keep it.
pub const MOVE_REASON: &str = "Moved to another account";

impl DomainState {
    /// The account a task is moving to: its pending selection names another account than the
    /// one its execution runs on.
    pub fn moving_to(&self, task_id: &TaskId) -> Option<&AccountKey> {
        let task = self.tasks.get(task_id)?;
        task.pending_selection.as_ref().map(|pending| &pending.account).filter(|account| **account != task.account_key())
    }

    /// The account the task runs on, or will start on: its execution's, or the one its selection
    /// pins while it has never run.
    fn current_account(&self, task: &Task) -> Option<AccountKey> {
        if task.started || task.agent_id.is_some() { Some(task.account_key()) } else { task.selection.as_ref().and_then(|selection| selection.account.clone()) }
    }

    /// The selection a task keeps once it moves to `account`, and what its next execution runs
    /// there: the account becomes its own, and a model of another provider gives way to the one
    /// the destination picks for the task's profile. Pending only for a task that already ran:
    /// one that never ran simply starts on `account`.
    pub fn moved_selection(&self, actor: &Actor, task_id: &TaskId, account: &AccountKey) -> Result<(TaskSelection, Option<PendingTaskSelection>)> {
        let task = self.task(task_id)?;
        let mut intent = task.selection.clone().unwrap_or_else(|| TaskSelection::automatic(crate::model_selection::ModelProfile::Implementation));
        if intent.requested.as_ref().is_some_and(|model| model.provider != account.provider) {
            intent.requested = None;
            intent.requested_reasoning_effort = None;
        }
        if account.provider != task.account_key().provider || intent.requested.is_none() { intent.allow_model_substitution = true; }
        intent.account = Some(account.clone());
        if intent.effective.as_ref().is_some_and(|effective| effective.model.provider != account.provider) { intent.effective = None; }
        let status = self.accounts.iter().find(|status| AccountKey::new(status.provider, status.id.clone()) == *account);
        intent.paid_account_grant = (matches!(actor, Actor::User { .. }) && status.is_some_and(|status| matches!(status.billing, AccountBilling::PayPerUse))).then(|| account.clone());
        let mut resolving = intent.clone();
        if resolving.requested.is_none() && resolving.allow_model_substitution { resolving.requested = intent.effective.as_ref().map(|effective| effective.model.clone()); }
        let decision = crate::model_selection::resolve_selection(&resolving, &self.model_profiles, &self.model_catalogs, std::slice::from_ref(account))
            .map_err(|reasons| DomainError::new("model_unavailable", format!("No model of {} can run this task: {}", account.wire_id(), reasons.join("; "))))?;
        let ran = self.lifecycle_of(task_id).is_ok_and(|lifecycle| lifecycle.execution().is_some());
        let pending = ran.then(|| PendingTaskSelection { account: account.clone(), model: decision.selection, reason: MOVE_REASON.into() });
        Ok((intent, pending))
    }

    pub(super) fn move_task_guard(&self, actor: &Actor, task_id: &TaskId, expected_selection: &str, account: &AccountKey) -> Result<(TaskSelection, Option<PendingTaskSelection>)> {
        if !matches!(actor, Actor::System) { user(actor)?; }
        self.not_ending(task_id)?;
        let task = self.active(task_id)?;
        if task.pending_selection.is_some() { return fail("selection_pending", "This task is already changing its model or account"); }
        if self.current_account(task).as_ref() == Some(account) { return fail("same_account", "The task already runs on this account"); }
        let status = self.accounts.iter().find(|status| AccountKey::new(status.provider, status.id.clone()) == *account)
            .ok_or_else(|| DomainError::new("not_found", "The destination account is not connected"))?;
        if status.state != AccountState::SignedIn { return fail("account_unavailable", "The destination account is off or signed out"); }
        if matches!(status.billing, AccountBilling::PayPerUse) && !matches!(actor, Actor::User { .. }) {
            return fail("paid_authorization_required", "Only the user moves a task to an account paid per use");
        }
        if account.provider == crate::accounts::Provider::Codex {
            let ran = task.started || task.agent_id.is_some();
            match task.selection.as_ref().and_then(|selection| selection.native_tool_denials.as_deref()) {
                Some([]) => {}
                Some(_) => return fail("unsupported_tool_policy", "Codex cannot enforce this task's tool restrictions"),
                None if ran => return fail("unsupported_tool_policy", "The task's tool policy must be verified before moving it to Codex"),
                None => {}
            }
        }
        let moved = self.moved_selection(actor, task_id, account)?;
        if self.task_selection_stamp(task_id).as_deref() != Some(expected_selection) { return fail("stale_selection", "The task's selection or execution changed; refresh before moving it"); }
        Ok(moved)
    }
}
