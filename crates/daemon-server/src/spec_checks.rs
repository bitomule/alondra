//! What Alondra reads of a task's worktrees before it takes a command about its specs or a final
//! delivery: the engine's reading of a proposal, and the spec files a branch changed without the
//! user's approval. Both read what the branch committed, from git's objects, never a file put on
//! the disk. The rules are the domain's; this only hands it the files.
use alondra_engine_ports::Runtime;
use alondra_domain_model::specs::{self, KeptImages as _, Overlay, SpecCheck, SpecFile, SpecFiles, SpecLevel, SpecMode, MAX_MOCKUP_BYTES};
use alondra_engine_ports::kept_images::HomeImages;
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
    let images = HomeImages::new(domain.home());
    let mut over = state.tasks.get(task).and_then(|task| task.specs.approved.as_ref()).map(|approved| approved.allowed()).unwrap_or_default();
    over.extend(changes.iter().map(|(path, content)| (path.clone(), content.clone())));
    let with_approved = Overlay { base: &*head, changes: &over, images: &images };
    let mut check = specs::check(&with_approved, changes, &images);
    if let Ok(base) = port.base(&root) {
        for (path, before) in check.before.iter_mut() {
            *before = specs::content_of(&*base, path, &images);
        }
    }
    Ok(check)
}

/// Keeps the mockup of each file of a change: from the worktree file its `source` names for an
/// agent's proposal, and as the copy Alondra already keeps for the user's edit. What a sender
/// says of a mockup is never taken as it is.
fn keep_mockups(domain: &dyn Runtime, state: &DomainState, task: &TaskId, files: &mut [SpecFile], from_agent: bool) -> Result<(), DomainError> {
    let images = HomeImages::new(domain.home());
    let port = domain.ports().spec_worktrees();
    for (index, file) in files.iter_mut().enumerate() {
        let refused = |field: &str, message: String| {
            let mut refused = DomainError::new("invalid_input", message);
            refused.subject = Some(format!("files[{index}].{field}"));
            refused
        };
        if !from_agent {
            if let Some(sent) = file.mockup.take() {
                let kept = images.read(&sent).and_then(|bytes| images.keep(&bytes))
                    .ok_or_else(|| refused("mockup", format!("Alondra keeps no copy of the mockup of {}", file.path)))?;
                file.mockup = Some(kept);
            }
            continue;
        }
        file.mockup = None;
        let Some(source) = file.source.clone() else { continue };
        let Some((port, root)) = port.as_ref().and_then(|port| Some((port, port.own(state, task)?))) else { continue };
        let bytes = port.file(&root, &source, MAX_MOCKUP_BYTES).map_err(|why| refused("source", format!("Alondra could not read the mockup: {why}")))?;
        file.mockup = Some(images.keep(&bytes).ok_or_else(|| refused("source", format!("{source} is not a PNG or JPEG image Alondra can read")))?);
    }
    Ok(())
}

/// Fills in a spec command what Alondra reads of the worktree, in place of whatever its sender
/// put there: an agent's proposal as a device's edit.
pub(crate) fn stamp(domain: &dyn Runtime, command: &mut Command) -> Result<(), DomainError> {
    let state = domain.state();
    match command {
        Command::ProposeSpecs { task_id, files, .. } | Command::ProposeAmendment { task_id, files, .. } => keep_mockups(domain, &state, task_id, files, true)?,
        Command::ReviseSpecs { task_id, files, .. } => keep_mockups(domain, &state, task_id, files, false)?,
        _ => {}
    }
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
    Some(DomainError::new("specs_differ", alondra_domain_model::specs_differ_text(&unapproved, &other, task.stages.staged)))
}
