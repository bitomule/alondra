mod agents;
mod draw;
mod inbox;
mod input;
mod keys;
mod md;
mod tasks;

use std::collections::{HashMap, HashSet};
use std::io::stdout;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use alondra_core::domain_v2::{
    AgentId, AvailableAction, Command, CommandEnvelope, CommandOutcome, DeliveryTarget, DomainProjection, InteractionId, Revision, TaskId,
};
use alondra_core::now_millis;
use anyhow::Result;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use crate::client::{Client, CommandFailure};
use crate::mcp::urlencode;
use input::TextInput;
use keys::{Cmd, Keymap};
use md::Block;
pub use tasks::state_label;

const SETTLE_MS: i64 = 700;
const FLASH_MS: i64 = 4500;
const HIDE_GRACE_MS: i64 = 15_000;
const CLOCK_MS: i64 = 20_000;
const CONFIRM_MS: i64 = 4000;
const IDLE_POLL: Duration = Duration::from_millis(250);

/// An inbox item by the id the daemon gave it; roots and children keep their own ids.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Key(pub String);

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Inbox,
    Tasks,
    Fleet,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Pane {
    List,
    Detail,
}

/// What the last frame drew for a scrollable pane: rows visible, rows of content, and whether it was on screen.
#[derive(Clone, Copy, Default)]
pub struct View {
    pub h: usize,
    pub total: usize,
    pub shown: bool,
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
        md::feedback(&pairs, &self.general, &crate::client::config().name)
    }
}

pub struct Find {
    pub input: TextInput,
    pub editing: bool,
}

impl Find {
    pub fn query(&self) -> String {
        self.input.text().trim().to_lowercase()
    }
}

pub struct Reader {
    pub key: Key,
    pub agent: String,
    pub title: String,
    pub kind: &'static str,
    pub glyph: &'static str,
    pub color: ratatui::style::Color,
    parts: Vec<String>,
    pub blocks: Vec<Block>,
    pub cursor: usize,
    pub scroll: usize,
    pub ranges: Vec<(usize, usize)>,
    pub view_h: usize,
    pub total: usize,
    pub find: Option<Find>,
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

    fn down(&mut self) {
        match self.ranges.get(self.cursor) {
            Some(&(_, e)) if e > self.scroll + self.view_h => self.scroll += 1,
            _ => self.step(1),
        }
    }

    fn up(&mut self) {
        match self.ranges.get(self.cursor) {
            Some(&(s, _)) if s < self.scroll => self.scroll -= 1,
            _ => self.step(-1),
        }
    }

    fn top(&mut self) {
        self.cursor = 0;
        self.scroll = 0;
        self.settle_cursor();
    }

    fn bottom(&mut self) {
        self.cursor = self.blocks.len().saturating_sub(1);
        if !self.blocks.get(self.cursor).is_some_and(Block::commentable) {
            self.step(-1);
        }
        self.scroll = self.total.saturating_sub(self.view_h);
    }

    /// Scrolls by lines, and moves the block cursor only when its block leaves the screen.
    fn scroll_by(&mut self, d: isize) {
        if self.ranges.is_empty() {
            return self.step(d.signum());
        }
        let (h, max) = (self.view_h.max(1), self.total.saturating_sub(self.view_h));
        self.scroll = (self.scroll as isize + d).clamp(0, max as isize) as usize;
        let (top, bottom) = (self.scroll, self.scroll + h);
        let fits = |&(s, e): &(usize, usize)| s >= top && e <= bottom;
        let covers = |&(s, e): &(usize, usize)| s <= top && e >= bottom;
        if self.ranges.get(self.cursor).is_some_and(|r| fits(r) || covers(r)) {
            return;
        }
        let ok = |i: &usize| self.blocks.get(*i).is_some_and(Block::commentable) && self.ranges.get(*i).is_some_and(|r| fits(r) || covers(r));
        let n = self.ranges.len();
        let found = if d > 0 { (0..n).find(ok) } else { (0..n).rev().find(ok) };
        if let Some(i) = found {
            self.cursor = i;
        } else if let Some(i) = (0..n).find(|&i| self.blocks.get(i).is_some_and(Block::commentable) && self.ranges[i].0 >= top) {
            self.cursor = i;
        }
    }

    fn matches(&self) -> Vec<usize> {
        let Some(q) = self.find.as_ref().map(Find::query).filter(|q| !q.is_empty()) else { return vec![] };
        (0..self.blocks.len()).filter(|&i| self.blocks[i].commentable() && self.blocks[i].plain().to_lowercase().contains(&q)).collect()
    }

    fn jump(&mut self, forward: bool, include_current: bool) -> bool {
        let hits = self.matches();
        let c = self.cursor;
        let next = if forward {
            hits.iter().find(|&&i| i > c || (include_current && i == c)).or(hits.first())
        } else {
            hits.iter().rev().find(|&&i| i < c).or(hits.last())
        };
        match next {
            Some(&i) => {
                self.cursor = i;
                if let Some(&(s, _)) = self.ranges.get(i) {
                    self.scroll = s.saturating_sub(1);
                }
                true
            }
            None => false,
        }
    }

    fn settle_cursor(&mut self) {
        self.cursor = self.cursor.min(self.blocks.len().saturating_sub(1));
        if !self.blocks.get(self.cursor).is_some_and(Block::commentable) {
            self.step(1);
        }
    }
}

/// What the composer's text becomes. Each carries the subject and revision it was opened on, so a
/// change elsewhere never sends the text to something else.
pub enum Target {
    Answer { key: Key, interaction: InteractionId, option: String, label: String, input: Option<String>, required: bool, title: String, revision: Revision },
    Feedback { key: Key, presentation: InteractionId, targets: Vec<DeliveryTarget>, title: String, revision: Revision },
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
            Target::Answer { interaction, option, .. } => Some(format!("answer:{interaction}:{option}")),
            Target::Feedback { presentation, .. } => Some(format!("feedback:{presentation}")),
            Target::NewRequest => Some("new".into()),
            Target::Comment { .. } | Target::General { .. } => None,
        }
    }
}

pub struct Search {
    pub input: TextInput,
    pub editing: bool,
}

impl Search {
    pub fn query(&self) -> String {
        self.input.text().trim().to_lowercase()
    }
}

enum Msg {
    Data(Box<DomainProjection>),
    Offline,
    SyncError(String),
    Command { slot: String, result: Result<CommandOutcome, CommandFailure> },
    Diff { key: Key, result: Result<Value, String> },
    TaskActions { task: TaskId, revision: Revision, result: Result<Vec<AvailableAction>, String> },
}

pub enum Flow {
    Go,
    Quit,
    Attach(String),
    EditSchedule(Option<Value>),
}

/// A command on its way, or one whose outcome is unknown: sending the same thing again repeats
/// this envelope, so the daemon answers with the first receipt instead of doing it twice.
struct Sent {
    envelope: CommandEnvelope,
    key: Option<Key>,
    ok: String,
    draft: Option<(String, String)>,
    waiting: bool,
}

pub struct App {
    pub domain: Option<DomainProjection>,
    pub connected: bool,
    pub loaded: bool,
    pub offline: bool,
    pub screen: Screen,
    pub schedules_open: bool,
    pub schedules: Vec<Value>,
    pub schedule_idx: usize,
    pub schedule_scroll: usize,
    schedule_delete: Option<(String, i64)>,
    pub sel: Option<Key>,
    sel_idx: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    settle_until: i64,
    hidden: HashMap<Key, i64>,
    pub agent_sel: Option<AgentId>,
    agent_idx: usize,
    pub agent_scroll: usize,
    pub agent_detail_scroll: usize,
    pub agent_view: View,
    pub agent_zoom: bool,
    /// A step that takes a second press within a few seconds, and until when.
    confirm: Option<(String, i64)>,
    sending: HashMap<String, Sent>,
    #[cfg(test)]
    sent: std::cell::RefCell<Vec<CommandEnvelope>>,
    pub search: Option<Search>,
    pub composer: Option<Composer>,
    pub reader: Option<Reader>,
    pub reviews: HashMap<Key, Review>,
    drafts: HashMap<String, String>,
    pub flash: Option<Flash>,
    pub help: bool,
    pub help_scroll: usize,
    pub help_view: View,
    pub focus: Pane,
    pub detail_view: View,
    pub task_sel: Option<tasks::TaskKey>,
    task_idx: usize,
    pub task_scroll: usize,
    pub task_detail_scroll: usize,
    pub task_view: View,
    pub task_zoom: bool,
    pub archived_open: bool,
    pub finished_open: bool,
    /// The selected task's controls as the daemon computed them, by the revision they were read at.
    pub task_actions: HashMap<TaskId, (Revision, Vec<AvailableAction>)>,
    task_actions_asked: HashSet<(TaskId, Revision)>,
    pub cmdline: Option<TextInput>,
    keymap: Keymap,
    last_clock: i64,
    last_task_animation: i64,
    tx: SyncSender<Msg>,
}

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Msg>(64);
    let quick = Client::for_tui(Duration::from_secs(15));
    let waiter = Client::for_tui(Duration::from_secs(70));
    let sync_tx = tx.clone();
    std::thread::Builder::new().name("alondra-sync".into()).spawn(move || sync_loop(sync_tx, quick, waiter))?;
    crate::attach::reset_terminal_modes();
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let app = App::new(tx);
    let res = main_loop(&mut terminal, app, rx);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::attach::reset_terminal_modes();
    res
}

fn background_sync_error(error: &anyhow::Error, prefix: &str) -> Msg {
    if error.is::<crate::account::AccountBusy>() { Msg::Offline } else { Msg::SyncError(format!("{prefix}{error}")) }
}

/// Reads the projection, then waits on the daemon until it moves past that revision.
fn sync_loop(tx: SyncSender<Msg>, quick: Client, waiter: Client) {
    let mut was_online = true;
    loop {
        let (epoch, revision) = match quick.domain() {
            Ok(p) => {
                was_online = true;
                let at = (p.data_epoch.clone(), p.revision);
                if tx.send(Msg::Data(Box::new(p))).is_err() {
                    return;
                }
                at
            }
            Err(error) => {
                if was_online && (tx.send(background_sync_error(&error, "")).is_err() || tx.send(Msg::Offline).is_err()) {
                    return;
                }
                was_online = false;
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if let Err(error) = waiter.domain_changed(&epoch, revision, 50) {
            let _ = tx.send(background_sync_error(&error, tr!("Waiting for changes failed: ", "Falló la espera de cambios: ")));
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

/// Back from an agent's session: its last frame is still on screen and ratatui does not know,
/// so the whole TUI is drawn over it in one synchronized update, with no blank frame between.
fn repaint(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    execute!(stdout(), BeginSynchronizedUpdate)?;
    let res = terminal.clear().and_then(|_| {
        app.reconcile();
        terminal.draw(|f| draw::draw(f, app)).map(|_| ())
    });
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
                        Flow::Attach(agent) => {
                            if crate::service_client::target().is_some() {
                                app.fail(tr!("Remote terminal attachment is unavailable; open the terminal on the destination Mac", "El terminal remoto no está disponible; ábrelo en el Mac de destino"));
                                break;
                            }
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            let res = crate::attach::attach(&agent, crate::attach::Over::Tui);
                            crossterm::terminal::enable_raw_mode()?;
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            if let Err(e) = res {
                                app.fail(trf!("Could not open its terminal: {e}", "No pude abrir su terminal: {e}"));
                            }
                            repaint(terminal, &mut app)?;
                            break;
                        }
                        Flow::EditSchedule(existing) => {
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            ratatui::restore();
                            let result = edit_schedule(existing);
                            *terminal = ratatui::init();
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            match result {
                                Ok(Some(())) => {
                                    app.flash(tr!("Schedule saved", "Programación guardada"));
                                    app.reload_schedules();
                                }
                                Ok(None) => app.flash(tr!("No changes saved", "No se guardaron cambios")),
                                Err(e) => app.fail(e.to_string()),
                            }
                            repaint(terminal, &mut app)?;
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

const SCHEDULES: &str = "/v2/domain/schedules";

/// The fields a schedule write accepts; what the daemon adds when it lists one stays out.
fn schedule_draft(value: &Value) -> Value {
    let fields = ["title", "text", "project", "options", "disallowed_tools", "frequency", "hour", "minute", "weekday", "interval", "start_at", "enabled", "revision"];
    Value::Object(fields.iter().filter_map(|f| value.get(*f).map(|v| (f.to_string(), v.clone()))).collect())
}

fn edit_schedule(existing: Option<Value>) -> Result<Option<()>> {
    let path = std::env::temp_dir().join(format!("alondra-schedule-{}-{}.json", std::process::id(), now_millis()));
    let body = existing.as_ref().map(schedule_draft).unwrap_or_else(|| json!({
        "title": "", "text": "", "project": null, "options": {}, "disallowed_tools": [],
        "frequency": "daily", "hour": 9, "minute": 0, "weekday": 1,
        "interval": 1, "start_at": now_millis() + 3_600_000, "enabled": true
    }));
    let before = serde_json::to_vec_pretty(&body)?;
    use std::io::Write;
    #[cfg(unix)] use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new();
    file.write(true).create_new(true);
    #[cfg(unix)] file.mode(0o600);
    file.open(&path)?.write_all(&before)?;
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
    let status = std::process::Command::new(editor).arg(&path).status();
    let edited = std::fs::read(&path);
    let _ = std::fs::remove_file(&path);
    if !status?.success() { anyhow::bail!("editor exited without saving"); }
    let edited = edited?;
    if edited == before { return Ok(None); }
    let body: Value = serde_json::from_slice(&edited).map_err(|e| anyhow::anyhow!("Invalid schedule JSON: {e}"))?;
    let client = Client::for_tui(Duration::from_secs(15));
    if let Some(id) = existing.as_ref().and_then(|v| v["id"].as_str()) {
        client.put(&format!("{SCHEDULES}/{}", urlencode(id)), &schedule_draft(&body))?;
    } else {
        client.post(SCHEDULES, &schedule_draft(&body))?;
    }
    Ok(Some(()))
}

/// The Mac's default app for a URL, through `open`. Tests never open one.
fn open_with_mac(target: &str) {
    if cfg!(test) {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open").arg(target).spawn();
}

impl App {
    fn new(tx: SyncSender<Msg>) -> Self {
        App {
            domain: None,
            connected: false,
            loaded: false,
            offline: false,
            screen: Screen::Inbox,
            schedules_open: false,
            schedules: vec![],
            schedule_idx: 0,
            schedule_scroll: 0,
            schedule_delete: None,
            sel: None,
            sel_idx: 0,
            list_scroll: 0,
            detail_scroll: 0,
            settle_until: 0,
            hidden: HashMap::new(),
            agent_sel: None,
            agent_idx: 0,
            agent_scroll: 0,
            agent_detail_scroll: 0,
            agent_view: View::default(),
            agent_zoom: false,
            confirm: None,
            sending: HashMap::new(),
            #[cfg(test)]
            sent: Default::default(),
            search: None,
            composer: None,
            reader: None,
            reviews: HashMap::new(),
            drafts: HashMap::new(),
            flash: None,
            help: false,
            help_scroll: 0,
            help_view: View::default(),
            focus: Pane::List,
            detail_view: View::default(),
            task_sel: None,
            task_idx: 0,
            task_scroll: 0,
            task_detail_scroll: 0,
            task_view: View::default(),
            task_zoom: false,
            archived_open: false,
            finished_open: false,
            task_actions: HashMap::new(),
            task_actions_asked: HashSet::new(),
            cmdline: None,
            keymap: Keymap::default(),
            last_clock: now_millis(),
            last_task_animation: now_millis() / 500,
            tx,
        }
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

    /// Every call to alondrad runs here. Tests never reach the user's live alondrad.
    fn spawn(&self, job: impl FnOnce(&SyncSender<Msg>) + Send + 'static) {
        if cfg!(test) {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || job(&tx));
    }

    /// A second press of the same step within a few seconds confirms it; anything else starts over.
    fn confirmed(&mut self, step: &str, ask: String) -> bool {
        let now = now_millis();
        if self.confirm.as_ref().is_some_and(|(s, until)| s == step && now < *until) {
            self.confirm = None;
            return true;
        }
        self.confirm = Some((step.to_string(), now + CONFIRM_MS));
        self.fail(ask);
        false
    }

    /// Sends a domain command. `slot` names what is being done; while one is on its way the same
    /// slot is refused, and after an unknown outcome the same slot resends the same envelope.
    fn send(&mut self, slot: String, key: Option<Key>, expected: Revision, command: Command, ok: String, draft: Option<(String, String)>) {
        let Some(epoch) = self.domain.as_ref().map(|d| d.data_epoch.clone()) else { return };
        let envelope = match self.sending.get(&slot) {
            Some(sent) if sent.waiting => {
                self.fail(tr!("Still sending the previous one: wait for its answer", "Aún se está enviando el anterior: espera a su respuesta"));
                return;
            }
            Some(sent) => sent.envelope.clone(),
            None => crate::client::envelope(&epoch, expected, command),
        };
        #[cfg(test)]
        self.sent.borrow_mut().push(envelope.clone());
        self.sending.insert(slot.clone(), Sent { envelope: envelope.clone(), key, ok, draft, waiting: true });
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(15)).command(&envelope);
            let _ = tx.send(Msg::Command { slot, result });
        });
    }

    fn command_done(&mut self, slot: String, result: Result<CommandOutcome, CommandFailure>) {
        let Some(mut sent) = self.sending.remove(&slot) else { return };
        match result {
            Ok(_) => {
                if let Some(k) = &sent.key {
                    self.reviews.remove(k);
                }
                self.flash(sent.ok);
            }
            Err(failure) => {
                if let Some(k) = &sent.key {
                    self.hidden.remove(k);
                }
                if let Some((draft, text)) = sent.draft.take().filter(|(_, t)| !t.trim().is_empty()) {
                    self.drafts.insert(draft, text);
                }
                match failure {
                    CommandFailure::Rejected(error) => {
                        let why = crate::client::refusal(&error);
                        self.fail(trf!("Not done: {why}. Your text is kept.", "No se hizo: {why}. Tu texto se conserva."));
                    }
                    CommandFailure::Uncertain(why) => {
                        sent.waiting = false;
                        self.sending.insert(slot, sent);
                        self.fail(trf!(
                            "Not confirmed: {why}. Doing it again resends the same request, so it cannot happen twice.",
                            "Sin confirmar: {why}. Si lo repites se reenvía la misma petición, así que no puede pasar dos veces."
                        ));
                    }
                }
            }
        }
    }

    fn reconcile(&mut self) {
        let now = now_millis();
        let present: HashSet<Key> = self.all_keys().into_iter().collect();
        self.hidden.retain(|k, at| present.contains(k) && now - *at < HIDE_GRACE_MS);
        let keys = self.inbox_keys();
        let prev = self.sel.clone();
        match self.sel.as_ref().and_then(|s| keys.iter().position(|k| k == s)) {
            Some(i) => self.sel_idx = i,
            None => {
                self.sel_idx = self.sel_idx.min(keys.len().saturating_sub(1));
                self.sel = keys.get(self.sel_idx).cloned();
            }
        }
        if self.sel != prev {
            self.detail_scroll = 0;
            if prev.is_some() {
                self.settle_until = now + SETTLE_MS;
            }
        }
        self.reconcile_agents();
        self.reconcile_tasks();
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Data(p) => {
                self.domain = Some(*p);
                self.connected = true;
                self.loaded = true;
                self.offline = false;
            }
            Msg::SyncError(error) => self.fail(error),
            Msg::Offline => {
                self.connected = false;
                self.offline = true;
            }
            Msg::Command { slot, result } => self.command_done(slot, result),
            Msg::Diff { key, result } => match result {
                Err(error) => self.fail(error),
                Ok(value) if self.sel.as_ref() == Some(&key) => {
                    self.open_reader();
                    if let Some(reader) = self.reader.as_mut() {
                        let diff = value["diff"].as_str().unwrap_or("");
                        let head = value["head"].as_str().unwrap_or("?");
                        reader.parts.push(format!("## Diff @ {head}\n\n```diff\n{diff}\n```"));
                        if value["truncated"] == true {
                            reader.parts.push(tr!("The diff was cut to fit: open the PR for all of it.", "El diff se ha recortado: abre el PR para verlo entero.").into());
                        }
                    }
                    self.rebuild_reader();
                }
                Ok(_) => {}
            },
            Msg::TaskActions { task, revision, result } => match result {
                Ok(actions) => {
                    self.task_actions.insert(task, (revision, actions));
                }
                Err(error) => {
                    self.task_actions_asked.remove(&(task, revision));
                    self.fail(error);
                }
            },
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
        if self.screen == Screen::Tasks && now / 500 != self.last_task_animation {
            self.last_task_animation = now / 500;
            dirty |= self.running_tasks();
        }
        dirty
    }

    fn on_paste(&mut self, s: &str) {
        if let Some(c) = self.composer.as_mut() {
            c.input.insert_str(s);
        } else if let Some(search) = self.search.as_mut().filter(|s| s.editing) {
            search.input.insert_str(&md::one_line(s));
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Flow {
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Flow::Quit;
        }
        if self.help {
            self.help_key(k);
            return Flow::Go;
        }
        if self.schedules_open {
            return self.schedule_key(k);
        }
        if self.composer.is_some() {
            self.composer_key(k);
            return Flow::Go;
        }
        if self.cmdline.is_some() {
            return self.cmdline_key(k);
        }
        if self.reader.as_ref().is_some_and(|r| r.find.as_ref().is_some_and(|f| f.editing)) {
            self.find_key(k);
            return Flow::Go;
        }
        if self.screen == Screen::Fleet && self.reader.is_none() && self.search.as_ref().is_some_and(|s| s.editing) {
            self.search_key(k);
            return Flow::Go;
        }
        let cmd = self.keymap.read(k);
        if let Cmd::Other(k) = cmd
            && k.modifiers.is_empty()
        {
            match k.code {
                KeyCode::Char('?') => {
                    self.help = true;
                    self.help_scroll = 0;
                    return Flow::Go;
                }
                KeyCode::Char(':') => {
                    self.cmdline = Some(TextInput::default());
                    return Flow::Go;
                }
                _ => {}
            }
        }
        if self.reader.is_some() {
            self.reader_cmd(cmd);
            return Flow::Go;
        }
        match cmd {
            Cmd::Pending => {}
            Cmd::Other(k) => match k.code {
                KeyCode::Char('q') => return Flow::Quit,
                KeyCode::Tab => self.switch_screen(),
                KeyCode::BackTab => self.switch_screen_back(),
                KeyCode::Char('f') => self.show(if self.screen == Screen::Fleet { Screen::Inbox } else { Screen::Fleet }),
                KeyCode::Char('n') => self.compose(Target::NewRequest, ""),
                KeyCode::Char('s') if self.screen == Screen::Tasks => {
                    self.schedules_open = true;
                    self.reload_schedules();
                }
                _ => {
                    return match self.screen {
                        Screen::Inbox => self.inbox_key(k),
                        Screen::Tasks => self.tasks_key(k),
                        Screen::Fleet => self.agents_key(k),
                    };
                }
            },
            _ => match self.screen {
                Screen::Inbox => self.inbox_cmd(cmd),
                Screen::Tasks => self.tasks_cmd(cmd),
                Screen::Fleet => self.agents_cmd(cmd),
            },
        }
        Flow::Go
    }

    /// Inbox, Tasks, Agents and round again.
    fn switch_screen(&mut self) {
        self.show(match self.screen {
            Screen::Inbox => Screen::Tasks,
            Screen::Tasks => Screen::Fleet,
            Screen::Fleet => Screen::Inbox,
        });
    }

    fn switch_screen_back(&mut self) {
        self.show(match self.screen {
            Screen::Inbox => Screen::Fleet,
            Screen::Tasks => Screen::Inbox,
            Screen::Fleet => Screen::Tasks,
        });
    }

    fn show(&mut self, screen: Screen) {
        self.screen = screen;
        self.schedules_open = false;
        self.focus = Pane::List;
        self.agent_zoom = false;
        self.task_zoom = false;
        self.reconcile_tasks();
    }

    fn reload_schedules(&mut self) {
        match Client::for_tui(Duration::from_secs(5)).get::<Value>(SCHEDULES) {
            Ok(value) => {
                self.schedules = value["schedules"].as_array().cloned().unwrap_or_default();
                self.schedule_idx = self.schedule_idx.min(self.schedules.len().saturating_sub(1));
            }
            Err(e) => self.fail(trf!("Schedules: {e}", "Programaciones: {e}")),
        }
    }

    fn schedule_key(&mut self, k: KeyEvent) -> Flow {
        let selected = self.schedules.get(self.schedule_idx).cloned();
        match k.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('s') => self.schedules_open = false,
            KeyCode::Char('j') | KeyCode::Down => {
                self.schedule_idx = (self.schedule_idx + 1).min(self.schedules.len().saturating_sub(1));
                self.schedule_delete = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.schedule_idx = self.schedule_idx.saturating_sub(1);
                self.schedule_delete = None;
            }
            KeyCode::Char('n') => return Flow::EditSchedule(None),
            KeyCode::Char('e') => {
                if let Some(value) = selected {
                    return Flow::EditSchedule(Some(value));
                }
            }
            KeyCode::Char('r') => self.reload_schedules(),
            KeyCode::Char('p') => {
                if let Some(value) = selected {
                    let id = value["id"].as_str().unwrap_or("").to_owned();
                    let mut draft = schedule_draft(&value);
                    draft["enabled"] = json!(!value["enabled"].as_bool().unwrap_or(false));
                    match Client::for_tui(Duration::from_secs(15)).put(&format!("{SCHEDULES}/{}", urlencode(&id)), &draft) {
                        Ok(_) => self.reload_schedules(),
                        Err(e) => self.fail(e.to_string()),
                    }
                }
            }
            KeyCode::Enter => {
                if let Some(value) = selected {
                    let id = value["id"].as_str().unwrap_or("");
                    match Client::for_tui(Duration::from_secs(15)).post(&format!("{SCHEDULES}/{}/run", urlencode(id)), &json!(null)) {
                        Ok(_) => {
                            self.flash(tr!("Run requested", "Ejecución pedida"));
                            self.reload_schedules();
                        }
                        Err(e) => {
                            self.fail(e.to_string());
                            self.reload_schedules();
                        }
                    }
                }
            }
            KeyCode::Char('d') => {
                if let Some(value) = selected {
                    let id = value["id"].as_str().unwrap_or("");
                    let now = now_millis();
                    if self.schedule_delete.as_ref().is_some_and(|(pending, until)| pending == id && now < *until) {
                        self.schedule_delete = None;
                        match Client::for_tui(Duration::from_secs(15)).delete(&format!("{SCHEDULES}/{}", urlencode(id))) {
                            Ok(()) => self.reload_schedules(),
                            Err(e) => self.fail(e.to_string()),
                        }
                    } else {
                        self.schedule_delete = Some((id.to_string(), now + CONFIRM_MS));
                        self.flash(tr!("Press d again to delete this schedule", "Pulsa d otra vez para borrar esta programación"));
                    }
                }
            }
            _ => {}
        }
        Flow::Go
    }

    fn help_key(&mut self, k: KeyEvent) {
        match self.keymap.read(k) {
            Cmd::Move(m) => self.help_scroll = keys::scroll(self.help_scroll, m, self.help_view.h, self.help_view.total),
            Cmd::Pending => {}
            _ => self.help = false,
        }
    }

    fn cmdline_key(&mut self, k: KeyEvent) -> Flow {
        let Some(input) = self.cmdline.as_mut() else { return Flow::Go };
        match k.code {
            KeyCode::Esc => self.cmdline = None,
            KeyCode::Backspace if input.text().is_empty() => self.cmdline = None,
            KeyCode::Enter => {
                let line = input.text();
                self.cmdline = None;
                return self.run_command(line.trim());
            }
            _ => {
                input.handle(k);
            }
        }
        Flow::Go
    }

    fn run_command(&mut self, line: &str) -> Flow {
        match line {
            "" => {}
            "q" | "q!" | "qa" | "qa!" | "quit" | "wq" | "x" => {
                if self.reader.is_some() && matches!(line, "q" | "q!" | "quit") {
                    self.reader = None;
                } else {
                    return Flow::Quit;
                }
            }
            "h" | "help" => {
                self.help = true;
                self.help_scroll = 0;
            }
            "inbox" | "i" => {
                self.reader = None;
                self.show(Screen::Inbox);
            }
            "tasks" | "t" => {
                self.reader = None;
                self.show(Screen::Tasks);
            }
            "fleet" | "f" | "agents" => {
                self.reader = None;
                self.show(Screen::Fleet);
            }
            n if n.parse::<usize>().is_ok_and(|n| n > 0) && self.reader.is_none() => {
                let i = n.parse::<isize>().unwrap_or(1) - 1;
                match self.screen {
                    Screen::Inbox => self.move_inbox(i),
                    Screen::Tasks => self.move_tasks(i),
                    Screen::Fleet => self.move_agents(i),
                }
            }
            other => self.fail(trf!("Not a command: {other}  (try :q, :help, :inbox, :tasks, :agents, :<number>)", "No es un comando: {other}  (prueba :q, :help, :inbox, :tasks, :agents, :<número>)")),
        }
        Flow::Go
    }

    fn compose(&mut self, target: Target, prefill: &str) {
        let text = target.draft_key().and_then(|k| self.drafts.remove(&k)).unwrap_or_else(|| prefill.to_string());
        self.composer = Some(Composer { target, input: TextInput::with(&text) });
    }

    fn cancel_composer(&mut self) {
        let Some(c) = self.composer.take() else { return };
        let text = c.input.text();
        if let Some(k) = c.target.draft_key()
            && !text.trim().is_empty()
        {
            self.drafts.insert(k, text);
            self.flash(tr!("Draft saved: open it again to pick up where you left off", "Borrador guardado: vuelve a abrirlo y sigue donde lo dejaste"));
        }
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
        let needs_text = match &c.target {
            Target::Answer { required, .. } => *required,
            Target::Feedback { .. } | Target::NewRequest => true,
            Target::Comment { .. } | Target::General { .. } => false,
        };
        if needs_text && text.is_empty() {
            self.composer = Some(c);
            return;
        }
        let draft = c.target.draft_key().map(|k| (k, text.clone()));
        match c.target {
            Target::Answer { key, interaction, option, label, input, revision, .. } => {
                let inputs = input.filter(|_| !text.is_empty()).map(|id| (id, alondra_core::domain_v2::InputValue::Text(text.clone()))).into_iter().collect();
                self.answer(key, interaction, option, label, inputs, revision, draft);
            }
            Target::Feedback { key, presentation, targets, revision, .. } => self.feedback(key, presentation, targets, text, revision, draft),
            Target::NewRequest => {
                let Some(revision) = self.domain.as_ref().map(|d| d.revision) else { return };
                let command = Command::CreateRequest { request_id: crate::client::new_id("request").into(), body: text.clone() };
                self.send(format!("request:{text}"), None, revision, command, tr!("Sent to the orchestrator", "Enviado al orquestador").into(), draft);
            }
            Target::Comment { block, quote } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else { return };
                let review = self.reviews.entry(key).or_default();
                review.comments.retain(|c| c.block != block);
                if !text.is_empty() {
                    review.comments.push(Comment { block, quote, text });
                    review.comments.sort_by_key(|c| c.block);
                }
            }
            Target::General { send } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else { return };
                self.reviews.entry(key).or_default().general = text;
                if send {
                    self.send_review();
                }
            }
        }
    }

    fn open_reader(&mut self) {
        let Some(key) = self.sel.clone() else { return };
        let Some(item) = self.item(&key).cloned() else { return };
        let (glyph, kind, color) = draw::kind_glyph(&item.kind);
        let parts = self.item_text(&item);
        self.reader = Some(Reader {
            key,
            agent: self.agent_name(item.agent_id.as_ref()),
            title: item.title.clone(),
            kind,
            glyph,
            color,
            parts,
            blocks: vec![],
            cursor: 0,
            scroll: 0,
            ranges: vec![],
            view_h: 0,
            total: 0,
            find: None,
        });
        self.rebuild_reader();
    }

    fn rebuild_reader(&mut self) {
        let Some(r) = self.reader.as_mut() else { return };
        r.blocks = r.parts.iter().flat_map(|t| md::parse(t)).collect();
        r.settle_cursor();
    }

    fn reader_cmd(&mut self, cmd: Cmd) {
        let Some(r) = self.reader.as_mut() else { return };
        match cmd {
            Cmd::Move(keys::Motion::Down) => r.down(),
            Cmd::Move(keys::Motion::Up) => r.up(),
            Cmd::Move(keys::Motion::Top) => r.top(),
            Cmd::Move(keys::Motion::Bottom) => r.bottom(),
            Cmd::Move(m) => {
                let d = keys::lines(m, r.view_h).unwrap_or(0);
                r.scroll_by(d);
            }
            Cmd::Back if r.find.is_some() => r.find = None,
            Cmd::Left | Cmd::Back | Cmd::Expand => self.reader = None,
            Cmd::Right | Cmd::Pending => {}
            Cmd::Other(k) => self.reader_key(k),
        }
    }

    fn find_key(&mut self, k: KeyEvent) {
        let Some(r) = self.reader.as_mut() else { return };
        let Some(f) = r.find.as_mut() else { return };
        match k.code {
            KeyCode::Esc => r.find = None,
            KeyCode::Enter => {
                f.editing = false;
                let q = f.input.text();
                if q.trim().is_empty() {
                    r.find = None;
                } else if !r.jump(true, true) {
                    self.fail(trf!("Not found: {q}", "No aparece: {q}"));
                }
            }
            _ => {
                f.input.handle(k);
            }
        }
    }

    fn reader_key(&mut self, k: KeyEvent) {
        let Some(r) = self.reader.as_mut() else { return };
        let key = r.key.clone();
        match k.code {
            KeyCode::Char('q') => self.reader = None,
            KeyCode::Char('/') => r.find = Some(Find { input: TextInput::default(), editing: true }),
            KeyCode::Char(c @ ('n' | 'N')) if r.find.is_some() => {
                r.jump(c == 'n', false);
            }
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
                        self.flash(tr!("Comment deleted", "Comentario borrado"));
                    }
                }
            }
            KeyCode::Char('s') => self.send_review(),
            KeyCode::Char('v') => self.accept(&key),
            KeyCode::Char('x') => self.mark_read(&key),
            KeyCode::Char(c @ '1'..='9') => {
                self.settle_until = 0;
                self.choose(&key, c as usize - '1' as usize);
            }
            _ => {}
        }
    }

    /// The reader's comments go back as feedback on the delivery it shows.
    fn send_review(&mut self) {
        let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else { return };
        let Some(review) = self.reviews.get(&key).filter(|rv| !rv.is_empty()) else {
            self.compose(Target::General { send: true }, "");
            return;
        };
        let text = review.feedback();
        let Some((presentation, targets, revision)) = self.feedback_subject(&key) else { return };
        self.feedback(key, presentation, targets, text, revision, None);
        self.reader = None;
    }

    pub fn search_key(&mut self, k: KeyEvent) {
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
                    self.agent_idx = 0;
                    self.agent_sel = None;
                }
            }
        }
    }

    pub fn agent_name(&self, id: Option<&AgentId>) -> String {
        let Some(id) = id else { return tr!("orchestrator", "orquestador").into() };
        self.domain.as_ref().and_then(|d| d.agents.iter().find(|a| &a.id == id)).map_or_else(|| id.0.chars().take(8).collect(), |a| a.name.clone())
    }
}

/// The daemon's reason an action is off, in the user's language where it is one it names.
pub fn unavailable(action: &AvailableAction) -> String {
    let reason = action.disabled_reason.clone().unwrap_or_default();
    match reason.as_str() {
        "already_read" => tr!("It is already read", "Ya está leído").into(),
        "task_not_active" => tr!("The task is no longer active", "La tarea ya no está activa").into(),
        "invalid_action_for_kind" => tr!("It is already answered", "Ya está contestada").into(),
        "forbidden" => tr!("It is not addressed to you", "No va dirigida a ti").into(),
        _ => reason,
    }
}

#[cfg(test)]
pub(crate) mod tests;
