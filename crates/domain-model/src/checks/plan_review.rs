//! A decision of the user on a Task that others wait for holds those that have not started, until
//! the orchestrator reviews the launch plan each was given before it. Only a settlement releases
//! one: the orchestrator's, or Alondra's once nobody reviewed it in time, which keeps the plan and
//! tells its node what was decided. Never the end of what it waits for.
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

    pub fn decides(&self, i: &Interaction) -> bool {
        i.card.kind == CardKind::Decide && !matches!(self.holds.get(&i.id), Some(Hold::Permission | Hold::Plan | Hold::Elicitation))
    }




    /// What the node of a task Alondra released unreviewed reads first: what the user decided, and
    /// on which task, after its plan was written.
    pub fn unreviewed_plan_note(&self, review_id: &InteractionId) -> Option<String> {
        let review = self.plan_reviews.get(review_id)?;
        let source = self.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
        let decided: Vec<String> = review.decisions.iter().filter_map(|id| self.interactions.get(id)).map(|card| match &card.payload {
            InteractionPayload::Question { options, answer: Some(answer) } => {
                let option = options.iter().find(|o| o.id == answer.option_id).map_or(answer.option_id.as_str(), |o| o.label.as_str());
                format!("{}: {option}", card.card.effective_title())
            }
            _ => card.card.effective_title().to_owned(),
        }).collect();
        Some(format!("The user decided “{}” on “{source}” after your plan was written: check that your plan still holds before you start.", decided.join("; ")))
    }



    /// The held Tasks of a review that could still start: a closed one asks for nothing until it
    /// is reopened, and then it is still held.
    pub fn plan_review_waiting<'a>(&'a self, review: &'a PlanReview) -> impl Iterator<Item = &'a Task> + 'a {
        review.open().filter_map(|task| self.tasks.get(task)).filter(|t| !t.over() && !t.archived)
    }

    pub fn validate_plan_reviews(&self) -> Result<()> {
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
