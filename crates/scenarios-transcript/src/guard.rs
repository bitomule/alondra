//! What may not be in a transcript that is committed: secrets, emails, home paths, ids of a real
//! session, private names and free text. A finding names its file, its line and its rule, never
//! the text it matched, so a report of it leaks nothing either.

use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub const PRIVATE_LITERALS_ENV: &str = "ALONDRA_PRIVATE_LITERALS";
/// What `ALONDRA_PRIVATE_LITERALS` says where there is no list on purpose: a public CI, a
/// contributor's Mac.
pub const NO_PRIVATE_LITERALS: &str = "none";
/// The user of the demo world every identity is moved to.
pub const DEMO_USER: &str = "ana";
pub const DEMO_EMAIL: &str = "ana@example.com";
pub const DEMO_HOME: &str = "/Users/ana";
/// The texts a transcript may carry besides markers and identifiers, one per line.
pub const SYNTHETIC_TEXTS: &str = "synthetic.txt";
/// The prefixes of the ids a transcript may carry: the anonymizer's, and those tests write by
/// hand, one hex digit and zeros.
const PSEUDO_UUID_TAIL: &str = "0000000-0000-4000-8000-";
const MAX_TOKEN: usize = 64;
const MAX_HEADER: usize = 4096;

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

/// Why the private literals could not be read. Without them no check may pass: a private name is
/// only found by its list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LiteralsError {
    Unset,
    Unreadable(String),
    Empty(String),
}

impl std::fmt::Display for LiteralsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unset => write!(
                f,
                "{PRIVATE_LITERALS_ENV} is not set: point it to the file of private literals, one per line, or set it to `{NO_PRIVATE_LITERALS}` where there is no list on purpose"
            ),
            Self::Unreadable(why) => write!(f, "the file {PRIVATE_LITERALS_ENV} names cannot be read: {why}"),
            Self::Empty(path) => write!(f, "the file {PRIVATE_LITERALS_ENV} names has no literal: {path}"),
        }
    }
}

impl std::error::Error for LiteralsError {}

/// The private literals, lowercased, from the file `value` names; none when it says `none`.
pub fn literals_from(value: Option<&std::ffi::OsStr>) -> Result<Vec<String>, LiteralsError> {
    let value = value.ok_or(LiteralsError::Unset)?;
    if value == NO_PRIVATE_LITERALS {
        return Ok(Vec::new());
    }
    if value.is_empty() {
        return Err(LiteralsError::Unset);
    }
    let text = std::fs::read_to_string(value).map_err(|error| LiteralsError::Unreadable(error.to_string()))?;
    let literals: Vec<String> = text.lines().map(str::trim).filter(|line| !line.is_empty() && !line.starts_with('#')).map(str::to_lowercase).collect();
    if literals.is_empty() {
        return Err(LiteralsError::Empty(Path::new(value).display().to_string()));
    }
    Ok(literals)
}

/// The private literals of this Mac from the file `ALONDRA_PRIVATE_LITERALS` names.
pub fn literals_from_env() -> Result<Vec<String>, LiteralsError> {
    literals_from(std::env::var_os(PRIVATE_LITERALS_ENV).as_deref())
}

pub fn is_marker(text: &str) -> bool {
    text.len() > 2 && text.starts_with('«') && text.ends_with('»') && !text[2..text.len() - 2].contains(['«', '»'])
}

/// A value that names a thing instead of saying something: no spaces, short, made of the
/// characters of identifiers, paths and timestamps.
pub fn is_token(text: &str) -> bool {
    text.len() <= MAX_TOKEN && text.chars().all(|c| c.is_ascii_alphanumeric() || "_.:/@+=,-".contains(c))
}

fn identifier_part(part: &str) -> bool {
    !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || "_.:+=,-".contains(c))
}

/// A token that is a word of the protocol, not an address: no `@`, no URL, no absolute path. A
/// method of the Codex app-server (`item/agentMessage/delta`) is words joined by `/`.
pub fn is_word(text: &str) -> bool {
    is_token(text) && text.split('/').all(identifier_part)
}

/// A key of a JSON object that is a name of the structure.
pub fn is_key(text: &str) -> bool {
    text.len() <= MAX_TOKEN && text.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && text.chars().all(|c| c.is_ascii_alphanumeric() || "_-".contains(c))
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
    let aws = after_all(line, "AKIA").into_iter().chain(after_all(line, "ASIA")).any(|at| run_at(line, at, |c| c.is_ascii_uppercase() || c.is_ascii_digit()) >= 16);
    jwt || aws
        || long("sk-", 20)
        || ["ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", "xoxb-", "xoxa-", "xoxp-", "xoxr-", "xoxs-", "glpat-", "npm_", "AIza"].iter().any(|prefix| long(prefix, 20))
        || line.contains("-----BEGIN")
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

/// An absolute path outside the demo home, or a disk of this Mac.
fn private_path(text: &str) -> bool {
    let outside = |path: &str| path.starts_with('/') && path != DEMO_HOME && !path.starts_with(&format!("{DEMO_HOME}/"));
    text.contains("/Volumes/") || text.starts_with("~/") || outside(text)
}

/// An address on the network that is not a reserved example one.
fn url(text: &str) -> bool {
    text.match_indices("://").any(|(at, _)| {
        let host = &text[at + 3..];
        let host = &host[..run_at(host, 0, |c| c.is_ascii_alphanumeric() || ".-".contains(c))];
        !reserved_domain(host)
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
        !before.is_some_and(|c| c.is_ascii_alphanumeric()) && !bytes.get(at).is_some_and(char::is_ascii_alphanumeric) && !text[1..].starts_with(PSEUDO_UUID_TAIL)
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

/// The rules that read a text as it is: a line, or one decoded string of it.
pub fn scan_line(line: &str, literals: &[String]) -> Vec<&'static str> {
    let lowered = line.to_lowercase();
    let mut rules = Vec::new();
    for (rule, hit) in [("secret", secret(line)), ("email", email(line)), ("home-path", home_path(line)), ("real-id", uuid(line)), ("opaque-token", opaque(line)), ("url", url(line))] {
        if hit {
            rules.push(rule);
        }
    }
    if literals.iter().any(|literal| lowered.contains(literal.as_str())) {
        rules.push("private-literal");
    }
    rules
}

/// Every string of a JSON value, keys first, each with whether it is a key.
fn strings<'a>(value: &'a Value, found: &mut Vec<(&'a str, bool)>) {
    match value {
        Value::String(text) => found.push((text, false)),
        Value::Array(items) => items.iter().for_each(|item| strings(item, found)),
        Value::Object(map) => {
            for (key, item) in map {
                found.push((key, true));
                strings(item, found);
            }
        }
        _ => {}
    }
}

fn free_text(text: &str, key: bool, synthetic: &BTreeSet<String>) -> bool {
    let allowed = if key { is_key(text) } else { text.is_empty() || is_word(text) || text == DEMO_HOME || text.starts_with(&format!("{DEMO_HOME}/")) || text == DEMO_EMAIL };
    !(allowed || is_marker(text) || synthetic.contains(text))
}

/// Every finding of a transcript, by line: the rules of `scan_line` over the line and over each
/// string it decodes to, keys included, so an escape (`\/Users`, `é`) hides nothing; a path
/// outside the demo home; and free text, a key or a value that is neither a marker, nor a word of
/// the protocol, nor one of the scenario's synthetic texts.
pub fn scan_transcript(text: &str, literals: &[String], synthetic: &BTreeSet<String>) -> Vec<(usize, &'static str)> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let mut rules: Vec<&'static str> = scan_line(line, literals);
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                let mut texts = Vec::new();
                strings(&value, &mut texts);
                for (text, key) in texts {
                    rules.extend(scan_line(text, literals));
                    if private_path(text) {
                        rules.push("private-path");
                    }
                    if free_text(text, key, synthetic) {
                        rules.push("free-text");
                    }
                }
            }
            Err(_) if line.trim().is_empty() => {}
            Err(_) => rules.push("not-json"),
        }
        let mut seen = BTreeSet::new();
        found.extend(rules.into_iter().filter(|rule| seen.insert(*rule)).map(|rule| (number, rule)));
    }
    found
}

/// Whether the file starts with the header of a transcript, whatever its name.
pub fn is_transcript(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else { return false };
    let mut head = Vec::new();
    if file.take(MAX_HEADER as u64).read_to_end(&mut head).is_err() {
        return false;
    }
    let Some(end) = head.iter().position(|byte| *byte == b'\n').or((head.len() < MAX_HEADER).then_some(head.len())) else { return false };
    std::str::from_utf8(&head[..end]).ok().and_then(|line| serde_json::from_str::<Value>(line.trim()).ok()).is_some_and(|line| line["type"] == "transcript_start")
}

fn transcripts_under(root: &Path, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(root) else { return };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|entry| entry.path()).collect();
    paths.sort();
    for path in paths {
        if path.is_dir() {
            transcripts_under(&path, found);
        } else if is_transcript(&path) {
            found.push(path);
        }
    }
}

/// Every transcript under `root`: each file that starts with a transcript's header.
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

    const HEADER: &str = r#"{"type":"transcript_start","provider":"claude","protocol_version":1,"sdk_version":"0.3.300","model":"model-a","captured_at":"2026-11-15","scenario":"demo","origin":"written"}"#;

    fn rules(line: &str) -> Vec<&'static str> {
        scan_line(line, &["projectcanaryhorizon".into()])
    }

    fn found(line: &str) -> Vec<&'static str> {
        scan_transcript(line, &["projectcanaryhorizon".into(), "canary-branch".into()], &BTreeSet::new()).into_iter().map(|(_, rule)| rule).collect()
    }

    #[test]
    fn each_shape_of_a_leak_is_found() {
        assert_eq!(rules(r#"{"a":"eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0"}"#), vec!["secret"]);
        assert_eq!(rules("token sk-ant-canary0123456789abcdef"), vec!["secret"]);
        assert_eq!(rules("token ghp_canary0123456789abcdef"), vec!["secret"]);
        assert_eq!(rules("key AKIACANARY0123456789"), vec!["secret"]);
        assert_eq!(rules("Authorization: Bearer canarytoken123456"), vec!["secret"]);
        assert_eq!(rules("canary.person@canary-mail.io"), vec!["email"]);
        assert_eq!(rules("/Users/canaryman/work"), vec!["home-path"]);
        assert_eq!(rules("a1b2c3d4-0000-4000-8000-0123456789ab"), vec!["real-id"]);
        assert_eq!(rules(&"Q".repeat(44)), vec!["opaque-token"]);
        assert_eq!(rules("https://github.com/canary-org/canary-repo/pull/7"), vec!["url"]);
        assert_eq!(rules("ProjectCanaryHorizon"), vec!["private-literal"]);
    }

    #[test]
    fn the_demo_world_is_not_a_finding() {
        assert!(rules("ana@example.com /Users/ana/demo 00000000-0000-4000-8000-000000000007 30000000-0000-4000-8000-000000000001 «user text 1» sk-short https://example.com/a").is_empty());
        assert!(found(r#"{"cwd":"/Users/ana/demo","method":"item/agentMessage/delta","at":"2026-01-01T00:00:00Z","«key 1»":"«text 2»"}"#).is_empty());
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
    fn a_canary_in_a_key_is_found() {
        assert_eq!(found(r#"{"refactor the canary billing":1}"#), vec!["free-text"]);
        assert_eq!(found(r#"{"canary.person@canary-mail.io":{"a":1}}"#), vec!["email", "free-text"]);
        assert_eq!(found(r#"{"projectcanaryhorizon":1}"#), vec!["private-literal"]);
    }

    #[test]
    fn a_canary_with_the_shape_of_a_token_is_found() {
        assert_eq!(found(r#"{"pr":"https://github.com/canary-org/canary-repo/pull/7"}"#), vec!["url", "free-text"]);
        assert_eq!(found(r#"{"path":"/Volumes/CanaryDisk/notes"}"#), vec!["private-path", "free-text"]);
        assert_eq!(found(r#"{"path":"/opt/canary"}"#), vec!["private-path", "free-text"]);
        assert_eq!(found(r#"{"name":"AKIACANARY0123456789"}"#), vec!["secret"]);
        assert_eq!(found(r#"{"name":"canary-branch"}"#), vec!["private-literal"]);
        assert_eq!(found(r#"{"author":"canary.person@canary-mail.io"}"#), vec!["email", "free-text"]);
    }

    #[test]
    fn a_canary_behind_an_escape_is_found() {
        assert_eq!(found(r#"{"a":"\/Users\/canaryman\/x"}"#), vec!["home-path", "private-path", "free-text"]);
        assert_eq!(found(r#"{"a":"projectcanaryhoriz\u006fn"}"#), vec!["private-literal"]);
        assert_eq!(found(r#"{"a":"canary.person\u0040canary-mail.io"}"#), vec!["email", "free-text"]);
        assert_eq!(found(r#"{"a":"sk-ant-canary01234\u00356789abcdef"}"#), vec!["secret"]);
    }

    #[test]
    fn a_tree_reports_the_file_and_line_of_a_canary_without_its_text() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("case/transcripts")).unwrap();
        std::fs::write(directory.path().join("case/transcripts/a.ndjson"), format!("{HEADER}\n{{\"a\":\"«user text 1»\"}}\n{{\"a\":\"canary.person@canary-mail.io\"}}\n")).unwrap();
        let findings = check_tree(directory.path(), &[]);
        assert_eq!(findings, vec![Finding { file: "case/transcripts/a.ndjson".into(), line: 3, rule: "email" }, Finding { file: "case/transcripts/a.ndjson".into(), line: 3, rule: "free-text" }]);
        assert!(!findings[0].to_string().contains("canary"));
    }

    #[test]
    fn every_file_that_starts_with_a_header_is_a_transcript_whatever_its_name() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        std::fs::create_dir_all(root.join("deep/er")).unwrap();
        std::fs::write(root.join("a.ndjson"), format!("{HEADER}\n")).unwrap();
        std::fs::write(root.join("deep/b.jsonl"), format!("{HEADER}\n{{\"a\":\"canary.person@canary-mail.io\"}}\n")).unwrap();
        std::fs::write(root.join("deep/er/c"), format!("{HEADER}\n")).unwrap();
        std::fs::write(root.join("deep/notes.ndjson"), "{\"a\":1}\n").unwrap();
        std::fs::write(root.join("deep/big.bin"), vec![b'x'; MAX_HEADER * 2]).unwrap();
        assert_eq!(transcripts(root), vec![root.join("a.ndjson"), root.join("deep/b.jsonl"), root.join("deep/er/c")]);
        assert_eq!(check_tree(root, &[]).iter().map(|finding| (finding.file.as_str(), finding.rule)).collect::<Vec<_>>(), vec![("deep/b.jsonl", "email"), ("deep/b.jsonl", "free-text")]);
    }

    #[test]
    fn without_its_list_of_private_literals_the_guard_does_not_run() {
        let directory = tempfile::tempdir().unwrap();
        let list = directory.path().join("literals.txt");
        assert_eq!(literals_from(None), Err(LiteralsError::Unset));
        assert_eq!(literals_from(Some("".as_ref())), Err(LiteralsError::Unset));
        assert!(matches!(literals_from(Some(list.as_os_str())), Err(LiteralsError::Unreadable(_))));
        std::fs::write(&list, "# only a comment\n\n").unwrap();
        assert!(matches!(literals_from(Some(list.as_os_str())), Err(LiteralsError::Empty(_))));
        std::fs::write(&list, "# names\nCanaryHorizon\n").unwrap();
        assert_eq!(literals_from(Some(list.as_os_str())), Ok(vec!["canaryhorizon".to_owned()]));
        assert_eq!(literals_from(Some(NO_PRIVATE_LITERALS.as_ref())), Ok(Vec::new()), "a CI without a list says so");
    }
}
