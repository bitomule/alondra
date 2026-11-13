//! What the orchestrator is told when the store stages the effects of a command.
use alondra_core::domain::{ANSWER_TEXT, DecisionPrompt, DomainState, Hold, InputValue, InteractionId, InteractionPayload, McpServerOrigin, RequestId, TaskId};
use alondra_daemon_engine_model::LaunchPlan;
use alondra_provider_contract::DecisionKind;
use serde_json::Value;
use std::path::Path;

/// What the domain is told of a decision Claude asks for through a task's SDK execution: the call
/// it is about and the card the user would answer, built exactly as a held hook's card.
pub struct SdkDecision {
    pub hold: Hold,
    pub tool: Option<String>,
    pub input: Value,
    pub mcp_server: Option<McpServerOrigin>,
    pub prompt: DecisionPrompt,
}

/// How the card of a decision Claude asks for is built, which the store does not know.
pub trait SdkDecisions: Send + Sync {
    fn decide(&self, task_title: &str, kind: DecisionKind, payload: &Value, worktree: Option<&str>, worktrees: &[String], home: &Path) -> SdkDecision;
}

/// What the orchestrator does with a request that holds a plan the user approved from a final result.
pub fn approved_plan_work(state: &DomainState, request: &RequestId) -> Option<String> {
    let approved = state.request_plans.get(request)?;
    let source = state.tasks.get(&approved.task_id).map_or("", |task| task.title.as_str());
    Some(format!(
        "The user approved the plan that Task “{source}” ({task}) proposed in its final result. Request {request} holds that plan: name it with title_request, then create one implementation Task under it, or one per phase when the plan is in phases: in the same project folder as that Task, with dependencies on {task} and role implementation. The first takes the whole plan as its checklist; each later one takes its phase's points from the first with plan_items, with dependencies only on the phases it truly needs. Each starts with its points approved and the whole plan in its mission: never ask its node for a new plan.",
        task = approved.task_id))
}

/// What the user decided on each card of the review: the question and the option, with any words.
fn decided(state: &DomainState, decisions: &[InteractionId]) -> Vec<String> {
    decisions.iter().filter_map(|id| state.interactions.get(id)).map(|card| {
        let InteractionPayload::Question { options, answer: Some(answer) } = &card.payload else {
            return format!("- “{}”: no answer", card.card.effective_title());
        };
        let option = options.iter().find(|o| o.id == answer.option_id).map_or(answer.option_id.as_str(), |o| o.label.as_str());
        let words = match answer.inputs.get(ANSWER_TEXT) {
            Some(InputValue::Text(text)) if !text.trim().is_empty() => format!(" They wrote: {}", text.trim()),
            _ => String::new(),
        };
        format!("- “{}” → {option}.{words}\n  What the node asked: {}", card.card.effective_title(), card.body.trim())
    }).collect()
}

/// The decision and the plan of every dependent it still holds, with how to settle each.
pub fn orchestrator_text(state: &DomainState, review_id: &InteractionId, plan_of: &dyn Fn(&TaskId) -> Option<LaunchPlan>) -> String {
    let Some(review) = state.plan_reviews.get(review_id) else { return String::new() };
    let source = state.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
    let held: Vec<String> = state.plan_review_waiting(review).map(|t| {
        let (task, title) = (&t.id, t.title.as_str());
        let plan = plan_of(task).map_or_else(|| "none stored".to_owned(), |plan| serde_json::to_string_pretty(&plan).unwrap_or_default());
        format!("- “{title}” ({task}). Its launch plan:\n{plan}")
    }).collect();
    format!(
        "The user decided on Task “{source}” ({}), which other tasks wait for:\n{}\n\n\
         These tasks have not started and Alondra holds them, because their launch plans were written before that decision. \
         Review each plan against the decision and settle every one with settle_plan_review(review_id: \"{review_id}\", task_id, outcome): \
         unchanged with the reason the plan still holds, plan_changed with the whole new launch plan and the reason, or cancelled with the reason. \
         None starts until you do, and set_launch_plan does not change a held plan. If it is a product decision only the user can make, ask them first, then settle it.\n{}",
        review.task_id, decided(state, &review.decisions).join("\n"), held.join("\n"))
}
