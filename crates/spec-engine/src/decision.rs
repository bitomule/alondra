use serde::Deserialize;

use crate::problem::{TEXT_MAX_BYTES, key_line, problem, toml_line, too_large};
use crate::{Code, Decision, Problem, Status};

const FENCE: &str = "+++";
const SECTIONS: [&str; 4] = ["Context", "Decision", "Consequences", "Confirmation"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    id: String,
    title: String,
    status: String,
    date: String,
    #[serde(default)]
    supersedes: Vec<String>,
    #[serde(default)]
    superseded_by: String,
}

fn status(word: &str) -> Option<Status> {
    match word {
        "proposed" => Some(Status::Proposed),
        "accepted" => Some(Status::Accepted),
        "rejected" => Some(Status::Rejected),
        "superseded" => Some(Status::Superseded),
        "deprecated" => Some(Status::Deprecated),
        _ => None,
    }
}

/// The name of a file without its folder and its `.md` extension.
pub(crate) fn stem(path: &str) -> &str {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.strip_suffix(".md").unwrap_or(name)
}

/// The decision a file holds, when its header can be read, and the problems of the file.
pub(crate) fn read(path: &str, bytes: &[u8]) -> (Option<Decision>, Vec<Problem>) {
    if let Some(found) = too_large(path, bytes, TEXT_MAX_BYTES) {
        return (None, vec![found]);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return (None, vec![problem(Code::InvalidUtf8, path, 0, "the decision is not valid UTF-8")]);
    };
    let lines: Vec<&str> = source.lines().map(str::trim_end).collect();
    if lines.first().copied() != Some(FENCE) {
        return (None, vec![problem(Code::InvalidDecision, path, 1, "a decision starts with a +++ line")]);
    }
    let Some(close) = lines.iter().skip(1).position(|line| *line == FENCE).map(|index| index + 1) else {
        return (None, vec![problem(Code::InvalidDecision, path, 1, "the header is never closed with a +++ line")]);
    };
    let header_text = lines[1..close].join("\n");
    let header: Header = match toml::from_str(&header_text) {
        Ok(header) => header,
        Err(failure) => {
            let line = match toml_line(&header_text, &failure) {
                0 => 1,
                line => line + 1,
            };
            return (None, vec![problem(Code::InvalidDecision, path, line, failure.message().to_string())]);
        }
    };
    let Some(status) = status(&header.status) else {
        let line = key_line(&header_text, "status") + 1;
        return (None, vec![problem(Code::InvalidDecision, path, line, format!("{:?} is not a status", header.status))]);
    };
    let mut problems = Vec::new();
    if header.id != stem(path) {
        problems.push(problem(
            Code::DecisionIdMismatch,
            path,
            key_line(&header_text, "id") + 1,
            format!("the decision says id {:?} but its file is {:?}", header.id, stem(path)),
        ));
    }
    let sections = sections(&lines[close + 1..]);
    let mut texts: Vec<String> = Vec::with_capacity(SECTIONS.len());
    for name in SECTIONS {
        let found = sections.iter().find(|(heading, _)| *heading == name).map(|(_, content)| content.clone());
        match found {
            Some(content) if !content.is_empty() => texts.push(content),
            _ => {
                problems.push(problem(Code::MissingSection, path, 0, format!("the section {name} is missing or empty")));
                texts.push(String::new());
            }
        }
    }
    let [context, decision, consequences, confirmation]: [String; 4] = match texts.try_into() {
        Ok(texts) => texts,
        Err(_) => return (None, problems),
    };
    let decision = Decision {
        id: header.id,
        path: path.to_string(),
        title: header.title,
        status,
        date: header.date,
        supersedes: header.supersedes,
        superseded_by: (!header.superseded_by.is_empty()).then_some(header.superseded_by),
        context,
        decision,
        consequences,
        confirmation,
    };
    (Some(decision), problems)
}

/// Each `## <Word>` heading of a body with the trimmed text up to the next `##` heading.
fn sections<'a>(lines: &[&'a str]) -> Vec<(&'a str, String)> {
    let mut found: Vec<(&'a str, Vec<&'a str>)> = Vec::new();
    let mut current: Option<usize> = None;
    for &line in lines {
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            ["##", name] => {
                found.push((*name, Vec::new()));
                current = Some(found.len() - 1);
            }
            ["##", ..] => current = None,
            _ => {
                if let Some(index) = current {
                    found[index].1.push(line);
                }
            }
        }
    }
    found.into_iter().map(|(name, content)| (name, content.join("\n").trim().to_string())).collect()
}
