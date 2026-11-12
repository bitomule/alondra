use super::{Lang, LiveNode, RunState, text::tf};
use crate::domain::Doing;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NowFact {
    pub doing: NowDoing,
    pub said: Option<Said>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NowDoing {
    Reported { what: String, progress: Option<String>, detail: Option<String>, since: String },
    Command { label: String, since: String },
    Background { label: String, more: usize, since: String },
    Turn { since: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Said {
    pub sentence: String,
    pub at: String,
}

pub fn now_fact(node: Option<&LiveNode>, _doing: &Doing, turn_since: &str, _lang: Lang) -> NowFact {
    let doing = node.and_then(|node| {
        if let Some(wait) = &node.reported_wait {
            return Some(NowDoing::Reported { what: wait.what.clone(), progress: wait.progress.clone(),
                detail: wait.detail.clone(), since: clock(&wait.since).into() });
        }
        if let Some(label) = node.tool.as_deref().and_then(short_line) {
            return Some(NowDoing::Command {
                label,
                since: clock(node.tool_since.as_deref().filter(|at| !at.is_empty()).unwrap_or(turn_since)).into(),
            });
        }
        let mut runs: Vec<_> = node.background.iter().filter(|run| run.state == RunState::Running)
            .map(|run| (run.since.as_str(), run.label.as_str()))
            .chain(node.workflows.iter().chain(&node.subagents).filter(|run| run.state == RunState::Running)
                .map(|run| (run.since.as_str(), run.label.as_str())))
            .filter(|(_, label)| label.lines().next().is_some_and(|line| !line.trim().is_empty())).collect();
        runs.sort();
        let (since, label) = runs.first()?;
        Some(NowDoing::Background { label: short_line(label)?, more: runs.len() - 1,
            since: clock(if since.is_empty() { turn_since } else { since }).into() })
    }).unwrap_or_else(|| NowDoing::Turn { since: clock(turn_since).into() });
    let said = node.and_then(|node| Some(Said {
        sentence: super::task_node::said(node.last_message.as_deref()?)?,
        at: clock(node.last_message_at.as_deref().filter(|at| !at.is_empty())?).into(),
    }));
    NowFact { doing, said }
}

pub(super) fn clock(at: &str) -> &str {
    at.get(11..16).unwrap_or(at)
}

fn short_line(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    if line.is_empty() { return None; }
    Some(shorten(line))
}

fn shorten(text: &str) -> String {
    if text.chars().count() <= 60 { return text.into(); }
    format!("{}…", text.chars().take(59).collect::<String>())
}

fn code_label(label: &str) -> String {
    let width = label.split(|c| c != '`').map(str::len).max().unwrap_or(0) + 1;
    let mark = "`".repeat(width);
    if label.starts_with('`') || label.ends_with('`') {
        format!("{mark} {label} {mark}")
    } else {
        format!("{mark}{label}{mark}")
    }
}

impl NowFact {
    pub fn text(&self, lang: Lang) -> String {
        let mut text = match &self.doing {
            NowDoing::Reported { what, progress, detail, since } => {
                let mut parts = vec![tf!(lang, "Waiting for {what}", "Espera a {what}")];
                parts.extend(progress.iter().chain(detail).cloned());
                parts.push(tf!(lang, "since {since}", "desde las {since}"));
                parts.join(" · ")
            }
            NowDoing::Command { label, since } => {
                let label = code_label(label);
                tf!(lang, "{label} · since {since}", "{label} · desde las {since}")
            },
            NowDoing::Background { label, more, since } => {
                let label = code_label(label);
                let label = if *more == 0 { label } else { tf!(lang, "{label} and {more} more", "{label} y {more} más") };
                tf!(lang, "In the background: {label} · since {since}", "En segundo plano: {label} · desde las {since}")
            }
            NowDoing::Turn { since } => tf!(lang, "Turn running since {since}", "Turno en marcha desde las {since}"),
        };
        if let Some(Said { sentence, at }) = &self.said {
            text.push_str(&tf!(lang, " · Said: “{sentence}” {at}", " · Dijo: «{sentence}» {at}"));
        }
        text
    }
}

#[cfg(test)]
mod tests;
