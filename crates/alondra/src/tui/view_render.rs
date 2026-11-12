//! Text rendering for daemon-built views. The component switch lives here so new view
//! components have one deliberate TUI representation.

use std::borrow::Cow;
use std::collections::BTreeSet;

use alondra_core::domain::CardKind;
use alondra_core::views::{
    Action, Card, CommentKind, Facts, FieldKind, Form, Icon, Input, Item, Label, Link, LocalLink, Notice, Presentation, Row,
    Section, Tone, Trailing, View,
};
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

/// Tasks folds its Completed section; the rest of the lists stay open as they were.
fn folds(view: &View, section: &Section) -> bool {
    view.key == "tasks" && section.collapsed
}

/// The list key of a section header the user opens and closes: one the daemon sends folded.
pub fn header_key(view: &View, section: &Section) -> Option<String> {
    (folds(view, section) && section.title.is_some() && (!section.items.is_empty() || section.more.is_some()))
        .then(|| section_key(&section.id))
}

/// The key of a section's header, which also names the section an action in that header empties.
pub fn section_key(id: &str) -> String {
    format!("{SECTION_HEADER}{id}")
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

/// The TUI lists each block of the Tasks list as a section of its own, its title over its tasks and, when the
/// block has one, the line that says what holds all of them.
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
        let mut children = block.children;
        for row in &mut children {
            scope_block_row(&block.id, row);
        }
        Section {
            id: block.id,
            title: Some(block.title),
            count,
            collapsed: false,
            items: block.subtitle.map(|text| Item::Notice(alondra_core::views::Notice {
                text, tone: alondra_core::views::Tone::Neutral, action: None,
            })).into_iter().chain(children.into_iter().map(Item::Row)).collect(),
            card_ids: vec![],
            card_waits: vec![],
            more: None, style: None, actions: vec![],
        }
    });
    sections.splice(at..at, blocks);
}

// A request can have a group in several blocks; its destination is not its list identity.
fn scope_block_row(block: &str, row: &mut Row) {
    row.id = serde_json::to_string(&(block, &row.id)).expect("row identity serializes");
    for child in &mut row.children {
        scope_block_row(block, child);
    }
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
                | Item::Actions(_)
                | Item::Unknown => {}
            }
        }
    }
    result
}

/// One list item: glyph and title, and on the right its state and since when, the way 8ccd131
/// laid rows out. A title wider than the list goes on as many lines as it takes; what does not fit
/// beside it goes on the line below; a subtitle (what a node runs now) goes under it.
pub fn row_lines(row: &Row, width: usize, selected: bool, nested: bool) -> Vec<Line<'static>> {
    row_lines_at(row, width, selected, nested, alondra_core::now_millis())
}

fn row_lines_at(row: &Row, width: usize, selected: bool, nested: bool, now: i64) -> Vec<Line<'static>> {
    let bar = Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT));
    let mut title_style = Style::default();
    if selected {
        title_style = title_style.add_modifier(Modifier::BOLD);
    }
    if row.struck {
        title_style = title_style.add_modifier(Modifier::CROSSED_OUT);
    }
    let indent = if nested { "  ".repeat(usize::from(row.depth.saturating_sub(1)).max(1)) } else { String::new() };
    let mut first = vec![
        bar.clone(),
        Span::styled(
            if nested { format!("{indent}↳ ") } else { String::new() },
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
        .map(|badge| (label_text_at(badge, now), label_style(badge)))
        .collect();
    match &row.trailing {
        Some(alondra_core::views::Trailing::Count { value }) if *value > 0 => {
            parts.push((value.to_string(), Style::new().fg(DIM)))
        }
        Some(alondra_core::views::Trailing::Count { .. } | alondra_core::views::Trailing::Unsupported) | None => {}
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
    let title = super::md::inline(&row.title, title_style);
    let title_width: usize = title.iter().map(Span::width).sum();
    let under = if nested { format!("{indent}    ") } else { "  ".to_owned() };
    let mut lines = Vec::new();
    if meta.is_empty() || lead_width + title_width + 2 + meta_width <= width {
        first.extend(title);
        if !meta.is_empty() {
            first.push(Span::raw(
                " ".repeat(width - lead_width - title_width - meta_width),
            ));
            first.extend(meta);
        }
        lines.push(fit(first, width));
    } else {
        let wrapped = vec![bar.clone(), Span::raw(" ".repeat(lead_width.saturating_sub(1)))];
        lines.extend(
            super::md::wrap(title, width, first, wrapped)
                .into_iter()
                .map(|line| fit(line.spans, width)),
        );
        let mut second = vec![bar.clone(), Span::raw(under.clone())];
        second.extend(meta);
        lines.push(fit(second, width));
    }
    if let Some(subtitle) = subtitle {
        let lead = vec![bar, Span::raw(under)];
        lines.extend(super::md::wrap(
            super::md::inline(subtitle, Style::new().fg(Color::Gray)), width, lead.clone(), lead,
        ).into_iter().map(|line| fit(line.spans, width)));
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

/// One list item per card: its type's glyph, what it needs over as many lines as it takes, and on
/// the right who sent it and how long ago, or below when that does not fit beside it. A card
/// another one holds back says by what, dim, on the line below.
pub fn card_row_lines(card: &Card, width: usize, selected: bool) -> Vec<Line<'static>> {
    let bar = Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT));
    let (glyph, color) = card_glyph(card);
    let lead = vec![
        bar.clone(),
        Span::styled(format!("{glyph} "), Style::new().fg(color).add_modifier(Modifier::BOLD)),
    ];
    let lead_width: usize = lead.iter().map(Span::width).sum();
    let budget = width.saturating_sub(lead_width);
    let rest = [card.meta.clone(), card_age(card)]
        .into_iter()
        .flatten()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let mut right = Vec::new();
    if let Some(state) = pr_state(card) {
        right.push(Span::styled(state.text.clone(), label_style(state)));
        if !rest.is_empty() {
            right.push(Span::styled(" · ", Style::new().fg(DIM)));
        }
    }
    if !rest.is_empty() {
        right.push(Span::styled(rest, Style::new().fg(DIM)));
    }
    let right_width: usize = right.iter().map(Span::width).sum();
    let right_beside = right_width <= budget / 3;
    let column = if right_beside && right_width > 0 { budget.saturating_sub(right_width + 2) } else { budget };
    let need = &card.title;
    let mut need_style = Style::default();
    if selected {
        need_style = need_style.add_modifier(Modifier::BOLD);
    }
    let under = vec![bar.clone(), Span::raw("  ")];
    let mut lines =
        super::md::wrap(super::md::inline(need, need_style), lead_width + column, lead, under.clone());
    if right_beside {
        let first = &mut lines[0];
        let padding = width.saturating_sub(first.width() + right_width);
        first.spans.push(Span::raw(" ".repeat(padding)));
        first.spans.extend(right);
    } else {
        let mut line = under;
        line.extend(right);
        lines.push(Line::from(line));
    }
    for chip in &card.blocked_by {
        lines.extend(super::md::wrap(
            vec![Span::styled(chip.label.clone(), Style::new().fg(DIM))],
            width,
            vec![bar.clone(), Span::raw("  ")],
            vec![bar.clone(), Span::raw("  ")],
        ));
    }
    lines.into_iter().map(|line| fit(line.spans, width)).collect()
}

/// A pull request's one state ("Checks failing", "Ready to merge"), in its tone.
fn pr_state(card: &Card) -> Option<&Label> {
    card.state.as_ref().filter(|_| card.icon == Icon::PullRequest)
}

/// The task or node a card came from, the way its header names it.
pub fn card_agent(card: &Card) -> Option<&alondra_core::views::ContextLine> {
    card.context.iter().find(|line| matches!(line.icon, Icon::Task | Icon::Node))
}

pub fn card_task_id(card: &Card) -> Option<alondra_core::domain::TaskId> {
    match card_agent(card)?.link.as_ref()? {
        alondra_core::views::Link::View { key } => key.strip_prefix("task:").map(|id| alondra_core::domain::TaskId(id.to_owned())),
        _ => None,
    }
}

/// The detail starts with its title; the pane frame carries the card's kind.
pub fn card_lines(card: &Card, width: usize) -> Vec<Line<'static>> {
    let mut out = super::md::wrap(super::md::inline(&card.title, Style::new().add_modifier(Modifier::BOLD)), width, vec![], vec![]);
    if let Some(need) = card.need.as_ref().filter(|_| card.title_is_separate) {
        out.extend(super::md::render(&super::md::parse(need), width).lines);
    }
    if let Some(why) = card.why.as_ref().or(card.subtitle.as_ref()) {
        out.extend(super::md::render(&super::md::parse(why), width).lines.into_iter().map(|line| grey(line, Color::Gray)));
    }
    let mut metadata = vec![];
    if let Some(state) = pr_state(card) {
        metadata.push(Span::styled(state.text.clone(), label_style(state)));
    }
    for part in [card.meta.clone(), card_age(card)].into_iter().flatten() {
        if !metadata.is_empty() { metadata.push(Span::styled(" · ", Style::new().fg(DIM))); }
        metadata.push(Span::styled(part, Style::new().fg(DIM)));
    }
    if !metadata.is_empty() {
        out.extend(super::md::wrap(metadata, width, vec![], vec![]));
    }
    for chip in card.blocked_by.iter().chain(&card.blocks) {
        out.push(Line::from(chip.label.clone().fg(ACCENT)));
    }
    for badge in &card.badges {
        out.push(Line::from(Span::styled(label_text_at(badge, alondra_core::now_millis()), label_style(badge))));
    }
    if let Some(body) = &card.body {
        out.extend(super::md::render(&super::md::parse(&body.text), width).lines);
    }
    if !card.attachments.is_empty() {
        out.push(Line::default());
        out.extend(card.attachments.iter().map(|attachment| Line::from(super::files::attachment_line(attachment))));
        out.push(Line::from(Span::styled(
            tr!("o opens the card to see and open them", "o abre la tarjeta para verlos y abrirlos"),
            Style::new().fg(DIM),
        )));
    }
    out
}

pub const BUTTONS: usize = 9;

/// The card's own field: the one place on it where the user writes.
pub fn card_reply(view: &View) -> Option<Action> {
    typed_card(view).and_then(|card| card.reply.clone()).map(Action::from)
}

/// What the keys of a card do: its buttons numbered from 1, the menu of the rest and the reply.
pub fn button_lines(card: &Card, width: usize) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for (index, action) in card.actions.iter().take(BUTTONS).enumerate() {
        lines.extend(button_line(&(index + 1).to_string(), action));
        if let Some(description) = &action.description {
            lines.extend(button_description(description, width));
        }
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
    if !action.enabled {
        let why = action.disabled_reason.as_ref().map(|reason| format!(" — {reason}")).unwrap_or_default();
        let mut spans = vec![keycap_span(key, DIM), Span::styled(" ", Style::new().fg(DIM))];
        spans.extend(super::md::inline(&action.label, Style::new().fg(DIM)));
        spans.push(Span::styled(why, Style::new().fg(DIM)));
        return vec![Line::from(spans)];
    }
    let mut spans = vec![keycap_span(key, role_color(action)), Span::raw(" ")];
    spans.extend(super::md::inline(&action.label, Style::new()));
    vec![Line::from(spans)]
}

/// A button's description under it, indented past its key and wrapped to the pane, in grey.
fn button_description(description: &str, width: usize) -> Vec<Line<'static>> {
    const INDENT: &str = "    ";
    description
        .lines()
        .filter(|line| !line.trim().is_empty())
        .flat_map(|line| super::md::render(&super::md::parse(line), width.saturating_sub(INDENT.len())).lines)
        .map(|line| {
            let mut spans = vec![Span::raw(INDENT)];
            spans.extend(grey(line, DIM).spans);
            Line::from(spans)
        })
        .collect()
}

/// Text Markdown left without a colour of its own, in `color`.
fn grey(line: Line<'static>, color: Color) -> Line<'static> {
    Line::from(
        line.spans
            .into_iter()
            .map(|span| match span.style.fg {
                Some(_) => span,
                None => span.fg(color),
            })
            .collect::<Vec<_>>(),
    )
}

/// A list the user picks from: the rest of a card's actions, or the options of one. 1 to 9 pick the
/// first nine; in a longer one ↑↓ move `sel` through all of them and ⏎ picks it.
#[derive(Clone, Debug, PartialEq)]
pub struct Menu {
    pub title: String,
    pub entries: Vec<(Action, Option<alondra_core::views::ActionOption>)>,
    pub sel: usize,
}

impl Menu {
    pub fn scrolls(&self) -> bool {
        self.entries.len() > 9
    }
}

/// The section of a card that lists its documents, each a link to its own screen.
pub const DOCUMENTS: &str = "documents";

/// The documents of the view on screen, to pick one and open it.
pub fn documents_menu(view: &View) -> Option<Menu> {
    let section = view.sections.iter().find(|section| section.id == DOCUMENTS)?;
    let entries: Vec<_> = section.items.iter().flat_map(|item| match item {
        Item::Facts(facts) => facts.items.as_slice(),
        _ => &[],
    }).filter(|fact| matches!(fact.link, Some(alondra_core::views::Link::View { .. }))).map(|fact| (Action {
        id: String::new(), label: fact.value.clone(), style: alondra_core::views::Style::Plain, role: None, option_id: None,
        description: Some(fact.label.clone()), presentation: alondra_core::views::Presentation::Button, options: vec![],
        input: alondra_core::views::Input::None, attachments: false, comments: None, confirm: None, enabled: true,
        disabled_reason: None, stamp: None, key: None, link: fact.link.clone(), resolves_card: false,
    }, None)).collect();
    (!entries.is_empty()).then(|| Menu { title: section.title.clone().unwrap_or_default(), entries, sel: 0 })
}

pub fn more_menu(card: &Card) -> Menu {
    Menu {
        title: tr!("More", "Más").into(),
        entries: card.more.iter().map(|action| (Action::from(action.clone()), None)).collect(),
        sel: 0,
    }
}

pub fn option_menu(action: &Action) -> Menu {
    Menu {
        title: super::md::inline_text(&action.label),
        entries: action.options.iter().map(|option| (action.clone(), Some(option.clone()))).collect(),
        sel: 0,
    }
}

/// The menu in at most `room` lines: its title, the entries around `sel` and how to leave.
pub fn menu_lines(menu: &Menu, room: usize) -> Vec<Line<'static>> {
    let entry_lines: Vec<Vec<Line<'static>>> = menu.entries.iter().enumerate().map(|(index, (action, option))| {
        let key = if index < 9 { (index + 1).to_string() } else { " ".into() };
        let mut entry = action.clone();
        if let Some(option) = option {
            entry.label = super::draw::option_label(option);
        }
        let mut lines = button_line(&key, &entry);
        if index == menu.sel && menu.scrolls() {
            for span in lines.iter_mut().flat_map(|line| line.spans.iter_mut()).skip(1) {
                span.style = span.style.add_modifier(Modifier::REVERSED);
            }
        }
        if let Some(description) = option.as_ref().and_then(|option| option.description.as_ref()) {
            let mut line = vec![Span::raw("    ")];
            line.extend(super::md::inline(description, Style::new().fg(DIM)));
            lines.push(Line::from(line));
        }
        lines
    }).collect();
    let room = room.saturating_sub(2).max(1);
    let sel = menu.sel.min(entry_lines.len().saturating_sub(1));
    let (mut start, mut end, mut used) = (sel, sel, 0);
    while end < entry_lines.len() && used + entry_lines[end].len() <= room {
        used += entry_lines[end].len();
        end += 1;
    }
    while start > 0 && used + entry_lines[start - 1].len() <= room {
        start -= 1;
        used += entry_lines[start].len();
    }
    let mut lines = vec![Line::from(Span::styled(
        menu.title.to_uppercase(),
        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
    ))];
    lines.extend(entry_lines[start..end.max(start + 1).min(entry_lines.len())].iter().flatten().cloned());
    let mut leave = vec![
        Span::styled(" Esc ", Style::new().fg(DIM)),
        Span::styled(tr!("back", "volver"), Style::new().fg(DIM)),
    ];
    if menu.scrolls() {
        let (at, of) = (sel + 1, menu.entries.len());
        let hint = match start > 0 || end < entry_lines.len() {
            true => trf!(" · ↑↓ {at} of {of} · ⏎ pick", " · ↑↓ {at} de {of} · ⏎ elige"),
            false => tr!(" · ↑↓ · ⏎ pick", " · ↑↓ · ⏎ elige").to_string(),
        };
        leave.push(Span::styled(hint, Style::new().fg(DIM)));
    }
    lines.push(Line::from(leave));
    lines
}

/// A diff says what it changes ("Changes · 4 files · +120 −30 · d view changes") and leaves its
/// lines to the reader: the card's count, or else `changes`, the count of the diff once loaded.
/// Another body says its size only when it has one.
fn content_ref_label(view: &View, content: &alondra_core::views::ContentRef, changes: &dyn Fn(&str, &str) -> Option<String>) -> String {
    let summary = detail_card(view)
        .and_then(|card| card.content_row.as_ref())
        .filter(|row| row.kind == "diff" && content.kind == alondra_core::views::ContentKind::Diff)
        .map(|row| row.summary.clone())
        .filter(|summary| !summary.is_empty())
        .or_else(|| {
            let head = content.digest.as_deref()?;
            changes(&content.fetch, head).filter(|_| content.kind == alondra_core::views::ContentKind::Diff)
        });
    let changes = tr!("Changes", "Cambios");
    let open = tr!("d view changes", "d ver cambios");
    match summary {
        Some(summary) => format!("{changes} · {summary} · {open}"),
        None if content.kind == alondra_core::views::ContentKind::Diff => format!("{changes} · {open}"),
        None if content.size == 0 => content.title.clone(),
        None => format!("{} · {} B", content.title, content.size),
    }
}

/// Whether the view folds its checks under a summary the user opens with `c`.
pub fn summarized_checks(view: &View) -> bool {
    view.sections.iter().flat_map(|section| &section.items)
        .any(|item| matches!(item, Item::Checks(checks) if checks.summary.is_some()))
}

/// Whether the view has sections the daemon sends folded, which the user opens with `z`.
pub fn folded_sections(view: &View) -> bool {
    view.sections.iter().any(|section| section.collapsed && section.title.is_some() && !section.items.is_empty())
}

/// The view as one document; `open_checks` shows the checks a summary folds and `open_folds` the
/// sections the daemon sends folded.
pub fn document_lines(view: &View, width: usize, open_checks: bool, open_folds: bool) -> Vec<Line<'static>> {
    document_lines_with(view, width, open_checks, open_folds, &|_, _| None)
}

/// `document_lines`, with `changes` counting a diff the view references by its route and head.
pub fn document_lines_with(
    view: &View,
    width: usize,
    open_checks: bool,
    open_folds: bool,
    changes: &dyn Fn(&str, &str) -> Option<String>,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    if let Some(title) = view.title.as_ref().filter(|_| view.view != "card" || typed_card(view).is_none()) {
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
                elapsed: None,
            }),
        )));
    }
    for section in view.sections.iter().filter(|section| !section.items.is_empty()) {
        let folds = section.collapsed && section.title.is_some();
        if let Some(title) = &section.title {
            if !lines.is_empty() {
                lines.push(Line::default());
            }
            let title = match section.count {
                Some(count) if folds => format!("{title} ({count})"),
                _ => title.clone(),
            };
            let mut line = vec![];
            if folds {
                line.push(Span::styled(if open_folds { "▾ " } else { "▸ " }, Style::new().fg(DIM)));
            }
            line.push(Span::styled(title, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
            lines.push(Line::from(line));
        }
        if folds && !open_folds {
            continue;
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
                    Span::raw(content_ref_label(view, content, changes)),
                ])),
                Item::Facts(facts) if section.id == DOCUMENTS => {
                    for fact in &facts.items {
                        lines.push(Line::from(vec![
                            Span::styled("▤ ", Style::new().fg(Color::Cyan)),
                            Span::raw(fact.value.clone()),
                            Span::styled(format!(" · {}", fact.label), Style::new().fg(DIM)),
                        ]));
                    }
                }
                Item::Facts(facts) => {
                    for fact in &facts.items {
                        let separator = if fact.label.ends_with('.') { " " } else { ": " };
                        let mut line = vec![Span::styled(format!("{}{separator}", fact.label), Style::new().fg(DIM))];
                        let style = fact.tone.map_or_else(Style::new, |tone| label_style(&Label { text: String::new(), tone, elapsed: None }));
                        line.extend(super::md::inline(&fact.value, style));
                        lines.push(Line::from(line));
                    }
                }
                Item::Checks(checks) => {
                    if let Some(summary) = &checks.summary {
                        lines.push(Line::from(vec![
                            Span::styled(if open_checks { "▾ " } else { "▸ " }, Style::new().fg(DIM)),
                            Span::styled(summary.text.clone(), label_style(summary)),
                        ]));
                        if !open_checks {
                            continue;
                        }
                    }
                    for check in &checks.items {
                        lines.push(Line::from(vec![
                            Span::styled(format!("{} ", check_state(check.state)), check_style(check.state)),
                            Span::raw(check.name.clone()),
                        ]));
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
                    let values = form.fields.iter().map(|field| (field.id.clone(), field.value.clone())).collect();
                    for field in form.fields.iter().filter(|field| super::work::field_visible(field, &values)) {
                        let value = match (&field.kind, &field.value) {
                            (FieldKind::Choice { items }, serde_json::Value::String(value)) => items.iter().find(|item| item.id == *value).map_or_else(|| value.clone(), |item| item.label.clone()),
                            (FieldKind::Toggle, serde_json::Value::Bool(on)) => if *on { tr!("On", "Activado") } else { tr!("Off", "Desactivado") }.into(),
                            (_, serde_json::Value::String(value)) => value.clone(),
                            (_, serde_json::Value::Null) => String::new(),
                            (_, value) => value.to_string(),
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!("{}: ", field.label), Style::new().fg(DIM)),
                            Span::raw(value),
                        ]));
                        if let Some(help) = &field.help {
                            lines.push(Line::from(format!("  {help}").fg(DIM)));
                        }
                    }
                    if !form.submit.enabled {
                        let why = form.submit.disabled_reason.as_ref().map(|reason| format!(" — {reason}")).unwrap_or_default();
                        lines.push(Line::from(Span::styled(format!("{}{why}", form.submit.label), Style::new().fg(DIM))));
                        continue;
                    }
                    lines.push(Line::from(vec![
                        Span::styled("[e] ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                        Span::raw({
                            let label = &form.submit.label;
                            trf!("Edit fields · {label}", "Edita los campos · {label}")
                        }),
                    ]));
                }
                Item::Notice(notice) => lines.push(Line::from(Span::styled(
                    notice.text.clone(),
                    label_style(&Label {
                        text: String::new(),
                        tone: notice.tone,
                        elapsed: None,
                    }),
                ))),
                Item::Actions(_) | Item::Unknown => {}
            }
        }
    }
    lines
}

/// What a newer daemon sent that this terminal does not know: an item kind becomes a row asking
/// to update, an action it cannot carry out stays disabled saying so, and an unknown link, form
/// field or trailing value goes.
pub fn placeholders(view: &mut View) {
    let mut n = 0;
    for item in view.sections.iter_mut().flat_map(|section| section.items.iter_mut()) {
        match item {
            Item::Unknown => {
                n += 1;
                *item = Item::Row(unknown_row(n));
            }
            Item::Card(card) => supported_card(card),
            Item::Row(row) => supported_row(row),
            Item::Markdown(markdown) => drop_unsupported(&mut markdown.more),
            Item::Facts(facts) => facts.items.iter_mut().for_each(|fact| drop_unsupported(&mut fact.link)),
            Item::Checks(checks) => drop_unsupported(&mut checks.link),
            Item::Form(form) => supported_form(form),
            Item::Notice(notice) => supported_notice(notice),
            Item::Actions(bar) => bar.actions.iter_mut().for_each(supported_action),
            Item::ContentRef(_) | Item::Timeline(_) => {}
        }
    }
    view.empty.iter_mut().chain(&mut view.notices).chain(&mut view.status.update).for_each(supported_notice);
    view.first_cards.iter_mut().for_each(placeholders);
}

fn unknown_row(n: usize) -> Row {
    Row {
        account: None, model: None,
        id: format!("unknown-{n}"),
        link: None,
        icon: Icon::Notice,
        title: tr!("Update the TUI to see this", "Actualiza la TUI para ver esto").to_owned(),
        subtitle: None,
        badges: vec![],
        trailing: None,
        depth: 0,
        struck: false,
        kind: None,
        children: vec![],
        folded: false,
        actions: vec![],
    }
}

pub(super) fn update_to_use() -> String {
    tr!("Update the TUI to use this", "Actualiza la TUI para usar esto").to_owned()
}

fn unsupported_link(link: &Link) -> bool {
    matches!(link, Link::Unsupported | Link::Local { local: LocalLink::Unsupported })
}

fn drop_unsupported(link: &mut Option<Link>) {
    if link.as_ref().is_some_and(unsupported_link) {
        *link = None;
    }
}

fn supported_action(action: &mut Action) {
    if action.comments.is_some_and(|comments| comments.kind == CommentKind::Unsupported) {
        action.comments = None;
    }
    let unsupported = action.link.as_ref().is_some_and(unsupported_link)
        || action.presentation == Presentation::Unsupported
        || std::iter::once(&action.input).chain(action.options.iter().map(|option| &option.input)).any(|input| *input == Input::Unsupported);
    if unsupported {
        disable(action);
        action.link = None;
        action.presentation = Presentation::Button;
        action.input = Input::None;
        action.options.retain(|option| option.input != Input::Unsupported);
    }
}

fn disable(action: &mut Action) {
    action.enabled = false;
    action.disabled_reason = Some(update_to_use());
}

fn supported_card(card: &mut Card) {
    drop_unsupported(&mut card.link);
    card.blocks.iter_mut().chain(&mut card.blocked_by).for_each(|chip| drop_unsupported(&mut chip.link));
    card.context.iter_mut().for_each(|line| drop_unsupported(&mut line.link));
    if let Some(body) = &mut card.body {
        drop_unsupported(&mut body.more);
    }
    card.actions.iter_mut().chain(&mut card.more).chain(&mut card.reply).for_each(|action| supported_action(action.on_client()));
}

fn supported_row(row: &mut Row) {
    drop_unsupported(&mut row.link);
    if row.trailing == Some(Trailing::Unsupported) {
        row.trailing = None;
    }
    row.actions.iter_mut().for_each(supported_action);
    row.children.iter_mut().for_each(supported_row);
}

fn supported_form(form: &mut Form) {
    let lost_required = form.fields.iter().any(|field| field.kind == FieldKind::Unsupported && field.required);
    form.fields.retain(|field| field.kind != FieldKind::Unsupported);
    supported_action(&mut form.submit);
    if lost_required {
        disable(&mut form.submit);
    }
}

fn supported_notice(notice: &mut Notice) {
    notice.action.iter_mut().for_each(supported_action);
}

pub(super) fn action_line(action: &Action) -> String {
    let hint = action_hint(action);
    if hint.is_empty() {
        action.label.clone()
    } else {
        format!("[{hint}] {}", action.label)
    }
}

/// A label that counts time reads the span up to `now`, or its frozen total once it has an end.
fn label_text_at(label: &Label, now: i64) -> String {
    let Some(elapsed) = &label.elapsed else {
        return label.text.clone();
    };
    let end = match &elapsed.until {
        Some(until) => parse_utc(until),
        None => Some(now),
    };
    match (parse_utc(&elapsed.since), end) {
        (Some(since), Some(end)) => alondra_core::views::text::short_duration(u64::try_from((end - since) / 1_000).unwrap_or(0)),
        _ => label.text.clone(),
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

pub(super) fn date_time_at_offset(millis: i64, utc_offset_seconds: i64) -> Option<(String, String)> {
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
/// A card view's card follows it even when its kind, from a newer daemon, is unknown here.
pub fn typed_card(view: &View) -> Option<&Card> {
    detail_card(view).filter(|card| card.kind.is_some() || view.view == "card")
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

/// How wide the labels and the values of a set of facts are, so several sets share one table.
#[derive(Clone, Copy, Debug, Default)]
pub struct FactColumns {
    pub label: usize,
    pub value: usize,
}

impl FactColumns {
    pub fn of<'a>(facts: impl IntoIterator<Item = &'a Facts>) -> Self {
        facts.into_iter().flat_map(|facts| &facts.items).fold(FactColumns::default(), |columns, fact| FactColumns {
            label: columns.label.max(super::md::str_width(&fact.label)),
            value: columns.value.max(super::md::str_width(fact_parts(&fact.value).0)),
        })
    }
}

/// A fact's value split into what it is now and what it is set beside: «A · before B (±x %)».
fn fact_parts(value: &str) -> (&str, Option<&str>) {
    match value.split_once(" · ") {
        Some((now, beside)) => (now, Some(beside)),
        None => (value, None),
    }
}

/// Facts as a table: each label in the label column, its value right-aligned in the value column,
/// bold and in its tone, and what it is set beside dimmed after it with its change in that tone.
/// On a line too narrow for all of it the value goes under its label.
pub fn facts_lines(facts: &Facts, width: usize, columns: FactColumns) -> Vec<Line<'static>> {
    let columns = FactColumns {
        label: columns.label.max(FactColumns::of([facts]).label),
        value: columns.value.max(FactColumns::of([facts]).value),
    };
    let mut lines = Vec::new();
    for fact in &facts.items {
        let tone = fact.tone.map_or_else(Style::new, tone_style);
        let (now, beside) = fact_parts(&fact.value);
        let mut value = vec![Span::styled(now.to_owned(), tone.add_modifier(Modifier::BOLD))];
        if let Some(beside) = beside {
            value.push(Span::styled("  ", Style::new()));
            match beside.rfind(" (").filter(|_| beside.ends_with(')')) {
                Some(at) => {
                    value.push(Span::styled(beside[..at].to_owned(), Style::new().fg(DIM)));
                    value.push(Span::styled(beside[at..].to_owned(), if fact.tone.is_some() { tone } else { Style::new().fg(DIM) }));
                }
                None => value.push(Span::styled(beside.to_owned(), Style::new().fg(DIM))),
            }
        }
        let label = Span::styled(fact.label.clone(), Style::new().fg(Color::Gray));
        let now_width = super::md::str_width(now);
        let value_width: usize = value.iter().map(Span::width).sum();
        if 2 + columns.label + 3 + columns.value + value_width - now_width <= width {
            let mut spans = vec![
                Span::raw("  "),
                label,
                Span::raw(" ".repeat(columns.label - super::md::str_width(&fact.label) + 3 + columns.value - now_width)),
            ];
            spans.extend(value);
            lines.push(Line::from(spans));
        } else {
            lines.push(fit(vec![Span::raw("  "), label], width));
            lines.extend(super::md::wrap(value, width.saturating_sub(4), vec![Span::raw("    ")], vec![Span::raw("    ")]));
        }
    }
    lines
}

/// A notice on its own line, in its tone.
pub fn notice_line(notice: &Notice) -> Line<'static> {
    Line::from(Span::styled(notice.text.clone(), tone_style(notice.tone)))
}

pub fn tone_style(tone: Tone) -> Style {
    label_style(&Label { text: String::new(), tone, elapsed: None })
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

fn check_style(state: alondra_core::views::CheckState) -> Style {
    Style::new().fg(match state {
        alondra_core::views::CheckState::Pass => Color::LightGreen,
        alondra_core::views::CheckState::Fail => Color::LightRed,
        alondra_core::views::CheckState::Pending => Color::Cyan,
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

    fn task_row(elapsed: serde_json::Value, fallback: &str) -> Row {
        serde_json::from_value(serde_json::json!({
            "id": "task", "icon": "task", "title": "Ship", "depth": 0, "struck": false,
            "badges": [{"text": "Working", "tone": "info"}, {"text": fallback, "tone": "neutral", "elapsed": elapsed}]
        })).unwrap()
    }

    fn rendered_at(row: &Row, now: &str) -> String {
        row_lines_at(row, 80, false, false, parse_utc(now).unwrap()).iter().map(ToString::to_string).collect()
    }

    #[test]
    fn a_running_task_counts_its_time_up_to_now_in_either_language() {
        for (lang, fallback) in [(crate::i18n::Lang::Es, "en marcha desde las 10:05"), (crate::i18n::Lang::En, "running since 10:05")] {
            crate::i18n::set_for_this_thread(lang);
            let row = task_row(serde_json::json!({"since": "2026-10-28T10:05:00.000+01:00"}), fallback);
            let early = rendered_at(&row, "2026-10-28T10:17:30.000+01:00");
            let later = rendered_at(&row, "2026-10-28T11:10:00.000+01:00");
            assert!(early.ends_with("Working · 12 min"), "{early}");
            assert!(later.ends_with("Working · 1 h 05 min"), "{later}");
            assert!(!early.contains(fallback), "{early}");
        }
    }

    #[test]
    fn a_finished_task_keeps_its_total_however_late_it_is_read() {
        for (lang, fallback) in [(crate::i18n::Lang::Es, "duró 1 h 05 min"), (crate::i18n::Lang::En, "took 1 h 05 min")] {
            crate::i18n::set_for_this_thread(lang);
            let row = task_row(
                serde_json::json!({"since": "2026-10-28T10:05:00.000+01:00", "until": "2026-10-28T10:10:00.000Z"}),
                fallback,
            );
            for now in ["2026-10-28T11:10:00.000+01:00", "2026-10-30T09:00:00.000+01:00"] {
                let rendered = rendered_at(&row, now);
                assert!(rendered.ends_with("Working · 1 h 05 min"), "{rendered}");
            }
        }
    }

    #[test]
    fn a_label_whose_times_cannot_be_read_keeps_its_text() {
        let label = Label {
            text: "running since 10:05".into(),
            tone: alondra_core::views::Tone::Neutral,
            elapsed: Some(alondra_core::views::Elapsed { since: "soon".into(), until: None }),
        };
        assert_eq!(label_text_at(&label, 0), "running since 10:05");
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
    fn the_task_list_counts_the_time_in_its_state_and_never_what_runs_now() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let view: View = serde_json::from_value(crate::tui::parity::view_fixture("tasks-tui.json")).unwrap();
        let (row, nested) = rows(&view).into_iter().find(|(row, _)| row.id == "t-badge-count").unwrap();
        let now = parse_utc("2026-10-02T09:20:00Z").unwrap();
        let lines = row_lines_at(row, 200, false, nested, now);
        let title = lines[0].to_string();
        assert!(title.contains("Count the retracted pushes") && title.contains("Working · Demo Garden · 2 d 0 h"), "{title}");
        assert!(!title.contains("running since"), "elapsed badges replace their legacy text");
        assert!(lines.iter().all(|line| !line.to_string().contains("Turn running")), "{lines:?}");
    }

    #[test]
    fn a_narrow_task_row_keeps_its_whole_reason() {
        let row: Row = serde_json::from_value(serde_json::json!({
            "id": "task", "icon": "task", "title": "Check the release", "depth": 0,
            "subtitle": "Waits for “Install the release” · since 15:38 · has your changes pending",
            "badges": [{"text": "Waiting", "tone": "neutral"}], "struck": false
        })).unwrap();
        let lines = row_lines_at(&row, 42, false, false, 0);
        let shown = lines.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ");
        for text in ["Install the release", "15:38", "changes pending"] {
            assert!(shown.contains(text), "{shown}");
        }
        assert!(lines.iter().all(|line| line.width() <= 42));
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
            disabled_reason: None, stamp: None, key: None, link: None, resolves_card: false,
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
            disabled_reason: None, stamp: None, key: None, link: None, resolves_card: false,
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
        assert_eq!(action_hint(&action("merge:pr-1@1", "Hacer merge")), "m");
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
    #[test]
    fn a_native_model_form_summary_uses_server_labels_and_visible_fields() {
        let view: View = serde_json::from_value(crate::tui::parity::view_fixture("model-profile-codex-tui.json")).unwrap();
        let text = document_lines(&view, 100, false, false).iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        assert!(text.contains("Fixture general"), "{text}");
        assert!(text.contains("Native default"), "{text}");
        assert!(!text.contains("model_profile"), "internal form IDs do not describe the screen: {text}");
        assert!(!text.contains(": auto"), "opaque option IDs have human labels: {text}");
        assert_eq!(text.matches("Reasoning effort:").count(), 1, "the empty next candidate has no visible reasoning field: {text}");
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
