use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verb {
    Ans,
    Read,
    Accept,
    Changes,
    Merge,
    Cancel,
    Reopen,
    Archive,
    Unarchive,
    ArchiveDone,
    Withdraw,
    Clarify,
    Ctx,
    Reply,
    AskPr,
    Conflicts,
    Msg,
    Relaunch,
    Close,
    Interrupt,
    OrchRetry,
    OrchAck,
    OrchStop,
    SchedRun,
    SchedToggle,
    SchedDelete,
    SchedSave,
    NewRequest,
    /// Close the delivered PR on GitHub without merging it, and cancel its task.
    CloseUnmerged,
    /// Take back an answer to one of the questions asked together with this one.
    Retake,
    /// Pause a working node, or take a queued task out of the queue.
    Pause,
    /// Resume a paused node or task at once, even over the limit.
    Resume,
    /// Set how many nodes work at once on this Mac: `auto` or a number, as `option_id`.
    SetLimit,
    /// Set a task's priority: `high`, `normal` or `low`, as `option_id`.
    SetPriority,
    /// Remove a rule «allow always» added to a repo's local settings on this Mac.
    RemoveRule,
    /// Take back what the user allowed in a task from one of its permission cards, named in extra.
    RevokeGrant,
    /// End Claude's process for a node whose turn stayed open after Claude accepted its interrupt.
    ForceStop,
    /// Have Claude forget, and revoke, its sign-in to an MCP server, named by its target.
    McpSignOut,
    /// Check the MCP servers of a project no node runs in, named by its target.
    McpCheck,
    /// Turn an account on or off, take it away, or move it in the order, named by its id.
    AccountEnable,
    AccountDisable,
    AccountRemove,
    AccountUp,
    AccountDown,
    /// Set an account's weight: a number, or `auto` for its plan's, as `option_id`.
    AccountWeight,
    /// Set how new work is spread over the accounts: `margin` or `chain`, as `option_id`.
    AccountPolicy,
    /// Set how this Mac merges PRs: `squash`, `merge` or `rebase`, as `option_id`.
    SetMergeMethod,
    /// Whether a node whose account reaches its limit goes on with another: `on` or `off`, as
    /// `option_id`.
    AccountMoves,
}
impl Verb {
    pub const ALL: [Self; 48] = [
        Self::Ans,
        Self::Read,
        Self::Accept,
        Self::Changes,
        Self::Merge,
        Self::Cancel,
        Self::Reopen,
        Self::Archive,
        Self::Unarchive,
        Self::ArchiveDone,
        Self::Withdraw,
        Self::Clarify,
        Self::Ctx,
        Self::Reply,
        Self::AskPr,
        Self::Conflicts,
        Self::Msg,
        Self::Relaunch,
        Self::Close,
        Self::Interrupt,
        Self::OrchRetry,
        Self::OrchAck,
        Self::OrchStop,
        Self::SchedRun,
        Self::SchedToggle,
        Self::SchedDelete,
        Self::SchedSave,
        Self::NewRequest,
        Self::CloseUnmerged,
        Self::Retake,
        Self::Pause,
        Self::Resume,
        Self::SetLimit,
        Self::SetPriority,
        Self::RemoveRule,
        Self::RevokeGrant,
        Self::ForceStop,
        Self::McpSignOut,
        Self::McpCheck,
        Self::SetMergeMethod,
        Self::AccountEnable,
        Self::AccountDisable,
        Self::AccountRemove,
        Self::AccountUp,
        Self::AccountDown,
        Self::AccountWeight,
        Self::AccountPolicy,
        Self::AccountMoves,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ans => "ans",
            Self::Read => "read",
            Self::Accept => "accept",
            Self::Changes => "changes",
            Self::Merge => "merge",
            Self::Cancel => "cancel",
            Self::Reopen => "reopen",
            Self::Archive => "archive",
            Self::Unarchive => "unarchive",
            Self::ArchiveDone => "archive_done",
            Self::Withdraw => "withdraw",
            Self::Clarify => "clarify",
            Self::Ctx => "ctx",
            Self::Reply => "reply",
            Self::AskPr => "ask_pr",
            Self::Conflicts => "conflicts",
            Self::Msg => "msg",
            Self::Relaunch => "relaunch",
            Self::Close => "close",
            Self::Interrupt => "interrupt",
            Self::OrchRetry => "orch_retry",
            Self::OrchAck => "orch_ack",
            Self::OrchStop => "orch_stop",
            Self::SchedRun => "sched_run",
            Self::SchedToggle => "sched_toggle",
            Self::SchedDelete => "sched_delete",
            Self::SchedSave => "sched_save",
            Self::NewRequest => "new_request",
            Self::CloseUnmerged => "close_unmerged",
            Self::Retake => "retake",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::SetLimit => "set_limit",
            Self::SetPriority => "set_priority",
            Self::RemoveRule => "remove_rule",
            Self::RevokeGrant => "revoke_grant",
            Self::ForceStop => "force_stop",
            Self::McpSignOut => "mcp_sign_out",
            Self::McpCheck => "mcp_check",
            Self::SetMergeMethod => "set_merge_method",
            Self::AccountEnable => "account_enable",
            Self::AccountDisable => "account_disable",
            Self::AccountRemove => "account_remove",
            Self::AccountUp => "account_up",
            Self::AccountDown => "account_down",
            Self::AccountWeight => "account_weight",
            Self::AccountPolicy => "account_policy",
            Self::AccountMoves => "account_moves",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|verb| verb.as_str() == value)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionId {
    pub verb: Verb,
    pub subject: String,
    pub revision: u64,
    pub extra: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActionIdError {
    Invalid,
    TooLong,
}
impl ActionId {
    pub fn new(
        verb: Verb,
        subject: impl Into<String>,
        revision: u64,
        extra: Option<String>,
    ) -> Result<Self, ActionIdError> {
        let id = Self {
            verb,
            subject: subject.into(),
            revision,
            extra,
        };
        id.validate()?;
        Ok(id)
    }
    pub fn parse(value: &str) -> Result<Self, ActionIdError> {
        if value.len() > 320 {
            return Err(ActionIdError::TooLong);
        }
        let (verb, tail) = value.split_once(':').ok_or(ActionIdError::Invalid)?;
        let (subject, revision_extra) = tail.split_once('@').ok_or(ActionIdError::Invalid)?;
        let (revision, extra) = match revision_extra.split_once('~') {
            Some((revision, extra)) => (revision, Some(extra.to_owned())),
            None => (revision_extra, None),
        };
        if revision.is_empty() || !revision.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ActionIdError::Invalid);
        }
        let revision = revision.parse().map_err(|_| ActionIdError::Invalid)?;
        Self::new(
            Verb::parse(verb).ok_or(ActionIdError::Invalid)?,
            subject,
            revision,
            extra,
        )
    }
    fn validate(&self) -> Result<(), ActionIdError> {
        if self.to_string().len() > 320 {
            return Err(ActionIdError::TooLong);
        }
        let valid = |s: &str| {
            !s.is_empty()
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        };
        let orchestrator_action = self.subject == "orchestrator"
            && matches!(self.verb, Verb::OrchRetry | Verb::OrchAck | Verb::OrchStop);
        if self.subject != "view" && !orchestrator_action && !super::id::valid(&self.subject) {
            return Err(ActionIdError::Invalid);
        }
        if self.extra.as_deref().is_some_and(|extra| !valid(extra)) {
            return Err(ActionIdError::Invalid);
        }
        Ok(())
    }
}
impl fmt::Display for ActionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}@{}",
            self.verb.as_str(),
            self.subject,
            self.revision
        )?;
        if let Some(extra) = &self.extra {
            write!(f, "~{extra}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_subjects_round_trip_and_unsafe_delimiters_fail() {
        for subject in ["hf-weekly-2026-09-30", "a98d034b-249e-4487-a89b-57de38c26372",
            "github.com/bitomule/alondra#154"] {
            let id = ActionId::new(Verb::Msg, subject, 117, None).unwrap();
            assert_eq!(ActionId::parse(&id.to_string()).unwrap(), id);
        }
        for subject in ["../secret", "github.com/../repo#154", "a@12", "a~b", "a:b"] {
            assert!(ActionId::new(Verb::Msg, subject, 117, None).is_err());
        }
    }
}

/// The option id that carries why the user asks a card's author to say it again.
pub fn clarification_name(reason: crate::domain::ClarificationReason) -> &'static str {
    use crate::domain::ClarificationReason::*;
    match reason {
        Unclear => "unclear",
        Contradicts => "contradicts",
        MissingContext => "missing_context",
    }
}
pub fn clarification_reason(name: &str) -> Option<crate::domain::ClarificationReason> {
    use crate::domain::ClarificationReason::*;
    match name {
        "unclear" => Some(Unclear),
        "contradicts" => Some(Contradicts),
        "missing_context" => Some(MissingContext),
        _ => None,
    }
}

/// The `extra` of an accept action on a result that proposes a plan.
pub const IMPLEMENT_PLAN: &str = "implement";
pub const ARCHIVE_PLAN: &str = "archive";

/// What an accept action's `extra` chooses for a result's proposed plan.
pub fn plan_choice(extra: &str) -> Option<crate::domain::PlanChoice> {
    match extra {
        IMPLEMENT_PLAN => Some(crate::domain::PlanChoice::Implement),
        ARCHIVE_PLAN => Some(crate::domain::PlanChoice::Archive),
        _ => None,
    }
}
