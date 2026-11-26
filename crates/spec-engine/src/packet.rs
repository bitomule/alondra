use std::collections::BTreeSet;

use crate::feature::write;
use crate::ledger::ledger_dir;
use crate::part;
use crate::problem::problem;
use crate::{Code, Component, Decision, LoadedSpec, PartContent, Problem, Project, Section, Spec, Status};

const RULE: &str = "Specs are the only source of truth. Where any other text disagrees with a spec, the spec wins.";
const NONE: &str = "None.";

pub(crate) fn build(project: &Project, component: &str) -> Result<String, Problem> {
    let found = project
        .component(component)
        .ok_or_else(|| problem(Code::UnknownComponent, "", 0, format!("there is no component {component:?}")))?;
    let mut specs: Vec<&LoadedSpec> = project.specs.iter().filter(|loaded| loaded.component == found.id).collect();
    specs.sort_by(|a, b| a.spec.id.cmp(&b.spec.id));
    let cited: BTreeSet<&str> =
        specs.iter().flat_map(|loaded| loaded.spec.decisions.iter().map(String::as_str)).collect();
    let decisions: Vec<&Decision> = cited
        .iter()
        .filter_map(|id| project.decisions.iter().find(|decision| decision.id == *id))
        .filter(|decision| decision.status == Status::Accepted)
        .collect();
    let sections = explaining(project, found, &specs, &cited);

    let mut blocks = vec![format!("# Component {}", found.id), RULE.to_string()];
    blocks.push(section("What you may write", &may_write(found)));
    blocks.push(section("What you must not change", &must_not_change(project, found)));
    blocks.push(section("Boundary", &boundary(found)));
    blocks.push(section("Decisions", &decisions_text(&decisions)));
    blocks.push(section("Specs", &specs_text(project, &specs)));
    if !sections.is_empty() {
        let texts: Vec<&str> = sections.iter().map(|section| section.text.as_str()).collect();
        blocks.push(section("Reference (context, not a requirement)", &texts.join("\n\n")));
    }
    let mut packet = blocks.join("\n\n");
    packet.push('\n');
    Ok(packet)
}

fn section(title: &str, content: &str) -> String {
    let shown = if content.trim().is_empty() { NONE } else { content.trim_end() };
    format!("## {title}\n\n{shown}")
}

fn list(values: &[String]) -> String {
    if values.is_empty() { "none".to_string() } else { values.join(", ") }
}

fn may_write(component: &Component) -> String {
    component.regenerates.iter().map(|pattern| format!("- {pattern}")).collect::<Vec<_>>().join("\n")
}

fn must_not_change(project: &Project, component: &Component) -> String {
    let specs_dir = &project.settings.specs.dir;
    let tests: Vec<String> = component.suites.iter().flat_map(|suite| suite.tests.iter().cloned()).collect();
    let cites_parts = project.specs.iter().any(|loaded| {
        let spec = &loaded.spec;
        loaded.component == component.id
            && (part::of_spec(spec).next().is_some() || spec.scenarios.iter().any(|scenario| part::of_scenario(scenario).next().is_some()))
    });
    let parts = if cites_parts { " and the diagrams and mockups they cite" } else { "" };
    [
        format!("- the specs: {specs_dir}/{}/*.feature{parts}", component.id),
        format!("- the tests: {}", list(&tests)),
        format!("- the boundary: {}", component.path),
        format!("- the fixtures: {}", list(&component.fixtures)),
        format!("- the ledger: {}/**", ledger_dir(specs_dir)),
    ]
    .join("\n")
}

fn boundary(component: &Component) -> String {
    let mut lines = vec![
        format!("- id: {}", component.id),
        format!("- owns: {}", list(&component.owns)),
        format!("- regenerates: {}", list(&component.regenerates)),
        format!("- inputs: {}", list(&component.inputs)),
        format!("- depends_on: {}", list(&component.depends_on)),
        format!("- data: {}", list(&component.data)),
        format!("- fixtures: {}", list(&component.fixtures)),
        format!("- surface: {}", component.surface.as_deref().unwrap_or("none")),
    ];
    for suite in &component.suites {
        lines.push(format!("- suite {}: {} ({} minutes)", suite.id, suite.run.join(" "), suite.timeout_minutes));
    }
    lines.join("\n")
}

fn decisions_text(decisions: &[&Decision]) -> String {
    decisions
        .iter()
        .map(|decision| format!("### {}\n\n{}", decision.title, decision.decision))
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn without_reserved(spec: &Spec) -> Spec {
    let mut kept = spec.clone();
    kept.scenarios.retain(|scenario| !scenario.reserved);
    kept
}

fn specs_text(project: &Project, specs: &[&LoadedSpec]) -> String {
    specs
        .iter()
        .map(|loaded| {
            let spec = without_reserved(&loaded.spec);
            let mut blocks = vec![format!("```gherkin\n{}```", write(&spec))];
            blocks.extend(parts_text(project, loaded, &spec));
            blocks.join("\n\n")
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Each part the spec of a packet cites, once: the text of a diagram and the path of a mockup.
fn parts_text(project: &Project, loaded: &LoadedSpec, spec: &Spec) -> Vec<String> {
    let mut seen = BTreeSet::new();
    part::of_spec(spec)
        .chain(spec.scenarios.iter().flat_map(part::of_scenario))
        .filter_map(|(_, file)| project.part(&part::path_of(&loaded.path, file)))
        .filter(|found| seen.insert(found.path.as_str()))
        .map(|found| match &found.content {
            PartContent::Diagram { text } => {
                let fence = fence_around(text);
                format!("Diagram {}:\n\n{fence}mermaid\n{}\n{fence}", found.path, text.trim_end())
            }
            PartContent::Mockup { width, height } => format!("Mockup {} ({width}×{height} pixels)", found.path),
        })
        .collect()
}

/// A fence longer than any run of backticks in the text, so the text cannot close it.
fn fence_around(text: &str) -> String {
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    "`".repeat(longest.max(2) + 1)
}

fn explaining<'a>(
    project: &'a Project,
    component: &Component,
    specs: &[&LoadedSpec],
    cited: &BTreeSet<&str>,
) -> Vec<&'a Section> {
    let mut targets: BTreeSet<String> = specs.iter().map(|loaded| loaded.spec.id.clone()).collect();
    targets.insert(format!("boundary:{}", component.id));
    for id in cited {
        targets.insert(format!("decision:{id}"));
    }
    let mut found: Vec<&Section> =
        project.sections.iter().filter(|section| section.explains.iter().any(|target| targets.contains(target))).collect();
    found.sort_by(|a, b| a.id.cmp(&b.id));
    found
}
