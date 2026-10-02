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
use super::{App, Key, Pane, Screen, Target, View, unavailable};
use crate::i18n::{self, Occupancy};
use crate::runtime::FleetNode;
use alondra_core::NodeState;

const ACCENT: Color = Color::LightYellow;
const DIM: Color = Color::DarkGray;
const SOFT: Color = Color::Gray;
const SEL_BG: Color = Color::Indexed(236);
const COMMENT: Color = Color::LightMagenta;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(f.area());
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
    } else if let Some(view) = app.home_view.clone() {
        draw_views_inbox(f, app, &view, body);
    } else {
        draw_waiting(f, app, body);
    }
    if let Some(form) = app.composer.as_ref().and_then(|c| c.form.as_ref()) {
        draw_form_popup(f, form, body);
    }
    draw_foot(f, app, foot);
    draw_flash(f, app, body);
    if app.help {
        draw_help(f, app);
    }
}

pub fn ago(ms: i64) -> String {
    let s = ((now_millis() - ms) / 1000).max(0);
    match s {
        0..=59 => tr!("now", "ahora").into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86400),
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

pub(super) fn since_phrase(ms: i64) -> String {
    let t = ago(ms);
    if now_millis() - ms < 60_000 {
        tr!("since just now", "desde ahora mismo").into()
    } else {
        trf!("for {t}", "desde hace {t}")
    }
}

pub(super) fn ago_phrase(ms: i64) -> String {
    if now_millis() - ms < 60_000 {
        return tr!("just now", "ahora mismo").into();
    }
    let t = ago(ms);
    trf!("{t} ago", "hace {t}")
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
    let mut left = vec![
        Span::styled(
            format!(" {} ", crate::service_client::target().unwrap_or("Alondra")),
            Style::new()
                .fg(Color::Black)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
    ];
    let count = |id: &str, fallback: usize| {
        app.view_status
            .as_ref()
            .and_then(|s| s.counts.iter().find(|c| c.id == id))
            .map_or(fallback, |c| c.value as usize)
    };
    let (inbox, tasks, agents) = (
        count("inbox", app.inbox_count()),
        count("tasks", 0),
        count("agents", 0),
    );
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
    left.push(tab(
        trf!("Fleet {agents}", "Flota {agents}"),
        app.screen == Screen::Fleet,
    ));
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
    let right = [
        bars(&status.gauges, working.as_ref(), true),
        bars(&status.gauges, working.as_ref(), false),
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
    app.detail_view.shown = w >= 84;
    if w >= 84 {
        let lw = if w >= 170 {
            50
        } else {
            (w * 2 / 5).clamp(36, 56)
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
        Screen::Fleet => tr!(" Fleet ", " Flota "),
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
                    "What you ask for shows up here at once, and the orchestrator splits it into tasks.",
                    "Lo que pidas sale aquí al momento y el orquestador lo reparte en tareas."
                )
                .fg(DIM),
            ),
            Line::default(),
            hint("n", tr!(" order new work", " encarga trabajo nuevo")),
        ],
        Screen::Fleet => vec![Line::from(
            tr!("No nodes yet.", "Todavía no hay nodos.").fg(DIM),
        )],
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
        if let Some(title) = &section.title {
            let count = section.count.unwrap_or_else(|| section.items.len() as u32);
            lines.push(Line::from(vec![
                Span::styled(
                    format!(" {} ", title.to_uppercase()),
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(count.to_string(), Style::new().fg(DIM)),
            ]));
        }
        for item in &section.items {
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
            for child in &row.children {
                let selected = app.sel.as_ref().is_some_and(|key| key.0 == child.id);
                let start = lines.len();
                lines.extend(super::view_render::row_lines(
                    child,
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
        let summary = if app.card_view_asked.is_none() && app.screen == Screen::Fleet {
            fleet_summary(app)
        } else {
            vec![]
        };
        if summary.is_empty() {
            centered_lines(f, inner, vec![Line::from(message.fg(DIM))]);
        } else {
            f.render_widget(Paragraph::new(summary), inner);
        }
        return;
    };
    let mut lines = super::view_render::document_lines(document, inner.width as usize);
    let actions = visible_detail_actions(document);
    if !actions.is_empty() {
        lines.push(Line::default());
        for action in &actions {
            let hint = super::view_render::action_hint(action);
            let label = if action.enabled {
                action.label.clone()
            } else {
                format!(
                    "{} · {}",
                    action.label,
                    action.disabled_reason.as_deref().unwrap_or("unavailable")
                )
            };
            let mut title = Vec::new();
            if action.enabled {
                let rendered = super::view_render::action_line(action);
                if let Some((hint, label)) = rendered
                    .strip_prefix('[')
                    .and_then(|rendered| rendered.split_once("] "))
                {
                    title.push(Span::styled(
                        format!(" {hint} "),
                        Style::new()
                            .fg(Color::Black)
                            .bg(ACCENT)
                            .add_modifier(Modifier::BOLD),
                    ));
                    title.push(Span::raw(format!(" {label}")));
                } else {
                    title.push(Span::raw(format!(" {rendered}")));
                }
            } else if hint.is_empty() {
                title.push(Span::raw(format!(" {label}")));
            } else {
                title.push(Span::styled(
                    format!(" {hint} "),
                    Style::new()
                        .fg(Color::Black)
                        .bg(ACCENT)
                        .add_modifier(Modifier::BOLD),
                ));
                title.push(Span::raw(format!(" {label}")));
            }
            lines.push(Line::from(title));
            for (index, option) in action.options.iter().enumerate() {
                let hint = option
                    .key
                    .clone()
                    .unwrap_or_else(|| (index + 1).to_string());
                let label = merge_option_label(option);
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("  {hint} "),
                        Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(label),
                ]));
            }
        }
    }
    app.detail_view.total = lines.len();
    app.detail_view.h = inner.height as usize;
    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((app.detail_scroll as u16, 0)),
        inner,
    );
    scrollbar(f, inner, app.detail_view.total, app.detail_scroll);
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
        Span::styled(
            truncate(
                &md::one_line(&r.title),
                (area.width as usize).saturating_sub(40).max(10),
            ),
            Style::new().bold(),
        ),
        Span::raw(" "),
    ];
    if !still_there && app.loaded {
        title.push(Span::styled(
            tr!(" no longer in the inbox ", " ya no está en la bandeja "),
            Style::new().fg(Color::Black).bg(Color::LightRed),
        ));
    }
    let n_comments = review.map(|rv| rv.count()).unwrap_or(0);
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
    let rendered = md::render(&r.blocks, width);
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
    scrollbar(f, doc, total, r.scroll);
    if let Some(side) = side {
        let cursor = r.cursor;
        draw_comments(f, app, side, cursor);
    }
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
            Span::styled(format!("{k:<6}"), Style::new().fg(ACCENT).bold()),
            Span::styled(d.to_string(), Style::new().fg(DIM)),
        ])
    };
    match review.filter(|review| !review.is_empty()) {
        None => {
            lines.push(Line::from(tr!("None yet.", "Todavía ninguno.").fg(DIM)));
            lines.push(Line::default());
            for (hint, label) in [
                (
                    "j/k",
                    tr!("move the cursor by blocks", "mueve el cursor por bloques"),
                ),
                (
                    "^d ^u",
                    tr!("half a page down / up", "media página abajo / arriba"),
                ),
                (
                    "/",
                    tr!(
                        "search the text; n/N next/prev",
                        "busca en el texto; n/N sig./ant."
                    ),
                ),
                (
                    "c",
                    tr!("comment on the marked block", "comenta el bloque marcado"),
                ),
                ("Shift+C", tr!("general comment", "comentario general")),
            ] {
                lines.push(key(hint, label));
            }
            if let Some(view) = app.card_view.as_ref().filter(|view| view.key == r.key.0) {
                for (hint, label) in view_footer_actions(view) {
                    lines.push(key(&hint, &label));
                }
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
        let icon = if chip.image() { "🖼" } else { "📄" };
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
            "New work: the orchestrator turns it into tasks",
            "Trabajo nuevo: el orquestador lo convierte en tareas"
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

/// The fleet in one glance for an empty detail pane, from the counts the server sends.
fn fleet_summary(app: &App) -> Vec<Line<'static>> {
    let Some(status) = app.view_status.as_ref() else {
        return vec![];
    };
    let mut lines = vec![
        Line::from(tr!("The fleet right now", "La flota ahora").fg(DIM).bold()),
        Line::default(),
    ];
    for id in ["working", "waiting"] {
        if let Some(count) = status.counts.iter().find(|count| count.id == id) {
            lines.push(Line::from(vec![
                Span::styled("● ", Style::new().fg(ACCENT)),
                Span::raw(format!("{} {}", count.value, count.label)),
            ]));
        }
    }
    lines
}

pub(super) fn view_footer_actions(view: &alondra_core::views::View) -> Vec<(String, String)> {
    let mut keys = Vec::new();
    let has_diff = view.sections.iter().flat_map(|section| &section.items).any(|item|
        matches!(item, alondra_core::views::Item::ContentRef(content)
            if content.kind == alondra_core::views::ContentKind::Diff));
    for action in super::view_render::actions(view)
        .into_iter()
        .filter(|action| action.enabled)
    {
        let hint = super::view_render::action_hint(&action);
        if !hint.is_empty() && !(has_diff && hint.eq_ignore_ascii_case("d")) {
            let entry = (
                if hint == "enter" { "⏎" } else { hint }.to_owned(),
                action.label.clone(),
            );
            if !keys.contains(&entry) {
                keys.push(entry);
            }
        }
        for (index, option) in action.options.iter().enumerate() {
            let hint = option
                .key
                .clone()
                .unwrap_or_else(|| (index + 1).to_string());
            keys.push((hint, merge_option_label(&option)));
        }
    }
    if has_diff {
        keys.push(("d".into(), tr!("diff and comments", "diff y comentarios").into()));
    }
    keys
}

fn merge_option_label(option: &alondra_core::views::ActionOption) -> String {
    let method = match option.id.as_str() {
        "merge" => Some(alondra_core::pr::MergeMethod::Merge),
        "squash" => Some(alondra_core::pr::MergeMethod::Squash),
        "rebase" => Some(alondra_core::pr::MergeMethod::Rebase),
        _ => None,
    };
    match method {
        Some(alondra_core::pr::MergeMethod::Merge) => tr!("Merge commit", "Commit de fusión").into(),
        Some(alondra_core::pr::MergeMethod::Squash) => tr!("Squash merge", "Fusión squash").into(),
        Some(alondra_core::pr::MergeMethod::Rebase) => "rebase".into(),
        None => option.label.clone(),
    }
}

pub(super) fn visible_detail_actions(document: &alondra_core::views::View) -> Vec<alondra_core::views::Action> {
    super::view_render::actions(document)
        .into_iter()
        .filter(|action| action.enabled || !action.id.starts_with("accept:"))
        .collect()
}

/// Only an uncertain handoff needs the user: a dispatching one settles by itself.
pub(super) fn footer_keys(app: &App) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if app.composer.is_none() {
        if let Some(reader) = app.reader.as_ref() {
            if reader.find.as_ref().is_some_and(|find| find.editing) {
                return vec![
                    k("⏎", tr!("find", "buscar")),
                    k("Esc", tr!("cancel", "cancela")),
                ];
            }
            let mut keys = vec![
                k("j/k", tr!("block", "bloque")),
                k("/", tr!("search", "buscar")),
                k("c", tr!("comment", "comentar")),
                k("Shift+C", tr!("general", "general")),
                k("d", tr!("delete", "borrar")),
            ];
            if let Some(view) = app
                .card_view
                .as_ref()
                .filter(|view| view.key == reader.key.0)
            {
                keys.extend(view_footer_actions(view));
            }
            keys.push(k("Esc/o", tr!("back", "volver")));
            return keys;
        }
        if let Some(home) = app.home_view.as_ref() {
            let mut keys = match app.screen {
                Screen::Inbox if app.focus == Pane::Detail => {
                    vec![k("j/k", tr!("scroll", "desplazar"))]
                }
                Screen::Inbox => vec![k("j/k", tr!("move", "mover"))],
                Screen::Tasks | Screen::Fleet if app.schedules_open => {
                    vec![k("j/k", tr!("move", "mover"))]
                }
                Screen::Tasks | Screen::Fleet => vec![
                    k("j/k", tr!("move", "mover")),
                    k("h/l", tr!("pane", "panel")),
                ],
            };
            if !app.schedules_open {
                keys.push(k("o", tr!("expand", "expandir")));
            }
            let mut from_server = Vec::new();
            if let Some(card) = app.card_view.as_ref() {
                from_server.extend(view_footer_actions(card));
            } else if let Some(selected) = app.sel.as_ref()
                && let Some(row) = super::view_render::rows(home)
                    .into_iter()
                    .find_map(|(row, _)| (row.id == selected.0).then_some(row))
            {
                for action in row.actions.iter().filter(|action| action.enabled) {
                    let hint = super::view_render::action_hint(action);
                    if !hint.is_empty() {
                        from_server.push((
                            if hint == "enter" { "⏎" } else { hint }.into(),
                            action.label.clone(),
                        ));
                    }
                }
            }
            for entry in from_server {
                if !keys.contains(&entry) {
                    keys.push(entry);
                }
            }
            if app.schedules_open {
                keys.extend([
                    k("n", tr!("create", "crear")),
                    k("Esc", tr!("back", "volver")),
                ]);
                return keys;
            }
            let mut shortcuts = Vec::new();
            match app.screen {
                Screen::Inbox => {}
                Screen::Tasks => shortcuts.extend([
                    k("r", tr!("message", "mensaje")),
                    k("⏎", tr!("its terminal", "su terminal")),
                    k("x", tr!("cancel / withdraw", "descartar / retirar")),
                    k("Shift+A", tr!("archive finished", "archivar terminadas")),
                    k("s", tr!("scheduled", "programadas")),
                ]),
                Screen::Fleet => shortcuts.extend([
                    k("/", tr!("search", "buscar")),
                    k("r", tr!("message", "mensaje")),
                    k("⏎", tr!("its terminal (← returns)", "su terminal (← vuelve)")),
                    k("x", tr!("close node", "cerrar nodo")),
                ]),
            }
            for shortcut in shortcuts {
                if !keys.iter().any(|(key, _)| key == &shortcut.0) {
                    keys.push(shortcut);
                }
            }
            if app.screen == Screen::Inbox {
                keys.push(k("^d/^u", tr!("scroll detail", "desplazar detalle")));
            }
            keys.extend([
                k(
                    "Tab",
                    match app.screen {
                        Screen::Inbox => tr!("tasks", "tareas"),
                        Screen::Tasks => tr!("fleet", "flota"),
                        Screen::Fleet => tr!("inbox", "bandeja"),
                    },
                ),
                k("n", tr!("new", "nuevo")),
                k("q", tr!("quit", "salir")),
            ]);
            return keys;
        }
    }
    if let Some(c) = app.composer.as_ref() {
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
        return vec![
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
        ];
    }
    match app.screen {
        Screen::Tasks if app.schedules_open => vec![
            k("j/k", tr!("move", "mover")),
            k("n", tr!("new", "nueva")),
            k("e", tr!("edit", "editar")),
            k("p", tr!("pause / resume", "pausar / reanudar")),
            k("⏎", tr!("run", "ejecutar")),
            k("d d", tr!("delete", "borrar")),
            k("Esc", tr!("back", "volver")),
        ],
        Screen::Tasks => {
            let mut keys = vec![k("j/k", tr!("move", "mover")), k("h/l", tr!("pane", "panel"))];
            match app.task_sel.as_ref() {
                Some(super::tasks::TaskKey::Finished | super::tasks::TaskKey::Archived) => {
                    keys.push(k("⏎", tr!("show / hide", "mostrar / ocultar")));
                }
                Some(super::tasks::TaskKey::Task(_)) => {
                    keys.extend([
                        k("o", tr!("detail", "detalle")),
                        k("⏎", tr!("its terminal", "su terminal")),
                        k("r", tr!("message", "mensaje")),
                        k("x", tr!("cancel", "descartar")),
                    ]);
                }
                Some(super::tasks::TaskKey::Request(_)) => {
                    keys.extend([
                        k("o", tr!("detail", "detalle")),
                        k("x", tr!("withdraw", "retirar")),
                    ]);
                }
                None => {}
            }
            keys.extend([
                k("Shift+A", tr!("archive finished", "archivar terminadas")),
                k("s", tr!("scheduled", "programadas")),
                k("Tab", tr!("fleet", "flota")),
                k("n", tr!("new", "nuevo")),
                k("q", tr!("quit", "salir")),
            ]);
            keys
        }
        Screen::Fleet if app.orchestrator_selected() => vec![
            k("j/k", tr!("move", "mover")),
            k("⏎/o", tr!("activity", "actividad")),
            k("r", tr!("message", "mensaje")),
            k("Esc", tr!("back", "volver")),
            k("n", tr!("new", "nuevo")),
            k("Tab", tr!("inbox", "bandeja")),
        ],
        Screen::Fleet if app.agent_zoom => vec![
            k("j/k", tr!("scroll", "desplazar")),
            k("Esc/o", tr!("back", "volver")),
            k("r", tr!("message", "mensaje")),
            k("x", tr!("close", "cerrar")),
            k("a", tr!("its terminal", "su terminal")),
        ],
        Screen::Fleet => vec![
            k("j/k", tr!("move", "mover")),
            k("h/l", tr!("pane", "panel")),
            k("/", tr!("search", "buscar")),
            k("o", tr!("expand", "expandir")),
            k("r", tr!("message", "mensaje")),
            k("⏎", tr!("its terminal", "su terminal")),
            k("x", tr!("close node", "cerrar nodo")),
            k("n", tr!("new", "nuevo")),
            k("Tab", tr!("inbox", "bandeja")),
            k("q", tr!("quit", "salir")),
        ],
        Screen::Inbox => vec![
            k("j/k", tr!("move", "mover")),
            k("⏎/o", tr!("detail", "detalle")),
            k("r", tr!("reply", "responder")),
            k("x", tr!("mark read", "marcar leído")),
            k("/", tr!("search", "buscar")),
            k("Tab", tr!("tasks", "tareas")),
            k("n", tr!("new", "nuevo")),
            k("q", tr!("quit", "salir")),
        ],
    }
}

/// Action results float above the key bar, wrapped, so they never hide the keys or get cut.
fn draw_flash(f: &mut Frame, app: &App, body: Rect) {
    let Some(fl) = &app.flash else { return };
    let (mark, color) = if fl.error {
        ("✗ ", Color::LightRed)
    } else {
        ("✓ ", Color::LightGreen)
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
    let width = area.width as usize;
    let help = vec![
        Span::styled("?", Style::new().fg(ACCENT).bold()),
        Span::styled(tr!(" help", " ayuda"), Style::new().fg(DIM)),
    ];
    let help_w: usize = help.iter().map(Span::width).sum();
    let mut spans = vec![Span::raw(" ")];
    let mut w = 1;
    let pieces: Vec<Vec<Span<'static>>> = footer_keys(app)
        .into_iter()
        .map(|(key, desc)| {
            vec![
                Span::styled(key, Style::new().fg(ACCENT).bold()),
                Span::styled(format!(" {desc}   "), Style::new().fg(DIM)),
            ]
        })
        .collect();
    let widths: Vec<usize> = pieces
        .iter()
        .map(|piece| piece.iter().map(Span::width).sum())
        .collect();
    let tail = if app.composer.is_none() && app.reader.is_none() {
        pieces.len().saturating_sub(3)
    } else {
        pieces.len()
    };
    let tail_w: usize = widths[tail..].iter().sum();
    for (at, piece) in pieces.into_iter().enumerate() {
        let reserved = if at < tail { tail_w } else { 0 };
        if w + widths[at] + reserved + help_w > width {
            if at < tail {
                continue;
            }
            break;
        }
        w += widths[at];
        spans.extend(piece);
    }
    if app.composer.is_none() {
        spans.extend(help);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(super) fn help_lines() -> Vec<Line<'static>> {
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
    vec![
        section(tr!(
            "Moving (vim style; arrows work too)",
            "Moverse (estilo vim; las flechas también valen)"
        )),
        row(
            "j/k  ↓↑",
            tr!(
                "down / up; after answering you move to the next one",
                "abajo / arriba; al contestar pasas solo a la siguiente"
            ),
        ),
        row("gg  Shift+G", tr!("first / last", "primero / último")),
        row(
            "Ctrl-d  Ctrl-u",
            tr!(
                "half a page down / up (scrolls the detail)",
                "media página abajo / arriba (desplaza el detalle)"
            ),
        ),
        row(
            "Ctrl-f  Ctrl-b",
            tr!(
                "a full page down / up; also Space, PgDn, PgUp",
                "página entera abajo / arriba; también Espacio, AvPág, RePág"
            ),
        ),
        row(
            "h/l  ←→",
            tr!(
                "focus the list / the detail pane",
                "enfocar la lista / el panel de detalle"
            ),
        ),
        row(
            "o  ⏎",
            tr!(
                "expand: open the whole item",
                "expandir: abrir entero el elemento"
            ),
        ),
        row(
            "Esc",
            tr!(
                "back: close, collapse or leave the detail pane",
                "volver: cerrar, plegar o salir del panel de detalle"
            ),
        ),
        row(
            ":",
            tr!(
                "command line: :q  :help  :inbox  :tasks  :fleet  :<number>",
                "línea de órdenes: :q  :help  :inbox  :tasks  :fleet  :<número>"
            ),
        ),
        Line::default(),
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
        section(tr!("Inbox", "Bandeja")),
        row(
            "1-9",
            tr!(
                "question: choose an option · delivery with a PR: authorize a merge method (twice to confirm)",
                "pregunta: elegir una opción · entrega con PR: autorizar un método de fusión (dos veces para confirmar)"
            ),
        ),
        row(
            "1",
            tr!(
                "on a PR in conflict: its node resolves the conflicts",
                "en un PR con conflictos: su nodo los resuelve"
            ),
        ),
        row(
            "r",
            tr!(
                "question: write the answer or reply to its node · delivery or PR: feedback · information: reply",
                "pregunta: escribir la respuesta o responder a su nodo · entrega o PR: feedback · información: responder"
            ),
        ),
        row(
            "c",
            tr!(
                "send the question back asking for more context",
                "devolver la pregunta pidiendo más contexto"
            ),
        ),
        row(
            "p",
            tr!("ask the PR's node about it", "preguntar al nodo del PR"),
        ),
        row(
            "v",
            tr!(
                "accept a delivery (the node still finishes the task) · elsewhere: open its images and files",
                "aceptar una entrega (el nodo sigue siendo quien termina la tarea) · en lo demás: abrir sus imágenes y ficheros"
            ),
        ),
        row(
            "x",
            tr!(
                "mark information read; nothing is sent to the node",
                "marcar información leída; no se envía nada al nodo"
            ),
        ),
        row(
            "d · w",
            tr!(
                "the PR in tuicr on its clone (its diff here otherwise) · the PR on GitHub",
                "el PR en tuicr sobre su copia (si no, su diff aquí) · el PR en GitHub"
            ),
        ),
        row(
            "Shift+J/Shift+K",
            tr!("scroll the detail 5 lines", "desplazar el detalle 5 líneas"),
        ),
        row(
            "a",
            tr!(
                "open the node's terminal (← on an empty line, Ctrl+Z or /exit returns to Alondra)",
                "abrir el terminal del nodo (← con la línea vacía, Ctrl+Z o /exit vuelven a Alondra)"
            ),
        ),
        Line::default(),
        section(tr!("Reading (after o)", "Lectura (tras o)")),
        row(
            "j/k",
            tr!(
                "next / previous block; long blocks scroll line by line",
                "bloque siguiente / anterior; los bloques largos avanzan línea a línea"
            ),
        ),
        row(
            "/  n  Shift+N",
            tr!(
                "search the text · next match · previous match",
                "buscar en el texto · siguiente · anterior"
            ),
        ),
        row(
            "c · Shift+C · d",
            tr!(
                "comment on block · general comment · delete comment",
                "comentar bloque · comentario general · borrar"
            ),
        ),
        row(
            "s · v · x",
            tr!(
                "send comments as feedback (or to the node) · accept · mark read",
                "enviar comentarios como feedback (o al nodo) · aceptar · leído"
            ),
        ),
        row(
            "1-9",
            tr!(
                "on a question: answer with that option and your comments",
                "en una pregunta: contestar con esa opción y tus comentarios"
            ),
        ),
        row("q  o  Esc", tr!("close the reading", "cerrar la lectura")),
        Line::default(),
        section(tr!("Tasks", "Tareas")),
        row(
            "o",
            tr!(
                "expand the task's detail to full screen",
                "expandir el detalle de la tarea a pantalla completa"
            ),
        ),
        row(
            "r · ⏎",
            tr!(
                "message its node · its node's terminal",
                "mensaje a su nodo · el terminal de su nodo"
            ),
        ),
        row(
            "x",
            tr!(
                "cancel the task, or withdraw a request without tasks (twice to confirm)",
                "descartar la tarea, o retirar una petición sin tareas (dos veces para confirmar)"
            ),
        ),
        row(
            "Shift+R",
            tr!(
                "reopen a finished or cancelled task",
                "reabrir una tarea terminada o descartada"
            ),
        ),
        row(
            "Shift+A",
            tr!(
                "archive every finished or cancelled task",
                "archivar todas las tareas terminadas o descartadas"
            ),
        ),
        row(
            "⏎ · o",
            tr!(
                "on Finished or Archived: show or hide them",
                "en Terminadas o Archivadas: mostrar u ocultar"
            ),
        ),
        row(
            "s",
            tr!(
                "scheduled tasks: n creates one, e edits it, Esc goes back",
                "tareas programadas: n crea una, e la edita, Esc vuelve"
            ),
        ),
        Line::default(),
        section(tr!("Fleet", "Flota")),
        row(
            "/",
            tr!(
                "search names and local node details",
                "buscar nombres y detalles locales del nodo"
            ),
        ),
        row(
            "r · ⏎ · a",
            tr!(
                "message the node · its terminal",
                "mensaje al nodo · su terminal"
            ),
        ),
        row(
            "o",
            tr!(
                "expand the node's detail to full screen",
                "expandir el detalle del nodo a pantalla completa"
            ),
        ),
        row(
            "x",
            tr!(
                "close the node (twice to confirm; never discards work)",
                "cerrar el nodo (dos veces para confirmar; nunca descarta trabajo)"
            ),
        ),
        row(
            "C · S · R",
            tr!(
                "orchestrator: continue without repeating · stop · retry (twice each)",
                "orquestador: continuar sin repetir · detener · reintentar (dos veces cada una)"
            ),
        ),
        Line::default(),
        section(tr!("Always", "Siempre")),
        row(
            "Tab",
            tr!(
                "inbox → tasks → fleet (Shift+Tab goes back)",
                "bandeja → tareas → flota (Shift+Tab vuelve)"
            ),
        ),
        row(
            "n · Shift+M",
            tr!(
                "new work for the orchestrator · message the orchestrator",
                "trabajo nuevo para el orquestador · mensaje al orquestador"
            ),
        ),
        row("q  Ctrl-c", tr!("quit", "salir")),
        row(
            tr!("typing", "al escribir"),
            tr!(
                "letters go to the text box; Esc leaves it",
                "las letras van al cuadro de texto; Esc sale"
            ),
        ),
    ]
}

fn draw_help(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let mut lines = help_lines();
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
