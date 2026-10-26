//! What a value may look like to enter the report as it is, and the pseudonyms of everything else.
use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::templates::{Catalog, Piece};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Task,
    Request,
    Node,
    Path,
    Repo,
    Account,
    Device,
    Value,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Task => "task",
            Kind::Request => "request",
            Kind::Node => "node",
            Kind::Path => "path",
            Kind::Repo => "repo",
            Kind::Account => "account",
            Kind::Device => "device",
            Kind::Value => "value",
        }
    }

    /// What a hole named like this holds, from the name the code gave it.
    fn of_hole(name: &str) -> Kind {
        match name {
            "task" | "task_id" | "next" => Kind::Task,
            "request" | "request_id" => Kind::Request,
            "agent" | "node" | "session" | "session_id" | "agent_id" | "node_id" => Kind::Node,
            "account" | "account_id" => Kind::Account,
            "device" | "device_id" => Kind::Device,
            "dir" | "path" | "file" | "worktree" | "home" | "cwd" => Kind::Path,
            "repo" | "repository" | "url" => Kind::Repo,
            _ => Kind::Value,
        }
    }
}

/// The same private value always gets the same pseudonym, in every section, so the log can be read
/// beside the state. The table stays on the user's Mac.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Pseudonyms {
    by_value: BTreeMap<String, String>,
    counts: BTreeMap<Kind, usize>,
}

impl Pseudonyms {
    pub fn of(&mut self, kind: Kind, value: &str) -> String {
        if let Some(name) = self.by_value.get(value) {
            return name.clone();
        }
        let count = self.counts.entry(kind).or_default();
        *count += 1;
        let name = format!("{}-{count}", kind.label());
        self.by_value.insert(value.to_owned(), name.clone());
        name
    }

    /// Every value that was replaced: the final check refuses a report where one still shows.
    pub fn originals(&self) -> impl Iterator<Item = &str> {
        self.by_value.keys().map(String::as_str)
    }

    pub fn is_empty(&self) -> bool {
        self.by_value.is_empty()
    }
}

/// A whole number, a decimal, a duration or a size: `12`, `0.8`, `250ms`, `1.5s`, `3 GiB`'s `3`.
pub fn is_number(token: &str) -> bool {
    let digits = token.trim_start_matches('-');
    let unit_at = digits.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(digits.len());
    let (number, unit) = digits.split_at(unit_at);
    let number_ok = !number.is_empty()
        && number.starts_with(|c: char| c.is_ascii_digit())
        && number.matches('.').count() <= 1
        && !number.ends_with('.');
    number_ok && UNITS.contains(&unit)
}

const UNITS: &[&str] = &[
    "", "ns", "µs", "us", "ms", "s", "m", "h", "d", "w", "B", "KB", "MB", "GB", "TB", "KiB", "MiB", "GiB", "TiB", "k", "%", "x", "×",
];

/// Durations Rust's `Debug` prints with several units, like `1m30s`.
fn is_compound_duration(token: &str) -> bool {
    let mut rest = token;
    let mut parts = 0;
    while !rest.is_empty() {
        let digits = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
        if digits == 0 {
            return false;
        }
        let after = &rest[digits..];
        let unit = ["ms", "µs", "us", "ns", "h", "m", "s", "d"].iter().find(|u| after.starts_with(*u));
        let Some(unit) = unit else { return false };
        rest = &after[unit.len()..];
        parts += 1;
    }
    parts > 1
}

/// `2026-10-25T05:43:41.854Z`, with any offset.
pub fn is_iso_time(token: &str) -> bool {
    let b = token.as_bytes();
    b.len() >= 19
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && token[11..].chars().all(|c| c.is_ascii_digit() || matches!(c, ':' | '.' | 'Z' | '+' | '-'))
}

/// `0.3.5`, `v2.1.30`, `1.2.3-beta.1`: two or three numbers, so an IPv4 address never is one.
pub fn is_version(token: &str) -> bool {
    let token = token.strip_prefix('v').unwrap_or(token);
    let (core, suffix) = token.split_once(['-', '+']).unwrap_or((token, ""));
    let parts: Vec<&str> = core.split('.').collect();
    (2..=3).contains(&parts.len())
        && parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
        && suffix.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
        && (parts.len() == 3 || !suffix.is_empty() || core.len() <= 6)
}

/// Alondra's random ids say nothing about the user: `sdk-<16 hex>`, `run-<hex>`, `o-<hex>`.
pub fn is_random_id(token: &str) -> bool {
    let hex = |s: &str, min: usize| s.len() >= min && s.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
    token.strip_prefix("sdk-").is_some_and(|rest| rest.len() == 16 && hex(rest, 16))
        || token.strip_prefix("run-").is_some_and(|rest| hex(rest, 6))
        || token.strip_prefix("o-").is_some_and(|rest| hex(rest, 6))
}

pub fn is_task_id(token: &str) -> bool {
    slug_after(token, "t-")
}

fn is_request_id(token: &str) -> bool {
    slug_after(token, "req-")
}

fn slug_after(token: &str, prefix: &str) -> bool {
    token.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty() && rest.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

fn is_uuid(token: &str) -> bool {
    let groups: Vec<&str> = token.split('-').collect();
    groups.len() == 5
        && groups.iter().zip([8, 4, 4, 4, 12]).all(|(g, n)| g.len() == n && g.chars().all(|c| c.is_ascii_hexdigit()))
}

fn is_path(token: &str) -> bool {
    (token.starts_with('/') && token.len() > 1) || token.starts_with("~/")
}

fn is_url(token: &str) -> bool {
    token.starts_with("http://") || token.starts_with("https://")
}

/// The pseudonym kind a value's own shape says, if it says one.
fn kind_of_shape(token: &str) -> Option<Kind> {
    if is_task_id(token) {
        Some(Kind::Task)
    } else if is_request_id(token) {
        Some(Kind::Request)
    } else if is_uuid(token) {
        Some(Kind::Node)
    } else if is_path(token) {
        Some(Kind::Path)
    } else if is_url(token) {
        Some(Kind::Repo)
    } else {
        None
    }
}

/// A token that is safe on its own: a number, a time, a version, a random id or a word of the code.
pub fn is_safe_token(token: &str, vocabulary: &HashSet<String>) -> bool {
    is_number(token)
        || is_compound_duration(token)
        || is_iso_time(token)
        || is_version(token)
        || is_random_id(token)
        || vocabulary.contains(token)
}

#[derive(Debug, PartialEq, Eq)]
enum Part<'a> {
    Token(&'a str),
    Gap(&'a str),
}

/// A value split into tokens and the punctuation between them; a URL is one token.
fn split(value: &str) -> Vec<Part<'_>> {
    let gap = |c: char| c.is_whitespace() || matches!(c, ',' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | ';' | '=' | '|' | '<' | '>' | '`');
    let mut parts = Vec::new();
    let mut rest = value;
    while !rest.is_empty() {
        let token_end = if is_url(rest) {
            rest.find(char::is_whitespace).unwrap_or(rest.len())
        } else {
            rest.find(gap).unwrap_or(rest.len())
        };
        if token_end > 0 {
            let token = &rest[..token_end];
            let trimmed = token.trim_end_matches([':', '.', ',', '!', '?']);
            let trimmed = if trimmed.is_empty() { token } else { trimmed };
            parts.push(Part::Token(trimmed));
            if trimmed.len() < token.len() {
                parts.push(Part::Gap(&token[trimmed.len()..]));
            }
            rest = &rest[token_end..];
            continue;
        }
        let gap_end = rest.find(|c: char| !gap(c)).unwrap_or(rest.len());
        parts.push(Part::Gap(&rest[..gap_end]));
        rest = &rest[gap_end..];
    }
    parts
}

pub struct Redactor<'a> {
    pub vocabulary: &'a HashSet<String>,
    pub pseudonyms: Pseudonyms,
}

impl<'a> Redactor<'a> {
    pub fn new(vocabulary: &'a HashSet<String>) -> Self {
        Redactor { vocabulary, pseudonyms: Pseudonyms::default() }
    }

    /// A value as the report may show it: itself when every token is safe, the safe tokens with each
    /// id, path or URL replaced when that is all it has, or one pseudonym for the whole of it.
    pub fn value(&mut self, value: &str, hint: Kind) -> String {
        if is_iso_time(value) {
            return super::time::iso_to_utc(value).unwrap_or_else(|| self.pseudonyms.of(Kind::Value, value));
        }
        let parts = split(value);
        let tokens = || parts.iter().filter_map(|p| if let Part::Token(t) = p { Some(*t) } else { None });
        if tokens().all(|t| is_safe_token(t, self.vocabulary) || kind_of_shape(t).is_some()) {
            return parts
                .iter()
                .map(|part| match part {
                    Part::Gap(gap) => (*gap).to_owned(),
                    Part::Token(t) if is_iso_time(t) => super::time::iso_to_utc(t).unwrap_or_else(|| self.pseudonyms.of(Kind::Value, t)),
                    Part::Token(t) if is_safe_token(t, self.vocabulary) => (*t).to_owned(),
                    Part::Token(t) => self.shaped(t),
                })
                .collect();
        }
        let kind = kind_of_shape(value).unwrap_or(hint);
        self.pseudonyms.of(kind, value)
    }

    /// An id, path or URL by its shape; a pull request URL keeps its number.
    fn shaped(&mut self, token: &str) -> String {
        if let Some((repo, number)) = pull_request(token) {
            return format!("{}/pull/{number}", self.pseudonyms.of(Kind::Repo, repo));
        }
        let kind = kind_of_shape(token).unwrap_or(Kind::Value);
        self.pseudonyms.of(kind, token)
    }

    /// An id the report names by itself (a task's, a node's): always a pseudonym, never the id.
    pub fn id(&mut self, kind: Kind, id: &str) -> String {
        if is_random_id(id) {
            return id.to_owned();
        }
        self.pseudonyms.of(kind, id)
    }

    /// A word that must be a word of the code (a state, a reason): itself, or a pseudonym.
    pub fn word(&mut self, word: &str) -> String {
        if is_safe_token(word, self.vocabulary) {
            word.to_owned()
        } else {
            self.pseudonyms.of(Kind::Value, word)
        }
    }

    /// A message of the daemon's log redacted by its template, or `None` when no template's fixed
    /// text matches all of it.
    pub fn log_message(&mut self, catalog: &Catalog, message: &str) -> Option<String> {
        self.templated(catalog, message, NESTING)
    }

    /// `message` as its template with each hole redacted; a hole that holds a sentence the code
    /// words elsewhere keeps that sentence's text too.
    fn templated(&mut self, catalog: &Catalog, message: &str, depth: usize) -> Option<String> {
        let (template, values) = catalog.matching(message)?;
        let mut out = String::new();
        let mut values = values.into_iter();
        for piece in &template.pieces {
            match piece {
                Piece::Text(text) => out.push_str(text),
                Piece::Hole(name) => {
                    let value = values.next()?;
                    let nested = if depth > 0 && value.contains(' ') { self.templated(catalog, value, depth - 1) } else { None };
                    let redacted = nested.unwrap_or_else(|| self.value(value, Kind::of_hole(name)));
                    out.push_str(&redacted);
                }
            }
        }
        Some(out)
    }
}

/// How many sentences deep a log line is read into its holes.
const NESTING: usize = 2;

fn pull_request(url: &str) -> Option<(&str, &str)> {
    let rest = url.strip_prefix("https://github.com/")?;
    let (repo, number) = rest.split_once("/pull/")?;
    let number = number.trim_end_matches('/');
    (repo.matches('/').count() == 1 && !number.is_empty() && number.chars().all(|c| c.is_ascii_digit())).then_some((repo, number))
}

#[cfg(test)]
pub(super) fn split_for_test(value: &str) -> Vec<(bool, String)> {
    split(value)
        .into_iter()
        .map(|part| match part {
            Part::Token(t) => (true, t.to_owned()),
            Part::Gap(g) => (false, g.to_owned()),
        })
        .collect()
}
