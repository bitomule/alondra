//! What each model call of an agent did, by the tools it called. A step is one model call: every
//! step re-reads the whole context, so steps that only look or wait are the ones worth batching.
//! The rules are the ones `tools/node_token_usage.py` measured nodes with.
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// A tool call, as the provider's adapter reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Tool {
    /// Reads or searches files and changes nothing.
    Read,
    /// Runs a shell command.
    Shell(String),
    /// Waits for the output of something running in the background.
    Wait,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// No tool: the model only wrote.
    Text,
    /// One call that only reads.
    Look,
    /// One shell call made only of commands that read.
    Looks,
    /// Several calls in one step.
    Parallel,
    /// Waits for something to finish: a sleep, a loop until something holds, a followed log.
    Poll,
    Other,
}

const LOOKS: [&str; 23] = [
    "grep", "rg", "sed", "cat", "head", "tail", "ls", "git", "python3", "python", "find", "wc", "awk", "jq", "diff",
    "stat", "file", "test", "echo", "cd", "pwd", "nl", "tree",
];

pub fn classify(tools: &[Tool]) -> StepKind {
    if tools.iter().any(|tool| match tool {
        Tool::Wait => true,
        Tool::Shell(command) => polls(command),
        _ => false,
    }) {
        return StepKind::Poll;
    }
    match tools {
        [] => StepKind::Text,
        [one] => match looks(one) {
            0 => StepKind::Other,
            1 => StepKind::Look,
            _ => StepKind::Looks,
        },
        _ => StepKind::Parallel,
    }
}

fn looks(tool: &Tool) -> usize {
    match tool {
        Tool::Read => 1,
        Tool::Shell(command) => {
            let parts = commands(command);
            let reads = |part: &&str| LOOKS.contains(&program(part.split('|').next().unwrap_or_default()));
            if !parts.is_empty() && parts.iter().all(|part| reads(part)) { parts.len() } else { 0 }
        }
        _ => 0,
    }
}

fn commands(command: &str) -> Vec<&str> {
    let mut parts = vec![];
    let mut rest = command;
    while !rest.is_empty() {
        let cut = ["&&", "||", ";", "\n"].iter().filter_map(|separator| rest.find(separator).map(|at| (at, separator.len()))).min();
        let (part, next) = match cut {
            Some((at, len)) => (&rest[..at], &rest[at + len..]),
            None => (rest, ""),
        };
        let part = part.trim();
        if !part.is_empty() && program(part) != "cd" {
            parts.push(part);
        }
        rest = next;
    }
    parts
}

/// The program a command runs: its first word that is not a variable assignment.
fn program(command: &str) -> &str {
    command.split_whitespace().find(|word| !word.contains('=')).map_or("", |word| word.rsplit('/').next().unwrap_or(word))
}

fn word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Each whole word of a line, with what follows it.
fn words(line: &str) -> impl Iterator<Item = (&str, &str)> {
    let mut at = 0;
    std::iter::from_fn(move || {
        let start = at + line[at..].find(word_char)?;
        let end = line[start..].find(|c: char| !word_char(c)).map_or(line.len(), |len| start + len);
        at = end;
        Some((&line[start..end], &line[end..]))
    })
}

fn polls(command: &str) -> bool {
    command.lines().any(|line| {
        let mut seen_while = false;
        words(line).any(|(word, after)| {
            let spaced = after.trim_start();
            let gap = after.len() > spaced.len();
            let found = match word {
                "sleep" => gap && spaced.starts_with(|c: char| c.is_ascii_digit()),
                "until" => true,
                "do" => seen_while,
                "tail" => gap && spaced.strip_prefix('-').is_some_and(|flags| flags.chars().take_while(char::is_ascii_alphabetic).any(|c| c == 'f')),
                "watch" => after.starts_with(char::is_whitespace),
                _ => false,
            };
            seen_while |= word == "while";
            found
        })
    })
}

/// Steps counted for one session, added to what the store holds for it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepCounts {
    /// The session's own steps.
    pub steps: i64,
    /// The steps of the subagents it ran.
    pub subagent_steps: i64,
    /// Steps, its own or its subagents', that only looked.
    pub looks: i64,
    /// Steps that only waited.
    pub polls: i64,
    pub tool_calls: i64,
}

impl StepCounts {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    fn add(&mut self, kind: StepKind, subagent: bool, calls: usize) {
        if subagent {
            self.subagent_steps += 1;
        } else {
            self.steps += 1;
        }
        self.looks += i64::from(matches!(kind, StepKind::Look | StepKind::Looks));
        self.polls += i64::from(kind == StepKind::Poll);
        self.tool_calls += calls as i64;
    }
}

const REMEMBERED: usize = 512;
const OPEN_LIMIT: usize = 4_096;

/// The steps of one session's stream. A provider may split one call into several messages that
/// share its id, so a step is classified only once it is taken, and an id already taken is not
/// counted again.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StepMeter {
    open: Vec<(String, bool, Vec<Tool>)>,
    taken: VecDeque<String>,
}

impl StepMeter {
    pub fn saw(&mut self, id: &str, subagent: bool, tools: Vec<Tool>) {
        if self.taken.iter().any(|known| known == id) {
            return;
        }
        if let Some((_, _, known)) = self.open.iter_mut().find(|(known, _, _)| known == id) {
            known.extend(tools);
        } else if self.open.len() < OPEN_LIMIT {
            self.open.push((id.to_owned(), subagent, tools));
        }
    }

    /// Every step seen since the last take, counted.
    pub fn take(&mut self) -> StepCounts {
        let mut counts = StepCounts::default();
        for (id, subagent, tools) in std::mem::take(&mut self.open) {
            counts.add(classify(&tools), subagent, tools.len());
            self.taken.push_back(id);
            if self.taken.len() > REMEMBERED {
                self.taken.pop_front();
            }
        }
        counts
    }
}

#[cfg(test)]
#[path = "steps_tests.rs"]
mod tests;
