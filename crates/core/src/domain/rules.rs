use super::*;
use std::collections::{BTreeMap, BTreeSet};
mod plan_review;
pub use plan_review::{PLAN_REVIEW_KEEP_PLANS, PLAN_REVIEW_PROBLEM_PREFIX, PLAN_REVIEW_REMIND, PlanReviewProblem};
#[path = "guards.rs"]
mod guards;
type Result<T> = std::result::Result<T, DomainError>;
fn fail<T>(code: &str, msg: &str) -> Result<T> {
    Err(DomainError::new(code, msg))
}
/// A rejection a node can act on: what is wrong, in which field, and how to fix it.
fn refuse<T>(code: &str, field: &str, fix: &str) -> Result<T> {
    let mut error = DomainError::new(code, format!("{field}: {fix}"));
    error.subject = Some(field.into());
    Err(error)
}
/// Stopping for the subscription's usage is decided by the user on the usage card and by
/// Alondra's pacing, never by a node or the orchestrator: a node's question about it is refused
/// before anyone can answer "hold".
fn usage_is_not_a_stop(card: &CardSpec, options: &[QuestionOption]) -> Result<()> {
    let mut asked = vec![card.need.as_str(), card.why.as_str()];
    for option in options {
        asked.push(&option.label);
        asked.extend(option.description.as_deref());
    }
    if crate::usage::usage_stop_question(&asked.join("\n")) {
        return refuse("usage_is_not_a_stop", "card.need",
            "The subscription's usage is never a reason to stop or wait: Alondra paces usage and the user decides on the usage card. Do not ask; carry on with your task");
    }
    Ok(())
}
/// The command a card arrives with, which bounds the kinds it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Information,
    Question,
    Action,
    Delivery,
    Hold,
    Problem,
}
impl Source {
    fn kinds(self) -> &'static [CardKind] {
        use CardKind::*;
        match self {
            Source::Information => &[Know],
            Source::Question => &[Decide, Provide, Test, Propose],
            Source::Action => &[Do, Test, Provide],
            Source::Delivery => &[Review, Know],
            Source::Hold => &[Authorize, Review, Decide, Provide],
            Source::Problem => &[Problem],
        }
    }
    fn command(self) -> &'static str {
        match self {
            Source::Information => "publish_information",
            Source::Question => "ask",
            Source::Action => "request_action",
            Source::Delivery => "present_result",
            Source::Hold => "a held hook",
            Source::Problem => "raise_problem",
        }
    }
}
pub const REQUEST_TITLE_LIMIT: usize = 60;
pub const POST_MERGE_WORK_MAX: usize = 120;
/// How the orchestrator settles a node's suggestion: the only answers a Propose card offers.
pub const SUGGESTION_ANSWERS: [(&str, &str, &str); 3] = [
    ("yours", "It's part of your plan: do it in this task", "The node does it as part of the task it is working on."),
    ("discarded", "Discarded", "An open task already covers it, or the user already turned it down: the reason says which."),
    ("new_task", "It becomes a new task", "The orchestrator created a task for it, with its own plan."),
];
/// What a node is told when its question to the orchestrator is withdrawn because nobody answered it.
pub fn unanswered_withdrawn_notice(need: &str) -> String {
    format!("Your question to the orchestrator \"{need}\" got no answer and is withdrawn. Carry on without it if you can; if you cannot, ask the user with ask.")
}
/// A node's suggestions still waiting for the orchestrator, per task.
pub const OPEN_SUGGESTIONS_MAX: usize = 5;
pub fn suggestion_options() -> Vec<QuestionOption> {
    SUGGESTION_ANSWERS.iter().map(|(id, label, description)| QuestionOption {
        id: (*id).into(),
        label: (*label).into(),
        description: Some((*description).into()),
    }).collect()
}
/// The notice that a task ended with a card still waiting for the user: this prefix and the card's id.
pub const ENDED_UNANSWERED_PREFIX: &str = "ended-unanswered-";
fn kind_name(kind: CardKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}
/// R18–R21: the card's own shape, before any state is looked at.
fn validate_card(card: &CardSpec, options: &[QuestionOption], source: Source, blocking: bool) -> Result<()> {
    let kind = kind_name(card.kind);
    if card.kind == CardKind::Problem && source != Source::Problem {
        return refuse("forbidden", "card.kind", "Only Alondra and the orchestrator raise a problem: describe it with ask instead");
    }
    match &card.cause {
        Some(_) if card.kind != CardKind::Problem => {
            return refuse("invalid_input", "card.cause", "Only a problem names its cause: remove cause, or use replaces to replace your own open card");
        }
        Some(cause) if cause.trim().is_empty() || cause.chars().count() > CardSlot::CAUSE_MAX => {
            return refuse("invalid_input", "card.cause", &format!("Name what failed in a short key of at most {} characters, the same each time it fails", CardSlot::CAUSE_MAX));
        }
        _ => {}
    }
    if !source.kinds().contains(&card.kind) {
        let allowed: Vec<String> = source.kinds().iter().map(|k| kind_name(*k)).collect();
        return refuse("invalid_input", "card.kind", &format!("{} does not send a {kind} card: use one of {}", source.command(), allowed.join(", ")));
    }
    let need = card.need.trim();
    if need.is_empty() || need.contains('\n') {
        return refuse("invalid_input", "card.need", "Say in one line, in the imperative, what you need from the user");
    }
    if need.chars().count() > CardSpec::NEED_MAX {
        return refuse("invalid_input", "card.need", &format!("At most {} characters: keep what you need short and move the rest to card.why or card.content", CardSpec::NEED_MAX));
    }
    if card.why.chars().count() > CardSpec::WHY_MAX {
        return refuse("invalid_input", "card.why", &format!("At most {} characters: long text goes in card.content.body, never in why", CardSpec::WHY_MAX));
    }
    if card.kind.needs_why() && source != Source::Hold && card.why.trim().is_empty() {
        return refuse("invalid_input", "card.why", &format!("A {kind} card says why: add one or two sentences, and whom it holds back"));
    }
    if card.kind == CardKind::Review && card.content.is_none() {
        return refuse("invalid_input", "card.content", "A review card carries what to review: add card.content with its kind and body");
    }
    if let Some(summary) = card.content.as_ref().and_then(|c| c.summary.as_ref())
        && summary.chars().count() > CardSpec::SUMMARY_MAX
    {
        return refuse("invalid_input", "card.content.summary", &format!("At most {} characters, like \"6 files · +120 −40\"; leave it out and Alondra computes one", CardSpec::SUMMARY_MAX));
    }
    match (&card.verify, card.kind) {
        (None, CardKind::Do) => return refuse("invalid_input", "card.verify", "A do card says how you will check the user did it"),
        (Some(_), k) if k != CardKind::Do => return refuse("invalid_input", "card.verify", "Only a do card carries verify: remove it"),
        (Some(v), _) if v.trim().is_empty() => return refuse("invalid_input", "card.verify", "Say how you will check the user did it"),
        _ => {}
    }
    if let Some(recommended) = &card.recommended
        && !options.iter().any(|o| &o.id == recommended)
    {
        return refuse("invalid_input", "card.recommended", "Recommend the id of one of the card's options, or leave it out");
    }
    if card.kind == CardKind::Decide && source != Source::Hold {
        decide_options(options)?;
        let body = card.content.as_ref().map_or("", |content| content.body.as_str());
        if let Some((code, fix)) = cited_files_problem(&format!("{}\n{}\n{body}", card.need, card.why)) {
            return refuse(code, "card.content.body", &fix);
        }
    }
    if blocking && card.kind.never_blocks() {
        return refuse("invalid_input", "blocking", &format!("A {kind} card never blocks: send it with blocking false"));
    }
    if card.blocks.len() > 16 {
        return refuse("invalid_input", "card.blocks", "At most 16 targets");
    }
    Ok(())
}
/// The cards of several questions asked together: 2 to 4 that each stand on their own.
fn group_cards(group: &InteractionId, why: &str, questions: &[GroupQuestion]) -> Result<Vec<(InteractionId, CardSpec, Vec<QuestionOption>)>> {
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
        card.recommended = question.recommended.clone();
        card.group = Some(QuestionGroup { id: group.0.clone(), index: (at + 1) as u8, total: questions.len() as u8 });
        Ok((QuestionGroup::card_id(group, at + 1), card, question.options.clone()))
    }).collect()
}
/// R19: a decision offers 2–8 short distinct options, at most one of them recommended.
fn decide_options(options: &[QuestionOption]) -> Result<()> {
    if !(2..=8).contains(&options.len()) {
        return refuse("invalid_input", "options", "A decision offers 2 to 8 options: merge or drop some, or ask for text with a provide card");
    }
    let mut labels = BTreeSet::new();
    for option in options {
        if option.label.chars().count() > 40 {
            return refuse("invalid_input", "options.label", &format!("\"{}\" is longer than 40 characters: shorten it and explain it in its description", option.label));
        }
        if !labels.insert(option.label.trim().to_lowercase()) {
            return refuse("invalid_input", "options.label", &format!("\"{}\" appears twice: every option says something different", option.label));
        }
    }
    if options.iter().filter(|o| o.label.trim_end().ends_with("(Recommended)")).count() > 1 {
        return refuse("invalid_input", "card.recommended", "Recommend at most one option, with card.recommended");
    }
    Ok(())
}
fn nonempty(s: &str) -> Result<()> {
    if s.trim().is_empty() || s.len() > 1_048_576 {
        fail("invalid_input", "Empty or oversized value")
    } else {
        Ok(())
    }
}
fn id(s: &str) -> Result<()> {
    nonempty(s)?;
    if s.len() > 256 || s.chars().any(char::is_control) {
        fail("invalid_input", "Invalid identifier")
    } else {
        Ok(())
    }
}
fn digest(s: &str) -> Result<()> {
    if s.len() != 64
        || !s
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        fail("invalid_input", "Content digest must be lowercase SHA-256")
    } else {
        Ok(())
    }
}
fn head(s: &str) -> Result<()> {
    nonempty(s)?;
    if s.len() > 256 || s.chars().any(char::is_whitespace) {
        fail("invalid_input", "Invalid head")
    } else {
        Ok(())
    }
}
/// Who reads a notice for the orchestrator: Alondra, once it handed the notice over.
pub const ORCHESTRATOR_READER: &str = "alondra-orchestrator";

fn user(actor: &Actor) -> Result<String> {
    match actor {
        Actor::User { user_id } => {
            id(user_id)?;
            Ok(user_id.clone())
        }
        _ => fail("forbidden", "User decision required"),
    }
}
fn system(actor: &Actor) -> Result<()> {
    if matches!(actor, Actor::System) {
        Ok(())
    } else {
        fail("forbidden", "Trusted observation required")
    }
}
/// What a node that tries to merge or complete its own Task is told.
pub const ALONDRA_MERGES: &str = "Alondra merges pull requests and completes tasks itself: deliver with present_result. Once it is merged Alondra asks you to clean up; if the merge fails, it hands the task back with the reason.";
const MERGE_FAILED: &str = "Alondra could not merge pull request #";
const MERGE_FAILED_FIX: &str = ". Fix what stops the merge";
/// What a node is told when Alondra cannot merge its PR, which the task also shows.
pub fn merge_failed_text(number: u64, url: &str, base: &str, reason: &str) -> String {
    format!(
        "{MERGE_FAILED}{number} ({url}): {reason}{MERGE_FAILED_FIX} (conflicts with {base}, failing checks, or a review GitHub \
         requires), push, and present the result again with present_result and the new head. If only the user can unblock it, ask them.",
        reason = reason.trim().trim_end_matches('.'),
    )
}
/// The PR number and GitHub's reason in a hand-back `merge_failed_text` wrote.
pub fn merge_failure(body: &str) -> Option<(u64, &str)> {
    let rest = body.strip_prefix(MERGE_FAILED)?;
    let (number, rest) = rest.split_once(' ')?;
    let reason = rest.split_once("): ")?.1;
    Some((number.parse().ok()?, reason.split_once(MERGE_FAILED_FIX).map_or(reason, |(reason, _)| reason)))
}
/// What a node is told when its task is finished and Alondra is about to close its session.
pub fn cleanup_request(title: &str, task: &TaskId) -> String {
    format!("Your task “{title}” is finished and is closing. Before Alondra closes this session and \
deletes its worktree, remove what you generated for this task outside the worktree that would \
otherwise stay on this Mac (build caches, temporary files), with the project's own tools (its clean \
commands, scripts or build targets). Do not delete files or links inside the worktree: Alondra \
deletes the whole worktree. If nothing was generated outside it, delete nothing. Do not change, \
commit or push anything else. Then call cleanup_done with task_id {task}. If you do \
not, Alondra closes the session after a while anyway. If something you started for this task is \
still running after the merge (a deploy, a release script), call post_merge_work with task_id {task} \
and what it is first: Alondra keeps the session open until you call it again without reason or call \
cleanup_done.", task = task.0)
}
/// What a node that declared work after the merge before delivering is told once its delivery
/// settles: the task stays in progress until that work ends.
pub fn post_merge_reminder(title: &str, task: &TaskId, reason: &str) -> String {
    format!("Your task “{title}” has its delivery accepted and merged, and you said work remains after the \
merge: {reason}. Do it now: the task stays in progress until you say it ended. When it ends, call \
post_merge_work with task_id {task} without reason, then remove what you generated for this task outside \
the worktree (build caches, temporary files) with the project's own tools, never files or links inside the \
worktree, and call cleanup_done with task_id {task}. If it cannot be done, say so with ask.", task = task.0)
}
/// What a node that waited on other Tasks is told when they are done.
pub fn dependencies_done_notice(titles: &[&str]) -> String {
    let names: Vec<String> = titles.iter().map(|title| format!("“{title}”")).collect();
    format!("What you were waiting for is done: {}. Go on with your task; update your branch from the default branch \
first if you need what it changed.", names.join(", "))
}
pub fn delivery_withdrawn_for_questions() -> String {
    "You asked the user something after your final delivery, so that delivery was withdrawn and the task \
is in progress again. When the answers arrive, present your result again as it is if nothing changes, or \
update the work first and then present it.".into()
}
pub fn delivery_withdrawn_behind_pr(number: u64) -> String {
    format!("Pull request #{number} has commits your final delivery does not include, so that delivery was withdrawn \
and the task is in progress again. When the work is ready, present your result again with the pull request at its new head.")
}
pub fn cancelled_notice(title: &str) -> String {
    format!("Your task “{title}” was cancelled. Stop working on it now and do not start anything new \
for it: do not commit, push, register a pull request or deliver anything. Before you end your \
turn, delete what you generated for it that git does not track (build outputs, caches, temporary \
files) with the project's own tools. Alondra then closes this session.")
}
fn one_pr_per_task(number: u64) -> String {
    format!("This task already has PR #{number}, and a task owns one PR: nothing was registered. Put more work for this task on PR #{number}; for work that needs another PR, ask the orchestrator for a new task (ask with recipient orchestrator).")
}
pub fn merged_notice(number: u64, left: usize) -> String {
    match left {
        0 => format!("Pull request #{number} is merged; no pull request of this task is left to merge."),
        n => format!("Pull request #{number} is merged; {n} left to merge."),
    }
}
impl DomainState {
    /// The PRs of the Task's final delivery that are not merged yet.
    pub fn unmerged_targets(&self, task: &TaskId) -> usize {
        let Some(InteractionPayload::Delivery { targets, .. }) = self.tasks.get(task)
            .and_then(|t| t.final_presentation.as_ref())
            .and_then(|p| self.interactions.get(p))
            .map(|p| &p.payload)
        else {
            return 0;
        };
        targets.iter().filter(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. }
            if self.pull_requests.get(pr_id).is_some_and(|pr| pr.state != PrState::Merged))).count()
    }
    fn fence(&self, actor: &Actor) -> Result<()> {
        match actor {
            Actor::Orchestrator { generation } if *generation != self.orchestrator_generation => {
                fail("stale_generation", "Orchestrator generation changed")
            }
            Actor::User { user_id } => id(user_id),
            Actor::Agent { agent_id } => id(&agent_id.0),
            _ => Ok(()),
        }
    }
    fn manager(&self, actor: &Actor) -> Result<()> {
        self.fence(actor)?;
        if matches!(actor, Actor::User { .. } | Actor::Orchestrator { .. }) {
            Ok(())
        } else {
            fail("forbidden", "User or current orchestrator required")
        }
    }
    fn task(&self, task: &TaskId) -> Result<&Task> {
        self.tasks
            .get(task)
            .ok_or_else(|| DomainError::new("not_found", "Task does not exist"))
    }
    fn active(&self, task: &TaskId) -> Result<&Task> {
        let t = self.task(task)?;
        if t.state.terminal() {
            fail("task_not_active", "Task is terminal")
        } else {
            Ok(t)
        }
    }
    /// A task that ends takes no node and starts no work: it closes, and then it can be reopened.
    fn not_ending(&self, task: &TaskId) -> Result<()> {
        match self.task(task)?.ending {
            Some(TaskEnding::Cancelling) => fail("task_ending", &format!("Task {task} is being cancelled: once it is closed, reopen it")),
            Some(TaskEnding::Completing) => fail("task_ending", &format!("Task {task} is closing as done: once it is closed, reopen it")),
            None => Ok(()),
        }
    }
    /// An Agent works on one Task at a time: the other non-terminal Task it holds, if any.
    pub fn busy_with(&self, agent: &AgentId, except: &TaskId) -> Option<&Task> {
        self.tasks.values().find(|t| &t.id != except && t.agent_id.as_ref() == Some(agent) && !t.over())
    }
    /// A node works on one Task for its whole life: the other Task, open or not, it holds or held.
    fn used_by_other(&self, agent: &AgentId, except: &TaskId) -> Result<()> {
        match self.tasks.values().find(|t| &t.id != except && t.agent_id.as_ref() == Some(agent)) {
            Some(other) => fail("agent_used", &format!(
                "Node {} already worked on task “{}”, and a node works on one task only: launch a new node for this task with launch.",
                self.agents.get(agent).map_or(agent.0.as_str(), |a| a.name.as_str()), other.title)),
            None => Ok(()),
        }
    }
    fn free(&self, agent: &AgentId, task: &TaskId) -> Result<()> {
        match self.busy_with(agent, task) {
            Some(other) => fail("agent_busy", &format!("Agent {} already works on Task {}: launch a new node for this Task", agent.0, other.id.0)),
            None => Ok(()),
        }
    }
    fn assigned(&self, actor: &Actor, task: &TaskId) -> Result<&Task> {
        let t = self.active(task)?;
        match actor {
            Actor::Agent { agent_id } if t.agent_id.as_ref() == Some(agent_id) => Ok(t),
            _ => fail("not_assigned", "Command requires the assigned Agent"),
        }
    }
    fn task_editor(&self, actor: &Actor, task: &TaskId) -> Result<()> {
        self.active(task)?;
        if matches!(actor, Actor::Agent { .. }) {
            self.assigned(actor, task)?;
        } else {
            self.manager(actor)?;
        }
        Ok(())
    }
    fn validate_scope(&self, scope: &Scope) -> Result<()> {
        match scope {
            Scope::Task { task_id } => {
                self.task(task_id)?;
            }
            Scope::Orchestrator {
                request_id: Some(r),
            } => {
                if !self.requests.contains_key(r) {
                    return fail("not_found", "Request does not exist");
                }
            }
            _ => {}
        }
        Ok(())
    }
    fn recipient(&self, to: &Recipient) -> Result<()> {
        if let Recipient::Agent { agent_id } = to {
            if !self.agents.contains_key(agent_id) {
                return fail("not_found", "Recipient Agent does not exist");
            }
        }
        Ok(())
    }
    fn interaction(&self, i: &InteractionId) -> Result<&Interaction> {
        self.interactions
            .get(i)
            .ok_or_else(|| DomainError::new("not_found", "Interaction does not exist"))
    }
    fn presentation(
        &self,
        i: &InteractionId,
    ) -> Result<(&Interaction, &Task, &Vec<DeliveryTarget>)> {
        let p = self.interaction(i)?;
        let Scope::Task { task_id } = &p.scope else {
            return fail("invalid_action_for_kind", "Delivery must have Task scope");
        };
        let t = self.active(task_id)?;
        let InteractionPayload::Delivery { targets, .. } = &p.payload else {
            return fail(
                "invalid_action_for_kind",
                "A delivery presentation is required",
            );
        };
        if t.final_presentation.as_ref() != Some(i) {
            return fail("acceptance_stale", "Presentation is not the current final");
        };
        self.targets(task_id, targets)?;
        Ok((p, t, targets))
    }
    /// A final delivery of a task with an approved plan marks every point of it done, each with
    /// what proves it; a task with no approved plan, or a result that is not final, carries none.
    fn checklist_marks_cover_the_plan(&self, task_id: &TaskId, is_final: bool, marks: &[ChecklistMark], attachments: &[AttachmentRef]) -> Result<()> {
        let plan = self.checklists.get(task_id);
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
                    EvidenceKind::Pr if self.task(task_id)?.pr_id.as_ref().is_none_or(|pr| pr.0 != value) => {
                        return refuse("evidence_unknown_pr", "checklist", &format!("point {point}: {value} is not this task's registered PR: cite its pr_id"));
                    }
                    _ => {}
                }
            }
        }
        let missing: Vec<String> = plan.items.iter()
            .filter(|item| !marked.contains(item.id.as_str()))
            .map(|item| format!("{} “{}”", item.id, item.text))
            .collect();
        if !missing.is_empty() {
            return refuse("checklist_incomplete", "checklist", &format!(
                "the approved plan has {} points and these are not marked done: {}. Finish them and add each to checklist with its evidence ({kinds}). If a point no longer applies, send a revised plan with ExitPlanMode; once the user approves it, it replaces this checklist",
                plan.items.len(), missing.join(", ")));
        }
        Ok(())
    }
    fn targets(&self, task: &TaskId, targets: &[DeliveryTarget]) -> Result<()> {
        let t = self.task(task)?;
        if targets.is_empty() || targets.len() > 64 {
            return fail(
                "invalid_input",
                "Delivery requires a bounded nonempty result set",
            );
        };
        let mut seen = BTreeSet::new();
        for target in targets {
            let key = serde_json::to_string(target)
                .map_err(|_| DomainError::new("invalid_input", "Invalid target"))?;
            if !seen.insert(key) {
                return fail("invalid_input", "Duplicate delivery target");
            };
            match target {
                DeliveryTarget::Text { text } => nonempty(text)?,
                DeliveryTarget::DocumentRevision {
                    document_id,
                    revision_id,
                } => {
                    if !t.documents.contains_key(document_id) {
                        return fail("forbidden", "Document is not linked to this Task");
                    };
                    let d = self
                        .documents
                        .get(document_id)
                        .ok_or_else(|| DomainError::new("not_found", "Document missing"))?;
                    let r = self
                        .document_revisions
                        .get(revision_id)
                        .ok_or_else(|| DomainError::new("not_found", "Revision missing"))?;
                    if &r.document_id != document_id {
                        return fail("invalid_input", "Revision belongs to another document");
                    };
                    if &d.current_revision != revision_id {
                        return fail("acceptance_stale", "Document has a newer revision");
                    }
                }
                DeliveryTarget::PullRequest {
                    pr_id,
                    head: expected,
                } => {
                    let p = self
                        .pull_requests
                        .get(pr_id)
                        .ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
                    if &p.task_id != task || t.pr_id.as_ref() != Some(pr_id) {
                        return fail("forbidden", "PR belongs to another Task");
                    };
                    if &p.head != expected {
                        return fail("stale_head", "PR head changed").map_err(|e| e.because(Refusal::ResultBehindPr { number: p.number }));
                    }
                }
            }
        }
        Ok(())
    }
    /// Puts a card in front of its recipient after every card rule holds. `Some(existing)` is a
    /// notice already waiting unread with the same words (R8): nothing new is created.
    #[allow(clippy::too_many_arguments)]
    fn insert_interaction(
        &mut self,
        actor: &Actor,
        i: &InteractionId,
        scope: &Scope,
        to: &Recipient,
        body: &str,
        blocking: bool,
        attachments: &[AttachmentRef],
        payload: InteractionPayload,
        card: &CardSpec,
        source: Source,
    ) -> Result<Option<InteractionId>> {
        id(&i.0)?;
        content(body, attachments)?;
        if self.interactions.contains_key(i) {
            return fail("invalid_input", "Interaction ID already exists");
        };
        let options = match &payload {
            InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. } => options.as_slice(),
            _ => &[],
        };
        validate_card(card, options, source, blocking)?;
        let author = match source {
            Source::Hold => Author::Agent { agent_id: match actor {
                Actor::Agent { agent_id } => agent_id.clone(),
                _ => return fail("forbidden", "Only an Agent's hook can be held"),
            } },
            _ => self.card_author(actor, scope, source)?,
        };
        self.recipient(to)?;
        if matches!(author, Author::Agent { .. }) && matches!(source, Source::Question | Source::Action) {
            usage_is_not_a_stop(card, options)?;
        }
        if card.kind == CardKind::Propose {
            self.suggestion(&author, scope, to, options)?;
        }
        if let Some(existing) = self.repeated_notice(&author, scope, to, body, card, source) {
            return Ok(Some(existing));
        }
        let task = match scope {
            Scope::Task { task_id } => Some(task_id.clone()),
            Scope::Orchestrator { .. } => None,
        };
        if source != Source::Hold {
            self.clarification_answered(&author, task.as_ref(), card)?;
            self.one_blocking_ask(&author, card, blocking)?;
        }
        self.card_blocks(task.as_ref(), card)?;
        if let Some(old) = &card.replaces {
            self.replaceable(&author, old)?;
            self.retire(old);
        }
        if let Some(slot) = CardSlot::of(card, &payload) {
            for old in self.slot_holders(scope, &author, &slot) {
                self.retire(&old);
            }
        }
        self.interactions.insert(
            i.clone(),
            Interaction {
                attachments: attachments.to_vec(),
                id: i.clone(),
                scope: scope.clone(),
                author,
                recipient: to.clone(),
                body: body.into(),
                blocking,
                subject: None,
                payload,
                revision: self.revision + 1,
                order: self.revision + 1,
                withdrawn: false,
                card: card.clone(),
                clarification: None,
            },
        );
        self.blocks_acyclic(i)?;
        Ok(None)
    }
    /// A suggestion is something a node saw outside its task: it goes to the orchestrator, never
    /// to the user, carries the orchestrator's fixed answers, and a task keeps only a few waiting.
    fn suggestion(&self, author: &Author, scope: &Scope, to: &Recipient, options: &[QuestionOption]) -> Result<()> {
        let (Author::Agent { .. }, Scope::Task { task_id }, Recipient::Orchestrator) = (author, scope, to) else {
            return refuse("forbidden", "card.kind", "Only a node suggests, to the orchestrator and within its own task: use suggest");
        };
        if options != suggestion_options().as_slice() {
            return refuse("invalid_input", "options", "A suggestion carries the orchestrator's fixed answers: send it with suggest");
        }
        let waiting = self.interactions.values().filter(|i| {
            i.card.kind == CardKind::Propose && i.scope == *scope && !i.withdrawn
                && matches!(i.payload, InteractionPayload::Question { answer: None, .. })
        }).count();
        if waiting >= OPEN_SUGGESTIONS_MAX {
            return refuse("too_many_suggestions", "scope.task_id", &format!(
                "Task {task_id} already has {OPEN_SUGGESTIONS_MAX} suggestions waiting for the orchestrator: wait for its answers, or withdraw one you no longer need"));
        }
        Ok(())
    }
    /// A node's question to the orchestrator that waits for its answer; a suggestion never waits.
    pub fn asks_the_orchestrator(i: &Interaction) -> bool {
        matches!(i.author, Author::Agent { .. }) && i.recipient == Recipient::Orchestrator && i.card.kind != CardKind::Propose
            && matches!(i.scope, Scope::Task { .. })
            && matches!(i.payload, InteractionPayload::Question { .. } | InteractionPayload::ActionRequest { .. })
    }
    /// The task's node waits for the orchestrator's answer to one of its questions.
    pub fn waits_for_orchestrator(&self, task: &TaskId) -> bool {
        self.interactions.values().any(|i| Self::asks_the_orchestrator(i) && self.open(i)
            && matches!(&i.scope, Scope::Task { task_id } if task_id == task))
    }
    /// The orchestrator settles a node's suggestion with one decision, in one command: a new task
    /// is created here, under a request of its own, or nothing is recorded at all.
    fn settle_suggestion(&mut self, actor: &Actor, interaction_id: &InteractionId, decision: &SuggestionDecision,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.fence(actor)?;
        if !matches!(actor, Actor::Orchestrator { .. }) {
            return fail("forbidden", "Only the orchestrator settles a suggestion");
        }
        let i = self.interaction(interaction_id)?.clone();
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
                self.apply(actor, &Command::CreateRequest {
                    request_id: request_id.clone(), body: format!("{}\n\n{}", i.card.need, i.body),
                    auto_merge: false, maintenance: false, message: true,
                }, effects, created, lifecycles)?;
                self.apply(actor, &Command::TitleRequest { request_id: request_id.clone(), title: title.clone() }, effects, created, lifecycles)?;
                self.apply(actor, &Command::CreateTask { plan_items: None,
                    task_id: new_task.clone(), request_id: request_id.clone(), title: title.clone(), agent_id: None,
                    review_prs: false, dependencies: dependencies.clone(), stack: None, launch: Some(launch.clone()),
                }, effects, created, lifecycles)?;
            }
            SuggestionDecision::CurrentTask { note } => {
                nonempty(note)?;
                if self.task(task_id)?.over() {
                    return refuse("task_not_active", "decision.kind",
                        "The task that suggested it has ended, so it cannot do it: create a new task or discard it");
                }
            }
            SuggestionDecision::Discard { reason } => nonempty(reason)?,
        }
        let inputs = decision.note().map(|note| BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.to_owned()))])).unwrap_or_default();
        let at = self.revision + 1;
        let i = self.interactions.get_mut(interaction_id).unwrap();
        if let InteractionPayload::Question { answer, .. } = &mut i.payload {
            *answer = Some(Answer {
                attachments: vec![], option_id: decision.option_id().into(), inputs,
                responder: "alondra-orchestrator".into(), at_revision: at, decision: Some(decision.clone()),
            });
        }
        i.revision = at;
        self.notify(task_id, Some(interaction_id.clone()), "Suggestion settled", effects)
    }
    /// R1, R2, R4: who may put a card in front of someone, and on which Task.
    fn card_author(&self, actor: &Actor, scope: &Scope, source: Source) -> Result<Author> {
        self.fence(actor)?;
        self.validate_scope(scope)?;
        let notice = source == Source::Information;
        match actor {
            Actor::Agent { agent_id } => {
                let holds = |t: &Task| t.agent_id.as_ref() == Some(agent_id);
                let working = self.tasks.values().any(|t| holds(t) && !t.over());
                let owned = match scope {
                    Scope::Task { task_id } => Some(self.task(task_id)?).filter(|t| holds(t)),
                    Scope::Orchestrator { .. } => None,
                };
                let winding_down_question = |t: &Task| Self::winding_down(t) && matches!(source, Source::Question | Source::Action | Source::Hold);
                match owned {
                    Some(t) if t.over() && !notice && !winding_down_question(t) => refuse(
                        "task_not_active",
                        "scope.task_id",
                        &format!("Task {} is {}: it accepts only a closing notice. Ask the orchestrator for a new Task", t.id, if t.archived { "archived" } else if t.cancelled() { "cancelled" } else { "done" }),
                    ),
                    Some(_) => Ok(Author::Agent { agent_id: agent_id.clone() }),
                    None if !working && notice && matches!(scope, Scope::Orchestrator { .. }) => {
                        Ok(Author::Agent { agent_id: agent_id.clone() })
                    }
                    None if !working => refuse(
                        "no_task",
                        "scope",
                        "This node has no Task: it sends no cards. Ask the orchestrator to assign you a Task first",
                    ),
                    None => refuse(
                        "not_assigned",
                        "scope.task_id",
                        "Only the node assigned to a Task sends cards for it: use your own Task's id",
                    ),
                }
            }
            Actor::Orchestrator { generation } => {
                if let Scope::Task { task_id } = scope
                    && !notice
                {
                    self.active(task_id)?;
                }
                Ok(Author::Orchestrator { generation: *generation })
            }
            Actor::System if matches!(source, Source::Problem | Source::Information | Source::Question) => Ok(Author::System),
            _ => fail("forbidden", "Only assigned Agent or current orchestrator may communicate"),
        }
    }
    /// A card stays open until it is answered, read, accepted, replaced or withdrawn.
    pub fn open(&self, i: &Interaction) -> bool {
        if i.withdrawn {
            return false;
        }
        match &i.payload {
            InteractionPayload::Information { read_by } => read_by.is_empty(),
            InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. } => answer.is_none(),
            InteractionPayload::Delivery { acceptance, is_final, .. } => {
                !acceptance.as_ref().is_some_and(|a| a.valid)
                    && (!is_final
                        || matches!(&i.scope, Scope::Task { task_id }
                            if self.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(&i.id))))
            }
        }
    }
    pub(super) fn retire(&mut self, id: &InteractionId) {
        if let Some(i) = self.interactions.get_mut(id) {
            i.withdrawn = true;
            i.revision = self.revision + 1;
        }
    }
    /// The questions asked together with `i`, in the order they were asked; just `i` for a question
    /// asked alone.
    pub fn group_members<'a>(&'a self, i: &'a Interaction) -> Vec<&'a Interaction> {
        let Some(group) = &i.card.group else { return vec![i] };
        let mut members: Vec<&Interaction> = self.interactions.values()
            .filter(|other| other.author == i.author && other.card.group.as_ref().is_some_and(|g| g.id == group.id))
            .collect();
        members.sort_by_key(|other| other.card.group.as_ref().map_or(0, |g| g.index));
        members
    }
    pub(super) fn answered_question(i: &Interaction) -> bool {
        matches!(&i.payload, InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. })
    }
    /// Every question of the group has its answer: the author is given them all together.
    pub fn group_complete(&self, i: &Interaction) -> bool {
        self.group_members(i).iter().all(|member| !member.withdrawn && Self::answered_question(member))
    }
    /// R8: the same notice, still unread, from the same author to the same place.
    fn repeated_notice(&self, author: &Author, scope: &Scope, to: &Recipient, body: &str, card: &CardSpec, source: Source) -> Option<InteractionId> {
        if source != Source::Information {
            return None;
        }
        self.interactions.values()
            .filter(|i| &i.author == author && &i.scope == scope && &i.recipient == to && self.open(i))
            .filter(|i| matches!(i.payload, InteractionPayload::Information { .. }))
            .max_by_key(|i| i.order)
            .filter(|i| i.body == body && i.card.need == card.need)
            .map(|i| i.id.clone())
    }
    /// R7: after the user asked for a clarification, the author's next card replaces that card.
    /// Plain text (a notice) is still allowed.
    fn clarification_answered(&self, author: &Author, task: Option<&TaskId>, card: &CardSpec) -> Result<()> {
        if card.kind == CardKind::Know {
            return Ok(());
        }
        let pending = self.interactions.values().find(|i| {
            &i.author == author
                && i.clarification.is_some()
                && self.open(i)
                && match (&i.scope, task) {
                    (Scope::Task { task_id }, Some(t)) => task_id == t,
                    (Scope::Orchestrator { .. }, None) => true,
                    _ => false,
                }
        });
        match pending {
            Some(x) if card.replaces.as_ref() != Some(&x.id) => refuse(
                "clarification_pending",
                "card.replaces",
                &format!("The user asked you to clarify card {}: send its corrected version with card.replaces = \"{}\", or withdraw it", x.id, x.id),
            ),
            _ => Ok(()),
        }
    }
    /// R5: one blocking Decide, Do or Provide per node at a time.
    fn one_blocking_ask(&self, author: &Author, card: &CardSpec, blocking: bool) -> Result<()> {
        if !blocking || !card.kind.one_at_a_time() || !matches!(author, Author::Agent { .. }) {
            return Ok(());
        }
        let open = self.interactions.values().find(|i| {
            &i.author == author && i.blocking && i.card.kind.one_at_a_time() && self.open(i)
                && card.replaces.as_ref() != Some(&i.id)
                && !(card.group.is_some() && card.group.as_ref().map(|g| &g.id) == i.card.group.as_ref().map(|g| &g.id))
        });
        match open {
            Some(x) => refuse(
                "ask_already_open",
                "blocking",
                &format!("Card {} already waits on the user: replace it with card.replaces = \"{}\" (one form holds up to 4 questions), withdraw it, or send this one with blocking false", x.id, x.id),
            ),
            None => Ok(()),
        }
    }
    /// R6: only an open card of the same author is replaced.
    fn replaceable(&self, author: &Author, old: &InteractionId) -> Result<()> {
        let Some(x) = self.interactions.get(old) else {
            return refuse("not_found", "card.replaces", &format!("Card {old} does not exist: replace one of your open cards"));
        };
        if &x.author != author {
            return refuse("forbidden", "card.replaces", &format!("Card {old} is not yours: replace only your own cards"));
        }
        if self.holds.contains_key(old) {
            return refuse("forbidden", "card.replaces", &format!("Card {old} is held by a Claude Code hook: it ends with its hook"));
        }
        if matches!(&x.payload, InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. }) {
            return refuse("already_answered", "card.replaces", &format!("Card {old} is already answered: send a new card without card.replaces"));
        }
        if !self.open(x) {
            return refuse("already_withdrawn", "card.replaces", &format!("Card {old} is no longer open: send a new card without card.replaces"));
        }
        Ok(())
    }
    /// R14: the open cards in `slot`, which a new card in it replaces. On a Task the slot is the
    /// Task's, whichever node wrote them; elsewhere it is the author's.
    fn slot_holders(&self, scope: &Scope, author: &Author, slot: &CardSlot) -> Vec<InteractionId> {
        self.interactions.values()
            .filter(|i| self.open(i) && CardSlot::of(&i.card, &i.payload).as_ref() == Some(slot))
            .filter(|i| match (scope, &i.scope) {
                (Scope::Task { task_id }, Scope::Task { task_id: other }) => task_id == other,
                (Scope::Orchestrator { .. }, Scope::Orchestrator { .. }) => &i.author == author,
                _ => false,
            })
            .map(|i| i.id.clone())
            .collect()
    }
    /// The open cards a reader may replace: the node's on its Task, or the orchestrator's own,
    /// oldest first.
    pub fn open_cards(&self, reader: &Actor, task: Option<&TaskId>) -> Vec<&Interaction> {
        let mut cards: Vec<&Interaction> = self.interactions.values()
            .filter(|i| self.open(i))
            .filter(|i| match reader {
                Actor::Agent { agent_id } => matches!(&i.author, Author::Agent { agent_id: a } if a == agent_id),
                Actor::Orchestrator { .. } => matches!(i.author, Author::Orchestrator { .. }),
                _ => false,
            })
            .filter(|i| task.is_none_or(|t| matches!(&i.scope, Scope::Task { task_id } if task_id == t)))
            .collect();
        cards.sort_by_key(|i| i.order);
        cards
    }
    /// R15: a card holds back only an open PR, document or delivery of its Task or of a Task it
    /// depends on.
    fn card_blocks(&self, task: Option<&TaskId>, card: &CardSpec) -> Result<()> {
        if card.blocks.is_empty() {
            return Ok(());
        }
        let Some(task) = task.and_then(|t| self.tasks.get(t)) else {
            return refuse("unknown_target", "card.blocks", "Only a card of a Task can block something: remove card.blocks");
        };
        let related: BTreeSet<&TaskId> = std::iter::once(&task.id)
            .chain(&task.dependencies)
            .chain(task.stack.iter().map(|s| &s.upstream_task))
            .collect();
        for target in &card.blocks {
            let ok = match target {
                CardTarget::PullRequest { pr_id } => self.pull_requests.get(pr_id)
                    .is_some_and(|p| p.state == PrState::Open && related.contains(&p.task_id)),
                CardTarget::Document { document_id } => related.iter()
                    .any(|t| self.tasks.get(*t).is_some_and(|t| t.documents.contains_key(document_id))),
                CardTarget::Delivery { interaction_id } => self.interactions.get(interaction_id).is_some_and(|i| {
                    matches!(i.payload, InteractionPayload::Delivery { .. })
                        && matches!(&i.scope, Scope::Task { task_id } if related.contains(task_id))
                        && self.open(i)
                }),
            };
            if !ok {
                return refuse(
                    "unknown_target",
                    "card.blocks",
                    &format!("{} is not an open PR, document or delivery of this Task or of one it depends on: point card.blocks at one, or remove it", serde_json::to_string(target).unwrap_or_default()),
                );
            }
        }
        Ok(())
    }
    fn card_target_matches_delivery(target: &CardTarget, delivery: &Interaction) -> bool {
        let InteractionPayload::Delivery { targets, .. } = &delivery.payload else { return false };
        match target {
            CardTarget::Delivery { interaction_id } => interaction_id == &delivery.id,
            CardTarget::PullRequest { pr_id } => targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id: known, .. } if known == pr_id)),
            CardTarget::Document { document_id } => targets.iter().any(|target| matches!(target, DeliveryTarget::DocumentRevision { document_id: known, .. } if known == document_id)),
        }
    }
    /// What an open card holds back, as the deliveries it stands in front of.
    pub(crate) fn blocked_deliveries(&self, i: &Interaction) -> Vec<InteractionId> {
        if !self.open(i) {
            return vec![];
        }
        let mut out = vec![];
        for target in &i.card.blocks {
            for d in self.interactions.values().filter(|d| self.open(d)) {
                if d.id != i.id && Self::card_target_matches_delivery(target, d) {
                    out.push(d.id.clone());
                }
            }
        }
        out
    }
    /// R17: what blocks a card never ends up blocked by it.
    fn blocks_acyclic(&self, start: &InteractionId) -> Result<()> {
        let mut stack = vec![start.clone()];
        let mut seen = BTreeSet::new();
        while let Some(id) = stack.pop() {
            let Some(i) = self.interactions.get(&id) else { continue };
            for next in self.blocked_deliveries(i) {
                if &next == start {
                    return refuse("dependency_cycle", "card.blocks", "These cards would block each other: remove the card.blocks entry that points back");
                }
                if seen.insert(next.clone()) {
                    stack.push(next);
                }
            }
        }
        Ok(())
    }
    /// R12: a delivery shows the current head of its own Task's registered PR.
    fn current_pr_targets(&self, task: &TaskId, targets: &[DeliveryTarget]) -> Result<()> {
        let owned = self.task(task)?.pr_id.clone();
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = self.pull_requests.get(pr_id) else {
                return refuse("pr_mismatch", "targets.pr_id", &format!("PR {pr_id} is not registered: call register_pr first"));
            };
            if &pr.task_id != task || owned.as_ref() != Some(pr_id) {
                return refuse("pr_mismatch", "targets.pr_id", &format!("PR {pr_id} belongs to another Task: present only your own Task's PR"));
            }
            if &pr.head != head {
                return refuse("pr_mismatch", "targets.head", &format!("PR {pr_id} is at {}: present its current head", pr.head));
            }
        }
        Ok(())
    }
    /// R22: a Request closed with an answer tells the user, unless something already did.
    /// A message to the orchestrator that made tasks is done once the last of them ends: nobody
    /// is left to close it, since `close_request` waits for its tasks.
    fn close_finished_messages(&mut self) {
        let mut open: BTreeSet<RequestId> = BTreeSet::new();
        let mut ended: BTreeSet<RequestId> = BTreeSet::new();
        for task in self.tasks.values() {
            if task.over() { &mut ended } else { &mut open }.insert(task.request_id.clone());
        }
        for request in self.requests.values_mut().filter(|request| request.message && !request.closed) {
            if ended.contains(&request.id) && !open.contains(&request.id) {
                request.closed = true;
            }
        }
    }
    fn tell_the_answer(&mut self, actor: &Actor, request: &RequestId, answer: &str, title: Option<&str>,
        created: &mut Vec<String>, effects: &mut Vec<DomainEffect>) -> Result<()> {
        let tasks: BTreeSet<&TaskId> = self.tasks.values().filter(|t| &t.request_id == request).map(|t| &t.id).collect();
        let told = self.interactions.values().any(|i| {
            i.recipient == Recipient::User
                && matches!(i.payload, InteractionPayload::Information { .. })
                && match &i.scope {
                    Scope::Orchestrator { request_id } => request_id.as_ref() == Some(request),
                    Scope::Task { task_id } => tasks.contains(task_id),
                }
        });
        if told {
            return Ok(());
        }
        let mut id = InteractionId(format!("{request}-answer"));
        while self.interactions.contains_key(&id) {
            id.0.push('-');
        }
        let author = match actor {
            Actor::Orchestrator { generation } => Author::Orchestrator { generation: *generation },
            _ => Author::System,
        };
        let need = title.map(|t| clip(t, CardSpec::NEED_MAX)).unwrap_or_else(|| CardSpec::derived(CardKind::Know, answer).need);
        let mut card = CardSpec::new(CardKind::Know, need);
        if answer.trim().chars().count() > CardSpec::WHY_MAX {
            card = card.content(ContentKind::Report, answer.trim());
        } else {
            card = card.why(answer.trim());
        }
        let scope = Scope::Orchestrator { request_id: Some(request.clone()) };
        self.interactions.insert(id.clone(), Interaction {
            attachments: vec![],
            id: id.clone(),
            scope,
            author,
            recipient: Recipient::User,
            body: answer.into(),
            blocking: false,
            subject: None,
            payload: InteractionPayload::Information { read_by: BTreeSet::new() },
            revision: self.revision + 1,
            order: self.revision + 1,
            withdrawn: false,
            card,
            clarification: None,
        });
        created.push(id.0.clone());
        effects.push(DomainEffect::NotifyUser { task_id: None, interaction_id: id });
        Ok(())
    }
    /// R16: the open cards that hold back this delivery, oldest first.
    pub fn blocked_by(&self, delivery: &InteractionId) -> Vec<InteractionId> {
        let Some(delivery) = self.interactions.get(delivery)
            .filter(|delivery| self.open(delivery) && matches!(delivery.payload, InteractionPayload::Delivery { .. }))
        else { return vec![] };
        let mut by: Vec<&Interaction> = self.interactions.values()
            .filter(|i| i.card.blocks.iter().any(|target| Self::card_target_matches_delivery(target, delivery))
                && i.id != delivery.id
                && self.open(i))
            .collect();
        by.sort_by_key(|i| i.order);
        by.into_iter().map(|i| i.id.clone()).collect()
    }
    fn unblocked(&self, delivery: &InteractionId, holds: &dyn Holds) -> Result<()> {
        if let Some(Scope::Task { task_id }) = self.interactions.get(delivery).map(|i| &i.scope)
            && let Some(blocker) = &holds.first_blocker(task_id)
        {
            let title = self.tasks.get(blocker).map_or_else(|| blocker.0.clone(), |task| task.title.clone());
            return refuse(
                "blocked",
                "presentation_id",
                &format!("Task {blocker} still has to review this delivery: nothing is merged or accepted before it"),
            )
            .map_err(|error| error.because(Refusal::BlockedByTask { title }));
        }
        match &holds.first_card(delivery) {
            Some(x) => {
                let need = self.interactions.get(x).map_or_else(String::new, |blocker| blocker.card.need.clone());
                refuse(
                    "blocked",
                    "presentation_id",
                    &format!("Card {x} blocks this delivery: answer or withdraw it first"),
                )
                .map_err(|error| error.because(Refusal::BlockedByQuestion { need }))
            }
            None => Ok(()),
        }
    }
    /// §5: a decision on a delivery is judged against the presentation's
    /// revision, so every decision or invalidation on it must move that revision.
    pub(super) fn decided(&mut self, presentation: &InteractionId) {
        if let Some(p) = self.interactions.get_mut(presentation) {
            p.revision = self.revision + 1;
        }
    }
    fn invalidate(&mut self, task: &TaskId) {
        let next = self.revision + 1;
        if let Some(presentation) = self.tasks.get(task).and_then(|t| t.final_presentation.clone()) {
            self.decided(&presentation);
        }
        for p in self.interactions.values_mut() {
            if p.scope
                == (Scope::Task {
                    task_id: task.clone(),
                })
            {
                if let InteractionPayload::Delivery {
                    acceptance: Some(a),
                    ..
                } = &mut p.payload
                {
                    if a.valid {
                        a.valid = false;
                        p.revision = next;
                    }
                }
            }
        }
        for a in self
            .merge_authorizations
            .values_mut()
            .filter(|a| &a.task_id == task)
        {
            a.valid = false;
        }
        if let Some(s) = self.stack_readiness.get_mut(task) {
            s.ready = false;
        }
    }
    fn notify(
        &self,
        task: &TaskId,
        interaction: Option<InteractionId>,
        message: &str,
        effects: &mut Vec<DomainEffect>,
    ) -> Result<()> {
        if let Some(agent_id) = &self.task(task)?.agent_id {
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
    fn resume_waiting_nodes(&self, before: &DomainState, effects: &mut Vec<DomainEffect>) {
        for t in self.tasks.values() {
            let waited = before.tasks.get(&t.id).is_some_and(|b| b.state == TaskState::WaitingDependency);
            if !waited || t.state != TaskState::Running || t.dependencies.is_empty() || t.final_presentation.is_some() {
                continue;
            }
            let titles: Vec<&str> = t.dependencies.iter()
                .filter_map(|dep| self.tasks.get(dep).map(|d| d.title.as_str())).collect();
            let _ = self.notify(&t.id, None, &dependencies_done_notice(&titles), effects);
        }
    }
    /// Moves one phase of an approved plan to a new Task: the points leave the Task that held
    /// them, so each point of the plan stays with exactly one Task.
    fn take_plan_items(&mut self, task_id: &TaskId, request_id: &RequestId, taken: &PlanItems) -> Result<()> {
        let source = self.tasks.get(&taken.from_task).filter(|t| t.request_id == *request_id);
        let Some(source) = source else {
            return refuse("invalid_input", "plan_items.from_task", "plan_items takes points from a task of this same request");
        };
        if matches!(source.state, TaskState::AwaitingBlocker | TaskState::ForYourReview | TaskState::Closing | TaskState::Done | TaskState::Cancelled) {
            return refuse("plan_items_unavailable", "plan_items.from_task",
                "that task has already delivered or closed, so its points are no longer free to take: create a new request for the work left");
        }
        let Some(held) = self.checklists.get(&taken.from_task) else {
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
        self.checklists.insert(taken.from_task.clone(), Checklist { plan: plan.clone(), items: kept });
        self.checklists.insert(task_id.clone(), Checklist { plan, items: moved });
        Ok(())
    }
    fn dependency_graph(&self) -> Result<()> {
        fn visit(
            s: &DomainState,
            t: &TaskId,
            path: &mut BTreeSet<TaskId>,
            done: &mut BTreeSet<TaskId>,
        ) -> Result<()> {
            if done.contains(t) {
                return Ok(());
            }
            if !path.insert(t.clone()) {
                return fail("dependency_cycle", "Task dependencies contain a cycle");
            };
            let task = s.task(t)?;
            for d in task
                .dependencies
                .iter()
                .chain(&task.delivered_dependencies)
                .chain(task.stack.iter().map(|s| &s.upstream_task))
            {
                visit(s, d, path, done)?;
            }
            path.remove(t);
            done.insert(t.clone());
            Ok(())
        }
        let mut done = BTreeSet::new();
        for t in self.tasks.keys() {
            visit(self, t, &mut BTreeSet::new(), &mut done)?;
        }
        self.blocks_graph()
    }
    /// A task cannot block one it waits for to be done, and blocks never loop: either would hold
    /// both back for ever. Starting after a delivery while blocking it is the review pair.
    fn blocks_graph(&self) -> Result<()> {
        for blocker in self.tasks.values() {
            for blocked in &blocker.blocks {
                self.task(blocked)?;
                if blocked == &blocker.id {
                    return fail("dependency_cycle", "A task cannot block itself");
                }
                if blocker.dependencies.contains(blocked) {
                    return fail("dependency_cycle", "A task cannot block one it waits for to be done");
                }
                let blocked_task = self.task(blocked)?;
                if blocked_task.dependencies.contains(&blocker.id) || blocked_task.delivered_dependencies.contains(&blocker.id) {
                    return fail("dependency_cycle", "A task cannot block one that depends on it: a reviewer depends on the task it reviews, never the other way round");
                }
            }
        }
        fn visit(s: &DomainState, t: &TaskId, path: &mut BTreeSet<TaskId>, done: &mut BTreeSet<TaskId>) -> Result<()> {
            if done.contains(t) {
                return Ok(());
            }
            if !path.insert(t.clone()) {
                return fail("dependency_cycle", "Task blocks contain a cycle");
            }
            for next in &s.task(t)?.blocks {
                visit(s, next, path, done)?;
            }
            path.remove(t);
            done.insert(t.clone());
            Ok(())
        }
        let mut done = BTreeSet::new();
        for t in self.tasks.keys() {
            visit(self, t, &mut BTreeSet::new(), &mut done)?;
        }
        Ok(())
    }
    fn ready_to_start(&self, t: &Task) -> Result<()> {
        self.waits_are_over(t)?;
        self.stack_ready(t)
    }
    /// What a Task waits for before it starts or goes on: its dependencies done and its
    /// delivered dependencies delivered. A wait never holds back the merge of a PR the user
    /// accepted; only `stack` orders merges.
    pub(super) fn waits_are_over(&self, t: &Task) -> Result<()> {
        for dep in &t.dependencies {
            if self.task(dep)?.state != TaskState::Done {
                return fail("dependency_not_done", "Objective dependency is not done");
            }
        }
        for dep in &t.delivered_dependencies {
            let up = self.task(dep)?;
            if up.state == TaskState::Cancelled {
                return fail("dependency_not_done", "Dependency was cancelled");
            }
            if up.final_presentation.is_none() {
                return fail("dependency_not_delivered", "Dependency has not delivered yet");
            }
        }
        Ok(())
    }
    pub(crate) fn stack_ready(&self, t: &Task) -> Result<()> {
        if let Some(stack) = &t.stack {
            let up = self.task(&stack.upstream_task)?;
            if up.state == TaskState::Cancelled {
                return fail("stack_not_ready", "Upstream Task is cancelled");
            };
            let ready = self.stack_readiness.get(&stack.upstream_task);
            if !ready.is_some_and(|r| {
                r.ready
                    && r.presentation_id == stack.presentation_id
                    && r.pr_id == stack.pr_id
                    && r.head == stack.head
            }) || up.final_presentation.as_ref() != Some(&stack.presentation_id)
                || !self
                    .pull_requests
                    .get(&stack.pr_id)
                    .is_some_and(|p| p.task_id == stack.upstream_task && p.head == stack.head)
            {
                return fail("stack_not_ready", "Stack readiness is absent or stale");
            }
        }
        Ok(())
    }
    pub(super) fn accepted(&self, p: &Interaction) -> bool {
        matches!(&p.payload,InteractionPayload::Delivery{acceptance:Some(a),..} if a.valid)
    }
    /// A final delivery whose PRs are all merged is settled by the merge itself, whoever merged
    /// them: Alondra after the user's acceptance or the checks of an auto-merge, or the user on
    /// GitHub.
    pub fn merged_delivery(&self, p: &Interaction) -> bool {
        let InteractionPayload::Delivery { targets, .. } = &p.payload else { return false };
        let mut prs = targets.iter().filter_map(|t| match t {
            DeliveryTarget::PullRequest { pr_id, .. } => Some(pr_id),
            _ => None,
        }).peekable();
        prs.peek().is_some()
            && prs.all(|pr_id| self.pull_requests.get(pr_id).is_some_and(|pr| pr.state == PrState::Merged))
    }
    /// An invalid acceptance the user did not withdraw: no feedback on that
    /// delivery and no document rejection on its Task after it was given.
    pub(super) fn superseded(&self, p: &Interaction) -> Option<SupersededAcceptance> {
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
            || self.document_reviews.iter().any(|r| {
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
    fn covered(&self, presentation: &InteractionId) -> bool {
        let Some(p) = self.interactions.get(presentation) else {
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
                    let decided = self
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
                    let authorized = self.merge_authorizations.values().any(|a| {
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
    fn settle(&mut self, presentation: &InteractionId, u: &str) {
        if self.covered(presentation) {
            self.record_acceptance(presentation, u);
        }
    }
    /// A question or request the task's node or the orchestrator asked the user and that still
    /// waits, blocking or not. A suggestion goes to the orchestrator, not the user: it outlives the
    /// task and holds nothing back. Alondra's own cards (problems, held prompts) are about what Alondra
    /// does with the task, which they would otherwise hold back themselves.
    /// Ends a task the user, the orchestrator or Alondra stopped: its cards leave in silence and a
    /// node it had is told to stop and is closed.
    pub(super) fn cancel(&mut self, task_id: &TaskId, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        self.invalidate(task_id);
        let closing_cards: Vec<InteractionId> = self.interactions.values().filter(|i| {
            i.id.0.starts_with("problem-close-") && i.scope == (Scope::Task { task_id: task_id.clone() })
                && !i.withdrawn && matches!(&i.payload, InteractionPayload::Question { answer: None, .. })
        }).map(|i| i.id.clone()).collect();
        for card in &closing_cards {
            self.retire(card);
        }
        let start_problems: Vec<InteractionId> = self.open_start_problem(task_id).map(|i| i.id.clone()).into_iter().collect();
        for card in &start_problems {
            self.retire(card);
        }
        self.retire_cards_of(task_id);
        let held: Vec<InteractionId> = self.lifecycle(lifecycles, task_id)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
        for card in &held {
            self.retire(card);
        }
        let record = LifecycleRecord::Finished {
            intent: TaskEnding::Cancelling,
            cleanup: self.next_operation("cleanup", task_id),
            post_merge: None,
        };
        self.decide(lifecycles, task_id, record)?;
        let t = self.tasks.get_mut(task_id).unwrap();
        if let Some(agent_id) = t.agent_id.clone() {
            t.wrap_up.close_cancelled = true;
            let message = cancelled_notice(&t.title);
            effects.push(DomainEffect::NotifyAgent {
                agent_id,
                task_id: Some(task_id.clone()),
                interaction_id: None,
                message,
            });
        }
        Ok(())
    }
    /// The user chose to close the node on the card about its work after the merge: that choice is
    /// the fact that ends the work, in the same commit as the answer.
    fn settle_post_merge_problem(&mut self, card: &InteractionId, option: &str, lifecycles: &mut Lifecycles) -> Result<()> {
        let Some(i) = self.interactions.get(card).filter(|i| i.id.0.starts_with(POST_MERGE_PROBLEM_PREFIX) && i.author == Author::System) else { return Ok(()) };
        let Scope::Task { task_id } = i.scope.clone() else { return Ok(()) };
        if option != POST_MERGE_CLOSE || self.tasks.get(&task_id).is_none_or(Task::over) {
            return Ok(());
        }
        self.end_post_merge(&task_id, PostMergeEnd::UserDecision, lifecycles)
    }
    /// What the user chose on a start problem card that the domain does itself: cancel the task, or
    /// let it start without the cancelled task it waited for. "Try again" is the daemon's to do.
    fn settle_start_problem(&mut self, card: &InteractionId, option: &str, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let Some(i) = self.interactions.get(card).filter(|i| i.id.0.starts_with(START_PROBLEM_PREFIX) && i.author == Author::System) else { return Ok(()) };
        let Scope::Task { task_id } = i.scope.clone() else { return Ok(()) };
        let Ok(problem) = serde_json::from_str::<StartProblem>(&i.body) else { return Ok(()) };
        if self.tasks.get(&task_id).is_none_or(Task::over) {
            return Ok(());
        }
        match (option, problem) {
            (START_CANCEL, _) => self.cancel(&task_id, effects, lifecycles)?,
            (START_WITHOUT, StartProblem::DependencyCancelled { dependency }) => {
                self.invalidate(&task_id);
                let t = self.tasks.get_mut(&task_id).unwrap();
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
    /// A cancelled task whose node Alondra has not closed yet: its open cards were taken away when
    /// it was cancelled, so any it has now were asked after. Its node can still ask the user what
    /// to do with work it cannot leave as it is.
    pub(crate) fn winding_down(t: &Task) -> bool {
        t.cancelled() && t.wrap_up.close_cancelled && t.wrap_up.node.is_none()
    }
    fn retire_cards_of(&mut self, task_id: &TaskId) {
        let open: Vec<InteractionId> = self.interactions.values().filter(|i| self.unanswered_card(i, task_id)).map(|i| i.id.clone()).collect();
        for card in &open {
            self.retire(card);
        }
    }
    fn unanswered_card(&self, i: &Interaction, t: &TaskId) -> bool {
        matches!(&i.scope, Scope::Task { task_id } if task_id == t)
            && i.recipient == Recipient::User
            && !matches!(i.author, Author::System)
            && i.card.kind != CardKind::Propose
            && !i.withdrawn
            && matches!(
                i.payload,
                InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. }
            )
    }
    /// The task has a question or request the user has not answered: it neither closes nor ends
    /// with it.
    pub fn unanswered(&self, t: &TaskId) -> bool {
        self.interactions.values().any(|i| self.unanswered_card(i, t))
    }
    /// An unanswered card that does not name what it holds back: no final delivery may stand next to
    /// it. A card that names its delivery or PR already holds that merge back where the user sees it.
    fn asked_aside(&self, i: &Interaction, t: &TaskId) -> bool {
        i.card.blocks.is_empty() && self.unanswered_card(i, t)
    }
    pub(super) fn blockers(&self, t: &TaskId) -> bool {
        self.interactions.values().any(|i| {
            i.scope == (Scope::Task { task_id: t.clone() })
                && i.blocking
                && !i.withdrawn
                && matches!(
                    i.payload,
                    InteractionPayload::Question { answer: None, .. }
                        | InteractionPayload::ActionRequest { answer: None, .. }
                )
        })
    }
    pub(super) fn settled(&self, t: &Task, p: &Interaction) -> bool {
        self.merged_delivery(p)
            || (self.accepted(p)
                && matches!(&p.payload, InteractionPayload::Delivery { targets, .. }
                    if self.completable(t, targets, true).is_ok()))
    }
    /// The final delivery is settled: every PR merged, or accepted with nothing left to merge.
    pub fn finished(&self, t: &Task) -> bool {
        t.final_presentation.as_ref().and_then(|p| self.interactions.get(p)).is_some_and(|p| self.settled(t, p))
    }
    /// A node that asks the user something after its final delivery takes that delivery back: it
    /// leaves the user's queue, the task is in progress again, and the node presents again once it
    /// has the answers. A delivery the user accepted or a PR already merged stays.
    fn withdraw_deliveries_asked_after(&mut self, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let asked: Vec<(TaskId, InteractionId)> = self.tasks.values()
            .filter(|t| !t.over() && self.interactions.values().any(|i| self.asked_aside(i, &t.id)))
            .filter_map(|t| {
                let p = self.interactions.get(t.final_presentation.as_ref()?)?;
                (!self.accepted(p) && !self.settled(t, p)).then(|| (t.id.clone(), p.id.clone()))
            })
            .collect();
        for (task_id, delivery) in asked {
            self.invalidate(&task_id);
            self.retire(&delivery);
            self.hand_back(&task_id, &delivery, lifecycles)?;
            let t = self.tasks.get_mut(&task_id).unwrap();
            if let Some(agent_id) = t.agent_id.clone() {
                effects.push(DomainEffect::NotifyAgent {
                    agent_id, task_id: Some(task_id), interaction_id: None, message: delivery_withdrawn_for_questions(),
                });
            }
        }
        Ok(())
    }
    /// A node that pushes to its PR after its final delivery leaves that delivery behind the PR: it
    /// could no longer be merged as presented, so it leaves the user's queue and the task is in
    /// progress again until the node presents the new head. A merged PR settles the delivery instead.
    fn withdraw_deliveries_behind_their_pr(&mut self, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        let behind: Vec<(TaskId, InteractionId, u64)> = self.tasks.values()
            .filter(|t| !t.over())
            .filter_map(|t| {
                let p = self.interactions.get(t.final_presentation.as_ref()?)?;
                let InteractionPayload::Delivery { targets, .. } = &p.payload else { return None };
                if p.withdrawn || self.settled(t, p) {
                    return None;
                }
                targets.iter().find_map(|target| match target {
                    DeliveryTarget::PullRequest { pr_id, head } => self.pull_requests.get(pr_id)
                        .filter(|pr| pr.state == PrState::Open && pr.head != *head)
                        .map(|pr| (t.id.clone(), p.id.clone(), pr.number)),
                    _ => None,
                })
            })
            .collect();
        for (task_id, delivery, number) in behind {
            self.invalidate(&task_id);
            self.retire(&delivery);
            self.hand_back(&task_id, &delivery, lifecycles)?;
            let t = self.tasks.get_mut(&task_id).unwrap();
            if let Some(agent_id) = t.agent_id.clone() {
                effects.push(DomainEffect::NotifyAgent {
                    agent_id, task_id: Some(task_id), interaction_id: None, message: delivery_withdrawn_behind_pr(number),
                });
            }
        }
        Ok(())
    }
    /// A final delivery leaves: the task is in progress again. The lifecycle takes the delivery
    /// back with the input the node goes on with.
    fn hand_back(&mut self, task_id: &TaskId, delivery: &InteractionId, lifecycles: &mut Lifecycles) -> Result<()> {
        if self.tasks[task_id].wrap_up.node.is_some() {
            self.tasks.get_mut(task_id).unwrap().wrap_up = WrapUp::default();
        }
        self.node_has_run(task_id, lifecycles)?;
        let record = LifecycleRecord::HandedBack { delivery: delivery.clone(), input: self.next_input(task_id) };
        self.decide(lifecycles, task_id, record)?;
        self.tasks.get_mut(task_id).unwrap().final_presentation = None;
        Ok(())
    }
    /// The cards of a task that ended leave the user's queue. A cancelled task takes them away in
    /// silence: the user stopped the work, so nothing it asked matters any more. A task that ended
    /// with its work done tells the user once about a question it left behind, unless it only
    /// asked to approve a plan or document for work that is already done. What the node of a
    /// cancelled task asks while it still winds down is about that ending, such as work it cannot
    /// leave as it is: it stays with the user until the node is gone.
    fn retire_cards_of_ended_tasks(&mut self, effects: &mut Vec<DomainEffect>) {
        let left: Vec<(TaskId, InteractionId)> = self.tasks.values()
            .filter(|t| t.state.terminal() && !Self::winding_down(t))
            .flat_map(|t| self.interactions.values().filter(|i| self.unanswered_card(i, &t.id)).map(|i| (t.id.clone(), i.id.clone())))
            .collect();
        let stale_notices: Vec<InteractionId> = self.interactions.values()
            .filter(|i| i.author == Author::System && !i.withdrawn && i.id.0.starts_with(ENDED_UNANSWERED_PREFIX))
            .filter(|i| matches!(&i.scope, Scope::Task { task_id } if self.tasks.get(task_id).is_some_and(|t| t.state == TaskState::Cancelled)))
            .map(|i| i.id.clone())
            .collect();
        for notice in stale_notices {
            self.retire(&notice);
        }
        for (task_id, question) in left {
            self.retire(&question);
            let q = &self.interactions[&question];
            if self.tasks[&task_id].state == TaskState::Cancelled || q.card.kind == CardKind::Review {
                continue;
            }
            let id = InteractionId(format!("{ENDED_UNANSWERED_PREFIX}{}", question.0));
            if self.interactions.contains_key(&id) {
                continue;
            }
            let need = clip(&format!("No longer waits for your answer: {}", q.card.need), CardSpec::NEED_MAX);
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
                revision: self.revision + 1,
                order: self.revision + 1,
                withdrawn: false,
                card,
                clarification: None,
            };
            self.interactions.insert(id.clone(), interaction);
            effects.push(DomainEffect::NotifyUser { task_id: Some(task_id), interaction_id: id });
        }
    }
    /// The task has a node whose end Alondra has not recorded.
    pub fn wraps_up(&self, t: &Task) -> bool {
        t.agent_id.is_some() && t.wrap_up.node.is_none()
    }
    /// The session of the task's node is over: nothing it declared can still be running.
    pub fn node_session_closed(&self, t: &Task) -> bool {
        t.agent_id.as_ref().and_then(|agent| self.node_phase(agent)) == Some(&NodePhase::Closed)
    }
    /// The task waits for its node to clean up and be closed.
    pub fn in_cleanup(&self, t: &Task) -> bool {
        !t.over() && t.wrap_up.asked_at.is_some()
    }
    /// Asks the node of every task whose delivery just settled to clean up, and forgets the
    /// request of one whose delivery is no longer settled.
    fn wrap_up_step(&mut self, effects: &mut Vec<DomainEffect>) {
        let revision = self.revision + 1;
        let ids: Vec<TaskId> = self.tasks.values().filter(|t| !t.over()).map(|t| t.id.clone()).collect();
        for id in ids {
            if self.blockers(&id) || self.unanswered(&id) {
                continue;
            }
            let t = &self.tasks[&id];
            let finished = self.finished(t);
            if finished && self.wraps_up(t) && t.wrap_up.asked_at.is_none() {
                let agent_id = t.agent_id.clone().expect("a task that wraps up has a node");
                let message = match &t.wrap_up.post_merge_work {
                    Some(work) => post_merge_reminder(&t.title, &t.id, &work.reason),
                    None => cleanup_request(&t.title, &t.id),
                };
                self.tasks.get_mut(&id).unwrap().wrap_up.asked_at = Some(revision);
                effects.push(DomainEffect::NotifyAgent { agent_id, task_id: Some(id), interaction_id: None, message });
            } else if !finished && (t.wrap_up.asked_at.is_some() || t.wrap_up.cleaned_at.is_some()) {
                let wrap_up = &mut self.tasks.get_mut(&id).unwrap().wrap_up;
                wrap_up.asked_at = None;
                wrap_up.cleaned_at = None;
            }
        }
    }
    /// What still keeps a Task with a settled final delivery from being done.
    /// A PR closed without merging after the user accepted the delivery settles like a registered
    /// one: only a person closes a PR, and nothing would ever merge it. A merged registered PR the
    /// delivery leaves out settles too, since the user accepted the Task with it merged.
    fn completable(&self, t: &Task, targets: &[DeliveryTarget], accepted: bool) -> Result<()> {
        if self.blockers(&t.id) {
            return fail(
                "blocking_interaction",
                "Task has an unanswered blocking Interaction",
            );
        };
        for target in targets {
            if let DeliveryTarget::PullRequest { pr_id, head } = target {
                let pr = &self.pull_requests[pr_id];
                let merged = pr.state == PrState::Merged && pr.merged_head.as_ref() == Some(head);
                if !merged && !(accepted && pr.state == PrState::Closed) {
                    return fail(
                        "merge_not_confirmed",
                        "Presented head is not confirmed merged",
                    );
                };
                // User conformity for the exact final delivery is mandatory. A PR
                // merged externally can be explicitly accepted after observation;
                // observation itself never grants that conformity.
            }
        }
        if t.pr_id.as_ref().is_some_and(|pr_id| self.pull_requests[pr_id].state == PrState::Open) {
            return fail(
                "merge_not_confirmed",
                "The Task's registered PR is still open",
            );
        }
        Ok(())
    }
    /// Waiting and activity states are always recomputed from the facts; an
    /// import runs this once so the first live command never rewrites every Task.
    pub fn derive_states(&mut self) {
        // A task that becomes done lets the ones waiting on it start in the same step.
        for _ in 0..=self.tasks.len() {
            if !self.derive_states_once() {
                break;
            }
        }
    }
    fn derive_states_once(&mut self) -> bool {
        let updates: Vec<_> = self
            .tasks
            .values()
            .filter(|t| !t.state.terminal())
            .map(|t| (t.id.clone(), self.state_within_phase(t)))
            .collect();
        let mut changed = false;
        for (id, state) in updates {
            let task = self.tasks.get_mut(&id).unwrap();
            changed |= task.state != state;
            task.state = state;
        }
        changed
    }
    /// A reviewer whose blocked tasks are all over (cancelled, merged outside, finished) has
    /// nothing left to review: it is cancelled, its node told, and its delivery never reaches the
    /// user.
    fn cancel_orphan_reviewers(&mut self, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<bool> {
        let orphans: Vec<TaskId> = self
            .tasks
            .values()
            .filter(|b| !b.over() && !b.blocks.is_empty() && !self.finished(b))
            .filter(|b| !b.blocks.iter().any(|a| self.tasks.get(a).is_some_and(|a| !a.over() && !self.finished(a))))
            .map(|b| b.id.clone())
            .collect();
        for id in &orphans {
            self.cancel(id, effects, lifecycles)?;
        }
        Ok(!orphans.is_empty())
    }
    /// Tasks whose revision must move: their row, their decisions and
    /// deliveries, PR, authorizations, stack readiness, reviews or linked
    /// document versions differ in `next`. Information neither blocks nor dates
    /// a decision (R07), so it never moves a Task's revision.
    fn touched_tasks(&self, next: &DomainState) -> BTreeSet<TaskId> {
        let mut touched = BTreeSet::new();
        for (id, task) in &next.tasks {
            if self.tasks.get(id) != Some(task) {
                touched.insert(id.clone());
            }
        }
        for (id, interaction) in &next.interactions {
            if matches!(interaction.payload, InteractionPayload::Information { .. }) {
                continue;
            }
            if let Scope::Task { task_id } = &interaction.scope
                && self.interactions.get(id) != Some(interaction)
            {
                touched.insert(task_id.clone());
            }
        }
        for (id, pr) in &next.pull_requests {
            if self.pull_requests.get(id) != Some(pr) {
                touched.insert(pr.task_id.clone());
            }
        }
        for (id, authorization) in &next.merge_authorizations {
            if self.merge_authorizations.get(id) != Some(authorization) {
                touched.insert(authorization.task_id.clone());
            }
        }
        for (id, readiness) in &next.stack_readiness {
            if self.stack_readiness.get(id) != Some(readiness) {
                touched.insert(id.clone());
            }
        }
        for review in &next.document_reviews {
            if !self.document_reviews.contains(review) {
                touched.insert(review.task_id.clone());
            }
        }
        for (id, journal) in &next.lifecycle_journal {
            if self.lifecycle_journal.get(id) != Some(journal) {
                touched.insert(id.clone());
            }
        }
        for (id, review) in &next.plan_reviews {
            let before = self.plan_reviews.get(id);
            touched.extend(review.dependents.iter()
                .filter(|(task, settled)| before.and_then(|b| b.dependents.get(*task)) != Some(*settled))
                .map(|(task, _)| task.clone()));
        }
        for (id, document) in &next.documents {
            if self.documents.get(id) != Some(document) {
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
    fn schedule_unchanged(&self, id: &ScheduleId, expected: Option<i64>) -> Result<()> {
        let Some(expected) = expected else {
            return Ok(());
        };
        if self
            .schedule
            .schedules
            .get(id)
            .is_some_and(|s| s.revision != expected)
        {
            let mut e = DomainError::new("stale_revision", "Schedule changed; refresh before applying");
            e.current_revision = Some(self.revision);
            return Err(e);
        }
        Ok(())
    }
    /// The revision a command's expected_revision must reach: its Task for
    /// Task-directed commands, the Interaction for decisions on one, and the
    /// whole state for commands without a narrower subject.
    fn subject_revision(&self, c: &Command) -> Option<Revision> {
        let task = |id: &TaskId| Some(self.tasks.get(id).map_or(0, |t| t.revision));
        let interaction =
            |id: &InteractionId| Some(self.interactions.get(id).map_or(0, |i| i.revision));
        match c {
            Command::AssignTask { task_id, .. }
            | Command::RetitleTask { task_id, .. }
            | Command::SetDependencies { task_id, .. }
            | Command::StartTask { task_id }
            | Command::CreateDocument { task_id, .. }
            | Command::ReviseDocument { task_id, .. }
            | Command::LinkTaskDocument { task_id, .. }
            | Command::DecideDocumentReview { task_id, .. }
            | Command::RegisterPr { task_id, .. }
            | Command::PresentResult { task_id, .. }
            | Command::PublishStackReadiness { task_id, .. }
            | Command::CompleteTask { task_id, .. }
            | Command::CancelTask { task_id }
            | Command::ReopenTask { task_id }
            | Command::ConfirmCleanup { task_id }
            | Command::DeclarePostMergeWork { task_id, .. }
            | Command::RecordNodeEnd { task_id, .. }
            | Command::RecordTaskCleanup { task_id }
            | Command::AdmitExecution { task_id, .. }
            | Command::RecordRuntimeFacts { task_id, .. }
            | Command::RecordTurnBoundary { task_id, .. }
            | Command::RecordHostEnded { task_id, .. }
            | Command::RecordDoubtfulHandoff { task_id, .. }
            | Command::ResendUntakenInput { task_id, .. }
            | Command::ArchiveTask { task_id, .. }
            | Command::HoldHook { task_id, .. }
            | Command::HoldQuestions { task_id, .. }
            | Command::SettlePlanReview { task_id, .. } => task(task_id),
            Command::PublishInformation { scope, .. }
            | Command::AskQuestion { scope, .. }
            | Command::AskQuestions { scope, .. }
            | Command::RequestAction { scope, .. }
            | Command::RaiseProblem { scope, .. } => match scope {
                Scope::Task { task_id } => task(task_id),
                Scope::Orchestrator { .. } => None,
            },
            Command::MarkRead { interaction_id }
            | Command::Withdraw { interaction_id }
            | Command::RequestClarification { interaction_id, .. }
            | Command::AnswerQuestion { interaction_id, .. }
            | Command::SettleSuggestion { interaction_id, .. }
            | Command::ReopenAnswer { interaction_id }
            | Command::LinkInteractionSubject { interaction_id, .. }
            | Command::SubmitFeedback {
                presentation_id: interaction_id,
                ..
            }
            | Command::AcceptDelivery {
                presentation_id: interaction_id,
                ..
            }
            | Command::DecideProposedPlan {
                presentation_id: interaction_id,
                ..
            }
            | Command::AuthorizeMerge {
                presentation_id: interaction_id,
                ..
            } => interaction(interaction_id),
            Command::ClaimMergeAuthorization {
                authorization_id, ..
            }
            | Command::ObserveMergeAttempt {
                authorization_id, ..
            } => Some(
                self.merge_authorizations
                    .get(authorization_id)
                    .and_then(|a| self.tasks.get(&a.task_id))
                    .map_or(0, |t| t.revision),
            ),
            Command::ObservePrState { pr_id, .. }
            | Command::ObservePrMergeMethods { pr_id, .. } => Some(
                self.pull_requests
                    .get(pr_id)
                    .and_then(|p| self.tasks.get(&p.task_id))
                    .map_or(0, |t| t.revision),
            ),
            // A Schedule's own revision is its subject: Update carries the next
            // one, Delete and Claim the one they read. The rule checks it.
            Command::CreateSchedule { .. } | Command::UpdateSchedule { .. } => Some(0),
            Command::DeleteSchedule {
                expected_schedule_revision: Some(_),
                ..
            }
            | Command::ClaimScheduleRun {
                expected_schedule_revision: Some(_),
                ..
            } => Some(0),
            _ => None,
        }
    }
    /// Clone, validate, swap: even an error discovered after preparing several changes is atomic.
    /// The current state was validated when it was loaded or committed; only the result is checked.
    /// Semantic receipts belong to the SQL transaction and are looked up before this method.
    pub fn execute(&mut self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome> {
        let (next, outcome) = self.prepare_transition(actor, envelope)?;
        *self = next;
        #[cfg(test)]
        super::read_index::assert_index_matches(self);
        Ok(outcome)
    }

    #[cfg(test)]
    pub(super) fn preview(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<CommandOutcome> {
        self.prepare_transition(actor, envelope).map(|(_, outcome)| outcome)
    }

    fn prepare_transition(&self, actor: &Actor, envelope: &CommandEnvelope) -> Result<(Self, CommandOutcome)> {
        self.admit(actor, envelope)?;
        let mut next = self.clone();
        let mut effects = vec![];
        let mut created = vec![];
        let mut lifecycles = Lifecycles::new();
        next.apply(actor, &envelope.command, &mut effects, &mut created, &mut lifecycles)?;
        next.withdraw_deliveries_asked_after(&mut effects, &mut lifecycles)?;
        next.withdraw_deliveries_behind_their_pr(&mut effects, &mut lifecycles)?;
        next.adopt_declared_post_merge(&mut lifecycles)?;
        next.finish_settled_deliveries(&mut lifecycles)?;
        next.derive_states();
        if next.cancel_orphan_reviewers(&mut effects, &mut lifecycles)? {
            next.derive_states();
        }
        next.retire_cards_of_ended_tasks(&mut effects);
        next.close_finished_messages();
        let acting_before = self.acting();
        for (task_id, card) in next.acting() {
            if acting_before.get(&task_id).map(|before| &before.interaction) != Some(&card.interaction) {
                effects.push(DomainEffect::NotifyUser { task_id: Some(task_id), interaction_id: card.interaction });
            }
        }
        next.resume_waiting_nodes(self, &mut effects);
        next.wrap_up_step(&mut effects);
        next.settle_nodes();
        for id in self.touched_tasks(&next) {
            let task = next.tasks.get_mut(&id).unwrap();
            if task.started_without_node() && !self.tasks.get(&id).is_some_and(Task::started_without_node) {
                return fail("conflict", &format!("Task {} would be in progress with no node", id.0));
            }
            task.revision = self.revision + 1;
        }
        let changed = next != *self;
        if changed {
            next.revision += 1;
        } else {
            effects.clear();
        }
        next.validate()?;
        next.validate_nodes()?;
        next.validate_lifecycles(self.touched_tasks(&next).iter())?;
        let superseded_acceptances = self
            .interactions
            .values()
            .filter(|p| self.accepted(p))
            .filter_map(|p| next.superseded(&next.interactions[&p.id]))
            .collect();
        let outcome = CommandOutcome {
            revision: next.revision,
            changed,
            effects,
            created_ids: created,
            superseded_acceptances,
        };
        Ok((next, outcome))
    }
    fn apply(
        &mut self,
        actor: &Actor,
        c: &Command,
        effects: &mut Vec<DomainEffect>,
        created: &mut Vec<String>,
        lifecycles: &mut Lifecycles,
    ) -> Result<()> {
        match c {
            Command::CreateSchedule { schedule } => {
                system(actor)?;
                id(&schedule.id.0)?;
                schedule.validate()?;
                if schedule.revision != 1 || schedule.deleted_at.is_some() {
                    return fail("invalid_input", "New Schedule needs revision one and no deletion");
                }
                if self.schedule.schedules.contains_key(&schedule.id) {
                    return fail("idempotency_conflict", "Schedule ID already exists");
                }
                self.schedule
                    .schedules
                    .insert(schedule.id.clone(), schedule.clone());
                created.push(schedule.id.0.clone());
            }
            Command::UpdateSchedule { schedule } => {
                system(actor)?;
                id(&schedule.id.0)?;
                schedule.validate()?;
                let current = self
                    .schedule
                    .schedules
                    .get(&schedule.id)
                    .ok_or_else(|| DomainError::new("not_found", "Schedule does not exist"))?;
                if current.deleted_at.is_some() || schedule.deleted_at.is_some() {
                    return fail("not_found", "Schedule was deleted");
                }
                if self.schedule.runs.values().any(|run| {
                    run.schedule_id == schedule.id && run.state == ScheduleRunState::Launching
                }) {
                    return fail("conflict", "Schedule run is being sent");
                }
                if current.revision.checked_add(1) != Some(schedule.revision) {
                    return fail("stale_revision", "Schedule revision must advance by one");
                }
                self.schedule
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
                self.schedule_unchanged(schedule_id, *expected_schedule_revision)?;
                let schedule = self
                    .schedule
                    .schedules
                    .get_mut(schedule_id)
                    .ok_or_else(|| DomainError::new("not_found", "Schedule does not exist"))?;
                if schedule.deleted_at.is_some() {
                    return fail("not_found", "Schedule was deleted");
                }
                if self.schedule.runs.values().any(|run| {
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
                self.schedule_unchanged(schedule_id, *expected_schedule_revision)?;
                self.schedule.claim(
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
                let run = self
                    .schedule
                    .runs
                    .get(run_id)
                    .ok_or_else(|| DomainError::new("not_found", "Schedule run not found"))?;
                let definition = run
                    .definition_snapshot
                    .as_ref()
                    .ok_or_else(|| DomainError::new("invalid_input", "Run has no claimed definition"))?;
                launch.validate(definition)?;
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
                if self.requests.contains_key(&request.id) || self.tasks.contains_key(&task.id) {
                    return fail("idempotency_conflict", "Scheduled Request or Task ID already exists");
                }
                self.schedule.record_sent(
                    run_id,
                    request.id.clone(),
                    task.id.clone(),
                    launch.clone(),
                    *finished_at,
                )?;
                self.requests.insert(request.id.clone(), request.clone());
                self.tasks.insert(task.id.clone(), task.clone());
                self.plan_lifecycle(lifecycles, &task.id)?;
                created.push(request.id.0.clone());
                created.push(task.id.0.clone());
            }
            Command::ClaimScheduleLaunch { run_id, now } => {
                system(actor)?;
                let task_id = self.schedule.runs.get(run_id)
                    .and_then(|run| run.task_id.as_ref())
                    .ok_or_else(|| DomainError::new("conflict", "Scheduled Run has no Task"))?;
                let task = self.tasks.get(task_id)
                    .ok_or_else(|| DomainError::new("conflict", "Scheduled Task is missing"))?;
                if task.agent_id.is_some()
                    || task.state != TaskState::NotStarted
                    || task.started
                    || !task.dependencies.is_empty()
                    || task.stack.is_some()
                {
                    return fail("conflict", "Scheduled Task is no longer ready to launch");
                }
                self.schedule.claim_launch(run_id, *now)?;
            }
            Command::RecordScheduleLaunchHanded { run_id, session_id, agent_id, finished_at } => {
                system(actor)?;
                if let Some(agent_id) = agent_id {
                    if !self.agents.contains_key(agent_id) {
                        return fail("not_found", "Agent missing");
                    }
                    let task_id = self.schedule.runs.get(run_id)
                        .and_then(|run| run.task_id.clone())
                        .ok_or_else(|| DomainError::new("conflict", "Scheduled Run has no Task"))?;
                    let task = self.active(&task_id)?;
                    if task.agent_id.is_some() {
                        return fail("conflict", "Scheduled Task is already assigned");
                    }
                    self.schedule.record_launch_handed(run_id, session_id.clone(), Some(agent_id.clone()), *finished_at)?;
                    self.admit_node(&task_id, agent_id, lifecycles)?;
                } else {
                    self.schedule.record_launch_handed(run_id, session_id.clone(), None, *finished_at)?;
                }
            }
            Command::RaiseStartProblem { task_id, problem } => {
                system(actor)?;
                let task = self.task(task_id)?;
                if task.over() {
                    return fail("task_not_active", "A closed Task has nothing to start");
                }
                if self.open_start_problem(task_id).is_some() {
                    return Ok(());
                }
                let title = task.title.clone();
                let n = self.interactions.keys().filter(|i| i.0.starts_with(&format!("{START_PROBLEM_PREFIX}{}-", task_id.0))).count() + 1;
                let problem_id = InteractionId(format!("{START_PROBLEM_PREFIX}{}-{n}", task_id.0));
                let dependency = match problem {
                    StartProblem::DependencyCancelled { dependency } => self.tasks.get(dependency).map(|t| t.title.as_str()),
                    _ => None,
                };
                let card = crate::start_problem::card(&title, problem, dependency, crate::views::Lang::En);
                let options = crate::start_problem::options(problem, crate::views::Lang::En);
                self.insert_interaction(
                    actor,
                    &problem_id,
                    &Scope::Task { task_id: task_id.clone() },
                    &Recipient::User,
                    &serde_json::to_string(problem).map_err(|e| DomainError::new("invalid_input", e.to_string()))?,
                    false,
                    &[],
                    InteractionPayload::Question { options, answer: None },
                    &card,
                    Source::Problem,
                )?;
                created.push(problem_id.0.clone());
                effects.push(DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: problem_id });
            }
            Command::RecordScheduleRunFailed {
                run_id,
                reason,
                finished_at,
            } => {
                system(actor)?;
                self.schedule
                    .record_failed(run_id, reason.clone(), *finished_at)?;
                let open_task = self.schedule.runs[run_id].task_id.as_ref().and_then(|id| self.tasks.get(id))
                    .filter(|task| !task.over() && !task.archived).cloned();
                if let Some(task) = open_task {
                    effects.push(DomainEffect::ScheduleLaunchFailed {
                        task_id: task.id.clone(),
                        title: task.title.clone(),
                        reason: reason.trim().to_owned(),
                    });
                    let problem = InteractionId(format!("{LAUNCH_PROBLEM_PREFIX}{}", run_id.0));
                    let card = CardSpec::new(CardKind::Problem, clip(&format!("Scheduled task \"{}\" could not start", task.title), CardSpec::NEED_MAX))
                        .why(clip(reason, CardSpec::WHY_MAX))
                        .content(ContentKind::Diagnosis, reason.trim());
                    self.insert_interaction(
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
                self.schedule.interrupt_on_restart();
            }
            Command::CreateRequest { request_id, body, auto_merge, maintenance, message } => {
                self.manager(actor)?;
                id(&request_id.0)?;
                nonempty(body)?;
                if self.requests.contains_key(request_id) {
                    return fail("invalid_input", "Request already exists");
                };
                self.requests.insert(
                    request_id.clone(),
                    Request { auto_merge: *auto_merge, maintenance: *maintenance, message: *message || *maintenance, ..Request::new(request_id.clone(), body.clone()) },
                );
                created.push(request_id.0.clone());
                // 8ccd131 handed every new request of the user to the orchestrator.
                if matches!(actor, Actor::User { .. }) {
                    effects.push(DomainEffect::NotifyOrchestrator { interaction_id: None, request_id: Some(request_id.clone()) });
                }
            }
            Command::CloseRequest { request_id, answer, withdrawn, title } => {
                if !matches!(actor, Actor::System) {
                    self.manager(actor)?;
                }
                if self
                    .tasks
                    .values()
                    .any(|t| &t.request_id == request_id && !t.over())
                {
                    return fail("task_not_active", "Request still has unfinished Tasks");
                };
                for text in answer.iter().chain(title) {
                    nonempty(text)?;
                }
                let request = self
                    .requests
                    .get_mut(request_id)
                    .ok_or_else(|| DomainError::new("not_found", "Request missing"))?;
                request.closed = true;
                request.withdrawn = *withdrawn;
                if answer.is_some() {
                    request.answer = answer.clone();
                }
                if title.is_some() {
                    request.title = title.clone();
                }
                if let Some(answer) = answer.as_ref().filter(|_| !request.maintenance) {
                    self.tell_the_answer(actor, request_id, answer, title.as_deref(), created, effects)?;
                }
            }
            Command::RegisterAgent { agent_id, name, at } => {
                system(actor)?;
                id(&agent_id.0)?;
                nonempty(name)?;
                if self.agents.contains_key(agent_id) {
                    return fail("invalid_input", "Agent instance already exists");
                };
                self.agents.insert(
                    agent_id.clone(),
                    Agent {
                        id: agent_id.clone(),
                        name: name.clone(),
                        phase: NodePhase::Starting { reviving: false, idle_since: None },
                        since: *at,
                    },
                );
                created.push(agent_id.0.clone());
            }
            Command::MoveNode { agent_id, event, at } => {
                system(actor)?;
                self.move_node(agent_id, event, *at)?;
                if *event == NodeEvent::TurnStarted {
                    let task = self.node_task(agent_id).filter(|t| !t.state.terminal() && t.agent_id.as_ref() == Some(agent_id)).map(|t| t.id.clone());
                    if let Some(task) = task {
                        self.node_has_run(&task, lifecycles)?;
                    }
                }
            }
            Command::RecordLastWords { agent_id, text } => {
                system(actor)?;
                if !self.agents.contains_key(agent_id) {
                    return fail("not_found", "Agent missing");
                }
                self.last_words.insert(agent_id.clone(), clip(text.trim(), LAST_WORDS_MAX));
            }
            Command::ReconcileNodes { listed, at } => {
                system(actor)?;
                self.reconcile_nodes(listed, *at);
            }
            Command::AdvanceOrchestratorGeneration { generation } => {
                system(actor)?;
                if *generation <= self.orchestrator_generation || *generation > MAX_REVISION {
                    return fail("stale_generation", "Generation must advance monotonically");
                };
                self.orchestrator_generation = *generation;
            }
            Command::BindOrchestratorSession { session_id } => {
                system(actor)?;
                if session_id.0.trim().is_empty() {
                    return fail("invalid_input", "The orchestrator's session id is empty");
                }
                self.orchestrator_session = Some(session_id.clone());
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
            } => {
                self.fence(actor)?;
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
                if self.tasks.contains_key(task_id) {
                    return fail("invalid_input", "Task already exists");
                };
                let Some(request) = self.requests.get(request_id).filter(|r| !r.closed) else {
                    return fail("not_found", "Open Request required");
                };
                if request.title.is_none() && self.tasks.values().any(|task| task.request_id == *request_id) {
                    return refuse(
                        "request_title_required",
                        "request.title",
                        "Name the Request with title_request (a few words, in the user's language, never the prompt) before adding a second Task",
                    );
                }
                if agent_id
                    .as_ref()
                    .is_some_and(|a| !self.agents.contains_key(a))
                {
                    return fail("not_found", "Agent missing");
                };
                if let Some(agent) = agent_id {
                    self.free(agent, task_id)?;
                    if matches!(actor, Actor::Orchestrator { .. }) {
                        self.used_by_other(agent, task_id)?;
                    }
                }
                self.tasks.insert(
                    task_id.clone(),
                    Task {
                        id: task_id.clone(),
                        request_id: request_id.clone(),
                        agent_id: agent_id.clone(),
                        title: title.clone(),
                        state: TaskState::NotStarted,
                        revision: self.revision + 1,
                        started: false,
                        review_prs: *review_prs,
                        final_presentation: None,
                        pr_id: None,
                        dependencies: dependencies.clone(),
                        stack: stack.clone(),
                        documents: BTreeMap::new(),
                        archived: false,
                        model: None,
                        delivered_dependencies: Default::default(), blocks: Default::default(), verdict: None, review_rounds: 0, wrap_up: WrapUp::default(), priority: Default::default(), ending: None,
                    },
                );
                self.dependency_graph()?;
                if let Some(taken) = plan_items {
                    self.take_plan_items(task_id, request_id, taken)?;
                } else if let Some(approved) = self.request_plans.get(request_id) {
                    if self.checklists.values().any(|c| c.plan == approved.presentation_id) {
                        return refuse("approved_plan_has_task", "plan_items",
                            "this request's approved plan already has a task: name with plan_items the points of that plan this task takes from the task that holds them, or create a new request for work the plan does not cover");
                    }
                    let plan = &self.interactions[&approved.presentation_id];
                    let checklist = Checklist { plan: approved.presentation_id.clone(), items: checklist_items(plan.card.plan.as_deref().unwrap_or_default()) };
                    self.checklists.insert(task_id.clone(), checklist);
                }
                self.plan_lifecycle(lifecycles, task_id)?;
                if let Some(agent) = agent_id {
                    self.decide(lifecycles, task_id, LifecycleRecord::Admitted { node: agent.clone(), execution: None })?;
                }
                created.push(task_id.0.clone());
            }
            Command::AssignTask { task_id, agent_id, model } => {
                self.not_held_for_plan_review(task_id)?;
                self.not_ending(task_id)?;
                if matches!(actor, Actor::System) {
                    let t = self.task(task_id)?;
                    if t.state != TaskState::NotStarted || t.started || t.agent_id.is_some() {
                        return fail("conflict", "Task already has a node");
                    }
                } else {
                    self.manager(actor)?;
                }
                self.active(task_id)?;
                if self.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(task_id)
                    && run.launch.as_ref().is_some_and(|launch| matches!(launch.state,
                        ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching))) {
                    return fail("conflict", "Scheduled Task launch is pending or dispatching");
                }
                if !self.agents.contains_key(agent_id) {
                    return fail("not_found", "Agent missing");
                };
                if self.task(task_id)?.agent_id.as_ref() != Some(agent_id) {
                    if matches!(actor, Actor::Orchestrator { .. } | Actor::System) {
                        self.used_by_other(agent_id, task_id)?;
                    }
                    self.free(agent_id, task_id)?;
                    self.invalidate(task_id);
                    self.admit_node(task_id, agent_id, lifecycles)?;
                }
                if model.is_some() {
                    self.tasks.get_mut(task_id).unwrap().model = model.clone();
                }
            }
            Command::TitleRequest { request_id, title } => {
                self.manager(actor)?;
                nonempty(title)?;
                if title.chars().count() > REQUEST_TITLE_LIMIT {
                    return fail("invalid_input", "Request title is too long");
                }
                self.requests
                    .get_mut(request_id)
                    .ok_or_else(|| DomainError::new("not_found", "Request missing"))?
                    .title = Some(title.clone());
            }
            Command::RetitleTask { task_id, title } => {
                self.manager(actor)?;
                nonempty(title)?;
                self.task(task_id)?;
                self.tasks.get_mut(task_id).unwrap().title = title.clone();
            }
            Command::SetDependencies {
                task_id,
                dependencies,
                stack,
            } => {
                self.manager(actor)?;
                self.active(task_id)?;
                if self.schedule.runs.values().any(|run| run.task_id.as_ref() == Some(task_id)
                    && run.launch.as_ref().is_some_and(|launch| matches!(launch.state,
                        ScheduleLaunchState::Pending | ScheduleLaunchState::Dispatching))) {
                    return fail("conflict", "Scheduled Task launch is pending or dispatching");
                }
                self.invalidate(task_id);
                let t = self.tasks.get_mut(task_id).unwrap();
                t.dependencies = dependencies.clone();
                t.stack = stack.clone();
                self.dependency_graph()?;
            }
            Command::SetRelations { task_id, delivered_dependencies, blocks } => {
                self.manager(actor)?;
                self.active(task_id)?;
                for other in delivered_dependencies.iter().chain(blocks) {
                    if self.task(other)?.state == TaskState::Cancelled {
                        return refuse("invalid_input", "relations", "A cancelled task cannot be related: reopen it or leave it out");
                    }
                }
                for blocked in blocks {
                    let over = self.task(blocked)?;
                    if over.over() || self.finished(over) {
                        return refuse(
                            "invalid_input",
                            "blocks",
                            &format!("Task {blocked} is already over, so there is nothing to block or review: leave it out"),
                        );
                    }
                }
                if !blocks.is_empty() && self.task(task_id)?.pr_id.is_some() {
                    return refuse(
                        "invalid_input",
                        "blocks",
                        "A task with a registered pull request cannot block others: a reviewer delivers a verdict, not a pull request",
                    );
                }
                let reviewed = blocks.iter().filter_map(|blocked| self.tasks.get(blocked)).map(|blocked| blocked.priority).min();
                let t = self.tasks.get_mut(task_id).unwrap();
                t.delivered_dependencies = delivered_dependencies.clone();
                t.blocks = blocks.clone();
                if let Some(priority) = reviewed {
                    t.priority = priority;
                }
                self.dependency_graph()?;
            }
            Command::SetPriority { task_id, priority } => {
                self.manager(actor)?;
                self.active(task_id)?;
                let reviewers: Vec<TaskId> = self.tasks.values()
                    .filter(|other| other.blocks.contains(task_id) && !other.over())
                    .map(|other| other.id.clone()).collect();
                if std::iter::once(task_id).chain(&reviewers).all(|id| self.tasks[id].priority == *priority) {
                    return fail("unchanged", &format!("Task {} is already {} priority: nothing changed", task_id.0, format!("{priority:?}").to_lowercase()));
                }
                for id in std::iter::once(task_id).chain(&reviewers) {
                    self.tasks.get_mut(id).unwrap().priority = *priority;
                }
            }
            Command::RecordVerdict { task_id, presentation_id, verdict } => {
                let t = self.assigned(actor, task_id)?;
                if t.final_presentation.as_ref() != Some(presentation_id) {
                    return fail("acceptance_stale", "The verdict is for a delivery that is no longer the final one");
                }
                if !self.reviewing(t) {
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
                let reviewed_at = self.interaction(presentation_id)?.order;
                let blocked: Vec<TaskId> = t.blocks.iter().filter(|b| self.tasks.get(*b).is_some_and(|b| !b.over())).cloned().collect();
                for blocked_id in &blocked {
                    let delivered = self.task(blocked_id)?.final_presentation.as_ref().and_then(|id| self.interactions.get(id));
                    if delivered.is_some_and(|delivery| delivery.order > reviewed_at) {
                        return refuse(
                            "acceptance_stale",
                            "verdict",
                            "The task you review delivered again after your result: review its new delivery, present your result again and then give the verdict",
                        );
                    }
                }
                let capped = blocked.iter().any(|b| self.tasks[b].review_rounds >= MAX_REVIEW_ROUNDS);
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
                            let Some(delivery) = self.task(blocked_id)?.final_presentation.clone() else {
                                return refuse("invalid_input", "verdict", "The blocked task has delivered nothing to send back");
                            };
                            let already_sent_back = self.handed_back(&self.interactions[&delivery]).is_some();
                            self.invalidate(blocked_id);
                            let revision = self.revision + 1;
                            let p = self.interactions.get_mut(&delivery).unwrap();
                            if let InteractionPayload::Delivery { feedback, .. } = &mut p.payload {
                                feedback.push(Feedback {
                                    attachments: vec![],
                                    user_id: Feedback::ALONDRA.to_owned(),
                                    body: text.clone(),
                                    at_revision: revision,
                                    instruction: Instruction::Changes,
                                });
                            }
                            p.revision = revision;
                            if !already_sent_back {
                                self.tasks.get_mut(blocked_id).unwrap().review_rounds += 1;
                            }
                            self.notify(blocked_id, Some(delivery), &Instruction::Changes.message(text), effects)?;
                        }
                        self.tasks.get_mut(task_id).unwrap().verdict = Some(verdict);
                    }
                    Verdict::Approved { .. } => {
                        self.tasks.get_mut(task_id).unwrap().verdict = Some(verdict);
                        self.record_acceptance(presentation_id, Feedback::ALONDRA);
                    }
                }
            }
            Command::StartTask { task_id } => {
                self.task_editor(actor, task_id)?;
                self.not_ending(task_id)?;
                let t = self.task(task_id)?;
                if t.agent_id.is_none() {
                    return fail("not_assigned", "Task requires an Agent");
                };
                self.not_held_for_plan_review(task_id)?;
                let t = self.task(task_id)?;
                self.ready_to_start(t)?;
                self.node_has_run(task_id, lifecycles)?;
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
                    && self.requests.get(request).is_some_and(|r| r.maintenance)
                {
                    return refuse("maintenance_request", "scope.request_id", MAINTENANCE_GETS_NO_CARD);
                }
                if let Some(existing) = self.insert_interaction(
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
                self.insert_interaction(
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
            Command::AskQuestions { interaction_id, scope, recipient, blocking, why, questions } => {
                for (card_id, card, options) in group_cards(interaction_id, why, questions)? {
                    validate_options(&options)?;
                    self.insert_interaction(
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
                self.assigned(&Actor::Agent { agent_id: agent_id.clone() }, task_id)?;
                for (card_id, card, options) in group_cards(interaction_id, "", questions)? {
                    validate_options(&options)?;
                    self.insert_interaction(
                        &Actor::Agent { agent_id: agent_id.clone() }, &card_id, &scope, &Recipient::User, &card.need, true, &[],
                        InteractionPayload::Question { options, answer: None }, &card, Source::Hold,
                    )?;
                    self.holds.insert(card_id.clone(), Hold::Question);
                    created.push(card_id.0.clone());
                    effects.push(DomainEffect::NotifyUser { task_id: Some(task_id.clone()), interaction_id: card_id.clone() });
                }
            }
            Command::ReopenAnswer { interaction_id } => {
                user(actor)?;
                let i = self.interaction(interaction_id)?.clone();
                if i.card.group.is_none() {
                    return refuse("invalid_action_for_kind", "interaction_id", "Only a question asked together with others can be reopened");
                }
                if !Self::answered_question(&i) || i.withdrawn {
                    return refuse("invalid_action_for_kind", "interaction_id", "This question has no answer to take back");
                }
                if self.group_complete(&i) {
                    return refuse("question_already_answered", "interaction_id", "Every question is answered and its author has the answers: they are final");
                }
                if let Scope::Task { task_id } = &i.scope {
                    self.active(task_id)?;
                }
                let revision = self.revision + 1;
                let i = self.interactions.get_mut(interaction_id).unwrap();
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
                }
                let scope = Scope::Task { task_id: task_id.clone() };
                let winding_down = self.tasks.get(task_id).is_some_and(|t| Self::winding_down(t) && t.agent_id.as_ref() == Some(agent_id));
                if !winding_down {
                    self.assigned(&Actor::Agent { agent_id: agent_id.clone() }, task_id)?;
                }
                self.insert_interaction(
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
                self.holds.insert(interaction_id.clone(), *hold);
                created.push(interaction_id.0.clone());
                effects.push(DomainEffect::NotifyUser {
                    task_id: Some(task_id.clone()),
                    interaction_id: interaction_id.clone(),
                });
            }
            Command::Withdraw { interaction_id } => {
                let i = self.interaction(interaction_id)?;
                match (&i.author, actor) {
                    (Author::Agent { agent_id }, Actor::Agent { agent_id: a }) if agent_id == a => {}
                    (Author::Orchestrator { .. }, Actor::Orchestrator { .. }) => {}
                    (_, Actor::System) if self.holds.contains_key(interaction_id) => {}
                    (Author::System, Actor::System) => {}
                    (Author::Agent { .. }, Actor::System) if Self::asks_the_orchestrator(i) => {}
                    _ => return refuse("forbidden", "interaction_id", "Only a card's author withdraws it: withdraw only your own cards"),
                }
                let unanswered = match (&i.author, &i.scope, actor) {
                    (Author::Agent { agent_id }, Scope::Task { task_id }, Actor::System) if Self::asks_the_orchestrator(i) && !i.withdrawn =>
                        Some((agent_id.clone(), task_id.clone(), unanswered_withdrawn_notice(&i.card.need))),
                    _ => None,
                };
                if i.card.group.is_some() {
                    if self.group_complete(i) {
                        return refuse("question_already_answered", "interaction_id", "The user answered every question: they are final, act on the answers");
                    }
                } else if Self::answered_question(i) {
                    return refuse("question_already_answered", "interaction_id", "The user already answered this card: it is final, act on the answer");
                }
                if matches!(&i.payload, InteractionPayload::Delivery { acceptance: Some(a), .. } if a.valid) {
                    return refuse("invalid_action_for_kind", "interaction_id", "The user already accepted this delivery: it cannot be withdrawn");
                }
                if !i.withdrawn {
                    let presented = match (&i.scope, &i.payload) {
                        (Scope::Task { task_id }, InteractionPayload::Delivery { .. })
                            if self.tasks.get(task_id).is_some_and(|t| t.final_presentation.as_ref() == Some(interaction_id)) => Some(task_id.clone()),
                        _ => None,
                    };
                    let current = self.interactions[interaction_id].clone();
                    let siblings: Vec<InteractionId> = self.group_members(&current).iter().map(|member| member.id.clone()).collect();
                    self.retire(interaction_id);
                    for sibling in &siblings {
                        self.retire(sibling);
                        if let Some(InteractionPayload::Question { answer, .. } | InteractionPayload::ActionRequest { answer, .. }) =
                            self.interactions.get_mut(sibling).map(|member| &mut member.payload)
                        {
                            *answer = None;
                        }
                    }
                    if let Some(task) = presented {
                        self.invalidate(&task);
                        self.hand_back(&task, interaction_id, lifecycles)?;
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
                let i = self.interaction(interaction_id)?;
                if i.recipient != Recipient::User {
                    return refuse("forbidden", "interaction_id", "Ask for a clarification only on a card addressed to you");
                }
                if self.holds.contains_key(interaction_id) || matches!(i.author, Author::System) {
                    return refuse("invalid_action_for_kind", "interaction_id", "Alondra wrote this card from a Claude Code prompt: answer it or reply to the node instead");
                }
                if !self.open(i) {
                    return refuse("invalid_action_for_kind", "interaction_id", "This card is no longer open: there is nothing to clarify");
                }
                let message = format!(
                    "The user asks you to clarify card {interaction_id} ({}){}. Send its corrected version with card.replaces = \"{interaction_id}\", or withdraw it.",
                    match reason { ClarificationReason::Unclear => "it is unclear", ClarificationReason::Contradicts => "it contradicts something", ClarificationReason::MissingContext => "it lacks context" },
                    text.as_ref().map(|t| format!(": {}", t.trim())).unwrap_or_default(),
                );
                let (author, scope) = (i.author.clone(), i.scope.clone());
                let i = self.interactions.get_mut(interaction_id).unwrap();
                i.clarification = Some(Clarification { reason: *reason, text: text.clone(), user_id: u, at_revision: self.revision + 1 });
                i.revision = self.revision + 1;
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
                self.insert_interaction(
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
                let to_orchestrator = self.interactions.get(interaction_id).is_some_and(|i| i.recipient == Recipient::Orchestrator);
                let u = match actor {
                    Actor::System if to_orchestrator => ORCHESTRATOR_READER.to_owned(),
                    _ => user(actor)?,
                };
                let i = self
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
                    i.revision = self.revision + 1;
                }
            }
            Command::AnswerQuestion {
                attachments,
                interaction_id,
                option_id,
                inputs,
            } => {
                let i = self.interaction(interaction_id)?.clone();
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
                    _ => return fail("forbidden", "Only the intended recipient can answer"),
                };
                if i.card.kind == CardKind::Propose {
                    return refuse("suggestion_needs_decision", "interaction_id",
                        "A suggestion is settled with settle_suggestion and one decision: new_task (create it now), current_task (the node does it in its task) or discard (with the reason)");
                }
                if let Scope::Task { task_id } = &i.scope
                    && !self.tasks.get(task_id).is_some_and(Self::winding_down)
                {
                    self.active(task_id)?;
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
                    return refuse("question_already_answered", "interaction_id", "This card is already answered and its answer is final");
                };
                if i.withdrawn {
                    return fail("withdrawn", "Its author withdrew this question");
                }
                validate_answer(options, option_id, inputs, attachments)?;
                validate_attachments(attachments)?;
                let a = Answer {
                    attachments: attachments.clone(),
                    option_id: option_id.clone(),
                    inputs: inputs.clone(),
                    responder,
                    at_revision: self.revision + 1,
                    decision: None,
                };
                let i = self.interactions.get_mut(interaction_id).unwrap();
                match &mut i.payload {
                    InteractionPayload::Question { answer, .. }
                    | InteractionPayload::ActionRequest { answer, .. } => *answer = Some(a),
                    _ => unreachable!(),
                };
                i.revision = self.revision + 1;
                if let (Some(Hold::Plan), Scope::Task { task_id }) = (self.holds.get(interaction_id), &i.scope)
                    && option_id == Hold::APPROVE
                {
                    let checklist = Checklist { plan: interaction_id.clone(), items: checklist_items(&i.body) };
                    self.checklists.insert(task_id.clone(), checklist);
                }
                let written = inputs.values().find_map(|value| match value { InputValue::Text(text) => Some(text.clone()), _ => None });
                if !self.settle_claude_card(interaction_id, lifecycles)? {
                    self.answer_lifecycle_card(interaction_id, option_id, written.as_deref(), effects, lifecycles)?;
                }
                self.settle_start_problem(interaction_id, option_id, effects, lifecycles)?;
                self.settle_post_merge_problem(interaction_id, option_id, lifecycles)?;
                self.settle_plan_review_problem(interaction_id, option_id, lifecycles)?;
                if matches!(actor, Actor::User { .. }) {
                    self.hold_for_plan_review(interaction_id, effects, lifecycles)?;
                }
                // D05: the answer reaches whoever holds the Task now, not the
                // session that asked and may since have been replaced.
                let complete = self.group_complete(&self.interactions[interaction_id]);
                let i = self.interactions.get_mut(interaction_id).unwrap();
                match (i.author.clone(), i.scope.clone()) {
                    _ if self.holds.contains_key(interaction_id) || !complete => {}
                    (Author::Agent { .. }, Scope::Task { task_id }) => {
                        self.notify(&task_id, Some(interaction_id.clone()), "Question answered", effects)?;
                    }
                    (Author::Orchestrator { .. }, _) => effects.push(DomainEffect::NotifyOrchestrator {
                        interaction_id: Some(interaction_id.clone()),
                        request_id: None,
                    }),
                    _ => {}
                }
            }
            Command::SettleSuggestion { interaction_id, decision } => {
                self.settle_suggestion(actor, interaction_id, decision, effects, created, lifecycles)?;
            }
            Command::SettlePlanReview { review_id, task_id, outcome } => {
                self.settle_plan_review(actor, review_id, task_id, outcome, effects, created, lifecycles)?;
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
                self.assigned(actor, task_id)?;
                id(&document_id.0)?;
                id(&revision_id.0)?;
                nonempty(title)?;
                nonempty(role)?;
                digest(content_digest)?;
                nonempty(content_reference)?;
                if self.documents.contains_key(document_id)
                    || self.document_revisions.contains_key(revision_id)
                {
                    return fail("invalid_input", "Document or revision already exists");
                };
                self.documents.insert(
                    document_id.clone(),
                    Document {
                        id: document_id.clone(),
                        title: title.clone(),
                        current_revision: revision_id.clone(),
                    },
                );
                self.document_revisions.insert(
                    revision_id.clone(),
                    DocumentRevision {
                        id: revision_id.clone(),
                        document_id: document_id.clone(),
                        number: 1,
                        content_digest: content_digest.clone(),
                        content_reference: content_reference.clone(),
                    },
                );
                self.tasks
                    .get_mut(task_id)
                    .unwrap()
                    .documents
                    .insert(document_id.clone(), role.clone());
                self.document_reviews.push(DocumentReview {
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
                self.active(task_id)?;
                self.manager(actor)?;
                nonempty(role)?;
                if !self.documents.contains_key(document_id) {
                    return fail("not_found", "Document missing");
                };
                self.tasks
                    .get_mut(task_id)
                    .unwrap()
                    .documents
                    .insert(document_id.clone(), role.clone());
                if !self
                    .document_reviews
                    .iter()
                    .any(|r| &r.task_id == task_id && &r.document_id == document_id)
                {
                    self.document_reviews.push(DocumentReview {
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
                self.assigned(actor, task_id)?;
                if !self.task(task_id)?.documents.contains_key(document_id) {
                    return fail("forbidden", "Document not linked to Task");
                };
                id(&revision_id.0)?;
                digest(content_digest)?;
                nonempty(content_reference)?;
                if self.document_revisions.contains_key(revision_id) {
                    return fail("invalid_input", "Revision is immutable");
                };
                let doc = self
                    .documents
                    .get(document_id)
                    .ok_or_else(|| DomainError::new("not_found", "Document missing"))?;
                let number = self.document_revisions[&doc.current_revision]
                    .number
                    .checked_add(1)
                    .filter(|n| *n <= MAX_REVISION)
                    .ok_or_else(|| DomainError::new("invalid_input", "Revision overflow"))?;
                self.document_revisions.insert(
                    revision_id.clone(),
                    DocumentRevision {
                        id: revision_id.clone(),
                        document_id: document_id.clone(),
                        number,
                        content_digest: content_digest.clone(),
                        content_reference: content_reference.clone(),
                    },
                );
                self.documents
                    .get_mut(document_id)
                    .unwrap()
                    .current_revision = revision_id.clone();
                let affected: Vec<_> = self
                    .tasks
                    .values()
                    .filter(|t| t.documents.contains_key(document_id))
                    .map(|t| t.id.clone())
                    .collect();
                for t in affected {
                    self.invalidate(&t);
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
                self.active(task_id)?;
                if !self.task(task_id)?.documents.contains_key(document_id) {
                    return fail("forbidden", "Document not linked to Task");
                };
                if !self
                    .documents
                    .get(document_id)
                    .is_some_and(|d| &d.current_revision == revision_id)
                {
                    return fail("acceptance_stale", "Document revision changed");
                };
                let presentation = self.task(task_id)?.final_presentation.clone().filter(|id| {
                    self.presentation(id).is_ok_and(|(_, _, targets)| {
                        targets.contains(&DeliveryTarget::DocumentRevision {
                            document_id: document_id.clone(),
                            revision_id: revision_id.clone(),
                        })
                    })
                });
                self.document_decision(task_id, document_id, revision_id, presentation.clone(), *accepted, &u)?;
                if !accepted {
                    self.invalidate(task_id);
                } else if let Some(presentation) = &presentation {
                    self.decided(presentation);
                    self.settle(presentation, &u);
                }
                self.notify(
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
                if !self.assigned(actor, task_id)?.blocks.is_empty() {
                    return refuse(
                        "invalid_input",
                        "task_id",
                        "A task that blocks another reviews it and delivers a verdict, not a pull request: leave the code to the task you review, and say what to change in record_verdict, with serious_findings: how many failures you found break behaviour, lose data, or open a security or session hole (0 if none)",
                    );
                }
                id(&pr_id.0)?;
                if !crate::views::id::valid(&pr_id.0) {
                    return refuse(
                        "invalid_input",
                        "pr_id",
                        &format!(
                            "pr_id \"{}\" cannot be addressed by the apps: use a short id such as \"pr-{number}\" (letters, digits, - and _) or github.com/owner/repo#N in lowercase",
                            pr_id.0
                        ),
                    );
                }
                canonical_pr(host, repository, *number)?;
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
                if let Some(existing) = self.task(task_id)?.pr_id.clone() {
                    let p = &self.pull_requests[&existing];
                    if p.repository != *repository || p.number != *number {
                        return fail("task_already_has_pr", &one_pr_per_task(p.number));
                    }
                    if existing == *pr_id
                        && p.host == *host
                        && p.head == *sha
                        && p.base == *base
                        && p.allowed_methods == *allowed_methods
                    {
                        return Ok(());
                    }
                    if p.state != PrState::Open {
                        return fail("task_already_has_pr", &one_pr_per_task(p.number));
                    }
                    let corrected = self.pull_requests.get_mut(&existing).unwrap();
                    corrected.host = host.clone();
                    corrected.head = sha.clone();
                    corrected.base = base.clone();
                    corrected.allowed_methods = allowed_methods.clone();
                    return Ok(());
                };
                if self.pull_requests.values().any(|p| {
                    p.id == *pr_id
                        || (p.host == *host && p.repository == *repository && p.number == *number)
                }) {
                    return fail("pr_owned_by_other_task", "PR is already registered");
                };
                let review = self
                    .task(task_id)?
                    .review_prs
                    .then(|| PullRequestReview { decisions: vec![] });
                self.pull_requests.insert(
                    pr_id.clone(),
                    PullRequest {
                        id: pr_id.clone(),
                        task_id: task_id.clone(),
                        host: host.clone(),
                        repository: repository.clone(),
                        number: *number,
                        head: sha.clone(),
                        base: base.clone(),
                        state: PrState::Open,
                        merged_head: None,
                        allowed_methods: allowed_methods.clone(),
                        review,
                    },
                );
                self.tasks.get_mut(task_id).unwrap().pr_id = Some(pr_id.clone());
                created.push(pr_id.0.clone());
            }
            Command::LinkInteractionSubject {
                interaction_id,
                subject,
            } => {
                let i = self.interaction(interaction_id)?;
                let Scope::Task { task_id } = &i.scope else {
                    return fail("invalid_input", "Subject link requires Task scope");
                };
                self.assigned(actor, task_id)?;
                if !matches!((&i.author,actor),(Author::Agent{agent_id:a},Actor::Agent{agent_id:b}) if a==b)
                {
                    return fail(
                        "forbidden",
                        "Only the Interaction author can link its subject",
                    );
                };
                self.subject(task_id, subject)?;
                if let Some(existing) = &i.subject {
                    if existing != subject {
                        return fail("subject_already_linked", "Interaction subject is immutable");
                    }
                    return Ok(());
                }
                let i = self.interactions.get_mut(interaction_id).unwrap();
                i.subject = Some(subject.clone());
                i.revision = self.revision + 1;
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
                self.assigned(actor, task_id)?;
                if *nothing_to_review && (!*is_final || targets.iter().any(|t| !matches!(t, DeliveryTarget::Text { .. }))) {
                    return refuse("invalid_input", "nothing_to_review",
                        "nothing_to_review is only for a final delivery without a pull request or document: it closes the task without a card");
                }
                self.current_pr_targets(task_id, targets)?;
                self.targets(task_id, targets)?;
                if *is_final && self.blockers(task_id) {
                    let open = self.interactions.values()
                        .filter(|i| i.scope == (Scope::Task { task_id: task_id.clone() }) && i.blocking && self.open(i))
                        .map(|i| i.id.0.clone()).collect::<Vec<_>>().join(", ");
                    return refuse("blocking_open", "is_final", &format!("Card {open} still waits on the user: wait for its answer or withdraw it before the final delivery"));
                }
                if let Some(pr) = self.task(task_id)?.pr_id.as_ref().map(|id| &self.pull_requests[id])
                    && *is_final
                    && pr.state == PrState::Open
                    && !targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. } if *pr_id == pr.id))
                {
                    return refuse("pr_missing", "targets", &format!(
                        "Your PR #{} is open: present it in the final delivery with its head {} so it is merged when the user accepts", pr.number, pr.head));
                }
                self.checklist_marks_cover_the_plan(task_id, *is_final, checklist, attachments)?;
                if let Some(plan) = &card.plan {
                    if !*is_final || *nothing_to_review || targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { .. })) {
                        return refuse("invalid_input", "plan",
                            "a plan goes only on a final result without a pull request: the user approves it to start a new task that implements it, or only archives the result");
                    }
                    plan_checklist(plan)?;
                }
                let mut card = card.clone();
                if targets.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. }
                    if self.pull_requests.get(pr_id).is_some_and(|p| p.state != PrState::Open)))
                {
                    card.kind = CardKind::Know;
                }
                self.insert_interaction(
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
                if *is_final && self.interactions.values().any(|i| self.asked_aside(i, task_id)) {
                    let open = self.interactions.values()
                        .filter(|i| self.asked_aside(i, task_id))
                        .map(|i| format!("“{}”", i.card.need)).collect::<Vec<_>>().join(", ");
                    return refuse("unanswered_questions", "is_final", &format!(
                        "You have unanswered questions ({open}): wait for the answers before you deliver, or withdraw the ones you no longer need"));
                }
                if !checklist.is_empty() {
                    self.checklist_marks.insert(interaction_id.clone(), checklist.clone());
                }
                if *is_final {
                    self.invalidate(task_id);
                    let standing = self.lifecycle(lifecycles, task_id)?.delivery().cloned();
                    if let Some(previous) = standing.filter(|previous| previous != interaction_id) {
                        self.hand_back(task_id, &previous, lifecycles)?;
                    }
                    self.node_has_run(task_id, lifecycles)?;
                    self.decide(lifecycles, task_id, LifecycleRecord::Presented { delivery: interaction_id.clone() })?;
                    let t = self.tasks.get_mut(task_id).unwrap();
                    t.final_presentation = Some(interaction_id.clone());
                    t.verdict = None;
                    self.blocks_acyclic(interaction_id)?;
                    let reviewers: Vec<TaskId> = self.open_blockers(task_id).into_iter()
                        .filter(|b| self.tasks.get(b).is_some_and(|b| b.started))
                        .collect();
                    for reviewer in reviewers {
                        self.notify(&reviewer, Some(interaction_id.clone()), "The task you review delivered again: review its new final delivery, present your result again and then give your verdict.", effects)?;
                    }
                    if *nothing_to_review {
                        self.accept(interaction_id, Feedback::ALONDRA)?;
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
            } => {
                let (u, task) = self.feedback_guard(actor, presentation_id, targets, body, attachments, *instruction)?;
                self.invalidate(&task);
                let p = self.interactions.get_mut(presentation_id).unwrap();
                if let InteractionPayload::Delivery { feedback, .. } = &mut p.payload {
                    feedback.push(Feedback {
                        attachments: attachments.clone(),
                        user_id: u,
                        body: body.clone(),
                        at_revision: self.revision + 1,
                        instruction: *instruction,
                    });
                }
                p.revision = self.revision + 1;
                self.notify(&task, Some(presentation_id.clone()), &instruction.message(body), effects)?;
            }
            Command::AcceptDelivery {
                presentation_id,
                targets,
            } => {
                let u = self.accept_delivery_guard(actor, presentation_id, targets, self)?;
                for (pr_id, head, method) in self.accepted_merges(presentation_id, targets)? {
                    let authorization = self.acceptance_authorization(pr_id);
                    self.authorize_merge(&authorization, presentation_id, pr_id, head, method, &u, true)?;
                    created.push(authorization.0);
                }
                self.accept(presentation_id, &u)?;
            }
            Command::DecideProposedPlan { presentation_id, targets, choice } => {
                let (u, plan) = self.decide_plan_guard(actor, presentation_id, targets, *choice, self)?;
                let source = self.presentation(presentation_id)?.1.clone();
                self.accept(presentation_id, &u)?;
                if *choice == PlanChoice::Implement {
                    let request_id = guards::implementation_request(presentation_id);
                    let auto_merge = self.requests.get(&source.request_id).is_some_and(|r| r.auto_merge);
                    self.requests.insert(request_id.clone(), Request { auto_merge, ..Request::new(request_id.clone(), plan) });
                    self.request_plans.insert(request_id.clone(), ApprovedPlan { task_id: source.id.clone(), presentation_id: presentation_id.clone() });
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
            } => {
                match self.merge_guard(actor, authorization_id, presentation_id, pr_id, expected_head, *method, self)? {
                    None => {
                        self.authorize_merge(authorization_id, presentation_id, pr_id, expected_head, *method, Feedback::ALONDRA, false)?;
                    }
                    Some(u) => {
                        self.authorize_merge(authorization_id, presentation_id, pr_id, expected_head, *method, &u, true)?;
                        self.settle(presentation_id, &u);
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
                let a = self
                    .merge_authorizations
                    .get(authorization_id)
                    .ok_or_else(|| DomainError::new("not_found", "Authorization missing"))?
                    .clone();
                if matches!(actor, Actor::Agent { .. }) {
                    return fail("forbidden", ALONDRA_MERGES);
                }
                system(actor)?;
                self.active(&a.task_id)?;
                self.presentation(&a.presentation_id)?;
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
                let pr = &self.pull_requests[&a.pr_id];
                if pr.state != PrState::Open || pr.head != a.head || remote_head != &a.head {
                    return fail("stale_head", "Approved and remote head differ");
                };
                if !base_verified || remote_base != &pr.base {
                    return fail("stack_not_ready", "Remote base must be verified");
                };
                let t = self.task(&a.task_id)?;
                self.stack_ready(t)?;
                if let Some(s) = &t.stack {
                    let upstream = self.pull_requests.get(&s.pr_id).ok_or_else(|| {
                        DomainError::new("stack_not_ready", "Upstream PR missing")
                    })?;
                    if upstream.state != PrState::Merged
                        || upstream.merged_head.as_ref() != Some(&s.head)
                        || upstream.base != pr.base
                    {
                        return fail(
                            "stack_not_ready",
                            "Upstream merge and retarget must be confirmed",
                        );
                    }
                }
                let a = self.merge_authorizations.get_mut(authorization_id).unwrap();
                a.attempt = MergeAttemptState::Claimed;
                a.claimed_by = None;
            }
            Command::ObserveMergeAttempt {
                authorization_id,
                state,
            } => {
                system(actor)?;
                let a = self
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
                let p = self
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
                let number = p.number;
                if p.head != *sha {
                    self.invalidate(&task);
                }
                let p = self.pull_requests.get_mut(pr_id).unwrap();
                p.head = sha.clone();
                p.base = base.clone();
                p.state = *state;
                p.merged_head = merged_head.clone();
                if *state == PrState::Merged {
                    for a in self.merge_authorizations.values_mut().filter(|a| {
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
                // A merge that finishes the Task is told by the cleanup request alone.
                let closing = self.task(&task).is_ok_and(|t| self.finished(t) && self.wraps_up(t));
                if newly_merged && self.active(&task).is_ok() && !closing {
                    let left = self.unmerged_targets(&task);
                    self.notify(&task, None, &merged_notice(number, left), effects)?;
                }
            }
            Command::ObservePrMergeMethods {
                pr_id,
                allowed_methods,
            } => {
                system(actor)?;
                if allowed_methods.is_empty() {
                    return fail("invalid_input", "Merge methods required");
                };
                let p = self
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
                self.assigned(actor, task_id)?;
                let (_, t, targets) = self.presentation(presentation_id)?;
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
                self.stack_readiness.insert(
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
                self.active(task_id)?;
                let (p, t, targets) = self.presentation(presentation_id)?;
                if &t.id != task_id {
                    return fail("forbidden", "Presentation belongs to another Task");
                };
                let auto_merged = self.requests.get(&t.request_id).is_some_and(|r| r.auto_merge)
                    && matches!(&p.payload, InteractionPayload::Delivery { acceptance: None, feedback, .. } if feedback.is_empty())
                    && targets.iter().any(|x| matches!(x, DeliveryTarget::PullRequest { .. }));
                if !self.accepted(p) && !auto_merged {
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
                self.completable(t, targets, self.accepted(p))?;
                if !self.wraps_up(t) && !self.unanswered(task_id) {
                    self.finish_task(task_id, lifecycles)?;
                }
            }
            Command::CancelTask { task_id } => {
                self.cancel_guard(actor, task_id)?;
                self.cancel(task_id, effects, lifecycles)?;
            }
            Command::ReopenTask { task_id } => {
                self.reopen_guard(actor, task_id)?;
                let task = &self.tasks[task_id];
                let released = task.agent_id.as_ref().is_some_and(|a| self.busy_with(a, task_id).is_some()
                    || self.agents.get(a).is_none_or(|agent| !agent.phase.live()));
                self.invalidate(task_id);
                let t = self.tasks.get_mut(task_id).unwrap();
                t.archived = false;
                let node_gone = matches!(t.wrap_up.node, Some(NodeEnd::Closed | NodeEnd::Gone));
                t.wrap_up = WrapUp::default();
                let kept = self.tasks[task_id].agent_id.clone().filter(|_| !released && !node_gone);
                let record = LifecycleRecord::Reopened { input: self.next_input(task_id) };
                self.decide(lifecycles, task_id, record)?;
                self.tasks.get_mut(task_id).unwrap().final_presentation = None;
                if let Some(node) = kept {
                    self.decide(lifecycles, task_id, LifecycleRecord::Admitted { node, execution: None })?;
                    self.decide(lifecycles, task_id, LifecycleRecord::ResourceAvailable)?;
                }
            }
            Command::ConfirmCleanup { task_id } => {
                let t = self.assigned(actor, task_id)?;
                if t.wrap_up.asked_at.is_none() {
                    return fail("not_closing", &format!(
                        "Task {} is not closing, so there is nothing to clean up yet: deliver your result with present_result first.", task_id.0));
                }
                let revision = self.revision + 1;
                let wrap_up = &mut self.tasks.get_mut(task_id).unwrap().wrap_up;
                wrap_up.cleaned_at = Some(revision);
                self.end_post_merge(task_id, PostMergeEnd::Cleanup, lifecycles)?;
                self.drain(task_id, lifecycles, |owed| matches!(owed, Drained::Cleanup { .. }))?;
            }
            Command::DeclarePostMergeWork { task_id, reason } => {
                self.assigned(actor, task_id)?;
                let declared_at = self.revision + 1;
                let work = reason.as_deref().map(str::trim).filter(|reason| !reason.is_empty())
                    .map(|reason| PostMergeWork { reason: clip(reason, POST_MERGE_WORK_MAX), declared_at });
                match work {
                    Some(work) => self.declare_post_merge(task_id, work, lifecycles)?,
                    None => self.end_post_merge(task_id, PostMergeEnd::NodeDeclaredEnd, lifecycles)?,
                }
            }
            Command::RecordNodeEnd { task_id, end, agent_id } => {
                system(actor)?;
                let t = self.task(task_id)?;
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
                let closing = self.in_cleanup(t) && self.wraps_up(t);
                if !closing && !t.state.terminal() && !t.ending() {
                    if matches!(end, NodeEnd::Closed | NodeEnd::Gone) && t.agent_id.is_some() {
                        self.end_post_merge(task_id, PostMergeEnd::SessionOver, lifecycles)?;
                        self.release(task_id, lifecycles)?;
                        self.tasks.get_mut(task_id).unwrap().wrap_up = WrapUp::default();
                        return Ok(());
                    }
                    return fail("not_closing", &format!("Task {} is not closing", task_id.0));
                }
                let fact = match end {
                    NodeEnd::Gone => PostMergeEnd::SessionOver,
                    NodeEnd::KeptByUser | NodeEnd::UsersOwn => PostMergeEnd::UserDecision,
                    NodeEnd::Closed if t.cancelled() => PostMergeEnd::UserDecision,
                    NodeEnd::Closed => {
                        if self.lifecycle(lifecycles, task_id)?.post_merge().is_some() {
                            return fail("post_merge_running", &format!(
                                "Task {}'s node still runs work after the merge: only the node, its cleanup or the user ends it, never Alondra closing it", task_id.0));
                        }
                        PostMergeEnd::SessionOver
                    }
                };
                self.end_post_merge(task_id, fact, lifecycles)?;
                self.tasks.get_mut(task_id).unwrap().wrap_up.node = Some(*end);
                if self.tasks[task_id].ending() {
                    self.drain(task_id, lifecycles, |_| true)?;
                } else if self.lifecycle(lifecycles, task_id)?.execution().is_some() {
                    self.release(task_id, lifecycles)?;
                }
            }
            Command::ReleaseTask { task_id, reason } => {
                system(actor)?;
                nonempty(reason)?;
                let task = self.active(task_id)?;
                if task.ending() {
                    return fail("task_not_active", "Task is ending: its node is drained by its receipts");
                }
                if task.agent_id.is_none() || task.final_presentation.is_some() {
                    return fail("conflict", "Task has no working Agent to release");
                }
                self.release(task_id, lifecycles)?;
            }
            Command::AdmitExecution { task_id, agent_id, conversation, instance, host } => {
                system(actor)?;
                self.not_held_for_plan_review(task_id)?;
                self.not_ending(task_id)?;
                self.active(task_id)?;
                if !self.agents.contains_key(agent_id) {
                    return fail("not_found", "Agent missing");
                }
                if self.task(task_id)?.agent_id.as_ref() != Some(agent_id) {
                    self.used_by_other(agent_id, task_id)?;
                    self.free(agent_id, task_id)?;
                }
                self.invalidate(task_id);
                let identity = ExecutionIdentity { conversation: conversation.clone(), instance: instance.clone(), host: host.clone() };
                self.admit_execution(task_id, agent_id, identity, lifecycles)?;
            }
            Command::RecordRuntimeFacts { task_id, generation, facts } => {
                system(actor)?;
                self.task(task_id)?;
                self.record_runtime_facts(task_id, *generation, facts, lifecycles)?;
            }
            Command::RecordTurnBoundary { task_id, generation, turn, failure } => {
                system(actor)?;
                self.task(task_id)?;
                nonempty(turn)?;
                self.record_turn_boundary(task_id, *generation, turn, failure.as_ref(), effects, lifecycles)?;
            }
            Command::RecordHostEnded { task_id, generation } => {
                system(actor)?;
                self.task(task_id)?;
                self.record_host_ended(task_id, *generation, effects, lifecycles)?;
            }
            Command::RecordDoubtfulHandoff { task_id, generation, input, text } => {
                system(actor)?;
                self.task(task_id)?;
                nonempty(input)?;
                nonempty(text)?;
                self.record_doubtful_handoff(task_id, *generation, input, text, effects, lifecycles)?;
            }
            Command::ResendUntakenInput { task_id, generation, input, text } => {
                system(actor)?;
                self.task(task_id)?;
                nonempty(input)?;
                nonempty(text)?;
                self.resend_untaken(task_id, *generation, input, text, lifecycles)?;
            }
            Command::ProbeClaudeAccess { hold } => {
                system(actor)?;
                if *hold == ClaudeHold::SignedOut {
                    return fail("invalid_input", "Only the user's answer probes Claude's sign-in");
                }
                if self.probe_claude(*hold, lifecycles)?.is_none() {
                    return fail("not_found", "No task waits for Claude");
                }
            }
            Command::RecordTaskCleanup { task_id } => {
                system(actor)?;
                let t = self.task(task_id)?;
                if !t.ending() {
                    return fail("not_closing", &format!("Task {} is not ending", task_id.0));
                }
                if self.wraps_up(t) {
                    return fail("conflict", "The task still has its node: its end is the receipt");
                }
                self.drain(task_id, lifecycles, |_| true)?;
            }
            Command::ArchiveTask { task_id, archived } => {
                self.archive_guard(actor, task_id, *archived)?;
                self.tasks.get_mut(task_id).unwrap().archived = *archived;
            }
        }
        Ok(())
    }
    fn subject(&self, task: &TaskId, subject: &InteractionSubject) -> Result<()> {
        match subject {
            InteractionSubject::PullRequest { pr_id } => {
                if !self
                    .pull_requests
                    .get(pr_id)
                    .is_some_and(|p| &p.task_id == task)
                {
                    return fail("forbidden", "PR belongs to another Task");
                }
            }
            InteractionSubject::DocumentReview {
                document_id,
                revision_id,
            } => {
                if !self.task(task)?.documents.contains_key(document_id)
                    || !self
                        .document_revisions
                        .get(revision_id)
                        .is_some_and(|r| &r.document_id == document_id)
                {
                    return fail("forbidden", "Document revision is not linked to this Task");
                }
            }
        }
        Ok(())
    }
    fn document_decision(
        &mut self,
        task: &TaskId,
        doc: &DocumentId,
        rev: &DocumentRevisionId,
        presentation: Option<InteractionId>,
        accepted: bool,
        u: &str,
    ) -> Result<()> {
        let r = self
            .document_reviews
            .iter_mut()
            .find(|r| &r.task_id == task && &r.document_id == doc)
            .ok_or_else(|| DomainError::new("not_found", "Document review missing"))?;
        r.decisions.push(DocumentDecision {
            revision_id: rev.clone(),
            presentation_id: presentation,
            accepted,
            user_id: u.into(),
            at_revision: self.revision + 1,
        });
        Ok(())
    }
    /// The user's go-ahead to merge one presented PR head, which Alondra then merges.
    fn authorize_merge(
        &mut self,
        authorization_id: &AuthorizationId,
        presentation_id: &InteractionId,
        pr_id: &PrId,
        expected_head: &str,
        method: MergeMethod,
        u: &str,
        consent: bool,
    ) -> Result<TaskId> {
        let task = self.authorize_merge_guard(authorization_id, presentation_id, pr_id, expected_head, method, self)?;
        for a in self.merge_authorizations.values_mut().filter(|a| a.pr_id == *pr_id) {
            a.valid = false;
        }
        self.merge_authorizations.insert(
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
                at_revision: self.revision + 1,
            },
        );
        if let Some(r) = self.pull_requests.get_mut(pr_id).unwrap().review.as_mut().filter(|_| consent) {
            r.decisions.push(PrDecision {
                presentation_id: presentation_id.clone(),
                head: expected_head.to_owned(),
                accepted: true,
                user_id: u.to_owned(),
                at_revision: self.revision + 1,
            });
        }
        self.decided(presentation_id);
        Ok(task)
    }
    fn accept(&mut self, presentation: &InteractionId, u: &str) -> Result<()> {
        self.acceptance_guard(presentation)?;
        let (_, t, targets) = self.presentation(presentation)?;
        let task = t.id.clone();
        let targets = targets.clone();
        for target in targets {
            if let DeliveryTarget::DocumentRevision {
                document_id,
                revision_id,
            } = target
            {
                self.document_decision(
                    &task,
                    &document_id,
                    &revision_id,
                    Some(presentation.clone()),
                    true,
                    u,
                )?;
            }
        }
        self.record_acceptance(presentation, u);
        Ok(())
    }
    fn record_acceptance(&mut self, presentation: &InteractionId, u: &str) {
        let p = self.interactions.get_mut(presentation).unwrap();
        if let InteractionPayload::Delivery { acceptance, .. } = &mut p.payload {
            *acceptance = Some(Acceptance {
                user_id: u.into(),
                at_revision: self.revision + 1,
                valid: true,
            });
        }
        p.revision = self.revision + 1;
    }
    /// Structural validation is also required after loading relational rows.
    /// Historical presentations retain old document revisions and heads deliberately.
    pub fn validate(&self) -> Result<()> {
        id(&self.data_epoch.0)?;
        if self.revision > MAX_REVISION || self.orchestrator_generation > MAX_REVISION {
            return fail("invalid_input", "Unsafe revision or generation");
        };
        self.schedule.validate()?;
        self.validate_plan_reviews()?;
        for run in self.schedule.runs.values() {
            if let (Some(request_id), Some(task_id)) = (&run.request_id, &run.task_id) {
                if !self.requests.contains_key(request_id)
                    || !self
                        .tasks
                        .get(task_id)
                        .is_some_and(|task| &task.request_id == request_id)
                {
                    return fail("invalid_input", "Schedule run has mismatched Request or Task");
                }
            }
        }
        for (k, r) in &self.requests {
            id(&k.0)?;
            if k != &r.id {
                return fail("invalid_input", "Request identity mismatch");
            };
            nonempty(&r.body)?;
        }
        for (k, a) in &self.agents {
            id(&k.0)?;
            if k != &a.id {
                return fail("invalid_input", "Agent identity mismatch");
            };
            nonempty(&a.name)?;
        }
        for (k, t) in &self.tasks {
            id(&k.0)?;
            nonempty(&t.title)?;
            if k != &t.id
                || !self.requests.contains_key(&t.request_id)
                || t.revision > self.revision
                || t.agent_id
                    .as_ref()
                    .is_some_and(|a| !self.agents.contains_key(a))
            {
                return fail("invalid_input", "Invalid Task relation or revision");
            };
            if t.archived && !t.state.terminal() {
                return fail("invalid_input", "Archived Task is still active");
            };
            for (d, role) in &t.documents {
                nonempty(role)?;
                if !self.documents.contains_key(d) {
                    return fail("not_found", "Linked document missing");
                }
            }
            if let Some(p) = &t.pr_id {
                if !self.pull_requests.get(p).is_some_and(|p| &p.task_id == k) {
                    return fail("invalid_input", "Task PR ownership mismatch");
                }
            }
            if let Some(i) = &t.final_presentation {
                let i = self.interaction(i)?;
                if i.scope != (Scope::Task { task_id: k.clone() })
                    || !matches!(
                        i.payload,
                        InteractionPayload::Delivery { is_final: true, .. }
                    )
                {
                    return fail(
                        "invalid_input",
                        "Final presentation must be a final delivery of the same Task",
                    );
                }
            }
            if let Some(s) = &t.stack {
                let up = self.task(&s.upstream_task)?;
                if up.pr_id.as_ref()!=Some(&s.pr_id) || !self.interactions.get(&s.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:s.upstream_task.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:s.pr_id.clone(),head:s.head.clone()}))){return fail("invalid_input","Stack references wrong upstream presentation")}
            }
        }
        self.dependency_graph()?;
        let mut numbers = BTreeSet::new();
        for (k, r) in &self.document_revisions {
            id(&k.0)?;
            if k != &r.id
                || !self.documents.contains_key(&r.document_id)
                || r.number == 0
                || r.number > MAX_REVISION
                || !numbers.insert((r.document_id.clone(), r.number))
            {
                return fail("invalid_input", "Invalid document revision identity");
            };
            digest(&r.content_digest)?;
            nonempty(&r.content_reference)?;
        }
        for (k, d) in &self.documents {
            id(&k.0)?;
            nonempty(&d.title)?;
            if k != &d.id
                || !self
                    .document_revisions
                    .get(&d.current_revision)
                    .is_some_and(|r| &r.document_id == k)
            {
                return fail("invalid_input", "Document current revision mismatch");
            }
        }
        let mut reviews = BTreeSet::new();
        for r in &self.document_reviews {
            if !reviews.insert((r.task_id.clone(), r.document_id.clone()))
                || !self
                    .task(&r.task_id)?
                    .documents
                    .contains_key(&r.document_id)
            {
                return fail("invalid_input", "Invalid or duplicate document review");
            };
            for d in &r.decisions {
                if !self
                    .document_revisions
                    .get(&d.revision_id)
                    .is_some_and(|v| v.document_id == r.document_id)
                    || d.at_revision > self.revision
                {
                    return fail("invalid_input", "Document decision revision mismatch");
                };
                id(&d.user_id)?;
                if let Some(p) = &d.presentation_id {
                    let i = self.interaction(p)?;
                    if i.scope
                        != (Scope::Task {
                            task_id: r.task_id.clone(),
                        })
                        || !matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::DocumentRevision{document_id:r.document_id.clone(),revision_id:d.revision_id.clone()}))
                    {
                        return fail("invalid_input", "Document decision presentation mismatch");
                    }
                }
            }
        }
        for t in self.tasks.values() {
            for d in t.documents.keys() {
                if !reviews.contains(&(t.id.clone(), d.clone())) {
                    return fail("invalid_input", "Linked document lacks review");
                }
            }
        }
        let mut identities = BTreeSet::new();
        let mut owners = BTreeSet::new();
        for (k, p) in &self.pull_requests {
            id(&k.0)?;
            canonical_pr(&p.host, &p.repository, p.number)?;
            head(&p.head)?;
            nonempty(&p.base)?;
            let t = self.task(&p.task_id)?;
            if k != &p.id
                || t.pr_id.as_ref() != Some(k)
                || !owners.insert(p.task_id.clone())
                || !identities.insert((&p.host, &p.repository, p.number))
                || p.review.is_some() != t.review_prs
                || p.allowed_methods.is_empty()
            {
                return fail(
                    "invalid_input",
                    "Invalid lifetime PR identity or review policy",
                );
            };
            if (p.state == PrState::Merged) != p.merged_head.is_some() {
                return fail("invalid_input", "Merge state lacks evidence");
            };
            if let Some(h) = &p.merged_head {
                head(h)?;
            }
            if let Some(r) = &p.review {
                for d in &r.decisions {
                    id(&d.user_id)?;
                    head(&d.head)?;
                    if d.at_revision>self.revision || !self.interactions.get(&d.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:p.task_id.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:k.clone(),head:d.head.clone()}))){return fail("invalid_input","PR review decision does not match presentation")}
                }
            }
        }
        for (k, i) in &self.interactions {
            id(&k.0)?;
            content(&i.body, &i.attachments)?;
            nonempty(&i.card.need)?;
            if k != &i.id || i.revision > self.revision || i.order > self.revision {
                return fail("invalid_input", "Invalid Interaction identity/revision");
            };
            self.validate_scope(&i.scope)?;
            self.recipient(&i.recipient)?;
            match &i.author {
                Author::Agent { agent_id } => {
                    if !self.agents.contains_key(agent_id)
                        || !(matches!(i.scope, Scope::Task { .. })
                            || matches!(i.payload, InteractionPayload::Information { .. }))
                    {
                        return fail("invalid_input", "Agent authored Interaction without Task");
                    }
                }
                Author::Orchestrator { generation }
                    if *generation > self.orchestrator_generation =>
                {
                    return fail("stale_generation", "Future author generation");
                }
                _ => {}
            };
            if let Some(s) = &i.subject {
                let Scope::Task { task_id } = &i.scope else {
                    return fail("invalid_input", "Subject requires Task scope");
                };
                self.subject(task_id, s)?;
            }
            match &i.payload {
                InteractionPayload::Information { read_by } => {
                    if i.blocking {
                        return fail("invalid_input", "Information cannot block");
                    };
                    for u in read_by {
                        id(u)?;
                    }
                }
                InteractionPayload::Question { options, answer }
                | InteractionPayload::ActionRequest { options, answer } => {
                    validate_options(options)?;
                    if let Some(a) = answer {
                        validate_attachments(&a.attachments)?;
                        validate_answer(options, &a.option_id, &a.inputs, &a.attachments)?;
                        id(&a.responder)?;
                        if a.at_revision > self.revision {
                            return fail("invalid_input", "Answer revision from future");
                        }
                    }
                }
                InteractionPayload::Delivery {
                    targets,
                    acceptance,
                    feedback,
                    ..
                } => {
                    let Scope::Task { task_id } = &i.scope else {
                        return fail("invalid_input", "Delivery must have Task scope");
                    };
                    if i.blocking
                        || i.recipient != Recipient::User
                        || targets.is_empty()
                        || targets.len() > 64
                    {
                        return fail("invalid_input", "Invalid delivery payload");
                    };
                    let t = self.task(task_id)?;
                    let mut seen = BTreeSet::new();
                    for target in targets {
                        if !seen.insert(serde_json::to_string(target).unwrap()) {
                            return fail("invalid_input", "Duplicate target");
                        };
                        match target {
                            DeliveryTarget::Text { text } => nonempty(text)?,
                            DeliveryTarget::DocumentRevision {
                                document_id,
                                revision_id,
                            } => {
                                if !t.documents.contains_key(document_id)
                                    || !self
                                        .document_revisions
                                        .get(revision_id)
                                        .is_some_and(|r| &r.document_id == document_id)
                                {
                                    return fail(
                                        "invalid_input",
                                        "Historical document target mismatch",
                                    );
                                }
                            }
                            DeliveryTarget::PullRequest { pr_id, head: sha } => {
                                head(sha)?;
                                if !self
                                    .pull_requests
                                    .get(pr_id)
                                    .is_some_and(|p| &p.task_id == task_id)
                                {
                                    return fail("invalid_input", "Historical PR target mismatch");
                                }
                            }
                        }
                    }
                    if let Some(a) = acceptance {
                        id(&a.user_id)?;
                        if a.at_revision > self.revision {
                            return fail("invalid_input", "Acceptance revision from future");
                        };
                        if a.valid && t.final_presentation.as_ref() != Some(k) {
                            return fail(
                                "invalid_input",
                                "Noncurrent delivery has valid acceptance",
                            );
                        }
                    }
                    for f in feedback {
                        id(&f.user_id)?;
                        content(&f.body, &f.attachments)?;
                        if f.at_revision > self.revision {
                            return fail("invalid_input", "Feedback revision from future");
                        }
                    }
                }
            }
        }
        for (k, a) in &self.merge_authorizations {
            id(&k.0)?;
            id(&a.user_id)?;
            head(&a.head)?;
            if k!=&a.id || a.at_revision>self.revision || !self.pull_requests.get(&a.pr_id).is_some_and(|p|p.task_id==a.task_id) || !self.interactions.get(&a.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:a.task_id.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:a.pr_id.clone(),head:a.head.clone()}))){return fail("invalid_input","Merge authorization relation mismatch")};
            if let Some(agent) = &a.claimed_by {
                if !self.agents.contains_key(agent) {
                    return fail("not_found", "Merge attempt Agent missing");
                }
            }
        }
        for (task_id, checklist) in &self.checklists {
            let Some(task) = self.tasks.get(task_id) else { return fail("invalid_input", "Invalid checklist relation") };
            let held_in_its_request = self.holds.get(&checklist.plan) == Some(&Hold::Plan)
                && self.interactions.get(&checklist.plan).is_some_and(|i| matches!(&i.scope,
                    Scope::Task { task_id: holder } if self.tasks.get(holder).is_some_and(|h| h.request_id == task.request_id)));
            let approved_for_its_request = self.request_plans.get(&task.request_id).is_some_and(|a| a.presentation_id == checklist.plan);
            if !held_in_its_request && !approved_for_its_request {
                return fail("invalid_input", "Invalid checklist relation");
            }
        }
        let mut owned = BTreeSet::new();
        for checklist in self.checklists.values() {
            if checklist.items.iter().any(|item| !owned.insert((&checklist.plan, &item.id))) {
                return fail("invalid_input", "A point of an approved plan belongs to two tasks");
            }
        }
        for (request_id, approved) in &self.request_plans {
            let proposed = self.interactions.get(&approved.presentation_id).is_some_and(|i| i.card.plan.is_some()
                && i.scope == (Scope::Task { task_id: approved.task_id.clone() })
                && matches!(&i.payload, InteractionPayload::Delivery { is_final: true, .. }));
            if !self.requests.contains_key(request_id) || !proposed {
                return fail("invalid_input", "Invalid approved plan relation");
            }
        }
        for id in self.checklist_marks.keys() {
            if !self.interactions.get(id).is_some_and(|i| matches!(i.payload, InteractionPayload::Delivery { is_final: true, .. })) {
                return fail("invalid_input", "Invalid checklist marks relation");
            }
        }
        for (k, s) in &self.stack_readiness {
            if k!=&s.task_id || !self.pull_requests.get(&s.pr_id).is_some_and(|p|p.task_id==s.task_id) || !self.interactions.get(&s.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:k.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:s.pr_id.clone(),head:s.head.clone()}))){return fail("invalid_input","Invalid stack readiness relation")}
        }
        Ok(())
    }
}
/// A plan states its scope as Markdown checklist lines, so its final delivery can be held to it.
fn plan_checklist(plan: &str) -> Result<()> {
    match plan_checklist_problem(plan).or_else(|| cited_files_problem(plan)) {
        Some((code, fix)) => refuse(code, "plan", &fix),
        None => Ok(()),
    }
}
fn validate_options(options: &[QuestionOption]) -> Result<()> {
    if options.len() > 32 {
        return fail(
            "invalid_input",
            "A question supports at most 32 stable options",
        );
    };
    let mut ids = BTreeSet::new();
    for option in options {
        id(&option.id)?;
        nonempty(&option.label)?;
        if let Some(description) = &option.description {
            nonempty(description)?;
        }
        if !ids.insert(&option.id) {
            return fail("invalid_input", "Duplicate option ID");
        };
    }
    Ok(())
}
/// An option is a button: its answer carries at most the words the user wrote on the card. With no
/// option, the answer is those words or the files the user attached.
fn validate_answer(
    options: &[QuestionOption],
    option: &str,
    inputs: &BTreeMap<String, InputValue>,
    attachments: &[AttachmentRef],
) -> Result<()> {
    let text = match inputs.iter().collect::<Vec<_>>().as_slice() {
        [] => None,
        [(_, InputValue::Text(text))] if text.len() <= 1_048_576 => Some(text.as_str()),
        _ => return fail("invalid_input", "An answer carries at most one text: the words written on the card"),
    };
    if options.is_empty() || option.is_empty() {
        let written = text.is_some_and(|text| !text.trim().is_empty()) && inputs.contains_key(ANSWER_TEXT);
        if !option.is_empty() || !(written || text.is_none() && !attachments.is_empty()) {
            return fail(
                "invalid_input",
                "Free response requires empty option_id and nonempty text input or attachments",
            );
        }
        return Ok(());
    }
    if !options.iter().any(|o| o.id == option) {
        return fail("invalid_input", "Unknown option ID");
    }
    Ok(())
}
fn canonical_pr(host: &str, repo: &str, number: u64) -> Result<()> {
    if number == 0
        || number > MAX_REVISION
        || host.is_empty()
        || host != host.to_ascii_lowercase()
        || !host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        || repo != repo.to_ascii_lowercase()
        || repo.split('/').count() != 2
        || repo.split('/').any(|s| {
            s.is_empty()
                || s == "."
                || s == ".."
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.')
        })
    {
        fail(
            "invalid_input",
            "Canonical lowercase host/owner/repository and positive PR number required",
        )
    } else {
        Ok(())
    }
}

impl AttachmentRef {
    /// The bounds every command puts on the files it carries.
    pub fn validate_list(attachments: &[AttachmentRef]) -> std::result::Result<(), DomainError> {
        validate_attachments(attachments)
    }
}
fn validate_attachments(attachments: &[AttachmentRef]) -> Result<()> {
    if attachments.len() > 32 {
        return fail("payload_too_large", "Too many attachments");
    }
    let mut seen = BTreeSet::new();
    let mut bytes = 0u64;
    for attachment in attachments {
        id(&attachment.id)?;
        nonempty(&attachment.name)?;
        nonempty(&attachment.content_reference)?;
        digest(&attachment.content_digest)?;
        if attachment.name.len() > 1024
            || attachment.content_reference.len() > 8192
            || attachment.name.chars().any(char::is_control)
            || !seen.insert(&attachment.id)
        {
            return fail("invalid_input", "Invalid or duplicate attachment reference");
        }
        bytes = bytes
            .checked_add(attachment.bytes)
            .ok_or_else(|| DomainError::new("payload_too_large", "Attachment size overflow"))?;
        if attachment.bytes > 64 * 1024 * 1024 || bytes > 128 * 1024 * 1024 {
            return fail("payload_too_large", "Attachment content exceeds limits");
        }
    }
    Ok(())
}
fn content(body: &str, attachments: &[AttachmentRef]) -> Result<()> {
    validate_attachments(attachments)?;
    if body.len() > 1_048_576 || body.trim().is_empty() && attachments.is_empty() {
        return fail(
            "invalid_input",
            "Content or attachment required within limits",
        );
    }
    Ok(())
}
