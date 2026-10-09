//! Text rendering for daemon-built views. The component switch lives here so new view
//! components have one deliberate TUI representation.

use std::borrow::Cow;
use std::collections::BTreeSet;

use alondra_core::domain::CardKind;
use alondra_core::views::{Action, Card, Icon, Item, Label, Row, Section, View};
use ratatui::{
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span},
};

const DIM: Color = Color::DarkGray;
const ACCENT: Color = Color::LightYellow;

/// The row that opens the schedules: `s` and the header's `Scheduled` tab do that in the TUI, so
/// the list of tasks starts at the first task.
fn opens_schedules(view: &View, id: &str) -> bool {
    view.key == "tasks" && id == "schedules"
}

pub fn sections(view: &View) -> impl Iterator<Item = &Section> {
    view.sections.iter().filter(|section| {
        header_key(view, section).is_some() || (!section.id.eq("detail")
            && !section.items.is_empty()
            && !(view.key == "tasks"
                && section.items.iter().all(
                    |item| matches!(item, Item::Row(row) if opens_schedules(view, &row.id)),
                )))
    })
}

/// What a list screen lets the user move over, in order: its rows and its cards, each with the
/// view it opens and whether it hangs under the one above.
const SECTION_HEADER: &str = "section:";

/// Tasks folds its finished and archived sections; the rest of the lists stay open as they were.
fn folds(view: &View, section: &Section) -> bool {
    view.key == "tasks" && section.collapsed
}

/// The list key of a section header the user opens and closes: one the daemon sends folded.
pub fn header_key(view: &View, section: &Section) -> Option<String> {
    (folds(view, section) && section.title.is_some() && (!section.items.is_empty() || section.more.is_some()))
        .then(|| format!("{SECTION_HEADER}{}", section.id))
}

/// The section a header key opens and closes.
pub fn header_section(key: &str) -> Option<&str> {
    key.strip_prefix(SECTION_HEADER)
}

/// A section the daemon sends folded stays folded until the user opens it.
pub fn folded(view: &View, section: &Section, open: &BTreeSet<String>) -> bool {
    folds(view, section) && !open.contains(&section.id)
}

pub fn entries(
    view: &View,
    open: &BTreeSet<String>,
    open_rows: &BTreeSet<String>,
) -> Vec<(String, bool, Option<alondra_core::views::Link>)> {
    let mut result = Vec::new();
    for section in &view.sections {
        if let Some(key) = header_key(view, section) {
            result.push((key, false, None));
        }
        if folded(view, section, open) {
            continue;
        }
        for item in &section.items {
            match item {
                Item::Row(row) if opens_schedules(view, &row.id) => {}
                Item::Row(row) => {
                    result.push((row.id.clone(), false, row.link.clone()));
                    result.extend(shown_rows(row, open_rows).into_iter().map(|child| (child.id.clone(), true, child.link.clone())));
                }
                Item::Card(card) if section.id != "detail" => {
                    result.push((card.id.clone(), false, card.link.clone()));
                }
                _ => {}
            }
        }
    }
    result
}

/// Everything under a row, in reading order: the list draws a task tree as one indented column.
pub fn nested_rows(row: &Row) -> Vec<&Row> {
    row.children.iter().flat_map(|child| std::iter::once(child).chain(nested_rows(child))).collect()
}

/// The rows under `row` the list shows: a row the daemon sends folded hides its own until the user opens it.
pub fn shown_rows<'a>(row: &'a Row, open: &BTreeSet<String>) -> Vec<&'a Row> {
    if row.folded && !open.contains(&row.id) {
        return Vec::new();
    }
    row.children.iter().flat_map(|child| std::iter::once(child).chain(shown_rows(child, open))).collect()
}

/// A folded row says with ▸ or ▾ whether the rows under it are shown.
pub fn fold_marked<'a>(row: &'a Row, open: &BTreeSet<String>) -> Cow<'a, Row> {
    if !row.folded {
        return Cow::Borrowed(row);
    }
    let mark = if open.contains(&row.id) { "▾" } else { "▸" };
    Cow::Owned(Row { title: format!("{mark} {}", row.title), ..row.clone() })
}

/// Whether a section carries the Tasks list's blocks (For you, In progress, Waiting) as rows.
pub fn has_blocks(section: &Section) -> bool {
    section.items.iter().any(|item| matches!(item, Item::Row(row) if row.id.starts_with("block:")))
}

/// The TUI lists each block of the Tasks list as a section of its own, its title over its tasks.
pub fn lift_blocks(sections: &mut Vec<Section>) {
    let Some(at) = sections.iter().position(has_blocks) else { return };
    let section = sections.remove(at);
    let blocks = section.items.into_iter().filter_map(|item| match item {
        Item::Row(row) if row.id.starts_with("block:") => Some(row),
        _ => None,
    }).map(|block| {
        let count = match &block.trailing {
            Some(alondra_core::views::Trailing::Text { text }) => text.parse().ok(),
            _ => None,
        };
        Section {
            id: block.id,
            title: Some(block.title),
            count,
            collapsed: false,
            items: block.children.into_iter().map(Item::Row).collect(),
            card_ids: vec![],
            card_waits: vec![],
            more: None,
        }
    });
    sections.splice(at..at, blocks);
}

pub fn rows(view: &View) -> Vec<(&Row, bool)> {
    let mut result = Vec::new();
    for section in &view.sections {
        for item in &section.items {
            match item {
                Item::Row(row) if opens_schedules(view, &row.id) => {}
                Item::Row(row) => {
                    result.push((row, false));
                    result.extend(nested_rows(row).into_iter().map(|child| (child, true)));
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

/// One list item: glyph and title, and on the right its state and since when, the way 8ccd131
/// laid rows out. What does not fit beside the title goes on the line below; a subtitle (what a
/// node runs now) goes under it.
pub fn row_lines(row: &Row, width: usize, selected: bool, nested: bool) -> Vec<Line<'static>> {
    let bar = Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT));
    let mut title_style = Style::default();
    if selected {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    if row.struck {
        title_style = title_style.add_modifier(Modifier::CROSSED_OUT);
    }
    let mut first = vec![
        bar.clone(),
        Span::styled(
            if nested { "  ↳ " } else { "" },
            Style::new().fg(Color::Magenta),
        ),
        Span::styled(
            format!("{} ", icon(row.icon)),
            Style::new()
                .fg(icon_color(row.icon))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let mut seen = std::collections::HashSet::new();
    let mut meta: Vec<Span<'static>> = Vec::new();
    let mut parts: Vec<(String, Style)> = row
        .badges
        .iter()
        .map(|badge| (badge.text.clone(), label_style(badge)))
        .collect();
    match &row.trailing {
        Some(alondra_core::views::Trailing::Count { value }) if *value > 0 => {
            parts.push((value.to_string(), Style::new().fg(DIM)))
        }
        Some(alondra_core::views::Trailing::Count { .. }) | None => {}
        Some(alondra_core::views::Trailing::Time { at, format }) => {
            parts.push((format_time(at, *format), Style::new().fg(DIM)))
        }
        Some(alondra_core::views::Trailing::Text { text }) => {
            parts.push((text.clone(), Style::new().fg(DIM)))
        }
    }
    for (text, style) in parts {
        if text.is_empty() || !seen.insert(text.clone()) {
            continue;
        }
        if !meta.is_empty() {
            meta.push(Span::styled(" · ", Style::new().fg(DIM)));
        }
        meta.push(Span::styled(text, style));
    }
    let subtitle = row
        .subtitle
        .as_ref()
        .filter(|subtitle| !subtitle.is_empty() && !seen.contains(*subtitle));
    let lead_width: usize = first.iter().map(Span::width).sum();
    let meta_width: usize = meta.iter().map(Span::width).sum();
    let title_width = super::md::str_width(&row.title);
    let under = if nested { "      " } else { "  " };
    first.push(Span::styled(row.title.clone(), title_style));
    let mut lines = Vec::new();
    if meta.is_empty() || lead_width + title_width + 2 + meta_width <= width {
        if !meta.is_empty() {
            first.push(Span::raw(
                " ".repeat(width - lead_width - title_width - meta_width),
            ));
            first.extend(meta);
        }
        lines.push(fit(first, width));
    } else {
        lines.push(fit(first, width));
        let mut second = vec![bar.clone(), Span::raw(under)];
        second.extend(meta);
        lines.push(fit(second, width));
    }
    if let Some(subtitle) = subtitle {
        lines.push(fit(
            vec![
                bar,
                Span::raw(under),
                Span::styled(subtitle.clone(), Style::new().fg(Color::Gray)),
            ],
            width,
        ));
    }
    lines
}

/// The cards a list view shows, in the order the daemon sorted them.
pub fn list_cards(view: &View) -> Vec<&Card> {
    sections(view)
        .flat_map(|section| &section.items)
        .filter_map(|item| match item {
            Item::Card(card) => Some(card),
            _ => None,
        })
        .collect()
}

pub fn kind_glyph(kind: CardKind) -> &'static str {
    match kind {
        CardKind::Decide => "?",
        CardKind::Review => "◎",
        CardKind::Authorize => "◆",
        CardKind::Know => "·",
        CardKind::Provide => "✎",
        CardKind::Test => "▶",
        CardKind::Do => "▲",
        CardKind::Problem => "!",
        CardKind::Propose => "✦",
    }
}

/// The terminal's nearest colour to each type's colour on the design board.
pub fn kind_color(kind: CardKind) -> Color {
    match kind {
        CardKind::Decide => ACCENT,
        CardKind::Review | CardKind::Test => Color::Green,
        CardKind::Authorize => Color::LightRed,
        CardKind::Know => DIM,
        CardKind::Provide => Color::Magenta,
        CardKind::Do => Color::LightBlue,
        CardKind::Problem => Color::Yellow,
        CardKind::Propose => Color::Gray,
    }
}

fn card_glyph(card: &Card) -> (&'static str, Color) {
    match card.kind {
        Some(kind) => (kind_glyph(kind), kind_color(kind)),
        None => (icon(card.icon), icon_color(card.icon)),
    }
}

fn card_age(card: &Card) -> Option<String> {
    card.timestamps
        .first()
        .map(|stamp| format_time(&stamp.at, alondra_core::views::TimeFormat::Relative))
}

/// One list item per card: its type's glyph, what it needs, and on the right who sent it and how
/// long ago. A card another one holds back says by what, dim, on the line below.
pub fn card_row_lines(card: &Card, width: usize, selected: bool) -> Vec<Line<'static>> {
    let bar = Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT));
    let (glyph, color) = card_glyph(card);
    let lead = vec![
        bar.clone(),
        Span::styled(format!("{glyph} "), Style::new().fg(color).add_modifier(Modifier::BOLD)),
    ];
    let lead_width: usize = lead.iter().map(Span::width).sum();
    let budget = width.saturating_sub(lead_width);
    let right = [card.meta.clone(), card_age(card)]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let right = super::md::truncate(&right, super::md::str_width(&right).min(budget / 3));
    let gap = if right.is_empty() { 0 } else { 2 };
    let need = card.need.as_deref().unwrap_or(&card.title);
    let need = super::md::truncate(need, budget.saturating_sub(super::md::str_width(&right) + gap));
    let padding = budget.saturating_sub(super::md::str_width(&need) + super::md::str_width(&right));
    let mut need_style = Style::default();
    if selected {
        need_style = need_style.add_modifier(Modifier::BOLD);
    }
    let mut first = lead;
    first.push(Span::styled(need, need_style));
    first.push(Span::raw(" ".repeat(padding)));
    first.push(Span::styled(right, Style::new().fg(DIM)));
    let mut lines = vec![fit(first, width)];
    for chip in &card.blocked_by {
        lines.push(fit(
            vec![bar.clone(), Span::raw("  "), Span::styled(chip.label.clone(), Style::new().fg(DIM))],
            width,
        ));
    }
    lines
}

/// The agent a card came from, the way its header names it.
pub fn card_agent(card: &Card) -> Option<&alondra_core::views::ContextLine> {
    card.context.iter().find(|line| line.icon == Icon::Node)
}

/// `◆ AUTHORIZE · Alondra · iOS · Demo Garden · 3d ago`: what it is, who it came from, how old.
fn card_header(card: &Card) -> Line<'static> {
    let (glyph, color) = card_glyph(card);
    let name = card.eyebrow.as_ref().map(|eyebrow| eyebrow.text.to_uppercase()).unwrap_or_default();
    let mut spans = vec![Span::styled(
        format!("{glyph} {name}").trim_end().to_owned(),
        Style::new().fg(color).add_modifier(Modifier::BOLD),
    )];
    for part in [card.meta.clone(), card_age(card)].into_iter().flatten() {
        spans.push(Span::styled(format!(" · {part}"), Style::new().fg(DIM)));
    }
    Line::from(spans)
}

/// The top of the detail: type, what is needed, why, and what holds it back. The content, the
/// context and the history follow as the daemon's own sections.
pub fn card_lines(card: &Card, width: usize) -> Vec<Line<'static>> {
    let mut out = vec![card_header(card)];
    let need = card.need.as_deref().unwrap_or(&card.title);
    out.push(Line::from(Span::styled(need.to_owned(), Style::new().add_modifier(Modifier::BOLD))));
    if let Some(why) = card.why.as_ref().or(card.subtitle.as_ref()) {
        out.push(Line::from(Span::styled(why.clone(), Style::new().fg(Color::Gray))));
    }
    for chip in card.blocked_by.iter().chain(&card.blocks) {
        out.push(Line::from(chip.label.clone().fg(ACCENT)));
    }
    for badge in &card.badges {
        out.push(Line::from(Span::styled(badge.text.clone(), label_style(badge))));
    }
    if let Some(body) = &card.body {
        out.extend(super::md::render(&super::md::parse(&body.text), width).lines);
    }
    out
}

pub const BUTTONS: usize = 3;

/// What the keys of a card do: its buttons numbered from 1, the menu of the rest, the reply.
pub fn button_lines(card: &Card) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, action) in card.actions.iter().take(BUTTONS).enumerate() {
        lines.extend(button_line(&(index + 1).to_string(), action));
    }
    if !card.more.is_empty() {
        lines.push(Line::from(vec![
            keycap_span("m", Color::Gray),
            Span::raw(format!(" {} ({})", tr!("More…", "Más…"), card.more.len())),
        ]));
    }
    if let Some(reply) = &card.reply {
        lines.extend(button_line("r", reply));
    }
    lines
}

fn keycap_span(key: &str, background: Color) -> Span<'static> {
    Span::styled(format!(" {key} "), Style::new().fg(Color::Black).bg(background).add_modifier(Modifier::BOLD))
}

fn role_color(action: &Action) -> Color {
    match action.role {
        Some(alondra_core::views::Role::Advance) => Color::Green,
        Some(alondra_core::views::Role::Risk) => Color::LightRed,
        Some(alondra_core::views::Role::Neutral) | None => Color::Gray,
    }
}

fn button_line(key: &str, action: &Action) -> Vec<Line<'static>> {
    vec![Line::from(vec![keycap_span(key, role_color(action)), Span::raw(format!(" {}", action.label))])]
}

/// A numbered list the user picks from: the rest of a card's actions, or the options of one.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<(Action, Option<alondra_core::views::ActionOption>)>,
}

pub fn more_menu(card: &Card) -> Menu {
    Menu {
        title: tr!("More", "Más").into(),
        entries: card.more.iter().take(9).map(|action| (Action::from(action.clone()), None)).collect(),
    }
}

pub fn option_menu(action: &Action) -> Menu {
    Menu {
        title: action.label.clone(),
        entries: action
            .options
            .iter()
            .take(9)
            .map(|option| (action.clone(), Some(option.clone())))
            .collect(),
    }
}

pub fn menu_lines(menu: &Menu) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(
        menu.title.to_uppercase(),
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    for (index, (action, option)) in menu.entries.iter().enumerate() {
        let key = (index + 1).to_string();
        match option {
            Some(option) => {
                let mut entry = action.clone();
                entry.label = super::draw::option_label(option);
                lines.extend(button_line(&key, &entry));
                if let Some(description) = &option.description {
                    lines.push(Line::from(Span::styled(format!("    {description}"), Style::new().fg(DIM))));
                }
            }
            None => lines.extend(button_line(&key, action)),
        }
    }
    lines.push(Line::from(vec![
        Span::styled(" Esc ", Style::new().fg(DIM)),
        Span::styled(tr!("back", "volver"), Style::new().fg(DIM)),
    ]));
    lines
}

/// A diff says what it changes ("4 files · +120 −30"), the size of a body only when it has one.
fn content_ref_label(view: &View, content: &alondra_core::views::ContentRef) -> String {
    let summary = detail_card(view)
        .and_then(|card| card.content_row.as_ref())
        .filter(|row| row.kind == "diff" && content.kind == alondra_core::views::ContentKind::Diff)
        .map(|row| row.summary.clone());
    match summary {
        Some(summary) => format!("{} · {summary}", content.title),
        None if content.size == 0 => content.title.clone(),
        None => format!("{} · {} B", content.title, content.size),
    }
}

pub fn document_lines(view: &View, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let cards_title = |title: &String| {
        view.sections
            .iter()
            .flat_map(|section| &section.items)
            .any(|item| matches!(item, Item::Card(card) if card.title == *title))
    };
    if let Some(title) = view.title.as_ref().filter(|title| !cards_title(title)) {
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
    for section in view.sections.iter().filter(|section| !section.items.is_empty()) {
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
                Item::Card(card) => lines.extend(card_lines(card, width)),
                Item::Row(row) => {
                    lines.extend(row_lines(row, width, false, row.depth > 0));
                    for child in shown_rows(row, &BTreeSet::new()) {
                        lines.extend(row_lines(&fold_marked(child, &BTreeSet::new()), width, false, true));
                    }
                }
                Item::Markdown(markdown) => {
                    lines.extend(super::md::render(&super::md::parse(&markdown.text), width).lines)
                }
                Item::ContentRef(content) => lines.push(Line::from(vec![
                    Span::styled("▤ ", Style::new().fg(Color::Cyan)),
                    Span::raw(content_ref_label(view, content)),
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
                            Span::styled(format!("{}  ", format_time(&entry.at, alondra_core::views::TimeFormat::Absolute)), Style::new().fg(DIM)),
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
                        Span::raw({
                            let label = &form.submit.label;
                            trf!("Edit the fields and {label}", "Edita los campos y {label}")
                        }),
                    ]));
                }
                Item::Notice(notice) => lines.push(Line::from(Span::styled(
                    notice.text.clone(),
                    label_style(&Label {
                        text: String::new(),
                        tone: notice.tone,
                    }),
                ))),
                Item::Actions(_) => {}
            }
        }
    }
    lines
}

pub(super) fn action_line(action: &Action) -> String {
    let hint = action_hint(action);
    if hint.is_empty() {
        action.label.clone()
    } else {
        format!("[{hint}] {}", action.label)
    }
}

pub fn format_time(at: &str, format: alondra_core::views::TimeFormat) -> String {
    let Some(millis) = parse_utc(at) else {
        return at.to_owned();
    };
    let Some(offset) = local_utc_offset(millis) else {
        return at.to_owned();
    };
    let now = alondra_core::now_millis();
    format_time_at(at, millis, offset, format, now, local_utc_offset(now).unwrap_or(offset))
}

fn format_time_at(
    at: &str,
    millis: i64,
    utc_offset_seconds: i64,
    format: alondra_core::views::TimeFormat,
    now: i64,
    now_offset_seconds: i64,
) -> String {
    let Some((date, time)) = date_time_at_offset(millis, utc_offset_seconds) else {
        return at.to_owned();
    };
    match format {
        alondra_core::views::TimeFormat::Relative => {
            relative(millis, utc_offset_seconds, now, now_offset_seconds, &time)
        }
        alondra_core::views::TimeFormat::Absolute => format!("{date} {time}"),
        alondra_core::views::TimeFormat::Time => time,
    }
}

/// "ahora", "hace 5 min", "hoy 05:00", "ayer 22:10", "hace 3 días", "28 sep": how long ago in the
/// words a person uses, by the local calendar day.
fn relative(millis: i64, offset: i64, now: i64, now_offset: i64, time: &str) -> String {
    let local_day = |at: i64, offset: i64| (at.div_euclid(1_000) + offset).div_euclid(86_400);
    let days = local_day(now, now_offset) - local_day(millis, offset);
    let seconds = (now - millis) / 1_000;
    if seconds.abs() < 60 {
        return tr!("now", "ahora").into();
    }
    if (60..3_600).contains(&seconds) {
        let minutes = seconds / 60;
        return trf!("{minutes} min ago", "hace {minutes} min");
    }
    match days {
        0 => trf!("today {time}", "hoy {time}"),
        1 => trf!("yesterday {time}", "ayer {time}"),
        -1 => trf!("tomorrow {time}", "mañana {time}"),
        2..=6 => trf!("{days} days ago", "hace {days} días"),
        _ => {
            let Some((date, _)) = date_time_at_offset(millis, offset) else {
                return time.to_owned();
            };
            let mut parts = date.split('-');
            let (year, month, day) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
            let month = month.parse::<usize>().unwrap_or(1).clamp(1, 12) - 1;
            let day = day.trim_start_matches('0');
            let spanish = crate::i18n::lang() == crate::i18n::Lang::Es;
            let name = if spanish {
                ["ene", "feb", "mar", "abr", "may", "jun", "jul", "ago", "sep", "oct", "nov", "dic"][month]
            } else {
                ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"][month]
            };
            let same_year = date_time_at_offset(now, now_offset)
                .is_some_and(|(today, _)| today.starts_with(year));
            match (spanish, same_year) {
                (true, true) => format!("{day} {name}"),
                (true, false) => format!("{day} {name} {year}"),
                (false, true) => format!("{name} {day}"),
                (false, false) => format!("{name} {day}, {year}"),
            }
        }
    }
}

/// An RFC 3339 instant, `Z` or with its UTC offset, in milliseconds since the epoch.
fn parse_utc(value: &str) -> Option<i64> {
    let (value, offset_seconds) = match value.strip_suffix('Z') {
        Some(value) => (value, 0),
        None => {
            let split = value.len().checked_sub(6)?;
            let (value, offset) = (value.get(..split)?, value.get(split..)?);
            let sign = match offset.as_bytes().first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let (hours, minutes) = offset.get(1..)?.split_once(':')?;
            (value, sign * (hours.parse::<i64>().ok()? * 3_600 + minutes.parse::<i64>().ok()? * 60))
        }
    };
    let (date, clock) = value.split_once('T')?;
    let mut date_parts = date.split('-');
    let (year, month, day) = (
        date_parts.next()?.parse::<i64>().ok()?,
        date_parts.next()?.parse::<i64>().ok()?,
        date_parts.next()?.parse::<i64>().ok()?,
    );
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let clock = clock.split('.').next()?;
    let mut time_parts = clock.split(':');
    let (hour, minute, second) = (
        time_parts.next()?.parse::<i64>().ok()?,
        time_parts.next()?.parse::<i64>().ok()?,
        time_parts.next()?.parse::<i64>().ok()?,
    );
    if time_parts.next().is_some() || hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    let year_adj = year - i64::from(month <= 2);
    let era = year_adj.div_euclid(400);
    let year_of_era = year_adj - era * 400;
    let month_adj = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_adj + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    let fraction = value
        .split_once('.')
        .map(|(_, fraction)| fraction)
        .filter(|fraction| !fraction.is_empty() && fraction.bytes().all(|b| b.is_ascii_digit()))
        .map(|fraction| format!("{fraction:0<3}")[..3].parse::<i64>().unwrap_or(0))
        .unwrap_or(0);
    let millis = ((days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds) * 1_000)
        + fraction;
    Some(millis)
}

fn local_utc_offset(millis: i64) -> Option<i64> {
    let seconds = millis.div_euclid(1_000);
    let timestamp = libc::time_t::try_from(seconds).ok()?;
    // SAFETY: localtime_r writes to the supplied `tm` and does not retain either pointer.
    let tm = unsafe {
        let mut tm = std::mem::MaybeUninit::<libc::tm>::uninit();
        libc::localtime_r(&timestamp, tm.as_mut_ptr()).as_ref()?;
        tm.assume_init()
    };
    Some(i64::from(tm.tm_gmtoff))
}

fn date_time_at_offset(millis: i64, utc_offset_seconds: i64) -> Option<(String, String)> {
    let seconds = millis.div_euclid(1_000) + utc_offset_seconds;
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_adj = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_adj + 2) / 5 + 1;
    let month = if month_adj < 10 { month_adj + 3 } else { month_adj - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Some((
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}", second_of_day / 3_600, second_of_day % 3_600 / 60),
    ))
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
                Item::Card(card) => out.extend(card.actions.iter().chain(&card.reply).chain(&card.more).cloned().map(Action::from)),
                Item::Row(row) => row_actions(row, &mut out),
                Item::Actions(bar) => out.extend(bar.actions.iter().cloned()),
                Item::Form(_) => {}
                Item::Notice(notice) => out.extend(notice.action.iter().cloned()),
                _ => {}
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    out.retain(|action| seen.insert((action.id.clone(), action.option_id.clone(), action.id.is_empty().then(|| action.label.clone()))));
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

/// The card a detail view is about, when it follows the card contract: its keys are 1-3, m and r.
pub fn typed_card(view: &View) -> Option<&Card> {
    detail_card(view).filter(|card| card.kind.is_some())
}

pub fn action_hint(action: &Action) -> &str {
    action.key.as_deref().unwrap_or_else(|| {
        use alondra_core::views::action_id::{ActionId, Verb};
        match ActionId::parse(&action.id).map(|id| id.verb) {
            Ok(Verb::Merge) => "m",
            Ok(Verb::Conflicts) => "1",
            Ok(Verb::Withdraw) => "x",
            _ => "",
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
        alondra_core::views::Tone::Warning | alondra_core::views::Tone::YourTurn => Color::Yellow,
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

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::views::{Input, Presentation, Style};

    #[test]
    fn view_times_are_formatted_for_people_instead_of_showing_wire_timestamps() {
        use alondra_core::views::TimeFormat;
        let at = "2026-10-01T23:55:00.000Z";
        let instant = parse_utc(at).unwrap();
        let format = |offset, format| format_time_at(at, instant, offset, format, instant, offset);
        assert_eq!(format(2 * 3_600, TimeFormat::Absolute), "2026-10-02 01:55");
        assert_eq!(format(2 * 3_600, TimeFormat::Time), "01:55");
        assert_eq!(format(-(5 * 3_600 + 30 * 60), TimeFormat::Absolute), "2026-10-01 18:25");
        assert_eq!(format(0, TimeFormat::Time), "23:55");
        assert_eq!(format_time("not-a-date", TimeFormat::Absolute), "not-a-date");
    }

    #[test]
    fn offset_timestamps_from_the_daemon_are_read_as_the_same_instant() {
        assert_eq!(
            parse_utc("2026-10-03T19:49:18.945+02:00"),
            parse_utc("2026-10-03T17:49:18.945Z")
        );
        assert_eq!(
            parse_utc("2026-10-03T05:00:00.861-03:30"),
            parse_utc("2026-10-03T08:30:00.861Z")
        );
        assert_eq!(parse_utc("2026-10-03T17:49:18.9Z").map(|ms| ms % 1_000), Some(900));
    }

    #[test]
    fn relative_times_read_like_people_say_them_by_local_day() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let madrid = 2 * 3_600;
        let now = parse_utc("2026-10-03T19:54:18.945+02:00").unwrap();
        let ago = |at: &str| {
            let instant = parse_utc(at).unwrap();
            format_time_at(at, instant, madrid, alondra_core::views::TimeFormat::Relative, now, madrid)
        };
        assert_eq!(ago("2026-10-03T19:54:00.000+02:00"), "ahora");
        assert_eq!(ago("2026-10-03T19:49:18.945+02:00"), "hace 5 min");
        assert_eq!(ago("2026-10-03T05:00:00.861+02:00"), "hoy 05:00");
        assert_eq!(ago("2026-10-02T22:10:00.000+02:00"), "ayer 22:10");
        assert_eq!(ago("2026-09-30T10:00:00.000+02:00"), "hace 3 días");
        assert_eq!(ago("2026-09-12T10:00:00.000+02:00"), "12 sep");
        assert_eq!(ago("2025-12-24T10:00:00.000+02:00"), "24 dic 2025");
        assert_eq!(ago("2026-10-04T09:00:00.000+02:00"), "mañana 09:00");
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(ago("2026-10-03T19:49:18.945+02:00"), "5 min ago");
        assert_eq!(ago("2026-10-03T05:00:00.861+02:00"), "today 05:00");
        assert_eq!(ago("2026-09-12T10:00:00.000+02:00"), "Sep 12");
    }

    #[test]
    fn duplicate_row_state_metadata_is_shown_once() {
        let row: Row = serde_json::from_value(serde_json::json!({
            "id": "task", "icon": "task", "title": "Ship", "depth": 0,
            "subtitle": "working", "badges": [{"text": "working", "tone": "info"}],
            "struck": false
        })).unwrap();
        let rendered: String = row_lines(&row, 80, false, false)
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(rendered.matches("working").count(), 1, "{rendered}");
    }

    #[test]
    fn a_row_keeps_its_state_beside_the_title_and_drops_empty_counters() {
        let row: Row = serde_json::from_value(serde_json::json!({
            "id": "node", "icon": "node", "title": "Ship", "depth": 0, "struck": false,
            "badges": [{"text": "Working", "tone": "info"}],
            "trailing": {"kind": "count", "value": 0}
        })).unwrap();
        let rendered = row_lines(&row, 30, false, false);
        assert_eq!(rendered.len(), 1);
        assert_eq!(rendered[0].to_string(), " ● Ship                Working");
        let narrow = row_lines(&row, 12, false, false);
        assert_eq!(
            narrow.iter().map(ToString::to_string).collect::<Vec<_>>(),
            [" ● Ship", "   Working"]
        );
    }

    #[test]
    fn actions_without_keyboard_bindings_do_not_render_empty_brackets() {
        let action = Action {
            id: "message".into(), label: "Message".into(), style: Style::Primary, role: None, option_id: None, description: None,
            presentation: Presentation::Button, options: vec![], input: Input::None,
            attachments: false, comments: None, confirm: None, enabled: true,
            disabled_reason: None, stamp: None, key: None, link: None,
        };
        assert_eq!(action_line(&action), "Message");
        let withdraw = Action {
            id: "withdraw:request-1@1".into(),
            label: "Cancel this request".into(),
            ..action
        };
        assert_eq!(action_hint(&withdraw), "x");
        assert_eq!(action_line(&withdraw), "[x] Cancel this request");
    }

    #[test]
    fn duplicate_attach_actions_with_legacy_empty_ids_are_shown_once() {
        let attach = serde_json::json!({
            "id":"", "label":"Attach to node", "style":"plain",
            "presentation":"button", "input":{"kind":"none"},
            "attachments":false, "enabled":true, "key":"a"
        });
        let view: View = serde_json::from_value(serde_json::json!({
            "protocol":1, "data_epoch":"test", "view":"card", "key":"card:pr",
            "client":"tui", "version":"v1", "cursor":"c",
            "status":{"version":"v1", "summary":"", "badge":0, "counts":[],
                "gauges":[], "orchestrator":{"text":"idle", "tone":"neutral"}},
            "sections":[{"id":"card", "collapsed":false, "items":[{
                "type":"card", "id":"pr", "icon":"pull_request", "title":"PR",
                "detail":"included", "actions":[attach.clone(), attach]
            }]}]
        })).unwrap();
        let actions = actions(&view);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].label, "Attach to node");
    }
}

#[cfg(test)]
mod action_tests {
    use super::*;
    use alondra_core::views::{Input, Presentation, Style};

    fn action(id: &str, label: &str) -> Action {
        Action {
            id: id.into(), label: label.into(), style: Style::Plain, role: None, option_id: None, description: None,
            presentation: Presentation::Button, options: vec![], input: Input::None,
            attachments: false, comments: None, confirm: None, enabled: true,
            disabled_reason: None, stamp: None, key: None, link: None,
        }
    }

    fn bar(actions: Vec<Action>) -> View {
        serde_json::from_value(serde_json::json!({
            "protocol":1, "data_epoch":"test", "view":"card", "key":"card:pr",
            "client":"tui", "version":"v1", "cursor":"c",
            "status":{"version":"v1", "summary":"", "badge":0, "counts":[],
                "gauges":[], "orchestrator":{"text":"idle", "tone":"neutral"}},
            "sections":[{"id":"card", "collapsed":false, "items":[{
                "type":"actions", "id":"bar", "actions": actions
            }]}]
        })).unwrap()
    }

    #[test]
    fn hints_follow_the_action_verb_whatever_the_label_says() {
        assert_eq!(action_hint(&action("merge:pr-1@1", "Fusionar")), "m");
        assert_eq!(action_hint(&action("conflicts:pr-1@1", "Resolver")), "1");
        assert_eq!(action_hint(&action("withdraw:request-1@1", "Retirar")), "x");
        assert_eq!(action_hint(&action("reply:i-1@1", "Withdraw the merge button")), "");
    }

    #[test]
    fn distinct_actions_sharing_a_label_are_all_shown() {
        let view = bar(vec![
            action("cancel:task-a@1", "Cancel"),
            action("cancel:task-b@1", "Cancel"),
            action("cancel:task-a@1", "Cancel"),
        ]);
        let ids: Vec<String> = actions(&view).into_iter().map(|action| action.id).collect();
        assert_eq!(ids, ["cancel:task-a@1", "cancel:task-b@1"]);
    }
}

/// What the detail pane and the reader call the open document: its card's kind, else its title.
pub fn document_kind(view: &View) -> Option<String> {
    view.sections
        .iter()
        .flat_map(|section| &section.items)
        .find_map(|item| match item {
            Item::Card(card) => card.eyebrow.as_ref().map(|eyebrow| eyebrow.text.to_lowercase()),
            _ => None,
        })
}

/// The terminal a document offers: an action or a row that attaches to its node's session.
pub fn attach_session(view: &View) -> Option<String> {
    use alondra_core::views::{Link, LocalLink};
    let session = |link: &Option<Link>| match link {
        Some(Link::Local {
            local: LocalLink::Attach { session },
        }) => Some(session.clone()),
        _ => None,
    };
    actions(view)
        .iter()
        .find_map(|action| session(&action.link))
        .or_else(|| {
            view.sections
                .iter()
                .flat_map(|section| &section.items)
                .find_map(|item| match item {
                    Item::Row(row) => session(&row.link),
                    _ => None,
                })
        })
}
