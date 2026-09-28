mod tasks;

use std::collections::HashMap;

use alondra_core::domain_v2::{InboxItem, InboxSubject, InputKind, InteractionPayload, TaskState};
use alondra_core::now_millis;
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block as Panel, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use super::inbox::{action, interaction_of, merge_method, pr_state};
use super::md::{self, str_width, truncate, wrap};
use super::{App, Key, Pane, Screen, Target, View, unavailable};
use crate::i18n;

const ACCENT: Color = Color::LightYellow;
const DIM: Color = Color::DarkGray;
const SOFT: Color = Color::Gray;
const SEL_BG: Color = Color::Indexed(236);
const COMMENT: Color = Color::LightMagenta;

pub fn draw(f: &mut Frame, app: &mut App) {
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(3), Constraint::Length(1)]).areas(f.area());
    draw_head(f, app, head);
    let body = match app.composer.as_ref() {
        Some(c) => {
            let h = composer_height(&c.input, body.width).min(body.height.saturating_sub(3));
            let [main, comp] = Layout::vertical([Constraint::Min(3), Constraint::Length(h)]).areas(body);
            draw_composer(f, app, comp);
            main
        }
        None => body,
    };
    if app.reader.is_some() {
        draw_reader(f, app, body);
    } else {
        match app.screen {
            Screen::Inbox => draw_inbox(f, app, body),
            Screen::Tasks if app.schedules_open => draw_schedules(f, app, body),
            Screen::Tasks => tasks::draw_tasks(f, app, body),
            Screen::Fleet => tasks::draw_agents(f, app, body),
        }
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
    Panel::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(if focused { ACCENT } else { DIM })).title(title)
}

fn keycap(k: &str) -> Span<'static> {
    Span::styled(format!(" {k} "), Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD))
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
            let r = Rect { x: area.x, y: area.y + (row - scroll) as u16, width: area.width, height: 1 };
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
        Scrollbar::new(ScrollbarOrientation::VerticalRight).begin_symbol(None).end_symbol(None).track_symbol(None).thumb_symbol("┃").thumb_style(Style::new().fg(DIM)),
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
    let text = if rest == 0 { tr!(" ↑ end ", " ↑ fin ").to_string() } else { trf!(" ↓ {rest} more lines ", " ↓ {rest} líneas más ") };
    Some(Line::from(Span::styled(text, Style::new().fg(if rest == 0 { DIM } else { ACCENT }))).right_aligned())
}

fn keep_visible(scroll: &mut usize, range: (usize, usize), height: usize, total: usize) {
    let (s, e) = range;
    if e.saturating_sub(s) >= height {
        *scroll = (*scroll).clamp(s.saturating_sub(1), e.saturating_sub(height).max(s.saturating_sub(1)));
    } else if s < *scroll {
        *scroll = s.saturating_sub(1);
    } else if e > *scroll + height {
        *scroll = e.saturating_sub(height).min(s);
    }
    *scroll = (*scroll).min(total.saturating_sub(height));
}

fn centered_lines(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let w = area.width.max(1) as usize;
    let h = lines.iter().map(|l| l.width().div_ceil(w).max(1)).sum::<usize>() as u16;
    let top = area.y + area.height.saturating_sub(h) / 2;
    let r = Rect { x: area.x, y: top, width: area.width, height: h.min(area.height) };
    f.render_widget(Paragraph::new(lines).centered().wrap(Wrap { trim: true }), r);
}

pub(super) fn state_glyph(s: TaskState) -> (&'static str, Color) {
    match s {
        TaskState::NotStarted => ("○", DIM),
        TaskState::WaitingDependency => ("◌", DIM),
        TaskState::Running => ("●", Color::Green),
        TaskState::NeedsYou => ("?", Color::Yellow),
        TaskState::InReview => ("⇄", Color::LightGreen),
        TaskState::Closing => ("⇉", Color::Cyan),
        TaskState::Done => ("✓", Color::LightGreen),
        TaskState::Cancelled => ("✗", DIM),
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
            Span::styled(format!(" {label} "), Style::new().fg(Color::Black).bg(SOFT).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(SOFT))
        }
    };
    let mut left = vec![Span::styled(format!(" {} ", crate::service_client::target().unwrap_or("Local")), Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)), Span::raw(" ")];
    let (inbox, tasks, agents) = (app.inbox_count(), app.open_task_count(), app.domain.as_ref().map_or(0, |d| d.agents.len()));
    left.push(tab(trf!("Inbox {inbox}", "Bandeja {inbox}"), app.screen == Screen::Inbox));
    left.push(tab(trf!("Tasks {tasks}", "Tareas {tasks}"), app.screen == Screen::Tasks && !app.schedules_open));
    if app.screen == Screen::Tasks {
        left.push(tab(tr!("Scheduled [s]", "Programadas [s]").into(), app.schedules_open));
    }
    left.push(tab(trf!("Agents {agents}", "Agentes {agents}"), app.screen == Screen::Fleet));
    if app.offline {
        left.push(Span::raw(" "));
        left.push(Span::styled(tr!(" no connection to alondrad ", " sin conexión con alondrad "), Style::new().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD)));
    }
    f.render_widget(Paragraph::new(Line::from(left)), area);
}

fn draw_inbox(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width;
    app.detail_view.shown = w >= 84;
    if w >= 84 {
        let lw = (w * 2 / 5).clamp(36, 56);
        let [list, detail] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        draw_inbox_list(f, app, list);
        draw_item_detail(f, app, detail);
    } else {
        draw_inbox_list(f, app, area);
    }
}

/// What a row says first: a child's own text, a root's name.
fn row_title(item: &InboxItem) -> String {
    match (&item.subject, item.body.as_deref().filter(|b| !b.trim().is_empty())) {
        (InboxSubject::Interaction { .. }, Some(body)) => md::one_line(body),
        _ => md::one_line(&item.title),
    }
}

/// One inbox row; a child is drawn indented under its root.
fn item_lines(app: &App, item: &InboxItem, width: usize, selected: bool, nested: bool) -> Vec<Line<'static>> {
    let key = Key(item.id.clone());
    let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let (hook, gap) = if nested { (Span::styled("  ↳ ", Style::new().fg(Color::Magenta)), Span::raw("    ")) } else { (Span::raw(""), Span::raw("")) };
    let (glyph, kind, color) = kind_glyph(&item.kind);
    let title_style = if selected { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let first = fit(vec![bar.clone(), hook, Span::styled(format!("{glyph} "), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(row_title(item), title_style)], width);
    let mut meta = vec![bar, gap, Span::styled(format!("  {} · {kind}", app.agent_name(item.agent_id.as_ref())), Style::new().fg(DIM))];
    if item.pending {
        meta.push(Span::styled(tr!(" · pending", " · pendiente"), Style::new().fg(ACCENT)));
    }
    let drafted = app.reviews.get(&key).map(|r| r.count()).unwrap_or(0);
    if drafted > 0 {
        meta.push(Span::styled(format!(" · ✎{drafted}"), Style::new().fg(COMMENT)));
    }
    if let Some(i) = interaction_of(item).and_then(|id| app.interaction(id)) {
        if let InteractionPayload::Question { options, answer: None } | InteractionPayload::ActionRequest { options, answer: None } = &i.payload {
            for (n, o) in options.iter().enumerate().take(9) {
                meta.push(Span::styled(format!("  {}", n + 1), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
                meta.push(Span::styled(format!(" {}", md::one_line(i18n::option_label(&o.label))), Style::new().fg(SOFT)));
            }
        }
    }
    if let Some(pr) = app.item_pr(item) {
        meta.push(Span::styled(format!(" · {} #{} {}", pr.repository, pr.number, pr_state(pr.state)), Style::new().fg(DIM)));
    }
    vec![first, fit(meta, width)]
}

fn draw_inbox_list(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Pane::List || !app.detail_view.shown;
    let block = panel(Line::from(tr!(" Inbox ", " Bandeja ").bold()), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !app.loaded {
        let msg = if app.offline { tr!("alondrad is not responding · retrying…", "alondrad no responde · reintentando…") } else { tr!("Connecting to alondrad…", "Conectando con alondrad…") };
        centered_lines(f, inner, vec![Line::from(msg.fg(DIM))]);
        return;
    }
    let rows: Vec<(InboxItem, bool)> = app.inbox_rows().into_iter().map(|(i, n)| (i.clone(), n)).collect();
    if rows.is_empty() {
        centered_lines(
            f,
            inner,
            vec![
                Line::from(tr!("✓ All caught up", "✓ Todo al día").fg(Color::LightGreen).bold()),
                Line::from(tr!("Nobody is waiting for you and there is nothing to read.", "Nadie te espera y no hay nada por leer.").fg(DIM)),
                Line::default(),
                Line::from(vec![Span::styled("n", Style::new().fg(ACCENT).bold()), tr!(" order new work", " encarga trabajo nuevo").fg(DIM)]),
                Line::from(vec![Span::styled("Tab", Style::new().fg(ACCENT).bold()), tr!(" see the tasks", " mira las tareas").fg(DIM)]),
            ],
        );
        return;
    }
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = vec![];
    let mut sel_range = None;
    for (item, nested) in &rows {
        if !nested && !lines.is_empty() {
            lines.push(Line::default());
        }
        let selected = app.sel.as_ref().is_some_and(|k| k.0 == item.id);
        let start = lines.len();
        lines.extend(item_lines(app, item, width, selected, *nested));
        if selected {
            sel_range = Some((start, lines.len()));
        }
    }
    let h = inner.height as usize;
    if let Some(range) = sel_range {
        keep_visible(&mut app.list_scroll, range, h, lines.len());
    }
    let total = lines.len();
    f.render_widget(Paragraph::new(lines).scroll((app.list_scroll as u16, 0)), inner);
    if let Some((s, e)) = sel_range {
        highlight(f, Rect { width: inner.width.saturating_sub(1), ..inner }, s..e, app.list_scroll);
    }
    scrollbar(f, inner, total, app.list_scroll);
}

fn detail(app: &App, item: &InboxItem, width: usize) -> Vec<Line<'static>> {
    let (glyph, kind, color) = kind_glyph(&item.kind);
    let mut meta = vec![Span::styled(format!("{glyph} {kind}"), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(format!(" · {}", app.agent_name(item.agent_id.as_ref())), Style::new().fg(SOFT))];
    if let Some(parent) = app.parent(&Key(item.id.clone())) {
        meta.push(Span::styled(format!(" · {}", md::one_line(&parent.title)), Style::new().fg(DIM)));
    }
    let mut lines = vec![fit(meta, width), Line::default()];
    let parts = app.item_text(item);
    lines.extend(md::render(&md::parse(&parts.join("\n\n")), width).lines);
    if let Some(root) = app.item(&Key(item.id.clone())).filter(|i| !i.children.is_empty()) {
        lines.push(Line::default());
        let n = root.children.len();
        lines.push(Line::from(trf!("Under it ({n})", "Debajo ({n})").fg(DIM).bold()));
        for c in &root.children {
            let (g, k, col) = kind_glyph(&c.kind);
            lines.push(fit(vec![Span::styled(format!(" {g} {k} "), Style::new().fg(col).bold()), Span::raw(row_title(c))], width));
        }
    }
    lines
}

/// The keys the selected item offers, straight from its available actions; an action that is off
/// says why.
fn action_lines(app: &App, item: &InboxItem, width: usize) -> Vec<Line<'static>> {
    let hint = |k: &str, label: String| vec![keycap(k), Span::styled(format!(" {label}  "), Style::new().add_modifier(Modifier::BOLD))];
    let off = |label: String, why: String| fit(vec![Span::styled(format!("{label}: {why}"), Style::new().fg(DIM))], width);
    let mut out = vec![];
    if interaction_of(item).is_none() {
        out.push(fit(vec![Span::styled(tr!("Select an item under it to act on it", "Selecciona un elemento de debajo para actuar"), Style::new().fg(DIM))], width));
    }
    let interaction = interaction_of(item).and_then(|id| app.interaction(id));
    for a in &item.available_actions {
        match a.kind.as_str() {
            "mark_read" if a.enabled => out.push(fit(hint("x", tr!("mark read", "marcar leído").into()), width)),
            "answer_question" if a.enabled => {
                if let Some(InteractionPayload::Question { options, .. } | InteractionPayload::ActionRequest { options, .. }) = interaction.map(|i| &i.payload) {
                    for (n, o) in options.iter().enumerate().take(9) {
                        let writes = if o.inputs.iter().any(|s| s.kind == InputKind::Text) { tr!(" …", " …") } else { "" };
                        out.push(fit(hint(&(n + 1).to_string(), format!("{}{writes}", md::one_line(i18n::option_label(&o.label)))), width));
                    }
                }
            }
            "accept_delivery" if a.enabled => out.push(fit(hint("v", tr!("accept the delivery", "aceptar la entrega").into()), width)),
            "submit_feedback" if a.enabled => out.push(fit(hint("r", tr!("send feedback", "enviar feedback").into()), width)),
            "authorize_merge" if a.enabled => {
                if let Some(pr) = app.item_pr(item) {
                    for (n, m) in pr.allowed_methods.iter().enumerate().take(9) {
                        let method = merge_method(*m);
                        out.push(fit(hint(&(n + 1).to_string(), trf!("authorize merge: {method}", "autorizar fusión: {method}")), width));
                    }
                }
            }
            kind => {
                let label = match kind {
                    "mark_read" => tr!("Mark read", "Marcar leído"),
                    "answer_question" => tr!("Answer", "Contestar"),
                    "accept_delivery" => tr!("Accept", "Aceptar"),
                    "submit_feedback" => tr!("Feedback", "Feedback"),
                    "authorize_merge" => tr!("Authorize merge", "Autorizar fusión"),
                    other => other,
                };
                out.push(off(label.to_string(), unavailable(a)));
            }
        }
    }
    let mut tail = vec![keycap("o"), Span::styled(tr!(" read all  ", " leer entero  "), Style::new().fg(SOFT))];
    if app.item_pr(item).is_some() {
        tail.extend([keycap("d"), Span::styled(" diff  ", Style::new().fg(SOFT)), keycap("w"), Span::styled(" GitHub  ", Style::new().fg(SOFT))]);
    }
    if item.agent_id.is_some() {
        tail.extend([keycap("a"), Span::styled(tr!(" its terminal", " su terminal"), Style::new().fg(SOFT))]);
    }
    out.push(fit(tail, width));
    out
}

fn draw_item_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(item) = app.sel.as_ref().and_then(|k| app.item(k)).cloned() else {
        app.detail_view.total = 0;
        f.render_widget(panel(Line::from(tr!(" Detail ", " Detalle ").fg(DIM)), false), area);
        return;
    };
    let (_, kind, color) = kind_glyph(&item.kind);
    let mut block = panel(Line::from(vec![Span::raw(" "), Span::styled(kind, Style::new().fg(color).bold()), Span::raw(" ")]), app.focus == Pane::Detail);
    let inner = block.clone().inner(area);
    let width = inner.width.saturating_sub(2) as usize;
    let actions = action_lines(app, &item, width);
    let ah = (actions.len() as u16 + 1).min(inner.height / 2);
    let [content, bottom] = Layout::vertical([Constraint::Min(1), Constraint::Length(ah)]).areas(inner);
    let lines = detail(app, &item, width);
    let total = lines.len();
    let h = content.height as usize;
    app.detail_scroll = app.detail_scroll.min(total.saturating_sub(h));
    app.detail_view = View { h, total, shown: true };
    if let Some(hint) = more_hint(app.detail_scroll, h, total) {
        block = block.title_bottom(hint);
    }
    f.render_widget(block, area);
    let text_area = Rect { x: content.x + 1, width: content.width.saturating_sub(2), ..content };
    f.render_widget(Paragraph::new(lines).scroll((app.detail_scroll as u16, 0)), text_area);
    scrollbar(f, content, total, app.detail_scroll);
    let mut bottom_lines = vec![Line::styled("─".repeat(inner.width as usize), Style::new().fg(DIM))];
    bottom_lines.extend(actions.into_iter().map(|l| {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(l.spans);
        Line::from(spans)
    }));
    f.render_widget(Paragraph::new(bottom_lines), bottom);
}

fn draw_reader(f: &mut Frame, app: &mut App, area: Rect) {
    let still_there = app.reader.as_ref().is_some_and(|r| app.item(&r.key).is_some());
    let Some(r) = app.reader.as_mut() else { return };
    let review = app.reviews.get(&r.key);
    let (glyph, color) = (r.glyph, r.color);
    let mut title = vec![
        Span::raw(" "),
        Span::styled(format!("{glyph} {}", r.kind), Style::new().fg(color).bold()),
        Span::styled(format!(" · {} · ", r.agent), Style::new().fg(SOFT)),
        Span::styled(truncate(&md::one_line(&r.title), (area.width as usize).saturating_sub(40).max(10)), Style::new().bold()),
        Span::raw(" "),
    ];
    if !still_there && app.loaded {
        title.push(Span::styled(tr!(" no longer in the inbox ", " ya no está en la bandeja "), Style::new().fg(Color::Black).bg(Color::LightRed)));
    }
    let n_comments = review.map(|rv| rv.count()).unwrap_or(0);
    let status = if n_comments == 0 {
        tr!(" no comments ", " sin comentarios ").to_string()
    } else {
        let c = i18n::comments(n_comments);
        trf!(" {c} unsent ", " {c} sin enviar ")
    };
    let mut block = panel(Line::from(title), true).title_bottom(Line::from(Span::styled(status, Style::new().fg(if n_comments > 0 { COMMENT } else { DIM }))).right_aligned());
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
            let [bar, rest] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(doc);
            let hits = r.matches();
            let status = match (find.query().is_empty(), hits.iter().position(|&i| i == r.cursor)) {
                (true, _) => String::new(),
                (false, _) if hits.is_empty() => tr!("no matches", "sin coincidencias").into(),
                (false, Some(i)) => {
                    let (i, n) = (i + 1, hits.len());
                    trf!("{i}/{n} · n next · N previous", "{i}/{n} · n siguiente · N anterior")
                }
                (false, None) => {
                    let n = hits.len();
                    trf!("{n} matches · n next", "{n} coincidencias · n siguiente")
                }
            };
            let line = Line::from(vec![Span::styled(" / ", Style::new().fg(ACCENT).bold()), Span::raw(find.input.text()), Span::styled(format!("   {status}"), Style::new().fg(DIM))]);
            f.render_widget(Paragraph::new(line), bar);
            if find.editing && app.composer.is_none() && app.cmdline.is_none() {
                let (_, (_, col)) = find.input.layout(usize::MAX / 2);
                f.set_cursor_position(Position { x: bar.x + 3 + col as u16, y: bar.y });
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
    let numbers: HashMap<usize, usize> = review.map(|rv| rv.comments.iter().enumerate().map(|(i, c)| (c.block, i + 1)).collect()).unwrap_or_default();
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
                Some(b) if numbers.contains_key(&b) && first => Span::styled(format!("{:<2}", numbers[&b]), Style::new().fg(COMMENT).bold()),
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
        highlight(f, Rect { x: doc.x + 3, width: doc.width.saturating_sub(4), ..doc }, s..e, r.scroll);
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
        .title(Line::from(vec![Span::styled(tr!(" Comments ", " Comentarios "), Style::new().bold()), Span::styled(n.to_string(), Style::new().fg(DIM))]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let width = text.width as usize;
    let mut lines: Vec<Line> = vec![];
    let key = |k: &str, d: &str| Line::from(vec![Span::styled(format!("{k:<6}"), Style::new().fg(ACCENT).bold()), Span::styled(d.to_string(), Style::new().fg(DIM))]);
    let item = app.item(&r.key);
    match review.filter(|rv| !rv.is_empty()) {
        None => {
            lines.push(Line::from(tr!("None yet.", "Todavía ninguno.").fg(DIM)));
            lines.push(Line::default());
            lines.push(key("j/k", tr!("move the cursor by blocks", "mueve el cursor por bloques")));
            lines.push(key("^d ^u", tr!("half a page down / up", "media página abajo / arriba")));
            lines.push(key("/", tr!("search the text; n/N next/prev", "busca en el texto; n/N sig./ant.")));
            lines.push(key("c", tr!("comment on the marked block", "comenta el bloque marcado")));
            lines.push(key("Shift+C", tr!("general comment", "comentario general")));
            if item.is_some_and(|i| action(i, "submit_feedback").is_some_and(|a| a.enabled)) {
                lines.push(key("s", tr!("send your comments as feedback", "envía tus comentarios como feedback")));
            }
            if item.is_some_and(|i| action(i, "accept_delivery").is_some_and(|a| a.enabled)) {
                lines.push(key("v", tr!("accept the delivery", "acepta la entrega")));
            }
            if item.is_some_and(|i| action(i, "mark_read").is_some_and(|a| a.enabled)) {
                lines.push(key("x", tr!("mark read", "marca leído")));
            }
            if let Some(InteractionPayload::Question { options, answer: None } | InteractionPayload::ActionRequest { options, answer: None }) = item.and_then(interaction_of).and_then(|id| app.interaction(id)).map(|i| &i.payload) {
                lines.push(Line::default());
                lines.push(Line::from(tr!("Or choose directly", "O elige directamente").fg(DIM)));
                for (i, o) in options.iter().enumerate().take(9) {
                    lines.push(fit(vec![Span::styled(format!("{:<6}", i + 1), Style::new().fg(ACCENT).bold()), Span::raw(md::one_line(i18n::option_label(&o.label)))], width));
                }
            }
        }
        Some(rv) => {
            for (i, c) in rv.comments.iter().enumerate() {
                let num_style = if c.block == cursor { Style::new().fg(Color::Black).bg(ACCENT).bold() } else { Style::new().fg(COMMENT).bold() };
                let quote = wrap(vec![Span::styled(format!("«{}»", md::clip(&c.quote, 90)), Style::new().fg(DIM).italic())], width, vec![Span::styled(format!("{:>2}.", i + 1), num_style), Span::raw(" ")], vec![Span::raw("    ")]);
                lines.extend(quote);
                lines.extend(wrap(vec![Span::raw(c.text.replace('\n', " "))], width, vec![Span::raw("    ")], vec![Span::raw("    ")]));
                lines.push(Line::default());
            }
            if !rv.general.trim().is_empty() {
                lines.push(Line::from("General".fg(COMMENT).bold()));
                lines.extend(wrap(vec![Span::raw(rv.general.replace('\n', " "))], width, vec![Span::raw("    ")], vec![Span::raw("    ")]));
            }
        }
    }
    f.render_widget(Paragraph::new(lines), text);
}

fn composer_height(input: &super::input::TextInput, width: u16) -> u16 {
    let (lines, _) = input.layout(width.saturating_sub(4) as usize);
    lines.len().clamp(1, 8) as u16 + 2
}

/// Whether what the composer answers is still the one on screen: a text opened on something that
/// changed stays, and says so, instead of going to something else.
fn composer_outdated(app: &App, target: &Target) -> bool {
    let key = match target {
        Target::Answer { key, .. } | Target::Feedback { key, .. } => key,
        _ => return false,
    };
    let kind = if matches!(target, Target::Answer { .. }) { "answer_question" } else { "submit_feedback" };
    !app.item(key).is_some_and(|i| action(i, kind).is_some_and(|a| a.enabled))
}

fn draw_composer(f: &mut Frame, app: &App, area: Rect) {
    let Some(c) = app.composer.as_ref() else { return };
    let mut title = match &c.target {
        Target::Answer { title, label, .. } => {
            let (t, o) = (md::one_line(title), i18n::option_label(label));
            trf!("{t} · {o}", "{t} · {o}")
        }
        Target::Feedback { title, .. } => {
            let t = md::one_line(title);
            trf!("Feedback on {t}", "Feedback sobre {t}")
        }
        Target::NewRequest => tr!("New work: the orchestrator turns it into tasks", "Trabajo nuevo: el orquestador lo convierte en tareas").into(),
        Target::Comment { quote, .. } => {
            let q = md::one_line(quote);
            trf!("Comment on “{q}”", "Comentario sobre «{q}»")
        }
        Target::General { send: true } => tr!("Your comments (⏎ sends them as feedback)", "Tus comentarios (⏎ los envía como feedback)").into(),
        Target::General { send: false } => tr!("General comment", "Comentario general").into(),
    };
    if composer_outdated(app, &c.target) {
        title.push_str(tr!(" · changed since you opened it: your text is kept", " · ha cambiado desde que lo abriste: tu texto se conserva"));
    }
    let action = composer_action(&c.target);
    let title = truncate(&title, (area.width as usize).saturating_sub(6));
    let hint = trf!(" ⏎ {action} · Alt+⏎ new line · Esc cancels ", " ⏎ {action} · Alt+⏎ nueva línea · Esc cancela ");
    let block = panel(Line::from(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).bold())), true).title_bottom(Line::from(hint.fg(DIM)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let (lines, (row, col)) = c.input.layout(text.width as usize);
    let scroll = row.saturating_sub(text.height.saturating_sub(1) as usize);
    if c.input.text().is_empty() {
        f.render_widget(Paragraph::new(Line::from(tr!("Type here…", "Escribe aquí…").fg(DIM))), text);
    } else {
        f.render_widget(Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()).scroll((scroll as u16, 0)), text);
    }
    f.set_cursor_position(Position { x: text.x + col as u16, y: text.y + (row - scroll) as u16 });
}

fn composer_action(t: &Target) -> &'static str {
    match t {
        Target::Comment { .. } | Target::General { send: false } => tr!("saves", "guarda"),
        _ => tr!("sends", "envía"),
    }
}

fn footer_keys(app: &App) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if app.schedules_open {
        let enabled = app.schedules.get(app.schedule_idx).and_then(|s| s["enabled"].as_bool()).unwrap_or(true);
        return vec![k("j/k", tr!("move", "mover")), k("n", tr!("new", "nueva")), k("e", tr!("edit", "editar")), k("p", if enabled { tr!("pause", "pausa") } else { tr!("resume", "reanuda") }), k("⏎", tr!("run", "ejecuta")), k("d d", tr!("delete", "borra")), k("Esc", tr!("back", "atrás"))];
    }
    if let Some(c) = app.composer.as_ref() {
        return vec![k("⏎", composer_action(&c.target)), k("Alt+⏎", tr!("new line", "nueva línea")), k("Ctrl+W", tr!("delete word", "borra palabra")), k("Ctrl+U", tr!("delete line", "borra línea")), k("Esc", tr!("cancel", "cancela"))];
    }
    if let Some(r) = app.reader.as_ref() {
        if r.find.as_ref().is_some_and(|f| f.editing) {
            return vec![k("⏎", tr!("find", "buscar")), k("Esc", tr!("cancel", "cancela"))];
        }
        let mut v = vec![k("j/k", tr!("block", "bloque")), k("^d/^u", tr!("half page", "media página")), k("/", tr!("search", "buscar"))];
        if r.find.is_some() {
            v.push(k("n/Shift+N", tr!("next/prev", "sig./ant.")));
        }
        v.extend([k("c", tr!("comment", "comentar")), k("Shift+C", tr!("general", "general")), k("d", tr!("delete", "borrar"))]);
        if let Some(item) = app.item(&r.key) {
            if action(item, "submit_feedback").is_some_and(|a| a.enabled) {
                v.push(k("s", tr!("send feedback", "enviar feedback")));
            }
            if action(item, "accept_delivery").is_some_and(|a| a.enabled) {
                v.push(k("v", tr!("accept", "aceptar")));
            }
            if action(item, "mark_read").is_some_and(|a| a.enabled) {
                v.push(k("x", tr!("mark read", "leído")));
            }
        }
        v.push(k("Esc/o", tr!("back", "volver")));
        return v;
    }
    match app.screen {
        Screen::Tasks => {
            if app.task_zoom {
                return vec![k("j/k", tr!("scroll", "desplazar")), k("^d/^u", tr!("half page", "media página")), k("Esc/o", tr!("back", "volver")), k("⏎", tr!("its terminal", "su terminal"))];
            }
            let mut v = vec![k("j/k", tr!("move", "mover")), k("h/l", tr!("pane", "panel"))];
            if matches!(app.task_sel, Some(super::tasks::TaskKey::Finished | super::tasks::TaskKey::Archived)) {
                v.push(k("⏎", tr!("show/hide", "mostrar/ocultar")));
            } else if let Some(t) = app.selected_task() {
                v.extend([k("o", tr!("detail", "detalle")), k("⏎", tr!("its terminal", "su terminal"))]);
                if t.state.terminal() { v.push(k("Shift+R", tr!("reopen", "reabrir"))); } else { v.push(k("x", tr!("cancel", "descartar"))); }
            } else if app.selected_request().is_some_and(|r| !r.closed) {
                v.push(k("x", tr!("withdraw", "retirar")));
            }
            v.extend([k("Shift+A", tr!("archive finished", "archivar terminadas")), k("s", tr!("scheduled", "programadas")), k("Tab", tr!("agents", "agentes")), k("n", tr!("new", "nuevo")), k("q", tr!("quit", "salir"))]);
            v
        }
        Screen::Fleet => {
            if app.search.as_ref().is_some_and(|s| s.editing) {
                return vec![k("⏎", tr!("keep filter", "fijar filtro")), k("Esc", tr!("clear search", "quitar búsqueda"))];
            }
            if app.agent_zoom {
                return vec![k("j/k", tr!("scroll", "desplazar")), k("^d/^u", tr!("half page", "media página")), k("Esc/o", tr!("back", "volver")), k("a", tr!("its terminal", "su terminal"))];
            }
            let mut v = vec![k("j/k", tr!("move", "mover")), k("h/l", tr!("pane", "panel")), k("/", tr!("search", "buscar"))];
            if app.search.is_some() {
                v.push(k("Esc", tr!("clear search", "quitar búsqueda")));
            }
            v.extend([k("o", tr!("expand", "expandir")), k("⏎", tr!("its terminal (← returns)", "su terminal (← vuelve)")), k("n", tr!("new", "nuevo")), k("Tab", tr!("inbox", "bandeja")), k("q", tr!("quit", "salir"))]);
            v
        }
        Screen::Inbox => {
            let pane = if app.focus == Pane::Detail { k("j/k", tr!("scroll", "desplazar")) } else { k("j/k", tr!("move", "mover")) };
            let mut v = vec![pane, k("o", tr!("expand", "expandir"))];
            if let Some(item) = app.sel.as_ref().and_then(|s| app.item(s)) {
                for a in item.available_actions.iter().filter(|a| a.enabled) {
                    match a.kind.as_str() {
                        "mark_read" => v.push(k("x", tr!("mark read", "leído"))),
                        "answer_question" => v.push(k("1-9", tr!("answer", "contestar"))),
                        "accept_delivery" => v.push(k("v", tr!("accept", "aceptar"))),
                        "submit_feedback" => v.push(k("r", tr!("feedback", "feedback"))),
                        "authorize_merge" => {
                            let n = app.item_pr(item).map_or(1, |p| p.allowed_methods.len().clamp(1, 9));
                            v.push(k(&format!("1-{n}"), tr!("authorize merge", "autorizar fusión")));
                        }
                        _ => {}
                    }
                }
                if item.agent_id.is_some() {
                    v.push(k("a", tr!("its terminal", "su terminal")));
                }
            }
            v.extend([k("^d/^u", tr!("scroll detail", "desplazar detalle")), k("Tab", tr!("tasks", "tareas")), k("n", tr!("new", "nuevo")), k("q", tr!("quit", "salir"))]);
            v
        }
    }
}

/// Action results float above the key bar, wrapped, so they never hide the keys or get cut.
fn draw_flash(f: &mut Frame, app: &App, body: Rect) {
    let Some(fl) = &app.flash else { return };
    let (mark, color) = if fl.error { ("✗ ", Color::LightRed) } else { ("✓ ", Color::LightGreen) };
    let width = body.width.saturating_sub(4).clamp(10, 100);
    let inner_width = width.saturating_sub(4).max(1);
    let text = format!("{mark}{}", fl.text);
    let lines = text.lines().map(|l| (l.chars().count() as u16).div_ceil(inner_width).max(1)).sum::<u16>().clamp(1, 10);
    let height = lines + 2;
    let area = Rect { x: body.x + (body.width.saturating_sub(width)) / 2, y: body.bottom().saturating_sub(height), width, height };
    f.render_widget(Clear, area);
    let panel = Panel::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(color));
    f.render_widget(Paragraph::new(Text::styled(text, Style::new().fg(color).bold())).wrap(Wrap { trim: true }).block(panel), area);
}

fn draw_foot(f: &mut Frame, app: &App, area: Rect) {
    if let Some(input) = app.cmdline.as_ref() {
        let text = input.text();
        f.render_widget(Paragraph::new(Line::from(vec![Span::styled(":", Style::new().fg(ACCENT).bold()), Span::raw(text)])), area);
        let (_, (_, col)) = input.layout(usize::MAX / 2);
        f.set_cursor_position(Position { x: area.x + 1 + col as u16, y: area.y });
        return;
    }
    let width = area.width as usize;
    let help = vec![Span::styled("?", Style::new().fg(ACCENT).bold()), Span::styled(tr!(" help", " ayuda"), Style::new().fg(DIM))];
    let help_w: usize = help.iter().map(Span::width).sum();
    let mut spans = vec![Span::raw(" ")];
    let mut w = 1;
    for (key, desc) in footer_keys(app) {
        let piece = vec![Span::styled(key, Style::new().fg(ACCENT).bold()), Span::styled(format!(" {desc}   "), Style::new().fg(DIM))];
        let pw: usize = piece.iter().map(Span::width).sum();
        if w + pw + help_w > width {
            break;
        }
        w += pw;
        spans.extend(piece);
    }
    if app.composer.is_none() {
        spans.extend(help);
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

pub(super) fn help_lines() -> Vec<Line<'static>> {
    let section = |t: &str| Line::from(Span::styled(format!(" {t}"), Style::new().fg(ACCENT).bold()));
    let row = |k: &str, d: &str| Line::from(vec![Span::styled(format!("  {k:<16}"), Style::new().bold()), Span::styled(d.to_string(), Style::new().fg(SOFT))]);
    vec![
        section(tr!("Moving (vim style; arrows work too)", "Moverse (estilo vim; las flechas también valen)")),
        row("j/k  ↓↑", tr!("down / up; after answering you move to the next one", "abajo / arriba; al contestar pasas solo a la siguiente")),
        row("gg  Shift+G", tr!("first / last", "primero / último")),
        row("Ctrl-d  Ctrl-u", tr!("half a page down / up (scrolls the detail)", "media página abajo / arriba (desplaza el detalle)")),
        row("Ctrl-f  Ctrl-b", tr!("a full page down / up; also Space, PgDn, PgUp", "página entera abajo / arriba; también Espacio, AvPág, RePág")),
        row("h/l  ←→", tr!("focus the list / the detail pane", "enfocar la lista / el panel de detalle")),
        row("o  ⏎", tr!("expand: open the whole item", "expandir: abrir entero el elemento")),
        row("Esc", tr!("back: close, collapse or leave the detail pane", "volver: cerrar, plegar o salir del panel de detalle")),
        row(":", tr!("command line: :q  :help  :inbox  :tasks  :agents  :<number>", "línea de órdenes: :q  :help  :inbox  :tasks  :agents  :<número>")),
        Line::default(),
        section(tr!("Inbox", "Bandeja")),
        row("1-9", tr!("question: choose an option · delivery with a PR: authorize a merge method (twice to confirm)", "pregunta: elegir una opción · entrega con PR: autorizar un método de fusión (dos veces para confirmar)")),
        row("r", tr!("question: write the answer · delivery: send feedback", "pregunta: escribir la respuesta · entrega: enviar feedback")),
        row("v", tr!("accept a delivery; the agent still finishes the task", "aceptar una entrega; el agente sigue siendo quien termina la tarea")),
        row("x", tr!("mark information read; nothing is sent to the agent", "marcar información leída; no se envía nada al agente")),
        row("d · w", tr!("the PR's diff · the PR on GitHub", "el diff del PR · el PR en GitHub")),
        row("Shift+J/Shift+K", tr!("scroll the detail 5 lines", "desplazar el detalle 5 líneas")),
        row("a", tr!("open the agent's terminal (← on an empty line, Ctrl+Z or /exit returns to Alondra)", "abrir el terminal del agente (← con la línea vacía, Ctrl+Z o /exit vuelven a Alondra)")),
        Line::default(),
        section(tr!("Reading (after o)", "Lectura (tras o)")),
        row("j/k", tr!("next / previous block; long blocks scroll line by line", "bloque siguiente / anterior; los bloques largos avanzan línea a línea")),
        row("/  n  Shift+N", tr!("search the text · next match · previous match", "buscar en el texto · siguiente · anterior")),
        row("c · Shift+C · d", tr!("comment on block · general comment · delete comment", "comentar bloque · comentario general · borrar")),
        row("s · v · x", tr!("send comments as feedback · accept · mark read", "enviar comentarios como feedback · aceptar · leído")),
        row("q  o  Esc", tr!("close the reading", "cerrar la lectura")),
        Line::default(),
        section(tr!("Tasks", "Tareas")),
        row("o", tr!("expand the task's detail to full screen", "expandir el detalle de la tarea a pantalla completa")),
        row("⏎", tr!("its agent's terminal", "el terminal de su agente")),
        row("x", tr!("cancel the task, or withdraw a request without tasks (twice to confirm)", "descartar la tarea, o retirar una petición sin tareas (dos veces para confirmar)")),
        row("Shift+R", tr!("reopen a finished or cancelled task", "reabrir una tarea terminada o descartada")),
        row("Shift+A", tr!("archive every finished or cancelled task", "archivar todas las tareas terminadas o descartadas")),
        row("⏎ · o", tr!("on Finished or Archived: show or hide them", "en Terminadas o Archivadas: mostrar u ocultar")),
        Line::default(),
        section(tr!("Agents", "Agentes")),
        row("/", tr!("search agents by name", "buscar agentes por nombre")),
        row("⏎ · a", tr!("the agent's terminal", "el terminal del agente")),
        row("o", tr!("expand the agent's detail to full screen", "expandir el detalle del agente a pantalla completa")),
        Line::default(),
        section(tr!("Always", "Siempre")),
        row("Tab", tr!("inbox → tasks → agents (Shift+Tab goes back)", "bandeja → tareas → agentes (Shift+Tab vuelve)")),
        row("n", tr!("new work for the orchestrator", "trabajo nuevo para el orquestador")),
        row("q  Ctrl-c", tr!("quit", "salir")),
        row(tr!("typing", "al escribir"), tr!("letters go to the text box; Esc leaves it", "las letras van al cuadro de texto; Esc sale")),
    ]
}

fn draw_help(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let lines = help_lines();
    let w = 96.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    let view_h = h.saturating_sub(2) as usize;
    app.help_view = View { h: view_h, total: lines.len(), shown: true };
    app.help_scroll = app.help_scroll.min(lines.len().saturating_sub(view_h));
    let mut block = panel(Line::from(tr!(" Keys · j/k scroll · any other key closes ", " Teclas · j/k desplaza · cualquier otra tecla cierra ").bold()), true);
    if let Some(hint) = more_hint(app.help_scroll, view_h, lines.len()) {
        block = block.title_bottom(hint);
    }
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).scroll((app.help_scroll as u16, 0)).block(block), r);
}

fn draw_schedules(f: &mut Frame, app: &mut App, area: Rect) {
    let [list, detail] = Layout::horizontal([Constraint::Percentage(42), Constraint::Percentage(58)]).areas(area);
    let box_list = panel(Line::from(tr!(" Scheduled tasks ", " Tareas programadas ").bold()), true);
    let inner = box_list.inner(list);
    f.render_widget(box_list, list);
    if app.schedules.is_empty() {
        let lines = vec![
            Line::from(Span::styled(tr!(" No schedules yet", " Sin programaciones"), Style::new().fg(DIM))),
            Line::default(),
            Line::from(vec![keycap("n"), Span::raw(tr!(" create a schedule", " crear una programación"))]),
            Line::default(),
            Line::from(Span::styled(tr!("Standalone launchd crons are managed separately.", "Los cron independientes de launchd se gestionan aparte."), Style::new().fg(DIM))),
        ];
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    } else {
        let projects = app.schedules.iter().map(|s| schedule_project(s)).collect::<Vec<_>>();
        let mut names = Vec::<String>::new();
        for name in &projects { if !names.contains(name) { names.push(name.clone()); } }
        let mut lines = Vec::new();
        let mut selected_line = 0;
        for (group, project) in names.iter().enumerate() {
            if group > 0 { lines.push(Line::default()); }
            let count = projects.iter().filter(|p| *p == project).count();
            lines.push(Line::from(Span::styled(format!(" ◆ {project} ({count})"), Style::new().fg(Color::Cyan).bold())));
            for (i, schedule) in app.schedules.iter().enumerate().filter(|(i, _)| projects[*i] == *project) {
                let selected = i == app.schedule_idx;
                if selected { selected_line = lines.len(); }
                let enabled = schedule["enabled"].as_bool().unwrap_or(true);
                let failed = schedule["last_state"] == "failed" || schedule["status"] == "failed";
                let (glyph, color) = if failed { ("!", Color::LightRed) } else if enabled { ("●", Color::LightGreen) } else { ("○", DIM) };
                let title = schedule["title"].as_str().unwrap_or(tr!("Untitled", "Sin título"));
                let title = title.strip_prefix(&format!("{project} · ")).unwrap_or(title);
                let next = if enabled { schedule["next_run"].as_i64().map(|at| format!("{} {}", tr!("in", "en"), span_to(at - now_millis()))).unwrap_or_default() } else { tr!("paused", "pausada").to_string() };
                let width = inner.width as usize;
                let right = str_width(&next);
                let title_width = width.saturating_sub(right + 6);
                let title = truncate(title, title_width);
                let gap = width.saturating_sub(str_width(&title) + right + 5);
                lines.push(fit(vec![
                    Span::styled(if selected { "▌ " } else { "  " }, Style::new().fg(ACCENT)),
                    Span::styled(format!("{glyph} "), Style::new().fg(color).bold()),
                    Span::styled(title, Style::new().fg(if enabled { Color::Reset } else { DIM })),
                    Span::raw(" ".repeat(gap)),
                    Span::styled(next, Style::new().fg(if failed { Color::LightRed } else { DIM })),
                ], width));
            }
        }
        let height = inner.height as usize;
        if selected_line < app.schedule_scroll { app.schedule_scroll = selected_line; }
        if selected_line >= app.schedule_scroll + height { app.schedule_scroll = selected_line + 1 - height; }
        app.schedule_scroll = app.schedule_scroll.min(lines.len().saturating_sub(height));
        let total = lines.len();
        f.render_widget(Paragraph::new(lines).scroll((app.schedule_scroll as u16, 0)), inner);
        scrollbar(f, inner, total, app.schedule_scroll);
    }
    let box_detail = panel(Line::from(tr!(" Schedule ", " Programación ").bold()), false).title_bottom(Line::from(vec![keycap("⏎"), Span::raw(tr!(" run ", " ejecutar ")), keycap("p"), Span::raw(tr!(" pause ", " pausar ")), keycap("e"), Span::raw(tr!(" edit ", " editar "))]).right_aligned());
    let inner = box_detail.inner(detail);
    f.render_widget(box_detail, detail);
    if let Some(s) = app.schedules.get(app.schedule_idx) {
        let frequency = match s["frequency"].as_str() {
            Some("weekly") => tr!("Weekly", "Semanal").to_string(),
            Some("every_hours") => match s["interval"].as_u64().unwrap_or(1) { 1 => tr!("Every hour", "Cada hora").into(), n => format!("{} {n} {}", tr!("Every", "Cada"), tr!("hours", "horas")) },
            Some("every_days") => match s["interval"].as_u64().unwrap_or(1) { 1 => tr!("Every day", "Cada día").into(), n => format!("{} {n} {}", tr!("Every", "Cada"), tr!("days", "días")) },
            Some("every_months") => match s["interval"].as_u64().unwrap_or(1) { 1 => tr!("Every month", "Cada mes").into(), n => format!("{} {n} {}", tr!("Every", "Cada"), tr!("months", "meses")) },
            Some("once") => tr!("Once", "Una vez").to_string(),
            _ => tr!("Daily", "Diaria").to_string(),
        };
        let h = s["hour"].as_u64().unwrap_or(0);
        let m = s["minute"].as_u64().unwrap_or(0);
        let weekday = s["weekday"].as_u64().map(|d| {
            let name = match d {
                0 => tr!("Sun", "dom"), 1 => tr!("Mon", "lun"),
                2 => tr!("Tue", "mar"), 3 => tr!("Wed", "mié"),
                4 => tr!("Thu", "jue"), 5 => tr!("Fri", "vie"),
                6 => tr!("Sat", "sáb"), _ => "?",
            };
            format!(" · {name}")
        }).unwrap_or_default();
        let next = if s["status"] == "waiting" {
            tr!("when the orchestrator returns", "cuando vuelva el orquestador").to_string()
        } else {
            s["next_run"].as_i64().map(|at| format!("{} {}", tr!("in", "en"), span_to(at - now_millis()))).unwrap_or_else(|| {
                if s["frequency"] == "once" && s["last_run"].is_number() { tr!("completed", "terminada").into() }
                else { tr!("paused", "pausada").into() }
            })
        };
        let status = match s["last_state"].as_str().or_else(|| s["status"].as_str()) {
            Some("failed") => tr!("failed", "fallida"),
            Some("interrupted") => tr!("interrupted", "interrumpida"),
            Some("waiting") => tr!("waiting for orchestrator", "en espera del orquestador"),
            Some("sent") => tr!("sent", "enviada"),
            Some("done") => tr!("done", "terminada"),
            Some("in_progress") => tr!("in progress", "en marcha"),
            Some("defining") => tr!("preparing", "preparando"),
            _ => "",
        };
        let last = s["last_run"].as_i64().map(|at| format!("{} · {status}", ago(at))).unwrap_or_else(|| tr!("never", "nunca").into());
        let enabled = s["enabled"].as_bool().unwrap_or(true);
        let state = if enabled { tr!("● Active", "● Activa") } else { tr!("○ Paused", "○ Pausada") };
        let schedule = if matches!(s["frequency"].as_str(), Some("every_hours" | "every_days" | "every_months" | "once")) {
            frequency
        } else { format!("{frequency}{weekday} · {h:02}:{m:02}") };
        let label = |name: &'static str, value: String| Line::from(vec![Span::styled(format!("  {name:<11}"), Style::new().fg(DIM)), Span::styled(value, Style::new().fg(SOFT))]);
        let mut lines = vec![
            Line::from(Span::styled(format!(" {}", s["title"].as_str().unwrap_or("")), Style::new().bold())),
            Line::from(Span::styled(format!(" {state}"), Style::new().fg(if enabled { Color::LightGreen } else { DIM }))),
            Line::default(),
            Line::from(Span::styled(tr!(" ─ SCHEDULE", " ─ HORARIO"), Style::new().fg(Color::Cyan).bold())),
            label(tr!("Frequency", "Frecuencia"), schedule),
            label(tr!("Next", "Próxima"), next),
            label(tr!("Last", "Última"), last),
            label(tr!("Project", "Proyecto"), schedule_project(s)),
            Line::default(),
        ];
        if let Some(error) = s["last_error"].as_str() {
            let error = schedule_error(error);
            lines.push(Line::from(error.to_string().red()));
        }
        if let Some(request) = s["last_request"].as_str() { lines.push(Line::from(format!("{}: {request}", tr!("Request", "Petición")))); }
        if let Some(nodes) = s["last_nodes"].as_array() { for node in nodes { if let Some(node) = node.as_str() { lines.push(Line::from(format!("{}: {node}", tr!("Node", "Nodo")))); } } }
        if let Some(text) = s["text"].as_str().filter(|text| !text.trim().is_empty()) {
            lines.push(Line::from(Span::styled(tr!(" ─ TASK", " ─ ENCARGO"), Style::new().fg(Color::Cyan).bold())));
            lines.push(Line::from(format!(" {text}")));
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
    }
}

fn schedule_project(schedule: &serde_json::Value) -> String {
    let project = schedule["project"].as_str().filter(|name| !name.trim().is_empty());
    match project {
        Some("alondra") => "Alondra".into(),
        Some("kestrel") => "Kestrel".into(),
        Some(name) => name.to_string(),
        None => tr!("Other", "Otros").into(),
    }
}

fn span_to(ms: i64) -> String {
    let minutes = (ms / 60_000).max(0);
    if minutes < 60 { format!("{minutes}m") } else if minutes < 1440 { format!("{}h", minutes / 60) } else { format!("{}d", minutes / 1440) }
}

fn schedule_error(error: &str) -> &str {
    match error {
        "project must be one of the available project folders" => tr!("Choose an available project", "Elige un proyecto disponible"),
        "projects directory does not exist" => tr!("The Projects folder is unavailable", "La carpeta Proyectos no está disponible"),
        "schedule project directory does not exist" => tr!("The project folder is unavailable", "La carpeta del proyecto no está disponible"),
        "the orchestrator is not connected to Alondra" => tr!("The orchestrator is offline", "El orquestador no está conectado"),
        _ => error,
    }
}
