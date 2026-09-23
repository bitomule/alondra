use std::collections::HashMap;

use alondra_core::{NodeState, Question, QuestionOrigin, Report, ReportKind, now_millis};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block as Panel, BorderType, Clear, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap};

use super::md::{self, str_width, truncate, wrap};
use super::{App, FleetRow, Item, Key, Remote, Screen, Target, said, who};
use crate::client::FleetNode;

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
    if app.help {
        draw_help(f);
    }
}

pub fn ago(ms: i64) -> String {
    let s = ((now_millis() - ms) / 1000).max(0);
    match s {
        0..=59 => "ahora".into(),
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

fn keep_visible(scroll: &mut usize, range: (usize, usize), height: usize, total: usize) {
    let (s, e) = range;
    if s < *scroll {
        *scroll = s.saturating_sub(1);
    }
    if e > *scroll + height {
        *scroll = e.saturating_sub(height).min(s);
    }
    *scroll = (*scroll).min(total.saturating_sub(height));
}

fn state_color(s: NodeState) -> Color {
    match s {
        NodeState::WaitingAnswer => Color::Yellow,
        NodeState::Unclear => Color::Magenta,
        NodeState::Delivered => Color::Cyan,
        NodeState::WaitingMachine => Color::LightRed,
        NodeState::Working => Color::Green,
        NodeState::Done => DIM,
    }
}

fn question_glyph(q: &Question) -> (&'static str, &'static str, Color) {
    match q.origin {
        QuestionOrigin::Plan => ("◆", "plan", Color::LightMagenta),
        QuestionOrigin::Permission => ("!", "permiso", Color::LightRed),
        _ => ("?", "pregunta", ACCENT),
    }
}

fn report_glyph(r: &Report) -> (&'static str, &'static str, Color) {
    match r.kind {
        ReportKind::Document => ("▤", "documento", Color::Cyan),
        ReportKind::Final => ("✓", "terminado", Color::LightGreen),
        _ => ("•", "aviso", Color::LightBlue),
    }
}

fn item_glyph(item: &Item) -> (&'static str, &'static str, Color) {
    match item {
        Item::Q(q) => question_glyph(q),
        Item::R(r) => report_glyph(r),
    }
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
    left.push(tab(format!("Bandeja {}", app.inbox_keys().len()), app.screen == Screen::Inbox));
    left.push(tab(format!("Flota {}", app.data.nodes.len()), app.screen == Screen::Fleet));
    if app.offline {
        left.push(Span::raw(" "));
        left.push(Span::styled(" sin conexión con alondrad ", Style::new().fg(Color::White).bg(Color::Red).add_modifier(Modifier::BOLD)));
    }
    let working = app.data.nodes.iter().filter(|n| n.state == NodeState::Working && n.alive).count();
    let mem = m.memory_free_percent.map(|p| format!("{p}% libre")).unwrap_or_else(|| format!("{:.1} GB libres", m.memory_available as f64 / 1_073_741_824.0));
    let warn = if m.unreliable { " ⚠" } else { "" };
    let right = format!("{working} trabajando · memoria {mem} · carga {:.1}{warn} ", m.load[0]);
    let lw: usize = left.iter().map(Span::width).sum();
    f.render_widget(Paragraph::new(Line::from(left)), area);
    if app.loaded && lw + str_width(&right) + 2 <= area.width as usize {
        f.render_widget(Paragraph::new(Line::from(right.fg(DIM)).right_aligned()), area);
    }
}

fn draw_inbox(f: &mut Frame, app: &mut App, area: Rect) {
    let w = area.width;
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
                meta.push(Span::styled(format!(" {}", md::one_line(&o.label)), Style::new().fg(SOFT)));
            }
        }
        Item::R(r) => {
            let clips = r.attachments.len();
            if clips > 0 {
                meta.push(Span::styled(format!(" · 📎{clips}"), Style::new().fg(DIM)));
            }
        }
    }
    vec![first, fit(meta, width)]
}

fn draw_inbox_list(f: &mut Frame, app: &mut App, area: Rect) {
    let keys = app.inbox_keys();
    let nq = keys.iter().filter(|k| matches!(k, Key::Q(_))).count();
    let block = panel(Line::from(" Bandeja ".bold()), true);
    let inner = block.inner(area);
    f.render_widget(block, area);
    if !app.loaded {
        let msg = if app.offline { "alondrad no responde · reintentando…" } else { "Conectando con alondrad…" };
        centered_lines(f, inner, vec![Line::from(msg.fg(DIM))]);
        return;
    }
    if keys.is_empty() {
        centered_lines(
            f,
            inner,
            vec![
                Line::from("✓ Todo al día".fg(Color::LightGreen).bold()),
                Line::from("Nadie te espera y no hay nada por leer.".fg(DIM)),
                Line::default(),
                Line::from(vec![Span::styled("n", Style::new().fg(ACCENT).bold()), " encarga trabajo nuevo".fg(DIM)]),
                Line::from(vec![Span::styled("Tab", Style::new().fg(ACCENT).bold()), " mira la flota".fg(DIM)]),
            ],
        );
        return;
    }
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines: Vec<Line> = vec![];
    let mut sel_range = None;
    let sections = [(true, "TE ESPERAN", ACCENT, nq), (false, "PARA LEER", Color::Cyan, keys.len() - nq)];
    for (questions, label, color, count) in sections {
        if count == 0 {
            continue;
        }
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        lines.push(Line::from(vec![Span::styled(format!(" {label} "), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(count.to_string(), Style::new().fg(DIM))]));
        for &k in keys.iter().filter(|k| matches!(k, Key::Q(_)) == questions) {
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
    let (title, node, at, body, attachments, links) = match item {
        Item::Q(q) => (&q.title, who(&q.node_name, &q.session_id), q.asked_at, &q.detail, &q.attachments, &q.links),
        Item::R(r) => (&r.title, who(&r.node_name, &r.session_id), r.created_at, &r.body, &r.attachments, &r.links),
    };
    let project = match item {
        Item::Q(q) => app.node(&q.session_id),
        Item::R(r) => app.node(&r.session_id),
    }
    .and_then(|n| n.project.clone());
    let mut meta = vec![Span::styled(format!("{glyph} {kind}"), Style::new().fg(color).add_modifier(Modifier::BOLD)), Span::styled(format!(" · {node}"), Style::new().fg(SOFT))];
    if let Some(p) = project.filter(|p| !node.starts_with(p.as_str())) {
        meta.push(Span::styled(format!(" · {p}"), Style::new().fg(DIM)));
    }
    meta.push(Span::styled(format!(" · hace {}", ago(at)).replace("hace ahora", "ahora mismo"), Style::new().fg(DIM)));
    lines.push(fit(meta, width));
    lines.push(Line::default());
    lines.extend(wrap(vec![Span::styled(md::one_line(title), Style::new().add_modifier(Modifier::BOLD))], width, vec![], vec![]));
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
            Some(Err(e)) => lines.push(Line::from(format!("No he podido cargarlo: {e}").fg(Color::LightRed))),
            None if a.stored_path.is_some() => lines.push(Line::from("Cargando…".fg(DIM))),
            None => lines.push(Line::from(format!("No se guardó copia: {}", a.source_path).fg(DIM))),
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
        lines.push(Line::from("Lo que hacía el nodo".fg(DIM).bold()));
        for l in c.lines() {
            lines.extend(wrap(vec![Span::styled(l.to_string(), Style::new().fg(DIM))], width, vec![], vec![]));
        }
    }
    lines
}

fn action_lines(app: &App, item: &Item, width: usize) -> Vec<Line<'static>> {
    let hint = |spans: Vec<Span<'static>>| fit(spans, width);
    match item {
        Item::Q(q) => {
            let mut out: Vec<Line> = q
                .options
                .iter()
                .enumerate()
                .take(9)
                .map(|(i, o)| {
                    let mut spans = vec![keycap(&(i + 1).to_string()), Span::styled(format!(" {}", md::one_line(&o.label)), Style::new().add_modifier(Modifier::BOLD))];
                    if let Some(d) = o.description.as_ref().filter(|d| !d.trim().is_empty()) {
                        spans.push(Span::styled(format!("  {}", md::one_line(d)), Style::new().fg(DIM)));
                    }
                    fit(spans, width)
                })
                .collect();
            let review = super::approval_of(q).is_some() || q.attachments.iter().any(|a| a.stored_path.is_some());
            let mut tail = vec![keycap("⏎"), Span::styled(if review { " revisar y comentar  " } else { " leer  " }, Style::new().fg(SOFT))];
            tail.extend([keycap("r"), Span::styled(" responder  ", Style::new().fg(SOFT)), keycap("c"), Span::styled(" más contexto", Style::new().fg(SOFT))]);
            if q.options.is_empty() {
                out.push(Line::from("Sin opciones: contesta con tus palabras.".fg(DIM)));
            }
            out.push(hint(tail));
            out
        }
        Item::R(r) => {
            let pending = app.reviews.get(&super::Key::R(r.id)).map(|rv| rv.count()).unwrap_or(0);
            let read = if r.kind == ReportKind::Document || r.attachments.iter().any(|a| a.stored_path.is_some()) { " leer y comentar  " } else { " leer  " };
            let mut spans = vec![keycap("⏎"), Span::styled(read, Style::new().fg(SOFT)), keycap("x"), Span::styled(" leído  ", Style::new().fg(SOFT)), keycap("r"), Span::styled(" responder", Style::new().fg(SOFT))];
            if pending > 0 {
                spans.push(Span::styled(format!("  ✎{pending} sin enviar"), Style::new().fg(COMMENT)));
            }
            vec![hint(spans)]
        }
    }
}

fn draw_item_detail(f: &mut Frame, app: &mut App, area: Rect) {
    let item = app.sel.and_then(|k| app.item(k));
    let Some(item) = item else {
        let block = panel(Line::from(" Detalle ".fg(DIM)), false);
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
    };
    let block = panel(Line::from(vec![Span::raw(" "), Span::styled(kind, Style::new().fg(color).bold()), Span::styled(format!(" {id} "), Style::new().fg(DIM))]), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(2) as usize;
    let actions = action_lines(app, &item, width);
    let ah = (actions.len() as u16 + 1).min(inner.height / 2);
    let [content, bottom] = Layout::vertical([Constraint::Min(1), Constraint::Length(ah)]).areas(inner);
    let lines = detail_lines(app, &item, width);
    let total = lines.len();
    let h = content.height as usize;
    app.detail_scroll = app.detail_scroll.min(total.saturating_sub(h));
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
    let mut lines = vec![Line::from("La flota ahora".fg(DIM).bold()), Line::default()];
    if counts.is_empty() {
        lines.push(Line::from("Ningún nodo despierto.".fg(DIM)));
    }
    for (s, c) in counts {
        lines.push(Line::from(vec![Span::styled("● ", Style::new().fg(state_color(s))), Span::raw(format!("{c} {}", s.label()))]));
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
        spans.push(Span::styled(format!(" {:<19}", truncate(n.state.label(), 19)), Style::new().fg(state_color(n.state))));
    }
    spans.push(Span::styled(format!("{since:>5}"), Style::new().fg(DIM)));
    fit(spans, width)
}

fn draw_fleet(f: &mut Frame, app: &mut App, area: Rect) {
    let (list, detail) = if area.width >= 100 {
        let lw = (area.width * 11 / 20).clamp(56, 90);
        let [l, d] = Layout::horizontal([Constraint::Length(lw), Constraint::Min(30)]).areas(area);
        (l, Some(d))
    } else {
        (area, None)
    };
    let hidden = app.sleeping_hidden();
    let mut block = panel(Line::from(" Flota por proyecto ".bold()), true);
    if hidden > 0 {
        block = block.title_bottom(Line::from(format!(" {hidden} dormidos ocultos · d los muestra ").fg(DIM)));
    } else if app.show_sleeping && app.search.is_none() {
        block = block.title_bottom(Line::from(" d oculta los dormidos ".fg(DIM)));
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
            (_, Some((idx, None))) if !idx.is_empty() => format!("{} por texto", idx.len()),
            (_, Some((idx, Some(_)))) if idx.is_empty() => "nada por texto ni según jev".into(),
            (_, Some((idx, Some(via)))) => format!("{} según {via}", idx.len()),
            (Remote::Pending, _) => "nada por texto · preguntando a jev…".into(),
            (Remote::Failed(e), _) => format!("jev falló: {}", md::one_line(e)),
            (Remote::Idle, _) if !q.is_empty() => "nada por texto".into(),
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
            FleetRow::Group { name, alive, total } => {
                if i > 0 {
                    lines.push(Line::default());
                }
                let label = if name == "orquestador" { "★ orquestador".to_string() } else { name.clone() };
                lines.push(Line::from(vec![
                    Span::styled(format!(" {label}"), Style::new().add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {alive} despiertos de {total}"), Style::new().fg(DIM)),
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
            Some(Remote::Pending) => "Preguntando a jev…",
            Some(_) => "Nada coincide con la búsqueda.",
            None => "No hay nodos despiertos. d muestra los dormidos.",
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
        f.render_widget(panel(Line::from(" Nodo ".fg(DIM)), false), area);
        return;
    };
    let block = panel(Line::from(vec![Span::raw(" "), Span::styled(md::one_line(n.display_name()), Style::new().bold()), Span::raw(" ")]), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let width = text.width as usize;
    let mut lines = vec![fit(
        vec![
            Span::styled(format!("● {}", n.state.label()), Style::new().fg(state_color(n.state)).bold()),
            Span::styled(format!(" · desde hace {}", ago(n.state_since)), Style::new().fg(DIM)),
            Span::styled(if n.alive { " · despierto" } else { " · dormido" }, Style::new().fg(DIM)),
        ],
        width,
    )];
    if let Some(p) = &n.project {
        lines.push(Line::from(format!("proyecto {p} · {}", n.short_id).fg(DIM)));
    }
    if let Some(cwd) = &n.cwd {
        lines.push(fit(vec![Span::styled(cwd.clone(), Style::new().fg(DIM))], width));
    }
    if let Some(t) = &n.tool {
        lines.push(Line::default());
        lines.extend(wrap(
            vec![Span::styled(format!("{} ", t.name), Style::new().fg(ACCENT)), Span::raw(t.summary.clone().unwrap_or_default()), Span::styled(format!(" · {}", ago(t.since)), Style::new().fg(DIM))],
            width,
            vec![Span::styled("Ahora: ", Style::new().fg(DIM))],
            vec![],
        ));
    }
    if let Some(needs) = &n.daemon_needs {
        lines.push(Line::default());
        lines.extend(wrap(vec![Span::styled(needs.clone(), Style::new().fg(Color::Yellow))], width, vec![Span::styled("Necesita: ", Style::new().fg(DIM))], vec![]));
    }
    let said = said(&n);
    if !said.trim().is_empty() {
        lines.push(Line::default());
        lines.push(Line::from("Lo último que dijo".fg(DIM).bold()));
        lines.extend(md::render(&md::parse(&said), width).lines);
    }
    let total = lines.len();
    app.fleet_detail_scroll = app.fleet_detail_scroll.min(total.saturating_sub(inner.height as usize));
    f.render_widget(Paragraph::new(lines).scroll((app.fleet_detail_scroll as u16, 0)), text);
    scrollbar(f, inner, total, app.fleet_detail_scroll);
}

fn draw_compact_fleet(f: &mut Frame, app: &App, area: Rect) {
    let block = panel(Line::from(" Flota despierta ".fg(SOFT)), false);
    let inner = block.inner(area);
    f.render_widget(block, area);
    let width = inner.width.saturating_sub(1) as usize;
    let mut lines = vec![];
    for r in app.fleet_rows() {
        match r {
            FleetRow::Group { name, alive, .. } => {
                if !lines.is_empty() {
                    lines.push(Line::default());
                }
                let label = if name == "orquestador" { "★ orquestador".to_string() } else { name };
                lines.push(fit(vec![Span::styled(format!(" {label}"), Style::new().bold()), Span::styled(format!(" {alive}"), Style::new().fg(DIM))], width));
            }
            FleetRow::Node(i) => {
                let n = &app.data.nodes[i];
                let name = n.display_name().rsplit('/').next().unwrap_or(n.display_name()).to_string();
                lines.push(fit(
                    vec![
                        Span::styled(if n.alive { "  ● " } else { "  ○ " }, Style::new().fg(state_color(n.state))),
                        Span::raw(md::one_line(&name)),
                        Span::styled(format!(" {}", n.state.label()), Style::new().fg(DIM)),
                    ],
                    width,
                ));
            }
        }
    }
    if lines.is_empty() {
        lines.push(Line::from(" Ningún nodo despierto.".fg(DIM)));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_reader(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(r) = app.reader.as_mut() else { return };
    let review = app.reviews.get(&r.key);
    let (glyph, color) = match r.kind {
        "plan" => ("◆", Color::LightMagenta),
        "permiso" => ("!", Color::LightRed),
        "pregunta" => ("?", ACCENT),
        "documento" => ("▤", Color::Cyan),
        "terminado" => ("✓", Color::LightGreen),
        _ => ("•", Color::LightBlue),
    };
    let still_open = match r.key {
        Key::Q(id) => app.data.open_questions.iter().any(|q| q.id == id),
        Key::R(id) => app.data.news.iter().any(|x| x.id == id),
    };
    let mut title = vec![
        Span::raw(" "),
        Span::styled(format!("{glyph} {}", r.kind), Style::new().fg(color).bold()),
        Span::styled(format!(" · {} · ", r.node), Style::new().fg(SOFT)),
        Span::styled(truncate(&md::one_line(&r.title), (area.width as usize).saturating_sub(40).max(10)), Style::new().bold()),
        Span::raw(" "),
    ];
    if !still_open && app.loaded {
        title.push(Span::styled(" ya no está pendiente ", Style::new().fg(Color::Black).bg(Color::LightRed)));
    }
    let n_comments = review.map(|rv| rv.count()).unwrap_or(0);
    let status = if n_comments == 0 {
        " sin comentarios ".to_string()
    } else {
        format!(" {n_comments} {} sin enviar ", if n_comments == 1 { "comentario" } else { "comentarios" })
    };
    let block = panel(Line::from(title), true).title_bottom(Line::from(Span::styled(status, Style::new().fg(if n_comments > 0 { COMMENT } else { DIM }))).right_aligned());
    let inner = block.inner(area);
    f.render_widget(block, area);
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
    let width = doc.width.saturating_sub(4) as usize;
    let rendered = md::render(&r.blocks, width);
    let h = doc.height as usize;
    r.view_h = h;
    let total = rendered.lines.len();
    if let Some(&range) = rendered.ranges.get(r.cursor) {
        keep_visible(&mut r.scroll, range, h, total);
    }
    r.ranges = rendered.ranges;
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
        .title(Line::from(vec![Span::styled(" Comentarios ", Style::new().bold()), Span::styled(n.to_string(), Style::new().fg(DIM))]));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let width = text.width as usize;
    let mut lines: Vec<Line> = vec![];
    let key = |k: &str, d: &str| Line::from(vec![Span::styled(format!("{k:<4}"), Style::new().fg(ACCENT).bold()), Span::styled(d.to_string(), Style::new().fg(DIM))]);
    match review.filter(|rv| !rv.is_empty()) {
        None => {
            lines.push(Line::from("Todavía ninguno.".fg(DIM)));
            lines.push(Line::default());
            lines.push(key("j/k", "mueve el cursor por bloques"));
            lines.push(key("c", "comenta el bloque marcado"));
            lines.push(key("C", "comentario general"));
            if approval {
                lines.push(key("a", "aprueba tal cual"));
                lines.push(key("p", "pide cambios con tus notas"));
            } else {
                lines.push(key("s", "envía tus comentarios"));
            }
            if let Key::Q(id) = r.key
                && let Some(q) = app.data.open_questions.iter().find(|q| q.id == id).filter(|q| !q.options.is_empty())
            {
                lines.push(Line::default());
                lines.push(Line::from("O elige directamente".fg(DIM)));
                for (i, o) in q.options.iter().enumerate().take(9) {
                    lines.push(fit(vec![Span::styled(format!("{:<4}", i + 1), Style::new().fg(ACCENT).bold()), Span::raw(md::one_line(&o.label))], width));
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
        Target::Answer { id, title } => format!("Responder a #{id} · {}", md::one_line(title)),
        Target::MoreContext { id, title } => format!("#{id} {} · ¿qué te falta saber? (opcional)", md::one_line(title)),
        Target::ReplyReport { name, .. } => format!("Responder a {name} (se marca leído)"),
        Target::Message { name, .. } => format!("Mensaje a {name}"),
        Target::NewRequest => "Trabajo nuevo: el orquestador prepara la misión y lanza el nodo".into(),
        Target::Comment { quote, .. } => format!("Comentario sobre «{}»", md::one_line(quote)),
        Target::General { send: true } if app.reader.as_ref().is_some_and(|r| r.approval.is_some()) => "¿Qué cambios pides? (⏎ los envía)".into(),
        Target::General { send: true } => "Tus comentarios (⏎ los envía)".into(),
        Target::General { send: false } => "Comentario general".into(),
    };
    let action = composer_action(&c.target);
    let title = truncate(&title, (area.width as usize).saturating_sub(6));
    let hint = format!(" ⏎ {action} · Alt+⏎ nueva línea · Esc cancela ");
    let block = panel(Line::from(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).bold())), true).title_bottom(Line::from(hint.fg(DIM)).right_aligned());
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let text = Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner };
    let (lines, (row, col)) = c.input.layout(text.width as usize);
    let scroll = row.saturating_sub(text.height.saturating_sub(1) as usize);
    if c.input.text().is_empty() {
        f.render_widget(Paragraph::new(Line::from("Escribe aquí…".fg(DIM))), text);
    } else {
        f.render_widget(Paragraph::new(lines.into_iter().map(Line::from).collect::<Vec<_>>()).scroll((scroll as u16, 0)), text);
    }
    f.set_cursor_position(Position { x: text.x + col as u16, y: text.y + (row - scroll) as u16 });
}

fn composer_action(t: &Target) -> &'static str {
    match t {
        Target::MoreContext { .. } => "la devuelve",
        Target::Comment { .. } | Target::General { send: false } => "guarda",
        _ => "envía",
    }
}

fn footer_keys(app: &App) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_string(), b.to_string());
    if let Some(c) = app.composer.as_ref() {
        return vec![k("⏎", composer_action(&c.target)), k("Alt+⏎", "nueva línea"), k("Ctrl+W", "borra palabra"), k("Ctrl+U", "borra línea"), k("Esc", "cancela")];
    }
    if let Some(r) = app.reader.as_ref() {
        let mut v = vec![k("j/k", "bloque"), k("c", "comentar"), k("C", "general"), k("d", "borrar")];
        match (&r.approval, r.key) {
            (Some(a), _) => {
                v.push(k("a", &format!("aprobar{}", a.approve.as_ref().map(|l| format!(" («{l}»)")).unwrap_or_default())));
                v.push(k("p", "pedir cambios"));
            }
            (None, Key::Q(_)) => v.push(k("s", "enviar como respuesta")),
            (None, Key::R(_)) => {
                v.push(k("s", "enviar al nodo"));
                v.push(k("x", "leído"));
            }
        }
        v.push(k("Esc", "volver"));
        return v;
    }
    if app.screen == Screen::Fleet {
        if app.search.as_ref().is_some_and(|s| s.editing) {
            return vec![k("⏎", "fijar filtro"), k("Esc", "quitar búsqueda")];
        }
        let mut v = vec![k("j/k", "mover"), k("/", "buscar")];
        if app.search.is_some() {
            v.push(k("Esc", "quitar búsqueda"));
        }
        v.extend([k("r", "mensaje"), k("⏎", "su terminal (← vuelve)"), k("d", "dormidos"), k("n", "nuevo"), k("Tab", "bandeja"), k("q", "salir")]);
        return v;
    }
    match app.sel.and_then(|s| app.item(s)) {
        Some(Item::Q(q)) => {
            let mut v = vec![];
            match q.options.len() {
                0 => {}
                1 => v.push(k("1", "elegir")),
                n => v.push(k(&format!("1-{}", n.min(9)), "elegir")),
            }
            v.extend([k("⏎", "leer"), k("r", "responder"), k("c", "más contexto"), k("a", "su terminal"), k("Tab", "flota"), k("n", "nuevo"), k("q", "salir")]);
            v
        }
        Some(Item::R(_)) => vec![k("⏎", "leer"), k("x", "leído"), k("r", "responder"), k("a", "su terminal"), k("Tab", "flota"), k("n", "nuevo"), k("q", "salir")],
        None => vec![k("n", "trabajo nuevo"), k("o", "orquestador"), k("Tab", "flota"), k("q", "salir")],
    }
}

fn draw_foot(f: &mut Frame, app: &App, area: Rect) {
    if let Some(fl) = &app.flash {
        let (mark, color) = if fl.error { ("✗ ", Color::LightRed) } else { ("✓ ", Color::LightGreen) };
        f.render_widget(Paragraph::new(fit(vec![Span::styled(format!(" {mark}{}", fl.text), Style::new().fg(color).bold())], area.width as usize)), area);
        return;
    }
    let width = area.width as usize;
    let help = vec![Span::styled("?", Style::new().fg(ACCENT).bold()), Span::styled(" ayuda", Style::new().fg(DIM))];
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

fn draw_help(f: &mut Frame) {
    let section = |t: &str| Line::from(Span::styled(format!(" {t}"), Style::new().fg(ACCENT).bold()));
    let row = |k: &str, d: &str| Line::from(vec![Span::styled(format!("  {k:<10}"), Style::new().bold()), Span::styled(d.to_string(), Style::new().fg(SOFT))]);
    let lines = vec![
        section("Bandeja"),
        row("j/k ↑↓", "moverte; al contestar pasas solo a la siguiente"),
        row("1-9", "elegir una opción de la pregunta"),
        row("⏎", "leer entero: plan o documento con comentarios por bloque"),
        row("r", "responder con tus palabras"),
        row("c", "devolver la pregunta pidiendo más contexto"),
        row("x", "marcar como leído"),
        row("J/K", "desplazar el detalle"),
        row("a", "abrir el terminal del nodo (← con la línea vacía vuelve)"),
        Line::default(),
        section("Lectura"),
        row("j/k", "bloque siguiente / anterior; espacio pasa página"),
        row("c · C · d", "comentar bloque · comentario general · borrar"),
        row("a · p", "aprobar · pedir cambios (planes)"),
        row("s · x", "enviar comentarios · leído (documentos)"),
        Line::default(),
        section("Flota"),
        row("/", "buscar; si el texto no encuentra nada, pregunta a jev"),
        row("r · ⏎", "mensaje al nodo · su terminal"),
        row("d", "mostrar u ocultar los dormidos"),
        Line::default(),
        section("Siempre"),
        row("Tab", "bandeja ⇄ flota"),
        row("n · o", "trabajo nuevo · mensaje al orquestador"),
        row("q", "salir"),
    ];
    let area = f.area();
    let w = 76.min(area.width.saturating_sub(2));
    let h = (lines.len() as u16 + 2).min(area.height.saturating_sub(2));
    let r = Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h };
    f.render_widget(Clear, r);
    f.render_widget(Paragraph::new(lines).block(panel(Line::from(" Teclas · cualquier tecla cierra ".bold()), true)), r);
}

#[cfg(test)]
mod tests {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    use super::super::tests::{app_with, question};
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
        assert!(s.contains("Todo al día"), "{s}");
        assert!(s.contains("encarga trabajo nuevo"));
    }

    #[test]
    fn options_are_visible_without_opening_anything_even_when_narrow() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Plan, &["Aprobar", "Pedir cambios"])]);
        app.loaded = true;
        for (w, h) in [(60, 16), (100, 30), (200, 60)] {
            let s = screen(&mut app, w, h);
            assert!(s.contains("1 Aprobar"), "{w}x{h}\n{s}");
            assert!(s.contains("? ayuda"), "{w}x{h}\n{s}");
        }
    }
}
