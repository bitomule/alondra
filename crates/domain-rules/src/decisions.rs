//! The decisions Claude asks for through a task's SDK execution: the rules Alondra decides some by
//! itself, the cards the user decides the rest on, and how their answers become the decision.
use super::*;
use alondra_core::allow_for_task::AllowForTask;
use alondra_core::task_git::{TaskBranch, Verdict};

pub(crate) fn reason_of(hold: Hold) -> DecisionReason {
    match hold {
        Hold::Permission | Hold::Plan => DecisionReason::Permission,
        Hold::Question | Hold::Elicitation => DecisionReason::Question,
    }
}
/// Why a task that is closing answers its node's request itself, with no card: a node that
/// only closes now asks nothing, and one whose delivery was accepted plans nothing.
pub(crate) fn closing_denial(domain: &DomainState, task: &TaskId, hold: Hold) -> Option<String> {
    let t = domain.tasks.get(task)?;
    if domain.node_closing(t) {
        return Some(node_closing_notice());
    }
    (hold == Hold::Plan && domain.in_cleanup(t)).then(plan_after_acceptance_notice)
}
/// The SDK execution of `generation` asks for the decision its host persisted at `epoch`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_decision_request(domain: &mut DomainState, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, hold: Hold,
    tool: Option<&str>, input: &serde_json::Value, mcp_server: Option<&McpServerOrigin>, prompt: &DecisionPrompt,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    system(actor)?;
    domain.task(task)?;
    domain.sdk_execution(task, generation, lifecycles)?;
    if domain.lifecycle_journal.get(task).is_some_and(|records| records.iter().rev()
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
        let decision = domain.lifecycle_journal.get(task).into_iter().flatten().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Admitted { .. }))
            .find_map(|record| match record {
                LifecycleRecord::Decided { epoch, decision, card } if epoch == previous
                    && domain.holds.get(card) == Some(&Hold::Plan) => Some(decision.clone()),
                LifecycleRecord::DecidedByPolicy { epoch, decision } if epoch == previous => Some(decision.clone()),
                _ => None,
            }).ok_or_else(|| DomainError::new("invalid_input", "The repeated plan has no recorded decision in this execution"))?;
        return domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision });
    }
    let plan = match (hold, prompt) {
        (Hold::Plan, DecisionPrompt::Card { body, .. }) => Some(body.as_str()),
        _ => None,
    };
    if let Some(message) = closing_denial(domain, task, hold) {
        return domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Deny { message } });
    }
    let waiting = (hold == Hold::Plan).then(|| domain.plan_while_waiting(task)).flatten()
        .map(|message| DecisionRecord::Deny { message });
    let role = (hold == Hold::Plan).then(|| role_plan_denial(domain, task)).flatten()
        .map(|message| DecisionRecord::Deny { message });
    if let Some(decision) = decision_policy(hold, tool, input, mcp_server, plan).or(waiting).or(role) {
        return domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision });
    }
    if hold == Hold::Permission {
        if let Some(decision) = task_git_decision(domain, task, tool, input, prompt) {
            return domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision });
        }
    }
    if hold == Hold::Permission && matches!(prompt, DecisionPrompt::Card { card, .. } if card.own_worktree) {
        return domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Allow });
    }
    if hold == Hold::Permission && allowed_in_task(domain, task, prompt) {
        return domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Allow });
    }
    let node = domain.lifecycle(lifecycles, task)?.execution().map(|stamp| stamp.node().clone()).ok_or_else(|| DomainError::new("stale_execution", "The task has no execution"))?;
    let winding_down = domain.tasks.get(task).is_some_and(|t| DomainState::winding_down(t) && t.agent_id.as_ref() == Some(&node));
    let asker = Actor::Agent { agent_id: node.clone() };
    if !winding_down {
        domain.assigned(&asker, task)?;
    }
    let scope = Scope::Task { task_id: task.clone() };
    if hold == Hold::Plan {
        let previous = domain.slot_holders(&scope, &Author::Agent { agent_id: node }, &CardSlot::Plan);
        for card in previous {
            let lifecycle = domain.lifecycle(lifecycles, task)?;
            let epoch = lifecycle.card_callback(&card).and_then(|callback| alondra_domain::decision_epoch(generation, callback));
            if let Some(epoch) = epoch {
                domain.decide(lifecycles, task, LifecycleRecord::Decided {
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
        domain.insert_interaction(&asker, &card_id, &scope, &Recipient::User, &body, true, &[],
            InteractionPayload::Question { options, answer: None }, &card, Source::Hold)?;
        domain.holds.insert(card_id.clone(), hold);
        created.push(card_id.0.clone());
        effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card_id });
    }
    domain.decide(lifecycles, task, LifecycleRecord::DecisionAsked { card: held, epoch, reason: reason_of(hold) })
}
/// What Alondra decides by itself for a git command of the conflict flow: allowed in the
/// task's own worktree on its own branch, denied when it would throw work away.
pub(crate) fn task_git_decision(domain: &DomainState, task: &TaskId, tool: Option<&str>, input: &serde_json::Value, prompt: &DecisionPrompt) -> Option<DecisionRecord> {
    let command = input.get("command").and_then(serde_json::Value::as_str).filter(|_| tool == Some("Bash"))?;
    let cwd = match prompt {
        DecisionPrompt::Card { card, .. } => card.allow_for_task.as_ref().and_then(AllowForTask::cwd),
        _ => None,
    };
    let branches: Vec<TaskBranch> = domain.tasks.get(task)?.open_worktrees()
        .filter_map(|(path, worktree)| Some(TaskBranch { worktree: path.into(), branch: worktree.branch.clone()? }))
        .collect();
    match alondra_core::task_git::judge(command, cwd, &branches)? {
        Verdict::Allow => Some(DecisionRecord::Allow),
        Verdict::Deny(message) => Some(DecisionRecord::Deny { message }),
    }
}
/// Whether something the user allowed in the open task covers this prompt; the grant that
/// covers it counts the use.
pub(crate) fn allowed_in_task(domain: &mut DomainState, task: &TaskId, prompt: &DecisionPrompt) -> bool {
    let DecisionPrompt::Card { card, .. } = prompt else { return false };
    let Some(asked) = &card.allow_for_task else { return false };
    let Some(open) = domain.tasks.get_mut(task).filter(|t| !t.over()) else { return false };
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
pub(crate) fn refuse_decision_request(domain: &mut DomainState, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, reason: &str,
    lifecycles: &mut Lifecycles) -> Result<()> {
    system(actor)?;
    domain.task(task)?;
    domain.sdk_execution(task, generation, lifecycles)?;
    let message = format!("{UNASKABLE}: {}", reason.trim());
    domain.decide(lifecycles, task, LifecycleRecord::DecidedByPolicy { epoch, decision: DecisionRecord::Deny { message } })
}
/// The request at `epoch` is over. One that ended without a decision takes its cards with it.
pub(crate) fn end_decision(domain: &mut DomainState, actor: &Actor, task: &TaskId, generation: u64, epoch: u64, answered: bool,
    effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
    let (records, unheard) = domain.decision_end_guard(actor, task, generation, epoch, answered)?;
    let callback = alondra_domain::decision_callback(generation, epoch);
    let lifecycle = domain.lifecycle(lifecycles, task)?;
    let asked: Vec<InteractionId> = lifecycle.cards().into_iter()
        .filter(|(card, _)| lifecycle.card_callback(card) == Some(&callback))
        .map(|(card, _)| card.clone())
        .collect();
    for card in asked {
        let open: Vec<InteractionId> = domain.interactions.get(&card).map(|i| domain.group_members(i)).unwrap_or_default().into_iter()
            .filter(|member| !member.withdrawn && !DomainState::answered_question(member))
            .map(|member| member.id.clone())
            .collect();
        for member in open {
            domain.retire(&member);
        }
    }
    for record in records {
        domain.decide(lifecycles, task, record)?;
    }
    if let Some(card) = unheard {
        let title = domain.tasks[task].title.clone();
        domain.insert_system_card(decision_cancelled_card(card.clone(), task, &title, domain.revision + 1))?;
        effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: card });
    }
    Ok(())
}
/// The card a request of the task's SDK execution was asked with, and its epoch, when `card`
/// is that card or one of the questions asked together with it.
pub(crate) fn asked_decision(domain: &DomainState, task: &TaskId, card: &Interaction) -> Option<(InteractionId, u64)> {
    let held = match &card.card.group {
        Some(group) => QuestionGroup::card_id(&InteractionId(group.id.clone()), 1),
        None => card.id.clone(),
    };
    domain.lifecycle_journal.get(task)?.iter().rev().find_map(|record| match record {
        LifecycleRecord::DecisionAsked { card, epoch, .. } if *card == held => Some((held.clone(), *epoch)),
        _ => None,
    })
}
/// The user answered a card of a decision Claude waits on: once every question asked with it
/// has its answer, the answers are the decision. False for any other card.
pub(crate) fn decide_sdk_card(domain: &mut DomainState, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<bool> {
    let Some(answered) = domain.interactions.get(card) else { return Ok(false) };
    let Scope::Task { task_id } = answered.scope.clone() else { return Ok(false) };
    let Some((asked, epoch)) = asked_decision(domain, &task_id, answered) else { return Ok(false) };
    let members: Vec<Interaction> = domain.group_members(answered).into_iter().cloned().collect();
    let lifecycle = domain.lifecycle(lifecycles, &task_id)?;
    if lifecycle.cards().iter().any(|(held, _)| **held == *card) && lifecycle.card_callback(card).is_none() {
        return Ok(false);
    }
    let callback = alondra_domain::decision_callback(lifecycle.generation(), epoch);
    let held = lifecycle.cards().into_iter()
        .map(|(card, _)| card.clone())
        .find(|card| lifecycle.card_callback(card) == Some(&callback))
        .ok_or_else(|| DomainError::new("conflict", format!("No card holds the decision {asked} asked")))?;
    let open = members.iter().find(|member| !member.withdrawn && !DomainState::answered_question(member));
    if let Some(open) = open {
        if held == *card {
            domain.decide(lifecycles, &task_id, LifecycleRecord::CardMoved { from: held, to: open.id.clone() })?;
        }
        return Ok(true);
    }
    if members.iter().any(|member| member.withdrawn) {
        return Ok(true);
    }
    let hold = domain.holds.get(&asked).copied().ok_or_else(|| DomainError::new("conflict", format!("Card {held} holds no decision")))?;
    let InteractionPayload::Question { answer: Some(answer), .. } = &members[0].payload else {
        return fail("conflict", "The decision has no answer");
    };
    let note = note(answer);
    if hold == Hold::Permission && answer.option_id == Hold::FOR_TASK {
        let allows = members[0].card.allow_for_task.clone().filter(AllowForTask::offered)
            .ok_or_else(|| DomainError::new("invalid_input", "This permission cannot be allowed in its task"))?;
        let open = domain.tasks.get_mut(&task_id).filter(|t| !t.over())
            .ok_or_else(|| DomainError::new("task_not_active", "The task is over: there is nothing left to allow in it"))?;
        open.allowed.insert(asked.clone(), TaskGrant { allows, uses: 0 });
    }
    let decision = match (hold, answer.option_id.as_str()) {
        (Hold::Plan, Hold::APPROVE) if hands_over(domain, &task_id, card) => DecisionRecord::HandedOver,
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
    domain.decide(lifecycles, &task_id, LifecycleRecord::Decided { card: held, epoch, decision })?;
    Ok(true)
}
