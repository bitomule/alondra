use serde::Deserialize;

use crate::fingerprint::{self, is_fingerprint, of_bytes};
use crate::problem::{TOML_MAX_BYTES, problem, toml_line, too_large};
use crate::{Code, Entry, Evidence, EvidenceKind, Kind, Problem, Project, Recorded, Run, Verdict, evidence_kind};

const EXTENSION: &str = ".toml";
const SUITES: &str = "suites";
const SCENARIOS: &str = "scenarios";
const REFERENCE: &str = "reference";

/// What one ledger file holds.
#[derive(Debug, Clone, Default)]
pub(crate) struct LedgerFile {
    pub(crate) suite: Option<Recorded>,
    pub(crate) adequate: Option<Recorded>,
    pub(crate) judged: Option<Recorded>,
    pub(crate) reference: Option<Recorded>,
}

/// What a ledger file is about, from its path.
pub(crate) enum Subject {
    Suite { component: String, suite: String },
    Scenario { component: String, scenario: String },
    Reference { section: String },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SuiteFile {
    inputs: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScenarioFile {
    adequate: Option<Table>,
    judged: Option<Table>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReferenceFile {
    reference: Table,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Table {
    inputs: String,
    verdict: String,
    #[serde(default)]
    evidence: Vec<String>,
}

pub(crate) fn ledger_dir(specs_dir: &str) -> String {
    format!("{specs_dir}/ledger")
}

pub(crate) fn classify(ledger: &str, path: &str) -> Option<Subject> {
    let rest = path.strip_prefix(ledger)?.strip_prefix('/')?;
    let segments: Vec<&str> = rest.split('/').collect();
    fn named(name: &str) -> Option<String> {
        name.strip_suffix(EXTENSION).filter(|stem| !stem.is_empty()).map(str::to_string)
    }
    match segments.as_slice() {
        [folder, name] if *folder == REFERENCE => Some(Subject::Reference { section: named(name)? }),
        [component, folder, name] if *folder == SUITES => {
            Some(Subject::Suite { component: component.to_string(), suite: named(name)? })
        }
        [component, folder, spec, name] if *folder == SCENARIOS => Some(Subject::Scenario {
            component: component.to_string(),
            scenario: format!("{spec}/{}", named(name)?),
        }),
        _ => None,
    }
}

fn invalid(path: &str, line: usize, message: impl Into<String>) -> Problem {
    problem(Code::InvalidLedger, path, line, message)
}

fn parse<T: for<'de> Deserialize<'de>>(path: &str, source: &str) -> Result<T, Problem> {
    toml::from_str(source).map_err(|failure| invalid(path, toml_line(source, &failure), failure.message().to_string()))
}

fn checked_fingerprint(path: &str, value: &str) -> Result<(), Problem> {
    if is_fingerprint(value) {
        Ok(())
    } else {
        Err(invalid(path, 0, format!("{value:?} is not a fingerprint")))
    }
}

fn recorded(path: &str, table: Table) -> Result<Recorded, Problem> {
    checked_fingerprint(path, &table.inputs)?;
    for evidence in &table.evidence {
        checked_fingerprint(path, evidence)?;
    }
    let verdict = match table.verdict.as_str() {
        "met" => Verdict::Met,
        "unmet" => Verdict::Unmet,
        other => return Err(invalid(path, 0, format!("{other:?} is not a verdict"))),
    };
    Ok(Recorded { inputs: table.inputs, verdict: Some(verdict), evidence: table.evidence })
}

pub(crate) fn read(path: &str, subject: &Subject, bytes: &[u8]) -> Result<LedgerFile, Problem> {
    if let Some(found) = too_large(path, bytes, TOML_MAX_BYTES) {
        return Err(found);
    }
    let source = std::str::from_utf8(bytes).map_err(|_| invalid(path, 0, "the ledger file is not valid UTF-8"))?;
    let mut file = LedgerFile::default();
    match subject {
        Subject::Suite { .. } => {
            let parsed: SuiteFile = parse(path, source)?;
            checked_fingerprint(path, &parsed.inputs)?;
            file.suite = Some(Recorded { inputs: parsed.inputs, verdict: None, evidence: Vec::new() });
        }
        Subject::Scenario { .. } => {
            let parsed: ScenarioFile = parse(path, source)?;
            file.adequate = parsed.adequate.map(|table| recorded(path, table)).transpose()?;
            file.judged = parsed.judged.map(|table| recorded(path, table)).transpose()?;
        }
        Subject::Reference { .. } => {
            let parsed: ReferenceFile = parse(path, source)?;
            file.reference = Some(recorded(path, parsed.reference)?);
        }
    }
    Ok(file)
}

/// A problem when the file holds a judgment, written without evidence, of a scenario whose
/// judgment needs some.
pub(crate) fn missing_evidence(project: &Project, path: &str, subject: &Subject, file: &LedgerFile) -> Option<Problem> {
    let Subject::Scenario { scenario, .. } = subject else { return None };
    let judged = file.judged.as_ref()?;
    let (_, found) = project.scenario(scenario)?;
    let needed = found.judgment.as_ref()?.evidence;
    (needed != EvidenceKind::None && judged.evidence.is_empty()).then(|| {
        problem(
            Code::EvidenceRequired,
            path,
            0,
            format!("{scenario} needs at least one {} file as evidence, and the judgment lists none", needed.as_str()),
        )
    })
}

pub(crate) fn subject_exists(project: &Project, subject: &Subject) -> bool {
    match subject {
        Subject::Suite { component, suite } => project
            .components
            .iter()
            .any(|found| found.id == *component && found.suites.iter().any(|candidate| candidate.id == *suite)),
        Subject::Scenario { component, scenario } => {
            project.scenario(scenario).is_some_and(|(loaded, _)| loaded.component == *component)
        }
        Subject::Reference { section } => project.sections.iter().any(|found| found.id == *section),
    }
}

fn suite_path(project: &Project, component: &str, suite: &str) -> String {
    format!("{}/{component}/{SUITES}/{suite}{EXTENSION}", ledger_dir(&project.settings.specs.dir))
}

fn scenario_path(project: &Project, component: &str, subject: &str) -> String {
    format!("{}/{component}/{SCENARIOS}/{subject}{EXTENSION}", ledger_dir(&project.settings.specs.dir))
}

fn reference_path(project: &Project, section: &str) -> String {
    format!("{}/{REFERENCE}/{section}{EXTENSION}", ledger_dir(&project.settings.specs.dir))
}

pub(crate) fn lookup<'a>(project: &'a Project, subject: &str, kind: Kind) -> Option<&'a Recorded> {
    match kind {
        Kind::Suite => {
            let (component, suite) = subject.split_once('/')?;
            project.ledger.get(&suite_path(project, component, suite))?.suite.as_ref()
        }
        Kind::Adequate | Kind::Judged => {
            let (loaded, _) = project.scenario(subject)?;
            let file = project.ledger.get(&scenario_path(project, &loaded.component, subject))?;
            if kind == Kind::Adequate { file.adequate.as_ref() } else { file.judged.as_ref() }
        }
        Kind::Reference => project.ledger.get(&reference_path(project, subject))?.reference.as_ref(),
        Kind::Passes => None,
    }
}

fn unknown_component(component: &str) -> Problem {
    problem(Code::UnknownComponent, "", 0, format!("there is no component {component:?}"))
}

fn unknown_suite(component: &str, suite: &str) -> Problem {
    problem(Code::UnknownSuite, "", 0, format!("the component {component:?} has no suite {suite:?}"))
}

fn unknown_subject(subject: &str, kind: Kind) -> Problem {
    problem(Code::UnknownSubject, "", 0, format!("there is nothing to record as {} for {subject:?}", kind.as_str()))
}

pub(crate) fn plan(project: &Project, component: &str, suite: Option<&str>) -> Result<Vec<Run>, Problem> {
    let found = project.components.iter().find(|candidate| candidate.id == component).ok_or_else(|| unknown_component(component))?;
    let suites: Vec<_> = match suite {
        Some(id) => {
            let chosen = found.suites.iter().find(|candidate| candidate.id == id).ok_or_else(|| unknown_suite(component, id))?;
            vec![chosen]
        }
        None => found.suites.iter().collect(),
    };
    Ok(suites
        .into_iter()
        .map(|chosen| Run {
            component: found.id.clone(),
            suite: chosen.id.clone(),
            arguments: chosen.run.clone(),
            timeout_minutes: chosen.timeout_minutes,
        })
        .collect())
}

pub(crate) fn suite_passed(project: &Project, component: &str, suite: &str) -> Result<Entry, Problem> {
    let found = project.components.iter().find(|candidate| candidate.id == component).ok_or_else(|| unknown_component(component))?;
    let chosen = found.suites.iter().find(|candidate| candidate.id == suite).ok_or_else(|| unknown_suite(component, suite))?;
    let inputs = fingerprint::suite(project, found, chosen);
    Ok(Entry { path: suite_path(project, component, suite), bytes: format!("inputs = \"{inputs}\"\n").into_bytes() })
}

fn table(name: &str, entry: &Recorded) -> String {
    let mut out = format!("[{name}]\ninputs = \"{}\"\n", entry.inputs);
    if let Some(verdict) = entry.verdict {
        out.push_str(&format!("verdict = \"{}\"\n", verdict.as_str()));
    }
    if !entry.evidence.is_empty() {
        let quoted: Vec<String> = entry.evidence.iter().map(|evidence| format!("\"{evidence}\"")).collect();
        out.push_str(&format!("evidence = [{}]\n", quoted.join(", ")));
    }
    out
}

fn has_evidence(evidence: &[Evidence], kind: EvidenceKind) -> bool {
    evidence.iter().any(|file| evidence_kind(&file.name) == kind)
}

pub(crate) fn verdict(
    project: &Project,
    subject: &str,
    kind: Kind,
    verdict: Verdict,
    evidence: &[Evidence],
) -> Result<Entry, Problem> {
    match kind {
        Kind::Adequate | Kind::Judged => {
            let (loaded, scenario) = project.scenario(subject).ok_or_else(|| unknown_subject(subject, kind))?;
            let path = scenario_path(project, &loaded.component, subject);
            let mut file = project.ledger.get(&path).cloned().unwrap_or_default();
            if kind == Kind::Adequate {
                if !project.markers.iter().any(|marker| marker.scenario == subject) {
                    return Err(unknown_subject(subject, kind));
                }
                let inputs = fingerprint::adequate(project, loaded, scenario);
                file.adequate = Some(Recorded { inputs, verdict: Some(verdict), evidence: Vec::new() });
            } else {
                let Some(judgment) = &scenario.judgment else {
                    return Err(problem(Code::NotJudged, "", 0, format!("{subject} is not a judgment")));
                };
                let needed = judgment.evidence;
                if needed != EvidenceKind::None && !has_evidence(evidence, needed) {
                    return Err(problem(
                        Code::EvidenceRequired,
                        "",
                        0,
                        format!("{subject} needs at least one {} file as evidence", needed.as_str()),
                    ));
                }
                let inputs = fingerprint::judged(project, loaded, scenario).unwrap_or_default();
                let fingerprints = evidence.iter().map(|file| of_bytes(&file.bytes)).collect();
                file.judged = Some(Recorded { inputs, verdict: Some(verdict), evidence: fingerprints });
            }
            let tables: Vec<String> = [("adequate", &file.adequate), ("judged", &file.judged)]
                .into_iter()
                .filter_map(|(name, entry)| entry.as_ref().map(|entry| table(name, entry)))
                .collect();
            Ok(Entry { path, bytes: tables.join("\n").into_bytes() })
        }
        Kind::Reference => {
            let section =
                project.sections.iter().find(|section| section.id == subject).ok_or_else(|| unknown_subject(subject, kind))?;
            let inputs = fingerprint::reference(project, section);
            let entry = Recorded { inputs, verdict: Some(verdict), evidence: Vec::new() };
            Ok(Entry { path: reference_path(project, subject), bytes: table(REFERENCE, &entry).into_bytes() })
        }
        Kind::Suite | Kind::Passes => Err(unknown_subject(subject, kind)),
    }
}
