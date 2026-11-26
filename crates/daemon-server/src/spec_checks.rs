//! What Alondra reads of a task's worktrees before it takes a command about its specs or a final
//! delivery: the engine's reading of a proposal, and the spec files a branch changed without the
//! user's approval. Both read what the branch committed, from git's objects, never a file put on
//! the disk. The rules are the domain's; this only hands it the files.
use alondra_engine_ports::Runtime;
use alondra_domain_model::specs::{self, Overlay, SpecCheck, SpecFiles, SpecLevel, SpecMode};
use alondra_domain_model::{Command, DomainError, DomainState, Task, TaskId};

fn unreadable(error: impl std::fmt::Display) -> DomainError {
    DomainError::new("worktree_unreadable", format!(
        "Alondra could not read the task's worktree with git to check its specs: {error}. Fix what git says and send the same call again."))
}

/// What the engine reads of the task's own worktree with `changes` over it: its last commit, the
/// approved files Alondra wrote into it, then `changes`. What each file held before comes from
/// where the branch leaves its base. Off where nothing reads worktrees or the task has none.
pub(crate) fn read(domain: &dyn Runtime, state: &DomainState, task: &TaskId, changes: &SpecFiles) -> Result<SpecCheck, DomainError> {
    let Some(port) = domain.ports().spec_worktrees() else { return Ok(SpecCheck::default()) };
    let Some(root) = port.own(state, task) else { return Ok(SpecCheck::default()) };
    let head = port.head(&root).map_err(unreadable)?;
    let mut over = state.tasks.get(task).and_then(|task| task.specs.approved.as_ref()).map(|approved| approved.allowed()).unwrap_or_default();
    over.extend(changes.iter().map(|(path, text)| (path.clone(), text.clone())));
    let with_approved = Overlay { base: &*head, changes: &over };
    let mut check = specs::check(&with_approved, changes);
    if let Ok(base) = port.base(&root) {
        for (path, before) in check.before.iter_mut() {
            *before = base.read(path).and_then(|bytes| String::from_utf8(bytes).ok());
        }
    }
    Ok(check)
}

/// Fills in a spec command what Alondra reads of the worktree, in place of whatever its sender
/// put there: an agent's proposal as a device's edit.
pub(crate) fn stamp(domain: &dyn Runtime, command: &mut Command) -> Result<(), DomainError> {
    let state = domain.state();
    match command {
        Command::ProposeSpecs { task_id, level, files, check } | Command::ReviseSpecs { task_id, level, files, check, .. } => {
            *check = match specs::proposal(*level, files) {
                Ok(changes) => read(domain, &state, task_id, &changes)?,
                Err(_) => SpecCheck::default(),
            };
        }
        Command::ProposeAmendment { task_id, files, check, .. } => {
            *check = match specs::proposal(SpecLevel::Light, files) {
                Ok(changes) => read(domain, &state, task_id, &changes)?,
                Err(_) => SpecCheck::default(),
            };
        }
        Command::ProposeDecision { task_id, decision, files, check } => {
            files.clear();
            *check = SpecCheck::default();
            let Some(port) = domain.ports().spec_worktrees() else { return Ok(()) };
            let Some(root) = port.own(&state, task_id) else { return Ok(()) };
            let head = port.head(&root).map_err(unreadable)?;
            let dirs = match specs::mode(&*head) {
                Ok((SpecMode::Off, _)) => return Ok(()),
                Ok((_, dirs)) => dirs,
                Err(problem) => {
                    *check = SpecCheck::unreadable(problem);
                    return Ok(());
                }
            };
            let today = specs::date_of(alondra_core::now_millis());
            *files = specs::decision_files(decision, &today, &dirs, &*head).map_err(|(field, message)| {
                let mut refused = DomainError::new("invalid_input", message);
                refused.subject = Some(field);
                refused
            })?;
            *check = read(domain, &state, task_id, files)?;
        }
        _ => {}
    }
    Ok(())
}

/// Why the final delivery of `task` cannot be taken for its specs, if it cannot: in any worktree
/// of the task, the settings cannot be read, the branch changed a spec, boundary or decision the
/// user never approved, or holds an approved one otherwise than approved. A worktree git cannot
/// read is refused too, unless what it committed shows a project without specs.
pub(crate) fn delivery_refusal(domain: &dyn Runtime, state: &DomainState, task_id: &TaskId) -> Option<DomainError> {
    let port = domain.ports().spec_worktrees()?;
    let task = state.tasks.get(task_id)?;
    let allowed = state.allowed_specs(task);
    let mut unapproved = vec![];
    let mut other = vec![];
    for root in port.all(state, task_id) {
        let head = match port.head(&root) {
            Ok(head) => head,
            Err(error) => return Some(unreadable(error)),
        };
        let base = match port.base(&root) {
            Ok(base) => base,
            Err(_) if allowed.is_empty() && matches!(specs::mode(&*head), Ok((SpecMode::Off, _))) => continue,
            Err(error) => return Some(unreadable(error)),
        };
        match specs::drift(&*head, &*base, &allowed) {
            Err(problem) => return Some(DomainError::new("spec_settings_unreadable", format!(
                "Nothing was presented: the project's alondra.toml cannot be read ({}). Fix it and present the result again: a broken settings file never turns specs off.",
                problem.located()))),
            Ok(found) => for drift in found {
                if drift.approved { other.push(drift.path) } else { unapproved.push(drift.path) }
            },
        }
    }
    if unapproved.is_empty() && other.is_empty() {
        return None;
    }
    let mut parts = vec![];
    if !unapproved.is_empty() {
        parts.push(format!("these spec files changed without the user's approval: {}", unapproved.join(", ")));
    }
    if !other.is_empty() {
        parts.push(format!("these approved spec files are not committed exactly as approved: {}", other.join(", ")));
    }
    Some(DomainError::new("specs_differ", format!(
        "Nothing was presented: {}. In a project with specs only the user changes them: commit each file as it was on the base branch, or as \
         Alondra wrote it when the user approved it; a change the work needs goes through request_spec_change (implementer) or a new plan.",
        parts.join("; "))))
}
