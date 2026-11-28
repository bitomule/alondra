//! What Alondra reads of a task's worktree for its validation: what the spec engine lists as not
//! validated when the implementer says the work is ready, the fingerprint of each thing the
//! validator records, the reading of each commit a delivery carries, and what the delivery's card
//! shows of its specs. Everything is read from git's objects of a commit, never from what an agent
//! says; the decisions are the domain's (`alondra_domain_model::delivery_validation`).
use alondra_engine_ports::Runtime;
use alondra_engine_ports::spec_reading::{self, load, owed_items, problems_text};
use alondra_domain_model::specs::{self, engine, Files, KeptImages};
use alondra_domain_model::DeliveredCapture;
use alondra_engine_ports::kept_images::HomeImages;
use alondra_domain_model::{
    Command, CommitReading, DeclaringTest, DeliveredScenario, DeliveredSection, DeliveredSpec, DeliveredSuite, DeliveredValidation, DeliveredVerdict, DomainError, DomainState,
    ExecutionRole, InteractionId, RoundState, Task, TaskId, ValidationKind,
};
use alondra_engine_ports::spec_worktrees::SpecWorktrees;
use std::collections::BTreeSet;

fn unreadable(error: impl std::fmt::Display) -> DomainError {
    DomainError::new("worktree_unreadable", format!(
        "Alondra could not read the task's worktree with git to check its specs: {error}. Fix what git says and send the same call again."))
}

/// The refusal of a delivery whose commits Alondra could not read.
pub(crate) fn delivery_unreadable(why: &str) -> DomainError {
    let mut error = DomainError::new("worktree_unreadable", format!(
        "Nothing was presented: Alondra could not read the task's worktree to check its validation: {why}. Fix what it says and present the result again."));
    error.subject = Some("is_final".into());
    error
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

/// Fills in a validation command what Alondra reads of the task's worktree, in place of whatever
/// its sender put there.
pub(crate) fn stamp(domain: &dyn Runtime, command: &mut Command) -> Result<(), DomainError> {
    if !matches!(command, Command::ReadyForValidation { .. } | Command::RecordValidation { .. }) {
        return Ok(());
    }
    let state = domain.state();
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
            *items = owed_items(&project, &report);
            if let Ok(base) = port.base_commit(&root, &commit)
                && let Ok(changed) = port.changed(&root, &base, &commit)
            {
                *scope = outside(&project, task, &changed);
            }
        }
        Command::RecordValidation { task_id, subject, check: kind, inputs, listed, .. } => {
            inputs.clear();
            *listed = None;
            let Some(round) = state.tasks.get(task_id).and_then(|task| task.validation.current()).filter(|round| round.state == RoundState::Validating) else { return Ok(()) };
            let Some(root) = port.own(state, task_id) else { return Ok(()) };
            let files = port.commit(&root, &round.head).map_err(unreadable)?;
            if let Ok(Some(project)) = load(&*files) {
                *inputs = project.fingerprint(subject.trim(), kind.engine()).unwrap_or_default();
                *listed = Some(owed_items(&project, &project.report(None)));
            }
        }
        _ => {}
    }
    Ok(())
}

/// What Alondra reads before a final delivery of `task_id`: the commits it carries, for the
/// domain's guard to decide on.
pub(crate) fn read_delivery(domain: &dyn Runtime, state: &DomainState, task_id: &TaskId, tips: &std::collections::BTreeMap<alondra_domain_model::PrId, spec_reading::GitHubBase>) -> Result<Vec<CommitReading>, String> {
    let Some(port) = domain.ports().spec_worktrees() else { return Ok(vec![]) };
    spec_reading::read_delivery(&*port, state, task_id, tips)
}

/// What a validator's delivery of `task_id` shows of its specs, read from the commit it delivers.
pub(crate) fn delivered(domain: &dyn Runtime, state: &DomainState, task_id: &TaskId, delivery: &InteractionId) -> Option<DeliveredValidation> {
    let port = domain.ports().spec_worktrees()?;
    delivered_with(&*port, &HomeImages::new(domain.home()), state, task_id, delivery)
}

pub(crate) fn delivered_with(port: &dyn SpecWorktrees, images: &dyn KeptImages, state: &DomainState, task_id: &TaskId, delivery: &InteractionId) -> Option<DeliveredValidation> {
    let task = state.tasks.get(task_id).filter(|task| task.stages.role() == Some(ExecutionRole::Validator) || task.validates_itself())?;
    let root = port.own(state, task_id)?;
    let head = match state.open_prs(task).next() {
        Some(pr) => pr.head.clone(),
        None => port.head_commit(&root).ok()?,
    };
    let files = port.commit(&root, &head).ok()?;
    let project = load(&*files).ok()??;
    let report = project.report(None);
    let base = port.base_commit(&root, &head).ok()?;
    let changed = port.changed(&root, &base, &head).ok()?;
    let base_files = port.commit(&root, &base).ok()?;
    let mut shown = snapshot(&project, &report, state, task_id, delivery, &head, &changed);
    keep_shown(&mut shown, &*files, &*base_files, images);
    let titles: std::collections::BTreeMap<&str, &str> = project.decisions().iter().map(|decision| (decision.id.as_str(), decision.title.as_str())).collect();
    for spec in &mut shown.specs {
        let cited = project.specs().iter().find(|loaded| loaded.path == spec.path).into_iter().flat_map(|loaded| &loaded.spec.decisions);
        spec.decisions = cited.filter_map(|id| Some((id.clone(), titles.get(id.as_str())?.to_string()))).collect();
    }
    Some(shown)
}

/// The most bytes of evidence one delivery keeps.
const MAX_KEPT_CAPTURES: usize = 128;

/// Adds what the card draws of the delivered commit: each spec's file and parts, on it and on its
/// base, and a copy of each capture a judgment rests on.
fn keep_shown(shown: &mut DeliveredValidation, head: &dyn Files, base: &dyn Files, images: &dyn KeptImages) {
    let text = |files: &dyn Files, path: &str| files.read(path).and_then(|bytes| String::from_utf8(bytes).ok());
    let parts = |files: &dyn Files, path: &str, source: Option<&str>| {
        let spec = source.and_then(|source| engine::read_spec(path, source.as_bytes()).ok());
        spec.map(|spec| specs::citations(path, &spec)).unwrap_or_default().into_iter()
            .filter_map(|part| specs::content_of(files, &part, images).map(|content| (part, content)))
            .collect()
    };
    let paths = head.paths();
    let mut kept = 0;
    for spec in &mut shown.specs {
        spec.source = text(head, &spec.path);
        spec.base = text(base, &spec.path);
        spec.parts = parts(head, &spec.path, spec.source.as_deref());
        spec.base_parts = parts(base, &spec.path, spec.base.as_deref());
        let read = spec.source.as_deref().and_then(|source| engine::read_spec(&spec.path, source.as_bytes()).ok());
        for scenario in &mut spec.scenarios {
            let pattern = read.as_ref().and_then(|read| read.scenarios.iter().find(|known| scenario.id.rsplit('/').next() == Some(known.id.as_str())))
                .and_then(|known| known.judgment.as_ref()?.pattern.clone());
            for verdict in scenario.verdicts.iter_mut().filter(|verdict| verdict.kind == ValidationKind::Judged) {
                let looked: Vec<String> = match (&verdict.evidence[..], &pattern) {
                    ([], Some(pattern)) => paths.iter().filter(|path| engine::matches(pattern, path)).take(MAX_KEPT_CAPTURES).cloned().collect(),
                    (evidence, _) => evidence.to_vec(),
                };
                for path in looked {
                    if kept == MAX_KEPT_CAPTURES {
                        break;
                    }
                    kept += 1;
                    let image = head.read(&path).and_then(|bytes| images.keep(&bytes));
                    verdict.captures.push(DeliveredCapture { path, image });
                }
            }
        }
    }
}

/// What the delivery's card shows of its specs: each spec the task's work is about, each scenario
/// with the engine's state of its validations, the tests that declare it and what the validator
/// said of its judgments; the suites; the files outside its components; the reference it changed.
fn snapshot(project: &engine::Project, report: &engine::Report, state: &DomainState, task_id: &TaskId, delivery: &InteractionId, head: &str, changed: &[String]) -> DeliveredValidation {
    let task = &state.tasks[task_id];
    let names = task_components(project, task);
    let state_of = |subject: &str, kind: engine::Kind| report.items.iter().find(|item| item.subject == subject && item.kind == kind).map(|item| item.state.as_str().to_owned());
    let mut suites = vec![];
    for component in project.components().iter().filter(|component| names.contains(&component.id)) {
        for suite in &component.suites {
            let id = format!("{}/{}", component.id, suite.id);
            let record = project.fingerprint(&id, engine::Kind::Suite).and_then(|inputs| state.validation_record(task_id, &id, ValidationKind::Suite, &inputs));
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
                let record = state.validation_record(task_id, &id, kind, &inputs)?;
                Some(DeliveredVerdict { kind, verdict: record.verdict, reasoning: record.reasoning.clone(), nearest_wrong: record.nearest_wrong.clone(), evidence: record.evidence.clone(), captures: vec![] })
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
        DeliveredSpec { id: loaded.spec.id.clone(), path: loaded.path.clone(), title: loaded.spec.title.clone(), scenarios, source: None, base: None, parts: Default::default(), base_parts: Default::default(), decisions: Default::default() }
    }).collect();
    let sections = project.sections().iter().filter(|section| changed.contains(&section.path))
        .map(|section| DeliveredSection { id: section.id.clone(), path: section.path.clone(), text: section.text.clone() })
        .collect();
    DeliveredValidation { delivery: delivery.clone(), head: head.to_owned(), specs, suites, outside: outside(project, task, changed), sections }
}

/// Keeps what Alondra read of a commit of the task, for the domain's guard of its delivery.
pub(crate) fn record_reading(domain: &dyn Runtime, task_id: &TaskId, reading: CommitReading) -> Result<(), DomainError> {
    let state = domain.state();
    let envelope = alondra_domain_model::CommandEnvelope {
        protocol_version: alondra_domain_model::PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("commit-reading-{}-{}-{}", task_id.0, reading.commit, state.revision).into(),
        expected_revision: state.revision,
        command: Command::RecordCommitReading { task_id: task_id.clone(), reading },
    };
    domain.execute_as_system(envelope).map(|_| ()).map_err(|error| delivery_unreadable(&error.to_string()))
}

/// Keeps what the validator's delivery showed of its specs for its card. A card without it says
/// its evidence is not available, so a failure here never stops the delivery.
pub(crate) fn record_delivered(domain: &dyn Runtime, task_id: &TaskId, delivery: &InteractionId, validation: DeliveredValidation) {
    let state = domain.state();
    let envelope = alondra_domain_model::CommandEnvelope {
        protocol_version: alondra_domain_model::PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("delivered-validation-{}", delivery.0).into(),
        expected_revision: state.revision,
        command: Command::RecordDeliveredValidation { task_id: task_id.clone(), validation },
    };
    if let Err(error) = domain.execute_as_system(envelope) {
        log!("{delivery}: its validation could not be kept for its card: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_domain_model::validation::RoundItem;
    use alondra_domain_model::{DeliveryCheck, DeliveryTarget, PullRequest, SpecFiles, TaskRun, TaskState, ValidationRecord, ValidationRound, ValidationState, ValidationVerdict};
    use alondra_domain_model::specs::{Files, NoImages};
    use alondra_domain_model::{DeliveredScenario, DeliveredSpec, DeliveredVerdict};
    use alondra_engine_ports::spec_worktrees::FileBytes;
    use alondra_engine_ports::spec_worktrees::SpecSource;
    use std::path::Path;
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
        gone: Mutex<bool>,
        no_base: Mutex<bool>,
        own_base: Mutex<bool>,
    }
    impl Repo {
        fn new(base: Tree) -> Repo {
            Repo { commits: Mutex::new(BTreeMap::from([(sha('0'), base)])), head: Mutex::new(sha('0')), dirty: Mutex::new(vec![]), gone: Mutex::new(false), no_base: Mutex::new(false), own_base: Mutex::new(false) }
        }
        fn commit(&self, name: char, tree: Tree) -> String {
            self.commits.lock().unwrap().insert(sha(name), tree);
            *self.head.lock().unwrap() = sha(name);
            sha(name)
        }
        fn check_out(&self, commit: &str) {
            *self.head.lock().unwrap() = commit.to_owned();
        }
        fn tree(&self, commit: &str) -> Tree {
            self.commits.lock().unwrap()[commit].clone()
        }
    }
    fn sha(name: char) -> String {
        name.to_string().repeat(40)
    }
    impl SpecWorktrees for Repo {
        fn own(&self, _: &DomainState, _: &TaskId) -> Option<PathBuf> { (!*self.gone.lock().unwrap()).then(|| PathBuf::from("/worktree")) }
        fn all(&self, _: &DomainState, _: &TaskId) -> Vec<PathBuf> { vec![PathBuf::from("/worktree")] }
        fn head(&self, _: &Path) -> Result<SpecSource, String> { Ok(Box::new(self.tree(&self.head.lock().unwrap().clone()))) }
        fn base(&self, _: &Path) -> Result<SpecSource, String> { Ok(Box::new(self.tree(&sha('0')))) }
        fn head_commit(&self, _: &Path) -> Result<String, String> { Ok(self.head.lock().unwrap().clone()) }
        fn commit(&self, _: &Path, commit: &str) -> Result<SpecSource, String> {
            self.commits.lock().unwrap().get(commit).cloned().map(|tree| Box::new(tree) as SpecSource).ok_or_else(|| format!("no commit {commit}"))
        }
        fn base_commit(&self, _: &Path, commit: &str) -> Result<String, String> {
            if *self.no_base.lock().unwrap() { return Err("the repository has no default branch to compare with".into()) }
            Ok(if *self.own_base.lock().unwrap() { commit.to_owned() } else { sha('0') })
        }
        fn merge_base(&self, _: &Path, _: &str, tip: &str) -> Result<String, String> {
            if tip == sha('0') { Ok(sha('0')) } else { Err(format!("no common history with {tip}")) }
        }
        fn changed(&self, _: &Path, from: &str, to: &str) -> Result<Vec<String>, String> {
            let (from, to) = (self.tree(from).0, self.tree(to).0);
            let paths: BTreeSet<&String> = from.keys().chain(to.keys()).collect();
            Ok(paths.into_iter().filter(|path| from.get(*path) != to.get(*path)).cloned().collect())
        }
        fn uncommitted(&self, _: &Path) -> Result<Vec<String>, String> { Ok(self.dirty.lock().unwrap().clone()) }
        fn settings(&self, _: &Path) -> Result<Option<Vec<u8>>, String> { Ok(None) }
        fn file(&self, _: &Path, path: &str, _: usize) -> Result<Vec<u8>, String> { Err(format!("{path} is not read here")) }
        fn write(&self, _: &Path, _: &FileBytes) -> Result<BTreeMap<String, String>, String> { Ok(BTreeMap::new()) }
    }

    #[test]
    fn a_delivery_keeps_what_its_card_draws_and_a_copy_of_each_capture_a_judgment_rests_on() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(home.join("files")).unwrap();
        let images = HomeImages::new(&home);
        let png = [&b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"[..], &390u32.to_be_bytes(), &844u32.to_be_bytes(), &[8, 6, 0, 0, 0]].concat();
        let feature = "@spec:billing.partial\n@diagram:flow.mmd\nFeature: Partial refunds\n\n  @id:shown @judged:capture @evidence:captures/*.png\n  Scenario: The sheet shows what is left\n    Then it shows 20 EUR\n";
        let head = base().with("specs/billing/shown.feature", feature).with("specs/billing/flow.mmd", "flowchart LR\n  A --> B\n")
            .with_bytes("captures/sheet.png", png).with("captures/notes.png", "not an image");
        let verdict = |evidence: Vec<String>| DeliveredVerdict { kind: ValidationKind::Judged, verdict: ValidationVerdict::Met, reasoning: None, nearest_wrong: None, evidence, captures: vec![] };
        let scenario = |evidence: Vec<String>| DeliveredScenario { id: "billing.partial/shown".into(), title: "The sheet shows what is left".into(), steps: vec![], states: vec![], tests: vec![], judged: Some("capture".into()), verdicts: vec![verdict(evidence)] };
        let shown = |evidence: Vec<String>| {
            let mut shown = DeliveredValidation { specs: vec![DeliveredSpec { id: "billing.partial".into(), path: "specs/billing/shown.feature".into(), title: "Partial refunds".into(),
                scenarios: vec![scenario(evidence)], source: None, base: None, parts: Default::default(), base_parts: Default::default(), decisions: Default::default() }], ..DeliveredValidation::default() };
            keep_shown(&mut shown, &head, &base(), &images);
            shown.specs.remove(0)
        };
        let spec = shown(vec!["captures/sheet.png".into(), "captures/notes.png".into(), "captures/gone.png".into()]);
        assert_eq!((spec.source.as_deref(), spec.base), (Some(feature), None), "a new spec has no base");
        assert!(spec.parts["specs/billing/flow.mmd"].text().is_some_and(|text| text.starts_with("flowchart")));
        let captures = &spec.scenarios[0].verdicts[0].captures;
        assert_eq!(captures.iter().map(|capture| (capture.path.as_str(), capture.image.as_ref().map(|image| (image.width, image.height)))).collect::<Vec<_>>(),
            [("captures/sheet.png", Some((390, 844))), ("captures/notes.png", None), ("captures/gone.png", None)]);
        let kept = captures[0].image.clone().unwrap();
        assert!(home.join(&kept.file).is_file() && images.read(&kept).is_some(), "the copy is downloadable from Alondra's files");
        let unnamed = shown(vec![]);
        assert_eq!(unnamed.scenarios[0].verdicts[0].captures.len(), 2, "with no path named, the captures are the ones @evidence matches");
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
        task.validation.rounds.push(ValidationRound { number: 1, head: head.into(), implemented: head.into(), items, scope: vec![], state: RoundState::Validating, generation: Some(3), findings: None, retried: false });
        state_with(task)
    }

    /// The task's pull request in `repository`, open at `head`.
    fn with_pr(mut state: DomainState, repository: &str, number: u64, head: &str) -> DomainState {
        let id = alondra_domain_model::PrId(format!("github.com/{repository}#{number}"));
        let pr = PullRequest {
            id: id.clone(), task_id: "t".into(), host: "github.com".into(), repository: repository.into(), number, head: head.into(), base: "main".into(),
            state: alondra_domain_model::PrState::Open, merged_head: None, allowed_methods: vec![], review: None, aliases: Default::default(), address_read_at: 0,
            identity: Default::default(), tracking: Default::default(), missing: None, relocation_asked_at: None,
        };
        let task = state.tasks.get_mut(&TaskId::from("t")).unwrap();
        if task.pr_id.is_none() { task.pr_id = Some(id.clone()) } else { task.later_prs.push(id.clone()) }
        state.pull_requests.insert(id, pr);
        state
    }

    fn github_base(branch: &str, tip: String) -> spec_reading::GitHubBase {
        spec_reading::GitHubBase { branch: branch.into(), tip, default_branch: "main".into() }
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

    /// What the domain's guard says of a final delivery of the task after Alondra read it as the
    /// daemon does: the commit of each open pull request and the worktree's.
    fn check(repo: &Repo, mut state: DomainState) -> Result<DeliveryCheck, String> {
        let task_id = TaskId::from("t");
        let tips = state.open_prs(&state.tasks[&task_id]).map(|pr| (pr.id.clone(), github_base(&pr.base, sha('0')))).collect();
        let readings = spec_reading::read_delivery(repo, &state, &task_id, &tips)?;
        let task = state.tasks.get_mut(&task_id).unwrap();
        for reading in readings {
            task.validation.push_reading(reading);
        }
        let targets: Vec<DeliveryTarget> = state.open_prs(&state.tasks[&task_id]).map(|pr| DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: pr.head.clone() }).collect();
        Ok(state.delivery_check(&task_id, &targets))
    }

    fn named(items: &[alondra_domain_model::ValidationItem]) -> Vec<String> {
        items.iter().map(|item| format!("{} {}", item.subject, item.kind.as_str())).collect()
    }

    #[test]
    fn a_validators_delivery_passes_when_every_ledger_entry_is_one_it_recorded_and_only_the_ledger_changed() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, true);
        repo.commit('2', ledger_for(&repo, &head));
        assert_eq!(check(&repo, state.clone()), Ok(DeliveryCheck::Validated));
        let shown = delivered_with(&repo, &NoImages, &state, &"t".into(), &delivery()).expect("its card shows the specs");
        let scenarios: Vec<&str> = shown.specs.iter().flat_map(|spec| &spec.scenarios).map(|scenario| scenario.id.as_str()).collect();
        assert_eq!(scenarios, ["billing.partial/part", "billing.partial/alone"]);
        let part = &shown.specs[0].scenarios[0];
        assert!(part.states.iter().all(|(_, state)| state == "valid"));
        assert_eq!((part.tests[0].path.as_str(), part.tests[0].line, part.tests[0].suites.clone()), ("crates/billing/tests/refunds.rs", 1, vec!["billing/unit".to_owned()]));
        assert_eq!(part.verdicts[0].verdict, ValidationVerdict::Met);
        assert!(shown.outside.is_empty());
    }

    #[test]
    fn the_card_of_a_continuations_delivery_shows_what_the_validator_of_its_original_recorded() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let mut state = validated(&repo, &head, true);
        let mut original = state.tasks.remove(&TaskId::from("t")).unwrap();
        original.id = "t-original".into();
        original.state = TaskState::Cancelled;
        let mut continuation = task(ExecutionRole::Validator);
        continuation.continues = Some(original.id.clone());
        state.tasks.insert(original.id.clone(), original);
        state.tasks.insert("t".into(), continuation);
        repo.commit('2', ledger_for(&repo, &head));
        let shown = delivered_with(&repo, &NoImages, &state, &"t".into(), &delivery()).expect("its card shows the specs");
        assert_eq!(shown.suites.iter().map(|suite| (suite.id.as_str(), suite.verdict)).collect::<Vec<_>>(), [("billing/unit", Some(ValidationVerdict::Met))]);
        let verdicts: Vec<(&str, Option<&str>)> = shown.specs.iter().flat_map(|spec| &spec.scenarios)
            .flat_map(|scenario| scenario.verdicts.iter().map(move |verdict| (scenario.id.as_str(), verdict.reasoning.as_deref()))).collect();
        assert_eq!(verdicts, [("billing.partial/part", Some("Checked")), ("billing.partial/alone", Some("Checked"))]);
    }

    #[test]
    fn a_ledger_written_by_hand_is_refused_because_no_validator_recorded_it() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, false);
        repo.commit('2', ledger_for(&repo, &head));
        let Ok(DeliveryCheck::Unrecorded { items, .. }) = check(&repo, state) else { panic!("the ledger is not the validator's") };
        assert_eq!(named(&items), ["billing.partial/alone adequate", "billing.partial/part adequate", "billing/unit suite"]);
    }

    #[test]
    fn a_test_weakened_after_its_judgment_leaves_the_delivery_not_validated() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, true);
        let weakened = ledger_for(&repo, &head).with("crates/billing/tests/refunds.rs", &TESTS.replace("assert_eq!(refund(30, 10), 10);", ""));
        repo.commit('2', weakened);
        let Ok(DeliveryCheck::Listed { items, .. }) = check(&repo, state) else { panic!("the weakened test owes its judgment again") };
        assert!(named(&items).contains(&"billing.partial/part adequate".to_owned()), "{items:?}");
    }

    #[test]
    fn anything_but_the_ledger_changed_after_the_validated_commit_is_refused() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let state = validated(&repo, &head, true);
        repo.commit('2', ledger_for(&repo, &head).with("README.md", "Refunds\n"));
        assert_eq!(check(&repo, state), Ok(DeliveryCheck::ChangedAfterValidation(vec!["README.md".into()])));
    }

    #[test]
    fn the_implementer_cannot_deliver_while_something_is_owed_and_delivers_as_today_when_nothing_is() {
        let repo = Repo::new(base());
        implemented(&repo);
        let state = state_with(task(ExecutionRole::Implementer));
        let owed = check(&repo, state.clone()).unwrap();
        assert_eq!(owed.refusal(Some(ExecutionRole::Implementer)).unwrap().code, "validation_required");
        let unchanged = Repo::new(ledger_for(&Repo::new(base()), &sha('0')));
        assert_eq!(check(&unchanged, state), Ok(DeliveryCheck::Validated));
    }

    #[test]
    fn a_project_without_specs_is_never_gated() {
        let repo = Repo::new(Tree(BTreeMap::new()).with("src/lib.rs", "fn main() {}\n"));
        assert_eq!(check(&repo, state_with(task(ExecutionRole::Implementer))), Ok(DeliveryCheck::NotGated));
    }

    #[test]
    fn an_implementer_who_writes_the_ledger_by_hand_over_a_weakened_test_is_refused() {
        let repo = Repo::new(base());
        let weakened = repo.tree(&sha('0')).with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, part: u32) -> u32 { paid }\n")
            .with("crates/billing/tests/refunds.rs", &TESTS.replace("assert_eq!(refund(30, 10), 10);", ""));
        let head = repo.commit('1', weakened);
        repo.commit('2', ledger_for(&repo, &head));
        let refused = check(&repo, state_with(task(ExecutionRole::Implementer))).unwrap();
        assert_eq!(refused.refusal(Some(ExecutionRole::Implementer)).unwrap().code, "ledger_unrecorded", "{refused:?}");
    }

    #[test]
    fn an_implementer_cannot_deliver_while_a_change_of_the_specs_is_open() {
        let unchanged = Repo::new(ledger_for(&Repo::new(base()), &sha('0')));
        let mut task = task(ExecutionRole::Implementer);
        task.specs.amendments.push(alondra_domain_model::SpecAmendment { id: "a1".into(), reason: "Refund in two steps".into(), state: alondra_domain_model::AmendmentState::Requested,
            files: SpecFiles::new(), before: BTreeMap::new(), check: None, card: None, words: None });
        assert_eq!(check(&unchanged, state_with(task)), Ok(DeliveryCheck::SpecChangeOpen));
    }

    #[test]
    fn the_guard_reads_the_commit_of_the_delivered_pull_request_not_the_one_checked_out() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        let validated_commit = repo.commit('2', ledger_for(&repo, &head));
        let code = repo.tree(&validated_commit).with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, _: u32) -> u32 { paid }\n");
        let pushed = repo.commit('3', code);
        repo.check_out(&validated_commit);
        let state = with_pr(validated(&repo, &head, true), "ana/app", 1, &pushed);
        assert!(!check(&repo, state).unwrap().admits(), "the pull request carries code no validator checked");
        let unknown = with_pr(validated(&repo, &head, true), "ana/site", 2, &sha('9'));
        assert!(check(&repo, unknown).is_err(), "a pull request whose commit Alondra cannot read is refused, whatever a remote says");

        repo.check_out(&sha('0'));
        let implementer = with_pr(state_with(task(ExecutionRole::Implementer)), "ana/app", 1, &head);
        let owed = check(&repo, implementer).unwrap();
        assert_eq!(owed.refusal(Some(ExecutionRole::Implementer)).unwrap().code, "validation_required", "checking out main does not hide the pull request");
    }

    #[test]
    fn a_branch_that_turns_specs_off_is_refused_on_its_pull_requests_commit() {
        let repo = Repo::new(base());
        let off = repo.tree(&sha('0')).with("alondra.toml", "format = 1\n\n[specs]\nenabled = false\n")
            .with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, _: u32) -> u32 { paid }\n");
        let head = repo.commit('1', off);
        repo.check_out(&sha('0'));
        let state = with_pr(state_with(task(ExecutionRole::Implementer)), "ana/app", 1, &head);
        let refused = check(&repo, state).unwrap();
        assert_eq!(refused, DeliveryCheck::SpecsDiffer { unapproved: vec!["alondra.toml".into()], other: vec![] });
        assert_eq!(refused.refusal(Some(ExecutionRole::Implementer)).unwrap().code, "specs_differ");
    }

    #[test]
    fn a_pull_requests_commit_is_compared_with_the_base_github_reports_never_with_the_repositorys_references() {
        let repo = Repo::new(base());
        let off = repo.tree(&sha('0')).with("alondra.toml", "format = 1\n\n[specs]\nenabled = false\n")
            .with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, _: u32) -> u32 { paid }\n");
        let head = repo.commit('1', off);
        *repo.own_base.lock().unwrap() = true;
        let state = with_pr(state_with(task(ExecutionRole::Implementer)), "ana/app", 1, &head);
        assert_eq!(check(&repo, state).unwrap(), DeliveryCheck::SpecsDiffer { unapproved: vec!["alondra.toml".into()], other: vec![] },
            "the local default branch pointed at the commit itself changes nothing");
    }

    #[test]
    fn a_commit_that_turns_specs_off_is_never_taken_without_comparing_it_with_its_base() {
        let repo = Repo::new(base());
        let off = repo.tree(&sha('0')).with("alondra.toml", "format = 1\n\n[specs]\nenabled = false\n")
            .with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, _: u32) -> u32 { paid }\n");
        let head = repo.commit('1', off);
        let state = with_pr(state_with(task(ExecutionRole::Implementer)), "ana/app", 1, &head);
        let tips = state.open_prs(&state.tasks[&TaskId::from("t")]).map(|pr| (pr.id.clone(), github_base(&pr.base, sha('8')))).collect();
        let refused = spec_reading::read_delivery(&repo, &state, &"t".into(), &tips).unwrap_err();
        assert!(refused.contains("ana/app#1") && refused.contains("own repository only"), "{refused}");
        assert!(spec_reading::read_delivery(&repo, &state, &"t".into(), &Default::default()).unwrap_err().contains("did not report the base branch"));
        *repo.no_base.lock().unwrap() = true;
        assert!(spec_reading::read_delivery(&repo, &state_with(task(ExecutionRole::Implementer)), &"t".into(), &Default::default()).is_err(), "a worktree commit whose base cannot be read is not read");
    }

    #[test]
    fn a_validator_never_delivers_code_of_its_own_even_after_its_round_moved_to_that_commit() {
        let repo = Repo::new(base());
        let handed_over = implemented(&repo);
        let own_code = repo.commit('3', repo.tree(&handed_over).with("crates/billing/src/lib.rs", "pub fn refund(paid: u32, _: u32) -> u32 { paid }\n"));
        let mut state = validated(&repo, &own_code, true);
        state.tasks.get_mut(&TaskId::from("t")).unwrap().validation.rounds[0].implemented = handed_over;
        repo.commit('4', ledger_for(&repo, &own_code));
        assert_eq!(check(&repo, state), Ok(DeliveryCheck::ChangedAfterValidation(vec!["crates/billing/src/lib.rs".into()])));
    }

    #[test]
    fn a_pull_request_whose_worktree_is_gone_cannot_be_read() {
        let repo = Repo::new(base());
        let head = implemented(&repo);
        *repo.gone.lock().unwrap() = true;
        let state = with_pr(state_with(task(ExecutionRole::Implementer)), "ana/app", 1, &head);
        assert!(check(&repo, state).is_err());
        assert_eq!(check(&repo, state_with(task(ExecutionRole::Implementer))), Ok(DeliveryCheck::NotGated), "no worktree and no pull request deliver no commit");
    }
}
