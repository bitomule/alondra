use std::collections::HashMap;

use alondra_core::{NodeState, Question, QuestionOrigin, Report, ReportKind, now_millis};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Panel, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use super::md::{self, str_width, truncate, wrap};
use super::{App, FleetRow, Item, Key, Pane, Remote, Screen, Target, View, said, who};
use crate::client::FleetNode;
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
            Screen::Fleet => draw_fleet(f, app, body),
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
        NodeState::Unclear => Color::Magenta,
        NodeState::Stalled => Color::LightRed,
        NodeState::Delivered => Color::Cyan,
        NodeState::WaitingMachine => Color::LightRed,
        NodeState::Working => Color::Green,
        NodeState::Done => DIM,
    }
}

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

/// `bitomule/alondra #12 · fase-1 ← menus · +120 −8 in 5 files · checks 2✓ 1✗ 1…`
pub(super) fn pr_summary(p: &crate::client::PrCard) -> String {
    use alondra_core::pr::CheckState;
    let pr = &p.pr;
    let mut s = format!("{} #{}", pr.repo, pr.number);
    if let (Some(base), Some(head)) = (&pr.base, &pr.head) {
        s.push_str(&format!(" · {base} ← {head}"));
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
    if pr.mergeable.as_deref() == Some("CONFLICTING") {
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
    let (inbox, fleet) = (app.inbox_keys().len(), app.data.nodes.len());
    left.push(tab(trf!("Inbox {inbox}", "Bandeja {inbox}"), app.screen == Screen::Inbox));
    left.push(tab(trf!("Fleet {fleet}", "Flota {fleet}"), app.screen == Screen::Fleet));
    if app.offline {
        left.push(Span::raw(" "));
        left.push(Span::styled(tr!(" no connection to alondrad ", " sin conexión con alondrad "), Style::new().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD)));
    }
    let working = app.data.nodes.iter().filter(|n| n.state == NodeState::Working && n.alive).count();
    let mut right = vec![Span::styled(trf!("{working} working ", "{working} trabajando "), Style::new().fg(DIM))];
    for r in readings(m, app.data.usage.as_ref()) {
        let color = match r.level {
            Level::Calm => ACCENT,
            Level::Warm => Color::Yellow,
            Level::Hot => Color::LightRed,
        };
        let filled = ((r.value.clamp(0.0, 1.0) * BAR_CELLS as f64).round() as usize).max(1);
        right.push(Span::raw(" "));
        right.push(Span::styled("▰".repeat(filled), Style::new().fg(color)));
        right.push(Span::styled("▱".repeat(BAR_CELLS - filled), Style::new().fg(DIM)));
        let label_style = if r.level == Level::Hot { Style::new().fg(Color::LightRed).add_modifier(Modifier::BOLD) } else { Style::new().fg(DIM) };
        right.push(Span::styled(format!(" {}", r.label), label_style));
    }
    right.push(Span::raw(" "));
    let lw: usize = left.iter().map(Span::width).sum();
    let rw: usize = right.iter().map(Span::width).sum();
    f.render_widget(Paragraph::new(Line::from(left)), area);
    if app.loaded && lw + rw + 2 <= area.width as usize {
        f.render_widget(Paragraph::new(Line::from(right).right_aligned()), area);
    }
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
    pub value: f64,
    pub label: String,
    pub level: Level,
}

/// The header bars, always in this order: the week's Claude usage, today against its share of
/// the week, memory in use and CPU in use. `?` explains them.
pub(super) fn readings(m: &alondra_core::Machine, u: Option<&alondra_core::usage::Usage>) -> Vec<Reading> {
    let mut out = vec![];
    if let Some(u) = u {
        out.push(Reading { value: u.week_percent / 100.0, label: format!("{}%", u.week_percent.round()), level: if u.week_percent >= 85.0 { Level::Hot } else { Level::Calm } });
        if let Some((spent, share)) = u.today_spent.zip(u.today_share) {
            let level = if u.over_pace { Level::Hot } else if spent >= share * 0.75 { Level::Warm } else { Level::Calm };
            out.push(Reading { value: spent / share.max(0.1), label: format!("{}/{}%", spent.round(), share.round()), level });
        }
    }
    if let Some(free) = m.memory_free_percent {
        let used = 100 - free.min(100);
        let level = if free < 15 { Level::Hot } else if free < 30 { Level::Warm } else { Level::Calm };
        out.push(Reading { value: used as f64 / 100.0, label: format!("{used}%"), level });
    }
    let cpu = m.cpu_usage as f64;
    let level = if m.unreliable { Level::Hot } else if cpu >= 85.0 { Level::Warm } else { Level::Calm };
    out.push(Reading { value: cpu / 100.0, label: format!("{}%", cpu.round()), level });
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

fn item_lines(app: &App, key: Key, width: usize, selected: bool) -> Vec<Line<'static>> {
    let Some(item) = app.item(key) else { return vec![] };
    let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let (glyph, _, color) = item_glyph(&item);
    let title_style = if selected { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() };
    let pending = app.reviews.get(&key).map(|r| r.count()).unwrap_or(0);
    let (title, node, at) = match &item {
        Item::Q(q) => (q.title.clone(), who(&q.node_name, &q.session_id), q.asked_at),
        Item::R(r) => (r.title.clone(), who(&r.node_name, &r.session_id), r.created_at),
        Item::P(p) => (p.pr.title.clone(), who(&p.pr.node_name, &p.pr.session_id), p.pr.opened_at),
    };
    let first = fit(vec![bar.clone(), Span::styled(format!("{glyph} "), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(md::one_line(&title), title_style)], width);
    let mut meta = vec![bar, Span::styled(format!("  {node} · {}", ago(at)), Style::new().fg(DIM))];
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
                Line::from(vec![Span::styled("Tab", Style::new().fg(ACCENT).bold()), tr!(" see the fleet", " mira la flota").fg(DIM)]),
            ],
        );
        return;
    }
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = vec![];
    let mut sel_range = None;
    let sections: [(fn(&Key) -> bool, &str, Color); 3] = [
        (|k| matches!(k, Key::Q(_)), tr!("WAITING FOR YOU", "TE ESPERAN"), ACCENT),
        (|k| matches!(k, Key::P(_)), tr!("PULL REQUESTS", "PULL REQUESTS"), Color::Magenta),
        (|k| matches!(k, Key::R(_)), tr!("TO READ", "PARA LEER"), Color::Cyan),
    ];
    for (belongs, label, color) in sections {
        let count = keys.iter().filter(|k| belongs(k)).count();
        if count == 0 {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![Span::styled(format!(" {label} "), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(count.to_string(), Style::new().fg(DIM))]));
        for &k in keys.iter().filter(|k| belongs(k)) {
            let selected = app.sel == Some(k);
            let start = lines.len();
            lines.extend(item_lines(app, k, width, selected));
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

fn detail_lines(app: &App, item: &Item, width: usize) -> Vec<Line<'static>> {
    let (glyph, kind, color) = item_glyph(item);
    let mut lines = vec![];
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
        match a.stored_path.as_ref().and_then(|p| app.files.get(p)) {
            Some(Ok(text)) => {
                lines.push(Line::default());
                lines.extend(md::render(&md::parse(text), width).lines);
            }
            Some(Err(e)) => lines.push(Line::from(trf!("Could not load it: {e}", "No he podido cargarlo: {e}").fg(Color::LightRed))),
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
        && let Some(c) = q.node_context.as_ref().filter(|c| c.trim().contains(char::is_whitespace))
    {
        lines.push(Line::default());
        lines.push(Line::from(tr!("What the node was doing", "Lo que hacía el nodo").fg(DIM).bold()));
        for l in c.lines() {
            lines.extend(wrap(vec![Span::styled(l.to_string(), Style::new().fg(DIM))], width, vec![], vec![]));
        }
    }
    lines
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

fn action_lines(app: &App, item: &Item, width: usize) -> Vec<Line<'static>> {
    let hint = |spans: Vec<Span<'static>>| fit(spans, width);
    match item {
        Item::P(p) => {
            let mut out: Vec<Line> = p
                .methods()
                .into_iter()
                .enumerate()
                .map(|(i, m)| {
                    let method = i18n::merge_method(m);
                    let mut spans = vec![keycap(&(i + 1).to_string()), Span::styled(trf!(" merge: {method}", " fusionar: {method}"), Style::new().add_modifier(Modifier::BOLD))];
                    if i == 0 && p.preferred_method == Some(m) {
                        spans.push(Span::styled(tr!("  last used here", "  el último usado aquí"), Style::new().fg(DIM)));
                    }
                    fit(spans, width)
                })
                .collect();
            out.push(hint(vec![
                keycap("d"),
                Span::styled(tr!(" diff and comment (tuicr)  ", " diff y comentarios (tuicr)  "), Style::new().fg(SOFT)),
                keycap("r"),
                Span::styled(tr!(" request changes  ", " pedir cambios  "), Style::new().fg(SOFT)),
                keycap("w"),
                Span::styled(" GitHub  ", Style::new().fg(SOFT)),
                keycap("x"),
                Span::styled(tr!(" set aside", " apartar"), Style::new().fg(SOFT)),
            ]));
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
                Span::styled(tr!(" reply", " responder"), Style::new().fg(SOFT)),
            ];
            if pending > 0 {
                spans.push(Span::styled(trf!("  ✎{pending} unsent", "  ✎{pending} sin enviar"), Style::new().fg(COMMENT)));
            }
            vec![hint(spans)]
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
    let lines = detail_lines(app, &item, width);
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

fn fleet_summary(app: &App) -> Vec<Line<'static>> {
    let alive: Vec<&FleetNode> = app.data.nodes.iter().filter(|n| n.alive).collect();
    let mut counts: Vec<(NodeState, usize)> = vec![];
    for n in &alive {
        match counts.iter_mut().find(|(s, _)| *s == n.state) {
            Some((_, c)) => *c += 1,
            None => counts.push((n.state, 1)),
        }
    }
    let mut lines = vec![Line::from(tr!("The fleet right now", "La flota ahora").fg(DIM).bold()), Line::default()];
    if counts.is_empty() {
        lines.push(Line::from(tr!("No node is awake.", "Ningún nodo despierto.").fg(DIM)));
    }
    for (s, c) in counts {
        lines.push(Line::from(vec![Span::styled("● ", Style::new().fg(state_color(s))), Span::raw(format!("{c} {}", i18n::state(s)))]));
    }
    lines
}

fn node_row(n: &FleetNode, width: usize, selected: bool) -> Line<'static> {
    let bar = if selected { Span::styled("▌", Style::new().fg(ACCENT)) } else { Span::raw(" ") };
    let dot = if n.alive { "●" } else { "○" };
    let state_w = if width >= 64 { 20 } else { 0 };
    let since = ago(n.state_since);
    let name_w = width.saturating_sub(4 + state_w + 5);
    let short = if n.is_orchestrator { format!("★ {}", n.display_name()) } else { n.display_name().to_string() };
    let name = truncate(&md::one_line(&short), name_w);
    let pad = name_w.saturating_sub(str_width(&name));
    let name_style = if n.alive { Style::new() } else { Style::new().fg(SOFT) };
    let mut spans = vec![bar, Span::styled(format!(" {dot} "), Style::new().fg(state_color(n.state))), Span::styled(name, name_style), Span::raw(" ".repeat(pad))];
    if state_w > 0 {
        spans.push(Span::styled(format!(" {:<19}", truncate(i18n::node_state(n), 19)), Style::new().fg(state_color(n.state))));
    }
    spans.push(Span::styled(format!("{since:>5}"), Style::new().fg(DIM)));
    fit(spans, width)
}

fn draw_fleet(f: &mut Frame, app: &mut App, area: Rect) {
    if app.fleet_zoom && app.selected_node().is_some() {
        draw_node_detail(f, app, area);
        return;
    }
    app.fleet_view.shown = area.width >= 100;
    let (list, detail) = if area.width >= 100 {
        let lw = (area.width * 11 / 20).clamp(56, 90);
        let [l, d] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        (l, Some(d))
    } else {
        (area, None)
    };
    let hidden = app.sleeping_hidden();
    let focused = app.focus == Pane::List || !app.fleet_view.shown;
    let mut block = panel(Line::from(tr!(" Fleet by project ", " Flota por proyecto ").bold()), focused);
    if hidden > 0 {
        block = block.title_bottom(Line::from(trf!(" {hidden} sleeping hidden · d shows them ", " {hidden} dormidos ocultos · d los muestra ").fg(DIM)));
    } else if app.show_sleeping && app.search.is_none() {
        block = block.title_bottom(Line::from(tr!(" d hides the sleeping ", " d oculta los dormidos ").fg(DIM)));
    }
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
            FleetRow::Group { name, orchestrator, alive, total } => {
                if i > 0 {
                    lines.push(Line::default());
                }
                let label = if *orchestrator { format!("★ {name}") } else { name.clone() };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label}"), Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(trf!("  {alive} awake of {total}", "  {alive} despiertos de {total}"), Style::new().fg(DIM)),
                ]));
            }
            FleetRow::Node(idx) => {
                let n = &app.data.nodes[*idx];
                let selected = app.fleet_sel.as_deref() == Some(n.session_id.as_str());
                if selected {
                    sel = Some((lines.len(), lines.len() + 1));
                }
                lines.push(node_row(n, width, selected));
            }
        }
    }
    if lines.is_empty() {
        let msg = match app.search.as_ref().map(|s| &s.remote) {
            Some(Remote::Pending) => tr!("Asking jev…", "Preguntando a jev…"),
            Some(_) => tr!("Nothing matches the search.", "Nada coincide con la búsqueda."),
            None => tr!("No node is awake. d shows the sleeping ones.", "No hay nodos despiertos. d muestra los dormidos."),
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
            Span::styled(format!("● {}", i18n::node_state(&n)), Style::new().fg(state_color(n.state)).bold()),
            Span::styled(format!(" · {}", since_phrase(n.state_since)), Style::new().fg(DIM)),
            Span::styled(if n.alive { tr!(" · awake", " · despierto") } else { tr!(" · asleep", " · dormido") }, Style::new().fg(DIM)),
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
    if let Some(t) = &n.tool {
        lines.push(Line::default());
        lines.extend(wrap(
            vec![Span::styled(format!("{} ", t.name), Style::new().fg(ACCENT)), Span::raw(t.summary.clone().unwrap_or_default()), Span::styled(format!(" · {}", ago(t.since)), Style::new().fg(DIM))],
            width,
            vec![Span::styled(tr!("Now: ", "Ahora: "), Style::new().fg(DIM))],
            vec![],
        ));
    }
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
    let block = panel(Line::from(tr!(" Awake fleet ", " Flota despierta ").fg(SOFT)), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    for r in app.fleet_rows() {
        match r {
            FleetRow::Group { name, orchestrator, alive, .. } => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let label = if orchestrator { format!("★ {name}") } else { name };
                lines.push(fit(vec![Span::styled(format!(" {label}"), Style::new().bold()), Span::styled(format!(" {alive}"), Style::new().fg(DIM))], width));
            }
            FleetRow::Node(i) => {
                let n = &app.data.nodes[i];
                let name = n.display_name().rsplit('/').next().unwrap_or(n.display_name()).to_string();
                lines.push(fit(
                    vec![
                        Span::styled(if n.alive { "  ● " } else { "  ○ " }, Style::new().fg(state_color(n.state))),
                        Span::raw(md::one_line(&name)),
                        Span::styled(format!(" {}", i18n::node_state(n)), Style::new().fg(DIM)),
                    ],
                    width,
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(tr!(" No node is awake.", " Ningún nodo despierto.").fg(DIM)));
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
            lines.push(key("C", tr!("general comment", "comentario general")));
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

fn draw_composer(f: &mut Frame, app: &App, area: Rect) {
    let Some(c) = app.composer.as_ref() else { return };
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
        Target::MoreContext { id, title } => {
            let t = md::one_line(title);
            trf!("#{id} {t} · what do you need to know? (optional)", "#{id} {t} · ¿qué te falta saber? (opcional)")
        }
        Target::ReplyReport { name, .. } => trf!("Reply to {name} (marks it read)", "Responder a {name} (se marca leído)"),
        Target::Message { name, .. } => trf!("Message to {name}", "Mensaje a {name}"),
        Target::NewRequest => tr!("New work: the orchestrator prepares the mission and launches the node", "Trabajo nuevo: el orquestador prepara la misión y lanza el nodo").into(),
        Target::Comment { quote, .. } => {
            let q = md::one_line(quote);
            trf!("Comment on «{q}»", "Comentario sobre «{q}»")
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
        Target::MoreContext { .. } => tr!("sends it back", "la devuelve"),
        Target::Comment { .. } | Target::General { send: false } => tr!("saves", "guarda"),
        _ => tr!("sends", "envía"),
    }
}

fn footer_keys(app: &App) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if let Some(c) = app.composer.as_ref() {
        return vec![
            k("⏎", composer_action(&c.target)),
            k("Alt+⏎", tr!("new line", "nueva línea")),
            k("Ctrl+W", tr!("delete word", "borra palabra")),
            k("Ctrl+U", tr!("delete line", "borra línea")),
            k("Esc", tr!("cancel", "cancela")),
        ];
    }
    if let Some(r) = app.reader.as_ref() {
        if r.find.as_ref().is_some_and(|f| f.editing) {
            return vec![k("⏎", tr!("find", "buscar")), k("Esc", tr!("cancel", "cancela"))];
        }
        let mut v = vec![k("j/k", tr!("block", "bloque")), k("^d/^u", tr!("half page", "media página")), k("/", tr!("search", "buscar"))];
        if r.find.is_some() {
            v.push(k("n/N", tr!("next/prev", "sig./ant.")));
        }
        v.extend([k("c", tr!("comment", "comentar")), k("C", tr!("general", "general")), k("d", tr!("delete", "borrar"))]);
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
    let tail = [k("^d/^u", tr!("scroll detail", "desplazar detalle")), k("Tab", tr!("fleet", "flota")), k("n", tr!("new", "nuevo")), k("q", tr!("quit", "salir"))];
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
        Some(Item::R(_)) => {
            let mut v = vec![pane, k("o", tr!("expand", "expandir")), k("x", tr!("mark read", "leído")), k("r", tr!("reply", "responder")), k("a", tr!("its terminal", "su terminal"))];
            v.extend(tail);
            v
        }
        Some(Item::P(p)) => {
            let n = p.methods().len();
            let mut v = vec![pane, k(&format!("1-{n}"), tr!("merge", "fusionar")), k("d", "diff"), k("r", tr!("request changes", "pedir cambios")), k("w", "GitHub"), k("x", tr!("set aside", "apartar")), k("a", tr!("its terminal", "su terminal"))];
            v.extend(tail);
            v
        }
        None => vec![k("n", tr!("new work", "trabajo nuevo")), k("M", tr!("orchestrator", "orquestador")), k("Tab", tr!("fleet", "flota")), k("q", tr!("quit", "salir"))],
    }
}

/// Action results float above the key bar, wrapped, so they never hide the keys or get cut.
fn draw_flash(f: &mut Frame, app: &App, body: Rect) {
    let Some(fl) = &app.flash else { return };
    let (mark, color) = if fl.error { ("✗ ", Color::LightRed) } else { ("✓ ", Color::LightGreen) };
    let width = body.width.saturating_sub(4).clamp(10, 100);
    let text = format!("{mark}{}", fl.text);
    let lines = (text.chars().count() as u16).div_ceil(width.saturating_sub(4).max(1)).clamp(1, 6);
    let height = lines + 2;
    let area = Rect { x: body.x + (body.width.saturating_sub(width)) / 2, y: body.bottom().saturating_sub(height), width, height };
    f.render_widget(Clear, area);
    let panel = Panel::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(color));
    f.render_widget(Paragraph::new(Span::styled(text, Style::new().fg(color).bold())).wrap(Wrap { trim: true }).block(panel), area);
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
        row("gg  G", tr!("first / last", "primero / último")),
        row("Ctrl-d  Ctrl-u", tr!("half a page down / up (scrolls the detail)", "media página abajo / arriba (desplaza el detalle)")),
        row("Ctrl-f  Ctrl-b", tr!("a full page down / up; also Space, PgDn, PgUp", "página entera abajo / arriba; también Espacio, AvPág, RePág")),
        row("h/l  ←→", tr!("focus the list / the detail pane", "enfocar la lista / el panel de detalle")),
        row("o  ⏎", tr!("expand: open the whole question, plan or report", "expandir: abrir entera la pregunta, plan o informe")),
        row("Esc", tr!("back: close, collapse or leave the detail pane", "volver: cerrar, plegar o salir del panel de detalle")),
        row(":", tr!("command line: :q  :help  :inbox  :fleet  :<number>", "línea de órdenes: :q  :help  :inbox  :fleet  :<número>")),
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
        row("x", tr!("mark as read; x again closes the node that sent it", "marcar como leído; x otra vez cierra el nodo que lo envió")),
        row("J/K", tr!("scroll the detail 5 lines", "desplazar el detalle 5 líneas")),
        row("a", tr!("open the node's terminal (← on an empty line returns)", "abrir el terminal del nodo (← con la línea vacía vuelve)")),
        Line::default(),
        section(tr!("Reading (after o)", "Lectura (tras o)")),
        row("j/k", tr!("next / previous block; long blocks scroll line by line", "bloque siguiente / anterior; los bloques largos avanzan línea a línea")),
        row("/  n  N", tr!("search the text · next match · previous match", "buscar en el texto · siguiente · anterior")),
        row("c · C · d", tr!("comment on block · general comment · delete comment", "comentar bloque · comentario general · borrar")),
        row("a · p", tr!("approve · request changes (plans)", "aprobar · pedir cambios (planes)")),
        row("s · x", tr!("send comments · mark read (documents)", "enviar comentarios · leído (documentos)")),
        row("q  o  Esc", tr!("close the reading", "cerrar la lectura")),
        Line::default(),
        section(tr!("Fleet", "Flota")),
        row("/", tr!("search; if text finds nothing, it asks jev", "buscar; si el texto no encuentra nada, pregunta a jev")),
        row("r · ⏎", tr!("message the node · its terminal", "mensaje al nodo · su terminal")),
        row("o", tr!("expand the node's detail to full screen", "expandir el detalle del nodo a pantalla completa")),
        row("x", tr!("close the node (twice to confirm; never discards work)", "cerrar el nodo (dos veces para confirmar; nunca descarta trabajo)")),
        Line::default(),
        section(tr!("Always", "Siempre")),
        row("Tab", tr!("inbox ⇄ fleet", "bandeja ⇄ flota")),
        row("n · M", tr!("new work · message the orchestrator", "trabajo nuevo · mensaje al orquestador")),
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

    use super::super::tests::{app_with, ctrl, pr_card, press, question};
    use super::*;

    fn screen(app: &mut App, w: u16, h: u16) -> String {
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| draw(f, app)).unwrap();
        let buf = t.backend().buffer().clone();
        (0..h).map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>()).collect::<Vec<_>>().join("\n")
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
        assert!(s.contains("▰▰▱▱ 53% ▰▰▰▰ 41/18% ▰▰▱▱ 44% ▰▱▱▱ 34%"), "{s}");
        let r = readings(&app.data.machine, app.data.usage.as_ref());
        assert_eq!(r.iter().map(|r| r.level).collect::<Vec<_>>(), [Level::Calm, Level::Hot, Level::Calm, Level::Calm]);
        app.data.usage = None;
        assert!(screen(&mut app, 160, 20).contains("▰▰▱▱ 44% ▰▱▱▱ 34%"));
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
}
