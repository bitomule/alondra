//! Nothing waits in the orchestrator, and no clock decides it. Once a turn of the orchestrator
//! ended for real (handled, not stopped, refused or held by the usage limit) and no message waits
//! for it, every item it owes that has no outcome yet goes back to it once, at once, as a
//! reminder; the next turn reads it. When that turn too ends handled and the item still has no
//! outcome, it leaves the orchestrator:
//!
//! - an open request of the user's becomes the user's Problem; a message it acted on closes;
//! - a request whose tasks all ended is reminded twice, then Alondra closes it with its Done card;
//! - a node's question is withdrawn and its node told to decide by itself;
//! - a node's suggestion is discarded with its reason;
//! - the review of the plans a decision holds releases each with the plan it has;
//! - a stopped node's case is settled by Alondra: the node goes on, or the task closes.
//!
//! Only a request reaches the user. Every outcome above is a fact of the domain the orchestrator
//! reaches with its own tools, so the rule reads state only: the reminder is a card of the inbox,
//! and its notice was read only once a batch that ended with its result was handed it. A reminder
//! whose notice expired, or whose batch was stopped, discarded or given up, was never read: the
//! next settle sends another instead of acting on it. A review the orchestrator asked the user
//! about waits for that answer.
//!
//! A task that waits for a cancelled task, and a launch plan the start refused, are the
//! orchestrator's to decide, so no clock settles them and no card asks the user. Alondra reads the
//! orchestrator's own state instead: every time it rests (a turn ended, handled or failed, or it
//! is idle) the open item is handed to it again, once per rest.
use crate::engine::OrchestratorOf as _;
use crate::domain_runtime::DomainRuntime;
use alondra_engine_ports::Runtime;
use alondra_domain_rules::obligation_policy::Millis;
use alondra_domain_rules::{
    Author, CardKind, CardSpec, Command, DomainState, InboxCard, Interaction, InteractionId, InteractionPayload, PlanReview, PlanReviewOutcome, QuestionOption,
    Request, RequestId, STALL_ATTEMPTS, Scope, StallOutcome, StartProblem, StartProblems, SuggestionDecision, SystemCard, Task, TaskId, UNSETTLED_INSTRUCT,
};
use alondra_domain_rules::orchestrator_cards::{
    DEPENDENCY_CANCELLED_NOTICE_PREFIX, UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL, UNSPLIT_NOTICE_PREFIX,
    UNSPLIT_PROBLEM_PREFIX, UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL, UnsplitProblem,
    PLAN_REVIEW_NOTICE_PREFIX, PLAN_REFUSED_NOTICE_PREFIX, STALL_NOTICE_PREFIX, TASKS_ENDED_NOTICE_PREFIX,
};

const UNSPLIT_CAUSE: &str = "request_unsplit";
const BODY_SHOWN: usize = 2_000;

#[derive(Debug, PartialEq, Eq)]
pub enum Step {
    Close { request: RequestId, withdrawn: bool },
    Remind { request: RequestId, id: String, card: SystemCard, text: String },
    Raise { request: RequestId, id: String, card: SystemCard, problem: UnsplitProblem },
    /// A reminder about a node's question or suggestion.
    Tell { id: String, card: SystemCard, text: String },
    /// A card that no longer asks anything: a node's question the orchestrator left unanswered
    /// past its reminder, whose node is told to decide by itself, or a problem the user no longer
    /// has to settle.
    Withdraw { question: InteractionId },
    /// A reminder about a decision of the user whose held plans are not reviewed yet.
    TellPlanReview { id: String, card: SystemCard, review: InteractionId, number: u32 },
    /// The orchestrator left the plan of a held task unreviewed past its reminder: it keeps its
    /// plan, and its node reads first what was decided.
    ReleaseUnreviewed { review: InteractionId, task: TaskId, note: String },
    /// The case of a stopped node is still unsettled past its reminder: Alondra settles it.
    SettleStall { case: InteractionId, task: TaskId, outcome: StallOutcome },
    /// A node's suggestion still without a decision past its reminder: Alondra discards it.
    Discard { suggestion: InteractionId },
}

/// What is left in the orchestrator's inbox after a turn of it ended handled and it was handed
/// everything waiting for it.
pub(crate) fn steps(state: &DomainState, read: &dyn Fn(&str) -> bool) -> Vec<Step> {
    let mut steps = owed(state, read);
    for problem in state.interactions.values().filter(|i| retired_problem(i) && state.open(i)) {
        steps.push(Step::Withdraw { question: problem.id.clone() });
    }
    for request in state.requests.values().filter(|request| !request.closed) {
        if asked_about(state, &request.id) {
            continue;
        }
        match tasks_of(state, &request.id) {
            Tasks::Open => continue,
            Tasks::Ended => {
                if !request.message {
                    steps.push(tasks_ended(state, request, read));
                }
                continue;
            }
            Tasks::None => {}
        }
        if request.message {
            steps.push(Step::Close { request: request.id.clone(), withdrawn: false });
            continue;
        }
        steps.extend(unsplit(state, request, read));
    }
    steps
}

/// What the orchestrator owes the nodes of live tasks and has no outcome yet: a reminder until
/// one was `read` by a batch that ended with its result, then Alondra's own outcome. Called only
/// when a turn ended handled and nothing waits for the orchestrator, never by a clock.
pub fn owed(state: &DomainState, read: &dyn Fn(&str) -> bool) -> Vec<Step> {
    let live = |i: &Interaction| task_of(state, i).filter(|task| !task.over());
    let mut steps = vec![];
    for suggestion in state.interactions.values().filter(|i| waits(i) && i.card.kind == CardKind::Propose) {
        if live(suggestion).is_some() {
            steps.push(unsettled(state, suggestion, read));
        }
    }
    for case in state.interactions.values().filter(|i| state.unsettled_stall(i)) {
        if let Some(task) = live(case) {
            steps.push(stalled(state, case, task, read));
        }
    }
    for question in state.interactions.values().filter(|i| waits(i) && i.card.kind != CardKind::Propose) {
        if live(question).is_some() {
            steps.push(unanswered(state, question, read));
        }
    }
    for (id, review) in &state.plan_reviews {
        let held: Vec<&Task> = state.plan_review_waiting(review).collect();
        if held.is_empty() || held.iter().any(|task| asked_about(state, &task.request_id)) {
            continue;
        }
        steps.extend(unreviewed(state, id, held.into_iter().map(|task| task.id.clone()).collect(), read));
    }
    steps
}

/// How many times one re-plan is handed to the orchestrator. Past it the task keeps saying the
/// orchestrator decides, and no batch is spent on it until what it waits on changes: another
/// cancelled dependency or another refusal is another subject.
pub const REPLAN_REMINDERS: usize = 3;

/// What the orchestrator has still to re-plan when it rests, `rest` being how many turns it has
/// ended for other work: a task that waits for a cancelled task, and a launch plan the start
/// refused. Each is handed to it once per rest, with the open item, at most `REPLAN_REMINDERS`
/// times; Alondra never decides it, and no clock counts.
pub fn replans(state: &DomainState, rest: u64) -> Vec<Step> {
    let mut problems = None;
    state.tasks.values().filter(|task| !task.over() && !task.archived)
        .filter_map(|task| {
            let problems = problems.get_or_insert_with(|| StartProblems::new(state));
            dependency_cancelled(state, problems, task, rest).or_else(|| plan_refused(state, problems, task, rest))
        })
        .collect()
}

/// Whether a batch carried only re-plan reminders, by the cards its messages came from: its end is
/// no rest, or each reminder would start the batch that hands the next one.
pub fn only_replan_reminders<'a>(cards: impl IntoIterator<Item = Option<&'a SystemCard>>) -> bool {
    let mut carried = false;
    for card in cards {
        if !matches!(card, Some(SystemCard::Inbox { about: InboxCard::DependencyCancelledNotice | InboxCard::PlanRefusedNotice, .. })) {
            return false;
        }
        carried = true;
    }
    carried
}

/// Hands the orchestrator what it has still to re-plan. Called each time it rests, whatever its
/// last turn did, and whether or not other messages wait for it: it only adds a reminder.
pub fn hand_replans(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    let rest = domain.orchestrator_rests.load(std::sync::atomic::Ordering::Relaxed);
    for step in replans(&state, rest) {
        carry_out(domain, &state, &step);
    }
}

/// How many reminders a request whose tasks all ended gets before Alondra closes it.
pub const TASKS_ENDED_REMINDERS: u32 = 2;

enum Tasks {
    None,
    Open,
    Ended,
}

fn tasks_of(state: &DomainState, request: &RequestId) -> Tasks {
    let mut tasks = state.tasks.values().filter(|task| task.request_id == *request).peekable();
    if tasks.peek().is_none() {
        return Tasks::None;
    }
    if tasks.all(Task::over) { Tasks::Ended } else { Tasks::Open }
}

/// A request whose tasks all ended and that the orchestrator left open: it is reminded until
/// `TASKS_ENDED_REMINDERS` reminders were read by batches that ended with their result, then
/// Alondra closes it, and the user gets its Done card summing up each task.
fn tasks_ended(state: &DomainState, request: &Request, read: &dyn Fn(&str) -> bool) -> Step {
    let reminders = reminded(state, TASKS_ENDED_NOTICE_PREFIX, InboxCard::TasksEndedNotice, &request.id.0, read);
    if reminders.read >= TASKS_ENDED_REMINDERS {
        return Step::Close { request: request.id.clone(), withdrawn: false };
    }
    let left = TASKS_ENDED_REMINDERS - reminders.read;
    let text = format!(
        "{}every Task of Request {} has ended and the Request is still open. Close it now with close_request, outcome tasks_ended, its answer saying what came of it; if it still needs work, create its next Task. If {} still leave it open, Alondra closes it itself with a card listing its Tasks and their results, without your answer.",
        reminder_of(reminders.next), request.id, if left == 1 { "this turn ends and you".to_owned() } else { format!("{left} more turns end and you") });
    let (id, card) = inbox_card(TASKS_ENDED_NOTICE_PREFIX, InboxCard::TasksEndedNotice, &request.id.0, reminders.next);
    Step::Remind { request: request.id.clone(), id, card, text }
}

/// The problems Alondra no longer raises: what they asked the user is settled by time now.
fn retired_problem(i: &Interaction) -> bool {
    i.author == Author::System
        && matches!(i.system_card, Some(SystemCard::Inbox { about: InboxCard::UnansweredProblem | InboxCard::PlanReviewProblem, .. }))
}

/// What a handled batch left without an outcome, as `request <id>` or `suggestion <id>`: one of
/// its open requests with no task or every task ended, no open question of the orchestrator, and neither a reminder on
/// its way back nor the user's problem; one of its node suggestions with no decision and no
/// reminder. `settle` leaves it empty.
pub(crate) fn unresolved(state: &DomainState, requests: &[RequestId], suggestions: &[InteractionId]) -> Vec<String> {
    let mut left = vec![];
    for id in requests {
        let Some(request) = state.requests.get(id) else { continue };
        if request.closed
            || matches!(tasks_of(state, id), Tasks::Open)
            || asked_about(state, id)
            || !numbered(state, InboxCard::TasksEndedNotice, &id.0).is_empty()
            || !numbered(state, InboxCard::UnsplitNotice, &id.0).is_empty()
            || !numbered(state, InboxCard::UnsplitProblem, &id.0).is_empty()
        {
            continue;
        }
        left.push(format!("request {id}"));
    }
    for id in suggestions {
        let Some(suggestion) = state.interactions.get(id) else { continue };
        if !waits(suggestion) || !numbered(state, InboxCard::UnsettledNotice, &id.0).is_empty() {
            continue;
        }
        left.push(format!("suggestion {id}"));
    }
    left
}

/// The most items the ledger of a turn lists, and the longest line of each.
const LEDGER_ITEMS: usize = 60;
const LEDGER_LINE: usize = 200;

/// What the orchestrator owes now, one item each: what a turn that resumes its conversation reads
/// instead of the whole state, which the conversation already holds an earlier copy of. A request
/// with no outcome comes with what it asks, as the user wrote it.
pub fn ledger(state: &DomainState, with_requests: bool) -> Vec<String> {
    let title = |i: &Interaction| i.card.effective_title().to_owned();
    let of_task = |i: &Interaction| task_of(state, i).map_or(String::new(), |task| format!(" of task {} “{}”", task.id, task.title));
    let mut requests = vec![];
    for request in state.requests.values().filter(|request| with_requests && !request.closed) {
        if state.tasks.values().any(|task| task.request_id == request.id) || asked_about(state, &request.id) || handed_to_user(state, &request.id) {
            continue;
        }
        let what = if request.maintenance { "maintenance message" } else if request.message { "message" } else { "request" };
        let body: String = request.body.trim().chars().take(BODY_SHOWN).collect();
        requests.push(format!("{what} {} (no task of its own, no outcome yet):\n{body}", request.id));
    }
    let mut lines = vec![];
    for i in state.interactions.values().filter(|i| waits(i)) {
        let what = if i.card.kind == CardKind::Propose { "suggestion" } else { "question" };
        if task_of(state, i).is_some_and(|task| !task.over()) {
            lines.push(format!("{what} {}{}: {}", i.id, of_task(i), title(i)));
        }
    }
    for case in state.interactions.values().filter(|i| state.unsettled_stall(i)) {
        if task_of(state, case).is_some_and(|task| !task.over()) {
            lines.push(format!("stopped node case {}{}", case.id, of_task(case)));
        }
    }
    for (id, review) in &state.plan_reviews {
        let held: Vec<String> = state.plan_review_waiting(review).map(|task| task.id.0.clone()).collect();
        if !held.is_empty() {
            lines.push(format!("plan review {id}: the plans of {} wait for your review", held.join(", ")));
        }
    }
    let total = requests.len() + lines.len();
    let mut shown: Vec<String> = requests.into_iter().take(LEDGER_ITEMS).collect();
    let room = LEDGER_ITEMS.saturating_sub(shown.len());
    shown.extend(lines.into_iter().take(room).map(|line| line.chars().take(LEDGER_LINE).collect::<String>()));
    if total > shown.len() {
        shown.push(format!("… and {} more: tasks_in_progress lists them", total - shown.len()));
    }
    shown
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

/// The orchestrator left this request without an outcome past its reminder, and it waits with the
/// user as a problem card: it is the user's now, not something the orchestrator owes.
pub(crate) fn handed_to_user(state: &DomainState, request: &RequestId) -> bool {
    numbered(state, InboxCard::UnsplitProblem, &request.0).iter().any(|(_, problem)| state.open(problem))
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
fn numbered<'a>(state: &'a DomainState, kind: InboxCard, of: &str) -> Vec<(u32, &'a alondra_domain_rules::Interaction)> {
    state.interactions.values()
        .filter_map(|i| match &i.system_card {
            Some(SystemCard::Inbox { about, subject, number }) if *about == kind && subject == of => Some((*number, i)),
            _ => None,
        })
        .collect()
}

/// The reminders of one kind about `subject`: the number the next one takes, and how many were
/// read by a batch that ended with its result. A reminder an older daemon wrote under an id with
/// no number never counts as read: it was written under another rule.
struct Reminded {
    next: u32,
    read: u32,
}

fn reminded(state: &DomainState, prefix: &str, kind: InboxCard, subject: &str, read: &dyn Fn(&str) -> bool) -> Reminded {
    let cards = numbered(state, kind, subject);
    let next = cards.iter().map(|(number, _)| *number).max().unwrap_or(0) + 1;
    let read = cards.iter().filter(|(number, card)| card.id.0 == inbox_card(prefix, kind, subject, *number).0 && read(&card.id.0)).count() as u32;
    Reminded { next, read }
}

/// How the `number`th reminder about something opens: each says which it is, so a reminder sent
/// again after the last one was never read is a new notice, not the same one repeated.
fn reminder_of(number: u32) -> String {
    if number <= 1 { "Reminder: ".to_owned() } else { format!("Reminder {number}, the last one never reached you: ") }
}

fn numbered_text(number: u32, text: String) -> String {
    if number <= 1 { text } else { format!("{}{text}", reminder_of(number)) }
}

/// The id and the kind of the `number`th card of the inbox about `subject`.
fn inbox_card(prefix: &str, about: InboxCard, subject: &str, number: u32) -> (String, SystemCard) {
    (format!("{prefix}{subject}-{number}"), SystemCard::Inbox { about, subject: subject.to_owned(), number })
}

fn unsplit(state: &DomainState, request: &Request, read: &dyn Fn(&str) -> bool) -> Option<Step> {
    let reminders = reminded(state, UNSPLIT_NOTICE_PREFIX, InboxCard::UnsplitNotice, &request.id.0, read);
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
    if reminders.read > raised {
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
    let (id, card) = inbox_card(UNSPLIT_NOTICE_PREFIX, InboxCard::UnsplitNotice, &request.id.0, reminders.next);
    Some(Step::Remind { request: request.id.clone(), id, card, text: numbered_text(reminders.next, text) })
}

pub(crate) use crate::domain_store::wording::approved_plan_work;

fn task_of<'a>(state: &'a DomainState, i: &Interaction) -> Option<&'a alondra_domain_rules::Task> {
    match &i.scope {
        Scope::Task { task_id } => state.tasks.get(task_id),
        Scope::Orchestrator { .. } => None,
    }
}

/// A node's question or suggestion to the orchestrator still without its answer.
fn waits(i: &Interaction) -> bool {
    matches!(i.author, Author::Agent { .. })
        && i.recipient == alondra_domain_rules::Recipient::Orchestrator
        && !i.withdrawn
        && matches!(&i.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. })
}

fn quoted(i: &Interaction) -> String {
    let body: String = i.body.trim().chars().take(BODY_SHOWN).collect();
    format!("\"{}\"\n{body}", i.card.effective_title())
}

/// A node's suggestion without a decision: the orchestrator is reminded, then Alondra discards it.
fn unsettled(state: &DomainState, suggestion: &Interaction, read: &dyn Fn(&str) -> bool) -> Step {
    let reminders = reminded(state, UNSETTLED_NOTICE_PREFIX, InboxCard::UnsettledNotice, &suggestion.id.0, read);
    if reminders.read > 0 {
        return Step::Discard { suggestion: suggestion.id.clone() };
    }
    let (id, card) = inbox_card(UNSETTLED_NOTICE_PREFIX, InboxCard::UnsettledNotice, &suggestion.id.0, reminders.next);
    Step::Tell {
        id,
        card,
        text: format!(
            "{}suggestion {} from a node has no decision. Settle it now with settle_suggestion: new_task (the call creates it), current_task or discard with the reason. If this turn ends with it still unsettled, Alondra discards it.\nSuggestion: {}",
            reminder_of(reminders.next), suggestion.id, quoted(suggestion)),
    }
}

/// Why Alondra discards a suggestion the orchestrator left without a decision past its reminder.
pub const UNSETTLED_DISCARD: &str = "The orchestrator did not settle this suggestion after a reminder, so Alondra discarded it. Raise it again if it still matters.";

/// A delivered node's case left unsettled keeps the delivery with the user; any other gets the
/// node back to work while it has attempts left, or while a message of the user's waits for it,
/// and closes the task after.
fn stalled(state: &DomainState, case: &Interaction, task: &Task, read: &dyn Fn(&str) -> bool) -> Step {
    let reminders = reminded(state, STALL_NOTICE_PREFIX, InboxCard::StallNotice, &case.id.0, read);
    if reminders.read > 0 {
        let delivered = state.lifecycle_of(&task.id).is_ok_and(|lifecycle| lifecycle.phase() == alondra_domain::TaskPhase::Delivered);
        let goes_on = task.stalls <= STALL_ATTEMPTS && !delivered;
        let outcome = if goes_on { StallOutcome::Instruct { text: UNSETTLED_INSTRUCT.into() } } else { StallOutcome::Unsettled };
        return Step::SettleStall { case: case.id.clone(), task: task.id.clone(), outcome };
    }
    let (id, card) = inbox_card(STALL_NOTICE_PREFIX, InboxCard::StallNotice, &case.id.0, reminders.next);
    Step::Tell {
        id,
        card,
        text: format!(
            "{}case {} about the stopped node of Task “{}” ({}) is still unsettled. Settle it now with settle_stall: instruct with one concrete next step, or close with the result. If this turn ends with it still unsettled, Alondra settles it itself: the node is told to go on, or, after {STALL_ATTEMPTS} cases, the task closes without a result of yours. Never ask the user about it.\nCase: {}",
            reminder_of(reminders.next), case.id, task.title, task.id, quoted(case)),
    }
}

/// A launch plan the start refused is the orchestrator's to fix or to cancel with its reason.
fn plan_refused(state: &DomainState, problems: &StartProblems<'_>, task: &Task, rest: u64) -> Option<Step> {
    let card = problems.plan_refusal(&task.id)?;
    let (id, notice) = inbox_card(PLAN_REFUSED_NOTICE_PREFIX, InboxCard::PlanRefusedNotice, &card.id.0, rest as u32);
    let told = numbered(state, InboxCard::PlanRefusedNotice, &card.id.0).len();
    if told >= REPLAN_REMINDERS || state.interactions.contains_key(&InteractionId(id.clone())) {
        return None;
    }
    Some(Step::Tell {
        id,
        card: notice,
        text: format!(
            "{}The launch plan of Task “{}” ({}) was refused and is still as it was, so the task cannot start. It is yours to decide: fix it with update_launch_plan or set_launch_plan, or answer {} with option cancel and the reason in inputs.text. Never ask the user about it.\nCard: {}",
            if told == 0 { String::new() } else { format!("Still open, reminder {told}: ") }, task.title, task.id, card.id, quoted(card)),
    })
}

/// A node's question without an answer: the orchestrator is reminded, then the question is
/// withdrawn and its node told to decide by itself.
fn unanswered(state: &DomainState, question: &Interaction, read: &dyn Fn(&str) -> bool) -> Step {
    let reminders = reminded(state, UNANSWERED_NOTICE_PREFIX, InboxCard::UnansweredNotice, &question.id.0, read);
    if reminders.read > 0 {
        return Step::Withdraw { question: question.id.clone() };
    }
    let (id, card) = inbox_card(UNANSWERED_NOTICE_PREFIX, InboxCard::UnansweredNotice, &question.id.0, reminders.next);
    Step::Tell {
        id,
        card,
        text: format!(
            "{}question {} from a node has no answer: its node stands still until you answer. Answer it now with answer_question, also if what it asked for is already done (say so). An answer written only in this conversation never reaches the node. If this turn ends with it still unanswered, Alondra withdraws it and tells the node to decide by itself.\nQuestion: {}",
            reminder_of(reminders.next), question.id, quoted(question)),
    }
}

/// The held tasks of a review the orchestrator has not settled: it is reminded, then each starts
/// with the plan it had.
fn unreviewed(state: &DomainState, id: &InteractionId, held: Vec<TaskId>, read: &dyn Fn(&str) -> bool) -> Vec<Step> {
    let reminders = reminded(state, PLAN_REVIEW_NOTICE_PREFIX, InboxCard::PlanReviewNotice, &id.0, read);
    if reminders.read == 0 {
        let (notice, card) = inbox_card(PLAN_REVIEW_NOTICE_PREFIX, InboxCard::PlanReviewNotice, &id.0, reminders.next);
        return vec![Step::TellPlanReview { id: notice, card, review: id.clone(), number: reminders.next }];
    }
    let Some(note) = state.unreviewed_plan_note(id) else { return vec![] };
    held.into_iter().map(|task| Step::ReleaseUnreviewed { review: id.clone(), task, note: note.clone() }).collect()
}

/// A task that waits for a cancelled task is the orchestrator's to decide: cancel it, re-point it
/// or let it start without the dependency. Once it no longer waits for that task, a card from
/// before still asking the user about it is withdrawn.
fn dependency_cancelled(state: &DomainState, problems: &StartProblems<'_>, task: &Task, rest: u64) -> Option<Step> {
    if let Some(card) = problems.open(&task.id)
        && let Some(StartProblem::DependencyCancelled { dependency }) = alondra_domain_rules::start_problem::from_body(&card.body)
        && state.cancelled_dependency(task).as_ref() != Some(&dependency)
    {
        return Some(Step::Withdraw { question: card.id.clone() });
    }
    let dependency = problems.replanned_for(task)?;
    let subject = format!("{}--{dependency}", task.id);
    let (id, card) = inbox_card(DEPENDENCY_CANCELLED_NOTICE_PREFIX, InboxCard::DependencyCancelledNotice, &subject, rest as u32);
    let told = numbered(state, InboxCard::DependencyCancelledNotice, &subject).len();
    if told >= REPLAN_REMINDERS || state.interactions.contains_key(&InteractionId(id.clone())) {
        return None;
    }
    let other = state.tasks.get(&dependency).map_or("", |t| t.title.as_str());
    let kept: Vec<&str> = task.dependencies.iter().filter(|d| **d != dependency).map(|d| d.0.as_str()).collect();
    let kept = if kept.is_empty() { "none".to_owned() } else { kept.join(", ") };
    Some(Step::Tell {
        id,
        card,
        text: format!(
            "{}Task “{}” ({}) waits for “{other}” ({dependency}), which was cancelled, so it does not start. Cancelling a task does not \
             always mean its dependents must be cancelled: it is yours to decide, and nobody else will. Do one of these now, giving \
             the reason, which the task shows and its node reads: if another task replaces “{other}”, make it depend on that one \
             with set_dependencies, keeping its other dependencies ({kept}); if it can go on without it, set_dependencies without \
             {dependency}, and update_launch_plan if its mission relied on that task's result; if it no longer makes sense, cancel_task. \
             Never ask the user about it.",
            if told == 0 { String::new() } else { format!("Still open, reminder {told}: ") }, task.title, task.id),
    })
}

/// Carries the steps out; one that fails is tried again on the next pass.
pub fn settle(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    let read = |interaction: &str| domain.store.lock().map_or(false, |store| store.orchestrator_notice_read(interaction).unwrap_or(false));
    for step in steps(&state, &read) {
        carry_out(domain, &state, &step);
    }
}

/// Steps the daemon's one clock at `now`, on every pass of the orchestrator, busy or failing:
/// what Alondra owes the nodes. Nothing the orchestrator owes is on it.
pub fn settle_overdue(domain: &DomainRuntime, now: Millis) {
    domain.engine().settle_overdue(now);
}

/// The command that carries out `step`, for the steps one command settles.
pub fn command_of(step: &Step) -> Option<Command> {
    Some(match step {
        Step::Tell { id, card, text } | Step::Remind { id, card, text, .. } => alondra_engine_ports::runtime::tell_command(id, Some(card.clone()), text.clone()),
        Step::Withdraw { question } => Command::Withdraw { interaction_id: question.clone() },
        Step::SettleStall { case, task, outcome } => Command::SettleStall { case_id: case.clone(), task_id: task.clone(), outcome: outcome.clone() },
        Step::ReleaseUnreviewed { review, task, note } => Command::SettlePlanReview {
            review_id: review.clone(), task_id: task.clone(), outcome: PlanReviewOutcome::Unchanged { reason: note.clone() },
        },
        Step::Discard { suggestion } => discard(suggestion),
        _ => return None,
    })
}

/// Whether `step` was carried out.
fn carry_out(runtime: &dyn Runtime, state: &DomainState, step: &Step) -> bool {
    let current = runtime.state();
    let (command_id, command) = match step {
        Step::Close { request, withdrawn } => (format!("settle-{request}"), Command::CloseRequest { covered_by: vec![], outcome: None,
            request_id: request.clone(), answer: None, withdrawn: *withdrawn, title: None, headline: None, folded_into: None,
        }),
        Step::Remind { id, card, text, .. } | Step::Tell { id, card, text } => (format!("tell-{id}"), told(id, card, text.clone())),
        Step::Raise { request, id, card, problem } => {
            let (need, why) = problem.text(alondra_view_model::Lang::En);
            let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
            let mut spec = CardSpec::new(CardKind::Problem, need).why(why);
            spec.recommended = Some(UNSPLIT_RETRY.to_owned());
            spec.system = Some(card.clone());
            (format!("raise-{id}"), Command::RaiseProblem {
                attachments: vec![], interaction_id: id.clone().into(), scope: Scope::Orchestrator { request_id: Some(request.clone()) },
                body: problem.to_body(), options: vec![option(UNSPLIT_RETRY, UNSPLIT_RETRY_LABEL), option(UNSPLIT_CLOSE, UNSPLIT_CLOSE_LABEL)],
                card: spec.cause(UNSPLIT_CAUSE),
            })
        }
        Step::Withdraw { question } => (format!("withdraw-{question}-{}", current.revision), Command::Withdraw { interaction_id: question.clone() }),
        Step::TellPlanReview { id, card, review, number } => {
            let plan_of = |task: &alondra_domain_rules::TaskId| runtime.launch_plan(task);
            let text = format!("Reminder {number}: these plans are still not reviewed. If this turn ends with them still not reviewed, Alondra starts each with the plan it has and tells its node what the user decided. {}",
                crate::domain_store::wording::orchestrator_text(state, review, &plan_of));
            (format!("tell-{id}"), told(id, card, text))
        }
        Step::ReleaseUnreviewed { review, task, note } => (format!("settle-{review}-{task}"), Command::SettlePlanReview {
            review_id: review.clone(), task_id: task.clone(), outcome: PlanReviewOutcome::Unchanged { reason: note.clone() },
        }),
        Step::SettleStall { case, task, outcome } => (format!("settle-{case}"), Command::SettleStall { case_id: case.clone(), task_id: task.clone(), outcome: outcome.clone() }),
        Step::Discard { suggestion } => (format!("discard-{suggestion}"), discard(suggestion)),
    };
    let done = if creates_card(step) && current.interactions.contains_key(&InteractionId(card_id(step).to_owned())) {
        Err(alondra_domain_rules::DomainError::new("invalid_input", "Already told"))
    } else {
        runtime.system_command(alondra_domain_rules::CommandEnvelope {
            protocol_version: alondra_domain_rules::PROTOCOL_VERSION, data_epoch: current.data_epoch.clone(),
            command_id: command_id.into(), expected_revision: current.revision, command,
        })
    };
    if let Err(error) = &done {
        log!("could not settle the orchestrator's inbox ({step:?}): {error:?}");
    }
    done.is_ok()
}

fn discard(suggestion: &InteractionId) -> Command {
    Command::SettleSuggestion { interaction_id: suggestion.clone(), decision: SuggestionDecision::Discard { reason: UNSETTLED_DISCARD.to_owned() } }
}

fn told(id: &str, card: &SystemCard, text: String) -> Command {
    alondra_engine_ports::runtime::tell_command(id, Some(card.clone()), text)
}

/// Whether `step` creates the card its id names, which must not exist yet.
fn creates_card(step: &Step) -> bool {
    matches!(step, Step::Remind { .. } | Step::Tell { .. } | Step::TellPlanReview { .. } | Step::Raise { .. })
}

fn card_id(step: &Step) -> &str {
    match step {
        Step::Remind { id, .. } | Step::Tell { id, .. } | Step::TellPlanReview { id, .. } | Step::Raise { id, .. } => id,
        _ => "",
    }
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;
    use alondra_domain_rules::{Actor, Command, CommandEnvelope, InteractionPayload, PROTOCOL_VERSION};
    use alondra_domain_rules::orchestrator_cards::UNANSWERED_NOTICE_PREFIX;
    use alondra_domain::{Owed, Party};

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

    /// A batch of the orchestrator read every message waiting for it and ended with its result:
    /// what it owes is settled.
    fn turn_ends(domain: &DomainRuntime) {
        hand(domain);
        settle(domain);
    }

    /// Every message waiting for the orchestrator was handed to a batch that ended with its result.
    fn hand(domain: &DomainRuntime) {
        let mut store = domain.store.lock().unwrap();
        for message in store.pending_messages().unwrap().into_iter().filter(|message| message.recipient_kind == "orchestrator") {
            if let Some(claim) = store.claim_message(message.id).unwrap() {
                store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap();
            }
        }
    }

    fn cards(domain: &DomainRuntime, prefix: &str) -> Vec<String> {
        domain.state().unwrap().interactions.keys().filter(|id| id.0.starts_with(prefix)).map(|id| id.0.clone()).collect()
    }

    /// 8 October: three messages the orchestrator acted on stayed open with no task for ever.
    #[test]
    fn a_message_acted_on_without_tasks_closes_once_the_orchestrator_is_done() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        request(&domain, "m-1", true);
        turn_ends(&domain);
        turn_ends(&domain);
        assert!(closed(&domain, "m-1"));
        assert!(cards(&domain, UNSPLIT_NOTICE_PREFIX).is_empty(), "a message is never sent back");
    }

    #[test]
    fn a_request_left_without_tasks_goes_back_once_then_becomes_the_users_problem() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        request(&domain, "r-1", false);
        turn_ends(&domain);
        assert_eq!(cards(&domain, UNSPLIT_NOTICE_PREFIX), [format!("{UNSPLIT_NOTICE_PREFIX}r-1-1")]);
        assert!(cards(&domain, UNSPLIT_PROBLEM_PREFIX).is_empty(), "the orchestrator gets one more go first");
        turn_ends(&domain);
        let problem = format!("{UNSPLIT_PROBLEM_PREFIX}r-1-1");
        assert_eq!(cards(&domain, UNSPLIT_PROBLEM_PREFIX), [problem.clone()]);
        turn_ends(&domain);
        assert_eq!(cards(&domain, UNSPLIT_PROBLEM_PREFIX).len(), 1, "an open problem is not raised twice");
        assert!(!closed(&domain, "r-1"), "a request of the user's never closes by itself");
        run(&domain, Actor::User { user_id: "u".into() }, "close-it", Command::AnswerQuestion {
            attachments: vec![], interaction_id: problem.as_str().into(), option_id: UNSPLIT_CLOSE.into(), inputs: Default::default(),
        });
        turn_ends(&domain);
        let state = domain.state().unwrap();
        assert!(state.requests[&"r-1".into()].closed && state.requests[&"r-1".into()].withdrawn);
    }

    #[test]
    fn a_request_with_a_task_or_an_open_question_of_the_orchestrator_is_left_alone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        request(&domain, "asked", false);
        request(&domain, "split", true);
        run(&domain, Actor::Orchestrator { generation: 0 }, "ask", Command::AskQuestion {
            attachments: vec![], interaction_id: "q-1".into(), scope: Scope::Orchestrator { request_id: Some("asked".into()) },
            recipient: alondra_domain_rules::Recipient::User, body: "Which app?".into(), blocking: true,
            options: vec![
                QuestionOption { id: "a".into(), label: "A".into(), description: None },
                QuestionOption { id: "b".into(), label: "B".into(), description: None },
            ],
            card: alondra_domain_rules::CardSpec::derived(alondra_domain_rules::CardKind::Decide, "Pick the app"),
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: "t-1".into(), request_id: "split".into(), title: "Task".into(), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        assert_eq!(steps(&domain.state().unwrap(), &|_| true), []);
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
        let domain = crate::wiring::open(&home).unwrap();
        request(&domain, "solo", false);
        request(&domain, "pair", false);
        let create = |id: &str, request: &str, dependencies: &[&str]| Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: id.into(), request_id: request.into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        };
        run(&domain, Actor::Orchestrator { generation: 0 }, "alone", create("alone", "solo", &[]));
        run(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "pair".into(), title: "Pair".into() });
        run(&domain, Actor::Orchestrator { generation: 0 }, "first", create("first", "pair", &[]));
        run(&domain, Actor::Orchestrator { generation: 0 }, "second", create("second", "pair", &["first"]));
        let before = orchestrator_messages(&domain);
        run(&domain, Actor::User { user_id: "u".into() }, "cancel-alone", Command::CancelTask { task_id: "alone".into(), reason: None });
        assert_eq!(orchestrator_messages(&domain), before, "the cancel leaves no message for the orchestrator");
        let state = domain.state().unwrap();
        assert!(state.requests[&"solo".into()].closed && state.requests[&"solo".into()].withdrawn);
        assert_eq!(steps(&state, &|_| true), []);

        run(&domain, Actor::User { user_id: "u".into() }, "cancel-first", Command::CancelTask { task_id: "first".into(), reason: None });
        let state = domain.state().unwrap();
        assert!(!state.requests[&"pair".into()].closed, "its other task still waits");
        assert!(state.open_start_problem(&"second".into()).is_none(), "the user is not asked first");
        let owed = state.obligation_of(&"second".into()).expect("it is still owed something");
        assert_eq!((owed.party, owed.owed), (Party::Alondra, Owed::Replan), "the orchestrator re-plans the task that waited for it");
        assert!(matches!(&replans(&state, 0)[..], [Step::Tell { text, .. }] if text.contains("it is yours to decide")), "{:?}", replans(&state, 0));
        assert_eq!(super::owed(&state, &|_| true), [], "nothing settles it for the orchestrator");
    }

    fn rest(domain: &DomainRuntime) {
        domain.orchestrator_rests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        hand_replans(domain);
    }

    fn pair_of_tasks(domain: &DomainRuntime, third: bool) {
        request(domain, "pair", false);
        let create = |id: &str, dependencies: &[&str]| Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: id.into(), request_id: "pair".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: dependencies.iter().map(|d| TaskId::from(*d)).collect(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        };
        run(domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "pair".into(), title: "Pair".into() });
        run(domain, Actor::Orchestrator { generation: 0 }, "first", create("first", &[]));
        if third {
            run(domain, Actor::Orchestrator { generation: 0 }, "other", create("other", &[]));
        }
        run(domain, Actor::Orchestrator { generation: 0 }, "second", create("second", &["first"]));
        run(domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "first".into(), reason: None });
    }

    #[test]
    fn replanning_reads_start_problem_history_once_for_all_tasks() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        pair_of_tasks(&domain, true);
        hand_replans(&domain);
        let mut state = (*domain.state().unwrap()).clone();
        let template = state.tasks[&TaskId::from("other")].clone();
        let card = state.interactions.values().next().unwrap().clone();
        state.tasks.clear();
        state.interactions.clear();
        for n in 0..40 {
            let id = TaskId::from(format!("task-{n}").as_str());
            state.tasks.insert(id.clone(), Task { id, ..template.clone() });
            let id = InteractionId::from(format!("history-{n}").as_str());
            state.interactions.insert(id.clone(), Interaction { id, ..card.clone() });
        }
        let before = alondra_domain_rules::measure::cards_read();
        assert!(replans(&state, 0).is_empty());
        let reads = alondra_domain_rules::measure::cards_read() - before;
        assert!(reads <= state.interactions.len() as u64,
            "replanning read {reads} cards for {} tasks and {} historical cards", state.tasks.len(), state.interactions.len());
        for task in state.tasks.values_mut() { task.archived = true; }
        let before = alondra_domain_rules::measure::cards_read();
        assert!(replans(&state, 0).is_empty());
        assert_eq!(alondra_domain_rules::measure::cards_read() - before, 0,
            "no open task must read no start-problem history");
    }

    /// The wait is handed to the orchestrator every time it rests, with the open item, whatever the
    /// clock says, up to its last reminder: Alondra never asks the user and never cancels it.
    #[test]
    fn a_wait_for_a_cancelled_task_is_handed_back_at_every_rest_and_never_decided_for_the_orchestrator() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        pair_of_tasks(&domain, false);
        let task = TaskId::from("second");
        let reminders = || cards(&domain, DEPENDENCY_CANCELLED_NOTICE_PREFIX).len();

        for _ in 0..4 {
            turn_ends(&domain);
        }
        assert_eq!(reminders(), 0, "a pass of an idle orchestrator is not a rest");
        hand_replans(&domain);
        hand_replans(&domain);
        assert_eq!(reminders(), 1, "told at once, once per rest");
        for hour in 0..30 {
            settle_overdue(&domain, hour * 60 * MINUTE);
        }
        assert_eq!(reminders(), 1, "no clock hands it again");
        rest(&domain);
        assert_eq!(reminders(), 2);
        rest(&domain);
        assert_eq!(reminders(), REPLAN_REMINDERS, "one more at each rest");
        for _ in 0..10 {
            rest(&domain);
        }
        assert_eq!(reminders(), REPLAN_REMINDERS, "past the last reminder no rest hands it again");
        let state = domain.state().unwrap();
        let text = &state.interactions[&"dependency-cancelled-second--first-2".into()].body;
        assert!(text.contains("Still open, reminder 2: Task “Task second” (second) waits for “Task first” (first), which was cancelled"), "{text}");
        assert!(state.open_start_problem(&task).is_none(), "the user is never asked");
        assert!(!state.tasks[&task].cancelled(), "and the task is never cancelled for the orchestrator");
        let owed = state.obligation_of(&task).expect("still owed");
        assert_eq!((owed.party, owed.owed), (Party::Alondra, Owed::Replan));
    }

    /// A batch that only carried a re-plan reminder and did nothing is no rest: the reminder does
    /// not start the batch that hands the next one, and however many such batches run, the
    /// orchestrator gets a bounded number of messages.
    #[test]
    fn a_replan_the_orchestrator_never_decides_starts_a_bounded_number_of_batches() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        pair_of_tasks(&domain, false);
        hand(&domain);
        let mut handed = 0;
        for _ in 0..20 {
            hand_replans(&domain);
            let messages: Vec<Option<String>> = domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
                .filter(|message| message.recipient_kind == "orchestrator").map(|message| message.interaction_id).collect();
            if messages.is_empty() {
                continue;
            }
            handed += messages.len();
            let state = domain.state().unwrap();
            let replans_only = only_replan_reminders(messages.iter()
                .map(|id| id.as_ref().and_then(|id| state.interactions.get(&InteractionId::from(id.as_str()))).and_then(|card| card.system_card.as_ref())));
            hand(&domain);
            if !replans_only {
                domain.orchestrator_rests.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        assert_eq!(handed, 1, "the one reminder its batch did nothing with is not handed again by that batch's end");
        for _ in 0..20 {
            rest(&domain);
            hand(&domain);
        }
        assert_eq!(cards(&domain, DEPENDENCY_CANCELLED_NOTICE_PREFIX).len(), REPLAN_REMINDERS, "rests caused by other work stop at the last reminder");
    }

    #[test]
    fn only_a_batch_of_nothing_but_replan_reminders_is_no_rest() {
        let cancelled = SystemCard::Inbox { about: InboxCard::DependencyCancelledNotice, subject: "b--a".into(), number: 1 };
        let refused = SystemCard::Inbox { about: InboxCard::PlanRefusedNotice, subject: "card".into(), number: 2 };
        let unsplit = SystemCard::Inbox { about: InboxCard::UnsplitNotice, subject: "r".into(), number: 1 };
        assert!(only_replan_reminders([Some(&cancelled), Some(&refused)]));
        assert!(!only_replan_reminders([Some(&cancelled), None]), "a message of other work makes it a rest");
        assert!(!only_replan_reminders([Some(&cancelled), Some(&unsplit)]));
        assert!(!only_replan_reminders(std::iter::empty()), "a batch of no message is not one of reminders");
    }

    /// Issue #353: a request whose tasks all ended was handed to the orchestrator once; when that
    /// batch did not close it, it stayed open for ever with no card for the user. Now it is
    /// reminded twice, then Alondra closes it with its Done card.
    #[test]
    fn a_request_whose_tasks_ended_is_reminded_twice_then_closed_by_alondra_with_its_done_card() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        request(&domain, "r", false);
        run(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "r".into(), title: "Build it".into() });
        run(&domain, Actor::System, "node", Command::RegisterAgent { agent_id: "n".into(), name: "n".into(), at: 0 });
        run(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("n".into()), review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        run(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        let state = domain.state().unwrap();
        assert!(state.tasks[&"t".into()].over() && !closed(&domain, "r"));
        assert_eq!(unresolved(&state, &["r".into()], &[]), ["request r"], "a batch that leaves it open left it without an outcome");

        turn_ends(&domain);
        assert_eq!(cards(&domain, TASKS_ENDED_NOTICE_PREFIX), [format!("{TASKS_ENDED_NOTICE_PREFIX}r-1")]);
        assert!(unresolved(&domain.state().unwrap(), &["r".into()], &[]).is_empty(), "a reminder is on its way");
        turn_ends(&domain);
        assert_eq!(cards(&domain, TASKS_ENDED_NOTICE_PREFIX).len(), 2);
        assert!(!closed(&domain, "r"), "the orchestrator gets its two reminders first");
        turn_ends(&domain);
        let state = domain.state().unwrap();
        let request = &state.requests[&"r".into()];
        assert!(request.closed && !request.withdrawn);
        let done = alondra_domain_rules::request_outcome::DoneCard::id("r");
        assert!(state.interactions.contains_key(&InteractionId::from(done.as_str())), "the user gets the Done card");
        assert_eq!(cards(&domain, TASKS_ENDED_NOTICE_PREFIX).len(), 2, "no third reminder");
    }

    /// Once the orchestrator re-points the task, nothing is owed any more; a card from before that
    /// still asked the user about the cancelled task is withdrawn.
    #[test]
    fn a_task_waiting_for_a_cancelled_one_that_is_re_pointed_owes_nothing_to_the_orchestrator() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        pair_of_tasks(&domain, true);
        run(&domain, Actor::System, "old-card", Command::RaiseStartProblem {
            task_id: "second".into(), problem: StartProblem::DependencyCancelled { dependency: "first".into() },
        });
        run(&domain, Actor::Orchestrator { generation: 0 }, "repoint", Command::SetDependencies {
            task_id: "second".into(), dependencies: ["other".into()].into(), stack: None, reason: None,});
        let state = domain.state().unwrap();
        let owed = state.obligation_of(&"second".into()).expect("owed");
        assert_ne!(owed.owed, Owed::Replan);
        assert!(matches!(&replans(&state, 0)[..], [Step::Withdraw { .. }]), "{:?}", replans(&state, 0));
        rest(&domain);
        let state = domain.state().unwrap();
        assert!(state.open_start_problem(&"second".into()).is_none(), "the old card no longer asks anything");
        assert_eq!(replans(&state, 1), []);
    }

    fn node_with_task(domain: &DomainRuntime) {
        run(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
        request(domain, "r", false);
        run(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Resume the migration".into(), agent_id: Some("a".into()),
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        run(domain, Actor::Agent { agent_id: "a".into() }, "start", Command::StartTask { task_id: "t".into() });
    }

    fn ask(domain: &DomainRuntime, id: &str) {
        run(domain, Actor::Agent { agent_id: "a".into() }, &format!("ask-{id}"), Command::AskQuestion {
            attachments: vec![], interaction_id: id.into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_domain_rules::Recipient::Orchestrator, body: "Create one task per phase of the migration".into(),
            blocking: true, options: vec![
                QuestionOption { id: "done".into(), label: "Done".into(), description: None },
                QuestionOption { id: "no".into(), label: "No".into(), description: None },
            ],
            card: alondra_domain_rules::CardSpec::new(alondra_domain_rules::CardKind::Decide, "Create the phase tasks").why("The plan needs them"),
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
    const HOUR: Millis = 60 * MINUTE;

    /// Task `t`, whose node `a` runs a turn through the SDK.
    fn sdk_node_with_task(domain: &DomainRuntime) {
        run(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "Node".into(), at: 0 });
        request(domain, "r", false);
        run(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Resume the migration".into(), agent_id: None,
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_domain_rules::LaunchPlan::new("node", "/tmp")),
        });
        crate::domain_store::admit_through_sdk(domain, &"t".into(), "a");
        run(domain, Actor::System, "available", Command::RecordRuntimeFacts { task_id: "t".into(), generation: 1, facts: vec![
            alondra_domain_rules::RuntimeObservation::ResourceAvailable,
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
                alondra_domain_rules::RuntimeObservation::TurnStarted { turn: format!("turn-{at}"), input: input.as_str().to_owned() },
            ] });
        }
    }

    fn node_messages(domain: &DomainRuntime) -> Vec<String> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|message| message.recipient_kind == "agent").map(|message| message.message).collect()
    }

    fn user_cards(domain: &DomainRuntime) -> Vec<String> {
        domain.state().unwrap().interactions.values()
            .filter(|i| i.recipient == alondra_domain_rules::Recipient::User).map(|i| i.id.0.clone()).collect()
    }

    /// 16 October: a node asked the orchestrator to create one task per phase of a migration; the
    /// orchestrator created the tasks and never answered, and the node could not deliver its PR.
    /// Now the first turn of the orchestrator that ends with it unanswered gets a reminder at once,
    /// and the next one that ends hands it back to the node. No clock takes part.
    #[test]
    fn an_unanswered_question_goes_back_to_its_node_after_one_reminder_without_a_card() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        sdk_node_with_task(&domain);
        ask(&domain, "q");
        node_step(&domain);
        turn_ends(&domain);
        let reminder = format!("{UNANSWERED_NOTICE_PREFIX}q-1");
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX), [reminder.clone()], "the turn that ended without an answer is reminded at once");
        let body = domain.state().unwrap().interactions[&reminder.as_str().into()].body.clone();
        assert!(body.contains("Create one task per phase"), "{body}");
        assert!(!body.contains("minutes"), "no deadline is promised: {body}");
        settle_overdue(&domain, 24 * HOUR);
        assert!(open(&domain, "q"), "a day on the clock changes nothing: one more turn for the orchestrator first");
        turn_ends(&domain);
        let state = domain.state().unwrap();
        assert!(state.interactions[&"q".into()].withdrawn, "the turn that read the reminder ended: the question is withdrawn");
        assert!(!state.waits_for_orchestrator(&"t".into()));
        assert!(node_messages(&domain).iter().any(|message| message.contains(
            "The orchestrator did not answer “Create the phase tasks”. Decide with what you know, or ask the user if it is theirs to decide.")),
            "{:?}", node_messages(&domain));
        assert!(user_cards(&domain).is_empty(), "no card for the user: {:?}", user_cards(&domain));
        turn_ends(&domain);
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX).len(), 1, "one reminder, ever");
    }

    /// A reminder no batch that ended with its result read (its notice expired, or its batch was
    /// stopped or discarded) settles nothing: Alondra reminds again, and acts only once a reminder
    /// was read.
    #[test]
    fn a_reminder_no_ended_batch_read_is_sent_again_instead_of_acted_on() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        sdk_node_with_task(&domain);
        ask(&domain, "q");
        node_step(&domain);
        turn_ends(&domain);
        assert_eq!(cards(&domain, UNANSWERED_NOTICE_PREFIX), [format!("{UNANSWERED_NOTICE_PREFIX}q-1")]);
        domain.store.lock().unwrap().discard_expired_orchestrator_messages(i64::MAX).unwrap();
        settle(&domain);
        assert!(open(&domain, "q"), "the reminder was never read: nothing is settled");
        let mut reminders = cards(&domain, UNANSWERED_NOTICE_PREFIX);
        reminders.sort();
        assert_eq!(reminders, [format!("{UNANSWERED_NOTICE_PREFIX}q-1"), format!("{UNANSWERED_NOTICE_PREFIX}q-2")], "it is reminded again");
        turn_ends(&domain);
        assert!(domain.state().unwrap().interactions[&"q".into()].withdrawn, "read once, the next ended batch settles it");
    }

    /// A suggestion an older daemon reminded the orchestrator of, under an id with no number, is
    /// reminded under this rule before Alondra discards it: the old rule left it open for good.
    #[test]
    fn a_suggestion_reminded_under_the_old_rule_is_reminded_again_before_it_is_discarded() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        node_with_task(&domain);
        run(&domain, Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
            attachments: vec![], interaction_id: "p".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_domain_rules::Recipient::Orchestrator, body: "README.md:12".into(), blocking: false,
            options: alondra_domain_rules::suggestion_options(),
            card: alondra_domain_rules::CardSpec::new(alondra_domain_rules::CardKind::Propose, "The README names the old flag").why("Not this task"),
        });
        let legacy = format!("{UNSETTLED_NOTICE_PREFIX}p");
        let state = domain.state().unwrap();
        carry_out(&domain, &state, &Step::Tell { id: legacy.clone(),
            card: SystemCard::Inbox { about: InboxCard::UnsettledNotice, subject: "p".into(), number: 1 }, text: "Suggestion p has no decision.".into() });
        turn_ends(&domain);
        assert!(open(&domain, "p"), "the old reminder does not count");
        assert!(cards(&domain, UNSETTLED_NOTICE_PREFIX).contains(&format!("{UNSETTLED_NOTICE_PREFIX}p-2")), "{:?}", cards(&domain, UNSETTLED_NOTICE_PREFIX));
        turn_ends(&domain);
        assert!(!open(&domain, "p"), "discarded once a reminder of this rule was read");
    }

    /// No clock reminds the orchestrator or settles for it what it owes the nodes, however long a
    /// question, a stopped node's case or a suggestion waits.
    #[test]
    fn the_clock_never_reminds_or_settles_what_the_orchestrator_owes() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        sdk_node_with_task(&domain);
        ask(&domain, "q");
        node_step(&domain);
        let revision = domain.state().unwrap().revision;
        for hours in [1, 2, 24, 24 * 7] {
            settle_overdue(&domain, hours * HOUR);
        }
        assert_eq!(domain.state().unwrap().revision, revision, "a week of clock changed nothing");
        assert!(open(&domain, "q"));
    }

    #[test]
    fn an_answered_question_or_one_of_a_task_that_ended_is_left_alone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "answered");
        answer(&domain, Actor::Orchestrator { generation: 0 }, "answered", "done");
        assert!(!open(&domain, "answered"));
        ask(&domain, "late");
        run(&domain, Actor::User { user_id: "u".into() }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        assert!(owed(&domain.state().unwrap(), &|_| true).is_empty(), "{:?}", owed(&domain.state().unwrap(), &|_| true));
    }

    /// A problem raised before the deadline existed no longer asks the user anything.
    #[test]
    fn a_problem_about_an_unanswered_question_is_withdrawn() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        node_with_task(&domain);
        ask(&domain, "q");
        let problem = alondra_domain_rules::orchestrator_cards::UnansweredProblem { question_id: "q".into(), task_title: "Task".into(), question: "Create the phase tasks".into() };
        let (need, why) = problem.text(alondra_view_model::Lang::En);
        let id = format!("{}q-1", alondra_domain_rules::orchestrator_cards::UNANSWERED_PROBLEM_PREFIX);
        domain.raise_worded_problem_in(Scope::Orchestrator { request_id: None }, &id,
            Some(SystemCard::Inbox { about: InboxCard::UnansweredProblem, subject: "q".into(), number: 1 }), "question_unanswered:q", need, why, problem.to_body(),
            vec![QuestionOption { id: "remind_again".into(), label: "Remind".into(), description: None }], None).unwrap();
        assert!(open(&domain, &id));
        turn_ends(&domain);
        assert!(!open(&domain, &id));
    }

    #[test]
    fn a_suggestion_left_unsettled_goes_back_once_then_is_discarded_and_never_reaches_the_user() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = crate::wiring::open(&home).unwrap();
        node_with_task(&domain);
        run(&domain, Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
            attachments: vec![], interaction_id: "p".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: alondra_domain_rules::Recipient::Orchestrator, body: "README.md:12".into(), blocking: false,
            options: alondra_domain_rules::suggestion_options(),
            card: alondra_domain_rules::CardSpec::new(alondra_domain_rules::CardKind::Propose, "The README names the old flag").why("Not this task"),
        });
        turn_ends(&domain);
        let reminder = format!("{UNSETTLED_NOTICE_PREFIX}p-1");
        assert_eq!(cards(&domain, UNSETTLED_NOTICE_PREFIX), [reminder.clone()]);
        assert!(domain.state().unwrap().interactions[&reminder.as_str().into()].body.contains("settle_suggestion"));
        assert!(open(&domain, "p"), "the reminder comes first");
        turn_ends(&domain);
        let state = domain.state().unwrap();
        match &state.interactions[&"p".into()].payload {
            InteractionPayload::Question { answer: Some(answer), .. } => assert_eq!(
                answer.decision, Some(SuggestionDecision::Discard { reason: UNSETTLED_DISCARD.to_owned() }), "Alondra discards it with its reason"),
            other => panic!("the suggestion is still open: {other:?}"),
        }
        turn_ends(&domain);
        assert_eq!(cards(&domain, UNSETTLED_NOTICE_PREFIX), [reminder], "one reminder, ever");
        assert!(cards(&domain, "problem-").is_empty(), "a suggestion never becomes the user's card");
    }

    #[derive(Clone, Debug)]
    enum Op {
        Ask(usize),
        Answer(usize),
        Withdraw(usize),
        TurnEnds,
        NodeStep,
        Minutes(i64),
    }

    fn op() -> impl proptest::strategy::Strategy<Value = Op> {
        use proptest::prelude::*;
        prop_oneof![
            3 => (0..4usize).prop_map(Op::Ask),
            2 => (0..4usize).prop_map(Op::Answer),
            1 => (0..4usize).prop_map(Op::Withdraw),
            2 => Just(Op::TurnEnds),
            3 => Just(Op::NodeStep),
            4 => (1..12i64).prop_map(Op::Minutes),
        ]
    }

    /// Every commit of a fixture waits for the disk unless the fixture relaxes durability, and the
    /// property below opens a runtime and commits hundreds of times for each of its cases: on a Mac
    /// whose disk other test runs keep busy, that wait is the whole cost of the test.
    #[test]
    fn the_runtimes_the_tests_open_do_not_wait_for_the_disk() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let _domain = crate::wiring::open(&home).unwrap();
        assert!(alondra_foundation::durability::relaxed());
    }

    proptest::proptest! {
        #![proptest_config(proptest::prelude::ProptestConfig { cases: 48, ..proptest::prelude::ProptestConfig::default() })]

        /// Whatever order the node asks and ends its turns, the orchestrator answers some, the node
        /// withdraws others and time passes, every question its node waits on ends answered or
        /// withdrawn once two turns of the orchestrator ended after it, time alone ends none, and
        /// none ever becomes a card for the user.
        #[test]
        fn no_question_to_the_orchestrator_outlives_two_ended_turns(ops in proptest::collection::vec(op(), 1..24)) {
            let home = crate::test_support::Directory::new();
            std::fs::create_dir_all(&home).unwrap();
            let domain = crate::wiring::open(&home).unwrap();
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
                        recipient: alondra_domain_rules::Recipient::Orchestrator, body: format!("Where does change {n} go?"),
                        blocking: true, options: vec![
                            QuestionOption { id: "done".into(), label: "Done".into(), description: None },
                            QuestionOption { id: "no".into(), label: "No".into(), description: None },
                        ],
                        card: alondra_domain_rules::CardSpec::new(alondra_domain_rules::CardKind::Decide, "Where does it go?").why("It decides the branch"),
                    }),
                    Op::Answer(n) if exists(n) => execute(Actor::Orchestrator { generation: 0 }, Command::AnswerQuestion {
                        attachments: vec![], interaction_id: question(n).as_str().into(), option_id: "done".into(), inputs: Default::default(),
                    }),
                    Op::Withdraw(n) if exists(n) => execute(Actor::Agent { agent_id: "a".into() }, Command::Withdraw { interaction_id: question(n).as_str().into() }),
                    Op::TurnEnds => turn_ends(&domain),
                    Op::NodeStep => node_step(&domain),
                    Op::Minutes(minutes) => {
                        clock += minutes * MINUTE;
                        settle_overdue(&domain, clock);
                    }
                    _ => {}
                }
                proptest::prop_assert!(user_cards(&domain).is_empty(), "a card for the user: {:?}", user_cards(&domain));
            }
            let waiting = |domain: &DomainRuntime| {
                let state = domain.state().unwrap();
                state.interactions.values().filter(|i| DomainState::asks_the_orchestrator(i) && state.open(i)).map(|i| i.id.clone()).collect::<Vec<_>>()
            };
            let before = waiting(&domain);
            for _ in 0..12 {
                clock += 60 * MINUTE;
                settle_overdue(&domain, clock);
            }
            proptest::prop_assert_eq!(waiting(&domain), before, "the clock alone settled a question");
            for _ in 0..2 {
                turn_ends(&domain);
                node_step(&domain);
            }
            let open = waiting(&domain);
            proptest::prop_assert!(open.is_empty(), "{open:?} outlived two ended turns");
        }
    }
}

