use std::collections::{BTreeMap, BTreeSet};

use crate::ident::split_subject;
use crate::ledger::{self, LedgerFile};
use crate::pattern::matches;
use crate::problem::{finish, key_line, problem, unreadable};
use crate::{
    Code, Component, Decision, Files, LoadedSpec, Marker, Problem, Project, Promise, Scenario, Section, Settings, Status,
    boundary, decision, feature, fingerprint, markers, reference, settings,
};

const BOUNDARY_FILE: &str = "boundary.toml";
const FEATURE_EXTENSION: &str = ".feature";
const MARKDOWN_EXTENSION: &str = ".md";

/// Reads each path through the caller at most once.
struct Source<'a> {
    files: &'a dyn Files,
    cache: BTreeMap<String, Option<Vec<u8>>>,
}

impl Source<'_> {
    fn read(&mut self, path: &str) -> Option<Vec<u8>> {
        if let Some(cached) = self.cache.get(path) {
            return cached.clone();
        }
        let bytes = self.files.read(path);
        self.cache.insert(path.to_string(), bytes.clone());
        bytes
    }

    fn hashes(&self) -> BTreeMap<String, String> {
        self.cache
            .iter()
            .filter_map(|(path, bytes)| bytes.as_ref().map(|bytes| (path.clone(), fingerprint::hex(bytes))))
            .collect()
    }
}

/// Where each listed file under the specs folder belongs.
#[derive(Default)]
struct Layout {
    boundaries: Vec<(String, String)>,
    features: Vec<(String, String)>,
    stray_features: Vec<String>,
    ledger: Vec<String>,
}

impl Layout {
    fn new(specs_dir: &str, listed: &[String]) -> Layout {
        let prefix = format!("{specs_dir}/");
        let ledger_prefix = format!("{}/", ledger::ledger_dir(specs_dir));
        let mut layout = Layout::default();
        for path in listed {
            let Some(rest) = path.strip_prefix(prefix.as_str()) else { continue };
            if path.starts_with(ledger_prefix.as_str()) {
                layout.ledger.push(path.clone());
                continue;
            }
            let segments: Vec<&str> = rest.split('/').collect();
            match segments.as_slice() {
                [folder, name] if *name == BOUNDARY_FILE => layout.boundaries.push((folder.to_string(), path.clone())),
                [folder, name] if name.ends_with(FEATURE_EXTENSION) => {
                    layout.features.push((folder.to_string(), path.clone()))
                }
                _ if rest.ends_with(FEATURE_EXTENSION) => layout.stray_features.push(path.clone()),
                _ => {}
            }
        }
        layout
    }
}

impl Project {
    fn empty(settings: Settings) -> Project {
        Project {
            settings,
            components: Vec::new(),
            specs: Vec::new(),
            decisions: Vec::new(),
            sections: Vec::new(),
            markers: Vec::new(),
            listed: Vec::new(),
            hashes: BTreeMap::new(),
            component_fingerprints: BTreeMap::new(),
            ledger: BTreeMap::new(),
            leftover: Vec::new(),
        }
    }

    /// The spec and the scenario named `<spec>/<scenario>`.
    pub(crate) fn scenario(&self, subject: &str) -> Option<(&LoadedSpec, &Scenario)> {
        let (spec_id, scenario_id) = subject.split_once('/')?;
        let loaded = self.specs.iter().find(|loaded| loaded.spec.id == spec_id)?;
        let scenario = loaded.spec.scenarios.iter().find(|scenario| scenario.id == scenario_id)?;
        Some((loaded, scenario))
    }

    pub(crate) fn component(&self, id: &str) -> Option<&Component> {
        self.components.iter().find(|component| component.id == id)
    }
}

fn scenario_component<'a>(specs: &'a [LoadedSpec], subject: &str) -> Option<&'a str> {
    let (spec_id, scenario_id) = subject.split_once('/')?;
    let loaded = specs.iter().find(|loaded| loaded.spec.id == spec_id)?;
    loaded.spec.scenarios.iter().any(|scenario| scenario.id == scenario_id).then_some(loaded.component.as_str())
}

pub(crate) fn load(files: &dyn Files) -> Result<Project, Vec<Problem>> {
    let mut source = Source { files, cache: BTreeMap::new() };
    let mut listed = files.paths();
    listed.sort();
    listed.dedup();
    let settings_bytes = source.read(settings::PATH);
    if settings_bytes.is_none() && listed.iter().any(|path| path == settings::PATH) {
        return Err(vec![unreadable(settings::PATH)]);
    }
    let settings = settings::read(settings_bytes.as_deref())?;
    if !settings.specs.enabled {
        return Ok(Project::empty(settings));
    }
    let layout = Layout::new(&settings.specs.dir, &listed);
    let mut problems = Vec::new();

    let boundary_folders: BTreeSet<String> = layout.boundaries.iter().map(|(folder, _)| folder.clone()).collect();
    let components = read_components(&mut source, &settings, &layout, &boundary_folders, &mut problems);

    for path in &layout.stray_features {
        problems.push(spec_without_component(path));
    }
    for (folder, path) in &layout.features {
        if !boundary_folders.contains(folder) {
            problems.push(spec_without_component(path));
        }
    }
    let (specs, decision_tags, failed_folders) = read_specs(&mut source, &layout, &boundary_folders, &mut problems);
    let any_spec_failed = !failed_folders.is_empty();
    let specs_kept_elsewhere =
        !settings.specs.committed && layout.features.is_empty() && layout.stray_features.is_empty();
    if !specs_kept_elsewhere {
        check_promises(&components, &specs, &failed_folders, &mut problems);
    }

    let (decisions, decision_stems) = read_decisions(&mut source, &settings, &listed, &mut problems);
    check_citations(&decisions, &decision_stems, &decision_tags, &mut problems);

    let known = Known {
        boundary_folders: &boundary_folders,
        decisions: &decisions,
        decision_stems: &decision_stems,
        specs: &specs,
        any_spec_failed,
    };
    let sections = read_sections(&mut source, &settings, &listed, &known, &mut problems);

    let skip_orphans = any_spec_failed || specs_kept_elsewhere;
    let found_markers = read_markers(&mut source, &components, &specs, &listed, skip_orphans, &mut problems);

    read_evidence(&mut source, &specs, &listed);
    read_component_files(&mut source, &components, &listed);

    let mut project = Project {
        settings,
        components,
        specs,
        decisions,
        sections,
        markers: found_markers,
        listed,
        hashes: source.hashes(),
        component_fingerprints: BTreeMap::new(),
        ledger: BTreeMap::new(),
        leftover: Vec::new(),
    };
    read_ledger(&mut source, &mut project, &layout.ledger, &mut problems);
    project.hashes = source.hashes();
    project.component_fingerprints = fingerprint::components(&project);
    finish(project, problems)
}

fn spec_without_component(path: &str) -> Problem {
    problem(
        Code::SpecWithoutComponent,
        path,
        0,
        "a feature file belongs to the component whose boundary.toml is in the same folder, and there is none",
    )
}

fn read_components(
    source: &mut Source<'_>,
    settings: &Settings,
    layout: &Layout,
    boundary_folders: &BTreeSet<String>,
    problems: &mut Vec<Problem>,
) -> Vec<Component> {
    let ledger_dir = ledger::ledger_dir(&settings.specs.dir);
    let mut components = Vec::new();
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    for (folder, path) in &layout.boundaries {
        let Some(bytes) = source.read(path) else {
            problems.push(unreadable(path));
            continue;
        };
        let (component, found) = boundary::read(path, folder, &bytes, &ledger_dir);
        problems.extend(found);
        if let Some(component) = component {
            texts.insert(component.id.clone(), String::from_utf8_lossy(&bytes).into_owned());
            components.push(component);
        }
    }
    components.sort_by(|a, b| a.id.cmp(&b.id));
    let depends_line = |id: &str| texts.get(id).map_or(0, |text| key_line(text, "depends_on"));
    for component in &components {
        for dependency in &component.depends_on {
            if !boundary_folders.contains(dependency) {
                problems.push(problem(
                    Code::UnknownComponent,
                    &component.path,
                    depends_line(&component.id),
                    format!("the component depends on {dependency:?}, which does not exist"),
                ));
            }
        }
    }
    check_cycles(&components, &depends_line, problems);
    components
}

fn check_cycles(components: &[Component], depends_line: &dyn Fn(&str) -> usize, problems: &mut Vec<Problem>) {
    let graph: BTreeMap<&str, Vec<&str>> = components
        .iter()
        .map(|component| (component.id.as_str(), component.depends_on.iter().map(String::as_str).collect()))
        .collect();
    let reach: BTreeMap<&str, BTreeSet<&str>> = graph.keys().map(|id| (*id, reachable(id, &graph))).collect();
    for component in components {
        let id = component.id.as_str();
        let Some(from) = reach.get(id) else { continue };
        if !from.contains(id) {
            continue;
        }
        let cycle: BTreeSet<&str> =
            from.iter().copied().filter(|other| reach.get(other).is_some_and(|back| back.contains(id))).collect();
        if cycle.first() == Some(&id) {
            let members: Vec<&str> = cycle.into_iter().collect();
            problems.push(problem(
                Code::DependencyCycle,
                &component.path,
                depends_line(id),
                format!("the components {} depend on each other", members.join(", ")),
            ));
        }
    }
}

fn reachable<'a>(start: &str, graph: &BTreeMap<&'a str, Vec<&'a str>>) -> BTreeSet<&'a str> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&'a str> = graph.get(start).cloned().unwrap_or_default();
    while let Some(next) = stack.pop() {
        if seen.insert(next) {
            if let Some(more) = graph.get(next) {
                stack.extend(more.iter().copied());
            }
        }
    }
    seen
}

type DecisionTag = (String, String, usize);

fn read_specs(
    source: &mut Source<'_>,
    layout: &Layout,
    boundary_folders: &BTreeSet<String>,
    problems: &mut Vec<Problem>,
) -> (Vec<LoadedSpec>, Vec<DecisionTag>, BTreeSet<String>) {
    let mut specs: Vec<LoadedSpec> = Vec::new();
    let mut tags = Vec::new();
    let mut failed = BTreeSet::new();
    let mut ids = BTreeSet::new();
    let mut features: Vec<&(String, String)> =
        layout.features.iter().filter(|(folder, _)| boundary_folders.contains(folder)).collect();
    features.sort_by(|a, b| a.1.cmp(&b.1));
    for (folder, path) in features {
        let Some(bytes) = source.read(path) else {
            problems.push(unreadable(path));
            failed.insert(folder.clone());
            continue;
        };
        match feature::read(path, &bytes) {
            Ok(read) => {
                if !ids.insert(read.spec.id.clone()) {
                    problems.push(problem(
                        Code::DuplicateSpecId,
                        path,
                        0,
                        format!("another feature file already is the spec {}", read.spec.id),
                    ));
                    continue;
                }
                for (id, line) in read.spec.decisions.iter().zip(read.decision_lines.iter()) {
                    tags.push((path.clone(), id.clone(), *line));
                }
                specs.push(LoadedSpec { path: path.clone(), component: folder.clone(), spec: read.spec });
            }
            Err(found) => {
                problems.extend(found);
                failed.insert(folder.clone());
            }
        }
    }
    (specs, tags, failed)
}

fn check_promises(
    components: &[Component],
    specs: &[LoadedSpec],
    failed_folders: &BTreeSet<String>,
    problems: &mut Vec<Problem>,
) {
    for component in components {
        if failed_folders.contains(&component.id) {
            continue;
        }
        let stated: Vec<Promise> = specs
            .iter()
            .filter(|loaded| loaded.component == component.id)
            .flat_map(|loaded| loaded.spec.scenarios.iter().filter_map(|scenario| scenario.promise))
            .collect();
        let mut needed = vec![Promise::Dependencies];
        if component.surface.is_some() {
            needed.push(Promise::Surface);
        }
        if !component.data.is_empty() {
            needed.push(Promise::Data);
        }
        for promise in needed {
            if !stated.contains(&promise) {
                problems.push(problem(
                    Code::BoundaryWithoutScenario,
                    &component.path,
                    0,
                    format!("no scenario of the component is tagged @boundary:{}", promise.as_str()),
                ));
            }
        }
    }
}

fn read_decisions(
    source: &mut Source<'_>,
    settings: &Settings,
    listed: &[String],
    problems: &mut Vec<Problem>,
) -> (Vec<Decision>, BTreeSet<String>) {
    let mut decisions: Vec<Decision> = Vec::new();
    let mut stems = BTreeSet::new();
    let mut texts: BTreeMap<String, String> = BTreeMap::new();
    if !settings.decisions.enabled {
        return (decisions, stems);
    }
    let prefix = format!("{}/", settings.decisions.dir);
    for path in listed {
        let Some(rest) = path.strip_prefix(prefix.as_str()) else { continue };
        if rest.contains('/') || !rest.ends_with(MARKDOWN_EXTENSION) {
            continue;
        }
        stems.insert(decision::stem(path).to_string());
        let Some(bytes) = source.read(path) else {
            problems.push(unreadable(path));
            continue;
        };
        let (found, found_problems) = decision::read(path, &bytes);
        problems.extend(found_problems);
        if let Some(found) = found {
            texts.insert(found.id.clone(), String::from_utf8_lossy(&bytes).into_owned());
            decisions.push(found);
        }
    }
    let line_of = |decision: &Decision, key: &str| texts.get(&decision.id).map_or(0, |text| key_line(text, key));
    for replacing in &decisions {
        for replaced in &replacing.supersedes {
            match decisions.iter().find(|candidate| candidate.id == *replaced) {
                Some(old) if old.status == Status::Superseded && old.superseded_by.as_deref() == Some(replacing.id.as_str()) => {}
                Some(old) => problems.push(problem(
                    Code::SupersessionMismatch,
                    &old.path,
                    0,
                    format!(
                        "{} supersedes {}, which must have status superseded and superseded_by {:?}",
                        replacing.id, old.id, replacing.id
                    ),
                )),
                None if stems.contains(replaced) => {}
                None => problems.push(problem(
                    Code::SupersessionMismatch,
                    &replacing.path,
                    line_of(replacing, "supersedes"),
                    format!("{} supersedes {replaced}, which does not exist", replacing.id),
                )),
            }
        }
    }
    for old in decisions.iter().filter(|decision| decision.status == Status::Superseded) {
        let named = match old.superseded_by.as_deref() {
            None => false,
            Some(by) => match decisions.iter().find(|candidate| candidate.id == by) {
                Some(replacing) => replacing.supersedes.iter().any(|replaced| *replaced == old.id),
                None => stems.contains(by) && by != old.id,
            },
        };
        if !named {
            problems.push(problem(
                Code::SupersessionMismatch,
                &old.path,
                line_of(old, "superseded_by"),
                format!(
                    "{} is superseded, so superseded_by must name a decision that lists {} in supersedes",
                    old.id, old.id
                ),
            ));
        }
    }
    (decisions, stems)
}

fn check_citations(
    decisions: &[Decision],
    stems: &BTreeSet<String>,
    tags: &[DecisionTag],
    problems: &mut Vec<Problem>,
) {
    for (path, id, line) in tags {
        match decisions.iter().find(|candidate| candidate.id == *id) {
            Some(found) if found.status == Status::Accepted => {}
            Some(found) => problems.push(problem(
                Code::DecisionNotAccepted,
                path,
                *line,
                format!("the decision {id} is {}, and a spec may only rest on an accepted one", found.status.as_str()),
            )),
            None if stems.contains(id) => {}
            None => problems.push(problem(Code::UnknownDecision, path, *line, format!("there is no decision {id}"))),
        }
    }
}

struct Known<'a> {
    boundary_folders: &'a BTreeSet<String>,
    decisions: &'a [Decision],
    decision_stems: &'a BTreeSet<String>,
    specs: &'a [LoadedSpec],
    any_spec_failed: bool,
}

impl Known<'_> {
    fn has(&self, target: &str) -> bool {
        if let Some(component) = target.strip_prefix("boundary:") {
            return self.boundary_folders.contains(component);
        }
        if let Some(id) = target.strip_prefix("decision:") {
            return self.decision_stems.contains(id) || self.decisions.iter().any(|found| found.id == id);
        }
        self.any_spec_failed || self.specs.iter().any(|loaded| loaded.spec.id == target)
    }
}

fn read_sections(
    source: &mut Source<'_>,
    settings: &Settings,
    listed: &[String],
    known: &Known<'_>,
    problems: &mut Vec<Problem>,
) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    if !settings.reference.enabled {
        return sections;
    }
    let prefix = format!("{}/", settings.reference.dir);
    let mut ids = BTreeSet::new();
    for path in listed {
        let Some(rest) = path.strip_prefix(prefix.as_str()) else { continue };
        if rest.contains('/') || !rest.ends_with(MARKDOWN_EXTENSION) {
            continue;
        }
        let Some(bytes) = source.read(path) else {
            problems.push(unreadable(path));
            continue;
        };
        let (found, found_problems) = reference::read(path, &bytes);
        problems.extend(found_problems);
        for read in found {
            for target in &read.section.explains {
                if !known.has(target) {
                    problems.push(problem(
                        Code::UnknownReferenceTarget,
                        path,
                        read.comment_line,
                        format!("the section explains {target}, which does not exist"),
                    ));
                }
            }
            if !ids.insert(read.section.id.clone()) {
                problems.push(problem(
                    Code::DuplicateSectionId,
                    path,
                    read.section.line,
                    format!("another section already has the id {}", read.section.id),
                ));
                continue;
            }
            sections.push(read.section);
        }
    }
    sections
}

fn read_markers(
    source: &mut Source<'_>,
    components: &[Component],
    specs: &[LoadedSpec],
    listed: &[String],
    skip_orphans: bool,
    problems: &mut Vec<Problem>,
) -> Vec<Marker> {
    let mut found = Vec::new();
    for path in listed {
        let owners: BTreeSet<&str> = components
            .iter()
            .filter(|component| component.suites.iter().any(|suite| suite.tests.iter().any(|pattern| matches(pattern, path))))
            .map(|component| component.id.as_str())
            .collect();
        if owners.is_empty() {
            continue;
        }
        let Some(bytes) = source.read(path) else {
            problems.push(unreadable(path));
            continue;
        };
        let Ok(content) = std::str::from_utf8(&bytes) else { continue };
        let (read, read_problems) = markers::read(path, content);
        problems.extend(read_problems);
        for marker in read {
            match scenario_component(specs, &marker.scenario) {
                Some(component) if !owners.contains(component) => problems.push(problem(
                    Code::MarkerOutsideComponent,
                    path,
                    marker.line,
                    format!("{} belongs to the component {component}, not to the suite of this file", marker.scenario),
                )),
                Some(_) => {}
                None if skip_orphans || split_subject(&marker.scenario).is_none() => {}
                None => problems.push(problem(
                    Code::OrphanMarker,
                    path,
                    marker.line,
                    format!("there is no scenario {}", marker.scenario),
                )),
            }
            found.push(marker);
        }
    }
    found.sort_by(|a: &Marker, b: &Marker| (&a.path, a.line, &a.scenario).cmp(&(&b.path, b.line, &b.scenario)));
    found
}

fn read_evidence(source: &mut Source<'_>, specs: &[LoadedSpec], listed: &[String]) {
    let patterns: Vec<&str> = specs
        .iter()
        .flat_map(|loaded| loaded.spec.scenarios.iter())
        .filter_map(|scenario| scenario.judgment.as_ref().and_then(|judgment| judgment.pattern.as_deref()))
        .collect();
    for path in listed {
        if patterns.iter().any(|pattern| matches(pattern, path)) {
            source.read(path);
        }
    }
}

fn read_component_files(source: &mut Source<'_>, components: &[Component], listed: &[String]) {
    for path in listed {
        let owned = components
            .iter()
            .any(|component| component.owns.iter().chain(component.inputs.iter()).any(|pattern| matches(pattern, path)));
        if owned {
            source.read(path);
        }
    }
}

fn read_ledger(source: &mut Source<'_>, project: &mut Project, paths: &[String], problems: &mut Vec<Problem>) {
    let dir = ledger::ledger_dir(&project.settings.specs.dir);
    let mut files: BTreeMap<String, LedgerFile> = BTreeMap::new();
    let mut leftover = Vec::new();
    for path in paths {
        let Some(subject) = ledger::classify(&dir, path) else {
            problems.push(problem(
                Code::InvalidLedger,
                path,
                0,
                "the file does not fit the layout of the ledger",
            ));
            continue;
        };
        let Some(bytes) = source.read(path) else {
            problems.push(unreadable(path));
            continue;
        };
        match ledger::read(path, &subject, &bytes) {
            Ok(file) if ledger::subject_exists(project, &subject) => {
                problems.extend(ledger::missing_evidence(project, path, &subject, &file));
                files.insert(path.clone(), file);
            }
            Ok(_) => leftover.push(path.clone()),
            Err(found) => problems.push(found),
        }
    }
    leftover.sort();
    project.ledger = files;
    project.leftover = leftover;
}
