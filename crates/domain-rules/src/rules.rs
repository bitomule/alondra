//! What each command changes in the domain's state, the passes every command runs after it, and
//! the cards they raise. The guards that decide whether a command may run, and every read, live in
//! the model (`alondra_domain_model`), which the screens share.
use alondra_domain_model::*;
use alondra_domain_model::checks::*;
use alondra_domain_model::checks::{decisions::*, mcp::*, spec_rules::*};
use std::collections::{BTreeMap, BTreeSet};

#[path = "account_login.rs"]
mod account_login;
#[path = "account_settings.rs"]
mod account_settings;
#[path = "claude_access.rs"]
mod claude_access;
#[path = "compaction.rs"]
mod compaction;
#[path = "decisions.rs"]
mod decisions;
mod effect;
#[path = "mcp.rs"]
mod mcp;
#[path = "mcp_wait.rs"]
mod mcp_wait;
#[path = "node_machine.rs"]
mod node_machine;
mod open_pr;
mod plan_review;
mod pr_identity;
mod scheduled_launch;
mod snooze;
mod spec_rules;
mod validation_rules;
#[path = "stage_rules.rs"]
mod stage_rules;
mod stall;

pub(crate) use self::{account_login::*, account_settings::*, claude_access::*, compaction::*, decisions::*, effect::*, mcp::*, mcp_wait::*, node_machine::*, open_pr::*, plan_review::*, pr_identity::*, scheduled_launch::*, snooze::*, spec_rules::*, stage_rules::*, stall::*, validation_rules::*};

type Result<T, E = DomainError> = std::result::Result<T, E>;


pub(crate) fn pr_head_mismatch(pr: &PullRequest, head: &str) -> String {
    format!("PR #{} is at {}, not {head}. If {head} is your latest commit, push it and present again; \
        to present the commit the PR is at, give head {} or leave head out", pr.number, pr.head, pr.head)
}
/// The cards of several questions asked together: 2 to 4 that each stand on their own.
pub(crate) fn group_cards(group: &InteractionId, why: &str, questions: &[GroupQuestion]) -> Result<Vec<(InteractionId, CardSpec, Vec<QuestionOption>)>> {
    id(&group.0)?;
    if !(2..=QuestionGroup::MAX).contains(&questions.len()) {
        return refuse("invalid_input", "questions", &format!("Ask 2 to {} questions together, or one question with options", QuestionGroup::MAX));
    }
    questions.iter().enumerate().map(|(at, question)| {
        if question.need.trim().is_empty() || question.need.contains('\n') {
            return refuse("invalid_input", "card.need", "Every question is one clear line that stands on its own");
        }
        let own_why = if question.why.trim().is_empty() { why } else { question.why.as_str() };
        let mut card = CardSpec::new(CardKind::Decide, question.need.trim()).why(own_why.trim());
        card.title = question.title.clone();
        card.need = question.need.clone();
        card.recommended = question.recommended.clone();
        card.group = Some(QuestionGroup { id: group.0.clone(), index: (at + 1) as u8, total: questions.len() as u8 });
        Ok((QuestionGroup::card_id(group, at + 1), card, question.options.clone()))
    }).collect()
}
pub(crate) fn delivery_withdrawn_behind_document(title: &str) -> String {
    format!("The document “{title}” has a newer revision than the one your final delivery carries, so that delivery \
was withdrawn and the task is in progress again. Check the new revision, then present your result again with it.")
}
pub(crate) fn delivery_withdrawn() -> String {
    "Your final delivery was withdrawn, so the task is in progress again. When the work is ready, present your \
result again.".into()
}
pub(crate) fn pr_not_open(number: u64) -> String {
    format!("PR #{number} is no longer open: nothing was registered. Open a new pull request for this work; Alondra adds it to this task.")
}
pub(crate) fn prs_missing_from_delivery(prs: &[&PullRequest]) -> String {
    let names = prs.iter().map(|pr| format!("{}#{}", pr.repository, pr.number)).collect::<Vec<_>>().join(", ");
    format!("{names} open in this task: a text result cannot be merged. Present them in the final delivery with targets [{{\"kind\":\"pull_request\"}}], which takes every open PR of the task: the user reviews and merges each on its own card")
}
pub(crate) fn opened_pr_unread(pr: &OpenedPr) -> String {
    format!("Alondra is still reading {}#{} from GitHub, a PR of this task: nothing was presented. Send the same result again in a moment", pr.repository, pr.number)
}
pub(crate) fn opened_pr_after_delivery(repository: &str, number: u64) -> String {
    format!("{repository}#{number} is now part of this task. Your last delivery does not carry it: present the final result again with targets [{{\"kind\":\"pull_request\"}}], which takes every open PR of the task.")
}
/// The PRs of the Task's final delivery still open: neither merged nor closed.
pub(crate) fn unmerged_targets(domain: &DomainState, task: &TaskId) -> usize {
    let Some(InteractionPayload::Delivery { targets, .. }) = domain.tasks.get(task)
        .and_then(|t| t.final_presentation.as_ref())
        .and_then(|p| domain.interactions.get(p))
        .map(|p| &p.payload)
    else {
        return 0;
    };
    targets.iter().filter(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. }
        if domain.pull_requests.get(pr_id).is_some_and(|pr| pr.is_tracked() && pr.state == PrState::Open))).count()
}
pub(crate) fn no_cancelled_dependency(domain: &DomainState, dependencies: &BTreeSet<TaskId>) -> Result<()> {
    match dependencies.iter().find(|d| domain.tasks.get(*d).is_some_and(Task::cancelled)) {
        Some(cancelled) => refuse("invalid_input", "dependencies", &format!(
            "Task {cancelled} is cancelled, so a task that waits for it would never start: leave it out, wait for the task that replaces it, or reopen it")),
        None => Ok(()),
    }
}
/// A task ends only once every worktree it made or took has ended.
pub(crate) fn no_open_worktrees(domain: &DomainState, task: &TaskId) -> Result<()> {
    match domain.task(task)?.open_worktrees().next() {
        Some((path, _)) => fail("worktrees_open", &format!("Task {task} still has its worktree {path}: it ends only once each of its worktrees is removed")),
        None => Ok(()),
    }
}
/// An Agent works on one Task at a time: the other non-terminal Task it holds, if any.
/// What the next node of a task whose node is being taken from it needs of that node.
pub(crate) fn closed_node(domain: &DomainState, task: &TaskId, cwd: Option<String>, relaunch: Relaunch) -> ClosedNode {
    let agent = domain.tasks.get(task).and_then(|task| task.agent_id.as_ref());
    let name = agent.and_then(|agent| domain.agents.get(agent)).map(|known| known.name.clone())
        .or_else(|| agent.map(|agent| format!("node/{}", agent.0.chars().take(8).collect::<String>())))
        .unwrap_or_else(|| format!("node/{}", task.0));
    ClosedNode { name, cwd, relaunch }
}
pub(crate) fn busy_with<'a>(domain: &'a DomainState, agent: &AgentId, except: &TaskId) -> Option<&'a Task> {
    domain.tasks.values().find(|t| &t.id != except && t.agent_id.as_ref() == Some(agent) && !t.over())
}
/// A node works on one Task for its whole life: the other Task, open or not, it holds or held.
pub(crate) fn used_by_other(domain: &DomainState, agent: &AgentId, except: &TaskId) -> Result<()> {
    match domain.tasks.values().find(|t| &t.id != except && t.agent_id.as_ref() == Some(agent)) {
        Some(other) => fail("agent_used", &format!(
            "Node {} already worked on task “{}”, and a node works on one task only: launch a new node for this task with launch.",
            domain.agents.get(agent).map_or(agent.0.as_str(), |a| a.name.as_str()), other.title)),
        None => Ok(()),
    }
}
pub(crate) fn free(domain: &DomainState, agent: &AgentId, task: &TaskId) -> Result<()> {
    match busy_with(domain, agent, task) {
        Some(other) => fail("agent_busy", &format!("Agent {} already works on Task {}: launch a new node for this Task", agent.0, other.id.0)),
        None => Ok(()),
    }
}
pub(crate) fn task_editor(domain: &DomainState, actor: &Actor, task: &TaskId) -> Result<()> {
    domain.active(task)?;
    if matches!(actor, Actor::Agent { .. }) {
        domain.assigned(actor, task)?;
    } else {
        domain.manager(actor)?;
    }
    Ok(())
}
/// An open PR can be reviewed before declared work after its merge is proved. Every mark
/// still needs evidence; omitted points remain in the approved plan until a later delivery.
pub(crate) fn checklist_marks_cover_the_plan(domain: &DomainState, task_id: &TaskId, is_final: bool, marks: &[ChecklistMark], attachments: &[AttachmentRef], targets: &[DeliveryTarget]) -> Result<()> {
    let plan = domain.checklists.get(task_id);
    if marks.is_empty() && (!is_final || plan.is_none()) {
        return Ok(());
    }
    if !is_final {
        return refuse("checklist_not_final", "checklist", "only the final delivery marks the plan's points done: leave checklist out of this result");
    }
    let Some(plan) = plan else {
        return refuse("checklist_without_plan", "checklist", "this task has no approved plan: leave checklist out");
    };
    let kinds = EvidenceKind::listed();
    let mut marked = BTreeSet::new();
    for mark in marks {
        let point = &mark.item;
        if !plan.items.iter().any(|item| item.id == *point) {
            return refuse("checklist_unknown_item", "checklist", &format!("point {point} is not in the approved plan (points 1–{})", plan.items.len()));
        }
        if !marked.insert(point.as_str()) {
            return refuse("checklist_unknown_item", "checklist", &format!("point {point} is marked twice: mark each point once, with all its evidence"));
        }
        if mark.evidence.iter().all(|e| e.value.trim().is_empty()) {
            return refuse("checklist_without_evidence", "checklist", &format!("point {point} has no evidence: add at least one ({kinds})"));
        }
        if mark.evidence.len() > EVIDENCE_MAX_PER_ITEM {
            return refuse("invalid_input", "checklist", &format!("point {point} has more than {EVIDENCE_MAX_PER_ITEM} pieces of evidence: keep the ones that prove it"));
        }
        for evidence in &mark.evidence {
            let value = evidence.value.trim();
            if value.is_empty() || value.chars().count() > EVIDENCE_MAX_TEXT || evidence.detail.as_ref().is_some_and(|d| d.chars().count() > EVIDENCE_MAX_TEXT) {
                return refuse("invalid_input", "checklist", &format!("point {point}: every piece of evidence has a value, and value and detail are at most {EVIDENCE_MAX_TEXT} characters"));
            }
            match evidence.kind {
                EvidenceKind::Commit if !is_commit_sha(value) => {
                    return refuse("invalid_input", "checklist", &format!("point {point}: {value} is not a commit sha: cite the commit by its sha"));
                }
                EvidenceKind::Screenshot if !attachments.iter().any(|a| a.id == value) => {
                    return refuse("evidence_unknown_attachment", "checklist", &format!("point {point}: screenshot {value} is not one of this result's attachments: attach it and cite its id"));
                }
                EvidenceKind::Pr if !domain.task(task_id)?.pr_ids().any(|pr| pr.0 == value) => {
                    return refuse("evidence_unknown_pr", "checklist", &format!("point {point}: {value} is not this task's registered PR: cite its pr_id"));
                }
                EvidenceKind::Scenario if !task_scenario(domain, task_id, value) => {
                    return refuse("evidence_unknown_scenario", "checklist", &format!("point {point}: {value} is not a scenario of this task's specs: cite it as <spec>/<scenario>"));
                }
                _ => {}
            }
        }
    }
    let missing: Vec<String> = plan.items.iter()
        .filter(|item| !marked.contains(item.id.as_str()))
        .map(|item| format!("{} “{}”", item.id, item.text))
        .collect();
    let task = domain.task(task_id)?;
    let after_merge = task.wrap_up.post_merge_work.is_some() && targets.iter().any(|target| match target {
        DeliveryTarget::PullRequest { pr_id, .. } => domain.pull_requests.get(pr_id).is_some_and(|pr| pr.task_id == *task_id && pr.state == PrState::Open),
        _ => false,
    });
    if !missing.is_empty() && !after_merge {
        return refuse("checklist_incomplete", "checklist", &format!(
            "the approved plan has {} points and these are not marked done: {}. Finish them and add each to checklist with its evidence ({kinds}). For work that follows an open PR's merge, declare post_merge_work with reason first, then present that PR with only the completed points in checklist; omitted points remain pending. After the merge, present every point with evidence before ending post_merge_work or calling cleanup_done. If a point no longer applies, send a revised plan with ExitPlanMode; once the user approves it, it replaces this checklist",
            plan.items.len(), missing.join(", ")));
    }
    Ok(())
}
pub(crate) fn suggests(i: &Interaction) -> bool {
    matches!(i.author, Author::Agent { .. }) && i.recipient == Recipient::Orchestrator && i.card.kind == CardKind::Propose
        && matches!(i.payload, InteractionPayload::Question { .. })
}
/// The orchestrator settles a node's suggestion with one decision, in one command: a new task
/// is created here, under a request of its own, or nothing is recorded at all.
pub(crate) fn settle_suggestion(domain: &mut DomainState, actor: &Actor, interaction_id: &InteractionId, decision: &SuggestionDecision,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    domain.fence(actor)?;
    let discarded_by_alondra = matches!(actor, Actor::System) && matches!(decision, SuggestionDecision::Discard { .. });
    if !matches!(actor, Actor::Orchestrator { .. }) && !discarded_by_alondra {
        return fail("forbidden", "Only the orchestrator settles a suggestion; Alondra only discards one left unsettled");
    }
    let i = domain.interaction(interaction_id)?.clone();
    let (CardKind::Propose, Author::Agent { .. }, Scope::Task { task_id }) = (i.card.kind, &i.author, &i.scope) else {
        return refuse("invalid_action_for_kind", "interaction_id", "Only a node's suggestion is settled: answer other questions with answer_question");
    };
    match &i.payload {
        InteractionPayload::Question { answer: Some(_), .. } =>
            return refuse("question_already_answered", "interaction_id", "This suggestion is already settled and its decision is final"),
        InteractionPayload::Question { answer: None, .. } => {}
        _ => return fail("invalid_action_for_kind", "This Interaction cannot be answered"),
    }
    if i.withdrawn {
        return fail("withdrawn", "Its author withdrew this suggestion");
    }
    match decision {
        SuggestionDecision::NewTask { task_id: new_task, request_id, title, launch, dependencies, note } => {
            if let Some(note) = note {
                nonempty(note)?;
            }
            id(&request_id.0)?;
            apply(domain, actor, &Command::CreateRequest {
                request_id: request_id.clone(), body: format!("{}\n\n{}", i.card.effective_title(), i.body),
                auto_merge: false, maintenance: false, message: true,
            }, effects, created, lifecycles)?;
            if let (Author::Agent { agent_id }, Some(request)) = (&i.author, domain.requests.get_mut(request_id)) {
                request.origin = RequestOrigin::Node { agent_id: agent_id.clone() };
            }
            apply(domain, actor, &Command::TitleRequest { request_id: request_id.clone(), title: title.clone() }, effects, created, lifecycles)?;
            apply(domain, actor, &Command::CreateTask { priority: Default::default(), plan_items: None, continues: None,
                task_id: new_task.clone(), request_id: request_id.clone(), title: title.clone(), agent_id: None,
                review_prs: false, dependencies: dependencies.clone(), stack: None, launch: Some(launch.clone()),
            }, effects, created, lifecycles)?;
        }
        SuggestionDecision::CurrentTask { note } => {
            nonempty(note)?;
            if domain.task(task_id)?.over() {
                return refuse("task_not_active", "decision.kind",
                    "The task that suggested it has ended, so it cannot do it: create a new task or discard it");
            }
        }
        SuggestionDecision::Discard { reason } => nonempty(reason)?,
    }
    let inputs = decision.note().map(|note| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.to_owned()))])).unwrap_or_default();
    let at = domain.revision + 1;
    let i = domain.interactions.get_mut(interaction_id).unwrap();
    if let InteractionPayload::Question { answer, .. } = &mut i.payload {
        *answer = Some(Answer {
            attachments: vec![], option_id: decision.option_id().into(), inputs,
            responder: if discarded_by_alondra { "alondra" } else { "alondra-orchestrator" }.into(), at_revision: at, decision: Some(decision.clone()),
        });
    }
    i.revision = at;
    notify(domain, task_id, Some(interaction_id.clone()), "Suggestion settled", effects)
}
pub(crate) fn node_answer(domain: &DomainState, task_id: &TaskId, card: &InteractionId) -> Result<()> {
    if !domain.answers_a_node_card(task_id, card) {
        return refuse("invalid_input", "answers", &format!("Card {card} is not an answered card of a node of task {task_id}"));
    }
    Ok(())
}
/// The PR is the task's from now on: its first one, or one more, in any repository.
pub(crate) fn adopt_pr(domain: &mut DomainState, mut pr: PullRequest) -> Result<()> {
    pr.review = domain.task(&pr.task_id)?.review_prs.then(|| PullRequestReview { decisions: vec![] });
    let task = domain.tasks.get_mut(&pr.task_id).unwrap();
    if task.pr_id.is_none() {
        task.pr_id = Some(pr.id.clone());
    } else {
        task.later_prs.push(pr.id.clone());
    }
    domain.pull_requests.insert(pr.id.clone(), pr);
    Ok(())
}
/// R12: a delivery shows the current head of its own Task's registered PR.
pub(crate) fn current_pr_targets(domain: &DomainState, task: &TaskId, targets: &[DeliveryTarget], is_final: bool) -> Result<()> {
    for target in targets {
        let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
        let Some(pr) = domain.pull_requests.get(pr_id) else {
            return refuse("pr_mismatch", "targets.pr_id", &format!("PR {pr_id} is not registered: call register_pr first"));
        };
        if &pr.task_id != task {
            return refuse("pr_mismatch", "targets.pr_id", &format!("PR {pr_id} belongs to another Task: present only your own Task's PR"));
        }
        if pr.state == PrState::Closed {
            let mut error = DomainError::new("pr_not_open", format!("targets.pr_id: PR {}#{} is closed: deliver the work's open PRs, or a text result for work already finished", pr.repository, pr.number));
            error.subject = Some("targets.pr_id".into());
            return Err(error.breaking(Some(if is_final { CardType::Result } else { CardType::Progress }), ContractRule::PrNotOpen));
        }
        if &pr.head != head {
            return refuse("pr_mismatch", "targets.head", &pr_head_mismatch(pr, head));
        }
    }
    Ok(())
}
/// A Request is closed or cleared only while none of its Tasks is open: a Task that starts again (a reply
/// after its merge, a reopening) takes its Request back, so open work never sits under a Request the
/// lists consider completed.
pub(crate) fn reopen_requests_with_open_tasks(domain: &mut DomainState) {
    let with_open_task: BTreeSet<&RequestId> = domain.tasks.values().filter(|task| !task.over()).map(|task| &task.request_id).collect();
    for request in domain.requests.values_mut().filter(|request| (request.closed || request.cleared) && with_open_task.contains(&request.id)) {
        request.closed = false;
        request.withdrawn = false;
        request.answer = None;
        request.cleared = false;
    }
}
/// A message to the orchestrator that made tasks is done once the last of them ends: nobody
/// is left to close it, since `close_request` waits for its tasks.
pub(crate) fn close_finished_messages(domain: &mut DomainState) {
    let ended = open_requests_with_every_task_over(domain);
    for request in domain.requests.values_mut().filter(|request| request.message && ended.contains(&request.id)) {
        request.closed = true;
    }
}
/// A Request gets another Task only while it has none, once all of its Tasks are over, or in
/// the same orchestrator batch that created the ones it has.
pub(crate) fn may_add_task(domain: &DomainState, request_id: &RequestId) -> bool {
    let Some(base) = domain.orchestrator_batch_base else {
        return true;
    };
    domain.tasks.values()
        .filter(|task| &task.request_id == request_id)
        .all(|task| task.over() || task.created_revision > base)
}
/// A Request folds only into an open Task of another Request, and only while it has no Task of its own.
pub(crate) fn check_fold(domain: &DomainState, request_id: &RequestId, task_id: &TaskId, withdrawn: bool) -> Result<()> {
    if withdrawn {
        return refuse("invalid_input", "folded_into", "A withdrawn Request folds into no Task: omit folded_into or withdrawn");
    }
    let Some(task) = domain.tasks.get(task_id) else {
        return refuse("not_found", "folded_into", "No Task has this id: name the open Task that already covers this Request");
    };
    if task.over() {
        return refuse("fold_target_over", "folded_into", "This Task is over: create a Task for this Request, or fold it into one still open");
    }
    if domain.tasks.values().any(|t| &t.request_id == request_id) {
        return refuse("fold_request_has_tasks", "folded_into", "This Request already has Tasks of its own: close it without folded_into");
    }
    Ok(())
}
/// The orchestrator closes a Request with one outcome, and the outcome is what the Request and
/// the other fields show: answered or covered only while it has no Task of its own, its Tasks
/// ended only when it has some, covered with the Tasks it names, folded into a Task, withdrawn
/// as withdrawn. The user and Alondra may leave it out; one they give is checked the same way.
pub(crate) fn check_close_outcome(domain: &DomainState, actor: &Actor, request_id: &RequestId, outcome: Option<CloseOutcome>, withdrawn: bool,
    folded_into: Option<&TaskId>, covered_by: &[TaskId]) -> Result<()> {
    let Some(outcome) = outcome else {
        if matches!(actor, Actor::Orchestrator { .. }) {
            return refuse("close_outcome_required", "outcome",
                "Say what came of the Request: answered, covered (with covered_by), folded (with folded_into), withdrawn or tasks_ended");
        }
        if !covered_by.is_empty() {
            return refuse("close_outcome_mismatch", "covered_by", "covered_by goes with the outcome covered");
        }
        return Ok(());
    };
    let has_tasks = domain.tasks.values().any(|task| &task.request_id == request_id);
    let mismatch = |fix: &str| refuse("close_outcome_mismatch", "outcome", fix);
    if (outcome == CloseOutcome::Withdrawn) != withdrawn {
        return mismatch("withdrawn goes with the outcome withdrawn, and only with it");
    }
    if (outcome == CloseOutcome::Folded) != folded_into.is_some() {
        return mismatch("folded_into goes with the outcome folded, and only with it");
    }
    if (outcome == CloseOutcome::Covered) != !covered_by.is_empty() {
        return mismatch("covered_by goes with the outcome covered, and it names at least one Task");
    }
    match outcome {
        CloseOutcome::Answered | CloseOutcome::Covered if has_tasks =>
            mismatch("This Request has Tasks of its own: once every one has ended, close it with the outcome tasks_ended"),
        CloseOutcome::TasksEnded if !has_tasks =>
            mismatch("This Request has no Task of its own: close it as answered, covered, folded or withdrawn"),
        _ => Ok(()),
    }
}
/// A Request the user made is never closed by the orchestrator in silence. One whose Tasks
/// ended gets its Done card from them; any other, settled with no Task of its own (answered,
/// covered or folded), and any it withdraws, Tasks or none, has `answer` tell the user, who
/// reads it on the Request's card. A message, a maintenance message and a Request a node
/// raised need none.
pub(crate) fn check_close_answer(domain: &DomainState, actor: &Actor, request_id: &RequestId, answer: Option<&str>, withdrawn: bool) -> Result<()> {
    let Some(request) = domain.requests.get(request_id) else { return Ok(()) };
    let has_tasks = domain.tasks.values().any(|task| &task.request_id == request_id);
    let answers_the_user = matches!(actor, Actor::Orchestrator { .. }) && request.origin.the_user_asked() && !request.maintenance && !request.message
        && (!has_tasks || withdrawn);
    if answers_the_user && answer.is_none() {
        return refuse("close_answer_required", "answer",
            "The user made this Request: say in answer what came of it, or why it no longer applies; the user reads it on the Request's card");
    }
    Ok(())
}
/// The Tasks a Request is closed as covered by are Tasks Alondra knows, whatever their state:
/// an archived or finished Task still counts, a made-up id never does.
pub(crate) fn check_cover(domain: &DomainState, covered_by: &[TaskId], withdrawn: bool) -> Result<()> {
    if withdrawn && !covered_by.is_empty() {
        return refuse("invalid_input", "covered_by", "A withdrawn Request is covered by no Task: omit covered_by or withdrawn");
    }
    if let Some(missing) = covered_by.iter().find(|task| !domain.tasks.contains_key(*task)) {
        return refuse("not_found", "covered_by", &format!("No Task has the id {missing}: name only Tasks that exist, as tasks_in_progress or task_detail shows them"));
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn close_request(domain: &mut DomainState, request_id: &RequestId, answer: Option<&String>, withdrawn: bool, title: Option<&String>, headline: Option<&str>,
    created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let request = domain
        .requests
        .get_mut(request_id)
        .ok_or_else(|| DomainError::new("not_found", "Request missing"))?;
    request.closed = true;
    request.withdrawn = withdrawn;
    if answer.is_some() {
        request.answer = answer.cloned();
    }
    if title.is_some() {
        request.title = title.cloned();
    }
    let tell = !request.maintenance && (!request.withdrawn || answer.is_some()) && request.origin.the_user_asked();
    let has_tasks = domain.tasks.values().any(|t| &t.request_id == request_id);
    // A withdrawn Request is told why it no longer applies, never that it is done.
    if tell && has_tasks && !withdrawn {
        tell_it_is_done(domain, request_id, answer.map(String::as_str), created, effects)?;
    } else if let Some(answer) = answer.filter(|_| tell) {
        tell_the_answer(domain, request_id, answer, headline, created, effects)?;
    }
    Ok(())
}
/// A request whose tasks just ended closes by itself when they built every approved plan it
/// holds, its Done card summing up each task's final result. Without a plan, or with a point
/// of one no finished task delivered, it stays for the orchestrator.
pub(crate) fn close_delivered_plans(domain: &mut DomainState, before: &DomainState, created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let ended_before = open_requests_with_every_task_over(&before);
    for request_id in open_requests_with_every_task_over(domain) {
        if ended_before.contains(&request_id) {
            continue;
        }
        if let Some(summary) = delivered_plans_summary(domain, &request_id) {
            close_request(domain, &request_id, Some(&summary), false, None, None, created, effects)?;
        }
    }
    Ok(())
}
/// What each task of the request delivered, when every one of them finished and together they
/// marked done every point of every plan approved for it.
pub(crate) fn delivered_plans_summary(domain: &DomainState, request: &RequestId) -> Option<String> {
    let mut tasks: Vec<&Task> = domain.tasks.values().filter(|t| &t.request_id == request).collect();
    let carried_on: BTreeSet<&TaskId> = tasks.iter().filter_map(|t| t.continues.as_ref()).collect();
    tasks.retain(|t| !carried_on.contains(&t.id));
    if tasks.iter().any(|t| t.state != TaskState::Done) {
        return None;
    }
    let held: Vec<(&Task, &Checklist)> = tasks.iter().filter_map(|t| domain.checklists.get(&t.id).map(|c| (*t, c))).collect();
    let mut plans: BTreeSet<&InteractionId> = held.iter().map(|(_, c)| &c.plan).collect();
    plans.extend(domain.request_plans.get(request).map(|approved| &approved.presentation_id));
    if plans.is_empty() {
        return None;
    }
    for plan in plans {
        let mut holders = held.iter().filter(|(_, c)| &c.plan == plan).peekable();
        holders.peek()?;
        if !holders.all(|(t, c)| marked_every_point(domain, t, c)) {
            return None;
        }
    }
    tasks.sort_by_key(|t| t.revision);
    let lines: Vec<String> = tasks.iter().filter_map(|t| {
        let result = domain.interactions.get(t.final_presentation.as_ref()?)?;
        let said = result.card.content.as_ref().and_then(|c| c.summary.as_deref()).map(str::trim).filter(|s| !s.is_empty())
            .map_or_else(|| body_title(&result.body).trim_start_matches('#').trim().to_owned(), str::to_owned);
        Some(if said.is_empty() { t.title.clone() } else { format!("{}: {said}", t.title) })
    }).collect();
    Some(lines.join("\n")).filter(|summary| !summary.is_empty())
}
pub(crate) fn marked_every_point(domain: &DomainState, task: &Task, checklist: &Checklist) -> bool {
    let Some(marks) = task.final_presentation.as_ref().and_then(|p| domain.checklist_marks.get(p)) else { return false };
    checklist.items.iter().all(|item| marks.iter().any(|mark| mark.item == item.id))
}
/// Open Requests that have Tasks and none of them still open: only the orchestrator moves them
/// on, by closing them or creating their next Tasks.
pub(crate) fn open_requests_with_every_task_over(domain: &DomainState) -> BTreeSet<RequestId> {
    let mut open: BTreeSet<&RequestId> = BTreeSet::new();
    let mut ended: BTreeSet<&RequestId> = BTreeSet::new();
    for task in domain.tasks.values() {
        if task.over() { &mut ended } else { &mut open }.insert(&task.request_id);
    }
    ended.into_iter()
        .filter(|id| !open.contains(id) && domain.requests.get(*id).is_some_and(|r| !r.closed))
        .cloned()
        .collect()
}
pub(crate) fn request_of_scope<'a>(domain: &'a DomainState, scope: &Scope) -> Option<&'a Request> {
    let id = match scope {
        Scope::Orchestrator { request_id } => request_id.as_ref()?,
        Scope::Task { task_id } => &domain.tasks.get(task_id)?.request_id,
    };
    domain.requests.get(id)
}
/// A card the orchestrator wrote about the request before Alondra wrote its outcome cards.
pub(crate) fn orchestrator_told(domain: &DomainState, request: &RequestId) -> bool {
    domain.interactions.values().any(|i| {
        i.recipient == Recipient::User
            && matches!(i.author, Author::Orchestrator { .. })
            && matches!(i.payload, InteractionPayload::Information { .. })
            && matches!(&i.scope, Scope::Orchestrator { request_id: Some(id) } if id == request)
    })
}
/// R22: a request of the user closed without tasks tells them its answer.
pub(crate) fn tell_the_answer(domain: &mut DomainState, request: &RequestId, answer: &str, headline: Option<&str>,
    created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
    if orchestrator_told(domain, request) {
        return Ok(());
    }
    let answer = answer.trim();
    let card = match headline {
        Some(headline) if answer.chars().count() > CardSpec::WHY_MAX || answer.lines().filter(|l| !l.trim().is_empty()).count() > 1 =>
            CardSpec::new(CardKind::Know, headline).content(ContentKind::Report, answer),
        Some(headline) => CardSpec::new(CardKind::Know, headline).why(answer),
        None => {
            let title = domain.requests.get(request).and_then(|request| request.title.as_deref())
                .map(|title| title.split_whitespace().collect::<Vec<_>>().join(" "));
            let title = title.as_deref().filter(|title| !title.is_empty()).unwrap_or("Answer");
            CardSpec::new(CardKind::Know, clip(title, CardSpec::TITLE_MAX)).content(ContentKind::Report, answer)
        }
    };
    tell_the_user(domain, format!("{request}-answer"), request, answer.into(), card, None, created, effects)
}
/// A request of the user closed once all its tasks are over tells them how it ended.
pub(crate) fn tell_it_is_done(domain: &mut DomainState, request: &RequestId, summary: Option<&str>,
    created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
    if orchestrator_told(domain, request) {
        return Ok(());
    }
    let mut tasks: Vec<&Task> = domain.tasks.values().filter(|t| &t.request_id == request).collect();
    let carried_on: BTreeSet<&TaskId> = tasks.iter().filter_map(|t| t.continues.as_ref()).collect();
    tasks.retain(|t| !carried_on.contains(&t.id));
    tasks.sort_by_key(|t| t.revision);
    let done = alondra_domain_model::request_outcome::DoneCard {
        request_id: request.0.clone(),
        summary: summary.map(str::trim).filter(|s| !s.is_empty()).map(String::from),
        tasks: tasks.iter().map(|t| alondra_domain_model::request_outcome::DoneTask {
            id: t.id.0.clone(),
            title: t.title.clone(),
            cancelled: t.cancelled(),
            pr: domain.merged_pr(t).map(|pr| pr.number),
            result: t.final_presentation.as_ref().and_then(|id| domain.interactions.get(id))
                .and_then(|delivery| delivery.card.content.as_ref().map(|content| content.body.as_str())
                    .filter(|body| !body.trim().is_empty()).or(Some(delivery.body.as_str())))
                .map(str::trim).filter(|body| !body.is_empty()).map(String::from),
        }).collect(),
    };
    let (need, why) = done.text(alondra_core::text::Lang::En);
    let card = CardSpec::new(CardKind::Know, need).why(why);
    retire_unread_tasks_cards(domain, request);
    tell_the_user(domain, alondra_domain_model::request_outcome::DoneCard::id(&request.0), request, done.to_body(), card, Some(SystemCard::RequestDone), created, effects)
}
/// `quiet` records the tasks as announced on a withdrawn card nobody sees: what the daemon
/// does with the tasks it finds when it opens its domain, which the user already knows.
pub(crate) fn announce_request_tasks(domain: &mut DomainState, quiet: bool, created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
    for (request, tasks) in domain.unannounced_tasks() {
        let earlier = domain.tasks_cards(&request).len();
        let card = alondra_domain_model::request_outcome::TasksCard {
            request_id: request.0.clone(),
            more: earlier > 0,
            tasks: tasks.iter().map(|id| {
                let task = &domain.tasks[id];
                alondra_domain_model::request_outcome::AnnouncedTask {
                    id: id.0.clone(),
                    title: task.title.clone(),
                    priority: task.priority,
                    after: task.dependencies.iter().chain(&task.delivered_dependencies)
                        .filter_map(|other| domain.tasks.get(other))
                        .map(|other| alondra_domain_model::request_outcome::After { id: other.id.0.clone(), title: other.title.clone() })
                        .collect(),
                }
            }).collect(),
        };
        let (need, why) = card.text(alondra_core::text::Lang::En);
        let spec = CardSpec::new(CardKind::Know, need).why(why);
        let id = alondra_domain_model::request_outcome::TasksCard::id(&request.0, earlier + 1);
        if quiet {
            write_system_card(domain, id, &request, card.to_body(), spec, Some(SystemCard::RequestTasks), true)?;
        } else {
            tell_the_user(domain, id, &request, card.to_body(), spec, Some(SystemCard::RequestTasks), created, effects)?;
        }
    }
    Ok(())
}
/// «Done» takes the place of the cards that named the request's tasks and nobody read.
pub(crate) fn retire_unread_tasks_cards(domain: &mut DomainState, request: &RequestId) {
    let unread: Vec<InteractionId> = domain.tasks_cards(request).into_iter()
        .filter(|(i, _)| !i.withdrawn && matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty()))
        .map(|(i, _)| i.id.clone())
        .collect();
    for card in &unread {
        domain.retire(card);
    }
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn tell_the_user(domain: &mut DomainState, id: String, request: &RequestId, body: String, card: CardSpec, system_card: Option<SystemCard>,
    created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let id = write_system_card(domain, id, request, body, card, system_card, false)?;
    created.push(id.0.clone());
    effects.push(DomainEffect::NotifyUser { task_id: None, interaction_id: id });
    Ok(())
}
/// A card of Alondra about a request; a `withdrawn` one only keeps what it records.
pub(crate) fn write_system_card(domain: &mut DomainState, id: String, request: &RequestId, body: String, card: CardSpec, system_card: Option<SystemCard>,
    withdrawn: bool) -> Result<InteractionId> {
    let mut id = InteractionId(id);
    while domain.interactions.contains_key(&id) {
        id.0.push('-');
    }
    domain.insert_system_card(Interaction {
        attachments: vec![],
        id: id.clone(),
        scope: Scope::Orchestrator { request_id: Some(request.clone()) },
        author: Author::System,
        recipient: Recipient::User,
        body,
        blocking: false,
        subject: None,
        payload: InteractionPayload::Information { read_by: BTreeSet::new() },
        revision: domain.revision + 1,
        order: domain.revision + 1,
        withdrawn,
        card,
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card, shortened: false,
    })?;
    Ok(id)
}
/// The user wrote to the task: its node owes them an answer before the task can finish, and
/// what the node presented before no longer settles it.
pub(crate) fn awaits_answer_to_user(domain: &mut DomainState, task: &TaskId, input: String) {
    invalidate_keeping_user_merges(domain, task, |state| state.invalidate(task));
    domain.tasks.get_mut(task).expect("the task the user wrote to exists").pending_user_inputs.insert(input);
}
/// A message the user sends after merging never takes the merge back: every authorization
/// the user gave stands, with the acceptance of its delivery, and the node still gets the
/// message. What only Alondra decided is decided again.
pub(crate) fn invalidate_keeping_user_merges(domain: &mut DomainState, task: &TaskId, invalidate: impl FnOnce(&mut DomainState)) {
    let kept: Vec<(AuthorizationId, InteractionId)> = domain.merge_authorizations.values()
        .filter(|a| &a.task_id == task && a.valid && a.user_id != Feedback::ALONDRA)
        .map(|a| (a.id.clone(), a.presentation_id.clone())).collect();
    let accepted: Vec<(InteractionId, bool)> = kept.iter().filter_map(|(_, presentation)| match &domain.interactions.get(presentation)?.payload {
        InteractionPayload::Delivery { acceptance: Some(a), .. } => Some((presentation.clone(), a.valid)),
        _ => None,
    }).collect();
    invalidate(domain);
    for (id, _) in &kept {
        domain.merge_authorizations.get_mut(id).expect("kept from the map").valid = true;
    }
    for (presentation, valid) in accepted {
        if let Some(InteractionPayload::Delivery { acceptance: Some(a), .. }) = domain.interactions.get_mut(&presentation).map(|p| &mut p.payload) {
            a.valid = valid;
        }
    }
}
pub(crate) fn invalidate_feedback(domain: &mut DomainState, task: &TaskId, target: &FeedbackTarget) {
    let FeedbackTarget::PullRequest { pr_id, .. } = target else {
        domain.invalidate(task);
        return;
    };
    if let Some(presentation) = domain.tasks[task].final_presentation.clone() {
        domain.decided(&presentation);
        if let InteractionPayload::Delivery { acceptance: Some(acceptance), .. } = &mut domain.interactions.get_mut(&presentation).unwrap().payload {
            acceptance.valid = false;
        }
    }
    for authorization in domain.merge_authorizations.values_mut().filter(|authorization| &authorization.task_id == task && &authorization.pr_id == pr_id) {
        authorization.valid = false;
    }
    if let Some(readiness) = domain.stack_readiness.get_mut(task).filter(|readiness| &readiness.pr_id == pr_id) {
        readiness.ready = false;
    }
}
pub(crate) fn notify(
    domain: &DomainState,
    task: &TaskId,
    interaction: Option<InteractionId>,
    message: &str,
    effects: &mut Vec<DomainEffect>,
) -> Result<()> {
    if let Some(agent_id) = &domain.task(task)?.agent_id {
        effects.push(DomainEffect::NotifyAgent {
            agent_id: agent_id.clone(),
            task_id: Some(task.clone()),
            interaction_id: interaction,
            message: message.into(),
        });
    }
    Ok(())
}
/// A node that waited on other Tasks goes on by itself once they are done: nobody has to
/// tell it, and it is told once, by the command that ended the wait.
pub(crate) fn resume_waiting_nodes(domain: &mut DomainState, before: &DomainState, effects: &mut Vec<DomainEffect>) {
    let revision = domain.revision + 1;
    let mut restamped = vec![];
    for t in domain.tasks.values() {
        let waited = before.tasks.get(&t.id).is_some_and(|b| b.state == TaskState::WaitingDependency);
        let declared_wait = before.tasks.get(&t.id).is_some_and(|b|
            b.waiting_for_tasks.iter().any(|id| !b.ended_task_waits.contains_key(id)));
        if !waited || (t.dependencies.is_empty() && !declared_wait) {
            continue;
        }
        let landed = landed_dependencies(domain, t);
        let message = match &t.wrap_up.post_merge_work {
            Some(work) if domain.works_after_merge(t) => {
                if t.state != TaskState::Running {
                    continue;
                }
                restamped.push(t.id.clone());
                post_merge_wait_over_notice(&t.id, &work.reason, &landed)
            }
            _ if matches!(t.state, TaskState::Running | TaskState::NeedsYou) && (t.final_presentation.is_none() || declared_wait) =>
                dependencies_done_notice(&landed, domain.branch_update),
            _ => continue,
        };
        let _ = notify(domain, &t.id, None, &message, effects);
    }
    for id in restamped {
        if let Some(work) = domain.tasks.get_mut(&id).and_then(|t| t.wrap_up.post_merge_work.as_mut()) {
            work.declared_at = revision;
        }
    }
}
/// The Tasks a node's work after its merge waits on, from the Tasks or pull requests it named.
pub(crate) fn wait_targets(domain: &DomainState, task: &TaskId, targets: &[WaitTarget], with_reason: bool) -> Result<BTreeSet<TaskId>> {
    if targets.is_empty() {
        return Ok(BTreeSet::new());
    }
    if !with_reason {
        return refuse("invalid_input", "reason",
            "waits_for needs reason: say in one line what you will do once what you wait for lands");
    }
    if domain.merged_pr(domain.task(task)?).is_none() {
        return refuse("invalid_input", "waits_for",
            "waits_for is for work after your pull request merged; before that, declare wait_for_tasks with the other task id");
    }
    let mut waited = BTreeSet::new();
    for target in targets {
        let other = match target {
            WaitTarget::Task { task_id } => match domain.tasks.get(task_id) {
                Some(t) => t,
                None => return refuse("invalid_input", "waits_for",
                    &format!("No Alondra task is called {task_id}: name the task by its id, or its pull request by repository and number")),
            },
            WaitTarget::PullRequest { repository, number } => {
                let pr = domain.pull_requests.values().find(|pr| pr.is_tracked() && pr.number == *number
                    && (pr.repository.eq_ignore_ascii_case(repository) || pr.aliases.iter().any(|alias| alias.eq_ignore_ascii_case(repository))));
                match pr.and_then(|pr| domain.tasks.get(&pr.task_id)) {
                    Some(t) => t,
                    None => return refuse("invalid_input", "waits_for", &format!(
                        "No Alondra task registered {repository}#{number}, so Alondra cannot see when it lands: ask the orchestrator for a task that delivers it, then wait on that task")),
                }
            }
        };
        if other.id == *task {
            return refuse("invalid_input", "waits_for", "Your task cannot wait on itself: name the other task or its pull request");
        }
        if other.cancelled() {
            return refuse("invalid_input", "waits_for",
                &format!("“{}” was cancelled, so it never lands: ask the orchestrator what replaces it", other.title));
        }
        if domain.landed(other) {
            return refuse("invalid_input", "waits_for",
                &format!("“{}” has already landed: go on with your work without waiting for it", other.title));
        }
        waited.insert(other.id.clone());
    }
    Ok(waited)
}
pub(crate) fn task_wait_result(domain: &DomainState, task: &Task) -> Landed {
    Landed {
        title: task.title.clone(),
        cancelled: task.cancelled(),
        result: task.final_presentation.as_ref().and_then(|p| domain.interactions.get(p)).map(|p| result_excerpt(&p.body).text),
        merged_pr: domain.merged_pr(task).map(|pr| format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number)),
    }
}
/// Each Task `t` depends on, with what it left: its final result and its merged PR.
pub(crate) fn landed_dependencies(domain: &DomainState, t: &Task) -> Vec<Landed> {
    t.dependencies.iter().chain(t.waiting_for_tasks.iter().filter(|id| !t.dependencies.contains(*id)))
        .filter_map(|id| t.ended_task_waits.get(id).cloned().or_else(|| domain.tasks.get(id).map(|dep| task_wait_result(domain, dep))))
        .collect()
}
/// Moves one phase of an approved plan to a new Task: the points leave the Task that held
/// them, so each point of the plan stays with exactly one Task.
pub(crate) fn take_plan_items(domain: &mut DomainState, task_id: &TaskId, request_id: &RequestId, taken: &PlanItems) -> Result<()> {
    let source = domain.tasks.get(&taken.from_task).filter(|t| t.request_id == *request_id);
    let Some(source) = source else {
        return refuse("invalid_input", "plan_items.from_task", "plan_items takes points from a task of this same request");
    };
    if matches!(source.state, TaskState::AwaitingBlocker | TaskState::ForYourReview | TaskState::Closing | TaskState::Done | TaskState::Cancelled) {
        return refuse("plan_items_unavailable", "plan_items.from_task",
            "that task has already delivered or closed, so its points are no longer free to take: create a new request for the work left");
    }
    let Some(held) = domain.checklists.get(&taken.from_task) else {
        return refuse("invalid_input", "plan_items.from_task", "that task has no approved plan to take points from");
    };
    let wanted: BTreeSet<&str> = taken.items.iter().map(String::as_str).collect();
    if wanted.is_empty() || wanted.len() != taken.items.len() || wanted.iter().any(|id| !held.items.iter().any(|item| item.id == *id)) {
        return refuse("plan_items_unknown", "plan_items.items",
            "name each point once, by its number in the approved plan, among the points that task still holds");
    }
    let (moved, kept): (Vec<_>, Vec<_>) = held.items.iter().cloned().partition(|item| wanted.contains(item.id.as_str()));
    if kept.is_empty() {
        return refuse("plan_items_take_all", "plan_items.items",
            "that task would keep no point of its plan: leave it the points of the phase it builds");
    }
    let plan = held.plan.clone();
    domain.checklists.insert(taken.from_task.clone(), Checklist { plan: plan.clone(), items: kept });
    domain.checklists.insert(task_id.clone(), Checklist { plan, items: moved });
    Ok(())
}
pub(crate) fn ready_to_start(domain: &DomainState, t: &Task) -> Result<()> {
    domain.waits_are_over(t)?;
    domain.stack_ready(t)
}
pub(crate) fn plan_waits(domain: &DomainState, task: &TaskId) -> Result<()> {
    let waited = domain.waited_tasks(task);
    if waited.is_empty() {
        return Ok(());
    }
    refuse("task_waiting", "option_id", &plan_while_waiting_notice(&waited))
        .map_err(|error| error.because(Refusal::WaitsForTask { title: waited.join(", ") }))
}
/// Every PR the task presented to the user was closed without merging, wherever it was closed,
/// before the user accepted the delivery, and none is open or being read: nothing is left to
/// review, so the task is cancelled. An accepted delivery whose PR was closed settles instead.
pub(crate) fn discarded(domain: &DomainState, t: &Task) -> bool {
    let Some(p) = t.final_presentation.as_ref().and_then(|id| domain.interactions.get(id)) else { return false };
    let InteractionPayload::Delivery { targets, .. } = &p.payload else { return false };
    !domain.accepted(p)
        && targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { .. }))
        && t.opened_prs.is_empty()
        && domain.task_prs(t).all(|pr| pr.state == PrState::Closed)
}
/// An invalid acceptance the user did not withdraw: no feedback on that
/// delivery and no document rejection on its Task after it was given.
pub(crate) fn superseded(domain: &DomainState, p: &Interaction) -> Option<SupersededAcceptance> {
    let (
        Scope::Task { task_id },
        InteractionPayload::Delivery {
            acceptance: Some(a),
            feedback,
            ..
        },
    ) = (&p.scope, &p.payload)
    else {
        return None;
    };
    let withdrawn = feedback.iter().any(|f| f.at_revision > a.at_revision)
        || domain.document_reviews.iter().any(|r| {
            &r.task_id == task_id
                && r.decisions
                    .iter()
                    .any(|d| !d.accepted && d.at_revision > a.at_revision)
        });
    (!a.valid && !withdrawn).then(|| SupersededAcceptance {
        task_id: task_id.clone(),
        presentation_id: p.id.clone(),
        user_id: a.user_id.clone(),
        accepted_at: a.at_revision,
    })
}
/// D04: a package is accepted only when every piece carries its own current
/// decision. A merge authorization covers its PR and the presentation text;
/// a document approval covers only that revision; feedback dates both out.
pub(crate) fn covered(domain: &DomainState, presentation: &InteractionId) -> bool {
    let Some(p) = domain.interactions.get(presentation) else {
        return false;
    };
    let (Scope::Task { task_id }, InteractionPayload::Delivery { targets, feedback, .. }) =
        (&p.scope, &p.payload)
    else {
        return false;
    };
    let after = feedback.iter().map(|f| f.at_revision).max().unwrap_or(0);
    let mut text_covered = !targets
        .iter()
        .any(|t| matches!(t, DeliveryTarget::Text { .. }));
    for target in targets {
        match target {
            DeliveryTarget::Text { .. } => {}
            DeliveryTarget::DocumentRevision {
                document_id,
                revision_id,
            } => {
                let decided = domain
                    .document_reviews
                    .iter()
                    .find(|r| &r.task_id == task_id && &r.document_id == document_id)
                    .and_then(|r| r.decisions.last())
                    .is_some_and(|d| {
                        d.accepted
                            && &d.revision_id == revision_id
                            && d.presentation_id.as_ref() == Some(presentation)
                            && d.at_revision > after
                    });
                if !decided {
                    return false;
                }
            }
            DeliveryTarget::PullRequest { pr_id, head } => {
                let authorized = domain.merge_authorizations.values().any(|a| {
                    a.valid
                        && &a.presentation_id == presentation
                        && &a.pr_id == pr_id
                        && &a.head == head
                });
                if !authorized {
                    return false;
                }
                text_covered = true;
            }
        }
    }
    text_covered
}
pub(crate) fn settle(domain: &mut DomainState, presentation: &InteractionId, u: &str) {
    if covered(domain, presentation) {
        record_acceptance(domain, presentation, u);
    }
}
/// The user chose to close the node on the card about its work after the merge: that choice is
/// the fact that ends the work, in the same commit as the answer.
pub(crate) fn settle_post_merge_problem(domain: &mut DomainState, card: &InteractionId, option: &str, lifecycles: &mut Lifecycles) -> Result<()> {
    let Some(i) = domain.interactions.get(card).filter(|i| i.system_card == Some(SystemCard::PostMergeProblem) && i.author == Author::System) else { return Ok(()) };
    let Scope::Task { task_id } = i.scope.clone() else { return Ok(()) };
    if option != POST_MERGE_CLOSE || domain.tasks.get(&task_id).is_none_or(Task::over) {
        return Ok(());
    }
    domain.require_checklist_proof(&domain.tasks[&task_id])?;
    domain.end_post_merge(&task_id, PostMergeEnd::UserDecision, lifecycles)
}
/// What the user chose on a start problem card that the domain does itself: cancel the task, or
/// let it start without the cancelled task it waited for. "Try again" is the daemon's to do.
pub(crate) fn settle_start_problem(domain: &mut DomainState, actor: &Actor, card: &InteractionId, option: &str, effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    let Some(i) = domain.interactions.get(card).filter(|i| i.system_card == Some(SystemCard::StartProblem) && i.author == Author::System) else { return Ok(()) };
    let Scope::Task { task_id } = i.scope.clone() else { return Ok(()) };
    let Ok(problem) = serde_json::from_str::<StartProblem>(&i.body) else { return Ok(()) };
    if domain.tasks.get(&task_id).is_none_or(Task::over) {
        return Ok(());
    }
    match (option, problem) {
        (START_CANCEL, _) => domain.close_task(actor, &task_id, None, effects, created, lifecycles)?,
        (START_RETRY, StartProblem::StageStartRefused { .. }) => retry_stage_handover(domain, &task_id),
        (START_WITHOUT, StartProblem::DependencyCancelled { dependency }) => {
            domain.invalidate(&task_id);
            let t = domain.tasks.get_mut(&task_id).unwrap();
            t.dependencies.remove(&dependency);
            t.delivered_dependencies.remove(&dependency);
            if t.stack.as_ref().is_some_and(|stack| stack.upstream_task == dependency) {
                t.stack = None;
            }
        }
        _ => {}
    }
    Ok(())
}
/// A request whose tasks the user all cancelled, the last before any node ran it, has nothing
/// for the orchestrator to sum up or follow: it closes as withdrawn.
pub(crate) fn withdraw_cancelled_requests(domain: &mut DomainState, before: &DomainState, actor: &Actor) {
    if !matches!(actor, Actor::User { .. }) {
        return;
    }
    let requests: BTreeSet<RequestId> = before.tasks.values()
        .filter(|t| !t.cancelled() && !t.started && t.agent_id.is_none() && domain.tasks.get(&t.id).is_some_and(Task::cancelled))
        .map(|t| t.request_id.clone())
        .collect();
    for request_id in requests {
        if !domain.tasks.values().filter(|t| t.request_id == request_id).all(Task::cancelled) {
            continue;
        }
        if let Some(request) = domain.requests.get_mut(&request_id).filter(|r| !r.closed) {
            request.closed = true;
            request.withdrawn = true;
        }
    }
}
/// A question the task left unanswered when it ended: the user's, or the one its node asked the
/// orchestrator, or a suggestion it sent, which nobody can answer any more.
pub(crate) fn left_open(domain: &DomainState, i: &Interaction, t: &TaskId) -> bool {
    domain.unanswered_card(i, t)
        || (matches!(&i.scope, Scope::Task { task_id } if task_id == t)
            && (DomainState::asks_the_orchestrator(i) || suggests(i))
            && !i.withdrawn
            && matches!(i.payload, InteractionPayload::Question { answer: None, .. }))
}
/// A node that asks the user something after its final delivery takes that delivery back: it
/// leaves the user's queue, the task is in progress again, and the node presents again once it
/// has the answers. A delivery the user accepted or a PR already merged stays.
pub(crate) fn withdraw_deliveries_asked_after(domain: &mut DomainState, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
    let cards = TaskCards::new(domain);
    let asked: Vec<(TaskId, InteractionId)> = domain.tasks.values()
        .filter(|t| !t.over() && cards.asked_aside(&t.id))
        .filter_map(|t| {
            let p = domain.interactions.get(t.final_presentation.as_ref()?)?;
            (!domain.accepted(p) && !domain.settled(t, p)).then(|| (t.id.clone(), p.id.clone()))
        })
        .collect();
    for (task_id, delivery) in asked {
        domain.invalidate(&task_id);
        domain.retire(&delivery);
        let told = domain.hand_back(&task_id, &delivery, delivery_withdrawn_for_questions(), lifecycles)?;
        let t = domain.tasks.get_mut(&task_id).unwrap();
        if let Some(agent_id) = t.agent_id.clone().filter(|_| !told) {
            effects.push(DomainEffect::NotifyAgent {
                agent_id, task_id: Some(task_id), interaction_id: None, message: delivery_withdrawn_for_questions(),
            });
        }
    }
    Ok(())
}
/// A final delivery whose pull request got new commits, or whose document got a new revision,
/// could no longer be accepted as presented: it leaves the user's queue and the task is in
/// progress again until its node presents what is current. The revision may come from the
/// node of another task sharing the document, so the node is told. A merged PR settles the
/// delivery instead. A node working on conflicts or changes Alondra handed back already knows
/// the delivery is stale and is on its way to present the new head: the push that moves the
/// head retires the delivery without a second notice, which would only make it present twice.
pub(crate) fn withdraw_deliveries_behind_their_targets(domain: &mut DomainState, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
    let behind: Vec<(TaskId, InteractionId, String, bool)> = domain.tasks.values()
        .filter(|t| !t.over())
        .filter_map(|t| {
            let p = domain.interactions.get(t.final_presentation.as_ref()?)?;
            let sent_back = matches!(domain.handed_back(p), Some(Instruction::Conflicts | Instruction::Changes));
            let InteractionPayload::Delivery { targets, .. } = &p.payload else { return None };
            if p.withdrawn || domain.settled(t, p) {
                return None;
            }
            targets.iter().find_map(|target| match target {
                DeliveryTarget::PullRequest { pr_id, head } => domain.pull_requests.get(pr_id)
                    .filter(|pr| pr.state != PrState::Closed && pr.head != *head)
                    .map(|pr| delivery_withdrawn_behind_pr(pr.number)),
                DeliveryTarget::DocumentRevision { document_id, revision_id } => domain.documents.get(document_id)
                    .filter(|d| d.current_revision != *revision_id)
                    .map(|d| delivery_withdrawn_behind_document(&d.title)),
                DeliveryTarget::Text { .. } => None,
            }).map(|notice| (t.id.clone(), p.id.clone(), notice, sent_back))
        })
        .collect();
    for (task_id, delivery, notice, sent_back) in behind {
        let moved = domain.tasks.get(&task_id).and_then(|task| domain.open_prs(task).next()).map(|pr| pr.head.clone());
        let notice = moved.and_then(|head| validated_head_moved(domain, &task_id, &head)).unwrap_or(notice);
        domain.invalidate(&task_id);
        domain.retire(&delivery);
        let working = matches!(domain.lifecycle(lifecycles, &task_id)?.activity(), alondra_domain::ActivityView::Working);
        let quiet = sent_back && working;
        let unheard = quiet.then(|| domain.next_input(&task_id));
        let told = domain.hand_back(&task_id, &delivery, notice.clone(), lifecycles)?;
        if let Some(unheard) = unheard {
            domain.consume_replacement_notice(&task_id, unheard, lifecycles)?;
        }
        let t = domain.tasks.get_mut(&task_id).unwrap();
        if let Some(agent_id) = t.agent_id.clone().filter(|_| !told && !quiet) {
            effects.push(DomainEffect::NotifyAgent { agent_id, task_id: Some(task_id), interaction_id: None, message: notice });
        }
    }
    Ok(())
}
/// The request building the plan this delivery proposed, once the user approved it.
pub(crate) fn approved_plan_build(domain: &DomainState, presentation_id: &InteractionId) -> Option<RequestId> {
    let build = guards::implementation_request(presentation_id);
    domain.request_plans.get(&build).filter(|approved| approved.presentation_id == *presentation_id).map(|_| build)
}
/// An approved plan is read-only: its build is already under way, so the user's word on it never
/// sends its node back to rewrite it. Changes become a new request; a reply or a question reaches
/// the orchestrator as a message about the build, which goes on.
#[allow(clippy::too_many_arguments)]
pub(crate) fn feedback_on_approved_plan(domain: &mut DomainState, actor: &Actor, presentation_id: &InteractionId, build: RequestId, body: &str,
    instruction: Instruction, effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    let plan = &domain.interactions[presentation_id];
    let reference = CardReference {
        interaction_id: presentation_id.clone(), title: plan.card.effective_title().to_owned(), scope: plan.scope.clone(),
        task_ids: domain.tasks.values().filter(|t| t.request_id == build).map(|t| t.id.clone()).collect(),
        request_id: Some(build.clone()),
    };
    let request_id = RequestId(format!("{}-feedback-{}", presentation_id.0, domain.revision + 1));
    let auto_merge = domain.requests.get(&build).is_some_and(|r| r.auto_merge);
    apply(domain, actor, &Command::CreateRequest {
        request_id: request_id.clone(), body: body.to_owned(), auto_merge, maintenance: false,
        message: instruction != Instruction::Changes,
    }, effects, created, lifecycles)?;
    domain.requests.get_mut(&request_id).unwrap().reply_to = Some(reference);
    Ok(())
}
/// A card that replaces the current final delivery withdraws it as `Withdraw` does: the task is
/// in progress again, never left delivered with nothing for the user to act on.
pub(crate) fn hand_back_replaced_deliveries(domain: &mut DomainState, lifecycles: &mut Lifecycles) -> Result<()> {
    let replaced: Vec<(TaskId, InteractionId)> = domain.tasks.values()
        .filter(|t| !t.over())
        .filter_map(|t| {
            let p = domain.interactions.get(t.final_presentation.as_ref()?)?;
            p.withdrawn.then(|| (t.id.clone(), p.id.clone()))
        })
        .collect();
    for (task_id, delivery) in replaced {
        domain.invalidate(&task_id);
        domain.hand_back(&task_id, &delivery, delivery_withdrawn(), lifecycles)?;
    }
    Ok(())
}
/// The cards of a task that ended leave the user's queue. A cancelled task takes them away in
/// silence: the user stopped the work, so nothing it asked matters any more. A task that ended
/// with its work done tells the user once about a question it left behind, unless it only
/// asked to approve a plan or document for work that is already done. What the node of a
/// cancelled task asks while it still winds down is about that ending, such as work it cannot
/// leave as it is: it stays with the user until the node is gone.
/// What the user allowed in a task lasts only while it is open.
pub(crate) fn forget_what_ended_tasks_allowed(domain: &mut DomainState) {
    for task in domain.tasks.values_mut().filter(|t| t.over()) {
        task.allowed.clear();
    }
}
pub(crate) fn retire_cards_of_ended_tasks(domain: &mut DomainState, effects: &mut Vec<DomainEffect>) -> Result<()> {
    let mut left: Vec<(TaskId, InteractionId)> = domain.interactions.values()
        .filter_map(|i| match &i.scope { Scope::Task { task_id } => Some((task_id, i)), Scope::Orchestrator { .. } => None })
        .filter(|(task_id, i)| domain.tasks.get(*task_id).is_some_and(|t| t.state.terminal() && !DomainState::winding_down(t)) && left_open(domain, i, task_id))
        .map(|(task_id, i)| (task_id.clone(), i.id.clone()))
        .collect();
    left.sort();
    let stale_notices: Vec<InteractionId> = domain.interactions.values()
        .filter(|i| i.author == Author::System && !i.withdrawn && matches!(i.system_card, Some(SystemCard::EndedUnanswered { .. })))
        .filter(|i| matches!(&i.scope, Scope::Task { task_id } if domain.tasks.get(task_id).is_some_and(|t| t.state == TaskState::Cancelled)))
        .map(|i| i.id.clone())
        .collect();
    for notice in stale_notices {
        domain.retire(&notice);
    }
    for (task_id, question) in left {
        domain.retire(&question);
        let q = &domain.interactions[&question];
        if domain.tasks[&task_id].state == TaskState::Cancelled || q.card.kind == CardKind::Review || q.recipient != Recipient::User {
            continue;
        }
        let id = InteractionId(format!("{ENDED_UNANSWERED_PREFIX}{}", question.0));
        if domain.interactions.contains_key(&id) {
            continue;
        }
        let need = clip(&format!("No longer waits for your answer: {}", q.card.effective_title()), CardSpec::NEED_MAX);
        let why = clip("The work was finished before you answered", CardSpec::WHY_MAX);
        let body = q.card.content.as_ref().map_or_else(|| q.body.clone(), |c| c.body.clone());
        let card = CardSpec::new(CardKind::Know, need).why(&why).content(ContentKind::Report, &body);
        let interaction = Interaction {
            attachments: vec![],
            id: id.clone(),
            scope: Scope::Task { task_id: task_id.clone() },
            author: Author::System,
            recipient: Recipient::User,
            body,
            blocking: false,
            subject: None,
            payload: InteractionPayload::Information { read_by: BTreeSet::new() },
            revision: domain.revision + 1,
            order: domain.revision + 1,
            withdrawn: false,
            card,
            clarification: None, snoozed_until: None, resurfaced_at_revision: None,
            system_card: Some(SystemCard::EndedUnanswered { question: question.clone() }), shortened: false,
        };
        domain.insert_system_card(interaction)?;
        effects.push(DomainEffect::NotifyUser { task_id: Some(task_id), interaction_id: id });
    }
    Ok(())
}
/// Asks the node of every task whose delivery just settled to clean up, and forgets the
/// request of one whose delivery is no longer settled.
pub(crate) fn wrap_up_step(domain: &mut DomainState, effects: &mut Vec<DomainEffect>) {
    let revision = domain.revision + 1;
    let cards = TaskCards::new(domain);
    let ids: Vec<TaskId> = domain.tasks.values()
        .filter(|t| !t.over() && !cards.blockers(&t.id) && !cards.unanswered(&t.id))
        .map(|t| t.id.clone())
        .collect();
    for id in ids {
        let t = &domain.tasks[&id];
        let finished = domain.finished(t);
        if finished && domain.wraps_up(t) && t.wrap_up.asked_at.is_none() {
            let agent_id = t.agent_id.clone().expect("a task that wraps up has a node");
            let message = match &t.wrap_up.post_merge_work {
                Some(work) => post_merge_reminder(&t.title, &t.id, &work.reason),
                None => match domain.require_checklist_proof(t) {
                    Err(error) => error.message,
                    Ok(()) => cleanup_request(&t.title, &t.id, &t.open_worktrees().map(|(path, _)| path.as_str()).collect::<Vec<_>>()),
                },
            };
            let wrap_up = &mut domain.tasks.get_mut(&id).unwrap().wrap_up;
            wrap_up.asked_at = Some(revision);
            wrap_up.cleanup_turn = None;
            effects.push(DomainEffect::NotifyAgent { agent_id, task_id: Some(id), interaction_id: None, message });
        } else if !finished && (t.wrap_up.asked_at.is_some() || t.wrap_up.cleaned_at.is_some()) {
            let wrap_up = &mut domain.tasks.get_mut(&id).unwrap().wrap_up;
            wrap_up.asked_at = None;
            wrap_up.cleaned_at = None;
            wrap_up.cleanup_turn = None;
        }
    }
}
/// A reviewer whose blocked tasks are all over (cancelled, merged outside, finished) has
/// nothing left to review: it is cancelled, its node told, and its delivery never reaches the
/// user.
pub(crate) fn cancel_orphan_reviewers(domain: &mut DomainState, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<bool> {
    let orphans: Vec<TaskId> = domain
        .tasks
        .values()
        .filter(|b| !b.over() && !b.blocks.is_empty() && !domain.finished(b))
        .filter(|b| b.pending_user_inputs.is_empty())
        .filter(|b| !b.blocks.iter().any(|a| domain.tasks.get(a).is_some_and(|a| !a.over() && !domain.finished(a))))
        .map(|b| b.id.clone())
        .collect();
    for id in &orphans {
        domain.cancel(id, effects, lifecycles)?;
    }
    Ok(!orphans.is_empty())
}
/// Tasks whose revision must move: their row, their decisions and
/// deliveries, PR, authorizations, stack readiness, reviews or linked
/// document versions differ in `next`. Information neither blocks nor dates
/// a decision (R07), so it never moves a Task's revision.
pub(crate) fn touched_tasks(domain: &DomainState, next: &DomainState) -> BTreeSet<TaskId> {
    let mut touched = BTreeSet::new();
    for (id, task) in &next.tasks {
        if domain.tasks.get(id) != Some(task) {
            touched.insert(id.clone());
        }
    }
    for (id, interaction) in &next.interactions {
        if matches!(interaction.payload, InteractionPayload::Information { .. }) {
            continue;
        }
        if let Scope::Task { task_id } = &interaction.scope
            && domain.interactions.get(id) != Some(interaction)
        {
            touched.insert(task_id.clone());
        }
    }
    for (id, pr) in &next.pull_requests {
        if domain.pull_requests.get(id) != Some(pr) {
            touched.insert(pr.task_id.clone());
        }
    }
    for (id, authorization) in &next.merge_authorizations {
        if domain.merge_authorizations.get(id) != Some(authorization) {
            touched.insert(authorization.task_id.clone());
        }
    }
    for (id, readiness) in &next.stack_readiness {
        if domain.stack_readiness.get(id) != Some(readiness) {
            touched.insert(id.clone());
        }
    }
    for review in &next.document_reviews {
        if !domain.document_reviews.contains(review) {
            touched.insert(review.task_id.clone());
        }
    }
    touched.extend(domain.lifecycle_journal.grown(&next.lifecycle_journal).map(|(id, _)| id.clone()));
    for (id, review) in &next.plan_reviews {
        let before = domain.plan_reviews.get(id);
        touched.extend(review.dependents.iter()
            .filter(|(task, settled)| before.and_then(|b| b.dependents.get(*task)) != Some(*settled))
            .map(|(task, _)| task.clone()));
    }
    for (id, document) in &next.documents {
        if domain.documents.get(id) != Some(document) {
            touched.extend(
                next.tasks
                    .values()
                    .filter(|task| task.documents.contains_key(id))
                    .map(|task| task.id.clone()),
            );
        }
    }
    touched
}
pub(crate) fn schedule_unchanged(domain: &DomainState, id: &ScheduleId, expected: Option<i64>) -> Result<()> {
    let Some(expected) = expected else {
        return Ok(());
    };
    if domain
        .schedule
        .schedules
        .get(id)
        .is_some_and(|s| s.revision != expected)
    {
        let mut e = DomainError::new("stale_revision", "Schedule changed; refresh before applying");
        e.current_revision = Some(domain.revision);
        return Err(e);
    }
    Ok(())
}
/// Clone, validate, swap: even an error discovered after preparing several changes is atomic.
/// The current state was validated when it was loaded or committed; only the result is checked.
/// Semantic receipts belong to the SQL transaction and are looked up before this method.
pub(crate) fn execute(domain: &mut DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome> {
    let (next, outcome) = transition(domain, actor, envelope)?;
    *domain = next;
    Ok(outcome)
}
/// The state `envelope` leaves and its outcome, decided on a single copy of this one, which
/// stays as it was.
pub(crate) fn transition(domain: &DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<(DomainState, CommandOutcome)> {
    let (next, outcome) = prepare_transition(domain, actor, envelope)?;
    crate::invariants::assert_hold(domain, actor, envelope, &next);
    #[cfg(test)]
    crate::invariants::check_every_button();
    #[cfg(test)]
    crate::invariants::assert_declared(domain, &next, &envelope.command);
    Ok((next, outcome))
}
pub(crate) fn prepare_transition(domain: &DomainState, actor: &Actor, envelope: &CommandEnvelope) -> Result<(DomainState, CommandOutcome)> {
    domain.admit(actor, envelope)?;
    let mut next = domain.clone();
    let mut effects = vec![];
    let mut created = vec![];
    let mut lifecycles = Lifecycles::new();
    count_progress(&mut next, actor, &envelope.command);
    apply(&mut next, actor, &envelope.command, &mut effects, &mut created, &mut lifecycles)?;
    next.end_lapsed_awaits(&mut effects, &mut lifecycles)?;
    withdraw_released_stalls(&mut next, &mut lifecycles)?;
    hand_back_replaced_deliveries(&mut next, &mut lifecycles)?;
    withdraw_deliveries_asked_after(&mut next, &mut effects, &mut lifecycles)?;
    withdraw_deliveries_behind_their_targets(&mut next, &mut effects, &mut lifecycles)?;
    if let Command::SubmitFeedback { presentation_id, body, instruction, .. } = &envelope.command
        && approved_plan_build(&next, presentation_id).is_none()
        && let Scope::Task { task_id } = next.interactions[presentation_id].scope.clone()
        && next.task(&task_id)?.agent_id.is_none()
        && next.lifecycle(&mut lifecycles, &task_id)?.execution().is_none()
    {
        let input = next.next_input(&task_id);
        if !next.lifecycle(&mut lifecycles, &task_id)?.pending_inputs().iter().any(|pending| pending.as_str() == input) {
            next.decide(&mut lifecycles, &task_id, LifecycleRecord::Told {
                input, text: Some(instruction.message(body)), answers: None,
            })?;
        }
    }
    next.adopt_declared_post_merge(&mut lifecycles)?;
    next.finish_settled_deliveries(&mut lifecycles)?;
    withdraw_cancelled_requests(&mut next, domain, actor);
    next.record_start_waits(&mut lifecycles)?;
    next.derive_states();
    if cancel_orphan_reviewers(&mut next, &mut effects, &mut lifecycles)? {
        next.derive_states();
    }
    retire_cards_of_ended_tasks(&mut next, &mut effects)?;
    reconcile_snoozed_cards(&mut next, domain, &mut effects);
    forget_what_ended_tasks_allowed(&mut next);
    reopen_requests_with_open_tasks(&mut next);
    close_finished_messages(&mut next);
    close_delivered_plans(&mut next, domain, &mut created, &mut effects)?;
    let ended_before = open_requests_with_every_task_over(domain);
    for request_id in open_requests_with_every_task_over(&next).into_iter().filter(|id| !ended_before.contains(id)) {
        let mut tasks = next.tasks.values().filter(|t| t.request_id == request_id);
        let last = tasks.clone().find(|t| !domain.tasks.get(&t.id).is_some_and(Task::over)).or_else(|| tasks.next());
        if let Some(task) = last {
            effects.push(DomainEffect::RequestTasksOver { request_id, task_id: task.id.clone() });
        }
    }
    let acting_before = domain.acting();
    for (task_id, card) in next.acting() {
        if acting_before.get(&task_id).map(|before| &before.interaction) != Some(&card.interaction) {
            effects.push(DomainEffect::NotifyUser { task_id: Some(task_id), interaction_id: card.interaction });
        }
    }
    resume_waiting_nodes(&mut next, domain, &mut effects);
    let waited: BTreeSet<TaskId> = next.tasks.values().flat_map(|t| t.waiting_for_tasks.iter()
        .filter(move |id| !t.ended_task_waits.contains_key(*id)).cloned()).collect();
    let completed: BTreeMap<TaskId, Landed> = waited.iter().filter_map(|id| next.tasks.get(id))
        .filter(|t| DomainState::task_wait_finished(t)).map(|t| (t.id.clone(), task_wait_result(&next, t))).collect();
    for task in next.tasks.values_mut() {
        for id in &task.waiting_for_tasks {
            if let Some(result) = completed.get(id) {
                task.ended_task_waits.entry(id.clone()).or_insert_with(|| result.clone());
            }
        }
    }
    wrap_up_step(&mut next, &mut effects);
    // Compacting an ended task changes nothing a screen shows of it, so the buttons it already
    // shows, which carry its revision, stay valid.
    let touched = if matches!(envelope.command, Command::CompactEndedTasks { .. }) { BTreeSet::new() } else { touched_tasks(domain, &next) };
    for id in &touched {
        let task = next.tasks.get_mut(id).unwrap();
        if task.started_without_node() && !domain.tasks.get(id).is_some_and(Task::started_without_node) {
            return fail("conflict", &format!("Task {} would be in progress with no node", id.0));
        }
        task.revision = domain.revision + 1;
    }
    let changed = next != *domain;
    let outside = effect_outside_the_domain(&envelope.command);
    if !changed && outside.is_none() && answers_with_its_effect(actor) {
        return Err(no_effect(domain, &envelope.command));
    }
    let effect = if changed {
        next.revision += 1;
        describe_effect(domain, &next, &touched)
    } else {
        effects.clear();
        outside.map(|line| vec![line.to_owned()]).unwrap_or_default()
    };
    next.validate()?;
    next.validate_lifecycles(touched.iter())?;
    let superseded_acceptances = domain
        .interactions
        .values()
        .filter(|p| domain.accepted(p))
        .filter_map(|p| superseded(&next, &next.interactions[&p.id]))
        .collect();
    let outcome = CommandOutcome {
        revision: next.revision,
        changed,
        effects,
        created_ids: created,
        superseded_acceptances,
        effect,
    };
    Ok((next, outcome))
}
pub(crate) fn apply(
    domain: &mut DomainState,
    actor: &Actor,
    c: &Command,
    effects: &mut Vec<DomainEffect>,
    created: &mut Vec<String>,
    lifecycles: &mut Lifecycles,
) -> Result<()> {
    domain.refuse_untracked_prs(c)?;
    match c {
        Command::ObservePrAddress { pr_id, read, read_started_at, verdict } => {
            system(actor)?;
            observe_pr_address(domain, pr_id, read, *read_started_at, verdict, effects)?;
        }
        Command::DetachPr { pr_id, at } => {
            detach_pr(domain, actor, pr_id, *at, effects, lifecycles)?;
        }
        Command::SnoozeCard { interaction_id, until, now } => {
            domain.snooze_guard(actor, interaction_id, *until, *now, domain)?;
            let card = domain.interactions.get_mut(interaction_id).unwrap();
            card.snoozed_until = Some(*until);
            card.revision = domain.revision + 1;
        }
        Command::UnsnoozeCard { interaction_id } => {
            domain.unsnooze_guard(actor, interaction_id)?;
            resurface_card(domain, interaction_id, effects);
        }
        Command::ResurfaceDueCards { now } => {
            system(actor)?;
            let due: Vec<_> = domain.interactions.values()
                .filter(|i| i.snoozed_until.is_some_and(|until| until <= *now) && domain.open(i))
                .map(|i| i.id.clone()).collect();
            for id in due { resurface_card(domain, &id, effects); }
        }
        Command::CreateSchedule { schedule } => {
            system(actor)?;
            id(&schedule.id.0)?;
            schedule.validate()?;
            if schedule.revision != 1 || schedule.deleted_at.is_some() {
                return fail("invalid_input", "New Schedule needs revision one and no deletion");
            }
            if domain.schedule.schedules.contains_key(&schedule.id) {
                return fail("idempotency_conflict", "Schedule ID already exists");
            }
            domain.schedule
                .schedules
                .insert(schedule.id.clone(), schedule.clone());
            created.push(schedule.id.0.clone());
        }
        Command::UpdateSchedule { schedule } => {
            system(actor)?;
            id(&schedule.id.0)?;
            schedule.validate()?;
            let current = domain
                .schedule
                .schedules
                .get(&schedule.id)
                .ok_or_else(|| DomainError::new("not_found", "Schedule does not exist"))?;
            if current.deleted_at.is_some() || schedule.deleted_at.is_some() {
                return fail("not_found", "Schedule was deleted");
            }
            if domain.schedule.runs.values().any(|run| {
                run.schedule_id == schedule.id && run.state == ScheduleRunState::Launching
            }) {
                return fail("conflict", "Schedule run is being sent");
            }
            if current.revision.checked_add(1) != Some(schedule.revision) {
                return fail("stale_revision", "Schedule revision must advance by one");
            }
            domain.schedule
                .schedules
                .insert(schedule.id.clone(), schedule.clone());
        }
        Command::DeleteSchedule {
            schedule_id,
            deleted_at,
            expected_schedule_revision,
        } => {
            system(actor)?;
            if *deleted_at < 0 {
                return fail("invalid_input", "Invalid deletion time");
            }
            schedule_unchanged(domain, schedule_id, *expected_schedule_revision)?;
            let schedule = domain
                .schedule
                .schedules
                .get_mut(schedule_id)
                .ok_or_else(|| DomainError::new("not_found", "Schedule does not exist"))?;
            if schedule.deleted_at.is_some() {
                return fail("not_found", "Schedule was deleted");
            }
            if domain.schedule.runs.values().any(|run| {
                &run.schedule_id == schedule_id && run.state == ScheduleRunState::Launching
            }) {
                return fail("conflict", "Schedule run is being sent");
            }
            if schedule.revision >= MAX_REVISION as i64 {
                return fail("invalid_input", "Schedule revision limit reached");
            }
            schedule.revision += 1;
            schedule.enabled = false;
            schedule.next_run = None;
            schedule.deleted_at = Some(*deleted_at);
        }
        Command::ClaimScheduleRun {
            schedule_id,
            run_id,
            origin,
            now,
            next_after_now,
            expected_schedule_revision,
        } => {
            system(actor)?;
            id(&run_id.0)?;
            schedule_unchanged(domain, schedule_id, *expected_schedule_revision)?;
            domain.schedule.claim(
                schedule_id,
                run_id.clone(),
                *origin,
                *now,
                *next_after_now,
            )?;
            created.push(run_id.0.clone());
        }
        Command::RecordScheduleRunSent {
            run_id,
            request,
            task,
            launch,
            finished_at,
        } => {
            system(actor)?;
            id(&request.id.0)?;
            id(&task.id.0)?;
            nonempty(&request.body)?;
            nonempty(&task.title)?;
            let run = domain
                .schedule
                .runs
                .get(run_id)
                .ok_or_else(|| DomainError::new("not_found", "Schedule run not found"))?;
            let definition = run
                .definition_snapshot
                .as_ref()
                .ok_or_else(|| DomainError::new("invalid_input", "Run has no claimed definition"))?;
            launch.validate(definition)?;
            let project = definition.project.clone();
            if request.body != definition.text
                || task.title != definition.title
                || task.request_id != request.id
                || request.closed
                || task.agent_id.is_some()
                || task.state != TaskState::NotStarted
                || task.started
                || task.final_presentation.is_some()
                || task.pr_id.is_some()
                || !task.dependencies.is_empty()
                || task.stack.is_some()
                || !task.documents.is_empty()
                || task.archived
            {
                return fail("invalid_input", "Request and Task must match the claimed Schedule definition and initial Task state");
            }
            if domain.requests.contains_key(&request.id) || domain.tasks.contains_key(&task.id) {
                return fail("idempotency_conflict", "Scheduled Request or Task ID already exists");
            }
            domain.schedule.record_sent(
                run_id,
                request.id.clone(),
                task.id.clone(),
                launch.clone(),
                *finished_at,
            )?;
            domain.requests.insert(request.id.clone(), Request { origin: RequestOrigin::Schedule, ..request.clone() });
            domain.tasks.insert(task.id.clone(), Task { project, ..task.clone() });
            domain.plan_lifecycle(lifecycles, &task.id)?;
            created.push(request.id.0.clone());
            created.push(task.id.0.clone());
        }
        Command::ClaimScheduleLaunch { run_id, now } => {
            system(actor)?;
            domain.scheduled_launch_guard(run_id)?;
            domain.schedule.claim_launch(run_id, *now)?;
        }
        Command::RecordScheduleLaunchHanded { run_id, session_id, agent_id, finished_at } => {
            system(actor)?;
            if let Some(agent_id) = agent_id {
                if !domain.agents.contains_key(agent_id) {
                    return fail("not_found", "Agent missing");
                }
                let task_id = domain.schedule.runs.get(run_id)
                    .and_then(|run| run.task_id.clone())
                    .ok_or_else(|| DomainError::new("conflict", "Scheduled Run has no Task"))?;
                let task = domain.active(&task_id)?;
                if task.agent_id.is_some() {
                    return fail("conflict", "Scheduled Task is already assigned");
                }
                domain.schedule.record_launch_handed(run_id, session_id.clone(), Some(agent_id.clone()), *finished_at)?;
                domain.admit_node(&task_id, agent_id, lifecycles)?;
            } else {
                domain.schedule.record_launch_handed(run_id, session_id.clone(), None, *finished_at)?;
            }
        }
        Command::RaiseStartProblem { task_id, problem } => {
            system(actor)?;
            let task = domain.task(task_id)?;
            if task.over() {
                return fail("task_not_active", "A closed Task has nothing to start");
            }
            if domain.open_start_problem(task_id).is_some() {
                return Ok(());
            }
            let title = task.title.clone();
            let n = domain.interactions.keys().filter(|i| i.0.starts_with(&format!("{START_PROBLEM_PREFIX}{}-", task_id.0))).count() + 1;
            let problem_id = InteractionId(format!("{START_PROBLEM_PREFIX}{}-{n}", task_id.0));
            let dependency = match problem {
                StartProblem::DependencyCancelled { dependency } => domain.tasks.get(dependency).map(|t| t.title.as_str()),
                _ => None,
            };
            let card = CardSpec { system: Some(SystemCard::StartProblem), ..alondra_domain_model::start_problem::card(&title, problem, dependency, alondra_core::text::Lang::En) };
            let options = alondra_domain_model::start_problem::options(problem, alondra_core::text::Lang::En);
            let recipient = match problem {
                StartProblem::PlanRefused { .. } => Recipient::Orchestrator,
                _ => Recipient::User,
            };
            domain.insert_interaction(
                actor,
                &problem_id,
                &Scope::Task { task_id: task_id.clone() },
                &recipient,
                &serde_json::to_string(problem).map_err(|e| DomainError::new("invalid_input", e.to_string()))?,
                false,
                &[],
                InteractionPayload::Question { options, answer: None },
                &card,
                Source::Problem,
            )?;
            created.push(problem_id.0.clone());
            effects.push(match recipient {
                Recipient::Orchestrator => DomainEffect::NotifyOrchestrator { interaction_id: Some(problem_id), request_id: None },
                _ => DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: problem_id },
            });
        }
        Command::RecordScheduleRunFailed {
            run_id,
            reason,
            finished_at,
        } => {
            system(actor)?;
            domain.schedule
                .record_failed(run_id, reason.clone(), *finished_at)?;
            let open_task = domain.schedule.runs[run_id].task_id.as_ref().and_then(|id| domain.tasks.get(id))
                .filter(|task| !task.over() && !task.archived).cloned();
            if let Some(task) = open_task {
                effects.push(DomainEffect::ScheduleLaunchFailed {
                    task_id: task.id.clone(),
                    title: task.title.clone(),
                    reason: reason.trim().to_owned(),
                });
                let problem = InteractionId(format!("{LAUNCH_PROBLEM_PREFIX}{}", run_id.0));
                let mut card = CardSpec::new(CardKind::Problem, clip(&format!("Scheduled task \"{}\" could not start", task.title), CardSpec::NEED_MAX))
                    .why(clip(reason, CardSpec::WHY_MAX))
                    .content(ContentKind::Diagnosis, reason.trim());
                card.system = Some(SystemCard::LaunchProblem { run: run_id.0.clone() });
                domain.insert_interaction(
                    actor,
                    &problem,
                    &Scope::Task { task_id: task.id.clone() },
                    &Recipient::User,
                    reason.trim(),
                    false,
                    &[],
                    InteractionPayload::Question {
                        options: vec![
                            QuestionOption { id: LAUNCH_RETRY_OPTION.into(), label: "Try again".into(), description: Some("Launch this schedule once more".into()) },
                            QuestionOption { id: "seen".into(), label: "Skip it".into(), description: Some("Its task is cancelled; the schedule runs again at its next time".into()) },
                        ],
                        answer: None,
                    },
                    &card,
                    Source::Problem,
                )?;
                created.push(problem.0.clone());
                effects.push(DomainEffect::NotifyUser { task_id: Some(task.id), interaction_id: problem });
            }
        }
        Command::InterruptScheduleRunsOnRestart => {
            system(actor)?;
            domain.schedule.interrupt_on_restart();
        }
        Command::ReviewWaits => system(actor)?,
        Command::BeginAccountLogin { account, attempt, started_at } => begin_account_login(domain, actor, account, attempt, *started_at)?,
        Command::CancelAccountLogin { account, attempt } => cancel_account_login(domain, actor, account, attempt)?,
        Command::RecordAccountLogin { login } => record_account_login(domain, actor, login)?,
        Command::AnnounceRequestTasks => {
            domain.announce_guard(actor)?;
            announce_request_tasks(domain, false, created, effects)?;
        }
        Command::AcknowledgeRequestTasks => {
            domain.announce_guard(actor)?;
            announce_request_tasks(domain, true, created, effects)?;
        }
        Command::RecordAccounts { accounts, reserve } => {
            system(actor)?;
            let mut seen = BTreeSet::new();
            if let Some(twice) = accounts.iter().find(|a| !seen.insert((a.provider, a.id.clone()))) {
                return fail("invalid_input", &format!("Account {} is recorded twice", twice.id));
            }
            if *reserve == alondra_core::usage::Share::NONE {
                return fail("invalid_input", "An account's reserve is above 0");
            }
            record_accounts(domain, accounts);
            domain.account_reserve = *reserve;
            domain.account_usage.retain(|usage| seen.contains(&(usage.provider, usage.account.clone())));
        }
        Command::RecordAccountPolicyRefused { provider, account } => {
            system(actor)?;
            let Some(status) = domain.accounts.iter_mut().find(|status| status.provider == *provider && status.id == *account) else {
                return fail("not_found", "The refused account is unknown");
            };
            if matches!(status.state, alondra_core::accounts::AccountState::SignedIn | alondra_core::accounts::AccountState::Unchecked) {
                status.state = alondra_core::accounts::AccountState::PolicyRefused;
            }
        }
        Command::RecordDistribution { policy, move_when_exhausted } => {
            system(actor)?;
            domain.account_policy = *policy;
            domain.account_moves = *move_when_exhausted;
        }
        Command::ChangeAccounts { provider, change } => {
            domain.accounts_guard(actor, *provider, change)?;
            change_accounts(domain, *provider, change);
        }
        Command::SetAccountOrder { order, expected_config_version } => {
            domain.account_order_guard(actor, order, expected_config_version)?;
            domain.account_order.clone_from(order);
        }
        Command::ReplaceAccountConfiguration { accounts, expected_config_version } => {
            let accounts = alondra_core::options::OptionsFile::parse(accounts).map_err(|why| DomainError::new("invalid_input", why))?.accounts;
            replace_account_configuration(domain, actor, &accounts, expected_config_version)?;
        }
        Command::RecordAccountCredential { account, set } => record_account_credential(domain, actor, account, *set)?,
        Command::RecordAccountConfiguration { order, version } => {
            system(actor)?;
            domain.validate_account_order(order)?;
            nonempty(version)?;
            domain.account_order.clone_from(order);
            domain.account_config_version.clone_from(version);
        }
        Command::RecordModelCatalog { catalog } => {
            system(actor)?;
            nonempty(&catalog.revision)?;
            if !domain.accounts.iter().any(|account| account.provider == catalog.account.provider && account.id == catalog.account.account) {
                return fail("not_found", "The model catalog belongs to an unknown account");
            }
            if let Some(at) = domain.model_catalogs.iter().position(|known| known.account == catalog.account) {
                if domain.model_catalogs[at].measured_at > catalog.measured_at { return fail("stale_revision", "A newer model catalog is already recorded"); }
                domain.model_catalogs[at] = catalog.clone();
            } else { domain.model_catalogs.push(catalog.clone()); }
            domain.model_catalogs.sort_by(|left, right| left.account.cmp(&right.account));
        }
        Command::RequestTaskSelection { task_id, expected_selection, selection, pending } => {
            domain.task_selection_guard(actor, task_id, expected_selection, selection, pending)?;
            let task = domain.tasks.get_mut(task_id).expect("checked above");
            task.selection = Some(selection.clone());
            task.pending_selection = Some(pending.clone());
            task.move_refused = None;
        }
        Command::MoveTask { task_id, expected_selection, account } => {
            let (selection, pending) = domain.move_task_guard(actor, task_id, expected_selection, account)?;
            let task = domain.tasks.get_mut(task_id).expect("checked above");
            let rests = pending.is_none();
            task.selection = Some(selection);
            task.pending_selection = pending;
            task.move_refused = None;
            if rests {
                leave_hold_for_the_move(domain, task_id, account, lifecycles)?;
            }
        }
        Command::RecordTaskSelection { task_id, selection, pending } => {
            system(actor)?;
            domain.not_ending(task_id)?;
            domain.active(task_id)?;
            if let Some(original) = &domain.task(task_id)?.selection
                && (selection.requested != original.requested
                    || selection.requested_reasoning_effort != original.requested_reasoning_effort
                    || selection.account != original.account
                    || selection.paid_account_grant != original.paid_account_grant
                    || selection.allow_model_substitution != original.allow_model_substitution
                    || selection.substitution_was_edited != original.substitution_was_edited)
            {
                return fail("invalid_input", "An execution cannot rewrite the user's model, account, reasoning effort or substitution permission");
            }
            if let Some(effective) = &selection.effective {
                let account = domain.task(task_id)?.account_key();
                if effective.model.provider != account.provider { return fail("invalid_input", "The effective model does not match the admitted provider"); }
                let clearing_unchanged = pending.is_none() && domain.task(task_id)?.pending_selection.is_some()
                    && domain.task(task_id)?.selection.as_ref() == Some(selection);
                if !clearing_unchanged {
                    let catalog = domain.model_catalogs.iter().find(|catalog| catalog.account == account)
                        .ok_or_else(|| DomainError::new("model_catalog_unavailable", "Refresh the admitted account's model catalog before recording its effective model"))?;
                    catalog.validate(effective).map_err(|why| DomainError::new("model_catalog_changed", why))?;
                }
            }
            if let (Some(account), Some(effective)) = (&selection.account, &selection.effective)
                && account.provider != effective.model.provider
            {
                return fail("invalid_input", "The effective model belongs to another provider than the selected account");
            }
            if let Some(pending) = pending {
                nonempty(&pending.reason)?;
                if pending.account.provider != pending.model.model.provider {
                    return fail("invalid_input", "The pending model belongs to another provider");
                }
            }
            let task = domain.tasks.get_mut(task_id).expect("checked above");
            task.selection = Some(selection.clone());
            task.pending_selection = pending.clone();
        }
        Command::RecordModelProfiles { profiles, version } => {
            system(actor)?;
            nonempty(version)?;
            alondra_core::model_selection::validate_profiles(profiles).map_err(|why| DomainError::new("invalid_input", why))?;
            domain.model_profiles.clone_from(profiles);
            domain.model_profiles_version.clone_from(version);
        }
        Command::SetModelProfile { provider, profile, policy, expected_config_version } => {
            domain.model_profile_guard(actor, *provider, *profile, policy, expected_config_version)?;
            domain.model_profiles.entry(*provider).or_default().insert(*profile, policy.clone());
            domain.model_profiles_version = alondra_core::model_selection::profiles_version(&domain.model_profiles);
        }
        Command::RecordAccountUsage { provider, account, reading, at, day } => {
            system(actor)?;
            reading.check().map_err(|why| DomainError::new("invalid_input", why))?;
            if !alondra_core::usage::is_day(day) {
                return fail("invalid_input", "The day of a usage reading is YYYY-MM-DD");
            }
            let index = domain.account_usage.iter().position(|usage| usage.provider == *provider && usage.account == *account);
            let recorded = alondra_core::usage::record(index.map(|i| &domain.account_usage[i]), *provider, account, reading, *at, day);
            match index {
                Some(i) => domain.account_usage[i] = recorded,
                None => domain.account_usage.push(recorded),
            }
        }
        Command::CreateCardReply { request_id, interaction_id, body, auto_merge } => {
            let reader = user(actor)?;
            let Some(card) = domain.interactions.get(interaction_id)
                .filter(|card| !card.withdrawn && card.recipient == Recipient::User) else {
                return refuse("invalid_input", "interaction_id", "Choose an existing card addressed to the user");
            };
            let (source_request, task_ids) = match &card.scope {
                Scope::Task { task_id } => {
                    let task = domain.tasks.get(task_id).ok_or_else(|| DomainError::new("not_found", "Task not found"))?;
                    (Some(task.request_id.clone()), vec![task_id.clone()])
                }
                Scope::Orchestrator { request_id } => {
                    let tasks = domain.tasks.values().filter(|task| Some(&task.request_id) == request_id.as_ref())
                        .map(|task| task.id.clone()).collect();
                    (request_id.clone(), tasks)
                }
            };
            let reference = CardReference {
                interaction_id: card.id.clone(), title: card.card.effective_title().to_owned(),
                scope: card.scope.clone(), request_id: source_request, task_ids,
            };
            apply(domain, actor, &Command::CreateRequest {
                request_id: request_id.clone(), body: body.clone(), auto_merge: *auto_merge,
                maintenance: false, message: true,
            }, effects, created, lifecycles)?;
            domain.requests.get_mut(request_id).unwrap().reply_to = Some(reference);
            let revision = domain.revision + 1;
            let card = domain.interactions.get_mut(interaction_id).expect("the card was found above");
            if let InteractionPayload::Information { read_by } = &mut card.payload
                && read_by.is_empty()
            {
                read_by.insert(reader);
                card.revision = revision;
            }
        }
        Command::CreateRequest { request_id, body, auto_merge, maintenance, message } => {
            domain.manager(actor)?;
            id(&request_id.0)?;
            nonempty(body)?;
            if domain.requests.contains_key(request_id) {
                return fail("invalid_input", "Request already exists");
            };
            domain.requests.insert(
                request_id.clone(),
                Request {
                    auto_merge: *auto_merge, maintenance: *maintenance, message: *message || *maintenance,
                    origin: if matches!(actor, Actor::Orchestrator { .. }) { RequestOrigin::Orchestrator } else { RequestOrigin::User },
                    ..Request::new(request_id.clone(), body.clone())
                },
            );
            created.push(request_id.0.clone());
            // 8ccd131 handed every new request of the user to the orchestrator.
            if matches!(actor, Actor::User { .. }) {
                effects.push(DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request_id.clone()) });
            }
        }
        Command::CloseRequest { request_id, answer, withdrawn, title, headline, folded_into, covered_by, outcome } => {
            if !matches!(actor, Actor::System) {
                domain.manager(actor)?;
            }
            if let Some(task_id) = folded_into {
                check_fold(domain, request_id, task_id, *withdrawn)?;
            }
            if domain
                .tasks
                .values()
                .any(|t| &t.request_id == request_id && !t.over())
            {
                return fail("task_not_active", "Request still has unfinished Tasks");
            };
            check_close_outcome(domain, actor, request_id, *outcome, *withdrawn, folded_into.as_ref(), covered_by)?;
            check_cover(domain, covered_by, *withdrawn)?;
            check_close_answer(domain, actor, request_id, answer.as_deref(), *withdrawn)?;
            for (field, text) in [("answer", answer.as_deref()), ("title", title.as_deref())] {
                if text.is_some_and(|text| text.trim().is_empty()) {
                    return refuse("invalid_input", field, "Provide non-empty text, or omit this field");
                }
            }
            if let Some(headline) = headline
                && (headline.trim().is_empty() || headline.chars().count() > CardSpec::NEED_MAX || headline.contains(['\n', '\r']))
            {
                return refuse("invalid_input", "headline", "Provide a non-empty subject on one line, at most 90 characters; put the complete answer in answer");
            }
            close_request(domain, request_id, answer.as_ref(), *withdrawn, title.as_ref(), headline.as_deref(), created, effects)?;
            if let Some(request) = domain.requests.get_mut(request_id) {
                request.folded_into = folded_into.clone();
            }
        }
        Command::RegisterAgent { agent_id, name, at } => {
            system(actor)?;
            id(&agent_id.0)?;
            nonempty(name)?;
            if domain.agents.contains_key(agent_id) {
                return fail("invalid_input", "Agent instance already exists");
            };
            domain.agents.insert(
                agent_id.clone(),
                Agent {
                    id: agent_id.clone(),
                    name: name.clone(),
                    phase: NodePhase::ThroughSdk,
                    since: *at,
                },
            );
            created.push(agent_id.0.clone());
        }
        Command::MoveNode { agent_id, event } => {
            system(actor)?;
            move_node(domain, agent_id, event, lifecycles)?;
        }
        Command::RecordLastWords { agent_id, text } => {
            system(actor)?;
            if !domain.agents.contains_key(agent_id) {
                return fail("not_found", "Agent missing");
            }
            domain.last_words.insert(agent_id.clone(), clip(text.trim(), LAST_WORDS_MAX));
        }
        Command::AdvanceOrchestratorGeneration { generation } => {
            system(actor)?;
            if *generation <= domain.orchestrator_generation || *generation > MAX_REVISION {
                return fail("stale_generation", "Generation must advance monotonically");
            };
            domain.orchestrator_generation = *generation;
        }
        Command::BindOrchestratorSession { session_id } => {
            system(actor)?;
            if session_id.0.trim().is_empty() {
                return fail("invalid_input", "The orchestrator's session id is empty");
            }
            domain.orchestrator_session = Some(session_id.clone());
            domain.orchestrator_batch_base = Some(domain.revision + 1);
        }
        Command::CreateTask {
            task_id,
            request_id,
            title,
            agent_id,
            review_prs,
            dependencies,
            stack,
            launch,
            plan_items,
            continues,
            priority,
        } => {
            domain.fence(actor)?;
            if !matches!(actor, Actor::Orchestrator { .. }) {
                return fail("forbidden", "Only the orchestrator creates Tasks: the user asks for work with a Request, which the orchestrator splits into Tasks");
            }
            id(&task_id.0)?;
            nonempty(title)?;
            match launch.as_ref().map(LaunchPlan::missing) {
                None => return refuse("launch_plan_required", "launch",
                    "every Task is created with its launch plan: pass launch with at least name and cwd (and, with auto effort, model and reason), so Alondra starts it by itself when there is room and what it depends on is done"),
                Some(Some(field)) => return refuse("launch_plan_required", field, "a launch plan needs a name and a folder to start the node in"),
                Some(None) => {}
            }
            if domain.tasks.contains_key(task_id) {
                return fail("invalid_input", "Task already exists");
            };
            no_cancelled_dependency(domain, dependencies)?;
            let Some(request) = domain.requests.get(request_id).filter(|r| !r.closed) else {
                return fail("not_found", "Open Request required");
            };
            if request.title.is_none() && domain.tasks.values().any(|task| task.request_id == *request_id) {
                return refuse(
                    "request_title_required",
                    "request.title",
                    "Name the Request with title_request (a few words, in the user's language, never the prompt) before adding a second Task",
                );
            }
            if continues.is_none() && plan_items.is_none() && !may_add_task(domain, request_id) {
                return refuse("request_has_tasks", "request_id",
                    "this Request already has Tasks from an earlier batch: fold what is new into the Task that covers it (send it to that Task's node), or create a new Request for separate work");
            }
            if let Some(original) = continues {
                let carried = domain.task(original)?;
                if carried.request_id != *request_id {
                    return refuse("continuation_request", "request_id", &format!(
                        "a task that carries on another goes in that task's request, so the request still counts its work: create it under request {}",
                        carried.request_id.0));
                }
                if plan_items.is_some() {
                    return refuse("invalid_input", "plan_items", "a task that carries on another takes that task's whole plan: leave plan_items out");
                }
            }
            if agent_id
                .as_ref()
                .is_some_and(|a| !domain.agents.contains_key(a))
            {
                return fail("not_found", "Agent missing");
            };
            if let Some(agent) = agent_id {
                free(domain, agent, task_id)?;
                if matches!(actor, Actor::Orchestrator { .. }) {
                    used_by_other(domain, agent, task_id)?;
                }
            }
            domain.tasks.insert(
                task_id.clone(),
                Task {
                    id: task_id.clone(),
                    request_id: request_id.clone(),
                    agent_id: agent_id.clone(),
                    title: title.clone(),
                    state: TaskState::NotStarted,
                    revision: domain.revision + 1,
                    created_revision: domain.revision + 1,
                    started: false,
                    review_prs: *review_prs,
                    final_presentation: None, pending_user_inputs: Default::default(),
                    pr_id: None,
                    later_prs: vec![],
                    opened_prs: BTreeSet::new(),
                    dependencies: dependencies.clone(),
                    stack: stack.clone(),
                    documents: BTreeMap::new(),
                    archived: false,
                    model: None,
                    waiting_for_tasks: Default::default(), ended_task_waits: Default::default(), delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: *priority, paused_in_queue: None, ending: None, stalls: 0, stall_closing: None, open_pr: None, released: None, stages: Default::default(), specs: Default::default(), validation: Default::default(), continues: continues.clone(), worktrees: Default::default(), provider: Default::default(), selection: None, pending_selection: None, move_refused: None, account: None, allowed: Default::default(), project: launch.as_ref().and_then(|launch| launch.project.clone()),
                },
            );
            domain.dependency_graph()?;
            if let Some(original) = continues {
                let inherited: BTreeMap<String, TaskWorktree> = domain.tasks[original].open_worktrees()
                    .map(|(path, worktree)| (path.clone(), TaskWorktree { origin: WorktreeOrigin::Inherited, branch: worktree.branch.clone(), discard: false, end: None }))
                    .collect();
                domain.tasks.get_mut(task_id).unwrap().worktrees = inherited;
                if let Some(checklist) = domain.checklists.remove(original) {
                    domain.checklists.insert(task_id.clone(), checklist);
                }
            } else if let Some(taken) = plan_items {
                take_plan_items(domain, task_id, request_id, taken)?;
            } else if let Some(approved) = domain.request_plans.get(request_id) {
                if domain.checklists.values().any(|c| c.plan == approved.presentation_id) {
                    return refuse("approved_plan_has_task", "plan_items",
                        "this request's approved plan already has a task: name with plan_items the points of that plan this task takes from the task that holds them, or create a new request for work the plan does not cover");
                }
                let plan = &domain.interactions[&approved.presentation_id];
                let checklist = Checklist { plan: approved.presentation_id.clone(), items: checklist_items(plan.card.plan.as_deref().unwrap_or_default()) };
                domain.checklists.insert(task_id.clone(), checklist);
            }
            domain.plan_lifecycle(lifecycles, task_id)?;
            if let Some(agent) = agent_id {
                domain.decide(lifecycles, task_id, LifecycleRecord::Admitted { node: agent.clone(), execution: None })?;
            }
            created.push(task_id.0.clone());
        }
        Command::AssignTask { task_id, agent_id, model } => {
            not_held_for_plan_review(domain, task_id)?;
            domain.not_ending(task_id)?;
            if matches!(actor, Actor::System) {
                let t = domain.task(task_id)?;
                if t.state != TaskState::NotStarted || t.started || t.agent_id.is_some() {
                    return fail("conflict", "Task already has a node");
                }
            } else {
                domain.manager(actor)?;
            }
            domain.active(task_id)?;
            if domain.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(task_id)
                && run.launch.as_ref().is_some_and(|launch| matches!(launch.state,
                    ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching))) {
                return fail("conflict", "Scheduled Task launch is pending or dispatching");
            }
            if !domain.agents.contains_key(agent_id) {
                return fail("not_found", "Agent missing");
            };
            if domain.task(task_id)?.agent_id.as_ref() != Some(agent_id) {
                if matches!(actor, Actor::Orchestrator { .. } | Actor::System) {
                    used_by_other(domain, agent_id, task_id)?;
                }
                free(domain, agent_id, task_id)?;
                domain.invalidate(task_id);
                domain.admit_node(task_id, agent_id, lifecycles)?;
            }
            if model.is_some() {
                domain.tasks.get_mut(task_id).unwrap().model = model.clone();
            }
        }
        Command::TitleRequest { request_id, title } => {
            domain.manager(actor)?;
            nonempty(title)?;
            if title.chars().count() > REQUEST_TITLE_LIMIT {
                return fail("invalid_input", "Request title is too long");
            }
            domain.requests
                .get_mut(request_id)
                .ok_or_else(|| DomainError::new("not_found", "Request missing"))?
                .title = Some(title.clone());
        }
        Command::RetitleTask { task_id, title } => {
            domain.manager(actor)?;
            nonempty(title)?;
            domain.task(task_id)?;
            domain.tasks.get_mut(task_id).unwrap().title = title.clone();
        }
        Command::WaitForTasks { task_id, tasks } => {
            let task = domain.assigned(actor, task_id)?;
            domain.not_ending(task_id)?;
            if let Some(presentation) = task.final_presentation.as_ref().and_then(|id| domain.interactions.get(id))
                && domain.handed_back(presentation).is_none() {
                return refuse("conflict", "task_id", "Your final result is still under review: withdraw it before declaring a task wait");
            }
            if tasks.is_empty() {
                return refuse("invalid_input", "tasks", "Name at least one task to wait for");
            }
            for other in tasks {
                id(&other.0)?;
                domain.task(other)?;
                if other == task_id {
                    return refuse("dependency_cycle", "tasks", "A task cannot wait for itself");
                }
            }
            let ended = tasks.iter().filter_map(|id| domain.tasks.get(id).filter(|task| DomainState::task_wait_finished(task))
                .map(|task| (id.clone(), task_wait_result(domain, task)))).collect();
            let task = domain.tasks.get_mut(task_id).unwrap();
            task.waiting_for_tasks = tasks.clone();
            task.ended_task_waits = ended;
            domain.dependency_graph()?;
        }
        Command::SetDependencies {
            task_id,
            dependencies,
            stack,
            ..
        } => {
            domain.manager(actor)?;
            domain.active(task_id)?;
            if domain.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(task_id)
                && run.launch.as_ref().is_some_and(|launch| matches!(launch.state,
                    ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching))) {
                return fail("conflict", "Scheduled Task launch is pending or dispatching");
            }
            no_cancelled_dependency(domain, dependencies)?;
            domain.invalidate(task_id);
            let t = domain.tasks.get_mut(task_id).unwrap();
            t.dependencies = dependencies.clone();
            t.stack = stack.clone();
            domain.dependency_graph()?;
        }
        Command::SetRelations { task_id, delivered_dependencies, blocks } => {
            domain.manager(actor)?;
            domain.active(task_id)?;
            for other in delivered_dependencies.iter().chain(blocks) {
                if domain.task(other)?.state == TaskState::Cancelled {
                    return refuse("invalid_input", "relations", "A cancelled task cannot be related: reopen it or leave it out");
                }
            }
            for blocked in blocks {
                let over = domain.task(blocked)?;
                if over.over() || domain.finished(over) {
                    return refuse(
                        "invalid_input",
                        "blocks",
                        &format!("Task {blocked} is already over, so there is nothing to block or review: leave it out"),
                    );
                }
            }
            if !blocks.is_empty() && domain.task(task_id)?.pr_id.is_some() {
                return refuse(
                    "invalid_input",
                    "blocks",
                    "A task with a registered pull request cannot block others: a reviewer delivers a verdict, not a pull request",
                );
            }
            let reviewed = blocks.iter().filter_map(|blocked| domain.tasks.get(blocked)).map(|blocked| blocked.priority).min();
            let t = domain.tasks.get_mut(task_id).unwrap();
            t.delivered_dependencies = delivered_dependencies.clone();
            t.blocks = blocks.clone();
            if let Some(priority) = reviewed {
                t.priority = priority;
            }
            domain.dependency_graph()?;
        }
        Command::SetPriority { task_id, priority } => {
            domain.manager(actor)?;
            domain.active(task_id)?;
            let reviewers: Vec<TaskId> = domain.tasks.values()
                .filter(|other| other.blocks.contains(task_id) && !other.over())
                .map(|other| other.id.clone()).collect();
            if std::iter::once(task_id).chain(&reviewers).all(|id| domain.tasks[id].priority == *priority) {
                return fail("unchanged", &format!("Task {} is already {} priority: nothing changed", task_id.0, format!("{priority:?}").to_lowercase()));
            }
            for id in std::iter::once(task_id).chain(&reviewers) {
                domain.tasks.get_mut(id).unwrap().priority = *priority;
            }
        }
        Command::PauseQueuedTask { task_id, at } => {
            if !matches!(actor, Actor::System) {
                domain.manager(actor)?;
            }
            domain.active(task_id)?;
            let t = domain.task(task_id)?;
            if t.agent_id.is_some() {
                return fail("conflict", "This task already has a node: pause its node instead");
            }
            if t.paused_in_queue.is_some() {
                return fail("unchanged", "This task is already paused");
            }
            domain.tasks.get_mut(task_id).unwrap().paused_in_queue = Some(*at);
        }
        Command::ResumeQueuedTask { task_id } => {
            if !matches!(actor, Actor::System) {
                domain.manager(actor)?;
            }
            if domain.task(task_id)?.paused_in_queue.is_none() {
                return fail("conflict", "This node or task is not paused");
            }
            domain.tasks.get_mut(task_id).unwrap().paused_in_queue = None;
        }
        Command::RecordVerdict { task_id, presentation_id, verdict } => {
            role_allows(domain, actor, task_id, RoleTool::RecordVerdict)?;
            let t = domain.assigned(actor, task_id)?;
            if t.final_presentation.as_ref() != Some(presentation_id) {
                return fail("acceptance_stale", "The verdict is for a delivery that is no longer the final one");
            }
            if !domain.reviewing(t) {
                return refuse("invalid_input", "verdict", "Only a task that blocks another open task gives a verdict");
            }
            if let Some(recorded) = &t.verdict {
                let same = match (recorded, verdict) {
                    (Verdict::Approved { .. }, Verdict::Approved { .. }) => true,
                    (Verdict::ChangesRequested { .. }, Verdict::ChangesRequested { .. }) => true,
                    (Verdict::Approved { open_issues, .. }, Verdict::ChangesRequested { .. }) => *open_issues,
                    _ => false,
                };
                if same {
                    return Ok(());
                }
                return refuse("invalid_input", "verdict", "A verdict is already recorded for this result: present your result again to change it");
            }
            let reviewed_at = domain.interaction(presentation_id)?.order;
            let blocked: Vec<TaskId> = t.blocks.iter().filter(|b| domain.tasks.get(*b).is_some_and(|b| !b.over())).cloned().collect();
            for blocked_id in &blocked {
                let delivered = domain.task(blocked_id)?.final_presentation.as_ref().and_then(|id| domain.interactions.get(id));
                if delivered.is_some_and(|delivery| delivery.order > reviewed_at) {
                    return refuse(
                        "acceptance_stale",
                        "verdict",
                        "The task you review delivered again after your result: review its new delivery, present your result again and then give the verdict",
                    );
                }
            }
            let capped = blocked.iter().any(|b| domain.tasks[b].review_rounds >= MAX_REVIEW_ROUNDS);
            let verdict = match verdict {
                Verdict::ChangesRequested { text, serious_findings } => {
                    nonempty(text)?;
                    let Some(serious_findings) = *serious_findings else {
                        return refuse(
                            "invalid_input",
                            "serious_findings",
                            "Say how many of the failures you found are serious (broken behaviour, lost data, or a security or session hole), 0 if none",
                        );
                    };
                    if capped {
                        Verdict::Approved { summary: text.clone(), open_issues: true, serious_findings }
                    } else {
                        verdict.clone()
                    }
                }
                Verdict::Approved { .. } => verdict.clone(),
            };
            match &verdict {
                Verdict::ChangesRequested { text, .. } => {
                    for blocked_id in &blocked {
                        let Some(delivery) = domain.task(blocked_id)?.final_presentation.clone() else {
                            return refuse("invalid_input", "verdict", "The blocked task has delivered nothing to send back");
                        };
                        let already_sent_back = domain.handed_back(&domain.interactions[&delivery]).is_some();
                        domain.invalidate(blocked_id);
                        let revision = domain.revision + 1;
                        let p = domain.interactions.get_mut(&delivery).unwrap();
                        if let InteractionPayload::Delivery { feedback, .. } = &mut p.payload {
                            feedback.push(Feedback {
                                target: FeedbackTarget::Delivery,
                                reply_window_start: None,
                                attachments: vec![],
                                user_id: Feedback::ALONDRA.to_owned(),
                                body: text.clone(),
                                at_revision: revision,
                                instruction: Instruction::Changes,
                                merge_failure: None,
                            });
                        }
                        p.revision = revision;
                        if !already_sent_back {
                            domain.tasks.get_mut(blocked_id).unwrap().review_rounds += 1;
                        }
                        notify(domain, blocked_id, Some(delivery), &Instruction::Changes.message(text), effects)?;
                    }
                    domain.tasks.get_mut(task_id).unwrap().verdict = Some(verdict);
                }
                Verdict::Approved { .. } => {
                    domain.tasks.get_mut(task_id).unwrap().verdict = Some(verdict);
                    record_acceptance(domain, presentation_id, Feedback::ALONDRA);
                }
            }
        }
        Command::StartTask { task_id } => {
            task_editor(domain, actor, task_id)?;
            domain.not_ending(task_id)?;
            let t = domain.task(task_id)?;
            if t.agent_id.is_none() {
                return fail("not_assigned", "Task requires an Agent");
            };
            not_held_for_plan_review(domain, task_id)?;
            let t = domain.task(task_id)?;
            ready_to_start(domain, t)?;
            domain.node_has_run(task_id, lifecycles)?;
        }
        Command::PublishInformation {
            attachments,
            interaction_id,
            scope,
            recipient,
            body,
            card,
        } => {
            if matches!(actor, Actor::Agent { .. }) && matches!(recipient, Recipient::User) {
                return refuse("forbidden", "card.kind", NODES_DO_NOT_NOTIFY);
            }
            if let Scope::Orchestrator { request_id: Some(request) } = scope
                && matches!(recipient, Recipient::User)
                && domain.requests.get(request).is_some_and(|r| r.maintenance)
            {
                return refuse("maintenance_request", "scope.request_id", MAINTENANCE_GETS_NO_CARD);
            }
            if matches!(actor, Actor::Orchestrator { .. })
                && matches!(recipient, Recipient::User)
                && request_of_scope(domain, scope).is_some_and(|r| r.origin.the_user_asked() && !r.maintenance)
            {
                return refuse("outcome_card", "scope", ALONDRA_TELLS_THE_OUTCOME);
            }
            if let Some(existing) = domain.insert_interaction(
                actor,
                interaction_id,
                scope,
                recipient,
                body,
                false,
                attachments,
                InteractionPayload::Information {
                    read_by: BTreeSet::new(),
                },
                card,
                Source::Information,
            )? {
                created.push(existing.0);
                return Ok(());
            }
            created.push(interaction_id.0.clone());
            if matches!(recipient, Recipient::User) {
                effects.push(DomainEffect::NotifyUser {
                    task_id: match scope {
                        Scope::Task { task_id } => Some(task_id.clone()),
                        Scope::Orchestrator { .. } => None,
                    },
                    interaction_id: interaction_id.clone(),
                });
            }
            if matches!(actor, Actor::System) && matches!(recipient, Recipient::Orchestrator) {
                effects.push(DomainEffect::NotifyOrchestrator {
                    interaction_id: Some(interaction_id.clone()),
                    request_id: None,
                });
            }
        }
        Command::AskQuestion {
            attachments,
            interaction_id,
            scope,
            recipient,
            body,
            blocking,
            options,
            card,
        }
        | Command::RequestAction {
            attachments,
            interaction_id,
            scope,
            recipient,
            body,
            blocking,
            options,
            card,
        } => {
            validate_options(options)?;
            let payload = if matches!(c, Command::AskQuestion { .. }) {
                InteractionPayload::Question {
                    options: options.clone(),
                    answer: None,
                }
            } else {
                InteractionPayload::ActionRequest {
                    options: options.clone(),
                    answer: None,
                }
            };
            let source = if matches!(c, Command::AskQuestion { .. }) { Source::Question } else { Source::Action };
            domain.insert_interaction(
                actor,
                interaction_id,
                scope,
                recipient,
                body,
                *blocking,
                attachments,
                payload,
                card,
                source,
            )?;
            created.push(interaction_id.0.clone());
            if matches!(recipient, Recipient::User) {
                effects.push(DomainEffect::NotifyUser {
                    task_id: match scope {
                        Scope::Task { task_id } => Some(task_id.clone()),
                        Scope::Orchestrator { .. } => None,
                    },
                    interaction_id: interaction_id.clone(),
                });
            }
            if matches!(recipient, Recipient::Orchestrator) {
                effects.push(DomainEffect::NotifyOrchestrator {
                    interaction_id: Some(interaction_id.clone()),
                    request_id: None,
                });
            }
        }
        Command::AskQuestions { interaction_id, scope, recipient, blocking, why, questions, language } => {
            for (card_id, mut card, options) in group_cards(interaction_id, why, questions)? {
                card.language = *language;
                validate_options(&options)?;
                domain.insert_interaction(
                    actor, &card_id, scope, recipient, &card.need, *blocking, &[],
                    InteractionPayload::Question { options, answer: None }, &card, Source::Question,
                )?;
                created.push(card_id.0.clone());
                if matches!(recipient, Recipient::User) {
                    effects.push(DomainEffect::NotifyUser {
                        task_id: match scope {
                            Scope::Task { task_id } => Some(task_id.clone()),
                            Scope::Orchestrator { .. } => None,
                        },
                        interaction_id: card_id.clone(),
                    });
                }
                if matches!(recipient, Recipient::Orchestrator) {
                    effects.push(DomainEffect::NotifyOrchestrator { interaction_id: Some(card_id.clone()), request_id: None });
                }
            }
        }
        Command::HoldQuestions { interaction_id, task_id, agent_id, questions } => {
            if !matches!(actor, Actor::System) {
                return fail("forbidden", "Only Alondra holds a hook");
            }
            let scope = Scope::Task { task_id: task_id.clone() };
            domain.assigned(&Actor::Agent { agent_id: agent_id.clone() }, task_id)?;
            for (card_id, card, options) in group_cards(interaction_id, "", questions)? {
                validate_options(&options)?;
                domain.insert_interaction(
                    &Actor::Agent { agent_id: agent_id.clone() }, &card_id, &scope, &Recipient::User, &card.need, true, &[],
                    InteractionPayload::Question { options, answer: None }, &card, Source::Hold,
                )?;
                domain.holds.insert(card_id.clone(), Hold::Question);
                created.push(card_id.0.clone());
                effects.push(DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: card_id.clone() });
            }
        }
        Command::ReopenAnswer { interaction_id } => {
            user(actor)?;
            let i = domain.interaction(interaction_id)?.clone();
            if i.card.group.is_none() {
                return refuse("invalid_action_for_kind", "interaction_id", "Only a question asked together with others can be reopened");
            }
            if !DomainState::answered_question(&i) || i.withdrawn {
                return refuse("invalid_action_for_kind", "interaction_id", "This question has no answer to take back");
            }
            if domain.group_complete(&i) {
                return refuse("question_already_answered", "interaction_id", "Every question is answered and its author has the answers: they are final");
            }
            if let Scope::Task { task_id } = &i.scope {
                domain.active(task_id)?;
            }
            let revision = domain.revision + 1;
            let i = domain.interactions.get_mut(interaction_id).unwrap();
            match &mut i.payload {
                InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => *answer = None,
                _ => unreachable!(),
            }
            i.revision = revision;
        }
        Command::HoldHook {
            interaction_id,
            task_id,
            agent_id,
            hold,
            body,
            options,
            card,
        } => {
            if !matches!(actor, Actor::System) {
                return fail("forbidden", "Only Alondra holds a hook");
            }
            validate_options(options)?;
            if *hold == Hold::Plan {
                plan_checklist(body)?;
                plan_waits(domain, task_id)?;
            }
            let scope = Scope::Task { task_id: task_id.clone() };
            let winding_down = domain.tasks.get(task_id).is_some_and(|t| DomainState::winding_down(t) && t.agent_id.as_ref() == Some(agent_id));
            if !winding_down {
                domain.assigned(&Actor::Agent { agent_id: agent_id.clone() }, task_id)?;
            }
            domain.insert_interaction(
                &Actor::Agent { agent_id: agent_id.clone() },
                interaction_id,
                &scope,
                &Recipient::User,
                body,
                true,
                &[],
                InteractionPayload::Question {
                    options: options.clone(),
                    answer: None,
                },
                card,
                Source::Hold,
            )?;
            domain.holds.insert(interaction_id.clone(), *hold);
            created.push(interaction_id.0.clone());
            effects.push(DomainEffect::NotifyUser {
                task_id: Some(task_id.clone()),
                interaction_id: interaction_id.clone(),
            });
        }
        Command::Withdraw { interaction_id } => {
            let i = domain.interaction(interaction_id)?;
            match (&i.author, actor) {
                (Author::Agent { agent_id }, Actor::Agent { agent_id: a }) if agent_id == a => {}
                (Author::Orchestrator { .. }, Actor::Orchestrator { .. }) => {}
                (_, Actor::System) if domain.holds.contains_key(interaction_id) => {}
                (Author::System, Actor::System) => {}
                (Author::Agent { .. }, Actor::System) if DomainState::asks_the_orchestrator(i) => {}
                _ => return refuse("forbidden", "interaction_id", "Only a card's author withdraws it: withdraw only your own cards"),
            }
            let unanswered = match (&i.author, &i.scope, actor) {
                (Author::Agent { agent_id }, Scope::Task { task_id }, Actor::System) if DomainState::asks_the_orchestrator(i) && !i.withdrawn =>
                    Some((agent_id.clone(), task_id.clone(), unanswered_withdrawn_notice(i.card.effective_title()))),
                _ => None,
            };
            if i.card.group.is_some() {
                if domain.group_complete(i) {
                    return refuse("question_already_answered", "interaction_id", &domain.already_answered(i));
                }
            } else if DomainState::answered_question(i) {
                return refuse("question_already_answered", "interaction_id", &domain.already_answered(i));
            }
            if matches!(&i.payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid) {
                return refuse("invalid_action_for_kind", "interaction_id", "The user already accepted this delivery: it cannot be withdrawn");
            }
            if !i.withdrawn {
                let presented = match (&i.scope, &i.payload) {
                    (Scope::Task { task_id }, InteractionPayload::Delivery { .. })
                        if domain.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(interaction_id)) => Some(task_id.clone()),
                    _ => None,
                };
                let current = domain.interactions[interaction_id].clone();
                let siblings: Vec<InteractionId> = domain.group_members(&current).iter().map(|member| member.id.clone()).collect();
                domain.retire(interaction_id);
                for sibling in &siblings {
                    domain.retire(sibling);
                    if let Some(InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. }) =
                        domain.interactions.get_mut(sibling).map(|member| &mut member.payload)
                    {
                        *answer = None;
                    }
                }
                if let Some(task) = presented {
                    domain.invalidate(&task);
                    domain.hand_back(&task, interaction_id, delivery_withdrawn(), lifecycles)?;
                }
            }
            if let Some((agent_id, task_id, message)) = unanswered {
                effects.push(DomainEffect::NotifyAgent { agent_id, task_id: Some(task_id), interaction_id: None, message });
            }
        }
        Command::RequestClarification { interaction_id, reason, text } => {
            let u = user(actor)?;
            if let Some(text) = text {
                nonempty(text)?;
            }
            let i = domain.interaction(interaction_id)?;
            if i.recipient != Recipient::User {
                return refuse("forbidden", "interaction_id", "Ask for a clarification only on a card addressed to you");
            }
            if domain.holds.contains_key(interaction_id) || matches!(i.author, Author::System) {
                return refuse("invalid_action_for_kind", "interaction_id", "Alondra wrote this card from a Claude Code prompt: answer it or reply to the node instead");
            }
            if !domain.open(i) {
                return refuse("invalid_action_for_kind", "interaction_id", "This card is no longer open: there is nothing to clarify");
            }
            let message = format!(
                "The user asks you to clarify card {interaction_id} ({}){}. Send its corrected version with card.replaces = \"{interaction_id}\", or withdraw it.",
                match reason { ClarificationReason::Unclear => "it is unclear", ClarificationReason::Contradicts => "it contradicts something", ClarificationReason::MissingContext => "it lacks context" },
                text.as_ref().map(|t| format!(": {}", t.trim())).unwrap_or_default(),
            );
            let (author, scope) = (i.author.clone(), i.scope.clone());
            let i = domain.interactions.get_mut(interaction_id).unwrap();
            i.clarification = Some(Clarification { reason: *reason, text: text.clone(), user_id: u, at_revision: domain.revision + 1 });
            i.revision = domain.revision + 1;
            match (author, scope) {
                (Author::Agent { agent_id }, Scope::Task { task_id }) => effects.push(DomainEffect::NotifyAgent {
                    agent_id, task_id: Some(task_id), interaction_id: Some(interaction_id.clone()), message,
                }),
                (Author::Orchestrator { .. }, _) => effects.push(DomainEffect::NotifyOrchestrator {
                    interaction_id: Some(interaction_id.clone()), request_id: None,
                }),
                _ => {}
            }
        }
        Command::RaiseProblem { attachments, interaction_id, scope, body, options, card } => {
            if !matches!(actor, Actor::System | Actor::Orchestrator { .. }) {
                return refuse("forbidden", "card.kind", "Only Alondra and the orchestrator raise a problem: describe it with ask instead");
            }
            validate_options(options)?;
            domain.insert_interaction(
                actor,
                interaction_id,
                scope,
                &Recipient::User,
                body,
                false,
                attachments,
                InteractionPayload::Question { options: options.clone(), answer: None },
                card,
                Source::Problem,
            )?;
            created.push(interaction_id.0.clone());
            effects.push(DomainEffect::NotifyUser {
                task_id: match scope {
                    Scope::Task { task_id } => Some(task_id.clone()),
                    Scope::Orchestrator { .. } => None,
                },
                interaction_id: interaction_id.clone(),
            });
        }
        Command::MarkRead { interaction_id } => {
            let to_orchestrator = domain.interactions.get(interaction_id).is_some_and(|i| i.recipient == Recipient::Orchestrator);
            let u = match actor {
                Actor::System if to_orchestrator => ORCHESTRATOR_READER.to_owned(),
                _ => user(actor)?,
            };
            let i = domain
                .interactions
                .get_mut(interaction_id)
                .ok_or_else(|| DomainError::new("not_found", "Interaction missing"))?;
            let InteractionPayload::Information { read_by } = &mut i.payload else {
                return fail(
                    "invalid_action_for_kind",
                    "Only information can be marked read",
                );
            };
            if read_by.is_empty() {
                read_by.insert(u);
                i.revision = domain.revision + 1;
            }
        }
        Command::AnswerQuestion {
            attachments,
            interaction_id,
            option_id,
            inputs,
        } => {
            let i = domain.interaction(interaction_id)?.clone();
            let responder = match (&i.recipient, actor) {
                (Recipient::User, Actor::User { user_id }) => user_id.clone(),
                (Recipient::Agent { agent_id }, Actor::Agent { agent_id: a })
                    if agent_id == a =>
                {
                    a.0.clone()
                }
                (Recipient::Orchestrator, Actor::Orchestrator { .. }) => {
                    "alondra-orchestrator".into()
                }
                (Recipient::User, Actor::System) if retakes(&i) => "alondra".into(),
                _ => return fail("forbidden", "Only the intended recipient can answer"),
            };
            if i.card.kind == CardKind::Propose {
                return refuse("suggestion_needs_decision", "interaction_id",
                    "A suggestion is settled with settle_suggestion and one decision: new_task (create it now), current_task (the node does it in its task) or discard (with the reason)");
            }
            if is_stall_case(&i) {
                return refuse("stall_needs_outcome", "interaction_id",
                    "A stopped node is settled with settle_stall and one outcome: instruct (the node gets your instruction) or close (the task ends with your result)");
            }
            let reasoned = inputs.values().any(|value| matches!(value, InputValue::Text(text) if !text.trim().is_empty()));
            if is_plan_refusal(&i) && (option_id != START_CANCEL || !reasoned) {
                return refuse("plan_refused_needs_outcome", "interaction_id",
                    "A refused launch plan is settled with one outcome: fix the plan with update_launch_plan or set_launch_plan (this card closes and the task starts), or answer cancel with the reason in inputs.text");
            }
            let (options, answer) = match &i.payload {
                InteractionPayload::Question { options, answer }
                | InteractionPayload::ActionRequest { options, answer } => (options, answer),
                _ => {
                    return fail(
                        "invalid_action_for_kind",
                        "This Interaction cannot be answered",
                    );
                }
            };
            if answer.is_some() {
                return refuse("question_already_answered", "interaction_id", &domain.already_answered(&i));
            };
            if i.withdrawn {
                return fail("withdrawn", "Its author withdrew this question");
            }
            if let Scope::Task { task_id } = &i.scope
                && !domain.tasks.get(task_id).is_some_and(DomainState::winding_down)
            {
                domain.active(task_id)?;
            }
            validate_answer(options, option_id, inputs, attachments)?;
            secret_answer_guard(&i.card, option_id, inputs, attachments)?;
            form_answer_guard(&i.card, option_id, inputs)?;
            mcp_answer_guard(&i, option_id)?;
            validate_attachments(attachments)?;
            if let (Some(Hold::Plan), Scope::Task { task_id }) = (domain.holds.get(interaction_id), &i.scope)
                && option_id == Hold::APPROVE
            {
                plan_waits(domain, task_id)?;
                plan_specs_refusal(domain, task_id)?;
            }
            let a = Answer {
                attachments: attachments.clone(),
                option_id: option_id.clone(),
                inputs: inputs.clone(),
                responder,
                at_revision: domain.revision + 1,
                decision: None,
            };
            let i = domain.interactions.get_mut(interaction_id).unwrap();
            match &mut i.payload {
                InteractionPayload::Question { answer, .. }
                | InteractionPayload::ActionRequest { answer, .. } => *answer = Some(a),
                _ => unreachable!(),
            };
            i.revision = domain.revision + 1;
            if let (Some(Hold::Plan), Scope::Task { task_id }) = (domain.holds.get(interaction_id), &i.scope)
                && option_id == Hold::APPROVE
            {
                let checklist = Checklist { plan: interaction_id.clone(), items: checklist_items(&i.body) };
                let task_id = task_id.clone();
                domain.checklists.insert(task_id.clone(), checklist);
                let generation = domain.lifecycle(lifecycles, &task_id)?.generation();
                let words = inputs.values().find_map(|value| match value { InputValue::Text(text) => Some(text.clone()), _ => None });
                approve_plan_specs(domain, &task_id, interaction_id, words.as_deref());
                record_plan_approval(domain, &task_id, interaction_id, generation, lifecycles)?;
            }
            let written = inputs.values().find_map(|value| match value { InputValue::Text(text) => Some(text.clone()), _ => None });
            if !settle_claude_card(domain, interaction_id, written.as_deref(), lifecycles)?
                && !settle_mcp_card(domain, interaction_id, lifecycles)?
                && !decide_sdk_card(domain, interaction_id, lifecycles)?
                && !domain.settle_host_lost(interaction_id, option_id, written.as_deref(), lifecycles)?
            {
                domain.answer_lifecycle_card(actor, interaction_id, option_id, written.as_deref(), effects, created, lifecycles)?;
            }
            settle_start_problem(domain, actor, interaction_id, option_id, effects, created, lifecycles)?;
            settle_spec_card(domain, interaction_id, option_id, written.as_deref(), lifecycles)?;
            settle_validation_card(domain, actor, interaction_id, option_id, written.as_deref(), effects, created, lifecycles)?;
            settle_post_merge_problem(domain, interaction_id, option_id, lifecycles)?;
            settle_plan_review_problem(domain, interaction_id, option_id, lifecycles)?;
            if matches!(actor, Actor::User { .. }) {
                hold_for_plan_review(domain, interaction_id, effects, lifecycles)?;
            }
            // D05: the answer reaches whoever holds the Task now, not the
            // session that asked and may since have been replaced.
            let complete = domain.group_complete(&domain.interactions[interaction_id]);
            let i = domain.interactions.get_mut(interaction_id).unwrap();
            match (i.author.clone(), i.scope.clone()) {
                _ if domain.holds.contains_key(interaction_id) || !complete => {}
                (Author::Agent { agent_id }, Scope::Task { task_id }) => {
                    let node = domain.task(&task_id)?.agent_id.clone().unwrap_or(agent_id);
                    effects.push(DomainEffect::NotifyAgent {
                        agent_id: node, task_id: Some(task_id), interaction_id: Some(interaction_id.clone()), message: "Question answered".into(),
                    });
                }
                (Author::Orchestrator { .. }, _) => effects.push(DomainEffect::NotifyOrchestrator {
                    interaction_id: Some(interaction_id.clone()),
                    request_id: None,
                }),
                _ => {}
            }
        }
        Command::SettleSuggestion { interaction_id, decision } => {
            settle_suggestion(domain, actor, interaction_id, decision, effects, created, lifecycles)?;
        }
        Command::SettlePlanReview { review_id, task_id, outcome } => {
            settle_plan_review(domain, actor, review_id, task_id, outcome, effects, created, lifecycles)?;
        }
        Command::SettleStall { case_id, task_id, outcome } => {
            settle_stall(domain, actor, case_id, task_id, outcome, effects, created, lifecycles)?;
        }
        Command::CreateDocument {
            task_id,
            document_id,
            title,
            revision_id,
            content_digest,
            content_reference,
            role,
        } => {
            domain.assigned(actor, task_id)?;
            id(&document_id.0)?;
            id(&revision_id.0)?;
            nonempty(title)?;
            nonempty(role)?;
            digest(content_digest)?;
            nonempty(content_reference)?;
            if domain.documents.contains_key(document_id)
                || domain.document_revisions.contains_key(revision_id)
            {
                return fail("invalid_input", "Document or revision already exists");
            };
            domain.documents.insert(
                document_id.clone(),
                Document {
                    id: document_id.clone(),
                    title: title.clone(),
                    current_revision: revision_id.clone(),
                },
            );
            domain.document_revisions.insert(
                revision_id.clone(),
                DocumentRevision {
                    id: revision_id.clone(),
                    document_id: document_id.clone(),
                    number: 1,
                    content_digest: content_digest.clone(),
                    content_reference: content_reference.clone(),
                },
            );
            domain.tasks
                .get_mut(task_id)
                .unwrap()
                .documents
                .insert(document_id.clone(), role.clone());
            domain.document_reviews.push(DocumentReview {
                task_id: task_id.clone(),
                document_id: document_id.clone(),
                decisions: vec![],
            });
            created.extend([document_id.0.clone(), revision_id.0.clone()]);
        }
        Command::LinkTaskDocument {
            task_id,
            document_id,
            role,
        } => {
            // A document can be shared across Tasks. Only a manager may grant
            // that cross-Task link; knowing a document ID is not authority.
            domain.active(task_id)?;
            domain.manager(actor)?;
            nonempty(role)?;
            if !domain.documents.contains_key(document_id) {
                return fail("not_found", "Document missing");
            };
            domain.tasks
                .get_mut(task_id)
                .unwrap()
                .documents
                .insert(document_id.clone(), role.clone());
            if !domain
                .document_reviews
                .iter()
                .any(|r| &r.task_id == task_id && &r.document_id == document_id)
            {
                domain.document_reviews.push(DocumentReview {
                    task_id: task_id.clone(),
                    document_id: document_id.clone(),
                    decisions: vec![],
                });
            }
        }
        Command::ReviseDocument {
            task_id,
            document_id,
            revision_id,
            content_digest,
            content_reference,
        } => {
            domain.assigned(actor, task_id)?;
            if !domain.task(task_id)?.documents.contains_key(document_id) {
                return fail("forbidden", "Document not linked to Task");
            };
            id(&revision_id.0)?;
            digest(content_digest)?;
            nonempty(content_reference)?;
            if domain.document_revisions.contains_key(revision_id) {
                return fail("invalid_input", "Revision is immutable");
            };
            let doc = domain
                .documents
                .get(document_id)
                .ok_or_else(|| DomainError::new("not_found", "Document missing"))?;
            let number = domain.document_revisions[&doc.current_revision]
                .number
                .checked_add(1)
                .filter(|n| *n <= MAX_REVISION)
                .ok_or_else(|| DomainError::new("invalid_input", "Revision overflow"))?;
            domain.document_revisions.insert(
                revision_id.clone(),
                DocumentRevision {
                    id: revision_id.clone(),
                    document_id: document_id.clone(),
                    number,
                    content_digest: content_digest.clone(),
                    content_reference: content_reference.clone(),
                },
            );
            domain.documents
                .get_mut(document_id)
                .unwrap()
                .current_revision = revision_id.clone();
            let affected: Vec<_> = domain
                .tasks
                .values()
                .filter(|t| t.documents.contains_key(document_id))
                .map(|t| t.id.clone())
                .collect();
            for t in affected {
                domain.invalidate(&t);
            }
            created.push(revision_id.0.clone());
        }
        Command::DecideDocumentReview {
            task_id,
            document_id,
            revision_id,
            accepted,
        } => {
            let u = user(actor)?;
            domain.active(task_id)?;
            if !domain.task(task_id)?.documents.contains_key(document_id) {
                return fail("forbidden", "Document not linked to Task");
            };
            if !domain
                .documents
                .get(document_id)
                .is_some_and(|d| &d.current_revision == revision_id)
            {
                return fail("acceptance_stale", "Document revision changed");
            };
            let presentation = domain.task(task_id)?.final_presentation.clone().filter(|id| {
                domain.presentation(id).is_ok_and(|(_, _, targets)| {
                    targets.contains(&DeliveryTarget::DocumentRevision {
                        document_id: document_id.clone(),
                        revision_id: revision_id.clone(),
                    })
                })
            });
            document_decision(domain, task_id, document_id, revision_id, presentation.clone(), *accepted, &u)?;
            if !accepted {
                domain.invalidate(task_id);
            } else if let Some(presentation) = &presentation {
                domain.decided(presentation);
                settle(domain, presentation, &u);
            }
            notify(domain, 
                task_id,
                None,
                if *accepted {
                    "The user accepted the document."
                } else {
                    "The user requested changes to the document."
                },
                effects,
            )?;
        }
        Command::RegisterPr {
            task_id,
            pr_id,
            host,
            repository,
            number,
            head: sha,
            base,
            allowed_methods,
        } => {
            role_allows(domain, actor, task_id, RoleTool::RegisterPr)?;
            if !domain.assigned(actor, task_id)?.blocks.is_empty() {
                return refuse(
                    "invalid_input",
                    "task_id",
                    "A task that blocks another reviews it and delivers a verdict, not a pull request: leave the code to the task you review, and say what to change in record_verdict, with serious_findings: how many failures you found break behaviour, lose data, or open a security or session hole (0 if none)",
                );
            }
            id(&pr_id.0)?;
            if !alondra_core::id::valid(&pr_id.0) {
                return refuse(
                    "invalid_input",
                    "pr_id",
                    &format!(
                        "pr_id \"{}\" cannot be addressed by the apps: it is {}",
                        pr_id.0,
                        hosted_pr_id(host, repository, *number).0
                    ),
                );
            }
            canonical_pr(host, repository, *number)?;
            if pr_id.0.contains('#') && *pr_id != hosted_pr_id(host, repository, *number) {
                return refuse(
                    "invalid_input",
                    "pr_id",
                    &format!(
                        "pr_id \"{}\" names another PR than {host}/{repository}#{number}",
                        pr_id.0
                    ),
                );
            }
            if !host.contains('.') {
                return refuse(
                    "invalid_input",
                    "host",
                    &format!("host \"{host}\" is not a real host: use github.com (or your GitHub Enterprise host)"),
                );
            }
            head(sha)?;
            nonempty(base)?;
            if allowed_methods.is_empty() {
                return fail("invalid_input", "Merge methods required");
            };
            let opened = OpenedPr { host: host.clone(), repository: repository.clone(), number: *number };
            domain.task(task_id)?;
            domain.tasks.get_mut(task_id).unwrap().opened_prs.remove(&opened);
            let by_alias = known_pr(domain, host, repository, *number, true).filter(|p| p.task_id == *task_id && !p.lives_at(host, repository, *number));
            let mut known = known_pr(domain, host, repository, *number, false).or(by_alias);
            if let Some(earlier) = known.filter(|p| finished_pr_of_another_task(domain, p, task_id, sha)).map(|p| p.id.clone()) {
                give_up_address(domain, &earlier, effects);
                known = None;
            }
            if let Some(p) = known {
                if p.task_id != *task_id {
                    if !domain.task(&p.task_id)?.state.terminal() {
                        return fail("pr_owned_by_other_task", "PR is already registered by a live task");
                    }
                    let review = p.review.clone();
                    let old_id = p.task_id.clone();
                    let existing = p.id.clone();
                    let taken = existing.clone();
                    domain.invalidate(&old_id);
                    let owners = domain.pr_task_history.entry(existing.clone()).or_default();
                    owners.insert(old_id.clone());
                    owners.insert(task_id.clone());
                    let old = domain.tasks.get_mut(&old_id).unwrap();
                    if old.pr_id.as_ref() == Some(&existing) { old.pr_id = old.later_prs.pop(); }
                    old.later_prs.retain(|id| id != &existing);
                    old.open_pr = None;
                    let mut adopted = PullRequest::registered(existing.clone(), task_id.clone(), host.clone(), repository.clone(),
                        *number, sha.clone(), base.clone(), PrState::Open, None, allowed_methods.clone());
                    let earlier = &domain.pull_requests[&existing];
                    adopted.repository = earlier.repository.clone();
                    adopted.aliases = earlier.aliases.clone();
                    adopted.address_read_at = earlier.address_read_at;
                    adopted.identity = earlier.identity.clone();
                    adopt_pr(domain, adopted)?;
                    if review.is_some() {
                        domain.pull_requests.get_mut(&taken).unwrap().review = review;
                    }
                    return Ok(());
                }
                if p.head == *sha
                    && p.base == *base
                    && p.allowed_methods == *allowed_methods
                {
                    return Ok(());
                }
                if p.state != PrState::Open {
                    return fail("pr_not_open", &pr_not_open(p.number));
                }
                let existing = p.id.clone();
                let corrected = domain.pull_requests.get_mut(&existing).unwrap();
                corrected.head = sha.clone();
                corrected.base = base.clone();
                corrected.allowed_methods = allowed_methods.clone();
                return Ok(());
            };
            let pr_id = match domain.pull_requests.get(pr_id) {
                Some(earlier) if earlier.answers_to(host, repository, *number) || !earlier.is_tracked() || earlier.identity.is_unresolvable() => fresh_pr_id(domain, pr_id),
                Some(_) => return refuse(
                    "invalid_input",
                    "pr_id",
                    &format!(
                        "pr_id \"{}\" is another repository's PR: this one is {}",
                        pr_id.0,
                        hosted_pr_id(host, repository, *number).0
                    ),
                ),
                None => pr_id.clone(),
            };
            adopt_pr(domain, PullRequest::registered(pr_id.clone(), task_id.clone(), host.clone(), repository.clone(), *number,
                sha.clone(), base.clone(), PrState::Open, None, allowed_methods.clone()))?;
            created.push(pr_id.0.clone());
        }
        Command::NotePrChanges { task_id } => {
            system(actor)?;
            let task = domain.task(task_id)?;
            let reading: Vec<_> = domain.open_prs(task).map(|pr| OpenedPr { host: pr.host.clone(), repository: pr.repository.clone(), number: pr.number }).collect();
            domain.tasks.get_mut(task_id).unwrap().opened_prs.extend(reading);
        }
        Command::NotePrOpened { task_id, host, repository, number } => {
            system(actor)?;
            let (host, repository) = (normalize_pr_host(host), normalize_pr_repository(repository));
            canonical_pr(&host, &repository, *number)?;
            let known = known_pr(domain, &host, &repository, *number, false)
                .is_some_and(|pr| pr.task_id == *task_id || pr.state == PrState::Open || !domain.tasks[&pr.task_id].state.terminal());
            if !known && !domain.task(task_id)?.state.terminal() {
                domain.tasks.get_mut(task_id).unwrap().opened_prs.insert(OpenedPr { host, repository, number: *number });
            }
        }
        Command::RecordOpenedPr { task_id, host, repository, number, head: sha, base, state, merged_head, allowed_methods } => {
            system(actor)?;
            let opened = OpenedPr { host: host.clone(), repository: repository.clone(), number: *number };
            if !domain.task(task_id)?.opened_prs.contains(&opened) {
                return Ok(());
            }
            domain.tasks.get_mut(task_id).unwrap().opened_prs.remove(&opened);
            let mut known = known_pr(domain, host, repository, *number, false);
            if let Some(earlier) = known.filter(|pr| finished_pr_of_another_task(domain, pr, task_id, sha)).map(|pr| pr.id.clone()) {
                give_up_address(domain, &earlier, effects);
                known = None;
            }
            if let Some(pr) = known {
                if pr.task_id != *task_id {
                    if pr.state != PrState::Open && domain.task(&pr.task_id)?.state.terminal() {
                        return Ok(());
                    }
                    return fail("pr_owned_by_other_task", "PR is registered by another task");
                }
                let pr_id = pr.id.clone();
                apply(domain, actor, &Command::ObservePrState { pr_id: pr_id.clone(), head: sha.clone(), base: base.clone(), state: *state, merged_head: merged_head.clone() }, effects, created, lifecycles)?;
                if !allowed_methods.is_empty() {
                    apply(domain, actor, &Command::ObservePrMergeMethods { pr_id, allowed_methods: allowed_methods.clone() }, effects, created, lifecycles)?;
                }
                return Ok(());
            }
            head(sha)?;
            nonempty(base)?;
            if allowed_methods.is_empty() {
                return fail("invalid_input", "Merge methods required");
            }
            let id = fresh_pr_id(domain, &hosted_pr_id(host, repository, *number));
            adopt_pr(domain, PullRequest::registered(id.clone(), task_id.clone(), host.clone(), repository.clone(), *number,
                sha.clone(), base.clone(), *state, merged_head.clone(), allowed_methods.clone()))?;
            created.push(id.0.clone());
            if *state == PrState::Open && domain.task(task_id)?.final_presentation.is_some() && domain.active(task_id).is_ok() {
                notify(domain, task_id, None, &opened_pr_after_delivery(repository, *number), effects)?;
            }
        }
        Command::AskToRegisterPr { task_id } => {
            system(actor)?;
            domain.task(task_id)?;
        }
        Command::LinkInteractionSubject {
            interaction_id,
            subject,
        } => {
            let i = domain.interaction(interaction_id)?;
            let Scope::Task { task_id } = &i.scope else {
                return fail("invalid_input", "Subject link requires Task scope");
            };
            domain.assigned(actor, task_id)?;
            let subject = &match subject {
                InteractionSubject::PullRequest { pr_id } => InteractionSubject::PullRequest { pr_id: domain.surviving_pr(pr_id).clone() },
                other => other.clone(),
            };
            if !matches!((&i.author,actor),(Author::Agent{agent_id:a},Actor::Agent{agent_id:b}) if a==b)
            {
                return fail(
                    "forbidden",
                    "Only the Interaction author can link its subject",
                );
            };
            domain.subject(task_id, subject)?;
            if let Some(existing) = &i.subject {
                if existing != subject {
                    return fail("subject_already_linked", "Interaction subject is immutable");
                }
                return Ok(());
            }
            let i = domain.interactions.get_mut(interaction_id).unwrap();
            i.subject = Some(subject.clone());
            i.revision = domain.revision + 1;
        }
        Command::PresentResult {
            attachments,
            task_id,
            interaction_id,
            body,
            targets,
            is_final,
            nothing_to_review,
            checklist,
            card,
        } => {
            domain.assigned(actor, task_id)?;
            if targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { .. })) {
                role_allows(domain, actor, task_id, RoleTool::PresentPullRequest)?;
            }
            let targets = &domain.with_open_prs(task_id, targets);
            domain.node_has_run(task_id, lifecycles)?;
            if *nothing_to_review && !domain.task(task_id)?.pending_user_inputs.is_empty() {
                return fail("pending_user_message", "A user message requires a visible final result that addresses it or explains why the work cannot be done");
            }
            if *is_final && !domain.task(task_id)?.pending_user_inputs.is_empty()
                && domain.lifecycle(lifecycles, task_id)?.pending_inputs().iter()
                    .any(|input| domain.tasks[task_id].pending_user_inputs.contains(input.as_str()))
            {
                return fail("pending_user_message", "Read the pending user messages before delivering; the final result must address them or explain why the work cannot be done");
            }
            if *nothing_to_review && (!*is_final || targets.iter().any(|t| !matches!(t, DeliveryTarget::Text { .. }))) {
                return refuse("invalid_input", "nothing_to_review",
                    "nothing_to_review is only for a final delivery without a pull request or document: it closes the task without a card");
            }
            current_pr_targets(domain, task_id, targets, *is_final)?;
            if *is_final && card.plan.is_none() {
                domain.cited_pr_targets(task_id, body, targets, card)?;
            }
            domain.targets(task_id, targets)?;
            let task = domain.task(task_id)?;
            if let Some(opened) = task.opened_prs.first() {
                return refuse("pr_being_read", "targets", &opened_pr_unread(opened));
            }
            let validation = is_final.then(|| (domain.delivery_check(task_id, targets), task.stages.role()));
            if let Some((check, role)) = validation.as_ref().filter(|(check, _)| !check.of_round())
                && let Some(refusal) = check.refusal(*role) {
                return Err(refusal);
            }
            if !*is_final && let Some(pr) = domain.open_prs(task).next() {
                return refuse("pr_needs_final", "is_final", &format!(
                    "PR #{} is open: present it with is_final true so the user can accept it and Alondra merges it. A progress notice cannot be merged. If work follows the merge (publish, deploy), declare it with post_merge_work first", pr.number));
            }
            if *is_final && domain.blockers(task_id) {
                let open = domain.interactions.values()
                    .filter(|i| i.scope == (Scope::Task { task_id: task_id.clone() }) && i.blocking && domain.open(i))
                    .map(|i| i.id.0.clone()).collect::<Vec<_>>().join(", ");
                return refuse("blocking_open", "is_final", &format!("Card {open} still waits on the user: wait for its answer or withdraw it before the final delivery"));
            }
            let missing: Vec<&PullRequest> = domain.open_prs(task)
                .filter(|pr| !targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. } if *pr_id == pr.id)))
                .collect();
            if !missing.is_empty() {
                return refuse("pr_missing", "targets", &prs_missing_from_delivery(&missing));
            }
            checklist_marks_cover_the_plan(domain, task_id, *is_final, checklist, attachments, targets)?;
            if let Some(refusal) = validation.and_then(|(check, role)| check.refusal(role)) {
                return Err(refusal);
            }
            if let Some(plan) = &card.plan {
                if !*is_final || *nothing_to_review || targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { .. })) {
                    return refuse("invalid_input", "plan",
                        "a plan goes only on a final result without a pull request: the user approves it to start a new task that implements it, or only archives the result");
                }
                plan_checklist(plan)?;
            }
            let mut card = card.clone();
            let mut presented = targets.iter().filter_map(|t| match t {
                DeliveryTarget::PullRequest { pr_id, .. } => domain.pull_requests.get(pr_id),
                _ => None,
            }).peekable();
            if presented.peek().is_some() && presented.all(|p| p.state != PrState::Open) {
                card.kind = CardKind::Know;
            }
            domain.insert_interaction(
                actor,
                interaction_id,
                &Scope::Task {
                    task_id: task_id.clone(),
                },
                &Recipient::User,
                body,
                false,
                attachments,
                InteractionPayload::Delivery {
                    targets: targets.clone(),
                    is_final: *is_final,
                    acceptance: None,
                    feedback: vec![],
                },
                &card,
                Source::Delivery,
            )?;
            if *is_final && domain.interactions.values().any(|i| domain.asked_aside(i, task_id)) {
                let open = domain.interactions.values()
                    .filter(|i| domain.asked_aside(i, task_id))
                    .map(|i| format!("“{}”", i.card.effective_title())).collect::<Vec<_>>().join(", ");
                return refuse("unanswered_questions", "is_final", &format!(
                    "You have unanswered questions ({open}): wait for the answers before you deliver, or withdraw the ones you no longer need"));
            }
            if !checklist.is_empty() || (*is_final && domain.checklists.contains_key(task_id)) {
                domain.checklist_marks.insert(interaction_id.clone(), checklist.clone());
            }
            if *is_final {
                domain.invalidate(task_id);
                let standing = domain.lifecycle(lifecycles, task_id)?.delivery().cloned();
                let mut replacement_notice = None;
                if let Some(previous) = standing.filter(|previous| previous != interaction_id) {
                    replacement_notice = Some(domain.next_input(task_id));
                    domain.hand_back(task_id, &previous, delivery_replaced(), lifecycles)?;
                }
                domain.node_has_run(task_id, lifecycles)?;
                domain.decide(lifecycles, task_id, LifecycleRecord::Presented { delivery: interaction_id.clone() })?;
                validator_delivered(domain, task_id);
                if let Some(notice) = replacement_notice {
                    domain.consume_replacement_notice(task_id, notice, lifecycles)?;
                }
                let t = domain.tasks.get_mut(task_id).unwrap();
                t.pending_user_inputs.clear();
                t.final_presentation = Some(interaction_id.clone());
                t.verdict = None;
                t.open_pr = None;
                domain.blocks_acyclic(interaction_id)?;
                let reviewers: Vec<TaskId> = domain.open_blockers(task_id).into_iter()
                    .filter(|b| domain.tasks.get(b).is_some_and(|b| b.started))
                    .collect();
                for reviewer in reviewers {
                    notify(domain, &reviewer, Some(interaction_id.clone()), "The task you review delivered again: review its new final delivery, present your result again and then give your verdict.", effects)?;
                }
                if *nothing_to_review {
                    accept(domain, interaction_id, Feedback::ALONDRA)?;
                }
            }
            created.push(interaction_id.0.clone());
            if !*is_final {
                effects.push(DomainEffect::NotifyUser {
                    task_id: Some(task_id.clone()),
                    interaction_id: interaction_id.clone(),
                });
            }
        }
        Command::SubmitFeedback {
            attachments,
            presentation_id,
            targets,
            body,
            instruction,
            merge_failure,
        } => {
            let (u, task) = domain.feedback_guard(actor, presentation_id, targets, body, attachments, *instruction, merge_failure.as_ref())?;
            if let Some(build) = approved_plan_build(domain, presentation_id) {
                feedback_on_approved_plan(domain, actor, presentation_id, build, body, *instruction, effects, created, lifecycles)?;
                return Ok(());
            }
            let target = DomainState::feedback_target(domain.presentation(presentation_id)?.2, targets)?;
            let presentation = &domain.interactions[presentation_id];
            let reply_window_start = (*instruction == Instruction::Reply).then(|| {
                if domain.open_reply(presentation)
                    && let InteractionPayload::Delivery { feedback, .. } = &presentation.payload
                    && let Some(last) = feedback.last()
                {
                    last.reply_window_start.unwrap_or(last.at_revision)
                } else {
                    domain.revision + 1
                }
            });
            if matches!(instruction, Instruction::Reply | Instruction::Question) {
                invalidate_keeping_user_merges(domain, &task, |state| invalidate_feedback(state, &task, &target));
            } else {
                invalidate_feedback(domain, &task, &target);
            }
            let p = domain.interactions.get_mut(presentation_id).unwrap();
            if let InteractionPayload::Delivery { feedback, .. } = &mut p.payload {
                feedback.push(Feedback {
                    target,
                    reply_window_start,
                    attachments: attachments.clone(),
                    user_id: u,
                    body: body.clone(),
                    at_revision: domain.revision + 1,
                    instruction: *instruction,
                    merge_failure: merge_failure.clone(),
                });
            }
            p.revision = domain.revision + 1;
            if matches!(instruction, Instruction::Changes | Instruction::Conflicts) {
                return_to_implementer(domain, &task);
            }
            notify(domain, &task, Some(presentation_id.clone()), &instruction.message(body), effects)?;
        }
        Command::AcceptDelivery {
            presentation_id,
            targets,
        } => {
            let u = domain.accept_delivery_guard(actor, presentation_id, targets, domain)?;
            for (pr_id, head, method) in domain.accepted_merges(presentation_id, targets)? {
                let authorization = domain.acceptance_authorization(pr_id);
                authorize_merge(domain, &authorization, presentation_id, pr_id, head, method, false, &u, true)?;
                created.push(authorization.0);
            }
            accept(domain, presentation_id, &u)?;
        }
        Command::DecideProposedPlan { presentation_id, targets, choice } => {
            let (u, plan) = domain.decide_plan_guard(actor, presentation_id, targets, *choice, domain)?;
            let source = domain.presentation(presentation_id)?.1.clone();
            accept(domain, presentation_id, &u)?;
            if *choice == PlanChoice::Implement {
                let request_id = guards::implementation_request(presentation_id);
                let auto_merge = domain.requests.get(&source.request_id).is_some_and(|r| r.auto_merge);
                domain.requests.insert(request_id.clone(), Request { auto_merge, ..Request::new(request_id.clone(), plan) });
                domain.request_plans.insert(request_id.clone(), ApprovedPlan { task_id: source.id.clone(), presentation_id: presentation_id.clone() });
                created.push(request_id.0.clone());
                effects.push(DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request_id) });
            }
        }
        Command::AuthorizeMerge {
            authorization_id,
            presentation_id,
            pr_id,
            expected_head,
            method,
            despite_pending_message,
        } => {
            match domain.merge_guard(actor, authorization_id, presentation_id, pr_id, expected_head, *method, *despite_pending_message, domain)? {
                None => {
                    authorize_merge(domain, authorization_id, presentation_id, pr_id, expected_head, *method, false, Feedback::ALONDRA, false)?;
                }
                Some(u) => {
                    authorize_merge(domain, authorization_id, presentation_id, pr_id, expected_head, *method, *despite_pending_message, &u, true)?;
                    settle(domain, presentation_id, &u);
                }
            }
            created.push(authorization_id.0.clone());
        }
        Command::ClaimMergeAuthorization {
            authorization_id,
            remote_head,
            remote_base,
            base_verified,
        } => {
            let a = domain
                .merge_authorizations
                .get(authorization_id)
                .ok_or_else(|| DomainError::new("not_found", "Authorization missing"))?
                .clone();
            if matches!(actor, Actor::Agent { .. }) {
                return fail("forbidden", ALONDRA_MERGES);
            }
            system(actor)?;
            domain.active(&a.task_id)?;
            let (presentation, _, _) = domain.presentation(&a.presentation_id)?;
            let target = FeedbackTarget::PullRequest { pr_id: a.pr_id.clone(), head: a.head.clone() };
            let held = domain.authorized_merge_held(presentation, &a.user_id, &target);
            if held {
                return fail("pending_user_reply", "The user's reply must be resolved before merging this pull request");
            }
            if !a.valid {
                return fail("acceptance_stale", "Authorization was invalidated");
            };
            let node_claim = matches!(a.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain) && a.claimed_by.is_some();
            if a.attempt != MergeAttemptState::Unclaimed && !node_claim {
                return fail(
                    "delivery_uncertain",
                    "Merge attempt already claimed; reconcile instead of retry",
                );
            };
            let pr = &domain.pull_requests[&a.pr_id];
            if pr.state != PrState::Open || pr.head != a.head || remote_head != &a.head {
                return fail("stale_head", "Approved and remote head differ");
            };
            if !base_verified || remote_base != &pr.base {
                return fail("stack_not_ready", "Remote base must be verified");
            };
            domain.stack_ready_for_merge(&a.pr_id)?;
            let a = domain.merge_authorizations.get_mut(authorization_id).unwrap();
            a.attempt = MergeAttemptState::Claimed;
            a.claimed_by = None;
        }
        Command::ObserveMergeAttempt {
            authorization_id,
            state,
        } => {
            system(actor)?;
            let a = domain
                .merge_authorizations
                .get_mut(authorization_id)
                .ok_or_else(|| DomainError::new("not_found", "Authorization missing"))?;
            if !matches!(
                state,
                MergeAttemptState::Uncertain | MergeAttemptState::Failed
            ) || !matches!(
                a.attempt,
                MergeAttemptState::Claimed | MergeAttemptState::Uncertain
            ) {
                return fail("invalid_input", "Invalid merge attempt transition");
            };
            a.attempt = *state;
        }
        Command::ObservePrState {
            pr_id,
            head: sha,
            base,
            state,
            merged_head,
        } => {
            system(actor)?;
            head(sha)?;
            nonempty(base)?;
            if (*state == PrState::Merged) != merged_head.is_some() {
                return fail(
                    "invalid_input",
                    "Merged observation requires exact merged head",
                );
            };
            if let Some(h) = merged_head {
                head(h)?;
            }
            let p = domain
                .pull_requests
                .get(pr_id)
                .ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
            if p.state == PrState::Merged
                && (*state != PrState::Merged || p.merged_head != *merged_head)
            {
                return fail(
                    "invalid_input",
                    "Confirmed merge cannot be undone by observation",
                );
            };
            let task = p.task_id.clone();
            let newly_merged = p.state != PrState::Merged && *state == PrState::Merged;
            let newly_closed = p.state == PrState::Open && *state == PrState::Closed;
            let number = p.number;
            if p.head != *sha {
                domain.invalidate(&task);
            }
            let p = domain.pull_requests.get_mut(pr_id).unwrap();
            p.head = sha.clone();
            p.base = base.clone();
            p.state = *state;
            p.merged_head = merged_head.clone();
            if *state != PrState::Open && domain.open_pr_of(&task).is_none()
                && let Some(t) = domain.tasks.get_mut(&task).filter(|t| matches!(t.open_pr, Some(OpenPrPending::Close { .. })))
            {
                t.open_pr = None;
            }
            if *state == PrState::Merged {
                for a in domain.merge_authorizations.values_mut().filter(|a| {
                    a.pr_id == *pr_id
                        && Some(&a.head) == merged_head.as_ref()
                        && matches!(
                            a.attempt,
                            MergeAttemptState::Claimed | MergeAttemptState::Uncertain
                        )
                }) {
                    a.attempt = MergeAttemptState::Confirmed;
                }
            }
            let discarded = newly_closed && domain.active(&task).is_ok() && domain.task(&task).is_ok_and(|t| discarded(domain, t));
            if discarded {
                domain.cancel(&task, effects, lifecycles)?;
            }
            // A merge that finishes the Task is told by the cleanup request alone.
            let closing = domain.task(&task).is_ok_and(|t| domain.finished(t) && domain.wraps_up(t));
            if (newly_merged || newly_closed) && !discarded && domain.active(&task).is_ok() && !closing {
                let left = unmerged_targets(domain, &task);
                let notice = if newly_merged { merged_notice(number, left) } else { closed_notice(number, left) };
                notify(domain, &task, None, &notice, effects)?;
            }
        }
        Command::ObserveMergeVerdict { pr_id, head: sha, verdict } => {
            system(actor)?;
            head(sha)?;
            let p = domain
                .pull_requests
                .get(pr_id)
                .ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
            if p.state != PrState::Open || &p.head != sha {
                return fail("stale_head", "The verdict is about another head");
            }
            domain.merge_verdicts.insert(pr_id.clone(), ObservedVerdict { head: sha.clone(), verdict: verdict.clone() });
        }
        Command::ObservePrMergeMethods {
            pr_id,
            allowed_methods,
        } => {
            system(actor)?;
            if allowed_methods.is_empty() {
                return fail("invalid_input", "Merge methods required");
            };
            let p = domain
                .pull_requests
                .get_mut(pr_id)
                .ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
            p.allowed_methods = allowed_methods.clone();
        }
        Command::PublishStackReadiness {
            task_id,
            presentation_id,
            pr_id,
            head: sha,
            ready,
        } => {
            domain.assigned(actor, task_id)?;
            let (_, t, targets) = domain.presentation(presentation_id)?;
            if &t.id != task_id
                || !targets.contains(&DeliveryTarget::PullRequest {
                    pr_id: pr_id.clone(),
                    head: sha.clone(),
                })
            {
                return fail(
                    "stack_not_ready",
                    "Stack must identify this Task's current PR delivery",
                );
            };
            domain.stack_readiness.insert(
                task_id.clone(),
                StackReadiness {
                    task_id: task_id.clone(),
                    presentation_id: presentation_id.clone(),
                    pr_id: pr_id.clone(),
                    head: sha.clone(),
                    ready: *ready,
                },
            );
        }
        Command::CompleteTask {
            task_id,
            presentation_id,
        } => {
            if matches!(actor, Actor::Agent { .. }) {
                return fail("forbidden", ALONDRA_MERGES);
            }
            system(actor)?;
            domain.active(task_id)?;
            let (p, t, targets) = domain.presentation(presentation_id)?;
            if &t.id != task_id {
                return fail("forbidden", "Presentation belongs to another Task");
            };
            let auto_merged = domain.requests.get(&t.request_id).is_some_and(|r| r.auto_merge)
                && matches!(&p.payload, InteractionPayload::Delivery { acceptance: None, feedback, .. } if feedback.is_empty())
                && targets.iter().any(|x| matches!(x, DeliveryTarget::PullRequest { .. }));
            if !domain.accepted(p) && !auto_merged {
                return fail(
                    if matches!(&p.payload, InteractionPayload::Delivery { acceptance: Some(a), feedback, .. }
                        if !a.valid && feedback.last().is_some_and(|f| f.at_revision > a.at_revision))
                    {
                        "acceptance_stale"
                    } else {
                        "acceptance_missing"
                    },
                    "Current delivery needs valid user acceptance",
                );
            };
            if domain.pending_reply(p, &FeedbackTarget::Delivery) {
                return fail("pending_user_reply", "A reply still needs a final delivery before completing the task");
            }
            domain.completable(t, targets, domain.accepted(p))?;
            if !domain.wraps_up(t) && !domain.unanswered(task_id) {
                domain.finish_task(task_id, lifecycles)?;
            }
        }
        Command::CancelTask { task_id, .. } => {
            domain.cancel_guard(actor, task_id)?;
            domain.close_task(actor, task_id, None, effects, created, lifecycles)?;
        }
        Command::SettleOpenPr { task_id, status } => {
            settle_open_pr(domain, actor, task_id, status, None, effects, created, lifecycles)?;
        }
        Command::ClosePrOfCancelledTask { task_id, pr_id, branch, status } => {
            close_pr_of_cancelled_task(domain, actor, task_id, pr_id, branch, status, effects, created, lifecycles)?;
        }
        Command::AdoptOpenPr { task_id, pr_id, branch, status } => {
            system(actor)?;
            nonempty(branch)?;
            if domain.branch_has_live_task(branch) { return Ok(()); }
            if !domain.task(task_id)?.state.terminal() {
                return fail("task_not_active", "Only a closed task's PR can be adopted");
            }
            if matches!(domain.task(task_id)?.open_pr, Some(OpenPrPending::Close { .. })) { return Ok(()); }
            if domain.pull_requests.get(pr_id).is_none_or(|pr| pr.task_id != *task_id) {
                return fail("not_found", "The PR is not registered by that task");
            }
            let t = domain.tasks.get_mut(task_id).unwrap();
            if t.pr_id.as_ref() != Some(pr_id) {
                if let Some(previous) = t.pr_id.replace(pr_id.clone()) {
                    t.later_prs.retain(|id| id != pr_id);
                    t.later_prs.push(previous);
                }
            }
            let request = domain.task(task_id)?.request_id.clone();
            let request = domain.requests.get_mut(&request).expect("the task's request exists");
            request.closed = false;
            request.withdrawn = false;
            request.answer = None;
            request.cleared = false;
            domain.invalidate(task_id);
            let t = domain.tasks.get_mut(task_id).unwrap();
            let result = match &t.stall_closing {
                Some(StallClosing::Settled { result }) => Some(result.clone()),
                _ => None,
            };
            t.archived = false;
            t.stall_closing = None;
            t.wrap_up = WrapUp::default();
            t.final_presentation = None;
            t.open_pr = Some(OpenPrPending::Deliver { result });
            domain.decide(lifecycles, task_id, LifecycleRecord::Reopened { input: domain.next_input(task_id) })?;
            settle_open_pr(domain, actor, task_id, status, Some(pr_id), effects, created, lifecycles)?;
        }
        Command::ReopenTask { task_id } => {
            domain.reopen_guard(actor, task_id)?;
            domain.invalidate(task_id);
            let t = domain.tasks.get_mut(task_id).unwrap();
            t.archived = false;
            t.stall_closing = None;
            t.open_pr = None;
            t.wrap_up = WrapUp::default();
            let record = LifecycleRecord::Reopened { input: domain.next_input(task_id) };
            domain.decide(lifecycles, task_id, record)?;
            domain.tasks.get_mut(task_id).unwrap().final_presentation = None;
        }
        Command::ConfirmCleanup { task_id, discard } => {
            let t = domain.assigned(actor, task_id)?;
            if !t.pending_user_inputs.is_empty() {
                return fail("pending_user_message", "A user message still needs a final delivery before cleanup");
            }
            if t.final_presentation.as_ref().and_then(|id| domain.interactions.get(id)).is_some_and(|p| domain.pending_reply(p, &FeedbackTarget::Delivery)) {
                return fail("pending_user_reply", "A reply still needs a final delivery before cleanup");
            }
            if t.wrap_up.asked_at.is_none() && !(t.ending() && t.open_worktrees().next().is_some()) {
                return fail("not_closing", &format!(
                    "Task {} is not closing, so there is nothing to clean up yet: deliver your result with present_result first.", task_id.0));
            }
            if !t.cancelled() && t.ending != Some(TaskEnding::Cancelling) { domain.require_checklist_proof(t)?; }
            if let Some(path) = discard.iter().find(|path| t.worktrees.get(*path).is_none_or(|w| w.end.is_some())) {
                return fail("not_found", &format!(
                    "{path} is not a worktree of task {} that still exists: discard only the worktrees Alondra named.", task_id.0));
            }
            let revision = domain.revision + 1;
            let task = domain.tasks.get_mut(task_id).unwrap();
            for path in discard {
                task.worktrees.get_mut(path).unwrap().discard = true;
            }
            let wrap_up = &mut task.wrap_up;
            wrap_up.cleaned_at = Some(revision);
            domain.end_post_merge(task_id, PostMergeEnd::Cleanup, lifecycles)?;
            domain.drain(task_id, lifecycles, |owed| matches!(owed, Drained::Cleanup { .. }))?;
            domain.reconcile_recovery_cards(task_id, lifecycles)?;
        }
        Command::DeclarePostMergeWork { task_id, reason, waits_for } => {
            role_allows(domain, actor, task_id, RoleTool::PostMergeWork)?;
            let task = domain.assigned(actor, task_id)?;
            let declared_at = domain.revision + 1;
            let work = reason.as_deref().map(str::trim).filter(|reason| !reason.is_empty())
                .map(|reason| PostMergeWork { reason: clip(reason, POST_MERGE_WORK_MAX), declared_at, turn_since: false });
            if work.is_none() && task.final_presentation.is_some() && !task.cancelled() && task.ending != Some(TaskEnding::Cancelling) {
                domain.require_checklist_proof(task)?;
            }
            let waited = wait_targets(domain, task_id, waits_for, work.is_some())?;
            match work {
                Some(work) => domain.declare_post_merge(task_id, work, lifecycles)?,
                None => domain.end_post_merge(task_id, PostMergeEnd::NodeDeclaredEnd, lifecycles)?,
            }
            if !waited.is_empty() {
                domain.invalidate(task_id);
                domain.tasks.get_mut(task_id).unwrap().dependencies.extend(waited);
                domain.dependency_graph()?;
            }
        }
        Command::RecordNodeEnd { task_id, end, agent_id } => {
            system(actor)?;
            let t = domain.task(task_id)?;
            if agent_id.as_ref().is_some_and(|node| t.agent_id.as_ref() != Some(node)) {
                return fail("conflict", "The task has another node by now");
            }
            match t.wrap_up.node {
                Some(recorded) if recorded == *end => return Ok(()),
                Some(_) => return fail("conflict", "node end already recorded"),
                None => {}
            }
            if t.ending() && t.agent_id.is_none() {
                return fail("not_closing", &format!("Task {} has no node: its cleanup is the receipt", task_id.0));
            }
            let closing = domain.in_cleanup(t) && domain.wraps_up(t)
                && (t.cancelled() || t.ending == Some(TaskEnding::Cancelling) || domain.checklist_owed(t).is_empty());
            if !closing && !t.state.terminal() && !t.ending() {
                if matches!(end, NodeEnd::Closed | NodeEnd::Gone) && t.agent_id.is_some() {
                    let node = closed_node(domain, task_id, None, Relaunch::ByItself);
                    domain.end_post_merge(task_id, PostMergeEnd::SessionOver, lifecycles)?;
                    domain.release(task_id, lifecycles)?;
                    let task = domain.tasks.get_mut(task_id).unwrap();
                    task.wrap_up = WrapUp::default();
                    task.released = Some(node);
                    return Ok(());
                }
                return fail("not_closing", &format!("Task {} is not closing", task_id.0));
            }
            let fact = match end {
                NodeEnd::Gone => PostMergeEnd::SessionOver,
                NodeEnd::KeptByUser | NodeEnd::UsersOwn => PostMergeEnd::UserDecision,
                NodeEnd::Closed if t.cancelled() => PostMergeEnd::UserDecision,
                NodeEnd::Closed => {
                    if domain.lifecycle(lifecycles, task_id)?.post_merge().is_some() {
                        return fail("post_merge_running", &format!(
                            "Task {}'s node still runs work after the merge: only the node, its cleanup or the user ends it, never Alondra closing it", task_id.0));
                    }
                    PostMergeEnd::SessionOver
                }
            };
            if matches!(end, NodeEnd::Closed | NodeEnd::Gone) {
                no_open_worktrees(domain, task_id)?;
            }
            domain.end_post_merge(task_id, fact, lifecycles)?;
            let task = domain.tasks.get_mut(task_id).unwrap();
            if matches!(end, NodeEnd::KeptByUser | NodeEnd::UsersOwn) {
                for worktree in task.worktrees.values_mut().filter(|w| w.end.is_none()) {
                    worktree.end = Some(WorktreeEnd::KeptByUser);
                }
            }
            task.wrap_up.node = Some(*end);
            if domain.tasks[task_id].ending() {
                domain.drain(task_id, lifecycles, |_| true)?;
            } else if domain.lifecycle(lifecycles, task_id)?.execution().is_some() {
                domain.release(task_id, lifecycles)?;
            }
        }
        Command::ReleaseTask { task_id, reason, relaunch, cwd } => {
            system(actor)?;
            nonempty(reason)?;
            let task = domain.active(task_id)?;
            if task.ending() {
                return fail("task_not_active", "Task is ending: its node is drained by its receipts");
            }
            if task.agent_id.is_none() || task.final_presentation.is_some() {
                return fail("conflict", "Task has no working Agent to release");
            }
            let node = closed_node(domain, task_id, cwd.clone(), *relaunch);
            domain.release(task_id, lifecycles)?;
            domain.tasks.get_mut(task_id).unwrap().released = Some(node);
        }
        Command::AdmitExecution { task_id, agent_id, conversation, instance, host, model, account, provider, leaves_limit } => {
            system(actor)?;
            not_held_for_plan_review(domain, task_id)?;
            domain.not_ending(task_id)?;
            domain.active(task_id)?;
            if !domain.agents.contains_key(agent_id) {
                return fail("not_found", "Agent missing");
            }
            if domain.task(task_id)?.agent_id.as_ref() != Some(agent_id) {
                used_by_other(domain, agent_id, task_id)?;
                free(domain, agent_id, task_id)?;
            }
            let task=domain.task(task_id)?;
            let previously_admitted=domain.lifecycle_journal.get(task_id).is_some_and(|records|records.iter().any(|record|matches!(record,LifecycleRecord::Admitted{execution:Some(_),..}|LifecycleRecord::ResourceAvailable)));
            let target=alondra_core::accounts::AccountKey::new(*provider,account.clone().unwrap_or_else(alondra_core::accounts::AccountId::default_account));
            let moved_here=target!=task.account_key() && task.selection.as_ref().and_then(|selection|selection.account.as_ref())==Some(&target) && task.pending_selection.is_none() && domain.lifecycle_of(task_id).is_ok_and(|lifecycle|lifecycle.execution().is_none());
            if (previously_admitted || task.started) && domain.pending_reopening(task_id).is_none()
                && target!=task.account_key() && domain.moving_to(task_id)!=Some(&target) && !moved_here{return fail("active_account_immutable","Choose an account and provider when creating a new node. This task keeps its original native session and account")}
            if let Some(card) = leaves_limit {
                let to = account.clone().unwrap_or_else(alondra_core::accounts::AccountId::default_account);
                leave_limit(domain, task_id, card, *provider, &to, lifecycles)?;
            }
            let identity = ExecutionIdentity { conversation: conversation.clone(), instance: instance.clone(), host: host.clone() };
            domain.admit_execution(task_id, agent_id, identity, lifecycles)?;
            let task = domain.tasks.get_mut(task_id).expect("admitted above");
            if model.is_some() {
                task.model = model.clone();
            }
            task.account = account.clone().filter(|account| !account.is_default());
            task.provider = *provider;
            if moved_here {
                task.move_refused = None;
            }
            let generation = domain.lifecycle(lifecycles, task_id)?.generation();
            record_run(domain, task_id, agent_id, generation, model.as_ref());
        }
        Command::RecordRuntimeFacts { task_id, generation, facts } => {
            system(actor)?;
            domain.task(task_id)?;
            domain.record_runtime_facts(task_id, *generation, facts, effects, lifecycles)?;
        }
        Command::RecordTurnBoundary { task_id, generation, turn, failure, failed } => {
            system(actor)?;
            domain.task(task_id)?;
            nonempty(turn)?;
            if let Some(code) = failed {
                nonempty(code)?;
            }
            domain.record_turn_boundary(task_id, *generation, turn, failure.as_ref(), failed.as_deref(), effects, lifecycles)?;
        }
        Command::RecordClaudeHold { task_id, generation, failure } => {
            system(actor)?;
            domain.task(task_id)?;
            domain.sdk_execution(task_id, *generation, lifecycles)?;
            domain.hold_after_failure(task_id, failure, effects, lifecycles)?;
        }
        Command::RecordDroppedInput { task_id, generation, input, text } => {
            system(actor)?;
            domain.task(task_id)?;
            nonempty(input)?;
            nonempty(text)?;
            domain.record_dropped_input(task_id, *generation, input, text, lifecycles)?;
        }
        Command::RecordHostEnded { task_id, generation, resumes, ended_work, by_alondra } => {
            system(actor)?;
            domain.task(task_id)?;
            domain.record_host_ended(task_id, *generation, *resumes, *by_alondra, ended_work, effects, lifecycles)?;
        }
        Command::DeferTaskMove { task_id, account } => {
            system(actor)?;
            domain.active(task_id)?;
            if domain.task(task_id)?.pending_selection.as_ref().map(|pending| &pending.account) != Some(account) {
                return fail("conflict", "The task is not moving to that account");
            }
            if domain.lifecycle(lifecycles, task_id)?.execution().is_some() {
                domain.leave_for_the_move(task_id, lifecycles)?;
            }
            end_gate_pause(domain, task_id, lifecycles)?;
            leave_hold_for_the_move(domain, task_id, account, lifecycles)?;
            domain.tasks.get_mut(task_id).expect("checked above").pending_selection = None;
        }
        Command::RecordHostReleased { task_id, generation } => {
            system(actor)?;
            domain.task(task_id)?;
            domain.record_host_released(task_id, *generation, lifecycles)?;
        }
        Command::RecordHostRetaken { task_id, generation } => {
            system(actor)?;
            domain.task(task_id)?;
            domain.record_host_retaken(task_id, *generation, lifecycles)?;
            domain.validator_retaken(task_id, *generation);
        }
        Command::RecordTaskStages { task_id, staged, reason, selections } => {
            record_task_stages(domain, actor, task_id, *staged, reason, selections)?;
        }
        Command::BeginStageHandover { task_id, expected_selection, transition, pending } => {
            begin_stage_handover(domain, actor, task_id, expected_selection, transition, pending)?;
        }
        Command::StageHandoverFailed { task_id, transition, refused } => {
            stage_handover_failed(domain, actor, task_id, transition.as_deref(), *refused)?;
        }
        Command::ProposeSpecs { task_id, level, files, check } => {
            propose_specs(domain, actor, task_id, *level, files, check)?;
        }
        Command::ReviseSpecs { task_id, expected, level, files, check } => {
            revise_specs(domain, actor, task_id, *expected, *level, files, check)?;
        }
        Command::RecordSpecSettings { task_id, problem } => {
            record_spec_settings(domain, actor, task_id, problem.as_ref())?;
        }
        Command::RecordSpecsWritten { task_id, written } => {
            record_specs_written(domain, actor, task_id, written, lifecycles)?;
        }
        Command::RequestSpecChange { task_id, reason } => {
            request_spec_change(domain, actor, task_id, reason, lifecycles)?;
        }
        Command::ProposeAmendment { task_id, amendment_id, files, check } => {
            propose_amendment(domain, actor, task_id, amendment_id, files, check, effects, created, lifecycles)?;
        }
        Command::ProposeDecision { task_id, decision, files, check } => {
            propose_decision(domain, actor, task_id, decision, files, check, effects, created)?;
        }
        Command::ReadyForValidation { task_id, head, items, scope } => {
            ready_for_validation(domain, actor, task_id, head, items, scope, lifecycles)?;
        }
        Command::RecordValidation { task_id, subject, check, verdict, reasoning, nearest_wrong, output, evidence, inputs, listed } => {
            let record = Recorded { subject, kind: *check, verdict: *verdict, reasoning: reasoning.as_deref(), nearest_wrong: nearest_wrong.as_deref(), output: output.as_deref(), evidence, inputs, listed: listed.as_deref() };
            record_validation(domain, actor, task_id, record)?;
        }
        Command::EndValidation { task_id, findings } => {
            end_validation(domain, actor, task_id, findings, effects, created, lifecycles)?;
        }
        Command::RecordCommitReading { task_id, reading } => {
            record_commit_reading(domain, actor, task_id, reading)?;
        }
        Command::RecordDeliveredValidation { task_id, validation } => {
            record_delivered_validation(domain, actor, task_id, validation)?;
        }
        Command::AbortTaskSelection { task_id, selection, refused } => {
            system(actor)?;
            domain.task(task_id)?;
            if let Some(refused) = refused { nonempty(&refused.reason)?; }
            let task = domain.tasks.get_mut(task_id).expect("checked above");
            task.selection = selection.clone();
            task.pending_selection = None;
            if refused.is_some() { task.move_refused = refused.clone(); }
        }
        Command::RecordDoubtfulHandoff { task_id, generation, input, text, resumes, .. } => {
            system(actor)?;
            domain.task(task_id)?;
            nonempty(input)?;
            nonempty(text)?;
            domain.record_doubtful_handoff(task_id, *generation, input, text, *resumes, effects, lifecycles)?;
        }
        Command::ReconcileRecoveryCards { task_id } => {
            system(actor)?;
            domain.task(task_id)?;
            domain.reconcile_recovery_cards(task_id, lifecycles)?;
        }
        Command::RecordHostLost { task_id, reason, quiet, signed_out, node } => {
            system(actor)?;
            if node.is_some() && domain.task(task_id)?.agent_id != *node {
                return Err(DomainError::new("conflict", format!("Task {task_id} no longer runs on the node whose host was lost")));
            }
            domain.task(task_id)?;
            nonempty(reason)?;
            domain.record_host_lost(task_id, reason, *quiet, *signed_out, effects, lifecycles)?;
        }
        Command::ResendUntakenInput { task_id, generation, input, text } => {
            system(actor)?;
            domain.task(task_id)?;
            nonempty(input)?;
            nonempty(text)?;
            domain.resend_untaken(task_id, *generation, input, text, lifecycles)?;
        }
        Command::ProbeClaudeAccess { provider, hold, account } => {
            system(actor)?;
            if *hold == ClaudeHold::SignedOut {
                return fail("invalid_input", "Only the user's answer probes Claude's sign-in");
            }
            if probe_claude(domain, *provider, *hold, account.as_ref(), lifecycles)?.is_none() {
                return fail("not_found", "No task waits for Claude");
            }
        }
        Command::InterruptTurn { agent_id } => {
            let (task, record) = domain.interrupt_guard(actor, agent_id)?;
            domain.decide(lifecycles, &task, record)?;
        }
        Command::ForceStopTurn { agent_id } => {
            let (task, record) = domain.force_stop_guard(actor, agent_id)?;
            domain.decide(lifecycles, &task, record)?;
        }
        Command::RecordInterruptAccepted { task_id, generation, turn } => {
            system(actor)?;
            domain.task(task_id)?;
            nonempty(turn)?;
            domain.record_interrupt_accepted(task_id, *generation, turn, lifecycles)?;
        }
        Command::SetModel { task_id, model } => {
            let record = domain.control_guard(actor, task_id, ControlRecord::SetModel { model: model.clone() })?;
            domain.decide(lifecycles, task_id, record)?;
        }
        Command::SetPermissionMode { task_id, mode } => {
            let record = domain.control_guard(actor, task_id, ControlRecord::SetPermissionMode { mode: *mode })?;
            domain.decide(lifecycles, task_id, record)?;
        }
        Command::ReadMcpStatus { task_id } => {
            let record = domain.control_guard(actor, task_id, ControlRecord::ReadMcpStatus)?;
            domain.decide(lifecycles, task_id, record)?;
        }
        Command::ReconnectMcp { task_id, server } => {
            let record = domain.mcp_reconnect_guard(actor, task_id, server)?;
            domain.decide(lifecycles, task_id, record)?;
        }
        Command::StartMcpSignIn { task_id, server } => {
            let record = domain.mcp_sign_in_guard(actor, task_id, server)?;
            domain.decide(lifecycles, task_id, record)?;
        }
        Command::ClearMcpSignIn { task_id, server } => {
            let record = domain.mcp_sign_out_guard(actor, task_id, server)?;
            domain.decide(lifecycles, task_id, record)?;
        }
        Command::RecordMcpServers { task_id, generation, servers } => {
            record_mcp_servers(domain, actor, task_id, *generation, servers)?;
            mcp_reported(domain, task_id, lifecycles)?;
        }
        Command::WaitForMcp { task_id, server } => wait_for_mcp(domain, actor, task_id, server, effects, lifecycles)?,
        Command::RecordMcpSignIn { task_id, generation, server, sign_in } => {
            record_mcp_sign_in(domain, actor, task_id, *generation, server, sign_in)?;
        }
        Command::CompleteMcpSignIn { task_id, server, state } => complete_mcp_sign_in(domain, actor, task_id, server, state)?,
        Command::CompactEndedTasks { task_ids } => {
            system(actor)?;
            compact_ended_tasks(domain, task_ids)?;
        }
        Command::TellNode { task_id, text, from_user, answers } => {
            system(actor)?;
            nonempty(text)?;
            if *from_user && answers.is_none() && domain.is_scheduled_task(task_id) {
                return scheduled_message(domain, task_id, text, effects, created, lifecycles);
            }
            let task = domain.task(task_id)?;
            if task.state.terminal() || (*from_user && task.ending.is_some()) {
                return fail("task_not_active", &format!("Task {task_id} has ended or is closing"));
            }
            if let Some(card) = answers {
                node_answer(domain, task_id, card)?;
            }
            if (!*from_user || answers.is_some()) && domain.lifecycle(lifecycles, task_id)?.execution().is_some_and(|stamp| stamp.host().as_str() == alondra_domain::CLAUDE_BG_HOST) {
                return fail("conflict", &format!("Task {task_id} has a node that does not run through the Agent SDK"));
            }
            let held: Vec<InteractionId> = domain.lifecycle(lifecycles, task_id)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
            // The user writing to a task whose host was lost is their «Try again», with what they wrote.
            let lost = held.iter().find(|card| *from_user && answers.is_none() && domain.interactions.get(*card).is_some_and(retakes)).cloned();
            let user_input = domain.next_input(task_id);
            if let Some(card) = lost {
                domain.settle_host_lost(&card, RETAKE, Some(text), lifecycles)?;
            } else {
                let input = domain.next_input(task_id);
                let record = LifecycleRecord::Told { input: input.clone(), text: Some(text.clone()), answers: answers.clone() };
                domain.decide(lifecycles, task_id, record)?;
            }
            if *from_user && answers.is_none() {
                awaits_answer_to_user(domain, task_id, user_input);
            }
            let answered: Vec<InteractionId> = {
                let still = domain.lifecycle(lifecycles, task_id)?.cards();
                held.into_iter().filter(|card| !still.iter().any(|(held, _)| *held == card)).collect()
            };
            for card in &answered {
                domain.retire(card);
            }
        }
        Command::RecordDecisionRequest { task_id, generation, epoch, hold, tool, input, mcp_server, prompt } => {
            record_decision_request(domain, actor, task_id, *generation, *epoch, *hold, tool.as_deref(), input,
                mcp_server.as_ref(), prompt, effects, created, lifecycles)?;
        }
        Command::RefuseDecisionRequest { task_id, generation, epoch, reason } => {
            refuse_decision_request(domain, actor, task_id, *generation, *epoch, reason, lifecycles)?;
        }
        Command::RecordDecisionSettled { task_id, generation, epoch } => {
            end_decision(domain, actor, task_id, *generation, *epoch, true, effects, lifecycles)?;
        }
        Command::RecordDecisionAborted { task_id, generation, epoch } => {
            end_decision(domain, actor, task_id, *generation, *epoch, false, effects, lifecycles)?;
        }
        Command::RecordTaskCleanup { task_id } => {
            system(actor)?;
            let t = domain.task(task_id)?;
            if !t.ending() {
                return fail("not_closing", &format!("Task {} is not ending", task_id.0));
            }
            if domain.wraps_up(t) {
                return fail("conflict", "The task still has its node: its end is the receipt");
            }
            no_open_worktrees(domain, task_id)?;
            domain.drain(task_id, lifecycles, |_| true)?;
        }
        Command::RecordTaskWorktree { task_id, path, origin, branch } => {
            system(actor)?;
            nonempty(path)?;
            if !path.starts_with('/') {
                return fail("invalid_input", "A worktree path is absolute");
            }
            if domain.task(task_id)?.state.terminal() {
                return fail("task_not_active", &format!("Task {} has ended", task_id.0));
            }
            let worktrees = &mut domain.tasks.get_mut(task_id).unwrap().worktrees;
            match worktrees.get_mut(path) {
                Some(known) if known.end.is_none() => {
                    if known.branch.is_none() {
                        known.branch = branch.clone();
                    }
                }
                _ => {
                    worktrees.insert(path.clone(), TaskWorktree { origin: *origin, branch: branch.clone(), discard: false, end: None });
                }
            }
        }
        Command::ReleaseTaskWorktree { task_id, path, end } => {
            system(actor)?;
            let t = domain.task(task_id)?;
            match t.worktrees.get(path) {
                None if *end == WorktreeEnd::Removed && t.state.terminal() => {
                    domain.tasks.get_mut(task_id).unwrap().worktrees.insert(path.clone(), TaskWorktree {
                        origin: WorktreeOrigin::Task, branch: None, discard: false, end: Some(WorktreeEnd::Removed),
                    });
                    return Ok(());
                }
                None => return fail("not_found", &format!("{path} is not a worktree of task {}", task_id.0)),
                Some(known) if known.end == Some(*end) => return Ok(()),
                Some(known) if *end == WorktreeEnd::Removed && matches!(known.end, Some(WorktreeEnd::KeptByUser | WorktreeEnd::UsersOwn)) => {}
                Some(known) if known.end.is_some() => return fail("conflict", "The worktree already ended"),
                Some(_) => {}
            }
            if *end == WorktreeEnd::HandedOver && domain.worktree_held_by_other(path, task_id).is_none() {
                return fail("conflict", &format!("No other open task works in {path}, so it cannot be handed over"));
            }
            domain.tasks.get_mut(task_id).unwrap().worktrees.get_mut(path).unwrap().end = Some(*end);
        }
        Command::ArchiveTask { task_id, archived } => {
            domain.archive_guard(actor, task_id, *archived)?;
            domain.tasks.get_mut(task_id).unwrap().archived = *archived;
        }
        Command::RevokeTaskGrant { task_id, card } => {
            domain.revoke_grant_guard(actor, task_id, card)?;
            domain.tasks.get_mut(task_id).unwrap().allowed.remove(card);
        }
        Command::ReportFailure { task_id } => {
            domain.report_failure_guard(actor, task_id)?;
        }
        Command::ClearCompleted { seen } => {
            let (tasks, requests) = domain.clear_guard(actor, *seen)?;
            for id in tasks {
                domain.tasks.get_mut(&id).unwrap().archived = true;
            }
            for id in requests {
                domain.requests.get_mut(&id).unwrap().cleared = true;
            }
        }
        Command::SetMergeMethod { method } => {
            domain.merge_method_guard(actor, *method)?;
            domain.merge_method = *method;
        }
        Command::SetBranchUpdate { update } => {
            domain.branch_update_guard(actor, *update)?;
            domain.branch_update = *update;
        }
    }
    Ok(())
}
pub(crate) fn document_decision(
    domain: &mut DomainState,
    task: &TaskId,
    doc: &DocumentId,
    rev: &DocumentRevisionId,
    presentation: Option<InteractionId>,
    accepted: bool,
    u: &str,
) -> Result<()> {
    let r = domain
        .document_reviews
        .iter_mut()
        .find(|r| &r.task_id == task && &r.document_id == doc)
        .ok_or_else(|| DomainError::new("not_found", "Document review missing"))?;
    r.decisions.push(DocumentDecision {
        revision_id: rev.clone(),
        presentation_id: presentation,
        accepted,
        user_id: u.into(),
        at_revision: domain.revision + 1,
    });
    Ok(())
}
/// The user's go-ahead to merge one presented PR head, which Alondra then merges.
pub(crate) fn authorize_merge(
    domain: &mut DomainState,
    authorization_id: &AuthorizationId,
    presentation_id: &InteractionId,
    pr_id: &PrId,
    expected_head: &str,
    method: MergeMethod,
    despite_pending_message: bool,
    u: &str,
    consent: bool,
) -> Result<TaskId> {
    let task = domain.authorize_merge_guard(authorization_id, presentation_id, pr_id, expected_head, method, despite_pending_message, domain)?;
    let (presentation, _, _) = domain.presentation(presentation_id)?;
    let despite_pending_message = despite_pending_message && domain.pending_user_message(presentation,
        &FeedbackTarget::PullRequest { pr_id: pr_id.clone(), head: expected_head.to_owned() });
    for a in domain.merge_authorizations.values_mut().filter(|a| a.pr_id == *pr_id) {
        a.valid = false;
    }
    domain.merge_authorizations.insert(
        authorization_id.clone(),
        MergeAuthorization {
            id: authorization_id.clone(),
            task_id: task.clone(),
            pr_id: pr_id.clone(),
            presentation_id: presentation_id.clone(),
            head: expected_head.to_owned(),
            method,
            user_id: u.to_owned(),
            valid: true,
            attempt: MergeAttemptState::Unclaimed,
            claimed_by: None,
            at_revision: domain.revision + 1,
            despite_pending_message,
        },
    );
    if let Some(r) = domain.pull_requests.get_mut(pr_id).unwrap().review.as_mut().filter(|_| consent) {
        r.decisions.push(PrDecision {
            presentation_id: presentation_id.clone(),
            head: expected_head.to_owned(),
            accepted: true,
            user_id: u.to_owned(),
            at_revision: domain.revision + 1,
        });
    }
    domain.decided(presentation_id);
    Ok(task)
}
pub(crate) fn accept(domain: &mut DomainState, presentation: &InteractionId, u: &str) -> Result<()> {
    domain.acceptance_guard(presentation)?;
    let (_, t, targets) = domain.presentation(presentation)?;
    let task = t.id.clone();
    let targets = targets.clone();
    for target in targets {
        if let DeliveryTarget::DocumentRevision {
            document_id,
            revision_id,
        } = target
        {
            document_decision(domain, 
                &task,
                &document_id,
                &revision_id,
                Some(presentation.clone()),
                true,
                u,
            )?;
        }
    }
    record_acceptance(domain, presentation, u);
    Ok(())
}
pub(crate) fn record_acceptance(domain: &mut DomainState, presentation: &InteractionId, u: &str) {
    let p = domain.interactions.get_mut(presentation).unwrap();
    if let InteractionPayload::Delivery { acceptance, .. } = &mut p.payload {
        *acceptance = Some(Acceptance {
            user_id: u.into(),
            at_revision: domain.revision + 1,
            valid: true,
        });
    }
    p.revision = domain.revision + 1;
}
/// A plan states its scope as Markdown checklist lines, so its final delivery can be held to it.
pub(crate) fn plan_checklist(plan: &str) -> Result<()> {
    match plan_checklist_problem(plan) {
        Some((code, fix)) => refuse(code, "plan", &fix),
        None => Ok(()),
    }
}
/// An option is a button: its answer carries at most the words the user wrote on the card. With no
/// option, the answer is those words or the files the user attached.
/// A card whose secret goes to a file is answered only with where it was written, or with one of
/// its options: its value never enters the domain.
pub(crate) fn secret_answer_guard(card: &CardSpec, option: &str, inputs: &BTreeMap<String, InputValue>, attachments: &[AttachmentRef]) -> Result<()> {
    let Some(destination) = &card.secret_destination else { return Ok(()) };
    let written = destination.written();
    let only_written = inputs.values().all(|value| matches!(value, InputValue::Text(text) if *text == written));
    if !attachments.is_empty() || !only_written || (option.is_empty() && inputs.is_empty()) {
        return refuse("invalid_input", "inputs", &format!("Alondra writes the secret to {} and answers with where it went: the value never goes in an answer", destination.file));
    }
    Ok(())
}
/// A form's values only answer a card that carries the form, with the option that accepts it, and
/// only when they fill it.
pub(crate) fn form_answer_guard(card: &CardSpec, option: &str, inputs: &BTreeMap<String, InputValue>) -> Result<()> {
    let Some(InputValue::Form(content)) = inputs.get(ANSWER_CONTENT) else { return Ok(()) };
    let Some(form) = &card.form else {
        return refuse("invalid_input", "input.content", "Only a card with a form takes form values");
    };
    if option != Hold::ACCEPT {
        return refuse("invalid_input", "input.content", "Form values go with the option that accepts the form");
    }
    form.check(content).or_else(|problem| refuse("invalid_input", "input.content", &problem.to_string()))
}
