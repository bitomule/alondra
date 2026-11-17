//! Reads the daemon's sources for the text of every `log!`/`eprintln!`/`writeln!` to stderr and the
//! words of the code:
//! the build writes them into the catalog the report redacts `alondrad.log` with. Plain `std`, so
//! the build script compiles it too.

/// The format string of every call to one of `macros` in `source`, in order, with its line.
pub fn calls(source: &str, macros: &[&str]) -> Vec<(usize, String)> {
    invocations(source, macros, Some)
}

/// The format string of every `write!`/`writeln!` to stderr in `source`, with its line: the daemon
/// writes some lines this way so that a full disk drops them instead of panicking.
pub fn stderr_writes(source: &str) -> Vec<(usize, String)> {
    invocations(source, &["writeln", "write"], |arguments| {
        let (target, format) = arguments.split_once(',')?;
        let to_stderr = target.contains("stderr") && target.chars().all(|c| c.is_alphanumeric() || c.is_whitespace() || "_:.()&".contains(c));
        to_stderr.then_some(format)
    })
}

fn invocations<'a>(source: &'a str, macros: &[&str], format_of: impl Fn(&'a str) -> Option<&'a str>) -> Vec<(usize, String)> {
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
        let Some(format) = format_of(&source[start..]) else { continue };
        let rest = format.trim_start();
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
pub const LOG_SOURCES: &[&str] =
    &["crates/alondrad/src", "crates/alondrad/launcher", "crates/daemon-support/src", "crates/daemon-engine/src", "crates/daemon-engine-model/src", "crates/daemon-engine-store/src", "crates/daemon-engine-machine/src", "crates/engine-admission/src", "crates/engine-github/src", "crates/engine-kernel/src", "crates/engine-orchestrator/src", "crates/engine-sdk/src", "crates/core/src",
        "crates/domain-rules/src", "crates/view-model/src", "crates/views/src", "crates/provider-claude/src"];

/// Functions that word a whole log line, which the daemon then logs as `log!("{line}")`: each of
/// their `format!` texts is a template too.
pub const LINE_BUILDERS: &[(&str, &str)] =
    &[("crates/engine-admission/src/admission.rs", "hold_line"), ("crates/engine-orchestrator/src/domain_orchestrator.rs", "account_wait_change")];

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
    "crates/daemon-support/src",
    "crates/daemon-engine/src", "crates/daemon-engine-model/src", "crates/daemon-engine-store/src", "crates/daemon-engine-machine/src", "crates/engine-admission/src", "crates/engine-github/src", "crates/engine-kernel/src", "crates/engine-orchestrator/src", "crates/engine-sdk/src",
    "crates/core/src",
    "crates/domain-rules/src",
    "crates/view-model/src",
    "crates/views/src",
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

/// Whether `file` is a test module by its name: `tests.rs`, `*_tests.rs`, anything under a `tests`
/// folder, or the test support every crate shares.
pub fn named_as_test(file: &std::path::Path) -> bool {
    let name = file.file_name().and_then(|name| name.to_str()).unwrap_or_default();
    name == "tests.rs" || name.ends_with("_tests.rs") || file.ends_with("crates/test_support.rs") || file.parent().is_some_and(|dir| dir.components().any(|part| part.as_os_str() == "tests"))
}

/// Whether a `#[cfg(...)]` condition holds only when tests are compiled, with or without the
/// `test-hooks` feature other crates' tests build with.
fn test_condition(condition: &str) -> bool {
    let hooks = "feature = \"test-hooks\"";
    condition == "test" || condition == hooks || condition == format!("any(test, {hooks})") || condition.starts_with("all(test,")
}

fn normalized(path: &std::path::Path) -> std::path::PathBuf {
    let mut out = std::path::PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir if out.file_name().is_some() => {
                out.pop();
            }
            part => out.push(part),
        }
    }
    out
}

/// The files of the `mod name;` items of `file`, each with whether a test-only `#[cfg]` gates it.
fn declared_modules(file: &std::path::Path, source: &str) -> Vec<(Vec<std::path::PathBuf>, bool)> {
    let dir = file.parent().unwrap_or(std::path::Path::new(""));
    let stem = file.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default();
    let base = if ["lib", "main", "mod"].contains(&stem) { dir.to_path_buf() } else { dir.join(stem) };
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let mut found = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let item = line.trim_start_matches("pub(crate) ").trim_start_matches("pub(super) ").trim_start_matches("pub ");
        let Some(name) = item.strip_prefix("mod ").and_then(|rest| rest.strip_suffix(';')) else { continue };
        if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let attributes = lines[..index].iter().rev().take_while(|line| line.starts_with("#["));
        let mut test = false;
        let mut path = None;
        for attribute in attributes {
            if let Some(condition) = attribute.strip_prefix("#[cfg(").and_then(|rest| rest.strip_suffix(")]")) {
                test |= test_condition(condition);
            } else if let Some(value) = attribute.strip_prefix("#[path = \"").and_then(|rest| rest.strip_suffix("\"]")) {
                path = Some(dir.join(value));
            }
        }
        let candidates = match path {
            Some(path) => vec![normalized(&path)],
            None => vec![normalized(&base.join(format!("{name}.rs"))), normalized(&base.join(name).join("mod.rs"))],
        };
        found.push((candidates, test));
    }
    found
}

/// The files among `files` that only tests compile: test modules by name, modules declared under
/// `#[cfg(test)]` or the `test-hooks` feature, and every module below one of them.
pub fn test_only(files: &[(std::path::PathBuf, String)]) -> std::collections::BTreeSet<std::path::PathBuf> {
    let present: std::collections::BTreeSet<std::path::PathBuf> = files.iter().map(|(file, _)| normalized(file)).collect();
    let mut edges = Vec::new();
    for (file, source) in files {
        for (candidates, test) in declared_modules(&normalized(file), source) {
            if let Some(child) = candidates.into_iter().find(|candidate| present.contains(candidate)) {
                edges.push((normalized(file), child, test));
            }
        }
    }
    let mut only: std::collections::BTreeSet<std::path::PathBuf> = present.iter().filter(|file| named_as_test(file)).cloned().collect();
    loop {
        let before = only.len();
        for (parent, child, test) in &edges {
            if *test || only.contains(parent) {
                only.insert(child.clone());
            }
        }
        if only.len() == before {
            return only;
        }
    }
}

/// One crate's part of the catalog from its production `files` (`origin`, source), in order: with
/// `log`, the line of every log call and the words of its fixed text; with `words`, the variants of
/// its enums and the `snake_case` words it quotes.
pub fn fragment(files: &[(String, String)], log: bool, words: bool) -> (String, std::collections::BTreeSet<String>) {
    let mut catalog = String::new();
    let mut vocabulary = std::collections::BTreeSet::new();
    for (origin, source) in files {
        if log {
            let macros: &[&str] = if origin.ends_with("src/log.rs") { &["log"] } else { &["log", "eprintln"] };
            let builders = LINE_BUILDERS.iter().filter(|(file, _)| file == origin).flat_map(|(_, function)| builder_formats(source, function));
            for (line, format) in calls(source, macros).into_iter().chain(stderr_writes(source)).chain(builders) {
                vocabulary.extend(fixed_words(&format));
                catalog.push_str(&catalog_line(&format!("{origin}:{line}"), &format));
                catalog.push('\n');
            }
        }
        if words {
            for variant in variants(source) {
                vocabulary.extend(spellings(&variant));
            }
            vocabulary.extend(quoted_snake_words(source));
        }
    }
    (catalog, vocabulary)
}

/// The catalog (`file:line<TAB>format`, one call per line) and the vocabulary (one word per line)
/// of the crates' fragments, in order.
pub fn join(fragments: impl IntoIterator<Item = (String, std::collections::BTreeSet<String>)>) -> (String, String) {
    let mut catalog = String::new();
    let mut vocabulary = std::collections::BTreeSet::new();
    for (origin, format) in BUILT_IN {
        catalog.push_str(&catalog_line(origin, format));
        catalog.push('\n');
    }
    for (part, words) in fragments {
        catalog.push_str(&part);
        vocabulary.extend(words);
    }
    let vocabulary: Vec<String> = vocabulary.into_iter().collect();
    (catalog, vocabulary.join("\n") + "\n")
}

/// The production sources under `dirs` of `root`, each with its origin relative to `root`.
pub fn production_files(root: &std::path::Path, dirs: &[&str]) -> Vec<(String, String)> {
    let files: Vec<(std::path::PathBuf, String)> = dirs
        .iter()
        .flat_map(|dir| rust_files(&root.join(dir)))
        .filter_map(|file| Some((file.strip_prefix(root).unwrap_or(&file).to_path_buf(), std::fs::read_to_string(&file).ok()?)))
        .collect();
    let tests = test_only(&files);
    files.into_iter().filter(|(file, _)| !tests.contains(&normalized(file))).map(|(file, source)| (file.to_string_lossy().into_owned(), source)).collect()
}

/// The catalog and vocabulary of the production sources under `root`, as Bazel joins them from the
/// crates' fragments.
pub fn build(root: &std::path::Path) -> (String, String) {
    join([fragment(&production_files(root, LOG_SOURCES), true, false), fragment(&production_files(root, VOCABULARY_SOURCES), false, true)])
}
