use serde::Serialize;

use crate::part::{self, of_scenario, of_spec};
use crate::{Component, Decision, LoadedSpec, Marker, PartContent, PartKind, Project, Scenario, Section, Settings};

const FORMAT: u32 = 1;

#[derive(Serialize)]
struct Listing<'a> {
    format: u32,
    settings: SettingsView<'a>,
    components: Vec<ComponentView<'a>>,
    specs: Vec<SpecView<'a>>,
    decisions: Vec<DecisionView<'a>>,
    sections: Vec<SectionView<'a>>,
    markers: Vec<MarkerView<'a>>,
}

#[derive(Serialize)]
struct SettingsView<'a> {
    specs: SpecsView<'a>,
    decisions: FolderView<'a>,
    reference: FolderView<'a>,
}

#[derive(Serialize)]
struct SpecsView<'a> {
    enabled: bool,
    committed: bool,
    dir: &'a str,
}

#[derive(Serialize)]
struct FolderView<'a> {
    enabled: bool,
    dir: &'a str,
}

#[derive(Serialize)]
struct ComponentView<'a> {
    id: &'a str,
    path: &'a str,
    owns: &'a [String],
    regenerates: &'a [String],
    inputs: &'a [String],
    depends_on: &'a [String],
    data: &'a [String],
    fixtures: &'a [String],
    surface: Option<&'a str>,
    suites: Vec<SuiteView<'a>>,
}

#[derive(Serialize)]
struct SuiteView<'a> {
    id: &'a str,
    run: &'a [String],
    tests: &'a [String],
    results: Option<&'a str>,
    timeout_minutes: u32,
}

#[derive(Serialize)]
struct SpecView<'a> {
    id: &'a str,
    path: &'a str,
    component: &'a str,
    title: &'a str,
    decisions: &'a [String],
    parts: Vec<PartView<'a>>,
    scenarios: Vec<ScenarioView<'a>>,
}

#[derive(Serialize)]
struct ScenarioView<'a> {
    id: &'a str,
    title: &'a str,
    line: usize,
    outline: bool,
    judged: Option<&'static str>,
    evidence: Option<&'a str>,
    boundary: Option<&'static str>,
    reserved: bool,
    parts: Vec<PartView<'a>>,
}

#[derive(Serialize)]
struct PartView<'a> {
    path: &'a str,
    kind: &'static str,
    fingerprint: &'a str,
    bytes: usize,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Serialize)]
struct DecisionView<'a> {
    id: &'a str,
    path: &'a str,
    title: &'a str,
    status: &'static str,
    date: &'a str,
    supersedes: &'a [String],
    superseded_by: Option<&'a str>,
}

#[derive(Serialize)]
struct SectionView<'a> {
    id: &'a str,
    path: &'a str,
    line: usize,
    heading: &'a str,
    explains: &'a [String],
}

#[derive(Serialize)]
struct MarkerView<'a> {
    path: &'a str,
    line: usize,
    scenario: &'a str,
    test: Option<&'a str>,
}

fn settings_view(settings: &Settings) -> SettingsView<'_> {
    SettingsView {
        specs: SpecsView { enabled: settings.specs.enabled, committed: settings.specs.committed, dir: &settings.specs.dir },
        decisions: FolderView { enabled: settings.decisions.enabled, dir: &settings.decisions.dir },
        reference: FolderView { enabled: settings.reference.enabled, dir: &settings.reference.dir },
    }
}

fn component_view(component: &Component) -> ComponentView<'_> {
    ComponentView {
        id: &component.id,
        path: &component.path,
        owns: &component.owns,
        regenerates: &component.regenerates,
        inputs: &component.inputs,
        depends_on: &component.depends_on,
        data: &component.data,
        fixtures: &component.fixtures,
        surface: component.surface.as_deref(),
        suites: component
            .suites
            .iter()
            .map(|suite| SuiteView {
                id: &suite.id,
                run: &suite.run,
                tests: &suite.tests,
                results: suite.results.as_deref(),
                timeout_minutes: suite.timeout_minutes,
            })
            .collect(),
    }
}

fn part_views<'a, 'f>(project: &'a Project, loaded: &LoadedSpec, cited: impl Iterator<Item = (PartKind, &'f str)>) -> Vec<PartView<'a>> {
    cited
        .filter_map(|(_, file)| project.part(&part::path_of(&loaded.path, file)))
        .map(|found| {
            let (width, height) = match found.content {
                PartContent::Mockup { width, height } => (Some(width), Some(height)),
                PartContent::Diagram { .. } => (None, None),
            };
            PartView { path: &found.path, kind: found.kind().as_str(), fingerprint: &found.fingerprint, bytes: found.bytes, width, height }
        })
        .collect()
}

fn scenario_view<'a>(project: &'a Project, loaded: &LoadedSpec, scenario: &'a Scenario) -> ScenarioView<'a> {
    ScenarioView {
        id: &scenario.id,
        title: &scenario.title,
        line: scenario.line,
        outline: scenario.outline,
        judged: scenario.judgment.as_ref().map(|judgment| judgment.evidence.as_str()),
        evidence: scenario.judgment.as_ref().and_then(|judgment| judgment.pattern.as_deref()),
        boundary: scenario.promise.map(|promise| promise.as_str()),
        reserved: scenario.reserved,
        parts: part_views(project, loaded, of_scenario(scenario)),
    }
}

fn spec_view<'a>(project: &'a Project, loaded: &'a LoadedSpec) -> SpecView<'a> {
    SpecView {
        id: &loaded.spec.id,
        path: &loaded.path,
        component: &loaded.component,
        title: &loaded.spec.title,
        decisions: &loaded.spec.decisions,
        parts: part_views(project, loaded, of_spec(&loaded.spec)),
        scenarios: loaded.spec.scenarios.iter().map(|scenario| scenario_view(project, loaded, scenario)).collect(),
    }
}

fn decision_view(decision: &Decision) -> DecisionView<'_> {
    DecisionView {
        id: &decision.id,
        path: &decision.path,
        title: &decision.title,
        status: decision.status.as_str(),
        date: &decision.date,
        supersedes: &decision.supersedes,
        superseded_by: decision.superseded_by.as_deref(),
    }
}

fn section_view(section: &Section) -> SectionView<'_> {
    SectionView {
        id: &section.id,
        path: &section.path,
        line: section.line,
        heading: &section.heading,
        explains: &section.explains,
    }
}

fn marker_view(marker: &Marker) -> MarkerView<'_> {
    MarkerView { path: &marker.path, line: marker.line, scenario: &marker.scenario, test: marker.test.as_deref() }
}

pub(crate) fn build(project: &Project) -> String {
    let listing = Listing {
        format: FORMAT,
        settings: settings_view(&project.settings),
        components: project.components.iter().map(component_view).collect(),
        specs: project.specs.iter().map(|loaded| spec_view(project, loaded)).collect(),
        decisions: project.decisions.iter().map(decision_view).collect(),
        sections: project.sections.iter().map(section_view).collect(),
        markers: project.markers.iter().map(marker_view).collect(),
    };
    serde_json::to_string_pretty(&listing).unwrap_or_default()
}
