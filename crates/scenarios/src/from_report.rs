//! Turns an `alondra report` package into the draft of a scenario: the tasks and what happened to
//! them as steps, the shape of the SDK host's frames as a transcript, and the list of what the
//! report cannot tell and a person completes. Nothing is written when the package or the draft
//! carries one of this Mac's private values, anything the transcript guard refuses, or anything
//! one of the checks the caller adds finds: the binary adds the report's own final check and the
//! repository's public data guard.

use alondra_agent_protocol::SdkEventKind;
use alondra_scenarios_transcript::guard::{self, DEMO_USER};
use alondra_scenarios_transcript::{Header, Line, Origin, Transcript};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write;
use std::path::Path;

/// The sections of a package the draft reads.
const SECTIONS: &[&str] = &["tasks.jsonl", "lifecycle.jsonl", "nodes.jsonl", "sdk-host.jsonl"];
const PSEUDONYMS: &str = "pseudonyms.json";
const SETTLE: &str = "30m";
const MAX_GAP_MINUTES: i64 = 240;

/// Events that happened to a task and that the report cannot say how to cause: what a person must
/// write as a step, in words.
const UNKNOWN_CAUSES: &[(&str, &str)] = &[
    ("ask", "the node asked a question"),
    ("answer", "the user answered a question"),
    ("decide", "a decision was settled"),
    ("tell", "the user sent the node a message"),
    ("accept", "the user accepted a delivery"),
    ("merge_observed", "a pull request merged"),
    ("pause", "the task was paused"),
    ("resume", "the task was resumed"),
    ("finish", "the node finished"),
    ("reopen", "the task was reopened"),
    ("block_delivery", "a delivery was held back"),
    ("hand_back", "the task was handed back to its node"),
    ("move_card", "a card was moved"),
];

#[derive(Debug, PartialEq, Eq)]
pub struct Draft {
    pub scenario_toml: String,
    pub transcripts: Vec<(String, String)>,
    pub gaps: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Leak {
    pub file: String,
    pub line: usize,
    pub rule: String,
}

/// A check every file of the package and of the draft passes before anything is written: it reads
/// the files by name and the values the report replaced, and says what it finds.
pub type Check<'a> = &'a dyn Fn(&[(&str, &str)], &[String]) -> Vec<Leak>;

impl std::fmt::Display for Leak {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.rule)
    }
}

#[derive(Debug)]
pub enum Refusal {
    Unreadable(String),
    Empty,
    Leaks(Vec<Leak>),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Unreadable(reason) => write!(f, "{reason}"),
            Refusal::Empty => write!(f, "the package has no tasks, lifecycle events or SDK host frames to draft from"),
            Refusal::Leaks(leaks) => {
                write!(f, "the package or its draft carries what must not be public; nothing was written:")?;
                for leak in leaks {
                    write!(f, "\n  {leak}")?;
                }
                Ok(())
            }
        }
    }
}

pub type Package = BTreeMap<String, String>;

pub fn read_package(dir: &Path) -> Result<(Package, Vec<String>), Refusal> {
    let mut package = Package::new();
    for name in SECTIONS {
        match std::fs::read_to_string(dir.join(name)) {
            Ok(text) => {
                package.insert((*name).to_owned(), text);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(Refusal::Unreadable(format!("{name}: {error}"))),
        }
    }
    let originals = std::fs::read_to_string(dir.join(PSEUDONYMS)).map(|text| pseudonym_originals(&text)).unwrap_or_default();
    Ok((package, originals))
}

/// The values the report replaced, from its table of pseudonyms: they must not show anywhere.
fn pseudonym_originals(table: &str) -> Vec<String> {
    let Ok(table) = serde_json::from_str::<Value>(table) else { return Vec::new() };
    table["by_value"]
        .as_object()
        .map(|values| values.keys().map(|value| value.trim().to_lowercase()).filter(|value| value.chars().count() >= 4 && !value.chars().all(|c| c.is_ascii_digit() || c == '.')).collect())
        .unwrap_or_default()
}

fn rows(package: &Package, section: &str) -> Vec<Value> {
    package.get(section).map(|text| text.lines().filter_map(|line| serde_json::from_str(line).ok()).collect()).unwrap_or_default()
}

fn minutes_between(from: &str, to: &str) -> Option<i64> {
    let seconds = |text: &str| -> Option<i64> {
        let (date, time) = text.trim_end_matches('Z').split_once('T')?;
        let mut date = date.split('-').map(|part| part.parse::<i64>().ok());
        let (year, month, day) = (date.next()??, date.next()??, date.next()??);
        let mut time = time.split(':').map(|part| part.split('.').next().and_then(|whole| whole.parse::<i64>().ok()));
        let (hour, minute, second) = (time.next()??, time.next()??, time.next()??);
        let days = (year - 1970) * 365 + (year - 1969) / 4 + [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334].get(usize::try_from(month - 1).ok()?)? + day - 1;
        Some(((days * 24 + hour) * 60 + minute) * 60 + second)
    };
    Some((seconds(to)? - seconds(from)?) / 60)
}

struct Task {
    alias: String,
    reported: String,
    state_changes: Vec<String>,
}

fn tasks_of(package: &Package) -> Vec<Task> {
    let mut seen = BTreeSet::new();
    let mut tasks = Vec::new();
    let mut add = |reported: &str, changes: Vec<String>| {
        if reported.is_empty() || !seen.insert(reported.to_owned()) {
            return;
        }
        tasks.push(Task { alias: format!("t{}", tasks.len() + 1), reported: reported.to_owned(), state_changes: changes });
    };
    for row in rows(package, "tasks.jsonl") {
        let changes = row["states_in_window"].as_array().into_iter().flatten().filter_map(|state| state["at"].as_str().map(str::to_owned)).collect();
        add(row["task"].as_str().unwrap_or_default(), changes);
    }
    for row in rows(package, "lifecycle.jsonl") {
        add(row["task"].as_str().unwrap_or_default(), Vec::new());
    }
    tasks
}

fn cause_of(event: &str) -> Option<&'static str> {
    UNKNOWN_CAUSES.iter().find(|(kind, _)| *kind == event).map(|(_, cause)| *cause)
}

/// The steps and the gaps of the tasks of the package.
fn steps_of(package: &Package, tasks: &[Task], toml: &mut String, gaps: &mut Vec<String>) -> i64 {
    let events = rows(package, "lifecycle.jsonl");
    let mut changes: Vec<(&str, &str)> = tasks.iter().flat_map(|task| task.state_changes.iter().map(|at| (at.as_str(), task.alias.as_str()))).collect();
    changes.sort();
    let mut total = 0;
    for task in tasks {
        let alias = &task.alias;
        let number = alias.trim_start_matches('t');
        if alias == "t1" {
            let _ = write!(toml, "\n[[step]]\ndo = \"request\"\ntext = \"«user text {number}»\"\ntask = \"{alias}\"\ntitle = \"«task title {number}»\"\n");
            gaps.push(format!("task {alias}: write what the user asked in place of the marker in `text`, and a title"));
        } else {
            let _ = write!(toml, "\n[[step]]\ndo = \"task\"\nid = \"{alias}\"\ntitle = \"«task title {number}»\"\n");
            gaps.push(format!("task {alias}: the report does not say which request it belongs to or whether it stands on another task's pull request; add `stack_on` if it does, and write its title"));
        }
        let own: Vec<&Value> = events.iter().filter(|event| event["task"] == task.reported.as_str()).collect();
        for event in own {
            let kind = event["event"].as_str().unwrap_or_default();
            let revision = event["revision"].as_i64().unwrap_or_default();
            if kind == "present" {
                let _ = write!(toml, "\n[[step]]\ndo = \"present\"\ntask = \"{alias}\"\nid = \"d{revision}\"\nprs = []\nbody = \"«model text {revision}»\"\n");
                gaps.push(format!("task {alias}, revision {revision}: it presented a result; list its pull requests in `prs`, open them with `github` and `register_pr` steps before it, and write the body"));
            } else if let Some(cause) = cause_of(kind) {
                gaps.push(format!("task {alias}, revision {revision}: {cause} ({kind}); write the step that causes it"));
            }
        }
    }
    let mut previous: Option<&str> = None;
    for (at, _) in &changes {
        if let Some(before) = previous {
            let minutes = minutes_between(before, at).unwrap_or(0).clamp(0, MAX_GAP_MINUTES);
            if minutes > 0 {
                let _ = write!(toml, "\n[[step]]\ndo = \"advance\"\nby = \"{minutes}m\"\n");
                total += minutes;
            }
        }
        previous = Some(at);
    }
    total
}

fn transcripts_of(package: &Package, scenario: &str, gaps: &mut Vec<String>) -> Vec<(String, String)> {
    let mut transcripts = Vec::new();
    for host in rows(package, "sdk-host.jsonl").iter().filter(|row| row["frames"].is_array()) {
        let number = transcripts.len() + 1;
        let version = host["protocol_versions"].as_array().and_then(|versions| versions.first()).and_then(Value::as_u64).and_then(|version| u16::try_from(version).ok()).unwrap_or(1);
        let mut lines = Vec::new();
        for frame in host["frames"].as_array().into_iter().flatten().filter(|frame| frame["type"] == "event") {
            match frame["observation"].as_str() {
                Some("sdk") => {
                    let kind = frame["detail"].as_str().filter(|kind| serde_json::from_value::<SdkEventKind>(json!(kind)).is_ok()).unwrap_or("message");
                    lines.push(Line::EmitInbound {
                        frame: json!({"protocol_version": version, "body": {"type": "event", "observation": {"type": "sdk", "kind": kind, "native": {}, "payload": {"text": format!("«model text {}»", lines.len() + 1)}, "failures": []}}}),
                    });
                }
                Some("process_exited") => lines.push(Line::RuntimeExit { code: frame["detail"]["exit_code"].as_i64().and_then(|code| i32::try_from(code).ok()) }),
                _ => {}
            }
        }
        if lines.is_empty() {
            continue;
        }
        let header = Header { provider: "claude".into(), protocol_version: version, sdk_version: "unknown".into(), model: "unknown".into(), captured_at: "unknown".into(), scenario: scenario.to_owned(), origin: Origin::PortedIncident };
        let name = format!("transcripts/host-{number}.ndjson");
        gaps.push(format!("{name}: only the kind and order of the frames the host wrote; the report keeps no content, so write what each said and which node step plays it"));
        transcripts.push((name, Transcript { header, lines }.render()));
    }
    transcripts
}

fn scan(file: &str, text: &str, literals: &[String]) -> Vec<Leak> {
    guard::scan_transcript(text, literals, &BTreeSet::new())
        .into_iter()
        .filter(|(_, rule)| *rule != "free-text" && *rule != "not-json")
        .map(|(line, rule)| Leak { file: file.to_owned(), line, rule: rule.to_owned() })
        .collect()
}

fn scan_toml(file: &str, text: &str, literals: &[String]) -> Vec<Leak> {
    text.lines()
        .enumerate()
        .flat_map(|(index, line)| guard::scan_line(line, literals).into_iter().map(move |rule| Leak { file: file.to_owned(), line: index + 1, rule: rule.to_owned() }))
        .collect()
}

/// The draft of a scenario named `name` from the package, or why nothing may be written.
pub fn draft(name: &str, package: &Package, originals: &[String], literals: &[String], checks: &[Check]) -> Result<Draft, Refusal> {
    let files: Vec<(&str, &str)> = package.iter().map(|(file, text)| (file.as_str(), text.as_str())).collect();
    let mut leaks: Vec<Leak> = checks.iter().flat_map(|check| check(&files, originals)).collect();
    let literals: Vec<String> = literals.iter().chain(originals).cloned().collect();
    leaks.extend(package.iter().flat_map(|(file, text)| scan(file, text, &literals)));
    if !leaks.is_empty() {
        return Err(Refusal::Leaks(leaks));
    }
    let tasks = tasks_of(package);
    let mut gaps = Vec::new();
    let mut steps = String::new();
    let spent = steps_of(package, &tasks, &mut steps, &mut gaps);
    let transcripts = transcripts_of(package, name, &mut gaps);
    if tasks.is_empty() && transcripts.is_empty() {
        return Err(Refusal::Empty);
    }
    let screens: Vec<String> = ["home".to_owned(), "tasks".to_owned()].into_iter().chain(tasks.iter().map(|task| format!("task:{}", task.alias))).collect();
    let mut scenario_toml = format!("name = \"{name}\"\nmax_virtual = \"{}m\"\n\n[world]\nrepository = \"demo/app\"\n{steps}", (spent + 60).max(60));
    let _ = write!(scenario_toml, "\n[[step]]\ndo = \"rest\"\nmax = \"{SETTLE}\"\n\n[[step]]\ndo = \"checkpoint\"\nname = \"end\"\nscreens = {}\npushes = true\n", json!(screens));
    gaps.push("checkpoint end: add the `expect` of what the report showed going wrong, then run `bazel run //crates/scenarios:accept -- <case>` and read what it wrote".into());
    gaps.push(format!("the demo world speaks as its user `{DEMO_USER}`; add a `[world]` account, clients and languages if the problem depends on them"));
    let mut gap_list = String::from("What the report cannot tell, for whoever writes the scenario to complete:\n");
    for gap in &gaps {
        let _ = writeln!(gap_list, "- {gap}");
    }
    leaks.extend(scan_toml("scenario.toml", &scenario_toml, &literals));
    leaks.extend(scan_toml("gaps.txt", &gap_list, &literals));
    for (file, text) in &transcripts {
        leaks.extend(scan(file, text, &literals));
    }
    let drafted: Vec<(&str, &str)> =
        [("scenario.toml", scenario_toml.as_str()), ("gaps.txt", gap_list.as_str())].into_iter().chain(transcripts.iter().map(|(file, text)| (file.as_str(), text.as_str()))).collect();
    leaks.extend(checks.iter().flat_map(|check| check(&drafted, originals)));
    if !leaks.is_empty() {
        return Err(Refusal::Leaks(leaks));
    }
    Ok(Draft { scenario_toml, transcripts, gaps: gap_list })
}

/// Writes the draft into `out`, a new folder under a `.scratch` one; never into the cases.
pub fn write(out: &Path, draft: &Draft) -> std::io::Result<()> {
    std::fs::create_dir_all(out.join("transcripts"))?;
    std::fs::write(out.join("scenario.toml"), &draft.scenario_toml)?;
    std::fs::write(out.join("gaps.txt"), &draft.gaps)?;
    for (name, text) in &draft.transcripts {
        std::fs::write(out.join(name), text)?;
    }
    Ok(())
}

/// Reads the package folder, drafts and writes into `out`; the folder is created only when the
/// draft is clean.
pub fn convert(package: &Path, name: &str, out: &Path, literals: &[String], checks: &[Check]) -> Result<Draft, Refusal> {
    let (files, originals) = read_package(package)?;
    let draft = draft(name, &files, &originals, literals, checks)?;
    write(out, &draft).map_err(|error| Refusal::Unreadable(error.to_string()))?;
    Ok(draft)
}

#[cfg(test)]
#[path = "from_report_tests.rs"]
mod tests;
