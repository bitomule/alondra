use super::{Lang, LiveNode, RunState, text::{t, tf}};
use crate::domain::Doing;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NowFact {
    pub doing: NowDoing,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NowDoing {
    Reported { what: String, progress: Option<String>, detail: Option<String>, since: String },
    Command { label: String, since: String },
    Background { label: String, code: bool, more: usize, since: String },
    Turn { since: String },
}

pub fn now_fact(node: Option<&LiveNode>, _doing: &Doing, turn_since: &str, lang: Lang) -> NowFact {
    let doing = node.and_then(|node| {
        if let Some(wait) = &node.reported_wait {
            return Some(NowDoing::Reported { what: wait.what.clone(), progress: wait.progress.clone(),
                detail: wait.detail.clone(), since: clock(&wait.since).into() });
        }
        if let Some(label) = node.tool.as_deref().and_then(safe_line) {
            return Some(NowDoing::Command {
                label,
                since: clock(node.tool_since.as_deref().filter(|at| !at.is_empty()).unwrap_or(turn_since)).into(),
            });
        }
        let command = |run: &super::LiveBackground| (run.kind != "agent").then(|| safe_line(&run.label)).flatten();
        let mut runs: Vec<(&str, Option<String>, &str)> = node.background.iter().filter(|run| run.state == RunState::Running)
            .map(|run| (run.since.as_str(), command(run), if run.kind == "agent" { SUBAGENT } else { WORK }))
            .chain(node.workflows.iter().filter(|run| run.state == RunState::Running).map(|run| (run.since.as_str(), None, WORKFLOW)))
            .chain(node.subagents.iter().filter(|run| run.state == RunState::Running).map(|run| (run.since.as_str(), None, SUBAGENT)))
            .collect();
        runs.sort_by(|left, right| left.0.cmp(right.0));
        let (since, command, kind) = runs.first()?;
        let (label, code) = match command {
            Some(command) => (command.clone(), true),
            None => (generic(kind, lang).into(), false),
        };
        Some(NowDoing::Background { label, code, more: runs.len() - 1,
            since: clock(if since.is_empty() { turn_since } else { since }).into() })
    }).unwrap_or_else(|| NowDoing::Turn { since: clock(turn_since).into() });
    NowFact { doing }
}

pub(super) fn clock(at: &str) -> &str {
    at.get(11..16).unwrap_or(at)
}

const WORK: &str = "work";
const WORKFLOW: &str = "workflow";
const SUBAGENT: &str = "subagent";

/// What runs when there is no command to name: its kind.
fn generic(kind: &str, lang: Lang) -> &'static str {
    match kind {
        WORKFLOW => t!(lang, "a workflow", "un flujo de trabajo"),
        SUBAGENT => t!(lang, "a subagent", "un subagente"),
        _ => t!(lang, "background work", "trabajo en segundo plano"),
    }
}

/// The first line of a tool or command as a list can say it: the program and its first arguments, a path by its
/// own name, and no value that could be a secret.
fn safe_line(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    match line.strip_prefix("Bash: ") {
        Some(command) => super::common::readable_command(command).map(|command| format!("Bash: {command}")),
        None => super::common::readable_command(line),
    }
}

/// A display label only: command arguments never become a task's state or its next action.
fn activity(label: &str, lang: Lang) -> &'static str {
    let label = label.strip_prefix("Bash: ").unwrap_or(label);
    let words: Vec<_> = label.split_whitespace().collect();
    match words.as_slice() {
        ["Read" | "Read:" | "cat" | "head" | "tail", ..] => t!(lang, "Reading files", "Leyendo archivos"),
        ["Edit" | "Edit:" | "Write" | "Write:" | "apply_patch", ..] => t!(lang, "Writing code", "Escribiendo código"),
        ["Grep" | "Glob" | "rg" | "grep" | "find", ..] => t!(lang, "Searching files", "Buscando en los archivos"),
        ["bazel" | "bazelisk" | "cargo" | "npm" | "swift", "test", ..]
            | ["npm", "run", "test" | "test:e2e" | "typecheck", ..]
            | ["make", "test", ..]
            | ["musts", "run" | "validate", ..] => t!(lang, "Running tests", "Pasando los tests"),
        ["bazel" | "bazelisk" | "cargo" | "npm" | "swift" | "make", "build", ..]
            | ["npm", "run", "build", ..] => t!(lang, "Building", "Compilando"),
        ["git", ..] => t!(lang, "Checking changes", "Revisando los cambios"),
        _ => t!(lang, "Running a command", "Ejecutando una orden"),
    }
}

impl NowFact {
    pub fn text(&self, lang: Lang) -> String {
        match &self.doing {
            NowDoing::Reported { what, progress, detail, since } => {
                let mut parts = vec![tf!(lang, "Waiting for {what}", "Espera a {what}")];
                parts.extend(progress.iter().chain(detail).cloned());
                parts.push(tf!(lang, "since {since}", "desde las {since}"));
                parts.join(" · ")
            }
            NowDoing::Command { label, since } => {
                let label = activity(label, lang);
                tf!(lang, "{label} · since {since}", "{label} · desde las {since}")
            },
            NowDoing::Background { label, code, more, since } => {
                let label = if *code { activity(label, lang).to_owned() } else { label.clone() };
                let label = if *more == 0 { label } else { tf!(lang, "{label} and {more} more", "{label} y {more} más") };
                tf!(lang, "In the background: {label} · since {since}", "En segundo plano: {label} · desde las {since}")
            }
            NowDoing::Turn { since } => tf!(lang, "Turn running since {since}", "Turno en marcha desde las {since}"),
        }
    }
}

#[cfg(test)]
mod tests;
