use std::sync::mpsc;
use std::time::Duration;

use alondra_core::{Message, MessageFrom, Node, NodeState, Question, QuestionOrigin, StateSource, now_millis};
use anyhow::Result;
use crossterm::event::{self, Event as TermEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState, Wrap};
use ratatui::{DefaultTerminal, Frame};
use serde_json::json;

use crate::client::{Client, Overview};
use crate::mcp::urlencode;

#[derive(PartialEq, Clone, Copy)]
enum Focus {
    Questions,
    News,
    Fleet,
}

enum Mode {
    Normal,
    Input { target: Target, buffer: String },
    Detail { title: String, body: Text<'static>, scroll: u16 },
}

#[derive(Clone)]
enum Target {
    Answer(u64, String),
    Message { session: String, name: String, ack: bool },
}

struct App {
    data: Overview,
    connected: bool,
    focus: Focus,
    fleet_view: bool,
    questions: ListState,
    news: ListState,
    fleet: TableState,
    mode: Mode,
    status: Option<(String, i64)>,
    pinned_question: Option<u64>,
    pinned_news: Option<String>,
    pinned_fleet: Option<String>,
}

enum Update {
    Data(Box<Overview>),
    Offline,
}

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Update>(4);
    std::thread::Builder::new().name("alondra-sync".into()).spawn(move || sync_loop(tx))?;
    let mut terminal = ratatui::init();
    let res = main_loop(&mut terminal, rx);
    ratatui::restore();
    res
}

fn sync_loop(tx: mpsc::SyncSender<Update>) {
    let quick = Client::new(Duration::from_secs(5));
    let waiter = Client::new(Duration::from_secs(70));
    loop {
        let tick;
        match quick.overview() {
            Ok(o) => {
                tick = o.tick;
                if tx.send(Update::Data(Box::new(o))).is_err() {
                    return;
                }
            }
            Err(_) => {
                if tx.send(Update::Offline).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        }
        if waiter.get::<serde_json::Value>(&format!("/v1/changes?after={tick}&wait=50")).is_err() {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn main_loop(terminal: &mut DefaultTerminal, rx: mpsc::Receiver<Update>) -> Result<()> {
    let mut app = App {
        data: Overview::default(),
        connected: false,
        focus: Focus::Questions,
        fleet_view: false,
        questions: ListState::default(),
        news: ListState::default(),
        fleet: TableState::default(),
        mode: Mode::Normal,
        status: None,
        pinned_question: None,
        pinned_news: None,
        pinned_fleet: None,
    };
    let client = Client::new(Duration::from_secs(10));
    loop {
        while let Ok(u) = rx.try_recv() {
            match u {
                Update::Data(o) => {
                    app.data = *o;
                    app.connected = true;
                    repin(&mut app);
                }
                Update::Offline => app.connected = false,
            }
        }
        clamp(&mut app);
        pin(&mut app);
        terminal.draw(|f| draw(f, &mut app))?;
        if event::poll(Duration::from_millis(150))?
            && let TermEvent::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
        {
            if matches!(app.mode, Mode::Normal) && k.code == KeyCode::Char('a') {
                if let Some(short) = attach_target(&app) {
                    ratatui::restore();
                    if let Err(e) = crate::attach::attach(&short) {
                        flash(&mut app, format!("Error: {e}"));
                    }
                    *terminal = ratatui::init();
                    terminal.clear()?;
                }
                continue;
            }
            if !handle_key(&mut app, &client, k) {
                return Ok(());
            }
        }
    }
}

fn clamp_list(state: &mut ListState, len: usize) {
    match state.selected() {
        _ if len == 0 => state.select(None),
        None => state.select(Some(0)),
        Some(i) if i >= len => state.select(Some(len - 1)),
        _ => {}
    }
}

fn clamp(app: &mut App) {
    clamp_list(&mut app.questions, app.data.open_questions.len());
    clamp_list(&mut app.news, app.data.news.len());
    let nn = app.data.nodes.len();
    match app.fleet.selected() {
        _ if nn == 0 => app.fleet.select(None),
        None => app.fleet.select(Some(0)),
        Some(i) if i >= nn => app.fleet.select(Some(nn - 1)),
        _ => {}
    }
    if !app.fleet_view && app.focus == Focus::Fleet {
        app.focus = Focus::Questions;
    }
    if app.focus == Focus::Questions && app.data.open_questions.is_empty() && !app.data.news.is_empty() {
        app.focus = Focus::News;
    }
}

fn repin(app: &mut App) {
    if let Some(id) = app.pinned_question
        && let Some(i) = app.data.open_questions.iter().position(|q| q.id == id)
    {
        app.questions.select(Some(i));
    }
    if let Some(sid) = &app.pinned_news
        && let Some(i) = app.data.news.iter().position(|n| &n.session_id == sid)
    {
        app.news.select(Some(i));
    }
    if let Some(sid) = &app.pinned_fleet
        && let Some(i) = app.data.nodes.iter().position(|n| &n.session_id == sid)
    {
        app.fleet.select(Some(i));
    }
}

fn pin(app: &mut App) {
    app.pinned_question = selected_question(app).map(|q| q.id);
    app.pinned_news = selected_news(app).map(|n| n.session_id);
    app.pinned_fleet = selected_fleet(app).map(|n| n.session_id);
}

fn flash(app: &mut App, msg: impl Into<String>) {
    app.status = Some((msg.into(), now_millis()));
}

fn selected_question(app: &App) -> Option<Question> {
    app.questions.selected().and_then(|i| app.data.open_questions.get(i).cloned())
}

fn selected_news(app: &App) -> Option<Node> {
    app.news.selected().and_then(|i| app.data.news.get(i).cloned())
}

fn selected_fleet(app: &App) -> Option<Node> {
    app.fleet.selected().and_then(|i| app.data.nodes.get(i).cloned())
}

fn orchestrator(app: &App) -> Option<Node> {
    app.data.nodes.iter().find(|n| n.is_orchestrator).cloned()
}

fn node_of(app: &App, session: &str) -> Option<Node> {
    app.data.nodes.iter().find(|n| n.session_id == session).cloned()
}

fn attach_target(app: &App) -> Option<String> {
    match app.focus {
        Focus::Fleet => selected_fleet(app).map(|n| n.short_id),
        Focus::News => selected_news(app).map(|n| n.short_id),
        Focus::Questions => selected_question(app).and_then(|q| node_of(app, &q.session_id)).map(|n| n.short_id),
    }
}

fn input(target: Target) -> Mode {
    Mode::Input { target, buffer: String::new() }
}

fn handle_key(app: &mut App, client: &Client, k: KeyEvent) -> bool {
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
        return false;
    }
    match &mut app.mode {
        Mode::Input { target, buffer } => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Normal,
                KeyCode::Enter if k.modifiers.contains(KeyModifiers::ALT) => buffer.push('\n'),
                KeyCode::Enter => {
                    let (target, text) = (target.clone(), buffer.trim().to_string());
                    app.mode = Mode::Normal;
                    if !text.is_empty() {
                        submit(app, client, target, text, None);
                    }
                }
                KeyCode::Backspace => {
                    buffer.pop();
                }
                KeyCode::Char(c) => buffer.push(c),
                _ => {}
            }
            return true;
        }
        Mode::Detail { scroll, .. } => {
            match k.code {
                KeyCode::Down | KeyCode::Char('j') => *scroll = scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => *scroll = scroll.saturating_sub(1),
                KeyCode::PageDown | KeyCode::Char(' ') => *scroll = scroll.saturating_add(10),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                KeyCode::Char('r') => {
                    app.mode = Mode::Normal;
                    return handle_key(app, client, k);
                }
                _ => app.mode = Mode::Normal,
            }
            return true;
        }
        Mode::Normal => {}
    }
    match k.code {
        KeyCode::Char('q') | KeyCode::Esc => return false,
        KeyCode::Char('f') => {
            app.fleet_view = !app.fleet_view;
            app.focus = if app.fleet_view { Focus::Fleet } else { Focus::Questions };
        }
        KeyCode::Tab | KeyCode::BackTab if !app.fleet_view => {
            app.focus = if app.focus == Focus::Questions { Focus::News } else { Focus::Questions };
        }
        KeyCode::Down | KeyCode::Char('j') => move_sel(app, 1),
        KeyCode::Up | KeyCode::Char('k') => move_sel(app, -1),
        KeyCode::Enter => open_detail(app),
        KeyCode::Char('o') => match orchestrator(app) {
            Some(n) => app.mode = input(Target::Message { session: n.session_id, name: "el orquestador".into(), ack: false }),
            None => flash(app, "El orquestador todavía no está conectado a Alondra"),
        },
        KeyCode::Char('r') | KeyCode::Char('m') => match app.focus {
            Focus::Questions => {
                if let Some(q) = selected_question(app) {
                    app.mode = input(Target::Answer(q.id, q.title));
                }
            }
            Focus::News => {
                if let Some(n) = selected_news(app) {
                    app.mode = input(Target::Message { session: n.session_id.clone(), name: n.display_name().to_string(), ack: true });
                }
            }
            Focus::Fleet => {
                if let Some(n) = selected_fleet(app) {
                    app.mode = input(Target::Message { session: n.session_id.clone(), name: n.display_name().to_string(), ack: true });
                }
            }
        },
        KeyCode::Char('x') | KeyCode::Char(' ') if app.focus == Focus::News => {
            if let Some(n) = selected_news(app) {
                match client.post(&format!("/v1/nodes/{}/ack", urlencode(&n.session_id)), &json!({})) {
                    Ok(_) => flash(app, format!("Visto: {}", n.display_name())),
                    Err(e) => flash(app, format!("Error: {e}")),
                }
            }
        }
        KeyCode::Char(c @ '1'..='9') if app.focus == Focus::Questions => {
            if let Some(q) = selected_question(app) {
                let i = c.to_digit(10).unwrap_or(1) as usize - 1;
                if let Some(opt) = q.options.get(i) {
                    submit(app, client, Target::Answer(q.id, q.title.clone()), opt.label.clone(), Some(opt.label.clone()));
                }
            }
        }
        _ => {}
    }
    true
}

fn move_sel(app: &mut App, d: i32) {
    fn step(len: usize, sel: Option<usize>, d: i32) -> Option<usize> {
        (len > 0).then(|| (sel.unwrap_or(0) as i32 + d).clamp(0, len as i32 - 1) as usize)
    }
    match app.focus {
        Focus::Questions => app.questions.select(step(app.data.open_questions.len(), app.questions.selected(), d)),
        Focus::News => app.news.select(step(app.data.news.len(), app.news.selected(), d)),
        Focus::Fleet => app.fleet.select(step(app.data.nodes.len(), app.fleet.selected(), d)),
    }
}

fn submit(app: &mut App, client: &Client, target: Target, text: String, option: Option<String>) {
    let res = match &target {
        Target::Answer(id, _) => client.post(&format!("/v1/questions/{id}/answer"), &json!({"text": text, "option": option, "by": "David", "device": "tui"})),
        Target::Message { session, ack, .. } => {
            let r = client.post(&format!("/v1/nodes/{}/messages", urlencode(session)), &json!({"text": text, "from": "david"}));
            if r.is_ok() && *ack {
                let _ = client.post(&format!("/v1/nodes/{}/ack", urlencode(session)), &json!({}));
            }
            r
        }
    };
    match (res, target) {
        (Ok(_), Target::Answer(id, _)) => flash(app, format!("Contestada #{id}: le llega al nodo que preguntó")),
        (Ok(_), Target::Message { name, .. }) => flash(app, format!("Enviado a {name}")),
        (Err(e), _) => flash(app, format!("Error: {e}")),
    }
}

fn ago(ms: i64) -> String {
    let s = ((now_millis() - ms) / 1000).max(0);
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86399 => format!("{}h{:02}", s / 3600, (s % 3600) / 60),
        _ => format!("{}d", s / 86400),
    }
}

fn state_color(s: NodeState) -> Color {
    match s {
        NodeState::WaitingAnswer => Color::Yellow,
        NodeState::Unclear => Color::Magenta,
        NodeState::Delivered => Color::Cyan,
        NodeState::WaitingMachine => Color::LightRed,
        NodeState::Working => Color::Green,
        NodeState::Done => Color::DarkGray,
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max { format!("{}…", flat.chars().take(max).collect::<String>()) } else { flat }
}

fn said(n: &Node) -> String {
    n.last_message.clone().or(n.daemon_needs.clone()).or(n.daemon_detail.clone()).unwrap_or_default()
}

fn block(title: &str, focused: bool) -> Block<'static> {
    let b = Block::new().borders(Borders::ALL).border_type(BorderType::Rounded).title(format!(" {title} "));
    if focused { b.border_style(Style::new().fg(Color::LightYellow)) } else { b.border_style(Style::new().fg(Color::DarkGray)) }
}

fn centered(area: Rect, width: u16, height: u16) -> Rect {
    let w = width.min(area.width.saturating_sub(2));
    let h = height.min(area.height.saturating_sub(2)).max(3);
    Rect { x: area.x + (area.width - w) / 2, y: area.y + (area.height - h) / 2, width: w, height: h }
}

fn draw(f: &mut Frame, app: &mut App) {
    let [head, body, foot] = Layout::vertical([Constraint::Length(1), Constraint::Min(8), Constraint::Length(2)]).areas(f.area());
    draw_head(f, app, head);
    if app.fleet_view {
        draw_fleet(f, app, body);
    } else {
        let [left, right] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(body);
        let nq = app.data.open_questions.len();
        let q_height = if nq == 0 { 3 } else { ((nq * 2) as u16 + 2).min(left.height / 2) };
        let [questions, news] = Layout::vertical([Constraint::Length(q_height), Constraint::Min(4)]).areas(left);
        draw_questions(f, app, questions);
        draw_news(f, app, news);
        draw_orchestrator(f, app, right);
    }
    draw_foot(f, app, foot);
    draw_overlay(f, app);
}

fn draw_head(f: &mut Frame, app: &App, area: Rect) {
    let m = &app.data.machine;
    let gb = |b: u64| b as f64 / 1_073_741_824.0;
    let conn = if app.connected { Span::raw("") } else { " SIN CONEXIÓN CON alondrad ".on_red().bold() };
    let mem = m.memory_free_percent.map(|p| format!("{p}% libre")).unwrap_or_else(|| format!("{:.1} GB libres", gb(m.memory_available)));
    let working = app.data.nodes.iter().filter(|n| n.state == NodeState::Working).count();
    let warn = if m.unreliable { " ⚠ medidas poco fiables" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            " Alondra ".bold().black().on_light_yellow(),
            conn,
            format!("  {working} trabajando · {} nodos · memoria {mem} · carga {:.0}{warn}", app.data.nodes.len(), m.load[0]).dark_gray(),
        ])),
        area,
    );
}

fn draw_questions(f: &mut Frame, app: &mut App, area: Rect) {
    let nq = app.data.open_questions.len();
    let b = block(&format!("Te esperan ({nq})"), app.focus == Focus::Questions);
    if nq == 0 {
        f.render_widget(Paragraph::new("  Nada pendiente.".dark_gray()).block(b), area);
        return;
    }
    let width = area.width.saturating_sub(10) as usize;
    let items: Vec<ListItem> = app
        .data
        .open_questions
        .iter()
        .map(|q| {
            let who = q.node_name.clone().unwrap_or_else(|| q.session_id.chars().take(8).collect());
            let kind = match q.origin {
                QuestionOrigin::Permission => "permiso · ",
                QuestionOrigin::Plan => "plan · ",
                _ => "",
            };
            let opts = q.options.iter().enumerate().map(|(i, o)| format!("{} {}", i + 1, o.label)).collect::<Vec<_>>().join("  ");
            ListItem::new(vec![
                Line::from(vec![format!("#{} ", q.id).bold(), format!("{kind}{}", one_line(&q.title, width)).into()]),
                Line::from(vec![format!("   {who} · {}", ago(q.asked_at)).dark_gray(), format!("   {opts}").yellow()]),
            ])
        })
        .collect();
    f.render_stateful_widget(List::new(items).block(b).highlight_style(Style::new().add_modifier(Modifier::REVERSED)), area, &mut app.questions);
}

fn draw_news(f: &mut Frame, app: &mut App, area: Rect) {
    let n = app.data.news.len();
    let b = block(&format!("Novedades ({n})"), app.focus == Focus::News);
    if n == 0 {
        f.render_widget(Paragraph::new("  Ningún nodo ha entregado nada nuevo.".dark_gray()).block(b), area);
        return;
    }
    let width = area.width.saturating_sub(6) as usize;
    let items: Vec<ListItem> = app
        .data
        .news
        .iter()
        .map(|node| {
            let at = node.last_message_at.unwrap_or(node.state_since);
            let sleep = if node.alive { "" } else { " 💤" };
            ListItem::new(vec![
                Line::from(vec![
                    Span::styled("● ", Style::new().fg(state_color(node.state))),
                    Span::raw(format!("{}{sleep}", node.display_name())).bold(),
                    format!("  {} · {}", node.state.label(), ago(at)).dark_gray(),
                ]),
                Line::from(format!("  {}", one_line(&said(node), width))),
            ])
        })
        .collect();
    f.render_stateful_widget(List::new(items).block(b).highlight_style(Style::new().add_modifier(Modifier::REVERSED)), area, &mut app.news);
}

fn draw_orchestrator(f: &mut Frame, app: &App, area: Rect) {
    let b = block("Orquestador  (o para escribirle)", false);
    let Some(o) = orchestrator(app) else {
        f.render_widget(
            Paragraph::new(vec![
                Line::from("No está conectado a Alondra.".dark_gray()),
                Line::default(),
                Line::from("Arráncalo con".dark_gray()),
                Line::from("--plugin-dir ~/Projects/alondra/plugin-orchestrator".dark_gray()),
            ])
            .wrap(Wrap { trim: false })
            .block(b),
            area,
        );
        return;
    };
    let mut lines: Vec<Line> = vec![Line::from(vec![
        Span::styled(o.state.label(), Style::new().fg(state_color(o.state)).bold()),
        format!(" · {}", ago(o.state_since)).dark_gray(),
    ])];
    if let Some(t) = &o.tool {
        lines.push(Line::from(format!("Ahora: {} {}", t.name, one_line(t.summary.as_deref().unwrap_or(""), 60)).dark_gray()));
    }
    lines.push(Line::default());
    lines.extend(said(&o).lines().map(|l| Line::from(l.to_string())));
    let mine: Vec<&Message> = app.data.orchestrator_messages.iter().filter(|m| m.from == MessageFrom::David).take(3).collect();
    if !mine.is_empty() {
        lines.push(Line::default());
        lines.push(Line::from("Le has escrito:".dark_gray()));
        for m in mine {
            let mark = if m.delivered_at.is_some() { "✓" } else if m.handed_at.is_some() { "→" } else { "…" };
            lines.push(Line::from(format!("{mark} {}", one_line(&m.text, 80)).dark_gray()));
        }
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }).block(b), area);
}

fn draw_fleet(f: &mut Frame, app: &mut App, area: Rect) {
    let rows: Vec<Row> = app
        .data
        .nodes
        .iter()
        .map(|n| {
            let star = if n.is_orchestrator { "★ " } else { "" };
            let sleep = if n.alive { "" } else { " 💤" };
            let src = match n.state_source {
                StateSource::Classifier => "*",
                StateSource::Daemon => "~",
                _ => "",
            };
            let doing = match &n.tool {
                Some(t) => format!("{} {}", t.name, one_line(t.summary.as_deref().unwrap_or(""), 60)),
                None => one_line(&said(n), 90),
            };
            Row::new(vec![
                Span::styled(format!("{}{src}", n.state.label()), Style::new().fg(state_color(n.state))),
                Span::raw(one_line(&format!("{star}{}{sleep}", n.display_name()), 36)),
                Span::raw(ago(n.state_since)).dark_gray(),
                Span::raw(doing),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Length(21), Constraint::Length(36), Constraint::Length(6), Constraint::Min(20)])
        .header(Row::new(vec!["estado", "nodo", "desde", "qué hace / qué dijo"]).dark_gray())
        .block(block(&format!("Flota ({})", app.data.nodes.len()), true))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, area, &mut app.fleet);
}

fn draw_foot(f: &mut Frame, app: &App, area: Rect) {
    let help = match app.focus {
        Focus::Questions => "1-4 elegir · r responder · Enter ver · a su terminal · o orquestador · Tab novedades · f flota · q salir",
        Focus::News => "r responder al nodo · x visto · Enter leer entero · a su terminal · o orquestador · Tab preguntas · f flota · q salir",
        Focus::Fleet => "r mensaje · Enter ver · a su terminal (Ctrl+Z vuelve) · o orquestador · f volver · q salir   ~ demonio · * jev · 💤 dormido",
    };
    let status = app
        .status
        .as_ref()
        .filter(|(_, at)| now_millis() - at < 5000)
        .map(|(s, _)| Line::from(s.clone().yellow()))
        .unwrap_or_default();
    f.render_widget(Paragraph::new(vec![status, Line::from(help.dark_gray())]), area);
}

fn draw_overlay(f: &mut Frame, app: &App) {
    match &app.mode {
        Mode::Input { target, buffer } => {
            let title = match target {
                Target::Answer(id, t) => format!("Responder #{id}: {}   (Enter envía · Alt+Enter salto · Esc cancela)", one_line(t, 50)),
                Target::Message { name, .. } => format!("Mensaje a {name}   (Enter envía · Esc cancela)"),
            };
            let area = centered(f.area(), 100, 8);
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(format!("{buffer}▏")).wrap(Wrap { trim: false }).block(block(&title, true)), area);
        }
        Mode::Detail { title, body, scroll } => {
            let area = centered(f.area(), 110, f.area().height.saturating_sub(4));
            f.render_widget(Clear, area);
            f.render_widget(
                Paragraph::new(body.clone()).wrap(Wrap { trim: false }).scroll((*scroll, 0)).block(block(&format!("{title}   (r responder · Esc cerrar)"), true)),
                area,
            );
        }
        Mode::Normal => {}
    }
}

fn open_detail(app: &mut App) {
    match app.focus {
        Focus::Questions => {
            let Some(q) = selected_question(app) else { return };
            let mut lines: Vec<Line> = vec![Line::from(q.title.clone().bold()), Line::default()];
            if let Some(d) = &q.detail {
                lines.extend(d.lines().map(|l| Line::from(l.to_string())));
                lines.push(Line::default());
            }
            for (i, o) in q.options.iter().enumerate() {
                let desc = o.description.clone().map(|d| format!(" — {d}")).unwrap_or_default();
                lines.push(Line::from(format!("  {}. {}{desc}", i + 1, o.label).yellow()));
            }
            for l in &q.links {
                lines.push(Line::from(format!("  ↗ {} {}", l.title.clone().unwrap_or_default(), l.url).cyan()));
            }
            for a in &q.attachments {
                lines.push(Line::from(format!("  📎 {}", a.stored_path.clone().unwrap_or_else(|| a.source_path.clone())).cyan()));
            }
            if let Some(c) = &q.node_context {
                lines.push(Line::default());
                lines.push(Line::from("Lo que hacía el nodo:".dark_gray()));
                lines.extend(c.lines().map(|l| Line::from(l.to_string().dark_gray())));
            }
            app.mode = Mode::Detail { title: format!("#{} · {}", q.id, q.node_name.clone().unwrap_or_default()), body: Text::from(lines), scroll: 0 };
        }
        Focus::News | Focus::Fleet => {
            let node = if app.focus == Focus::News { selected_news(app) } else { selected_fleet(app) };
            let Some(n) = node else { return };
            let mut lines: Vec<Line> = vec![
                Line::from(vec![
                    Span::styled(n.state.label(), Style::new().fg(state_color(n.state)).bold()),
                    format!("  desde hace {}{}", ago(n.state_since), if n.alive { "" } else { " · 💤 dormido" }).dark_gray(),
                ]),
                Line::from(n.cwd.clone().unwrap_or_default().dark_gray()),
                Line::default(),
            ];
            if let Some(t) = &n.tool {
                lines.push(Line::from(format!("Ahora: {} {} (desde hace {})", t.name, t.summary.clone().unwrap_or_default(), ago(t.since))));
                lines.push(Line::default());
            }
            if let Some(needs) = &n.daemon_needs {
                lines.push(Line::from(format!("Necesita: {needs}").yellow()));
                lines.push(Line::default());
            }
            lines.extend(said(&n).lines().map(|l| Line::from(l.to_string())));
            app.mode = Mode::Detail { title: n.display_name().to_string(), body: Text::from(lines), scroll: 0 };
        }
    }
}
