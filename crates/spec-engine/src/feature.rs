use std::collections::BTreeSet;

use crate::ident::{is_identifier, is_scenario_id};
use crate::problem::{TEXT_MAX_BYTES, finish, problem, too_large};
use crate::{Code, EvidenceKind, Judgment, Keyword, Problem, Promise, Scenario, Spec, Step};

const MAX_SCENARIOS: usize = 200;
const DOC_DELIMITER: &str = "\"\"\"";
const ESCAPE: char = '\\';
const CELL_END: char = '|';
const STEP_INDENT: &str = "    ";
const ATTACHED_INDENT: &str = "      ";

/// A spec and the line of each of its `@decision:` tags, in the order of `spec.decisions`.
pub(crate) struct ReadSpec {
    pub(crate) spec: Spec,
    pub(crate) decision_lines: Vec<usize>,
}

pub(crate) fn read(path: &str, bytes: &[u8]) -> Result<ReadSpec, Vec<Problem>> {
    if let Some(found) = too_large(path, bytes, TEXT_MAX_BYTES) {
        return Err(vec![found]);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Err(vec![problem(Code::InvalidUtf8, path, 0, "the file is not valid UTF-8")]);
    };
    let mut reader = Reader::new(path);
    for (index, line) in source.lines().map(str::trim_end).enumerate() {
        reader.line(index + 1, line);
    }
    let scenarios = reader.drafts.len();
    if scenarios > MAX_SCENARIOS {
        return Err(vec![problem(
            Code::TooManyScenarios,
            path,
            0,
            format!("the file has {scenarios} scenarios, more than {MAX_SCENARIOS}"),
        )]);
    }
    reader.finish()
}

enum Head<'a> {
    Feature(&'a str),
    Background,
    Scenario { outline: bool, title: &'a str },
    Examples,
    Unknown,
}

fn heading(trimmed: &str) -> Option<Head<'_>> {
    let (head, rest) = trimmed.split_once(':')?;
    let words: Vec<&str> = head.split_whitespace().collect();
    let title = rest.trim();
    match words.as_slice() {
        ["Feature"] => Some(Head::Feature(title)),
        ["Background"] => Some(Head::Background),
        ["Scenario"] => Some(Head::Scenario { outline: false, title }),
        ["Scenario", "Outline"] => Some(Head::Scenario { outline: true, title }),
        ["Examples"] => Some(Head::Examples),
        ["Scenario", ..] | ["Rule"] | ["Example"] | ["Scenarios"] => Some(Head::Unknown),
        _ => None,
    }
}

fn step_keyword(word: &str) -> Option<Keyword> {
    match word {
        "Given" => Some(Keyword::Given),
        "When" => Some(Keyword::When),
        "Then" => Some(Keyword::Then),
        "And" => Some(Keyword::And),
        "But" => Some(Keyword::But),
        _ => None,
    }
}

fn promise(word: &str) -> Option<Promise> {
    match word {
        "surface" => Some(Promise::Surface),
        "data" => Some(Promise::Data),
        "dependencies" => Some(Promise::Dependencies),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Block {
    Start,
    Feature,
    Background,
    Scenario,
    Examples,
}

struct Draft {
    scenario: Scenario,
    has_id: bool,
    examples_line: Option<usize>,
}

struct DocDraft {
    line: usize,
    indent: usize,
    attached: bool,
    lines: Vec<String>,
}

struct Reader<'p> {
    path: &'p str,
    problems: Vec<Problem>,
    pending: Vec<(String, usize)>,
    feature_line: Option<usize>,
    title: String,
    spec_id: Option<String>,
    has_spec_tag: bool,
    decisions: Vec<(String, usize)>,
    description: Vec<String>,
    background: Vec<Step>,
    has_background: bool,
    drafts: Vec<Draft>,
    block: Block,
    doc: Option<DocDraft>,
}

impl<'p> Reader<'p> {
    fn new(path: &'p str) -> Self {
        Reader {
            path,
            problems: Vec::new(),
            pending: Vec::new(),
            feature_line: None,
            title: String::new(),
            spec_id: None,
            has_spec_tag: false,
            decisions: Vec::new(),
            description: Vec::new(),
            background: Vec::new(),
            has_background: false,
            drafts: Vec::new(),
            block: Block::Start,
            doc: None,
        }
    }

    fn report(&mut self, code: Code, line: usize, message: impl Into<String>) {
        self.problems.push(problem(code, self.path, line, message));
    }

    fn unsupported(&mut self, line: usize) {
        self.report(Code::UnsupportedSyntax, line, "this line is outside the supported subset of Gherkin");
    }

    fn line(&mut self, number: usize, line: &str) {
        if self.doc.is_some() {
            self.doc_line(line);
            return;
        }
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            return;
        }
        if trimmed.starts_with('@') {
            self.tags(number, trimmed);
            return;
        }
        if trimmed.starts_with('|') {
            self.row(number, trimmed);
            return;
        }
        if trimmed == DOC_DELIMITER {
            self.open_doc(number, leading_spaces(line));
            return;
        }
        let first = trimmed.split_whitespace().next().unwrap_or_default();
        if let Some(keyword) = step_keyword(first) {
            self.step(number, keyword, trimmed[first.len()..].trim());
            return;
        }
        match heading(trimmed) {
            Some(Head::Feature(title)) => self.feature(number, title),
            Some(Head::Background) => self.background(number),
            Some(Head::Scenario { outline, title }) => self.scenario(number, outline, title),
            Some(Head::Examples) => self.examples(number),
            Some(Head::Unknown) => {
                self.reject_pending();
                self.unsupported(number);
            }
            None => self.free(number, trimmed),
        }
    }

    fn reject_pending(&mut self) {
        if let Some((_, line)) = self.pending.first() {
            let line = *line;
            self.pending.clear();
            self.report(Code::UnsupportedSyntax, line, "tags only go above a feature or a scenario");
        }
    }

    fn tags(&mut self, number: usize, trimmed: &str) {
        for token in trimmed.split_whitespace() {
            if token.starts_with('@') {
                self.pending.push((token.to_string(), number));
            } else {
                self.unsupported(number);
                return;
            }
        }
    }

    fn steps_mut(&mut self) -> Option<&mut Vec<Step>> {
        match self.block {
            Block::Background => Some(&mut self.background),
            Block::Scenario => self.drafts.last_mut().map(|draft| &mut draft.scenario.steps),
            _ => None,
        }
    }

    fn step(&mut self, number: usize, keyword: Keyword, content: &str) {
        self.reject_pending();
        let step = Step { keyword, text: content.to_string(), doc: None, table: Vec::new() };
        match self.steps_mut() {
            Some(steps) => steps.push(step),
            None => self.unsupported(number),
        }
    }

    fn row(&mut self, number: usize, trimmed: &str) {
        self.reject_pending();
        let cells = row_cells(trimmed);
        if self.block == Block::Examples {
            if let Some(draft) = self.drafts.last_mut() {
                draft.scenario.examples.push(cells);
                return;
            }
        }
        match self.steps_mut().and_then(|steps| steps.last_mut()) {
            Some(step) => step.table.push(cells),
            None => self.unsupported(number),
        }
    }

    fn open_doc(&mut self, number: usize, indent: usize) {
        self.reject_pending();
        let attached = matches!(self.steps_mut().and_then(|steps| steps.last_mut()), Some(step) if step.doc.is_none());
        if !attached {
            self.unsupported(number);
        }
        self.doc = Some(DocDraft { line: number, indent, attached, lines: Vec::new() });
    }

    fn doc_line(&mut self, line: &str) {
        if line.trim() == DOC_DELIMITER {
            self.close_doc();
            return;
        }
        if let Some(doc) = self.doc.as_mut() {
            let content = &line[leading_spaces(line).min(doc.indent)..];
            let (indent, text) = content.split_at(leading_spaces(content));
            let read = match unescaped(text) {
                Some(original) if is_delimiter_like(text) => format!("{indent}{original}"),
                _ => content.to_string(),
            };
            doc.lines.push(read);
        }
    }

    fn close_doc(&mut self) {
        let Some(doc) = self.doc.take() else { return };
        if !doc.attached {
            return;
        }
        let content = doc.lines.join("\n");
        if let Some(step) = self.steps_mut().and_then(|steps| steps.last_mut()) {
            step.doc = Some(content);
        }
    }

    fn feature(&mut self, number: usize, title: &str) {
        if self.block != Block::Start {
            self.reject_pending();
            self.unsupported(number);
            return;
        }
        self.feature_line = Some(number);
        self.title = title.to_string();
        self.block = Block::Feature;
        let tags = std::mem::take(&mut self.pending);
        for (token, line) in tags {
            match token.split_once(':') {
                Some(("@spec", value)) if !self.has_spec_tag => {
                    self.has_spec_tag = true;
                    if is_identifier(value) {
                        self.spec_id = Some(value.to_string());
                    } else {
                        self.report(Code::InvalidIdentifier, line, format!("{value:?} is not a valid spec identifier"));
                    }
                }
                Some(("@decision", value)) => {
                    if is_identifier(value) {
                        self.decisions.push((value.to_string(), line));
                    } else {
                        self.report(Code::InvalidIdentifier, line, format!("{value:?} is not a valid decision identifier"));
                    }
                }
                _ => self.report(Code::UnknownTag, line, format!("{token} is not a tag a feature can carry")),
            }
        }
    }

    fn background(&mut self, number: usize) {
        self.reject_pending();
        if self.block != Block::Feature || self.has_background {
            self.unsupported(number);
            return;
        }
        self.has_background = true;
        self.block = Block::Background;
    }

    fn scenario(&mut self, number: usize, outline: bool, title: &str) {
        if self.block == Block::Start {
            self.reject_pending();
            self.unsupported(number);
            return;
        }
        let mut draft = Draft {
            scenario: Scenario {
                id: String::new(),
                title: title.to_string(),
                line: number,
                outline,
                judgment: None,
                promise: None,
                reserved: false,
                steps: Vec::new(),
                examples: Vec::new(),
            },
            has_id: false,
            examples_line: None,
        };
        let tags = std::mem::take(&mut self.pending);
        self.scenario_tags(&mut draft, tags);
        self.drafts.push(draft);
        self.block = Block::Scenario;
    }

    fn scenario_tags(&mut self, draft: &mut Draft, tags: Vec<(String, usize)>) {
        let mut judged = false;
        let mut evidence: Option<(String, usize)> = None;
        for (token, line) in tags {
            let (name, value) = match token.split_once(':') {
                Some((name, value)) => (name, Some(value)),
                None => (token.as_str(), None),
            };
            match (name, value) {
                ("@id", Some(value)) if !draft.has_id => {
                    draft.has_id = true;
                    if is_scenario_id(value) {
                        draft.scenario.id = value.to_string();
                    } else {
                        self.report(Code::InvalidIdentifier, line, format!("{value:?} is not a valid scenario identifier"));
                    }
                }
                ("@judged", None) if !judged => {
                    judged = true;
                    draft.scenario.judgment = Some(Judgment { evidence: EvidenceKind::None, pattern: None });
                }
                ("@judged", Some("capture")) if !judged => {
                    judged = true;
                    draft.scenario.judgment = Some(Judgment { evidence: EvidenceKind::Capture, pattern: None });
                }
                ("@judged", Some("video")) if !judged => {
                    judged = true;
                    draft.scenario.judgment = Some(Judgment { evidence: EvidenceKind::Video, pattern: None });
                }
                ("@evidence", Some(value)) if evidence.is_none() && !value.is_empty() => {
                    evidence = Some((value.to_string(), line));
                }
                ("@boundary", Some(value)) if draft.scenario.promise.is_none() && promise(value).is_some() => {
                    draft.scenario.promise = promise(value);
                }
                ("@reserved", None) if !draft.scenario.reserved => draft.scenario.reserved = true,
                _ => self.report(Code::UnknownTag, line, format!("{token} is not a tag a scenario can carry")),
            }
        }
        if let Some((pattern, line)) = evidence {
            match draft.scenario.judgment.as_mut() {
                Some(judgment) if judgment.evidence == EvidenceKind::Capture => judgment.pattern = Some(pattern),
                _ => self.report(
                    Code::EvidenceWithoutCapture,
                    line,
                    "a pattern of committed evidence only goes with a judgment of captures",
                ),
            }
        }
    }

    fn examples(&mut self, number: usize) {
        self.reject_pending();
        let outline = match self.drafts.last() {
            Some(draft) if self.block == Block::Scenario && draft.examples_line.is_none() => draft.scenario.outline,
            _ => {
                self.unsupported(number);
                return;
            }
        };
        if !outline {
            self.report(Code::UnsupportedSyntax, number, "only a Scenario Outline has examples");
        }
        if let Some(draft) = self.drafts.last_mut() {
            draft.examples_line = Some(number);
        }
        self.block = Block::Examples;
    }

    fn free(&mut self, number: usize, trimmed: &str) {
        if self.block == Block::Feature && self.pending.is_empty() {
            self.description.push(trimmed.to_string());
        } else {
            self.reject_pending();
            self.unsupported(number);
        }
    }

    fn finish(mut self) -> Result<ReadSpec, Vec<Problem>> {
        if let Some(doc) = self.doc.take() {
            self.report(Code::UnsupportedSyntax, doc.line, "this doc string is never closed");
        }
        self.reject_pending();
        let feature_line = self.feature_line;
        let has_spec_tag = self.has_spec_tag;
        match feature_line {
            None => self.report(Code::UnsupportedSyntax, 0, "the file has no Feature line"),
            Some(line) if !has_spec_tag => self.report(Code::MissingSpecTag, line, "the feature has no @spec: tag"),
            Some(_) => {}
        }
        let drafts = std::mem::take(&mut self.drafts);
        let mut ids = BTreeSet::new();
        let mut scenarios = Vec::with_capacity(drafts.len());
        for draft in drafts {
            let line = draft.scenario.line;
            if !draft.has_id {
                self.report(Code::MissingScenarioId, line, "the scenario has no @id: tag");
            } else if !draft.scenario.id.is_empty() && !ids.insert(draft.scenario.id.clone()) {
                self.report(
                    Code::DuplicateScenarioId,
                    line,
                    format!("another scenario of this spec already has the id {}", draft.scenario.id),
                );
            }
            match draft.examples_line {
                None if draft.scenario.outline => {
                    self.report(Code::UnsupportedSyntax, line, "a Scenario Outline needs an Examples table")
                }
                Some(examples) if draft.scenario.outline && draft.scenario.examples.is_empty() => {
                    self.report(Code::UnsupportedSyntax, examples, "the Examples table has no rows")
                }
                _ => {}
            }
            scenarios.push(draft.scenario);
        }
        let (decisions, decision_lines): (Vec<String>, Vec<usize>) = self.decisions.into_iter().unzip();
        let spec = Spec {
            id: self.spec_id.unwrap_or_default(),
            decisions,
            title: self.title,
            description: self.description,
            background: self.background,
            scenarios,
        };
        finish(ReadSpec { spec, decision_lines }, self.problems)
    }
}

fn leading_spaces(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

/// The cells of a table row: `\|` stands for a pipe, `\\` for a backslash, and any other pipe
/// ends a cell.
fn row_cells(trimmed: &str) -> Vec<String> {
    let mut cells = Vec::new();
    let mut current = String::new();
    let mut ended = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        ended = false;
        match c {
            ESCAPE => match chars.peek() {
                Some(&next @ (ESCAPE | CELL_END)) => {
                    current.push(next);
                    chars.next();
                }
                _ => current.push(ESCAPE),
            },
            CELL_END => {
                cells.push(std::mem::take(&mut current));
                ended = true;
            }
            other => current.push(other),
        }
    }
    if !ended {
        cells.push(current);
    }
    cells.into_iter().skip(1).map(|cell| cell.trim().to_string()).collect()
}

fn escaped_cell(cell: &str) -> String {
    let mut out = String::with_capacity(cell.len());
    for c in cell.chars() {
        if matches!(c, ESCAPE | CELL_END) {
            out.push(ESCAPE);
        }
        out.push(c);
    }
    out
}

/// A doc line escaped once: each backslash and each quote gets a backslash before it.
fn escaped_doc(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    for c in text.chars() {
        if matches!(c, ESCAPE | '"') {
            out.push(ESCAPE);
        }
        out.push(c);
    }
    out
}

/// A doc line with one escape undone, when every character of it is a backslash followed by a
/// backslash or a quote.
fn unescaped(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len() / 2);
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, chars.next()) {
            (ESCAPE, Some(next @ (ESCAPE | '"'))) => out.push(next),
            _ => return None,
        }
    }
    Some(out)
}

/// Whether undoing the escape of a doc line zero or more times gives the doc string delimiter.
fn is_delimiter_like(text: &str) -> bool {
    let mut current = text.to_string();
    loop {
        if current == DOC_DELIMITER {
            return true;
        }
        match unescaped(&current) {
            Some(next) if !next.is_empty() => current = next,
            _ => return false,
        }
    }
}

pub(crate) fn write(spec: &Spec) -> String {
    let mut out = String::new();
    out.push_str(&format!("@spec:{}\n", spec.id));
    for decision in &spec.decisions {
        out.push_str(&format!("@decision:{decision}\n"));
    }
    out.push_str(&keyword_line("Feature", &spec.title));
    out.push('\n');
    for line in &spec.description {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    if !spec.background.is_empty() {
        out.push_str("\n  Background:\n");
        out.push_str(&write_steps(&spec.background));
    }
    for scenario in &spec.scenarios {
        out.push('\n');
        out.push_str(&write_scenario(scenario));
    }
    out
}

/// The canonical block of one scenario: its tags, its keyword line, its steps and its examples.
pub(crate) fn write_scenario(scenario: &Scenario) -> String {
    let mut out = String::new();
    out.push_str("  ");
    out.push_str(&tag_line(scenario));
    out.push('\n');
    out.push_str("  ");
    let keyword = if scenario.outline { "Scenario Outline" } else { "Scenario" };
    out.push_str(&keyword_line(keyword, &scenario.title));
    out.push('\n');
    out.push_str(&write_steps(&scenario.steps));
    if scenario.outline {
        out.push_str("\n    Examples:\n");
        out.push_str(&write_table(&scenario.examples));
    }
    out
}

fn tag_line(scenario: &Scenario) -> String {
    let mut tags = vec![format!("@id:{}", scenario.id)];
    if let Some(judgment) = &scenario.judgment {
        tags.push(match judgment.evidence {
            EvidenceKind::None => "@judged".to_string(),
            kind => format!("@judged:{}", kind.as_str()),
        });
        if let Some(pattern) = &judgment.pattern {
            tags.push(format!("@evidence:{pattern}"));
        }
    }
    if let Some(promise) = scenario.promise {
        tags.push(format!("@boundary:{}", promise.as_str()));
    }
    if scenario.reserved {
        tags.push("@reserved".to_string());
    }
    tags.join(" ")
}

fn keyword_line(keyword: &str, title: &str) -> String {
    if title.is_empty() { format!("{keyword}:") } else { format!("{keyword}: {title}") }
}

pub(crate) fn write_steps(steps: &[Step]) -> String {
    let mut out = String::new();
    for step in steps {
        out.push_str(STEP_INDENT);
        out.push_str(step.keyword.as_str());
        if !step.text.is_empty() {
            out.push(' ');
            out.push_str(&step.text);
        }
        out.push('\n');
        out.push_str(&write_table(&step.table));
        if let Some(doc) = &step.doc {
            out.push_str(&write_doc(doc));
        }
    }
    out
}

fn write_table(rows: &[Vec<String>]) -> String {
    let rows: Vec<Vec<String>> = rows.iter().map(|row| row.iter().map(|cell| escaped_cell(cell)).collect()).collect();
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let widths: Vec<usize> = (0..columns)
        .map(|column| rows.iter().filter_map(|row| row.get(column)).map(|cell| cell.chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for row in &rows {
        out.push_str(ATTACHED_INDENT);
        if row.is_empty() {
            out.push_str("|\n");
            continue;
        }
        let cells: Vec<String> = row
            .iter()
            .enumerate()
            .map(|(column, cell)| {
                let width = widths.get(column).copied().unwrap_or(0);
                format!("{cell:<width$}")
            })
            .collect();
        out.push_str("| ");
        out.push_str(&cells.join(" | "));
        out.push_str(" |\n");
    }
    out
}

fn write_doc(doc: &str) -> String {
    let mut out = String::new();
    out.push_str(ATTACHED_INDENT);
    out.push_str(DOC_DELIMITER);
    out.push('\n');
    for line in doc.split('\n') {
        let (indent, text) = line.split_at(leading_spaces(line));
        if is_delimiter_like(text.trim_end()) {
            out.push_str(ATTACHED_INDENT);
            out.push_str(indent);
            out.push_str(&escaped_doc(text.trim_end()));
        } else if !line.is_empty() {
            out.push_str(ATTACHED_INDENT);
            out.push_str(line);
        }
        out.push('\n');
    }
    out.push_str(ATTACHED_INDENT);
    out.push_str(DOC_DELIMITER);
    out.push('\n');
    out
}
