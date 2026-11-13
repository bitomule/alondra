//! A node that stops with nothing explaining its next step is Alondra's to fix, never the user's:
//! Alondra hands it back its turn up to `EMPTY_TURN_WAKES` times in a row, and the next stop is a
//! case for the orchestrator, which settles it with one closed outcome. Cases are counted since
//! the task last moved on; past the limit the only outcome is to close the task with a result.
use super::*;
use alondra_domain::EMPTY_TURN_WAKES;
use serde::{Deserialize, Serialize};

/// The case Alondra opens for the orchestrator when a node stopped: this prefix, then the task,
/// the execution and the turn.
pub const STALLED_PREFIX: &str = "stalled-";
/// How many times the orchestrator may send a stopped node back to work before the task closes.
pub const STALL_ATTEMPTS: u32 = 3;
pub const STALL_INSTRUCT: &str = "instruct";
pub const STALL_CLOSE: &str = "close";
/// What a delivered node is told when nobody settled the case of its rest: the delivery is the user's.
pub const UNSETTLED_DELIVERY: &str = "Your delivery stays with the user for review: do nothing more until they write to you.";
pub const UNSETTLED_AFTER_MERGE: &str = "Nobody settled how your work after the merge goes on: do nothing more until the user writes to you.";
/// What a stopped node is told when nobody settled its case in time and it has attempts left.
pub const UNSETTLED_INSTRUCT: &str = "Nobody settled why you stopped. Go on with your task; if you cannot, ask the user or deliver what you have.";
/// The longest instruction or result a settlement carries.
pub const STALL_TEXT_MAX: usize = 4000;

/// How the orchestrator settles a stopped node: a concrete instruction it gets, or the end of its
/// task with a result that says what got done. Never words alone. A case nobody settled in time
/// Alondra settles itself: it instructs the node to go on, or, past the limit, closes it `Unsettled`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StallOutcome {
    Instruct { text: String },
    Close { result: String },
    Unsettled,
}
impl StallOutcome {
    pub fn option_id(&self) -> &'static str {
        match self {
            Self::Instruct { .. } => STALL_INSTRUCT,
            Self::Close { .. } | Self::Unsettled => STALL_CLOSE,
        }
    }
    fn written(&self) -> Option<(&'static str, &str)> {
        match self {
            Self::Instruct { text } => Some(("instruction", text)),
            Self::Close { result } => Some(("result", result)),
            Self::Unsettled => None,
        }
    }
}

/// The case of `turn`, the root turn of execution `generation` of `task`, in the grammar of card ids.
pub fn stalled_card_id(task: &TaskId, generation: u64, turn: &str) -> InteractionId {
    let id = turn_ended_card_id(task, generation, turn);
    stalled_in_place_of(&id)
}

/// The case that takes the place of the card a turn end named before stops went to the orchestrator.
pub(in crate::domain) fn stalled_in_place_of(card: &InteractionId) -> InteractionId {
    let rest = card.0.strip_prefix(TURN_ENDED_PREFIX).or_else(|| card.0.strip_prefix(STALLED_PREFIX)).unwrap_or(&card.0);
    InteractionId(format!("{STALLED_PREFIX}{rest}"))
}

/// Whether `i` is a case Alondra opened for the orchestrator about a stopped node.
pub fn is_stall_case(i: &Interaction) -> bool {
    i.system_card == Some(SystemCard::Stalled) && i.author == Author::System && i.recipient == Recipient::Orchestrator
}

/// Whether a stop of the task `lifecycle` follows opens a case at once instead of waking its node:
/// Alondra already woke it `EMPTY_TURN_WAKES` times in a row with nothing new since.
pub(in crate::domain) fn stalled_before(lifecycle: &alondra_domain::TaskLifecycle) -> bool {
    lifecycle.empty_turns() >= EMPTY_TURN_WAKES
}

impl DomainState {
    /// Whether `i` is a case about a stopped node still waiting for someone to settle it. A case
    /// closed while the task's PR was open stays open until Alondra delivers the PR, and nobody
    /// settles it again.
    pub fn unsettled_stall(&self, i: &Interaction) -> bool {
        is_stall_case(i) && self.open(i) && !match &i.scope {
            Scope::Task { task_id } => self.tasks.get(task_id).is_some_and(|t| matches!(t.open_pr, Some(OpenPrPending::Deliver { .. }))),
            Scope::Orchestrator { .. } => false,
        }
    }
}

fn stall_options(only_close: bool) -> Vec<QuestionOption> {
    let instruct = QuestionOption {
        id: STALL_INSTRUCT.into(),
        label: "Tell it what to do".into(),
        description: Some("The node gets your instruction, one concrete next step, and goes on with its task.".into()),
    };
    let close = QuestionOption {
        id: STALL_CLOSE.into(),
        label: "Close the task with a result".into(),
        description: Some("The task ends without a delivery; your result says what got done and what did not.".into()),
    };
    if only_close { vec![close] } else { vec![instruct, close] }
}

impl DomainState {
    /// The case the lifecycle of `task` now waits on, `card`, goes to the orchestrator.
    pub(in crate::domain) fn open_stall(&mut self, task: &TaskId, card: InteractionId, effects: &mut Vec<DomainEffect>) -> Result<()> {
        let why = self.empty_turn(task);
        self.open_stall_on(task, card, why, effects)
    }

    /// The case `card` of `task`, whose node stopped for `why`, goes to the orchestrator.
    pub(in crate::domain) fn open_stall_on(&mut self, task: &TaskId, card: InteractionId, why: EmptyTurn, effects: &mut Vec<DomainEffect>) -> Result<()> {
        let Some(t) = self.tasks.get_mut(task) else { return Ok(()) };
        t.stalls += 1;
        let (number, title) = (t.stalls, t.title.clone());
        let only_close = number > STALL_ATTEMPTS;
        let delivered = !matches!(why, EmptyTurn::Unexplained | EmptyTurn::UnpresentedPr { .. });
        let why = why.stall_text();
        let delivery = if delivered {
            " Its delivery stays with the user while you settle it: instruct sends the node back to its step, close cancels the task and its delivery, and a case nobody settles leaves the delivery with the user."
        } else {
            ""
        };
        let outcomes = if only_close {
            format!("This is case {number}: the orchestrator already sent it back {STALL_ATTEMPTS} times and it stopped again. Settle it with close: the task ends with your result, what got done and what did not.")
        } else {
            format!("This is case {number} of {STALL_ATTEMPTS} since the task last moved on. Settle it with instruct (one concrete next step the node gets) or close (the task ends with your result, what got done and what did not). After case {STALL_ATTEMPTS}, close is the only outcome.")
        };
        let body = format!(
            "The node of Task “{title}” ({task}) stopped with nothing that explains its next step: no result, no question, no work of its own running and nothing it waits for. {why} Alondra already handed it back its turn {EMPTY_TURN_WAKES} times in a row with what happened. \
             This is never a question for the user: read its mission, its last words and its worktree below, and settle it with settle_stall(case_id: \"{card}\", task_id: \"{task}\", outcome). {outcomes}{delivery}");
        let need = fit_title(&title, CardSpec::TITLED_NEED_MAX, |title| format!("“{title}” stopped without a result or a question: settle how it goes on"));
        self.insert_system_card(Interaction {
            attachments: vec![],
            id: card.clone(),
            scope: Scope::Task { task_id: task.clone() },
            author: Author::System,
            recipient: Recipient::Orchestrator,
            body,
            blocking: true,
            subject: None,
            payload: InteractionPayload::Question { options: stall_options(only_close), answer: None },
            revision: self.revision + 1,
            order: self.revision + 1,
            withdrawn: false,
            card: CardSpec::new(CardKind::Decide, clip(&need, CardSpec::NEED_MAX)),
            clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(SystemCard::Stalled), shortened: false,
        })?;
        effects.push(DomainEffect::NotifyOrchestrator { interaction_id: Some(card), request_id: None });
        Ok(())
    }

    /// The case `case_id` of `task` the orchestrator, or Alondra once nobody settled it in time, may settle with `outcome` now.
    pub(in crate::domain) fn settle_stall_guard(&self, actor: &Actor, case_id: &InteractionId, task: &TaskId, outcome: &StallOutcome) -> Result<()> {
        self.fence(actor)?;
        match (actor, outcome) {
            (Actor::System, StallOutcome::Unsettled | StallOutcome::Instruct { .. }) | (Actor::Orchestrator { .. }, StallOutcome::Instruct { .. } | StallOutcome::Close { .. }) => {}
            (Actor::Orchestrator { .. }, StallOutcome::Unsettled) => return refuse("invalid_input", "outcome.kind", "Settle it with instruct or close"),
            _ => return fail("forbidden", "Only the orchestrator settles a stopped node"),
        }
        let i = self.interaction(case_id)?;
        if !is_stall_case(i) || i.scope != (Scope::Task { task_id: task.clone() }) {
            return refuse("invalid_action_for_kind", "case_id", &format!("{case_id} is not a case about a stopped node of task {task}: answer other questions with answer_question"));
        }
        if !self.unsettled_stall(i) || !self.lifecycle_of(task).is_ok_and(|lifecycle| lifecycle.cards().iter().any(|(held, _)| *held == case_id)) {
            return refuse("stall_settled", "case_id", "This case is already settled, or the node went on without it: nothing is left to settle");
        }
        if let Some((what, text)) = outcome.written()
            && (text.trim().is_empty() || text.trim().len() > STALL_TEXT_MAX) {
            return refuse("invalid_input", "outcome", &format!("Write the {what} in 1 to {STALL_TEXT_MAX} bytes"));
        }
        if matches!(outcome, StallOutcome::Instruct { .. }) && self.tasks.get(task).is_some_and(|t| t.stalls > STALL_ATTEMPTS) {
            return refuse("stall_limit", "outcome.kind", &format!(
                "The node of task {task} stopped again after {STALL_ATTEMPTS} instructions: close the task with a result that says what got done"));
        }
        Ok(())
    }

    /// The orchestrator's settlement of a stopped node: the node gets the instruction, or the task
    /// ends with the result.
    pub(in crate::domain) fn settle_stall(&mut self, actor: &Actor, case_id: &InteractionId, task: &TaskId, outcome: &StallOutcome,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.settle_stall_guard(actor, case_id, task, outcome)?;
        let text = outcome.written().map(|(_, text)| text.trim().to_owned());
        if !matches!(outcome, StallOutcome::Instruct { .. }) && self.open_pr_of(task).is_some()
            && (self.lifecycle(lifecycles, task)?.phase() != alondra_domain::TaskPhase::Delivered
                || (text.is_some() && !self.tasks[task].pending_user_inputs.is_empty())) {
            return self.close_task(actor, task, text, effects, created, lifecycles);
        }
        let at = self.revision + 1;
        let i = self.interactions.get_mut(case_id).expect("the guard found it");
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
                let record = LifecycleRecord::Answered { card: case_id.clone(), input: self.next_input(task), text };
                self.decide(lifecycles, task, record)
            }
            StallOutcome::Close { .. } | StallOutcome::Unsettled if self.lifecycle(lifecycles, task)?.phase() == alondra_domain::TaskPhase::Delivered
                && (*outcome == StallOutcome::Unsettled || self.open_pr_of(task).is_some()) => {
                let text = if self.work_after_merge(task, self.lifecycle(lifecycles, task)?).is_some() { UNSETTLED_AFTER_MERGE } else { UNSETTLED_DELIVERY };
                let record = LifecycleRecord::Answered { card: case_id.clone(), input: self.next_input(task), text: Some(text.into()) };
                self.decide(lifecycles, task, record)
            }
            StallOutcome::Close { .. } | StallOutcome::Unsettled => {
                self.tasks.get_mut(task).expect("the guard found it").stall_closing = Some(match text {
                    Some(result) => StallClosing::Settled { result },
                    None => StallClosing::Unsettled,
                });
                self.tasks.get_mut(task).expect("the guard found it").pending_user_inputs.clear();
                self.cancel(task, effects, lifecycles)
            }
        }
    }

    /// A delivery, a card of the node to the user or the user's word on its task means the task
    /// moved on: its count of cases starts again.
    pub(in crate::domain) fn count_progress(&mut self, actor: &Actor, command: &Command) {
        let task = match (actor, command) {
            (Actor::Agent { .. }, Command::PresentResult { task_id, .. }) => Some(task_id.clone()),
            (Actor::Agent { .. }, Command::AskQuestion { scope: Scope::Task { task_id }, recipient: Recipient::User, .. }
                | Command::AskQuestions { scope: Scope::Task { task_id }, recipient: Recipient::User, .. }
                | Command::RequestAction { scope: Scope::Task { task_id }, recipient: Recipient::User, .. }) => Some(task_id.clone()),
            (Actor::System, Command::TellNode { task_id, from_user: true, .. }) | (Actor::User { .. }, Command::ReopenTask { task_id }) => Some(task_id.clone()),
            (Actor::User { .. }, Command::AnswerQuestion { interaction_id, .. }
                | Command::SubmitFeedback { presentation_id: interaction_id, .. }
                | Command::AcceptDelivery { presentation_id: interaction_id, .. }
                | Command::DecideProposedPlan { presentation_id: interaction_id, .. }) => match self.interactions.get(interaction_id).map(|i| &i.scope) {
                Some(Scope::Task { task_id }) => Some(task_id.clone()),
                _ => None,
            },
            _ => None,
        };
        if let Some(t) = task.and_then(|task| self.tasks.get_mut(&task)) {
            t.stalls = 0;
        }
    }

    /// A case the lifecycle no longer waits on (the user told the node something, or it went on
    /// by itself) is withdrawn, so the orchestrator never settles what no longer holds.
    pub(in crate::domain) fn withdraw_released_stalls(&mut self, lifecycles: &mut Lifecycles) -> Result<()> {
        let open: Vec<(TaskId, InteractionId)> = self.interactions.values()
            .filter(|i| is_stall_case(i) && self.open(i))
            .filter_map(|i| match &i.scope { Scope::Task { task_id } => Some((task_id.clone(), i.id.clone())), Scope::Orchestrator { .. } => None })
            .collect();
        for (task, case) in open {
            let held = self.tasks.get(&task).is_some_and(|t| !t.state.terminal())
                && self.lifecycle_journal.contains_key(&task)
                && self.lifecycle(lifecycles, &task)?.cards().iter().any(|(held, _)| **held == case);
            if !held {
                self.retire(&case);
            }
        }
        Ok(())
    }
}
