use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use sha2::{Digest, Sha256};

use crate::feature::{write, write_scenario, write_steps};
use crate::part;
use crate::pattern::matches;
use crate::{Component, Kind, LoadedSpec, PartKind, Project, Scenario, Section, Suite};

const PREFIX: &str = "sha256:";
const ABSENT: &str = "absent";

pub(crate) fn hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

pub(crate) fn of_bytes(bytes: &[u8]) -> String {
    format!("{PREFIX}{}", hex(bytes))
}

pub(crate) fn is_fingerprint(value: &str) -> bool {
    match value.split_once(':') {
        Some(("sha256", digits)) => digits.len() == 64 && digits.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')),
        _ => false,
    }
}

/// The text a fingerprint is the hash of: a first line naming its kind, then one line per input,
/// each value written as a JSON string so that no value can run into the next.
struct Lines {
    content: String,
}

impl Lines {
    fn new(kind: &str) -> Lines {
        Lines { content: format!("{kind}\n") }
    }

    fn push(&mut self, parts: &[&str]) {
        let quoted: Vec<String> = parts.iter().map(|part| serde_json::to_string(part).unwrap_or_default()).collect();
        self.content.push_str(&quoted.join(" "));
        self.content.push('\n');
    }

    fn finish(self) -> String {
        of_bytes(self.content.as_bytes())
    }
}

fn hash<'a>(project: &'a Project, path: &str) -> &'a str {
    project.hashes.get(path).map_or(ABSENT, String::as_str)
}

pub(crate) fn components(project: &Project) -> BTreeMap<String, String> {
    let mut memo = BTreeMap::new();
    for component in &project.components {
        let mut stack = Vec::new();
        component_fingerprint(project, &component.id, &mut memo, &mut stack);
    }
    memo
}

fn component_fingerprint(
    project: &Project,
    id: &str,
    memo: &mut BTreeMap<String, String>,
    stack: &mut Vec<String>,
) -> String {
    if let Some(done) = memo.get(id) {
        return done.clone();
    }
    let Some(component) = project.components.iter().find(|component| component.id == id) else {
        return String::new();
    };
    if stack.iter().any(|open| open == id) {
        return String::new();
    }
    stack.push(id.to_string());
    let mut lines = Lines::new("component");
    for path in &project.listed {
        if component.owns.iter().chain(component.inputs.iter()).any(|pattern| matches(pattern, path)) {
            lines.push(&["file", path.as_str(), hash(project, path)]);
        }
    }
    let dependencies: BTreeSet<&String> = component.depends_on.iter().collect();
    for dependency in dependencies {
        let fingerprint = component_fingerprint(project, dependency, memo, stack);
        lines.push(&["depends", dependency.as_str(), fingerprint.as_str()]);
    }
    stack.pop();
    let fingerprint = lines.finish();
    memo.insert(id.to_string(), fingerprint.clone());
    fingerprint
}

fn component_of<'a>(project: &'a Project, id: &str) -> &'a str {
    project.component_fingerprints.get(id).map_or("", String::as_str)
}

pub(crate) fn suite(project: &Project, component: &Component, suite: &Suite) -> String {
    let mut lines = Lines::new("suite");
    lines.push(&["component", component.id.as_str(), component_of(project, &component.id)]);
    lines.push(&["id", suite.id.as_str()]);
    for argument in &suite.run {
        lines.push(&["argument", argument.as_str()]);
    }
    for pattern in &suite.tests {
        lines.push(&["tests", pattern.as_str()]);
    }
    for path in project.listed.iter().filter(|path| suite.tests.iter().any(|pattern| matches(pattern, path))) {
        lines.push(&["test", path.as_str(), hash(project, path)]);
    }
    lines.finish()
}

fn push_parts<'a>(lines: &mut Lines, label: &str, project: &Project, loaded: &LoadedSpec, parts: impl Iterator<Item = (PartKind, &'a str)>) {
    for (_, file) in parts {
        let path = part::path_of(&loaded.path, file);
        lines.push(&[label, path.as_str(), hash(project, &path)]);
    }
}

pub(crate) fn adequate(project: &Project, loaded: &LoadedSpec, scenario: &Scenario) -> String {
    let subject = format!("{}/{}", loaded.spec.id, scenario.id);
    let mut lines = Lines::new("adequate");
    lines.push(&["scenario", write_scenario(scenario).as_str()]);
    lines.push(&["title", loaded.spec.title.as_str()]);
    for line in &loaded.spec.description {
        lines.push(&["description", line.as_str()]);
    }
    lines.push(&["background", write_steps(&loaded.spec.background).as_str()]);
    push_parts(&mut lines, "header-part", project, loaded, part::of_spec(&loaded.spec));
    push_parts(&mut lines, "part", project, loaded, part::of_scenario(scenario));
    let tests: BTreeSet<&str> =
        project.markers.iter().filter(|marker| marker.scenario == subject).map(|marker| marker.path.as_str()).collect();
    for path in tests {
        lines.push(&["test", path, hash(project, path)]);
    }
    lines.finish()
}

pub(crate) fn judged(project: &Project, loaded: &LoadedSpec, scenario: &Scenario) -> Option<String> {
    let judgment = scenario.judgment.as_ref()?;
    let mut lines = Lines::new("judged");
    lines.push(&["scenario", write_scenario(scenario).as_str()]);
    match &judgment.pattern {
        Some(pattern) => {
            for path in project.listed.iter().filter(|path| matches(pattern, path)) {
                lines.push(&["evidence", path.as_str(), hash(project, path)]);
            }
        }
        None => lines.push(&["component", loaded.component.as_str(), component_of(project, &loaded.component)]),
    }
    push_parts(&mut lines, "part", project, loaded, part::of_scenario(scenario));
    Some(lines.finish())
}

pub(crate) fn reference(project: &Project, section: &Section) -> String {
    let mut lines = Lines::new("reference");
    lines.push(&["text", section.text.as_str()]);
    let targets: BTreeSet<&String> = section.explains.iter().collect();
    for target in targets {
        if let Some(id) = target.strip_prefix("boundary:") {
            let path = project.components.iter().find(|component| component.id == id).map_or("", |component| component.path.as_str());
            lines.push(&["boundary", id, hash(project, path)]);
        } else if let Some(id) = target.strip_prefix("decision:") {
            let path = project.decisions.iter().find(|decision| decision.id == id).map_or("", |decision| decision.path.as_str());
            lines.push(&["decision", id, hash(project, path)]);
        } else {
            let canonical = project.specs.iter().find(|loaded| loaded.spec.id == **target).map(|loaded| write(&loaded.spec));
            lines.push(&["spec", target.as_str(), canonical.as_deref().unwrap_or("")]);
        }
    }
    lines.finish()
}

pub(crate) fn of(project: &Project, subject: &str, kind: Kind) -> Option<String> {
    match kind {
        Kind::Suite => {
            let (component_id, suite_id) = subject.split_once('/')?;
            let component = project.components.iter().find(|component| component.id == component_id)?;
            let found = component.suites.iter().find(|suite| suite.id == suite_id)?;
            Some(suite(project, component, found))
        }
        Kind::Adequate => {
            let (loaded, scenario) = project.scenario(subject)?;
            Some(adequate(project, loaded, scenario))
        }
        Kind::Judged => {
            let (loaded, scenario) = project.scenario(subject)?;
            judged(project, loaded, scenario)
        }
        Kind::Reference => {
            let section = project.sections.iter().find(|section| section.id == subject)?;
            Some(reference(project, section))
        }
        Kind::Passes => None,
    }
}
