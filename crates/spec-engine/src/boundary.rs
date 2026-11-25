use std::collections::BTreeSet;

use serde::Deserialize;

use crate::ident::is_identifier;
use crate::pattern::{is_inside, reaches_below};
use crate::problem::{TOML_MAX_BYTES, array_key_line, key_line, problem, toml_line, too_large, value_line};
use crate::{Code, Component, Problem, Suite};

const DEFAULT_TIMEOUT_MINUTES: u32 = 30;
const SUITE_TABLE: &str = "suite";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawBoundary {
    id: String,
    owns: Vec<String>,
    #[serde(default)]
    regenerates: Vec<String>,
    #[serde(default)]
    inputs: Vec<String>,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    data: Vec<String>,
    #[serde(default)]
    fixtures: Vec<String>,
    surface: Option<String>,
    #[serde(default)]
    suite: Vec<RawSuite>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSuite {
    id: String,
    run: Vec<String>,
    #[serde(default)]
    tests: Vec<String>,
    results: Option<String>,
    #[serde(default = "default_timeout")]
    timeout_minutes: u32,
}

fn default_timeout() -> u32 {
    DEFAULT_TIMEOUT_MINUTES
}

/// The component a boundary file declares, named after its folder, with the problems found in the
/// file alone. `None` when the file cannot be read as a boundary at all.
pub(crate) fn read(path: &str, folder: &str, bytes: &[u8], ledger_dir: &str) -> (Option<Component>, Vec<Problem>) {
    if let Some(found) = too_large(path, bytes, TOML_MAX_BYTES) {
        return (None, vec![found]);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return (None, vec![problem(Code::InvalidUtf8, path, 0, "the boundary is not valid UTF-8")]);
    };
    let raw: RawBoundary = match toml::from_str(source) {
        Ok(raw) => raw,
        Err(failure) => {
            let found = problem(Code::InvalidBoundary, path, toml_line(source, &failure), failure.message().to_string());
            return (None, vec![found]);
        }
    };
    let mut problems = Vec::new();
    if raw.id != folder {
        problems.push(problem(
            Code::ComponentIdMismatch,
            path,
            key_line(source, "id"),
            format!("the boundary says id {:?} but its folder is {folder:?}", raw.id),
        ));
    }
    let suites: Vec<Suite> = raw
        .suite
        .into_iter()
        .map(|suite| Suite {
            id: suite.id,
            run: suite.run,
            tests: suite.tests,
            results: suite.results,
            timeout_minutes: suite.timeout_minutes,
        })
        .collect();
    let component = Component {
        id: folder.to_string(),
        path: path.to_string(),
        owns: raw.owns,
        regenerates: raw.regenerates,
        inputs: raw.inputs,
        depends_on: raw.depends_on,
        data: raw.data,
        fixtures: raw.fixtures,
        surface: raw.surface,
        suites,
    };
    check_suites(&component, source, &mut problems);
    check_patterns(&component, source, &mut problems);
    check_ledger(&component, source, ledger_dir, &mut problems);
    check_regenerates(&component, source, &mut problems);
    (Some(component), problems)
}

fn check_suites(component: &Component, source: &str, problems: &mut Vec<Problem>) {
    let mut seen = BTreeSet::new();
    for (index, suite) in component.suites.iter().enumerate() {
        let id_line = array_key_line(source, SUITE_TABLE, index, "id");
        if !is_identifier(&suite.id) {
            problems.push(problem(
                Code::InvalidIdentifier,
                &component.path,
                id_line,
                format!("{:?} is not a valid suite identifier", suite.id),
            ));
        }
        if !seen.insert(suite.id.as_str()) {
            problems.push(problem(
                Code::DuplicateSuiteId,
                &component.path,
                id_line,
                format!("two suites have the id {:?}", suite.id),
            ));
        }
        if suite.run.is_empty() {
            problems.push(problem(
                Code::InvalidBoundary,
                &component.path,
                array_key_line(source, SUITE_TABLE, index, "run"),
                format!("the suite {:?} has nothing to run", suite.id),
            ));
        }
    }
}

fn check_patterns(component: &Component, source: &str, problems: &mut Vec<Problem>) {
    let lists = [&component.owns, &component.regenerates, &component.inputs, &component.data, &component.fixtures];
    let suite_patterns = component.suites.iter().flat_map(|suite| suite.tests.iter().chain(suite.results.iter()));
    let patterns = lists.into_iter().flatten().chain(component.surface.iter()).chain(suite_patterns);
    for pattern in patterns {
        if !is_inside(pattern) {
            problems.push(problem(
                Code::PathOutsideRepository,
                &component.path,
                value_line(source, pattern),
                format!("the pattern {pattern:?} is not a relative path inside the repository"),
            ));
        }
    }
}

fn check_ledger(component: &Component, source: &str, ledger_dir: &str, problems: &mut Vec<Problem>) {
    let reaches = component.owns.iter().chain(component.inputs.iter()).find(|pattern| reaches_below(pattern, ledger_dir));
    if let Some(pattern) = reaches {
        problems.push(problem(
            Code::LedgerInsideComponent,
            &component.path,
            value_line(source, pattern),
            format!("the pattern {pattern:?} reaches the ledger, which can never be part of a component"),
        ));
    }
}

fn check_regenerates(component: &Component, source: &str, problems: &mut Vec<Problem>) {
    for pattern in &component.regenerates {
        if !component.owns.iter().any(|owned| within(pattern, owned)) {
            problems.push(problem(
                Code::RegeneratesOutsideOwns,
                &component.path,
                value_line(source, pattern),
                format!("the component regenerates {pattern:?}, which it does not own"),
            ));
        }
    }
}

fn within(pattern: &str, owned: &str) -> bool {
    if pattern == owned {
        return true;
    }
    match owned.strip_suffix("**") {
        Some(prefix) if prefix.ends_with('/') && !prefix.contains('*') => pattern.starts_with(prefix),
        _ => false,
    }
}
