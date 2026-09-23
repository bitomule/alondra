mod draw;
mod input;
mod md;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::stdout;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use alondra_core::{Node, NodeState, Question, QuestionOrigin, Report, now_millis};
use anyhow::Result;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use crate::client::{Client, FleetNode, Overview};
use crate::mcp::urlencode;
use input::TextInput;
use md::Block;

const SETTLE_MS: i64 = 700;
const FLASH_MS: i64 = 4500;
const SEARCH_DEBOUNCE_MS: i64 = 1200;
const HIDE_GRACE_MS: i64 = 15_000;
const CLOCK_MS: i64 = 20_000;
const IDLE_POLL: Duration = Duration::from_millis(250);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Key {
    Q(u64),
    R(u64),
}

pub enum Item<'a> {
    Q(&'a Question),
    R(&'a Report),
}

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Inbox,
    Fleet,
}

pub struct Flash {
    pub text: String,
    pub error: bool,
    pub at: i64,
}

pub struct Comment {
    pub block: usize,
    pub quote: String,
    pub text: String,
}

#[derive(Default)]
pub struct Review {
    pub comments: Vec<Comment>,
    pub general: String,
}

impl Review {
    pub fn is_empty(&self) -> bool {
        self.comments.is_empty() && self.general.trim().is_empty()
    }

    pub fn count(&self) -> usize {
        self.comments.len() + usize::from(!self.general.trim().is_empty())
    }

    fn feedback(&self) -> String {
        let pairs: Vec<(String, String)> = self.comments.iter().map(|c| (c.quote.clone(), c.text.clone())).collect();
        md::feedback(&pairs, &self.general)
    }
}

enum Part {
    Text(String),
    File { name: String, path: String },
}

pub struct Approval {
    pub approve: Option<String>,
    pub changes: Option<String>,
}

pub struct Reader {
    pub key: Key,
    pub session: String,
    pub node: String,
    pub title: String,
    pub kind: &'static str,
    pub approval: Option<Approval>,
    parts: Vec<Part>,
    pub blocks: Vec<Block>,
    pub cursor: usize,
    pub scroll: usize,
    pub ranges: Vec<(usize, usize)>,
    pub view_h: usize,
}

impl Reader {
    fn step(&mut self, d: isize) {
        let mut i = self.cursor as isize;
        loop {
            i += d;
            if i < 0 || i >= self.blocks.len() as isize {
                return;
            }
            if self.blocks[i as usize].commentable() {
                self.cursor = i as usize;
                return;
            }
        }
    }

    fn page(&mut self, d: isize) {
        let Some(&(start, _)) = self.ranges.get(self.cursor) else { return };
        let jump = self.view_h.saturating_sub(2).max(1);
        let target = if d > 0 { start + jump } else { start.saturating_sub(jump) };
        let found = if d > 0 {
            self.ranges.iter().position(|&(s, _)| s >= target).unwrap_or(self.blocks.len().saturating_sub(1))
        } else {
            self.ranges.iter().rposition(|&(s, _)| s <= target).unwrap_or(0)
        };
        self.cursor = found;
        if !self.blocks.get(found).is_some_and(Block::commentable) {
            self.step(if d > 0 { -1 } else { 1 });
        }
        if let Some(&(s, _)) = self.ranges.get(self.cursor) {
            self.scroll = s.saturating_sub(1);
        }
    }

    fn settle_cursor(&mut self) {
        self.cursor = self.cursor.min(self.blocks.len().saturating_sub(1));
        if !self.blocks.get(self.cursor).is_some_and(Block::commentable) {
            self.step(1);
        }
    }
}

pub enum Target {
    Answer { id: u64, title: String },
    MoreContext { id: u64, title: String },
    ReplyReport { id: u64, session: String, name: String },
    Message { session: String, name: String, ack: bool },
    NewRequest,
    Comment { block: usize, quote: String },
    General { send: bool },
}

pub struct Composer {
    pub target: Target,
    pub input: TextInput,
}

impl Target {
    fn draft_key(&self) -> Option<String> {
        match self {
            Target::Answer { id, .. } => Some(format!("answer:{id}")),
            Target::MoreContext { id, .. } => Some(format!("context:{id}")),
            Target::ReplyReport { id, .. } => Some(format!("reply:{id}")),
            Target::Message { session, .. } => Some(format!("message:{session}")),
            Target::NewRequest => Some("new".into()),
            Target::Comment { .. } | Target::General { .. } => None,
        }
    }
}

pub enum Remote {
    Idle,
    Pending,
    Done { query: String, via: String, sessions: Vec<String> },
    Failed(String),
}

pub struct Search {
    pub input: TextInput,
    pub editing: bool,
    pub typed_at: i64,
    pub remote: Remote,
}

impl Search {
    pub fn query(&self) -> String {
        self.input.text().trim().to_lowercase()
    }
}

pub enum FleetRow {
    Group { name: String, alive: usize, total: usize },
    Node(usize),
}

enum Msg {
    Data(Box<Overview>),
    Offline,
    Posted { key: Option<Key>, result: Result<String, String> },
    File { path: String, result: Result<String, String> },
    Found { query: String, result: Result<(String, Vec<String>), String> },
}

pub enum Flow {
    Go,
    Quit,
    Attach(String),
}

pub struct App {
    pub data: Overview,
    pub connected: bool,
    pub loaded: bool,
    pub offline: bool,
    pub screen: Screen,
    pub sel: Option<Key>,
    sel_idx: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    settle_until: i64,
    hidden: HashMap<Key, i64>,
    pub fleet_sel: Option<String>,
    fleet_idx: usize,
    pub fleet_scroll: usize,
    pub fleet_detail_scroll: usize,
    pub show_sleeping: bool,
    pub search: Option<Search>,
    pub composer: Option<Composer>,
    pub reader: Option<Reader>,
    pub reviews: HashMap<Key, Review>,
    drafts: HashMap<String, String>,
    pub files: HashMap<String, Result<String, String>>,
    loading: HashSet<String>,
    pub flash: Option<Flash>,
    pub help: bool,
    last_clock: i64,
    tx: SyncSender<Msg>,
}

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Msg>(64);
    let sync_tx = tx.clone();
    std::thread::Builder::new().name("alondra-sync".into()).spawn(move || sync_loop(sync_tx))?;
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let res = main_loop(&mut terminal, App::new(tx), rx);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::attach::reset_terminal_modes();
    res
}

fn sync_loop(tx: SyncSender<Msg>) {
    let quick = Client::new(Duration::from_secs(5));
    let waiter = Client::new(Duration::from_secs(70));
    let mut was_online = true;
    loop {
        let tick = match quick.overview() {
            Ok(o) => {
                was_online = true;
                let tick = o.tick;
                if tx.send(Msg::Data(Box::new(o))).is_err() {
                    return;
                }
                tick
            }
            Err(_) => {
                if was_online && tx.send(Msg::Offline).is_err() {
                    return;
                }
                was_online = false;
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if waiter.get::<Value>(&format!("/v1/changes?after={tick}&wait=50")).is_err() {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
}

fn draw(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    execute!(stdout(), BeginSynchronizedUpdate)?;
    let res = terminal.draw(|f| draw::draw(f, app));
    execute!(stdout(), EndSynchronizedUpdate)?;
    res?;
    Ok(())
}

fn main_loop(terminal: &mut DefaultTerminal, mut app: App, rx: Receiver<Msg>) -> Result<()> {
    let mut dirty = true;
    loop {
        if dirty {
            app.reconcile();
            draw(terminal, &mut app)?;
            dirty = false;
        }
        if event::poll(IDLE_POLL)? {
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => match app.on_key(k) {
                        Flow::Quit => return Ok(()),
                        Flow::Attach(short) => {
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            ratatui::restore();
                            let res = crate::attach::attach(&short);
                            *terminal = ratatui::init();
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            terminal.clear()?;
                            if let Err(e) = res {
                                app.fail(format!("No pude abrir su terminal: {e}"));
                            }
                            dirty = true;
                            break;
                        }
                        Flow::Go => {}
                    },
                    Event::Paste(s) => app.on_paste(&s),
                    _ => {}
                }
                dirty = true;
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        while let Ok(m) = rx.try_recv() {
            app.on_msg(m);
            dirty = true;
        }
        dirty |= app.on_timers();
    }
}

pub fn who(name: &Option<String>, session: &str) -> String {
    name.clone().unwrap_or_else(|| session.chars().take(8).collect())
}

pub fn said(n: &Node) -> String {
    n.last_message.clone().or(n.daemon_needs.clone()).or(n.daemon_detail.clone()).unwrap_or_default()
}

fn approval_of(q: &Question) -> Option<Approval> {
    let find = |pred: &dyn Fn(&str) -> bool| q.options.iter().find(|o| pred(&o.label.to_lowercase())).map(|o| o.label.clone());
    let approve = find(&|l| l.starts_with("aprob") || l.starts_with("acept"));
    let changes = find(&|l| l.contains("cambi") || l.contains("pedir") || l.contains("rechaz"));
    (q.origin == QuestionOrigin::Plan || (approve.is_some() && changes.is_some())).then(|| Approval {
        approve: approve.or_else(|| q.options.first().map(|o| o.label.clone())),
        changes: changes.or_else(|| q.options.get(1).map(|o| o.label.clone())),
    })
}

fn answer_body(text: &str, option: Option<&str>) -> Value {
    json!({"text": text, "option": option, "by": "David", "device": "tui"})
}

impl App {
    fn new(tx: SyncSender<Msg>) -> Self {
        App {
            data: Overview::default(),
            connected: false,
            loaded: false,
            offline: false,
            screen: Screen::Inbox,
            sel: None,
            sel_idx: 0,
            list_scroll: 0,
            detail_scroll: 0,
            settle_until: 0,
            hidden: HashMap::new(),
            fleet_sel: None,
            fleet_idx: 0,
            fleet_scroll: 0,
            fleet_detail_scroll: 0,
            show_sleeping: false,
            search: None,
            composer: None,
            reader: None,
            reviews: HashMap::new(),
            drafts: HashMap::new(),
            files: HashMap::new(),
            loading: HashSet::new(),
            flash: None,
            help: false,
            last_clock: now_millis(),
            tx,
        }
    }

    pub fn inbox_keys(&self) -> Vec<Key> {
        let q = self.data.open_questions.iter().map(|q| Key::Q(q.id));
        let r = self.data.news.iter().map(|r| Key::R(r.id));
        q.chain(r).filter(|k| !self.hidden.contains_key(k)).collect()
    }

    pub fn item(&self, key: Key) -> Option<Item<'_>> {
        match key {
            Key::Q(id) => self.data.open_questions.iter().find(|q| q.id == id).map(Item::Q),
            Key::R(id) => self.data.news.iter().find(|r| r.id == id).map(Item::R),
        }
    }

    fn selected_question(&self) -> Option<Question> {
        match self.sel.and_then(|k| self.item(k)) {
            Some(Item::Q(q)) => Some(q.clone()),
            _ => None,
        }
    }

    fn selected_report(&self) -> Option<Report> {
        match self.sel.and_then(|k| self.item(k)) {
            Some(Item::R(r)) => Some(r.clone()),
            _ => None,
        }
    }

    pub fn node(&self, session: &str) -> Option<&FleetNode> {
        self.data.nodes.iter().find(|n| n.session_id == session)
    }

    pub fn orchestrator(&self) -> Option<&FleetNode> {
        self.data.nodes.iter().find(|n| n.is_orchestrator)
    }

    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash { text: text.into(), error: false, at: now_millis() });
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash { text: text.into(), error: true, at: now_millis() });
    }

    fn hide(&mut self, key: Key) {
        self.hidden.insert(key, now_millis());
        self.reconcile();
    }

    fn spawn(&self, job: impl FnOnce(&SyncSender<Msg>) + Send + 'static) {
        let tx = self.tx.clone();
        std::thread::spawn(move || job(&tx));
    }

    fn post(&self, key: Option<Key>, calls: Vec<(String, Value)>, ok: String) {
        self.spawn(move |tx| {
            let client = Client::new(Duration::from_secs(15));
            let mut result = Ok(ok);
            for (i, (path, body)) in calls.into_iter().enumerate() {
                if let Err(e) = client.post(&path, &body)
                    && i == 0
                {
                    result = Err(e.to_string());
                    break;
                }
            }
            let _ = tx.send(Msg::Posted { key, result });
        });
    }

    fn fetch(&mut self, path: &str) {
        if self.files.contains_key(path) || !self.loading.insert(path.to_string()) {
            return;
        }
        let path = path.to_string();
        self.spawn(move |tx| {
            let result = Client::new(Duration::from_secs(10))
                .get_text(&format!("/v1/file?path={}", urlencode(&path)))
                .map(|(_, body)| body)
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::File { path, result });
        });
    }

    fn reconcile(&mut self) {
        let now = now_millis();
        let present: HashSet<Key> =
            self.data.open_questions.iter().map(|q| Key::Q(q.id)).chain(self.data.news.iter().map(|r| Key::R(r.id))).collect();
        self.hidden.retain(|k, at| present.contains(k) && now - *at < HIDE_GRACE_MS);
        let keys = self.inbox_keys();
        let prev = self.sel;
        match self.sel.and_then(|s| keys.iter().position(|k| *k == s)) {
            Some(i) => self.sel_idx = i,
            None => {
                self.sel_idx = self.sel_idx.min(keys.len().saturating_sub(1));
                self.sel = keys.get(self.sel_idx).copied();
            }
        }
        if self.sel != prev {
            self.detail_scroll = 0;
            if prev.is_some() {
                self.settle_until = now + SETTLE_MS;
            }
        }
        let paths: Vec<String> = match self.sel.and_then(|k| self.item(k)) {
            Some(Item::Q(q)) => attachment_paths(&q.attachments),
            Some(Item::R(r)) => attachment_paths(&r.attachments),
            None => vec![],
        };
        for p in paths.iter().take(2) {
            self.fetch(p);
        }
        let sessions = self.fleet_sessions();
        match self.fleet_sel.as_ref().and_then(|s| sessions.iter().position(|x| x == s)) {
            Some(i) => self.fleet_idx = i,
            None => {
                self.fleet_idx = self.fleet_idx.min(sessions.len().saturating_sub(1));
                let next = sessions.get(self.fleet_idx).cloned();
                if next != self.fleet_sel {
                    self.fleet_detail_scroll = 0;
                }
                self.fleet_sel = next;
            }
        }
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Data(o) => {
                self.data = *o;
                self.connected = true;
                self.loaded = true;
                self.offline = false;
            }
            Msg::Offline => {
                self.connected = false;
                self.offline = true;
            }
            Msg::Posted { key, result: Ok(text) } => {
                if let Some(k) = key {
                    self.reviews.remove(&k);
                }
                self.flash(text);
            }
            Msg::Posted { key, result: Err(e) } => {
                if let Some(k) = key {
                    self.hidden.remove(&k);
                }
                self.fail(format!("No se envió: {e}"));
            }
            Msg::File { path, result } => {
                self.loading.remove(&path);
                self.files.insert(path.clone(), result);
                if self.reader.as_ref().is_some_and(|r| r.parts.iter().any(|p| matches!(p, Part::File { path: fp, .. } if *fp == path))) {
                    self.rebuild_reader();
                }
            }
            Msg::Found { query, result } => {
                if let Some(s) = self.search.as_mut().filter(|s| s.query() == query) {
                    s.remote = match result {
                        Ok((via, sessions)) => Remote::Done { query, via, sessions },
                        Err(e) => Remote::Failed(e),
                    };
                }
            }
        }
    }

    fn on_timers(&mut self) -> bool {
        let now = now_millis();
        let mut dirty = false;
        if self.flash.as_ref().is_some_and(|f| now - f.at > FLASH_MS) {
            self.flash = None;
            dirty = true;
        }
        if now - self.last_clock >= CLOCK_MS {
            self.last_clock = now;
            dirty = true;
        }
        if self.hidden.values().any(|at| now - at >= HIDE_GRACE_MS) {
            dirty = true;
        }
        let wants_remote = self.search.as_ref().and_then(|s| {
            let q = s.query();
            let idle = matches!(&s.remote, Remote::Idle);
            (idle && !q.is_empty() && now - s.typed_at >= SEARCH_DEBOUNCE_MS).then_some(q)
        });
        if let Some(q) = wants_remote
            && self.local_matches(&q).is_empty()
        {
            if let Some(s) = self.search.as_mut() {
                s.remote = Remote::Pending;
            }
            self.spawn(move |tx| {
                let result = Client::new(Duration::from_secs(120))
                    .get::<Value>(&format!("/v1/search?q={}", urlencode(&q)))
                    .map(|v| {
                        let via = v["via"].as_str().unwrap_or("").to_string();
                        let sessions = v["sessions"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default();
                        (via, sessions)
                    })
                    .map_err(|e| e.to_string());
                let _ = tx.send(Msg::Found { query: q, result });
            });
            dirty = true;
        }
        dirty
    }

    fn on_paste(&mut self, s: &str) {
        if let Some(c) = self.composer.as_mut() {
            c.input.insert_str(s);
        } else if let Some(search) = self.search.as_mut().filter(|s| s.editing) {
            search.input.insert_str(&md::one_line(s));
            search.typed_at = now_millis();
            search.remote = Remote::Idle;
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Flow {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Flow::Quit;
        }
        if self.help {
            self.help = false;
            return Flow::Go;
        }
        if self.composer.is_some() {
            self.composer_key(k);
            return Flow::Go;
        }
        if self.reader.is_some() {
            self.reader_key(k);
            return Flow::Go;
        }
        if self.screen == Screen::Fleet && self.search.as_ref().is_some_and(|s| s.editing) {
            self.search_key(k);
            return Flow::Go;
        }
        match k.code {
            KeyCode::Char('q') => return Flow::Quit,
            KeyCode::Char('?') => self.help = true,
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('f') => {
                self.screen = if self.screen == Screen::Inbox { Screen::Fleet } else { Screen::Inbox };
            }
            KeyCode::Char('n') => self.compose(Target::NewRequest, ""),
            KeyCode::Char('o') => match self.orchestrator() {
                Some(n) => {
                    let session = n.session_id.clone();
                    self.compose(Target::Message { session, name: "el orquestador".into(), ack: false }, "");
                }
                None => self.fail("El orquestador todavía no está conectado a Alondra"),
            },
            _ => {
                return match self.screen {
                    Screen::Inbox => self.inbox_key(k),
                    Screen::Fleet => self.fleet_key(k),
                };
            }
        }
        Flow::Go
    }

    fn compose(&mut self, target: Target, prefill: &str) {
        let draft = target.draft_key().and_then(|k| self.drafts.remove(&k));
        let text = draft.as_deref().unwrap_or(prefill);
        self.composer = Some(Composer { target, input: TextInput::with(text) });
    }

    fn cancel_composer(&mut self) {
        let Some(c) = self.composer.take() else { return };
        let text = c.input.text();
        if let Some(k) = c.target.draft_key()
            && !text.trim().is_empty()
        {
            self.drafts.insert(k, text);
            self.flash("Borrador guardado: vuelve a abrirlo y sigue donde lo dejaste");
        }
    }

    fn move_inbox(&mut self, to: isize) {
        let keys = self.inbox_keys();
        if keys.is_empty() {
            return;
        }
        let idx = to.clamp(0, keys.len() as isize - 1) as usize;
        if self.sel != Some(keys[idx]) {
            self.detail_scroll = 0;
        }
        self.sel_idx = idx;
        self.sel = Some(keys[idx]);
        self.settle_until = 0;
    }

    fn attach_to(&mut self, session: &str) -> Flow {
        match self.node(session) {
            Some(n) => Flow::Attach(n.short_id.clone()),
            None => {
                self.fail("Ese nodo ya no está en la flota");
                Flow::Go
            }
        }
    }

    fn inbox_key(&mut self, k: KeyEvent) -> Flow {
        let i = self.sel_idx as isize;
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_inbox(i + 1),
            KeyCode::Up | KeyCode::Char('k') => self.move_inbox(i - 1),
            KeyCode::Home | KeyCode::Char('g') => self.move_inbox(0),
            KeyCode::End | KeyCode::Char('G') => self.move_inbox(isize::MAX),
            KeyCode::PageDown | KeyCode::Char('J') | KeyCode::Char(' ') => self.detail_scroll += 5,
            KeyCode::PageUp | KeyCode::Char('K') => self.detail_scroll = self.detail_scroll.saturating_sub(5),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => self.open_reader(),
            KeyCode::Char('a') => {
                let session = match self.sel.and_then(|k| self.item(k)) {
                    Some(Item::Q(q)) => q.session_id.clone(),
                    Some(Item::R(r)) => r.session_id.clone(),
                    None => return Flow::Go,
                };
                return self.attach_to(&session);
            }
            KeyCode::Char('r') => {
                if let Some(q) = self.selected_question() {
                    self.compose(Target::Answer { id: q.id, title: q.title }, "");
                } else if let Some(r) = self.selected_report() {
                    let name = who(&r.node_name, &r.session_id);
                    self.compose(Target::ReplyReport { id: r.id, session: r.session_id, name }, "");
                }
            }
            KeyCode::Char('c') => {
                if let Some(q) = self.selected_question() {
                    self.compose(Target::MoreContext { id: q.id, title: q.title }, "");
                }
            }
            KeyCode::Char('x') => {
                if let Some(r) = self.selected_report() {
                    self.mark_read(&r);
                }
            }
            KeyCode::Char(c @ '1'..='9') => {
                if let Some(q) = self.selected_question() {
                    self.choose(&q, c as usize - '1' as usize);
                }
            }
            _ => {}
        }
        Flow::Go
    }

    fn choose(&mut self, q: &Question, i: usize) {
        if now_millis() < self.settle_until {
            self.fail("La selección acaba de cambiar: mira cuál es y vuelve a pulsar");
            return;
        }
        let Some(opt) = q.options.get(i) else {
            self.fail(format!("Esta pregunta no tiene opción {}", i + 1));
            return;
        };
        let who = who(&q.node_name, &q.session_id);
        self.post(
            Some(Key::Q(q.id)),
            vec![(format!("/v1/questions/{}/answer", q.id), answer_body(&opt.label, Some(&opt.label)))],
            format!("«{}» → {who}", opt.label),
        );
        self.reader = None;
        self.hide(Key::Q(q.id));
    }

    fn mark_read(&mut self, r: &Report) {
        self.post(Some(Key::R(r.id)), vec![(format!("/v1/reports/{}/read", r.id), json!({}))], format!("Leído: {}", md::truncate(&r.title, 60)));
        self.reader = None;
        self.hide(Key::R(r.id));
    }

    fn composer_key(&mut self, k: KeyEvent) {
        let Some(c) = self.composer.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.cancel_composer(),
            KeyCode::Enter if k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
                c.input.handle(k);
            }
            KeyCode::Enter => self.submit(),
            _ => {
                c.input.handle(k);
            }
        }
    }

    fn submit(&mut self) {
        let Some(c) = self.composer.take() else { return };
        let text = c.input.text().trim().to_string();
        let needs_text = matches!(c.target, Target::Answer { .. } | Target::ReplyReport { .. } | Target::Message { .. } | Target::NewRequest);
        if needs_text && text.is_empty() {
            self.composer = Some(c);
            return;
        }
        match c.target {
            Target::Answer { id, .. } => {
                self.post(Some(Key::Q(id)), vec![(format!("/v1/questions/{id}/answer"), answer_body(&text, None))], format!("#{id} contestada"));
                self.hide(Key::Q(id));
            }
            Target::MoreContext { id, .. } => {
                self.post(
                    Some(Key::Q(id)),
                    vec![(format!("/v1/questions/{id}/more-context"), json!({"note": text}))],
                    format!("#{id} devuelta: te la volverá a preguntar con más contexto"),
                );
                self.hide(Key::Q(id));
            }
            Target::ReplyReport { id, session, name } => {
                self.post(
                    Some(Key::R(id)),
                    vec![
                        (format!("/v1/nodes/{}/messages", urlencode(&session)), json!({"text": text, "from": "david"})),
                        (format!("/v1/reports/{id}/read"), json!({})),
                    ],
                    format!("Enviado a {name} y marcado leído"),
                );
                self.hide(Key::R(id));
            }
            Target::Message { session, name, ack } => {
                let mut calls = vec![(format!("/v1/nodes/{}/messages", urlencode(&session)), json!({"text": text, "from": "david"}))];
                if ack {
                    calls.push((format!("/v1/nodes/{}/ack", urlencode(&session)), json!({})));
                }
                self.post(None, calls, format!("Enviado a {name}"));
            }
            Target::NewRequest => {
                self.post(None, vec![("/v1/requests".into(), json!({"text": text}))], "Enviado al orquestador: preparará la misión y lanzará el nodo".into())
            }
            Target::Comment { block, quote } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key) else { return };
                let review = self.reviews.entry(key).or_default();
                review.comments.retain(|c| c.block != block);
                if !text.is_empty() {
                    review.comments.push(Comment { block, quote, text });
                    review.comments.sort_by_key(|c| c.block);
                }
            }
            Target::General { send } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key) else { return };
                self.reviews.entry(key).or_default().general = text;
                if send {
                    self.send_feedback();
                }
            }
        }
    }

    fn open_reader(&mut self) {
        let Some(key) = self.sel else { return };
        let (reader, paths) = match self.item(key) {
            Some(Item::Q(q)) => {
                let mut parts = vec![Part::Text(format!("# {}", q.title))];
                parts.extend(q.detail.clone().map(Part::Text));
                parts.extend(q.attachments.iter().filter_map(|a| a.stored_path.clone().map(|path| Part::File { name: a.name.clone(), path })));
                parts.extend(links_part(&q.links));
                let kind = match q.origin {
                    QuestionOrigin::Plan => "plan",
                    QuestionOrigin::Permission => "permiso",
                    _ => "pregunta",
                };
                (new_reader(key, &q.session_id, &q.node_name, &q.title, kind, approval_of(q), parts), attachment_paths(&q.attachments))
            }
            Some(Item::R(r)) => {
                let mut parts = vec![Part::Text(format!("# {}", r.title))];
                parts.extend(r.body.clone().map(Part::Text));
                parts.extend(r.attachments.iter().filter_map(|a| a.stored_path.clone().map(|path| Part::File { name: a.name.clone(), path })));
                parts.extend(links_part(&r.links));
                (new_reader(key, &r.session_id, &r.node_name, &r.title, r.kind.label(), None, parts), attachment_paths(&r.attachments))
            }
            None => return,
        };
        self.reader = Some(reader);
        for p in paths {
            self.fetch(&p);
        }
        self.rebuild_reader();
    }

    fn rebuild_reader(&mut self) {
        let Some(r) = self.reader.as_mut() else { return };
        let mut blocks = vec![];
        for part in &r.parts {
            match part {
                Part::Text(t) => blocks.extend(md::parse(t)),
                Part::File { name, path } => {
                    blocks.push(Block::label(&format!("📎 {name}")));
                    match self.files.get(path) {
                        Some(Ok(text)) => blocks.extend(md::parse(text)),
                        Some(Err(e)) => blocks.push(Block::para(&format!("No he podido cargar el fichero: {e}"))),
                        None => blocks.push(Block::para("Cargando…")),
                    }
                }
            }
        }
        r.blocks = blocks;
        r.settle_cursor();
    }

    fn reader_key(&mut self, k: KeyEvent) {
        let Some(r) = self.reader.as_mut() else { return };
        let key = r.key;
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Left | KeyCode::Char('h') => self.reader = None,
            KeyCode::Down | KeyCode::Char('j') => r.step(1),
            KeyCode::Up | KeyCode::Char('k') => r.step(-1),
            KeyCode::Home | KeyCode::Char('g') => {
                r.cursor = 0;
                r.settle_cursor();
            }
            KeyCode::End | KeyCode::Char('G') => {
                r.cursor = r.blocks.len().saturating_sub(1);
                if !r.blocks.get(r.cursor).is_some_and(Block::commentable) {
                    r.step(-1);
                }
            }
            KeyCode::PageDown | KeyCode::Char(' ') | KeyCode::Char('J') => r.page(1),
            KeyCode::PageUp | KeyCode::Char('K') => r.page(-1),
            KeyCode::Char('c') | KeyCode::Enter => {
                let Some(b) = r.blocks.get(r.cursor).filter(|b| b.commentable()) else { return };
                let (block, quote) = (r.cursor, b.quote());
                let prefill = self.reviews.get(&key).and_then(|rv| rv.comments.iter().find(|c| c.block == block)).map(|c| c.text.clone()).unwrap_or_default();
                self.compose(Target::Comment { block, quote }, &prefill);
            }
            KeyCode::Char('C') => {
                let prefill = self.reviews.get(&key).map(|rv| rv.general.clone()).unwrap_or_default();
                self.compose(Target::General { send: false }, &prefill);
            }
            KeyCode::Char('d') => {
                let block = r.cursor;
                if let Some(rv) = self.reviews.get_mut(&key) {
                    let before = rv.comments.len();
                    rv.comments.retain(|c| c.block != block);
                    if rv.comments.len() < before {
                        self.flash("Comentario borrado");
                    }
                }
            }
            KeyCode::Char('a') if r.approval.is_some() => self.approve(),
            KeyCode::Char('p') if r.approval.is_some() => self.send_feedback(),
            KeyCode::Char('s') if r.approval.is_none() => self.send_feedback(),
            KeyCode::Char('x') if matches!(key, Key::R(_)) => {
                if let Some(rep) = self.selected_report().filter(|rep| Key::R(rep.id) == key) {
                    self.mark_read(&rep);
                }
            }
            KeyCode::Char(c @ '1'..='9') if matches!(key, Key::Q(_)) => {
                if let Some(q) = self.data.open_questions.iter().find(|q| Key::Q(q.id) == key).cloned() {
                    self.settle_until = 0;
                    self.choose(&q, c as usize - '1' as usize);
                }
            }
            _ => {}
        }
    }

    fn approve(&mut self) {
        let Some(r) = self.reader.as_ref() else { return };
        let Key::Q(id) = r.key else { return };
        let label = r.approval.as_ref().and_then(|a| a.approve.clone());
        let text = match self.reviews.get(&r.key).filter(|rv| !rv.is_empty()) {
            Some(rv) => rv.feedback(),
            None => label.clone().unwrap_or_else(|| "Aprobado".into()),
        };
        let node = r.node.clone();
        self.post(Some(r.key), vec![(format!("/v1/questions/{id}/answer"), answer_body(&text, label.as_deref()))], format!("Aprobado → {node}"));
        self.reader = None;
        self.hide(Key::Q(id));
    }

    fn send_feedback(&mut self) {
        let Some(r) = self.reader.as_ref() else { return };
        let key = r.key;
        let Some(review) = self.reviews.get(&key).filter(|rv| !rv.is_empty()) else {
            self.compose(Target::General { send: true }, "");
            return;
        };
        let text = review.feedback();
        let n = review.count();
        let node = r.node.clone();
        let plural = if n == 1 { "comentario" } else { "comentarios" };
        match key {
            Key::Q(id) => {
                let option = r.approval.as_ref().and_then(|a| a.changes.clone());
                let what = if option.is_some() { "Cambios pedidos" } else { "Respuesta enviada" };
                self.post(Some(key), vec![(format!("/v1/questions/{id}/answer"), answer_body(&text, option.as_deref()))], format!("{what} a {node} ({n} {plural})"));
            }
            Key::R(id) => {
                self.post(
                    Some(key),
                    vec![
                        (format!("/v1/nodes/{}/messages", urlencode(&r.session)), json!({"text": text, "from": "david"})),
                        (format!("/v1/reports/{id}/read"), json!({})),
                    ],
                    format!("{n} {plural} enviados a {node}; marcado leído"),
                );
            }
        }
        self.reader = None;
        self.hide(key);
    }

    pub fn node_matches(&self, n: &FleetNode, q: &str) -> bool {
        let hay = format!(
            "{} {} {} {} {} {}",
            n.display_name(),
            n.short_id,
            n.project.clone().unwrap_or_default(),
            n.state.label(),
            said(n),
            n.cwd.clone().unwrap_or_default()
        )
            .to_lowercase();
        q.split_whitespace().all(|w| hay.contains(w))
    }

    fn local_matches(&self, q: &str) -> Vec<usize> {
        (0..self.data.nodes.len()).filter(|&i| self.node_matches(&self.data.nodes[i], q)).collect()
    }

    pub fn fleet_filter(&self) -> Option<(Vec<usize>, Option<String>)> {
        let s = self.search.as_ref()?;
        let q = s.query();
        if q.is_empty() {
            return None;
        }
        let local = self.local_matches(&q);
        if !local.is_empty() {
            return Some((local, None));
        }
        match &s.remote {
            Remote::Done { query, via, sessions } if *query == q => {
                let idx = (0..self.data.nodes.len()).filter(|&i| sessions.contains(&self.data.nodes[i].session_id)).collect();
                Some((idx, Some(via.clone())))
            }
            _ => Some((vec![], None)),
        }
    }

    pub fn sleeping_hidden(&self) -> usize {
        if self.show_sleeping || self.fleet_filter().is_some() {
            return 0;
        }
        self.data.nodes.iter().filter(|n| !visible_awake(n)).count()
    }

    pub fn fleet_rows(&self) -> Vec<FleetRow> {
        let indices: Vec<usize> = match self.fleet_filter() {
            Some((idx, _)) => idx,
            None => (0..self.data.nodes.len()).filter(|&i| self.show_sleeping || visible_awake(&self.data.nodes[i])).collect(),
        };
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        let mut orchestrator = vec![];
        for i in indices {
            let n = &self.data.nodes[i];
            if n.is_orchestrator {
                orchestrator.push(i);
            } else {
                groups.entry(n.project.clone().unwrap_or_else(|| "otros".into())).or_default().push(i);
            }
        }
        let mut sorted: Vec<(String, Vec<usize>)> = groups.into_iter().collect();
        sorted.sort_by_key(|(p, v)| (!v.iter().any(|&i| self.data.nodes[i].alive), p.to_lowercase()));
        if !orchestrator.is_empty() {
            sorted.insert(0, ("orquestador".into(), orchestrator));
        }
        let mut rows = vec![];
        for (name, nodes) in sorted {
            let alive = nodes.iter().filter(|&&i| self.data.nodes[i].alive).count();
            let total = self.data.nodes.iter().filter(|n| !n.is_orchestrator && n.project.as_deref() == Some(name.as_str())).count().max(nodes.len());
            rows.push(FleetRow::Group { name, alive, total });
            rows.extend(nodes.into_iter().map(FleetRow::Node));
        }
        rows
    }

    fn fleet_sessions(&self) -> Vec<String> {
        self.fleet_rows()
            .iter()
            .filter_map(|r| match r {
                FleetRow::Node(i) => Some(self.data.nodes[*i].session_id.clone()),
                FleetRow::Group { .. } => None,
            })
            .collect()
    }

    pub fn selected_node(&self) -> Option<&FleetNode> {
        self.fleet_sel.as_ref().and_then(|s| self.node(s))
    }

    fn move_fleet(&mut self, to: isize) {
        let sessions = self.fleet_sessions();
        if sessions.is_empty() {
            return;
        }
        let idx = to.clamp(0, sessions.len() as isize - 1) as usize;
        if self.fleet_sel.as_ref() != Some(&sessions[idx]) {
            self.fleet_detail_scroll = 0;
        }
        self.fleet_idx = idx;
        self.fleet_sel = Some(sessions[idx].clone());
    }

    fn fleet_key(&mut self, k: KeyEvent) -> Flow {
        let i = self.fleet_idx as isize;
        match k.code {
            KeyCode::Down | KeyCode::Char('j') => self.move_fleet(i + 1),
            KeyCode::Up | KeyCode::Char('k') => self.move_fleet(i - 1),
            KeyCode::Home | KeyCode::Char('g') => self.move_fleet(0),
            KeyCode::End | KeyCode::Char('G') => self.move_fleet(isize::MAX),
            KeyCode::PageDown | KeyCode::Char('J') | KeyCode::Char(' ') => self.fleet_detail_scroll += 5,
            KeyCode::PageUp | KeyCode::Char('K') => self.fleet_detail_scroll = self.fleet_detail_scroll.saturating_sub(5),
            KeyCode::Char('/') => match self.search.as_mut() {
                Some(s) => s.editing = true,
                None => self.search = Some(Search { input: TextInput::default(), editing: true, typed_at: now_millis(), remote: Remote::Idle }),
            },
            KeyCode::Esc if self.search.is_some() => self.search = None,
            KeyCode::Char('d') => {
                self.show_sleeping = !self.show_sleeping;
                self.flash(if self.show_sleeping { "Muestro también los nodos dormidos" } else { "Oculto los nodos dormidos" });
            }
            KeyCode::Char('r') | KeyCode::Char('m') => {
                if let Some(n) = self.selected_node() {
                    let target = Target::Message { session: n.session_id.clone(), name: n.display_name().to_string(), ack: true };
                    self.compose(target, "");
                }
            }
            KeyCode::Char('a') | KeyCode::Enter => {
                if let Some(s) = self.fleet_sel.clone() {
                    return self.attach_to(&s);
                }
            }
            _ => {}
        }
        Flow::Go
    }

    fn search_key(&mut self, k: KeyEvent) {
        let Some(s) = self.search.as_mut() else { return };
        match k.code {
            KeyCode::Esc => self.search = None,
            KeyCode::Enter | KeyCode::Down | KeyCode::Up | KeyCode::Tab => {
                s.editing = false;
                if s.query().is_empty() {
                    self.search = None;
                }
            }
            _ => {
                let before = s.query();
                s.input.handle(k);
                if s.query() != before {
                    s.typed_at = now_millis();
                    s.remote = Remote::Idle;
                    self.fleet_idx = 0;
                    self.fleet_sel = None;
                }
            }
        }
    }
}

fn visible_awake(n: &FleetNode) -> bool {
    n.alive || n.is_orchestrator || matches!(n.state, NodeState::WaitingAnswer | NodeState::Unclear)
}

fn attachment_paths(a: &[alondra_core::Attachment]) -> Vec<String> {
    a.iter().filter_map(|a| a.stored_path.clone()).collect()
}

fn links_part(links: &[alondra_core::Link]) -> Option<Part> {
    (!links.is_empty()).then(|| {
        let items: Vec<String> = links.iter().map(|l| format!("- {} {}", l.title.clone().unwrap_or_default(), l.url)).collect();
        Part::Text(format!("## Enlaces\n\n{}", items.join("\n")))
    })
}

fn new_reader(key: Key, session: &str, node: &Option<String>, title: &str, kind: &'static str, approval: Option<Approval>, parts: Vec<Part>) -> Reader {
    Reader {
        key,
        session: session.to_string(),
        node: who(node, session),
        title: title.to_string(),
        kind,
        approval,
        parts,
        blocks: vec![],
        cursor: 0,
        scroll: 0,
        ranges: vec![],
        view_h: 0,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use alondra_core::{QuestionOption, QuestionStatus};

    pub(super) fn question(id: u64, origin: QuestionOrigin, options: &[&str]) -> Question {
        Question {
            id,
            session_id: "s1".into(),
            node_name: Some("demo".into()),
            title: format!("Pregunta {id}"),
            detail: None,
            options: options.iter().map(|l| QuestionOption { label: l.to_string(), description: None }).collect(),
            links: vec![],
            attachments: vec![],
            blocking: true,
            origin,
            tool_use_id: None,
            to_orchestrator: false,
            node_context: None,
            status: QuestionStatus::Open,
            answer: None,
            asked_at: 0,
        }
    }

    pub(super) fn app_with(questions: Vec<Question>) -> (App, Receiver<Msg>) {
        let (tx, rx) = mpsc::sync_channel(8);
        let mut app = App::new(tx);
        app.data.open_questions = questions;
        app.reconcile();
        (app, rx)
    }

    #[test]
    fn plans_and_approve_or_change_questions_get_the_review_keys() {
        let plan = approval_of(&question(1, QuestionOrigin::Plan, &["Aprobar", "Pedir cambios"])).unwrap();
        assert_eq!(plan.approve.as_deref(), Some("Aprobar"));
        assert_eq!(plan.changes.as_deref(), Some("Pedir cambios"));
        let demo = approval_of(&question(2, QuestionOrigin::Ask, &["Aprobar", "Cambiar"])).unwrap();
        assert_eq!(demo.changes.as_deref(), Some("Cambiar"));
        assert!(approval_of(&question(3, QuestionOrigin::Ask, &["Sí", "No"])).is_none());
    }

    #[test]
    fn answering_advances_to_the_next_item_and_guards_against_a_double_press() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &["Sí"]), question(2, QuestionOrigin::Ask, &["Sí"])]);
        assert_eq!(app.sel, Some(Key::Q(1)));
        app.hide(Key::Q(1));
        assert_eq!(app.sel, Some(Key::Q(2)));
        assert!(app.settle_until > now_millis());
        let q2 = app.selected_question().unwrap();
        app.choose(&q2, 0);
        assert!(app.flash.as_ref().is_some_and(|f| f.error));
        assert_eq!(app.sel, Some(Key::Q(2)));
    }

    #[test]
    fn selection_follows_the_item_when_the_list_reorders() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &[]), question(2, QuestionOrigin::Ask, &[])]);
        app.move_inbox(1);
        assert_eq!(app.sel, Some(Key::Q(2)));
        app.data.open_questions.insert(0, question(9, QuestionOrigin::Ask, &[]));
        app.reconcile();
        assert_eq!(app.sel, Some(Key::Q(2)));
        assert_eq!(app.sel_idx, 2);
    }

    #[test]
    fn a_failed_send_brings_the_item_back() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &[])]);
        app.hide(Key::Q(1));
        assert!(app.inbox_keys().is_empty());
        app.on_msg(Msg::Posted { key: Some(Key::Q(1)), result: Err("caído".into()) });
        app.reconcile();
        assert_eq!(app.sel, Some(Key::Q(1)));
    }

    #[test]
    fn a_cancelled_reply_comes_back_as_a_draft() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &[])]);
        app.compose(Target::Answer { id: 1, title: "t".into() }, "");
        app.composer.as_mut().unwrap().input.insert_str("medio escrito");
        app.cancel_composer();
        assert!(app.composer.is_none());
        app.compose(Target::Answer { id: 1, title: "t".into() }, "");
        assert_eq!(app.composer.as_ref().unwrap().input.text(), "medio escrito");
    }

    #[test]
    fn review_feedback_follows_document_order() {
        let mut rv = Review::default();
        rv.comments.push(Comment { block: 1, quote: "a".into(), text: "x".into() });
        rv.comments.push(Comment { block: 0, quote: "b".into(), text: "y".into() });
        rv.comments.sort_by_key(|c| c.block);
        assert!(rv.feedback().starts_with("Comentarios de David:\n\n1. Sobre «b»:"));
    }
}
