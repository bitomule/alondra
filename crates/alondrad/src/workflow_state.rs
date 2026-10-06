//! Where a running workflow has got, read from the files Claude Code writes for it. While it runs,
//! `<session>/subagents/workflows/<runId>/` holds `journal.jsonl` (one line as each agent starts
//! and returns) and `agent-<id>.meta.json` per agent; once it ends, `<session>/workflows/<runId>.json`
//! sums it up. The script under `<session>/workflows/scripts/` lists the phases from the start.
//! Every read is bounded and a half-written or unknown file reads as nothing.
use alondra_core::views::{LiveWorkflow, LiveWorkflowAgent, WORKFLOW_AGENT_ROWS};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

const JOURNAL_BYTES: u64 = 2 * 1024 * 1024;
const SUMMARY_BYTES: u64 = 4 * 1024 * 1024;
const SMALL_BYTES: u64 = 64 * 1024;
const TRANSCRIPT_BYTES: u64 = 512 * 1024;
const CACHE_LIMIT: usize = 64;
const ASSISTANT: &[u8] = b"\"type\":\"assistant\"";

#[derive(Deserialize)]
struct JournalLine {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, rename = "agentId")]
    agent_id: Option<String>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    phase: Option<String>,
}

#[derive(Deserialize)]
struct AgentMeta {
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct Summary {
    #[serde(default)]
    phases: Vec<SummaryPhase>,
    #[serde(default, rename = "workflowProgress")]
    progress: Vec<SummaryProgress>,
    #[serde(default, rename = "durationMs")]
    duration_ms: Option<u64>,
    #[serde(default, rename = "totalTokens")]
    total_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct SummaryPhase {
    title: String,
}

#[derive(Deserialize)]
struct SummaryProgress {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    label: Option<String>,
    #[serde(default, rename = "phaseTitle")]
    phase: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default, rename = "startedAt")]
    started_at: Option<i64>,
}

struct Started {
    id: String,
    label: String,
    phase: Option<String>,
    running: bool,
}

type Stamp = Vec<Option<(SystemTime, u64)>>;

static CACHE: Mutex<Option<HashMap<PathBuf, (Stamp, Option<LiveWorkflow>)>>> = Mutex::new(None);

/// The run's `<session>` folder and id, when the transcript folder has the shape Claude Code gives it.
fn session_and_run(dir: &Path) -> Option<(&Path, &str)> {
    let run = dir.file_name()?.to_str()?;
    let workflows = dir.parent()?;
    let subagents = workflows.parent()?;
    (workflows.file_name()? == "workflows" && subagents.file_name()? == "subagents").then_some(())?;
    Some((subagents.parent()?, run))
}

fn stamp_of(path: &Path) -> Option<(SystemTime, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    Some((meta.modified().ok()?, meta.len()))
}

fn summary_path(dir: &Path) -> Option<PathBuf> {
    let (session, run) = session_and_run(dir)?;
    Some(session.join("workflows").join(format!("{run}.json")))
}

/// The workflow as its files tell it now, read again only once one of them changed.
pub(crate) fn read(dir: &Path, finished: bool) -> Option<LiveWorkflow> {
    let stamp: Stamp = vec![stamp_of(dir), stamp_of(&dir.join("journal.jsonl")), summary_path(dir).and_then(|path| stamp_of(&path))];
    stamp[0]?;
    let key = dir.join(if finished { "finished" } else { "running" });
    if let Some((known, workflow)) = CACHE.lock().expect("workflow cache poisoned").as_ref().and_then(|cache| cache.get(&key)) {
        if *known == stamp { return workflow.clone(); }
    }
    let workflow = read_uncached(dir, finished);
    let mut cache = CACHE.lock().expect("workflow cache poisoned");
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() >= CACHE_LIMIT && !cache.contains_key(&key) { cache.clear(); }
    cache.insert(key, (stamp, workflow.clone()));
    workflow
}

fn read_uncached(dir: &Path, finished: bool) -> Option<LiveWorkflow> {
    let phases = session_and_run(dir).and_then(|(session, run)| script_phases(session, run));
    if let Some(summary) = summary_path(dir).and_then(|path| read_head(&path, SUMMARY_BYTES))
        .and_then(|bytes| serde_json::from_slice::<Summary>(&bytes).ok()) {
        let phases = if summary.phases.is_empty() { phases } else { Some(summary.phases.into_iter().map(|phase| phase.title).collect()) };
        let mut agents: Vec<(Option<i64>, Started, Option<String>)> = summary.progress.into_iter()
            .filter(|entry| entry.kind == "workflow_agent")
            .map(|entry| (entry.started_at, Started {
                id: String::new(),
                label: entry.label.unwrap_or_default(),
                phase: entry.phase,
                running: !finished && matches!(entry.state.as_deref(), Some("running" | "queued")),
            }, entry.model))
            .collect();
        agents.sort_by_key(|(started_at, ..)| *started_at);
        let (started, models): (Vec<_>, Vec<_>) = agents.into_iter().map(|(_, started, model)| (started, model)).unzip();
        let mut workflow = summarise(&started, phases.as_deref(), |index| models[index].clone());
        workflow.duration_ms = summary.duration_ms;
        workflow.total_tokens = summary.total_tokens;
        return Some(workflow);
    }
    let journal = read_tail(&dir.join("journal.jsonl"), JOURNAL_BYTES)?;
    let mut started: Vec<Started> = vec![];
    for line in journal.split(|byte| *byte == b'\n') {
        let Ok(line) = serde_json::from_slice::<JournalLine>(line) else { continue };
        let Some(id) = line.agent_id else { continue };
        match line.kind.as_str() {
            "started" => {
                started.retain(|agent| agent.id != id);
                started.push(Started { label: line.label.unwrap_or_else(|| id.clone()), id, phase: line.phase, running: !finished });
            }
            "result" => {
                if let Some(agent) = started.iter_mut().find(|agent| agent.id == id) { agent.running = false; }
            }
            _ => {}
        }
    }
    if started.is_empty() { return None; }
    Some(summarise(&started, phases.as_deref(), |index| agent_model(dir, &started[index].id)))
}

fn summarise(started: &[Started], phases: Option<&[String]>, model: impl Fn(usize) -> Option<String>) -> LiveWorkflow {
    let current = started.iter().rev().find(|agent| agent.running).or(started.last());
    let phase = current.and_then(|agent| agent.phase.clone());
    let phase_index = phase.as_ref().zip(phases)
        .and_then(|(phase, phases)| phases.iter().position(|title| title == phase))
        .map(|index| index as u32 + 1);
    let running = started.iter().filter(|agent| agent.running).count() as u32;
    LiveWorkflow {
        phase,
        phase_index,
        phase_count: phase_index.and(phases.map(|phases| phases.len() as u32)),
        running,
        done: started.len() as u32 - running,
        agents: started.iter().enumerate().rev().take(WORKFLOW_AGENT_ROWS).map(|(index, agent)| LiveWorkflowAgent {
            label: agent.label.clone(),
            phase: agent.phase.clone(),
            model: model(index),
            running: agent.running,
        }).collect(),
        duration_ms: None,
        total_tokens: None,
    }
}

/// The model the script asked for, else the one the agent's first answer ran on.
fn agent_model(dir: &Path, id: &str) -> Option<String> {
    let meta = read_head(&dir.join(format!("agent-{id}.meta.json")), SMALL_BYTES)
        .and_then(|bytes| serde_json::from_slice::<AgentMeta>(&bytes).ok())
        .and_then(|meta| meta.model)
        .filter(|model| !model.is_empty());
    meta.or_else(|| transcript_model(&dir.join(format!("agent-{id}.jsonl"))))
}

fn transcript_model(path: &Path) -> Option<String> {
    let bytes = read_head(path, TRANSCRIPT_BYTES)?;
    bytes.split(|byte| *byte == b'\n')
        .filter(|line| line.windows(ASSISTANT.len()).any(|window| window == ASSISTANT))
        .find_map(|line| {
            let value: serde_json::Value = serde_json::from_slice(line).ok()?;
            (value.get("type")?.as_str()? == "assistant").then_some(())?;
            value.pointer("/message/model")?.as_str().map(str::to_owned)
        })
}

/// The titles of `meta.phases` in the run's script, when they are written as plain literals.
fn script_phases(session: &Path, run: &str) -> Option<Vec<String>> {
    let suffix = format!("-{run}.js");
    let script = std::fs::read_dir(session.join("workflows").join("scripts")).ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.ends_with(&suffix)))?;
    let text = String::from_utf8(read_head(&script, SMALL_BYTES)?).ok()?;
    phase_titles(&text)
}

fn phase_titles(script: &str) -> Option<Vec<String>> {
    let meta = &script[script.find("meta")?..];
    let list = &meta[meta.find("phases")?..];
    let list = &list[list.find('[')? + 1..];
    let mut titles = vec![];
    let mut depth = 0usize;
    let mut chars = list.char_indices().peekable();
    while let Some((at, char)) = chars.next() {
        match char {
            '[' | '{' => depth += 1,
            '}' => depth = depth.checked_sub(1)?,
            ']' if depth == 0 => return (!titles.is_empty()).then_some(titles),
            ']' => depth -= 1,
            '\'' | '"' | '`' => {
                let end = list[at + 1..].find(char)? + at + 1;
                while chars.peek().is_some_and(|(next, _)| *next <= end) { chars.next(); }
            }
            't' if depth == 1 && list[at..].starts_with("title")
                && !list[..at].ends_with(|before: char| before.is_alphanumeric() || before == '_') => {
                let rest = list[at + 5..].trim_start().strip_prefix(':')?.trim_start();
                let quote = rest.chars().next().filter(|quote| matches!(quote, '\'' | '"' | '`'))?;
                let title = &rest[1..rest[1..].find(quote)? + 1];
                titles.push(title.to_owned());
            }
            _ => {}
        }
    }
    None
}

fn read_head(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut bytes = vec![];
    std::fs::File::open(path).ok()?.take(limit).read_to_end(&mut bytes).ok()?;
    Some(bytes)
}

/// The last `limit` bytes, starting at a whole line.
fn read_tail(path: &Path, limit: u64) -> Option<Vec<u8>> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(limit);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = vec![];
    file.take(limit).read_to_end(&mut bytes).ok()?;
    if start > 0 {
        let first = bytes.iter().position(|byte| *byte == b'\n').map_or(bytes.len(), |at| at + 1);
        bytes.drain(..first);
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUN: &str = "wf_3337df55-81d";
    const A: &str = "a15e40322473ae6d9";
    const B: &str = "ad315649bfc4c9064";
    const C: &str = "a25a90245f5242e1d";
    const JOURNAL: &str = include_str!("../../../fixtures/workflow/journal.jsonl");
    const SUMMARY: &str = include_str!("../../../fixtures/workflow/wf_3337df55-81d.json");
    const SCRIPT: &str = include_str!("../../../fixtures/workflow/state-file-probe-wf_3337df55-81d.js");
    const TRANSCRIPT: &str = include_str!("../../../fixtures/workflow/agent-a15e40322473ae6d9.jsonl");

    fn meta(id: &str) -> &'static str {
        match id {
            A => include_str!("../../../fixtures/workflow/agent-a15e40322473ae6d9.meta.json"),
            B => include_str!("../../../fixtures/workflow/agent-ad315649bfc4c9064.meta.json"),
            _ => include_str!("../../../fixtures/workflow/agent-a25a90245f5242e1d.meta.json"),
        }
    }

    struct Session(PathBuf);

    impl Session {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!("alondra-workflow-state-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("subagents/workflows").join(RUN)).unwrap();
            std::fs::create_dir_all(root.join("workflows/scripts")).unwrap();
            std::fs::write(root.join("workflows/scripts/state-file-probe-wf_3337df55-81d.js"), SCRIPT).unwrap();
            Self(root)
        }

        fn dir(&self) -> PathBuf { self.0.join("subagents/workflows").join(RUN) }

        fn journal(&self, lines: usize) {
            let text: String = JOURNAL.lines().take(lines).map(|line| format!("{line}\n")).collect();
            std::fs::write(self.dir().join("journal.jsonl"), text).unwrap();
        }

        fn agent(&self, id: &str) {
            std::fs::write(self.dir().join(format!("agent-{id}.meta.json")), meta(id)).unwrap();
        }
    }

    impl Drop for Session {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn rows(workflow: &LiveWorkflow) -> Vec<(&str, Option<&str>, Option<&str>, bool)> {
        workflow.agents.iter().map(|agent| (agent.label.as_str(), agent.phase.as_deref(), agent.model.as_deref(), agent.running)).collect()
    }

    #[test]
    fn mid_run_the_journal_names_the_phase_and_which_agent_still_runs() {
        let session = Session::new("live");
        session.agent(A);
        session.agent(B);
        session.journal(4);
        let workflow = read_uncached(&session.dir(), false).unwrap();
        assert_eq!((workflow.phase.as_deref(), workflow.phase_index, workflow.phase_count), (Some("First"), Some(1), Some(2)));
        assert_eq!((workflow.running, workflow.done), (1, 1));
        assert_eq!(rows(&workflow), [("count-b", Some("First"), Some("haiku"), true), ("count-a", Some("First"), Some("haiku"), false)]);
        assert_eq!(workflow.total_tokens, None);

        session.agent(C);
        session.journal(6);
        let workflow = read_uncached(&session.dir(), false).unwrap();
        assert_eq!((workflow.phase.as_deref(), workflow.phase_index, workflow.running, workflow.done), (Some("Second"), Some(2), 1, 2));
        assert_eq!(read_uncached(&session.dir(), true).unwrap().running, 0, "a workflow that ended runs no agent");
    }

    #[test]
    fn an_agent_without_a_model_in_its_meta_takes_the_one_its_transcript_ran_on() {
        let session = Session::new("transcript");
        std::fs::write(session.dir().join(format!("agent-{A}.meta.json")), r#"{"agentType":"workflow-subagent","description":"count-a"}"#).unwrap();
        std::fs::write(session.dir().join(format!("agent-{A}.jsonl")), TRANSCRIPT).unwrap();
        session.journal(2);
        assert_eq!(read_uncached(&session.dir(), false).unwrap().agents[0].model.as_deref(), Some("claude-haiku-4-5-20251001"));
    }

    #[test]
    fn once_finished_the_summary_gives_the_totals_and_the_resolved_models() {
        let session = Session::new("finished");
        for id in [A, B, C] { session.agent(id); }
        session.journal(7);
        std::fs::write(session.0.join("workflows").join(format!("{RUN}.json")), SUMMARY).unwrap();
        let workflow = read(&session.dir(), true).unwrap();
        assert_eq!((workflow.phase.as_deref(), workflow.phase_index, workflow.phase_count), (Some("Second"), Some(2), Some(2)));
        assert_eq!((workflow.running, workflow.done, workflow.duration_ms, workflow.total_tokens), (0, 3, Some(19_870), Some(88_896)));
        assert_eq!(rows(&workflow)[0], ("count-c", Some("Second"), Some("claude-haiku-4-5-20251001"), false));
        assert_eq!(read(&session.dir(), true), Some(workflow), "an unchanged folder answers from the cache");
    }

    #[test]
    fn a_missing_folder_or_a_garbled_line_reads_as_what_is_whole() {
        assert_eq!(read(Path::new("/nonexistent/subagents/workflows/wf_x"), false), None);
        let session = Session::new("garbled");
        assert_eq!(read_uncached(&session.dir(), false), None, "no journal yet");
        session.journal(3);
        let mut journal = std::fs::OpenOptions::new().append(true).open(session.dir().join("journal.jsonl")).unwrap();
        std::io::Write::write_all(&mut journal, b"{\"type\":\"started\",\"key\":\"v2:8b\",\"agentId\":\"ad31").unwrap();
        let workflow = read_uncached(&session.dir(), false).unwrap();
        assert_eq!((workflow.running, workflow.done, workflow.phase.as_deref()), (0, 1, Some("First")));
        std::fs::write(session.0.join("workflows").join(format!("{RUN}.json")), &SUMMARY[..SUMMARY.len() / 2]).unwrap();
        assert_eq!(read_uncached(&session.dir(), false).unwrap().done, 1, "a half-written summary falls back to the journal");
    }

    #[test]
    fn phase_titles_come_from_the_literal_meta_or_not_at_all() {
        assert_eq!(phase_titles(SCRIPT), Some(vec!["First".to_owned(), "Second".to_owned()]));
        assert_eq!(phase_titles("export const meta = { phases: [{ title: \"Plan, then [x]\", detail: 'a } b' }, { title: `Review` }] }"),
            Some(vec!["Plan, then [x]".to_owned(), "Review".to_owned()]));
        assert_eq!(phase_titles("export const meta = { phases: PHASES }"), None);
        assert_eq!(phase_titles("export const meta = { phases: [{ title: 'Unclosed"), None);
    }
}
