use super::*;
use std::collections::{BTreeMap, BTreeSet};
#[path = "account_login.rs"]
pub mod account_login;
pub use account_login::{AccountLogin, AccountLoginPhase};
pub mod plan_review;
pub mod scheduled_launch;
pub use scheduled_launch::{SCHEDULED_TASK_CANCELLED, SCHEDULED_TASK_ENDED};
pub mod snooze;
pub mod open_pr;
pub mod pr_identity;
pub use pr_identity::{PR_DETACHED, PR_SUPERSEDED, detached_notice, joined_elsewhere_notice};
pub mod effect;
pub use effect::command_name;
pub use open_pr::cancelled_pr_comment;
pub mod stall;
pub use stall::{STALL_ATTEMPTS, STALL_CLOSE, STALL_INSTRUCT, STALL_TEXT_MAX, STALLED_PREFIX, StallOutcome, UNSETTLED_AFTER_MERGE, UNSETTLED_DELIVERY, UNSETTLED_INSTRUCT, UNSETTLED_UNANSWERED, is_stall_case, stalled_card_id};
pub(in crate) use stall::{stalled_before, stalled_in_place_of};
pub mod turn_result;
pub use turn_result::TurnResult;
pub use plan_review::{PLAN_REVIEW_KEEP_PLANS, PLAN_REVIEW_PROBLEM_PREFIX, PLAN_REVIEW_REMIND, PlanReviewProblem};
#[path = "guards.rs"]
pub mod guards;
#[path = "compaction.rs"]
pub mod compaction;
pub use compaction::{body_title, result_excerpt, Clipped, RESULT_LIMIT};
#[path = "decisions.rs"]
pub mod decisions;
#[path = "mcp.rs"]
pub mod mcp;
#[path = "account_settings.rs"]
pub mod account_settings;
#[path = "task_selection.rs"]
pub mod task_selection;
#[path = "stage_rules.rs"]
pub mod stage_rules;
pub mod spec_rules;
pub use stage_rules::{HANDED_OVER_PLAN, STAGE_REFUSED};
#[path = "mcp_wait.rs"]
pub mod mcp_wait;
pub use mcp::{MAX_MCP_SERVERS, McpNodeReading, McpServerState, McpSignIn, McpStatus, NodeMcp, OpenSignIn};
pub use mcp_wait::{MCP_GO_ON_WITHOUT, MCP_SERVER_PREFIX, MCP_WAIT_PREFIX, McpServerKey, McpWaiting};
pub use decisions::{ALONDRA_MCP_SERVER, PLUGIN_SOURCE, UNASKABLE, UNWRITTEN_PLAN, decision_policy};
type Result<T> = std::result::Result<T, DomainError>;
pub fn fail<T>(code: &str, msg: &str) -> Result<T> {
    Err(DomainError::new(code, msg))
}
/// A rejection a node can act on: what is wrong, in which field, and how to fix it.
pub fn refuse<T>(code: &str, field: &str, fix: &str) -> Result<T> {
    let mut error = DomainError::new(code, format!("{field}: {fix}"));
    error.subject = Some(field.into());
    Err(error)
}
/// The command a card arrives with, which bounds the kinds it may be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Information,
    Question,
    Action,
    Delivery,
    Hold,
    Problem,
}
impl Source {
    pub fn kinds(self) -> &'static [CardKind] {
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
    pub fn command(self) -> &'static str {
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
    format!("The orchestrator did not answer “{need}”. Decide with what you know, or ask the user if it is theirs to decide.")
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
pub fn kind_name(kind: CardKind) -> String {
    serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}
/// R18–R21: the card's own shape, before any state is looked at.
pub fn validate_card(card: &CardSpec, options: &[QuestionOption], source: Source, blocking: bool) -> Result<()> {
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
    if card.title.is_none() {
        return refuse("invalid_input", "card.title", "Provide a title separately from need and content.body");
    }
    if let Some(title) = &card.title
        && (title.trim().is_empty() || title.chars().count() > CardSpec::TITLE_MAX || title.contains(['\n', '\r']))
    {
        return refuse("invalid_input", "card.title", "Provide a non-empty title on one line, at most 160 characters; put the content in card.content.body");
    }
    if let Some(content) = &card.content
        && content.body.trim().is_empty()
    {
        return refuse("invalid_input", "card.content.body", "Provide the complete content, or omit card.content when there is no body");
    }
    let need = card.need.trim();
    if need.is_empty() || card.need.contains(['\n', '\r']) {
        return refuse("invalid_input", "card.need", "Say in one line, in the imperative, what you need from the user");
    }
    if card.need.chars().count() > CardSpec::NEED_MAX {
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
        && (summary.chars().count() > CardSpec::SUMMARY_MAX || summary.contains(['\n', '\r']))
    {
        return refuse("invalid_input", "card.content.summary", &format!("One line of at most {} characters, like \"6 files · +120 −40\"; leave it out and Alondra computes one", CardSpec::SUMMARY_MAX));
    }
    match (&card.verify, card.kind) {
        (None, CardKind::Do) => return refuse("invalid_input", "card.verify", "A do card says how you will check the user did it"),
        (Some(_), k) if k != CardKind::Do => return refuse("invalid_input", "card.verify", "Only a do card carries verify: remove it"),
        (Some(v), _) if v.trim().is_empty() => return refuse("invalid_input", "card.verify", "Say how you will check the user did it"),
        _ => {}
    }
    let asks_which_worktrees = card.kind == CardKind::Problem && card.system == Some(SystemCard::DiscardWorktrees);
    if card.form.is_some() && !asks_which_worktrees && (card.kind != CardKind::Provide || source != Source::Hold) {
        return refuse("invalid_input", "card.form", "Only a held request for input from an MCP server carries a form");
    }
    if let Some(destination) = &card.secret_destination {
        if card.kind != CardKind::Provide || source != Source::Question {
            return refuse("invalid_input", "card.secret_destination", "Only request_input with kind secret says where the secret goes");
        }
        if let Some(problem) = destination.shape_problem() {
            return refuse("invalid_input", "card.secret_destination", problem);
        }
    }
    if let Some(recommended) = &card.recommended
        && !options.iter().any(|o| &o.id == recommended)
    {
        return refuse("invalid_input", "card.recommended", "Recommend the id of one of the card's options, or leave it out");
    }
    if card.kind == CardKind::Decide && source != Source::Hold {
        decide_options(options)?;
    }
    let shown = [card.content.as_ref().map_or("", |content| content.body.as_str()), card.plan.as_deref().unwrap_or_default()];
    if let Some(fix) = listed_files_problem(&card.files, &shown) {
        return refuse("invalid_input", "card.files", &fix);
    }
    if blocking && card.kind.never_blocks() {
        return refuse("invalid_input", "blocking", &format!("A {kind} card never blocks: send it with blocking false"));
    }
    if card.blocks.len() > 16 {
        return refuse("invalid_input", "card.blocks", "At most 16 targets");
    }
    Ok(())
}
/// R19: a decision offers 2–8 short distinct options, at most one of them recommended.
pub fn decide_options(options: &[QuestionOption]) -> Result<()> {
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
    Ok(())
}
pub fn nonempty(s: &str) -> Result<()> {
    if s.trim().is_empty() || s.len() > 1_048_576 {
        fail("invalid_input", "Empty or oversized value")
    } else {
        Ok(())
    }
}
pub fn id(s: &str) -> Result<()> {
    nonempty(s)?;
    if s.len() > 256 || s.chars().any(char::is_control) {
        fail("invalid_input", "Invalid identifier")
    } else {
        Ok(())
    }
}
pub fn digest(s: &str) -> Result<()> {
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
pub fn head(s: &str) -> Result<()> {
    nonempty(s)?;
    if s.len() > 256 || s.chars().any(char::is_whitespace) {
        fail("invalid_input", "Invalid head")
    } else {
        Ok(())
    }
}
/// Who reads a notice for the orchestrator: Alondra, once it handed the notice over.
pub const ORCHESTRATOR_READER: &str = "alondra-orchestrator";

pub fn user(actor: &Actor) -> Result<String> {
    match actor {
        Actor::User { user_id } => {
            id(user_id)?;
            Ok(user_id.clone())
        }
        _ => fail("forbidden", "User decision required"),
    }
}
pub fn system(actor: &Actor) -> Result<()> {
    if matches!(actor, Actor::System) {
        Ok(())
    } else {
        fail("forbidden", "Trusted observation required")
    }
}
/// What a node that tries to merge or complete its own Task is told.
pub const ALONDRA_MERGES: &str = "Alondra merges pull requests and completes tasks itself: deliver with present_result. Once it is merged Alondra asks you to clean up; if the merge fails, it hands the task back with the reason.";
pub const MERGE_FAILED: &str = "Alondra could not merge pull request #";
pub const MERGE_FAILED_FIX: &str = ". Fix what stops the merge";
/// What a node is told when Alondra cannot merge its PR, which the task also shows.
pub fn merge_failed_text(number: u64, url: &str, base: &str, reason: &str) -> String {
    format!(
        "{MERGE_FAILED}{number} ({url}): {reason}{MERGE_FAILED_FIX} (conflicts with {base}, failing checks, or a review GitHub \
         requires), push, and present the result again with present_result and the new head. If only the user can unblock it, ask them.",
        reason = reason.trim().trim_end_matches('.'),
    )
}
impl MergeFailure {
    /// The merge failure a hand-back stored before Alondra recorded it wrote in its body. Only the
    /// database's one-time upgrade reads it.
    pub fn of_stored_body(body: &str) -> Option<Self> {
        let rest = body.strip_prefix(MERGE_FAILED)?;
        let (number, rest) = rest.split_once(' ')?;
        let reason = rest.split_once("): ")?.1;
        let reason = reason.split_once(MERGE_FAILED_FIX).map_or(reason, |(reason, _)| reason);
        Some(Self { number: number.parse().ok()?, reason: reason.to_owned() })
    }
}
/// What a node is told when its task is finished and Alondra is about to close its session.
pub fn cleanup_request(title: &str, task: &TaskId, worktrees: &[&str]) -> String {
    format!("Your task “{title}” is finished and is closing. Before Alondra closes this session and \
deletes its worktrees, remove what you generated for this task outside them that would otherwise \
stay on this Mac (build caches, temporary files), with the project's own tools (its clean commands, \
scripts or build targets). Do not delete files or links inside a worktree: Alondra deletes each \
worktree of the task. If nothing was generated outside them, delete nothing.{worktrees} Then call \
cleanup_done with task_id {task}. If something you started for this task is still running after \
the merge (a deploy, a release script), call post_merge_work with task_id {task} and what it is \
first: Alondra keeps the session open until you call it again without reason or call cleanup_done.",
        task = task.0, worktrees = worktrees_to_settle(worktrees))
}
/// The worktrees a closing task still has, and what the node does with work left in one: Alondra
/// deletes only a worktree with nothing left in it, or one the node gives up.
pub fn worktrees_to_settle(worktrees: &[&str]) -> String {
    if worktrees.is_empty() {
        return String::new();
    }
    let list = worktrees.iter().map(|path| format!("\n- {path}")).collect::<String>();
    format!(" The task's worktrees, yours and those of the subagents you launched:{list}\nAlondra closes \
the task only once none of them has uncommitted changes or commits no remote has. For each one with \
work left, commit it and push it to its branch, or give it up: list its path in cleanup_done's \
discard and Alondra deletes it as it is.")
}
/// What a node that declared work after the merge before delivering is told once its delivery
/// settles: the task stays in progress until that work ends.
pub fn post_merge_reminder(title: &str, task: &TaskId, reason: &str) -> String {
    format!("Your task “{title}” has its delivery accepted and merged, and you said work remains after the \
merge: {reason}. Do it now: the task stays in progress until you say it ended. If your approved plan \
still has unproved points, call present_result with every point and its evidence first. When the \
work ends, call post_merge_work \
with task_id {task} without reason. Then remove what you generated for this task outside \
the worktree (build caches, temporary files) with the project's own tools, never files or links inside the \
worktree, and call cleanup_done with task_id {task}. If it cannot be done, say so with ask.", task = task.0)
}
/// One Task a node waited on, as the node is told once the wait is over.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Landed {
    pub title: String,
    pub cancelled: bool,
    pub result: Option<String>,
    pub merged_pr: Option<String>,
}
impl Landed {
    pub fn describe(&self) -> String {
        if self.cancelled {
            return format!("### “{}”\nIt was cancelled: nothing of it landed.", self.title);
        }
        let mut entry = format!("### “{}”\n{}", self.title, self.result.as_deref().unwrap_or("No final result was recorded for it."));
        if let Some(pr) = &self.merged_pr {
            entry.push_str(&format!("\nMerged pull request: {pr}"));
        }
        entry
    }
}
/// What a node that waited on other Tasks is told when they are done.
pub fn dependencies_done_notice(landed: &[Landed], update: BranchUpdate) -> String {
    let names: Vec<String> = landed.iter().map(|l| format!("“{}”", l.title)).collect();
    let details: Vec<String> = landed.iter().map(Landed::describe).collect();
    format!("What you were waiting for is done: {}. Go on with your task; if you need what it changed, first update your \
branch: {}.\n\n{}", names.join(", "), update.from_default_branch(), details.join("\n\n"))
}
/// What a node whose work after the merge waited on other Tasks is told when that wait is over.
pub fn post_merge_wait_over_notice(task: &TaskId, reason: &str, landed: &[Landed]) -> String {
    let details: Vec<String> = landed.iter().map(Landed::describe).collect();
    format!("What your work after the merge waited for is over. Go on with it now: {reason}. Update from the default \
branch first if you need what landed. When that work ends, call post_merge_work with task_id {task} without reason; \
if what landed is not what you needed, wait again with post_merge_work and waits_for naming the task or pull request.\n\n{}",
        details.join("\n\n"), task = task.0)
}
/// What a node is told when it presents a plan while its Task waits for others.
pub fn plan_while_waiting_notice(titles: &[String]) -> String {
    let names: Vec<String> = titles.iter().map(|title| format!("“{title}”")).collect();
    format!("Your task waits for {} to be done, so this plan is not reviewed: do not go on with it. If that wait is no \
longer needed, ask the orchestrator to remove it, naming the fact that proves it (for example the pull request that is \
already on the default branch). Once your task is running again, present the plan.", names.join(", "))
}
pub fn node_closing_notice() -> String {
    "Your task is closing: your cleanup is confirmed and this session ends now, so nothing is asked of the user. \
Stop here and end your turn.".into()
}
pub fn plan_after_acceptance_notice() -> String {
    "Your delivery was accepted and the task is closing, so a plan is not reviewed: do not go on with it. Finish \
the cleanup Alondra asked for and call cleanup_done. New work goes to the orchestrator as a new request.".into()
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
pub fn delivery_withdrawn_base_moved(number: u64, base: &str) -> String {
    format!("Pull request #{number} now targets {base}, a branch your final delivery was not validated against, so that delivery was withdrawn \
and the task is in progress again. Present your result again so its commit is read against that branch.")
}
pub fn delivery_replaced() -> String {
    "Your new final delivery replaces the one you presented before, which was withdrawn. Nothing else is needed \
for it.".into()
}
pub fn cancelled_notice(title: &str) -> String {
    format!("Your task “{title}” was cancelled. Stop working on it now and do not start anything new \
for it: do not commit, push, register a pull request or deliver anything. Before you end your \
turn, delete what you generated for it that git does not track (build outputs, caches, temporary \
files) with the project's own tools. Alondra then closes this session.")
}
pub fn merged_notice(number: u64, left: usize) -> String {
    match left {
        0 => format!("Pull request #{number} is merged; no pull request of this task is left to merge."),
        n => format!("Pull request #{number} is merged; {n} left to merge."),
    }
}
pub fn closed_notice(number: u64, left: usize) -> String {
    match left {
        0 => format!("Pull request #{number} was closed without merging; no pull request of this task is left to merge."),
        n => format!("Pull request #{number} was closed without merging; {n} left to merge."),
    }
}
impl DomainState {
    pub fn fence(&self, actor: &Actor) -> Result<()> {
        match actor {
            Actor::Orchestrator { generation } if *generation != self.orchestrator_generation => {
                fail("stale_generation", "Orchestrator generation changed")
            }
            Actor::User { user_id } => id(user_id),
            Actor::Agent { agent_id } => id(&agent_id.0),
            _ => Ok(()),
        }
    }
    pub fn manager(&self, actor: &Actor) -> Result<()> {
        self.fence(actor)?;
        if matches!(actor, Actor::User { .. } | Actor::Orchestrator { .. }) {
            Ok(())
        } else {
            fail("forbidden", "User or current orchestrator required")
        }
    }
    pub fn task(&self, task: &TaskId) -> Result<&Task> {
        self.tasks
            .get(task)
            .ok_or_else(|| DomainError::new("not_found", "Task does not exist"))
    }
    pub fn active(&self, task: &TaskId) -> Result<&Task> {
        let t = self.task(task)?;
        if t.state.terminal() {
            fail("task_not_active", "Task is terminal")
        } else {
            Ok(t)
        }
    }
    /// A task that ends takes no node and starts no work: it closes, and then it can be reopened.
    pub fn not_ending(&self, task: &TaskId) -> Result<()> {
        match self.task(task)?.ending {
            Some(TaskEnding::Cancelling) => fail("task_ending", &format!("Task {task} is being cancelled: once it is closed, reopen it")),
            Some(TaskEnding::Completing) => fail("task_ending", &format!("Task {task} is closing as done: once it is closed, reopen it")),
            None => Ok(()),
        }
    }
    /// The task, other than `except`, that has not ended and works in the worktree at `path`.
    pub fn worktree_held_by_other(&self, path: &str, except: &TaskId) -> Option<&TaskId> {
        self.tasks.values()
            .find(|t| &t.id != except && !t.over() && t.open_worktrees().any(|(open, _)| open == path))
            .map(|t| &t.id)
    }

    pub fn assigned(&self, actor: &Actor, task: &TaskId) -> Result<&Task> {
        let t = self.active(task)?;
        match actor {
            Actor::Agent { agent_id } if t.agent_id.as_ref() == Some(agent_id) => Ok(t),
            _ => fail("not_assigned", "Command requires the assigned Agent"),
        }
    }
    pub fn validate_scope(&self, scope: &Scope) -> Result<()> {
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
    pub fn recipient(&self, to: &Recipient) -> Result<()> {
        if let Recipient::Agent { agent_id } = to {
            if !self.agents.contains_key(agent_id) {
                return fail("not_found", "Recipient Agent does not exist");
            }
        }
        Ok(())
    }
    pub fn interaction(&self, i: &InteractionId) -> Result<&Interaction> {
        self.interactions
            .get(i)
            .ok_or_else(|| DomainError::new("not_found", "Interaction does not exist"))
    }
    pub fn presentation(
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
        if t.final_presentation.as_ref() != Some(i) || p.withdrawn {
            return fail("acceptance_stale", "Presentation is not the current final");
        };
        self.targets(task_id, targets)?;
        Ok((p, t, targets))
    }
    /// Approved plan points that the latest final delivery has not proved complete.
    pub fn unproved_checklist(&self, task: &Task) -> Vec<&ChecklistItem> {
        let marks = task.final_presentation.as_ref().and_then(|id| self.checklist_marks.get(id));
        self.checklists.get(&task.id).into_iter().flat_map(|plan| &plan.items)
            .filter(|item| !marks.is_some_and(|marks| marks.iter().any(|mark| mark.item == item.id))).collect()
    }
    /// The unproved points that still hold the task. A delivery Alondra made for the node carries
    /// no checklist and never deferred a point to after the merge: once a PR of it merged, with none
    /// left open and no work owed after the merge, the user's merge settles those points.
    pub fn checklist_owed(&self, task: &Task) -> Vec<&ChecklistItem> {
        let delivered_by_alondra = task.final_presentation.as_ref().and_then(|id| self.interactions.get(id))
            .is_some_and(|p| p.system_card == Some(SystemCard::OpenPrDelivery));
        if delivered_by_alondra && self.merged_pr(task).is_some() && self.open_prs(task).next().is_none() && !self.post_merge_owed(task) {
            return Vec::new();
        }
        self.unproved_checklist(task)
    }
    pub fn require_checklist_proof(&self, task: &Task) -> Result<()> {
        let pending = self.checklist_owed(task);
        if pending.is_empty() { return Ok(()) }
        refuse("checklist_incomplete", "checklist", &format!(
            "the approved plan still needs evidence for {}. Finish that work and call present_result with every point and its evidence before ending post_merge_work or calling cleanup_done",
            pending.iter().map(|item| format!("{} “{}”", item.id, item.text)).collect::<Vec<_>>().join(", ")))
    }
    pub fn targets(&self, task: &TaskId, targets: &[DeliveryTarget]) -> Result<()> {
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
                    if &p.task_id != task {
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
    pub fn insert_interaction(
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
        let held = source == Source::Hold;
        let card_type = CardType::of(card, card.system.as_ref(), &payload, held);
        let worded;
        let card = match card_type.map(|t| t.contract().need) {
            Some(Need::Alondra(need)) => {
                worded = CardSpec { need: need.into(), ..card.clone() };
                &worded
            }
            _ => card,
        };
        validate_card(card, options, source, blocking).map_err(|e| e.breaking(card_type, ContractRule::Shape))?;
        if card.secret_destination.is_some() && !matches!((scope, actor), (Scope::Task { .. }, Actor::Agent { .. })) {
            return refuse("invalid_input", "card.secret_destination", "Only a task's node asks for a secret written to a file of its repository");
        }
        let author = match source {
            Source::Hold => Author::Agent { agent_id: match actor {
                Actor::Agent { agent_id } => agent_id.clone(),
                _ => return fail("forbidden", "Only an Agent's hook can be held"),
            } },
            _ => self.card_author(actor, scope, source)?,
        };
        if author != Author::System {
            if card.system.is_some() {
                return refuse("forbidden", "card.system", "Only Alondra marks a card as one of its own");
            }
            if reserved_card_id(&i.0) {
                return refuse("invalid_input", "interaction_id", "This id names one of Alondra's own cards: choose another interaction_id");
            }
        }
        if let (Author::Agent { .. }, Scope::Task { task_id }) = (&author, scope)
            && let Some(t) = self.tasks.get(task_id)
        {
            if self.cleanup_confirmed(t) {
                return refuse(CLOSING, "interaction_id", &node_closing_notice());
            }
            if card.plan.is_some() && self.in_cleanup(t) {
                return refuse(CLOSING, "plan", &plan_after_acceptance_notice());
            }
        }
        self.recipient(to)?;
        NewCard { card, system: card.system.as_ref(), payload: &payload, author: &author, to, blocking, held }
            .check()
            .map_err(DomainError::of_breach)?;
        if let (Author::Orchestrator { .. }, Scope::Task { .. }, Recipient::User, Source::Question | Source::Action) = (&author, scope, to, source) {
            return refuse("forbidden", "scope", "A task's node asks the user what its task needs, never the orchestrator for it: answer the node's \
                question telling it to ask the user, or send it what to ask. A question about a request goes with scope orchestrator and its request_id");
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
        let mut replaces_snoozed = false;
        if let Some(old) = &card.replaces {
            self.replaceable(&author, old)?;
            replaces_snoozed = self.interactions[old].snoozed_until.is_some();
            self.retire(old);
        }
        if let Some(slot) = CardSlot::of(card, &payload) {
            for old in self.slot_holders(scope, &author, &slot) {
                replaces_snoozed |= self.interactions[&old].snoozed_until.is_some();
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
                card: CardSpec { system: None, ..card.clone() },
                clarification: None, snoozed_until: None,
                resurfaced_at_revision: replaces_snoozed.then_some(self.revision + 1),
                system_card: card.system.clone(), shortened: false,
            },
        );
        self.blocks_acyclic(i)?;
        Ok(None)
    }
    /// One of Alondra's own cards, made whole by its code, after its type's contract holds.
    pub fn insert_system_card(&mut self, card: Interaction) -> Result<()> {
        let held = self.holds.contains_key(&card.id);
        NewCard {
            card: &card.card, system: card.system_card.as_ref(), payload: &card.payload, author: &card.author,
            to: &card.recipient, blocking: card.blocking, held,
        }.check().map_err(DomainError::of_breach)?;
        self.interactions.insert(card.id.clone(), card);
        Ok(())
    }
    /// A suggestion is something a node saw outside its task: it goes to the orchestrator, never
    /// to the user, carries the orchestrator's fixed answers, and a task keeps only a few waiting.
    pub fn suggestion(&self, author: &Author, scope: &Scope, to: &Recipient, options: &[QuestionOption]) -> Result<()> {
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
    /// R1, R2, R4: who may put a card in front of someone, and on which Task.
    pub fn card_author(&self, actor: &Actor, scope: &Scope, source: Source) -> Result<Author> {
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
            Actor::System if matches!(source, Source::Problem | Source::Information | Source::Question | Source::Delivery) => Ok(Author::System),
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
    pub fn retire(&mut self, id: &InteractionId) {
        if let Some(i) = self.interactions.get_mut(id) {
            i.withdrawn = true;
            i.snoozed_until = None;
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
    pub fn answered_question(i: &Interaction) -> bool {
        matches!(&i.payload, InteractionPayload::Question { answer: Some(_), .. } | InteractionPayload::ActionRequest { answer: Some(_), .. })
    }
    /// The refusal that gives an answered card's answer back, every member's for a group.
    pub fn already_answered(&self, i: &Interaction) -> String {
        let said = |member: &Interaction| answer_said(member).unwrap_or_default().trim_end_matches('.').to_owned();
        let answer = if i.card.group.is_some() {
            self.group_members(i).into_iter().map(|member| format!("{} ({}) → {}", member.card.effective_title(), member.id, said(member)))
                .collect::<Vec<_>>().join("; ")
        } else {
            said(i)
        };
        format!("Card {} is already answered: {answer}. It is final: act on it", i.id)
    }
    /// `card` is a card a node of `task_id` asked on it, answered and held by no hook: what the
    /// task's node is told carries its answer. The node that asked may have been replaced since
    /// (D05): the answer reaches whoever holds the Task now.
    pub fn answers_a_node_card(&self, task_id: &TaskId, card: &InteractionId) -> bool {
        !self.holds.contains_key(card) && self.interactions.get(card).is_some_and(|i| {
            matches!(&i.scope, Scope::Task { task_id: scope } if scope == task_id)
                && matches!(&i.author, Author::Agent { .. })
                && Self::answered_question(i)
        })
    }
    /// Every question of the group has its answer: the author is given them all together.
    pub fn group_complete(&self, i: &Interaction) -> bool {
        self.group_members(i).iter().all(|member| !member.withdrawn && Self::answered_question(member))
    }
    /// R8: the same notice, still unread, from the same author to the same place.
    pub fn repeated_notice(&self, author: &Author, scope: &Scope, to: &Recipient, body: &str, card: &CardSpec, source: Source) -> Option<InteractionId> {
        if source != Source::Information {
            return None;
        }
        self.interactions.values()
            .filter(|i| &i.author == author && &i.scope == scope && &i.recipient == to && self.open(i))
            .filter(|i| matches!(i.payload, InteractionPayload::Information { .. }))
            .max_by_key(|i| i.order)
            .filter(|i| i.body == body && i.card.title == card.title && i.card.need == card.need)
            .map(|i| i.id.clone())
    }
    /// R7: after the user asked for a clarification, the author's next card replaces that card.
    /// Plain text (a notice) is still allowed.
    pub fn clarification_answered(&self, author: &Author, task: Option<&TaskId>, card: &CardSpec) -> Result<()> {
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
    pub fn one_blocking_ask(&self, author: &Author, card: &CardSpec, blocking: bool) -> Result<()> {
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
    pub fn replaceable(&self, author: &Author, old: &InteractionId) -> Result<()> {
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
            return refuse("already_answered", "card.replaces", &self.already_answered(x));
        }
        if !self.open(x) {
            return refuse("already_withdrawn", "card.replaces", &format!("Card {old} is no longer open: send a new card without card.replaces"));
        }
        if let Some(group) = &x.card.group {
            return refuse("question_in_group", "card.replaces", &format!(
                "Card {old} is one of the questions asked together as {}: its answers reach you only all together, so one of them is never replaced alone. To change the questions, withdraw {old} (it withdraws the whole group) and ask them again together",
                group.id));
        }
        Ok(())
    }
    /// R14: the open cards in `slot`, which a new card in it replaces. On a Task the slot is the
    /// Task's, whichever node wrote them; elsewhere it is the author's.
    pub fn slot_holders(&self, scope: &Scope, author: &Author, slot: &CardSlot) -> Vec<InteractionId> {
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
    pub fn card_blocks(&self, task: Option<&TaskId>, card: &CardSpec) -> Result<()> {
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
    pub fn card_target_matches_delivery(target: &CardTarget, delivery: &Interaction) -> bool {
        let InteractionPayload::Delivery { targets, .. } = &delivery.payload else { return false };
        match target {
            CardTarget::Delivery { interaction_id } => interaction_id == &delivery.id,
            CardTarget::PullRequest { pr_id } => targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id: known, .. } if known == pr_id)),
            CardTarget::Document { document_id } => targets.iter().any(|target| matches!(target, DeliveryTarget::DocumentRevision { document_id: known, .. } if known == document_id)),
        }
    }
    /// What an open card holds back, as the deliveries it stands in front of.
    pub fn blocked_deliveries(&self, i: &Interaction) -> Vec<InteractionId> {
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
    pub fn blocks_acyclic(&self, start: &InteractionId) -> Result<()> {
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
    /// A delivery of a PR carries every open PR of its task.
    pub fn with_open_prs(&self, task: &TaskId, targets: &[DeliveryTarget]) -> Vec<DeliveryTarget> {
        let mut all = self.with_current_heads(targets);
        let carried = |all: &[DeliveryTarget], pr: &PullRequest| all.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { pr_id, .. } if *pr_id == pr.id));
        if let (true, Some(t)) = (all.iter().any(|t| matches!(t, DeliveryTarget::PullRequest { .. })), self.tasks.get(task)) {
            for pr in self.open_prs(t) {
                if !carried(&all, pr) {
                    all.push(DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: pr.head.clone() });
                }
            }
        }
        all
    }
    /// A pull request target presented without a head shows the commit its PR is at in this same
    /// state, the one R12 checks it against, and one naming a copy that joined another names the
    /// record that stays.
    pub fn with_current_heads(&self, targets: &[DeliveryTarget]) -> Vec<DeliveryTarget> {
        let mut current: Vec<DeliveryTarget> = Vec::with_capacity(targets.len());
        for target in targets {
            let target = match target {
                DeliveryTarget::PullRequest { pr_id, head } => {
                    let pr_id = self.surviving_pr(pr_id).clone();
                    let head = if head.is_empty() { self.pull_requests.get(&pr_id).map(|pr| pr.head.clone()).unwrap_or_default() } else { head.clone() };
                    DeliveryTarget::PullRequest { pr_id, head }
                }
                other => other.clone(),
            };
            if !current.contains(&target) {
                current.push(target);
            }
        }
        current
    }
    /// The cards that named the tasks of a request, oldest first.
    pub fn tasks_cards(&self, request: &RequestId) -> Vec<(&Interaction, crate::request_outcome::TasksCard)> {
        let mut cards: Vec<_> = self.interactions.values()
            .filter(|i| i.author == Author::System
                && i.system_card == Some(SystemCard::RequestTasks)
                && matches!(&i.scope, Scope::Orchestrator { request_id: Some(id) } if id == request))
            .filter_map(|i| crate::request_outcome::TasksCard::from_body(&i.body).map(|card| (i, card)))
            .collect();
        cards.sort_by_key(|(i, _)| i.order);
        cards
    }
    /// The tasks of each open request of the user that no card of it names yet, in the order
    /// they run: what `Command::AnnounceRequestTasks` tells. A schedule's request is left out,
    /// since its task comes with it.
    pub fn unannounced_tasks(&self) -> Vec<(RequestId, Vec<TaskId>)> {
        self.requests.values()
            .filter(|r| !r.closed && !r.withdrawn && !r.message && !r.maintenance && r.origin == RequestOrigin::User)
            .filter_map(|r| {
                let named: BTreeSet<String> = self.tasks_cards(&r.id).into_iter()
                    .flat_map(|(_, card)| card.tasks.into_iter().map(|task| task.id))
                    .collect();
                let fresh: Vec<&Task> = self.tasks.values()
                    .filter(|t| t.request_id == r.id && !t.cancelled() && !named.contains(&t.id.0))
                    .collect();
                (!fresh.is_empty()).then(|| (r.id.clone(), in_running_order(fresh)))
            })
            .collect()
    }
    /// R16: the open cards that hold back this delivery, oldest first.
    pub fn blocked_by(&self, delivery: &InteractionId) -> Vec<InteractionId> {
        self.blocked_by_among(delivery, self.interactions.values())
    }
    /// `blocked_by` over `cards`, which hold every interaction that names what it holds back.
    pub fn blocked_by_among<'a>(&self, delivery: &InteractionId, cards: impl Iterator<Item = &'a Interaction>) -> Vec<InteractionId> {
        let Some(delivery) = self.interactions.get(delivery)
            .filter(|delivery| self.open(delivery) && matches!(delivery.payload, InteractionPayload::Delivery { .. }))
        else { return vec![] };
        let mut by: Vec<&Interaction> = cards
            .inspect(|_| measure::card_read())
            .filter(|i| i.card.blocks.iter().any(|target| Self::card_target_matches_delivery(target, delivery))
                && i.id != delivery.id
                && self.open(i))
            .collect();
        by.sort_by_key(|i| i.order);
        by.into_iter().map(|i| i.id.clone()).collect()
    }
    pub fn unblocked(&self, delivery: &InteractionId, holds: &dyn Holds) -> Result<()> {
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
                let need = self.interactions.get(x).map_or_else(String::new, |blocker| blocker.card.effective_title().to_owned());
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
    pub fn decided(&mut self, presentation: &InteractionId) {
        if let Some(p) = self.interactions.get_mut(presentation) {
            p.revision = self.revision + 1;
        }
    }
    pub fn invalidate(&mut self, task: &TaskId) {
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


    pub fn dependency_graph(&self) -> Result<()> {
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
                .chain(task.waiting_for_tasks.iter().filter(|id| !task.ended_task_waits.contains_key(*id)))
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
    pub fn blocks_graph(&self) -> Result<()> {
        for blocker in self.tasks.values() {
            for blocked in &blocker.blocks {
                self.task(blocked)?;
                if blocked == &blocker.id {
                    return fail("dependency_cycle", "A task cannot block itself");
                }
                if blocker.dependencies.contains(blocked) || (!blocker.ended_task_waits.contains_key(blocked) && blocker.waiting_for_tasks.contains(blocked)) {
                    return fail("dependency_cycle", "A task cannot block one it waits for to be done");
                }
                let blocked_task = self.task(blocked)?;
                if blocked_task.dependencies.contains(&blocker.id) || (!blocked_task.ended_task_waits.contains_key(&blocker.id) && blocked_task.waiting_for_tasks.contains(&blocker.id)) || blocked_task.delivered_dependencies.contains(&blocker.id) {
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
    /// What a Task waits for before it starts or goes on: its dependencies done and its
    /// delivered dependencies delivered. A wait never holds back the merge of a PR the user
    /// accepted; only `stack` orders merges.
    /// What a Task waited on is in: it is done, or its PR merged with no work left after the merge.
    pub fn landed(&self, dep: &Task) -> bool {
        dep.state == TaskState::Done
            || (!dep.cancelled() && dep.wrap_up.post_merge_work.is_none() && self.checklist_owed(dep).is_empty()
                && self.merged_pr(dep).is_some() && self.open_prs(dep).next().is_none())
    }
    /// The Task's latest merged PR.
    pub fn merged_pr(&self, t: &Task) -> Option<&PullRequest> {
        self.task_prs(t).filter(|pr| pr.state == PrState::Merged).last()
    }
    /// Every PR of the task, in any repository, the first one first.
    pub fn task_prs<'a>(&'a self, t: &Task) -> impl Iterator<Item = &'a PullRequest> {
        t.pr_ids().filter_map(|id| self.pull_requests.get(id)).filter(|pr| pr.is_tracked())
    }
    pub fn open_prs<'a>(&'a self, t: &Task) -> impl Iterator<Item = &'a PullRequest> {
        self.task_prs(t).filter(|pr| pr.state == PrState::Open)
    }
    /// The PR a person reads about the task: an open one, else its first.
    pub fn main_pr(&self, t: &Task) -> Option<&PullRequest> {
        let first = self.task_prs(t).next();
        self.open_prs(t).next().or(first)
    }
    /// An open PR of the task its standing final delivery does not carry.
    pub fn undelivered_pr(&self, t: &Task) -> Option<&PullRequest> {
        let delivered = t.final_presentation.as_ref().and_then(|id| self.interactions.get(id)).map(|i| &i.payload);
        let targets: &[DeliveryTarget] = match delivered {
            Some(InteractionPayload::Delivery { targets, .. }) => targets,
            _ => &[],
        };
        self.open_prs(t).find(|pr| !targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id, .. } if *pr_id == pr.id)))
    }
    pub fn waits_are_over(&self, t: &Task) -> Result<()> {
        for waited in t.waiting_for_tasks.iter().filter(|id| !t.ended_task_waits.contains_key(*id)) {
            if !Self::task_wait_finished(self.task(waited)?) {
                return fail("dependency_not_done", "A task the node waits for has not ended");
            }
        }
        for dep in &t.dependencies {
            if !self.landed(self.task(dep)?) {
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
    /// The titles of the Tasks `task` still waits for, while its state is waiting for them:
    /// none when it waits for nothing another Task has to do.
    pub fn waited_tasks(&self, task: &TaskId) -> Vec<String> {
        self.tasks.get(task).filter(|t| t.state == TaskState::WaitingDependency).map(|t| self.unmet_dependencies(t)).unwrap_or_default()
    }
    /// The titles of the Tasks `t` depends on that are not done yet, whatever its state says.
    pub fn unmet_dependencies(&self, t: &Task) -> Vec<String> {
        self.unmet_dependency_ids(t).into_iter()
            .map(|dep| self.tasks.get(dep).map_or_else(|| dep.0.clone(), |d| d.title.clone()))
            .collect()
    }
    pub fn task_wait_finished(task: &Task) -> bool {
        task.state.terminal()
    }
    /// The tasks `t` still waits on, by id.
    pub fn unmet_dependency_ids<'a>(&'a self, t: &'a Task) -> Vec<&'a TaskId> {
        let undone = t.dependencies.iter().filter(|dep| self.tasks.get(*dep).is_none_or(|d| !self.landed(d)));
        let undelivered = t.delivered_dependencies.iter()
            .filter(|dep| self.tasks.get(*dep).is_none_or(|d| d.final_presentation.is_none() || d.state == TaskState::Cancelled));
        let upstream = t.stack.iter().map(|s| &s.upstream_task).filter(|_| self.stack_ready(t).is_err());
        undone.chain(t.waiting_for_tasks.iter().filter(|dep| !t.ended_task_waits.contains_key(*dep) && self.tasks.get(*dep).is_none_or(|d| !Self::task_wait_finished(d))))
            .chain(undelivered).chain(upstream).collect()
    }
    /// What a node is told when it presents a plan while its Task waits for others: the plan is
    /// not reviewed while the recorded wait remains open.
    pub fn plan_while_waiting(&self, task: &TaskId) -> Option<String> {
        let waited = self.waited_tasks(task);
        (!waited.is_empty()).then(|| plan_while_waiting_notice(&waited))
    }
    /// The dependency still names the upstream delivery and head that were marked ready.
    pub fn stack_ready(&self, t: &Task) -> Result<()> {
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
    /// A stacked PR can merge only after its pinned upstream head merged into its current base.
    pub fn stack_ready_for_merge(&self, pr_id: &PrId) -> Result<()> {
        let pr = self.pull_requests.get(pr_id).ok_or_else(|| DomainError::new("not_found", "PR missing"))?;
        let task = self.task(&pr.task_id)?;
        self.stack_ready(task)?;
        if let Some(stack) = &task.stack {
            let upstream = self.pull_requests.get(&stack.pr_id)
                .ok_or_else(|| DomainError::new("stack_not_ready", "Upstream PR missing"))?;
            if upstream.state != PrState::Merged || upstream.merged_head.as_ref() != Some(&stack.head) || upstream.base != pr.base {
                return fail("stack_not_ready", "Upstream merge and retarget must be confirmed");
            }
        }
        Ok(())
    }
    pub fn accepted(&self, p: &Interaction) -> bool {
        matches!(&p.payload,InteractionPayload::Delivery{acceptance:Some(a),..} if a.valid)
    }
    /// A final delivery whose PRs are all resolved, at least one merged and the rest closed without
    /// merging, is settled by the merge itself, whoever merged them: Alondra after the user's
    /// acceptance or the checks of an auto-merge, or the user on GitHub.
    pub fn merged_delivery(&self, p: &Interaction) -> bool {
        let InteractionPayload::Delivery { targets, .. } = &p.payload else { return false };
        let states: Vec<PrState> = targets.iter().filter_map(|t| match t {
            DeliveryTarget::PullRequest { pr_id, head } => Some(self.pull_requests.get(pr_id)
                .filter(|pr| pr.state != PrState::Merged || pr.merged_head.as_ref() == Some(head))
                .map(|pr| pr.state)),
            _ => None,
        }).collect::<Option<_>>().unwrap_or_default();
        states.contains(&PrState::Merged) && !states.contains(&PrState::Open)
    }
    /// A question or request the task's node or the orchestrator asked the user and that still
    /// waits, blocking or not. A suggestion goes to the orchestrator, not the user: it outlives the
    /// task and holds nothing back. Alondra's own cards (problems, held prompts) are about what Alondra
    /// does with the task, which they would otherwise hold back themselves.
    /// Ends a task the user, the orchestrator or Alondra stopped: its cards leave in silence and a
    /// node it had is told to stop and is closed.
    pub fn cancel(&mut self, task_id: &TaskId, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        if !self.task(task_id)?.pending_user_inputs.is_empty() {
            return fail("pending_user_message", "A user message still needs a final delivery before the task can be cancelled");
        }
        self.invalidate(task_id);
        let closing_cards: Vec<InteractionId> = self.interactions.values().filter(|i| {
            matches!(i.system_card, Some(SystemCard::CloseProblem | SystemCard::SessionEnded | SystemCard::HostLost { .. } | SystemCard::HandoffDoubt))
                && i.scope == (Scope::Task { task_id: task_id.clone() })
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
        let held: Vec<InteractionId> = self.lifecycle(lifecycles, task_id)?.cards().into_iter().map(|(card, _)| card.clone())
            .filter(|card| !self.interactions.get(card).is_some_and(Self::answered_question))
            .collect();
        for card in &held {
            self.retire(card);
        }
        let record = LifecycleRecord::Finished {
            intent: TaskEnding::Cancelling,
            cleanup: self.next_operation("cleanup", task_id),
            post_merge: None,
        };
        self.decide(lifecycles, task_id, record)?;
        self.settle_scheduled_launch(task_id)?;
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
    /// A cancelled task whose node Alondra has not closed yet: its open cards were taken away when
    /// it was cancelled, so any it has now were asked after. Its node can still ask the user what
    /// to do with work it cannot leave as it is.
    pub fn winding_down(t: &Task) -> bool {
        t.cancelled() && t.wrap_up.close_cancelled && t.wrap_up.node.is_none()
    }
    pub fn retire_cards_of(&mut self, task_id: &TaskId) {
        let open: Vec<InteractionId> = self.interactions.values().filter(|i| self.unanswered_card(i, task_id)).map(|i| i.id.clone()).collect();
        for card in &open {
            self.retire(card);
        }
    }
    pub fn unanswered_card(&self, i: &Interaction, t: &TaskId) -> bool {
        measure::card_read();
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
    /// An unanswered question of the node that does not name what it holds back: no final delivery
    /// may stand next to it. A card that names its delivery or PR already holds that merge back where
    /// the user sees it. Neither a permission nor an MCP server's elicitation is a question of the
    /// node: any tool call asks one, a background subagent's too, and it decides that call, not the
    /// delivery. Nor is the card Alondra makes of the node's last words.
    pub fn asked_aside(&self, i: &Interaction, t: &TaskId) -> bool {
        i.card.blocks.is_empty()
            && !matches!(self.holds.get(&i.id), Some(Hold::Permission | Hold::Elicitation))
            && i.system_card != Some(SystemCard::Unheard)
            && self.unanswered_card(i, t)
    }
    /// A card of the task that holds it on the user: a question to the orchestrator holds it on
    /// the orchestrator, and a turn-ended card a recorded wait covers holds it on that wait.
    pub fn blockers(&self, t: &TaskId) -> bool {
        self.interactions.values().any(|i| self.blocker_card(i, t))
    }
    pub fn blocker_card(&self, i: &Interaction, t: &TaskId) -> bool {
        measure::card_read();
        matches!(&i.scope, Scope::Task { task_id } if task_id == t)
            && i.blocking
            && !i.withdrawn
            && i.recipient != Recipient::Orchestrator
            && matches!(
                i.payload,
                InteractionPayload::Question { answer: None, .. }
                    | InteractionPayload::ActionRequest { answer: None, .. }
            )
            && !self.out_of_sight(i)
    }
    pub fn settled(&self, t: &Task, p: &Interaction) -> bool {
        !self.pending_reply(p, &FeedbackTarget::Delivery) && ((self.merged_delivery(p) && self.open_prs(t).next().is_none())
            || (self.accepted(p)
                && matches!(&p.payload, InteractionPayload::Delivery { targets, .. }
                    if self.completable(t, targets, true).is_ok())))
    }
    /// The final delivery is settled: every PR merged, or accepted with nothing left to merge.
    pub fn finished(&self, t: &Task) -> bool {
        t.pending_user_inputs.is_empty()
            && t.final_presentation.as_ref().and_then(|p| self.interactions.get(p)).is_some_and(|p| self.settled(t, p))
    }
    /// A final delivery leaves: the task is in progress again. The lifecycle takes the delivery
    /// back with the input the node goes on with. A running SDK execution is told `text` as that
    /// input, which leaves for its host with the decision; whether it was is the result.
    pub fn hand_back(&mut self, task_id: &TaskId, delivery: &InteractionId, text: String, lifecycles: &mut Lifecycles) -> Result<bool> {
        if self.tasks[task_id].wrap_up.node.is_some() {
            self.tasks.get_mut(task_id).unwrap().wrap_up = WrapUp::default();
        }
        self.node_has_run(task_id, lifecycles)?;
        let through_sdk = self.lifecycle(lifecycles, task_id)?.execution()
            .is_some_and(|stamp| stamp.host().as_str() != alondra_domain::CLAUDE_BG_HOST);
        let feedback = self.lifecycle(lifecycles, task_id)?.execution().is_none().then(|| {
            let InteractionPayload::Delivery { feedback, .. } = &self.interactions[delivery].payload else { return None };
            feedback.last().filter(|feedback| feedback.at_revision == self.revision + 1)
                .map(|feedback| feedback.instruction.message(&feedback.body))
        }).flatten();
        let text = feedback.or_else(|| through_sdk.then_some(text));
        let held: Vec<InteractionId> = self.lifecycle(lifecycles, task_id)?.cards().into_iter().map(|(card, _)| card.clone()).collect();
        let record = LifecycleRecord::HandedBack { delivery: delivery.clone(), input: self.next_input(task_id), text };
        self.decide(lifecycles, task_id, record)?;
        let still = self.lifecycle(lifecycles, task_id)?.cards().into_iter().map(|(card, _)| card.clone()).collect::<Vec<_>>();
        for card in held.iter().filter(|card| !still.contains(card)) {
            self.retire(card);
        }
        self.tasks.get_mut(task_id).unwrap().final_presentation = None;
        Ok(through_sdk)
    }
    /// The node that presents the replacement is the one the notice tells, so the notice is read
    /// inside that turn and never waits for another.
    pub fn consume_replacement_notice(&mut self, task_id: &TaskId, notice: String, lifecycles: &mut Lifecycles) -> Result<()> {
        let lifecycle = self.lifecycle(lifecycles, task_id)?;
        let pending = lifecycle.pending_inputs().iter().any(|input| input.as_str() == notice);
        if pending && matches!(lifecycle.activity(), alondra_domain::ActivityView::Working) {
            self.decide(lifecycles, task_id, LifecycleRecord::InputConsumed { input: notice })?;
        }
        Ok(())
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
    /// The task's node has nothing left to do but close: it confirmed its cleanup, the user chose
    /// to close it, its end is recorded, or the task ended. Such a node never reads as working,
    /// is never taken over, and opens no card. The node of a cancelled task still winds down and
    /// may ask the user until Alondra closes it.
    pub fn node_closing(&self, t: &Task) -> bool {
        t.state.terminal() || t.wrap_up.node.is_some() || self.cleanup_confirmed(t)
    }
    /// The node of an accepted task said its cleanup is done, or the user chose to close it.
    pub fn cleanup_confirmed(&self, t: &Task) -> bool {
        self.in_cleanup(t) && (t.wrap_up.cleaned_at.is_some() || self.post_merge_closed_by_user(&t.id))
    }
    /// What still keeps a Task with a settled final delivery from being done.
    /// A PR closed without merging after the user accepted the delivery settles like a registered
    /// one: only a person closes a PR, and nothing would ever merge it. A merged registered PR the
    /// delivery leaves out settles too, since the user accepted the Task with it merged.
    pub fn completable(&self, t: &Task, targets: &[DeliveryTarget], accepted: bool) -> Result<()> {
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
        if self.open_prs(t).next().is_some() {
            return fail(
                "merge_not_confirmed",
                "The Task's registered PR is still open",
            );
        }
        self.require_checklist_proof(t)?;
        Ok(())
    }
    /// What opening the domain derives. A task stored waiting whose waits are over keeps waiting:
    /// only a command may end the wait, saving it and telling its node (`Command::ReviewWaits`).
    pub fn derive_loaded_states(&mut self) {
        let waiting: BTreeSet<TaskId> = self.tasks.values()
            .filter(|t| t.state == TaskState::WaitingDependency)
            .map(|t| t.id.clone())
            .collect();
        self.derive_states();
        for id in waiting {
            let task = self.tasks.get_mut(&id).unwrap();
            if matches!(task.state, TaskState::NotStarted | TaskState::Running | TaskState::NeedsYou) {
                task.state = TaskState::WaitingDependency;
            }
        }
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
    /// The tasks kept waiting whose waits are over: what `Command::ReviewWaits` moves on.
    pub fn ended_waits(&self) -> Vec<TaskId> {
        let cards = TaskCards::new(self);
        self.tasks.values()
            .filter(|t| t.state == TaskState::WaitingDependency && self.state_within_phase(t, &cards) != TaskState::WaitingDependency)
            .map(|t| t.id.clone())
            .collect()
    }
    pub fn announce_guard(&self, actor: &Actor) -> Result<()> {
        system(actor).map(drop)
    }
    pub fn derive_states_once(&mut self) -> bool {
        let cards = TaskCards::new(self);
        let updates: Vec<_> = self
            .tasks
            .values()
            .filter(|t| !t.state.terminal())
            .map(|t| (t.id.clone(), self.state_within_phase(t, &cards)))
            .collect();
        let mut changed = false;
        for (id, state) in updates {
            let task = self.tasks.get_mut(&id).unwrap();
            changed |= task.state != state;
            task.state = state;
        }
        changed
    }
    /// The revision a command's expected_revision must reach: its Task for
    /// Task-directed commands, the Interaction for decisions on one, and the
    /// whole state for commands without a narrower subject.
    pub fn subject_revision(&self, c: &Command) -> Option<Revision> {
        let task = |id: &TaskId| Some(self.tasks.get(id).map_or(0, |t| t.revision));
        let interaction =
            |id: &InteractionId| Some(self.interactions.get(id).map_or(0, |i| i.revision));
        match c {
            Command::RequestTaskSelection { .. } | Command::MoveTask { .. } => Some(0),
            Command::AssignTask { task_id, .. }
            | Command::RetitleTask { task_id, .. }
            | Command::SetDependencies { task_id, .. }
            | Command::WaitForTasks { task_id, .. }
            | Command::StartTask { task_id }
            | Command::CreateDocument { task_id, .. }
            | Command::ReviseDocument { task_id, .. }
            | Command::LinkTaskDocument { task_id, .. }
            | Command::DecideDocumentReview { task_id, .. }
            | Command::RegisterPr { task_id, .. }
            | Command::PresentResult { task_id, .. }
            | Command::PublishStackReadiness { task_id, .. }
            | Command::CompleteTask { task_id, .. }
            | Command::CancelTask { task_id, .. }
            | Command::AdoptOpenPr { task_id, .. }
            | Command::ClosePrOfCancelledTask { task_id, .. }
            | Command::SettleOpenPr { task_id, .. }
            | Command::ReopenTask { task_id }
            | Command::ConfirmCleanup { task_id, .. }
            | Command::RecordTaskWorktree { task_id, .. }
            | Command::NotePrOpened { task_id, .. }
            | Command::NotePrChanges { task_id }
            | Command::RecordOpenedPr { task_id, .. }
            | Command::AskToRegisterPr { task_id }
            | Command::ReleaseTaskWorktree { task_id, .. }
            | Command::DeclarePostMergeWork { task_id, .. }
            | Command::RecordNodeEnd { task_id, .. }
            | Command::RecordTaskCleanup { task_id }
            | Command::AdmitExecution { task_id, .. }
            | Command::RecordRuntimeFacts { task_id, .. }
            | Command::RecordTurnBoundary { task_id, .. }
            | Command::RecordClaudeHold { task_id, .. }
            | Command::RecordHostEnded { task_id, .. }
            | Command::DeferTaskMove { task_id, .. }
            | Command::AbortTaskSelection { task_id, .. }
            | Command::RecordTaskStages { task_id, .. }
            | Command::ProposeSpecs { task_id, .. }
            | Command::ReviseSpecs { task_id, .. }
            | Command::RecordSpecSettings { task_id, .. }
            | Command::RecordSpecsWritten { task_id, .. }
            | Command::RequestSpecChange { task_id, .. }
            | Command::ProposeAmendment { task_id, .. }
            | Command::ProposeDecision { task_id, .. }
            | Command::ReadyForValidation { task_id, .. }
            | Command::RecordValidation { task_id, .. }
            | Command::EndValidation { task_id, .. }
            | Command::RecordDeliveredValidation { task_id, .. }
            | Command::RecordCommitReading { task_id, .. }
            | Command::BeginStageHandover { task_id, .. }
            | Command::StageHandoverFailed { task_id, .. }
            | Command::RecordHostReleased { task_id, .. }
            | Command::RecordHostRetaken { task_id, .. }
            | Command::RecordInterruptAccepted { task_id, .. }
            | Command::RecordDoubtfulHandoff { task_id, .. }
            | Command::RecordHostLost { task_id, .. }
            | Command::ReconcileRecoveryCards { task_id }
            | Command::ResendUntakenInput { task_id, .. }
            | Command::RecordDroppedInput { task_id, .. }
            | Command::SetModel { task_id, .. }
            | Command::SetPermissionMode { task_id, .. }
            | Command::ReadMcpStatus { task_id }
            | Command::ReconnectMcp { task_id, .. }
            | Command::StartMcpSignIn { task_id, .. }
            | Command::ClearMcpSignIn { task_id, .. }
            | Command::RecordMcpServers { task_id, .. }
            | Command::RecordMcpSignIn { task_id, .. }
            | Command::CompleteMcpSignIn { task_id, .. }
            | Command::WaitForMcp { task_id, .. }
            | Command::TellNode { task_id, .. }
            | Command::RecordDecisionRequest { task_id, .. }
            | Command::RefuseDecisionRequest { task_id, .. }
            | Command::RecordDecisionSettled { task_id, .. }
            | Command::RecordDecisionAborted { task_id, .. }
            | Command::ArchiveTask { task_id, .. }
            | Command::RevokeTaskGrant { task_id, .. }
            | Command::ReportFailure { task_id }
            | Command::HoldHook { task_id, .. }
            | Command::HoldQuestions { task_id, .. }
            | Command::SettlePlanReview { task_id, .. }
            | Command::SettleStall { task_id, .. } => task(task_id),
            Command::PublishInformation { scope, .. }
            | Command::AskQuestion { scope, .. }
            | Command::AskQuestions { scope, .. }
            | Command::RequestAction { scope, .. }
            | Command::RaiseProblem { scope, .. } => match scope {
                Scope::Task { task_id } => task(task_id),
                Scope::Orchestrator { .. } => None,
            },
            Command::SnoozeCard { interaction_id, .. }
            | Command::UnsnoozeCard { interaction_id }
            | Command::MarkRead { interaction_id }
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
            | Command::ObserveMergeVerdict { pr_id, .. }
            | Command::ObservePrMergeMethods { pr_id, .. }
            | Command::ObservePrAddress { pr_id, .. }
            | Command::DetachPr { pr_id, .. } => Some(
                self.pull_requests
                    .get(pr_id)
                    .and_then(|p| self.tasks.get(&p.task_id))
                    .map_or(0, |t| t.revision),
            ),
            // A Schedule's own revision is its subject: Update carries the next
            // one, Delete and Claim the one they read. The rule checks it.
            Command::CreateSchedule { .. } | Command::UpdateSchedule { .. } => Some(0),
            Command::BeginAccountLogin { .. } | Command::CancelAccountLogin { .. } | Command::RecordAccountLogin { .. } => Some(0),
            Command::SetAccountOrder { .. } | Command::SetModelProfile { .. } | Command::ReplaceAccountConfiguration { .. } | Command::RecordAccountCredential { .. } => Some(0),
            // Clearing takes only what the user saw at `seen`, so any snapshot may ask for it.
            Command::ClearCompleted { .. } => Some(0),
            // A setting of this Mac's, not of anything the client read: any snapshot may change it.
            Command::SetMergeMethod { .. } | Command::SetBranchUpdate { .. } => Some(0),
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



    pub fn pr_belonged_to(&self, pr: &PrId, task: &TaskId) -> bool {
        self.pull_requests.get(pr).is_some_and(|pr| &pr.task_id == task)
            || self.pr_task_history.get(pr).is_some_and(|tasks| tasks.contains(task))
    }
    pub fn subject(&self, task: &TaskId, subject: &InteractionSubject) -> Result<()> {
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
    /// Structural validation is also required after loading relational rows.
    /// Historical presentations retain old document revisions and heads deliberately.
    pub fn validate(&self) -> Result<()> {
        id(&self.data_epoch.0)?;
        if self.revision > MAX_REVISION || self.orchestrator_generation > MAX_REVISION {
            return fail("invalid_input", "Unsafe revision or generation");
        };
        self.schedule.validate()?;
        for (pr_id, owners) in &self.pr_task_history {
            if self.pull_requests.get(pr_id).is_none_or(|pr| !owners.contains(&pr.task_id))
                || owners.iter().any(|task| !self.tasks.contains_key(task))
            {
                return fail("invalid_input", "Invalid PR ownership history");
            }
        }
        self.validate_plan_reviews()?;
        if let Some(t) = self.tasks.values().find(|t| t.state.terminal() && t.open_worktrees().next().is_some()) {
            return fail("invalid_input", &format!("Task {} ended with a worktree of its own still there", t.id));
        }
        if let Some(t) = self.tasks.values().find(|t| t.over() && !t.allowed.is_empty()) {
            return fail("invalid_input", &format!("Task {} ended with what was allowed in it still allowed", t.id));
        }
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
            if r.cleared && !r.closed {
                return fail("invalid_input", "Cleared Request is still open");
            }
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
            if t.pr_id.is_none() && !t.later_prs.is_empty() {
                return fail("invalid_input", "Task PR ownership mismatch");
            }
            for p in t.pr_ids() {
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
                if (up.pr_id.as_ref()!=Some(&s.pr_id) && !self.pr_belonged_to(&s.pr_id, &s.upstream_task)) || !self.interactions.get(&s.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:s.upstream_task.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:s.pr_id.clone(),head:s.head.clone()}))){return fail("invalid_input","Stack references wrong upstream presentation")}
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
        for (k, p) in &self.pull_requests {
            id(&k.0)?;
            canonical_pr(&p.host, &p.repository, p.number)?;
            head(&p.head)?;
            nonempty(&p.base)?;
            let t = self.task(&p.task_id)?;
            self.validate_pr_identity(p)?;
            if k != &p.id
                || (p.is_tracked() && !t.pr_ids().any(|id| id == k))
                || (p.is_tracked() && !p.identity.is_unresolvable() && !identities.insert((&p.host, &p.repository, p.number)))
                || (p.review.is_some() != t.review_prs && !self.pr_task_history.contains_key(k))
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
                    if d.at_revision>self.revision || !self.interactions.get(&d.presentation_id).is_some_and(|i|matches!(&i.scope, Scope::Task { task_id } if self.pr_belonged_to(k, task_id)) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:k.clone(),head:d.head.clone()}))){return fail("invalid_input","PR review decision does not match presentation")}
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
                match s {
                    InteractionSubject::PullRequest { pr_id } if self.pr_belonged_to(pr_id, task_id) => (),
                    _ => self.subject(task_id, s)?,
                }
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
                                if !self.pr_belonged_to(pr_id, task_id) {
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
                        if let FeedbackTarget::PullRequest { pr_id, head } = &f.target
                            && !targets.contains(&DeliveryTarget::PullRequest { pr_id: pr_id.clone(), head: head.clone() })
                        {
                            return fail("invalid_input", "Feedback target is not part of its delivery");
                        }
                        if f.at_revision > self.revision {
                            return fail("invalid_input", "Feedback revision from future");
                        }
                        if f.reply_window_start.is_some_and(|start| start > f.at_revision || f.instruction != Instruction::Reply) {
                            return fail("invalid_input", "Feedback reply window mismatch");
                        }
                    }
                }
            }
        }
        for (k, a) in &self.merge_authorizations {
            id(&k.0)?;
            id(&a.user_id)?;
            head(&a.head)?;
            if k!=&a.id || a.at_revision>self.revision || !self.pr_belonged_to(&a.pr_id, &a.task_id) || !self.interactions.get(&a.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:a.task_id.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:a.pr_id.clone(),head:a.head.clone()}))){return fail("invalid_input","Merge authorization relation mismatch")};
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
            if k!=&s.task_id || !self.pr_belonged_to(&s.pr_id, &s.task_id) || !self.interactions.get(&s.presentation_id).is_some_and(|i|i.scope==(Scope::Task{task_id:k.clone()}) && matches!(&i.payload,InteractionPayload::Delivery{targets,..} if targets.contains(&DeliveryTarget::PullRequest{pr_id:s.pr_id.clone(),head:s.head.clone()}))){return fail("invalid_input","Invalid stack readiness relation")}
        }
        Ok(())
    }
}
pub fn validate_options(options: &[QuestionOption]) -> Result<()> {
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
pub fn validate_answer(
    options: &[QuestionOption],
    option: &str,
    inputs: &BTreeMap<String, InputValue>,
    attachments: &[AttachmentRef],
) -> Result<()> {
    let mut text = None;
    for (key, value) in inputs {
        match (key.as_str(), value) {
            (ANSWER_CONTENT, InputValue::Form(_)) => {}
            (_, InputValue::Text(written)) if text.is_none() && written.len() <= 1_048_576 => text = Some(written.as_str()),
            _ => return fail("invalid_input", "An answer carries at most one text: the words written on the card"),
        }
    }
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
pub fn canonical_pr(host: &str, repo: &str, number: u64) -> Result<()> {
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
pub fn validate_attachments(attachments: &[AttachmentRef]) -> Result<()> {
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
        if attachment.bytes > MAX_ATTACHMENT_BYTES || bytes > 128 * 1024 * 1024 {
            return fail("payload_too_large", "Attachment content exceeds limits");
        }
    }
    Ok(())
}
pub fn content(body: &str, attachments: &[AttachmentRef]) -> Result<()> {
    validate_attachments(attachments)?;
    if body.len() > 1_048_576 || body.trim().is_empty() && attachments.is_empty() {
        return fail(
            "invalid_input",
            "Content or attachment required within limits",
        );
    }
    Ok(())
}

/// Tasks in the order they can run: each after the tasks among them it waits on, the higher
/// priority first among those free to go, then the older.
pub fn in_running_order(mut left: Vec<&Task>) -> Vec<TaskId> {
    left.sort_by_key(|t| (t.priority, t.revision, t.id.clone()));
    let mut order: Vec<TaskId> = Vec::with_capacity(left.len());
    while !left.is_empty() {
        let waits = |t: &Task| t.dependencies.iter().chain(&t.delivered_dependencies)
            .any(|d| d != &t.id && left.iter().any(|other| &other.id == d));
        let next = left.iter().position(|t| !waits(t)).unwrap_or(0);
        order.push(left.remove(next).id.clone());
    }
    order
}
