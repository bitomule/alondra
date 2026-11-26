//! The specs of a task: what its author proposes with the plan, the exact set the user approves
//! with it, and every later change, which waits for the user too. Alondra reads them with the spec
//! engine over the files of the task's worktree, and compares what the branch holds with what the
//! user approved by the content of each file, never by what an agent says.
use crate::{InteractionId, Revision};
pub use alondra_spec_engine as engine;
pub use alondra_spec_engine::Files;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

/// The most files one proposal, amendment or decision changes.
pub const MAX_SPEC_FILES: usize = 64;
/// The most bytes of one file, the engine's own limit for a feature file or a decision.
pub const MAX_SPEC_FILE_BYTES: usize = 262_144;
/// The longest reason an implementer gives for a change of spec.
pub const MAX_SPEC_REASON: usize = 4000;
/// The project's settings file, at the root of its repository.
pub const SETTINGS_PATH: &str = "alondra.toml";

/// Files a change writes, by path relative to the repository: the new text, or `None` to remove it.
pub type SpecFiles = BTreeMap<String, Option<String>>;

/// How much design a change of behaviour gets: no new specs, a light round, or the full one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecLevel {
    None,
    #[default]
    Light,
    Full,
}

impl SpecLevel {
    pub const ALL: [SpecLevel; 3] = [Self::None, Self::Light, Self::Full];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Light => "light",
            Self::Full => "full",
        }
    }

    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.as_str() == word)
    }
}

/// What the project's settings say of its specs, as the task's worktree holds them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecMode {
    /// No settings file, or specs not enabled: nothing about specs applies.
    #[default]
    Off,
    /// Specs are committed in the repository.
    Committed,
    /// Specs live in Alondra while the task lasts and are never committed.
    Uncommitted,
    /// The settings cannot be read: never taken as off.
    Unreadable,
}

impl SpecMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Committed => "committed",
            Self::Uncommitted => "uncommitted",
            Self::Unreadable => "unreadable",
        }
    }
}

/// One thing the engine could not read, with the file and line it is about (0 for a whole file).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecProblem {
    pub path: String,
    #[serde(default)]
    pub line: usize,
    pub code: String,
    pub message: String,
}

impl SpecProblem {
    pub fn new(code: &str, path: &str, line: usize, message: impl Into<String>) -> Self {
        Self { path: path.into(), line, code: code.into(), message: message.into() }
    }

    fn from_engine(problem: &engine::Problem) -> Self {
        Self::new(problem.code.as_str(), &problem.path, problem.line, problem.message.clone())
    }

    /// `path:line: message`, or `path: message` for a whole file.
    pub fn located(&self) -> String {
        match self.line {
            0 => format!("{}: {}", self.path, self.message),
            line => format!("{}:{line}: {}", self.path, self.message),
        }
    }
}

/// Where the project keeps its specs, decisions and reference prose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDirs {
    pub specs: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decisions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl Default for SpecDirs {
    fn default() -> Self {
        Self { specs: "specs".into(), decisions: None, reference: None }
    }
}

impl SpecDirs {
    fn of(settings: &engine::Settings) -> Self {
        Self {
            specs: settings.specs.dir.clone(),
            decisions: settings.decisions.enabled.then(|| settings.decisions.dir.clone()),
            reference: settings.reference.enabled.then(|| settings.reference.dir.clone()),
        }
    }

    fn ledger(&self) -> String {
        format!("{}/ledger/", self.specs)
    }

    /// What `path` is to the specs of the project; `None` for a file that is not one of them.
    pub fn kind(&self, path: &str) -> Option<SpecFileKind> {
        if path == SETTINGS_PATH {
            return Some(SpecFileKind::Settings);
        }
        if path.starts_with(&self.ledger()) {
            return None;
        }
        if let Some(inside) = path.strip_prefix(&format!("{}/", self.specs)) {
            return Some(if inside.ends_with(".feature") {
                SpecFileKind::Spec
            } else if inside.ends_with("/boundary.toml") {
                SpecFileKind::Boundary
            } else {
                SpecFileKind::Other
            });
        }
        let under = |dir: &Option<String>| dir.as_ref().is_some_and(|dir| path.starts_with(&format!("{dir}/")) && path.ends_with(".md"));
        if under(&self.decisions) {
            return Some(SpecFileKind::Decision);
        }
        if under(&self.reference) {
            return Some(SpecFileKind::Reference);
        }
        None
    }

    /// Whether a change to `path` needs the user's approval: every spec file but the reference
    /// prose, whose changes are reviewed with the delivery.
    pub fn guarded(&self, path: &str) -> bool {
        self.kind(path).is_some_and(|kind| kind != SpecFileKind::Reference)
    }

    /// The file a new spec of `component` with identifier `id` goes to.
    pub fn spec_path(&self, component: &str, id: &str) -> String {
        let name = id.strip_prefix(&format!("{component}.")).unwrap_or(id);
        format!("{}/{component}/{name}.feature", self.specs)
    }

    pub fn decision_path(&self, id: &str) -> Option<String> {
        self.decisions.as_ref().map(|dir| format!("{dir}/{id}.md"))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecFileKind {
    Settings,
    Spec,
    Boundary,
    Decision,
    Reference,
    /// Another file of a component's folder, such as the snapshot of its public surface.
    Other,
}

/// What Alondra read of a set of spec changes over the task's worktree.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecCheck {
    pub mode: SpecMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<SpecProblem>,
    /// What each changed file holds in the worktree now: `None` when it does not exist.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub before: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub dirs: SpecDirs,
}

impl SpecCheck {
    pub fn unreadable(problem: SpecProblem) -> Self {
        Self { mode: SpecMode::Unreadable, problems: vec![problem], ..Self::default() }
    }

    pub fn clean(&self) -> bool {
        self.problems.is_empty() && matches!(self.mode, SpecMode::Committed | SpecMode::Uncommitted)
    }

    /// Every problem, one per line.
    pub fn summary(&self) -> String {
        self.problems.iter().map(SpecProblem::located).collect::<Vec<_>>().join("\n")
    }
}

/// One file of a proposal, as an agent sends it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecFile {
    pub path: String,
    /// The whole new text; absent to remove the file.
    #[serde(default)]
    pub text: Option<String>,
}

/// The author's proposal for the plan, as the user edits it on the plan's card.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDraft {
    pub level: SpecLevel,
    pub files: SpecFiles,
    pub check: SpecCheck,
    pub revision: Revision,
    /// The user edited it on the card since the author proposed it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub edited: bool,
}

/// The specs the user approved, with the plan and every change approved since.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedSpecs {
    /// The card whose approval fixed them: the plan's, or a decision's when the task had no plan.
    pub card: InteractionId,
    pub revision: Revision,
    pub level: SpecLevel,
    pub mode: SpecMode,
    pub files: SpecFiles,
    /// What each approved file held in the worktree before.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub before: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub dirs: SpecDirs,
    /// The approved files Alondra has still to write into the worktree.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unwritten: BTreeSet<String>,
}

impl ApprovedSpecs {
    /// The files Alondra still writes: the committed ones not yet in the worktree.
    pub fn due(&self) -> SpecFiles {
        if self.mode != SpecMode::Committed {
            return SpecFiles::new();
        }
        self.unwritten.iter().filter_map(|path| self.files.get(path).map(|text| (path.clone(), text.clone()))).collect()
    }

    /// The changes the branch may hold for the user's approval to stand: every approved file in a
    /// committed project, and only the settings in one whose specs live in Alondra.
    pub fn allowed(&self) -> SpecFiles {
        self.files.iter().filter(|(path, _)| self.mode == SpecMode::Committed || *path == SETTINGS_PATH)
            .map(|(path, text)| (path.clone(), text.clone())).collect()
    }

    pub fn merge(&mut self, files: &SpecFiles, before: &BTreeMap<String, Option<String>>) {
        for (path, text) in files {
            self.before.entry(path.clone()).or_insert_with(|| before.get(path).cloned().flatten());
            self.files.insert(path.clone(), text.clone());
            self.unwritten.insert(path.clone());
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmendmentState {
    /// The implementer asked for it; the author has still to draft it.
    Requested,
    /// The author drafted it; the user decides on its card.
    Proposed,
    Approved,
    Rejected,
}

/// A change to the approved specs while the task is implemented.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecAmendment {
    pub id: String,
    /// Why the implementer asked for it, in its words.
    pub reason: String,
    pub state: AmendmentState,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub files: SpecFiles,
    /// What each file it changes held before: the approved text, else the worktree's.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub before: BTreeMap<String, Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<InteractionId>,
    /// What the user wrote when deciding, word for word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<String>,
}

/// A lasting decision proposed on a card of its own, waiting for the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProposedDecision {
    pub id: String,
    pub title: String,
    pub card: InteractionId,
    /// The decision's file and the ones of the decisions it supersedes, as written once accepted.
    pub files: SpecFiles,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub before: BTreeMap<String, Option<String>>,
    #[serde(default)]
    pub dirs: SpecDirs,
    pub mode: SpecMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionVerdict {
    Accepted,
    Rejected,
}

/// The user's answer on a lasting decision, kept in Alondra with their words.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecisionAnswer {
    pub id: String,
    pub verdict: DecisionVerdict,
    pub card: InteractionId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub words: Option<String>,
    pub revision: Revision,
}

/// The most amendments and decision answers a task keeps; the oldest settled go first.
pub const MAX_SPEC_HISTORY: usize = 64;

/// What a task knows of its specs. A task in a project without specs has none of it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSpecs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<SpecDraft>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved: Option<ApprovedSpecs>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amendments: Vec<SpecAmendment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub proposed_decisions: Vec<ProposedDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decisions: Vec<DecisionAnswer>,
    /// Why the settings of the task's worktree cannot be read, when Alondra last found they could
    /// not: nothing about the task's specs is approved or delivered until they can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SpecProblem>,
}

impl TaskSpecs {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn amendment(&self, id: &str) -> Option<&SpecAmendment> {
        self.amendments.iter().find(|amendment| amendment.id == id)
    }

    /// The amendment the implementer asked for that the author has not drafted yet.
    pub fn requested(&self) -> Option<&SpecAmendment> {
        self.amendments.iter().find(|amendment| amendment.state == AmendmentState::Requested)
    }

    /// The approved files Alondra has still to write into the worktree.
    pub fn due(&self) -> SpecFiles {
        self.approved.as_ref().map(ApprovedSpecs::due).unwrap_or_default()
    }

    pub fn approve(&mut self, card: &InteractionId, revision: Revision, level: SpecLevel, mode: SpecMode, dirs: &SpecDirs,
        files: &SpecFiles, before: &BTreeMap<String, Option<String>>) {
        let approved = self.approved.get_or_insert_with(|| ApprovedSpecs {
            card: card.clone(), revision, level, mode, files: SpecFiles::new(), before: BTreeMap::new(), dirs: dirs.clone(), unwritten: BTreeSet::new(),
        });
        approved.merge(files, before);
    }

    pub fn answered(&mut self, answer: DecisionAnswer) {
        self.decisions.retain(|known| known.id != answer.id || known.card != answer.card);
        self.decisions.push(answer);
        let over = self.decisions.len().saturating_sub(MAX_SPEC_HISTORY);
        self.decisions.drain(..over);
    }

    pub fn amended(&mut self, amendment: SpecAmendment) {
        self.amendments.push(amendment);
        while self.amendments.len() > MAX_SPEC_HISTORY {
            let Some(settled) = self.amendments.iter().position(|a| matches!(a.state, AmendmentState::Approved | AmendmentState::Rejected)) else { break };
            self.amendments.remove(settled);
        }
    }

    /// Records that Alondra wrote `written` (path and fingerprint of what it wrote): a path leaves
    /// the files to write only when what was written is still what is approved for it.
    pub fn written(&mut self, written: &BTreeMap<String, String>) -> Vec<String> {
        let Some(approved) = self.approved.as_mut() else { return vec![] };
        let done: Vec<String> = approved.unwritten.iter()
            .filter(|path| written.get(*path).is_some_and(|print| approved.files.get(*path).is_some_and(|text| *print == fingerprint(text.as_deref()))))
            .cloned().collect();
        for path in &done {
            approved.unwritten.remove(path);
        }
        done
    }
}

/// `sha256:` and the hexadecimal SHA-256 of a file's text; the same for any absent file.
pub fn fingerprint(text: Option<&str>) -> String {
    match text {
        Some(text) => format!("sha256:{:x}", Sha256::digest(text.as_bytes())),
        None => "absent".into(),
    }
}

/// Why `path` cannot name a file of the repository, if it cannot: it must be relative, with `/`
/// between plain segments.
pub fn path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        Some("the path is empty")
    } else if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        Some("the path must be relative to the repository, with / between folders")
    } else if path.split('/').any(|segment| segment.is_empty() || segment == "." || segment == "..") {
        Some("the path must stay inside the repository: no empty, . or .. folders")
    } else if path.chars().any(char::is_control) {
        Some("the path holds a control character")
    } else {
        None
    }
}

/// The files of a proposal, checked for what the domain can tell without the worktree: how many,
/// how large, and whether each path names a file of the repository. The error names the field.
pub fn proposal(level: SpecLevel, files: &[SpecFile]) -> Result<SpecFiles, (String, String)> {
    if files.len() > MAX_SPEC_FILES {
        return Err(("files".into(), format!("A proposal changes at most {MAX_SPEC_FILES} files")));
    }
    let mut out = SpecFiles::new();
    for (index, file) in files.iter().enumerate() {
        if let Some(problem) = path_problem(&file.path) {
            return Err((format!("files[{index}].path"), format!("{}: {problem}", file.path)));
        }
        if file.text.as_ref().is_some_and(|text| text.len() > MAX_SPEC_FILE_BYTES) {
            return Err((format!("files[{index}].text"), format!("{} is larger than {MAX_SPEC_FILE_BYTES} bytes", file.path)));
        }
        if out.insert(file.path.clone(), file.text.clone()).is_some() {
            return Err((format!("files[{index}].path"), format!("{} appears twice", file.path)));
        }
    }
    if out.is_empty() && level != SpecLevel::None {
        return Err(("files".into(), "A light or full proposal changes at least one spec file; with no new specs, the level is none".into()));
    }
    Ok(out)
}

/// The repository's files with a set of changes over them.
pub struct Overlay<'a> {
    pub base: &'a dyn Files,
    pub changes: &'a SpecFiles,
}

impl Files for Overlay<'_> {
    fn paths(&self) -> Vec<String> {
        let mut paths: BTreeSet<String> = self.base.paths().into_iter().filter(|path| !matches!(self.changes.get(path), Some(None))).collect();
        paths.extend(self.changes.iter().filter(|(_, text)| text.is_some()).map(|(path, _)| path.clone()));
        paths.into_iter().collect()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        match self.changes.get(path) {
            Some(text) => text.as_ref().map(|text| text.clone().into_bytes()),
            None => self.base.read(path),
        }
    }
}

fn text_of(files: &dyn Files, path: &str) -> Option<String> {
    files.read(path).and_then(|bytes| String::from_utf8(bytes).ok())
}

fn settings_of(files: &dyn Files) -> Result<engine::Settings, Vec<SpecProblem>> {
    let listed = files.paths().iter().any(|path| path == SETTINGS_PATH);
    let bytes = files.read(SETTINGS_PATH);
    if listed && bytes.is_none() {
        return Err(vec![SpecProblem::new("unreadable_file", SETTINGS_PATH, 0, "the settings file cannot be read")]);
    }
    engine::read_settings(bytes.as_deref()).map_err(|problems| problems.iter().map(SpecProblem::from_engine).collect())
}

/// The decisions a set of changes proposes, as they are once the user accepts them.
pub fn accepted(files: &SpecFiles, dirs: &SpecDirs) -> SpecFiles {
    files.iter().map(|(path, text)| {
        let text = match (dirs.kind(path), text) {
            (Some(SpecFileKind::Decision), Some(text)) if frontmatter(text, "status").as_deref() == Some("proposed") =>
                Some(set_frontmatter(text, "status", "accepted")),
            _ => text.clone(),
        };
        (path.clone(), text)
    }).collect()
}

/// What the engine reads of the worktree `base` with `changes` over it. Proposed decisions are
/// read as accepted, as they are once approved, so a spec may cite one of its own proposal.
pub fn check(base: &dyn Files, changes: &SpecFiles) -> SpecCheck {
    let before: BTreeMap<String, Option<String>> = changes.keys().map(|path| (path.clone(), text_of(base, path))).collect();
    let overlay = Overlay { base, changes };
    let settings = match settings_of(&overlay) {
        Ok(settings) => settings,
        Err(problems) => return SpecCheck { mode: SpecMode::Unreadable, problems, before, dirs: SpecDirs::default() },
    };
    let dirs = SpecDirs::of(&settings);
    if !settings.specs.enabled {
        return SpecCheck { mode: SpecMode::Off, problems: vec![], before, dirs };
    }
    let mode = if settings.specs.committed { SpecMode::Committed } else { SpecMode::Uncommitted };
    let mut problems: Vec<SpecProblem> = changes.keys().filter_map(|path| {
        if let Some(problem) = path_problem(path) {
            return Some(SpecProblem::new("path_outside_repository", path, 0, problem));
        }
        match dirs.kind(path) {
            Some(SpecFileKind::Decision | SpecFileKind::Reference | SpecFileKind::Spec | SpecFileKind::Boundary | SpecFileKind::Settings | SpecFileKind::Other) => None,
            None => Some(SpecProblem::new("path_outside_specs", path, 0, format!(
                "a proposal only changes {SETTINGS_PATH}, files under {}/ (not its ledger){}{}",
                dirs.specs,
                dirs.decisions.as_ref().map(|dir| format!(", decisions under {dir}/")).unwrap_or_default(),
                dirs.reference.as_ref().map(|dir| format!(", reference prose under {dir}/")).unwrap_or_default()))),
        }
    }).collect();
    let promoted = accepted(changes, &dirs);
    let read = Overlay { base, changes: &promoted };
    if let Err(found) = engine::load(&read) {
        problems.extend(found.iter().map(SpecProblem::from_engine));
    }
    problems.sort_by(|a, b| (&a.path, a.line, &a.code).cmp(&(&b.path, b.line, &b.code)));
    problems.dedup();
    SpecCheck { mode, problems, before, dirs }
}

/// Why the settings file `bytes` (`None` when there is none) cannot be read, if it cannot.
pub fn settings_problem(bytes: Option<&[u8]>) -> Option<SpecProblem> {
    engine::read_settings(bytes).err().and_then(|problems| problems.first().map(SpecProblem::from_engine))
}

/// What the settings of `files` say of its specs, and why they cannot be read when they cannot.
pub fn mode(files: &dyn Files) -> Result<(SpecMode, SpecDirs), SpecProblem> {
    match settings_of(files) {
        Ok(settings) => Ok((
            match (settings.specs.enabled, settings.specs.committed) {
                (false, _) => SpecMode::Off,
                (true, true) => SpecMode::Committed,
                (true, false) => SpecMode::Uncommitted,
            },
            SpecDirs::of(&settings),
        )),
        Err(mut problems) => Err(problems.remove(0)),
    }
}

/// A spec file the branch holds unlike what the user approved.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecDrift {
    pub path: String,
    /// True when the user approved a change to it, and the branch holds another.
    pub approved: bool,
}

/// The spec files `head` changed from `base` without the user's approval, and the approved ones
/// it does not hold as approved. Reference prose and the ledger are never compared: the first is
/// reviewed with the delivery, the second is written by the validation. `Err` when the settings
/// of `head` cannot be read.
pub fn drift(head: &dyn Files, base: &dyn Files, allowed: &SpecFiles) -> Result<Vec<SpecDrift>, SpecProblem> {
    let (head_mode, dirs) = mode(head)?;
    let base_dirs = mode(base).map(|(_, dirs)| dirs).unwrap_or_else(|_| dirs.clone());
    let base_mode = mode(base).map(|(mode, _)| mode).unwrap_or(SpecMode::Unreadable);
    if head_mode == SpecMode::Off && base_mode == SpecMode::Off && !allowed.contains_key(SETTINGS_PATH) {
        return Ok(vec![]);
    }
    let guarded = |path: &str| dirs.guarded(path) || base_dirs.guarded(path);
    let mut paths: BTreeSet<String> = head.paths().into_iter().chain(base.paths()).filter(|path| guarded(path)).collect();
    paths.extend(allowed.keys().cloned());
    let mut found = vec![];
    for path in paths {
        let now = text_of(head, &path);
        match allowed.get(&path) {
            Some(approved) => {
                if now != *approved {
                    found.push(SpecDrift { path, approved: true });
                }
            }
            None => {
                if now != text_of(base, &path) {
                    found.push(SpecDrift { path, approved: false });
                }
            }
        }
    }
    Ok(found)
}

/// The value of `key` in the `+++` header of a decision, unquoted.
pub fn frontmatter(text: &str, key: &str) -> Option<String> {
    let header = header_lines(text)?;
    header.iter().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn header_lines(text: &str) -> Option<Vec<&str>> {
    let mut lines = text.lines();
    if lines.next()?.trim_end() != "+++" {
        return None;
    }
    let header: Vec<&str> = lines.take_while(|line| line.trim_end() != "+++").collect();
    Some(header)
}

/// `text` with `key` set to the string `value` in its `+++` header, added before the header's
/// end when it has none.
pub fn set_frontmatter(text: &str, key: &str, value: &str) -> String {
    let quoted = toml_string(value);
    let mut out = Vec::new();
    let mut lines = text.split_inclusive('\n');
    let mut in_header = false;
    let mut done = false;
    let mut first = true;
    for line in lines.by_ref() {
        let bare = line.trim_end();
        if first {
            first = false;
            in_header = bare == "+++";
            out.push(line.to_owned());
            continue;
        }
        if in_header && bare == "+++" {
            if !done {
                out.push(format!("{key} = {quoted}\n"));
                done = true;
            }
            in_header = false;
            out.push(line.to_owned());
            continue;
        }
        if in_header && line.split_once('=').is_some_and(|(name, _)| name.trim() == key) {
            out.push(format!("{key} = {quoted}\n"));
            done = true;
            continue;
        }
        out.push(line.to_owned());
    }
    out.concat()
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_owned()).to_string()
}

/// A lasting decision, as a person writes it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionFields {
    pub id: String,
    pub title: String,
    pub context: String,
    pub decision: String,
    pub consequences: String,
    pub confirmation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub supersedes: Vec<String>,
}

const DECISION_SECTIONS: [&str; 4] = ["Context", "Decision", "Consequences", "Confirmation"];

/// The file of a decision with `status`, dated `date`.
pub fn decision_text(fields: &DecisionFields, status: &str, date: &str) -> String {
    let supersedes = toml::Value::Array(fields.supersedes.iter().map(|id| toml::Value::String(id.clone())).collect()).to_string();
    let sections = [&fields.context, &fields.decision, &fields.consequences, &fields.confirmation];
    let body: Vec<String> = DECISION_SECTIONS.iter().zip(sections).map(|(name, text)| format!("## {name}\n\n{}\n", text.trim())).collect();
    format!(
        "+++\nid = {}\ntitle = {}\nstatus = {}\ndate = {}\nsupersedes = {supersedes}\nsuperseded_by = \"\"\n+++\n\n{}",
        toml_string(&fields.id), toml_string(fields.title.trim()), toml_string(status), toml_string(date), body.join("\n"),
    )
}

/// The fields of a decision's file, as far as they can be read.
pub fn decision_fields(text: &str) -> DecisionFields {
    let header = header_lines(text).unwrap_or_default();
    let close = if header.is_empty() && !text.starts_with("+++") { 0 } else { header.len() + 2 };
    let mut sections: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut current = None;
    for line in text.lines().skip(close) {
        match line.trim_end().strip_prefix("## ") {
            Some(name) if DECISION_SECTIONS.contains(&name.trim()) => current = Some(name.trim()),
            Some(_) => current = None,
            None => if let Some(name) = current { sections.entry(name).or_default().push(line) },
        }
    }
    let section = |name: &str| sections.get(name).map(|lines| lines.join("\n").trim().to_owned()).unwrap_or_default();
    let supersedes = header.iter().find_map(|line| {
        let (name, value) = line.split_once('=')?;
        (name.trim() == "supersedes").then(|| toml::from_str::<toml::Table>(&format!("v = {}", value.trim())).ok())?
    }).and_then(|table| table.get("v").and_then(|value| value.as_array()).map(|ids| ids.iter().filter_map(|id| id.as_str().map(str::to_owned)).collect()))
        .unwrap_or_default();
    DecisionFields {
        id: frontmatter(text, "id").unwrap_or_default(),
        title: frontmatter(text, "title").unwrap_or_default(),
        context: section("Context"),
        decision: section("Decision"),
        consequences: section("Consequences"),
        confirmation: section("Confirmation"),
        supersedes,
    }
}

/// The files a lasting decision writes once accepted: its own, and every decision it supersedes
/// marked as superseded by it. `base` is the worktree the superseded ones are read from.
pub fn decision_files(fields: &DecisionFields, date: &str, dirs: &SpecDirs, base: &dyn Files) -> Result<SpecFiles, (String, String)> {
    let Some(path) = dirs.decision_path(&fields.id) else {
        return Err(("decision".into(), "This project does not keep decisions: its alondra.toml has no [decisions] enabled".into()));
    };
    if let Some(problem) = identifier_problem(&fields.id) {
        return Err(("decision.id".into(), problem.into()));
    }
    let mut files = SpecFiles::new();
    files.insert(path, Some(decision_text(fields, "proposed", date)));
    for old in &fields.supersedes {
        let Some(old_path) = dirs.decision_path(old) else { continue };
        let Some(text) = text_of(base, &old_path) else {
            return Err(("decision.supersedes".into(), format!("There is no decision {old} at {old_path} to supersede")));
        };
        let text = set_frontmatter(&set_frontmatter(&text, "status", "superseded"), "superseded_by", &fields.id);
        files.insert(old_path, Some(text));
    }
    Ok(files)
}

/// Why `id` cannot identify a spec, scenario or decision: lowercase letters, digits, dots and
/// hyphens, starting with a letter or a digit.
pub fn identifier_problem(id: &str) -> Option<&'static str> {
    let valid = id.chars().next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-');
    (!valid).then_some("An identifier uses lowercase letters, digits, dots and hyphens, and starts with a letter or a digit")
}

/// One change the user makes on the plan's card to the proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpecEdit {
    Level(SpecLevel),
    /// A new spec of `component` when `path` is `None`; else the title and explanation of one.
    PutSpec { path: Option<String>, component: String, id: String, title: String, explanation: String },
    RemoveSpec { path: String },
    /// A new scenario when `scenario` is `None`; else the scenario of that identifier.
    PutScenario { path: String, scenario: Option<String>, id: String, title: String, steps: String, judged: bool },
    RemoveScenario { path: String, scenario: String },
    PutDecision { path: Option<String>, fields: DecisionFields, date: String },
    RemoveDecision { path: String },
}

/// Why an edit cannot be made, with the field of the form it is about.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpecEditError {
    pub field: String,
    pub message: String,
}

fn edit_error(field: &str, message: impl Into<String>) -> SpecEditError {
    SpecEditError { field: field.into(), message: message.into() }
}

fn draft_text<'a>(draft: &'a SpecDraft, path: &str) -> Result<&'a str, SpecEditError> {
    match draft.files.get(path) {
        Some(Some(text)) => Ok(text),
        _ => Err(edit_error("path", format!("{path} is not a file of this proposal"))),
    }
}

fn read_draft_spec(draft: &SpecDraft, path: &str) -> Result<engine::Spec, SpecEditError> {
    let text = draft_text(draft, path)?;
    engine::read_spec(path, text.as_bytes()).map_err(|problems| edit_error("path", problems.first().map_or_else(String::new, |p| p.message.clone())))
}

/// The body of a scenario as the form edits it: its steps, tables, doc strings and examples, one
/// level of indentation less than in its file.
pub fn scenario_body(scenario: &engine::Scenario) -> String {
    let spec = engine::Spec { id: "x".into(), decisions: vec![], title: "x".into(), description: vec![], background: vec![], scenarios: vec![engine::Scenario { judgment: None, promise: None, reserved: false, ..scenario.clone() }] };
    let text = engine::write_spec(&spec);
    let mut lines = text.lines().skip_while(|line| !line.trim_start().starts_with("Scenario")).skip(1);
    let body: Vec<String> = lines.by_ref().map(|line| line.strip_prefix("    ").unwrap_or(line.trim_start()).to_owned()).collect();
    body.join("\n").trim_end().to_owned()
}

fn parse_scenario(id: &str, title: &str, steps: &str) -> Result<engine::Scenario, SpecEditError> {
    let outline = steps.lines().any(|line| line.trim() == "Examples:");
    let keyword = if outline { "Scenario Outline" } else { "Scenario" };
    let body: Vec<String> = steps.lines().map(|line| format!("    {line}")).collect();
    let text = format!("@spec:x\nFeature: x\n\n  @id:{id}\n  {keyword}: {}\n{}\n", title.trim(), body.join("\n"));
    let spec = engine::read_spec("scenario", text.as_bytes()).map_err(|problems| {
        let problem = problems.first();
        let field = match problem.map(|p| p.line) {
            Some(5) => "title",
            Some(line) if line > 5 => "steps",
            _ => "id",
        };
        let message = match problem {
            Some(p) if p.line > 5 => format!("Line {}: {}", p.line - 5, p.message),
            Some(p) => p.message.clone(),
            None => "The scenario cannot be read".into(),
        };
        edit_error(field, message)
    })?;
    let scenario = spec.scenarios.into_iter().next().ok_or_else(|| edit_error("steps", "The scenario cannot be read"))?;
    if scenario.steps.is_empty() {
        return Err(edit_error("steps", "A scenario has at least one step: Given, When or Then"));
    }
    Ok(scenario)
}

/// The proposal with `edit` made, or why it cannot be made. Every file it writes is read back by
/// the engine, so an edit never leaves a spec it cannot read.
pub fn edited(draft: &SpecDraft, edit: &SpecEdit) -> Result<(SpecLevel, SpecFiles), SpecEditError> {
    let mut files = draft.files.clone();
    let mut level = draft.level;
    let removes = |files: &mut SpecFiles, path: &str| {
        files.remove(path);
    };
    match edit {
        SpecEdit::Level(new) => level = *new,
        SpecEdit::PutSpec { path, component, id, title, explanation } => {
            if title.trim().is_empty() {
                return Err(edit_error("title", "A spec has a title"));
            }
            if let Some(problem) = identifier_problem(id) {
                return Err(edit_error("id", problem));
            }
            let description: Vec<String> = explanation.lines().map(|line| line.trim().to_owned()).filter(|line| !line.is_empty()).collect();
            let (path, spec) = match path {
                Some(path) => {
                    let mut spec = read_draft_spec(draft, path)?;
                    spec.id = id.clone();
                    spec.title = title.trim().to_owned();
                    spec.description = description;
                    (path.clone(), spec)
                }
                None => {
                    if let Some(problem) = identifier_problem(component) {
                        return Err(edit_error("component", problem));
                    }
                    let path = draft.check.dirs.spec_path(component, id);
                    if draft.files.get(&path).is_some_and(Option::is_some) || draft.check.before.get(&path).is_some_and(Option::is_some) {
                        return Err(edit_error("id", format!("{path} already exists")));
                    }
                    (path, engine::Spec { id: id.clone(), decisions: vec![], title: title.trim().to_owned(), description, background: vec![], scenarios: vec![] })
                }
            };
            let text = engine::write_spec(&spec);
            engine::read_spec(&path, text.as_bytes()).map_err(|problems| edit_error("explanation", problems.first().map_or_else(String::new, |p| p.message.clone())))?;
            files.insert(path, Some(text));
        }
        SpecEdit::RemoveSpec { path } => {
            draft_text(draft, path)?;
            removes(&mut files, path);
        }
        SpecEdit::PutScenario { path, scenario, id, title, steps, judged } => {
            if let Some(problem) = identifier_problem(id) {
                return Err(edit_error("id", problem));
            }
            if title.trim().is_empty() {
                return Err(edit_error("title", "A scenario has a title"));
            }
            let mut spec = read_draft_spec(draft, path)?;
            let mut made = parse_scenario(id, title, steps)?;
            let others = spec.scenarios.iter().filter(|known| Some(&known.id) != scenario.as_ref());
            if others.clone().any(|known| known.id == *id) {
                return Err(edit_error("id", format!("The spec already has a scenario {id}")));
            }
            match scenario {
                Some(old) => {
                    let known = spec.scenarios.iter_mut().find(|known| known.id == *old).ok_or_else(|| edit_error("scenario", format!("The spec has no scenario {old}")))?;
                    made.promise = known.promise;
                    made.reserved = known.reserved;
                    made.judgment = match (*judged, known.judgment.take()) {
                        (false, _) => None,
                        (true, Some(kept)) => Some(kept),
                        (true, None) => Some(engine::Judgment { evidence: engine::EvidenceKind::None, pattern: None }),
                    };
                    *known = made;
                }
                None => {
                    made.judgment = judged.then_some(engine::Judgment { evidence: engine::EvidenceKind::None, pattern: None });
                    spec.scenarios.push(made);
                }
            }
            let text = engine::write_spec(&spec);
            engine::read_spec(path, text.as_bytes()).map_err(|problems| edit_error("steps", problems.first().map_or_else(String::new, |p| p.message.clone())))?;
            files.insert(path.clone(), Some(text));
        }
        SpecEdit::RemoveScenario { path, scenario } => {
            let mut spec = read_draft_spec(draft, path)?;
            let before = spec.scenarios.len();
            spec.scenarios.retain(|known| known.id != *scenario);
            if spec.scenarios.len() == before {
                return Err(edit_error("scenario", format!("The spec has no scenario {scenario}")));
            }
            files.insert(path.clone(), Some(engine::write_spec(&spec)));
        }
        SpecEdit::PutDecision { path, fields, date } => {
            if let Some(problem) = identifier_problem(&fields.id) {
                return Err(edit_error("id", problem));
            }
            for (name, text) in [("title", &fields.title), ("context", &fields.context), ("decision", &fields.decision), ("consequences", &fields.consequences), ("confirmation", &fields.confirmation)] {
                if text.trim().is_empty() {
                    return Err(edit_error(name, "Every part of a decision is written"));
                }
            }
            let target = draft.check.dirs.decision_path(&fields.id).ok_or_else(|| edit_error("id", "This project does not keep decisions: its alondra.toml has no [decisions] enabled"))?;
            match path {
                Some(path) => {
                    draft_text(draft, path)?;
                    if *path != target {
                        removes(&mut files, path);
                    }
                }
                None => {
                    if files.get(&target).is_some_and(Option::is_some) || draft.check.before.get(&target).is_some_and(Option::is_some) {
                        return Err(edit_error("id", format!("{target} already exists")));
                    }
                }
            }
            files.insert(target, Some(decision_text(fields, "proposed", date)));
        }
        SpecEdit::RemoveDecision { path } => {
            draft_text(draft, path)?;
            removes(&mut files, path);
        }
    }
    if files.len() > MAX_SPEC_FILES {
        return Err(edit_error("files", format!("A proposal changes at most {MAX_SPEC_FILES} files")));
    }
    Ok((level, files))
}

/// The decisions a proposal holds, by identifier, read from their files.
pub fn draft_decisions(files: &SpecFiles, dirs: &SpecDirs) -> Vec<(String, DecisionFields)> {
    files.iter().filter(|(path, _)| dirs.kind(path) == Some(SpecFileKind::Decision))
        .filter_map(|(path, text)| text.as_ref().map(|text| (path.clone(), decision_fields(text))))
        .collect()
}

/// The new decisions `before` proposes that `after` no longer holds: the user took them out. A
/// decision the project already had keeps its file and its status.
pub fn removed_decisions(before: &SpecDraft, after: &SpecFiles) -> Vec<String> {
    draft_decisions(&before.files, &before.check.dirs).into_iter()
        .filter(|(path, _)| !matches!(after.get(path), Some(Some(_))) && before.check.before.get(path).is_none_or(Option::is_none))
        .map(|(_, fields)| fields.id)
        .filter(|id| !id.is_empty())
        .collect()
}

/// The UTC calendar date of `millis` since the Unix epoch, as `YYYY-MM-DD`.
pub fn date_of(millis: i64) -> String {
    let days = millis.div_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The short name an edit screen gives a file or a scenario of a proposal.
pub fn handle(text: &str) -> String {
    fingerprint(Some(text)).chars().skip("sha256:".len()).take(12).collect()
}

pub fn scenario_handle(path: &str, id: &str) -> String {
    handle(&format!("{path}#{id}"))
}

/// The path of the file of `draft` with handle `h`.
pub fn file_of<'a>(draft: &'a SpecDraft, h: &str) -> Option<&'a str> {
    draft.files.keys().find(|path| handle(path) == h).map(String::as_str)
}

/// The path and identifier of the scenario of `draft` with handle `h`.
pub fn scenario_of(draft: &SpecDraft, h: &str) -> Option<(String, String)> {
    draft.files.iter().filter_map(|(path, text)| Some((path, text.as_ref()?)))
        .filter_map(|(path, text)| engine::read_spec(path, text.as_bytes()).ok().map(|spec| (path, spec)))
        .find_map(|(path, spec)| spec.scenarios.iter().find(|scenario| scenario_handle(path, &scenario.id) == h).map(|scenario| (path.clone(), scenario.id.clone())))
}
