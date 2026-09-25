mod tasks;

use std::collections::HashMap;

use alondra_core::tasks::{BlockerKind, TaskState};
use alondra_core::{NodeState, Question, QuestionOrigin, Report, ReportKind, now_millis};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block as Panel, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use super::md::{self, str_width, truncate, wrap};
use super::{App, FleetRow, Item, Key, Pane, Remote, Screen, Target, View, said, who};
use crate::client::{ClientTask, FleetNode};
use crate::i18n::{self, Occupancy};

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
            let preview = c.chips.iter().rev().find(|c| app.previews.contains_key(&c.seq)).map(|c| c.seq);
            let preview_h = if preview.is_some() { PREVIEW_ROWS } else { 0 };
            let h = (composer_height(&c.input, body.width) + chips_height(&c.chips) + form_height(c.form.as_ref()) + preview_h).min(body.height.saturating_sub(3));
            let [main, comp] = Layout::vertical([Constraint::Min(3), Constraint::Length(h)]).areas(body);
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
    } else {
        match app.screen {
            Screen::Inbox => draw_inbox(f, app, body),
            Screen::Tasks => tasks::draw_tasks(f, app, body),
            Screen::Fleet => draw_fleet(f, app, body),
        }
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

fn since_phrase(ms: i64) -> String {
    let t = ago(ms);
    if now_millis() - ms < 60_000 { tr!("since just now", "desde ahora mismo").into() } else { trf!("for {t}", "desde hace {t}") }
}

fn ago_phrase(ms: i64) -> String {
    if now_millis() - ms < 60_000 {
        return tr!("just now", "ahora mismo").into();
    }
    let t = ago(ms);
    trf!("{t} ago", "hace {t}")
}

fn state_color(s: NodeState) -> Color {
    match s {
        NodeState::WaitingAnswer => Color::Yellow,
        NodeState::YourTurn => Color::LightYellow,
        NodeState::WaitingMerge => Color::LightGreen,
        NodeState::Unclear => Color::Magenta,
        NodeState::Stalled => Color::LightRed,
        NodeState::Delivered => Color::Cyan,
        NodeState::WaitingMachine => Color::LightRed,
        NodeState::Working => Color::Green,
        NodeState::Done => DIM,
    }
}

/// The orchestrator keeps its node state's colour; every other node is busy, free or waiting.
fn node_color(n: &FleetNode) -> Color {
    if n.is_orchestrator {
        return state_color(n.state);
    }
    match Occupancy::of(n) {
        Occupancy::Busy => Color::Green,
        Occupancy::Free => SOFT,
        Occupancy::WaitingMachine => Color::LightRed,
    }
}

pub(super) fn state_glyph(s: TaskState) -> (&'static str, Color) {
    match s {
        TaskState::WaitingDependency => ("◌", DIM),
        TaskState::Running => ("●", Color::Green),
        TaskState::NeedsYou => ("?", Color::Yellow),
        TaskState::InReview => ("⇄", Color::LightGreen),
        TaskState::Closing => ("⇉", Color::Cyan),
        TaskState::Done => ("✓", Color::LightGreen),
        TaskState::Cancelled => ("✗", DIM),
    }
}

/// An action the user has to carry out reads as "your turn", like its card.
pub(super) fn task_glyph(t: &ClientTask) -> (&'static str, Color) {
    if t.state == TaskState::NeedsYou && t.blocker.is_some_and(|b| b.kind == BlockerKind::Action) {
        return ("▶", ACCENT);
    }
    state_glyph(t.state)
}

const DEFINING: (&str, Color) = ("◇", Color::LightMagenta);

pub(super) fn question_glyph(q: &Question) -> (&'static str, &'static str, Color) {
    if q.action {
        return ("▶", tr!("your turn", "te toca"), Color::LightYellow);
    }
    match q.origin {
        QuestionOrigin::Plan => ("◆", tr!("plan", "plan"), Color::LightMagenta),
        QuestionOrigin::Permission => ("!", tr!("permission", "permiso"), Color::LightRed),
        _ => ("?", tr!("question", "pregunta"), ACCENT),
    }
}

pub(super) fn report_glyph(r: &Report) -> (&'static str, &'static str, Color) {
    if r.looks_like_question && r.kind != ReportKind::Final {
        return ("?", tr!("notice that asks", "aviso que pregunta"), Color::LightYellow);
    }
    match r.kind {
        ReportKind::Document => ("▤", i18n::report_kind(r.kind), Color::Cyan),
        ReportKind::Final => ("✓", i18n::report_kind(r.kind), Color::LightGreen),
        _ => ("•", i18n::report_kind(r.kind), Color::LightBlue),
    }
}

fn item_glyph(item: &Item) -> (&'static str, &'static str, Color) {
    match item {
        Item::Q(q) => question_glyph(q),
        Item::R(r) => report_glyph(r),
        Item::P(_) => pr_glyph(),
    }
}

pub(super) fn pr_glyph() -> (&'static str, &'static str, Color) {
    ("⇄", "PR", Color::LightGreen)
}

/// `bitomule/alondra #12 · main ← menus · +120 −8 in 5 files · checks 2✓ 1✗ 1…`
pub(super) fn pr_summary(p: &crate::client::PrCard) -> String {
    use alondra_core::pr::CheckState;
    let pr = &p.pr;
    let mut s = format!("{} #{}", pr.repo, pr.number);
    if let (Some(base), Some(head)) = (&pr.base, &pr.head) {
        s.push_str(&format!(" · {base} ← {head}"));
    }
    if let Some(below) = p.below_number {
        s.push_str(&trf!(" · on top of PR #{below}", " · va encima del PR #{below}"));
    }
    if pr.fetched {
        let (a, d, n) = (pr.additions, pr.deletions, pr.changed_files);
        s.push_str(&trf!(" · +{a} −{d} in {n} files", " · +{a} −{d} en {n} ficheros"));
    }
    let count = |st: CheckState| pr.checks.iter().filter(|c| c.state == st).count();
    if !pr.checks.is_empty() {
        let (ok, bad, run) = (count(CheckState::Passed), count(CheckState::Failed), count(CheckState::Running));
        s.push_str(&trf!(" · checks {ok}✓ {bad}✗ {run}…", " · checks {ok}✓ {bad}✗ {run}…"));
    }
    if pr.conflicts() {
        s.push_str(tr!(" · conflicts", " · conflictos"));
    }
    s
}

fn draw_head(f: &mut Frame, app: &App, area: Rect) {
    let m = &app.data.machine;
    let tab = |label: String, active: bool| {
        if active {
            Span::styled(format!(" {label} "), Style::new().fg(Color::Black).bg(SOFT).add_modifier(Modifier::BOLD))
        } else {
            Span::styled(format!(" {label} "), Style::new().fg(SOFT))
        }
    };
    let mut left = vec![Span::styled(" Alondra ", Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD)), Span::raw(" ")];
    let (inbox, tasks, fleet) = (app.inbox_count(), app.open_task_count(), app.data.nodes.len());
    left.push(tab(trf!("Inbox {inbox}", "Bandeja {inbox}"), app.screen == Screen::Inbox));
    left.push(tab(trf!("Tasks {tasks}", "Tareas {tasks}"), app.screen == Screen::Tasks));
    left.push(tab(trf!("Fleet {fleet}", "Flota {fleet}"), app.screen == Screen::Fleet));
    if app.offline {
        left.push(Span::raw(" "));
        left.push(Span::styled(tr!(" no connection to alondrad ", " sin conexión con alondrad "), Style::new().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD)));
    }
    let working = app.data.nodes.iter().filter(|n| n.state == NodeState::Working).count();
    let lw: usize = left.iter().map(Span::width).sum();
    f.render_widget(Paragraph::new(Line::from(left)), area);
    if !app.loaded {
        return;
    }
    let reads = readings(m, app.data.usage.as_ref());
    let fits = |spans: &[Span]| lw + spans.iter().map(Span::width).sum::<usize>() + 2 <= area.width as usize;
    let right = [gauges(&reads, working, true), gauges(&reads, working, false)].into_iter().find(|r| fits(r));
    if let Some(right) = right {
        f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    }
}

/// The header bars, named when there is room (`Claude week ▰▰▱▱ 53%`) and bare when not.
fn gauges(reads: &[Reading], working: usize, named: bool) -> Vec<Span<'static>> {
    let mut out = vec![Span::styled(trf!("{working} working ", "{working} trabajando "), Style::new().fg(DIM))];
    let mut group = None;
    for r in reads {
        let color = match r.level {
            Level::Calm => ACCENT,
            Level::Warm => Color::Yellow,
            Level::Hot => Color::LightRed,
        };
        let new_group = group != Some(r.group);
        let gap = match (named, group.is_some() && new_group) {
            (false, _) => " ",
            (true, true) => " │ ",
            (true, false) => "  ",
        };
        out.push(Span::styled(gap, Style::new().fg(DIM)));
        if named {
            if new_group {
                out.push(Span::styled(format!("{} ", r.group), Style::new().fg(SOFT).add_modifier(Modifier::BOLD)));
            }
            out.push(Span::styled(format!("{} ", r.name), Style::new().fg(SOFT)));
        }
        group = Some(r.group);
        let filled = ((r.value.clamp(0.0, 1.0) * BAR_CELLS as f64).round() as usize).max(1);
        out.push(Span::styled("▰".repeat(filled), Style::new().fg(color)));
        out.push(Span::styled("▱".repeat(BAR_CELLS - filled), Style::new().fg(DIM)));
        let label_style = if r.level == Level::Hot { Style::new().fg(Color::LightRed).add_modifier(Modifier::BOLD) } else { Style::new().fg(DIM) };
        out.push(Span::styled(format!(" {}", if named { &r.label } else { &r.short }), label_style));
    }
    out.push(Span::raw(" "));
    out
}

const BAR_CELLS: usize = 4;

#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum Level {
    Calm,
    Warm,
    Hot,
}

#[derive(Debug, PartialEq)]
pub(super) struct Reading {
    pub group: &'static str,
    pub name: &'static str,
    pub value: f64,
    pub label: String,
    pub short: String,
    pub level: Level,
}

/// The header bars, always in this order: the week's Claude usage, today against its share of
/// the week, memory in use and CPU in use. `?` explains them.
pub(super) fn readings(m: &alondra_core::Machine, u: Option<&alondra_core::usage::Usage>) -> Vec<Reading> {
    let mut out = vec![];
    if let Some(u) = u {
        let week = format!("{}%", u.week_percent.round());
        out.push(Reading { group: "Claude", name: tr!("week", "semana"), value: u.week_percent / 100.0, short: week.clone(), label: week, level: if u.week_percent >= 85.0 { Level::Hot } else { Level::Calm } });
        if let Some((spent, share)) = u.today_spent.zip(u.today_share) {
            let level = if u.over_pace { Level::Hot } else if spent >= share * 0.75 { Level::Warm } else { Level::Calm };
            let (spent, share) = (spent.round(), share.round());
            out.push(Reading {
                group: "Claude",
                name: tr!("today", "hoy"),
                value: spent / share.max(0.1),
                label: trf!("{spent}% of {share}%", "{spent}% de {share}%"),
                short: format!("{spent}/{share}%"),
                level,
            });
        }
    }
    if let Some(free) = m.memory_free_percent {
        let used = 100 - free.min(100);
        let level = if free < 15 { Level::Hot } else if free < 30 { Level::Warm } else { Level::Calm };
        out.push(Reading { group: "Mac", name: "RAM", value: used as f64 / 100.0, label: format!("{used}%"), short: format!("{used}%"), level });
    }
    let cpu = m.cpu_usage as f64;
    let level = if m.unreliable { Level::Hot } else if cpu >= 85.0 { Level::Warm } else { Level::Calm };
    out.push(Reading { group: "Mac", name: "CPU", value: cpu / 100.0, label: format!("{}%", cpu.round()), short: format!("{}%", cpu.round()), level });
    out
}

fn draw_inbox(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width;
    app.detail_view.shown = w >= 84;
    if w >= 170 {
        let [list, detail, fleet] = Layout::horizontal([Constraint::Length(50), Constraint::Min(40), Constraint::Length(40)]).areas(area);
        draw_inbox_list(f, app, list);
        draw_item_detail(f, app, detail);
        draw_compact_fleet(f, app, fleet);
    } else if w >= 84 {
        let lw = (w * 2 / 5).clamp(36, 56);
        let [list, detail] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        draw_inbox_list(f, app, list);
        draw_item_detail(f, app, detail);
    } else {
        draw_inbox_list(f, app, area);
    }
}

fn centered_lines(f: &mut Frame, area: Rect, lines: Vec<Line<'static>>) {
    let w = area.width.max(1) as usize;
    let h = lines.iter().map(|l| l.width().div_ceil(w).max(1)).sum::<usize>() as u16;
    let top = area.y + area.height.saturating_sub(h) / 2;
    let r = Rect { x: area.x, y: top, width: area.width, height: h.min(area.height) };
    f.render_widget(Paragraph::new(lines).centered().wrap(Wrap { trim: true }), r);
}

/// One inbox row; a question inside a PR card is drawn indented under it.
fn item_lines(app: &App, key: Key, width: usize, selected: bool, nested: bool) -> Vec<Line<'static>> {
    let Some(item) = app.item(key) else { return vec![] };
    let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let (hook, gap) = if nested { (Span::styled("  ↳ ", Style::new().fg(Color::Magenta)), Span::raw("    ")) } else { (Span::raw(""), Span::raw("")) };
    let (glyph, _, color) = item_glyph(&item);
    let title_style = if selected { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let pending = app.reviews.get(&key).map(|r| r.count()).unwrap_or(0);
    let (title, node, at) = match &item {
        Item::Q(q) => (q.title.clone(), who(&q.node_name, &q.session_id), q.asked_at),
        Item::R(r) => (r.title.clone(), who(&r.node_name, &r.session_id), r.created_at),
        Item::P(p) => (p.pr.title.clone(), who(&p.pr.node_name, &p.pr.session_id), p.pr.opened_at),
    };
    let first = fit(vec![bar.clone(), hook, Span::styled(format!("{glyph} "), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(md::one_line(&title), title_style)], width);
    let mut meta = vec![bar, gap, Span::styled(format!("  {node} · {}", ago(at)), Style::new().fg(DIM))];
    if pending > 0 {
        meta.push(Span::styled(format!(" · ✎{pending}"), Style::new().fg(COMMENT)));
    }
    match &item {
        Item::Q(q) => {
            for (i, o) in q.options.iter().enumerate().take(9) {
                meta.push(Span::styled(format!("  {}", i + 1), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
                meta.push(Span::styled(format!(" {}", md::one_line(i18n::option_label(&o.label))), Style::new().fg(SOFT)));
            }
        }
        Item::R(r) => {
            let clips = r.attachments.len();
            if clips > 0 {
                meta.push(Span::styled(format!(" · 📎{clips}"), Style::new().fg(DIM)));
            }
        }
        Item::P(p) => {
            let failed = p.pr.checks.iter().any(|c| c.state == alondra_core::pr::CheckState::Failed);
            let (mark, color) = if failed { ("✗", Color::LightRed) } else if p.pr.checks.iter().any(|c| c.state == alondra_core::pr::CheckState::Running) { ("…", Color::Yellow) } else { ("✓", Color::LightGreen) };
            meta.push(Span::styled(format!(" · {} #{} ", p.pr.repo, p.pr.number), Style::new().fg(DIM)));
            if !p.pr.checks.is_empty() {
                meta.push(Span::styled(mark, Style::new().fg(color)));
            }
            if let Some(below) = p.below_number {
                meta.push(Span::styled(trf!(" ↑ on top of PR #{below}", " ↑ va encima del PR #{below}"), Style::new().fg(DIM)));
            }
        }
    }
    vec![first, fit(meta, width)]
}

fn draw_inbox_list(f: &mut Frame, app: &mut App, area: Rect) {
    let keys = app.inbox_keys();
    let focused = app.focus == Pane::List || !app.detail_view.shown;
    let block = panel(Line::from(tr!(" Inbox ", " Bandeja ").bold()), focused);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !app.loaded {
        let msg = if app.offline { tr!("alondrad is not responding · retrying…", "alondrad no responde · reintentando…") } else { tr!("Connecting to alondrad…", "Conectando con alondrad…") };
        centered_lines(f, inner, vec![Line::from(msg.fg(DIM))]);
        return;
    }
    if keys.is_empty() {
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
    let nested: Vec<bool> = keys.iter().map(|k| app.parent(*k).is_some()).collect();
    type Section = (fn(Key, bool) -> bool, &'static str, Color);
    let sections: [Section; 3] = [
        (|k, nested| matches!(k, Key::Q(_)) && !nested, tr!("WAITING FOR YOU", "TE ESPERAN"), ACCENT),
        (|k, nested| matches!(k, Key::P(_)) || nested, tr!("PULL REQUESTS", "PULL REQUESTS"), Color::Magenta),
        (|k, nested| matches!(k, Key::R(_)) && !nested, tr!("TO READ", "PARA LEER"), Color::Cyan),
    ];
    for (belongs, label, color) in sections {
        let members: Vec<(Key, bool)> = keys.iter().zip(&nested).map(|(k, n)| (*k, *n)).filter(|(k, n)| belongs(*k, *n)).collect();
        let count = members.iter().filter(|(_, n)| !n).count();
        if count == 0 {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![Span::styled(format!(" {label} "), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(count.to_string(), Style::new().fg(DIM))]));
        for (k, inside) in members {
            let selected = app.sel == Some(k);
            let start = lines.len();
            lines.extend(item_lines(app, k, width, selected, inside));
            if selected {
                sel_range = Some((start, lines.len()));
            }
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

fn carried_report(r: &Report, width: usize) -> Vec<Line<'static>> {
    let mut lines = wrap(vec![Span::styled(r.title.clone(), Style::new().bold())], width, vec![], vec![]);
    if let Some(body) = r.body.as_deref().filter(|b| !b.trim().is_empty()) {
        lines.extend(md::render(&md::parse(body), width).lines);
    }
    lines
}

/// Where an image goes in the detail text: the first of the blank rows kept for it, how many,
/// and the stored path it was loaded from.
struct ImageSlot {
    line: usize,
    rows: usize,
    path: String,
}

const IMAGE_MAX_ROWS: usize = 20;

/// Rows an image takes at `width` columns, keeping its proportions (a cell is about twice as
/// tall as it is wide).
fn image_rows(img: &image::DynamicImage, width: usize) -> usize {
    let rows = (width as f64 * img.height() as f64 / img.width().max(1) as f64 / 2.0).ceil() as usize;
    rows.clamp(4, IMAGE_MAX_ROWS)
}

fn detail(app: &App, item: &Item, width: usize) -> (Vec<Line<'static>>, Vec<ImageSlot>) {
    let (glyph, kind, color) = item_glyph(item);
    let mut lines = vec![];
    let mut slots = vec![];
    let (no_files, no_links) = (vec![], vec![]);
    let (title, node, at, body, attachments, links) = match item {
        Item::Q(q) => (&q.title, who(&q.node_name, &q.session_id), q.asked_at, &q.detail, &q.attachments, &q.links),
        Item::R(r) => (&r.title, who(&r.node_name, &r.session_id), r.created_at, &r.body, &r.attachments, &r.links),
        Item::P(p) => (&p.pr.title, who(&p.pr.node_name, &p.pr.session_id), p.pr.opened_at, &p.pr.body, &no_files, &no_links),
    };
    let project = match item {
        Item::Q(q) => app.node(&q.session_id),
        Item::R(r) => app.node(&r.session_id),
        Item::P(p) => app.node(&p.pr.session_id),
    }
    .and_then(|n| n.project.clone());
    let mut meta = vec![Span::styled(format!("{glyph} {kind}"), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(format!(" · {node}"), Style::new().fg(SOFT))];
    if let Some(p) = project.filter(|p| !node.starts_with(p.as_str())) {
        meta.push(Span::styled(format!(" · {p}"), Style::new().fg(DIM)));
    }
    meta.push(Span::styled(format!(" · {}", ago_phrase(at)), Style::new().fg(DIM)));
    lines.push(fit(meta, width));
    if let Some(task) = app.card_task_title(item) {
        let label = Span::styled(tr!("Task: ", "Tarea: "), Style::new().fg(DIM).add_modifier(Modifier::BOLD));
        lines.extend(wrap(vec![Span::styled(md::one_line(&task), Style::new().fg(SOFT))], width, vec![label], vec![Span::raw("  ")]));
    }
    let mission = match item {
        Item::Q(q) => q.mission.as_deref(),
        Item::R(r) => r.mission.as_deref(),
        Item::P(_) => None,
    };
    if let Some(m) = mission.map(mission_excerpt).filter(|m| !m.is_empty()) {
        let label = Span::styled(tr!("Mission: ", "Misión: "), Style::new().fg(DIM).add_modifier(Modifier::BOLD));
        lines.extend(wrap(vec![Span::styled(m, Style::new().fg(DIM))], width, vec![label], vec![Span::raw("  ")]));
    }
    lines.push(Line::default());
    lines.extend(wrap(vec![Span::styled(md::one_line(title), Style::new().add_modifier(Modifier::BOLD))], width, vec![], vec![]));
    if let Item::P(p) = item {
        use alondra_core::pr::CheckState;
        lines.push(Line::default());
        lines.extend(wrap(vec![Span::styled(pr_summary(p), Style::new().fg(SOFT))], width, vec![], vec![]));
        for c in &p.pr.checks {
            let (mark, color) = match c.state {
                CheckState::Passed => ("✓", Color::LightGreen),
                CheckState::Failed => ("✗", Color::LightRed),
                CheckState::Running => ("…", Color::Yellow),
                CheckState::Other => ("·", DIM),
            };
            lines.push(fit(vec![Span::styled(format!("  {mark} "), Style::new().fg(color)), Span::raw(c.name.clone())], width));
        }
        lines.push(fit(vec![Span::styled(p.pr.url.clone(), Style::new().fg(Color::LightBlue).underlined())], width));
        for q in app.asked_on(p.pr.id) {
            let label = Span::styled(tr!("The node asks: ", "El nodo pregunta: "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD));
            lines.push(Line::default());
            lines.extend(wrap(vec![Span::styled(md::one_line(&q.title), Style::new().add_modifier(Modifier::BOLD))], width, vec![label], vec![Span::raw("  ")]));
        }
        for r in app.reported_on(p.pr.id) {
            let label = Span::styled(tr!("The node says: ", "El nodo dice: "), Style::new().fg(Color::Cyan).add_modifier(Modifier::BOLD));
            lines.push(Line::default());
            lines.extend(wrap(vec![Span::styled(md::one_line(&r.title), Style::new().add_modifier(Modifier::BOLD))], width, vec![label], vec![Span::raw("  ")]));
        }
    }
    if let Item::Q(q) = item
        && !q.steps.is_empty()
    {
        let steps: String = q.steps.iter().enumerate().map(|(i, s)| format!("{}. {}\n", i + 1, s.trim())).collect();
        lines.push(Line::default());
        lines.extend(md::render(&md::parse(&steps), width).lines);
    }
    if let Some(b) = body.as_ref().filter(|b| !b.trim().is_empty()) {
        lines.push(Line::default());
        lines.extend(md::render(&md::parse(b), width).lines);
    }
    for a in attachments {
        lines.push(Line::default());
        lines.push(md::render(&[md::Block::label(&format!("📎 {}", a.name))], width).lines.remove(0));
        match a.stored_path.as_ref().and_then(|p| app.files.get(p).map(|f| (p, f))) {
            Some((_, Ok(super::files::FileBody::Text(text)))) => {
                lines.push(Line::default());
                lines.extend(md::render(&md::parse(text), width).lines);
            }
            Some((path, Ok(body @ super::files::FileBody::Image(img)))) => {
                lines.push(Line::from(super::files::not_text(body).fg(DIM)));
                lines.push(Line::default());
                let rows = image_rows(img, width);
                slots.push(ImageSlot { line: lines.len(), rows, path: path.clone() });
                lines.extend(std::iter::repeat_n(Line::default(), rows));
            }
            Some((_, Ok(body))) => lines.push(Line::from(super::files::not_text(body).fg(DIM))),
            Some((_, Err(e))) => lines.push(Line::from(trf!("Could not load it: {e}", "No he podido cargarlo: {e}").fg(Color::LightRed))),
            None if a.stored_path.is_some() => lines.push(Line::from(tr!("Loading…", "Cargando…").fg(DIM))),
            None => {
                let src = &a.source_path;
                lines.push(Line::from(trf!("No copy was kept: {src}", "No se guardó copia: {src}").fg(DIM)));
            }
        }
    }
    if !links.is_empty() {
        lines.push(Line::default());
        for l in links {
            let t = l.title.clone().map(|t| format!("{t} ")).unwrap_or_default();
            lines.extend(wrap(vec![Span::raw(t), Span::styled(l.url.clone(), Style::new().fg(Color::LightBlue).underlined())], width, vec![Span::raw("↗ ")], vec![Span::raw("  ")]));
        }
    }
    if let Item::Q(q) = item
        && let Some(o) = app.data.open_questions.iter().find(|o| o.id == q.id)
    {
        for r in o.about() {
            lines.push(Line::default());
            let id = r.id;
            lines.push(Line::from(trf!("About report #{id}", "Sobre el informe #{id}").fg(DIM).bold()));
            lines.extend(carried_report(r, width));
        }
    }
    if let Item::R(r) = item
        && let Some(n) = app.data.news.iter().find(|n| n.id == r.id).filter(|n| !n.earlier.is_empty())
    {
        lines.push(Line::default());
        let count = n.earlier.len();
        lines.push(Line::from(trf!("Earlier in this delivery ({count})", "Antes en esta entrega ({count})").fg(DIM).bold()));
        for e in &n.earlier {
            lines.push(Line::default());
            lines.extend(carried_report(e, width));
        }
    }
    if let Item::Q(q) = item
        && let Some(c) = q.node_context.as_ref().filter(|c| c.trim().contains(char::is_whitespace))
    {
        lines.push(Line::default());
        lines.push(Line::from(tr!("What the node was doing", "Lo que hacía el nodo").fg(DIM).bold()));
        for l in c.lines() {
            lines.extend(wrap(vec![Span::styled(l.to_string(), Style::new().fg(DIM))], width, vec![], vec![]));
        }
    }
    (lines, slots)
}

const MISSION_MAX: usize = 240;

/// A mission is the whole launch prompt; on a card its opening says enough about why.
fn mission_excerpt(mission: &str) -> String {
    let flat = md::one_line(mission);
    if flat.chars().count() <= MISSION_MAX {
        return flat;
    }
    let cut: String = flat.chars().take(MISSION_MAX).collect();
    format!("{}…", cut.trim_end())
}

fn pr_keys(width: usize) -> Line<'static> {
    fit(
        vec![
            keycap("d"),
            Span::styled(tr!(" diff and comment (tuicr)  ", " diff y comentarios (tuicr)  "), Style::new().fg(SOFT)),
            keycap("r"),
            Span::styled(tr!(" request changes  ", " pedir cambios  "), Style::new().fg(SOFT)),
            keycap("w"),
            Span::styled(" GitHub  ", Style::new().fg(SOFT)),
            keycap("x"),
            Span::styled(tr!(" set aside", " apartar"), Style::new().fg(SOFT)),
        ],
        width,
    )
}

fn action_lines(app: &App, item: &Item, width: usize) -> Vec<Line<'static>> {
    let hint = |spans: Vec<Span<'static>>| fit(spans, width);
    match item {
        Item::P(p) if p.merge_blocked().is_some() => {
            vec![hint(vec![Span::styled(p.merge_blocked().unwrap_or_default(), Style::new().fg(DIM))]), pr_keys(width)]
        }
        Item::P(p) => {
            let mut out: Vec<Line> = vec![];
            if p.pr.conflicts() {
                let base = p.pr.base.clone().unwrap_or_else(|| tr!("its base", "su base").to_string());
                out.push(fit(vec![Span::styled(trf!("Conflicts with {base}: it cannot be merged", "Conflictos con {base}: no se puede fusionar"), Style::new().fg(Color::LightRed))], width));
            } else if p.pr.mergeability_unknown() && p.pr.fetched {
                out.push(fit(vec![Span::styled(tr!("GitHub is checking whether it merges…", "GitHub está comprobando si se puede fusionar…"), Style::new().fg(DIM))], width));
            }
            out.extend(p.actions().into_iter().enumerate().map(|(i, action)| {
                let label = match action {
                    crate::client::PrAction::Merge(m) => {
                        let method = i18n::merge_method(m);
                        trf!(" merge: {method}", " fusionar: {method}")
                    }
                    crate::client::PrAction::ResolveConflicts => tr!(" resolve conflicts (its node does it)", " resolver conflictos (lo hace su nodo)").to_string(),
                };
                let mut spans = vec![keycap(&(i + 1).to_string()), Span::styled(label, Style::new().add_modifier(Modifier::BOLD))];
                if let crate::client::PrAction::Merge(m) = action
                    && i == 0
                    && p.preferred_method == Some(m)
                {
                    spans.push(Span::styled(tr!("  last used here", "  el último usado aquí"), Style::new().fg(DIM)));
                }
                fit(spans, width)
            }));
            out.push(pr_keys(width));
            out
        }
        Item::Q(q) => {
            let mut out: Vec<Line> = q
                .options
                .iter()
                .enumerate()
                .take(9)
                .map(|(i, o)| {
                    let mut spans = vec![keycap(&(i + 1).to_string()), Span::styled(format!(" {}", md::one_line(i18n::option_label(&o.label))), Style::new().add_modifier(Modifier::BOLD))];
                    if let Some(d) = o.description.as_ref().filter(|d| !d.trim().is_empty()) {
                        spans.push(Span::styled(format!("  {}", md::one_line(i18n::option_label(d))), Style::new().fg(DIM)));
                    }
                    fit(spans, width)
                })
                .collect();
            let review = super::approval_of(q).is_some() || q.attachments.iter().any(|a| a.stored_path.is_some());
            let open = if review { tr!(" review and comment  ", " revisar y comentar  ") } else { tr!(" read all  ", " leer entero  ") };
            let mut tail = vec![keycap("o"), Span::styled(open, Style::new().fg(SOFT))];
            tail.extend([keycap("r"), Span::styled(tr!(" reply  ", " responder  "), Style::new().fg(SOFT)), keycap("c"), Span::styled(tr!(" more context", " más contexto"), Style::new().fg(SOFT))]);
            if q.options.is_empty() {
                out.push(Line::from(tr!("No options: answer in your own words.", "Sin opciones: contesta con tus palabras.").fg(DIM)));
            }
            out.push(hint(tail));
            out
        }
        Item::R(r) => {
            let pending = app.reviews.get(&super::Key::R(r.id)).map(|rv| rv.count()).unwrap_or(0);
            let read = if r.kind == ReportKind::Document || r.attachments.iter().any(|a| a.stored_path.is_some()) {
                tr!(" read and comment  ", " leer y comentar  ")
            } else {
                tr!(" read all  ", " leer entero  ")
            };
            let mut spans = vec![
                keycap("o"),
                Span::styled(read, Style::new().fg(SOFT)),
                keycap("x"),
                Span::styled(tr!(" mark read  ", " leído  "), Style::new().fg(SOFT)),
                keycap("r"),
                Span::styled(tr!(" reply  ", " responder  "), Style::new().fg(SOFT)),
            ];
            if pending > 0 {
                spans.push(Span::styled(trf!("  ✎{pending} unsent", "  ✎{pending} sin enviar"), Style::new().fg(COMMENT)));
            }
            let mut out = vec![];
            let artifacts = alondra_core::artifact::in_report(r).len();
            if artifacts > 0 {
                let mut open = vec![
                    keycap("b"),
                    Span::styled(format!(" {}  ", i18n::open_in_browser()), Style::new().add_modifier(Modifier::BOLD)),
                    keycap("c"),
                    Span::styled(format!(" {}  ", i18n::rescue_comments()), Style::new().add_modifier(Modifier::BOLD)),
                    keycap("v"),
                    Span::styled(format!(" {}", i18n::approve_artifact()), Style::new().add_modifier(Modifier::BOLD)),
                ];
                if artifacts > 1 {
                    open.push(Span::styled(trf!("  {artifacts} artifacts", "  {artifacts} artefactos"), Style::new().fg(DIM)));
                }
                out.push(fit(open, width));
            }
            out.push(hint(spans));
            out
        }
    }
}

fn draw_item_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let item = app.sel.and_then(|k| app.item(k));
    let Some(item) = item else {
        app.detail_view.total = 0;
        let block = panel(Line::from(tr!(" Detail ", " Detalle ").fg(DIM)), false);
        let inner = block.inner(area);
        f.render_widget(block, area);
        if app.loaded {
            centered_lines(f, inner, fleet_summary(app));
        }
        return;
    };
    let (_, kind, color) = item_glyph(&item);
    let id = match &item {
        Item::Q(q) => format!("#{}", q.id),
        Item::R(r) => format!("#{}", r.id),
        Item::P(p) => format!("#{}", p.pr.number),
    };
    let mut block = panel(Line::from(vec![Span::raw(" "), Span::styled(kind, Style::new().fg(color).bold()), Span::styled(format!(" {id} "), Style::new().fg(DIM))]), app.focus == Pane::Detail);
    let inner = block.clone().inner(area);
    let width = inner.width.saturating_sub(2) as usize;
    let actions = action_lines(app, &item, width);
    let ah = (actions.len() as u16 + 1).min(inner.height / 2);
    let [content, bottom] = Layout::vertical([Constraint::Min(1), Constraint::Length(ah)]).areas(inner);
    let (lines, slots) = detail(app, &item, width);
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
    for slot in slots {
        let Some(top) = slot.line.checked_sub(app.detail_scroll).filter(|top| top + slot.rows <= h) else { continue };
        if let Some(state) = app.images.get_mut(&slot.path) {
            let at = Rect { y: text_area.y + top as u16, height: slot.rows as u16, ..text_area };
            f.render_stateful_widget(ratatui_image::StatefulImage::default(), at, state);
        }
    }
    scrollbar(f, content, total, app.detail_scroll);
    let mut bottom_lines = vec![Line::styled("─".repeat(inner.width as usize), Style::new().fg(DIM))];
    bottom_lines.extend(actions.into_iter().map(|l| {
        let mut spans = vec![Span::raw(" ")];
        spans.extend(l.spans);
        Line::from(spans)
    }));
    f.render_widget(Paragraph::new(bottom_lines), bottom);
}

fn fleet_summary(app: &App) -> Vec<Line<'static>> {
    let mut counts: Vec<(NodeState, usize)> = vec![];
    for n in &app.data.nodes {
        match counts.iter_mut().find(|(s, _)| *s == n.state) {
            Some((_, c)) => *c += 1,
            None => counts.push((n.state, 1)),
        }
    }
    let mut lines = vec![Line::from(tr!("The fleet right now", "La flota ahora").fg(DIM).bold()), Line::default()];
    if counts.is_empty() {
        lines.push(Line::from(tr!("No nodes yet.", "Todavía no hay nodos.").fg(DIM)));
    }
    for (s, c) in counts {
        lines.push(Line::from(vec![Span::styled("● ", Style::new().fg(state_color(s))), Span::raw(format!("{c} {}", i18n::state(s)))]));
    }
    lines
}

/// "2 subagentes · 1 shell", in the warning colour and marked when one may be hung.
fn background_badge(n: &FleetNode) -> Option<(String, Style)> {
    let text = i18n::background_badge(&n.background)?;
    if n.background.iter().any(|b| b.suspect) {
        Some((format!("{text} · {}", i18n::possibly_hung()), Style::new().fg(Color::LightRed)))
    } else {
        Some((text, Style::new().fg(DIM)))
    }
}

fn background_lines(n: &FleetNode, width: usize) -> Vec<Line<'static>> {
    if n.background.is_empty() {
        return vec![];
    }
    let mut lines = vec![Line::default(), Line::from(tr!("Running in the background", "En marcha en segundo plano").fg(DIM).bold())];
    for b in &n.background {
        let color = if b.suspect { Color::LightRed } else { ACCENT };
        let mut tail = vec![Span::styled(format!(" · {}", ago(b.started_at)), Style::new().fg(DIM))];
        if b.suspect {
            tail.push(Span::styled(format!(" · {}", i18n::possibly_hung()), Style::new().fg(Color::LightRed)));
        }
        let mut spans = vec![Span::raw(md::one_line(&b.label))];
        spans.extend(tail);
        lines.extend(wrap(spans, width, vec![Span::styled(format!("{} ", i18n::background_kind(b.kind)), Style::new().fg(color))], vec![]));
    }
    lines
}

fn node_row(n: &FleetNode, width: usize, selected: bool) -> Line<'static> {
    let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let state_w = if width >= 64 { 20 } else { 0 };
    let since = ago(n.state_since);
    let room = width.saturating_sub(4 + state_w + 5);
    let short = if n.is_orchestrator { format!("★ {}", n.display_name()) } else { n.display_name().to_string() };
    let short = md::one_line(&short);
    let badge_room = room.saturating_sub(str_width(&short).min(room / 2));
    let badge = background_badge(n).map(|(text, style)| (truncate(&format!(" {text}"), badge_room), style));
    let name_w = room.saturating_sub(badge.as_ref().map_or(0, |(t, _)| str_width(t)));
    let name = truncate(&short, name_w);
    let pad = name_w.saturating_sub(str_width(&name));
    let mut spans = vec![bar, Span::styled(" ● ", Style::new().fg(node_color(n))), Span::styled(name, Style::new())];
    if let Some((text, style)) = badge {
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::raw(" ".repeat(pad)));
    if state_w > 0 {
        spans.push(Span::styled(format!(" {:<19}", truncate(i18n::fleet_state(n), 19)), Style::new().fg(node_color(n))));
    }
    spans.push(Span::styled(format!("{since:>5}"), Style::new().fg(DIM)));
    fit(spans, width)
}

/// The orchestrator's own fixed strip, always at the top and never inside a project folder: it
/// is not a node among the stable-ordered fleet list, so it gets its own look instead of a
/// `FleetRow::Group` header.
fn draw_orchestrator_strip(f: &mut Frame, area: Rect, n: &FleetNode, selected: bool) -> Rect {
    let [strip, rest] = Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(area);
    let block = Panel::bordered().border_type(BorderType::Double).border_style(Style::new().fg(ACCENT));
    let inner = block.inner(strip);
    f.render_widget(block, strip);
    if selected {
        f.buffer_mut().set_style(inner, Style::new().bg(SEL_BG));
    }
    let width = inner.width.saturating_sub(1) as usize;
    let line = fit(
        vec![
            Span::styled(" ★ ", Style::new().fg(ACCENT).bold()),
            Span::styled(format!("{} ", tr!("Orchestrator", "Orquestador")), Style::new().bold()),
            Span::styled(format!("● {}", i18n::node_state(n)), Style::new().fg(state_color(n.state))),
            Span::styled(format!(" · {}", since_phrase(n.state_since)), Style::new().fg(DIM)),
        ],
        width,
    );
    f.render_widget(Paragraph::new(line), inner);
    rest
}

fn draw_fleet(f: &mut Frame, app: &mut App, area: Rect) {
    if app.fleet_zoom && app.selected_node().is_some() {
        draw_node_detail(f, app, area);
        return;
    }
    let orchestrator = app.orchestrator().cloned();
    let area = match &orchestrator {
        Some(o) => {
            let selected = app.fleet_sel.as_deref() == Some(o.session_id.as_str());
            draw_orchestrator_strip(f, area, o, selected)
        }
        None => area,
    };
    app.fleet_view.shown = area.width >= 100;
    let (list, detail) = if area.width >= 100 {
        let lw = (area.width * 11 / 20).clamp(56, 90);
        let [l, d] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        (l, Some(d))
    } else {
        (area, None)
    };
    let focused = app.focus == Pane::List || !app.fleet_view.shown;
    let block = panel(Line::from(tr!(" Fleet by project ", " Flota por proyecto ").bold()), focused);
    let inner = block.inner(list);
    f.render_widget(block, list);
    let mut body = inner;
    if let Some(s) = app.search.as_ref() {
        let [bar, rest] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(inner);
        body = rest;
        let q = s.query();
        let filter = app.fleet_filter();
        let status = match (&s.remote, filter.as_ref()) {
            (_, None) => String::new(),
            (_, Some((idx, None))) if !idx.is_empty() => {
                let n = idx.len();
                trf!("{n} by text", "{n} por texto")
            }
            (_, Some((idx, Some(_)))) if idx.is_empty() => tr!("nothing by text nor according to jev", "nada por texto ni según jev").into(),
            (_, Some((idx, Some(via)))) => {
                let n = idx.len();
                trf!("{n} according to {via}", "{n} según {via}")
            }
            (Remote::Pending, _) => tr!("nothing by text · asking jev…", "nada por texto · preguntando a jev…").into(),
            (Remote::Failed(e), _) => {
                let e = md::one_line(e);
                trf!("jev failed: {e}", "jev falló: {e}")
            }
            (Remote::Idle, _) if !q.is_empty() => tr!("nothing by text", "nada por texto").into(),
            _ => String::new(),
        };
        let text = s.input.text();
        let line = Line::from(vec![Span::styled(" / ", Style::new().fg(ACCENT).bold()), Span::raw(text.clone()), Span::styled(format!("   {status}"), Style::new().fg(DIM))]);
        f.render_widget(Paragraph::new(vec![line, Line::styled("─".repeat(bar.width as usize), Style::new().fg(DIM))]), bar);
        if s.editing && app.composer.is_none() {
            let (_, (_, col)) = s.input.layout(usize::MAX / 2);
            f.set_cursor_position(Position { x: bar.x + 3 + col as u16, y: bar.y });
        }
    }
    let rows = app.fleet_rows();
    let width = body.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    let mut sel = None;
    for (i, r) in rows.iter().enumerate() {
        match r {
            FleetRow::Group { name, total } => {
                if i > 0 {
                    lines.push(Line::default());
                }
                lines.push(Line::from(vec![
                    Span::styled(format!(" {name}"), Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {total}"), Style::new().fg(DIM)),
                ]));
            }
            FleetRow::Node(idx) => {
                let n = &app.data.nodes[*idx];
                let selected = app.fleet_sel.as_deref() == Some(n.session_id.as_str());
                let start = lines.len();
                lines.push(node_row(n, width, selected));
                lines.extend(tasks::node_task_lines(app, n, width, selected));
                if selected {
                    sel = Some((start, lines.len()));
                }
            }
        }
    }
    if lines.is_empty() {
        let msg = match app.search.as_ref().map(|s| &s.remote) {
            Some(Remote::Pending) => tr!("Asking jev…", "Preguntando a jev…"),
            Some(_) => tr!("Nothing matches the search.", "Nada coincide con la búsqueda."),
            None => tr!("No nodes yet.", "Todavía no hay nodos."),
        };
        centered_lines(f, body, vec![Line::from(msg.fg(DIM))]);
    } else {
        let h = body.height as usize;
        if let Some(range) = sel {
            keep_visible(&mut app.fleet_scroll, range, h, lines.len());
        }
        let total = lines.len();
        f.render_widget(Paragraph::new(lines).scroll((app.fleet_scroll as u16, 0)), body);
        if let Some((s, e)) = sel {
            highlight(f, Rect { width: body.width.saturating_sub(1), ..body }, s..e, app.fleet_scroll);
        }
        scrollbar(f, body, total, app.fleet_scroll);
    }
    if let Some(d) = detail {
        draw_node_detail(f, app, d);
    }
}

fn draw_node_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(n) = app.selected_node().cloned() else {
        app.fleet_view.total = 0;
        f.render_widget(panel(Line::from(tr!(" Node ", " Nodo ").fg(DIM)), false), area);
        return;
    };
    let focused = app.fleet_zoom || app.focus == Pane::Detail;
    let mut block = panel(Line::from(vec![Span::raw(" "), Span::styled(md::one_line(n.display_name()), Style::new().bold()), Span::raw(" ")]), focused);
    let inner = block.clone().inner(area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let width = text.width as usize;
    let mut lines = vec![fit(
        vec![
            Span::styled(format!("● {}", i18n::fleet_state(&n)), Style::new().fg(node_color(&n)).bold()),
            Span::styled(format!(" · {}", since_phrase(n.state_since)), Style::new().fg(DIM)),
        ],
        width,
    )];
    if let Some(p) = &n.project {
        let id = &n.short_id;
        lines.push(Line::from(trf!("project {p} · {id}", "proyecto {p} · {id}").fg(DIM)));
    }
    if let Some(cwd) = &n.cwd {
        lines.push(fit(vec![Span::styled(cwd.clone(), Style::new().fg(DIM))], width));
    }
    lines.extend(tasks::node_detail_lines(app, &n, width));
    lines.extend(background_lines(&n, width));
    if let Some(needs) = &n.daemon_needs {
        lines.push(Line::default());
        lines.extend(wrap(vec![Span::styled(needs.clone(), Style::new().fg(Color::Yellow))], width, vec![Span::styled(tr!("Needs: ", "Necesita: "), Style::new().fg(DIM))], vec![]));
    }
    let said = said(&n);
    if !said.trim().is_empty() {
        lines.push(Line::default());
        lines.push(Line::from(tr!("The last thing it said", "Lo último que dijo").fg(DIM).bold()));
        lines.extend(md::render(&md::parse(&said), width).lines);
    }
    let total = lines.len();
    let h = inner.height as usize;
    app.fleet_detail_scroll = app.fleet_detail_scroll.min(total.saturating_sub(h));
    app.fleet_view.h = h;
    app.fleet_view.total = total;
    if let Some(hint) = more_hint(app.fleet_detail_scroll, h, total) {
        block = block.title_bottom(hint);
    }
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines).scroll((app.fleet_detail_scroll as u16, 0)), text);
    scrollbar(f, inner, total, app.fleet_detail_scroll);
}

fn draw_compact_fleet(f: &mut Frame, app: &App, area: Rect) {
    let block = panel(Line::from(tr!(" Fleet ", " Flota ").fg(SOFT)), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    if let Some(o) = app.orchestrator() {
        lines.push(fit(
            vec![
                Span::styled(" ★ ", Style::new().fg(ACCENT).bold()),
                Span::styled(format!("{} ", tr!("Orchestrator", "Orquestador")), Style::new().bold()),
                Span::styled(i18n::node_state(o), Style::new().fg(state_color(o.state))),
            ],
            width,
        ));
    }
    for r in app.fleet_rows() {
        match r {
            FleetRow::Group { name, total } => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                lines.push(fit(vec![Span::styled(format!(" {name}"), Style::new().bold()), Span::styled(format!(" {total}"), Style::new().fg(DIM))], width));
            }
            FleetRow::Node(i) => {
                let n = &app.data.nodes[i];
                let name = n.display_name().rsplit('/').next().unwrap_or(n.display_name()).to_string();
                lines.push(fit(
                    vec![
                        Span::styled("  ● ", Style::new().fg(node_color(n))),
                        Span::raw(md::one_line(&name)),
                        Span::styled(format!(" {}", i18n::fleet_state(n)), Style::new().fg(DIM)),
                    ],
                    width,
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(tr!(" No nodes yet.", " Todavía no hay nodos.").fg(DIM)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_reader(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(r) = app.reader.as_mut() else { return };
    let review = app.reviews.get(&r.key);
    let (glyph, color) = (r.glyph, r.color);
    let still_open = match r.key {
        Key::Q(id) => app.data.open_questions.iter().any(|q| q.id == id),
        Key::R(id) => app.data.news.iter().any(|x| x.id == id),
        Key::P(id) => app.data.prs.iter().any(|x| x.pr.id == id),
    };
    let mut title = vec![
        Span::raw(" "),
        Span::styled(format!("{glyph} {}", r.kind), Style::new().fg(color).bold()),
        Span::styled(format!(" · {} · ", r.node), Style::new().fg(SOFT)),
        Span::styled(truncate(&md::one_line(&r.title), (area.width as usize).saturating_sub(40).max(10)), Style::new().bold()),
        Span::raw(" "),
    ];
    if !still_open && app.loaded {
        title.push(Span::styled(tr!(" no longer pending ", " ya no está pendiente "), Style::new().fg(Color::Black).bg(Color::LightRed)));
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
    let has_comments = n_comments > 0;
    let (doc, side) = if inner.width >= 110 {
        let [d, s] = Layout::horizontal([Constraint::Min(60), Constraint::Length(42)]).areas(inner);
        (d, Some(s))
    } else if has_comments && inner.height >= 16 {
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
        let approval = r.approval.is_some();
        draw_comments(f, app, side, cursor, approval);
    }
}

fn draw_comments(f: &mut Frame, app: &App, area: Rect, cursor: usize, approval: bool) {
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
    match review.filter(|rv| !rv.is_empty()) {
        None => {
            lines.push(Line::from(tr!("None yet.", "Todavía ninguno.").fg(DIM)));
            lines.push(Line::default());
            lines.push(key("j/k", tr!("move the cursor by blocks", "mueve el cursor por bloques")));
            lines.push(key("^d ^u", tr!("half a page down / up", "media página abajo / arriba")));
            lines.push(key("/", tr!("search the text; n/N next/prev", "busca en el texto; n/N sig./ant.")));
            lines.push(key("c", tr!("comment on the marked block", "comenta el bloque marcado")));
            lines.push(key("Shift+C", tr!("general comment", "comentario general")));
            if approval {
                lines.push(key("a", tr!("approve as is", "aprueba tal cual")));
                lines.push(key("p", tr!("request changes with your notes", "pide cambios con tus notas")));
            } else {
                lines.push(key("s", tr!("send your comments", "envía tus comentarios")));
            }
            if let Key::Q(id) = r.key
                && let Some(q) = app.data.open_questions.iter().find(|q| q.id == id).filter(|q| !q.options.is_empty())
            {
                lines.push(Line::default());
                lines.push(Line::from(tr!("Or choose directly", "O elige directamente").fg(DIM)));
                for (i, o) in q.options.iter().enumerate().take(9) {
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
    let focus = review.and_then(|rv| rv.comments.iter().position(|c| c.block == cursor));
    let scroll = if focus.is_some() && lines.len() > text.height as usize {
        let before: usize = review.map(|rv| rv.comments.iter().take(focus.unwrap_or(0)).count() * 3).unwrap_or(0);
        before.min(lines.len().saturating_sub(text.height as usize))
    } else {
        0
    };
    f.render_widget(Paragraph::new(lines).scroll((scroll as u16, 0)), text);
}

fn composer_height(input: &super::input::TextInput, width: u16) -> u16 {
    let (lines, _) = input.layout(width.saturating_sub(4) as usize);
    lines.len().clamp(1, 8) as u16 + 2
}

fn chips_height(chips: &[super::dropped::Chip]) -> u16 {
    u16::from(!chips.is_empty())
}

fn form_height(form: Option<&super::work::OptionsForm>) -> u16 {
    form.map_or(0, |f| if f.loaded { f.rows_shown().len() as u16 } else { 1 })
}

fn form_lines(form: &super::work::OptionsForm) -> Vec<Line<'static>> {
    if !form.loaded {
        return vec![Line::from(tr!("Loading the options…", "Cargando las opciones…").fg(DIM))];
    }
    let width = form.rows_shown().iter().map(|(l, _, _)| l.chars().count()).max().unwrap_or(0);
    let mut lines: Vec<Line<'static>> = form
        .rows_shown()
        .into_iter()
        .map(|(label, value, focused)| {
            let (marker, style) = if focused { ("▸ ", Style::new().fg(Color::Black).bg(ACCENT)) } else { ("  ", Style::new()) };
            Line::from(vec![Span::styled(marker, Style::new().fg(ACCENT)), Span::styled(format!("{label:<width$}  "), Style::new().fg(DIM)), Span::styled(value, style)])
        })
        .collect();
    if let (Some(e), Some(first)) = (&form.error, lines.first_mut()) {
        first.spans.push(Span::styled(format!("   ⚠ {e}"), Style::new().fg(Color::Red)));
    }
    lines
}

/// The list a row of "New" opens, drawn over the screen above the composer.
fn draw_form_popup(f: &mut Frame, form: &super::work::OptionsForm, area: Rect) {
    use super::work::Popup;
    let Some(popup) = form.popup.as_ref() else { return };
    let (title, hint, lines): (String, &str, Vec<Line<'static>>) = match popup {
        Popup::Issues(p) => {
            let mut lines = vec![Line::from(vec![Span::styled("/ ", Style::new().fg(DIM)), Span::raw(p.query.clone())])];
            if p.loading {
                lines.push(Line::from(tr!("Asking GitHub…", "Preguntando a GitHub…").fg(DIM)));
            }
            if let Some(e) = &p.error {
                lines.push(Line::from(Span::styled(format!("⚠ {e}"), Style::new().fg(Color::Red))));
            }
            let visible = p.visible();
            if visible.is_empty() && !p.loading {
                lines.push(Line::from(tr!("No open issues match", "Ninguna issue abierta coincide").fg(DIM)));
            }
            let room = (area.height as usize).saturating_sub(lines.len() + 2).max(1);
            let start = p.sel.saturating_sub(room - 1);
            for (i, issue) in visible.iter().enumerate().skip(start).take(room) {
                let style = if i == p.sel { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new() };
                let labels = if issue.labels.is_empty() { String::new() } else { format!("  [{}]", issue.labels.join(", ")) };
                let text = truncate(&format!("{}  {}{labels}", issue.reference(), issue.title), area.width.saturating_sub(4) as usize);
                lines.push(Line::from(Span::styled(text, style)));
            }
            let hint = tr!(" type to filter · ↑↓ · ⏎ picks · Ctrl+R searches GitHub again · Esc ", " escribe para filtrar · ↑↓ · ⏎ elige · Ctrl+R busca de nuevo en GitHub · Esc ");
            (tr!("Issues", "Issues").to_string(), hint, lines)
        }
        Popup::Projects { query, sel } => {
            let mut lines = vec![Line::from(vec![Span::styled("/ ", Style::new().fg(DIM)), Span::raw(query.clone())])];
            let choices = form.project_choices(query);
            if choices.is_empty() {
                lines.push(Line::from(tr!("No project matches", "Ningún proyecto coincide").fg(DIM)));
            }
            let sel = (*sel).min(choices.len().saturating_sub(1));
            let room = (area.height as usize).saturating_sub(lines.len() + 2).max(1);
            let start = sel.saturating_sub(room - 1);
            for (i, choice) in choices.iter().enumerate().skip(start).take(room) {
                let style = if i == sel { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new() };
                let (name, note) = match choice {
                    None => (tr!("The orchestrator decides", "Lo decide el orquestador").to_string(), String::new()),
                    Some(p) if form.suggested.as_ref() == Some(p) => (p.clone(), tr!("  · this directory", "  · este directorio").to_string()),
                    Some(p) => (p.clone(), String::new()),
                };
                lines.push(Line::from(vec![Span::styled(name, style), Span::styled(note, Style::new().fg(DIM))]));
            }
            let hint = tr!(" type to filter · ↑↓ · ⏎ picks · Esc ", " escribe para filtrar · ↑↓ · ⏎ elige · Esc ");
            (tr!("Project", "Proyecto").to_string(), hint, lines)
        }
        Popup::Values { spec, sel } => {
            let title = form.specs.get(*spec).map(super::work::label).unwrap_or_default();
            let mut lines = vec![];
            for (i, e) in form.entries(*spec).into_iter().enumerate() {
                let style = if i == *sel { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new().bold() };
                lines.push(Line::from(Span::styled(format!(" {} ", e.label), style)));
                if let Some(d) = e.description {
                    lines.push(Line::from(Span::styled(format!("   {}", truncate(&d, area.width.saturating_sub(8) as usize)), Style::new().fg(DIM))));
                }
            }
            (title, tr!(" ↑↓ · ⏎ picks · Esc ", " ↑↓ · ⏎ elige · Esc "), lines)
        }
    };
    let height = (lines.len() as u16 + 2).min(area.height);
    let area = Rect { y: area.y + area.height - height, height, ..area };
    let block = panel(Line::from(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).bold())), true).title_bottom(Line::from(hint.fg(DIM)).right_aligned());
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
            super::dropped::Upload::Sending => (format!(" {icon} {} · {size} · {} ", chip.name, tr!("copying…", "copiando…")), Style::new().fg(DIM)),
            super::dropped::Upload::Stored(_) => (format!(" {icon} {} · {size} ", chip.name), Style::new().fg(Color::Black).bg(SOFT)),
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

const PREVIEW_ROWS: u16 = 6;

/// Draws the composer and gives back where the preview of its image goes, if it has room.
fn draw_composer(f: &mut Frame, app: &App, area: Rect, preview_h: u16) -> Option<Rect> {
    let c = app.composer.as_ref()?;
    let title = match &c.target {
        Target::Answer { id, title, option: Some(o) } => {
            let t = md::one_line(title);
            let o = i18n::option_label(o);
            trf!("#{id} {t} · {o}: write why", "#{id} {t} · {o}: escribe por qué")
        }
        Target::Answer { id, title, option: None } => {
            let t = md::one_line(title);
            trf!("Reply to #{id} · {t}", "Responder a #{id} · {t}")
        }
        Target::PrChanges { title, .. } => {
            let t = md::one_line(title);
            trf!("{t} · what should change?", "{t} · ¿qué hay que cambiar?")
        }
        Target::PrAsk { title, .. } => {
            let t = md::one_line(title);
            trf!("{t} · what do you want to ask?", "{t} · ¿qué quieres preguntar?")
        }
        Target::MoreContext { id, title } => {
            let t = md::one_line(title);
            trf!("#{id} {t} · what do you need to know? (optional)", "#{id} {t} · ¿qué te falta saber? (opcional)")
        }
        Target::ReplyReport { name, .. } => trf!("Reply to {name} (marks it read)", "Responder a {name} (se marca leído)"),
        Target::Message { name, .. } => trf!("Message to {name}", "Mensaje a {name}"),
        Target::NewRequest => tr!("New work: the orchestrator prepares the mission and launches the node", "Trabajo nuevo: el orquestador prepara la misión y lanza el nodo").into(),
        Target::Comment { quote, .. } => {
            let q = md::one_line(quote);
            trf!("Comment on “{q}”", "Comentario sobre «{q}»")
        }
        Target::General { send: true } if app.reader.as_ref().is_some_and(|r| r.approval.is_some()) => tr!("What changes do you want? (⏎ sends them)", "¿Qué cambios pides? (⏎ los envía)").into(),
        Target::General { send: true } => tr!("Your comments (⏎ sends them)", "Tus comentarios (⏎ los envía)").into(),
        Target::General { send: false } => tr!("General comment", "Comentario general").into(),
    };
    let action = composer_action(&c.target);
    let title = truncate(&title, (area.width as usize).saturating_sub(6));
    let hint = trf!(" ⏎ {action} · Alt+⏎ new line · Esc cancels ", " ⏎ {action} · Alt+⏎ nueva línea · Esc cancela ");
    let block = panel(Line::from(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).bold())), true).title_bottom(Line::from(hint.fg(DIM)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let form_h = form_height(c.form.as_ref()).min(text.height.saturating_sub(1));
    let chips_h = chips_height(&c.chips).min(text.height.saturating_sub(form_h + 1));
    let preview_h = preview_h.min(text.height.saturating_sub(form_h + chips_h + 1));
    let [preview, chips, form, text] =
        Layout::vertical([Constraint::Length(preview_h), Constraint::Length(chips_h), Constraint::Length(form_h), Constraint::Min(1)]).areas(text);
    if chips_h > 0 {
        f.render_widget(Paragraph::new(chip_line(&c.chips)), chips);
    }
    if let Some(o) = c.form.as_ref().filter(|_| form_h > 0) {
        f.render_widget(Paragraph::new(form_lines(o)), form);
    }
    let (lines, (row, col)) = c.input.layout(text.width as usize);
    let scroll = row.saturating_sub(text.height.saturating_sub(1) as usize);
    if c.input.text().is_empty() {
        f.render_widget(Paragraph::new(Line::from(tr!("Type here…", "Escribe aquí…").fg(DIM))), text);
    } else {
        f.render_widget(Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()).scroll((scroll as u16, 0)), text);
    }
    f.set_cursor_position(Position { x: text.x + col as u16, y: text.y + (row - scroll) as u16 });
    (preview_h > 0).then_some(preview)
}

fn composer_action(t: &Target) -> &'static str {
    match t {
        Target::MoreContext { .. } => tr!("sends it back", "la devuelve"),
        Target::Comment { .. } | Target::General { send: false } => tr!("saves", "guarda"),
        _ => tr!("sends", "envía"),
    }
}

fn footer_keys(app: &App) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if let Some(c) = app.composer.as_ref() {
        if let Some(form) = c.form.as_ref() {
            if form.popup.is_some() {
                return vec![k("↑/↓", tr!("move", "mover")), k("⏎", tr!("picks", "elige")), k("Esc", tr!("closes the list", "cierra la lista"))];
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
                k("Tab", tr!("project, issue and options", "proyecto, issue y opciones")),
                k("Alt+⏎", tr!("new line", "nueva línea")),
                k(tr!("drop a file", "arrastra un archivo"), tr!("attaches it", "lo adjunta")),
                k("Esc", tr!("cancel", "cancela")),
            ];
        }
        return vec![
            k("⏎", composer_action(&c.target)),
            k("Alt+⏎", tr!("new line", "nueva línea")),
            k("Ctrl+W", tr!("delete word", "borra palabra")),
            k("Ctrl+U", tr!("delete line", "borra línea")),
            if c.chips.is_empty() {
                k(tr!("drop a file", "arrastra un archivo"), tr!("attaches it", "lo adjunta"))
            } else {
                k(tr!("⌫ on empty", "⌫ en vacío"), tr!("removes the last attachment", "quita el último adjunto"))
            },
            k("Esc", tr!("cancel", "cancela")),
        ];
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
        match (&r.approval, r.key) {
            (Some(a), _) => {
                let label = a.approve.as_ref().map(|l| format!(" («{l}»)")).unwrap_or_default();
                v.push(k("a", &trf!("approve{label}", "aprobar{label}")));
                v.push(k("p", tr!("request changes", "pedir cambios")));
            }
            (None, Key::Q(_)) => v.push(k("s", tr!("send as answer", "enviar como respuesta"))),
            (None, Key::R(_)) => {
                v.push(k("s", tr!("send to node", "enviar al nodo")));
                v.push(k("x", tr!("mark read", "leído")));
            }
            (None, Key::P(_)) => v.push(k("s", tr!("request changes", "pedir cambios"))),
        }
        v.push(k("Esc/o", tr!("back", "volver")));
        return v;
    }
    if app.screen == Screen::Tasks {
        if app.task_zoom {
            return vec![k("j/k", tr!("scroll", "desplazar")), k("^d/^u", tr!("half page", "media página")), k("Esc/o", tr!("back", "volver")), k("r", tr!("message", "mensaje")), k("⏎", tr!("its terminal", "su terminal"))];
        }
        let folds = matches!(app.task_sel, Some(super::tasks::TaskKey::Finished | super::tasks::TaskKey::Archived));
        let mut v = vec![k("j/k", tr!("move", "mover")), k("h/l", tr!("pane", "panel"))];
        if folds {
            v.push(k("⏎", tr!("open/fold", "abrir/plegar")));
        } else {
            v.extend([k("o", tr!("expand", "expandir")), k("r", tr!("message", "mensaje")), k("⏎", tr!("its terminal", "su terminal"))]);
        }
        if app.selected_defining_request().is_some() {
            v.push(k("x", tr!("withdraw", "retirar")));
        } else if !app.task_actions_missing {
            v.extend([k("x", tr!("cancel", "descartar")), k("Shift+R", tr!("reopen", "reabrir"))]);
        }
        if !app.archive_missing {
            v.push(k("Shift+A", tr!("archive finished", "archivar terminadas")));
        }
        v.extend([k("Tab", tr!("fleet", "flota")), k("n", tr!("new", "nuevo")), k("q", tr!("quit", "salir"))]);
        return v;
    }
    if app.screen == Screen::Fleet {
        if app.search.as_ref().is_some_and(|s| s.editing) {
            return vec![k("⏎", tr!("keep filter", "fijar filtro")), k("Esc", tr!("clear search", "quitar búsqueda"))];
        }
        if app.fleet_zoom {
            return vec![k("j/k", tr!("scroll", "desplazar")), k("^d/^u", tr!("half page", "media página")), k("Esc/o", tr!("back", "volver")), k("r", tr!("message", "mensaje")), k("a", tr!("its terminal", "su terminal"))];
        }
        let mut v = vec![k("j/k", tr!("move", "mover")), k("h/l", tr!("pane", "panel")), k("/", tr!("search", "buscar"))];
        if app.search.is_some() {
            v.push(k("Esc", tr!("clear search", "quitar búsqueda")));
        }
        v.extend([
            k("o", tr!("expand", "expandir")),
            k("r", tr!("message", "mensaje")),
            k("⏎", tr!("its terminal (← returns)", "su terminal (← vuelve)")),
            k("x", tr!("close node", "cerrar nodo")),
            k("n", tr!("new", "nuevo")),
            k("Tab", tr!("inbox", "bandeja")),
            k("q", tr!("quit", "salir")),
        ]);
        return v;
    }
    let pane = if app.focus == Pane::Detail { k("j/k", tr!("scroll", "desplazar")) } else { k("j/k", tr!("move", "mover")) };
    let tail = [k("^d/^u", tr!("scroll detail", "desplazar detalle")), k("Tab", tr!("tasks", "tareas")), k("n", tr!("new", "nuevo")), k("q", tr!("quit", "salir"))];
    match app.sel.and_then(|s| app.item(s)) {
        Some(Item::Q(q)) => {
            let mut v = vec![];
            match q.options.len() {
                0 => {}
                1 => v.push(k("1", tr!("choose", "elegir"))),
                n => v.push(k(&format!("1-{}", n.min(9)), tr!("choose", "elegir"))),
            }
            v.extend([pane, k("o", tr!("expand", "expandir")), k("r", tr!("reply", "responder")), k("c", tr!("more context", "más contexto")), k("a", tr!("its terminal", "su terminal"))]);
            v.extend(tail);
            v
        }
        Some(Item::R(r)) => {
            let mut v = vec![pane];
            if !alondra_core::artifact::in_report(r).is_empty() {
                v.extend([k("b", i18n::open_in_browser()), k("c", i18n::rescue_comments()), k("v", i18n::approve_artifact())]);
            }
            v.extend([k("o", tr!("expand", "expandir")), k("x", tr!("mark read", "leído"))]);
            v.extend([k("r", tr!("reply", "responder")), k("a", tr!("its terminal", "su terminal"))]);
            v.extend(tail);
            v
        }
        Some(Item::P(p)) => {
            let first = if let Some(reason) = p.merge_blocked() {
                k("", &reason)
            } else if p.pr.conflicts() {
                k("1", tr!("resolve conflicts", "resolver conflictos"))
            } else {
                k(&format!("1-{}", p.methods().len()), tr!("merge", "fusionar"))
            };
            let mut v = vec![pane, first, k("d", "diff"), k("r", tr!("request changes", "pedir cambios")), k("p", tr!("ask", "preguntar")), k("w", "GitHub"), k("x", tr!("set aside", "apartar")), k("a", tr!("its terminal", "su terminal"))];
            v.extend(tail);
            v
        }
        None => vec![k("n", tr!("new work", "trabajo nuevo")), k("Shift+M", tr!("orchestrator", "orquestador")), k("Tab", tr!("tasks", "tareas")), k("q", tr!("quit", "salir"))],
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
        row("o  ⏎", tr!("expand: open the whole question, plan or report", "expandir: abrir entera la pregunta, plan o informe")),
        row("Esc", tr!("back: close, collapse or leave the detail pane", "volver: cerrar, plegar o salir del panel de detalle")),
        row(":", tr!("command line: :q  :help  :inbox  :tasks  :fleet  :<number>", "línea de órdenes: :q  :help  :inbox  :tasks  :fleet  :<número>")),
        Line::default(),
        section(tr!("Bars in the header, left to right", "Barras de la cabecera, de izquierda a derecha")),
        row(tr!("week", "semana"), tr!("Claude's weekly limit used; it turns red past 85%", "límite semanal de Claude usado; en rojo pasado el 85%")),
        row(tr!("today", "hoy"), tr!("today's spend / today's share of the week; red when over it (no launches without asking)", "gastado hoy / lo que te toca hoy de la semana; en rojo si te pasas (no se lanzan nodos sin preguntarte)")),
        row(tr!("memory", "memoria"), tr!("the Mac's memory in use", "memoria del Mac en uso")),
        row("CPU", tr!("the Mac's CPU in use; red when the load is too high to trust the readings", "CPU del Mac en uso; en rojo si la carga es tan alta que las lecturas no son fiables")),
        Line::default(),
        section(tr!("Inbox", "Bandeja")),
        row("1-9", tr!("choose one of the question's options", "elegir una opción de la pregunta")),
        row("r", tr!("reply in your own words", "responder con tus palabras")),
        row("c", tr!("send the question back asking for more context", "devolver la pregunta pidiendo más contexto")),
        row("b · c · v", tr!("artifact: open in the browser · the node reads your comments on it · approve it", "artefacto: abrir en el navegador · el nodo lee tus comentarios · aprobarlo")),
        row("x", tr!("mark as read", "marcar como leído")),
        row("Shift+J/Shift+K", tr!("scroll the detail 5 lines", "desplazar el detalle 5 líneas")),
        row("a", tr!("open the node's terminal (← on an empty line, Ctrl+Z or /exit returns to Alondra)", "abrir el terminal del nodo (← con la línea vacía, Ctrl+Z o /exit vuelven a Alondra)")),
        Line::default(),
        section(tr!("Reading (after o)", "Lectura (tras o)")),
        row("j/k", tr!("next / previous block; long blocks scroll line by line", "bloque siguiente / anterior; los bloques largos avanzan línea a línea")),
        row("/  n  Shift+N", tr!("search the text · next match · previous match", "buscar en el texto · siguiente · anterior")),
        row("c · Shift+C · d", tr!("comment on block · general comment · delete comment", "comentar bloque · comentario general · borrar")),
        row("a · p", tr!("approve · request changes (plans)", "aprobar · pedir cambios (planes)")),
        row("s · x", tr!("send comments · mark read (documents)", "enviar comentarios · leído (documentos)")),
        row("q  o  Esc", tr!("close the reading", "cerrar la lectura")),
        Line::default(),
        section(tr!("Tasks", "Tareas")),
        row("o", tr!("expand the task's detail to full screen", "expandir el detalle de la tarea a pantalla completa")),
        row("r · ⏎", tr!("message its node · its node's terminal", "mensaje a su nodo · el terminal de su nodo")),
        row("x", tr!("cancel the task (twice to confirm)", "descartar la tarea (dos veces para confirmar)")),
        row("Shift+R", tr!("reopen a done or cancelled task", "reabrir una tarea hecha o descartada")),
        row("Shift+A", tr!("archive every done or cancelled task", "archivar todas las tareas hechas o descartadas")),
        row("⏎ · o", tr!("on Archived: show or hide the archived tasks", "en Archivadas: mostrar u ocultar las archivadas")),
        Line::default(),
        section(tr!("Fleet", "Flota")),
        row("/", tr!("search; if text finds nothing, it asks jev", "buscar; si el texto no encuentra nada, pregunta a jev")),
        row("r · ⏎", tr!("message the node · its terminal", "mensaje al nodo · su terminal")),
        row("o", tr!("expand the node's detail to full screen", "expandir el detalle del nodo a pantalla completa")),
        row("x", tr!("close the node (twice to confirm; never discards work)", "cerrar el nodo (dos veces para confirmar; nunca descarta trabajo)")),
        Line::default(),
        section(tr!("Always", "Siempre")),
        row("Tab", tr!("inbox → tasks → fleet (Shift+Tab goes back)", "bandeja → tareas → flota (Shift+Tab vuelve)")),
        row("n · Shift+M", tr!("new work · message the orchestrator", "trabajo nuevo · mensaje al orquestador")),
        row("q  Ctrl-c", tr!("quit", "salir")),
        row(tr!("typing", "al escribir"), tr!("letters go to the text box; Esc leaves it", "las letras van al cuadro de texto; Esc sale")),
    ]
}

fn draw_help(f: &mut Frame, app: &mut App) {
    let lines = help_lines();
    let area = f.area();
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

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::tests::{alive_node, app_with, ctrl, pr_card, press, question, report, train_card};
    use super::*;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    #[test]
    fn a_composer_shows_its_attachments_as_chips_and_previews_the_image() {
        use super::super::dropped::{Chip, Upload};
        use super::super::{Msg, Target};
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let (mut app, _rx) = app_with(vec![question(1, alondra_core::QuestionOrigin::Ask, &[])]);
        app.compose(Target::Answer { id: 1, title: "t".into(), option: None }, "");
        let c = app.composer.as_mut().unwrap();
        c.chips.push(Chip { seq: 7, name: "captura.png".into(), bytes: 240 * 1024, upload: Upload::Stored("0123456789abcdef".into()) });
        c.chips.push(Chip { seq: 8, name: "informe.pdf".into(), bytes: 1_150_000, upload: Upload::Sending });
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("captura.png · 240 KB"), "{s}");
        assert!(s.contains("informe.pdf · 1.1 MB · copying…") || s.contains("informe.pdf · 1.1 MB · copiando…"), "{s}");
        assert!(!s.contains('▀') && !s.contains('▄'), "no preview before the image is decoded: {s}");
        let image = image::load_from_memory(&super::super::files::png(40, 30)).unwrap();
        app.on_msg(Msg::Preview { seq: 7, image });
        let s = screen(&mut app, 120, 40);
        assert!(s.contains('▀') || s.contains('▄'), "the image is previewed (half-blocks in tests): {s}");
    }

    #[test]
    fn new_shows_one_row_per_setting_and_a_row_opens_its_values_with_descriptions() {
        use super::super::{Msg, Target};
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let (mut app, _rx) = app_with(vec![]);
        app.compose(Target::NewRequest, "");
        let specs = alondra_core::options::OptionsFile::default().catalog(None);
        app.on_msg(Msg::Options { project: None, result: Ok((vec!["App".into(), "Lib".into()], Some("Lib".into()), specs, None)) });
        let s = screen(&mut app, 120, 40);
        for row in ["Project  the orchestrator decides", "Issue    none", "PRs      review each one", "Effort   auto", "Tab project, issue and options"] {
            assert!(s.contains(row), "{row}: {s}");
        }
        let key = |app: &mut App, code| app.composer_key(KeyEvent::new(code, KeyModifiers::NONE));
        for code in [KeyCode::Tab, KeyCode::Down, KeyCode::Down, KeyCode::Down, KeyCode::Enter] {
            key(&mut app, code);
        }
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("The orchestrator picks the model and effort from the task"), "{s}");
        assert!(s.contains("The strongest model and the most effort"), "{s}");
        key(&mut app, KeyCode::Esc);
        key(&mut app, KeyCode::Char('p'));
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("Lib  · this directory"), "{s}");
        assert!(s.find("Lib  · this directory") < s.find("App"), "this directory's project comes first: {s}");
    }

    #[test]
    fn a_node_row_counts_what_runs_in_the_background_and_the_detail_lists_it() {
        use alondra_core::{BackgroundItem, BackgroundKind};
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let mut n = alive_node("s");
        let item = |kind, label: &str, suspect| BackgroundItem { kind, label: label.into(), started_at: now_millis() - 3_600_000, suspect };
        n.node.background = vec![item(BackgroundKind::Agent, "Revisar el PR", false), item(BackgroundKind::Agent, "Auditar", false), item(BackgroundKind::Shell, "bazelisk build", false)];
        let row = |n: &FleetNode| node_row(n, 90, false).spans.iter().map(|s| s.content.to_string()).collect::<String>();
        assert!(row(&n).contains("2 subagentes · 1 shell"), "{}", row(&n));
        assert!(!row(&n).contains("¿colgada?"));
        n.node.background[2].suspect = true;
        assert!(row(&n).contains("2 subagentes · 1 shell · ¿colgada?"), "{}", row(&n));
        let detail: String = background_lines(&n, 60).iter().flat_map(|l| l.spans.iter().map(|s| s.content.to_string())).collect();
        assert!(detail.contains("subagente Revisar el PR · 1h") && detail.contains("shell bazelisk build · 1h · ¿colgada?"), "{detail}");
        n.node.background.clear();
        assert!(!row(&n).contains("shell"));
    }

    #[test]
    fn a_report_with_an_artifact_offers_opening_it_in_the_browser() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let mut r = report(1);
        r.body = Some("El plan está en https://claude.ai/code/artifact/0f1e2d3c-4b5a.".into());
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.news = vec![r.into()];
        app.reconcile();
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let s = screen(&mut app, 120, 30);
        assert!(s.contains("b Abrir en el navegador"), "{s}");
        assert!(s.contains("c Rescatar comentarios") && s.contains("v Aprobar"), "{s}");
        press(&mut app, "b");
        assert_eq!(app.flash.as_ref().map(|f| f.text.as_str()), Some("Abierto en el navegador"));
        press(&mut app, "c");
        assert!(app.hidden.contains_key(&crate::tui::Key::R(1)), "the card leaves until the node sends the artifact again");

        let (mut plain, _rx) = app_with(vec![]);
        plain.loaded = true;
        plain.data.news = vec![report(2).into()];
        plain.reconcile();
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        assert!(!screen(&mut plain, 120, 30).contains("Abrir en el navegador"));
    }

    /// 24 Sep: a report with a PNG attached showed the file's bytes as text ("?PNG IHDR?…").
    #[test]
    fn an_attached_image_is_drawn_and_a_binary_file_is_named_never_dumped() {
        use super::super::{Msg, files};
        let attach = |name: &str, path: &str| alondra_core::model::Attachment { name: name.into(), source_path: format!("/tmp/{name}"), stored_path: Some(path.into()), bytes: None };
        let mut r = report(1);
        r.attachments = vec![attach("ios-inbox.png", "/files/1.png"), attach("plan.pdf", "/files/2.pdf")];
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.news = vec![r.into()];
        app.reconcile();
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        app.on_msg(Msg::File { path: "/files/1.png".into(), result: Ok(files::FileBody::classify(files::png(40, 30))) });
        app.on_msg(Msg::File { path: "/files/2.pdf".into(), result: Ok(files::FileBody::classify(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3 binary".to_vec())) });
        let s = screen(&mut app, 120, 50);
        assert!(s.contains("Imagen 40×30 · v la abre en Vista Previa"), "{s}");
        assert!(s.contains("Fichero binario, 21 B · v lo abre con su aplicación"), "{s}");
        assert!(!s.contains("PDF-1.7") && !s.contains("PNG"), "no bytes as text: {s}");
        assert!(s.contains('▀') || s.contains('▄'), "the image is drawn (half-blocks in tests): {s}");
        press(&mut app, "v");
        assert_eq!(app.flash.as_ref().map(|f| f.text.as_str()), Some("Abierto con su aplicación"));
    }

    #[test]
    fn a_notice_the_gate_flagged_is_marked_as_asking() {
        let mut r = report(1);
        r.kind = ReportKind::Notice;
        assert_eq!(report_glyph(&r).0, "•");
        r.looks_like_question = true;
        assert_eq!(report_glyph(&r).0, "?");
        r.kind = ReportKind::Final;
        assert_eq!(report_glyph(&r).0, "✓");
    }

    #[test]
    fn empty_inbox_says_so_and_offers_the_next_step() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        let s = screen(&mut app, 100, 24);
        assert!(s.contains("All caught up"), "{s}");
        assert!(s.contains("order new work"));
    }

    #[test]
    fn pull_requests_have_their_own_section_between_questions_and_reading() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Plan, &["Approve"])]);
        app.loaded = true;
        app.data.prs = vec![pr_card(None)];
        app.reconcile();
        let s = screen(&mut app, 120, 30);
        let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing\n{s}"));
        assert!(at("WAITING FOR YOU 1") < at("PULL REQUESTS 1"), "{s}");
        assert!(at("PULL REQUESTS 1") < at("Menus become cards"), "{s}");
    }

    #[test]
    fn a_train_is_drawn_bottom_first_and_its_upper_card_cannot_be_merged() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.prs = vec![train_card(3, "Top of train", 1, 2, 10), pr_card(None), train_card(2, "Middle of train", 1, 1, 20), train_card(1, "Bottom of train", 1, 0, 300)];
        app.reconcile();
        app.settle_until = 0;
        let s = screen(&mut app, 140, 40);
        let at = |s: &str, t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing\n{s}"));
        assert!(at(&s, "Menus become cards") < at(&s, "Bottom of train"), "{s}");
        assert!(at(&s, "Bottom of train") < at(&s, "Middle of train") && at(&s, "Middle of train") < at(&s, "Top of train"), "{s}");
        assert!(s.contains("↑ on top of PR #12"), "{s}");
        press(&mut app, "jj");
        let s = screen(&mut app, 140, 40);
        assert!(s.contains("Merge PR #11 first"), "{s}");
        assert!(!s.contains("merge: "), "no merge key on a card on top of an open PR\n{s}");
        assert!(s.contains("request changes") && s.contains("set aside"), "{s}");
        press(&mut app, "k");
        let s = screen(&mut app, 140, 40);
        assert!(s.contains("merge: "), "the bottom card merges\n{s}");
    }

    #[test]
    fn a_question_about_a_pr_is_drawn_under_it_and_not_as_waiting() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.prs = vec![pr_card(None)];
        let mut q = question(2, QuestionOrigin::Ask, &["Merge it"]);
        q.title = "Merge it before the release?".into();
        app.data.open_questions.push(crate::client::OpenQuestion { question: q, pr_id: Some(4), task: None, report: None, reports: vec![] });
        app.reconcile();
        let s = screen(&mut app, 120, 30);
        let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing\n{s}"));
        assert!(!s.contains("WAITING FOR YOU"), "{s}");
        assert!(s.contains("Inbox 1"), "{s}");
        assert!(at("PULL REQUESTS 1") < at("Menus become cards"), "{s}");
        assert!(at("Menus become cards") < at("↳ ? Merge it before the release?"), "{s}");
        assert!(s.contains("The node asks: Merge it before the release?"), "{s}");
    }

    #[test]
    fn a_report_about_a_pr_is_drawn_under_it_and_not_as_reading() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.prs = vec![pr_card(None)];
        let mut r = report(2);
        r.title = "PR ready: leaving a node returns to Alondra".into();
        app.data.news.push(crate::client::NewsReport { report: r, pr_id: Some(4), task: None, earlier: vec![] });
        app.reconcile();
        let s = screen(&mut app, 120, 30);
        let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing\n{s}"));
        assert!(!s.contains("TO READ"), "{s}");
        assert!(s.contains("Inbox 1"), "{s}");
        assert!(at("Menus become cards") < at("↳ "), "{s}");
        assert!(s.contains("The node says: PR ready: leaving a node returns to Alondra"), "{s}");
    }

    #[test]
    fn a_question_about_a_report_shows_the_report_inside_it() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        let report: alondra_core::Report = serde_json::from_value(serde_json::json!({
            "id": 7, "session_id": "audit", "node_name": "alondra/auditoria", "title": "Audit: 13 things left",
            "body": "The worst three are in iOS.", "kind": "document", "created_at": 1
        }))
        .unwrap();
        let mut q = question(2, QuestionOrigin::Ask, &["Launch it"]);
        q.title = "Launch a node to fix the audit?".into();
        app.data.open_questions.push(crate::client::OpenQuestion { question: q, pr_id: None, task: None, report: Some(report), reports: vec![] });
        app.reconcile();
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("Launch a node to fix the audit?"), "{s}");
        assert!(s.contains("About report #7"), "{s}");
        assert!(s.contains("Audit: 13 things left") && s.contains("The worst three are in iOS."), "{s}");
    }

    fn delivered(id: u64, title: &str) -> alondra_core::Report {
        serde_json::from_value(serde_json::json!({
            "id": id, "session_id": "s1", "node_name": "alondra/plan", "title": title, "kind": "final", "created_at": id
        }))
        .unwrap()
    }

    #[test]
    fn a_question_shows_every_report_it_takes_in() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        let mut q = question(2, QuestionOrigin::Ask, &["Yes"]);
        q.title = "Go on with the TUI jump?".into();
        let reports = vec![delivered(7, "Menus become cards"), delivered(8, "Subscription usage")];
        app.data.open_questions.push(crate::client::OpenQuestion { question: q, pr_id: None, task: None, report: reports.last().cloned(), reports });
        app.reconcile();
        let s = screen(&mut app, 120, 40);
        let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing\n{s}"));
        assert!(at("About report #7") < at("About report #8"), "{s}");
        assert!(s.contains("Subscription usage"), "{s}");
    }

    #[test]
    fn a_report_card_carries_the_earlier_reports_of_its_delivery() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.news = vec![crate::client::NewsReport { report: delivered(2, "Annex to the audit"), pr_id: None, task: None, earlier: vec![delivered(1, "Audit redone")] }];
        app.reconcile();
        let s = screen(&mut app, 120, 40);
        assert!(s.contains("TO READ 1"), "{s}");
        let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing\n{s}"));
        assert!(at("Earlier in this delivery (1)") < at("Audit redone"), "{s}");
    }

    #[test]
    fn the_overview_carries_a_questions_reports_and_a_cards_earlier_ones() {
        let q = serde_json::json!({
            "id": 2, "session_id": "s1", "title": "Go on?", "options": [], "links": [], "attachments": [], "blocking": true,
            "origin": "ask", "status": "open", "asked_at": 3, "reports": [{"id": 1, "session_id": "s1", "title": "A", "kind": "final", "created_at": 1}]
        });
        let q: crate::client::OpenQuestion = serde_json::from_value(q).unwrap();
        assert_eq!(q.about().iter().map(|r| r.id).collect::<Vec<_>>(), [1]);
        let r = serde_json::json!({"id": 2, "session_id": "s1", "title": "B", "kind": "final", "created_at": 2, "earlier": [{"id": 1, "session_id": "s1", "title": "A", "kind": "final", "created_at": 1}]});
        let r: crate::client::NewsReport = serde_json::from_value(r).unwrap();
        assert_eq!(r.earlier.len(), 1);
    }

    #[test]
    fn options_are_visible_without_opening_anything_even_when_narrow() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Plan, &["Approve", "Request changes"])]);
        app.loaded = true;
        for (w, h) in [(60, 16), (100, 30), (200, 60)] {
            let s = screen(&mut app, w, h);
            assert!(s.contains("1 Approve"), "{w}x{h}\n{s}");
            assert!(s.contains("? help"), "{w}x{h}\n{s}");
        }
    }

    #[test]
    fn a_card_built_from_the_terminal_says_what_the_node_was_on() {
        let mut q = question(1, QuestionOrigin::Classifier, &[]);
        q.mission = Some(format!("Ship Lumo 4.5 to the App Store. {}", "Details. ".repeat(80)));
        let (mut app, _rx) = app_with(vec![q]);
        app.loaded = true;
        let s = screen(&mut app, 120, 30);
        assert!(s.contains("Mission: Ship Lumo 4.5 to the App Store."), "{s}");
        assert!(s.contains('…'), "{s}");
        assert_eq!(mission_excerpt("  a\n\n b "), "a b");
    }

    #[test]
    fn the_header_shows_week_today_memory_and_cpu_as_bars() {
        let (mut app, _rx) = app_with(vec![]);
        app.loaded = true;
        app.data.machine.memory_free_percent = Some(56);
        app.data.machine.cpu_usage = 34.0;
        app.data.usage = Some(alondra_core::usage::Usage { week_percent: 53.0, today_spent: Some(41.0), today_share: Some(17.6), per_day_left: 9.0, over_pace: true, ..Default::default() });
        let s = screen(&mut app, 160, 20);
        assert!(s.contains("Claude week ▰▰▱▱ 53%  today ▰▰▰▰ 41% of 18% │ Mac RAM ▰▰▱▱ 44%  CPU ▰▱▱▱ 34%"), "{s}");
        let s = screen(&mut app, 90, 20);
        assert!(s.contains("▰▰▱▱ 53% ▰▰▰▰ 41/18% ▰▰▱▱ 44% ▰▱▱▱ 34%"), "bare bars when there is no room for names\n{s}");
        let r = readings(&app.data.machine, app.data.usage.as_ref());
        assert_eq!(r.iter().map(|r| r.level).collect::<Vec<_>>(), [Level::Calm, Level::Hot, Level::Calm, Level::Calm]);
        app.data.usage = None;
        assert!(screen(&mut app, 160, 20).contains("Mac RAM ▰▰▱▱ 44%  CPU ▰▱▱▱ 34%"));
    }

    #[test]
    fn spanish_follows_the_language_setting() {
        let (mut app, _rx) = app_with(vec![]);
        i18n::set_for_this_thread(i18n::Lang::Es);
        app.loaded = true;
        let s = screen(&mut app, 100, 24);
        assert!(s.contains("Todo al día"), "{s}");
        assert!(s.contains("? ayuda"), "{s}");
    }

    #[test]
    fn a_long_question_shows_there_is_more_and_scrolls_to_its_end() {
        let mut q = question(1, QuestionOrigin::Ask, &["Yes"]);
        q.detail = Some((1..=60).map(|i| format!("Line {i}.")).collect::<Vec<_>>().join("\n\n"));
        let (mut app, _rx) = app_with(vec![q]);
        app.loaded = true;
        let s = screen(&mut app, 110, 30);
        assert!(s.contains("more lines"), "{s}");
        assert!(!s.contains("Line 60."), "{s}");
        press(&mut app, "l");
        press(&mut app, "G");
        let s = screen(&mut app, 110, 30);
        assert!(s.contains("Line 60."), "{s}");
        assert!(s.contains("↑ end"), "{s}");
        press(&mut app, "gg");
        ctrl(&mut app, 'd');
        assert!(app.detail_scroll > 0);
    }

    #[test]
    fn the_reader_shows_the_end_of_a_single_huge_paragraph() {
        let mut q = question(1, QuestionOrigin::Ask, &[]);
        q.detail = Some(format!("{} FINAL", "word ".repeat(1500)));
        let (mut app, _rx) = app_with(vec![q]);
        app.loaded = true;
        press(&mut app, "o");
        let s = screen(&mut app, 80, 24);
        assert!(!s.contains("FINAL"), "{s}");
        press(&mut app, "G");
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("FINAL"), "{s}");
        for _ in 0..200 {
            press(&mut app, "k");
            screen(&mut app, 80, 24);
        }
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("Question 1"), "{s}");
        for _ in 0..200 {
            press(&mut app, "j");
            screen(&mut app, 80, 24);
        }
        let s = screen(&mut app, 80, 24);
        assert!(s.contains("FINAL"), "{s}");
    }

    #[test]
    fn help_fits_short_terminals_by_scrolling() {
        let (mut app, _rx) = app_with(vec![]);
        press(&mut app, "?");
        let s = screen(&mut app, 100, 20);
        assert!(s.contains("more lines"), "{s}");
        press(&mut app, "G");
        let s = screen(&mut app, 100, 20);
        assert!(s.contains("Always"), "{s}");
    }

    /// A shortcut key that needs an uppercase letter must read "Shift+X", never a bare capital:
    /// the help screen is the one place that lists every key, so scanning it here catches a
    /// regression anywhere a footer or card label spells one out again.
    #[test]
    fn help_never_shows_a_bare_uppercase_shortcut_key() {
        for line in help_lines() {
            let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
            let chars: Vec<char> = text.chars().collect();
            for (i, &c) in chars.iter().enumerate() {
                if !c.is_ascii_uppercase() {
                    continue;
                }
                let boundary_before = i == 0 || !chars[i - 1].is_alphanumeric();
                let boundary_after = i + 1 == chars.len() || !chars[i + 1].is_alphanumeric();
                if !boundary_before || !boundary_after {
                    continue; // part of a longer word like "Esc", "Tab", "CPU"
                }
                // A real modifier combo (Ctrl+Z, Alt+…) already says so; only a bare capital
                // typed with Shift, with no modifier prefix at all, needs the Shift+ label.
                let has_modifier_prefix =
                    ["Shift+", "Ctrl+", "Ctrl-", "Alt+"].iter().any(|m| i >= m.chars().count() && chars[i - m.chars().count()..i].iter().collect::<String>() == *m);
                assert!(has_modifier_prefix, "bare uppercase key {c:?} in help line {text:?}: should read Shift+{c}");
            }
        }
    }
}
