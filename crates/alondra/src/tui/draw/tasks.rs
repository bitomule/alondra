use alondra_core::domain_v2::{Task, TaskState};
use alondra_core::now_millis;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::inbox::{authorization_state, pr_line};
use super::super::tasks::{TaskKey, TaskListRow, TreeRow, state_label};
use super::super::{App, Pane, View, unavailable};
use super::{ACCENT, DIM, SOFT, centered_lines, fit, highlight, keep_visible, kind_glyph, more_hint, panel, scrollbar, state_glyph};
use crate::tui::md::{self, str_width, truncate, wrap};

pub fn draw_tasks(f: &mut Frame, app: &mut App, area: Rect) {
    if app.task_zoom && app.task_sel.is_some() {
        draw_task_detail(f, app, area);
        return;
    }
    app.task_view.shown = area.width >= 100;
    let (list, detail) = if app.task_view.shown {
        let lw = (area.width * 3 / 5).clamp(56, 96);
        let [l, d] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        (l, Some(d))
    } else {
        (area, None)
    };
    let focused = app.focus == Pane::List || !app.task_view.shown;
    let block = panel(Line::from(tr!(" Tasks ", " Tareas ").bold()), focused);
    let inner = block.inner(list);
    f.render_widget(block, list);
    if !app.loaded {
        centered_lines(f, inner, vec![Line::from(tr!("Connecting to alondrad…", "Conectando con alondrad…").fg(DIM))]);
    } else {
        let width = inner.width.saturating_sub(1) as usize;
        let (lines, sel) = list_lines(app, width);
        let h = inner.height as usize;
        if let Some(range) = sel {
            keep_visible(&mut app.task_scroll, range, h, lines.len());
        }
        let total = lines.len();
        f.render_widget(Paragraph::new(lines).scroll((app.task_scroll as u16, 0)), inner);
        if let Some((s, e)) = sel {
            highlight(f, Rect { width: inner.width.saturating_sub(1), ..inner }, s..e, app.task_scroll);
        }
        scrollbar(f, inner, total, app.task_scroll);
    }
    if let Some(d) = detail {
        draw_task_detail(f, app, d);
    }
}

/// One row per item: the lead, the title cut to fit and what goes on the right.
fn titled(bar: &Span<'static>, lead: Vec<Span<'static>>, title: &str, style: Style, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let lead_w = bar.width() + lead.iter().map(Span::width).sum::<usize>();
    let right_w: usize = right.iter().map(Span::width).sum();
    let available = width.saturating_sub(lead_w + right_w + usize::from(right_w > 0));
    let part = truncate(&md::one_line(title), available);
    let used = lead_w + str_width(&part);
    let mut spans = vec![bar.clone()];
    spans.extend(lead);
    spans.push(Span::styled(part, style));
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used + right_w))));
    spans.extend(right);
    fit(spans, width)
}

fn title_style(t: &Task) -> Style {
    Style::new().add_modifier(if t.state == TaskState::Cancelled { Modifier::CROSSED_OUT } else { Modifier::empty() })
}

/// A small activity cue on running rows; the Tasks timer repaints it twice a second.
fn list_glyph(t: &Task) -> (&'static str, Color) {
    if t.state == TaskState::Running && !t.archived {
        let frames = ["◐", "◓", "◑", "◒"];
        (frames[(now_millis() / 500).rem_euclid(4) as usize], Color::Green)
    } else {
        state_glyph(t.state)
    }
}

fn state_span(t: &Task) -> Vec<Span<'static>> {
    let (_, color) = state_glyph(t.state);
    vec![Span::styled(state_label(t.state), Style::new().fg(color))]
}

fn tree_lead(row: &TreeRow) -> String {
    let mut s = String::from("   ");
    for on in &row.lines {
        s.push(if *on { '│' } else { ' ' });
    }
    if row.depth > 0 {
        s.push(if row.last { '└' } else { '├' });
    }
    s
}

fn tree_under(row: &TreeRow) -> String {
    let mut s = String::from("   ");
    for on in &row.lines {
        s.push(if *on { '│' } else { ' ' });
    }
    if row.depth > 0 {
        s.push(if row.last { ' ' } else { '│' });
    }
    s.push_str("  ");
    s
}

fn list_lines(app: &App, width: usize) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = vec![];
    let mut sel = None;
    for row in app.task_rows() {
        let key = row.key();
        let selected = key.is_some() && key == app.task_sel;
        let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
        if matches!(row, TaskListRow::Finished(_) | TaskListRow::Archived(_)) && !lines.is_empty() {
            lines.push(Line::default());
        }
        let start = lines.len();
        match row {
            TaskListRow::Active(n) => {
                lines.push(fit(vec![bar, Span::styled(tr!("IN PROGRESS", "EN CURSO"), Style::new().fg(ACCENT).bold())], width));
                if n == 0 {
                    lines.push(fit(vec![Span::raw(" "), Span::styled(tr!("Nothing open · n new work", "Nada abierto · n trabajo nuevo"), Style::new().fg(DIM))], width));
                }
            }
            TaskListRow::Single(id) => {
                let Some(t) = app.task(&id) else { continue };
                let (glyph, color) = list_glyph(t);
                let lead = vec![Span::raw("  "), Span::styled(format!("{glyph} "), Style::new().fg(color).bold())];
                lines.push(titled(&bar, lead, &t.title, title_style(t), state_span(t), width));
                if let Some(now) = app.task_now(t) {
                    lines.push(fit(vec![bar.clone(), Span::raw("    "), Span::styled(md::one_line(&now), Style::new().fg(SOFT))], width));
                }
            }
            TaskListRow::Request(id, done, total) => {
                let Some(r) = app.request(&id) else { continue };
                let right = vec![Span::styled(format!("{done}/{total}"), Style::new().fg(DIM))];
                let lead = vec![Span::raw("  "), Span::styled("▸ ", Style::new().fg(Color::Cyan))];
                lines.push(titled(&bar, lead, &app.request_title(r), Style::new().bold(), right, width));
            }
            TaskListRow::Task(row) => {
                let Some(t) = app.task(&row.id) else { continue };
                let (glyph, color) = list_glyph(t);
                let lead = vec![Span::styled(tree_lead(&row), Style::new().fg(DIM)), Span::styled(format!("{glyph} "), Style::new().fg(color).bold())];
                lines.push(titled(&bar, lead, &t.title, title_style(t), state_span(t), width));
                if let Some(now) = app.task_now(t) {
                    lines.push(fit(vec![bar.clone(), Span::styled(tree_under(&row), Style::new().fg(DIM)), Span::styled(md::one_line(&now), Style::new().fg(SOFT))], width));
                }
            }
            TaskListRow::Taskless(id) => {
                let Some(r) = app.request(&id) else { continue };
                let (glyph, color, state) = if r.closed { ("✓", DIM, tr!("closed", "cerrada")) } else { ("◇", Color::LightMagenta, tr!("preparing", "preparando")) };
                let lead = vec![Span::raw("  "), Span::styled(format!("{glyph} "), Style::new().fg(color).bold())];
                lines.push(titled(&bar, lead, &app.request_title(r), Style::new(), vec![Span::styled(state, Style::new().fg(color))], width));
            }
            TaskListRow::Finished(n) => {
                let arrow = if app.finished_open { "▾" } else { "▸" };
                lines.push(fit(vec![bar, Span::styled(format!("{arrow} "), Style::new().fg(DIM)), Span::styled(trf!("Finished · {n}", "Terminadas · {n}"), Style::new().bold())], width));
            }
            TaskListRow::Archived(n) => {
                let arrow = if app.archived_open { "▾" } else { "▸" };
                lines.push(fit(vec![bar, Span::styled(format!("{arrow} "), Style::new().fg(DIM)), Span::styled(trf!("Archived ({n})", "Archivadas ({n})"), Style::new().fg(DIM).bold())], width));
            }
        }
        if selected {
            sel = Some((start, lines.len()));
        }
    }
    (lines, sel)
}

fn section(lines: &mut Vec<Line<'static>>, title: &str, width: usize) {
    let title = title.trim().to_uppercase();
    let rest = width.saturating_sub(str_width(&title) + 4);
    lines.push(Line::default());
    lines.push(Line::from(vec![Span::styled(" ─ ", Style::new().fg(DIM)), Span::styled(title, Style::new().fg(Color::Cyan).bold()), Span::styled(format!(" {}", "─".repeat(rest)), Style::new().fg(DIM))]));
}

fn draw_task_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.task_zoom || app.focus == Pane::Detail;
    let width = area.width.saturating_sub(4) as usize;
    let (title, lines) = match app.task_sel.clone() {
        Some(TaskKey::Task(id)) => match app.task(&id) {
            Some(t) => {
                let (glyph, color) = state_glyph(t.state);
                (Line::from(vec![Span::styled(format!(" {glyph} "), Style::new().fg(color).bold()), Span::styled(tr!("Task ", "Tarea "), Style::new().bold())]), task_lines(app, t, width))
            }
            None => (Line::from(tr!(" Task ", " Tarea ")), vec![]),
        },
        Some(TaskKey::Request(id)) => match app.request(&id) {
            Some(r) => {
                let mut lines = vec![Line::from(Span::styled(if r.closed { tr!("✓ closed", "✓ cerrada") } else { tr!("◇ the orchestrator is splitting it into tasks", "◇ el orquestador la está repartiendo en tareas") }, Style::new().fg(SOFT).bold()))];
                section(&mut lines, tr!(" Request", " Petición"), width);
                lines.extend(md::render(&md::parse(&r.body), width).lines);
                (Line::from(tr!(" Request ", " Petición ")), lines)
            }
            None => (Line::from(tr!(" Request ", " Petición ")), vec![]),
        },
        Some(TaskKey::Finished) => (Line::from(tr!(" Finished ", " Terminadas ")), vec![Line::from(vec![super::keycap("⏎"), Span::raw(tr!(" show or hide · ", " mostrar u ocultar · ")), super::keycap("Shift+A"), Span::raw(tr!(" archive", " archivar"))])]),
        Some(TaskKey::Archived) => (Line::from(tr!(" Archived ", " Archivadas ")), vec![Line::from(vec![super::keycap("⏎"), Span::raw(tr!(" show or hide", " mostrar u ocultar"))])]),
        None => (Line::from(tr!(" Task ", " Tarea ")), vec![]),
    };
    if lines.is_empty() {
        app.task_view.total = 0;
        f.render_widget(panel(title, false), area);
        return;
    }
    let mut block = panel(title, focused);
    let inner = block.clone().inner(area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let total = lines.len();
    let h = inner.height as usize;
    app.task_detail_scroll = app.task_detail_scroll.min(total.saturating_sub(h));
    app.task_view = View { h, total, shown: app.task_view.shown || app.task_zoom };
    if let Some(hint) = more_hint(app.task_detail_scroll, h, total) {
        block = block.title_bottom(hint);
    }
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines).scroll((app.task_detail_scroll as u16, 0)), text);
    scrollbar(f, inner, total, app.task_detail_scroll);
}

fn task_lines(app: &App, t: &Task, width: usize) -> Vec<Line<'static>> {
    let (glyph, color) = state_glyph(t.state);
    let mut lines = vec![Line::from(Span::styled(format!("{glyph} {}", state_label(t.state)), Style::new().fg(color).bold()))];
    lines.extend(wrap(vec![Span::styled(md::one_line(&t.title), Style::new().bold())], width, vec![], vec![]));
    let agent = match &t.agent_id {
        Some(id) => {
            let name = app.agent_name(Some(id));
            let connected = app.domain.as_ref().and_then(|d| d.agents.iter().find(|a| &a.id == id)).is_some_and(|a| a.connected);
            if connected { trf!("agent {name} · connected", "agente {name} · conectado") } else { trf!("agent {name} · not connected", "agente {name} · no conectado") }
        }
        None => tr!("no agent assigned yet", "sin agente asignado todavía").into(),
    };
    lines.push(fit(vec![Span::styled(agent, Style::new().fg(DIM))], width));
    let progress = app.task_progress(t);
    if !progress.is_empty() {
        section(&mut lines, tr!(" Result", " Resultado"), width);
        lines.extend(wrap(vec![Span::styled(progress.join(" · "), Style::new().fg(SOFT))], width, vec![Span::raw(" ")], vec![Span::raw(" ")]));
    }
    let waiting: Vec<_> = app.inbox_rows().into_iter().filter(|(i, _)| i.task_id.as_ref() == Some(&t.id) && i.children.is_empty()).map(|(i, _)| i.clone()).collect();
    if !waiting.is_empty() {
        section(&mut lines, tr!(" In the inbox", " En la bandeja"), width);
        for item in waiting {
            let (g, k, c) = kind_glyph(&item.kind);
            let text = item.body.clone().unwrap_or_else(|| item.title.clone());
            lines.push(fit(vec![Span::styled(format!(" {g} {k} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&text))], width));
        }
    }
    if let Some(pr) = t.pr_id.as_ref().and_then(|id| app.pull_request(id)) {
        section(&mut lines, "PR", width);
        lines.extend(wrap(vec![Span::raw(pr_line(pr))], width, vec![Span::raw(" ")], vec![Span::raw(" ")]));
        for a in app.authorizations(&pr.id) {
            lines.push(fit(vec![Span::raw(" "), Span::styled(authorization_state(a), Style::new().fg(SOFT))], width));
        }
    }
    let deps: Vec<&Task> = t.dependencies.iter().filter_map(|id| app.task(id)).collect();
    if !deps.is_empty() {
        section(&mut lines, tr!(" Depends on", " Depende de"), width);
        for d in deps {
            let (g, c) = state_glyph(d.state);
            lines.push(fit(vec![Span::styled(format!(" {g} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&d.title)), Span::styled(format!(" · {}", state_label(d.state)), Style::new().fg(c))], width));
        }
    }
    let documents: Vec<String> = t
        .documents
        .keys()
        .filter_map(|id| app.domain.as_ref()?.documents.iter().find(|d| &d.id == id))
        .map(|d| {
            let number = app.domain.as_ref().and_then(|x| x.document_revisions.iter().find(|r| r.id == d.current_revision)).map_or(0, |r| r.number);
            let title = md::one_line(&d.title);
            trf!("{title} · version {number}", "{title} · versión {number}")
        })
        .collect();
    if !documents.is_empty() {
        section(&mut lines, tr!(" Documents", " Documentos"), width);
        for d in documents {
            lines.push(fit(vec![Span::styled(" ▤ ", Style::new().fg(Color::Cyan).bold()), Span::raw(d)], width));
        }
    }
    if let Some((_, actions)) = app.task_actions.get(&t.id) {
        section(&mut lines, tr!(" Controls", " Controles"), width);
        for a in actions {
            let (key, label) = match a.kind.as_str() {
                "cancel_task" => ("x", tr!("cancel", "descartar")),
                "reopen_task" => ("Shift+R", tr!("reopen", "reabrir")),
                "archive_task" => ("Shift+A", tr!("archive", "archivar")),
                "unarchive_task" => ("", tr!("unarchive", "desarchivar")),
                other => ("", other),
            };
            let line = if a.enabled {
                vec![super::keycap(key), Span::raw(format!(" {label}"))]
            } else {
                vec![Span::styled(format!(" {label}: {}", unavailable(a)), Style::new().fg(DIM))]
            };
            lines.push(fit(line, width));
        }
    }
    if let Some(r) = app.request(&t.request_id).filter(|r| !r.body.trim().is_empty()) {
        section(&mut lines, tr!(" Original request", " Petición original"), width);
        lines.extend(md::render(&md::parse(&r.body), width).lines);
    }
    lines
}

pub fn draw_agents(f: &mut Frame, app: &mut App, area: Rect) {
    if app.agent_zoom && app.selected_agent().is_some() {
        draw_agent_detail(f, app, area);
        return;
    }
    app.agent_view.shown = area.width >= 100;
    let (list, detail) = if app.agent_view.shown {
        let lw = (area.width * 11 / 20).clamp(56, 90);
        let [l, d] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        (l, Some(d))
    } else {
        (area, None)
    };
    let focused = app.focus == Pane::List || !app.agent_view.shown;
    let block = panel(Line::from(tr!(" Agents ", " Agentes ").bold()), focused);
    let inner = block.inner(list);
    f.render_widget(block, list);
    let mut body = inner;
    if let Some(s) = app.search.as_ref() {
        let [bar, rest] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        body = rest;
        let line = Line::from(vec![Span::styled(" / ", Style::new().fg(ACCENT).bold()), Span::raw(s.input.text())]);
        f.render_widget(Paragraph::new(vec![line, Line::styled("─".repeat(bar.width as usize), Style::new().fg(DIM))]), bar);
        if s.editing && app.composer.is_none() {
            let (_, (_, col)) = s.input.layout(usize::MAX / 2);
            f.set_cursor_position(ratatui::layout::Position { x: bar.x + 3 + col as u16, y: bar.y });
        }
    }
    let width = body.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    let mut sel = None;
    let agents: Vec<_> = app.agent_list().into_iter().cloned().collect();
    for a in &agents {
        let selected = app.agent_sel.as_ref() == Some(&a.id);
        let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
        let start = lines.len();
        let (dot, color, state) = if a.connected { ("●", Color::Green, tr!("connected", "conectado")) } else { ("○", DIM, tr!("not connected", "no conectado")) };
        lines.push(titled(&bar, vec![Span::styled(format!(" {dot} "), Style::new().fg(color))], &a.name, Style::new(), vec![Span::styled(state, Style::new().fg(color))], width));
        for t in app.agent_tasks(&a.id) {
            let (g, c) = state_glyph(t.state);
            lines.push(fit(vec![bar.clone(), Span::raw("    "), Span::styled(format!("{g} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&t.title)), Span::styled(format!(" · {}", state_label(t.state)), Style::new().fg(c))], width));
        }
        if selected {
            sel = Some((start, lines.len()));
        }
    }
    if lines.is_empty() {
        let msg = if app.search.is_some() { tr!("Nothing matches the search.", "Nada coincide con la búsqueda.") } else { tr!("No agents yet.", "Todavía no hay agentes.") };
        centered_lines(f, body, vec![Line::from(msg.fg(DIM))]);
    } else {
        let h = body.height as usize;
        if let Some(range) = sel {
            keep_visible(&mut app.agent_scroll, range, h, lines.len());
        }
        let total = lines.len();
        f.render_widget(Paragraph::new(lines).scroll((app.agent_scroll as u16, 0)), body);
        if let Some((s, e)) = sel {
            highlight(f, Rect { width: body.width.saturating_sub(1), ..body }, s..e, app.agent_scroll);
        }
        scrollbar(f, body, total, app.agent_scroll);
    }
    if let Some(d) = detail {
        draw_agent_detail(f, app, d);
    }
}

fn draw_agent_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(a) = app.selected_agent().cloned() else {
        app.agent_view.total = 0;
        f.render_widget(panel(Line::from(tr!(" Agent ", " Agente ").fg(DIM)), false), area);
        return;
    };
    let focused = app.agent_zoom || app.focus == Pane::Detail;
    let mut block = panel(Line::from(vec![Span::raw(" "), Span::styled(md::one_line(&a.name), Style::new().bold()), Span::raw(" ")]), focused);
    let inner = block.clone().inner(area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let width = text.width as usize;
    let state = if a.connected { tr!("● connected", "● conectado") } else { tr!("○ not connected", "○ no conectado") };
    let mut lines = vec![Line::from(Span::styled(state, Style::new().fg(if a.connected { Color::Green } else { DIM }).bold())), fit(vec![Span::styled(a.id.0.clone(), Style::new().fg(DIM))], width)];
    let tasks = app.agent_tasks(&a.id);
    if !tasks.is_empty() {
        section(&mut lines, tr!(" Tasks", " Tareas"), width);
        for t in tasks {
            let (g, c) = state_glyph(t.state);
            lines.push(fit(vec![Span::styled(format!(" {g} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&t.title)), Span::styled(format!(" · {}", state_label(t.state)), Style::new().fg(c))], width));
        }
    }
    let waiting: Vec<_> = app.inbox_rows().into_iter().filter(|(i, _)| i.agent_id.as_ref() == Some(&a.id) && i.children.is_empty()).map(|(i, _)| i.clone()).collect();
    if !waiting.is_empty() {
        section(&mut lines, tr!(" In the inbox", " En la bandeja"), width);
        for item in waiting {
            let (g, k, c) = kind_glyph(&item.kind);
            let text = item.body.clone().unwrap_or_else(|| item.title.clone());
            lines.push(fit(vec![Span::styled(format!(" {g} {k} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&text))], width));
        }
    }
    let total = lines.len();
    let h = inner.height as usize;
    app.agent_detail_scroll = app.agent_detail_scroll.min(total.saturating_sub(h));
    app.agent_view.h = h;
    app.agent_view.total = total;
    if let Some(hint) = more_hint(app.agent_detail_scroll, h, total) {
        block = block.title_bottom(hint);
    }
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines).scroll((app.agent_detail_scroll as u16, 0)), text);
    scrollbar(f, inner, total, app.agent_detail_scroll);
}
