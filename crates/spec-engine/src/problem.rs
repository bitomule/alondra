use crate::{Code, Problem};

pub(crate) const TOML_MAX_BYTES: usize = 65_536;
pub(crate) const TEXT_MAX_BYTES: usize = 262_144;
pub(crate) const RESULTS_MAX_BYTES: usize = 16_777_216;

pub(crate) fn problem(code: Code, path: &str, line: usize, message: impl Into<String>) -> Problem {
    Problem { code, path: path.to_string(), line, message: message.into() }
}

/// A `too_large` problem when the bytes are more than `max`.
pub(crate) fn too_large(path: &str, bytes: &[u8], max: usize) -> Option<Problem> {
    (bytes.len() > max)
        .then(|| problem(Code::TooLarge, path, 0, format!("the file has {} bytes, more than {max}", bytes.len())))
}

pub(crate) fn unreadable(path: &str) -> Problem {
    problem(Code::UnreadableFile, path, 0, "the file is listed but could not be read")
}

pub(crate) fn sort(problems: &mut Vec<Problem>) {
    problems.sort_by(|a, b| {
        (a.path.as_str(), a.line, a.code, a.message.as_str()).cmp(&(b.path.as_str(), b.line, b.code, b.message.as_str()))
    });
    problems.dedup();
}

pub(crate) fn finish<T>(value: T, mut problems: Vec<Problem>) -> Result<T, Vec<Problem>> {
    if problems.is_empty() {
        Ok(value)
    } else {
        sort(&mut problems);
        Err(problems)
    }
}

/// The line, from 1, where a TOML error starts, or 0 when the error has no position.
pub(crate) fn toml_line(source: &str, failure: &toml::de::Error) -> usize {
    match failure.span() {
        Some(span) => source.as_bytes().iter().take(span.start).filter(|byte| **byte == b'\n').count() + 1,
        None => 0,
    }
}

fn is_key(line: &str, key: &str) -> bool {
    line.split('=').next().map(str::trim) == Some(key)
}

fn is_header(line: &str) -> bool {
    line.trim_start().starts_with('[')
}

/// The line, from 1, of the first `key = …` line of a TOML text, or 0 when there is none.
pub(crate) fn key_line(source: &str, key: &str) -> usize {
    source.lines().position(|line| is_key(line, key)).map_or(0, |index| index + 1)
}

/// The line, from 1, of `key` in the `[table]` of a TOML text, or 0 when it is not written there.
pub(crate) fn table_key_line(source: &str, table: &str, key: &str) -> usize {
    let header = format!("[{table}]");
    let lines: Vec<&str> = source.lines().collect();
    let Some(start) = lines.iter().position(|line| line.trim() == header) else { return 0 };
    lines[start + 1..]
        .iter()
        .take_while(|line| !is_header(line))
        .position(|line| is_key(line, key))
        .map_or(0, |offset| start + offset + 2)
}

/// The line, from 1, of `key` in the `index`-th `[[array]]` table of a TOML text, or 0.
pub(crate) fn array_key_line(source: &str, array: &str, index: usize, key: &str) -> usize {
    let header = format!("[[{array}]]");
    let lines: Vec<&str> = source.lines().collect();
    let Some(start) = lines.iter().enumerate().filter(|(_, line)| line.trim() == header).map(|(at, _)| at).nth(index) else {
        return 0;
    };
    lines[start + 1..]
        .iter()
        .take_while(|line| !is_header(line))
        .position(|line| is_key(line, key))
        .map_or(0, |offset| start + offset + 2)
}

/// The line, from 1, of the first line that holds `value` as a TOML string, or 0.
pub(crate) fn value_line(source: &str, value: &str) -> usize {
    let quoted = serde_json::to_string(value).unwrap_or_default();
    source.lines().position(|line| line.contains(&quoted)).map_or(0, |index| index + 1)
}
