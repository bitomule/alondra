//! The one line a card shows for its content: what the agent sent, or one Alondra counts itself.

use super::{
    Lang,
    text::{t, tf},
};
use crate::domain::{ContentKind, CardContent, clip};

fn items(count: usize, one: &str, many: &str) -> String {
    if count == 1 { format!("1 {one}") } else { format!("{count} {many}") }
}

fn steps(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => items(count, "step", "steps"),
        Lang::Es => items(count, "paso", "pasos"),
    }
}
fn sections(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => items(count, "section", "sections"),
        Lang::Es => items(count, "sección", "secciones"),
    }
}
fn points(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => items(count, "point", "points"),
        Lang::Es => items(count, "punto", "puntos"),
    }
}
pub(super) fn options(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => items(count, "option", "options"),
        Lang::Es => items(count, "opción", "opciones"),
    }
}
pub(super) fn attachments(lang: Lang, count: usize) -> String {
    match lang {
        Lang::En => items(count, "attachment", "attachments"),
        Lang::Es => items(count, "adjunto", "adjuntos"),
    }
}
fn files(lang: Lang, count: u64) -> String {
    match lang {
        Lang::En => items(count as usize, "file", "files"),
        Lang::Es => items(count as usize, "archivo", "archivos"),
    }
}

/// "6 files · +120 −40".
pub(super) fn changes(lang: Lang, files_changed: u64, added: u64, removed: u64) -> String {
    tf!(lang, "{files} · +{added} −{removed}", "{files} · +{added} −{removed}",
        files = files(lang, files_changed), added = added, removed = removed)
}

/// The files and lines of a unified diff.
pub(super) fn diff_stats(diff: &str) -> Option<(u64, u64, u64)> {
    let (mut files, mut added, mut removed) = (0, 0, 0);
    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            files += 1;
        } else if line.starts_with("+++") || line.starts_with("---") {
            continue;
        } else if line.starts_with('+') {
            added += 1;
        } else if line.starts_with('-') {
            removed += 1;
        }
    }
    (files > 0).then_some((files, added, removed))
}

fn list_marker(line: &str) -> bool {
    let line = line.trim_start();
    if line.starts_with(['-', '*', '+']) && line[1..].starts_with(' ') {
        return true;
    }
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    digits > 0 && line[digits..].starts_with(['.', ')']) && line[digits + 1..].starts_with(' ')
}
fn headings(body: &str) -> usize {
    body.lines().filter(|line| line.trim_start().starts_with('#')).count()
}
fn paragraphs(body: &str) -> usize {
    body.split("\n\n").filter(|paragraph| !paragraph.trim().is_empty()).count()
}
fn first_line(body: &str) -> String {
    body.lines().map(str::trim).find(|line| !line.is_empty()).map(|line| clip(line, 60)).unwrap_or_default()
}

/// What the agent sent, else what Alondra counts in the content itself.
pub(super) fn of_content(lang: Lang, content: &CardContent) -> Option<String> {
    if let Some(summary) = content.summary.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        return Some(clip(summary, 60));
    }
    let body = content.body.as_str();
    let counted = body.lines().filter(|line| list_marker(line)).count();
    match content.kind {
        ContentKind::Plan => Some(match (counted, headings(body)) {
            (0, 0) => t!(lang, "Plan", "Plan").to_owned(),
            (0, sections_found) => sections(lang, sections_found),
            (steps_found, _) => steps(lang, steps_found),
        }),
        ContentKind::Steps => Some(steps(lang, if counted > 0 { counted } else { body.lines().filter(|l| !l.trim().is_empty()).count() })),
        ContentKind::Document => Some(match headings(body) {
            0 => t!(lang, "Document", "Documento").to_owned(),
            found => sections(lang, found),
        }),
        ContentKind::Diff => diff_stats(body).map(|(files, added, removed)| changes(lang, files, added, removed)),
        ContentKind::Command => Some(first_line(body)).filter(|line| !line.is_empty()),
        ContentKind::Report => Some(points(lang, if counted > 0 { counted } else { paragraphs(body).max(1) })),
        ContentKind::Diagnosis => Some(first_line(body)).filter(|line| !line.is_empty()),
        ContentKind::Options | ContentKind::Attachments => None,
    }
}

pub(super) fn label(lang: Lang, kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Plan => t!(lang, "Plan", "Plan"),
        ContentKind::Document => t!(lang, "Document", "Documento"),
        ContentKind::Diff => t!(lang, "Changes", "Cambios"),
        ContentKind::Command => t!(lang, "Command", "Comando"),
        ContentKind::Report => t!(lang, "Report", "Informe"),
        ContentKind::Steps => t!(lang, "Steps", "Pasos"),
        ContentKind::Options => t!(lang, "Why each option", "Por qué cada opción"),
        ContentKind::Diagnosis => t!(lang, "Diagnosis", "Diagnóstico"),
        ContentKind::Attachments => t!(lang, "Attachments", "Adjuntos"),
    }
}

pub(super) fn kind_name(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Plan => "plan",
        ContentKind::Document => "document",
        ContentKind::Diff => "diff",
        ContentKind::Command => "command",
        ContentKind::Report => "report",
        ContentKind::Steps => "steps",
        ContentKind::Options => "options",
        ContentKind::Diagnosis => "diagnosis",
        ContentKind::Attachments => "attachments",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(kind: ContentKind, body: &str) -> CardContent {
        CardContent { kind, summary: None, body: body.into() }
    }
    fn summary_of(kind: ContentKind, body: &str, lang: Lang) -> Option<String> {
        of_content(lang, &content(kind, body))
    }

    #[test]
    fn a_diff_reads_files_and_lines() {
        let diff = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n+one\n+two\n-three\ndiff --git a/b.rs b/b.rs\n+++ b/b.rs\n+four\n";
        assert_eq!(summary_of(ContentKind::Diff, diff, Lang::En).as_deref(), Some("2 files · +3 −1"));
        assert_eq!(summary_of(ContentKind::Diff, diff, Lang::Es).as_deref(), Some("2 archivos · +3 −1"));
        assert_eq!(summary_of(ContentKind::Diff, "not a diff", Lang::En), None, "without a diff there is nothing to count");
        assert_eq!(changes(Lang::En, 1, 12, 0), "1 file · +12 −0");
    }

    #[test]
    fn a_plan_and_steps_count_their_steps() {
        let plan = "# Ship it\n\n1. Split the lexer\n2. Add tests\n- Tidy up";
        assert_eq!(summary_of(ContentKind::Plan, plan, Lang::En).as_deref(), Some("3 steps"));
        assert_eq!(summary_of(ContentKind::Plan, plan, Lang::Es).as_deref(), Some("3 pasos"));
        assert_eq!(summary_of(ContentKind::Plan, "# One\n\ntext\n\n# Two", Lang::En).as_deref(), Some("2 sections"));
        assert_eq!(summary_of(ContentKind::Plan, "just words", Lang::En).as_deref(), Some("Plan"));
        assert_eq!(summary_of(ContentKind::Steps, "1. Open\n2. Press\n3. Wait\n4. Done", Lang::En).as_deref(), Some("4 steps"));
        assert_eq!(summary_of(ContentKind::Steps, "Open it", Lang::En).as_deref(), Some("1 step"));
    }

    #[test]
    fn a_command_shows_its_first_line_and_a_report_its_points() {
        assert_eq!(summary_of(ContentKind::Command, "\nalondra update\nrm -rf build", Lang::En).as_deref(), Some("alondra update"));
        assert_eq!(summary_of(ContentKind::Report, "- one\n- two", Lang::En).as_deref(), Some("2 points"));
        assert_eq!(summary_of(ContentKind::Report, "First.\n\nSecond.\n\nThird.", Lang::Es).as_deref(), Some("3 puntos"));
        assert_eq!(summary_of(ContentKind::Report, "One thing", Lang::En).as_deref(), Some("1 point"));
        assert_eq!(summary_of(ContentKind::Document, "# A\n## B\n## C", Lang::En).as_deref(), Some("3 sections"));
    }

    #[test]
    fn what_the_agent_sent_wins_and_is_kept_short() {
        let mut sent = content(ContentKind::Steps, "1. Open\n2. Press");
        sent.summary = Some("two taps".into());
        assert_eq!(of_content(Lang::En, &sent).as_deref(), Some("two taps"));
        sent.summary = Some("x".repeat(80));
        assert_eq!(of_content(Lang::En, &sent).unwrap().chars().count(), 60);
        assert_eq!(options(Lang::En, 2), "2 options");
        assert_eq!(attachments(Lang::Es, 3), "3 adjuntos");
    }
}
