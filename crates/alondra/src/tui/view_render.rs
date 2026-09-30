//! Text rendering for daemon-built views. The component switch lives here so new view
//! components have one deliberate TUI representation.

use alondra_core::views::{Action, Card, Icon, Item, Label, Row, Section, View};
use ratatui::{
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
};

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightYellow;

pub fn sections(view: &View) -> impl Iterator<Item = &Section> {
    view.sections
        .iter()
        .filter(|section| !section.id.eq("detail"))
}

pub fn rows(view: &View) -> Vec<(&Row, bool)> {
    let mut result = Vec::new();
    for section in &view.sections {
        for item in &section.items {
            match item {
                Item::Row(row) => {
                    result.push((row, false));
                    result.extend(row.children.iter().map(|child| (child, true)));
                }
                Item::Card(_) => {}
                Item::Markdown(_)
                | Item::ContentRef(_)
                | Item::Facts(_)
                | Item::Checks(_)
                | Item::Timeline(_)
                | Item::Form(_)
                | Item::Notice(_)
                | Item::Actions(_) => {}
            }
        }
    }
    result
}

pub fn row_lines(row: &Row, width: usize, selected: bool, nested: bool) -> Vec<Line<'static>> {
    let glyph = icon(row.icon);
    let prefix = if selected { "▌" } else { " " };
    let indent = if nested { "  ↳ " } else { "" };
    let mut first = vec![Span::styled(prefix, Style::new().fg(ACCENT))];
    first.push(Span::styled(
        indent.to_owned(),
        Style::new().fg(Color::Magenta),
    ));
    first.push(Span::styled(
        format!("{glyph} "),
        Style::new()
            .fg(icon_color(row.icon))
            .add_modifier(Modifier::BOLD),
    ));
    first.push(Span::styled(
        row.title.clone(),
        if selected {
            Style::new().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        },
    ));
    let first = fit(first, width);
    let mut lines = vec![first];
    let mut meta = Vec::new();
    if let Some(subtitle) = &row.subtitle {
        meta.push(subtitle.clone());
    }
    meta.extend(row.badges.iter().map(|badge| badge.text.clone()));
    if let Some(trailing) = &row.trailing {
        match trailing {
            alondra_core::views::Trailing::Count { value } => meta.push(value.to_string()),
            alondra_core::views::Trailing::Time { at, .. } => meta.push(at.clone()),
            alondra_core::views::Trailing::Text { text } => meta.push(text.clone()),
        }
    }
    if !meta.is_empty() {
        lines.push(fit(
            vec![
                Span::raw(if nested { "      " } else { "  " }),
                Span::styled(meta.join(" · "), Style::new().fg(DIM)),
            ],
            width,
        ));
    }
    lines
}

pub fn card_lines(card: &Card, width: usize, depth: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let indent = "  ".repeat(depth);
    if let Some(eyebrow) = &card.eyebrow {
        out.push(Line::from(Span::styled(
            format!("{indent}{}", eyebrow.text),
            label_style(eyebrow),
        )));
    }
    out.push(Line::from(Span::styled(
        format!("{indent}{} {}", icon(card.icon), card.title),
        Style::new().add_modifier(Modifier::BOLD),
    )));
    if let Some(subtitle) = &card.subtitle {
        out.push(Line::from(format!("{indent}{subtitle}").fg(DIM)));
    }
    for context in &card.context {
        out.push(Line::from(format!("{indent}{}", context.text).fg(DIM)));
    }
    for badge in &card.badges {
        out.push(Line::from(Span::styled(
            format!("{indent}{}", badge.text),
            label_style(badge),
        )));
    }
    if let Some(body) = &card.body {
        let rendered = super::md::render(&super::md::parse(&body.text), width);
        out.extend(rendered.lines);
    }
    for quote in &card.quotes {
        out.push(Line::from(Span::styled(
            format!("{indent}{}:", quote.label),
            Style::new().fg(DIM).add_modifier(Modifier::ITALIC),
        )));
        out.extend(
            quote
                .text
                .lines()
                .map(|line| Line::from(format!("{indent}  {line}"))),
        );
    }
    if let Some(facts) = &card.facts {
        for fact in &facts.items {
            out.push(Line::from(format!(
                "{indent}{}: {}",
                fact.label, fact.value
            )));
        }
    }
    if let Some(checks) = &card.checks {
        for check in &checks.items {
            out.push(Line::from(format!(
                "{indent}{} {}",
                check_state(check.state),
                check.name
            )));
        }
    }
    for child in &card.children {
        out.push(Line::default());
        out.extend(card_lines(child, width, depth + 1));
    }
    let _ = width;
    out
}

pub fn document_lines(view: &View, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(title) = &view.title {
        lines.push(Line::from(Span::styled(
            title.clone(),
            Style::new().add_modifier(Modifier::BOLD),
        )));
    }
    for notice in &view.notices {
        lines.push(Line::from(Span::styled(
            notice.text.clone(),
            label_style(&Label {
                text: String::new(),
                tone: notice.tone,
            }),
        )));
    }
    for section in &view.sections {
        if let Some(title) = &section.title {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            lines.push(Line::from(Span::styled(
                title.clone(),
                Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
            )));
        }
        for item in &section.items {
            match item {
                Item::Card(card) => lines.extend(card_lines(card, width, 0)),
                Item::Row(row) => {
                    lines.extend(row_lines(row, width, false, row.depth > 0));
                    for child in &row.children {
                        lines.extend(row_lines(child, width, false, true));
                    }
                }
                Item::Markdown(markdown) => {
                    lines.extend(super::md::render(&super::md::parse(&markdown.text), width).lines)
                }
                Item::ContentRef(content) => lines.push(Line::from(vec![
                    Span::styled("▤ ", Style::new().fg(Color::Cyan)),
                    Span::raw(format!("{} · {} B", content.title, content.size)),
                ])),
                Item::Facts(facts) => {
                    for fact in &facts.items {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}: ", fact.label), Style::new().fg(DIM)),
                            Span::raw(fact.value.clone()),
                        ]));
                    }
                }
                Item::Checks(checks) => {
                    for check in &checks.items {
                        lines.push(Line::from(format!(
                            "{} {}",
                            check_state(check.state),
                            check.name
                        )));
                    }
                }
                Item::Timeline(timeline) => {
                    for entry in &timeline.entries {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}  ", entry.at), Style::new().fg(DIM)),
                            Span::raw(entry.title.clone()),
                        ]));
                        if let Some(text) = &entry.text {
                            lines.push(Line::from(format!("  {text}")));
                        }
                    }
                }
                Item::Form(form) => {
                    lines.push(Line::from(Span::styled(
                        form.id.clone(),
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    )));
                    for field in &form.fields {
                        let value = match &field.value {
                            serde_json::Value::String(s) => s.clone(),
                            value => value.to_string(),
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}: ", field.label), Style::new().fg(DIM)),
                            Span::raw(value),
                        ]));
                        if let Some(help) = &field.help {
                            lines.push(Line::from(format!("  {help}").fg(DIM)));
                        }
                    }
                    lines.push(Line::from(vec![
                        Span::styled("[e] ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                        Span::raw(format!("Edit fields and {}", form.submit.label)),
                    ]));
                }
                Item::Notice(notice) => lines.push(Line::from(Span::styled(
                    notice.text.clone(),
                    label_style(&Label {
                        text: String::new(),
                        tone: notice.tone,
                    }),
                ))),
                Item::Actions(bar) => {
                    for action in &bar.actions {
                        lines.push(Line::from(format!(
                            "[{}] {}",
                            action_hint(action),
                            action.label
                        )));
                    }
                }
            }
        }
    }
    lines
}

pub fn actions(view: &View) -> Vec<Action> {
    fn row_actions(row: &Row, out: &mut Vec<Action>) {
        out.extend(row.actions.iter().cloned());
        for child in &row.children {
            row_actions(child, out);
        }
    }
    let mut out = Vec::new();
    for section in sections(view) {
        for item in &section.items {
            match item {
                Item::Card(card) => out.extend(card.actions.iter().cloned()),
                Item::Row(row) => row_actions(row, &mut out),
                Item::Actions(bar) => out.extend(bar.actions.iter().cloned()),
                Item::Form(_) => {}
                Item::Notice(notice) => out.extend(notice.action.iter().cloned()),
                _ => {}
            }
        }
    }
    out
}

pub fn detail_card(view: &View) -> Option<&Card> {
    view.sections
        .iter()
        .find(|section| section.id == "detail" || section.id == "card")?
        .items
        .iter()
        .find_map(|item| match item {
            Item::Card(card) => Some(card),
            _ => None,
        })
}

pub fn action_hint(action: &Action) -> &str {
    action.key.as_deref().unwrap_or_else(|| {
        let label = action.label.to_ascii_lowercase();
        if label.contains("merge") {
            "m"
        } else if label.contains("conflict") {
            "1"
        } else {
            ""
        }
    })
}

pub fn icon(icon: Icon) -> &'static str {
    match icon {
        Icon::Question => "?",
        Icon::Action => "⚡",
        Icon::Document => "▤",
        Icon::PullRequest => "⑂",
        Icon::Notice => "·",
        Icon::Delivery => "⇢",
        Icon::Task => "□",
        Icon::Request => "＋",
        Icon::Node => "●",
        Icon::Orchestrator => "◆",
        Icon::Mission => "◇",
        Icon::Project => "▰",
        Icon::Schedule => "◷",
        Icon::Attachment => "📎",
        Icon::Check => "✓",
        Icon::Warning => "!",
        Icon::Merge => "⑂",
        Icon::Branch => "⑂",
        Icon::History => "◷",
        Icon::Machine => "⌘",
    }
}

pub fn icon_color(icon: Icon) -> Color {
    match icon {
        Icon::Question => ACCENT,
        Icon::PullRequest | Icon::Merge => Color::Magenta,
        Icon::Warning => Color::LightRed,
        _ => Color::Cyan,
    }
}

fn label_style(label: &Label) -> Style {
    Style::new().fg(match label.tone {
        alondra_core::views::Tone::Neutral => DIM,
        alondra_core::views::Tone::Info => Color::Cyan,
        alondra_core::views::Tone::Accent => ACCENT,
        alondra_core::views::Tone::Success => Color::LightGreen,
        alondra_core::views::Tone::Warning => Color::Yellow,
        alondra_core::views::Tone::Critical => Color::LightRed,
    })
}

fn check_state(state: alondra_core::views::CheckState) -> &'static str {
    match state {
        alondra_core::views::CheckState::Pass => "✓",
        alondra_core::views::CheckState::Fail => "✗",
        alondra_core::views::CheckState::Pending => "…",
    }
}

fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let line = Line::from(spans);
    let mut result = Vec::new();
    let mut remaining = width;
    for span in line.spans {
        if remaining == 0 {
            break;
        }
        let text = span.content.to_string();
        let take = super::md::truncate(&text, remaining);
        remaining = remaining.saturating_sub(super::md::str_width(&take));
        result.push(Span::styled(take, span.style));
    }
    Line::from(result)
}
