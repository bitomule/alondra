use std::collections::BTreeMap;

use serde::Serialize;

use crate::fingerprint;
use crate::ledger::lookup;
use crate::pattern::matches;
use crate::{Item, Kind, Marker, Project, Recorded, Report, ReportStatus, State, TestResults, Verdict};

const FORMAT: u32 = 1;

fn recorded_state(recorded: Option<&Recorded>, current: &str) -> State {
    match recorded {
        None => State::Missing,
        Some(entry) if entry.inputs != current => State::Stale,
        Some(entry) if entry.verdict == Some(Verdict::Unmet) => State::Unmet,
        Some(_) => State::Valid,
    }
}

fn severity(state: State) -> u8 {
    match state {
        State::Valid => 0,
        State::Stale => 1,
        State::Missing => 2,
        State::Unmet | State::Failed | State::Uncovered => 3,
    }
}

pub(crate) fn build(project: &Project, results: Option<&TestResults>) -> Report {
    if !project.settings.specs.enabled {
        return Report { status: ReportStatus::Clean, items: Vec::new(), leftover: Vec::new(), problems: Vec::new() };
    }
    let mut items = Vec::new();
    let mut suite_states: BTreeMap<(String, String), State> = BTreeMap::new();
    for component in &project.components {
        for suite in &component.suites {
            let subject = format!("{}/{}", component.id, suite.id);
            let current = fingerprint::suite(project, component, suite);
            let state = recorded_state(lookup(project, &subject, Kind::Suite), &current);
            suite_states.insert((component.id.clone(), suite.id.clone()), state);
            items.push(Item { subject, component: component.id.clone(), kind: Kind::Suite, state });
        }
    }
    for loaded in &project.specs {
        for scenario in &loaded.spec.scenarios {
            let subject = format!("{}/{}", loaded.spec.id, scenario.id);
            let declaring: Vec<&Marker> = project.markers.iter().filter(|marker| marker.scenario == subject).collect();
            let item = |kind: Kind, state: State| Item {
                subject: subject.clone(),
                component: loaded.component.clone(),
                kind,
                state,
            };
            if declaring.is_empty() {
                if scenario.judgment.is_none() {
                    items.push(item(Kind::Passes, State::Uncovered));
                }
            } else {
                items.push(item(Kind::Passes, passes(project, &declaring, &suite_states, results)));
                let current = fingerprint::adequate(project, loaded, scenario);
                items.push(item(Kind::Adequate, recorded_state(lookup(project, &subject, Kind::Adequate), &current)));
            }
            if let Some(current) = fingerprint::judged(project, loaded, scenario) {
                items.push(item(Kind::Judged, recorded_state(lookup(project, &subject, Kind::Judged), &current)));
            }
        }
    }
    for section in &project.sections {
        let current = fingerprint::reference(project, section);
        let state = recorded_state(lookup(project, &section.id, Kind::Reference), &current);
        items.push(Item { subject: section.id.clone(), component: String::new(), kind: Kind::Reference, state });
    }
    items.sort_by(|a, b| (a.subject.as_str(), a.kind.as_str()).cmp(&(b.subject.as_str(), b.kind.as_str())));
    let status =
        if items.iter().all(|item| item.state == State::Valid) { ReportStatus::Clean } else { ReportStatus::Pending };
    Report { status, items, leftover: project.leftover.clone(), problems: Vec::new() }
}

fn passes(
    project: &Project,
    declaring: &[&Marker],
    suite_states: &BTreeMap<(String, String), State>,
    results: Option<&TestResults>,
) -> State {
    let mut worst: Option<State> = None;
    for component in &project.components {
        for suite in &component.suites {
            let holds = declaring.iter().any(|marker| suite.tests.iter().any(|pattern| matches(pattern, &marker.path)));
            if !holds {
                continue;
            }
            let state = suite_states.get(&(component.id.clone(), suite.id.clone())).copied().unwrap_or(State::Missing);
            worst = Some(match worst {
                Some(previous) if severity(previous) >= severity(state) => previous,
                _ => state,
            });
        }
    }
    let state = worst.unwrap_or(State::Missing);
    if state != State::Valid {
        return state;
    }
    let failed = results.is_some_and(|results| {
        declaring.iter().any(|marker| marker.test.as_deref().is_none_or(|test| !results.passed(test)))
    });
    if failed { State::Failed } else { State::Valid }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    format: u32,
    status: &'static str,
    items: Vec<JsonItem<'a>>,
    leftover: &'a [String],
    problems: Vec<JsonProblem<'a>>,
}

#[derive(Serialize)]
struct JsonItem<'a> {
    subject: &'a str,
    component: &'a str,
    kind: &'static str,
    state: &'static str,
}

#[derive(Serialize)]
pub(crate) struct JsonProblem<'a> {
    code: &'static str,
    path: &'a str,
    line: usize,
    message: &'a str,
}

pub(crate) fn to_json(report: &Report) -> String {
    let json = JsonReport {
        format: FORMAT,
        status: report.status.as_str(),
        items: report
            .items
            .iter()
            .map(|item| JsonItem {
                subject: &item.subject,
                component: &item.component,
                kind: item.kind.as_str(),
                state: item.state.as_str(),
            })
            .collect(),
        leftover: &report.leftover,
        problems: report
            .problems
            .iter()
            .map(|found| JsonProblem { code: found.code.as_str(), path: &found.path, line: found.line, message: &found.message })
            .collect(),
    };
    serde_json::to_string_pretty(&json).unwrap_or_default()
}
