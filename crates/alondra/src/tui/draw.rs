use std::collections::HashMap;

use alondra_core::now_millis;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block as Panel, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
    Wrap,
};

use super::md::{self, str_width, truncate, wrap};
use super::{App, FlashKind, Key, Pane, Screen, Target, View, unavailable};
use crate::i18n::{self, Occupancy};
use crate::runtime::FleetNode;
use alondra_core::NodeState;

const ACCENT: Color = Color::LightYellow;
const DIM: Color = Color::DarkGray;
const SOFT: Color = Color::Gray;
const SEL_BG: Color = Color::Indexed(236);
const COMMENT: Color = Color::LightMagenta;

pub fn draw(f: &mut Frame, app: &mut App) {
    let usage = app.view_status.as_ref().and_then(|s| s.usage_exhausted.clone());
    let [banner, head, body, foot] = Layout::vertical([
        Constraint::Length(u16::from(usage.is_some())),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(foot_height(app, f.area().width)),
    ])
    .areas(f.area());
    if let Some(usage) = &usage {
        let text = super::usage_banner::banner_text(usage, now_millis());
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {} ", truncate(&text, (banner.width as usize).saturating_sub(2))),
                Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD),
            ))),
            banner,
        );
    }
    draw_head(f, app, head);
    let body = match app.composer.as_ref() {
        Some(c) => {
            let preview = c
                .chips
                .iter()
                .rev()
                .find(|c| app.previews.contains_key(&c.seq))
                .map(|c| c.seq);
            let preview_h = if preview.is_some() { PREVIEW_ROWS } else { 0 };
            let h = (composer_height(&c.input, body.width)
                + chips_height(&c.chips)
                + form_height(c.form.as_ref())
                + preview_h)
                .min(body.height.saturating_sub(3));
            let [main, comp] =
                Layout::vertical([Constraint::Min(3), Constraint::Length(h)]).areas(body);
            let preview_area = draw_composer(f, app, comp, preview_h);
            if let (Some(seq), Some(area)) = (preview, preview_area)
                && let Some(state) = app.previews.get_mut(&seq)
            {
                f.render_stateful_widget(ratatui_image::StatefulImage::default(), area, state);
            }
            main
        }
        None => body,
    };
    if app.reader.is_some() {
        draw_reader(f, app, body);
    } else if app.rules.is_some() {
        draw_rules(f, app, body);
    } else if app.mcp_servers.is_some() {
        draw_mcp_servers(f, app, body);
    } else if let Some(view) = app.shown_home().map(std::borrow::Cow::into_owned) {
        draw_views_inbox(f, app, &view, body);
    } else {
        draw_waiting(f, app, body);
    }
    if let Some(form) = app.composer.as_ref().and_then(|c| c.form.as_ref()) {
        draw_form_popup(f, form, body);
    }
    draw_foot(f, app, foot);
    draw_flash(f, app, body);
    if let Some(modal) = &app.modal {
        draw_modal(f, modal, body);
    }
    if app.help {
        draw_help(f, app);
    }
    if let Some(device) = app.pairing_prompt().filter(|_| app.pairing_answers_keys()) {
        draw_pairing(f, device, body);
    }
}

fn panel(title: Line<'static>, focused: bool) -> Panel<'static> {
    Panel::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { DIM }))
        .title(title)
}

fn keycap(k: &str) -> Span<'static> {
    Span::styled(
        format!(" {k} "),
        Style::new()
            .fg(Color::Black)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

fn fit(spans: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let mut out = vec![];
    let mut w = 0;
    for s in spans {
        let sw = s.width();
        if w + sw <= width {
            w += sw;
            out.push(s);
        } else {
            let rest = width.saturating_sub(w);
            if rest > 1 {
                out.push(Span::styled(truncate(&s.content, rest), s.style));
            }
            break;
        }
    }
    Line::from(out)
}

fn highlight(f: &mut Frame, area: Rect, rows: std::ops::Range<usize>, scroll: usize) {
    for row in rows {
        if row >= scroll && row - scroll < area.height as usize {
            let r = Rect {
                x: area.x,
                y: area.y + (row - scroll) as u16,
                width: area.width,
                height: 1,
            };
            f.buffer_mut().set_style(r, Style::new().bg(SEL_BG));
        }
    }
}

fn scrollbar(f: &mut Frame, area: Rect, total: usize, pos: usize) {
    if total <= area.height as usize {
        return;
    }
    let mut state = ScrollbarState::new(total.saturating_sub(area.height as usize)).position(pos);
    f.render_stateful_widget(
        Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("┃")
            .thumb_style(Style::new().fg(DIM)),
        area,
        &mut state,
    );
}

/// Bottom-border hint that there is more to read below (or that the end is reached).
fn more_hint(scroll: usize, h: usize, total: usize) -> Option<Line<'static>> {
    if total <= h {
        return None;
    }
    let rest = total.saturating_sub(scroll + h);
    let text = if rest == 0 {
        tr!(" ↑ end ", " ↑ fin ").to_string()
    } else {
        trf!(" ↓ {rest} more lines ", " ↓ {rest} líneas más ")
    };
    Some(
        Line::from(Span::styled(
            text,
            Style::new().fg(if rest == 0 { DIM } else { ACCENT }),
        ))
        .right_aligned(),
    )
}

fn keep_visible(scroll: &mut usize, range: (usize, usize), height: usize, total: usize) {
    let (s, e) = range;
    if e.saturating_sub(s) >= height {
        *scroll = (*scroll).clamp(
            s.saturating_sub(1),
            e.saturating_sub(height).max(s.saturating_sub(1)),
        );
    } else if s < *scroll {
        *scroll = s.saturating_sub(1);
    } else if e > *scroll + height {
        *scroll = e.saturating_sub(height).min(s);
    }
    *scroll = (*scroll).min(total.saturating_sub(height));
}

fn centered_lines(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let w = area.width.max(1) as usize;
    let h = lines
        .iter()
        .map(|l| l.width().div_ceil(w).max(1))
        .sum::<usize>() as u16;
    let top = area.y + area.height.saturating_sub(h) / 2;
    let r = Rect {
        x: area.x,
        y: top,
        width: area.width,
        height: h.min(area.height),
    };
    f.render_widget(
        Paragraph::new(lines).centered().wrap(Wrap { trim: true }),
        r,
    );
}


pub(super) fn state_color(s: NodeState) -> Color {
    match s {
        NodeState::WaitingAnswer => Color::Yellow,
        NodeState::YourTurn => Color::LightYellow,
        NodeState::WaitingMerge => Color::LightGreen,
        NodeState::Unclear => Color::DarkGray,
        NodeState::Stalled => Color::LightRed,
        NodeState::Delivered => Color::Cyan,
        NodeState::WaitingMachine => Color::LightRed,
        NodeState::Working => Color::Green,
        NodeState::Done => DIM,
    }
}

/// The orchestrator keeps its node state's colour; every other session is busy, free or waiting.
pub(super) fn node_color(n: &FleetNode) -> Color {
    if n.is_orchestrator {
        return state_color(n.state);
    }
    match Occupancy::of(n) {
        Occupancy::Busy => Color::Green,
        Occupancy::Free => SOFT,
        Occupancy::WaitingMachine => Color::LightRed,
    }
}

/// The glyph, name and colour of an inbox item's kind, as the daemon names it.
pub fn kind_glyph(kind: &str) -> (&'static str, &'static str, Color) {
    match kind {
        "information" => ("•", tr!("information", "información"), Color::LightBlue),
        "question" => ("?", tr!("question", "pregunta"), ACCENT),
        "action_request" => ("▶", tr!("your turn", "te toca"), Color::LightYellow),
        "delivery" => ("✓", tr!("delivery", "entrega"), Color::LightGreen),
        "pull_request" => ("⇄", "PR", Color::LightGreen),
        "document_review" => ("▤", tr!("document", "documento"), Color::Cyan),
        _ => ("·", tr!("item", "elemento"), SOFT),
    }
}

fn draw_head(f: &mut Frame, app: &App, area: Rect) {
    let tab = |label: String, active: bool| {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::new()
                    .fg(Color::Black)
                    .bg(SOFT)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(SOFT))
        }
    };
    let title = Style::new()
        .fg(Color::Black)
        .bg(ACCENT)
        .add_modifier(Modifier::BOLD);
    let mut left = if app.macs.len() > 1 {
        app.macs
            .iter()
            .map(|mac| {
                let mut label = mac.name.clone();
                if let Some(status) = mac.status.label() {
                    label = format!("{label} · {status}");
                }
                if mac.attention > 0 {
                    label = format!("{label} {}", mac.attention);
                }
                Span::styled(
                    format!(" {label} "),
                    if mac.active {
                        title
                    } else {
                        Style::new().fg(SOFT)
                    },
                )
            })
            .collect()
    } else {
        vec![Span::styled(
            format!(" {} ", crate::service_client::target().unwrap_or("Alondra")),
            title,
        )]
    };
    left.push(Span::raw(" "));
    let count = |id: &str, fallback: usize| {
        app.view_status
            .as_ref()
            .and_then(|s| s.counts.iter().find(|c| c.id == id))
            .map_or(fallback, |c| c.value as usize)
    };
    let (inbox, tasks) = (app.attention(), count("tasks", 0));
    left.push(tab(
        trf!("Inbox {inbox}", "Bandeja {inbox}"),
        app.screen == Screen::Inbox,
    ));
    left.push(tab(
        trf!("Tasks {tasks}", "Tareas {tasks}"),
        app.screen == Screen::Tasks && !app.schedules_open,
    ));
    if app.screen == Screen::Tasks {
        left.push(tab(
            tr!("Scheduled [s]", "Programadas [s]").into(),
            app.schedules_open,
        ));
    }
    if app.offline {
        left.push(Span::raw(" "));
        left.push(Span::styled(
            if app.view_status.is_some() {
                tr!(
                    " saved data · connection lost ",
                    " datos guardados · sin conexión "
                )
            } else {
                tr!(" no connection to alondrad ", " sin conexión con alondrad ")
            },
            Style::new()
                .fg(Color::White)
                .bg(Color::Red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(update) = app.view_status.as_ref().and_then(|s| s.update.as_ref()) {
        left.push(Span::raw(" "));
        left.push(Span::styled(update.text.clone(), Style::new().fg(ACCENT)));
    }
    let daemon = app.daemon.as_ref();
    let reading = |pick: for<'a> fn(&'a crate::runtime::Runtime) -> bool| {
        app.runtime
            .as_ref()
            .filter(|r| pick(r))
            .or(daemon.filter(|r| pick(r)))
    };
    if let Some(notice) = reading(|r| r.update.is_some())
        .and_then(|r| r.update.as_ref())
        .and_then(update_notice)
    {
        left.push(Span::raw(" "));
        left.push(notice);
    }
    let lw: usize = left.iter().map(Span::width).sum();
    f.render_widget(Paragraph::new(Line::from(left)), area);
    if !app.loaded {
        return;
    }
    let Some(status) = &app.view_status else {
        return;
    };
    let working = status
        .counts
        .iter()
        .find(|count| count.id == "working")
        .map(|count| (count.value, count.label.clone()));
    let fits = |spans: &[Span]| {
        lw + spans.iter().map(Span::width).sum::<usize>() + 2 <= area.width as usize
    };
    let notice: Vec<Span<'static>> = app.updated.notice.iter().map(|n| Span::styled(
        if n.restarting { format!("↻ {}", n.text) } else { n.text.clone() },
        Style::new().fg(ACCENT),
    )).collect();
    let with_notice = |mut spans: Vec<Span<'static>>| {
        if !notice.is_empty() {
            spans.splice(0..0, notice.iter().cloned().chain([Span::raw("  ")]));
        }
        spans
    };
    let right = [
        with_notice(bars(&status.gauges, working.as_ref(), true)),
        with_notice(bars(&status.gauges, working.as_ref(), false)),
        notice.clone(),
    ]
    .into_iter()
    .find(|r| fits(r));
    if let Some(right) = right {
        f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    }
}

/// A newer release, or one that was installed but refused to start: one quiet mark in the header.
fn update_notice(update: &alondra_core::update::Status) -> Option<Span<'static>> {
    if let Some(rejected) = &update.rejected {
        let version = rejected.version.clone().unwrap_or_default();
        return Some(Span::styled(
            trf!(
                "{version} did not start · alondra doctor",
                "{version} no arrancó · alondra doctor"
            ),
            Style::new().fg(ACCENT),
        ));
    }
    let latest = update.latest.as_deref().filter(|_| update.available)?;
    Some(Span::styled(
        trf!("↑ {latest} · alondra update", "↑ {latest} · alondra update"),
        Style::new().fg(DIM),
    ))
}

/// The header bars, named when there is room (`Claude week ▰▰▱▱ 53%`) and bare when not.
fn bars(
    gauges: &[alondra_core::views::Gauge],
    working: Option<&(u32, String)>,
    named: bool,
) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    if let Some((value, label)) = working {
        out.push(Span::styled(format!("{value} {label}"), Style::new().fg(DIM)));
    }
    for gauge in gauges {
        let hot = gauge.level == alondra_core::views::Level::Hot;
        let color = match gauge.level {
            alondra_core::views::Level::Calm => ACCENT,
            alondra_core::views::Level::Warm => Color::Yellow,
            alondra_core::views::Level::Hot => Color::LightRed,
        };
        out.push(Span::raw(if named { "  " } else { " " }));
        if named {
            out.push(Span::styled(
                format!("{} ", gauge.label),
                Style::new().fg(SOFT),
            ));
        }
        let filled = ((gauge.value.clamp(0.0, 1.0) * BAR_CELLS as f64).round() as usize).max(1);
        out.push(Span::styled("▰".repeat(filled), Style::new().fg(color)));
        out.push(Span::styled(
            "▱".repeat(BAR_CELLS - filled),
            Style::new().fg(DIM),
        ));
        out.push(Span::styled(
            format!(" {}", gauge.text),
            if hot {
                Style::new()
                    .fg(Color::LightRed)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::new().fg(DIM)
            },
        ));
    }
    out.push(Span::raw(" "));
    out
}

const BAR_CELLS: usize = 4;

fn draw_views_inbox(f: &mut Frame, app: &mut App, home: &alondra_core::views::View, area: Rect) {
    let w = area.width;
    let lists = app.screen != Screen::Inbox && !app.schedules_open;
    app.detail_view.shown = w >= if lists { 100 } else { 84 };
    if app.detail_view.shown {
        let lw = match app.screen {
            _ if !lists && w >= 170 => 50,
            _ if !lists => (w * 2 / 5).clamp(36, 56),
            _ => (w * 3 / 5).clamp(56, 96),
        };
        let [list, detail] =
            Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        draw_views_inbox_list(f, app, home, list);
        draw_views_card(f, app, detail);
    } else {
        draw_views_inbox_list(f, app, home, area);
    }
}

fn list_title(app: &App) -> &'static str {
    match app.screen {
        Screen::Inbox => tr!(" Inbox ", " Bandeja "),
        Screen::Tasks if app.schedules_open => tr!(" Schedules ", " Programaciones "),
        Screen::Tasks => tr!(" Tasks ", " Tareas "),
    }
}

/// Before the first answer of alondrad: connecting, or not answering.
fn draw_waiting(f: &mut Frame, app: &App, area: Rect) {
    let block = panel(Line::from(list_title(app).bold()), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let message = if app.offline {
        tr!(
            "alondrad is not responding · retrying…",
            "alondrad no responde · reintentando…"
        )
    } else if app.loaded {
        tr!("Loading…", "Cargando…")
    } else {
        tr!("Connecting to alondrad…", "Conectando con alondrad…")
    };
    centered_lines(f, inner, vec![Line::from(message.fg(DIM))]);
}

fn empty_list(app: &App) -> Vec<Line<'static>> {
    let hint = |key: &'static str, text: &'static str| {
        Line::from(vec![
            Span::styled(key, Style::new().fg(ACCENT).bold()),
            Span::styled(text, Style::new().fg(DIM)),
        ])
    };
    match app.screen {
        Screen::Inbox => vec![
            Line::from(
                tr!("✓ All caught up", "✓ Todo al día")
                    .fg(Color::LightGreen)
                    .bold(),
            ),
            Line::from(
                tr!(
                    "Nobody is waiting for you and there is nothing to read.",
                    "Nadie te espera y no hay nada por leer."
                )
                .fg(DIM),
            ),
            Line::default(),
            hint("n", tr!(" order new work", " encarga trabajo nuevo")),
            hint("Tab", tr!(" see the tasks", " mira las tareas")),
        ],
        Screen::Tasks if app.schedules_open => vec![
            Line::from(tr!("No scheduled tasks", "No hay tareas programadas").fg(DIM).bold()),
            Line::default(),
            hint("n", tr!(" create one", " crea una")),
        ],
        Screen::Tasks => vec![
            Line::from(tr!("No tasks yet", "Aún no hay tareas").fg(DIM).bold()),
            Line::from(
                tr!(
                    "What you ask for shows up here at once, split into tasks.",
                    "Lo que pidas sale aquí al momento, repartido en tareas."
                )
                .fg(DIM),
            ),
            Line::default(),
            hint("n", tr!(" order new work", " encarga trabajo nuevo")),
        ],
    }
}

fn draw_views_inbox_list(
    f: &mut Frame,
    app: &mut App,
    home: &alondra_core::views::View,
    area: Rect,
) {
    let focused = app.focus == Pane::List || !app.detail_view.shown;
    let block = panel(Line::from(list_title(app).bold()), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let mut lines: Vec<Line> = Vec::new();
    let mut selected_range = None;
    for section in super::view_render::sections(home) {
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        let folded = super::view_render::folded(home, section, &app.open_sections);
        if let Some(title) = &section.title {
            let count = section.count.unwrap_or_else(|| section.items.len() as u32);
            let (text, color) = match section.id.as_str() {
                "pull_requests" => (title.to_uppercase(), Color::Magenta),
                "to_read" => (title.to_uppercase(), Color::Cyan),
                id if id.starts_with("project:") => (title.clone(), Color::Reset),
                _ => (title.to_uppercase(), ACCENT),
            };
            let header = super::view_render::header_key(home, section);
            let selected = header
                .as_ref()
                .is_some_and(|key| app.sel.as_ref().is_some_and(|sel| sel.0 == *key));
            let mut spans = Vec::new();
            if header.is_some() {
                spans.extend([
                    Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT)),
                    Span::styled(if folded { "▸" } else { "▾" }, Style::new().fg(DIM)),
                ]);
            }
            spans.extend([
                Span::styled(
                    format!(" {text} "),
                    Style::new().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(count.to_string(), Style::new().fg(DIM)),
            ]);
            if selected {
                selected_range = Some((lines.len(), lines.len() + 1));
            }
            lines.push(Line::from(spans));
        }
        if folded {
            continue;
        }
        for item in &section.items {
            if let alondra_core::views::Item::Card(card) = item {
                let selected = app.sel.as_ref().is_some_and(|key| key.0 == card.id);
                let start = lines.len();
                lines.extend(super::view_render::card_row_lines(
                    card,
                    inner.width.saturating_sub(1) as usize,
                    selected,
                ));
                if selected {
                    selected_range = Some((start, lines.len()));
                }
                continue;
            }
            let alondra_core::views::Item::Row(row) = item else {
                continue;
            };
            let selected = app.sel.as_ref().is_some_and(|key| key.0 == row.id);
            let start = lines.len();
            lines.extend(super::view_render::row_lines(
                row,
                inner.width.saturating_sub(1) as usize,
                selected,
                false,
            ));
            if selected {
                selected_range = Some((start, lines.len()));
            }
            for child in super::view_render::shown_rows(row, &app.open_rows) {
                let selected = app.sel.as_ref().is_some_and(|key| key.0 == child.id);
                let start = lines.len();
                lines.extend(super::view_render::row_lines(
                    &super::view_render::fold_marked(child, &app.open_rows),
                    inner.width.saturating_sub(1) as usize,
                    selected,
                    true,
                ));
                if selected {
                    selected_range = Some((start, lines.len()));
                }
            }
        }
    }
    if lines.is_empty() {
        centered_lines(f, inner, empty_list(app));
        return;
    }
    if let Some(range) = selected_range {
        keep_visible(
            &mut app.list_scroll,
            range,
            inner.height as usize,
            lines.len(),
        );
    }
    let total = lines.len();
    f.render_widget(
        Paragraph::new(lines).scroll((app.list_scroll as u16, 0)),
        inner,
    );
    if let Some((start, end)) = selected_range {
        highlight(
            f,
            Rect {
                width: inner.width.saturating_sub(1),
                ..inner
            },
            start..end,
            app.list_scroll,
        );
    }
    scrollbar(f, inner, total, app.list_scroll);
}

fn draw_views_card(f: &mut Frame, app: &mut App, area: Rect) {
    let selected = app.selected_view_key();
    let document = app
        .card_view
        .as_ref()
        .filter(|view| selected.as_deref() == Some(view.key.as_str()));
    let title = document
        .and_then(super::view_render::document_kind)
        .map_or_else(
            || tr!(" Detail ", " Detalle ").to_owned(),
            |kind| format!(" {kind} "),
        );
    let block = panel(Line::from(title.bold()), app.focus == Pane::Detail);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let Some(document) = document else {
        if let Some(menu) = &app.card_menu {
            f.render_widget(Paragraph::new(super::view_render::menu_lines(menu)), inner);
            return;
        }
        let message = if app.card_view_asked.is_some() {
            if selected
                .as_deref()
                .is_some_and(|key| key.starts_with("search"))
            {
                tr!("Searching…", "Buscando…")
            } else {
                tr!("Loading…", "Cargando…")
            }
        } else {
            tr!("Select an item", "Elige un elemento")
        };
        centered_lines(f, inner, vec![Line::from(message.fg(DIM))]);
        return;
    };
    let mut lines = super::view_render::document_lines(document, inner.width as usize);
    let mut keys = Vec::new();
    if let Some(card) = super::view_render::typed_card(document) {
        keys = match &app.card_menu {
            Some(menu) => super::view_render::menu_lines(menu),
            None => super::view_render::button_lines(card, inner.width as usize),
        };
    } else {
        let mut actions = action_menu(&app.bindings());
        actions.extend(super::view_render::actions(document).into_iter().filter(|action| !action.enabled).map(|action| {
            let why = action.disabled_reason.map(|reason| format!(" — {reason}")).unwrap_or_default();
            Line::from(Span::styled(format!("    {}{why}", action.label), Style::new().fg(DIM)))
        }));
        if let Some(menu) = &app.card_menu {
            keys = super::view_render::menu_lines(menu);
        } else if !actions.is_empty() {
            lines.push(Line::default());
            lines.extend(actions);
        }
    }
    let keys_height = (keys.len() as u16).min(inner.height.saturating_sub(4));
    let [body, keys_area] = Layout::vertical([
        Constraint::Min(1),
        Constraint::Length(if keys.is_empty() { 0 } else { keys_height + 1 }),
    ])
    .areas(inner);
    app.detail_view.total = lines.len();
    app.detail_view.h = body.height as usize;
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll as u16, 0)),
        body,
    );
    scrollbar(f, body, app.detail_view.total, app.detail_scroll);
    if !keys.is_empty() {
        let mut shown = vec![Line::default()];
        shown.extend(keys);
        f.render_widget(Paragraph::new(shown), keys_area);
    }
}

fn draw_rules(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(rules) = app.rules.as_mut() else { return };
    let title = rules
        .title()
        .unwrap_or(tr!("Permissions allowed always", "Permisos permitidos siempre"))
        .to_owned();
    let block = panel(Line::from(format!(" {title} ").bold()), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected = (0, 0);
    for (index, row) in rules.rows().into_iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        let start = lines.len();
        lines.extend(super::rules::row_lines(row, width, index == rules.sel));
        if index == rules.sel {
            selected = (start, lines.len());
        }
    }
    if lines.is_empty() {
        let message = match (&rules.view, &rules.failed) {
            (Some(_), _) => tr!("No command is allowed always yet", "Ningún comando se permite siempre todavía").to_owned(),
            (None, Some(error)) => error.clone(),
            (None, None) => tr!("Loading…", "Cargando…").to_owned(),
        };
        centered_lines(f, inner, vec![Line::from(message.fg(DIM))]);
        return;
    }
    let total = lines.len();
    keep_visible(&mut rules.scroll, selected, inner.height as usize, total);
    f.render_widget(Paragraph::new(lines).scroll((rules.scroll as u16, 0)), inner);
    highlight(f, Rect { width: inner.width.saturating_sub(1), ..inner }, selected.0..selected.1, rules.scroll);
    scrollbar(f, inner, total, rules.scroll);
}

fn draw_mcp_servers(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(screen) = app.mcp_servers.as_mut() else { return };
    let title = match screen.general.is_some() {
        true => tr!("Settings", "Ajustes").to_owned(),
        false => format!("{} › {}", tr!("Settings", "Ajustes"), screen.title().unwrap_or(tr!("MCP servers", "Servidores MCP"))),
    };
    let block = panel(Line::from(format!(" {title} ").bold()), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut selected = (0, 0);
    let mut index = 0;
    for entry in screen.entries() {
        match entry {
            super::mcp_servers::Entry::Group(title) => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.push(Line::from(title.to_uppercase().bold().fg(ACCENT)));
            }
            super::mcp_servers::Entry::Heading(title) => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.push(Line::from(title.to_owned().bold().fg(DIM)));
            }
            super::mcp_servers::Entry::Note(text) => lines.push(Line::from(text.to_owned().fg(DIM))),
            super::mcp_servers::Entry::Row(row) => {
                let start = lines.len();
                lines.extend(super::mcp_servers::row_lines(row, width, index == screen.sel));
                if index == screen.sel {
                    selected = (start, lines.len());
                }
                index += 1;
            }
        }
    }
    if lines.is_empty() {
        let message = match (&screen.view, &screen.failed) {
            (Some(view), _) => view.empty.as_ref().map_or_else(String::new, |notice| notice.text.clone()),
            (None, Some(error)) => error.clone(),
            (None, None) => tr!("Loading…", "Cargando…").to_owned(),
        };
        centered_lines(f, inner, vec![Line::from(message.fg(DIM))]);
        return;
    }
    let (inner, menu) = match &app.card_menu {
        Some(menu) => {
            let menu = super::view_render::menu_lines(menu);
            let height = (menu.len() as u16 + 1).min(inner.height / 2);
            let [list, keys] = Layout::vertical([Constraint::Min(1), Constraint::Length(height)]).areas(inner);
            (list, Some((menu, keys)))
        }
        None => (inner, None),
    };
    let total = lines.len();
    keep_visible(&mut screen.scroll, selected, inner.height as usize, total);
    f.render_widget(Paragraph::new(lines).scroll((screen.scroll as u16, 0)), inner);
    highlight(f, Rect { width: inner.width.saturating_sub(1), ..inner }, selected.0..selected.1, screen.scroll);
    scrollbar(f, inner, total, screen.scroll);
    if let Some((menu, area)) = menu {
        let mut text = vec![Line::default()];
        text.extend(menu);
        f.render_widget(Paragraph::new(text), area);
    }
}

/// What a row says first: a child's own text, a root's name.
const IMAGE_MAX_ROWS: usize = 20;
const PREVIEW_ROWS: u16 = 6;

fn image_rows(img: &image::DynamicImage, width: usize) -> usize {
    let rows =
        (width as f64 * img.height() as f64 / img.width().max(1) as f64 / 2.0).ceil() as usize;
    rows.clamp(4, IMAGE_MAX_ROWS)
}

fn draw_reader(f: &mut Frame, app: &mut App, area: Rect) {
    let still_there = app.reader.as_ref().is_some_and(|r| {
        app.card_view
            .as_ref()
            .is_some_and(|view| view.key == r.key.0)
    });
    let Some(r) = app.reader.as_mut() else { return };
    let review = app.reviews.get(&r.key);
    let (glyph, color) = (r.glyph, r.color);
    let who = if r.agent.is_empty() {
        " · ".to_owned()
    } else {
        format!(" · {} · ", r.agent)
    };
    let mut title = vec![
        Span::raw(" "),
        Span::styled(format!("{glyph} {}", r.kind), Style::new().fg(color).bold()),
        Span::styled(who, Style::new().fg(SOFT)),
        Span::raw(" "),
    ];
    let n_comments = review.map(|rv| rv.count()).unwrap_or(0);
    if !still_there && app.loaded && !app.review_sends.values().any(|key| *key == r.key) {
        let gone = match (n_comments > 0, r.task_id.is_some(), r.agent.is_empty()) {
            (true, true, false) => {
                let agent = &r.agent;
                trf!(
                    " no longer in the inbox · s: send your comments to {agent} ",
                    " ya no está en la bandeja · s: enviar tus comentarios a {agent} "
                )
            }
            (true, true, true) => tr!(
                " no longer in the inbox · s: send your comments to its node ",
                " ya no está en la bandeja · s: enviar tus comentarios a su nodo "
            )
            .to_owned(),
            _ => tr!(" no longer in the inbox ", " ya no está en la bandeja ").to_owned(),
        };
        title.push(Span::styled(gone, Style::new().fg(Color::Black).bg(Color::LightRed)));
    }
    let heading = md::one_line(&r.title);
    let around: usize = title.iter().map(Span::width).sum();
    if around + md::str_width(&heading) + 2 <= area.width as usize {
        title.insert(3, Span::styled(heading, Style::new().bold()));
    } else {
        let who = title[2].content.trim_end_matches(" · ").to_owned();
        title[2].content = who.into();
    }
    let status = if n_comments == 0 {
        tr!(" no comments ", " sin comentarios ").to_string()
    } else {
        let c = i18n::comments(n_comments);
        trf!(" {c} unsent ", " {c} sin enviar ")
    };
    let mut block = panel(Line::from(title), true).title_bottom(
        Line::from(Span::styled(
            status,
            Style::new().fg(if n_comments > 0 { COMMENT } else { DIM }),
        ))
        .right_aligned(),
    );
    let inner = block.clone().inner(area);
    let (doc, side) = if inner.width >= 110 {
        let [d, s] = Layout::horizontal([Constraint::Min(60), Constraint::Length(42)]).areas(inner);
        (d, Some(s))
    } else if n_comments > 0 && inner.height >= 16 {
        let [d, s] = Layout::vertical([Constraint::Min(8), Constraint::Length(8)]).areas(inner);
        (d, Some(s))
    } else {
        (inner, None)
    };
    let doc = match r.find.as_ref() {
        Some(find) => {
            let [bar, rest] =
                Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(doc);
            let hits = r.matches();
            let status = match (
                find.query().is_empty(),
                hits.iter().position(|&i| i == r.cursor),
            ) {
                (true, _) => String::new(),
                (false, _) if hits.is_empty() => tr!("no matches", "sin coincidencias").into(),
                (false, Some(i)) => {
                    let (i, n) = (i + 1, hits.len());
                    trf!(
                        "{i}/{n} · n next · N previous",
                        "{i}/{n} · n siguiente · N anterior"
                    )
                }
                (false, None) => {
                    let n = hits.len();
                    trf!("{n} matches · n next", "{n} coincidencias · n siguiente")
                }
            };
            let line = Line::from(vec![
                Span::styled(" / ", Style::new().fg(ACCENT).bold()),
                Span::raw(find.input.text()),
                Span::styled(format!("   {status}"), Style::new().fg(DIM)),
            ]);
            f.render_widget(Paragraph::new(line), bar);
            if find.editing && app.composer.is_none() && app.cmdline.is_none() {
                let (_, (_, col)) = find.input.layout(usize::MAX / 2);
                f.set_cursor_position(Position {
                    x: bar.x + 3 + col as u16,
                    y: bar.y,
                });
            }
            rest
        }
        None => doc,
    };
    let width = doc.width.saturating_sub(4) as usize;
    let rendered = md::render_linked(&r.blocks, width);
    let h = doc.height as usize;
    r.view_h = h;
    let total = rendered.lines.len();
    r.total = total;
    if let Some(&range) = rendered.ranges.get(r.cursor) {
        keep_visible(&mut r.scroll, range, h, total);
    }
    r.ranges = rendered.ranges;
    if let Some(hint) = more_hint(r.scroll, h, total) {
        block = block.title_bottom(hint.left_aligned());
    }
    f.render_widget(block, area);
    let numbers: HashMap<usize, usize> = review
        .map(|rv| {
            rv.comments
                .iter()
                .enumerate()
                .map(|(i, c)| (c.block, i + 1))
                .collect()
        })
        .unwrap_or_default();
    let mut owner = vec![None; total];
    for (b, &(s, e)) in r.ranges.iter().enumerate() {
        for slot in owner.iter_mut().take(e).skip(s) {
            *slot = Some(b);
        }
    }
    let visible: Vec<Line> = rendered
        .lines
        .into_iter()
        .enumerate()
        .skip(r.scroll)
        .take(h)
        .map(|(i, line)| {
            let b = owner[i];
            let first = b.is_some_and(|b| r.ranges[b].0 == i);
            let gutter = match b {
                Some(b) if b == r.cursor => Span::styled("▌ ", Style::new().fg(ACCENT)),
                Some(b) if numbers.contains_key(&b) && first => Span::styled(
                    format!("{:<2}", numbers[&b]),
                    Style::new().fg(COMMENT).bold(),
                ),
                Some(b) if numbers.contains_key(&b) => Span::styled("┃ ", Style::new().fg(COMMENT)),
                _ => Span::raw("  "),
            };
            let mut spans = vec![Span::raw(" "), gutter];
            spans.extend(line.spans);
            Line::from(spans)
        })
        .collect();
    f.render_widget(Paragraph::new(visible), doc);
    if let Some(&(s, e)) = r.ranges.get(r.cursor) {
        highlight(
            f,
            Rect {
                x: doc.x + 3,
                width: doc.width.saturating_sub(4),
                ..doc
            },
            s..e,
            r.scroll,
        );
    }
    md::hyperlinks(f.buffer_mut(), doc, &r.blocks, app.hyperlinks);
    if r.picking {
        if let Some(b) = r.blocks.get(r.cursor).filter(|b| b.targets.len() > 1) {
            draw_targets(f, doc, &b.targets);
        }
    }
    scrollbar(f, doc, total, r.scroll);
    if let Some(side) = side {
        let cursor = r.cursor;
        draw_comments(f, app, side, cursor);
    }
}

/// What the selected block opens, numbered, over the bottom of the document.
fn draw_targets(f: &mut Frame, doc: Rect, targets: &[md::Target]) {
    let width = doc.width.saturating_sub(4) as usize;
    let lines: Vec<Line> = targets
        .iter()
        .take(9)
        .enumerate()
        .map(|(i, t)| {
            let glyph = match t.kind {
                md::TargetKind::Link => "↗",
                md::TargetKind::Image => "📷",
                md::TargetKind::Stored => "📎",
            };
            let label = md::truncate(&format!("{glyph} {}", md::one_line(&t.label)), width / 2);
            let place = if t.kind == md::TargetKind::Stored || t.label == t.url {
                String::new()
            } else {
                format!("  {}", md::truncate(&t.url, width.saturating_sub(md::str_width(&label) + 6)))
            };
            Line::from(vec![
                Span::styled(format!(" {} ", i + 1), Style::new().fg(ACCENT).bold()),
                Span::raw(label),
                Span::styled(place, Style::new().fg(DIM)),
            ])
        })
        .collect();
    let height = (lines.len() as u16 + 2).min(doc.height);
    let area = Rect {
        y: doc.bottom().saturating_sub(height),
        height,
        ..doc
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).block(panel(Line::from(tr!(" Open ", " Abrir ")), true)),
        area,
    );
}

fn draw_comments(f: &mut Frame, app: &App, area: Rect, cursor: usize) {
    let Some(r) = app.reader.as_ref() else { return };
    let review = app.reviews.get(&r.key);
    let n = review.map(|rv| rv.count()).unwrap_or(0);
    let block = Panel::new()
        .borders(ratatui::widgets::Borders::LEFT)
        .border_style(Style::new().fg(DIM))
        .title(Line::from(vec![
            Span::styled(tr!(" Comments ", " Comentarios "), Style::new().bold()),
            Span::styled(n.to_string(), Style::new().fg(DIM)),
        ]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let width = text.width as usize;
    let mut lines: Vec<Line> = vec![];
    let key = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("{k:<6} "), Style::new().fg(ACCENT).bold()),
            Span::styled(d.to_string(), Style::new().fg(DIM)),
        ])
    };
    match review.filter(|review| !review.is_empty()) {
        None => {
            lines.push(Line::from(tr!("None yet.", "Todavía ninguno.").fg(DIM)));
            lines.push(Line::default());
            for (hint, label) in bar_keys(&app.bindings()) {
                lines.push(key(&hint, &label));
            }
        }
        Some(rv) => {
            for (i, c) in rv.comments.iter().enumerate() {
                let num_style = if c.block == cursor {
                    Style::new().fg(Color::Black).bg(ACCENT).bold()
                } else {
                    Style::new().fg(COMMENT).bold()
                };
                let quote = wrap(
                    vec![Span::styled(
                        format!("«{}»", md::clip(&c.quote, 90)),
                        Style::new().fg(DIM).italic(),
                    )],
                    width,
                    vec![
                        Span::styled(format!("{:>2}.", i + 1), num_style),
                        Span::raw(" "),
                    ],
                    vec![Span::raw("    ")],
                );
                lines.extend(quote);
                lines.extend(wrap(
                    vec![Span::raw(c.text.replace('\n', " "))],
                    width,
                    vec![Span::raw("    ")],
                    vec![Span::raw("    ")],
                ));
                lines.push(Line::default());
            }
            if !rv.general.trim().is_empty() {
                lines.push(Line::from("General".fg(COMMENT).bold()));
                lines.extend(wrap(
                    vec![Span::raw(rv.general.replace('\n', " "))],
                    width,
                    vec![Span::raw("    ")],
                    vec![Span::raw("    ")],
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), text);
}

fn composer_height(input: &super::input::TextInput, width: u16) -> u16 {
    let (lines, _) = input.layout(width.saturating_sub(4) as usize);
    lines.len().clamp(1, 8) as u16 + 2
}

fn chips_height(chips: &[super::dropped::Chip]) -> u16 {
    u16::from(!chips.is_empty())
}

fn form_height(form: Option<&super::work::OptionsForm>) -> u16 {
    form.map_or(0, |f| {
        if f.loaded {
            f.rows_shown().len() as u16
        } else {
            1
        }
    })
}

fn form_lines(form: &super::work::OptionsForm) -> Vec<Line<'static>> {
    if !form.loaded {
        return vec![Line::from(
            tr!("Loading the form…", "Cargando el formulario…").fg(DIM),
        )];
    }
    let width = form
        .rows_shown()
        .iter()
        .map(|(l, _, _)| l.chars().count())
        .max()
        .unwrap_or(0);
    let mut lines: Vec<Line<'static>> = form
        .rows_shown()
        .into_iter()
        .map(|(label, value, focused)| {
            let (marker, style) = if focused {
                ("▸ ", Style::new().fg(Color::Black).bg(ACCENT))
            } else {
                ("  ", Style::new())
            };
            Line::from(vec![
                Span::styled(marker, Style::new().fg(ACCENT)),
                Span::styled(format!("{label:<width$}  "), Style::new().fg(DIM)),
                Span::styled(value, style),
            ])
        })
        .collect();
    if let (Some(e), Some(first)) = (&form.error, lines.first_mut()) {
        first.spans.push(Span::styled(
            format!("   ⚠ {e}"),
            Style::new().fg(Color::Red),
        ));
    }
    lines
}

/// The list a row of "New" opens, drawn over the screen above the composer.
fn draw_form_popup(f: &mut Frame, form: &super::work::OptionsForm, area: Rect) {
    use super::work::Popup;
    let Some(popup) = form.popup.as_ref() else {
        return;
    };
    let title = form.popup_title();
    let selected = |on: bool| {
        if on {
            Style::new().fg(Color::Black).bg(ACCENT)
        } else {
            Style::new()
        }
    };
    let (hint, lines): (&str, Vec<Line<'static>>) = match popup {
        Popup::Issues { picker: p, .. } => {
            let mut lines = vec![Line::from(vec![
                Span::styled("/ ", Style::new().fg(DIM)),
                Span::raw(p.query.clone()),
            ])];
            if p.loading {
                lines.push(Line::from(
                    tr!("Asking GitHub…", "Preguntando a GitHub…").fg(DIM),
                ));
            }
            if let Some(e) = &p.error {
                lines.push(Line::from(Span::styled(
                    format!("⚠ {e}"),
                    Style::new().fg(Color::Red),
                )));
            }
            let visible = p.visible();
            if visible.is_empty() && !p.loading {
                lines.push(Line::from(match p.typed_reference() {
                    Some(reference) => trf!("⏎ uses {reference}", "⏎ usa {reference}").fg(DIM),
                    None => tr!("No open issues match", "Ninguna issue abierta coincide").fg(DIM),
                }));
            }
            let room = (area.height as usize)
                .saturating_sub(lines.len() + 2)
                .max(1);
            let start = p.sel.saturating_sub(room - 1);
            for (i, issue) in visible.iter().enumerate().skip(start).take(room) {
                let labels = if issue.labels.is_empty() {
                    String::new()
                } else {
                    format!("  [{}]", issue.labels.join(", "))
                };
                let text = truncate(
                    &format!("{}  {}{labels}", issue.reference(), issue.title),
                    area.width.saturating_sub(4) as usize,
                );
                lines.push(Line::from(Span::styled(text, selected(i == p.sel))));
            }
            (
                tr!(
                    " type to filter · ↑↓ · ⏎ picks · Ctrl+R searches GitHub again · Esc ",
                    " escribe para filtrar · ↑↓ · ⏎ elige · Ctrl+R busca de nuevo en GitHub · Esc "
                ),
                lines,
            )
        }
        Popup::Filter { field, query, sel } => {
            let mut lines = vec![Line::from(vec![
                Span::styled("/ ", Style::new().fg(DIM)),
                Span::raw(query.clone()),
            ])];
            let choices = form.filtered(*field, query);
            if choices.is_empty() {
                lines.push(Line::from(tr!("Nothing matches", "Nada coincide").fg(DIM)));
            }
            let sel = (*sel).min(choices.len().saturating_sub(1));
            let room = (area.height as usize)
                .saturating_sub(lines.len() + 2)
                .max(1);
            let start = sel.saturating_sub(room - 1);
            for (i, choice) in choices.iter().enumerate().skip(start).take(room) {
                lines.push(Line::from(Span::styled(
                    choice.label.clone(),
                    selected(i == sel),
                )));
            }
            (
                tr!(
                    " type to filter · ↑↓ · ⏎ picks · Esc ",
                    " escribe para filtrar · ↑↓ · ⏎ elige · Esc "
                ),
                lines,
            )
        }
        Popup::Values { field, sel } => {
            let lines = form
                .choices(*field)
                .iter()
                .enumerate()
                .map(|(i, item)| {
                    let style = if i == *sel {
                        selected(true)
                    } else {
                        Style::new().bold()
                    };
                    Line::from(Span::styled(format!(" {} ", item.label), style))
                })
                .collect();
            (tr!(" ↑↓ · ⏎ picks · Esc ", " ↑↓ · ⏎ elige · Esc "), lines)
        }
        Popup::Text { input, .. } => (
            tr!(" ⏎ saves · Esc ", " ⏎ guarda · Esc "),
            vec![Line::from(vec![
                Span::raw(input.clone()),
                Span::styled("▏", Style::new().fg(ACCENT)),
            ])],
        ),
    };
    let height = (lines.len() as u16 + 2).min(area.height);
    let area = Rect {
        y: area.y + area.height - height,
        height,
        ..area
    };
    let block = panel(
        Line::from(Span::styled(
            format!(" {title} "),
            Style::new().fg(ACCENT).bold(),
        )),
        true,
    )
    .title_bottom(Line::from(hint.fg(DIM)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}

fn chip_line(chips: &[super::dropped::Chip]) -> Line<'static> {
    let mut spans = vec![];
    for chip in chips {
        let icon = if chip.image() { "📷" } else { "📄" };
        let size = super::files::size(chip.bytes as usize);
        let (label, style) = match chip.upload {
            super::dropped::Upload::Sending => (
                format!(
                    " {icon} {} · {size} · {} ",
                    chip.name,
                    tr!("copying…", "copiando…")
                ),
                Style::new().fg(DIM),
            ),
            super::dropped::Upload::Stored(_) => (
                format!(" {icon} {} · {size} ", chip.name),
                Style::new().fg(Color::Black).bg(SOFT),
            ),
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

fn draw_composer(f: &mut Frame, app: &App, area: Rect, preview_h: u16) -> Option<Rect> {
    let c = app.composer.as_ref()?;
    let mut title = match &c.target {
        Target::ViewAction { label, .. } => md::one_line(label),
        Target::Message {
            name,
            title,
            message,
            ..
        } => {
            let t = md::one_line(title);
            match message.kind {
                crate::ops::MessageKind::MoreContext => trf!(
                    "{t} · what do you need to know? (optional)",
                    "{t} · ¿qué te falta saber? (opcional)"
                ),
                crate::ops::MessageKind::PrChanges => {
                    trf!("{t} · what should change?", "{t} · ¿qué hay que cambiar?")
                }
                crate::ops::MessageKind::PrQuestion => trf!(
                    "{t} · what do you want to ask?",
                    "{t} · ¿qué quieres preguntar?"
                ),
                crate::ops::MessageKind::Reply if message.mark_read => trf!(
                    "Reply to {name} (marks it read)",
                    "Responder a {name} (se marca leído)"
                ),
                crate::ops::MessageKind::Reply => trf!(
                    "Reply to {name}: the question stays open",
                    "Responder a {name}: la pregunta sigue abierta"
                ),
                crate::ops::MessageKind::Message => trf!("Message to {name}", "Mensaje a {name}"),
            }
        }
        Target::Form { view } if view.starts_with("new_request") => tr!(
            "New work: Alondra turns it into tasks",
            "Trabajo nuevo: Alondra lo convierte en tareas"
        )
        .into(),
        Target::Form { .. } => c
            .form
            .as_ref()
            .and_then(|form| form.submit.as_ref())
            .map_or_else(String::new, |submit| md::one_line(&submit.label)),
        Target::Comment { quote, .. } => {
            let q = md::one_line(quote);
            trf!("Comment on “{q}”", "Comentario sobre «{q}»")
        }
        Target::General { send: true } => tr!(
            "Your comments (⏎ sends them as feedback)",
            "Tus comentarios (⏎ los envía como feedback)"
        )
        .into(),
        Target::General { send: false } => tr!("General comment", "Comentario general").into(),
        Target::McpReturn { server, .. } => trf!(
            "Sign in to {server} in the browser, then paste the address of the page that did not load",
            "Inicia sesión en {server} en el navegador y pega la dirección de la página que no carga"
        ),
    };
    let action = composer_action(&c.target);
    let title = truncate(&title, (area.width as usize).saturating_sub(6));
    let hint = trf!(
        " ⏎ {action} · Alt+⏎ new line · Esc cancels ",
        " ⏎ {action} · Alt+⏎ nueva línea · Esc cancela "
    );
    let block = panel(
        Line::from(Span::styled(
            format!(" {title} "),
            Style::new().fg(ACCENT).bold(),
        )),
        true,
    )
    .title_bottom(Line::from(hint.fg(DIM)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let form_h = form_height(c.form.as_ref()).min(text.height.saturating_sub(1));
    let chips_h = chips_height(&c.chips).min(text.height.saturating_sub(form_h + 1));
    let preview_h = preview_h.min(text.height.saturating_sub(form_h + chips_h + 1));
    let [preview, chips, form, text] = Layout::vertical([
        Constraint::Length(preview_h),
        Constraint::Length(chips_h),
        Constraint::Length(form_h),
        Constraint::Min(1),
    ])
    .areas(text);
    if chips_h > 0 {
        f.render_widget(Paragraph::new(chip_line(&c.chips)), chips);
    }
    if let Some(o) = c.form.as_ref().filter(|_| form_h > 0) {
        f.render_widget(Paragraph::new(form_lines(o)), form);
    }
    let (lines, (row, col)) = c.input.layout(text.width as usize);
    let scroll = row.saturating_sub(text.height.saturating_sub(1) as usize);
    if c.input.text().is_empty() {
        f.render_widget(
            Paragraph::new(Line::from(tr!("Type here…", "Escribe aquí…").fg(DIM))),
            text,
        );
    } else {
        f.render_widget(
            Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>())
                .scroll((scroll as u16, 0)),
            text,
        );
    }
    f.set_cursor_position(Position {
        x: text.x + col as u16,
        y: text.y + (row - scroll) as u16,
    });
    (preview_h > 0).then_some(preview)
}

fn composer_action(t: &Target) -> &'static str {
    match t {
        Target::Message { message, .. } if message.kind == crate::ops::MessageKind::MoreContext => {
            tr!("sends it back", "la devuelve")
        }
        Target::Comment { .. } | Target::General { send: false } => tr!("saves", "guarda"),
        _ => tr!("sends", "envía"),
    }
}

/// What a document lets you do, one line each and all alike, as 8ccd131 drew a card's keys: each
/// action and option with the key the table gives it. Only what can be done is listed.
pub(super) fn action_menu(table: &super::bindings::Table) -> Vec<Line<'static>> {
    table
        .0
        .iter()
        .filter(|binding| matches!(binding.act, super::bindings::Act::Server(..) | super::bindings::Act::Form))
        .map(|binding| Line::from(vec![keycap(&binding.hint), Span::raw(format!(" {}", binding.label))]))
        .collect()
}

/// The actions a list screen offers whatever is selected, such as archiving every finished task.
pub(super) fn screen_actions(view: &alondra_core::views::View) -> Vec<alondra_core::views::Action> {
    view.sections
        .iter()
        .flat_map(|section| &section.items)
        .filter_map(|item| match item {
            alondra_core::views::Item::Actions(bar) => Some(bar.actions.iter()),
            _ => None,
        })
        .flatten()
        .filter(|action| action.enabled && !super::view_render::action_hint(action).is_empty())
        .cloned()
        .collect()
}

pub(super) fn option_label(option: &alondra_core::views::ActionOption) -> String {
    option.label.clone()
}

/// The key bar: the composer's keys while it is open, otherwise the table's bar entries.
pub(super) fn footer_keys(app: &App) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if app.help {
        return vec![
            k("j/k", tr!("scroll", "desplazar")),
            k(tr!("any other key", "otra tecla"), tr!("closes", "cierra")),
        ];
    }
    if let Some(c) = app.composer.as_ref().filter(|_| app.modal.is_none()) {
        return composer_keys(c);
    }
    if app.reader.as_ref().is_some_and(|reader| reader.find.as_ref().is_some_and(|find| find.editing)) {
        return vec![k("⏎", tr!("find", "buscar")), k("Esc", tr!("cancel", "cancela"))];
    }
    if app.screen == Screen::Tasks && app.reader.is_none() && !app.schedules_open
        && app.search.as_ref().is_some_and(|search| search.editing)
    {
        return vec![k("⏎/↑/↓", tr!("go to the results", "ir a los resultados")), k("Esc", tr!("cancel", "cancela"))];
    }
    bar_keys(&app.bindings())
}

/// A table's bar entries as the key bar names them.
pub(super) fn bar_keys(table: &super::bindings::Table) -> Vec<(String, String)> {
    table
        .0
        .iter()
        .filter(|binding| binding.shown == super::bindings::Shown::Bar)
        .map(|binding| (binding.hint.clone(), binding.label.clone()))
        .collect()
}

fn composer_keys(c: &super::Composer) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if let Some(form) = c.form.as_ref() {
        if form.popup.is_some() {
            return vec![
                k("↑/↓", tr!("move", "mover")),
                k("⏎", tr!("picks", "elige")),
                k("Esc", tr!("closes the list", "cierra la lista")),
            ];
        }
        if form.focus.is_some() {
            return vec![
                k("↑/↓", tr!("setting", "ajuste")),
                k("⏎", tr!("shows its values", "muestra sus valores")),
                k("p", tr!("project", "proyecto")),
                k("i", tr!("issue", "issue")),
                k("x", tr!("drop the issue", "quita la issue")),
                k("Tab", tr!("back to the text", "vuelve al texto")),
            ];
        }
        return vec![
            k("⏎", composer_action(&c.target)),
            k(
                "Tab",
                tr!("project, issue and options", "proyecto, issue y opciones"),
            ),
            k("Alt+⏎", tr!("new line", "nueva línea")),
            k(
                tr!("drop a file", "arrastra un archivo"),
                tr!("attaches it", "lo adjunta"),
            ),
            k("Esc", tr!("cancel", "cancela")),
        ];
    }
    vec![
        k("⏎", composer_action(&c.target)),
        k("Alt+⏎", tr!("new line", "nueva línea")),
        k("Ctrl+W", tr!("delete word", "borra palabra")),
        k("Ctrl+U", tr!("delete line", "borra línea")),
        if c.chips.is_empty() {
            k(
                tr!("drop a file", "arrastra un archivo"),
                tr!("attaches it", "lo adjunta"),
            )
        } else {
            k(
                tr!("⌫ on empty", "⌫ en vacío"),
                tr!("removes the last attachment", "quita el último adjunto"),
            )
        },
        k("Esc", tr!("cancel", "cancela")),
    ]
}

/// Action results float above the key bar, wrapped, so they never hide the keys or get cut.
fn draw_pairing(f: &mut Frame, device: &super::PendingDevice, body: Rect) {
    let (name, code) = (&device.name, &device.code);
    let text = trf!(
        "Allow “{name}” to use Alondra? Code {code} — [p] Allow · [d] Deny",
        "¿Permitir que «{name}» use Alondra? Código {code} — [p] Permitir · [d] Denegar"
    );
    let width = body.width.saturating_sub(4).clamp(10, 90);
    let inner_width = width.saturating_sub(4).max(1);
    let height = (text.chars().count() as u16).div_ceil(inner_width).clamp(1, 6) + 2;
    let area = Rect {
        x: body.x + body.width.saturating_sub(width) / 2,
        y: body.y + body.height.saturating_sub(height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(Text::styled(text, Style::new().fg(ACCENT).bold()))
            .wrap(Wrap { trim: true })
            .block(
                Panel::bordered()
                    .border_type(BorderType::Thick)
                    .border_style(Style::new().fg(ACCENT)),
            ),
        area,
    );
}

fn draw_flash(f: &mut Frame, app: &App, body: Rect) {
    let Some(fl) = &app.flash else { return };
    let (mark, color) = match fl.kind {
        FlashKind::Error => ("✗ ", Color::LightRed),
        FlashKind::Success => ("✓ ", Color::LightGreen),
        FlashKind::Pending => ("… ", ACCENT),
    };
    let width = body.width.saturating_sub(4).clamp(10, 100);
    let inner_width = width.saturating_sub(4).max(1);
    let text = format!("{mark}{}", fl.text);
    let lines = text
        .lines()
        .map(|l| (l.chars().count() as u16).div_ceil(inner_width).max(1))
        .sum::<u16>()
        .clamp(1, 10);
    let height = lines + 2;
    let area = Rect {
        x: body.x + (body.width.saturating_sub(width)) / 2,
        y: body.bottom().saturating_sub(height),
        width,
        height,
    };
    f.render_widget(Clear, area);
    let panel = Panel::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(color));
    f.render_widget(
        Paragraph::new(Text::styled(text, Style::new().fg(color).bold()))
            .wrap(Wrap { trim: true })
            .block(panel),
        area,
    );
}

fn draw_modal(f: &mut Frame, modal: &super::Modal, body: Rect) {
    let color = if modal.destructive {
        Color::LightRed
    } else {
        ACCENT
    };
    let width = body.width.saturating_sub(8).clamp(20, 70);
    let inner_width = width.saturating_sub(4).max(1);
    let button = &modal.button;
    let keys = trf!("⏎ {button} · Esc cancel", "⏎ {button} · Esc cancelar");
    let mut lines = vec![
        Line::from(Span::styled(
            format!("? {}", modal.title),
            Style::new().fg(color).bold(),
        )),
        Line::raw(""),
    ];
    if !modal.body.is_empty() {
        lines.push(Line::raw(modal.body.clone()));
        lines.push(Line::raw(""));
    }
    lines.push(Line::from(Span::styled(keys, Style::new().fg(color).bold())));
    let rows: u16 = lines
        .iter()
        .map(|line| (line.width() as u16).div_ceil(inner_width).max(1))
        .sum();
    let height = (rows + 2).min(body.height);
    let area = Rect {
        x: body.x + body.width.saturating_sub(width) / 2,
        y: body.y + body.height.saturating_sub(height) / 2,
        width,
        height,
    };
    f.render_widget(Clear, area);
    f.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: true }).block(
            Panel::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(color)),
        ),
        area,
    );
}

fn draw_foot(f: &mut Frame, app: &App, area: Rect) {
    if let Some(input) = app.cmdline.as_ref() {
        let text = input.text();
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(":", Style::new().fg(ACCENT).bold()),
                Span::raw(text),
            ])),
            area,
        );
        let (_, (_, col)) = input.layout(usize::MAX / 2);
        f.set_cursor_position(Position {
            x: area.x + 1 + col as u16,
            y: area.y,
        });
        return;
    }
    f.render_widget(Paragraph::new(foot_lines(app, area.width as usize)), area);
}

const FOOT_ROWS: usize = 3;

fn foot_height(app: &App, width: u16) -> u16 {
    if app.cmdline.is_some() {
        return 1;
    }
    foot_lines(app, width as usize).len() as u16
}

/// The key bar, on up to three lines so no label is cut. When even three are not enough, the
/// screen's own keys go from the end; the last three (tasks, new, quit) and help always stay.
fn foot_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut pieces: Vec<Vec<Span<'static>>> = footer_keys(app)
        .into_iter()
        .map(|(key, desc)| {
            vec![
                Span::styled(key, Style::new().fg(ACCENT).bold()),
                Span::styled(format!(" {desc}   "), Style::new().fg(DIM)),
            ]
        })
        .collect();
    let tail = if app.composer.is_none() && app.reader.is_none() {
        pieces.len().saturating_sub(3)
    } else {
        0
    };
    if app.composer.is_none() {
        pieces.push(vec![
            Span::styled("?", Style::new().fg(ACCENT).bold()),
            Span::styled(tr!(" help", " ayuda"), Style::new().fg(DIM)),
        ]);
    }
    let mut shown: Vec<&[Span<'static>]> = pieces.iter().map(Vec::as_slice).collect();
    let mut droppable = tail;
    loop {
        let rows = packed(&shown, width);
        if rows.len() <= FOOT_ROWS || droppable == 0 {
            return rows.into_iter().take(FOOT_ROWS).collect();
        }
        droppable -= 1;
        shown.remove(droppable);
    }
}

fn packed(pieces: &[&[Span<'static>]], width: usize) -> Vec<Line<'static>> {
    let mut rows = Vec::new();
    let mut row = vec![Span::raw(" ")];
    let mut used = 1;
    for piece in pieces {
        let full: usize = piece.iter().map(Span::width).sum();
        let trailing = piece.last().map_or(0, |span| span.content.len() - span.content.trim_end().len());
        if used > 1 && used + full - trailing > width {
            rows.push(Line::from(std::mem::replace(&mut row, vec![Span::raw(" ")])));
            used = 1;
        }
        row.extend(piece.iter().cloned());
        used += full;
    }
    rows.push(Line::from(row));
    rows
}

/// The help screen: every key of the screen behind it, from the same table the key bar and the
/// handler read, then what the header bars mean and the keys that work everywhere.
pub(super) fn help_lines(app: &App) -> Vec<Line<'static>> {
    let section = |t: &str| {
        Line::from(Span::styled(
            format!(" {t}"),
            Style::new().fg(ACCENT).bold(),
        ))
    };
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("  {k:<16}"), Style::new().bold()),
            Span::styled(d.to_string(), Style::new().fg(SOFT)),
        ])
    };
    let table = app.bindings();
    let (bar, always): (Vec<_>, Vec<_>) = table.0.iter().partition(|binding| binding.shown == super::bindings::Shown::Bar);
    let mut lines = vec![section(tr!("This screen", "Esta pantalla"))];
    lines.extend(bar.iter().map(|binding| row(&binding.hint, &binding.label)));
    if bar.iter().any(|binding| matches!(binding.act, super::bindings::Act::Move)) {
        lines.push(row(
            "gg  Shift+G",
            tr!("first / last; arrows, PgUp and PgDn work too", "primero / último; también flechas, RePág y AvPág"),
        ));
    }
    lines.push(Line::default());
    lines.extend([
        section(tr!(
            "Bars in the header, left to right",
            "Barras de la cabecera, de izquierda a derecha"
        )),
        row(
            tr!("week", "semana"),
            tr!(
                "Claude's weekly limit used; it turns red past 85%",
                "límite semanal de Claude usado; en rojo pasado el 85%"
            ),
        ),
        row(
            tr!("today", "hoy"),
            tr!(
                "today's spend / today's share of the week; red when over it (no launches without asking)",
                "gastado hoy / lo que te toca hoy de la semana; en rojo si te pasas (no se lanzan sesiones sin preguntarte)"
            ),
        ),
        row(
            "RAM",
            tr!("the Mac's memory in use", "memoria del Mac en uso"),
        ),
        row(
            "CPU",
            tr!(
                "the Mac's CPU in use; red when the load is too high to trust the readings",
                "CPU del Mac en uso; en rojo si la carga es tan alta que las lecturas no son fiables"
            ),
        ),
        row(
            tr!("disk", "disco"),
            tr!(
                "free space on the Mac's disk; red when it is nearly full",
                "espacio libre en el disco del Mac; en rojo si casi está lleno"
            ),
        ),
        Line::default(),
        section(tr!("Always", "Siempre")),
    ]);
    lines.extend(always.iter().map(|binding| row(&binding.hint, &binding.label)));
    lines.extend([
        row("Ctrl-c", tr!("quit", "salir")),
        row(
            tr!("typing", "al escribir"),
            tr!(
                "letters go to the text box; Esc leaves it",
                "las letras van al cuadro de texto; Esc sale"
            ),
        ),
    ]);
    lines
}

fn draw_help(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let mut lines = help_lines(app);
    if let Some(status) = &app.view_status {
        for gauge in &status.gauges {
            if let Some(detail) = &gauge.detail {
                lines.push(Line::from(format!("{}: {detail}", gauge.label)).fg(DIM));
            }
        }
    }
    let w = 96.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    };
    let view_h = h.saturating_sub(2) as usize;
    app.help_view = View {
        h: view_h,
        total: lines.len(),
        shown: true,
    };
    app.help_scroll = app.help_scroll.min(lines.len().saturating_sub(view_h));
    let title = tr!(
        " Keys · j/k scroll · any other key closes ",
        " Teclas · j/k desplaza · cualquier otra tecla cierra "
    );
    let mut block = panel(Line::from(title.bold()), true);
    if let Some(hint) = more_hint(app.help_scroll, view_h, lines.len()) {
        block = block.title_bottom(hint);
    }
    f.render_widget(Clear, r);
    f.render_widget(
        Paragraph::new(lines)
            .scroll((app.help_scroll as u16, 0))
            .block(block),
        r,
    );
}
