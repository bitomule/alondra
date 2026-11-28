//! What a new execution of a task in stages starts from: a short packet for its role that names
//! the files holding the approved plan, the task's record and, for a project that keeps its specs
//! out of the repository, the approved specs. The packet is worked out from the domain's state and
//! the task's folder its caller gives; the caller writes the files it names.
use alondra_domain_model::{DomainState, ExecutionRole, SpecMode, Task};
use alondra_domain_model::specs::SpecContent;

/// The file of the task's folder that holds the approved plan.
pub const PLAN_FILE: &str = "approved-plan.md";
/// The file of the task's folder that holds the task's record.
pub const RECORD_FILE: &str = "task-records.md";
/// The folder of the task's folder that holds specs a project keeps out of the repository.
pub const SPECS_FOLDER: &str = "specs";

/// What one file the packet names holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StageFileBody<'a> {
    Text(String),
    Spec(&'a SpecContent),
}

/// A file the packet names, by its path inside the task's folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StageFile<'a> {
    pub path: String,
    pub body: StageFileBody<'a>,
}

/// The files the packet of `task` names, to write into the task's folder before it starts: the
/// approved plan, the task's record without the plan, and the approved specs a project keeps out
/// of the repository. A source of the record Alondra no longer has is missing.
pub fn stage_files<'a>(state: &DomainState, task: &'a Task) -> Result<Vec<StageFile<'a>>, Vec<String>> {
    let record = crate::launches::task_record(state, task, false);
    if !record.missing_sources.is_empty() {
        return Err(record.missing_sources);
    }
    let mut files = vec![];
    let plan = stage_plan(state, task);
    if !plan.is_empty() {
        files.push(StageFile { path: PLAN_FILE.to_owned(), body: StageFileBody::Text(plan) });
    }
    files.push(StageFile { path: RECORD_FILE.to_owned(), body: StageFileBody::Text(record.text) });
    if let Some(approved) = task.specs.approved.as_ref().filter(|approved| approved.mode == SpecMode::Uncommitted) {
        for (path, content) in &approved.files {
            if let Some(content) = content {
                files.push(StageFile { path: format!("{SPECS_FOLDER}/{path}"), body: StageFileBody::Spec(content) });
            }
        }
    }
    Ok(files)
}

/// What a new execution of a staged task starts from, written by Alondra instead of the
/// conversation of the execution before it: what its role does, where its work is, the files in
/// `folder` (the task's folder in Alondra's home) that hold its plan and record, its specs and
/// the validation that concerns its role.
pub fn stage_packet(state: &DomainState, task: &Task, role: ExecutionRole, worktree: &str, branch: Option<&str>, folder: &str) -> Result<String, Vec<String>> {
    let record = crate::launches::task_record(state, task, false);
    if !record.missing_sources.is_empty() {
        return Err(record.missing_sources);
    }
    let instructions = match role {
        ExecutionRole::Author => "You are the author of this task: plan it through plan mode, ask the user every decision first, and end your turn once the user approves the plan. Do not build it.",
        ExecutionRole::Implementer => "You are the implementer of this task. The user approved the plan named below, which another agent wrote: implement exactly that plan and its tests, validate them as the repository says, and deliver the result with present_result, marking every point of the plan done with its evidence. Do not plan again. When the plan does not cover something, ask the user with `ask` instead of deciding it.",
        ExecutionRole::Validator => "You are the validator of this task: another agent implemented it, and you check it against its specs. Load the spec-validation skill first (read_skill spec-validation where the provider has no Skill tool) and follow it: run and judge every validation listed below, record each with record_validation, then either commit only the ledger, push and deliver with present_result, or send the work back with end_validation. Never change its code, tests, specs or plan.",
    };
    let mut lines = vec![
        format!("# Alondra task {}: {}", task.id, task.title),
        String::new(),
        format!("This work is Alondra task {}; pass it as task_id to the Alondra tools. Load the role skill alondra:alondra before your first Alondra call and follow it: ask through Alondra, and deliver with present_result and is_final true.", task.id),
        String::new(),
        "## Your stage".to_owned(),
        String::new(),
        instructions.to_owned(),
        String::new(),
        format!("Work in {worktree}{}. Earlier work on this task is already there: read it, never redo it.", branch.map(|branch| format!(", on branch {branch}")).unwrap_or_default()),
    ];
    let prs: Vec<String> = state.task_prs(task).map(|pr| format!("- https://{}/{}/pull/{}", pr.host, pr.repository, pr.number)).collect();
    if !prs.is_empty() {
        lines.extend([String::new(), "Its pull requests:".to_owned()]);
        lines.extend(prs);
    }
    if !stage_plan(state, task).is_empty() {
        lines.extend([String::new(), "## The plan the user approved".to_owned(), String::new(), format!(
            "The plan the user approved is in {folder}/{PLAN_FILE}: read it there before you start. It is already this task's approved plan, so do not plan again or send it for approval.{} Implement it, and mark every point of yours done with its evidence in your final result. If a point turns out wrong or impossible, send a revised plan with ExitPlanMode.",
            phase_part(state, task))]);
    }
    let specs = spec_packet(task, role, folder);
    if !specs.is_empty() {
        lines.extend([String::new(), specs]);
    }
    let validation = validation_packet(state, task, role);
    if !validation.is_empty() {
        lines.extend([String::new(), validation]);
    }
    lines.extend([String::new(), "## The task's record".to_owned(), String::new(), format!(
        "The task's record (its request, every message and answer of the user, the changes asked of it, the decisions on its cards, its documents and the earlier work it builds on) is in {folder}/{RECORD_FILE}: search it with grep and read only the part you need, never the whole file.")]);
    Ok(lines.join("\n"))
}

/// The points of a plan in phases that are this task's, when it implements only some of them.
pub fn phase_part(state: &DomainState, task: &Task) -> String {
    use alondra_domain_model::checklist_items;
    let Some(checklist) = state.checklists.get(&task.id) else { return String::new() };
    if checklist.items.len() >= checklist_items(&stage_plan(state, task)).len() {
        return String::new();
    }
    let numbers: Vec<&str> = checklist.items.iter().map(|item| item.id.as_str()).collect();
    format!(" Your part of it is points {}; the other points are other tasks: do not implement them.", numbers.join(", "))
}

/// What a new execution of a staged task is told of its specs: the paths of the approved ones,
/// which win over any other text, and the change of them its role has to draft or wait for.
pub fn spec_packet(task: &Task, role: ExecutionRole, folder: &str) -> String {
    use alondra_domain_model::AmendmentState;
    let Some(approved) = &task.specs.approved else { return String::new() };
    let mut out = vec!["## The approved specs".to_owned(), String::new(),
        "Specs are the only source of truth: where any other text, the plan included, disagrees with a spec, the spec wins.".to_owned(), String::new()];
    let paths: Vec<&str> = approved.files.iter().filter(|(_, content)| content.is_some()).map(|(path, _)| path.as_str()).collect();
    match approved.mode {
        SpecMode::Uncommitted => {
            let files: Vec<String> = paths.iter().map(|path| format!("{folder}/{SPECS_FOLDER}/{path}")).collect();
            out.push(format!("This project keeps its specs in Alondra, never in the repository: do not commit them. Alondra wrote the approved ones into the task's folder: {}. Read them there before you start.", files.join(", ")));
        }
        _ if approved.unwritten.is_empty() => out.push(format!(
            "Alondra wrote the approved spec files into the worktree: {}. Read them in the worktree before you start, and commit them exactly as they are, in a commit of their own if they are not committed yet.", paths.join(", "))),
        _ => out.push(format!("Alondra writes these approved spec files into the worktree: {}. Read them in the worktree before you start.", paths.join(", "))),
    }
    let removed: Vec<&str> = approved.files.iter().filter(|(_, content)| content.is_none()).map(|(path, _)| path.as_str()).collect();
    if !removed.is_empty() {
        out.extend([String::new(), format!("The approved specs remove these files: {}.", removed.join(", "))]);
    }
    out.extend([String::new(), "Never edit a spec, boundary or decision: a delivery whose spec files differ from what the user approved is refused. A change the work needs goes through request_spec_change.".to_owned()]);
    for amendment in &task.specs.amendments {
        match (role, amendment.state) {
            (ExecutionRole::Author, AmendmentState::Requested) => out.extend([String::new(), format!("## The change of specs the implementer asks for ({})", amendment.id), String::new(),
                amendment.reason.clone(), String::new(),
                format!("Draft it with propose_amendment, amendment_id {}, as the whole new text of each file it changes, then end your turn. If the user has to decide something first, ask with `ask` before drafting it.", amendment.id)]),
            (ExecutionRole::Implementer, AmendmentState::Proposed) => out.extend([String::new(), format!("## A change of specs waits for the user ({})", amendment.id), String::new(),
                format!("You asked: {}. The author drafted it and the user decides on its card; carry on with what does not depend on it. Alondra tells you what the user decided.", amendment.reason)]),
            _ => {}
        }
    }
    out.join("\n")
}

/// What a new execution of a staged task is told of its validation: the validator, what its round
/// owes and what an earlier validator recorded; the implementer, why its work came back.
pub fn validation_packet(state: &DomainState, task: &Task, role: ExecutionRole) -> String {
    use alondra_domain_model::{InteractionPayload, RoundState, ValidationState};
    use alondra_core::text::Lang;
    let Some(round) = task.validation.current() else { return String::new() };
    match role {
        ExecutionRole::Validator if round.state.open() => {
            let mut out = vec![format!("## Round {} of validation", round.number), String::new(),
                format!("Validate commit {}: between it and the commit you deliver only the ledger may change. Alondra records the fingerprint of each validation from that commit itself.", round.head),
                String::new(), "What the spec engine listed as not validated on it, and where each stands:".to_owned()];
            out.extend(round.items.iter().map(|item| format!("- {} · {} · {}{}", item.subject, item.kind.as_str(), item.state.as_str(),
                item.reason.as_deref().map(|reason| format!(" ({reason})")).unwrap_or_default())));
            if !round.scope.is_empty() {
                out.extend([String::new(), format!("The branch also changed files outside the components of its specs: {}. Say in your delivery whether each belongs to the work.", round.scope.join(", "))]);
            }
            out.join("\n")
        }
        ExecutionRole::Implementer if round.state == RoundState::Failed => {
            let failing = alondra_domain_model::spec_card::failing(&task.validation, round.number, Lang::En);
            format!("## The validator sent your work back (round {})\n\nFix what it found, commit, and call ready_for_validation again; a new validator checks it. Never change a spec to make a check pass.\n\n{failing}", round.number)
        }
        ExecutionRole::Implementer => {
            let feedback = task.final_presentation.as_ref().or(state.interactions.values().filter(|i| i.scope == alondra_domain_model::Scope::Task { task_id: task.id.clone() }).filter(|i| matches!(i.payload, InteractionPayload::Delivery { is_final: true, .. })).map(|i| &i.id).last());
            let said = feedback.and_then(|id| state.interactions.get(id)).and_then(|i| match &i.payload {
                InteractionPayload::Delivery { feedback, .. } => feedback.last().map(|f| f.body.trim().to_owned()),
                _ => None,
            }).filter(|said| !said.is_empty());
            let returned = round.state == RoundState::Delivered && round.items.iter().all(|item| item.state != ValidationState::Running);
            match (returned, said) {
                (true, Some(said)) => format!("## The validated delivery came back\n\nThe user answered the delivery the validator presented: «{said}». Do that work, commit, and call ready_for_validation again so a validator checks it before it reaches the user."),
                (true, None) => "## The validated delivery came back\n\nDo the work its pull request needs now, commit, and call ready_for_validation again so a validator checks it before it reaches the user.".to_owned(),
                _ => String::new(),
            }
        }
        _ => String::new(),
    }
}

/// The plan the user approved for the task, whoever wrote it.
pub fn stage_plan(state: &DomainState, task: &Task) -> String {
    use alondra_domain_model::InteractionPayload;
    let Some(source) = state.checklists.get(&task.id).and_then(|checklist| state.interactions.get(&checklist.plan)) else { return String::new() };
    match &source.payload {
        InteractionPayload::Delivery { .. } => source.card.plan.as_deref().unwrap_or_default(),
        _ => source.body.as_str(),
    }.trim().to_owned()
}

#[cfg(test)]
#[path = "stage_packet_tests.rs"]
mod tests;
