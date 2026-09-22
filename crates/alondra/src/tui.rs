use std::sync::mpsc;
use std::time::Duration;

use alondra_core::{Node, NodeState, Question, now_millis};
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
    Nodes,
}

enum Mode {
    Normal,
    Input { target: Target, buffer: String },
    Detail { title: String, body: Text<'static>, scroll: u16 },
}

#[derive(Clone)]
enum Target {
    Answer(u64, String),
    Message(String, String),
}

struct App {
    data: Overview,
    connected: bool,
    focus: Focus,
    questions: ListState,
    nodes: TableState,
    mode: Mode,
    status: Option<(String, i64)>,
}

enum Update {
    Data(Box<Overview>),
    Offline,
}

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Update>(4);
    std::thread::Builder::new().name("alondra-sync".into()).spawn(move || sync_loop(tx))?;
    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::BeginSynchronizedUpdate, crossterm::terminal::EndSynchronizedUpdate);
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
        let path = format!("/v1/changes?after={tick}&wait=50");
        if waiter.get::<serde_json::Value>(&path).is_err() {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn main_loop(terminal: &mut DefaultTerminal, rx: mpsc::Receiver<Update>) -> Result<()> {
    let mut app = App {
        data: Overview::default(),
        connected: false,
        focus: Focus::Questions,
        questions: ListState::default(),
        nodes: TableState::default(),
        mode: Mode::Normal,
        status: None,
    };
    let client = Client::new(Duration::from_secs(10));
    loop {
        while let Ok(u) = rx.try_recv() {
            match u {
                Update::Data(o) => {
                    app.data = *o;
                    app.connected = true;
                }
                Update::Offline => app.connected = false,
            }
        }
        clamp(&mut app);
        terminal.draw(|f| draw(f, &mut app))?;
        if event::poll(Duration::from_millis(150))?
            && let TermEvent::Key(k) = event::read()?
            && k.kind == KeyEventKind::Press
            && !handle_key(&mut app, &client, k)
        {
            return Ok(());
        }
    }
}

fn clamp(app: &mut App) {
    let nq = app.data.open_questions.len();
    let nn = app.data.nodes.len();
    match app.questions.selected() {
        _ if nq == 0 => app.questions.select(None),
        None => app.questions.select(Some(0)),
        Some(i) if i >= nq => app.questions.select(Some(nq - 1)),
        _ => {}
    }
    match app.nodes.selected() {
        _ if nn == 0 => app.nodes.select(None),
        None => app.nodes.select(Some(0)),
        Some(i) if i >= nn => app.nodes.select(Some(nn - 1)),
        _ => {}
    }
    if app.focus == Focus::Questions && nq == 0 {
        app.focus = Focus::Nodes;
    }
}

fn flash(app: &mut App, msg: impl Into<String>) {
    app.status = Some((msg.into(), now_millis()));
}

fn selected_question(app: &App) -> Option<Question> {
    app.questions.selected().and_then(|i| app.data.open_questions.get(i).cloned())
}

fn selected_node(app: &App) -> Option<Node> {
    app.nodes.selected().and_then(|i| app.data.nodes.get(i).cloned())
}

fn orchestrator(app: &App) -> Option<Node> {
    app.data.nodes.iter().find(|n| n.is_orchestrator && n.alive).cloned()
}

fn handle_key(app: &mut App, client: &Client, k: KeyEvent) -> bool {
    if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
        return false;
    }
    match &mut app.mode {
        Mode::Input { target, buffer } => {
            match k.code {
                KeyCode::Esc => app.mode = Mode::Normal,
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
                KeyCode::PageDown => *scroll = scroll.saturating_add(10),
                KeyCode::PageUp => *scroll = scroll.saturating_sub(10),
                _ => app.mode = Mode::Normal,
            }
            return true;
        }
        Mode::Normal => {}
    }
    match k.code {
        KeyCode::Char('q') | KeyCode::Esc => return false,
        KeyCode::Tab | KeyCode::BackTab => {
            app.focus = if app.focus == Focus::Questions { Focus::Nodes } else { Focus::Questions };
        }
        KeyCode::Down | KeyCode::Char('j') => move_sel(app, 1),
        KeyCode::Up | KeyCode::Char('k') => move_sel(app, -1),
        KeyCode::Enter => open_detail(app, client),
        KeyCode::Char('r') => {
            if let Some(q) = selected_question(app) {
                app.mode = Mode::Input { target: Target::Answer(q.id, q.title), buffer: String::new() };
            }
        }
        KeyCode::Char('m') => {
            if let Some(n) = selected_node(app) {
                app.mode = Mode::Input { target: Target::Message(n.session_id.clone(), n.display_name().to_string()), buffer: String::new() };
            }
        }
        KeyCode::Char('o') => match orchestrator(app) {
            Some(n) => app.mode = Mode::Input { target: Target::Message(n.session_id.clone(), "orquestador".into()), buffer: String::new() },
            None => flash(app, "No hay orquestador conectado a Alondra"),
        },
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
    let (state_len, sel) = match app.focus {
        Focus::Questions => (app.data.open_questions.len(), app.questions.selected()),
        Focus::Nodes => (app.data.nodes.len(), app.nodes.selected()),
    };
    if state_len == 0 {
        return;
    }
    let next = (sel.unwrap_or(0) as i32 + d).clamp(0, state_len as i32 - 1) as usize;
    match app.focus {
        Focus::Questions => app.questions.select(Some(next)),
        Focus::Nodes => app.nodes.select(Some(next)),
    }
}

fn submit(app: &mut App, client: &Client, target: Target, text: String, option: Option<String>) {
    let res = match &target {
        Target::Answer(id, _) => client.post(&format!("/v1/questions/{id}/answer"), &json!({"text": text, "option": option, "by": "David", "device": "tui"})),
        Target::Message(sid, _) => client.post(&format!("/v1/nodes/{}/messages", urlencode(sid)), &json!({"text": text, "from": "david"})),
    };
    match (res, target) {
        (Ok(_), Target::Answer(id, _)) => flash(app, format!("Contestada #{id}")),
        (Ok(_), Target::Message(_, name)) => flash(app, format!("Mensaje en la bandeja de {name}")),
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
        NodeState::Down => Color::Red,
    }
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() > max { format!("{}…", flat.chars().take(max).collect::<String>()) } else { flat }
}

fn node_summary(n: &Node) -> String {
    if let Some(t) = &n.tool {
        return format!("{} {} · {}", t.name, one_line(t.summary.as_deref().unwrap_or(""), 60), ago(t.since));
    }
    if let Some(m) = &n.last_message {
        return one_line(m, 90);
    }
    one_line(n.daemon_needs.as_deref().or(n.daemon_detail.as_deref()).unwrap_or(""), 90)
}

fn draw(f: &mut Frame, app: &mut App) {
    let nq = app.data.open_questions.len();
    let q_height = if nq == 0 { 3 } else { (nq as u16 + 2).min(10) };
    let [head, questions, nodes, foot] = Layout::vertical([Constraint::Length(1), Constraint::Length(q_height), Constraint::Min(5), Constraint::Length(2)]).areas(f.area());

    let m = &app.data.machine;
    let gb = |b: u64| b as f64 / 1_073_741_824.0;
    let conn = if app.connected { Span::raw("") } else { " SIN CONEXIÓN CON alondrad ".on_red().bold() };
    let mem = m.memory_free_percent.map(|p| format!("{p}% libre")).unwrap_or_else(|| format!("{:.1} GB libres", gb(m.memory_available)));
    let load = format!("carga {:.0}/{:.0}/{:.0}", m.load[0], m.load[1], m.load[2]);
    let warn = if m.unreliable { " ⚠ medidas poco fiables" } else { "" };
    f.render_widget(
        Paragraph::new(Line::from(vec![
            " Alondra ".bold().black().on_light_yellow(),
            conn,
            format!("  memoria {mem} · {load} · disco {:.0} GB libres{warn}", gb(m.disk_available)).into(),
        ])),
        head,
    );

    let qblock = block(&format!("Te esperan ({nq})"), app.focus == Focus::Questions);
    if nq == 0 {
        f.render_widget(Paragraph::new("  Nada pendiente.".dark_gray()).block(qblock), questions);
    } else {
        let items: Vec<ListItem> = app
            .data
            .open_questions
            .iter()
            .map(|q| {
                let who = q.node_name.clone().unwrap_or_else(|| q.session_id.chars().take(8).collect());
                let kind = match q.origin {
                    alondra_core::QuestionOrigin::Permission => "permiso ",
                    alondra_core::QuestionOrigin::Plan => "plan ",
                    _ => "",
                };
                ListItem::new(Line::from(vec![
                    format!("#{:<4}", q.id).bold(),
                    format!("{kind}{}", one_line(&q.title, 80)).into(),
                    format!("  · {who} · {}", ago(q.asked_at)).dark_gray(),
                ]))
            })
            .collect();
        f.render_stateful_widget(List::new(items).block(qblock).highlight_style(Style::new().add_modifier(Modifier::REVERSED)), questions, &mut app.questions);
    }

    let rows: Vec<Row> = app
        .data
        .nodes
        .iter()
        .map(|n| {
            let name = if n.is_orchestrator { format!("★ {}", n.display_name()) } else { n.display_name().to_string() };
            let src = match n.state_source {
                alondra_core::StateSource::Classifier => "*",
                alondra_core::StateSource::Daemon => "~",
                _ => "",
            };
            Row::new(vec![
                Span::styled(format!("{}{src}", n.state.label()), Style::new().fg(state_color(n.state))),
                Span::raw(one_line(&name, 34)),
                Span::raw(ago(n.state_since)).dark_gray(),
                Span::raw(node_summary(n)),
            ])
        })
        .collect();
    let table = Table::new(rows, [Constraint::Length(21), Constraint::Length(34), Constraint::Length(6), Constraint::Min(20)])
        .header(Row::new(vec!["estado", "nodo", "desde", "qué hace / último mensaje"]).dark_gray())
        .block(block(&format!("Flota ({})", app.data.nodes.len()), app.focus == Focus::Nodes))
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(table, nodes, &mut app.nodes);

    let help = match app.focus {
        Focus::Questions => "Enter ver · 1-4 elegir opción · r responder · o orquestador · Tab flota · q salir",
        Focus::Nodes => "Enter ver · m mensaje al nodo · o orquestador · Tab preguntas · q salir   (~ demonio, * jev)",
    };
    let status = app
        .status
        .as_ref()
        .filter(|(_, at)| now_millis() - at < 5000)
        .map(|(s, _)| Line::from(s.clone().yellow()))
        .unwrap_or_default();
    f.render_widget(Paragraph::new(vec![status, Line::from(help.dark_gray())]), foot);

    match &app.mode {
        Mode::Input { target, buffer } => {
            let title = match target {
                Target::Answer(id, t) => format!("Responder #{id}: {}", one_line(t, 60)),
                Target::Message(_, name) => format!("Mensaje a {name}"),
            };
            let area = centered(f.area(), 80, 5);
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(format!("{buffer}▏")).wrap(Wrap { trim: false }).block(block(&title, true)), area);
        }
        Mode::Detail { title, body, scroll } => {
            let area = centered(f.area(), 90, f.area().height.saturating_sub(4));
            f.render_widget(Clear, area);
            f.render_widget(Paragraph::new(body.clone()).wrap(Wrap { trim: false }).scroll((*scroll, 0)).block(block(title, true)), area);
        }
        Mode::Normal => {}
    }
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

fn open_detail(app: &mut App, client: &Client) {
    match app.focus {
        Focus::Questions => {
            if let Some(q) = selected_question(app) {
                let mut lines: Vec<Line> = vec![Line::from(q.title.clone().bold()), Line::default()];
                if let Some(d) = &q.detail {
                    lines.extend(d.lines().map(|l| Line::from(l.to_string())));
                    lines.push(Line::default());
                }
                for (i, o) in q.options.iter().enumerate() {
                    let desc = o.description.clone().map(|d| format!(" — {d}")).unwrap_or_default();
                    lines.push(Line::from(format!("  {}. {}{desc}", i + 1, o.label)));
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
                let who = q.node_name.clone().unwrap_or_default();
                app.mode = Mode::Detail { title: format!("#{} · {who}", q.id), body: Text::from(lines), scroll: 0 };
            }
        }
        Focus::Nodes => {
            if let Some(n) = selected_node(app) {
                let mut lines: Vec<Line> = vec![
                    Line::from(vec![Span::styled(n.state.label(), Style::new().fg(state_color(n.state)).bold()), format!("  desde hace {} · fuente {:?}", ago(n.state_since), n.state_source).dark_gray()]),
                    Line::from(format!("{} · {}", n.session_id, n.cwd.clone().unwrap_or_default()).dark_gray()),
                    Line::default(),
                ];
                if let Some(t) = &n.tool {
                    lines.push(Line::from(format!("Ahora: {} {} (desde hace {})", t.name, t.summary.clone().unwrap_or_default(), ago(t.since))));
                }
                if let Some(d) = &n.daemon_detail {
                    lines.push(Line::from(format!("Resumen del demonio ({}): {d}", n.daemon_updated_at.map(ago).unwrap_or_default()).dark_gray()));
                }
                if let Some(needs) = &n.daemon_needs {
                    lines.push(Line::from(format!("Necesita: {needs}").yellow()));
                }
                if let Some(m) = &n.last_message {
                    lines.push(Line::default());
                    lines.push(Line::from(format!("Último mensaje ({}):", n.last_message_at.map(ago).unwrap_or_default()).dark_gray()));
                    lines.extend(m.lines().map(|l| Line::from(l.to_string())));
                }
                if let Ok(detail) = client.get::<serde_json::Value>(&format!("/v1/nodes/{}", urlencode(&n.session_id))) {
                    let reports = detail.get("reports").and_then(|r| r.as_array()).cloned().unwrap_or_default();
                    if !reports.is_empty() {
                        lines.push(Line::default());
                        lines.push(Line::from("Informes:".dark_gray()));
                        for r in reports.iter().rev().take(10) {
                            lines.push(Line::from(format!("  • {}", r.get("title").and_then(|t| t.as_str()).unwrap_or(""))));
                        }
                    }
                }
                app.mode = Mode::Detail { title: n.display_name().to_string(), body: Text::from(lines), scroll: 0 };
            }
        }
    }
}
