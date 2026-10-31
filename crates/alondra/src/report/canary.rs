//! The last check before a report may leave the Mac: none of this user's own private values and
//! none of the known shapes of a secret, an address or an email anywhere in it. It is the safety
//! net under the redaction, never a replacement for it.
use std::collections::HashSet;

use base64::Engine;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub section: String,
    pub line: usize,
    pub what: String,
}

/// What the check looks for besides the fixed shapes: values of this user, read from their Mac.
#[derive(Debug, Default)]
pub struct Private {
    values: Vec<(String, Vec<String>, &'static str)>,
}

impl Private {
    /// Adds a value; one that is too short to tell apart, or made only of words of the code, says
    /// nothing the code does not.
    pub fn add(&mut self, what: &'static str, value: &str, vocabulary: &HashSet<String>) {
        let value = value.trim();
        let of_the_code = value.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).all(|w| vocabulary.contains(w) || vocabulary.contains(&w.to_lowercase()));
        if value.chars().count() < 4 || of_the_code || value.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return;
        }
        let lowered = value.to_lowercase();
        if self.values.iter().any(|(v, _, _)| *v == lowered) {
            return;
        }
        let words = word_set(&lowered).into_iter().collect();
        self.values.push((lowered, words, what));
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }
}

fn word_set(text: &str) -> HashSet<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect()
}

/// Every hit in `text`, with its line. `private` is skipped for text the user wrote themselves.
pub fn scan(section: &str, text: &str, private: Option<&Private>) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for what in shapes(line) {
            hits.push(Hit { section: section.to_owned(), line: index + 1, what: what.to_owned() });
        }
    }
    let Some(private) = private else { return hits };
    let lowered = text.to_lowercase();
    let present = word_set(&lowered);
    for (value, words, what) in &private.values {
        if !words.iter().all(|w| present.contains(w)) {
            continue;
        }
        let mut from = 0;
        while let Some(found) = lowered[from..].find(value.as_str()) {
            let at = from + found;
            let end = at + value.len();
            from = end;
            let before = lowered[..at].chars().next_back();
            let after = lowered[end..].chars().next();
            let bounded = |edge: Option<char>, inner: Option<char>| {
                !(edge.is_some_and(char::is_alphanumeric) && inner.is_some_and(char::is_alphanumeric))
            };
            if bounded(before, value.chars().next()) && bounded(after, value.chars().next_back()) {
                let line = lowered[..at].matches('\n').count() + 1;
                hits.push(Hit { section: section.to_owned(), line, what: (*what).to_owned() });
                break;
            }
        }
    }
    hits.sort_by_key(|hit| hit.line);
    hits.dedup_by_key(|hit| hit.line);
    hits
}

/// The shapes of a secret, an email or an address in one line.
pub fn shapes(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    let lowered = line.to_ascii_lowercase();
    for (prefix, what) in [
        ("sk-ant-", "an Anthropic key"),
        ("sk-proj-", "a key"),
        ("sk_live_", "a key"),
        ("rk_live_", "a key"),
        ("glpat-", "a key"),
        ("npm_", "a key"),
        ("ghp_", "a GitHub token"),
        ("gho_", "a GitHub token"),
        ("ghu_", "a GitHub token"),
        ("ghs_", "a GitHub token"),
        ("github_pat_", "a GitHub token"),
        ("-----begin", "a key"),
        ("authorization:", "an Authorization header"),
        ("bearer ", "an Authorization header"),
    ] {
        if lowered.contains(prefix) {
            found.push(what);
        }
    }
    if lowered.as_bytes().windows(5).any(|s| s.starts_with(b"xox") && s[3].is_ascii_alphabetic() && s[4] == b'-')
        || ["akia", "aiza"].iter().any(|prefix| has_marker(&lowered, prefix, |_| true))
        || has_marker(&lowered, "x-api-key", |tail| tail.trim_start().starts_with([':', '=']))
    {
        found.push("a key");
    }
    if has_marker(&lowered, "cookie", |tail| tail.trim_start().starts_with([':', '='])) {
        found.push("a Cookie header");
    }
    if has_marker(&lowered, "authorization", |tail| tail.trim_start().starts_with([':', '=']))
        || has_marker(&lowered, "bearer", |tail| tail.starts_with(auth_separator))
        || has_marker(&lowered, "basic", |tail| basic_credentials(&line[line.len() - tail.len()..]))
    {
        found.push("an Authorization header");
    }
    if has_jwt(line) {
        found.push("a JWT");
    }
    if has_email(line) {
        found.push("an email");
    }
    if has_ipv4(line) {
        found.push("an IP address");
    }
    if has_ipv6(line) {
        found.push("an IP address");
    }
    found.dedup();
    found
}

fn auth_separator(c: char) -> bool {
    c.is_ascii_whitespace() || matches!(c, ':' | '=')
}

fn basic_credentials(tail: &str) -> bool {
    if !tail.starts_with(auth_separator) {
        return false;
    }
    let token = tail.trim_start_matches(auth_separator).split(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))).next().unwrap_or_default();
    base64::engine::general_purpose::STANDARD
        .decode(token)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(token))
        .is_ok_and(|bytes| bytes.contains(&b':'))
}

fn has_marker(line: &str, marker: &str, followed_by: impl Fn(&str) -> bool) -> bool {
    line.match_indices(marker).any(|(at, _)| {
        let before = line[..at].chars().next_back();
        !before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-')) && followed_by(&line[at + marker.len()..])
    })
}

fn has_jwt(line: &str) -> bool {
    let base64url = |c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_';
    line.match_indices("eyJ").any(|(at, _)| {
        let segments: Vec<&str> = line[at..].split(|c: char| !(base64url(c) || c == '.')).next().unwrap_or("").split('.').collect();
        segments.len() >= 2 && segments[0].len() >= 10 && segments[1].len() >= 2
    })
}

fn has_email(line: &str) -> bool {
    let local = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-');
    let domain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-');
    line.match_indices('@').any(|(at, _)| {
        let before = line[..at].chars().rev().take_while(|c| local(*c)).count();
        let host: String = line[at + 1..].chars().take_while(|c| domain(*c)).collect();
        let host = host.trim_end_matches('.');
        let tld = host.rsplit('.').next().unwrap_or("");
        before > 0 && host.contains('.') && tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic())
    })
}

fn has_ipv4(line: &str) -> bool {
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if !bytes[at].is_ascii_digit() || (at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'.')) {
            at += 1;
            continue;
        }
        let run_end = line[at..].find(|c: char| !(c.is_ascii_digit() || c == '.')).map_or(line.len(), |e| at + e);
        let run = line[at..run_end].trim_end_matches('.');
        let parts: Vec<&str> = run.split('.').collect();
        let after = line[at + run.len()..].chars().next();
        if parts.len() == 4
            && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u16>().is_ok_and(|n| n <= 255))
            && !after.is_some_and(|c| c.is_ascii_alphanumeric())
        {
            return true;
        }
        at = run_end.max(at + 1);
    }
    false
}

fn has_ipv6(line: &str) -> bool {
    let candidate = |c: char| c.is_ascii_hexdigit() || c == ':';
    let mut rest = line;
    let mut offset = 0;
    while let Some(start) = rest.find(candidate) {
        let begin = offset + start;
        let len = line[begin..].find(|c: char| !candidate(c)).unwrap_or(line.len() - begin);
        let run = &line[begin..begin + len];
        let before = line[..begin].chars().next_back();
        let after = line[begin + len..].chars().next();
        let isolated = !before.is_some_and(|c| c.is_alphanumeric() || c == '_') && !after.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let groups: Vec<&str> = run.split(':').collect();
        let hex_groups = groups.iter().filter(|g| !g.is_empty()).count();
        let well_formed = groups.iter().all(|g| g.len() <= 4);
        let looks_like_address = (run.contains("::") && hex_groups >= 1 && run.chars().any(|c| c.is_ascii_hexdigit()) && run.len() >= 3)
            || groups.len() == 8;
        if isolated && well_formed && looks_like_address && !run.contains(":::") {
            return true;
        }
        offset = begin + len.max(1);
        rest = &line[offset..];
    }
    false
}
