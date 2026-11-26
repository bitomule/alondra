//! What Alondra reads of a task's worktree for its validation: what the spec engine lists as not
//! validated when the implementer says the work is ready, the fingerprint of each thing the
//! validator records, and the gate a delivery passes before it reaches the user. Everything is read
//! from git's objects of a commit, never from what an agent says; the rules are the domain's.
use crate::domain_runtime::DomainRuntime;
use alondra_domain_model::specs::{self, engine, Files, SpecMode};
use alondra_domain_model::{
    Command, DeclaringTest, DeliveredScenario, DeliveredSection, DeliveredSpec, DeliveredSuite, DeliveredValidation, DeliveredVerdict, DomainError, DomainState,
    ExecutionRole, InteractionId, RoundState, Task, TaskId, ValidationItem, ValidationKind, ValidationVerdict,
};
use alondra_engine_ports::spec_worktrees::SpecWorktrees;
use std::collections::BTreeSet;
use std::path::Path;

/// Why a delivery does not pass the gate, each with the code the refusal carries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GateRefusal {
    /// The implementer delivers while the engine still lists something to validate.
    ValidationRequired(Vec<String>),
    /// The spec engine does not read the delivered commit clean.
    ValidationPending(Vec<String>),
    /// A change of the specs still waits for its author or the user.
    SpecChangeOpen,
    /// The delivered ledger holds entries no validator recorded.
    LedgerUnrecorded(Vec<String>),
    /// Files other than the ledger changed after the commit the validator checked.
    ChangedAfterValidation(Vec<String>),
    /// Git could not read the worktree, or not in time.
    Unreadable(String),
}

impl GateRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::ValidationRequired(_) => "validation_required",
            Self::ValidationPending(_) => "validation_pending",
            Self::SpecChangeOpen => "spec_change_open",
            Self::LedgerUnrecorded(_) => "ledger_unrecorded",
            Self::ChangedAfterValidation(_) => "changed_after_validation",
            Self::Unreadable(_) => "worktree_unreadable",
        }
    }

    pub(crate) fn error(&self) -> DomainError {
        let listed = |items: &[String]| items.iter().take(20).cloned().collect::<Vec<_>>().join(", ");
        let message = match self {
            Self::ValidationRequired(items) => format!(
                "Nothing was presented: the spec engine lists what is not validated on your commit ({}). In a task in stages a validator checks it: commit your work and call ready_for_validation.",
                listed(items)),
            Self::ValidationPending(items) => format!(
                "Nothing was presented: alondra spec validate is not clean on the delivered commit ({}). Validate each, record it with record_validation, commit only the ledger and deliver again.",
                listed(items)),
            Self::SpecChangeOpen => "Nothing was presented: a change of the specs still waits for its author or the user. The work is delivered once it is settled.".to_owned(),
            Self::LedgerUnrecorded(items) => format!(
                "Nothing was presented: the ledger holds entries no validator recorded in Alondra ({}). Record each with record_validation, or remove the entry and validate it again.",
                listed(items)),
            Self::ChangedAfterValidation(paths) => format!(
                "Nothing was presented: since the commit the validator checked, files other than the ledger changed ({}). A validator never changes code, tests or specs: take those changes out, or end the round with end_validation so the implementer makes them.",
                listed(paths)),
            Self::Unreadable(why) => format!("Nothing was presented: Alondra could not read the task's worktree to check its validation: {why}. Fix what it says and present the result again."),
        };
        DomainError::new(self.code(), message)
    }
}

fn unreadable(error: impl std::fmt::Display) -> DomainError {
    DomainError::new("worktree_unreadable", format!(
        "Alondra could not read the task's worktree with git to check its specs: {error}. Fix what git says and send the same call again."))
}

fn load(files: &dyn Files) -> Result<Option<engine::Project>, Vec<engine::Problem>> {
    match specs::mode(files) {
        Ok((SpecMode::Off, _)) => Ok(None),
        Ok(_) => engine::load(files).map(Some),
        Err(problem) => Err(vec![engine::Problem { path: problem.path, line: problem.line, code: engine::Code::InvalidSettings, message: problem.message }]),
    }
}

fn problems_text(problems: &[engine::Problem]) -> String {
    problems.iter().take(10).map(|p| format!("{}:{}: {}", p.path, p.line, p.message)).collect::<Vec<_>>().join("; ")
}

/// What the engine lists as not validated: every item that is not valid, by subject and kind.
fn pending(report: &engine::Report) -> Vec<String> {
    report.items.iter().filter(|item| item.state != engine::State::Valid)
        .map(|item| format!("{} {} {}", item.subject, item.kind.as_str(), item.state.as_str()))
        .collect()
}

/// The components the task's work is about: those its approved specs belong to, and those
/// whose validations a round listed; every component when it names none.
fn task_components(project: &engine::Project, task: &Task) -> BTreeSet<String> {
    let approved: BTreeSet<&str> = task.specs.approved.iter().flat_map(|approved| approved.files.keys()).map(String::as_str).collect();
    let mut named: BTreeSet<String> = project.specs().iter().filter(|spec| approved.contains(spec.path.as_str())).map(|spec| spec.component.clone()).collect();
    for item in task.validation.rounds.iter().flat_map(|round| &round.items) {
        if let Some(spec) = project.specs().iter().find(|spec| item.subject.split('/').next() == Some(spec.spec.id.as_str())) {
            named.insert(spec.component.clone());
        }
        if item.kind == ValidationKind::Suite && let Some(component) = item.subject.split('/').next() {
            named.insert(component.to_owned());
        }
    }
    if named.is_empty() {
        named = project.components().iter().map(|component| component.id.clone()).collect();
    }
    named
}

/// The files of `changed` that belong to none of the task's components nor to the specs
/// themselves.
fn outside(project: &engine::Project, task: &Task, changed: &[String]) -> Vec<String> {
    let names = task_components(project, task);
    let settings = project.settings();
    let dirs: Vec<String> = [Some(&settings.specs.dir), settings.decisions.enabled.then_some(&settings.decisions.dir), settings.reference.enabled.then_some(&settings.reference.dir)]
        .into_iter().flatten().map(|dir| format!("{}/", dir.trim_end_matches('/'))).collect();
    changed.iter()
        .filter(|path| path.as_str() != specs::SETTINGS_PATH && !dirs.iter().any(|dir| path.starts_with(dir.as_str())))
        .filter(|path| !project.components().iter().filter(|c| names.contains(&c.id))
            .any(|c| c.owns.iter().chain(&c.inputs).any(|pattern| engine::matches(pattern, path))))
        .cloned()
        .collect()
}

fn ledger_dir(project: &engine::Project) -> String {
    format!("{}/ledger/", project.settings().specs.dir.trim_end_matches('/'))
}

/// Fills in a validation command what Alondra reads of the task's worktree, in place of whatever
/// its sender put there.
pub(crate) fn stamp(domain: &DomainRuntime, command: &mut Command) -> Result<(), DomainError> {
    if !matches!(command, Command::ReadyForValidation { .. } | Command::RecordValidation { .. }) {
        return Ok(());
    }
    let state = domain.state().map_err(|_| DomainError::new("storage_unavailable", "domain state could not be read"))?;
    let Some(port) = domain.ports().spec_worktrees() else { return Ok(()) };
    stamp_with(&*port, &state, command)
}

pub(crate) fn stamp_with(port: &dyn SpecWorktrees, state: &DomainState, command: &mut Command) -> Result<(), DomainError> {
    match command {
        Command::ReadyForValidation { task_id, head, items, scope } => {
            head.clear();
            items.clear();
            scope.clear();
            let Some(root) = port.own(state, task_id) else { return Ok(()) };
            let Some(task) = state.tasks.get(task_id) else { return Ok(()) };
            let left = port.uncommitted(&root).map_err(unreadable)?;
            if !left.is_empty() {
                return Err(DomainError::new("uncommitted_work", format!(
                    "Commit your work first: a validator checks a commit, and these files are not in one: {}", left.iter().take(20).cloned().collect::<Vec<_>>().join(", "))));
            }
            let commit = port.head_commit(&root).map_err(unreadable)?;
            let files = port.commit(&root, &commit).map_err(unreadable)?;
            let project = match load(&*files) {
                Ok(Some(project)) => project,
                Ok(None) => return Err(DomainError::new("specs_disabled", "This project does not use specs: deliver with present_result as usual")),
                Err(problems) => return Err(DomainError::new("specs_unreadable", format!("The spec engine cannot read your commit: {}", problems_text(&problems)))),
            };
            let report = project.report(None);
            let uncovered: Vec<String> = report.items.iter().filter(|item| item.state == engine::State::Uncovered).map(|item| item.subject.clone()).collect();
            if !uncovered.is_empty() {
                return Err(DomainError::new("specs_uncovered", format!(
                    "No test declares these scenarios, and they are not judgments: {}. Mark the test that checks each with a `spec: <spec>/<scenario>` comment above it before asking for validation.",
                    uncovered.join(", "))));
            }
            *head = commit.clone();
            *items = report.items.iter()
                .filter(|item| item.state != engine::State::Valid)
                .filter_map(|item| {
                    let kind = ValidationKind::of_engine(item.kind)?;
                    Some(ValidationItem { subject: item.subject.clone(), kind, inputs: project.fingerprint(&item.subject, item.kind)? })
                })
                .collect();
            if let Ok(base) = port.base_commit(&root)
                && let Ok(changed) = port.changed(&root, &base, &commit)
            {
                *scope = outside(&project, task, &changed);
            }
        }
        Command::RecordValidation { task_id, subject, check: kind, inputs, .. } => {
            inputs.clear();
            let Some(round) = state.tasks.get(task_id).and_then(|task| task.validation.current()).filter(|round| round.state == RoundState::Validating) else { return Ok(()) };
            let Some(root) = port.own(state, task_id) else { return Ok(()) };
            let files = port.commit(&root, &round.head).map_err(unreadable)?;
            if let Ok(Some(project)) = load(&*files) {
                *inputs = project.fingerprint(subject.trim(), kind.engine()).unwrap_or_default();
            }
        }
        _ => {}
    }
    Ok(())
}

/// What a final delivery of `task` must hold for its validation, and, when it passes, what its
/// card shows of its specs. Only tasks in stages in a project with specs are gated.
pub(crate) fn gate(domain: &DomainRuntime, state: &DomainState, task_id: &TaskId, delivery: &InteractionId) -> Result<Option<DeliveredValidation>, GateRefusal> {
    let Some(port) = domain.ports().spec_worktrees() else { return Ok(None) };
    gate_with(&*port, state, task_id, delivery)
}

pub(crate) fn gate_with(port: &dyn SpecWorktrees, state: &DomainState, task_id: &TaskId, delivery: &InteractionId) -> Result<Option<DeliveredValidation>, GateRefusal> {
    let Some(task) = state.tasks.get(task_id) else { return Ok(None) };
    let role = task.stages.role();
    if !matches!(role, Some(ExecutionRole::Implementer | ExecutionRole::Validator)) {
        return Ok(None);
    }
    let Some(root) = port.own(state, task_id) else { return Ok(None) };
    let read = |error: String| GateRefusal::Unreadable(error);
    let head = port.head_commit(&root).map_err(read)?;
    let files = port.commit(&root, &head).map_err(read)?;
    let project = match load(&*files) {
        Ok(Some(project)) => project,
        Ok(None) => return Ok(None),
        Err(problems) => return Err(GateRefusal::ValidationPending(vec![problems_text(&problems)])),
    };
    let report = project.report(None);
    let left = pending(&report);
    if role == Some(ExecutionRole::Implementer) {
        return if left.is_empty() { Ok(None) } else { Err(GateRefusal::ValidationRequired(left)) };
    }
    if !left.is_empty() || !report.problems.is_empty() {
        let mut all = left;
        all.extend(report.problems.iter().map(|p| format!("{}:{}: {}", p.path, p.line, p.message)));
        return Err(GateRefusal::ValidationPending(all));
    }
    if task.specs.amendments.iter().any(|a| matches!(a.state, alondra_domain_model::AmendmentState::Requested | alondra_domain_model::AmendmentState::Proposed)) {
        return Err(GateRefusal::SpecChangeOpen);
    }
    let base_commit = port.base_commit(&root).map_err(read)?;
    let base = port.commit(&root, &base_commit).map_err(read)?;
    let before = load(&*base).ok().flatten();
    let unrecorded = unrecorded(&project, before.as_ref(), &report, task, port, &root, &base_commit, &head)?;
    if !unrecorded.is_empty() {
        return Err(GateRefusal::LedgerUnrecorded(unrecorded));
    }
    if let Some(round) = task.validation.current().filter(|round| round.head != head) {
        let ledger = ledger_dir(&project);
        let changed: Vec<String> = port.changed(&root, &round.head, &head).map_err(read)?.into_iter().filter(|path| !path.starts_with(&ledger)).collect();
        if !changed.is_empty() {
            return Err(GateRefusal::ChangedAfterValidation(changed));
        }
    }
    let changed = port.changed(&root, &base_commit, &head).map_err(read)?;
    Ok(Some(snapshot(&project, &report, task, delivery, &head, &changed)))
}

/// Every ledger entry of the delivered commit that differs from where the branch leaves its base
/// and that no validator recorded in Alondra with the same fingerprint and verdict.
#[allow(clippy::too_many_arguments)]
fn unrecorded(project: &engine::Project, before: Option<&engine::Project>, report: &engine::Report, task: &Task,
    port: &dyn SpecWorktrees, root: &Path, base: &str, head: &str) -> Result<Vec<String>, GateRefusal> {
    let mut seen = BTreeSet::new();
    let mut out = vec![];
    for item in &report.items {
        let Some(kind) = ValidationKind::of_engine(item.kind) else { continue };
        if !seen.insert((item.subject.clone(), kind)) {
            continue;
        }
        let Some(now) = project.recorded(&item.subject, item.kind) else { continue };
        if before.and_then(|before| before.recorded(&item.subject, item.kind)).as_ref() == Some(&now) {
            continue;
        }
        let verdict = match now.verdict {
            None | Some(engine::Verdict::Met) => ValidationVerdict::Met,
            Some(engine::Verdict::Unmet) => ValidationVerdict::Unmet,
        };
        if task.validation.record(&item.subject, kind, &now.inputs).is_none_or(|record| record.verdict != verdict) {
            out.push(format!("{} ({})", item.subject, kind.as_str()));
        }
    }
    let ledger = ledger_dir(project);
    let changed = port.changed(root, base, head).map_err(GateRefusal::Unreadable)?;
    out.extend(report.leftover.iter().filter(|path| path.starts_with(&ledger) && changed.contains(path)).cloned());
    Ok(out)
}

/// What the delivery's card shows of its specs: each spec the task's work is about, each scenario
/// with the engine's state of its validations, the tests that declare it and what the validator
/// said of its judgments; the suites; the files outside its components; the reference it changed.
fn snapshot(project: &engine::Project, report: &engine::Report, task: &Task, delivery: &InteractionId, head: &str, changed: &[String]) -> DeliveredValidation {
    let names = task_components(project, task);
    let state_of = |subject: &str, kind: engine::Kind| report.items.iter().find(|item| item.subject == subject && item.kind == kind).map(|item| item.state.as_str().to_owned());
    let mut suites = vec![];
    for component in project.components().iter().filter(|component| names.contains(&component.id)) {
        for suite in &component.suites {
            let id = format!("{}/{}", component.id, suite.id);
            let record = project.fingerprint(&id, engine::Kind::Suite).and_then(|inputs| task.validation.record(&id, ValidationKind::Suite, &inputs));
            suites.push(DeliveredSuite { state: state_of(&id, engine::Kind::Suite).unwrap_or_else(|| "valid".into()), verdict: record.map(|r| r.verdict), output: record.and_then(|r| r.output.clone()), id });
        }
    }
    let specs = project.specs().iter().filter(|spec| names.contains(&spec.component)).map(|loaded| {
        let scenarios = loaded.spec.scenarios.iter().map(|scenario| {
            let id = format!("{}/{}", loaded.spec.id, scenario.id);
            let states = [engine::Kind::Passes, engine::Kind::Adequate, engine::Kind::Judged].into_iter()
                .filter_map(|kind| state_of(&id, kind).map(|state| (kind.as_str().to_owned(), state)))
                .collect();
            let component = project.components().iter().find(|c| c.id == loaded.component);
            let tests = project.markers().iter().filter(|marker| marker.scenario == id).map(|marker| DeclaringTest {
                path: marker.path.clone(),
                line: marker.line,
                name: marker.test.clone(),
                suites: component.into_iter().flat_map(|c| c.suites.iter().filter(|s| s.tests.iter().any(|pattern| engine::matches(pattern, &marker.path))).map(move |s| format!("{}/{}", c.id, s.id))).collect(),
            }).collect();
            let verdicts = [ValidationKind::Adequate, ValidationKind::Judged].into_iter().filter_map(|kind| {
                let inputs = project.fingerprint(&id, kind.engine())?;
                let record = task.validation.record(&id, kind, &inputs)?;
                Some(DeliveredVerdict { kind, verdict: record.verdict, reasoning: record.reasoning.clone(), nearest_wrong: record.nearest_wrong.clone(), evidence: record.evidence.clone() })
            }).collect();
            DeliveredScenario {
                title: scenario.title.clone(),
                steps: scenario.steps.iter().map(|step| format!("{} {}", step.keyword.as_str(), step.text)).collect(),
                states,
                tests,
                judged: scenario.judgment.as_ref().map(|judgment| match judgment.evidence {
                    engine::EvidenceKind::None => "judged".to_owned(),
                    other => other.as_str().to_owned(),
                }),
                verdicts,
                id,
            }
        }).collect();
        DeliveredSpec { id: loaded.spec.id.clone(), path: loaded.path.clone(), title: loaded.spec.title.clone(), scenarios }
    }).collect();
    let sections = project.sections().iter().filter(|section| changed.contains(&section.path))
        .map(|section| DeliveredSection { id: section.id.clone(), path: section.path.clone(), text: section.text.clone() })
        .collect();
    DeliveredValidation { delivery: delivery.clone(), head: head.to_owned(), specs, suites, outside: outside(project, task, changed), sections }
}

/// Keeps what the validator's delivery showed of its specs for its card. A card without it says
/// its evidence is not available, so a failure here never stops the delivery.
pub(crate) fn record_delivered(domain: &DomainRuntime, task_id: &TaskId, delivery: &InteractionId, validation: DeliveredValidation) {
    let Ok(state) = domain.state() else { return };
    let envelope = alondra_domain_model::CommandEnvelope {
        protocol_version: alondra_domain_model::PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("delivered-validation-{}", delivery.0).into(),
        expected_revision: state.revision,
        command: Command::RecordDeliveredValidation { task_id: task_id.clone(), validation },
    };
    if let Err(error) = domain.execute(&alondra_domain_model::Actor::System, &envelope, None, || Ok(())) {
        log!("{delivery}: its validation could not be kept for its card: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_domain_model::validation::RoundItem;
    use alondra_domain_model::{RoundState, SpecFiles, TaskRun, TaskState, ValidationRecord, ValidationRound, ValidationState};
    use alondra_engine_ports::spec_worktrees::SpecSource;
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::PathBuf;
    use std::sync::Mutex;

    const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n";
    const BOUNDARY: &str = "id = \"billing\"\nowns = [\"crates/billing/**\"]\n\n[[suite]]\nid = \"unit\"\nrun = [\"true\"]\ntests = [\"crates/billing/tests/*.rs\"]\n";
    const FEATURE: &str = "@spec:billing.partial\nFeature: Partial refunds\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n\n  @id:alone @boundary:dependencies\n  Scenario: Billing uses no other component\n    Given the billing crate\n    When its dependencies are listed\n    Then none is another component\n";
    const TESTS: &str = "// spec: billing.partial/part\n#[test]\nfn part() { assert_eq!(refund(30, 10), 10); }\n\n// spec: billing.partial/alone\n#[test]\nfn alone() { assert!(dependencies().is_empty()); }\n";

    #[derive(Clone)]
    struct Tree(BTreeMap<String, Vec<u8>>);
    impl Files for Tree {
        fn paths(&self) -> Vec<String> { self.0.keys().cloned().collect() }
        fn read(&self, path: &str) -> Option<Vec<u8>> { self.0.get(path).cloned() }
    }
    impl Tree {
        fn with(&self, path: &str, text: &str) -> Tree {
            let mut next = self.clone();
            next.0.insert(path.into(), text.as_bytes().to_vec());
            next
        }
        fn with_bytes(&self, path: &str, bytes: Vec<u8>) -> Tree {
            let mut next = self.clone();
            next.0.insert(path.into(), bytes);
            next
        }
    }

    /// A repository as git would show it: named commits, the checked out one and the base.
    struct Repo {
        commits: Mutex<BTreeMap<String, Tree>>,
        head: Mutex<String>,
        dirty: Mutex<Vec<String>>,
    }
    impl Repo {
        fn new(base: Tree) -> Repo {
            Repo { commits: Mutex::new(BTreeMap::from([(sha('0'), base)])), head: Mutex::new(sha('0')), dirty: Mutex::new(vec![]) }
        }
        fn commit(&self, name: char, tree: Tree) -> String {
            self.commits.lock().unwrap().insert(sha(name), tree);
            *self.head.lock().unwrap() = sha(name);
            sha(name)
        }
        fn tree(&self, commit: &str) -> Tree {
            self.commits.lock().unwrap()[commit].clone()
        }
    }
    fn sha(name: char) -> String {
        name.to_string().repeat(40)
    }
    impl SpecWorktrees for Repo {
        fn own(&self, _: &DomainState, _: &TaskId) -> Option<PathBuf> { Some(PathBuf::from("/worktree")) }
        fn all(&self, _: &DomainState, _: &TaskId) -> Vec<PathBuf> { vec![PathBuf::from("/worktree")] }
        fn head(&self, _: &Path) -> Result<SpecSource, String> { Ok(Box::new(self.tree(&self.head.lock().unwrap().clone()))) }
        fn base(&self, _: &Path) -> Result<SpecSource, String> { Ok(Box::new(self.tree(&sha('0')))) }
        fn head_commit(&self, _: &Path) -> Result<String, String> { Ok(self.head.lock().unwrap().clone()) }
        fn commit(&self, _: &Path, commit: &str) -> Result<SpecSource, String> {
            self.commits.lock().unwrap().get(commit).cloned().map(|tree| Box::new(tree) as SpecSource).ok_or_else(|| format!("no commit {commit}"))
        }
        fn base_commit(&self, _: &Path) -> Result<String, String> { Ok(sha('0')) }
        fn changed(&self, _: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
            let (from, to) = (self.tree(from).0, self.tree(to).0);
            let paths: BTreeSet<&String> = from.keys().chain(to.keys()).collect();
            Ok(paths.into_iter().filter(|path| from.get(*path) != to.get(*path)).cloned().collect())
        }
        fn uncommitted(&self, _: &Path) -> Result<Vec<String>, String> { Ok(self.dirty.lock().unwrap().clone()) }
        fn settings(&self, _: &Path) -> Result<Option<Vec<u8>>, String> { Ok(None) }
        fn write(&self, _: &Path, _: &SpecFiles) -> Result<BTreeMap<String, String>, String> { Ok(BTreeMap::new()) }
    }

    fn base() -> Tree {
        Tree(BTreeMap::new()).with("alondra.toml", SETTINGS).with("specs/billing/boundary.toml", BOUNDARY)
            .with("specs/billing/partial.feature", FEATURE).with("crates/billing/tests/refunds.rs", TESTS)
            .with("crates/billing/src/lib.rs", "pub fn refund() {}\n")
    }

    /// The implementer's commit: the code changed, so every validation of billing is owed.
    fn implemented(repo: &Repo) -> String {
        let tree = repo.tree(&sha('0')).with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, part: u32) -> u32 { part.min(paid) }\n");
        repo.commit('1', tree)
    }

    /// The ledger the engine writes for every owed validation on `commit`, met.
    fn ledger_for(repo: &Repo, commit: &str) -> Tree {
        let tree = repo.tree(commit);
        let project = engine::load(&tree).unwrap();
        let mut entries = vec![project.suite_passed("billing", "unit").unwrap()];
        for scenario in ["billing.partial/part", "billing.partial/alone"] {
            entries.push(project.verdict(scenario, engine::Kind::Adequate, engine::Verdict::Met, &[]).unwrap());
        }
        entries.into_iter().fold(tree, |tree, entry| tree.with_bytes(&entry.path, entry.bytes))
    }

    fn task(role: ExecutionRole) -> Task {
        let mut task: Task = serde_json::from_value(serde_json::json!({
            "id": "t", "request_id": "r", "agent_id": "c", "title": "Refund part of an order", "state": "running", "revision": 1,
            "started": true, "review_prs": false, "created_revision": 0, "pending_user_inputs": [], "final_presentation": null, "pr_id": null,
            "dependencies": [], "stack": null, "documents": {}, "archived": false, "model": null,
        })).expect("a task as it is stored");
        task.state = TaskState::Running;
        task.stages.staged = true;
        task.stages.runs.push(TaskRun { generation: 3, agent: "c".into(), role: Some(role), provider: Default::default(), account: None, model: None });
        task
    }

    fn state_with(task: Task) -> DomainState {
        let mut state = DomainState::new("epoch");
        state.tasks.insert("t".into(), task);
        state
    }

    /// A validator's task, its round on `head` with what the engine lists there, and a record of
    /// each at its fingerprint when `recorded`.
    fn validated(repo: &Repo, head: &str, recorded: bool) -> DomainState {
        let mut task = task(ExecutionRole::Validator);
        let project = engine::load(&repo.tree(head)).unwrap();
        let items: Vec<RoundItem> = project.report(None).items.iter().filter(|item| item.state != engine::State::Valid)
            .filter_map(|item| Some(RoundItem { subject: item.subject.clone(), kind: ValidationKind::of_engine(item.kind)?, inputs: project.fingerprint(&item.subject, item.kind)?, state: ValidationState::Met, reason: None }))
            .collect();
        if recorded {
            for item in &items {
                task.validation.records.push(ValidationRecord { subject: item.subject.clone(), kind: item.kind, inputs: item.inputs.clone(), verdict: ValidationVerdict::Met, round: 1, generation: 3,
                    reasoning: Some("Checked".into()), nearest_wrong: None, output: None, evidence: vec![], revision: 1 });
            }
        }
        task.validation.rounds.push(ValidationRound { number: 1, head: head.into(), items, scope: vec![], state: RoundState::Validating, generation: Some(3), findings: None, retried: false });
        state_with(task)
    }

    fn delivery() -> InteractionId {
        InteractionId("result-1".into())
    }

    #[test]
    fn ready_for_validation_lists_what_the_engine_owes_on_the_commit_at_the_fingerprints_alondra_read() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = state_with(task(ExecutionRole::Implementer));
        let mut command = Command::ReadyForValidation { task_id: "t".into(), head: "an agent's guess".into(), items: vec![], scope: vec![] };
        repo.dirty.lock().unwrap().push("crates/billing/src/lib.rs".into());
        assert_eq!(stamp_with(&repo, &state, &mut command).unwrap_err().code, "uncommitted_work", "a validator checks a commit");
        repo.dirty.lock().unwrap().clear();
        stamp_with(&repo, &state, &mut command).unwrap();
        let Command::ReadyForValidation { head: stamped, items, .. } = &command else { unreachable!() };
        assert_eq!(*stamped, head);
        let project = engine::load(&repo.tree(&head)).unwrap();
        let listed: Vec<(&str, ValidationKind)> = items.iter().map(|item| (item.subject.as_str(), item.kind)).collect();
        assert_eq!(listed, [("billing.partial/alone", ValidationKind::Adequate), ("billing.partial/part", ValidationKind::Adequate), ("billing/unit", ValidationKind::Suite)]);
        assert!(items.iter().all(|item| Some(item.inputs.clone()) == project.fingerprint(&item.subject, item.kind.engine())));
    }

    #[test]
    fn a_validators_delivery_passes_when_every_ledger_entry_is_one_it_recorded_and_only_the_ledger_changed() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, true);
        repo.commit('2', ledger_for(&repo, &head));
        let shown = gate_with(&repo, &state, &"t".into(), &delivery()).unwrap().expect("its card shows the specs");
        let scenarios: Vec<&str> = shown.specs.iter().flat_map(|spec| &spec.scenarios).map(|scenario| scenario.id.as_str()).collect();
        assert_eq!(scenarios, ["billing.partial/part", "billing.partial/alone"]);
        let part = &shown.specs[0].scenarios[0];
        assert!(part.states.iter().all(|(_, state)| state == "valid"));
        assert_eq!((part.tests[0].path.as_str(), part.tests[0].line, part.tests[0].suites.clone()), ("crates/billing/tests/refunds.rs", 1, vec!["billing/unit".to_owned()]));
        assert_eq!(part.verdicts[0].verdict, ValidationVerdict::Met);
        assert!(shown.outside.is_empty());
    }

    #[test]
    fn a_ledger_written_by_hand_is_refused_because_no_validator_recorded_it() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, false);
        repo.commit('2', ledger_for(&repo, &head));
        let refused = gate_with(&repo, &state, &"t".into(), &delivery()).unwrap_err();
        assert_eq!(refused.code(), "ledger_unrecorded");
        let GateRefusal::LedgerUnrecorded(entries) = refused else { unreachable!() };
        assert_eq!(entries.len(), 3, "the suite and both judgments: {entries:?}");
    }

    #[test]
    fn a_test_weakened_after_its_judgment_leaves_the_delivery_not_validated() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, true);
        let weakened = ledger_for(&repo, &head).with("crates/billing/tests/refunds.rs", &TESTS.replace("assert_eq!(refund(30, 10), 10);", ""));
        repo.commit('2', weakened);
        let refused = gate_with(&repo, &state, &"t".into(), &delivery()).unwrap_err();
        assert_eq!(refused.code(), "validation_pending");
        let GateRefusal::ValidationPending(left) = refused else { unreachable!() };
        assert!(left.iter().any(|item| item.starts_with("billing.partial/part adequate stale")), "{left:?}");
    }

    #[test]
    fn anything_but_the_ledger_changed_after_the_validated_commit_is_refused() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, true);
        repo.commit('2', ledger_for(&repo, &head).with("README.md", "Refunds\n"));
        let refused = gate_with(&repo, &state, &"t".into(), &delivery()).unwrap_err();
        assert_eq!(refused, GateRefusal::ChangedAfterValidation(vec!["README.md".into()]));
    }

    #[test]
    fn the_implementer_cannot_deliver_while_something_is_owed_and_delivers_as_today_when_nothing_is() {
        let repo = Repo::new(base());
        implemented(&repo);
        let state = state_with(task(ExecutionRole::Implementer));
        assert_eq!(gate_with(&repo, &state, &"t".into(), &delivery()).unwrap_err().code(), "validation_required");
        let unchanged = Repo::new(ledger_for(&Repo::new(base()), &sha('0')));
        assert_eq!(gate_with(&unchanged, &state, &"t".into(), &delivery()), Ok(None));
    }

    #[test]
    fn a_project_without_specs_is_never_gated() {
        let repo = Repo::new(Tree(BTreeMap::new()).with("src/lib.rs", "fn main() {}\n"));
        let state = state_with(task(ExecutionRole::Implementer));
        assert_eq!(gate_with(&repo, &state, &"t".into(), &delivery()), Ok(None));
    }
}
