//! The decisions Claude asks for through a task's SDK execution: the rules Alondra decides some by
//! itself, the cards the user decides the rest on, and how their answers become the decision.
use super::*;
use alondra_core::node_guard::forbidden_for_nodes;

/// The MCP server the Alondra plugin registers in a node's Claude Code.
pub const ALONDRA_MCP_SERVER: &str = "plugin:alondra:alondra";
/// The source of an MCP server a plugin defines.
pub const PLUGIN_SOURCE: &str = "plugin";
pub const USER_DENIED: &str = "The user denied it";
pub const USER_REQUESTED_CHANGES: &str = "The user requests changes to the plan";
/// What Claude is told of a request Alondra could not put in front of the user, before the reason.
pub const UNASKABLE: &str = "Alondra could not ask the user this, so it is denied; change the request and ask again, or go on without it";
/// Claude Code asks to leave plan mode with the plan file's content, and with none while Claude
/// has not written that file.
pub const UNWRITTEN_PLAN: &str = "There is no plan to review: write your plan to the plan file plan mode gave you, then call ExitPlanMode again";

/// What Alondra decides by itself for a request, before anyone is asked; `None` puts it to the
/// user. Nothing is allowed by default: only Alondra's own tools, served by the Alondra plugin's
/// server as the SDK reports it, pass without a card. What nodes may never do is denied, and so is
/// a plan Claude has not written or without the checklist its delivery is held to.
pub fn decision_policy(hold: Hold, tool: Option<&str>, input: &serde_json::Value, mcp_server: Option<&McpServerOrigin>, plan: Option<&str>) -> Option<DecisionRecord> {
    match hold {
        Hold::Permission => {
            if let Some(reason) = forbidden_for_nodes(tool, Some(input)) {
                return Some(DecisionRecord::Deny { message: reason });
            }
            let alondra = tool.is_some_and(|tool| tool.starts_with(alondra_core::activity::ALONDRA_TOOLS))
                && mcp_server.is_some_and(|server| server.source == PLUGIN_SOURCE && server.name == ALONDRA_MCP_SERVER);
            alondra.then_some(DecisionRecord::Allow)
        }
        Hold::Plan => match plan.filter(|plan| !plan.trim().is_empty()) {
            None => Some(DecisionRecord::Deny { message: UNWRITTEN_PLAN.into() }),
            Some(plan) => plan_checklist_problem(plan).map(|(_, fix)| DecisionRecord::Deny { message: fix }),
        },
        Hold::Question | Hold::Elicitation => None,
    }
}


pub fn note(answer: &Answer) -> Option<String> {
    answer.inputs.values().find_map(|value| match value {
        InputValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    })
}

/// What a question card's answer tells Claude: the option chosen, or what the user wrote.
pub fn said(card: &Interaction) -> Option<String> {
    let InteractionPayload::Question { options, answer: Some(answer) } = &card.payload else { return None };
    options.iter().find(|option| option.id == answer.option_id).map(|option| option.label.clone()).or_else(|| note(answer))
}

impl DomainState {





    /// The records that end the request at `epoch`, judged on a replay of the task: its decision
    /// reached Claude, and the turn that asked goes on with it; or it ended without one, with the
    /// card the task then needs when nothing else explains its next step.
    pub fn decision_end_guard(&self, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, answered: bool) -> Result<(Vec<LifecycleRecord>, Option<InteractionId>)> {
        system(actor)?;
        self.task(task)?;
        let mut lifecycle = self.sdk_lifecycle_at(task, generation)?;
        let open = lifecycle.callback(&alondra_domain::decision_callback(generation, epoch));
        if open.is_none() || answered && open != Some(alondra_domain::CallbackView::Answered) {
            return fail("conflict", &format!("The decision at {epoch} of task {task} is not open to end that way"));
        }
        let unheard = decision_cancelled_card_id(task, generation, epoch);
        let mut records = vec![if answered {
            LifecycleRecord::DecisionSettled { epoch }
        } else {
            LifecycleRecord::DecisionAborted { epoch, unheard_card: unheard.clone() }
        }];
        self.judge(task, &mut lifecycle, &records[0])?;
        // A root turn never ends while it waits on a decision: one answered after it ended was asked
        // by the turn's own work, a background subagent, which goes on with it.
        if answered && matches!(lifecycle.activity(), alondra_domain::ActivityView::Working | alondra_domain::ActivityView::OwnWork) {
            records.push(LifecycleRecord::DecisionContinued { epoch });
            self.judge(task, &mut lifecycle, &records[1])?;
        }
        let required = lifecycle.cards().iter().any(|(card, _)| **card == unheard);
        if required && self.interactions.contains_key(&unheard) {
            return fail("conflict", &format!("The end of the decision at {epoch} of task {task} is already recorded"));
        }
        Ok((records, required.then_some(unheard)))
    }



}
