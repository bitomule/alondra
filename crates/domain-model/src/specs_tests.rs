//! What Alondra reads of a proposal over a worktree, what it finds changed without approval, and
//! the edits the user makes on a plan's card.
use crate::specs::*;
use std::collections::BTreeMap;

pub(crate) const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n\n[decisions]\nenabled = true\n\n[reference]\nenabled = true\n";
pub(crate) const BOUNDARY: &str = "id = \"billing\"\nowns = [\"crates/billing/**\"]\n";
pub(crate) const REFUNDS: &str = "@spec:billing.refunds\nFeature: Refunds return the money\n  A refund gives back what an order cost.\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order of 30 EUR\n    When the order is refunded\n    Then the customer receives 30 EUR\n\n  @id:stands-alone @boundary:dependencies\n  Scenario: Billing uses no other component\n    Given the billing crate\n    When its dependencies are listed\n    Then none is another component\n";
pub(crate) const REFUNDS_PATH: &str = "specs/billing/refunds.feature";

/// The files of a repository, in memory.
#[derive(Clone, Default)]
pub(crate) struct Mem(pub(crate) BTreeMap<String, String>);

impl Mem {
    pub(crate) fn with(mut self, path: &str, text: &str) -> Self {
        self.0.insert(path.into(), text.into());
        self
    }

    pub(crate) fn project() -> Self {
        Mem::default().with(SETTINGS_PATH, SETTINGS).with("specs/billing/boundary.toml", BOUNDARY).with(REFUNDS_PATH, REFUNDS)
    }
}

impl Files for Mem {
    fn paths(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).map(|text| text.clone().into_bytes())
    }
}

pub(crate) fn changes(entries: &[(&str, Option<&str>)]) -> SpecFiles {
    entries.iter().map(|(path, text)| ((*path).to_owned(), text.map(str::to_owned))).collect()
}

const PARTIAL: &str = "@spec:billing.partial\nFeature: Partial refunds\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n";

pub(crate) fn decision(id: &str, status: &str) -> String {
    decision_text(&DecisionFields { id: id.into(), title: format!("Decide {id}"), context: "Why".into(), decision: "What".into(), consequences: "Then".into(), confirmation: "A test".into(), supersedes: vec![] }, status, "2026-11-26")
}

#[test]
fn a_proposal_the_engine_reads_is_clean_and_keeps_what_each_file_held() {
    let base = Mem::project();
    let check = check(&base, &changes(&[("specs/billing/partial.feature", Some(PARTIAL)), (REFUNDS_PATH, Some(REFUNDS))]));
    assert_eq!(check.mode, SpecMode::Committed);
    assert!(check.clean(), "{}", check.summary());
    assert_eq!(check.before["specs/billing/partial.feature"], None);
    assert_eq!(check.before[REFUNDS_PATH].as_deref(), Some(REFUNDS));
    let gone = super::specs::check(&base, &changes(&[(REFUNDS_PATH, None)]));
    assert!(gone.problems.iter().any(|p| p.code == "boundary_without_scenario"), "removing the spec that states the boundary's promise is read as a problem: {}", gone.summary());
    assert_eq!(check.dirs.decisions.as_deref(), Some("docs/decisions"));
}

#[test]
fn a_proposal_the_engine_cannot_read_names_each_problem_by_file_and_line() {
    let broken = PARTIAL.replace("  @id:part\n", "");
    let check = check(&Mem::project(), &changes(&[("specs/billing/partial.feature", Some(&broken))]));
    assert!(!check.clean());
    let problem = &check.problems[0];
    assert_eq!((problem.path.as_str(), problem.code.as_str()), ("specs/billing/partial.feature", "missing_scenario_id"));
    assert!(problem.line > 0, "a problem names its line");
}

#[test]
fn a_spec_may_cite_a_decision_its_own_proposal_proposes() {
    let cited = PARTIAL.replace("@spec:billing.partial\n", "@spec:billing.partial\n@decision:keep-refunds\n");
    let proposal = changes(&[("specs/billing/partial.feature", Some(&cited)), ("docs/decisions/keep-refunds.md", Some(&decision("keep-refunds", "proposed")))]);
    let check = check(&Mem::project(), &proposal);
    assert!(check.clean(), "the proposed decision reads as accepted, as it is once approved: {}", check.summary());
    let accepted = accepted(&proposal, &check.dirs);
    assert_eq!(frontmatter(accepted["docs/decisions/keep-refunds.md"].as_deref().unwrap(), "status").as_deref(), Some("accepted"));
}

#[test]
fn a_proposal_only_changes_spec_files_never_code_nor_the_ledger() {
    for path in ["crates/billing/src/lib.rs", "specs/ledger/billing/suites/unit.toml", "../outside.feature"] {
        let check = check(&Mem::project(), &changes(&[(path, Some("x"))]));
        assert!(check.problems.iter().any(|p| p.path == path && matches!(p.code.as_str(), "path_outside_specs" | "path_outside_repository")), "{path}: {}", check.summary());
    }
}

#[test]
fn settings_decide_the_mode_and_unreadable_settings_never_read_as_off() {
    let off = Mem::default().with(SETTINGS_PATH, "format = 1\n");
    assert_eq!(check(&off, &SpecFiles::new()).mode, SpecMode::Off);
    assert_eq!(check(&Mem::default(), &SpecFiles::new()).mode, SpecMode::Off, "no settings file: specs are off");
    let broken = Mem::project().with(SETTINGS_PATH, "format = 1\n[specs\n");
    let unreadable = check(&broken, &SpecFiles::new());
    assert_eq!(unreadable.mode, SpecMode::Unreadable);
    assert_eq!(unreadable.problems[0].path, SETTINGS_PATH);
    assert!(settings_problem(Some(b"format = 1\n[specs\n")).is_some());
    assert!(settings_problem(None).is_none());
    let unknown = Mem::project().with(SETTINGS_PATH, "format = 1\nsurprise = true\n");
    assert_eq!(check(&unknown, &SpecFiles::new()).mode, SpecMode::Unreadable, "an unknown key is a problem, not a way to turn specs off");
    let elsewhere = Mem::default().with(SETTINGS_PATH, "format = 1\n\n[specs]\nenabled = true\ncommitted = false\n");
    assert_eq!(check(&elsewhere, &SpecFiles::new()).mode, SpecMode::Uncommitted);
}

#[test]
fn drift_finds_spec_changes_nobody_approved_and_approved_ones_not_held_as_approved() {
    let base = Mem::project();
    let changed = REFUNDS.replace("30 EUR\n    When the order", "40 EUR\n    When the order");
    let head = base.clone().with(REFUNDS_PATH, &changed);
    assert_eq!(drift(&head, &base, &SpecFiles::new()).unwrap(), [SpecDrift { path: REFUNDS_PATH.into(), approved: false }]);
    assert_eq!(drift(&head, &base, &changes(&[(REFUNDS_PATH, Some(&changed))])).unwrap(), [], "the approved change, held as approved");
    let other = changed.replace("40 EUR", "50 EUR");
    assert_eq!(drift(&head, &base, &changes(&[(REFUNDS_PATH, Some(&other))])).unwrap(), [SpecDrift { path: REFUNDS_PATH.into(), approved: true }]);
    let removed = Mem::default().with(SETTINGS_PATH, SETTINGS).with("specs/billing/boundary.toml", BOUNDARY);
    assert_eq!(drift(&removed, &base, &SpecFiles::new()).unwrap(), [SpecDrift { path: REFUNDS_PATH.into(), approved: false }], "a removed spec is a change too");
    assert_eq!(drift(&removed, &base, &changes(&[(REFUNDS_PATH, None)])).unwrap(), []);
}

#[test]
fn drift_leaves_out_code_reference_prose_and_the_ledger() {
    let base = Mem::project();
    let head = base.clone().with("crates/billing/src/lib.rs", "fn main() {}")
        .with("docs/reference/billing.md", "## Refunds\n<!-- section: billing.refunds explains: billing.refunds -->\nWords.\n")
        .with("specs/ledger/billing/suites/unit.toml", "inputs = \"sha256:00\"\n");
    assert_eq!(drift(&head, &base, &SpecFiles::new()).unwrap(), []);
    let decided = base.clone().with("docs/decisions/keep.md", &decision("keep", "accepted"));
    assert_eq!(drift(&decided, &base, &SpecFiles::new()).unwrap(), [SpecDrift { path: "docs/decisions/keep.md".into(), approved: false }], "a decision is guarded like a spec");
    let settings = base.clone().with(SETTINGS_PATH, &SETTINGS.replace("[reference]\nenabled = true\n", ""));
    assert_eq!(drift(&settings, &base, &SpecFiles::new()).unwrap(), [SpecDrift { path: SETTINGS_PATH.into(), approved: false }], "changing the settings is changing the specs");
}

#[test]
fn drift_refuses_a_head_whose_settings_cannot_be_read_and_ignores_a_project_without_specs() {
    let base = Mem::project();
    let broken = base.clone().with(SETTINGS_PATH, "[specs\n");
    assert!(drift(&broken, &base, &SpecFiles::new()).is_err());
    let none = Mem::default().with("README.md", "x");
    assert_eq!(drift(&none.clone().with("specs/x.feature", "y"), &none, &SpecFiles::new()).unwrap(), [], "a project without specs changes no spec");
}

fn draft(files: SpecFiles) -> SpecDraft {
    let check = check(&Mem::project(), &files);
    SpecDraft { level: SpecLevel::Light, files, check, revision: 3, edited: false }
}

#[test]
fn a_scenario_added_on_the_card_is_written_in_its_spec_and_read_back() {
    let draft = draft(changes(&[("specs/billing/partial.feature", Some(PARTIAL))]));
    let edit = SpecEdit::PutScenario { path: "specs/billing/partial.feature".into(), scenario: None, id: "twice".into(), title: "Refunded twice".into(),
        steps: "Given a refunded order\nWhen it is refunded again\nThen nothing is paid".into(), judged: true };
    let (_, files) = edited(&draft, &edit).unwrap();
    let spec = engine::read_spec("x", files["specs/billing/partial.feature"].as_ref().unwrap().as_bytes()).unwrap();
    let added = spec.scenarios.iter().find(|s| s.id == "twice").unwrap();
    assert_eq!(added.steps.len(), 3);
    assert!(added.judgment.is_some(), "the scenario is judged");
}

#[test]
fn editing_a_scenario_keeps_its_tags_and_a_step_it_cannot_read_names_the_line() {
    let base = draft(changes(&[(REFUNDS_PATH, Some(REFUNDS))]));
    let edit = |steps: &str| SpecEdit::PutScenario { path: REFUNDS_PATH.into(), scenario: Some("stands-alone".into()), id: "stands-alone".into(), title: "Billing stands alone".into(), steps: steps.into(), judged: false };
    let (_, files) = edited(&base, &edit("Given the billing crate\nThen none is another component")).unwrap();
    let spec = engine::read_spec("x", files[REFUNDS_PATH].as_ref().unwrap().as_bytes()).unwrap();
    let kept = spec.scenarios.iter().find(|s| s.id == "stands-alone").unwrap();
    assert_eq!(kept.title, "Billing stands alone");
    assert!(kept.promise.is_some(), "its boundary tag stays");
    let refused = edited(&base, &edit("Given the billing crate\nThis is not a step")).unwrap_err();
    assert_eq!(refused.field, "steps");
    assert!(refused.message.starts_with("Line 2"), "{}", refused.message);
    let duplicate = SpecEdit::PutScenario { path: REFUNDS_PATH.into(), scenario: None, id: "full".into(), title: "Again".into(), steps: "Given x\nThen y".into(), judged: false };
    assert_eq!(edited(&base, &duplicate).unwrap_err().field, "id");
}

#[test]
fn removing_from_the_card_drops_a_new_file_and_marks_an_existing_one_removed() {
    let base = draft(changes(&[(REFUNDS_PATH, Some(REFUNDS)), ("specs/billing/partial.feature", Some(PARTIAL))]));
    let (_, files) = edited(&base, &SpecEdit::RemoveSpec { path: "specs/billing/partial.feature".into() }).unwrap();
    assert!(!files.contains_key("specs/billing/partial.feature"), "a spec that never existed leaves the proposal");
    let (_, files) = edited(&base, &SpecEdit::RemoveSpec { path: REFUNDS_PATH.into() }).unwrap();
    assert!(!files.contains_key(REFUNDS_PATH), "taking a spec of the repository out of the plan leaves it as it is, never deletes it");
    let (_, files) = edited(&base, &SpecEdit::RemoveScenario { path: REFUNDS_PATH.into(), scenario: "full".into() }).unwrap();
    assert!(!files[REFUNDS_PATH].as_ref().unwrap().contains("@id:full"));
    let (level, _) = edited(&base, &SpecEdit::Level(SpecLevel::Full)).unwrap();
    assert_eq!(level, SpecLevel::Full);
}

#[test]
fn a_new_spec_goes_to_its_components_folder_and_a_decision_to_the_decisions_folder() {
    let base = draft(SpecFiles::new());
    let (_, files) = edited(&base, &SpecEdit::PutSpec { path: None, component: "billing".into(), id: "billing.credits".into(), title: "Credits".into(), explanation: "Money kept for later.\nNever expires.".into() }).unwrap();
    let text = files["specs/billing/credits.feature"].as_ref().unwrap();
    assert_eq!(engine::read_spec("x", text.as_bytes()).unwrap().description, ["Money kept for later.", "Never expires."]);
    let fields = DecisionFields { id: "keep".into(), title: "Keep".into(), context: "c".into(), decision: "d".into(), consequences: "e".into(), confirmation: "f".into(), supersedes: vec![] };
    let (_, files) = edited(&base, &SpecEdit::PutDecision { path: None, fields: fields.clone(), date: "2026-11-26".into() }).unwrap();
    let written = files["docs/decisions/keep.md"].as_ref().unwrap();
    assert_eq!(decision_fields(written), fields);
    assert_eq!(frontmatter(written, "status").as_deref(), Some("proposed"));
    let empty = DecisionFields { context: " ".into(), ..fields };
    assert_eq!(edited(&base, &SpecEdit::PutDecision { path: None, fields: empty, date: "2026-11-26".into() }).unwrap_err().field, "context");
}

#[test]
fn a_decision_that_supersedes_another_marks_it_superseded_by_it() {
    let base = Mem::project().with("docs/decisions/old.md", &decision("old", "accepted"));
    let fields = DecisionFields { id: "new".into(), title: "New".into(), context: "c".into(), decision: "d".into(), consequences: "e".into(), confirmation: "f".into(), supersedes: vec!["old".into()] };
    let dirs = check(&base, &SpecFiles::new()).dirs;
    let files = decision_files(&fields, "2026-11-26", &dirs, &base).unwrap();
    let old = files["docs/decisions/old.md"].as_deref().unwrap();
    assert_eq!(frontmatter(old, "status").as_deref(), Some("superseded"));
    assert_eq!(frontmatter(old, "superseded_by").as_deref(), Some("new"));
    let read = check(&base, &accepted(&files, &dirs));
    assert!(read.clean(), "both sides of the supersession agree: {}", read.summary());
    let missing = DecisionFields { supersedes: vec!["nowhere".into()], ..fields };
    assert_eq!(decision_files(&missing, "2026-11-26", &dirs, &base).unwrap_err().0, "decision.supersedes");
}

#[test]
fn a_proposal_is_bounded_in_files_size_and_paths() {
    let file = |path: &str, text: &str| SpecFile { path: path.into(), text: Some(text.into()) };
    assert!(proposal(SpecLevel::Light, &[file(REFUNDS_PATH, REFUNDS)]).is_ok());
    assert_eq!(proposal(SpecLevel::Light, &[]).unwrap_err().0, "files", "a light proposal changes something");
    assert!(proposal(SpecLevel::None, &[]).is_ok());
    assert_eq!(proposal(SpecLevel::Light, &[file("/etc/passwd", "x")]).unwrap_err().0, "files[0].path");
    assert_eq!(proposal(SpecLevel::Light, &[file("a/../b", "x")]).unwrap_err().0, "files[0].path");
    assert_eq!(proposal(SpecLevel::Light, &[file("a.feature", &"x".repeat(MAX_SPEC_FILE_BYTES + 1))]).unwrap_err().0, "files[0].text");
    assert_eq!(proposal(SpecLevel::Light, &[file("a", "x"), file("a", "y")]).unwrap_err().0, "files[1].path");
    let many: Vec<SpecFile> = (0..=MAX_SPEC_FILES).map(|n| file(&format!("specs/x/{n}.feature"), "x")).collect();
    assert_eq!(proposal(SpecLevel::Light, &many).unwrap_err().0, "files");
}

#[test]
fn a_header_value_is_set_in_place_or_added_and_dates_are_calendar_days() {
    let text = decision("x", "proposed");
    let set = set_frontmatter(&text, "status", "accepted");
    assert_eq!(frontmatter(&set, "status").as_deref(), Some("accepted"));
    assert_eq!(set.matches("status").count(), 1);
    let without = text.replace("superseded_by = \"\"\n", "");
    assert_eq!(frontmatter(&set_frontmatter(&without, "superseded_by", "y"), "superseded_by").as_deref(), Some("y"));
    assert_eq!(date_of(0), "1970-01-01");
    assert_eq!(date_of(1_795_651_200_000), "2026-11-26");
}

#[test]
fn handles_name_files_and_scenarios_of_a_proposal() {
    let draft = draft(changes(&[(REFUNDS_PATH, Some(REFUNDS))]));
    assert_eq!(file_of(&draft, &handle(REFUNDS_PATH)), Some(REFUNDS_PATH));
    assert_eq!(scenario_of(&draft, &scenario_handle(REFUNDS_PATH, "full")), Some((REFUNDS_PATH.into(), "full".into())));
    assert_eq!(file_of(&draft, "nothing"), None);
}
