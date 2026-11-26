//! What Alondra reads of a commit of a task with git and the spec engine, for the guard of its
//! delivery: facts only. The domain decides with them (`alondra_domain_model::delivery_validation`).
use crate::spec_worktrees::SpecWorktrees;
use alondra_domain_model::delivery_validation::{CommitReading, LedgerEntry, MAX_READING_PROBLEMS};
use alondra_domain_model::specs::{self, engine, Files, SpecMode};
use alondra_domain_model::validation::{MAX_ROUND_ITEMS, MAX_SCOPE_FILES};
use alondra_domain_model::{DomainState, PullRequest, Task, TaskId, ValidationItem, ValidationKind, ValidationVerdict};
use std::path::{Path, PathBuf};

/// The project the spec engine reads in `files`: `None` when it does not use specs.
pub fn load(files: &dyn Files) -> Result<Option<engine::Project>, Vec<engine::Problem>> {
    match specs::mode(files) {
        Ok((SpecMode::Off, _)) => Ok(None),
        Ok(_) => engine::load(files).map(Some),
        Err(problem) => Err(vec![engine::Problem { path: problem.path, line: problem.line, code: engine::Code::InvalidSettings, message: problem.message }]),
    }
}

pub fn problems_text(problems: &[engine::Problem]) -> String {
    problems.iter().take(10).map(|p| format!("{}:{}: {}", p.path, p.line, p.message)).collect::<Vec<_>>().join("; ")
}

/// What the engine lists as not validated, each at the fingerprint Alondra reads.
pub fn owed_items(project: &engine::Project, report: &engine::Report) -> Vec<ValidationItem> {
    report.items.iter()
        .filter(|item| item.state != engine::State::Valid)
        .filter_map(|item| {
            let kind = ValidationKind::of_engine(item.kind)?;
            Some(ValidationItem { subject: item.subject.clone(), kind, inputs: project.fingerprint(&item.subject, item.kind)? })
        })
        .collect()
}

pub fn ledger_dir(project: &engine::Project) -> String {
    format!("{}/ledger/", project.settings().specs.dir.trim_end_matches('/'))
}

/// What Alondra reads of `commit` in the repository at `root` for `task`.
pub fn read_commit(port: &dyn SpecWorktrees, root: &Path, task: &Task, commit: &str, worktree: bool) -> Result<CommitReading, String> {
    let mut reading = CommitReading { commit: commit.to_owned(), worktree, specs: true, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![] };
    let files = port.commit(root, commit)?;
    let project = match load(&*files) {
        Ok(Some(project)) => project,
        Ok(None) => return Ok(CommitReading { specs: false, ..reading }),
        Err(problems) => {
            reading.problems = problems.iter().take(MAX_READING_PROBLEMS).map(|p| format!("{}:{}: {}", p.path, p.line, p.message)).collect();
            return Ok(reading);
        }
    };
    let report = project.report(None);
    reading.owed = owed_items(&project, &report).into_iter().take(MAX_ROUND_ITEMS).collect();
    reading.problems = report.problems.iter().take(MAX_READING_PROBLEMS).map(|p| format!("{}:{}: {}", p.path, p.line, p.message)).collect();
    let base_commit = port.base_commit(root, commit)?;
    let before = load(&*port.commit(root, &base_commit)?).ok().flatten();
    let mut seen = std::collections::BTreeSet::new();
    for item in &report.items {
        let Some(kind) = ValidationKind::of_engine(item.kind) else { continue };
        if !seen.insert((item.subject.clone(), kind)) {
            continue;
        }
        let Some(now) = project.recorded(&item.subject, item.kind) else { continue };
        if before.as_ref().and_then(|before| before.recorded(&item.subject, item.kind)).as_ref() == Some(&now) {
            continue;
        }
        let verdict = match now.verdict {
            None | Some(engine::Verdict::Met) => ValidationVerdict::Met,
            Some(engine::Verdict::Unmet) => ValidationVerdict::Unmet,
        };
        reading.ledger.push(LedgerEntry { subject: item.subject.clone(), kind, inputs: now.inputs, verdict });
    }
    reading.ledger.truncate(MAX_ROUND_ITEMS);
    let ledger = ledger_dir(&project);
    let branch = port.changed(root, &base_commit, commit)?;
    reading.stray = report.leftover.iter().filter(|path| path.starts_with(&ledger) && branch.contains(path)).take(MAX_SCOPE_FILES).cloned().collect();
    if let Some(round) = task.validation.current() {
        reading.since = Some(round.implemented.clone());
        reading.changed = port.changed(root, &round.implemented, commit)?.into_iter().filter(|path| !path.starts_with(&ledger)).take(MAX_SCOPE_FILES).collect();
    }
    Ok(reading)
}

/// The open pull requests of `task` in the repository of its worktree at `root`.
pub fn own_prs<'a>(port: &dyn SpecWorktrees, state: &'a DomainState, task: &Task, root: &Path) -> Result<Vec<&'a PullRequest>, String> {
    let repository = port.repository(root)?;
    Ok(state.open_prs(task)
        .filter(|pr| repository.as_deref().is_none_or(|repository| repository.eq_ignore_ascii_case(&pr.repository)))
        .collect())
}

/// What Alondra reads before a delivery of `task_id` in stages: the commit each of its open pull
/// requests is at, and the commit its worktree is at for a delivery without one. A pull request
/// of another repository is outside the specs of the task's worktree: its commit reads as one
/// without specs.
pub fn read_delivery(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId) -> Result<Vec<CommitReading>, String> {
    let Some(task) = state.tasks.get(task_id).filter(|task| task.stages.staged) else { return Ok(vec![]) };
    let Some(root) = port.own(state, task_id) else {
        return if state.open_prs(task).next().is_none() { Ok(vec![]) } else { Err("the task's worktree is gone, so its pull request cannot be read".into()) };
    };
    let own: Vec<&PullRequest> = own_prs(port, state, task, &root)?;
    let mut readings = vec![];
    for pr in state.open_prs(task) {
        readings.push(if own.iter().any(|mine| mine.id == pr.id) {
            read_commit(port, &root, task, &pr.head, false)?
        } else {
            CommitReading { commit: pr.head.clone(), worktree: false, specs: false, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![] }
        });
    }
    readings.push(read_commit(port, &root, task, &port.head_commit(&root)?, true)?);
    Ok(readings)
}

/// The commits of `task_id`'s open pull requests a held delivery waits on that Alondra has not read
/// yet, and the worktree to read them in.
pub fn unread_held(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId) -> Option<(PathBuf, Vec<String>)> {
    let task = state.tasks.get(task_id).filter(|task| task.stages.staged)?;
    let root = port.own(state, task_id)?;
    let heads: Vec<String> = state.open_prs(task).map(|pr| pr.head.clone()).filter(|head| task.validation.reading(head).is_none()).collect();
    (!heads.is_empty()).then_some((root, heads))
}
