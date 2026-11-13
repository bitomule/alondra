//! The decisions Claude asks for through a task's SDK execution: the rules Alondra decides some by
//! itself, the cards the user decides the rest on, and how their answers become the decision.
use super::*;
use crate::allow_for_task::AllowForTask;
use crate::node_guard::forbidden_for_nodes;

/// The MCP server the Alondra plugin registers in a node's Claude Code.
pub const ALONDRA_MCP_SERVER: &str = "plugin:alondra:alondra";
/// The source of an MCP server a plugin defines.
pub const PLUGIN_SOURCE: &str = "plugin";
const USER_DENIED: &str = "The user denied it";
const USER_REQUESTED_CHANGES: &str = "The user requests changes to the plan";
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
            let alondra = tool.is_some_and(|tool| tool.starts_with(crate::activity::ALONDRA_TOOLS))
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

fn reason_of(hold: Hold) -> DecisionReason {
    match hold {
        Hold::Permission | Hold::Plan => DecisionReason::Permission,
        Hold::Question | Hold::Elicitation => DecisionReason::Question,
    }
}

fn note(answer: &Answer) -> Option<String> {
    answer.inputs.values().find_map(|value| match value {
        InputValue::Text(text) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    })
}

/// What a question card's answer tells Claude: the option chosen, or what the user wrote.
fn said(card: &Interaction) -> Option<String> {
    let InteractionPayload::Question { options, answer: Some(answer) } = &card.payload else { return None };
    options.iter().find(|option| option.id == answer.option_id).map(|option| option.label.clone()).or_else(|| note(answer))
}

impl DomainState {
    /// Why a task that is closing answers its node's request itself, with no card: a node that
    /// only closes now asks nothing, and one whose delivery was accepted plans nothing.
    fn closing_denial(&self, task: &TaskId, hold: Hold) -> Option<String> {
        let t = self.tasks.get(task)?;
        if self.node_closing(t) {
            return Some(node_closing_notice());
        }
        (hold == Hold::Plan && self.in_cleanup(t)).then(plan_after_acceptance_notice)
    }

    /// The SDK execution of `generation` asks for the decision its host persisted at `epoch`.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_decision_request(&mut self, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, hold: Hold,
        tool: Option<&str>, input: &serde_json::Value, mcp_server: Option<&McpServerOrigin>, prompt: &DecisionPrompt,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        system(actor)?;
        self.task(task)?;
        self.sdk_execution(task, generation, lifecycles)?;
        if self.lifecycle_journal.get(task).is_some_and(|records| records.iter().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Admitted { .. }))
            .any(|record| match record {
                LifecycleRecord::DecisionAsked { epoch: known, .. } => hold == Hold::Plan && *known == epoch,
                LifecycleRecord::Decided { epoch: known, .. } | LifecycleRecord::DecidedByPolicy { epoch: known, .. }
                | LifecycleRecord::DecisionSettled { epoch: known } | LifecycleRecord::DecisionAborted { epoch: known, .. } => *known == epoch,
                _ => false,
            })) {
            return Ok(());
        }
        if let DecisionPrompt::RecordedPlan { epoch: previous } = prompt {
            if hold != Hold::Plan || *previous >= epoch {
                return fail("invalid_input", "A repeated plan must name an earlier plan request");
            }
            let decision = self.lifecycle_journal.get(task).into_iter().flatten().rev()
                .take_while(|record| !matches!(record, LifecycleRecord::Admitted { .. }))
                .find_map(|record| match record {
                    LifecycleRecord::Decided { epoch, decision, card } if epoch == previous
                        && self.holds.get(card) == Some(&Hold::Plan) => Some(decision.clone()),
                    LifecycleRecord::DecidedByPolicy { epoch, decision } if epoch == previous => Some(decision.clone()),
                    _ => None,
                }).ok_or_else(|| DomainError::new("invalid_input", "The repeated plan has no recorded decision in this execution"))?;
            return self.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision });
        }
        let plan = match (hold, prompt) {
            (Hold::Plan, DecisionPrompt::Card { body, .. }) => Some(body.as_str()),
            _ => None,
        };
        if let Some(message) = self.closing_denial(task, hold) {
            return self.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Deny { message } });
        }
        let waiting = (hold == Hold::Plan).then(|| self.plan_while_waiting(task)).flatten()
            .map(|message| DecisionRecord::Deny { message });
        if let Some(decision) = decision_policy(hold, tool, input, mcp_server, plan).or(waiting) {
            return self.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision });
        }
        if hold == Hold::Permission && self.allowed_in_task(task, prompt) {
            return self.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Allow });
        }
        let node = self.lifecycle(lifecycles, task)?.execution().map(|stamp| stamp.node().clone()).ok_or_else(|| DomainError::new("stale_execution", "The task has no execution"))?;
        let winding_down = self.tasks.get(task).is_some_and(|t| Self::winding_down(t) && t.agent_id.as_ref() == Some(&node));
        let asker = Actor::Agent { agent_id: node.clone() };
        if !winding_down {
            self.assigned(&asker, task)?;
        }
        let scope = Scope::Task { task_id: task.clone() };
        if hold == Hold::Plan {
            let previous = self.slot_holders(&scope, &Author::Agent { agent_id: node }, &CardSlot::Plan);
            for card in previous {
                let lifecycle = self.lifecycle(lifecycles, task)?;
                let epoch = lifecycle.card_callback(&card).and_then(|callback| alondra_domain::decision_epoch(generation, callback));
                if let Some(epoch) = epoch {
                    self.decide(lifecycles, task, LifecycleRecord::Decided {
                        card, epoch, decision: DecisionRecord::Superseded,
                    })?;
                }
            }
        }
        let id = decision_card_id(task, generation, epoch);
        let mut cards = vec![];
        match prompt {
            DecisionPrompt::RecordedPlan { .. } => return fail("invalid_input", "A repeated plan creates no card"),
            DecisionPrompt::Card { body, options, card } => {
                validate_options(options)?;
                cards.push((id.clone(), body.clone(), options.clone(), card.clone()));
            }
            DecisionPrompt::Questions { questions } => {
                if hold != Hold::Question {
                    return refuse("invalid_input", "prompt", "Only Claude's questions are asked together");
                }
                for (card_id, card, options) in group_cards(&id, "", questions)? {
                    validate_options(&options)?;
                    cards.push((card_id, card.need.clone(), options, card));
                }
            }
        }
        let held = cards.first().map(|(card_id, ..)| card_id.clone()).ok_or_else(|| DomainError::new("invalid_input", "A decision asks something"))?;
        for (card_id, body, options, card) in cards {
            self.insert_interaction(&asker, &card_id, &scope, &Recipient::User, &body, true, &[],
                InteractionPayload::Question { options, answer: None }, &card, Source::Hold)?;
            self.holds.insert(card_id.clone(), hold);
            created.push(card_id.0.clone());
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card_id });
        }
        self.decide(lifecycles, task, LifecycleRecord::DecisionAsked { card: held, epoch, reason: reason_of(hold) })
    }

    /// Whether something the user allowed in the open task covers this prompt; the grant that
    /// covers it counts the use.
    fn allowed_in_task(&mut self, task: &TaskId, prompt: &DecisionPrompt) -> bool {
        let DecisionPrompt::Card { card, .. } = prompt else { return false };
        let Some(asked) = &card.allow_for_task else { return false };
        let Some(open) = self.tasks.get_mut(task).filter(|t| !t.over()) else { return false };
        match open.allowed.values_mut().find(|grant| grant.allows.covers(asked)) {
            Some(grant) => {
                grant.uses = grant.uses.saturating_add(1);
                true
            }
            None => false,
        }
    }

    /// Alondra could not put the request at `epoch` in front of the user: Claude is denied it with
    /// `reason`, as a decision of Alondra's own, and goes on.
    pub(super) fn refuse_decision_request(&mut self, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, reason: &str,
        lifecycles: &mut Lifecycles) -> Result<()> {
        system(actor)?;
        self.task(task)?;
        self.sdk_execution(task, generation, lifecycles)?;
        let message = format!("{UNASKABLE}: {}", reason.trim());
        self.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Deny { message } })
    }

    /// The records that end the request at `epoch`, judged on a replay of the task: its decision
    /// reached Claude, and the turn that asked goes on with it; or it ended without one, with the
    /// card the task then needs when nothing else explains its next step.
    pub(super) fn decision_end_guard(&self, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, answered: bool) -> Result<(Vec<LifecycleRecord>, Option<InteractionId>)> {
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

    /// The request at `epoch` is over. One that ended without a decision takes its cards with it.
    pub(super) fn end_decision(&mut self, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, answered: bool,
        effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let (records, unheard) = self.decision_end_guard(actor, task, generation, epoch, answered)?;
        let callback = alondra_domain::decision_callback(generation, epoch);
        let lifecycle = self.lifecycle(lifecycles, task)?;
        let asked: Vec<InteractionId> = lifecycle.cards().into_iter()
            .filter(|(card, _)| lifecycle.card_callback(card) == Some(&callback))
            .map(|(card, _)| card.clone())
            .collect();
        for card in asked {
            let open: Vec<InteractionId> = self.interactions.get(&card).map(|i| self.group_members(i)).unwrap_or_default().into_iter()
                .filter(|member| !member.withdrawn && !Self::answered_question(member))
                .map(|member| member.id.clone())
                .collect();
            for member in open {
                self.retire(&member);
            }
        }
        for record in records {
            self.decide(lifecycles, task, record)?;
        }
        if let Some(card) = unheard {
            let title = self.tasks[task].title.clone();
            self.interactions.insert(card.clone(), decision_cancelled_card(card.clone(), task, &title, self.revision + 1));
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
        }
        Ok(())
    }

    /// The card a request of the task's SDK execution was asked with, and its epoch, when `card`
    /// is that card or one of the questions asked together with it.
    fn asked_decision(&self, task: &TaskId, card: &Interaction) -> Option<(InteractionId, u64)> {
        let held = match &card.card.group {
            Some(group) => QuestionGroup::card_id(&InteractionId(group.id.clone()), 1),
            None => card.id.clone(),
        };
        self.lifecycle_journal.get(task)?.iter().rev().find_map(|record| match record {
            LifecycleRecord::DecisionAsked { card, epoch, .. } if *card == held => Some((held.clone(), *epoch)),
            _ => None,
        })
    }

    /// The user answered a card of a decision Claude waits on: once every question asked with it
    /// has its answer, the answers are the decision. False for any other card.
    pub(super) fn decide_sdk_card(&mut self, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<bool> {
        let Some(answered) = self.interactions.get(card) else { return Ok(false) };
        let Scope::Task { task_id } = answered.scope.clone() else { return Ok(false) };
        let Some((asked, epoch)) = self.asked_decision(&task_id, answered) else { return Ok(false) };
        let members: Vec<Interaction> = self.group_members(answered).into_iter().cloned().collect();
        let lifecycle = self.lifecycle(lifecycles, &task_id)?;
        if lifecycle.cards().iter().any(|(held, _)| **held == *card) && lifecycle.card_callback(card).is_none() {
            return Ok(false);
        }
        let callback = alondra_domain::decision_callback(lifecycle.generation(), epoch);
        let held = lifecycle.cards().into_iter()
            .map(|(card, _)| card.clone())
            .find(|card| lifecycle.card_callback(card) == Some(&callback))
            .ok_or_else(|| DomainError::new("conflict", format!("No card holds the decision {asked} asked")))?;
        let open = members.iter().find(|member| !member.withdrawn && !Self::answered_question(member));
        if let Some(open) = open {
            if held == *card {
                self.decide(lifecycles, &task_id, LifecycleRecord::CardMoved { from: held, to: open.id.clone() })?;
            }
            return Ok(true);
        }
        if members.iter().any(|member| member.withdrawn) {
            return Ok(true);
        }
        let hold = self.holds.get(&asked).copied().ok_or_else(|| DomainError::new("conflict", format!("Card {held} holds no decision")))?;
        let InteractionPayload::Question { answer: Some(answer), .. } = &members[0].payload else {
            return fail("conflict", "The decision has no answer");
        };
        let note = note(answer);
        if hold == Hold::Permission && answer.option_id == Hold::FOR_TASK {
            let allows = members[0].card.allow_for_task.clone().filter(AllowForTask::offered)
                .ok_or_else(|| DomainError::new("invalid_input", "This permission cannot be allowed in its task"))?;
            let open = self.tasks.get_mut(&task_id).filter(|t| !t.over())
                .ok_or_else(|| DomainError::new("task_not_active", "The task is over: there is nothing left to allow in it"))?;
            open.allowed.insert(asked.clone(), TaskGrant { allows, uses: 0 });
        }
        let decision = match (hold, answer.option_id.as_str()) {
            (Hold::Permission, Hold::ALLOW | Hold::ALWAYS | Hold::FOR_TASK) | (Hold::Plan, Hold::APPROVE) => DecisionRecord::Allow,
            (Hold::Permission, Hold::DENY) => DecisionRecord::Deny {
                message: note.map_or_else(|| USER_DENIED.to_owned(), |note| format!("{USER_DENIED}: {note}")),
            },
            (Hold::Plan, Hold::REQUEST_CHANGES) => DecisionRecord::Deny {
                message: note.map_or_else(|| USER_REQUESTED_CHANGES.to_owned(), |note| format!("{USER_REQUESTED_CHANGES}: {note}")),
            },
            (Hold::Elicitation, Hold::ACCEPT) => DecisionRecord::Accept { text: note.unwrap_or_default(), content: answer.form_values().cloned() },
            (Hold::Elicitation, Hold::DECLINE) => DecisionRecord::Decline,
            (Hold::Question, _) => DecisionRecord::Answers { answers: members.iter().map(|member| said(member).unwrap_or_default()).collect() },
            (_, option) => return refuse("invalid_input", "option_id", &format!("{option} does not decide this card")),
        };
        self.decide(lifecycles, &task_id, LifecycleRecord::Decided { card: held, epoch, decision })?;
        Ok(true)
    }
}
