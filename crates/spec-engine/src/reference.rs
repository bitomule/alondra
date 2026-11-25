use crate::ident::is_identifier;
use crate::problem::{TEXT_MAX_BYTES, problem, too_large};
use crate::{Code, Problem, Section};

/// A section and the line of the comment that anchors it.
pub(crate) struct ReadSection {
    pub(crate) section: Section,
    pub(crate) comment_line: usize,
}

enum Anchor {
    Missing,
    Malformed,
    Found { id: String, explains: Vec<String> },
}

fn is_heading(line: &str) -> bool {
    line.split_whitespace().next() == Some("##")
}

const COMMENT_OPENER: &str = "<!--";

fn anchor(line: &str) -> Anchor {
    if !line.trim_start().starts_with(COMMENT_OPENER) {
        return Anchor::Missing;
    }
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["<!--", "section:", id, "explains:", rest @ .., "-->"] => {
            let explains = rest
                .join(" ")
                .split(',')
                .map(|target| target.trim().to_string())
                .filter(|target| !target.is_empty())
                .collect();
            Anchor::Found { id: id.to_string(), explains }
        }
        _ => Anchor::Malformed,
    }
}

pub(crate) fn read(path: &str, bytes: &[u8]) -> (Vec<ReadSection>, Vec<Problem>) {
    if let Some(found) = too_large(path, bytes, TEXT_MAX_BYTES) {
        return (Vec::new(), vec![found]);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return (Vec::new(), vec![problem(Code::InvalidUtf8, path, 0, "the document is not valid UTF-8")]);
    };
    let lines: Vec<&str> = source.lines().map(str::trim_end).collect();
    let headings: Vec<usize> = (0..lines.len()).filter(|index| is_heading(lines[*index])).collect();
    let mut sections = Vec::new();
    let mut problems = Vec::new();
    for (position, &start) in headings.iter().enumerate() {
        let end = headings.get(position + 1).copied().unwrap_or(lines.len());
        let heading_line = start + 1;
        let comment_line = start + 2;
        let found = lines.get(start + 1).filter(|_| start + 1 < end).map_or(Anchor::Missing, |line| anchor(line));
        let (id, explains) = match found {
            Anchor::Missing => {
                problems.push(problem(
                    Code::SectionExplainsNothing,
                    path,
                    heading_line,
                    "the heading is not followed by a section comment",
                ));
                continue;
            }
            Anchor::Malformed => {
                problems.push(problem(
                    Code::SectionExplainsNothing,
                    path,
                    comment_line,
                    "the section comment does not read <!-- section: <id> explains: <targets> -->",
                ));
                continue;
            }
            Anchor::Found { id, explains } => (id, explains),
        };
        if explains.is_empty() {
            problems.push(problem(Code::SectionExplainsNothing, path, comment_line, "the section explains nothing"));
            continue;
        }
        if !is_identifier(&id) {
            problems.push(problem(Code::InvalidIdentifier, path, comment_line, format!("{id:?} is not a valid section identifier")));
            continue;
        }
        let heading = lines[start].trim_start().trim_start_matches('#').trim().to_string();
        let mut content: Vec<&str> = vec![lines[start]];
        content.extend(lines[(start + 2).min(end)..end].iter().copied());
        let section = Section {
            id,
            path: path.to_string(),
            line: heading_line,
            heading,
            text: content.join("\n").trim_end().to_string(),
            explains,
        };
        sections.push(ReadSection { section, comment_line });
    }
    (sections, problems)
}
