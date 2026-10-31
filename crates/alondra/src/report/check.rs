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
}

fn json(name: &str, text: &str, offset: usize, private: &Private, hits: &mut Vec<Hit>) {
    let root = serde_json::from_str::<Value>(text);
    let mut parser = JsonStrings { name, text, root: root.as_ref().ok(), at: 0, line: offset + 1, strings: Vec::new(), duplicate: false };
    let parsed = root.as_ref().is_ok_and(Value::is_object) && parser.value(&mut Vec::new(), name != "apps.jsonl").is_some() && !parser.duplicate;
    if parsed {
        for occurrence in parser.strings {
            if !occurrence.trusted {
                add_private(hits, name, &occurrence.value, occurrence.line, private);
            }
        }
    } else {
        hits.push(Hit { section: name.to_owned(), line: offset + 1, what: tr!("invalid report JSON", "JSON de informe no válido").to_owned() });
    }
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
                    self.strings.push(Occurrence { value: key.clone(), line, trusted });
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
                self.strings.push(Occurrence { value, line, trusted });
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
