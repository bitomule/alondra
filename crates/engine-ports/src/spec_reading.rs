//! What Alondra reads of a commit of a task with git and the spec engine, for the guard of its
//! delivery: facts only. The domain decides with them (`alondra_domain_model::delivery_validation`).
use crate::spec_worktrees::SpecWorktrees;
use alondra_domain_model::delivery_validation::{CommitReading, LedgerEntry, SpecFileChange, MAX_READING_PROBLEMS};
use alondra_domain_model::specs::{self, engine, Files, SpecFiles, SpecMode};
use alondra_domain_model::validation::{MAX_ROUND_ITEMS, MAX_SCOPE_FILES};
use alondra_domain_model::{DomainState, Task, TaskId, ValidationItem, ValidationKind, ValidationVerdict};
use std::path::Path;

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

fn empty(commit: &str, worktree: bool) -> CommitReading {
    CommitReading { commit: commit.to_owned(), worktree, specs: true, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![], spec_changes: vec![] }
}

/// A reading that says why Alondra could not read `commit`: a fact like any other, which never
/// lets a delivery through.
pub fn unreadable(commit: &str, why: &str) -> CommitReading {
    CommitReading { problems: vec![format!("Alondra could not read commit {commit}: {why}")], ..empty(commit, false) }
}

/// What Alondra reads of `commit` in the repository at `root` for `task`, whose branch may hold
/// `allowed` otherwise than its base.
pub fn read_commit(port: &dyn SpecWorktrees, root: &Path, task: &Task, commit: &str, worktree: bool, allowed: &SpecFiles) -> Result<CommitReading, String> {
    let mut reading = empty(commit, worktree);
    let files = port.commit(root, commit)?;
    let off = matches!(specs::mode(&*files), Ok((SpecMode::Off, _)));
    let base = match port.base_commit(root, commit).and_then(|base| port.commit(root, &base).map(|files| (base, files))) {
        Ok(base) => Some(base),
        Err(_) if off && allowed.is_empty() => None,
        Err(error) => return Err(error),
    };
    if let Some((_, base)) = &base {
        match specs::drift(&*files, &**base, allowed) {
            Ok(found) => reading.spec_changes = found.into_iter().take(MAX_SCOPE_FILES).map(|drift| SpecFileChange { path: drift.path, approved: drift.approved }).collect(),
            Err(problem) => reading.problems.push(format!("alondra.toml cannot be read: {}", problem.located())),
        }
    }
    let project = match load(&*files) {
        Ok(Some(project)) => project,
        Ok(None) => return Ok(CommitReading { specs: false, ..reading }),
        Err(problems) => {
            reading.problems.extend(problems.iter().take(MAX_READING_PROBLEMS).map(|p| format!("{}:{}: {}", p.path, p.line, p.message)));
            reading.problems.truncate(MAX_READING_PROBLEMS);
            return Ok(reading);
        }
    };
    let Some((base_commit, base)) = base else { return Err("the base of the commit cannot be read".into()) };
    let report = project.report(None);
    reading.owed = owed_items(&project, &report).into_iter().take(MAX_ROUND_ITEMS).collect();
    reading.problems.extend(report.problems.iter().map(|p| format!("{}:{}: {}", p.path, p.line, p.message)));
    reading.problems.truncate(MAX_READING_PROBLEMS);
    let before = load(&*base).ok().flatten();
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

/// What Alondra reads before a delivery of `task_id` in stages: the commit each of its open pull
/// requests is at, and the commit its worktree is at for a delivery without one. A commit it
/// cannot read, a pull request of another repository included, refuses the delivery: what is read
/// never depends on a remote the agent can change.
pub fn read_delivery(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId) -> Result<Vec<CommitReading>, String> {
    let Some(task) = state.tasks.get(task_id).filter(|task| task.stages.staged) else { return Ok(vec![]) };
    let Some(root) = port.own(state, task_id) else {
        return if state.open_prs(task).next().is_none() { Ok(vec![]) } else { Err("the task's worktree is gone, so its pull request cannot be read".into()) };
    };
    let allowed = state.allowed_specs(task);
    let mut readings = vec![];
    for pr in state.open_prs(task) {
        readings.push(read_commit(port, &root, task, &pr.head, false, &allowed).map_err(|why| format!("the commit of pull request {}#{} ({}): {why}", pr.repository, pr.number, pr.head))?);
    }
    readings.push(read_commit(port, &root, task, &port.head_commit(&root)?, true, &allowed)?);
    Ok(readings)
}

/// What Alondra reads for a delivery it holds for `task_id`: each commit of its open pull requests
/// it has not read yet, or why it cannot read it.
pub fn read_held(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId) -> Vec<CommitReading> {
    let Some(task) = state.tasks.get(task_id).filter(|task| task.stages.staged) else { return vec![] };
    let heads: Vec<String> = state.open_prs(task).map(|pr| pr.head.clone()).filter(|head| task.validation.reading(head).is_none()).collect();
    let root = port.own(state, task_id);
    let allowed = state.allowed_specs(task);
    heads.iter().map(|head| match &root {
        Some(root) => read_commit(port, root, task, head, false, &allowed).unwrap_or_else(|why| unreadable(head, &why)),
        None => unreadable(head, "the task has no worktree left to read it in"),
    }).collect()
}
