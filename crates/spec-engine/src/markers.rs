use crate::ident::split_subject;
use crate::problem::problem;
use crate::{Code, Marker, Problem};

const MARKER_TOKEN: &str = "spec:";
const SEARCH_LINES: usize = 8;

enum Found<'a> {
    Valid(&'a str),
    Invalid,
}

/// What follows a comment opener on a line, when the line is a comment.
fn comment(line: &str) -> Option<&str> {
    let rest = line.trim_start();
    rest.strip_prefix("///").or_else(|| rest.strip_prefix("//")).or_else(|| rest.strip_prefix('#'))
}

fn marker(line: &str) -> Option<Found<'_>> {
    let mut words = comment(line)?.split_whitespace();
    if words.next() != Some(MARKER_TOKEN) {
        return None;
    }
    let (Some(scenario), None) = (words.next(), words.next()) else {
        return Some(Found::Invalid);
    };
    Some(if split_subject(scenario).is_some() { Found::Valid(scenario) } else { Found::Invalid })
}

pub(crate) fn read(path: &str, source: &str) -> (Vec<Marker>, Vec<Problem>) {
    let lines: Vec<&str> = source.lines().collect();
    let mut markers = Vec::new();
    let mut problems = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        if marker(lines[index]).is_none() {
            index += 1;
            continue;
        }
        let start = index;
        while index < lines.len() && marker(lines[index]).is_some() {
            index += 1;
        }
        let test = lines[index..].iter().take(SEARCH_LINES).find_map(|line| declared_test(line));
        for (offset, line) in lines[start..index].iter().enumerate() {
            match marker(line) {
                Some(Found::Valid(scenario)) => markers.push(Marker {
                    path: path.to_string(),
                    line: start + offset + 1,
                    scenario: scenario.to_string(),
                    test: test.clone(),
                }),
                _ => problems.push(problem(
                    Code::InvalidMarker,
                    path,
                    start + offset + 1,
                    "a marker names one scenario as <spec>/<scenario>",
                )),
            }
        }
    }
    (markers, problems)
}

fn is_name_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The name of the test a line declares: `fn`, `func` or `def` followed by a name and `(`, or the
/// first quoted text of a `test(` or `it(` call.
fn declared_test(line: &str) -> Option<String> {
    let words: Vec<&str> = line.split_whitespace().collect();
    for pair in words.windows(2) {
        if matches!(pair[0], "fn" | "func" | "def") {
            let name: String = pair[1].chars().take_while(|c| is_name_char(*c)).collect();
            let after = pair[1][name.len()..].chars().next();
            if !name.is_empty() && matches!(after, Some('(' | '<')) {
                return Some(name);
            }
        }
    }
    call_name(line, "test(").or_else(|| call_name(line, "it("))
}

fn call_name(line: &str, call: &str) -> Option<String> {
    let mut from = 0;
    while let Some(offset) = line[from..].find(call) {
        let start = from + offset;
        let preceded = line[..start].chars().next_back();
        from = start + call.len();
        if preceded.is_some_and(|c| is_name_char(c) || c == '.') {
            continue;
        }
        let rest = line[from..].trim_start();
        let quote = rest.chars().next().filter(|c| matches!(c, '\'' | '\u{22}'))?;
        let quoted = &rest[quote.len_utf8()..];
        let end = quoted.find(quote)?;
        return Some(quoted[..end].to_string());
    }
    None
}
