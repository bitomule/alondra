//! What a launch of a task's node is made of, below the engine's subsystems: the mission it starts
//! with, the context a next node carries on from, the checks of the choices it was asked with, and
//! the worktree it owns from the start.
use crate::Runtime;
use crate::domain_runtime::RuntimeRecords as _;
use alondra_domain_model::{Command, DomainError, DomainState, TaskId, WorktreeOrigin};
use alondra_engine_ports::worktrees::Workplace;

pub fn validate_request_selection(domain: &dyn Runtime, resolved: &alondra_core::options::Resolved) -> Result<(), DomainError> {
    let Some(intent) = &resolved.selection else { return Ok(()) };
    let config = crate::accounts_file::config(domain.home()).map_err(|why| DomainError::new("settings_unavailable", why))?;
    if let Some(account) = &intent.account
        && !config.accounts(account.provider).iter().any(|known| known.id == account.account && known.enabled)
    { return Err(DomainError::new("account_unavailable", "The selected account is not connected and enabled")); }
    if let Some(requested) = &intent.requested {
        let read = domain.read();
        let candidate = alondra_core::model_selection::ModelCandidate { model: requested.model.clone(), reasoning_effort: intent.requested_reasoning_effort.clone() };
        let offered = read.index().model_catalogs().iter().filter(|catalog| catalog.account.provider == requested.provider && intent.account.as_ref().is_none_or(|account| catalog.account == *account))
            .any(|catalog| config.accounts(requested.provider).iter().any(|account| account.id == catalog.account.account && account.enabled) && catalog.resolve(&candidate).is_ok());
        if !offered { return Err(DomainError::new("model_unavailable", "Refresh the connected account's models and choose an advertised model and reasoning effort")); }
    }
    Ok(())
}

pub fn validate_native_requirements(intent: Option<&alondra_core::model_selection::TaskSelection>, denied_tools: &[String]) -> Result<(), DomainError> {
    let Some(intent) = intent.filter(|_| !denied_tools.is_empty()) else { return Ok(()) };
    let pinned = intent.account.as_ref().map(|account| account.provider)
        .into_iter().chain(intent.requested.as_ref().filter(|_| !intent.allow_model_substitution).map(|model| model.provider));
    if let Some(provider) = pinned.into_iter().find(|provider| provider.lacks_tool_denials()) {
        return Err(DomainError::new("unsupported_capability", format!("{} cannot enforce this task's denied tools; choose a compatible provider or permit model substitution with Automatic accounts", provider.display_name())));
    }
    Ok(())
}

/// The worktree a launch works in is the task's own from the moment it exists, before any node
/// runs in it: it is cleaned with the task and its subagents' worktrees go beside it, even when the
/// launch fails. A task that ended while the worktree was being made already had its close, so the
/// worktree goes at once, unless it holds work not saved.
pub async fn own_worktree(domain: &dyn Runtime, task: &TaskId, workplace: &Workplace) -> Result<(), DomainError> {
    let Some(dir) = domain.ports().worktrees().chosen(workplace) else { return Ok(()) };
    if !dir.join(".git").is_file() {
        return Ok(());
    }
    let shown = dir.to_string_lossy();
    let recorded = domain.set_task_worktree(task, &shown);
    if recorded.is_ok() && let Some(branch) = domain.ports().worktrees().branch_in(&dir).await {
        let command = Command::RecordTaskWorktree { task_id: task.clone(), path: shown.to_string(), origin: WorktreeOrigin::Task, branch: Some(branch) };
        if let Err(error) = domain.execute_system_command(command) {
            log!("could not record the branch of the worktree of {task}: {error}");
        }
    }
    let ended = domain.state().tasks.get(task).is_some_and(|ended| ended.state.terminal());
    match (recorded, ended) {
        (Ok(()), false) => return Ok(()),
        (Err(error), false) => return Err(DomainError::new("storage_unavailable", format!("could not record that {task} works in {shown}: {error:#}"))),
        (_, true) => {}
    }
    match domain.ports().worktrees().release_recorded_worktree(&dir, false).await {
        Ok(alondra_engine_ports::worktrees::TaskWorktree::Unsaved(left)) => log!("the worktree of {task}, which ended while it started, is kept: {left}"),
        Ok(_) => {}
        Err(why) => log!("could not delete the worktree of {task}, which ended while it started: {why}"),
    }
    Err(DomainError::new("task_not_active", format!("Task is terminal: {task} ended while it started")))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalTaskContext {
    pub text: String,
    pub missing_sources: Vec<String>,
}

/// What a task's next node must know to carry on, whole: its request, plan, every input the user
/// and Alondra gave it, decisions, documents and the work it builds on. A source Alondra no longer
/// has is missing; whether the whole fits the next model is for the handoff to decide.
pub fn canonical_task_context(state: &DomainState, task: &alondra_domain_model::Task) -> CanonicalTaskContext {
    task_record(state, task, true)
}

/// The task's record as `canonical_task_context` gives it, with its approved plan or without it.
pub(crate) fn task_record(state: &DomainState, task: &alondra_domain_model::Task, with_plan: bool) -> CanonicalTaskContext {
    use alondra_domain_model::{InteractionPayload, Recipient, Scope};
    let mut text = format!("Task {}: {}\nRequest: {}", task.id, task.title, task.request_id);
    let mut missing_sources = vec![];
    let mut add = |part: String| {
        if !part.is_empty() { text.push_str("\n\n"); text.push_str(&part); }
    };
    match state.requests.get(&task.request_id) {
        Some(request) => add(format!("## Original request\n{}", request.body)),
        None => { add("[The original request is missing.]".into()); missing_sources.push(format!("request:{}", task.request_id)); }
    }
    if with_plan { add(approved_plan(state, task)); }
    let mut included_inputs = std::collections::BTreeSet::new();
    for record in state.lifecycle_journal.get(&task.id).into_iter().flat_map(|journal| journal.iter()) {
        let (input, said) = match record {
            alondra_domain_model::LifecycleRecord::Told { input, text, .. }
            | alondra_domain_model::LifecycleRecord::Answered { input, text, .. }
            | alondra_domain_model::LifecycleRecord::HandedBack { input, text, .. } => (input, text),
            _ => continue,
        };
        if !included_inputs.insert(input) { continue; }
        match said {
            Some(said) => add(format!("## Public user input ({input})\n{said}")),
            None => missing_sources.push(format!("task:{}:input:{input}", task.id)),
        }
    }
    add(alondra_domain_model::asked_changes(state, task));
    let mut public: Vec<_> = state.interactions.values().filter(|interaction| interaction.scope == (Scope::Task { task_id: task.id.clone() }) && interaction.recipient == Recipient::User).collect();
    public.sort_by_key(|interaction| interaction.order);
    for interaction in public {
        if let InteractionPayload::Question { answer: Some(answer), .. } | InteractionPayload::ActionRequest { answer: Some(answer), .. } = &interaction.payload {
            let said = alondra_domain_model::answer_said(interaction).unwrap_or_default();
            add(format!("Public user decision {}: {said} (revision {})", interaction.id, answer.at_revision));
        }
    }
    for (id, revision) in &task.documents {
        add(format!("Document attached to this task: {id}, revision {revision}"));
    }
    let dependencies: std::collections::BTreeSet<_> = task.dependencies.iter().chain(&task.delivered_dependencies)
        .chain(task.stack.iter().map(|stack| &stack.upstream_task)).collect();
    for dependency in dependencies {
        match state.tasks.get(dependency) {
            Some(earlier) => {
                let mut part = format!("## Earlier work {}: {}", earlier.id, earlier.title);
                if let Some(result) = earlier.final_presentation.as_ref().and_then(|id| state.interactions.get(id)) {
                    part.push_str(&format!("\n{}", result.body));
                }
                for pr in state.task_prs(earlier) {
                    part.push_str(&format!("\nPull request: https://{}/{}/pull/{}", pr.host, pr.repository, pr.number));
                }
                add(part);
            }
            None => missing_sources.push(format!("dependency:{dependency}")),
        }
    }
    CanonicalTaskContext { text, missing_sources }
}

/// The approved plan this node needs to implement, including its predecessor's own plan when
/// the task reopens, with only this task's points assigned when the plan is in phases.
pub fn approved_plan(state: &DomainState, task: &alondra_domain_model::Task) -> String {
    use alondra_domain_model::{checklist_items, InteractionPayload, Scope};
    let Some(checklist) = state.checklists.get(&task.id) else { return String::new() };
    let Some(source) = state.interactions.get(&checklist.plan) else { return String::new() };
    if source.scope == (Scope::Task { task_id: task.id.clone() }) && state.pending_reopening(&task.id).is_none() {
        return String::new();
    }
    let plan = match &source.payload {
        InteractionPayload::Delivery { .. } => source.card.plan.as_deref().unwrap_or_default(),
        _ => source.body.as_str(),
    };
    let part = if checklist.items.len() < checklist_items(plan).len() {
        let numbers: Vec<&str> = checklist.items.iter().map(|item| item.id.as_str()).collect();
        format!(" Your part of it is points {}; the other points are other tasks: do not implement them.", numbers.join(", "))
    } else {
        String::new()
    };
    format!("## The plan the user approved

The user approved this plan to be implemented: it is already this task's approved plan, so do not plan again or send it for approval.{part} Implement it, and mark every point of yours done with its evidence in your final result. If a point turns out wrong or impossible, send a revised plan with ExitPlanMode.

{}", plan.trim())
}

/// What the author of a staged task is told when it starts.
pub const AUTHOR_MISSION: &str = "## Your stage

This task goes in stages, and you are its author. Plan it through plan mode: ask the user every decision first, then present the plan with its checklist. Do not build it. When the project has specs (an alondra.toml at the root of the repository that turns them on), load the spec-design skill first and send the plan's specs with propose_specs before presenting the plan. Once the user approves the plan, Alondra hands the task to a new agent that implements it in this same worktree: end your turn then, without changing anything else. You never register or deliver a pull request.";

/// Who wrote the text a node starts with: a decision the orchestrator attributes to the user in a
/// mission is its account of it, while a schedule's text is the user's own.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MissionAuthor {
    Orchestrator,
    Schedule,
}

/// The mission a node starts with, followed by the choices made for it.
pub fn producer_prompt(
    text: &str,
    author: MissionAuthor,
    resolved: &alondra_core::options::Resolved,
    task: &TaskId,
    approach: alondra_core::options::Approach,
) -> String {
    let user = alondra_core::config::user_name();
    let planning = if resolved.selected_provider() == Some(alondra_core::accounts::Provider::Claude) {
        format!("a plan goes through plan mode (`EnterPlanMode`, then `ExitPlanMode`, which {user} approves or sends back in Alondra), never a document or an `ask`")
    } else {
        "submit plans through the Alondra planning tools advertised for this execution and wait for the user approval; use only native planning tools available in this session".to_owned()
    };
    let mut lines = vec![
        text.trim_end().to_owned(),
        String::new(),
        "## Your Alondra task".into(),
        String::new(),
        format!("This work is Alondra task {task}; pass it as task_id to the Alondra tools. Your terminal does not reach {user}: only the Alondra tools do, and {user}'s answers are typed into this session. Use `ask` for a decision and end your turn after it; {planning}. Use `request_action` for something {user} does outside Alondra, and `publish_information` for a finding {user} should read now. When you are done, deliver it with `present_result` and is_final true (the answer in body, files in attachments, a PR you registered or a document revision in targets) and end your turn: {user} accepts it in Alondra. A file or a final message left in this session is not a delivery."),
        String::new(),
    ];
    if author == MissionAuthor::Orchestrator {
        lines.push(format!("The orchestrator wrote this mission: a decision it attributes to {user} is its account of it, not {user}'s answer. Present a decision as {user}'s only from {user}'s own answer or message; ask {user} for one you do not have."));
        lines.push(String::new());
    }
    lines.extend([
        "## Choices for this work".into(),
        String::new(),
        "The user chose these options for this work:".into(),
    ]);
    if resolved.auto_merge {
        lines.push("- PR handling: auto-merge. Register the PR and deliver it with present_result as your final result: once its checks pass, Alondra merges it and tells you. Do not merge it yourself.".into());
    }
    for picked in resolved
        .picked
        .iter()
        .filter(|p| p.id != alondra_core::options::PR && p.id != alondra_core::options::EFFORT)
    {
        lines.push(match &picked.mission {
            Some(mission) => format!("- {}: {}. {mission}", picked.label, picked.value_label),
            None => format!("- {}: {}.", picked.label, picked.value_label),
        });
    }
    if let Some(issue) = &resolved.issue {
        lines.push(format!("- This work comes from issue {issue}: write `Fixes {issue}` in the body of the PR that solves it."));
    }
    lines.push(String::new());
    lines.extend(approach.how_to_work_for(if resolved.selection.is_some() { None } else { resolved.selected_provider() }));
    lines.join("\n")
}
