use alondra_core::tasks::{RequestState, TaskState};
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

const STATE_W: usize = 18;

pub fn draw_tasks(f: &mut Frame, app: &mut App, area: Rect) {
    if app.task_zoom && app.task_sel.is_some() {
        draw_task_detail(f, app, area);
        return;
    }
    app.task_view.shown = area.width >= 100;
    let (list, detail) = if app.task_view.shown {
        let lw = (area.width * 11 / 20).clamp(56, 90);
        let [l, d] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        (l, Some(d))
    } else {
        (area, None)
    };
    let focused = app.focus == Pane::List || !app.task_view.shown;
    let block = panel(Line::from(tr!(" Tasks by request ", " Tareas por petición ").bold()), focused);
    let inner = block.inner(list);
    f.render_widget(block, list);
    if !app.loaded {
        centered_lines(f, inner, vec![Line::from(tr!("Connecting to alondrad…", "Conectando con alondrad…").fg(DIM))]);
    } else if app.data.requests.is_empty() {
        centered_lines(
            f,
            inner,
            vec![
                Line::from(tr!("No tasks yet", "Aún no hay tareas").fg(DIM).bold()),
                Line::from(tr!("What you ask for shows up here at once, and the orchestrator splits it into tasks.", "Lo que pidas sale aquí al momento y el orquestador lo reparte en tareas.").fg(DIM)),
                Line::default(),
                Line::from(vec![Span::styled("n", Style::new().fg(ACCENT).bold()), tr!(" order new work", " encarga trabajo nuevo").fg(DIM)]),
            ],
        );
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

/// Left and right parts of a row, the right one pushed to the edge.
fn spread(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: usize) -> Line<'static> {
    let rw: usize = right.iter().map(Span::width).sum();
    let mut left = fit(left, width.saturating_sub(rw + 1)).spans;
    let lw: usize = left.iter().map(Span::width).sum();
    left.push(Span::raw(" ".repeat(width.saturating_sub(lw + rw))));
    left.extend(right);
    fit(left, width)
}

/// The tree drawn before a task: the rails of ancestors that go on, and the elbow into it.
fn tree_prefix(row: &TreeRow) -> String {
    let mut s = String::from("  ");
    for on in &row.lines {
        s.push_str(if *on { "│ " } else { "  " });
    }
    if row.depth > 0 {
        s.push_str(if row.last { "└─" } else { "├─" });
    }
    s
}

/// The tree drawn before the line under a task: rails that go on past it.
fn under_prefix(row: &TreeRow) -> String {
    let mut s = String::from("  ");
    for on in &row.lines {
        s.push_str(if *on { "│ " } else { "  " });
    }
    if row.depth > 0 {
        s.push_str(if row.last { "  " } else { "│ " });
    }
    s.push_str(if row.has_children { "│ " } else { "  " });
    s
}

fn list_lines(app: &App, width: usize) -> (Vec<Line<'static>>, Option<(usize, usize)>) {
    let mut lines: Vec<Line<'static>> = vec![];
    let mut sel = None;
    let bar = |selected: bool| if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    for row in app.task_rows() {
        match row {
            TaskListRow::Request(id) => {
                let Some(r) = app.request(&id) else { continue };
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let meta = [app.request_project(r), Some(i18n::request_state(r.state).to_string())].into_iter().flatten().collect::<Vec<_>>().join(" · ");
                lines.push(spread(vec![Span::styled(format!(" {}", md::one_line(&app.request_title(r))), Style::new().bold())], vec![Span::styled(meta, Style::new().fg(DIM))], width));
            }
            TaskListRow::Taskless(id) => {
                let Some(r) = app.request(&id) else { continue };
                let selected = app.task_sel == Some(TaskKey::Request(id.clone()));
                let ((glyph, color), what) = taskless(r);
                let start = lines.len();
                lines.push(spread(
                    vec![bar(selected), Span::styled(format!(" {glyph} "), Style::new().fg(color).bold()), Span::raw(what)],
                    vec![Span::styled(format!("{:<w$}", i18n::request_state(r.state), w = STATE_W), Style::new().fg(color)), Span::styled(format!("{:>5}", ago(r.created_at)), Style::new().fg(DIM))],
                    width,
                ));
                if selected {
                    sel = Some((start, lines.len()));
                }
            }
            TaskListRow::Archived(n) => {
                let selected = app.task_sel == Some(TaskKey::Archived);
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let start = lines.len();
                let arrow = if app.archived_open { "▾" } else { "▸" };
                lines.push(fit(vec![bar(selected), Span::styled(format!("{arrow} "), Style::new().fg(DIM)), Span::styled(trf!("Archived ({n})", "Archivadas ({n})"), Style::new().fg(DIM).bold())], width));
                if selected {
                    sel = Some((start, lines.len()));
                }
            }
            TaskListRow::Task(row) => {
                let Some(t) = app.task(&row.id) else { continue };
                let selected = app.task_sel == Some(TaskKey::Task(row.id.clone()));
                let (glyph, color) = task_glyph(t);
                let start = lines.len();
                let title = Style::new().add_modifier(if t.state == TaskState::Cancelled { Modifier::CROSSED_OUT } else { Modifier::empty() });
                lines.push(spread(
                    vec![
                        bar(selected),
                        Span::styled(tree_prefix(&row), Style::new().fg(DIM)),
                        Span::styled(format!("{glyph} "), Style::new().fg(color).bold()),
                        Span::styled(md::one_line(&t.title), title),
                    ],
                    vec![Span::styled(format!("{:<w$}", truncate(i18n::task_state(t.state), STATE_W), w = STATE_W), Style::new().fg(color)), Span::styled(format!("{:>5}", ago(t.state_since)), Style::new().fg(DIM))],
                    width,
                ));
                if let Some(now) = app.task_now(t) {
                    lines.push(fit(vec![bar(selected), Span::styled(under_prefix(&row), Style::new().fg(DIM)), Span::styled(format!("  {}", md::one_line(&now)), Style::new().fg(SOFT))], width));
                }
                if selected {
                    sel = Some((start, lines.len()));
                }
            }
        }
    }
    (lines, sel)
}

/// How a request with no task shows: still being defined, or finished without tasks.
fn taskless(r: &ClientRequest) -> ((&'static str, Color), &'static str) {
    if r.state == RequestState::Done {
        (state_glyph(TaskState::Done), tr!("finished without tasks", "terminada sin tareas"))
    } else {
        (DEFINING, tr!("the orchestrator is splitting it into tasks", "el orquestador la reparte en tareas"))
    }
}

fn section(lines: &mut Vec<Line<'static>>, title: &str) {
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(title.to_string(), Style::new().fg(DIM).bold())));
}

fn draw_task_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.task_zoom || app.focus == Pane::Detail;
    let (title, lines) = match app.task_sel.clone() {
        Some(TaskKey::Task(id)) => match app.task(&id) {
            Some(t) => (t.title.clone(), task_lines(app, t, area.width.saturating_sub(4) as usize)),
            None => (String::new(), vec![]),
        },
        Some(TaskKey::Request(id)) => match app.request(&id) {
            Some(r) => (app.request_title(r), request_lines(app, r, area.width.saturating_sub(4) as usize)),
            None => (String::new(), vec![]),
        },
        Some(TaskKey::Archived) => (tr!("Archived", "Archivadas").to_string(), archived_lines(app, area.width.saturating_sub(4) as usize)),
        None => (String::new(), vec![]),
    };
    if lines.is_empty() {
        app.task_view.total = 0;
        f.render_widget(panel(Line::from(tr!(" Task ", " Tarea ").fg(DIM)), false), area);
        return;
    }
    let mut block = panel(Line::from(vec![Span::raw(" "), Span::styled(truncate(&md::one_line(&title), area.width.saturating_sub(6) as usize), Style::new().bold()), Span::raw(" ")]), focused);
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

fn task_lines(app: &App, t: &ClientTask, width: usize) -> Vec<Line<'static>> {
    let (glyph, color) = task_glyph(t);
    let mut lines = vec![fit(
        vec![Span::styled(format!("{glyph} {}", i18n::task_state(t.state)), Style::new().fg(color).bold()), Span::styled(format!(" · {}", since_phrase(t.state_since)), Style::new().fg(DIM))],
        width,
    )];
    let request = app.request(&t.request);
    let origin = [app.task_project(t), request.map(|r| app.request_title(r))].into_iter().flatten().collect::<Vec<_>>().join(" · ");
    if !origin.is_empty() {
        lines.extend(wrap(vec![Span::styled(origin, Style::new().fg(DIM))], width, vec![], vec![]));
    }
    let node = t.assignee.as_ref().and_then(|a| app.node(&a.session_id));
    let prs = app.task_prs(t);
    let mut where_ = vec![];
    if let Some(n) = node {
        let name = n.display_name();
        where_.push(trf!("node {name}", "nodo {name}"));
    }
    where_.extend(prs.iter().map(|p| format!("PR #{}", p.pr.number)));
    if !where_.is_empty() {
        lines.push(fit(vec![Span::styled(where_.join(" · "), Style::new().fg(DIM))], width));
    }
    if let Some(now) = app.waiting_reason(t).or_else(|| app.task_now(t)) {
        lines.push(Line::default());
        lines.extend(wrap(vec![Span::raw(md::one_line(&now))], width, vec![Span::styled(tr!("Now: ", "Ahora: "), Style::new().fg(DIM))], vec![Span::raw("  ")]));
    }
    let items = app.task_items(t);
    if !items.is_empty() {
        section(&mut lines, tr!(" Waiting for you", " Te espera"));
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
    if let Some(text) = request.and_then(|r| r.text.clone()).filter(|t| !t.trim().is_empty()) {
        section(&mut lines, tr!(" Request", " Petición"));
        lines.extend(md::render(&md::parse(&text), width).lines);
    }
    let train: Vec<(&ClientTask, &crate::client::PrCard)> = app.merge_order(t).into_iter().flat_map(|owner| app.task_prs(owner).into_iter().map(move |p| (owner, p))).collect();
    if !train.is_empty() {
        section(&mut lines, tr!(" PRs in merge order", " PRs en orden de fusión"));
        for (i, (owner, p)) in train.iter().enumerate() {
            let (g, c) = if owner.state == TaskState::InReview { (pr_glyph().0, pr_glyph().2) } else { task_glyph(owner) };
            let mut spans = vec![Span::styled(format!(" {} ", i + 1), Style::new().fg(DIM)), Span::styled(format!("{g} "), Style::new().fg(c).bold()), Span::raw(format!("#{} {}", p.pr.number, md::one_line(&p.pr.title)))];
            if owner.id == t.id {
                spans.push(Span::styled(tr!("  this task", "  esta tarea"), Style::new().fg(ACCENT)));
            } else if let Some(below) = p.below_number {
                spans.push(Span::styled(trf!("  on top of #{below}", "  va encima del #{below}"), Style::new().fg(DIM)));
            }
            lines.push(fit(spans, width));
            lines.extend(wrap(vec![Span::styled(pr_summary(p), Style::new().fg(DIM))], width, vec![Span::raw("     ")], vec![Span::raw("     ")]));
            lines.push(fit(vec![Span::raw("     "), Span::styled(pr_progress(p, owner.auto_merge_pending), Style::new().fg(SOFT))], width));
        }
    }
    let (deps, dependents) = (app.dependencies(t), app.dependents(t));
    if !deps.is_empty() || !dependents.is_empty() {
        section(&mut lines, tr!(" Dependencies", " Dependencias"));
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
        section(&mut lines, tr!(" Documents", " Documentos"));
        for d in documents {
            let read = if app.data.news.iter().any(|n| n.id == d.id) { tr!("unread", "sin leer") } else { tr!("read", "leído") };
            lines.push(fit(vec![Span::styled(" ▤ ", Style::new().fg(Color::Cyan).bold()), Span::raw(md::one_line(&d.title)), Span::styled(format!(" · {read} · {}", ago_phrase(d.created_at)), Style::new().fg(DIM))], width));
        }
    }
    section(&mut lines, tr!(" Options", " Opciones"));
    let yes_no = |b: bool| if b { tr!("yes", "sí") } else { tr!("no", "no") };
    let mut options = vec![
        Span::styled(tr!(" auto-merge ", " auto-merge "), Style::new().fg(DIM)),
        Span::raw(yes_no(t.options.auto_merge)),
        Span::styled(tr!(" · review PR ", " · revisar PR "), Style::new().fg(DIM)),
        Span::raw(yes_no(!t.options.auto_merge)),
    ];
    if let Some(effort) = &t.options.effort {
        options.push(Span::styled(tr!(" · effort ", " · esfuerzo "), Style::new().fg(DIM)));
        options.push(Span::raw(effort.clone()));
    }
    lines.push(fit(options, width));
    if let Some((_, history)) = app.histories.get(&t.id).filter(|(_, h)| !h.is_empty()) {
        section(&mut lines, tr!(" History", " Historia"));
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
    lines
}

fn archived_lines(app: &App, width: usize) -> Vec<Line<'static>> {
    let n = app.archived_count();
    let mut lines = vec![Line::from(Span::styled(trf!("{n} archived", "{n} archivadas"), Style::new().bold())), Line::default()];
    lines.extend(wrap(
        vec![Span::raw(tr!(
            "Finished work you archived: it left the list above. ⏎ shows or hides it here. Alondra deletes it once the days in archived_tasks_days (30 unless changed) are over.",
            "Trabajo terminado que archivaste: salió de la lista de arriba. ⏎ lo muestra u oculta aquí. Alondra lo borra cuando pasan los días de archived_tasks_days (30 si no lo cambias)."
        ))],
        width,
        vec![],
        vec![],
    ));
    lines
}

fn request_lines(app: &App, r: &ClientRequest, width: usize) -> Vec<Line<'static>> {
    let ((glyph, color), _) = taskless(r);
    let mut lines = vec![fit(
        vec![Span::styled(format!("{glyph} {}", i18n::request_state(r.state)), Style::new().fg(color).bold()), Span::styled(format!(" · {}", since_phrase(r.created_at)), Style::new().fg(DIM))],
        width,
    )];
    if let Some(p) = app.request_project(r) {
        lines.push(Line::from(Span::styled(p, Style::new().fg(DIM))));
    }
    lines.push(Line::default());
    let now = if r.state == RequestState::Done {
        tr!("It finished without tasks: the orchestrator answered it.", "Terminó sin tareas: el orquestador la respondió.")
    } else {
        tr!("The orchestrator is splitting it into tasks.", "El orquestador la está repartiendo en tareas.")
    };
    lines.extend(wrap(vec![Span::raw(now)], width, vec![Span::styled(tr!("Now: ", "Ahora: "), Style::new().fg(DIM))], vec![Span::raw("  ")]));
    if let Some(text) = r.text.clone().filter(|t| !t.trim().is_empty()) {
        section(&mut lines, tr!(" Request", " Petición"));
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

/// A node's tasks under its row in the fleet and, for those running, what each thread does.
pub fn node_task_lines(app: &App, n: &FleetNode, width: usize, selected: bool) -> Vec<Line<'static>> {
    if n.is_orchestrator {
        return vec![];
    }
    let bar = || if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let mut lines = vec![];
    for t in app.tasks_on(&n.session_id) {
        let (g, c) = task_glyph(t);
        lines.push(fit(
            vec![bar(), Span::raw("    "), Span::styled(format!("{g} "), Style::new().fg(c).bold()), Span::raw(md::one_line(&t.title)), Span::styled(format!(" · {}", i18n::task_state(t.state)), Style::new().fg(c))],
            width,
        ));
        if t.state == TaskState::Running {
            for a in app.task_activity(t) {
                lines.push(activity_line(&a, "      ", width, bar()));
            }
        }
    }
    lines
}

/// The node detail's tasks and, per thread, what it does now.
pub fn node_detail_lines(app: &App, n: &FleetNode, width: usize) -> Vec<Line<'static>> {
    let mut lines = vec![];
    let tasks = if n.is_orchestrator { vec![] } else { app.tasks_on(&n.session_id) };
    if !tasks.is_empty() {
        section(&mut lines, tr!(" Tasks", " Tareas"));
        for t in tasks {
            let (g, c) = task_glyph(t);
            lines.push(fit(
                vec![
                    Span::styled(format!(" {g} "), Style::new().fg(c).bold()),
                    Span::raw(md::one_line(&t.title)),
                    Span::styled(format!(" · {}", i18n::task_state(t.state)), Style::new().fg(c)),
                    Span::styled(format!(" · {}", ago(t.state_since)), Style::new().fg(DIM)),
                ],
                width,
            ));
        }
    }
    let threads = app.node_activity(n);
    if !threads.is_empty() {
        section(&mut lines, tr!(" Now", " Ahora"));
        for a in &threads {
            lines.push(activity_line(a, "", width, Span::raw("")));
            if let Some(task) = a.task.as_deref().and_then(|id| app.task(id)).filter(|_| !a.is_main()) {
                let title = md::one_line(&task.title);
                lines.push(fit(vec![Span::raw(" ".repeat(11)), Span::styled(trf!("subagent of «{title}»", "subagente de «{title}»"), Style::new().fg(DIM))], width));
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

    use super::super::super::tests::{alive_node, app_with, code, pr_card, press, question};
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
        busy.activity = serde_json::from_value(json!([
            {"agent_id": null, "task": "t-3", "tool": "Bash", "summary": "bazel build //ios:App", "since": now - 60_000},
            {"agent_id": "a1", "agent_type": "tester", "task": "t-3", "tool": "mav", "summary": "tap Save", "since": now - 20_000},
        ]))
        .unwrap();
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
            "Tareas 4",
            "Tareas por petición",
            "Strike-through phase 2",
            "⇄ Editor covers readable text",
            "└─◌ Album and Siri",
            "esperando a otra",
            "en revisión",
            "PR #150 · listo para fusionar",
            "el orquestador la reparte en tareas",
            "definiendo",
            "PRs en orden de fusión",
            "esta tarea",
            "x descartar",
            "Shift+R reabrir",
        ] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
    }

    #[test]
    fn the_fleet_lists_each_node_free_or_busy_with_its_tasks_and_what_each_thread_does() {
        let (mut app, _rx) = app_with(vec![]);
        set_for_this_thread(Lang::Es);
        board(&mut app);
        press(&mut app, "f");
        let s = screen(&mut app, 130, 33);
        for text in ["libre", "ocupado", "⇄ Editor covers readable text · en revisión", "◌ Album and Siri · esperando a otra", "principal  Bash bazel build //ios:App", "tester     mav tap Save"] {
            assert!(s.contains(text), "{text}:\n{s}");
        }
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
        let s = screen(&mut app, 150, 40);
        for text in ["Shipped fix", "How is it going?", "finished without tasks", "Shift+A archive finished"] {
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
        for shown in ["▾ Archivadas (2)", "Old work", "Shipped fix", "How is it going?", "terminada sin tareas"] {
            assert!(s.contains(shown), "{shown}:\n{s}");
        }
        assert!(app.archivable_count() == 0, "archived work is not archived again");
        press(&mut app, "o");
        assert!(!screen(&mut app, 150, 40).contains("Shipped fix"), "o closes it again");
    }
}
