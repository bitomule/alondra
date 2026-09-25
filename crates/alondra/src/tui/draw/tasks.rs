use alondra_core::tasks::{RequestState, TaskState};
use alondra_core::now_millis;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::super::tasks::{TaskKey, TaskListRow, TreeRow, activity_doing, clock, pr_progress, transition_summary};
use super::super::{App, Pane, View};
use super::{ACCENT, DEFINING, DIM, SOFT, ago, ago_phrase, centered_lines, fit, highlight, keep_visible, more_hint, panel, pr_glyph, pr_summary, scrollbar, since_phrase, state_glyph, task_glyph};
use crate::client::{Activity, ClientRequest, ClientTask, FleetNode};
use crate::i18n;
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

/// Keep the list scannable: one row per item. The full title remains in the detail pane.
fn titled(bar: &Span<'static>, lead: Vec<Span<'static>>, title: &str, style: Style, _under: &str, right: Vec<Span<'static>>, width: usize) -> Vec<Line<'static>> {
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
    vec![fit(spans, width)]
}

/// The state and age a task's row ends with.
fn state_and_age(app: &App, t: &ClientTask) -> Vec<Span<'static>> {
    let (_, color) = task_glyph(t);
    let state = match t.state {
        TaskState::WaitingDependency if app.waits_elsewhere(t) => tr!("waiting ↗", "en espera ↗").to_string(),
        TaskState::WaitingDependency => tr!("waiting", "en espera").to_string(),
        s => i18n::task_state(s).to_string(),
    };
    vec![Span::styled(state, Style::new().fg(color)), Span::styled(format!("{:>5}", ago(t.state_since)), Style::new().fg(DIM))]
}

fn title_style(t: &ClientTask) -> Style {
    Style::new().add_modifier(if t.state == TaskState::Cancelled { Modifier::CROSSED_OUT } else { Modifier::empty() })
}

/// A small activity cue on running rows; the existing Tasks timer repaints it twice a second.
fn list_glyph(t: &ClientTask) -> (&'static str, Color) {
    if t.state == TaskState::Running && t.archived_at.is_none() {
        let frames = ["◐", "◓", "◑", "◒"];
        (frames[(now_millis() / 500).rem_euclid(4) as usize], Color::Green)
    } else {
        task_glyph(t)
    }
}

/// The tree before a task's glyph: the rails of ancestors that go on, and the elbow into it.
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

/// The tree before the rest of a task's title: the rails that go on past it.
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
    let mut previous_was_section = false;
    for row in app.task_rows() {
        let key = row.key();
        let selected = key.is_some() && key == app.task_sel;
        let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
        if matches!(row, TaskListRow::Project(_) | TaskListRow::Finished(..) | TaskListRow::Archived(_)) && !lines.is_empty() && !previous_was_section {
            lines.push(Line::default());
        }
        previous_was_section = matches!(row, TaskListRow::Active(_) | TaskListRow::Finished(..) | TaskListRow::Archived(_));
        let start = lines.len();
        match row {
            TaskListRow::Active(n) => {
                lines.push(fit(vec![bar, Span::styled(tr!("IN PROGRESS", "EN CURSO"), Style::new().fg(ACCENT).bold())], width));
                if n == 0 {
                    lines.push(fit(vec![Span::raw(" "), Span::styled(tr!("Nothing open · n new task", "Nada abierto · n nueva tarea"), Style::new().fg(DIM))], width));
                }
            }
            TaskListRow::Project(name) => {
                let label = if name.eq_ignore_ascii_case("alondra") { "Alondra" } else { &name };
                lines.push(fit(vec![bar, Span::styled(format!("◆ {label}"), Style::new().fg(Color::Cyan).bold())], width));
            }
            TaskListRow::Single(id) => {
                let Some(t) = app.task(&id) else { continue };
                let (glyph, color) = list_glyph(t);
                let lead = vec![Span::raw("  "), Span::styled(format!("{glyph} "), Style::new().fg(color).bold())];
                let right = state_and_age(app, t);
                lines.extend(titled(&bar, lead, &app.single_title(t), title_style(t), "  ", right, width));
                if t.archived_at.is_none() && let Some(now) = app.task_now(t) {
                    lines.push(fit(vec![bar.clone(), Span::raw("  "), Span::styled(md::one_line(&now), Style::new().fg(SOFT))], width));
                }
            }
            TaskListRow::Request(id, done, total) => {
                let Some(r) = app.request(&id) else { continue };
                let right = vec![Span::styled(trf!("{done}/{total}", "{done}/{total}"), Style::new().fg(DIM))];
                let lead = vec![Span::raw("  "), Span::styled("▸ ", Style::new().fg(Color::Cyan))];
                lines.extend(titled(&bar, lead, &app.request_title(r), Style::new().bold(), "  ", right, width));
            }
            TaskListRow::Task(row) => {
                let Some(t) = app.task(&row.id) else { continue };
                let (glyph, color) = list_glyph(t);
                let lead = vec![Span::styled(tree_lead(&row), Style::new().fg(DIM)), Span::styled(format!("{glyph} "), Style::new().fg(color).bold())];
                let under = tree_under(&row);
                lines.extend(titled(&bar, lead, &t.title, title_style(t), &under, state_and_age(app, t), width));
                if t.archived_at.is_none() && let Some(now) = app.task_now(t) {
                    lines.push(fit(vec![bar.clone(), Span::styled(under, Style::new().fg(DIM)), Span::styled(md::one_line(&now), Style::new().fg(SOFT))], width));
                }
            }
            TaskListRow::Taskless(id) => {
                let Some(r) = app.request(&id) else { continue };
                let ((glyph, color), state) = taskless(app, r);
                let lead = vec![Span::raw("  "), Span::styled(format!("{glyph} "), Style::new().fg(color).bold())];
                let at = r.closed.as_ref().map_or(r.created_at, |c| c.at);
                let rest = vec![Span::styled(state.to_string(), Style::new().fg(color)), Span::styled(format!("{:>5}", ago(at)), Style::new().fg(DIM))];
                let right = rest;
                lines.extend(titled(&bar, lead, &app.request_title(r), Style::new(), "  ", right, width));
            }
            TaskListRow::Finished(n, last) => {
                let arrow = if app.finished_open { "▾" } else { "▸" };
                let when = ago_phrase(last);
                let right = vec![Span::styled(trf!("the last one {when}", "el último {when}"), Style::new().fg(DIM))];
                lines.extend(titled(&bar, vec![Span::styled(format!("{arrow} "), Style::new().fg(DIM))], &trf!("Finished · {n}", "Terminadas · {n}"), Style::new().bold(), "  ", right, width));
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

/// How a request with no task shows: still being defined, left without tasks, answered or
/// withdrawn.
fn taskless(app: &App, r: &ClientRequest) -> ((&'static str, Color), &'static str) {
    match &r.closed {
        Some(c) if c.withdrawn => (state_glyph(TaskState::Cancelled), tr!("withdrawn", "retirada")),
        _ if r.state == RequestState::Done => (state_glyph(TaskState::Done), tr!("answered", "respondida")),
        _ if app.left_defining(r) => (DEFINING, tr!("no tasks", "sin tareas")),
        _ => (DEFINING, i18n::request_state(r.state)),
    }
}

fn section(lines: &mut Vec<Line<'static>>, title: &str, width: usize) {
    let title = title.trim().to_uppercase();
    let rest = width.saturating_sub(str_width(&title) + 4);
    lines.push(Line::default());
    lines.push(Line::from(vec![Span::styled(" ─ ", Style::new().fg(DIM)), Span::styled(title, Style::new().fg(Color::Cyan).bold()), Span::styled(format!(" {}", "─".repeat(rest)), Style::new().fg(DIM))]));
}

fn shortcut_line(shortcut: &str) -> Line<'static> {
    let mut spans = vec![];
    for item in shortcut.trim().split(" · ") {
        if let Some((key, label)) = item.split_once(' ') {
            spans.push(super::keycap(key));
            spans.push(Span::styled(format!(" {label} "), Style::new().fg(SOFT)));
        }
    }
    Line::from(spans).right_aligned()
}

fn draw_task_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.task_zoom || app.focus == Pane::Detail;
    let width = area.width.saturating_sub(4) as usize;
    let (title, lines, shortcut) = match app.task_sel.clone() {
        Some(TaskKey::Task(id)) => match app.task(&id) {
            Some(t) => {
                let (glyph, color) = task_glyph(t);
                (Line::from(vec![Span::styled(format!(" {glyph} "), Style::new().fg(color).bold()), Span::styled(tr!("Task ", "Tarea "), Style::new().bold())]), task_lines(app, t, width), tr!(" ⏎ terminal · r message · o expand ", " ⏎ terminal · r mensaje · o ampliar "))
            }
            None => (Line::from(tr!(" Task ", " Tarea ")), vec![], ""),
        },
        Some(TaskKey::Request(id)) => match app.request(&id) {
            Some(r) => (Line::from(tr!(" Request ", " Petición ")), request_lines(app, r, width), tr!(" x withdraw ", " x retirar ")),
            None => (Line::from(tr!(" Request ", " Petición ")), vec![], ""),
        },
        Some(TaskKey::Finished) => (Line::from(tr!(" Finished ", " Terminadas ")), finished_lines(app, width), if app.archive_missing { tr!(" ⏎ show/hide ", " ⏎ mostrar/ocultar ") } else { tr!(" ⏎ show/hide · Shift+A archive ", " ⏎ mostrar/ocultar · Shift+A archivar ") }),
        Some(TaskKey::Archived) => (Line::from(tr!(" Archived ", " Archivadas ")), archived_lines(app, width), tr!(" ⏎ show/hide ", " ⏎ mostrar/ocultar ")),
        None => (Line::from(tr!(" Task ", " Tarea ")), vec![], ""),
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
    block = block.title_bottom(more_hint(app.task_detail_scroll, h, total).unwrap_or_else(|| shortcut_line(shortcut)));
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines).scroll((app.task_detail_scroll as u16, 0)), text);
    scrollbar(f, inner, total, app.task_detail_scroll);
}

fn task_lines(app: &App, t: &ClientTask, width: usize) -> Vec<Line<'static>> {
    let (glyph, color) = task_glyph(t);
    let mut lines = vec![fit(
        vec![Span::styled(format!("{glyph} {}", i18n::task_state(t.state)), Style::new().fg(color).bold()), Span::styled(format!(" · {}", since_phrase(t.state_since)), Style::new().fg(DIM))],
        width,
    )];
    lines.extend(wrap(vec![Span::styled(md::one_line(&t.title), Style::new().bold())], width, vec![], vec![]));
    let request = app.request(&t.request);
    let mut where_: Vec<String> = app.task_project(t).into_iter().collect();
    if let Some(n) = t.assignee.as_ref().and_then(|a| app.node(&a.session_id)) {
        let name = n.display_name();
        where_.push(trf!("node {name}", "nodo {name}"));
    }
    where_.extend(app.task_prs(t).iter().map(|p| format!("PR #{}", p.pr.number)));
    if !where_.is_empty() {
        lines.extend(wrap(vec![Span::styled(where_.join(" · "), Style::new().fg(DIM))], width, vec![], vec![]));
    }
    if let Some(brief) = app.task_briefs.get(&t.id).filter(|brief| !brief.trim().is_empty()) {
        section(&mut lines, tr!(" Assignment", " Encargo"), width);
        lines.extend(md::render(&md::parse(brief), width).lines);
    }
    if let Some(now) = app.waiting_reason(t).or_else(|| app.task_now(t)) {
        section(&mut lines, tr!(" Current status", " Estado actual"), width);
        lines.extend(wrap(vec![Span::styled(md::one_line(&now), Style::new().fg(SOFT))], width, vec![Span::raw(" ")], vec![Span::raw(" ")]));
    }
    let items = app.task_items(t);
    if !items.is_empty() {
        section(&mut lines, tr!(" Waiting for you", " Te espera"), width);
        for k in items {
            if let Some(item) = app.item(k) {
                let (glyph, kind, color) = super::item_glyph(&item);
                let title = match &item {
                    super::Item::Q(q) => q.title.clone(),
                    super::Item::R(r) => r.title.clone(),
                    super::Item::P(p) => p.pr.title.clone(),
                };
                lines.push(fit(vec![Span::styled(format!(" {glyph} {kind} "), Style::new().fg(color).bold()), Span::raw(md::one_line(&title))], width));
            }
        }
    }
    let train: Vec<(&ClientTask, &crate::client::PrCard)> = app.merge_order(t).into_iter().flat_map(|owner| app.task_prs(owner).into_iter().map(move |p| (owner, p))).collect();
    if !train.is_empty() {
        section(&mut lines, tr!(" PRs in merge order", " PRs en orden de fusión"), width);
        for (i, (owner, p)) in train.iter().enumerate() {
            let (g, c) = if owner.state == TaskState::InReview { (pr_glyph().0, pr_glyph().2) } else { task_glyph(owner) };
            let lead = vec![Span::styled(format!(" {} ", i + 1), Style::new().fg(DIM)), Span::styled(format!("{g} "), Style::new().fg(c).bold())];
            let mut spans = vec![Span::raw(format!("#{} {}", p.pr.number, md::one_line(&p.pr.title)))];
            if owner.id == t.id {
                spans.push(Span::styled(tr!("  this task", "  esta tarea"), Style::new().fg(ACCENT)));
            } else if let Some(below) = p.below_number {
                spans.push(Span::styled(trf!("  on top of #{below}", "  va encima del #{below}"), Style::new().fg(DIM)));
            }
            lines.extend(wrap(spans, width, lead, vec![Span::raw("     ")]));
            lines.extend(wrap(vec![Span::styled(pr_summary(p), Style::new().fg(DIM))], width, vec![Span::raw("     ")], vec![Span::raw("     ")]));
            lines.push(fit(vec![Span::raw("     "), Span::styled(pr_progress(p, owner.auto_merge_pending), Style::new().fg(SOFT))], width));
        }
    }
    let (deps, dependents) = (app.dependencies(t), app.dependents(t));
    if !deps.is_empty() || !dependents.is_empty() {
        section(&mut lines, tr!(" Dependencies", " Dependencias"), width);
        for (label, tasks) in [(tr!("depends on", "depende de"), deps), (tr!("waiting on it", "la esperan"), dependents)] {
            if tasks.is_empty() {
                lines.push(fit(vec![Span::styled(format!(" {label} "), Style::new().fg(DIM)), Span::styled(tr!("none", "ninguna"), Style::new().fg(DIM))], width));
            }
            for (i, other) in tasks.iter().enumerate() {
                let (g, c) = task_glyph(other);
                let lead = if i == 0 { format!(" {label} ") } else { " ".repeat(str_width(label) + 2) };
                lines.push(fit(
                    vec![Span::styled(lead, Style::new().fg(DIM)), Span::styled(format!("{g} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&other.title)), Span::styled(format!(" · {}", i18n::task_state(other.state)), Style::new().fg(c))],
                    width,
                ));
            }
        }
    }
    let documents: Vec<&alondra_core::Report> = t.documents.iter().filter_map(|id| app.report_by_id(*id)).collect();
    if !documents.is_empty() {
        section(&mut lines, tr!(" Documents", " Documentos"), width);
        for d in documents {
            let read = if app.data.news.iter().any(|n| n.id == d.id) { tr!("unread", "sin leer") } else { tr!("read", "leído") };
            lines.push(fit(vec![Span::styled(" ▤ ", Style::new().fg(Color::Cyan).bold()), Span::raw(md::one_line(&d.title)), Span::styled(format!(" · {read} · {}", ago_phrase(d.created_at)), Style::new().fg(DIM))], width));
        }
    }
    if let Some((_, history)) = app.histories.get(&t.id).filter(|(_, h)| !h.is_empty()) {
        section(&mut lines, tr!(" History", " Historia"), width);
        for step in history {
            let (g, c) = state_glyph(step.to);
            let label = format!("{g} {:<14}", i18n::task_state(step.to));
            lines.extend(wrap(
                vec![Span::styled(label, Style::new().fg(c)), Span::raw(transition_summary(step))],
                width,
                vec![Span::styled(format!(" {} ", clock(step.at)), Style::new().fg(DIM))],
                vec![Span::raw("       ")],
            ));
        }
    }
    if let Some(text) = request.and_then(|r| r.text.as_deref()).filter(|t| !t.trim().is_empty()) {
        section(&mut lines, tr!(" Original request", " Petición original"), width);
        lines.extend(md::render(&md::parse(text), width).lines);
    }
    section(&mut lines, tr!(" Options", " Opciones"), width);
    lines.push(fit(vec![Span::styled(format!("  {:<12}", tr!("PR handling", "Gestión PR")), Style::new().fg(DIM)), Span::styled(super::super::work::pr_handling(t.options.auto_merge), Style::new().fg(SOFT))], width));
    if let Some(effort) = &t.options.effort {
        lines.push(fit(vec![Span::styled(format!("  {:<12}", tr!("Effort", "Esfuerzo")), Style::new().fg(DIM)), Span::styled(effort.clone(), Style::new().fg(SOFT))], width));
    }
    lines
}

fn finished_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![Line::from(Span::styled(tr!("✓ Completed work", "✓ Trabajo terminado"), Style::new().fg(Color::LightGreen).bold()))];
    section(&mut lines, tr!(" Actions", " Acciones"), width);
    lines.push(Line::from(vec![super::keycap("⏎"), Span::raw(tr!(" show or hide", " mostrar u ocultar"))]));
    if !app.archive_missing {
        lines.push(Line::from(vec![super::keycap("Shift+A"), Span::raw(tr!(" archive finished work", " archivar lo terminado"))]));
    }
    lines
}

fn archived_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let n = app.archived_count();
    let mut lines = vec![Line::from(Span::styled(trf!("✓ {n} archived", "✓ {n} archivadas"), Style::new().fg(Color::LightGreen).bold()))];
    section(&mut lines, tr!(" Actions", " Acciones"), width);
    lines.push(Line::from(vec![super::keycap("⏎"), Span::raw(tr!(" show or hide", " mostrar u ocultar"))]));
    lines
}

fn request_lines(app: &App, r: &ClientRequest, width: usize) -> Vec<Line<'static>> {
    let ((glyph, color), state) = taskless(app, r);
    let mut lines = vec![fit(vec![Span::styled(format!("{glyph} {state}"), Style::new().fg(color).bold()), Span::styled(format!(" · {}", since_phrase(r.created_at)), Style::new().fg(DIM))], width)];
    lines.extend(wrap(vec![Span::styled(app.request_title(r), Style::new().bold())], width, vec![], vec![]));
    if let Some(p) = app.request_project(r) {
        lines.push(Line::from(Span::styled(p, Style::new().fg(DIM))));
    }
    lines.push(Line::default());
    let now = match &r.closed {
        Some(c) if c.withdrawn => tr!("You withdrew it: it ended without tasks.", "La retiraste: terminó sin tareas.").to_string(),
        Some(c) => match c.answer.as_deref().map(md::one_line).filter(|a| !a.is_empty()) {
            Some(answer) => trf!("The orchestrator answered it: {answer}", "El orquestador la respondió: {answer}"),
            None => tr!("It finished without tasks: the orchestrator answered it.", "Terminó sin tareas: el orquestador la respondió.").to_string(),
        },
        None if app.left_defining(r) => tr!("The orchestrator left it without tasks and did not close it: x withdraws it.", "El orquestador la dejó sin tareas y sin cerrar: x la retira.").to_string(),
        None => tr!("The orchestrator is splitting it into tasks.", "El orquestador la está repartiendo en tareas.").to_string(),
    };
    lines.extend(wrap(vec![Span::raw(now)], width, vec![Span::styled(tr!("Now: ", "Ahora: "), Style::new().fg(DIM))], vec![Span::raw("  ")]));
    if let Some(text) = r.text.clone().filter(|t| !t.trim().is_empty()) {
        section(&mut lines, tr!(" Original request", " Petición original"), width);
        lines.extend(md::render(&md::parse(&text), width).lines);
    }
    lines
}

/// "principal  Bash bazel build //ios:App · 1m": one thread of a node and what it does.
fn activity_line(a: &Activity, indent: &str, width: usize, bar: Span<'static>) -> Line<'static> {
    let mut spans = vec![bar, Span::raw(indent.to_string()), Span::styled(format!("{:<11}", truncate(&a.thread(), 10)), Style::new().fg(SOFT))];
    match (&a.tool, &a.summary) {
        (Some(tool), summary) => {
            spans.push(Span::styled(format!("{tool} "), Style::new().fg(ACCENT)));
            spans.push(Span::raw(md::one_line(summary.as_deref().unwrap_or_default())));
        }
        (None, _) => spans.push(Span::raw(activity_doing(a).map(|d| md::one_line(&d)).unwrap_or_default())),
    }
    spans.push(Span::styled(format!(" · {}", ago(a.since)), Style::new().fg(DIM)));
    fit(spans, width)
}

/// A node's tasks under its row in the fleet, each with its state.
pub fn node_task_lines(app: &App, n: &FleetNode, width: usize, selected: bool) -> Vec<Line<'static>> {
    if n.is_orchestrator {
        return vec![];
    }
    let bar = || if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let mut lines = vec![];
    for t in app.tasks_on(&n.session_id) {
        let (g, c) = task_glyph(t);
        lines.push(fit(
            vec![
                bar(),
                Span::raw("    "),
                Span::styled(format!("{g} "), Style::new().fg(c).bold()),
                Span::raw(md::one_line(&t.fleet_title(n))),
                Span::styled(format!(" · {}", i18n::task_state(t.state)), Style::new().fg(c)),
            ],
            width,
        ));
    }
    lines
}

/// The node detail's tasks and, per thread, what it does now.
pub fn node_detail_lines(app: &App, n: &FleetNode, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![];
    let tasks = if n.is_orchestrator { vec![] } else { app.tasks_on(&n.session_id) };
    if !tasks.is_empty() {
        section(&mut lines, tr!(" Tasks", " Tareas"), width);
        for t in tasks {
            let (g, c) = task_glyph(t);
            lines.push(fit(
                vec![
                    Span::styled(format!(" {g} "), Style::new().fg(c).bold()),
                    Span::raw(md::one_line(&t.fleet_title(n))),
                    Span::styled(format!(" · {}", i18n::task_state(t.state)), Style::new().fg(c)),
                    Span::styled(format!(" · {}", ago(t.state_since)), Style::new().fg(DIM)),
                ],
                width,
            ));
        }
    }
    let threads = app.node_activity(n);
    if !threads.is_empty() {
        section(&mut lines, tr!(" Now", " Ahora"), width);
        for a in &threads {
            lines.push(activity_line(a, "", width, Span::raw("")));
            if let Some(task) = a.task.as_deref().and_then(|id| app.task(id)).filter(|_| !a.is_main()) {
                let title = md::one_line(&task.title);
                lines.push(fit(vec![Span::raw(" ".repeat(11)), Span::styled(trf!("subagent of “{title}”", "subagente de «{title}»"), Style::new().fg(DIM))], width));
            }
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use alondra_core::now_millis;
    use crossterm::event::KeyCode;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use serde_json::json;

    use super::super::super::tasks::{TaskKey, TaskListRow};
    use super::super::super::tests::{alive_node, app_with, code, pr_card, press, question, report};
    use super::super::super::{App, Screen};
    use crate::i18n::{Lang, set_for_this_thread};

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| super::super::draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// The board of the mockups: a PR in review, a task stacked after it, one running in
    /// parallel on another node with two threads, and a request still being defined.
    fn board(app: &mut App) {
        let now = now_millis();
        app.loaded = true;
        app.data.requests = serde_json::from_value(json!([
            {"id": "r-1", "text": "Strike-through phase 2\nwith the album and Siri", "created_at": now - 7_200_000, "state": "in_progress", "tasks": ["t-1", "t-2", "t-3"]},
            {"id": "r-2", "text": "Import lists from Notes", "created_at": now - 180_000, "state": "defining", "tasks": []},
        ]))
        .unwrap();
        let task = |id: &str, title: &str, state: &str, deps: &[&str], session: &str, created: i64| {
            json!({"id": id, "request": "r-1", "title": title, "depends_on": deps, "assignee": {"session_id": session}, "prs": if id == "t-1" { vec![4] } else { vec![] },
                   "blocker": if id == "t-1" { json!({"kind": "question", "question": 7, "since": now}) } else { json!(null) },
                   "state": state, "state_since": now - 240_000, "created_at": now - created})
        };
        app.data.tasks = serde_json::from_value(json!([
            task("t-1", "Editor covers readable text", "in_review", &[], "s1", 6_000_000),
            task("t-2", "Album and Siri", "waiting_dependency", &["t-1"], "s1", 5_000_000),
            task("t-3", "Measure on the iPhone", "running", &[], "s2", 4_000_000),
        ]))
        .unwrap();
        let mut free = alive_node("s1");
        free.node.state = alondra_core::NodeState::WaitingMerge;
        free.project = Some("App".into());
        let mut busy = alive_node("s2");
        busy.node.state = alondra_core::NodeState::Working;
        busy.project = Some("App".into());
        app.data.nodes = vec![free, busy];
        let mut pr = pr_card(None);
        pr.pr.number = 150;
        pr.pr.title = "Editor covers readable text".into();
        app.data.prs = vec![pr];
        app.reconcile();
    }

    #[test]
    fn tab_goes_from_the_inbox_to_tasks_to_the_fleet_and_shift_tab_comes_back() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        let mut seen = vec![];
        for _ in 0..3 {
            code(&mut app, KeyCode::Tab);
            seen.push(app.screen);
        }
        assert!(seen == [Screen::Tasks, Screen::Fleet, Screen::Inbox]);
        code(&mut app, KeyCode::BackTab);
        assert!(app.screen == Screen::Fleet);
    }

    #[test]
    fn the_tasks_tab_draws_each_request_with_its_tree_and_the_selected_task() {
        set_for_this_thread(Lang::Es);
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        let s = screen(&mut app, 130, 33);
        for text in [
            "Tareas 3",
            "EN CURSO",
            "◆ App",
            "⇄ Editor covers readable text",
            "└◌ Album and Siri",
            "en espera",
            "en revisión",
            "PR #150 · listo para fusionar",
            "◇ Preparando petición",
            "definiendo",
            "PRS EN ORDEN DE FUSIÓN",
            "#150 Editor covers readable text  esta",
            "x descartar",
            "s programadas",
        ] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
    }

    #[test]
    fn the_fleet_lists_each_node_with_its_tasks_states_and_only_the_opened_node_says_what_each_thread_does() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        board(&mut app);
        press(&mut app, "f");
        let s = screen(&mut app, 130, 33);
        for text in ["libre", "ocupado", "⇄ Editor covers readable text · en revisión", "◌ Album and Siri · esperando a otra", "Measure on the iPhone · en marcha"] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
        press(&mut app, "j");
        let now = now_millis();
        let threads = serde_json::from_value(json!([
            {"agent_id": null, "task": "t-3", "tool": "Bash", "summary": "bazel build //ios:App", "since": now - 60_000},
            {"agent_id": "a1", "agent_type": "tester", "task": "t-3", "tool": "mav", "summary": "tap Save", "since": now - 20_000},
        ]))
        .unwrap();
        app.on_msg(super::super::super::Msg::NodeThreads { session: "s2".into(), threads: Some(threads) });
        let s = screen(&mut app, 130, 33);
        for text in ["principal  Bash bazel build //ios:App", "tester     mav tap Save"] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
        assert_eq!(s.matches("bazel build").count(), 1, "in the opened node's detail, not under its row:\n{s}");
        code(&mut app, KeyCode::BackTab);
        assert!(!screen(&mut app, 130, 33).contains("bazel build"), "a running task shows its state, not what its node does");
    }

    #[test]
    fn cancel_takes_a_second_press_and_a_404_turns_task_actions_off() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        press(&mut app, "x");
        assert!(app.posted.borrow().is_empty(), "the first press only asks");
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("Press x again")));
        press(&mut app, "x");
        assert_eq!(app.posted.borrow().last().map(|(paths, _)| paths.clone()), Some(vec!["/v1/tasks/t-1/cancel".to_string()]));
        app.task_action_done(Err(("not found".into(), true)));
        let s = screen(&mut app, 130, 33);
        assert!(!s.contains("x cancel"), "{s}");
        press(&mut app, "R");
        assert_eq!(app.posted.borrow().len(), 1, "nothing more is sent once alondrad said it has no route");
    }

    /// Round 2: a request the orchestrator never defined nor closed stayed defining for good,
    /// with nothing the user could do about it.
    #[test]
    fn a_request_left_defining_is_withdrawn_with_a_second_press() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::En);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        let at = app.task_keys().iter().position(|k| *k == super::super::super::tasks::TaskKey::Request("r-2".into())).expect("the defining request is listed");
        app.move_tasks(at as isize);
        let shown = screen(&mut app, 130, 33);
        assert!(shown.contains("x withdraw") && shown.contains("Import lists from Notes"), "the original message remains in the detail: {shown}");
        assert!(!list_of(&shown).contains("Import lists from Notes"), "the list uses a short label: {shown}");
        press(&mut app, "x");
        assert!(app.posted.borrow().is_empty(), "the first press only asks");
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("Press x again to withdraw “Preparing request”")), "{:?}", app.flash.as_ref().map(|f| &f.text));
        press(&mut app, "x");
        assert_eq!(app.posted.borrow().last().map(|(paths, ok)| (paths.clone(), ok.clone())), Some((vec!["/v1/requests/r-2/close".to_string()], "Withdrawn: Preparing request".to_string())));
    }

    #[test]
    fn a_card_names_its_task_under_the_node() {
        let (mut app, _rx) = app_with(vec![question(7, alondra_core::QuestionOrigin::Ask, &["Yes", "No"])]);
        board(&mut app);
        let s = screen(&mut app, 130, 33);
        assert!(s.contains("Task: Editor covers readable text"), "{s}");
    }

    #[test]
    fn an_overview_decodes_with_and_without_tasks_and_a_task_state_it_does_not_know_costs_only_that_task() {
        let base = json!({"tick": 1, "nodes": [], "open_questions": [], "machine": serde_json::to_value(alondra_core::Machine::default()).unwrap()});
        let old: crate::client::Overview = serde_json::from_value(base.clone()).unwrap();
        assert!(old.requests.is_empty() && old.tasks.is_empty());
        let mut new = base;
        new["tasks"] = json!([
            {"id": "t-1", "request": "r", "title": "Known", "state": "running", "state_since": 1, "created_at": 1, "auto_merge_pending": true},
            {"id": "t-2", "request": "r", "title": "From the future", "state": "sleeping", "state_since": 1, "created_at": 1},
        ]);
        new["requests"] = json!([{"id": "r", "created_at": 1, "state": "in_progress", "tasks": ["t-1", "t-2"]}]);
        let decoded: crate::client::Overview = serde_json::from_value(new).unwrap();
        assert_eq!(decoded.tasks.iter().map(|t| (t.id.as_str(), t.auto_merge_pending)).collect::<Vec<_>>(), [("t-1", true)]);
        assert_eq!(decoded.requests[0].tasks, ["t-1", "t-2"]);
    }

    /// The board plus finished work: r-3's task is done and r-4 finished without tasks.
    fn finished(app: &mut App, archived_at: Option<i64>) {
        let now = now_millis();
        app.data.requests.extend(serde_json::from_value::<Vec<crate::client::ClientRequest>>(json!([
            {"id": "r-3", "text": "Old work", "created_at": now - 9_000_000, "state": "done", "tasks": ["t-4"], "archived_at": archived_at},
            {"id": "r-4", "text": "How is it going?", "created_at": now - 8_000_000, "state": "done", "tasks": [], "archived_at": archived_at},
        ]))
        .unwrap());
        app.data.tasks.push(
            serde_json::from_value(json!({"id": "t-4", "request": "r-3", "title": "Shipped fix", "state": "done", "state_since": now - 60_000, "created_at": now - 9_000_000, "archived_at": archived_at}))
                .unwrap(),
        );
        app.reconcile();
    }

    #[test]
    fn shift_a_archives_what_is_finished_and_a_404_hides_it() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        press(&mut app, "A");
        assert!(app.posted.borrow().is_empty(), "nothing finished yet");
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("Nothing finished")));

        finished(&mut app, None);
        app.move_tasks(app.task_keys().iter().position(|k| *k == super::super::super::tasks::TaskKey::Finished).expect("the finished header") as isize);
        code(&mut app, KeyCode::Enter);
        let s = screen(&mut app, 150, 40);
        for text in ["▾ Finished · 2", "Shipped fix", "Orchestrator reply", "answered", "Shift+A archive finished"] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
        assert!(!s.contains("Archived ("), "nothing archived yet:\n{s}");
        press(&mut app, "A");
        assert_eq!(app.posted.borrow().last().cloned(), Some((vec!["/v1/tasks/archive".to_string()], "Archived: 2".to_string())));

        app.archive_done(Err(("not found".into(), true)));
        assert!(!screen(&mut app, 150, 40).contains("Shift+A"));
        press(&mut app, "A");
        assert_eq!(app.posted.borrow().len(), 1, "nothing more is sent once alondrad said it has no route");
    }

    #[test]
    fn archived_work_leaves_the_list_for_a_closed_section_that_enter_opens() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        board(&mut app);
        finished(&mut app, Some(now_millis()));
        code(&mut app, KeyCode::Tab);
        let s = screen(&mut app, 150, 40);
        assert!(s.contains("▸ Archivadas (2)"), "{s}");
        for gone in ["Old work", "Shipped fix", "How is it going?"] {
            assert!(!s.contains(gone), "{gone} is archived:\n{s}");
        }
        press(&mut app, "G");
        assert!(app.task_sel == Some(super::super::super::tasks::TaskKey::Archived));
        code(&mut app, KeyCode::Enter);
        let s = screen(&mut app, 150, 40);
        for shown in ["▾ Archivadas (2)", "Shipped fix", "Respuesta del orquestador", "respondida"] {
            assert!(s.contains(shown), "{shown}:\n{s}");
        }
        assert!(app.archivable_count() == 0, "archived work is not archived again");
        press(&mut app, "o");
        assert!(!screen(&mut app, 150, 40).contains("Shipped fix"), "o closes it again");
    }

    #[test]
    fn a_tasks_history_is_asked_again_when_the_task_changes_or_the_fetch_failed() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        let (id, since) = app.selected_task().map(|t| (t.id.clone(), t.state_since)).expect("a task is selected");
        assert!(app.history_asked.contains(&(id.clone(), since)));
        app.history_loaded(id.clone(), since, Ok((vec![], vec![], None)));
        let later = since + 5;
        app.data.tasks.iter_mut().find(|t| t.id == id).unwrap().last_change = Some(later);
        app.reconcile();
        assert!(app.history_asked.contains(&(id.clone(), later)), "a move that kept its state still reaches its history");
        app.history_loaded(id.clone(), later, Err(false));
        app.reconcile();
        assert!(app.history_asked.contains(&(id.clone(), later)) && !app.history_missing, "a failed fetch is asked again");
    }

    #[test]
    fn a_document_older_than_the_overviews_reports_comes_from_its_task() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        let (id, since) = app.selected_task().map(|t| (t.id.clone(), t.state_since)).expect("a task is selected");
        assert!(app.report_by_id(9_001).is_none(), "the overview does not carry it");
        app.history_loaded(id, since, Ok((vec![], vec![report(9_001)], Some("Fix the editor without blocking scrolling.".into()))));
        assert_eq!(app.report_by_id(9_001).map(|r| r.id), Some(9_001));
        let detail = screen(&mut app, 130, 40);
        assert!(detail.contains("ASSIGNMENT") && detail.contains("Fix the editor without blocking scrolling."), "{detail}");
    }

    #[test]
    fn a_request_never_uses_the_users_message_as_its_title() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        finished(&mut app, None);
        let title = |app: &App, id: &str| app.request_title(app.request(id).unwrap());
        assert_eq!(title(&app, "r-2"), "Preparing request", "a pending request has no title yet");
        assert_eq!(title(&app, "r-3"), "Shipped fix", "its only task's title, not the prompt");
        assert_eq!(title(&app, "r-1"), "App", "a task group without a title uses its project");
        let given: crate::client::ClientRequest = serde_json::from_value(json!({"id": "r-1", "created_at": 1, "state": "in_progress", "tasks": ["t-1"], "title": "Strike-through, phase 2"})).unwrap();
        app.data.requests[0] = given;
        assert_eq!(title(&app, "r-1"), "Strike-through, phase 2", "the orchestrator's title wins");
    }

    /// Opción A's board (t-44370), from the real overview of 24 Sep: three requests open, one of
    /// them left defining, finished work and three probe sessions.
    fn mockup(app: &mut App) {
        let now = now_millis();
        let min = 60_000;
        app.loaded = true;
        app.data.requests = serde_json::from_value(json!([
            {"id": "r-view", "text": "La vista de las tareas en tui es horrenda, mira. en alondra", "created_at": now - 6 * min, "state": "in_progress", "tasks": ["t-44370"]},
            {"id": "r-redraft", "text": "Entonces lo de Redraft como ha quedado? Subimos beta al usuario y que reproduzca el bloqueo otra vez? El pr ha hecho algún fix?",
             "created_at": now - 11 * min, "state": "in_progress", "tasks": ["t-44136", "t-44137"]},
            {"id": "r-hf", "text": "En qué punto tenemos a BlurCamera? Cómo puedo probar lo último que hemos metido?", "created_at": now - 120 * min, "state": "defining", "tasks": []},
            {"id": "r-log", "text": "Vale. El usuario de Redraft nos ha mandado su export", "created_at": now - 70 * min, "state": "done", "tasks": ["t-41601"]},
            {"id": "r-mine", "text": "Prueba algo", "created_at": now - 300 * min, "state": "done", "tasks": ["t-gone"]},
            {"id": "r-rama", "created_at": now - 580 * min, "state": "done", "tasks": ["t-rama"], "synthetic": true},
            {"id": "r-shift", "created_at": now - 500 * min, "state": "done", "tasks": ["t-shift"], "synthetic": true},
            {"id": "r-inject", "created_at": now - 370 * min, "state": "done", "tasks": ["t-inject"], "synthetic": true},
            {"id": "r-rebase", "created_at": now - 570 * min, "state": "done", "tasks": ["t-rebase"], "synthetic": true},
        ]))
        .unwrap();
        let task = |id: &str, request: &str, title: &str, state: &str, ago: i64, more: serde_json::Value| {
            let mut t = json!({"id": id, "request": request, "title": title, "state": state, "state_since": now - ago * min, "created_at": now - ago * min});
            t.as_object_mut().unwrap().extend(more.as_object().unwrap().clone());
            t
        };
        let redraft = json!({"project": "Redraft"});
        app.data.tasks = serde_json::from_value(json!([
            task("t-44370", "r-view", "Alondra: rehacer la vista Tareas de la TUI para que se lea bien", "running", 5,
                 json!({"recorded": true, "assignee": {"session_id": "s1"}, "options": {"effort": "normal", "project": "alondra"}})),
            task("t-44136", "r-redraft", "Redraft: sacar las llamadas síncronas a PhotoKit de los hilos de Swift", "running", 10,
                 json!({"recorded": true, "assignee": {"session_id": "s2"}, "options": redraft})),
            task("t-44137", "r-redraft", "Redraft: beta 4.5.0 (192) con el arreglo y el registro, para el usuario atascado", "waiting_dependency", 10,
                 json!({"recorded": true, "depends_on": ["t-44136"], "options": redraft})),
            task("t-41601", "r-log", "Redraft: leer el log del usuario y encontrar dónde se atasca el análisis", "done", 41, json!({"recorded": true, "prs": [91], "options": redraft})),
            task("t-gone", "r-mine", "alondra/abandonada", "cancelled", 290, json!({})),
            task("t-rama", "r-rama", "alondra/rama-main", "done", 575, json!({"prs": [65]})),
            task("t-shift", "r-shift", "test/shift-x", "cancelled", 496, json!({})),
            task("t-inject", "r-inject", "prompt injection test", "cancelled", 364, json!({})),
            task("t-rebase", "r-rebase", "git rebase fase-1", "cancelled", 562, json!({})),
        ]))
        .unwrap();
        let node = |session: &str, name: &str, project: &str| {
            let mut n = alive_node(session);
            n.node.name = Some(name.into());
            n.node.state = alondra_core::NodeState::Working;
            n.project = Some(project.into());
            n
        };
        app.data.nodes = vec![node("s1", "alondra/estados-por-tarea", "alondra"), node("s2", "redraft/atasco-analizando", "Redraft")];
        app.reconcile();
    }

    fn keys(app: &App) -> Vec<String> {
        app.task_keys()
            .into_iter()
            .map(|k| match k {
                TaskKey::Task(id) | TaskKey::Request(id) => id,
                TaskKey::Finished => "finished".into(),
                TaskKey::Archived => "archived".into(),
            })
            .collect()
    }

    /// The list panel's columns of a 130-wide screen.
    fn list_of(s: &str) -> String {
        s.lines().map(|l| l.chars().take(78).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    fn open_finished(app: &mut App) {
        app.screen = Screen::Tasks;
        let at = app.task_keys().iter().position(|k| *k == TaskKey::Finished).expect("the finished header");
        app.move_tasks(at as isize);
        code(app, KeyCode::Enter);
    }

    #[test]
    fn the_tasks_tab_keeps_rows_compact_at_130_by_33() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        mockup(&mut app);
        code(&mut app, KeyCode::Tab);
        let s = screen(&mut app, 130, 33);
        for text in ["Tareas 3", "╭ Tareas", "╭ ● Tarea", "EN CURSO", "◆ Alondra", "◆ Redraft", "◆ Otros", "Alondra: rehacer la vista", "└◌ Redraft: beta", "Preparando petición", "Terminadas · 3", "OPCIONES", "Gestión PR  revisar cada una", "Esfuerzo    normal"] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
        let list = list_of(&s);
        let alondra = list.find("◆ Alondra").unwrap();
        let redraft = list.find("◆ Redraft").unwrap();
        let otros = list.find("◆ Otros").unwrap();
        assert!(alondra < redraft && redraft < otros, "group order:\n{list}");
        let list = list_of(&s);
        for gone in ["test/shift-x", "prompt injection", "git rebase", "tras «", "general-purpose", "t-44"] {
            assert!(!list.contains(gone), "{gone}:\n{list}");
        }
    }

    #[test]
    fn at_100_by_30_the_list_stays_compact_and_detail_keeps_the_full_title() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        mockup(&mut app);
        code(&mut app, KeyCode::Tab);
        let s = screen(&mut app, 100, 30);
        for text in ["│ EN CURSO", "╭ ● Tarea ", "◆ Alondra", "◆ Redraft", "Alondra: rehacer la vista", "para que se lea bien", "└◌ Redraft: beta 4.5.0 (192)", "en espera", "▸ Terminadas · 3", "Programadas [s]"] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
        let list: String = s.lines().map(|l| l.chars().take(60).collect::<String>()).collect::<Vec<_>>().join("\n");
        assert!(list.contains('…'), "long titles are shortened in the list:\n{list}");
    }

    #[test]
    fn probe_sessions_leave_the_list_and_the_counts_but_nothing_of_a_request_david_made_does() {
        let (mut app, _rx) = app_with(vec![]);
        mockup(&mut app);
        open_finished(&mut app);
        let listed = keys(&app);
        for probe in ["t-shift", "t-inject", "t-rebase"] {
            assert!(!listed.contains(&probe.to_string()), "{probe}: {listed:?}");
        }
        assert!(listed.contains(&"t-rama".to_string()), "a session that delivered a PR stays: {listed:?}");
        assert!(listed.contains(&"t-gone".to_string()), "a request David made keeps even a task cancelled with nothing: {listed:?}");
        assert!(matches!(app.task_rows().iter().find(|r| matches!(r, TaskListRow::Finished(..))), Some(TaskListRow::Finished(3, _))));
    }

    /// 24 Sep: alondra/estados-por-tarea took t-44370 and went on with the review its own task
    /// from before tasks were recorded carried: both are its work, and both show.
    #[test]
    fn a_node_that_holds_several_open_tasks_shows_each() {
        let (mut app, _rx) = app_with(vec![]);
        mockup(&mut app);
        let now = now_millis();
        app.data.requests.push(serde_json::from_value(json!({"id": "r-estados", "created_at": now - 12_000_000, "state": "in_progress", "tasks": ["t-s1"]})).unwrap());
        app.data.tasks.push(
            serde_json::from_value(json!({"id": "t-s1", "request": "r-estados", "title": "alondra/estados-por-tarea", "assignee": {"session_id": "s1"}, "prs": [98, 99],
                                         "state": "running", "state_since": now - 5_640_000, "created_at": now - 12_000_000}))
            .unwrap(),
        );
        assert!(keys(&app).contains(&"t-s1".to_string()));
        assert_eq!(app.open_task_count(), 4, "only open tasks count; a request being prepared is not a task");
        assert_eq!(app.tasks_on("s1").iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["t-s1", "t-44370"]);
    }

    /// t-47118: Fleet showed `t-s1` titled "alondra/estados-por-tarea" — its own node's name —
    /// right under that same name in the node's row, indistinguishable from it.
    #[test]
    fn a_tasks_fleet_row_never_repeats_its_own_nodes_name() {
        let (mut app, _rx) = app_with(vec![]);
        mockup(&mut app);
        let now = now_millis();
        app.data.requests.push(serde_json::from_value(json!({"id": "r-estados", "created_at": now - 12_000_000, "state": "in_progress", "tasks": ["t-s1"]})).unwrap());
        app.data.tasks.push(
            serde_json::from_value(json!({"id": "t-s1", "request": "r-estados", "title": "alondra/estados-por-tarea", "assignee": {"session_id": "s1"}, "prs": [98, 99],
                                         "state": "running", "state_since": now - 5_640_000, "created_at": now - 12_000_000}))
            .unwrap(),
        );
        app.screen = Screen::Fleet;
        app.reconcile();
        let s = screen(&mut app, 100, 40);
        assert_eq!(
            s.matches("alondra/estados-por-tarea").count(),
            2,
            "the node's own name, once in the list row and once in the detail's header — never again as its task's title:\n{s}"
        );
        assert_eq!(s.matches("main task").count(), 2, "the fallen-back title reads as the node's main task instead, in the list and the detail:\n{s}");
    }

    #[test]
    fn a_request_with_one_task_is_one_row_under_the_orchestrators_title_or_the_tasks() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        mockup(&mut app);
        code(&mut app, KeyCode::Tab);
        assert!(app.task_rows().iter().any(|r| matches!(r, TaskListRow::Single(id) if id == "t-44370")));
        assert!(!app.task_rows().iter().any(|r| matches!(r, TaskListRow::Request(id, ..) if id == "r-view")));
        assert_eq!(list_of(&screen(&mut app, 130, 33)).matches("Alondra: rehacer la vista").count(), 1, "one row, the prompt only in the detail");
        app.data.requests.iter_mut().find(|r| r.id == "r-view").unwrap().title = Some("Vista de Tareas legible".into());
        let list = list_of(&screen(&mut app, 130, 33));
        assert!(list.contains("Vista de Tareas legible") && !list.contains("Alondra: rehacer"), "{list}");
    }

    #[test]
    fn tasks_from_one_request_stay_with_their_own_apps() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        mockup(&mut app);
        app.data.tasks.iter_mut().find(|t| t.id == "t-44137").unwrap().task.options.project = Some("Lumo".into());
        let rows = app.task_rows();
        let project = |name: &str| rows.iter().position(|row| matches!(row, TaskListRow::Project(p) if p == name)).unwrap();
        let task = |id: &str| rows.iter().position(|row| matches!(row, TaskListRow::Single(t) if t == id) || matches!(row, TaskListRow::Task(t) if t.id == id)).unwrap();
        assert!(project("Redraft") < task("t-44136") && task("t-44136") < project("Lumo"));
        assert!(project("Lumo") < task("t-44137"));
        assert_eq!(rows.iter().filter(|row| row.key() == Some(TaskKey::Task("t-44137".into()))).count(), 1);
    }

    #[test]
    fn open_work_goes_by_its_last_change_and_finished_work_folds_under_one_line_newest_first() {
        let (mut app, _rx) = app_with(vec![]);
        mockup(&mut app);
        assert_eq!(keys(&app), ["t-44370", "t-44136", "t-44137", "r-hf", "finished"], "folded: nothing finished is listed");
        app.data.tasks.iter_mut().find(|t| t.id == "t-44137").unwrap().last_change = Some(now_millis());
        assert_eq!(keys(&app)[..3], ["t-44136", "t-44137", "t-44370"], "the request that changed last goes first");
        open_finished(&mut app);
        assert_eq!(keys(&app)[5..], ["t-41601", "t-gone", "t-rama"]);
        code(&mut app, KeyCode::Enter);
        assert!(!keys(&app).contains(&"t-41601".to_string()), "⏎ folds it again");
    }

    #[test]
    fn a_waiting_task_shows_its_elbow_and_waits_with_an_arrow_when_its_dependency_is_in_another_request() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        mockup(&mut app);
        code(&mut app, KeyCode::Tab);
        let list = list_of(&screen(&mut app, 130, 33));
        assert!(list.contains("└◌ Redraft: beta 4.5.0") && list.contains("en espera  10m"), "{list}");
        assert!(!list.contains("tras «") && !list.contains("esperando a otra"), "{list}");
        app.data.tasks.iter_mut().find(|t| t.id == "t-44137").unwrap().task.request = "r-view".into();
        app.data.requests.iter_mut().find(|r| r.id == "r-view").unwrap().tasks.push("t-44137".into());
        let list = list_of(&screen(&mut app, 130, 33));
        assert!(list.contains("en espera ↗"), "{list}");
        let at = app.task_keys().iter().position(|k| *k == TaskKey::Task("t-44137".into())).expect("the waiting task");
        app.move_tasks(at as isize);
        assert!(screen(&mut app, 130, 33).contains("depende de ● Redraft: sacar las llamadas"), "the detail names what it waits for");
    }

    #[test]
    fn a_request_the_orchestrator_left_defining_says_it_has_no_tasks() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        mockup(&mut app);
        code(&mut app, KeyCode::Tab);
        assert!(list_of(&screen(&mut app, 130, 33)).contains("sin tareas   2h"));
        app.data.requests.iter_mut().find(|r| r.id == "r-hf").unwrap().request.created_at = now_millis() - 60_000;
        let list = list_of(&screen(&mut app, 130, 33));
        assert!(list.contains("◇ Preparando petición") && list.contains("definiendo   1m") && !list.contains("En qué punto"), "{list}");
    }

    /// The Tasks tab of the overview in `ALONDRA_OVERVIEW`, at 130×33, written to `ALONDRA_SCREEN`
    /// (`make tasks-screen`).
    #[test]
    #[ignore]
    fn tasks_screen_of_an_overview() {
        let (Ok(input), Ok(out)) = (std::env::var("ALONDRA_OVERVIEW"), std::env::var("ALONDRA_SCREEN")) else { return };
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        app.data = serde_json::from_str(&std::fs::read_to_string(input).unwrap()).unwrap();
        app.loaded = true;
        app.reconcile();
        code(&mut app, KeyCode::Tab);
        std::fs::write(out, screen(&mut app, 130, 33)).unwrap();
    }



    #[test]
    fn a_task_shows_how_its_prs_are_handled_once() {
        let (mut app, _rx) = app_with(vec![]);
        board(&mut app);
        code(&mut app, KeyCode::Tab);
        press(&mut app, "j");
        let s = screen(&mut app, 130, 40);
        assert!(s.contains("PR handling review each one"), "{s}");
        assert!(!s.contains("review PR") && !s.contains("auto-merge no"), "one setting, one row:\n{s}");
    }

}
