//! A node that stops with nothing explaining its next step is Alondra's to fix, never the user's:
//! Alondra hands it back its turn up to `EMPTY_TURN_WAKES` times in a row, and the next stop is a
//! case for the orchestrator, which settles it with one closed outcome. Cases are counted since
//! the task last moved on; past the limit the only outcome is to close the task with a result.
use super::*;

/// The orchestrator's settlement of a stopped node: the node gets the instruction, or the task
/// ends with the result.
pub(crate) fn settle_stall(domain: &mut DomainState, actor: &Actor, case_id: &InteractionId, task: &TaskId, outcome: &StallOutcome,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    domain.settle_stall_guard(actor, case_id, task, outcome)?;
    // Alondra closing a task whose node never answered a message of the user's says so: the
    // close then delivers its open PR to the user instead of being refused for that message.
    let text = match outcome {
        StallOutcome::Unsettled if !domain.task(task)?.pending_user_inputs.is_empty() && domain.open_pr_of(task).is_some() => Some(UNSETTLED_UNANSWERED.to_owned()),
        outcome => outcome.written().map(|(_, text)| text.trim().to_owned()),
    };
    if !matches!(outcome, StallOutcome::Instruct { .. }) && domain.open_pr_of(task).is_some()
        && (domain.lifecycle(lifecycles, task)?.phase() != alondra_domain::TaskPhase::Delivered
            || (text.is_some() && !domain.tasks[task].pending_user_inputs.is_empty())) {
        return domain.close_task(actor, task, text, effects, created, lifecycles);
    }
    let at = domain.revision + 1;
    let i = domain.interactions.get_mut(case_id).expect("the guard found it");
    if let InteractionPayload::Question { answer, .. } = &mut i.payload {
        *answer = Some(Answer {
            attachments: vec![],
            option_id: outcome.option_id().into(),
            inputs: text.iter().map(|text| (ANSWER_TEXT.to_owned(), InputValue::Text(text.clone()))).collect(),
            responder: match actor { Actor::System => "alondra", _ => "alondra-orchestrator" }.into(),
            at_revision: at,
            decision: None,
        });
    }
    i.revision = at;
    match outcome {
        StallOutcome::Instruct { .. } => {
            let record = LifecycleRecord::Answered { card: case_id.clone(), input: domain.next_input(task), text };
            domain.decide(lifecycles, task, record)
        }
        StallOutcome::Close { .. } | StallOutcome::Unsettled if domain.lifecycle(lifecycles, task)?.phase() == alondra_domain::TaskPhase::Delivered
            && (*outcome == StallOutcome::Unsettled || domain.open_pr_of(task).is_some()) => {
            let text = if domain.work_after_merge(task, domain.lifecycle(lifecycles, task)?).is_some() { UNSETTLED_AFTER_MERGE } else { UNSETTLED_DELIVERY };
            let record = LifecycleRecord::Answered { card: case_id.clone(), input: domain.next_input(task), text: Some(text.into()) };
            domain.decide(lifecycles, task, record)
        }
        StallOutcome::Close { .. } | StallOutcome::Unsettled => {
            domain.tasks.get_mut(task).expect("the guard found it").stall_closing = Some(match text {
                Some(result) => StallClosing::Settled { result },
                None => StallClosing::Unsettled,
            });
            domain.tasks.get_mut(task).expect("the guard found it").pending_user_inputs.clear();
            domain.cancel(task, effects, lifecycles)
        }
    }
}
/// A delivery, a card of the node to the user or the user's word on its task means the task
/// moved on: its count of cases starts again.
pub(crate) fn count_progress(domain: &mut DomainState, actor: &Actor, command: &Command) {
    let task = match (actor, command) {
        (Actor::Agent { .. }, Command::PresentResult { task_id, .. }) => Some(task_id.clone()),
        (Actor::Agent { .. }, Command::AskQuestion { scope: Scope::Task { task_id }, recipient: Recipient::User, .. }
            | Command::AskQuestions { scope: Scope::Task { task_id }, recipient: Recipient::User, .. }
            | Command::RequestAction { scope: Scope::Task { task_id }, recipient: Recipient::User, .. }) => Some(task_id.clone()),
        (Actor::System, Command::TellNode { task_id, from_user: true, .. }) | (Actor::User { .. }, Command::ReopenTask { task_id }) => Some(task_id.clone()),
        (Actor::User { .. }, Command::AnswerQuestion { interaction_id, .. }
            | Command::SubmitFeedback { presentation_id: interaction_id, .. }
            | Command::AcceptDelivery { presentation_id: interaction_id, .. }
            | Command::DecideProposedPlan { presentation_id: interaction_id, .. }) => match domain.interactions.get(interaction_id).map(|i| &i.scope) {
            Some(Scope::Task { task_id }) => Some(task_id.clone()),
            _ => None,
        },
        _ => None,
    };
    if let Some(t) = task.and_then(|task| domain.tasks.get_mut(&task)) {
        t.stalls = 0;
    }
}
/// A case the lifecycle no longer waits on (the user told the node something, or it went on
/// by itself) is withdrawn, so the orchestrator never settles what no longer holds.
pub(crate) fn withdraw_released_stalls(domain: &mut DomainState, lifecycles: &mut Lifecycles) -> Result<()> {
    let open: Vec<(TaskId, InteractionId)> = domain.interactions.values()
        .filter(|i| is_stall_case(i) && domain.open(i))
        .filter_map(|i| match &i.scope { Scope::Task { task_id } => Some((task_id.clone(), i.id.clone())), Scope::Orchestrator { .. } => None })
        .collect();
    for (task, case) in open {
        let held = domain.tasks.get(&task).is_some_and(|t| !t.state.terminal())
            && domain.lifecycle_journal.contains_key(&task)
            && domain.lifecycle(lifecycles, &task)?.cards().iter().any(|(held, _)| **held == case);
        if !held {
            domain.retire(&case);
        }
    }
    Ok(())
}
