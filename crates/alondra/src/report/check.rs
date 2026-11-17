//! Private data is checked at its occurrence, separately from the code-owned report structure.
//! Secret shapes are still checked across the complete original files.
mod schema;

use std::collections::HashSet;

use serde_json::Value;

use super::canary::{self, Hit, Private};
use super::templates::Catalog;
use super::{bundle, collect, redact};

pub(super) fn section(name: &str, text: &str, private: &Private) -> Vec<Hit> {
    let mut hits = canary::scan(name, text, None);
    if name == bundle::DESCRIPTION {
        return hits;
    }
    match name {
        "daemon.log" => {
            for (index, line) in text.lines().enumerate() {
                let message = line.split_once(' ').filter(|(stamp, _)| redact::is_iso_time(stamp.strip_prefix('~').unwrap_or(stamp))).map_or(line, |(_, message)| message);
                for value in redact::free_log_values(Catalog::built(), message) {
                    add_private(&mut hits, name, value, index + 1, private);
                }
            }
        }
        "machine-samples.csv" => samples(name, text, private, &mut hits),
        _ if name.ends_with(".json") => json(name, text, 0, private, &mut hits),
        _ if name.ends_with(".jsonl") => {
            for (index, line) in text.lines().enumerate() {
                json(name, line, index, private, &mut hits);
            }
        }
        _ => hits.extend(canary::private_hits(name, text, private)),
    }
    hits.sort_by_key(|hit| hit.line);
    hits.dedup_by_key(|hit| hit.line);
    hits
}

fn add_private(hits: &mut Vec<Hit>, section: &str, value: &str, line: usize, private: &Private) {
    hits.extend(canary::scan(section, value, Some(private)).into_iter().map(|mut hit| {
        hit.line = line;
        hit
    }));
}

fn samples(name: &str, text: &str, private: &Private, hits: &mut Vec<Hit>) {
    let mut lines = text.lines();
    if lines.next() != Some(collect::SAMPLE_COLUMNS.join(",").as_str()) {
        hits.extend(canary::private_hits(name, text, private));
        return;
    }
    for (index, line) in lines.enumerate() {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() != collect::SAMPLE_COLUMNS.len() {
            add_private(hits, name, line, index + 2, private);
            continue;
        }
        for (column, value) in collect::SAMPLE_COLUMNS.iter().zip(fields) {
            let fixed = matches!((*column, value), ("health", "room" | "no_room") | ("memory_pressure", "normal" | "warning" | "critical") | ("cpu_busy", "true" | "false"))
                || (*column == "at" && redact::is_iso_time(value))
                || redact::is_pseudonym(value);
            if !fixed {
                add_private(hits, name, value, index + 2, private);
            }
        }
    }
}

struct Occurrence {
    value: String,
    line: usize,
    trusted: bool,
    field: String,
    path: Vec<String>,
    key: bool,
}

/// Every string of a JSON object with whether the report's structure vouches for it, or `None` when
/// the text is not one object with unique keys.
fn occurrences(name: &str, text: &str, offset: usize) -> Option<(Value, Vec<Occurrence>)> {
    let root = serde_json::from_str::<Value>(text).ok().filter(Value::is_object)?;
    let mut parser = JsonStrings { name, text, root: Some(&root), at: 0, line: offset + 1, strings: Vec::new(), duplicate: false };
    parser.value(&mut Vec::new(), name != "apps.jsonl")?;
    let (strings, duplicate) = (std::mem::take(&mut parser.strings), parser.duplicate);
    (!duplicate).then_some((root, strings))
}

fn json(name: &str, text: &str, offset: usize, private: &Private, hits: &mut Vec<Hit>) {
    if let Some((_, strings)) = occurrences(name, text, offset) {
        for occurrence in strings {
            if !occurrence.trusted {
                let before = hits.len();
                add_private(hits, name, &occurrence.value, occurrence.line, private);
                let field = &occurrence.field;
                for hit in &mut hits[before..] {
                    let what = hit.what.clone();
                    hit.what = trf!("{what}, in the field {field}", "{what}, en el campo {field}");
                }
            }
        }
    } else {
        hits.push(Hit { section: name.to_owned(), line: offset + 1, what: tr!("invalid report JSON", "JSON de informe no válido").to_owned() });
    }
}

/// What a section's value or line became after masking: where it was and which field held it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Masked {
    pub line: usize,
    pub field: String,
}

/// The field recorded for a line masked whole, when no single field of it could be masked.
pub(super) const WHOLE_LINE: &str = "(line)";

/// The section with every value the final check would flag replaced by `MASKED`: a field when the
/// structure allows it, else its whole line. A single JSON document cannot lose a line, so what is
/// left there still fails the check.
pub(super) fn mask(name: &str, text: &str, private: &Private) -> (String, Vec<Masked>) {
    let (mut text, mut masked) = if name == "machine-samples.csv" {
        mask_samples(text, private)
    } else if name.ends_with(".json") {
        return mask_json(name, text, 0, private).unwrap_or_else(|| (text.to_owned(), Vec::new()));
    } else if name.ends_with(".jsonl") {
        let mut masked = Vec::new();
        let lines: Vec<String> = text
            .lines()
            .enumerate()
            .map(|(index, line)| match mask_json(name, line, index, private) {
                Some((row, fields)) => {
                    masked.extend(fields);
                    row
                }
                None => line.to_owned(),
            })
            .collect();
        (if masked.is_empty() { text.to_owned() } else { joined(&lines) }, masked)
    } else {
        (text.to_owned(), Vec::new())
    };
    let flagged: HashSet<usize> = section(name, &text, private).iter().map(|hit| hit.line).collect();
    if !flagged.is_empty() {
        masked.retain(|m| !flagged.contains(&m.line));
        let lines: Vec<String> = text
            .lines()
            .enumerate()
            .map(|(index, line)| {
                if !flagged.contains(&(index + 1)) {
                    return line.to_owned();
                }
                masked.push(Masked { line: index + 1, field: WHOLE_LINE.to_owned() });
                whole_line_masked(name, line)
            })
            .collect();
        text = joined(&lines);
        masked.sort_by(|a, b| (a.line, &a.field).cmp(&(b.line, &b.field)));
    }
    (text, masked)
}

fn joined(lines: &[String]) -> String {
    lines.iter().map(|line| line.clone() + "\n").collect()
}

fn whole_line_masked(name: &str, line: &str) -> String {
    let masked = super::MASKED;
    if name.ends_with(".jsonl") {
        return "{}".to_owned();
    }
    match line.split_once(' ') {
        Some((stamp, _)) if name == "daemon.log" && redact::is_iso_time(stamp.strip_prefix('~').unwrap_or(stamp)) => format!("{stamp} {masked}"),
        _ => masked.to_owned(),
    }
}

/// The JSON text with each value the check flags masked, and where; `None` when nothing changed or
/// the text cannot be read as one object.
fn mask_json(name: &str, text: &str, offset: usize, private: &Private) -> Option<(String, Vec<Masked>)> {
    let (mut root, strings) = occurrences(name, text, offset)?;
    let mut masked = Vec::new();
    for occurrence in strings.iter().filter(|o| !o.trusted && !o.key && o.value != super::MASKED) {
        if canary::scan(name, &occurrence.value, Some(private)).is_empty() {
            continue;
        }
        let pointer: String = occurrence.path.iter().map(|part| format!("/{}", part.replace('~', "~0").replace('/', "~1"))).collect();
        if let Some(value) = root.pointer_mut(&pointer) {
            *value = Value::from(super::MASKED);
            masked.push(Masked { line: occurrence.line, field: occurrence.field.clone() });
        }
    }
    if masked.is_empty() {
        return None;
    }
    let rewritten = if name.ends_with(".jsonl") { root.to_string() } else { serde_json::to_string_pretty(&root).ok()? + "\n" };
    Some((rewritten, masked))
}

fn mask_samples(text: &str, private: &Private) -> (String, Vec<Masked>) {
    let mut lines = text.lines();
    if lines.next() != Some(collect::SAMPLE_COLUMNS.join(",").as_str()) {
        return (text.to_owned(), Vec::new());
    }
    let mut masked = Vec::new();
    let mut out = vec![collect::SAMPLE_COLUMNS.join(",")];
    for (index, line) in lines.enumerate() {
        let mut fields: Vec<String> = line.split(',').map(str::to_owned).collect();
        if fields.len() == collect::SAMPLE_COLUMNS.len() {
            for (column, field) in collect::SAMPLE_COLUMNS.iter().zip(fields.iter_mut()) {
                let mut hits = Vec::new();
                add_private(&mut hits, "machine-samples.csv", field, index + 2, private);
                if !hits.is_empty() {
                    *field = super::MASKED.to_owned();
                    masked.push(Masked { line: index + 2, field: (*column).to_owned() });
                }
            }
        }
        out.push(fields.join(","));
    }
    (if masked.is_empty() { text.to_owned() } else { joined(&out) }, masked)
}

fn field_of(path: &[String], key: &str) -> String {
    let mut parts: Vec<&str> = path.iter().map(String::as_str).collect();
    if !key.is_empty() {
        parts.push(key);
    }
    if parts.is_empty() { "(root)".to_owned() } else { parts.join(".") }
}

struct JsonStrings<'a> {
    name: &'a str,
    text: &'a str,
    root: Option<&'a Value>,
    at: usize,
    line: usize,
    strings: Vec<Occurrence>,
    duplicate: bool,
}

impl JsonStrings<'_> {
    fn whitespace(&mut self) {
        while self.text.as_bytes().get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.line += usize::from(self.text.as_bytes()[self.at] == b'\n');
            self.at += 1;
        }
    }

    fn take(&mut self, byte: u8) -> Option<()> {
        self.whitespace();
        if self.text.as_bytes().get(self.at) != Some(&byte) {
            return None;
        }
        self.at += 1;
        Some(())
    }

    fn string(&mut self) -> Option<(String, usize)> {
        self.whitespace();
        let (start, line) = (self.at, self.line);
        self.take(b'"')?;
        while let Some(byte) = self.text.as_bytes().get(self.at).copied() {
            self.at += 1;
            match byte {
                b'\\' => self.at += 1,
                b'"' => return Some((serde_json::from_str(self.text.get(start..self.at)?).ok()?, line)),
                _ => {}
            }
        }
        None
    }

    fn value(&mut self, path: &mut Vec<String>, known: bool) -> Option<()> {
        self.whitespace();
        match *self.text.as_bytes().get(self.at)? {
            b'{' => {
                self.at += 1;
                let mut keys = HashSet::new();
                self.whitespace();
                if self.text.as_bytes().get(self.at) == Some(&b'}') {
                    self.at += 1;
                    return Some(());
                }
                loop {
                    let (key, line) = self.string()?;
                    if !keys.insert(key.clone()) {
                        self.duplicate = true;
                    }
                    let refs: Vec<&str> = path.iter().map(String::as_str).collect();
                    let trusted = known && self.root.is_some_and(|root| schema::key(self.name, &refs, &key, root));
                    let field = field_of(path, &key);
                    self.strings.push(Occurrence { value: key.clone(), line, trusted, field, path: path.clone(), key: true });
                    self.take(b':')?;
                    path.push(key);
                    self.value(path, trusted)?;
                    path.pop();
                    self.whitespace();
                    if self.text.as_bytes().get(self.at) == Some(&b'}') {
                        self.at += 1;
                        break;
                    }
                    self.take(b',')?;
                }
            }
            b'[' => {
                self.at += 1;
                self.whitespace();
                if self.text.as_bytes().get(self.at) == Some(&b']') {
                    self.at += 1;
                    return Some(());
                }
                for index in 0usize.. {
                    path.push(index.to_string());
                    self.value(path, known)?;
                    path.pop();
                    self.whitespace();
                    if self.text.as_bytes().get(self.at) == Some(&b']') {
                        self.at += 1;
                        break;
                    }
                    self.take(b',')?;
                }
            }
            b'"' => {
                let (value, line) = self.string()?;
                let refs: Vec<&str> = path.iter().map(String::as_str).collect();
                let trusted = known && (redact::is_pseudonym(&value) || self.root.is_some_and(|root| schema::value(self.name, &refs, &value, root)));
                let field = field_of(path, "");
                self.strings.push(Occurrence { value, line, trusted, field, path: path.clone(), key: false });
            }
            _ => {
                let length = self.text[self.at..].find(|c: char| c.is_ascii_whitespace() || matches!(c, ',' | ']' | '}')).unwrap_or(self.text.len() - self.at);
                if length == 0 {
                    return None;
                }
                self.at += length;
            }
        }
        Some(())
    }
}
