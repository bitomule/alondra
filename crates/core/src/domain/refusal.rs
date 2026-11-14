//! Why the domain turned down something the user asked for, with what a person needs to read it:
//! the PR number, the card that blocks, the methods that are allowed. Its words live in one
//! catalog (`views::messages`), whose `match` has no fallback: a variant without text in both
//! languages does not compile.

use serde::{Deserialize, Serialize};

use super::MergeMethod;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Refusal {
    /// A question of the task has to be answered before this result.
    BlockedByQuestion { need: String },
    /// A task that reviews this one has not given its verdict yet.
    BlockedByTask { title: String },
    /// The task waits for another, so its plan is not approved until that wait ends.
    WaitsForTask { title: String },
    /// The PR builds on another that is still open.
    UpstreamPrOpen { number: u64 },
    /// The repository does not allow this way of merging.
    MethodNotAllowed { method: MergeMethod, allowed: Vec<MergeMethod> },
    /// The merge came without saying how.
    NoMethodChosen { number: u64 },
    /// The PR moved since the user looked.
    StaleHead { number: u64 },
    /// The PR has commits the presented result does not include.
    ResultBehindPr { number: u64 },
    /// Alondra is already merging it.
    MergeInProgress { number: u64 },
    /// The PR is closed or merged on GitHub.
    PrClosed { number: u64 },
    /// GitHub says the PR was merged already, so it cannot be closed without merging.
    PrAlreadyMerged { number: u64 },
    /// A newer result replaced the one the user looked at.
    NewerResult,
    PendingUserMessage,
    UserMessageUnavailable,
    AlreadyAnswered,
    Withdrawn,
    ClarificationPending,
    TaskNotActive,
    /// The task's PR is merged and the task is closing: it takes no more changes.
    TaskClosing,
    /// The thing changed while the user looked at it.
    Changed,
    /// The action is meant for the orchestrator or a node.
    NotForYou,
    NodeUnreachable { node: String },
    /// The node runs no turn and no work of its own that an interrupt could stop.
    NothingToInterrupt,
    /// Claude has not accepted an interrupt of the turn: it is interrupted before it is forced.
    InterruptNotAccepted,
    /// Alondra is already ending Claude's process for this turn.
    AlreadyStopping,
    DeviceNotApproved,
    StorageUnavailable,
    /// GitHub did not answer, so nothing was done there.
    GithubUnreachable,
    /// What the user asked for no longer exists or no longer applies.
    NoLongerAvailable,
    /// It was already as the user asked, so nothing changed.
    Unchanged,
    /// The provider's own sign-in on this Mac is never removed.
    DefaultAccountStays { name: String },
    /// Every other account is off: this one stays on.
    LastAccountOn { name: String },
    /// Open tasks still run on the account.
    AccountInUse { name: String },
}

/// Every code the domain gives a refusal with no context of its own. The test of this module
/// holds the list to the codes `rules.rs` can produce.
pub const INTERNAL_CODES: &[&str] = &[
    "active_account_immutable",
    "invalid_input", "not_found", "forbidden", "post_merge_running", "worktrees_open", "conflict", "invalid_action_for_kind", "acceptance_stale",
    "stack_not_ready", "dependency_cycle", "task_not_active", "question_already_answered", "stale_revision",
    "stale_head", "stale_generation", "pr_mismatch", "payload_too_large", "not_assigned", "dependency_not_done",
    "unknown_target", "pr_not_open", "pr_being_read", "not_closing", "merge_not_confirmed", "idempotency_conflict",
    "delivery_uncertain", "blocked", "withdrawn", "task_closing", "subject_already_linked", "stale_epoch",
    "request_title_required", "pr_owned_by_other_task", "pr_missing", "pr_needs_final", "no_task", "incompatible_protocol",
    "dependency_not_delivered", "clarification_pending", "blocking_open", "blocking_interaction",
    "unanswered_questions", "ask_already_open", "already_withdrawn", "already_answered", "agent_used", "agent_busy",
    "maintenance_request", "outcome_card", "launch_plan_required", "too_many_suggestions", "suggestion_needs_decision",
    "plan_without_checklist", "plan_checklist_too_long", "checklist_not_final", "checklist_without_plan",
    "checklist_unknown_item", "checklist_without_evidence", "evidence_unknown_attachment", "evidence_unknown_pr",
    "checklist_incomplete", "usage_is_not_a_stop", "plan_choice_required", "approved_plan_has_task", "fold_target_over", "fold_request_has_tasks", "request_has_tasks",
    "plan_review_pending", "plan_review_not_found", "not_in_plan_review", "plan_review_settled",
    "plan_items_unknown", "plan_items_take_all", "plan_items_unavailable", "task_waiting", "continuation_request",
    "model_catalog_changed", "model_catalog_unavailable", "stall_needs_outcome", "stall_settled", "stall_limit", "plan_refused_needs_outcome",
];

impl Refusal {
    /// The refusal a code means when the domain gave none with context. Codes only a node can
    /// reach read as "no longer available" to a person.
    pub fn for_code(code: &str) -> Self {
        match code {
            "stale_revision" | "conflict" | "stale_generation" | "stale_epoch" | "plan_approve_or_change" => Self::Changed,
            "acceptance_stale" => Self::NewerResult,
            "pending_user_message" | "pending_user_reply" => Self::PendingUserMessage,
            "message_receipt_unavailable" => Self::UserMessageUnavailable,
            "question_already_answered" | "already_answered" => Self::AlreadyAnswered,
            "withdrawn" | "already_withdrawn" => Self::Withdrawn,
            "clarification_pending" => Self::ClarificationPending,
            "task_not_active" | "task_ending" | "not_assigned" => Self::TaskNotActive,
            "task_closing" => Self::TaskClosing,
            "forbidden" => Self::NotForYou,
            "storage_unavailable" => Self::StorageUnavailable,
            "unchanged" => Self::Unchanged,
            _ => Self::NoLongerAvailable,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every code `rules.rs` can produce is either one `for_code` gives its own meaning or one
    /// the catalog lists as only reachable by a node; a new code must be put in one of them.
    #[test]
    fn every_code_the_rules_produce_is_known_to_the_refusal_catalog() {
        let source = [include_str!("rules.rs"), include_str!("rules/plan_review.rs"), include_str!("checklist.rs")].concat();
        let mut found = std::collections::BTreeSet::new();
        for marker in ["fail(\"", "refuse(\"", "DomainError::new(\"", "fail(\n", "Some((\""] {
            for part in source.split(marker).skip(1) {
                let code: String = if marker.ends_with('\n') {
                    part.trim_start().strip_prefix('"').map(|rest| rest.chars().take_while(|c| *c != '"').collect()).unwrap_or_default()
                } else {
                    part.chars().take_while(|c| *c != '"').collect()
                };
                if !code.is_empty() && code.chars().all(|c| c.is_ascii_lowercase() || c == '_') {
                    found.insert(code);
                }
            }
        }
        assert!(found.len() > 20, "the scan found {found:?}");
        let unknown: Vec<_> = found
            .iter()
            .filter(|code| !INTERNAL_CODES.contains(&code.as_str()) && Refusal::for_code(code) == Refusal::NoLongerAvailable)
            .collect();
        assert!(unknown.is_empty(), "codes with no place in the refusal catalog: {unknown:?}");
    }
}
