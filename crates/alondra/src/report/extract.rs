//! Reads the daemon's sources for the text of every `log!`/`eprintln!` and the words of the code:
//! the build writes them into the catalog the report redacts `alondrad.log` with. Plain `std`, so
//! the build script compiles it too.

/// The format string of every call to one of `macros` in `source`, in order, with its line.
pub fn calls(source: &str, macros: &[&str]) -> Vec<(usize, String)> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let Some(name) = macros.iter().find(|name| source[at..].starts_with(&format!("{name}!("))) else {
            at += source[at..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let preceded_by_identifier = source[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
        let call_line = source[..at].matches('\n').count() + 1;
        let start = at + name.len() + 2;
        at = start;
        if preceded_by_identifier {
            continue;
        }
        let rest = source[start..].trim_start();
        let literal_at = source.len() - rest.len();
        if let Some((text, length)) = string_literal(rest) {
            found.push((call_line, text));
            at = literal_at + length;
        }
    }
    found
}

/// A Rust string literal at the start of `text` (plain or raw), unescaped, and its length.
fn string_literal(text: &str) -> Option<(String, usize)> {
    if let Some(raw) = text.strip_prefix('r') {
        let hashes = raw.len() - raw.trim_start_matches('#').len();
        let body = raw[hashes..].strip_prefix('"')?;
        let close = format!("\"{}", "#".repeat(hashes));
        let end = body.find(&close)?;
        return Some((body[..end].to_owned(), 1 + hashes + 1 + end + close.len()));
    }
    let body = text.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = body.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        match c {
            '"' => return Some((out, at + 2)),
            '\\' => match chars.next()?.1 {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                '0' => out.push('\0'),
                '\\' => out.push('\\'),
                '"' => out.push('"'),
                '\'' => out.push('\''),
                '\n' => {
                    while chars.peek().is_some_and(|(_, c)| c.is_whitespace()) {
                        chars.next();
                    }
                }
                'u' => {
                    let mut hex = String::new();
                    for (_, c) in chars.by_ref() {
                        match c {
                            '{' => {}
                            '}' => break,
                            c => hex.push(c),
                        }
                    }
                    out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                }
                other => {
                    out.push('\\');
                    out.push(other);
                }
            },
            c => out.push(c),
        }
    }
    None
}

/// The names of the variants of every `enum` in `source`.
pub fn variants(source: &str) -> Vec<String> {
    let source = without_line_comments(source);
    let mut found = Vec::new();
    let mut rest = source.as_str();
    while let Some(at) = rest.find("enum ") {
        let preceded_by_identifier = rest[..at].chars().next_back().is_some_and(|c| c.is_alphanumeric() || c == '_');
        rest = &rest[at + 5..];
        if preceded_by_identifier {
            continue;
        }
        let Some(open) = rest.find('{') else { break };
        if rest[..open].contains([';', '(', '=']) {
            continue;
        }
        let mut depth = 0usize;
        let mut close = rest.len();
        let mut expect_variant = true;
        let mut word = String::new();
        let mut in_attribute = 0usize;
        for (index, c) in rest[open..].char_indices() {
            match c {
                '#' if depth == 1 => in_attribute = 1,
                '[' if in_attribute > 0 => in_attribute += 1,
                ']' if in_attribute > 0 => {
                    in_attribute -= 1;
                    if in_attribute == 1 {
                        in_attribute = 0;
                    }
                }
                _ if in_attribute > 0 => {}
                '{' | '(' => {
                    if depth == 1 && !word.is_empty() {
                        found.push(std::mem::take(&mut word));
                    }
                    expect_variant = depth == 0;
                    depth += 1;
                }
                '}' | ')' => {
                    if depth == 1 && !word.is_empty() {
                        found.push(std::mem::take(&mut word));
                    }
                    depth -= 1;
                    if depth == 0 {
                        close = open + index;
                        break;
                    }
                }
                ',' if depth == 1 => {
                    if !word.is_empty() {
                        found.push(std::mem::take(&mut word));
                    }
                    expect_variant = true;
                }
                '=' if depth == 1 => {
                    if !word.is_empty() {
                        found.push(std::mem::take(&mut word));
                    }
                    expect_variant = false;
                }
                c if depth == 1 && expect_variant && (c.is_alphanumeric() || c == '_') => word.push(c),
                c if depth == 1 && c.is_whitespace() && !word.is_empty() => {
                    found.push(std::mem::take(&mut word));
                    expect_variant = false;
                }
                _ => {}
            }
        }
        rest = &rest[close.min(rest.len())..];
    }
    found.retain(|word| word.chars().next().is_some_and(char::is_uppercase));
    found
}

fn without_line_comments(source: &str) -> String {
    source.lines().map(|line| line.split_once("//").map_or(line, |(code, _)| code)).collect::<Vec<_>>().join("\n")
}

/// The fixed text of a format string, without its `{…}` holes.
pub fn fixed_words(format: &str) -> Vec<String> {
    let mut text = String::new();
    let mut depth = 0;
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if depth == 0 && chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '{' => {
                depth += 1;
                text.push(' ');
            }
            '}' if depth > 0 => depth -= 1,
            c if depth == 0 => text.push(c),
            _ => {}
        }
    }
    words(&text)
}

/// The words of `text`: runs of letters, digits, `_` and `-`, trimmed of a trailing `-`.
pub fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
        .map(|word| word.trim_matches('-'))
        .filter(|word| word.chars().any(char::is_alphabetic))
        .map(str::to_owned)
        .collect()
}

/// The `snake_case` words the code writes between quotes, such as the reasons it stores in SQL.
pub fn quoted_snake_words(source: &str) -> Vec<String> {
    let segments: Vec<&str> = source.split(['"', '\'']).collect();
    segments
        .iter()
        .skip(1)
        .take(segments.len().saturating_sub(2))
        .filter(|word| word.contains('_') && word.split('_').all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_lowercase())))
        .map(|word| (*word).to_owned())
        .collect()
}

/// `HostEnded` also as `hostended`, `host_ended`, `host-ended` and `host ended`'s words: the forms
/// serde, `Debug` and `Display` print a variant in.
pub fn spellings(variant: &str) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    for c in variant.chars() {
        if c.is_uppercase() || parts.is_empty() || c == '_' {
            parts.push(String::new());
        }
        if c != '_' {
            parts.last_mut().expect("a part").extend(c.to_lowercase());
        }
    }
    parts.retain(|part| !part.is_empty());
    let mut out = vec![variant.to_owned(), variant.to_lowercase(), parts.join("_"), parts.join("-")];
    let mut upper = parts.clone();
    if let Some(first) = upper.first_mut() {
        *first = first.chars().next().map(|c| c.to_uppercase().chain(first.chars().skip(1)).collect()).unwrap_or_default();
    }
    out.extend(parts);
    out.extend(upper.into_iter().take(1));
    out.sort();
    out.dedup();
    out
}

/// One catalog line: where the call is and its format string, escaped so it fits on one line.
pub fn catalog_line(origin: &str, format: &str) -> String {
    format!("{origin}\t{}", escape(format))
}

pub fn parse_catalog_line(line: &str) -> Option<(&str, String)> {
    let (origin, format) = line.split_once('\t')?;
    Some((origin, unescape(format)))
}

fn escape(text: &str) -> String {
    text.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t").replace('\r', "\\r")
}

fn unescape(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// Where the catalog's calls are, relative to the workspace root: the daemon, its launcher, and the
/// crates it runs whose `eprintln!` also land in `alondrad.log`.
pub const LOG_SOURCES: &[&str] = &["crates/alondrad/src", "crates/alondrad/launcher", "crates/core/src", "crates/provider-claude/src"];

/// Functions that word a whole log line, which the daemon then logs as `log!("{line}")`: each of
/// their `format!` texts is a template too.
pub const LINE_BUILDERS: &[(&str, &str)] =
    &[("crates/alondrad/src/admission.rs", "hold_line"), ("crates/alondrad/src/domain_orchestrator.rs", "account_wait_change")];

/// The `format!` texts in the body of `function`, with their line in `source`.
pub fn builder_formats(source: &str, function: &str) -> Vec<(usize, String)> {
    let Some(at) = source.find(&format!("fn {function}(")) else { return Vec::new() };
    let line_start = source[..at].rfind('\n').map_or(0, |i| i + 1);
    let indent: String = source[line_start..].chars().take_while(|c| *c == ' ').collect();
    let end = source[at..].find(&format!("\n{indent}}}\n")).map_or(source.len(), |i| at + i);
    let offset = source[..line_start].matches('\n').count();
    calls(&source[line_start..end], &["format"]).into_iter().map(|(line, text)| (line + offset, text)).collect()
}

/// What `main` prints when it returns an error, from the standard library.
pub const BUILT_IN: &[(&str, &str)] = &[("std:main", "Error: {}")];

/// The crates whose enum variants are words of the code a log value may be made of.
pub const VOCABULARY_SOURCES: &[&str] = &[
    "crates/alondrad/src",
    "crates/core/src",
    "crates/domain/src",
    "crates/foundation/src",
    "crates/store/src",
    "crates/agent-application/src",
    "crates/agent-protocol/src",
    "crates/agent-runtime/src",
    "crates/provider-claude/src",
    "crates/provider-contract/src",
    "crates/process-group/src",
];

/// The `.rs` files under `dir`, sorted, so the catalog is the same on every build.
pub fn rust_files(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The catalog (`file:line<TAB>format`, one call per line) and the vocabulary (one word per line)
/// of the sources under `root`.
pub fn build(root: &std::path::Path) -> (String, String) {
    let mut catalog = String::new();
    let mut vocabulary = std::collections::BTreeSet::new();
    for (origin, format) in BUILT_IN {
        catalog.push_str(&catalog_line(origin, format));
        catalog.push('\n');
    }
    for dir in LOG_SOURCES {
        for file in rust_files(&root.join(dir)) {
            let Ok(source) = std::fs::read_to_string(&file) else { continue };
            let origin = file.strip_prefix(root).unwrap_or(&file).to_string_lossy().into_owned();
            let macros: &[&str] = if origin.ends_with("src/log.rs") { &["log"] } else { &["log", "eprintln"] };
            let builders = LINE_BUILDERS.iter().filter(|(file, _)| *file == origin).flat_map(|(_, function)| builder_formats(&source, function));
            for (line, format) in calls(&source, macros).into_iter().chain(builders) {
                vocabulary.extend(fixed_words(&format));
                catalog.push_str(&catalog_line(&format!("{origin}:{line}"), &format));
                catalog.push('\n');
            }
        }
    }
    for dir in VOCABULARY_SOURCES {
        for file in rust_files(&root.join(dir)) {
            let Ok(source) = std::fs::read_to_string(&file) else { continue };
            for variant in variants(&source) {
                vocabulary.extend(spellings(&variant));
            }
            vocabulary.extend(quoted_snake_words(&source));
        }
    }
    let vocabulary: Vec<String> = vocabulary.into_iter().collect();
    (catalog, vocabulary.join("\n") + "\n")
}
