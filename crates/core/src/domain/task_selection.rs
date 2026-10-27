use super::*;
use crate::accounts::{AccountBilling, AccountKey, AccountState};
use crate::model_selection::{PendingTaskSelection, TaskSelection};

fn scoped_code_cell(turn: &str, work: &str, generation: u64) -> bool {
    let prefix = format!("codex:{generation}:");
    let Some((process, kind)) = work.strip_prefix(&prefix).and_then(|rest| rest.split_once(':')) else { return false };
    let Some(identity) = kind.strip_prefix("cell:") else { return false };
    let Ok((thread, origin, call)) = serde_json::from_str::<(String, String, String)>(identity) else { return false };
    !process.is_empty() && !thread.is_empty() && !origin.is_empty() && !call.is_empty()
        && turn == format!("{prefix}{process}:turn:{origin}")
}

impl DomainState {
    pub fn has_live_native_code_cells(&self, task_id: &TaskId) -> bool {
        if self.tasks.get(task_id).is_none_or(|task| task.provider != crate::accounts::Provider::Codex) { return false }
        let Ok(lifecycle) = self.lifecycle_of(task_id) else { return false };
        let Some(execution) = lifecycle.execution() else { return false };
        let mut live = std::collections::BTreeSet::new();
        for record in self.lifecycle_journal.get(task_id).into_iter().flatten() {
            match record {
                LifecycleRecord::Admitted { .. } | LifecycleRecord::HostEnded { .. } => live.clear(),
                LifecycleRecord::WorkStarted { turn, work } if scoped_code_cell(turn, work, execution.generation()) => { live.insert(work.clone()); }
                LifecycleRecord::WorkEnded { work, .. } => { live.remove(work); }
                _ => {}
            }
        }
        !live.is_empty()
    }

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
        if task.selection.as_ref().and_then(|selection| selection.effective.as_ref()).is_some_and(|effective| effective.model != pending.model.model)
            && self.has_live_native_code_cells(task_id) {
            return fail("native_code_cells_active", "Finish the running native code cells before changing this model; their continuation will use the current model");
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

#[cfg(test)]
mod cell_scope_tests {
    use super::scoped_code_cell;
    #[test]
    fn only_the_canonical_cell_kind_in_the_current_execution_is_a_model_fence() {
        let cell = r#"codex:4:process:cell:["thread","origin","call:cell:fake"]"#;
        assert!(scoped_code_cell("codex:4:process:turn:origin", cell, 4));
        assert!(!scoped_code_cell("codex:4:process:turn:other", cell, 4));
        assert!(!scoped_code_cell("codex:4:other:turn:origin", cell, 4));
        assert!(!scoped_code_cell("codex:4:process:turn:origin", cell, 5));
        assert!(!scoped_code_cell("codex:4:process:turn:origin", r#"codex:4:process:work:item:cell:["thread","origin","call"]"#, 4));
        assert!(!scoped_code_cell("codex:4:process:turn:origin", "codex:4:process:cell:untrusted code", 4));
    }
}
