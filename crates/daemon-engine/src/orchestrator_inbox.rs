//! Nothing waits in the orchestrator. Once it handled everything it was handed, every open request
//! without a task, an answer or a question of its own is settled: a message was acted on and
//! closes, a request of the user's goes back to it once and then becomes the user's Problem. A
//! node's suggestion goes back once. A task that waits for a task that was cancelled is the
//! orchestrator's to re-plan: it is told, reminded once, and only then does the user decide whether
//! it starts without it. Nor is a launch plan the start refused: the orchestrator is reminded once
//! to fix it or cancel the task, then Alondra cancels it.
//!
//! What a task waits for from the orchestrator has a deadline instead, whatever else it is busy
//! with: a stopped node's case, a node's question and the review of the plans a decision holds.
//! The first time it passes the orchestrator is reminded; the second time Alondra settles it
//! itself, and never with a card for the user.
use crate::domain_runtime::DomainRuntime;
use crate::locks::Poisonable as _;
use alondra_core::domain::obligation_policy::{ANSWER, Millis};
use alondra_core::domain::{
    Author, CardKind, DomainState, InboxCard, Interaction, InteractionId, InteractionPayload, PlanReview, PlanReviewOutcome, QuestionOption,
    Request, RequestId, STALL_ATTEMPTS, Scope, StallOutcome, StartProblem, SystemCard, Task, TaskId, UNSETTLED_INSTRUCT,
};
use alondra_core::orchestrator_cards::{
    DEPENDENCY_CANCELLED_NOTICE_PREFIX, UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL, UNSPLIT_NOTICE_PREFIX,
    UNSPLIT_PROBLEM_PREFIX, UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL, UnsplitProblem,
    PLAN_REVIEW_NOTICE_PREFIX, PLAN_REFUSED_NOTICE_PREFIX, STALL_NOTICE_PREFIX,
};
use std::collections::BTreeSet;

const UNSPLIT_CAUSE: &str = "request_unsplit";
const BODY_SHOWN: usize = 2_000;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Close { request: RequestId, withdrawn: bool },
    Remind { request: RequestId, id: String, card: SystemCard, text: String },
    Raise { request: RequestId, id: String, card: SystemCard, problem: UnsplitProblem },
    /// A reminder about a node's question or suggestion.
    Tell { id: String, card: SystemCard, text: String },
    /// A card that no longer asks anything: a node's question nobody answered in time, whose node
    /// is told to decide by itself, or a problem the user no longer has to settle.
    Withdraw { question: InteractionId },
    /// A reminder about a decision of the user whose held plans are not reviewed yet.
    TellPlanReview { id: String, card: SystemCard, review: InteractionId, number: u32 },
    /// Nobody reviewed the plan of a held task in time: it keeps its plan, and its node reads first
    /// what was decided.
    ReleaseUnreviewed { review: InteractionId, task: TaskId, note: String },
    /// The orchestrator was told and reminded and the task still waits for a cancelled task: the
    /// user decides whether it starts without it.
    RaiseDependencyCancelled { task: TaskId, dependency: TaskId },
    /// The case of a stopped node is still unsettled past its reminder: Alondra settles it.
    SettleStall { case: InteractionId, task: TaskId, outcome: StallOutcome },
    /// A refused launch plan is still as it was after its reminder: Alondra cancels the task.
    CancelRefusedPlan { card: InteractionId, task: TaskId, error: String },
}

/// What is left in the orchestrator's inbox after it handled everything it was handed.
pub(crate) fn steps(state: &DomainState) -> Vec<Step> {
    let mut steps = vec![];
    for interaction in state.interactions.values().filter(|i| waits(i) && i.card.kind == CardKind::Propose) {
        steps.extend(unsettled(state, interaction));
    }
    for problem in state.interactions.values().filter(|i| retired_problem(i) && state.open(i)) {
        steps.push(Step::Withdraw { question: problem.id.clone() });
    }
    for task in state.tasks.values().filter(|task| !task.over() && !task.archived) {
        steps.extend(dependency_cancelled(state, task));
        steps.extend(plan_refused(state, task));
    }
    for request in state.requests.values().filter(|request| !request.closed) {
        if state.tasks.values().any(|task| task.request_id == request.id) || asked_about(state, &request.id) {
            continue;
        }
        if request.message {
            steps.push(Step::Close { request: request.id.clone(), withdrawn: false });
            continue;
        }
        steps.extend(unsplit(state, request));
    }
    steps
}

/// What the orchestrator owes past its deadline, each step with the tasks whose clock it acts on:
/// a reminder the first time, then Alondra's own outcome.
pub(crate) fn overdue(state: &DomainState, due: &BTreeSet<TaskId>) -> Vec<(Step, Vec<TaskId>)> {
    let late = |i: &Interaction| task_of(state, i).filter(|task| !task.over() && due.contains(&task.id));
    let mut steps = vec![];
    for case in state.interactions.values().filter(|i| state.unsettled_stall(i)) {
        if let Some(task) = late(case) {
            steps.push((stalled(state, case, task), vec![task.id.clone()]));
        }
    }
    for question in state.interactions.values().filter(|i| waits(i) && i.card.kind != CardKind::Propose) {
        if let Some(task) = late(question) {
            steps.push((unanswered(state, question), vec![task.id.clone()]));
        }
    }
    for (id, review) in &state.plan_reviews {
        let held: Vec<TaskId> = state.plan_review_waiting(review).filter(|task| due.contains(&task.id)).map(|task| task.id.clone()).collect();
        if !held.is_empty() {
            steps.extend(unreviewed(state, id, held));
        }
    }
    steps
}

/// The problems Alondra no longer raises: what they asked the user is settled by time now.
fn retired_problem(i: &Interaction) -> bool {
    i.author == Author::System
        && matches!(i.system_card, Some(SystemCard::Inbox { about: InboxCard::UnansweredProblem | InboxCard::PlanReviewProblem, .. }))
}

/// What a handled batch left without an outcome, as `request <id>` or `suggestion <id>`: one of
/// its open requests with no task, no open question of the orchestrator, and neither a reminder on
/// its way back nor the user's problem; one of its node suggestions with no decision and no
/// reminder. `settle` leaves it empty.
pub(crate) fn unresolved(state: &DomainState, requests: &[RequestId], suggestions: &[InteractionId]) -> Vec<String> {
    let mut left = vec![];
    for id in requests {
        let Some(request) = state.requests.get(id) else { continue };
        if request.closed
            || state.tasks.values().any(|task| task.request_id == *id)
            || asked_about(state, id)
            || !numbered(state, InboxCard::UnsplitNotice, &id.0).is_empty()
            || !numbered(state, InboxCard::UnsplitProblem, &id.0).is_empty()
        {
            continue;
        }
        left.push(format!("request {id}"));
    }
    for id in suggestions {
        let Some(suggestion) = state.interactions.get(id) else { continue };
        if !waits(suggestion) || state.interactions.contains_key(&InteractionId(format!("{UNSETTLED_NOTICE_PREFIX}{id}"))) {
            continue;
        }
        left.push(format!("suggestion {id}"));
    }
    left
}

/// The open requests and the node suggestions a batch is asked to leave with an outcome.
pub(crate) fn asked(state: &DomainState) -> (Vec<RequestId>, Vec<InteractionId>) {
    let requests = state.requests.values().filter(|request| !request.closed).map(|request| request.id.clone()).collect();
    let suggestions = state.interactions.values()
        .filter(|i| waits(i) && i.card.kind == CardKind::Propose)
        .map(|i| i.id.clone())
        .collect();
    (requests, suggestions)
}

/// The orchestrator waits for the user's answer about this request: nothing is left in it.
fn asked_about(state: &DomainState, request: &RequestId) -> bool {
    state.interactions.values().any(|i| {
        i.author != Author::System
            && !i.withdrawn
            && matches!(&i.scope, Scope::Orchestrator { request_id: Some(id) } if id == request)
            && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
    })
}

/// The cards of one kind about `request`, by their number.
fn numbered<'a>(state: &'a DomainState, kind: InboxCard, of: &str) -> Vec<(u32, &'a alondra_core::domain::Interaction)> {
    state.interactions.values()
        .filter_map(|i| match &i.system_card {
            Some(SystemCard::Inbox { about, subject, number }) if *about == kind && subject == of => Some((*number, i)),
            _ => None,
        })
        .collect()
}

/// The id and the kind of the `number`th card of the inbox about `subject`.
fn inbox_card(prefix: &str, about: InboxCard, subject: &str, number: u32) -> (String, SystemCard) {
    (format!("{prefix}{subject}-{number}"), SystemCard::Inbox { about, subject: subject.to_owned(), number })
}

fn unsplit(state: &DomainState, request: &Request) -> Option<Step> {
    let reminders = numbered(state, InboxCard::UnsplitNotice, &request.id.0).len() as u32;
    let problems = numbered(state, InboxCard::UnsplitProblem, &request.id.0);
    let mut last: Option<(u32, Option<&str>)> = None;
    for (n, problem) in &problems {
        let answer = match &problem.payload {
            InteractionPayload::Question { answer: Some(answer), .. } => Some(answer.option_id.as_str()),
            InteractionPayload::Question { answer: None, .. } if !problem.withdrawn => return None,
            _ => None,
        };
        if last.is_none_or(|(m, _)| *n > m) {
            last = Some((*n, answer));
        }
    }
    if let Some((_, Some(UNSPLIT_CLOSE))) = last {
        return Some(Step::Close { request: request.id.clone(), withdrawn: true });
    }
    let raised = problems.len() as u32;
    if reminders > raised {
        let title = request.title.clone().unwrap_or_else(|| request.body.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or_default().to_owned());
        let (id, card) = inbox_card(UNSPLIT_PROBLEM_PREFIX, InboxCard::UnsplitProblem, &request.id.0, raised + 1);
        return Some(Step::Raise {
            request: request.id.clone(),
            id,
            card,
            problem: UnsplitProblem { request_id: request.id.0.clone(), title },
        });
    }
    let text = match approved_plan_work(state, &request.id) {
        Some(work) => format!(
            "Request {} holds a plan the user approved and still has no implementation Task, and nothing else waits for you. Create its Tasks now; close it with close_request only if Tasks that already exist cover the whole plan, naming them. {work}",
            request.id),
        None => {
            let body: String = request.body.trim().chars().take(BODY_SHOWN).collect();
            format!(
                "Request {} has no Task, no answer and no open question of yours, and nothing else waits for you. Handle it now: create its Task, answer it and close it with close_request, or ask the user. An answer written only in this conversation reaches nobody. If it no longer applies, close it with close_request and say why.\nRequest: {body}",
                request.id)
        }
    };
    let (id, card) = inbox_card(UNSPLIT_NOTICE_PREFIX, InboxCard::UnsplitNotice, &request.id.0, reminders + 1);
    Some(Step::Remind { request: request.id.clone(), id, card, text })
}

/// What the orchestrator does with a request that holds a plan the user approved from a final result.
pub(crate) fn approved_plan_work(state: &DomainState, request: &RequestId) -> Option<String> {
    let approved = state.request_plans.get(request)?;
    let source = state.tasks.get(&approved.task_id).map_or("", |task| task.title.as_str());
    Some(format!(
        "The user approved the plan that Task “{source}” ({task}) proposed in its final result. Request {request} holds that plan: name it with title_request, then create one implementation Task under it, or one per phase when the plan is in phases: in the same project folder as that Task, with dependencies on {task} and role implementation. The first takes the whole plan as its checklist; each later one takes its phase's points from the first with plan_items, with dependencies only on the phases it truly needs. Each starts with its points approved and the whole plan in its mission: never ask its node for a new plan.",
        task = approved.task_id))
}

fn task_of<'a>(state: &'a DomainState, i: &Interaction) -> Option<&'a alondra_core::domain::Task> {
    match &i.scope {
        Scope::Task { task_id } => state.tasks.get(task_id),
        Scope::Orchestrator { .. } => None,
    }
}

/// A node's question or suggestion to the orchestrator still without its answer.
fn waits(i: &Interaction) -> bool {
    matches!(i.author, Author::Agent { .. })
        && i.recipient == alondra_core::domain::Recipient::Orchestrator
        && !i.withdrawn
        && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
}

fn quoted(i: &Interaction) -> String {
    let body: String = i.body.trim().chars().take(BODY_SHOWN).collect();
    format!("\"{}\"\n{body}", i.card.effective_title())
}

fn unsettled(state: &DomainState, suggestion: &Interaction) -> Option<Step> {
    let id = format!("{UNSETTLED_NOTICE_PREFIX}{}", suggestion.id);
    (!state.interactions.contains_key(&InteractionId(id.clone()))).then(|| Step::Tell {
        id,
        card: SystemCard::Inbox { about: InboxCard::UnsettledNotice, subject: suggestion.id.0.clone(), number: 1 },
        text: format!(
            "Suggestion {} from a node has no decision, and nothing else waits for you. Settle it now with settle_suggestion: new_task (the call creates it), current_task or discard with the reason. If you asked the user about it, settle it once they answer.\nSuggestion: {}",
            suggestion.id, quoted(suggestion)),
    })
}

/// A delivered node's case left unsettled keeps the delivery with the user; any other gets the
/// node back to work while it has attempts left, and closes the task after.
fn stalled(state: &DomainState, case: &Interaction, task: &Task) -> Step {
    let id = format!("{STALL_NOTICE_PREFIX}{}", case.id);
    if state.interactions.contains_key(&InteractionId(id.clone())) {
        let delivered = state.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.phase() == alondra_domain::TaskPhase::Delivered);
        let outcome = if task.stalls <= STALL_ATTEMPTS && !delivered { StallOutcome::Instruct { text: UNSETTLED_INSTRUCT.into() } } else { StallOutcome::Unsettled };
        return Step::SettleStall { case: case.id.clone(), task: task.id.clone(), outcome };
    }
    Step::Tell {
        id,
        card: SystemCard::Inbox { about: InboxCard::StallNotice, subject: case.id.0.clone(), number: 1 },
        text: format!(
            "Reminder: case {} about the stopped node of Task “{}” ({}) is still unsettled. Settle it now with settle_stall: instruct with one concrete next step, or close with the result. If it is still unsettled in {} minutes, Alondra settles it itself: the node is told to go on, or, after {STALL_ATTEMPTS} cases, the task closes without a result of yours. Never ask the user about it.\nCase: {}",
            case.id, task.title, task.id, ANSWER / 60_000, quoted(case)),
    }
}

fn plan_refused(state: &DomainState, task: &Task) -> Option<Step> {
    let card = state.open_plan_refusal(&task.id)?;
    let id = format!("{PLAN_REFUSED_NOTICE_PREFIX}{}", card.id);
    if state.interactions.contains_key(&InteractionId(id.clone())) {
        let error = match alondra_core::start_problem::from_body(&card.body) {
            Some(StartProblem::PlanRefused { error }) => error,
            _ => String::new(),
        };
        return Some(Step::CancelRefusedPlan { card: card.id.clone(), task: task.id.clone(), error });
    }
    Some(Step::Tell {
        id,
        card: SystemCard::Inbox { about: InboxCard::PlanRefusedNotice, subject: card.id.0.clone(), number: 1 },
        text: format!(
            "Reminder: the launch plan of Task “{}” ({}) was refused and is still as it was, and nothing else waits for you. Fix it now with update_launch_plan or set_launch_plan, or answer {} with option cancel and the reason in inputs.text. If you leave it again, Alondra cancels the task itself. Never ask the user about it.\nCard: {}",
            task.title, task.id, card.id, quoted(card)),
    })
}

/// A node's question nobody answered in time: the orchestrator is reminded, then the question is
/// withdrawn and its node told to decide by itself.
fn unanswered(state: &DomainState, question: &Interaction) -> Step {
    let reminders = numbered(state, InboxCard::UnansweredNotice, &question.id.0).len() as u32;
    if reminders > 0 {
        return Step::Withdraw { question: question.id.clone() };
    }
    let (id, card) = inbox_card(UNANSWERED_NOTICE_PREFIX, InboxCard::UnansweredNotice, &question.id.0, 1);
    Step::Tell {
        id,
        card,
        text: format!(
            "Reminder: question {} from a node has no answer: its node stands still until you answer. Answer it now with answer_question, also if what it asked for is already done (say so). An answer written only in this conversation never reaches the node. If it is still unanswered in {} minutes, Alondra withdraws it and tells the node to decide by itself.\nQuestion: {}",
            question.id, ANSWER / 60_000, quoted(question)),
    }
}

/// The held tasks of a review nobody settled in time: the orchestrator is reminded, then each
/// starts with the plan it had.
fn unreviewed(state: &DomainState, id: &InteractionId, held: Vec<TaskId>) -> Vec<(Step, Vec<TaskId>)> {
    let reminders = numbered(state, InboxCard::PlanReviewNotice, &id.0).len() as u32;
    if reminders == 0 {
        let (notice, card) = inbox_card(PLAN_REVIEW_NOTICE_PREFIX, InboxCard::PlanReviewNotice, &id.0, 1);
        return vec![(Step::TellPlanReview { id: notice, card, review: id.clone(), number: 1 }, held)];
    }
    let Some(note) = state.unreviewed_plan_note(id) else { return vec![] };
    held.into_iter().map(|task| (Step::ReleaseUnreviewed { review: id.clone(), task: task.clone(), note: note.clone() }, vec![task])).collect()
}

/// A task that waits for a cancelled task goes to the orchestrator twice, then to the user. Once
/// it no longer waits for it, a card still asking about it is withdrawn.
fn dependency_cancelled(state: &DomainState, task: &Task) -> Option<Step> {
    let open = state.open_start_problem(&task.id);
    let waits_for = state.cancelled_dependency(task);
    if let Some(card) = open
        && let Some(StartProblem::DependencyCancelled { dependency }) = alondra_core::start_problem::from_body(&card.body)
        && waits_for.as_ref() != Some(&dependency)
    {
        return Some(Step::Withdraw { question: card.id.clone() });
    }
    let dependency = waits_for?;
    if open.is_some() {
        return None;
    }
    let subject = format!("{}--{dependency}", task.id);
    let told = numbered(state, InboxCard::DependencyCancelledNotice, &subject).len() as u32;
    if told >= 2 {
        return Some(Step::RaiseDependencyCancelled { task: task.id.clone(), dependency });
    }
    let other = state.tasks.get(&dependency).map_or("", |t| t.title.as_str());
    let kept: Vec<&str> = task.dependencies.iter().filter(|d| **d != dependency).map(|d| d.0.as_str()).collect();
    let kept = if kept.is_empty() { "none".to_owned() } else { kept.join(", ") };
    let (id, card) = inbox_card(DEPENDENCY_CANCELLED_NOTICE_PREFIX, InboxCard::DependencyCancelledNotice, &subject, told + 1);
    Some(Step::Tell {
        id,
        card,
        text: format!(
            "{}Task “{}” ({}) waits for “{other}” ({dependency}), which was cancelled, so it does not start. Re-plan it now with one \
             of these: if another task replaces “{other}”, make it depend on that one with set_dependencies, keeping its other \
             dependencies ({kept}); if it can go on without it, set_dependencies without {dependency}, and set_launch_plan if its \
             mission relied on that task's result; if it no longer makes sense, cancel_task. Ask the user only for a decision \
             that is theirs. If you leave it as it is, the user is asked whether to start it without “{other}” or cancel it.",
            if told == 0 { "" } else { "Reminder: " }, task.title, task.id),
    })
}

/// The orchestrator left a refused plan past its reminder: the task is cancelled, saying why.
fn cancel_refused_plan(domain: &DomainRuntime, card: &InteractionId, task: &TaskId, error: &str) -> Result<(), crate::domain_store::StoreError> {
    crate::domain_github::execute_system(domain, alondra_core::domain::Command::CancelTask { task_id: task.clone() })?;
    let reason = format!("Its launch plan was refused ({}) and the orchestrator neither fixed it nor cancelled the task", error.trim());
    if let Err(error) = domain.store.lock_or_exit("the domain store").set_cancel_reason(task, &reason) {
        log!("could not record why {task} was cancelled: {error:#}");
    }
    if domain.state().is_ok_and(|state| state.open_plan_refusal(task).is_some()) {
        domain.withdraw_problem(card)?;
    }
    Ok(())
}

/// Carries the steps out; one that fails is tried again on the next pass.
pub(crate) fn settle(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    for step in steps(&state) {
        carry_out(domain, &state, &step);
    }
}

/// Steps the daemon's one clock at `now`. What the orchestrator owes past its deadline is settled
/// on every pass of the orchestrator, busy or failing, so nothing a task waits for from it lasts
/// for ever.
pub(crate) fn settle_overdue(domain: &DomainRuntime, now: Millis) {
    crate::obligation_watch::pass(domain, &mut domain.live.obligations.lock_or_recover("the obligation watch"), now);
}

/// What the orchestrator owes the `due` tasks past its deadline: a reminder the first time, then
/// Alondra's own outcome.
pub(crate) fn settle_answers(domain: &DomainRuntime, state: &DomainState, due: &BTreeSet<TaskId>) {
    for (step, tasks) in overdue(state, due) {
        if carry_out(domain, state, &step) {
            log!("the orchestrator owed {} past its deadline: {step:?}", tasks.iter().map(|task| task.0.as_str()).collect::<Vec<_>>().join(", "));
        }
    }
}

/// Whether `step` was carried out.
fn carry_out(domain: &DomainRuntime, state: &DomainState, step: &Step) -> bool {
    let done = match step {
        Step::Close { request, withdrawn } => domain.close_request_by_system(request, *withdrawn).map(drop),
        Step::Remind { id, card, text, .. } => domain.tell_orchestrator(id, Some(card.clone()), text.clone()).map(drop),
        Step::Raise { request, id, card, problem } => {
            let (need, why) = problem.text(alondra_core::views::Lang::En);
            let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
            domain.raise_worded_problem_in(
                Scope::Orchestrator { request_id: Some(request.clone()) },
                id, Some(card.clone()), UNSPLIT_CAUSE, need, why, problem.to_body(),
                vec![option(UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL), option(UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL)],
                Some(UNSPLIT_RETRY),
            ).map(drop)
        }
        Step::Tell { id, card, text } => domain.tell_orchestrator(id, Some(card.clone()), text.clone()).map(drop),
        Step::Withdraw { question } => domain.withdraw_problem(question).map(drop),
        Step::TellPlanReview { id, card, review, number } => {
            let plan_of = |task: &alondra_core::domain::TaskId| domain.launch_plan(task).ok().flatten();
            let text = format!("Reminder {number}: these plans are still not reviewed. If they are still not in {} minutes, Alondra starts each with the plan it has and tells its node what the user decided. {}",
                ANSWER / 60_000, crate::plan_reviews::orchestrator_text(state, review, &plan_of));
            domain.tell_orchestrator(id, Some(card.clone()), text).map(drop)
        }
        Step::ReleaseUnreviewed { review, task, note } => {
            domain.settle_plan_review_by_system(review, task, PlanReviewOutcome::Unchanged { reason: note.clone() }).map(drop)
        }
        Step::SettleStall { case, task, outcome } => domain.settle_unsettled_stall(case, task, outcome.clone()).map(drop),
        Step::CancelRefusedPlan { card, task, error } => cancel_refused_plan(domain, card, task, error),
        Step::RaiseDependencyCancelled { task, dependency } => {
            if let Err(error) = crate::start_failures::raise(domain, task, StartProblem::DependencyCancelled { dependency: dependency.clone() }) {
                log!("could not ask the user about {task}, which waits for the cancelled {dependency}: {error:#}");
            }
            Ok(())
        }
    };
    if let Err(error) = &done {
        log!("could not settle the orchestrator's inbox ({step:?}): {error:?}");
    }
    done.is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, InteractionPayload, PROTOCOL_VERSION};
    use alondra_core::orchestrator_cards::UNANSWERED_NOTICE_PREFIX;

    fn run(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        let shown = format!("{command:?}");
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap_or_else(|error| panic!("{shown} was refused: {error:?}"));
    }

    fn request(domain: &DomainRuntime, id: &str, message: bool) {
        run(domain, Actor::User { user_id: "u".into() }, id, Command::CreateRequest {
            request_id: id.into(), body: format!("Do {id}"), auto_merge: false, maintenance: false, message,
        });
    }

    fn closed(domain: &DomainRuntime, id: &str) -> bool {
        domain.state().unwrap().requests[&id.into()].closed
    }

    fn cards(domain: &DomainRuntime, prefix: &str) -> Vec<String> {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(prefix)).map(|id| id.0.clone()).collect()
    }

    /// 8 October: three messages the orchestrator acted on stayed open with no task for ever.
    #[test]
    fn a_message_acted_on_without_tasks_closes_once_the_orchestrator_is_done() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "m-1", true);
        settle(&domain);
        settle(&domain);
        assert!(closed(&domain, "m-1"));
        assert!(cards(&domain, UNSPLIT_NOTICE_PREFIX).is_empty(), "a message is never sent back");
    }

    #[test]
    fn a_request_left_without_tasks_goes_back_once_then_becomes_the_users_problem() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "r-1", false);
        settle(&domain);
        assert_eq!(cards(&domain, UNSPLIT_NOTICE_PREFIX), [format!("{UNSPLIT_NOTICE_PREFIX}r-1-1")]);
        assert!(cards(&domain, UNSPLIT_PROBLEM_PREFIX).is_empty(), "the orchestrator gets one more go first");
        settle(&domain);
        let problem = format!("{UNSPLIT_PROBLEM_PREFIX}r-1-1");
        assert_eq!(cards(&domain, UNSPLIT_PROBLEM_PREFIX), [problem.clone()]);
        settle(&domain);
        assert_eq!(cards(&domain, UNSPLIT_PROBLEM_PREFIX).len(), 1, "an open problem is not raised twice");
        assert!(!closed(&domain, "r-1"), "a request of the user's never closes by itself");
        run(&domain, Actor::User { user_id: "u".into() }, "close-it", Command::AnswerQuestion {
            attachments: vec![], interaction_id: problem.as_str().into(), option_id: UNSPLIT_CLOSE.into(), inputs: Default::default(),
        });
        settle(&domain);
        let state = domain.state().unwrap();
        assert!(state.requests[&"r-1".into()].closed && state.requests[&"r-1".into()].withdrawn);
    }

    #[test]
    fn a_request_with_a_task_or_an_open_question_of_the_orchestrator_is_left_alone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "asked", false);
        request(&domain, "split", true);
        run(&domain, Actor::Orchestrator { generation: 0 }, "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: "q-1".into(), scope: Scope::Orchestrator { request_id: Some("asked".into()) },
            recipient: alondra_core::domain::Recipient::User, body: "Which app?".into(), blocking: true,
            options: vec![
                QuestionOption { id: "a".into(), label: "A".into(), description: None },
                QuestionOption { id: "b".into(), label: "B".into(), description: None },
            ],
            card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Decide, "Pick the app"),
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t-1".into(), request_id: "split".into(), title: "Task".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        assert_eq!(steps(&domain.state().unwrap()), []);
        assert!(matches!(domain.state().unwrap().interactions[&"q-1".into()].payload, InteractionPayload::Question { answer: None, .. }));
    }

    fn orchestrator_messages(domain: &DomainRuntime) -> Vec<i64> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|message| message.recipient_kind == "orchestrator").map(|message| message.id).collect()
    }

    /// 25 October: the user cancelled the only task of a request before any node ran it, and the
    /// orchestrator was woken to close the request. A task that waited for a cancelled one is
    /// still the orchestrator's to re-plan.
    #[test]
    fn a_request_whose_only_task_the_user_cancels_before_it_starts_hands_the_orchestrator_nothing() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        request(&domain, "solo", false);
        request(&domain, "pair", false);
        let create = |id: &str, request: &str, dependencies: &[&str]| Command::CreateTask { plan_items: None, continues: None,
            task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        };
        run(&domain, Actor::Orchestrator { generation: 0 }, "alone", create("alone", "solo", &[]));
        run(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "pair".into(), title: "Pair".into() });
        run(&domain, Actor::Orchestrator { generation: 0 }, "first", create("first", "pair", &[]));
        run(&domain, Actor::Orchestrator { generation: 0 }, "second", create("second", "pair", &["first"]));
        let before = orchestrator_messages(&domain);
        run(&domain, Actor::User { user_id: "u".into() }, "cancel-alone", Command::CancelTask { task_id: "alone".into() });
        assert_eq!(orchestrator_messages(&domain), before, "the cancel leaves no message for the orchestrator");
        let state = domain.state().unwrap();
        assert!(state.requests[&"solo".into()].closed && state.requests[&"solo".into()].withdrawn);
        assert_eq!(steps(&state), []);

        run(&domain, Actor::User { user_id: "u".into() }, "cancel-first", Command::CancelTask { task_id: "first".into() });
        let state = domain.state().unwrap();
        assert!(!state.requests[&"pair".into()].closed, "its other task still waits");
        assert!(state.open_start_problem(&"second".into()).is_none(), "the user is not asked first");
        assert!(matches!(&steps(&state)[..], [Step::Tell { text, .. }] if text.contains("Re-plan it now")),
            "the orchestrator re-plans the task that waited for it: {:?}", steps(&state));
    }

    fn node_with_task(domain: &DomainRuntime) {
        run(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
        request(domain, "r", false);
        run(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Resume the migration".into(), agent_id: Some("a".into()),
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        run(domain, Actor::Agent { agent_id: "a".into() }, "start", Command::StartTask { task_id: "t".into() });
    }

    fn ask(domain: &DomainRuntime, id: &str) {
        run(domain, Actor::Agent { agent_id: "a".into() }, &format!("ask-{id}"), Command::AskQuestion {
            attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_core::domain::Recipient::Orchestrator, body: "Create one task per phase of the migration".into(),
            blocking: true, options: vec![
                QuestionOption { id: "done".into(), label: "Done".into(), description: None },
                QuestionOption { id: "no".into(), label: "No".into(), description: None },
            ],
            card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Create the phase tasks").why("The plan needs them"),
        });
    }

    fn answer(domain: &DomainRuntime, actor: Actor, card: &str, option: &str) {
        run(domain, actor, &format!("answer-{card}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: card.into(), option_id: option.into(), inputs: Default::default(),
        });
    }

    fn open(domain: &DomainRuntime, id: &str) -> bool {
        let state = domain.state().unwrap();
        state.open(&state.interactions[&id.into()])
    }

    const MINUTE: Millis = 60_000;

    /// Task `t`, whose node `a` runs a turn through the SDK.
    fn sdk_node_with_task(domain: &DomainRuntime) {
        run(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
        request(domain, "r", false);
        run(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Resume the migration".into(), agent_id: None,
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        crate::domain_store::admit_through_sdk(domain, &"t".into(), "a");
        run(domain, Actor::System, "available", Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
            alondra_core::domain::RuntimeObservation::ResourceAvailable,
        ] });
        node_step(domain);
    }

    /// The node's next step: the running turn ends, or a turn starts with what it was told.
    fn node_step(domain: &DomainRuntime) {
        let state = domain.state().unwrap();
        let lifecycle = state.lifecycle_of(&"t".into()).unwrap();
        let at = state.revision;
        if let Some(turn) = lifecycle.turn() {
            run(domain, Actor::System, &format!("end-{at}"), Command::RecordTurnBoundary {
                task_id: "t".into(), generation: 1, turn: turn.as_str().to_owned(), failure: None, failed: None,
            });
        } else if let Some(input) = lifecycle.pending_inputs().first() {
            run(domain, Actor::System, &format!("start-{at}"), Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
                alondra_core::domain::RuntimeObservation::TurnStarted { turn: format!("turn-{at}"), input: input.as_str().to_owned() },
            ] });
        }
    }

    fn node_messages(domain: &DomainRuntime) -> Vec<String> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|message| message.recipient_kind == "agent").map(|message| message.message).collect()
    }

    fn user_cards(domain: &DomainRuntime) -> Vec<String> {
        domain.state().unwrap().interactions.values()
            .filter(|i| i.recipient == alondra_core::domain::Recipient::User).map(|i| i.id.0.clone()).collect()
    }

    /// 16 October: a node asked the orchestrator to create one task per phase of a migration; the
    /// orchestrator created the tasks and never answered, and the node could not deliver its PR.
    #[test]
    fn an_unanswered_question_goes_back_to_its_node_without_a_card() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        sdk_node_with_task(&domain);
        ask(&domain, "q");
        node_step(&domain);
        settle(&domain);
        assert!(cards(&domain, UNANSWERED_NOTICE_PREFIX).is_empty(), "a handled batch reminds of nothing");
        for minutes in [0, 9] {
            settle_overdue(&domain, minutes * MINUTE);
        }
        assert!(cards(&domain, UNANSWERED_NOTICE_PREFIX).is_empty(), "nothing before its deadline");
        settle_overdue(&domain, 10 * MINUTE);
        let reminder = format!("{UNANSWERED_NOTICE_PREFIX}q-1");
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX), [reminder.clone()]);
        assert!(domain.state().unwrap().interactions[&reminder.as_str().into()].body.contains("Create one task per phase"));
        settle_overdue(&domain, 19 * MINUTE);
        assert!(open(&domain, "q"), "one more go for the orchestrator first");
        settle_overdue(&domain, 21 * MINUTE);
        let state = domain.state().unwrap();
        assert!(state.interactions[&"q".into()].withdrawn, "the question is withdrawn");
        assert!(!state.waits_for_orchestrator(&"t".into()));
        assert!(node_messages(&domain).iter().any(|message| message.contains(
            "The orchestrator did not answer “Create the phase tasks”. Decide with what you know, or ask the user if it is theirs to decide.")),
            "{:?}", node_messages(&domain));
        assert!(user_cards(&domain).is_empty(), "no card for the user: {:?}", user_cards(&domain));
        settle_overdue(&domain, 60 * MINUTE);
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX).len(), 1);
    }

    #[test]
    fn an_answered_question_or_one_of_a_task_that_ended_is_left_alone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "answered");
        answer(&domain, Actor::Orchestrator { generation: 0 }, "answered", "done");
        assert!(!open(&domain, "answered"));
        ask(&domain, "late");
        run(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into() });
        let everyone: BTreeSet<TaskId> = domain.state().unwrap().tasks.keys().cloned().collect();
        assert!(overdue(&domain.state().unwrap(), &everyone).is_empty(), "{:?}", overdue(&domain.state().unwrap(), &everyone));
    }

    /// A problem raised before the deadline existed no longer asks the user anything.
    #[test]
    fn a_problem_about_an_unanswered_question_is_withdrawn() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "q");
        let problem = alondra_core::orchestrator_cards::UnansweredProblem { question_id: "q".into(), task_title: "Task".into(), question: "Create the phase tasks".into() };
        let (need, why) = problem.text(alondra_core::views::Lang::En);
        let id = format!("{}q-1", alondra_core::orchestrator_cards::UNANSWERED_PROBLEM_PREFIX);
        domain.raise_worded_problem_in(Scope::Orchestrator { request_id: None }, &id,
            Some(SystemCard::Inbox { about: InboxCard::UnansweredProblem, subject: "q".into(), number: 1 }), "question_unanswered:q", need, why, problem.to_body(),
            vec![QuestionOption { id: "remind_again".into(), label: "Remind".into(), description: None }], None).unwrap();
        assert!(open(&domain, &id));
        settle(&domain);
        assert!(!open(&domain, &id));
    }

    #[test]
    fn a_suggestion_left_unsettled_goes_back_once_and_never_to_the_user() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        node_with_task(&domain);
        run(&domain, Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
            attachments: vec![], interaction_id: "p".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_core::domain::Recipient::Orchestrator, body: "README.md:12".into(), blocking: false,
            options: alondra_core::domain::suggestion_options(),
            card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Propose, "The README names the old flag").why("Not this task"),
        });
        settle(&domain);
        settle(&domain);
        let reminder = format!("{UNSETTLED_NOTICE_PREFIX}p");
        assert_eq!(cards(&domain, UNSETTLED_NOTICE_PREFIX), [reminder.clone()]);
        assert!(domain.state().unwrap().interactions[&reminder.as_str().into()].body.contains("settle_suggestion"));
        assert!(cards(&domain, "problem-").is_empty(), "a suggestion never becomes the user's card");
    }

    #[derive(Clone, Debug)]
    enum Op {
        Ask(usize),
        Answer(usize),
        Withdraw(usize),
        BatchEnds,
        NodeStep,
        Minutes(i64),
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            3 => (0..4usize).prop_map(Op::Ask),
            2 => (0..4usize).prop_map(Op::Answer),
            1 => (0..4usize).prop_map(Op::Withdraw),
            2 => Just(Op::BatchEnds),
            3 => Just(Op::NodeStep),
            4 => (1..12i64).prop_map(Op::Minutes),
        ]
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 48, ..proptest::prelude::ProptestConfig::default() })]

        /// Whatever order the node asks and ends its turns, the orchestrator answers some and the node
        /// withdraws others, every question its node waits on ends answered or withdrawn once its
        /// deadlines pass, and none ever becomes a card for the user.
        #[test]
        fn no_question_to_the_orchestrator_outlives_its_deadlines(ops in proptest::collection::vec(op(), 1..24)) {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir_all(&home).unwrap();
            let domain = DomainRuntime::open(&home).unwrap();
            sdk_node_with_task(&domain);
            let mut clock = 0;
            for (at, op) in ops.into_iter().enumerate() {
                let state = domain.state().unwrap();
                let question = |n: usize| format!("q{n}");
                let exists = |n: usize| state.interactions.contains_key(&question(n).as_str().into());
                let execute = |actor: Actor, command: Command| {
                    let _ = domain.execute(&actor, &CommandEnvelope {
                        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: format!("op-{at}").into(),
                        expected_revision: state.revision, command,
                    }, None, || Ok(()));
                };
                match op {
                    Op::Ask(n) if !exists(n) => execute(Actor::Agent { agent_id: "a".into() }, Command::AskQuestion {
                        attachments: vec![], interaction_id: question(n).as_str().into(), scope: Scope::Task { task_id: "t".into() },
                        recipient: alondra_core::domain::Recipient::Orchestrator, body: format!("Where does change {n} go?"),
                        blocking: true, options: vec![
                            QuestionOption { id: "done".into(), label: "Done".into(), description: None },
                            QuestionOption { id: "no".into(), label: "No".into(), description: None },
                        ],
                        card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Decide, "Where does it go?").why("It decides the branch"),
                    }),
                    Op::Answer(n) if exists(n) => execute(Actor::Orchestrator { generation: 0 }, Command::AnswerQuestion {
                        attachments: vec![], interaction_id: question(n).as_str().into(), option_id: "done".into(), inputs: Default::default(),
                    }),
                    Op::Withdraw(n) if exists(n) => execute(Actor::Agent { agent_id: "a".into() }, Command::Withdraw { interaction_id: question(n).as_str().into() }),
                    Op::BatchEnds => settle(&domain),
                    Op::NodeStep => node_step(&domain),
                    Op::Minutes(minutes) => {
                        clock += minutes * MINUTE;
                        settle_overdue(&domain, clock);
                    }
                    _ => {}
                }
                proptest::prop_assert!(user_cards(&domain).is_empty(), "a card for the user: {:?}", user_cards(&domain));
            }
            for _ in 0..12 {
                node_step(&domain);
                clock += 5 * MINUTE;
                settle_overdue(&domain, clock);
            }
            let after = domain.state().unwrap();
            let open: Vec<_> = after.interactions.values().filter(|i| DomainState::asks_the_orchestrator(i) && after.open(i)).map(|i| i.id.clone()).collect();
            proptest::prop_assert!(open.is_empty(), "{open:?} outlived its deadlines");
        }
    }
}
