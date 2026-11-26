//! A task worked in stages: an author plans it, an implementer builds the approved plan and a
//! validator checks it, each a new agent instance with the model of its own role, one at a time.
//! A task without stages runs as a single execution, with no role.
use crate::{AgentId, InteractionId, Revision, TaskModel};
use alondra_core::accounts::{AccountId, Provider};
use alondra_core::model_selection::{ModelProfile, TaskSelection};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The part an execution of a staged task plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionRole {
    Author,
    Implementer,
    Validator,
}

impl ExecutionRole {
    pub const ALL: [ExecutionRole; 3] = [Self::Author, Self::Implementer, Self::Validator];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Author => "author",
            Self::Implementer => "implementer",
            Self::Validator => "validator",
        }
    }

    /// The model profile of the user's settings an execution of this role starts with.
    pub fn profile(self) -> ModelProfile {
        match self {
            Self::Author => ModelProfile::Design,
            Self::Implementer => ModelProfile::Implementation,
            Self::Validator => ModelProfile::Review,
        }
    }
}

/// The input the new execution of `role` starts from, after the context Alondra writes for it.
pub fn stage_input(role: ExecutionRole) -> String {
    format!("Carry on with this Alondra task as its {}, from the context above.", role.as_str())
}

/// Every agent tool whose use depends on the role of the execution that calls it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RoleTool {
    RequestPlanMode,
    Plan,
    RegisterPr,
    PresentPullRequest,
    PostMergeWork,
    RecordVerdict,
    ProposeSpecs,
    ProposeAmendment,
    ProposeDecision,
    RequestSpecChange,
    ReadyForValidation,
    RecordValidation,
    EndValidation,
}

impl RoleTool {
    pub fn name(self) -> &'static str {
        match self {
            Self::RequestPlanMode => "request_plan_mode",
            Self::Plan => "ExitPlanMode",
            Self::RegisterPr => "register_pr",
            Self::PresentPullRequest => "present_result",
            Self::PostMergeWork => "post_merge_work",
            Self::RecordVerdict => "record_verdict",
            Self::ProposeSpecs => "propose_specs",
            Self::ProposeAmendment => "propose_amendment",
            Self::ProposeDecision => "propose_decision",
            Self::RequestSpecChange => "request_spec_change",
            Self::ReadyForValidation => "ready_for_validation",
            Self::RecordValidation => "record_validation",
            Self::EndValidation => "end_validation",
        }
    }
}

impl ExecutionRole {
    /// Whether an execution of this role may use `tool`. The author plans, writes the specs and
    /// their changes, and never delivers code; the implementer builds and delivers, and asks for a
    /// change of spec instead of writing one; the validator checks and delivers, never plans.
    pub fn allows(self, tool: RoleTool) -> bool {
        match (self, tool) {
            (Self::Author, RoleTool::RequestPlanMode | RoleTool::Plan | RoleTool::ProposeSpecs | RoleTool::ProposeAmendment | RoleTool::ProposeDecision) => true,
            (Self::Author, _) => false,
            (Self::Implementer, RoleTool::RecordVerdict | RoleTool::ProposeSpecs | RoleTool::ProposeAmendment | RoleTool::ProposeDecision
                | RoleTool::RecordValidation | RoleTool::EndValidation) => false,
            (Self::Implementer, _) => true,
            (Self::Validator, RoleTool::RequestPlanMode | RoleTool::Plan | RoleTool::RegisterPr | RoleTool::RecordVerdict
                | RoleTool::ProposeSpecs | RoleTool::ProposeAmendment | RoleTool::ProposeDecision | RoleTool::RequestSpecChange
                | RoleTool::ReadyForValidation) => false,
            (Self::Validator, _) => true,
        }
    }

    /// What the role does instead, said to the agent whose call it refuses.
    pub fn refusal_fix(self, tool: RoleTool) -> String {
        let instead = match self {
            Self::Author => "You are the author of this task: you plan it and write its specs, and once the user approves the plan a new agent implements it. Present the plan through plan mode, or ask the user; never register or deliver a pull request.",
            Self::Implementer => "You are the implementer of this task: build the approved plan and deliver it, or say it is ready with ready_for_validation when its specs need validating. Never write a spec or a decision: ask for the change with request_spec_change. Only the validator records validations.",
            Self::Validator => "You are the validator of this task: run its checks, record each with record_validation, and deliver their result, or send the work back with end_validation. Never plan, write a spec, change its code or tests, or open a pull request.",
        };
        format!("{} is not available to the {} of a task. {instead}", tool.name(), self.as_str())
    }
}

/// Why a staged task changes role: what the next execution starts after.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HandoverCause {
    /// The user approved the author's plan on this card.
    PlanApproved { card: InteractionId },
    /// The implementer asked for this change of the approved specs: the author drafts it.
    SpecChangeRequested { amendment: String },
    /// The author drafted this change and it waits for the user: the implementer carries on.
    AmendmentProposed { amendment: String },
    /// The implementer said its work is ready: a validator checks it in this round.
    ReadyForValidation { round: u32 },
    /// The validator sent the work of this round back with its findings.
    ValidationFailed { round: u32 },
    /// The validator of this round could not run all of it: a new validator tries once more.
    ValidatorRetry { round: u32 },
    /// The user decided how the task goes on after this round failed the last time allowed.
    ValidationDecided { round: u32 },
    /// The user asked for changes, or the work needs something after its delivery: the validator
    /// delivered it, and an implementer does that work.
    DeliveryReturned,
}

impl HandoverCause {
    /// A new execution starts even when the one working already plays the role it hands over to.
    pub fn renews(&self) -> bool {
        matches!(self, Self::ValidatorRetry { .. })
    }
}

/// A change of role the task owes: its current execution ends once it rests with nothing
/// pending, and a new execution of `to` takes the task over from what Alondra writes for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageHandover {
    pub to: ExecutionRole,
    pub cause: HandoverCause,
    /// The selection change that carries it out, once Alondra started it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<String>,
    /// The changes that ended without the new execution starting, since the user last asked.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub attempts: u8,
    /// Its start was refused for good: it waits for the user to try again or cancel the task.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub refused: bool,
}

fn is_zero(value: &u8) -> bool {
    *value == 0
}

/// How many changes of role Alondra tries before it asks the user.
pub const MAX_STAGE_ATTEMPTS: u8 = 3;

impl StageHandover {
    /// Whether Alondra starts the change now: none runs, and its start is not waiting for the user.
    pub fn due(&self) -> bool {
        self.transition.is_none() && !self.refused
    }

    /// Whether an execution playing `role` owes this change: it plays another role, or a new
    /// execution of the same role takes over.
    pub fn owed_by(&self, role: Option<ExecutionRole>) -> bool {
        role != Some(self.to) || self.cause.renews()
    }
}

/// A plan the user approved, as Alondra records it on the task.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanApproval {
    pub card: InteractionId,
    pub revision: Revision,
    /// The execution that presented it, and its role when the task has stages.
    pub generation: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ExecutionRole>,
}

/// One execution of a task, as its detail shows it: who ran it, with which model, in which role.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRun {
    pub generation: u64,
    pub agent: AgentId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ExecutionRole>,
    #[serde(default, skip_serializing_if = "Provider::is_claude")]
    pub provider: Provider,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<AccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<TaskModel>,
}

/// The most executions a task keeps for its detail; the oldest go first.
pub const MAX_TASK_RUNS: usize = 64;
/// The most plan approvals a task keeps; the oldest go first.
pub const MAX_PLAN_APPROVALS: usize = 32;

/// What a task knows of its stages and executions. A task created without stages has
/// `staged` false: its executions carry no role and none hands over to another.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStages {
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub staged: bool,
    /// Why the orchestrator chose stages for the task, or chose none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The selection each role starts with, kept apart from the task's own: what the user chose
    /// by hand for the task, with the profile of the role.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selections: BTreeMap<ExecutionRole, TaskSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub handover: Option<StageHandover>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub approvals: Vec<PlanApproval>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runs: Vec<TaskRun>,
}

impl TaskStages {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// The role of the task's current execution; none when the task has no stages.
    pub fn role(&self) -> Option<ExecutionRole> {
        if !self.staged {
            return None;
        }
        self.runs.last().and_then(|run| run.role)
    }

    /// The role the task's next execution takes: the one it hands over to, else the one it has,
    /// else the author's for a staged task that never ran.
    pub fn next_role(&self) -> Option<ExecutionRole> {
        if !self.staged {
            return None;
        }
        self.handover.as_ref().map(|handover| handover.to).or_else(|| self.role()).or(Some(ExecutionRole::Author))
    }

    pub fn admitted(&mut self, run: TaskRun) {
        if self.handover.as_ref().is_some_and(|handover| Some(handover.to) == run.role) {
            self.handover = None;
        }
        self.runs.push(run);
        let over = self.runs.len().saturating_sub(MAX_TASK_RUNS);
        self.runs.drain(..over);
    }

    pub fn approved(&mut self, approval: PlanApproval) {
        self.approvals.push(approval);
        let over = self.approvals.len().saturating_sub(MAX_PLAN_APPROVALS);
        self.approvals.drain(..over);
    }

    /// The selections of every role for a task whose user chose `chosen` for it: what they chose
    /// by hand (model, effort, account) holds in every role, and the role's profile fills the rest.
    pub fn role_selections(chosen: &TaskSelection) -> BTreeMap<ExecutionRole, TaskSelection> {
        ExecutionRole::ALL
            .into_iter()
            .map(|role| {
                let mut selection = TaskSelection::automatic(role.profile());
                selection.requested = chosen.requested.clone();
                selection.requested_reasoning_effort = chosen.requested_reasoning_effort.clone();
                selection.allow_model_substitution = chosen.requested.is_none() && chosen.allow_model_substitution;
                selection.substitution_was_edited = chosen.substitution_was_edited;
                selection.account = chosen.account.clone();
                selection.paid_account_grant = chosen.paid_account_grant.clone();
                selection.native_tool_denials = chosen.native_tool_denials.clone();
                (role, selection)
            })
            .collect()
    }
}
