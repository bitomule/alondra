//! A decision of the user on a Task that others wait for holds those that have not started, until
//! the orchestrator reviews the launch plan each was given before it. Only a settlement releases
//! one: never time, and never the end of what it waits for.
use super::*;
use serde::{Deserialize, Serialize};

/// The problem the user gets when the orchestrator left a plan review unsettled.
pub const PLAN_REVIEW_PROBLEM_PREFIX: &str = "problem-plan-review-";
pub const PLAN_REVIEW_REMIND: &str = "remind_again";
pub const PLAN_REVIEW_KEEP_PLANS: &str = "keep_plans";

/// What the plan-review problem card carries in its body: the review it is about, and the titles
/// every client words the card with.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlanReviewProblem {
    pub review_id: InteractionId,
    pub task_title: String,
    pub dependents: Vec<String>,
}
impl PlanReviewProblem {
    pub fn to_body(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
    pub fn from_body(body: &str) -> Option<Self> {
        serde_json::from_str(body).ok()
    }
}

impl DomainState {
    /// The Tasks that wait for `source`, directly or through others, and have not started: the
    /// plans they would start with were written before the decision.
    pub fn unstarted_dependents(&self, source: &TaskId) -> BTreeSet<TaskId> {
        let waits_on = |t: &Task, up: &TaskId| {
            t.dependencies.contains(up) || t.delivered_dependencies.contains(up) || t.stack.as_ref().is_some_and(|s| &s.upstream_task == up)
        };
        let mut seen = BTreeSet::from([source.clone()]);
        let mut frontier = vec![source.clone()];
        let mut found = BTreeSet::new();
        while let Some(up) = frontier.pop() {
            for t in self.tasks.values().filter(|t| waits_on(t, &up)) {
                if seen.insert(t.id.clone()) {
                    frontier.push(t.id.clone());
                    if Self::unstarted(t) {
                        found.insert(t.id.clone());
                    }
                }
            }
        }
        found
    }

    fn unstarted(t: &Task) -> bool {
        !t.started && t.agent_id.is_none() && !t.archived && t.final_presentation.is_none()
            && matches!(t.state, TaskState::NotStarted | TaskState::WaitingDependency)
    }

    /// A decision of the user holds the Task until the orchestrator reviews its plan.
    pub fn plan_review_pending(&self, task: &TaskId) -> bool {
        self.plan_reviews.values().any(|review| review.holds(task))
    }

    /// The reviews still holding `task`, oldest first.
    pub fn plan_reviews_holding<'a>(&'a self, task: &'a TaskId) -> impl Iterator<Item = (&'a InteractionId, &'a PlanReview)> + 'a {
        let mut holding: Vec<_> = self.plan_reviews.iter().filter(|(_, review)| review.holds(task)).collect();
        holding.sort_by_key(|(_, review)| review.at_revision);
        holding.into_iter()
    }

    fn decides(&self, i: &Interaction) -> bool {
        i.card.kind == CardKind::Decide && !matches!(self.holds.get(&i.id), Some(Hold::Permission | Hold::Plan | Hold::Elicitation))
    }

    /// The user's answer completed a node's decision on a Task others wait for: every dependent
    /// that has not started is held in the same commit, and the orchestrator is told.
    pub(super) fn hold_for_plan_review(&mut self, answered: &InteractionId, effects: &mut Vec<DomainEffect>) {
        let i = &self.interactions[answered];
        let (Author::Agent { .. }, Scope::Task { task_id }, Recipient::User) = (&i.author, &i.scope, &i.recipient) else { return };
        if !self.group_complete(i) {
            return;
        }
        let decisions: Vec<InteractionId> = self.group_members(i).into_iter().filter(|m| self.decides(m)).map(|m| m.id.clone()).collect();
        if decisions.is_empty() {
            return;
        }
        let task_id = task_id.clone();
        let dependents = self.unstarted_dependents(&task_id);
        if dependents.is_empty() {
            return;
        }
        self.plan_reviews.insert(answered.clone(), PlanReview {
            task_id: task_id.clone(),
            decisions,
            at_revision: self.revision + 1,
            dependents: dependents.into_iter().map(|t| (t, None)).collect(),
        });
        effects.push(DomainEffect::ReviewDependentPlans { review_id: answered.clone(), task_id });
    }

    /// No node is assigned to or starts a Task a decision holds.
    pub(super) fn not_held_for_plan_review(&self, task: &TaskId) -> Result<()> {
        let Some((_, review)) = self.plan_reviews_holding(task).next() else { return Ok(()) };
        let source = self.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
        refuse("plan_review_pending", "task_id", &format!(
            "Task {task} waits for the orchestrator to review its plan after the user's decision on «{source}»: it starts once settle_plan_review settles it"))
    }

    pub(super) fn settle_plan_review(&mut self, actor: &Actor, review_id: &InteractionId, task_id: &TaskId, outcome: &PlanReviewOutcome,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>) -> Result<()> {
        self.fence(actor)?;
        if !matches!(actor, Actor::Orchestrator { .. }) {
            return fail("forbidden", "Only the orchestrator settles a plan review");
        }
        let Some(review) = self.plan_reviews.get(review_id) else {
            return refuse("plan_review_not_found", "review_id", "No decision of the user is waiting for a plan review with this id: use the review_id the message gave you");
        };
        match review.dependents.get(task_id) {
            None => {
                let held: Vec<&str> = review.dependents.keys().map(|t| t.0.as_str()).collect();
                return refuse("not_in_plan_review", "task_id", &format!("This review holds {}, not {task_id}", held.join(", ")));
            }
            Some(Some(_)) => return refuse("plan_review_settled", "task_id", &format!("The plan review of {task_id} is already settled and its outcome is final")),
            Some(None) => {}
        }
        match outcome {
            PlanReviewOutcome::Unchanged { reason } | PlanReviewOutcome::Cancelled { reason } => nonempty(reason)?,
            PlanReviewOutcome::PlanChanged { launch, reason } => {
                nonempty(reason)?;
                if let Some(field) = launch.missing() {
                    return refuse("launch_plan_required", field, "a launch plan needs a name and a folder to start the node in");
                }
            }
            PlanReviewOutcome::KeptByUser =>
                return refuse("invalid_input", "outcome.kind", "Settle it with unchanged, plan_changed (with the new launch plan) or cancelled, each with its reason"),
        }
        self.settle_held(review_id, task_id, outcome.clone());
        if matches!(outcome, PlanReviewOutcome::Cancelled { .. }) && !self.task(task_id)?.state.terminal() {
            self.apply(actor, &Command::CancelTask { task_id: task_id.clone() }, effects, created)?;
        }
        Ok(())
    }

    /// Once a review holds nothing, the problem raised about it no longer asks anything.
    fn settle_held(&mut self, review_id: &InteractionId, task_id: &TaskId, outcome: PlanReviewOutcome) {
        let at_revision = self.revision + 1;
        let Some(review) = self.plan_reviews.get_mut(review_id) else { return };
        if let Some(entry) = review.dependents.get_mut(task_id) {
            *entry = Some(PlanReviewSettlement { outcome, at_revision });
        }
        if review.open().next().is_some() {
            return;
        }
        let lead = format!("{PLAN_REVIEW_PROBLEM_PREFIX}{review_id}-");
        let moot: Vec<InteractionId> = self.interactions.values()
            .filter(|i| i.author == Author::System && i.id.0.starts_with(&lead) && self.open(i))
            .map(|i| i.id.clone()).collect();
        for problem in moot {
            self.retire(&problem);
        }
    }

    /// The user answered the problem about a review the orchestrator left unsettled.
    pub(super) fn settle_plan_review_problem(&mut self, card: &InteractionId, option: &str) {
        let Some(i) = self.interactions.get(card).filter(|i| i.id.0.starts_with(PLAN_REVIEW_PROBLEM_PREFIX) && i.author == Author::System) else { return };
        let Some(problem) = PlanReviewProblem::from_body(&i.body) else { return };
        if option != PLAN_REVIEW_KEEP_PLANS {
            return;
        }
        let open: Vec<TaskId> = self.plan_reviews.get(&problem.review_id).map(|r| r.open().cloned().collect()).unwrap_or_default();
        for task in open {
            self.settle_held(&problem.review_id, &task, PlanReviewOutcome::KeptByUser);
        }
    }

    /// The held Tasks of a review that could still start: a closed one asks for nothing until it
    /// is reopened, and then it is still held.
    pub fn plan_review_waiting<'a>(&'a self, review: &'a PlanReview) -> impl Iterator<Item = &'a Task> + 'a {
        review.open().filter_map(|task| self.tasks.get(task)).filter(|t| !t.state.terminal() && !t.archived)
    }

    pub(super) fn validate_plan_reviews(&self) -> Result<()> {
        for (id, review) in &self.plan_reviews {
            if !self.interactions.contains_key(id) || !self.tasks.contains_key(&review.task_id)
                || review.decisions.is_empty() || review.decisions.iter().any(|d| !self.interactions.contains_key(d))
                || review.dependents.is_empty()
            {
                return fail("invalid_input", "Plan review references are inconsistent");
            }
            for task in review.open() {
                let t = self.task(task)?;
                if t.started || t.agent_id.is_some() {
                    return fail("conflict", &format!("Task {task} would have a node while a decision holds it for a plan review"));
                }
            }
        }
        Ok(())
    }
}
