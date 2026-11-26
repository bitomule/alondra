//! What Alondra reads of a commit of a task with git and the spec engine, for the guard of its
//! delivery: facts only. The domain decides with them (`alondra_domain_model::delivery_validation`).
use crate::spec_worktrees::SpecWorktrees;
use alondra_domain_model::delivery_validation::{CommitReading, LedgerEntry, SpecFileChange, MAX_READING_PROBLEMS};
use alondra_domain_model::specs::{self, engine, Files, SpecFiles, SpecMode};
use alondra_domain_model::validation::{MAX_ROUND_ITEMS, MAX_SCOPE_FILES};
use alondra_domain_model::{DomainState, PrId, Task, TaskId, ValidationItem, ValidationKind, ValidationVerdict};
use std::collections::BTreeMap;
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
    CommitReading { commit: commit.to_owned(), worktree, specs: true, owed: vec![], problems: vec![], ledger: vec![], stray: vec![], since: None, changed: vec![], base: None, tip: None, spec_changes: vec![] }
}

/// A reading that says why Alondra could not read `commit`: a fact like any other, which never
/// lets a delivery through.
pub fn unreadable(commit: &str, why: &str) -> CommitReading {
    CommitReading { problems: vec![format!("Alondra could not read commit {commit}: {why}")], ..empty(commit, false) }
}

/// What Alondra reads of `commit` in the repository at `root` for `task`, whose branch may hold
/// `allowed` otherwise than its base. A commit whose base cannot be read is not read: what it says
/// of specs is never taken without comparing it with its base. A pull request's commit is compared
/// with where it leaves `tip`, the commit GitHub reports its base branch at, never with what the
/// repository's own references say; the worktree's, read for a delivery without a pull request,
/// with the repository's default branch.
pub fn read_commit(port: &dyn SpecWorktrees, root: &Path, task: &Task, commit: &str, tip: Option<&str>, allowed: &SpecFiles) -> Result<CommitReading, String> {
    let mut reading = empty(commit, tip.is_none());
    let files = port.commit(root, commit)?;
    let base_commit = match tip {
        Some(tip) => {
            port.commit(root, tip)?;
            reading.tip = Some(tip.to_owned());
            port.merge_base(root, commit, tip)?
        }
        None => port.base_commit(root, commit)?,
    };
    let base = port.commit(root, &base_commit)?;
    reading.base = Some(base_commit.clone());
    match specs::drift(&*files, &*base, allowed) {
        Ok(found) => reading.spec_changes = found.into_iter().take(MAX_SCOPE_FILES).map(|drift| SpecFileChange { path: drift.path, approved: drift.approved }).collect(),
        Err(problem) => reading.problems.push(format!("alondra.toml cannot be read: {}", problem.located())),
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
/// requests is at, against the commit GitHub reports its base branch at (`tips`, by pull request),
/// and the commit its worktree is at for a delivery without one. A commit it cannot read, a pull
/// request of another repository included, refuses the delivery: what is read never depends on a
/// remote or a reference the agent can change.
pub fn read_delivery(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId, tips: &BTreeMap<PrId, String>) -> Result<Vec<CommitReading>, String> {
    let Some(task) = state.tasks.get(task_id).filter(|task| task.stages.staged) else { return Ok(vec![]) };
    let Some(root) = port.own(state, task_id) else {
        return if state.open_prs(task).next().is_none() { Ok(vec![]) } else { Err("the task's worktree is gone, so its pull request cannot be read".into()) };
    };
    let allowed = state.allowed_specs(task);
    let mut readings = vec![];
    for pr in state.open_prs(task) {
        let tip = tips.get(&pr.id).ok_or_else(|| format!("GitHub did not report the base branch of pull request {}#{}, so its commit cannot be compared with it", pr.repository, pr.number))?;
        readings.push(read_commit(port, &root, task, &pr.head, Some(tip), &allowed).map_err(|why| format!(
            "the commit {} of pull request {}#{} cannot be read in the task's worktree ({why}). A task in stages delivers pull requests of its own repository only, \
             because Alondra reads every delivered commit with the spec engine: close a pull request of another repository, or do that work in a task of its own",
            pr.head, pr.repository, pr.number))?);
    }
    readings.push(read_commit(port, &root, task, &port.head_commit(&root)?, None, &allowed)?);
    Ok(readings)
}

/// What Alondra reads for a delivery it holds for `task_id`: each commit of its open pull requests
/// it has not read yet, against the commit GitHub reports its base branch at, or why it cannot.
pub fn read_held(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId, tips: &BTreeMap<PrId, String>) -> Vec<CommitReading> {
    let Some(task) = state.tasks.get(task_id).filter(|task| task.stages.staged) else { return vec![] };
    let root = port.own(state, task_id);
    let allowed = state.allowed_specs(task);
    state.open_prs(task).filter(|pr| task.validation.reading_of(&pr.head, false).is_none()).map(|pr| {
        let Some(root) = &root else { return unreadable(&pr.head, "the task has no worktree left to read it in") };
        let Some(tip) = tips.get(&pr.id) else { return unreadable(&pr.head, "GitHub did not report the base branch of its pull request") };
        read_commit(port, root, task, &pr.head, Some(tip), &allowed).unwrap_or_else(|why| unreadable(&pr.head, &why))
    }).collect()
}
