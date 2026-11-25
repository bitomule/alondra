//! Reads specs, boundaries, decisions and reference prose from the files its caller hands it, and
//! says what is validated and what is missing. It never touches the disk, the network, the clock
//! or another program (`docs/architecture/specs.md`).

mod boundary;
mod decision;
mod feature;
mod fingerprint;
mod ident;
mod ledger;
mod listing;
mod markers;
mod packet;
mod pattern;
mod problem;
mod project;
mod reference;
mod report;
mod results;
mod settings;

use std::collections::BTreeMap;

/// The committed files of a repository, as the caller sees them. Paths are relative to the root,
/// with `/` between segments.
pub trait Files {
    fn paths(&self) -> Vec<String>;
    fn read(&self, path: &str) -> Option<Vec<u8>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub code: Code,
    pub path: String,
    /// 0 when the problem is about the whole file.
    pub line: usize,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Code {
    BoundaryWithoutScenario,
    ComponentIdMismatch,
    DecisionIdMismatch,
    DecisionNotAccepted,
    DependencyCycle,
    DuplicateScenarioId,
    DuplicateSectionId,
    DuplicateSpecId,
    DuplicateSuiteId,
    EvidenceRequired,
    EvidenceWithoutCapture,
    InvalidBoundary,
    InvalidDecision,
    InvalidIdentifier,
    InvalidLedger,
    InvalidMarker,
    InvalidResults,
    InvalidSettings,
    InvalidUtf8,
    LedgerInsideComponent,
    MarkerOutsideComponent,
    MissingScenarioId,
    MissingSection,
    MissingSpecTag,
    NotJudged,
    OrphanMarker,
    PathOutsideRepository,
    RegeneratesOutsideOwns,
    SectionExplainsNothing,
    SpecWithoutComponent,
    SupersessionMismatch,
    TooLarge,
    TooManyScenarios,
    UnknownComponent,
    UnknownDecision,
    UnknownReferenceTarget,
    UnknownSubject,
    UnknownSuite,
    UnknownTag,
    UnreadableFile,
    UnsupportedFormat,
    UnsupportedSyntax,
}

impl Code {
    /// The name written in reports: the variant in snake case, such as `missing_spec_tag`.
    pub fn as_str(self) -> &'static str {
        match self {
            Code::BoundaryWithoutScenario => "boundary_without_scenario",
            Code::ComponentIdMismatch => "component_id_mismatch",
            Code::DecisionIdMismatch => "decision_id_mismatch",
            Code::DecisionNotAccepted => "decision_not_accepted",
            Code::DependencyCycle => "dependency_cycle",
            Code::DuplicateScenarioId => "duplicate_scenario_id",
            Code::DuplicateSectionId => "duplicate_section_id",
            Code::DuplicateSpecId => "duplicate_spec_id",
            Code::DuplicateSuiteId => "duplicate_suite_id",
            Code::EvidenceRequired => "evidence_required",
            Code::EvidenceWithoutCapture => "evidence_without_capture",
            Code::InvalidBoundary => "invalid_boundary",
            Code::InvalidDecision => "invalid_decision",
            Code::InvalidIdentifier => "invalid_identifier",
            Code::InvalidLedger => "invalid_ledger",
            Code::InvalidMarker => "invalid_marker",
            Code::InvalidResults => "invalid_results",
            Code::InvalidSettings => "invalid_settings",
            Code::InvalidUtf8 => "invalid_utf8",
            Code::LedgerInsideComponent => "ledger_inside_component",
            Code::MarkerOutsideComponent => "marker_outside_component",
            Code::MissingScenarioId => "missing_scenario_id",
            Code::MissingSection => "missing_section",
            Code::MissingSpecTag => "missing_spec_tag",
            Code::NotJudged => "not_judged",
            Code::OrphanMarker => "orphan_marker",
            Code::PathOutsideRepository => "path_outside_repository",
            Code::RegeneratesOutsideOwns => "regenerates_outside_owns",
            Code::SectionExplainsNothing => "section_explains_nothing",
            Code::SpecWithoutComponent => "spec_without_component",
            Code::SupersessionMismatch => "supersession_mismatch",
            Code::TooLarge => "too_large",
            Code::TooManyScenarios => "too_many_scenarios",
            Code::UnknownComponent => "unknown_component",
            Code::UnknownDecision => "unknown_decision",
            Code::UnknownReferenceTarget => "unknown_reference_target",
            Code::UnknownSubject => "unknown_subject",
            Code::UnknownSuite => "unknown_suite",
            Code::UnknownTag => "unknown_tag",
            Code::UnreadableFile => "unreadable_file",
            Code::UnsupportedFormat => "unsupported_format",
            Code::UnsupportedSyntax => "unsupported_syntax",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub specs: SpecsSettings,
    pub decisions: FolderSettings,
    pub reference: FolderSettings,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecsSettings {
    pub enabled: bool,
    pub committed: bool,
    pub dir: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderSettings {
    pub enabled: bool,
    pub dir: String,
}

/// The settings of a project from the bytes of its `alondra.toml`, or `None` when it has none.
pub fn read_settings(bytes: Option<&[u8]>) -> Result<Settings, Vec<Problem>> {
    settings::read(bytes)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    pub id: String,
    pub decisions: Vec<String>,
    pub title: String,
    pub description: Vec<String>,
    pub background: Vec<Step>,
    pub scenarios: Vec<Scenario>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Scenario {
    pub id: String,
    pub title: String,
    /// The line of its `Scenario:` keyword, from 1; 0 for a scenario that was not read from a file.
    pub line: usize,
    pub outline: bool,
    pub judgment: Option<Judgment>,
    pub promise: Option<Promise>,
    pub reserved: bool,
    pub steps: Vec<Step>,
    /// The rows of its `Examples:` table, header first; empty when it has none.
    pub examples: Vec<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Judgment {
    pub evidence: EvidenceKind,
    pub pattern: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvidenceKind {
    None,
    Capture,
    Video,
}

impl EvidenceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EvidenceKind::None => "none",
            EvidenceKind::Capture => "capture",
            EvidenceKind::Video => "video",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Promise {
    Surface,
    Data,
    Dependencies,
}

impl Promise {
    pub fn as_str(self) -> &'static str {
        match self {
            Promise::Surface => "surface",
            Promise::Data => "data",
            Promise::Dependencies => "dependencies",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Step {
    pub keyword: Keyword,
    pub text: String,
    pub doc: Option<String>,
    pub table: Vec<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keyword {
    Given,
    When,
    Then,
    And,
    But,
}

impl Keyword {
    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Given => "Given",
            Keyword::When => "When",
            Keyword::Then => "Then",
            Keyword::And => "And",
            Keyword::But => "But",
        }
    }
}

/// A spec from the bytes of a `.feature` file. Two equal specs may differ in the `line` of their
/// scenarios: comparing specs ignores nothing, so compare what `write_spec` gives when lines differ.
pub fn read_spec(path: &str, bytes: &[u8]) -> Result<Spec, Vec<Problem>> {
    feature::read(path, bytes).map(|read| read.spec)
}

/// The canonical text of a spec, ending in a line break.
pub fn write_spec(spec: &Spec) -> String {
    feature::write(spec)
}

/// Whether a path pattern (`*` inside one segment, `**` across segments) matches a path.
pub fn matches(pattern: &str, path: &str) -> bool {
    pattern::matches(pattern, path)
}

/// Which kind of evidence a file is, from the extension of its name.
pub fn evidence_kind(name: &str) -> EvidenceKind {
    match name.rsplit_once('.').map(|(_, extension)| extension) {
        Some("png" | "jpg" | "jpeg" | "webp") => EvidenceKind::Capture,
        Some("mov" | "mp4" | "webm") => EvidenceKind::Video,
        _ => EvidenceKind::None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Component {
    pub id: String,
    /// The path of its boundary file.
    pub path: String,
    pub owns: Vec<String>,
    pub regenerates: Vec<String>,
    pub inputs: Vec<String>,
    pub depends_on: Vec<String>,
    pub data: Vec<String>,
    pub fixtures: Vec<String>,
    pub surface: Option<String>,
    pub suites: Vec<Suite>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suite {
    pub id: String,
    pub run: Vec<String>,
    pub tests: Vec<String>,
    pub results: Option<String>,
    pub timeout_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedSpec {
    pub path: String,
    pub component: String,
    pub spec: Spec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub id: String,
    pub path: String,
    pub title: String,
    pub status: Status,
    pub date: String,
    pub supersedes: Vec<String>,
    pub superseded_by: Option<String>,
    pub context: String,
    pub decision: String,
    pub consequences: String,
    pub confirmation: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Proposed,
    Accepted,
    Rejected,
    Superseded,
    Deprecated,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Proposed => "proposed",
            Status::Accepted => "accepted",
            Status::Rejected => "rejected",
            Status::Superseded => "superseded",
            Status::Deprecated => "deprecated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub id: String,
    pub path: String,
    /// The line of its heading, from 1.
    pub line: usize,
    pub heading: String,
    /// The heading and every line up to the next section, without the comment line.
    pub text: String,
    pub explains: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    pub path: String,
    pub line: usize,
    /// `<spec>/<scenario>`.
    pub scenario: String,
    /// The name of the test function under the marker, when one is found.
    pub test: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Kind {
    Suite,
    Passes,
    Adequate,
    Judged,
    Reference,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Suite => "suite",
            Kind::Passes => "passes",
            Kind::Adequate => "adequate",
            Kind::Judged => "judged",
            Kind::Reference => "reference",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Valid,
    Missing,
    Stale,
    Unmet,
    Failed,
    Uncovered,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Valid => "valid",
            State::Missing => "missing",
            State::Stale => "stale",
            State::Unmet => "unmet",
            State::Failed => "failed",
            State::Uncovered => "uncovered",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Met,
    Unmet,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Met => "met",
            Verdict::Unmet => "unmet",
        }
    }
}

/// An evidence file handed over with a judgment: its name and its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// What the ledger holds for one subject and kind of validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recorded {
    pub inputs: String,
    /// `None` for a suite, which is only recorded when it passed.
    pub verdict: Option<Verdict>,
    /// The fingerprints of the evidence handed over with a judgment.
    pub evidence: Vec<String>,
}

/// A ledger file for the caller to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// One command of a plan, for the caller to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    pub component: String,
    pub suite: String,
    pub arguments: Vec<String>,
    pub timeout_minutes: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub status: ReportStatus,
    pub items: Vec<Item>,
    /// Ledger files of subjects that no longer exist, in path order.
    pub leftover: Vec<String>,
    pub problems: Vec<Problem>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportStatus {
    Clean,
    Pending,
    Unreadable,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Clean => "clean",
            ReportStatus::Pending => "pending",
            ReportStatus::Unreadable => "unreadable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// `<spec>/<scenario>`, `<component>/<suite>` or the id of a reference section.
    pub subject: String,
    /// Empty for a reference section.
    pub component: String,
    pub kind: Kind,
    pub state: State,
}

impl Report {
    /// The report of a project that did not load.
    pub fn unreadable(mut problems: Vec<Problem>) -> Report {
        problem::sort(&mut problems);
        Report { status: ReportStatus::Unreadable, items: Vec::new(), leftover: Vec::new(), problems }
    }

    /// The report as JSON, indented with two spaces, without a final line break.
    pub fn to_json(&self) -> String {
        report::to_json(self)
    }
}

/// The results of a test run, read from JUnit XML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestResults {
    cases: Vec<(String, bool)>,
}

impl TestResults {
    /// Whether a test of that name passed: at least one case is that test, and every case that is
    /// that test passed. A case is the test when its name is the test, or ends in `::` or `.`
    /// followed by it.
    pub fn passed(&self, test: &str) -> bool {
        let qualified = format!("::{test}");
        let dotted = format!(".{test}");
        let mut matching = self
            .cases
            .iter()
            .filter(|(case, _)| case == test || case.ends_with(&qualified) || case.ends_with(&dotted))
            .peekable();
        matching.peek().is_some() && matching.all(|(_, passed)| *passed)
    }
}

/// The results of a test run from JUnit XML of at most 16777216 bytes.
pub fn read_results(bytes: &[u8]) -> Result<TestResults, Problem> {
    results::read(bytes).map(|cases| TestResults { cases })
}

/// A project read in full: everything needed to report, plan, record and build packets without
/// reading a file again.
#[derive(Debug, Clone)]
pub struct Project {
    settings: Settings,
    components: Vec<Component>,
    specs: Vec<LoadedSpec>,
    decisions: Vec<Decision>,
    sections: Vec<Section>,
    markers: Vec<Marker>,
    listed: Vec<String>,
    hashes: BTreeMap<String, String>,
    component_fingerprints: BTreeMap<String, String>,
    ledger: BTreeMap<String, ledger::LedgerFile>,
    leftover: Vec<String>,
}

pub fn load(files: &dyn Files) -> Result<Project, Vec<Problem>> {
    project::load(files)
}

impl Project {
    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn components(&self) -> &[Component] {
        &self.components
    }

    pub fn specs(&self) -> &[LoadedSpec] {
        &self.specs
    }

    pub fn decisions(&self) -> &[Decision] {
        &self.decisions
    }

    pub fn sections(&self) -> &[Section] {
        &self.sections
    }

    pub fn markers(&self) -> &[Marker] {
        &self.markers
    }

    /// The current fingerprint of a subject for a kind of validation: `Suite` of
    /// `<component>/<suite>`, `Adequate` or `Judged` of `<spec>/<scenario>`, `Reference` of a section.
    pub fn fingerprint(&self, subject: &str, kind: Kind) -> Option<String> {
        fingerprint::of(self, subject, kind)
    }

    /// What the ledger holds for a subject and a kind (`Suite`, `Adequate`, `Judged` or `Reference`).
    pub fn recorded(&self, subject: &str, kind: Kind) -> Option<Recorded> {
        ledger::lookup(self, subject, kind).cloned()
    }

    pub fn report(&self, results: Option<&TestResults>) -> Report {
        report::build(self, results)
    }

    pub fn plan(&self, component: &str, suite: Option<&str>) -> Result<Vec<Run>, Problem> {
        ledger::plan(self, component, suite)
    }

    pub fn suite_passed(&self, component: &str, suite: &str) -> Result<Entry, Problem> {
        ledger::suite_passed(self, component, suite)
    }

    /// The ledger entry of a verdict of kind `Adequate` or `Judged` on a scenario, or `Reference`
    /// on a section, keeping what the entry already holds.
    pub fn verdict(&self, subject: &str, kind: Kind, verdict: Verdict, evidence: &[Evidence]) -> Result<Entry, Problem> {
        ledger::verdict(self, subject, kind, verdict, evidence)
    }

    pub fn packet(&self, component: &str) -> Result<String, Problem> {
        packet::build(self, component)
    }

    /// Everything the project holds, as JSON, for a caller that shows it.
    pub fn listing(&self) -> String {
        listing::build(self)
    }
}
