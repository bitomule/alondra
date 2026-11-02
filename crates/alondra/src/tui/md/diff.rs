//! A PR diff as reader blocks: the list of files on top, then each file under a header that folds,
//! its hunks numbered old and new, added lines green, removed red, and what changed inside a line
//! marked. A folded file keeps its line blocks, hidden, so comments stay on the block they quote.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::{Block, DIM, Fmt, Kind, Run, char_width, truncate};
use crate::tui::unified_diff::{Diff, File, Op};

const ADDED: Color = Color::Green;
const REMOVED: Color = Color::Red;
const ADDED_CHANGE: Color = Color::Indexed(22);
const REMOVED_CHANGE: Color = Color::Indexed(52);
const HUNK: Color = Color::Cyan;

/// A file of a diff part of the reader.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct At {
    pub part: usize,
    pub file: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Why {
    Binary,
    Large,
    Generated,
    Renamed,
    Empty,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub at: At,
    pub badge: char,
    pub path: String,
    pub old_path: Option<String>,
    pub added: usize,
    pub removed: usize,
    pub folded: bool,
    pub why: Option<Why>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub op: Op,
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub path: String,
    pub gutter: usize,
    pub hidden: bool,
}

impl Row {
    pub fn commentable(&self) -> bool {
        !self.hidden && matches!(self.op, Op::Added | Op::Removed | Op::Context)
    }

    pub fn place(&self) -> String {
        match self.new.or(self.old) {
            Some(line) => format!("{}:{line}", self.path),
            None => self.path.clone(),
        }
    }
}

fn why(file: &File) -> Option<Why> {
    if file.binary {
        Some(Why::Binary)
    } else if !file.has_code() && file.old_path.is_some() {
        Some(Why::Renamed)
    } else if !file.has_code() {
        Some(Why::Empty)
    } else if file.generated() {
        Some(Why::Generated)
    } else if file.large() {
        Some(Why::Large)
    } else {
        None
    }
}

pub fn blocks(diff: &Diff, part: usize, folded: impl Fn(usize) -> bool) -> Vec<Block> {
    let mut out = vec![Block::plain_text(Kind::DiffFiles { truncated: diff.truncated }, &summary(diff))];
    let headers: Vec<Header> = diff
        .files
        .iter()
        .enumerate()
        .map(|(index, file)| Header {
            at: At { part, file: index },
            badge: file.status.badge(file.binary),
            path: file.path.clone(),
            old_path: file.old_path.clone(),
            added: file.added(),
            removed: file.removed(),
            folded: folded(index),
            why: why(file),
        })
        .collect();
    out.extend(headers.iter().map(|header| Block::plain_text(Kind::DiffEntry(header.clone()), &header.path)));
    for (file, header) in diff.files.iter().zip(headers) {
        let hidden = header.folded;
        out.push(Block::plain_text(Kind::DiffFile(header), &file.path));
        let gutter = file
            .lines
            .iter()
            .flat_map(|line| [line.old, line.new])
            .flatten()
            .max()
            .map_or(1, |n| n.to_string().len())
            .max(3);
        for line in &file.lines {
            let runs = match &line.changed {
                Some(range) => vec![
                    run(&line.text[..range.start], false),
                    run(&line.text[range.clone()], true),
                    run(&line.text[range.end..], false),
                ],
                None => vec![run(&line.text, false)],
            };
            out.push(Block {
                kind: Kind::DiffLine(Row {
                    op: line.op,
                    old: line.old,
                    new: line.new,
                    path: file.path.clone(),
                    gutter,
                    hidden,
                }),
                depth: 0,
                lines: vec![runs.into_iter().filter(|run| !run.text.is_empty()).collect()],
                targets: vec![],
            });
        }
    }
    out
}

fn run(text: &str, changed: bool) -> Run {
    Run {
        text: text.replace('\t', "    "),
        fmt: Fmt { changed, ..Fmt::default() },
    }
}

fn summary(diff: &Diff) -> String {
    format!("{} · {}", tr!("Changes", "Cambios"), diff_counts(diff))
}

/// "4 files · +120 −30".
pub fn diff_counts(diff: &Diff) -> String {
    let n = diff.files.len();
    let files = match n {
        1 => tr!("1 file", "1 archivo").to_owned(),
        _ => trf!("{n} files", "{n} archivos"),
    };
    format!("{files} · +{} −{}", diff.added(), diff.removed())
}

pub(super) fn hidden(kind: &Kind) -> bool {
    matches!(kind, Kind::DiffLine(row) if row.hidden)
}

pub(super) fn joins(prev: &Kind, cur: &Kind) -> bool {
    matches!(
        (prev, cur),
        (Kind::DiffFiles { .. } | Kind::DiffEntry(_), Kind::DiffEntry(_))
            | (Kind::DiffFile(_) | Kind::DiffLine(_), Kind::DiffLine(_))
            | (Kind::DiffFile(_), Kind::DiffFile(_))
    )
}

fn badge_color(badge: char) -> Color {
    match badge {
        'A' => ADDED,
        'D' => REMOVED,
        'R' => Color::LightBlue,
        'B' => Color::Magenta,
        _ => Color::Yellow,
    }
}

fn path_spans(header: &Header, style: Style) -> Vec<Span<'static>> {
    let mut spans = vec![];
    if let Some(old) = header.old_path.as_ref().filter(|old| **old != header.path) {
        spans.push(Span::styled(old.clone(), Style::new().fg(DIM)));
        spans.push(Span::styled(" → ", Style::new().fg(DIM)));
    }
    spans.push(Span::styled(header.path.clone(), style));
    spans
}

fn counts(header: &Header) -> Vec<Span<'static>> {
    if matches!(header.why, Some(Why::Binary | Why::Renamed | Why::Empty)) {
        return vec![];
    }
    vec![
        Span::raw("  "),
        Span::styled(format!("+{}", header.added), Style::new().fg(ADDED)),
        Span::styled(format!(" −{}", header.removed), Style::new().fg(REMOVED)),
    ]
}

fn why_text(why: Why, folded: bool) -> Option<String> {
    let text = match why {
        Why::Binary => tr!("Binary file", "Fichero binario").to_owned(),
        Why::Renamed => tr!("Renamed, no changes", "Renombrado, sin cambios").to_owned(),
        Why::Empty => tr!("No lines changed", "Sin líneas cambiadas").to_owned(),
        Why::Large if folded => tr!("Large file · ⏎ shows it", "Fichero grande · ⏎ lo muestra").to_owned(),
        Why::Generated if folded => tr!("Generated file · ⏎ shows it", "Fichero generado · ⏎ lo muestra").to_owned(),
        Why::Large | Why::Generated => return None,
    };
    Some(text)
}

pub(super) fn render(b: &Block, width: usize) -> Vec<Line<'static>> {
    match &b.kind {
        Kind::DiffFiles { truncated } => {
            let mut lines = vec![Line::styled(
                truncate(&b.plain(), width),
                Style::new().fg(super::ACCENT).add_modifier(Modifier::BOLD),
            )];
            if *truncated {
                lines.push(Line::styled(
                    truncate(
                        tr!(
                            "Part of the diff is not shown. Open the PR on GitHub to read the rest.",
                            "Parte del diff no se muestra. Abre la PR en GitHub para leer el resto."
                        ),
                        width,
                    ),
                    Style::new().fg(Color::Yellow),
                ));
            }
            lines
        }
        Kind::DiffEntry(header) => {
            let mut spans = vec![Span::styled(
                format!("  {}  ", header.badge),
                Style::new().fg(badge_color(header.badge)).add_modifier(Modifier::BOLD),
            )];
            spans.extend(path_spans(header, Style::new()));
            spans.extend(counts(header));
            vec![clipped(spans, width)]
        }
        Kind::DiffFile(header) => {
            let mut spans = vec![
                Span::styled(if header.folded { "› " } else { "⌄ " }, Style::new().fg(DIM)),
                Span::styled(
                    format!("{}  ", header.badge),
                    Style::new().fg(badge_color(header.badge)).add_modifier(Modifier::BOLD),
                ),
            ];
            spans.extend(path_spans(header, Style::new().add_modifier(Modifier::BOLD)));
            spans.extend(counts(header));
            if let Some(text) = header.why.and_then(|why| why_text(why, header.folded)) {
                spans.push(Span::styled(format!(" · {text}"), Style::new().fg(DIM)));
            }
            vec![clipped(spans, width)]
        }
        Kind::DiffLine(row) if row.hidden => vec![],
        Kind::DiffLine(row) => row_lines(b, row, width),
        _ => vec![],
    }
}

fn clipped(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut out = vec![];
    let mut used = 0;
    for span in spans {
        let w = span.width();
        if used + w <= width {
            used += w;
            out.push(span);
            continue;
        }
        let rest = width.saturating_sub(used);
        if rest > 0 {
            out.push(Span::styled(truncate(&span.content, rest), span.style));
        }
        break;
    }
    Line::from(out)
}

fn number(n: Option<u32>, gutter: usize) -> String {
    n.map_or_else(|| " ".repeat(gutter), |n| format!("{n:>gutter$}"))
}

fn row_lines(b: &Block, row: &Row, width: usize) -> Vec<Line<'static>> {
    let gutter = format!("{} {} ", number(row.old, row.gutter), number(row.new, row.gutter));
    let (sign, color, change) = match row.op {
        Op::Added => ("+", Some(ADDED), ADDED_CHANGE),
        Op::Removed => ("-", Some(REMOVED), REMOVED_CHANGE),
        Op::Context => (" ", None, Color::Reset),
        Op::Hunk | Op::Note => ("", Some(HUNK), Color::Reset),
    };
    let base = match (row.op, color) {
        (Op::Note, _) => Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
        (_, Some(color)) => Style::new().fg(color),
        (_, None) => Style::new(),
    };
    let mut text: Vec<Span<'static>> = vec![];
    if !sign.is_empty() {
        text.push(Span::styled(format!("{sign} "), base));
    }
    for run in b.lines.iter().flatten() {
        let style = if run.fmt.changed {
            base.bg(change).add_modifier(Modifier::BOLD)
        } else {
            base
        };
        text.push(Span::styled(run.text.clone(), style));
    }
    let first = Span::styled(gutter.clone(), Style::new().fg(DIM));
    let rest = Span::raw(" ".repeat(gutter.chars().count() + sign.len().min(1) * 2));
    char_wrap(text, width, first, rest)
}

/// Code keeps its spaces: lines break at the width, not at words.
fn char_wrap(spans: Vec<Span<'static>>, width: usize, first: Span<'static>, rest: Span<'static>) -> Vec<Line<'static>> {
    let mut lines = vec![];
    let mut line = vec![first.clone()];
    let mut used = first.width();
    let mut any = false;
    for span in spans {
        let mut cur = String::new();
        for c in span.content.chars() {
            let w = char_width(c);
            if used + w > width && any {
                if !cur.is_empty() {
                    line.push(Span::styled(std::mem::take(&mut cur), span.style));
                }
                lines.push(Line::from(std::mem::replace(&mut line, vec![rest.clone()])));
                used = rest.width();
            }
            cur.push(c);
            used += w;
            any = true;
        }
        if !cur.is_empty() {
            line.push(Span::styled(cur, span.style));
        }
    }
    lines.push(Line::from(line));
    lines
}
