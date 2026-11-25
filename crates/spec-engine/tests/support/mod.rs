#![allow(dead_code)]

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use alondra_spec_engine::{Code, Entry, Evidence, EvidenceKind, Files, Kind, Problem, Project, Report, Spec, State, Verdict, load};

pub const BOUNDARY_PATH: &str = "specs/billing/boundary.toml";
pub const REFUNDS_PATH: &str = "specs/billing/refunds.feature";

pub const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n\n[decisions]\nenabled = true\n\n[reference]\nenabled = true\n";

pub const BOUNDARY: &str = r#"id = "billing"
owns = ["crates/billing/**"]
regenerates = ["crates/billing/src/**"]

[[suite]]
id = "unit"
run = ["bazelisk", "test", "//crates/billing:test"]
tests = ["crates/billing/tests/**/*.rs"]
"#;

pub const REFUNDS: &str = r#"@spec:billing.refunds
Feature: Refunds return the money

  @id:full
  Scenario: Full refund
    Given a paid order of 30 EUR
    When the order is refunded
    Then the customer receives 30 EUR

  @id:partial
  Scenario: Partial refund
    Given a paid order of 30 EUR
    When 10 EUR are refunded
    Then the customer receives 10 EUR

  @id:stands-alone @boundary:dependencies
  Scenario: Billing uses no other component
    Given the billing crate
    When its dependencies are listed
    Then none is another component
"#;

/// The committed files of a repository held in memory. It counts every read, lists its paths in
/// either order, and can hold files that exist without being listed.
#[derive(Default, Clone)]
pub struct Tree {
    files: BTreeMap<String, Vec<u8>>,
    unlisted: BTreeMap<String, Vec<u8>>,
    unreadable: BTreeSet<String>,
    reversed: bool,
    seed: Option<u64>,
    reads: RefCell<BTreeMap<String, usize>>,
}

fn scramble(seed: u64, path: &str) -> u64 {
    path.bytes().fold(seed ^ 0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3))
}

impl Tree {
    pub fn new() -> Tree {
        Tree::default()
    }

    pub fn with(mut self, path: &str, text: &str) -> Tree {
        self.put(path, text);
        self
    }

    pub fn put(&mut self, path: &str, text: &str) {
        self.files.insert(path.to_string(), text.as_bytes().to_vec());
    }

    pub fn put_bytes(&mut self, path: &str, bytes: &[u8]) {
        self.files.insert(path.to_string(), bytes.to_vec());
    }

    pub fn remove(&mut self, path: &str) {
        self.files.remove(path);
    }

    pub fn text(&self, path: &str) -> String {
        String::from_utf8(self.files.get(path).unwrap_or_else(|| panic!("{path} is not in the tree")).clone()).unwrap()
    }

    /// Replaces `from` with `to` in a file that must contain it.
    pub fn edit(&mut self, path: &str, from: &str, to: &str) {
        let text = self.text(path);
        assert!(text.contains(from), "{path} does not contain {from:?}");
        self.put(path, &text.replacen(from, to, 1));
    }

    pub fn reversed(mut self) -> Tree {
        self.reversed = true;
        self
    }

    /// The same files, listed in an order that depends on `seed`.
    pub fn shuffled(mut self, seed: u64) -> Tree {
        self.seed = Some(seed);
        self
    }

    pub fn forget_reads(&self) {
        self.reads.borrow_mut().clear();
    }

    /// A file that can be read and is not among the listed paths.
    pub fn unlisted(mut self, path: &str, text: &str) -> Tree {
        self.unlisted.insert(path.to_string(), text.as_bytes().to_vec());
        self
    }

    /// Keeps the path listed and makes every read of it give nothing.
    pub fn unreadable(mut self, path: &str) -> Tree {
        assert!(self.files.contains_key(path), "{path} is not in the tree");
        self.unreadable.insert(path.to_string());
        self
    }

    pub fn apply(&mut self, entry: &Entry) {
        self.files.insert(entry.path.clone(), entry.bytes.clone());
    }

    pub fn reads(&self) -> BTreeMap<String, usize> {
        self.reads.borrow().clone()
    }

    pub fn was_read(&self, path: &str) -> bool {
        self.reads.borrow().contains_key(path)
    }
}

impl Files for Tree {
    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.files.keys().cloned().collect();
        if let Some(seed) = self.seed {
            paths.sort_by_key(|path| scramble(seed, path));
        }
        if self.reversed {
            paths.reverse();
        }
        paths
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        *self.reads.borrow_mut().entry(path.to_string()).or_insert(0) += 1;
        if self.unreadable.contains(path) {
            return None;
        }
        self.files.get(path).or_else(|| self.unlisted.get(path)).cloned()
    }
}

pub fn test_file(scenario: &str, name: &str) -> String {
    format!("// spec: {scenario}\n#[test]\nfn {name}() {{}}\n")
}

/// A project with one component, `billing`, one suite, `unit`, and the spec `billing.refunds`
/// with the scenarios `full`, `partial` and `stands-alone`, each declared by a test of its own.
pub fn billing() -> Tree {
    Tree::new()
        .with("alondra.toml", SETTINGS)
        .with("specs/billing/boundary.toml", BOUNDARY)
        .with("specs/billing/refunds.feature", REFUNDS)
        .with("crates/billing/src/lib.rs", "pub fn refund() {}\n")
        .with("crates/billing/tests/full.rs", &test_file("billing.refunds/full", "refunds_in_full"))
        .with("crates/billing/tests/partial.rs", &test_file("billing.refunds/partial", "refunds_in_part"))
        .with("crates/billing/tests/alone.rs", &test_file("billing.refunds/stands-alone", "stands_alone"))
}

pub fn project(tree: &Tree) -> Project {
    match load(tree) {
        Ok(project) => project,
        Err(problems) => panic!("the project does not load: {problems:#?}"),
    }
}

pub fn problems(tree: &Tree) -> Vec<Problem> {
    match load(tree) {
        Ok(_) => panic!("the project loads and it should not"),
        Err(problems) => problems,
    }
}

/// The only problem of a project that must have exactly one.
pub fn problem(tree: &Tree) -> Problem {
    let found = problems(tree);
    assert_eq!(found.len(), 1, "expected one problem: {found:#?}");
    found[0].clone()
}

pub fn codes(problems: &[Problem]) -> Vec<Code> {
    problems.iter().map(|problem| problem.code).collect()
}

/// The line, from 1, of the first line of a text that contains `needle`.
pub fn line_of(text: &str, needle: &str) -> usize {
    text.lines().position(|line| line.contains(needle)).unwrap_or_else(|| panic!("no line contains {needle:?}")) + 1
}

pub fn state(report: &Report, subject: &str, kind: Kind) -> State {
    report
        .items
        .iter()
        .find(|item| item.subject == subject && item.kind == kind)
        .unwrap_or_else(|| panic!("no {kind:?} item for {subject}: {:#?}", report.items))
        .state
}

pub fn kinds(report: &Report, subject: &str) -> Vec<Kind> {
    report.items.iter().filter(|item| item.subject == subject).map(|item| item.kind).collect()
}

/// The evidence a judgment of that subject needs: one capture, one video or none.
pub fn evidence_for(project: &Project, subject: &str, kind: Kind) -> Vec<Evidence> {
    let Some((spec_id, scenario_id)) = subject.split_once('/') else { return Vec::new() };
    if kind != Kind::Judged {
        return Vec::new();
    }
    let judgment = project
        .specs()
        .iter()
        .filter(|loaded| loaded.spec.id == spec_id)
        .flat_map(|loaded| &loaded.spec.scenarios)
        .find(|scenario| scenario.id == scenario_id)
        .and_then(|scenario| scenario.judgment.clone());
    match judgment.map(|judgment| judgment.evidence) {
        Some(EvidenceKind::Capture) => vec![Evidence { name: "evidence.png".to_string(), bytes: b"capture".to_vec() }],
        Some(EvidenceKind::Video) => vec![Evidence { name: "evidence.mp4".to_string(), bytes: b"video".to_vec() }],
        _ => Vec::new(),
    }
}

/// Records every suite as passed and every pending verdict as met, until the project is clean.
pub fn record_everything(tree: &mut Tree) {
    let loaded = project(tree);
    let mut entries = Vec::new();
    for component in loaded.components() {
        for suite in &component.suites {
            entries.push(loaded.suite_passed(&component.id, &suite.id).unwrap());
        }
    }
    for entry in &entries {
        tree.apply(entry);
    }
    loop {
        let loaded = project(tree);
        let report = loaded.report(None);
        let next = report.items.iter().find(|item| {
            matches!(item.kind, Kind::Adequate | Kind::Judged | Kind::Reference) && item.state != State::Valid
        });
        let Some(item) = next else { break };
        let evidence = evidence_for(&loaded, &item.subject, item.kind);
        let entry = loaded.verdict(&item.subject, item.kind, Verdict::Met, &evidence).unwrap();
        tree.apply(&entry);
    }
}

/// The first problem with that code of a project that must not load.
pub fn problem_with(tree: &Tree, code: Code) -> Problem {
    let found = problems(tree);
    found.iter().find(|problem| problem.code == code).cloned().unwrap_or_else(|| panic!("no {code:?} among {found:#?}"))
}

pub fn sorted_kinds(report: &Report, subject: &str) -> Vec<Kind> {
    let mut found = kinds(report, subject);
    found.sort();
    found
}

/// The fingerprint of every item of the report that has one.
pub fn fingerprints(project: &Project) -> BTreeMap<(String, Kind), Option<String>> {
    project
        .report(None)
        .items
        .iter()
        .filter(|item| item.kind != Kind::Passes)
        .map(|item| ((item.subject.clone(), item.kind), project.fingerprint(&item.subject, item.kind)))
        .collect()
}

/// A text of exactly `size` bytes: `text`, then lines of `filler` until the size is reached.
pub fn padded(text: &str, filler: char, size: usize) -> String {
    let mut out = text.to_string();
    let line = format!("{}\n", filler.to_string().repeat(63));
    while out.len() + line.len() <= size {
        out.push_str(&line);
    }
    let rest = size - out.len();
    if rest > 0 {
        out.push_str(&filler.to_string().repeat(rest - 1));
        out.push('\n');
    }
    assert_eq!(out.len(), size);
    out
}

pub fn is_fingerprint(text: &str) -> bool {
    text.strip_prefix("sha256:")
        .is_some_and(|hex| hex.len() == 64 && hex.chars().all(|digit| matches!(digit, '0'..='9' | 'a'..='f')))
}

pub fn without_lines(mut spec: Spec) -> Spec {
    for scenario in &mut spec.scenarios {
        scenario.line = 0;
    }
    spec
}

pub fn settings(decisions: bool, reference: bool) -> String {
    format!("format = 1\n\n[specs]\nenabled = true\n\n[decisions]\nenabled = {decisions}\n\n[reference]\nenabled = {reference}\n")
}

/// Adds lines (each ending in a line break) to the top table of a component's boundary.
pub fn boundary_keys(tree: &mut Tree, component: &str, keys: &str) {
    let path = format!("specs/{component}/boundary.toml");
    let text = tree.text(&path);
    let at = text.find("\n[[suite]]").unwrap_or(text.len());
    tree.put(&path, &format!("{}{keys}{}", &text[..at], &text[at..]));
}

/// A component with one suite, `unit`, and the spec `<id>.core` whose only scenario,
/// `stands-alone`, states its dependencies and is declared by a test.
pub fn add_component(tree: &mut Tree, id: &str, keys: &str) {
    tree.put(
        &format!("specs/{id}/boundary.toml"),
        &format!(
            "id = \"{id}\"\nowns = [\"crates/{id}/**\"]\n{keys}\n[[suite]]\nid = \"unit\"\nrun = [\"bazelisk\", \"test\", \"//crates/{id}:test\"]\ntests = [\"crates/{id}/tests/**/*.rs\"]\n"
        ),
    );
    tree.put(
        &format!("specs/{id}/core.feature"),
        &format!(
            "@spec:{id}.core\nFeature: The {id} component\n\n  @id:stands-alone @boundary:dependencies\n  Scenario: It uses only what it declares\n    Given the {id} crate\n    Then it uses only its declared dependencies\n"
        ),
    );
    tree.put(&format!("crates/{id}/src/lib.rs"), "pub fn run() {}\n");
    tree.put(&format!("crates/{id}/tests/alone.rs"), &test_file(&format!("{id}.core/stands-alone"), "stands_alone"));
}

pub const CONTRACT_SUITE: &str = "\n[[suite]]\nid = \"contract\"\nrun = [\"python3\", \"crates/billing/contract_test.py\"]\ntests = [\"crates/billing/contract_test.py\"]\ntimeout_minutes = 10\n";

/// Adds the suite `contract` after the suites of `billing`, with its test file.
pub fn add_contract_suite(tree: &mut Tree) {
    let text = tree.text(BOUNDARY_PATH);
    tree.put(BOUNDARY_PATH, &format!("{text}{CONTRACT_SUITE}"));
    tree.put("crates/billing/contract_test.py", "import unittest\n");
}

pub const PROMISES: &str = r#"@spec:billing.promises
Feature: What billing promises to the code around it

  @id:surface @boundary:surface
  Scenario: The public surface is the committed one
    Given the billing sources
    Then their public items equal the snapshot

  @id:data @boundary:data
  Scenario: Stored data still reads
    Given data written by an earlier build
    Then it still reads
"#;

/// `billing()` with inputs, a dependency on `accounts`, data, fixtures, a surface snapshot, the
/// suites `unit` and `contract`, and a scenario for every promise.
pub fn promised() -> Tree {
    let mut tree = billing()
        .with("Cargo.toml", "[workspace]\n")
        .with("specs/billing/surface.txt", "pub fn refund()\n")
        .with("specs/billing/promises.feature", PROMISES)
        .with(
            "crates/billing/tests/promises.rs",
            "// spec: billing.promises/surface\n#[test]\nfn keeps_its_surface() {}\n\n// spec: billing.promises/data\n#[test]\nfn reads_old_data() {}\n",
        );
    add_component(&mut tree, "accounts", "");
    boundary_keys(
        &mut tree,
        "billing",
        "inputs = [\"Cargo.toml\"]\ndepends_on = [\"accounts\"]\ndata = [\"crates/billing/data/**\"]\nfixtures = [\"crates/billing/fixtures/**\"]\nsurface = \"specs/billing/surface.txt\"\n",
    );
    add_contract_suite(&mut tree);
    tree
}

/// A decision file whose four sections each say which section of which decision they are.
pub fn decision_file(id: &str, title: &str, status: &str, supersedes: &[&str], superseded_by: &str) -> String {
    let supersedes: Vec<String> = supersedes.iter().map(|other| format!("\"{other}\"")).collect();
    let mut text = format!(
        "+++\nid = \"{id}\"\ntitle = \"{title}\"\nstatus = \"{status}\"\ndate = \"2026-11-25\"\nsupersedes = [{}]\nsuperseded_by = \"{superseded_by}\"\n+++\n",
        supersedes.join(", ")
    );
    for section in ["Context", "Decision", "Consequences", "Confirmation"] {
        text.push_str(&section_block(id, section));
    }
    text
}

pub fn section_block(id: &str, section: &str) -> String {
    format!("\n## {section}\n\n{}\n", section_text(id, section))
}

pub fn section_text(id: &str, section: &str) -> String {
    format!("The {} of {id}.", section.to_lowercase())
}

pub fn decision_path(id: &str) -> String {
    format!("docs/decisions/{id}.md")
}

pub const OVERVIEW_PATH: &str = "docs/reference/billing.md";

pub const OVERVIEW: &str = "# Billing

## How refunds work
<!-- section: billing.overview explains: billing.refunds, boundary:billing, decision:keep-ledger -->

A refund returns what the customer paid.
";

pub const AUDITED: &str = "
  @id:audited @judged:capture @evidence:shots/*.png
  Scenario: A refund shows on the receipt
    Given a refunded order
    Then the receipt shows the refund
";

/// `billing()` with a cited decision, a reference section that explains the spec, the boundary
/// and the decision, and a judged scenario, `audited`, that looks at two committed captures.
pub fn rich() -> Tree {
    let mut tree = billing()
        .with(&decision_path("keep-ledger"), &decision_file("keep-ledger", "Keep a ledger", "accepted", &[], ""))
        .with(OVERVIEW_PATH, OVERVIEW)
        .with("shots/receipt.png", "receipt capture")
        .with("shots/refund.png", "refund capture");
    tree.edit(REFUNDS_PATH, "@spec:billing.refunds\n", "@spec:billing.refunds\n@decision:keep-ledger\n");
    let text = tree.text(REFUNDS_PATH);
    tree.put(REFUNDS_PATH, &format!("{text}{AUDITED}"));
    tree
}
