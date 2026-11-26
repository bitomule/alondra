//! The one guard between a task in stages and its delivery. Alondra reads each commit a delivery
//! carries with git and the spec engine and records what it read as a [`CommitReading`]: facts,
//! never a decision. Whatever delivers the task (its node, or Alondra delivering its open pull
//! request for it), the lifecycle records the delivery only when [`DomainState::delivery_check`]
//! says every delivered commit is read and covered by what a validator recorded, each at the
//! fingerprint Alondra read.
use crate::validation::{RoundState, ValidationItem, ValidationKind, ValidationState, ValidationVerdict, MAX_ROUND_ITEMS, MAX_SCOPE_FILES};
use crate::{AmendmentState, DeliveryTarget, DomainError, DomainState, ExecutionRole, Task, TaskId};
use serde::{Deserialize, Serialize};

/// The most commits whose reading a task keeps; the oldest go first.
pub const MAX_READINGS: usize = 16;
/// The most problems of the spec engine a reading keeps.
pub const MAX_READING_PROBLEMS: usize = 50;

/// One entry of a commit's ledger that differs from where its branch leaves the base branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub subject: String,
    pub kind: ValidationKind,
    pub inputs: String,
    pub verdict: ValidationVerdict,
}

/// A spec, boundary, decision or settings file a commit holds otherwise than its base or the user
/// approved it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecFileChange {
    pub path: String,
    /// The user approved a version of it, and the commit holds another.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub approved: bool,
}

/// What Alondra read of one commit of a task with git and the spec engine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitReading {
    pub commit: String,
    /// Read at the task's worktree, for a delivery that carries no pull request.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub worktree: bool,
    /// The project uses specs on that commit.
    pub specs: bool,
    /// What the spec engine lists as not validated there, each at its fingerprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub owed: Vec<ValidationItem>,
    /// What keeps the spec engine from reading it clean.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
    /// The ledger entries the branch changed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ledger: Vec<LedgerEntry>,
    /// Ledger files the branch changed that no entry of the engine explains.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stray: Vec<String>,
    /// The commit the implementer handed over that `changed` counts from, when the task had a round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<String>,
    /// Files other than the ledger that changed since `since`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changed: Vec<String>,
    /// The commit's base Alondra compared it with; only a reading that says why the commit could
    /// not be read has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// The commit GitHub reports the pull request's base branch at, which `base` was computed
    /// against; a commit of a pull request is never compared with what the repository's own
    /// references say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<String>,
    /// The base branch GitHub reported for the pull request when Alondra read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_base: Option<String>,
    /// The repository's default branch, as GitHub reported it then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_branch: Option<String>,
    /// Spec files the commit changed from its base without the user's approval, turning specs off
    /// included, read on the commit whatever it says of specs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spec_changes: Vec<SpecFileChange>,
}

impl CommitReading {
    /// Whether it holds no more than a task keeps of one commit.
    pub fn bounded(&self) -> bool {
        self.owed.len() <= MAX_ROUND_ITEMS && self.ledger.len() <= MAX_ROUND_ITEMS && self.stray.len() <= MAX_SCOPE_FILES
            && self.changed.len() <= MAX_SCOPE_FILES && self.problems.len() <= MAX_READING_PROBLEMS && self.spec_changes.len() <= MAX_SCOPE_FILES
    }
}

/// Where a delivery of a task in stages stands against its validation. Only `NotGated` and
/// `Validated` let the lifecycle record it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeliveryCheck {
    /// The task is not in stages, or no delivered commit is of a project that uses specs.
    NotGated,
    Validated,
    /// Alondra has no current reading of these commits.
    Unread(Vec<String>),
    /// A pull request targets a base branch other than the one a task in stages delivers to: the
    /// repository's default branch, or the branch of the task it is stacked on.
    UnexpectedBase { number: u64, base: String, expected: Vec<String> },
    /// A delivered commit is already on its pull request's base branch: nothing in it is the
    /// task's work, and nothing in it was compared with anything.
    OnBase(Vec<String>),
    /// A delivered commit holds spec files otherwise than its base or the user approved them.
    SpecsDiffer { unapproved: Vec<String>, other: Vec<String> },
    /// A change of the specs still waits for its author or the user.
    SpecChangeOpen,
    /// A validator checks the work now, and someone else delivers it.
    RoundOpen,
    /// Validations of the open round its validator found not met.
    Unmet(Vec<String>),
    /// Validations of the open round its validator has not recorded.
    RoundOwed(Vec<String>),
    /// What the spec engine lists as not validated on a delivered commit, or cannot read.
    Listed { commit: String, items: Vec<ValidationItem>, problems: Vec<String> },
    /// Ledger entries of a delivered commit no validator recorded with that fingerprint and verdict.
    Unrecorded { commit: String, items: Vec<ValidationItem>, stray: Vec<String> },
    /// Files other than the ledger changed after the commit the implementer handed over.
    ChangedAfterValidation(Vec<String>),
}

fn listed<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    items.into_iter().take(20).cloned().collect::<Vec<_>>().join(", ")
}

fn named(items: &[ValidationItem]) -> Vec<String> {
    items.iter().map(|item| format!("{} ({})", item.subject, item.kind.as_str())).collect()
}

impl DeliveryCheck {
    pub fn admits(&self) -> bool {
        matches!(self, Self::NotGated | Self::Validated)
    }

    /// Whether it is about the open round rather than a delivered commit.
    pub fn of_round(&self) -> bool {
        matches!(self, Self::RoundOpen | Self::Unmet(_) | Self::RoundOwed(_))
    }

    /// Why the delivery is refused, said to whoever delivers in `role`.
    pub fn refusal(&self, role: Option<ExecutionRole>) -> Option<DomainError> {
        let validator = role == Some(ExecutionRole::Validator);
        let (code, message) = match self {
            Self::NotGated | Self::Validated => return None,
            Self::Unread(commits) => ("validation_unread", format!(
                "Nothing was presented: Alondra has not read the delivered commit ({}) with the spec engine yet. Present the result again: Alondra reads it first.", listed(commits))),
            Self::UnexpectedBase { number, base, expected } => ("pr_base_unexpected", format!(
                "Nothing was presented: pull request #{number} targets {base}, and a task in stages delivers to {}. Point the pull request at that branch and present it again.",
                if expected.is_empty() { "the repository's default branch".to_owned() } else { expected.join(" or ") })),
            Self::OnBase(commits) => ("commit_on_base", format!(
                "Nothing was presented: the delivered commit ({}) is already on its pull request's base branch as GitHub reports it, so nothing in it is the task's work. Push the task's commits to its pull request and present it again.",
                listed(commits))),
            Self::SpecsDiffer { unapproved, other } => ("specs_differ", {
                let mut parts = vec![];
                if !unapproved.is_empty() {
                    parts.push(format!("these spec files changed without the user's approval: {}", listed(unapproved)));
                }
                if !other.is_empty() {
                    parts.push(format!("these approved spec files are not committed exactly as approved: {}", listed(other)));
                }
                format!("Nothing was presented: on the delivered commit {}. In a project with specs only the user changes them, turning them off included: commit each file as it was on the base branch, or as Alondra wrote it when the user approved it; a change the work needs goes through request_spec_change (implementer) or a new plan.",
                    parts.join("; "))
            }),
            Self::SpecChangeOpen => ("spec_change_open",
                "Nothing was presented: a change of the specs still waits for its author or the user. The work is delivered once it is settled.".to_owned()),
            Self::RoundOpen => ("validation_open", "A validator checks this work now: end your turn, it delivers once every check passes".to_owned()),
            Self::Unmet(items) => ("validation_unmet", format!(
                "These validations of the round are not met: {}. Send the work back with end_validation and your findings instead of delivering it", listed(items))),
            Self::RoundOwed(items) => ("validation_pending", format!(
                "These validations of the round are not recorded yet: {}. Run each and record it with record_validation before delivering", listed(items))),
            Self::Listed { items, problems, .. } if !validator => ("validation_required", format!(
                "Nothing was presented: the spec engine lists what is not validated on your commit ({}). In a task in stages a validator checks it: commit your work and call ready_for_validation.",
                listed(named(items).iter().chain(problems)))),
            Self::Listed { items, problems, .. } => ("validation_pending", format!(
                "Nothing was presented: alondra spec validate is not clean on the delivered commit ({}). Validate each, record it with record_validation, commit only the ledger and deliver again.",
                listed(named(items).iter().chain(problems)))),
            Self::Unrecorded { items, stray, .. } => ("ledger_unrecorded", format!(
                "Nothing was presented: the ledger holds entries no validator recorded in Alondra ({}). {}",
                listed(named(items).iter().chain(stray)),
                if validator { "Record each with record_validation, or remove the entry and validate it again." }
                else { "Only a validator records validations: remove those entries, commit your work and call ready_for_validation." })),
            Self::ChangedAfterValidation(paths) => ("changed_after_validation", format!(
                "Nothing was presented: since the commit the implementer handed over, files other than the ledger changed ({}). A validator never delivers code, tests or specs it did not receive: take those changes out, or end the round with end_validation, naming them, so the implementer takes them.",
                listed(paths))),
        };
        let mut error = DomainError::new(code, message);
        error.subject = Some("is_final".into());
        Some(error)
    }

    /// The commit a validator checks before this delivery can go out and what it checks there,
    /// when that is what it needs.
    pub fn to_validate(&self) -> Option<(String, Vec<ValidationItem>)> {
        let (commit, mut items) = match self {
            Self::Listed { commit, items, .. } | Self::Unrecorded { commit, items, .. } => (commit.clone(), items.clone()),
            _ => return None,
        };
        items.sort_by(|a, b| (&a.subject, a.kind, &a.inputs).cmp(&(&b.subject, b.kind, &b.inputs)));
        items.dedup();
        Some((commit, items))
    }
}

/// The commits a delivery carries: the head of each pull request it presents, or, without one,
/// the last commit Alondra read at the task's worktree, which it reads before every final delivery.
/// A task without a worktree delivers no commit.
fn delivered_commits(task: &Task, targets: &[DeliveryTarget]) -> Vec<(String, Option<crate::PrId>)> {
    let heads: Vec<(String, Option<crate::PrId>)> = targets.iter().filter_map(|target| match target {
        DeliveryTarget::PullRequest { pr_id, head } => Some((head.clone(), Some(pr_id.clone()))),
        _ => None,
    }).collect();
    if !heads.is_empty() {
        return heads;
    }
    task.validation.readings.iter().rev().find(|reading| reading.worktree).map(|reading| (reading.commit.clone(), None)).into_iter().collect()
}

impl DomainState {
    /// Where a delivery of `task_id` carrying `targets` stands against its validation.
    pub fn delivery_check(&self, task_id: &TaskId, targets: &[DeliveryTarget]) -> DeliveryCheck {
        let Some(task) = self.tasks.get(task_id).filter(|task| task.stages.staged) else { return DeliveryCheck::NotGated };
        let role = task.stages.role();
        let commits = delivered_commits(task, targets);
        let unread: Vec<String> = commits.iter().filter(|(commit, pr)| task.validation.reading_of(commit, pr.is_none()).is_none()).map(|(commit, _)| commit.clone()).collect();
        if !unread.is_empty() {
            return DeliveryCheck::Unread(unread);
        }
        for (commit, pr) in &commits {
            let (Some(pr), Some(reading)) = (pr.as_ref().and_then(|id| self.pull_requests.get(id)), task.validation.reading_of(commit, false)) else { continue };
            if !reading.problems.is_empty() {
                continue;
            }
            if reading.pr_base.as_deref() != Some(pr.base.as_str()) {
                return DeliveryCheck::Unread(vec![commit.clone()]);
            }
            let expected = self.expected_bases(task);
            if !expected.contains(&pr.base) {
                return DeliveryCheck::UnexpectedBase { number: pr.number, base: pr.base.clone(), expected };
            }
        }
        let read: Vec<&CommitReading> = commits.iter().filter_map(|(commit, pr)| task.validation.reading_of(commit, pr.is_none())).collect();
        let compared = |reading: &CommitReading| reading.base.is_some() && (reading.worktree || reading.tip.is_some());
        let uncompared: Vec<String> = read.iter().filter(|reading| reading.problems.is_empty() && !compared(reading)).map(|reading| reading.commit.clone()).collect();
        if !uncompared.is_empty() {
            return DeliveryCheck::Unread(uncompared);
        }
        let on_base: Vec<String> = read.iter()
            .filter(|reading| !reading.worktree && reading.problems.is_empty() && reading.base.as_deref() == Some(reading.commit.as_str()))
            .map(|reading| reading.commit.clone()).collect();
        if !on_base.is_empty() {
            return DeliveryCheck::OnBase(on_base);
        }
        let (unapproved, other): (Vec<&SpecFileChange>, Vec<&SpecFileChange>) = read.iter().flat_map(|reading| &reading.spec_changes).partition(|change| !change.approved);
        if !unapproved.is_empty() || !other.is_empty() {
            let paths = |changes: Vec<&SpecFileChange>| changes.into_iter().map(|change| change.path.clone()).collect();
            return DeliveryCheck::SpecsDiffer { unapproved: paths(unapproved), other: paths(other) };
        }
        let readings: Vec<&CommitReading> = read.into_iter().filter(|reading| reading.specs).collect();
        let spec_change_open = task.specs.amendments.iter().any(|a| matches!(a.state, AmendmentState::Requested | AmendmentState::Proposed));
        for reading in &readings {
            if !reading.owed.is_empty() || !reading.problems.is_empty() {
                return DeliveryCheck::Listed { commit: reading.commit.clone(), items: reading.owed.clone(), problems: reading.problems.clone() };
            }
            if spec_change_open {
                return DeliveryCheck::SpecChangeOpen;
            }
            let items: Vec<ValidationItem> = reading.ledger.iter()
                .filter(|entry| task.validation.record(&entry.subject, entry.kind, &entry.inputs).is_none_or(|record| record.verdict != entry.verdict))
                .map(|entry| ValidationItem { subject: entry.subject.clone(), kind: entry.kind, inputs: entry.inputs.clone() })
                .collect();
            if !items.is_empty() || !reading.stray.is_empty() {
                return DeliveryCheck::Unrecorded { commit: reading.commit.clone(), items, stray: reading.stray.clone() };
            }
            if role == Some(ExecutionRole::Validator) && let Some(round) = task.validation.current() {
                if reading.since.as_deref() != Some(round.implemented.as_str()) {
                    return DeliveryCheck::Unread(vec![reading.commit.clone()]);
                }
                if !reading.changed.is_empty() {
                    return DeliveryCheck::ChangedAfterValidation(reading.changed.clone());
                }
            }
        }
        if spec_change_open {
            return DeliveryCheck::SpecChangeOpen;
        }
        if let Some(round) = task.validation.open_round() {
            if role != Some(ExecutionRole::Validator) {
                return DeliveryCheck::RoundOpen;
            }
            let unmet: Vec<String> = round.items.iter().filter(|item| item.state == ValidationState::Unmet).map(|item| format!("{} ({})", item.subject, item.kind.as_str())).collect();
            if !unmet.is_empty() {
                return DeliveryCheck::Unmet(unmet);
            }
            let owed: Vec<String> = round.owed().map(|item| format!("{} ({})", item.subject, item.kind.as_str())).collect();
            if !owed.is_empty() {
                return DeliveryCheck::RoundOwed(owed);
            }
        }
        if readings.is_empty() && task.validation.open_round().is_none() {
            return DeliveryCheck::NotGated;
        }
        DeliveryCheck::Validated
    }

    /// The guard of the lifecycle's delivery: the delivery `delivery` of `task_id` is recorded
    /// only when its validation admits it.
    pub fn delivery_admitted(&self, task_id: &TaskId, delivery: &crate::InteractionId) -> Result<(), DomainError> {
        let Some(crate::InteractionPayload::Delivery { targets, .. }) = self.interactions.get(delivery).map(|i| &i.payload) else { return Ok(()) };
        let role = self.tasks.get(task_id).and_then(|task| task.stages.role());
        match self.delivery_check(task_id, targets).refusal(role) {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl DomainState {
    /// Alondra was about to deliver the open pull request of `task_id` for its node, and `check`
    /// does not admit it: nothing reaches the user, and the work goes to whoever moves it next. An
    /// unread commit waits for Alondra to read it and is delivered then; what a validator has not
    /// checked opens a round on that commit; code nobody validated, spec files changed without
    /// approval and a commit Alondra could not read go back to the implementer; an open round goes
    /// to a new validator. A change of the specs already waits for its author or the user.
    pub fn hold_unvalidated_delivery(&mut self, task_id: &TaskId, check: DeliveryCheck, result: Option<String>, lifecycles: &mut crate::Lifecycles) -> Result<(), DomainError> {
        use crate::validation::{RoundItem, ValidationRound};
        use crate::{HandoverCause, LifecycleRecord, OpenPrPending, StageHandover};
        let handover = |to: ExecutionRole, cause: HandoverCause| StageHandover { to, cause, transition: None, attempts: 0, refused: false };
        let task = self.tasks.get_mut(task_id).expect("the delivering task exists");
        task.open_pr = None;
        let next = match &check {
            DeliveryCheck::NotGated | DeliveryCheck::Validated => return Ok(()),
            DeliveryCheck::Unread(_) => {
                task.open_pr = Some(OpenPrPending::Deliver { result });
                return Ok(());
            }
            DeliveryCheck::SpecChangeOpen => return Ok(()),
            DeliveryCheck::RoundOpen | DeliveryCheck::Unmet(_) | DeliveryCheck::RoundOwed(_) => {
                let round = task.validation.current().map_or(0, |round| round.number);
                handover(ExecutionRole::Validator, HandoverCause::ValidatorRetry { round })
            }
            DeliveryCheck::ChangedAfterValidation(_) | DeliveryCheck::SpecsDiffer { .. } | DeliveryCheck::OnBase(_) | DeliveryCheck::UnexpectedBase { .. } => handover(ExecutionRole::Implementer, HandoverCause::DeliveryReturned),
            DeliveryCheck::Listed { items, .. } if items.is_empty() => handover(ExecutionRole::Implementer, HandoverCause::DeliveryReturned),
            DeliveryCheck::Listed { .. } | DeliveryCheck::Unrecorded { .. } => {
                let (commit, items) = check.to_validate().expect("listed or unrecorded");
                let number = task.validation.rounds.last().map_or(1, |round| round.number + 1);
                task.validation.push_round(ValidationRound {
                    number, head: commit.clone(), implemented: commit,
                    items: items.into_iter().take(MAX_ROUND_ITEMS).map(|item| RoundItem { subject: item.subject, kind: item.kind, inputs: item.inputs, state: ValidationState::Running, reason: None }).collect(),
                    scope: vec![], state: RoundState::Requested, generation: None, findings: None, retried: false,
                });
                handover(ExecutionRole::Validator, HandoverCause::ReadyForValidation { round: number })
            }
        };
        if task.stages.handover.is_some() {
            return Ok(());
        }
        let to = next.to;
        task.stages.handover = Some(next);
        let input = self.next_input(task_id);
        self.decide(lifecycles, task_id, LifecycleRecord::Told { input, text: Some(crate::stage_input(to)), answers: None })
    }
}

impl DomainState {
    /// The branches a pull request of `task`, in stages, may target: the repository's default
    /// branch as GitHub last reported it in a reading of the task, and, stacked, the branch of the
    /// task it is stacked on. Empty while Alondra has read neither.
    pub fn expected_bases(&self, task: &Task) -> Vec<String> {
        let mut expected: Vec<String> = task.validation.readings.iter().rev().find_map(|reading| reading.default_branch.clone()).into_iter().collect();
        if let Some(upstream) = task.stack.as_ref().and_then(|stack| self.tasks.get(&stack.upstream_task)) {
            expected.extend(upstream.worktrees.values().filter_map(|worktree| worktree.branch.clone()));
        }
        expected.sort();
        expected.dedup();
        expected
    }

    /// The spec files the user approved for `task`, and for the task it is stacked on: what its
    /// branch may hold otherwise than its base.
    pub fn allowed_specs(&self, task: &Task) -> crate::SpecFiles {
        let mut allowed = crate::SpecFiles::new();
        let upstream = task.stack.as_ref().and_then(|stack| self.tasks.get(&stack.upstream_task));
        for approved in upstream.into_iter().chain([task]).filter_map(|task| task.specs.approved.as_ref()) {
            allowed.extend(approved.allowed());
        }
        allowed
    }
}

impl crate::TaskValidation {
    /// What Alondra read of `commit`, the latest first.
    pub fn reading(&self, commit: &str) -> Option<&CommitReading> {
        self.readings.iter().rev().find(|reading| reading.commit == commit)
    }

    /// What Alondra read of `commit` as a pull request's commit, or as the worktree's.
    pub fn reading_of(&self, commit: &str, worktree: bool) -> Option<&CommitReading> {
        self.readings.iter().rev().find(|reading| reading.commit == commit && reading.worktree == worktree)
    }

    pub fn push_reading(&mut self, reading: CommitReading) {
        self.readings.retain(|old| !(old.commit == reading.commit && old.worktree == reading.worktree));
        self.readings.push(reading);
        let over = self.readings.len().saturating_sub(MAX_READINGS);
        self.readings.drain(..over);
    }
}
