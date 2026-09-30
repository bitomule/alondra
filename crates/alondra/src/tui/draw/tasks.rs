use alondra_core::domain_v2::{AgentId, Task, TaskState};
use alondra_core::now_millis;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Panel, BorderType, Paragraph};

use super::super::agents::{AgentRow, said};
use super::super::inbox::{authorization_state, pr_line};
use super::super::tasks::{TaskKey, TaskListRow, TreeRow, state_label};
use super::super::{App, Pane, Remote, View, unavailable};
use super::{
    ACCENT, DIM, SEL_BG, SOFT, ago, centered_lines, fit, highlight, keep_visible, kind_glyph,
    more_hint, node_color, panel, scrollbar, since_phrase, state_color, state_glyph,
};
use crate::i18n;
use crate::runtime::{Activity, FleetNode};
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
        centered_lines(
            f,
            inner,
            vec![Line::from(
                tr!("Connecting to alondrad…", "Conectando con alondrad…").fg(DIM),
            )],
        );
    } else {
        let width = inner.width.saturating_sub(1) as usize;
        let (lines, sel) = list_lines(app, width);
        let h = inner.height as usize;
        if let Some(range) = sel {
            keep_visible(&mut app.task_scroll, range, h, lines.len());
        }
        let total = lines.len();
        f.render_widget(
            Paragraph::new(lines).scroll((app.task_scroll as u16, 0)),
            inner,
        );
        if let Some((s, e)) = sel {
            highlight(
                f,
                Rect {
                    width: inner.width.saturating_sub(1),
                    ..inner
                },
                s..e,
                app.task_scroll,
            );
        }
        scrollbar(f, inner, total, app.task_scroll);
    }
    if let Some(d) = detail {
        draw_task_detail(f, app, d);
    }
}

/// One row per item: the lead, the title cut to fit and what goes on the right.
fn titled(
    bar: &Span<'static>,
    lead: Vec<Span<'static>>,
    title: &str,
    style: Style,
    right: Vec<Span<'static>>,
    width: usize,
) -> Line<'static> {
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
    Style::new().add_modifier(if t.state == TaskState::Cancelled {
        Modifier::CROSSED_OUT
    } else {
        Modifier::empty()
    })
}

/// A small activity cue on running rows; the Tasks timer repaints it twice a second.
fn list_glyph(t: &Task) -> (&'static str, Color) {
    if t.state == TaskState::Running && !t.archived {
        let frames = ["◐", "◓", "◑", "◒"];
        (
            frames[(now_millis() / 500).rem_euclid(4) as usize],
            Color::Green,
        )
    } else {
        state_glyph(t.state)
    }
}

fn state_span(app: &App, t: &Task) -> Vec<Span<'static>> {
    let (_, color) = state_glyph(t.state);
    let mut spans = vec![Span::styled(state_label(t.state), Style::new().fg(color))];
    if let Some(since) = task_since(app, t) {
        spans.push(Span::styled(
            format!(" · {}", ago(since)),
            Style::new().fg(DIM),
        ));
    }
    spans
}

/// When the task entered its state, as the runtime recorded it.
fn task_since(app: &App, t: &Task) -> Option<i64> {
    app.runtime.as_ref()?.task(&t.id.0)?.state_since
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
        let bar = if selected {
            Span::styled("▌", Style::new().fg(ACCENT))
        } else {
            Span::raw(" ")
        };
        if matches!(row, TaskListRow::Finished(_) | TaskListRow::Archived(_)) && !lines.is_empty() {
            lines.push(Line::default());
        }
        let start = lines.len();
        match row {
            TaskListRow::Project(name) => {
                lines.push(fit(
                    vec![
                        bar,
                        Span::raw(" "),
                        Span::styled(name, Style::new().fg(SOFT).bold()),
                    ],
                    width,
                ));
            }
            TaskListRow::Active(n) => {
                lines.push(fit(
                    vec![
                        bar,
                        Span::styled(
                            tr!("IN PROGRESS", "EN CURSO"),
                            Style::new().fg(ACCENT).bold(),
                        ),
                    ],
                    width,
                ));
                if n == 0 {
                    lines.push(fit(
                        vec![
                            Span::raw(" "),
                            Span::styled(
                                tr!(
                                    "Nothing open · n new work",
                                    "Nada abierto · n trabajo nuevo"
                                ),
                                Style::new().fg(DIM),
                            ),
                        ],
                        width,
                    ));
                }
            }
            TaskListRow::Single(id) => {
                let Some(t) = app.task(&id) else { continue };
                let (glyph, color) = list_glyph(t);
                let lead = vec![
                    Span::raw("  "),
                    Span::styled(format!("{glyph} "), Style::new().fg(color).bold()),
                ];
                lines.push(titled(
                    &bar,
                    lead,
                    &t.title,
                    title_style(t),
                    state_span(app, t),
                    width,
                ));
                if let Some(now) = app.task_now(t) {
                    lines.push(fit(
                        vec![
                            bar.clone(),
                            Span::raw("    "),
                            Span::styled(md::one_line(&now), Style::new().fg(SOFT)),
                        ],
                        width,
                    ));
                }
            }
            TaskListRow::Request(id, done, total) => {
                let Some(r) = app.request(&id) else { continue };
                let right = vec![Span::styled(
                    format!("{done}/{total}"),
                    Style::new().fg(DIM),
                )];
                let lead = vec![
                    Span::raw("  "),
                    Span::styled("▸ ", Style::new().fg(Color::Cyan)),
                ];
                lines.push(titled(
                    &bar,
                    lead,
                    &app.request_title(r),
                    Style::new().bold(),
                    right,
                    width,
                ));
            }
            TaskListRow::Task(row) => {
                let Some(t) = app.task(&row.id) else { continue };
                let (glyph, color) = list_glyph(t);
                let lead = vec![
                    Span::styled(tree_lead(&row), Style::new().fg(DIM)),
                    Span::styled(format!("{glyph} "), Style::new().fg(color).bold()),
                ];
                lines.push(titled(
                    &bar,
                    lead,
                    &t.title,
                    title_style(t),
                    state_span(app, t),
                    width,
                ));
                if let Some(now) = app.task_now(t) {
                    lines.push(fit(
                        vec![
                            bar.clone(),
                            Span::styled(tree_under(&row), Style::new().fg(DIM)),
                            Span::styled(md::one_line(&now), Style::new().fg(SOFT)),
                        ],
                        width,
                    ));
                }
            }
            TaskListRow::Taskless(id) => {
                let Some(r) = app.request(&id) else { continue };
                let (glyph, color, state) = taskless(app, r);
                let lead = vec![
                    Span::raw("  "),
                    Span::styled(format!("{glyph} "), Style::new().fg(color).bold()),
                ];
                lines.push(titled(
                    &bar,
                    lead,
                    &app.request_title(r),
                    Style::new(),
                    vec![Span::styled(state, Style::new().fg(color))],
                    width,
                ));
            }
            TaskListRow::Finished(n) => {
                let arrow = if app.finished_open { "▾" } else { "▸" };
                let right = last_finished(app)
                    .map(|at| {
                        let when = super::ago_phrase(at);
                        vec![Span::styled(
                            trf!("the last one {when}", "el último {when}"),
                            Style::new().fg(DIM),
                        )]
                    })
                    .unwrap_or_default();
                lines.push(titled(
                    &bar,
                    vec![Span::styled(format!("{arrow} "), Style::new().fg(DIM))],
                    &trf!("Finished · {n}", "Terminadas · {n}"),
                    Style::new().bold(),
                    right,
                    width,
                ));
            }
            TaskListRow::Archived(n) => {
                let arrow = if app.archived_open { "▾" } else { "▸" };
                lines.push(fit(
                    vec![
                        bar,
                        Span::styled(format!("{arrow} "), Style::new().fg(DIM)),
                        Span::styled(
                            trf!("Archived ({n})", "Archivadas ({n})"),
                            Style::new().fg(DIM).bold(),
                        ),
                    ],
                    width,
                ));
            }
        }
        if selected {
            sel = Some((start, lines.len()));
        }
    }
    (lines, sel)
}

const LEFT_DEFINING: i64 = 10 * 60 * 1000;

/// How a request with no task stands: withdrawn, answered, closed, left without tasks by the
/// orchestrator, or still being split into tasks.
fn taskless(
    app: &App,
    r: &alondra_core::domain_v2::Request,
) -> (&'static str, Color, &'static str) {
    let runtime = app.runtime.as_ref().and_then(|x| x.request(&r.id.0));
    match (r.closed, runtime) {
        (true, Some(x)) if x.withdrawn => ("✗", DIM, tr!("withdrawn", "retirada")),
        (true, Some(x)) if x.answer.is_some() => ("✓", DIM, tr!("answered", "respondida")),
        (true, _) => ("✓", DIM, tr!("closed", "cerrada")),
        (false, Some(x))
            if x.created_at
                .is_some_and(|at| now_millis() - at >= LEFT_DEFINING) =>
        {
            ("◇", Color::LightMagenta, tr!("no tasks", "sin tareas"))
        }
        (false, _) => ("◇", Color::LightMagenta, tr!("preparing", "preparando")),
    }
}

/// When the last finished task finished, as the runtime recorded it.
fn last_finished(app: &App) -> Option<i64> {
    let d = app.domain.as_ref()?;
    d.tasks
        .iter()
        .filter(|t| t.state.terminal() && !t.archived)
        .filter_map(|t| task_since(app, t))
        .max()
}

fn section(lines: &mut Vec<Line<'static>>, title: &str, width: usize) {
    let title = title.trim().to_uppercase();
    let rest = width.saturating_sub(str_width(&title) + 4);
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled(" ─ ", Style::new().fg(DIM)),
        Span::styled(title, Style::new().fg(Color::Cyan).bold()),
        Span::styled(format!(" {}", "─".repeat(rest)), Style::new().fg(DIM)),
    ]));
}

fn draw_task_detail(f: &mut Frame, app: &mut App, area: Rect) {
    if app
        .card_view
        .as_ref()
        .is_some_and(|view| app.selected_view_key().as_deref() == Some(view.key.as_str()))
        && super::super::view_render::detail_card(app.card_view.as_ref().unwrap()).is_some()
    {
        super::draw_views_card(f, app, area);
        return;
    }
    let focused = app.task_zoom || app.focus == Pane::Detail;
    let width = area.width.saturating_sub(4) as usize;
    let (title, lines) = match app.task_sel.clone() {
        Some(TaskKey::Task(id)) => match app.task(&id) {
            Some(t) => {
                let (glyph, color) = state_glyph(t.state);
                (
                    Line::from(vec![
                        Span::styled(format!(" {glyph} "), Style::new().fg(color).bold()),
                        Span::styled(tr!("Task ", "Tarea "), Style::new().bold()),
                    ]),
                    task_lines(app, t, width),
                )
            }
            None => (Line::from(tr!(" Task ", " Tarea ")), vec![]),
        },
        Some(TaskKey::Request(id)) => match app.request(&id) {
            Some(r) => {
                let (glyph, color, state) = taskless(app, r);
                let runtime = app.runtime.as_ref().and_then(|x| x.request(&r.id.0));
                let mut first = vec![Span::styled(
                    format!("{glyph} {state}"),
                    Style::new().fg(color).bold(),
                )];
                if let Some(at) = runtime.and_then(|x| x.created_at) {
                    first.push(Span::styled(
                        format!(" · {}", since_phrase(at)),
                        Style::new().fg(DIM),
                    ));
                }
                let mut lines = vec![fit(first, width), Line::default()];
                let now = match runtime {
                    Some(x) if r.closed && x.withdrawn => tr!("You withdrew it: it ended without tasks.", "La retiraste: terminó sin tareas.").to_string(),
                    Some(x) if r.closed && x.answer.is_some() => {
                        let answer = md::one_line(x.answer.as_deref().unwrap_or_default());
                        trf!("The orchestrator answered it: {answer}", "El orquestador la respondió: {answer}")
                    }
                    _ if r.closed => tr!("It finished without tasks.", "Terminó sin tareas.").to_string(),
                    _ if state == tr!("no tasks", "sin tareas") => tr!("The orchestrator left it without tasks and did not close it: x withdraws it.", "El orquestador la dejó sin tareas y sin cerrar: x la retira.").to_string(),
                    _ => tr!("The orchestrator is splitting it into tasks.", "El orquestador la está repartiendo en tareas.").to_string(),
                };
                lines.extend(wrap(
                    vec![Span::raw(now)],
                    width,
                    vec![Span::styled(tr!("Now: ", "Ahora: "), Style::new().fg(DIM))],
                    vec![Span::raw("  ")],
                ));
                section(&mut lines, tr!(" Request", " Petición"), width);
                lines.extend(md::render(&md::parse(&r.body), width).lines);
                (Line::from(tr!(" Request ", " Petición ")), lines)
            }
            None => (Line::from(tr!(" Request ", " Petición ")), vec![]),
        },
        Some(TaskKey::Finished) => (
            Line::from(tr!(" Finished ", " Terminadas ")),
            vec![Line::from(vec![
                super::keycap("⏎"),
                Span::raw(tr!(" show or hide · ", " mostrar u ocultar · ")),
                super::keycap("Shift+A"),
                Span::raw(tr!(" archive", " archivar")),
            ])],
        ),
        Some(TaskKey::Archived) => (
            Line::from(tr!(" Archived ", " Archivadas ")),
            vec![Line::from(vec![
                super::keycap("⏎"),
                Span::raw(tr!(" show or hide", " mostrar u ocultar")),
            ])],
        ),
        None => (Line::from(tr!(" Task ", " Tarea ")), vec![]),
    };
    if lines.is_empty() {
        app.task_view.total = 0;
        f.render_widget(panel(title, false), area);
        return;
    }
    let mut block = panel(title, focused);
    let inner = block.clone().inner(area);
    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let total = lines.len();
    let h = inner.height as usize;
    app.task_detail_scroll = app.task_detail_scroll.min(total.saturating_sub(h));
    app.task_view = View {
        h,
        total,
        shown: app.task_view.shown || app.task_zoom,
    };
    if let Some(hint) = more_hint(app.task_detail_scroll, h, total) {
        block = block.title_bottom(hint);
    }
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(lines).scroll((app.task_detail_scroll as u16, 0)),
        text,
    );
    scrollbar(f, inner, total, app.task_detail_scroll);
}

fn task_lines(app: &App, t: &Task, width: usize) -> Vec<Line<'static>> {
    let (glyph, color) = state_glyph(t.state);
    let mut first = vec![Span::styled(
        format!("{glyph} {}", state_label(t.state)),
        Style::new().fg(color).bold(),
    )];
    if let Some(since) = task_since(app, t) {
        first.push(Span::styled(
            format!(" · {}", since_phrase(since)),
            Style::new().fg(DIM),
        ));
    }
    let mut lines = vec![fit(first, width)];
    lines.extend(wrap(
        vec![Span::styled(md::one_line(&t.title), Style::new().bold())],
        width,
        vec![],
        vec![],
    ));
    let agent = match &t.agent_id {
        Some(id) => {
            let name = app.agent_name(Some(id));
            let connected = app
                .domain
                .as_ref()
                .and_then(|d| d.agents.iter().find(|a| &a.id == id))
                .is_some_and(|a| a.connected);
            if connected {
                trf!("agent {name} · connected", "agente {name} · conectado")
            } else {
                trf!(
                    "agent {name} · not connected",
                    "agente {name} · no conectado"
                )
            }
        }
        None => tr!("no agent assigned yet", "sin agente asignado todavía").into(),
    };
    let mut where_ = vec![agent];
    where_.extend(app.task_project(t));
    lines.extend(wrap(
        vec![Span::styled(where_.join(" · "), Style::new().fg(DIM))],
        width,
        vec![],
        vec![],
    ));
    if let Some(reason) = app
        .runtime
        .as_ref()
        .and_then(|r| r.task(&t.id.0))
        .and_then(|r| r.cancel_reason.clone())
        .filter(|_| t.state == TaskState::Cancelled)
    {
        lines.extend(wrap(
            vec![Span::styled(
                trf!("Cancelled: {reason}", "Descartada: {reason}"),
                Style::new().fg(DIM),
            )],
            width,
            vec![],
            vec![],
        ));
    }
    if let Some(brief) = app
        .histories
        .get(&t.id)
        .and_then(|h| h.brief.clone())
        .filter(|b| !b.trim().is_empty())
    {
        section(&mut lines, tr!(" Assignment", " Encargo"), width);
        lines.extend(md::render(&md::parse(&brief), width).lines);
    }
    let progress = app.task_progress(t);
    if !progress.is_empty() {
        section(&mut lines, tr!(" Result", " Resultado"), width);
        lines.extend(wrap(
            vec![Span::styled(progress.join(" · "), Style::new().fg(SOFT))],
            width,
            vec![Span::raw(" ")],
            vec![Span::raw(" ")],
        ));
    }
    let waiting: Vec<_> = app
        .inbox_rows()
        .into_iter()
        .filter(|(i, _)| i.task_id.as_ref() == Some(&t.id) && i.children.is_empty())
        .map(|(i, _)| i.clone())
        .collect();
    if !waiting.is_empty() {
        section(&mut lines, tr!(" In the inbox", " En la bandeja"), width);
        for item in waiting {
            let (g, k, c) = kind_glyph(&item.kind);
            let text = item.body.clone().unwrap_or_else(|| item.title.clone());
            lines.push(fit(
                vec![
                    Span::styled(format!(" {g} {k} "), Style::new().fg(c).bold()),
                    Span::raw(md::one_line(&text)),
                ],
                width,
            ));
        }
    }
    let stack = app.merge_order(t);
    if stack.len() > 1 {
        section(
            &mut lines,
            tr!(" PRs in merge order", " PRs en orden de fusión"),
            width,
        );
        for (i, owner) in stack.iter().enumerate() {
            let (g, c) = state_glyph(owner.state);
            let lead = vec![
                Span::styled(format!(" {} ", i + 1), Style::new().fg(DIM)),
                Span::styled(format!("{g} "), Style::new().fg(c).bold()),
            ];
            let mut spans = match owner.pr_id.as_ref().and_then(|id| app.pull_request(id)) {
                Some(pr) => vec![Span::raw(format!(
                    "#{} {}",
                    pr.number,
                    md::one_line(&owner.title)
                ))],
                None => vec![
                    Span::raw(md::one_line(&owner.title)),
                    Span::styled(
                        tr!(" · no PR yet", " · sin PR todavía"),
                        Style::new().fg(DIM),
                    ),
                ],
            };
            if owner.id == t.id {
                spans.push(Span::styled(
                    tr!("  this task", "  esta tarea"),
                    Style::new().fg(ACCENT),
                ));
            }
            lines.extend(wrap(spans, width, lead, vec![Span::raw("     ")]));
            if let Some(pr) = owner.pr_id.as_ref().and_then(|id| app.pull_request(id)) {
                lines.extend(wrap(
                    vec![Span::styled(pr_line(pr), Style::new().fg(DIM))],
                    width,
                    vec![Span::raw("     ")],
                    vec![Span::raw("     ")],
                ));
            }
        }
    } else if let Some(pr) = t.pr_id.as_ref().and_then(|id| app.pull_request(id)) {
        section(&mut lines, "PR", width);
        lines.extend(wrap(
            vec![Span::raw(pr_line(pr))],
            width,
            vec![Span::raw(" ")],
            vec![Span::raw(" ")],
        ));
        for a in app.authorizations(&pr.id) {
            lines.push(fit(
                vec![
                    Span::raw(" "),
                    Span::styled(authorization_state(a), Style::new().fg(SOFT)),
                ],
                width,
            ));
        }
    }
    let (deps, dependents): (Vec<&Task>, Vec<&Task>) = (
        t.dependencies
            .iter()
            .filter_map(|id| app.task(id))
            .collect(),
        app.dependents(t),
    );
    if !deps.is_empty() || !dependents.is_empty() {
        section(&mut lines, tr!(" Dependencies", " Dependencias"), width);
        for (label, tasks) in [
            (tr!("depends on", "depende de"), deps),
            (tr!("waiting on it", "la esperan"), dependents),
        ] {
            if tasks.is_empty() {
                lines.push(fit(
                    vec![
                        Span::styled(format!(" {label} "), Style::new().fg(DIM)),
                        Span::styled(tr!("none", "ninguna"), Style::new().fg(DIM)),
                    ],
                    width,
                ));
            }
            for (i, other) in tasks.iter().enumerate() {
                let (g, c) = state_glyph(other.state);
                let lead = if i == 0 {
                    format!(" {label} ")
                } else {
                    " ".repeat(str_width(label) + 2)
                };
                lines.push(fit(
                    vec![
                        Span::styled(lead, Style::new().fg(DIM)),
                        Span::styled(format!("{g} "), Style::new().fg(c).bold()),
                        Span::raw(md::one_line(&other.title)),
                        Span::styled(
                            format!(" · {}", state_label(other.state)),
                            Style::new().fg(c),
                        ),
                    ],
                    width,
                ));
            }
        }
    }
    let documents: Vec<String> = t
        .documents
        .keys()
        .filter_map(|id| app.domain.as_ref()?.documents.iter().find(|d| &d.id == id))
        .map(|d| {
            let number = app.domain.as_ref().and_then(|x| x.document_revisions.iter().find(|r| r.id == d.current_revision)).map_or(0, |r| r.number);
            let title = md::one_line(&d.title);
            let unread = app.domain.as_ref().is_some_and(|x| x.inbox.iter().any(|r| r.pending && matches!(&r.subject, alondra_core::domain_v2::InboxSubject::DocumentReview { document_id, .. } if document_id == &d.id)));
            let read = if unread { tr!("unread", "sin leer") } else { tr!("read", "leído") };
            trf!("{title} · version {number} · {read}", "{title} · versión {number} · {read}")
        })
        .collect();
    if !documents.is_empty() {
        section(&mut lines, tr!(" Documents", " Documentos"), width);
        for d in documents {
            lines.push(fit(
                vec![
                    Span::styled(" ▤ ", Style::new().fg(Color::Cyan).bold()),
                    Span::raw(d),
                ],
                width,
            ));
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
                vec![Span::styled(
                    format!(" {label}: {}", unavailable(a)),
                    Style::new().fg(DIM),
                )]
            };
            lines.push(fit(line, width));
        }
    }
    if let Some(history) = app.histories.get(&t.id).filter(|h| !h.history.is_empty()) {
        section(&mut lines, tr!(" History", " Historia"), width);
        for step in &history.history {
            let (g, c) = state_glyph(step.state);
            let label = format!("{g} {:<14}", state_label(step.state));
            lines.extend(wrap(
                vec![
                    Span::styled(label, Style::new().fg(c)),
                    Span::raw(md::one_line(&step.summary)),
                ],
                width,
                vec![Span::styled(
                    format!(" {:>4} ", ago(step.at)),
                    Style::new().fg(DIM),
                )],
                vec![Span::raw("       ")],
            ));
        }
    }
    if let Some(r) = app
        .request(&t.request_id)
        .filter(|r| !r.body.trim().is_empty())
    {
        section(
            &mut lines,
            tr!(" Original request", " Petición original"),
            width,
        );
        lines.extend(md::render(&md::parse(&r.body), width).lines);
    }
    section(&mut lines, tr!(" Options", " Opciones"), width);
    let prs = if t.review_prs {
        super::super::work::pr_handling(false)
    } else {
        tr!(
            "no PR review; merging still needs your authorization",
            "sin revisión de PR; fusionar sigue necesitando tu autorización"
        )
    };
    lines.push(fit(
        vec![
            Span::styled(
                format!("  {:<12}", tr!("PR handling", "Gestión PR")),
                Style::new().fg(DIM),
            ),
            Span::styled(prs, Style::new().fg(SOFT)),
        ],
        width,
    ));
    if let Some(effort) = app
        .runtime
        .as_ref()
        .and_then(|r| r.task(&t.id.0))
        .and_then(|r| r.effort.clone())
    {
        lines.push(fit(
            vec![
                Span::styled(
                    format!("  {:<12}", tr!("Effort", "Esfuerzo")),
                    Style::new().fg(DIM),
                ),
                Span::styled(effort, Style::new().fg(SOFT)),
            ],
            width,
        ));
    }
    lines
}

/// "principal  Bash bazel build //ios:App · 1m": one thread of a session and what it does.
fn activity_line(a: &Activity, width: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!("{:<11}", truncate(&a.thread(), 10)),
        Style::new().fg(SOFT),
    )];
    match (&a.tool, &a.summary) {
        (Some(tool), summary) => {
            spans.push(Span::styled(format!("{tool} "), Style::new().fg(ACCENT)));
            spans.push(Span::raw(md::one_line(
                summary.as_deref().unwrap_or_default(),
            )));
        }
        (None, summary) => spans.push(Span::raw(md::one_line(
            summary.as_deref().unwrap_or(tr!("thinking", "pensando")),
        ))),
    }
    spans.push(Span::styled(
        format!(" · {}", ago(a.since)),
        Style::new().fg(DIM),
    ));
    fit(spans, width)
}

/// "2 subagentes · 1 shell", in the warning colour and marked when one may be hung.
fn background_badge(n: &FleetNode) -> Option<(String, Style)> {
    let text = i18n::background_badge(&n.background)?;
    if n.background.iter().any(|b| b.suspect) {
        Some((
            format!("{text} · {}", i18n::possibly_hung()),
            Style::new().fg(Color::LightRed),
        ))
    } else {
        Some((text, Style::new().fg(DIM)))
    }
}

fn background_lines(n: &FleetNode, width: usize) -> Vec<Line<'static>> {
    if n.background.is_empty() {
        return vec![];
    }
    let mut lines = vec![
        Line::default(),
        Line::from(
            tr!("Running in the background", "En marcha en segundo plano")
                .fg(DIM)
                .bold(),
        ),
    ];
    for b in &n.background {
        let color = if b.suspect { Color::LightRed } else { ACCENT };
        let mut spans = vec![
            Span::raw(md::one_line(&b.label)),
            Span::styled(format!(" · {}", ago(b.started_at)), Style::new().fg(DIM)),
        ];
        if b.suspect {
            spans.push(Span::styled(
                format!(" · {}", i18n::possibly_hung()),
                Style::new().fg(Color::LightRed),
            ));
        }
        lines.extend(wrap(
            spans,
            width,
            vec![Span::styled(
                format!("{} ", i18n::background_kind(b.kind)),
                Style::new().fg(color),
            )],
            vec![],
        ));
    }
    lines
}

/// An agent's row: the runtime's state, badge and age when alondrad reports its session,
/// whether it is connected otherwise.
fn agent_row(app: &App, id: &AgentId, width: usize, bar: &Span<'static>) -> Line<'static> {
    let name = app.agent_name(Some(id));
    let connected = app
        .domain
        .as_ref()
        .and_then(|d| d.agents.iter().find(|a| &a.id == id))
        .is_some_and(|a| a.connected);
    match app.node(id) {
        Some(n) => {
            let badge = background_badge(n);
            let mut right = vec![];
            if let Some((text, style)) = badge {
                right.push(Span::styled(format!("{text}  "), style));
            }
            if width >= 64 {
                right.push(Span::styled(
                    format!("{:<19}", truncate(n.status_label(), 19)),
                    Style::new().fg(node_color(n)),
                ));
            }
            right.push(Span::styled(
                format!("{:>5}", ago(n.state_since)),
                Style::new().fg(DIM),
            ));
            titled(
                bar,
                vec![Span::styled(" ● ", Style::new().fg(node_color(n)))],
                &name,
                Style::new(),
                right,
                width,
            )
        }
        None => {
            let (dot, color, state) = if connected {
                ("●", Color::Green, tr!("connected", "conectado"))
            } else {
                ("○", DIM, tr!("not connected", "no conectado"))
            };
            titled(
                bar,
                vec![Span::styled(format!(" {dot} "), Style::new().fg(color))],
                &name,
                Style::new(),
                vec![Span::styled(state, Style::new().fg(color))],
                width,
            )
        }
    }
}

/// The orchestrator's own fixed strip, always on top and never inside a project.
fn draw_orchestrator_strip(f: &mut Frame, area: Rect, n: &FleetNode, selected: bool) -> Rect {
    let [strip, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
    let block = Panel::bordered()
        .border_type(BorderType::Double)
        .border_style(Style::new().fg(ACCENT));
    let inner = block.inner(strip);
    f.render_widget(block, strip);
    if selected {
        f.buffer_mut().set_style(inner, Style::new().bg(SEL_BG));
    }
    let width = inner.width.saturating_sub(1) as usize;
    let line = fit(
        vec![
            Span::styled(" ★ ", Style::new().fg(ACCENT).bold()),
            Span::styled(
                format!("{} ", tr!("Orchestrator", "Orquestador")),
                Style::new().bold(),
            ),
            Span::styled(
                format!("● {}", n.status_label()),
                Style::new().fg(state_color(n.state)),
            ),
            Span::styled(
                format!(" · {}", since_phrase(n.state_since)),
                Style::new().fg(DIM),
            ),
        ],
        width,
    );
    f.render_widget(Paragraph::new(line), inner);
    rest
}

/// The agents in a narrow column beside the inbox on wide terminals.
pub fn draw_compact_agents(f: &mut Frame, app: &App, area: Rect) {
    let block = panel(Line::from(tr!(" Agents ", " Agentes ").fg(SOFT)), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    if let Some(o) = app.orchestrator() {
        lines.push(fit(
            vec![
                Span::styled(" ★ ", Style::new().fg(ACCENT).bold()),
                Span::styled(
                    format!("{} ", tr!("Orchestrator", "Orquestador")),
                    Style::new().bold(),
                ),
                Span::styled(o.status_label(), Style::new().fg(state_color(o.state))),
            ],
            width,
        ));
    }
    for r in app.agent_rows() {
        match r {
            AgentRow::Group { name, total } => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.push(fit(
                    vec![
                        Span::styled(format!(" {name}"), Style::new().bold()),
                        Span::styled(format!(" {total}"), Style::new().fg(DIM)),
                    ],
                    width,
                ));
            }
            AgentRow::Agent(id) => {
                let name = app.agent_name(Some(&id));
                let (color, state) = match app.node(&id) {
                    Some(n) => (node_color(n), n.status_label().to_string()),
                    None if app
                        .domain
                        .as_ref()
                        .and_then(|d| d.agents.iter().find(|a| a.id == id))
                        .is_some_and(|a| a.connected) =>
                    {
                        (Color::Green, tr!("connected", "conectado").to_string())
                    }
                    None => (DIM, tr!("not connected", "no conectado").to_string()),
                };
                lines.push(fit(
                    vec![
                        Span::styled("  ● ", Style::new().fg(color)),
                        Span::raw(md::one_line(&name)),
                        Span::styled(format!(" {state}"), Style::new().fg(DIM)),
                    ],
                    width,
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(
            tr!(" No agents yet.", " Todavía no hay agentes.").fg(DIM),
        ));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

pub fn draw_agents(f: &mut Frame, app: &mut App, area: Rect) {
    if app.agent_zoom && (app.selected_agent().is_some() || app.orchestrator_selected()) {
        draw_agent_detail(f, app, area);
        return;
    }
    let area = match app.orchestrator().cloned() {
        Some(o) => draw_orchestrator_strip(f, area, &o, app.orchestrator_selected()),
        None => area,
    };
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
        let [bar, rest] =
            Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        body = rest;
        let q = s.query();
        let local = app.local_agent_matches(&q).len();
        let status = match &s.remote {
            _ if q.is_empty() => String::new(),
            _ if local > 0 => trf!("{local} by text", "{local} por texto"),
            Remote::Done {
                via,
                sessions,
                query,
            } if *query == q => {
                let n = sessions.len();
                trf!("{n} according to {via}", "{n} según {via}")
            }
            Remote::Pending => {
                tr!("searching local details…", "buscando en detalles locales…").into()
            }
            Remote::Failed(e) => {
                let e = md::one_line(e);
                trf!("local search failed: {e}", "falló la búsqueda local: {e}")
            }
            _ => tr!("nothing by text", "nada por texto").into(),
        };
        let line = Line::from(vec![
            Span::styled(" / ", Style::new().fg(ACCENT).bold()),
            Span::raw(s.input.text()),
            Span::styled(format!("   {status}"), Style::new().fg(DIM)),
        ]);
        f.render_widget(
            Paragraph::new(vec![
                line,
                Line::styled("─".repeat(bar.width as usize), Style::new().fg(DIM)),
            ]),
            bar,
        );
        if s.editing && app.composer.is_none() {
            let (_, (_, col)) = s.input.layout(usize::MAX / 2);
            f.set_cursor_position(Position {
                x: bar.x + 3 + col as u16,
                y: bar.y,
            });
        }
    }
    let width = body.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    let mut sel = None;
    for (i, row) in app.agent_rows().into_iter().enumerate() {
        let id = match row {
            AgentRow::Group { name, total } => {
                if i > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::from(vec![
                    Span::styled(
                        format!(" {name}"),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {total}"), Style::new().fg(DIM)),
                ]));
                continue;
            }
            AgentRow::Agent(id) => id,
        };
        let selected = app.agent_sel.as_ref() == Some(&id);
        let bar = if selected {
            Span::styled("▌", Style::new().fg(ACCENT))
        } else {
            Span::raw(" ")
        };
        let start = lines.len();
        lines.push(agent_row(app, &id, width, &bar));
        for t in app.agent_tasks(&id) {
            let (g, c) = state_glyph(t.state);
            lines.push(fit(
                vec![
                    bar.clone(),
                    Span::raw("    "),
                    Span::styled(format!("{g} "), Style::new().fg(c).bold()),
                    Span::raw(md::one_line(&t.title)),
                    Span::styled(format!(" · {}", state_label(t.state)), Style::new().fg(c)),
                ],
                width,
            ));
        }
        if selected {
            sel = Some((start, lines.len()));
        }
    }
    if lines.is_empty() {
        let msg = match app.search.as_ref().map(|s| &s.remote) {
            Some(Remote::Pending) => tr!("Searching locally…", "Buscando localmente…"),
            Some(_) => tr!(
                "Nothing matches the search.",
                "Nada coincide con la búsqueda."
            ),
            None => tr!("No agents yet.", "Todavía no hay agentes."),
        };
        centered_lines(f, body, vec![Line::from(msg.fg(DIM))]);
    } else {
        let h = body.height as usize;
        if let Some(range) = sel {
            keep_visible(&mut app.agent_scroll, range, h, lines.len());
        }
        let total = lines.len();
        f.render_widget(
            Paragraph::new(lines).scroll((app.agent_scroll as u16, 0)),
            body,
        );
        if let Some((s, e)) = sel {
            highlight(
                f,
                Rect {
                    width: body.width.saturating_sub(1),
                    ..body
                },
                s..e,
                app.agent_scroll,
            );
        }
        scrollbar(f, body, total, app.agent_scroll);
    }
    if let Some(d) = detail {
        draw_agent_detail(f, app, d);
    }
}

fn draw_agent_detail(f: &mut Frame, app: &mut App, area: Rect) {
    if app
        .card_view
        .as_ref()
        .is_some_and(|view| app.selected_view_key().as_deref() == Some(view.key.as_str()))
        && super::super::view_render::detail_card(app.card_view.as_ref().unwrap()).is_some()
    {
        super::draw_views_card(f, app, area);
        return;
    }
    let Some(id) = app
        .agent_sel
        .clone()
        .filter(|_| app.selected_agent().is_some() || app.orchestrator_selected())
    else {
        app.agent_view.total = 0;
        f.render_widget(
            panel(Line::from(tr!(" Agent ", " Agente ").fg(DIM)), false),
            area,
        );
        return;
    };
    let orchestrator = app.orchestrator_selected();
    let name = if orchestrator {
        tr!("Orchestrator", "Orquestador").to_string()
    } else {
        app.agent_name(Some(&id))
    };
    let connected = app.selected_agent().is_some_and(|a| a.connected);
    let focused = app.agent_zoom || app.focus == Pane::Detail;
    let mut block = panel(
        Line::from(vec![
            Span::raw(" "),
            Span::styled(md::one_line(&name), Style::new().bold()),
            Span::raw(" "),
        ]),
        focused,
    );
    let inner = block.clone().inner(area);
    let text = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let width = text.width as usize;
    let node = app.node(&id).cloned();
    let mut lines = match &node {
        Some(n) => {
            let mut lines = vec![fit(
                vec![
                    Span::styled(
                        format!("● {}", n.status_label()),
                        Style::new().fg(node_color(n)).bold(),
                    ),
                    Span::styled(
                        format!(" · {}", since_phrase(n.state_since)),
                        Style::new().fg(DIM),
                    ),
                ],
                width,
            )];
            let short = &n.short_id;
            lines.push(Line::from(
                match &n.project {
                    Some(p) => trf!("project {p} · {short}", "proyecto {p} · {short}"),
                    None => short.clone(),
                }
                .fg(DIM),
            ));
            if let Some(cwd) = n.cwd.as_ref().filter(|_| !n.is_orchestrator) {
                lines.push(fit(
                    vec![Span::styled(cwd.clone(), Style::new().fg(DIM))],
                    width,
                ));
            }
            lines
        }
        None => {
            let state = if connected {
                tr!("● connected", "● conectado")
            } else {
                tr!("○ not connected", "○ no conectado")
            };
            vec![
                Line::from(Span::styled(
                    state,
                    Style::new()
                        .fg(if connected { Color::Green } else { DIM })
                        .bold(),
                )),
                fit(
                    vec![Span::styled(id.0.clone(), Style::new().fg(DIM))],
                    width,
                ),
            ]
        }
    };
    if orchestrator && let Some(runtime) = app.runtime.as_ref().and_then(|r| r.orchestrator_state())
    {
        let queued = runtime.queue_depth;
        lines.push(Line::from(
            trf!("Queued messages: {queued}", "Mensajes en cola: {queued}").fg(DIM),
        ));
        if let Some(run) = &runtime.active_run_id {
            lines.push(Line::from(
                trf!("Execution {run}", "Ejecución {run}").fg(DIM),
            ));
        }
        if let Some(error) = &runtime.last_error {
            lines.extend(wrap(
                vec![Span::styled(error.clone(), Style::new().fg(Color::Yellow))],
                width,
                vec![],
                vec![],
            ));
        }
    }
    let tasks = app.agent_tasks(&id);
    if !tasks.is_empty() {
        section(&mut lines, tr!(" Tasks", " Tareas"), width);
        for t in tasks {
            let (g, c) = state_glyph(t.state);
            lines.push(fit(
                vec![
                    Span::styled(format!(" {g} "), Style::new().fg(c).bold()),
                    Span::raw(md::one_line(&t.title)),
                    Span::styled(format!(" · {}", state_label(t.state)), Style::new().fg(c)),
                ],
                width,
            ));
        }
    }
    let waiting: Vec<_> = app
        .inbox_rows()
        .into_iter()
        .filter(|(i, _)| i.agent_id.as_ref() == Some(&id) && i.children.is_empty())
        .map(|(i, _)| i.clone())
        .collect();
    if !waiting.is_empty() {
        section(&mut lines, tr!(" In the inbox", " En la bandeja"), width);
        for item in waiting {
            let (g, k, c) = kind_glyph(&item.kind);
            let text = item.body.clone().unwrap_or_else(|| item.title.clone());
            lines.push(fit(
                vec![
                    Span::styled(format!(" {g} {k} "), Style::new().fg(c).bold()),
                    Span::raw(md::one_line(&text)),
                ],
                width,
            ));
        }
    }
    let threads = app.agent_activity(&id);
    if !threads.is_empty() {
        section(&mut lines, tr!(" Now", " Ahora"), width);
        for a in &threads {
            lines.push(activity_line(a, width));
            if let Some(task) = a
                .task
                .as_deref()
                .and_then(|t| app.task(&t.into()))
                .filter(|_| !a.is_main())
            {
                let title = md::one_line(&task.title);
                lines.push(fit(
                    vec![
                        Span::raw(" ".repeat(11)),
                        Span::styled(
                            trf!("subagent of “{title}”", "subagente de «{title}»"),
                            Style::new().fg(DIM),
                        ),
                    ],
                    width,
                ));
            }
        }
    }
    if let Some(n) = &node {
        lines.extend(background_lines(n, width));
        if let Some(needs) = &n.daemon_needs {
            lines.push(Line::default());
            lines.extend(wrap(
                vec![Span::styled(needs.clone(), Style::new().fg(Color::Yellow))],
                width,
                vec![Span::styled(
                    tr!("Needs: ", "Necesita: "),
                    Style::new().fg(DIM),
                )],
                vec![],
            ));
        }
        let said = said(n);
        if !said.trim().is_empty() {
            lines.push(Line::default());
            lines.push(Line::from(
                tr!("The last thing it said", "Lo último que dijo")
                    .fg(DIM)
                    .bold(),
            ));
            lines.extend(md::render(&md::parse(&said), width).lines);
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
    f.render_widget(
        Paragraph::new(lines).scroll((app.agent_detail_scroll as u16, 0)),
        text,
    );
    scrollbar(f, inner, total, app.agent_detail_scroll);
}
