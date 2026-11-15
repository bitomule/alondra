//! What may not be in a transcript that is committed: secrets, emails, home paths, ids of a real
//! session, private names and free text. A finding names its file, its line and its rule, never
//! the text it matched, so a report of it leaks nothing either.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const PRIVATE_LITERALS_ENV: &str = "ALONDRA_PRIVATE_LITERALS";
/// The user of the demo world every identity is moved to.
pub const DEMO_USER: &str = "ana";
pub const DEMO_EMAIL: &str = "ana@example.com";
pub const DEMO_HOME: &str = "/Users/ana";
/// The texts a transcript may carry besides markers and identifiers, one per line.
pub const SYNTHETIC_TEXTS: &str = "synthetic.txt";
const PSEUDO_UUID_PREFIX: &str = "00000000-0000-4000-8000-";
const MAX_TOKEN: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub file: String,
    pub line: usize,
    pub rule: &'static str,
}

impl std::fmt::Display for Finding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.file, self.line, self.rule)
    }
}

/// The private literals of this Mac, lowercased, from the file `ALONDRA_PRIVATE_LITERALS` names.
pub fn literals_from_env() -> Vec<String> {
    let Some(path) = std::env::var_os(PRIVATE_LITERALS_ENV) else { return Vec::new() };
    let Ok(text) = std::fs::read_to_string(path) else { return Vec::new() };
    text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_lowercase).collect()
}

fn is_marker(text: &str) -> bool {
    text.len() > 2 && text.starts_with('«') && text.ends_with('»') && !text[2..text.len() - 2].contains(['«', '»'])
}

/// A value that names a thing instead of saying something: no spaces, short, made of the
/// characters of identifiers, paths and timestamps.
pub fn is_token(text: &str) -> bool {
    text.len() <= MAX_TOKEN && text.chars().all(|c| c.is_ascii_alphanumeric() || "_.:/@+=,-".contains(c))
}

fn run_at(text: &str, start: usize, accepts: impl Fn(char) -> bool) -> usize {
    text[start..].chars().take_while(|c| accepts(*c)).map(char::len_utf8).sum()
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

fn after_all(line: &str, prefix: &str) -> Vec<usize> {
    line.match_indices(prefix)
        .filter(|(at, _)| line[..*at].chars().next_back().is_none_or(|before| !before.is_ascii_alphanumeric()))
        .map(|(at, _)| at + prefix.len())
        .collect()
}

fn secret(line: &str) -> bool {
    let long = |prefix: &str, min: usize| after_all(line, prefix).into_iter().any(|at| run_at(line, at, token_char) >= min);
    let jwt = line.match_indices("eyJ").any(|(at, _)| {
        let head = run_at(line, at + 3, token_char);
        head >= 8 && line[at + 3 + head..].strip_prefix('.').is_some_and(|rest| run_at(rest, 0, token_char) >= 8)
    });
    jwt || long("sk-", 20)
        || ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxa-", "xoxp-"].iter().any(|prefix| long(prefix, 20))
        || line.to_lowercase().match_indices("bearer ").any(|(at, _)| run_at(&line.to_lowercase(), at + 7, |c| token_char(c) || c == '.') >= 8)
}

fn reserved_domain(domain: &str) -> bool {
    let domain = domain.to_lowercase();
    ["example.com", "example.org", "example.net"].contains(&domain.as_str()) || domain.ends_with(".example") || domain.ends_with(".test") || domain.ends_with(".invalid")
}

fn email(line: &str) -> bool {
    line.match_indices('@').any(|(at, _)| {
        let local = line[..at].chars().rev().take_while(|c| c.is_ascii_alphanumeric() || "._+-".contains(*c)).count();
        let domain = run_at(line, at + 1, |c| c.is_ascii_alphanumeric() || ".-".contains(c));
        let domain = line[at + 1..at + 1 + domain].trim_end_matches('.');
        local > 0 && domain.contains('.') && !reserved_domain(domain)
    })
}

fn home_path(line: &str) -> bool {
    ["/Users/", "/home/"].iter().any(|root| {
        line.match_indices(root).any(|(at, _)| {
            let name = &line[at + root.len()..];
            name[..run_at(name, 0, |c| c.is_ascii_alphanumeric() || "._-".contains(c))] != *DEMO_USER
        })
    })
}

fn uuid(line: &str) -> bool {
    let shape = [8, 4, 4, 4, 12];
    let bytes: Vec<char> = line.chars().collect();
    (0..bytes.len()).any(|start| {
        let mut at = start;
        for (index, size) in shape.iter().enumerate() {
            if index > 0 {
                if bytes.get(at) != Some(&'-') {
                    return false;
                }
                at += 1;
            }
            if !(0..*size).all(|offset| bytes.get(at + offset).is_some_and(char::is_ascii_hexdigit)) {
                return false;
            }
            at += size;
        }
        let before = start.checked_sub(1).and_then(|i| bytes.get(i));
        let text: String = bytes[start..at].iter().collect();
        !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !bytes.get(at).is_some_and(char::is_ascii_alphanumeric) && !text.starts_with(PSEUDO_UUID_PREFIX)
    })
}

fn opaque(line: &str) -> bool {
    let mut run = 0;
    for c in line.chars() {
        run = if c.is_ascii_alphanumeric() || "_+=-".contains(c) { run + 1 } else { 0 };
        if run >= 40 {
            return true;
        }
    }
    false
}

/// The rules that read a line of text as it is.
pub fn scan_line(line: &str, literals: &[String]) -> Vec<&'static str> {
    let lowered = line.to_lowercase();
    let mut rules = Vec::new();
    for (rule, hit) in [("secret", secret(line)), ("email", email(line)), ("home-path", home_path(line)), ("real-id", uuid(line)), ("opaque-token", opaque(line))] {
        if hit {
            rules.push(rule);
        }
    }
    if literals.iter().any(|literal| lowered.contains(literal.as_str())) {
        rules.push("private-literal");
    }
    rules
}

fn free_text(value: &Value, synthetic: &BTreeSet<String>) -> bool {
    match value {
        Value::String(text) => !(text.is_empty() || is_marker(text) || is_token(text) || synthetic.contains(text)),
        Value::Array(items) => items.iter().any(|item| free_text(item, synthetic)),
        Value::Object(map) => map.values().any(|item| free_text(item, synthetic)),
        _ => false,
    }
}

/// Every finding of a transcript, by line: the rules of `scan_line`, and free text that is neither
/// a marker, nor an identifier, nor one of the scenario's synthetic texts.
pub fn scan_transcript(text: &str, literals: &[String], synthetic: &BTreeSet<String>) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        found.extend(scan_line(line, literals).into_iter().map(|rule| (number, rule)));
        match serde_json::from_str::<Value>(line) {
            Ok(value) if free_text(&value, synthetic) => found.push((number, "free-text")),
            Ok(_) => {}
            Err(_) if line.trim().is_empty() => {}
            Err(_) => found.push((number, "not-json")),
        }
    }
    found
}

fn transcripts_under(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            transcripts_under(&path, found);
        } else if path.extension().is_some_and(|extension| extension == "ndjson") {
            found.push(path);
        }
    }
}

/// Every transcript (`*.ndjson`) under `root`.
pub fn transcripts(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    transcripts_under(root, &mut found);
    found
}

fn synthetic_texts(directory: &Path) -> BTreeSet<String> {
    std::fs::read_to_string(directory.join(SYNTHETIC_TEXTS)).map(|text| text.lines().filter(|line| !line.is_empty()).map(str::to_owned).collect()).unwrap_or_default()
}

/// Checks every transcript under `root`; paths in the findings are relative to it.
pub fn check_tree(root: &Path, literals: &[String]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for path in transcripts(root) {
        let file = path.strip_prefix(root).unwrap_or(&path).display().to_string();
        let Ok(text) = std::fs::read_to_string(&path) else {
            findings.push(Finding { file, line: 0, rule: "unreadable" });
            continue;
        };
        let synthetic = path.parent().map(synthetic_texts).unwrap_or_default();
        findings.extend(scan_transcript(&text, literals, &synthetic).into_iter().map(|(line, rule)| Finding { file: file.clone(), line, rule }));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(line: &str) -> Vec<&'static str> {
        scan_line(line, &["projectcanaryhorizon".into()])
    }

    #[test]
    fn each_shape_of_a_leak_is_found() {
        assert_eq!(rules(r#"{"a":"eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0"}"#), vec!["secret"]);
        assert_eq!(rules("token sk-ant-canary0123456789abcdef"), vec!["secret"]);
        assert_eq!(rules("token ghp_canary0123456789abcdef"), vec!["secret"]);
        assert_eq!(rules("Authorization: Bearer canarytoken123456"), vec!["secret"]);
        assert_eq!(rules("canary.person@canary-mail.io"), vec!["email"]);
        assert_eq!(rules("/Users/canaryman/work"), vec!["home-path"]);
        assert_eq!(rules("a1b2c3d4-0000-4000-8000-0123456789ab"), vec!["real-id"]);
        assert_eq!(rules(&"Q".repeat(44)), vec!["opaque-token"]);
        assert_eq!(rules("ProjectCanaryHorizon"), vec!["private-literal"]);
    }

    #[test]
    fn the_demo_world_is_not_a_finding() {
        assert!(rules("ana@example.com /Users/ana/demo 00000000-0000-4000-8000-000000000007 «user text 1» sk-short").is_empty());
    }

    #[test]
    fn free_text_is_a_finding_unless_it_is_a_marker_a_token_or_listed() {
        let line = |text: &str| format!(r#"{{"type":"emit_inbound","frame":{{"text":"{text}"}}}}"#);
        let none = BTreeSet::new();
        assert!(scan_transcript(&line("«model text 3»"), &[], &none).is_empty());
        assert!(scan_transcript(&line("authentication_failed"), &[], &none).is_empty());
        assert_eq!(scan_transcript(&line("please rename the column"), &[], &none), vec![(1, "free-text")]);
        let listed = BTreeSet::from(["please rename the column".to_owned()]);
        assert!(scan_transcript(&line("please rename the column"), &[], &listed).is_empty());
    }

    #[test]
    fn a_tree_reports_the_file_and_line_of_a_canary_without_its_text() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("case/transcripts")).unwrap();
        std::fs::write(directory.path().join("case/transcripts/a.ndjson"), "{\"a\":\"«user text 1»\"}\n{\"a\":\"canary.person@canary-mail.io\"}\n").unwrap();
        let findings = check_tree(directory.path(), &[]);
        assert_eq!(findings, vec![Finding { file: "case/transcripts/a.ndjson".into(), line: 2, rule: "email" }]);
        assert!(!findings[0].to_string().contains("canary"));
    }
}
