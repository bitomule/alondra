mod draw;
mod dropped;
mod files;
mod input;
mod keys;
mod md;
mod tasks;
mod work;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::stdout;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use alondra_core::{Node, Question, QuestionOption, QuestionOrigin, Report, ReviewVerdict, now_millis};
use alondra_core::tasks::TaskState;
use anyhow::Result;
use crossterm::event::{self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use crate::client::{Client, FleetNode, Overview};
use crate::mcp::urlencode;
use crate::i18n;
use dropped::{Chip, Upload};
use input::TextInput;
use keys::{Cmd, Keymap, Motion};
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
    P(u64),
}

pub enum Item<'a> {
    Q(&'a Question),
    R(&'a Report),
    P(&'a crate::client::PrCard),
}

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
    pub chips: Vec<Chip>,
}

impl Review {
    pub fn is_empty(&self) -> bool {
        self.comments.is_empty() && self.general.trim().is_empty() && self.chips.is_empty()
    }

    pub fn count(&self) -> usize {
        self.comments.len() + usize::from(!self.general.trim().is_empty()) + self.chips.len()
    }

    fn feedback(&self) -> String {
        let pairs: Vec<(String, String)> = self.comments.iter().map(|c| (c.quote.clone(), c.text.clone())).collect();
        md::feedback(&pairs, &self.general, &crate::client::config().name)
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
    pub session: String,
    pub node: String,
    pub title: String,
    pub kind: &'static str,
    pub glyph: &'static str,
    pub color: ratatui::style::Color,
    pub approval: Option<Approval>,
    parts: Vec<Part>,
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

pub enum Target {
    PrChanges { id: u64, title: String },
    PrAsk { id: u64, title: String },
    Answer { id: u64, title: String, option: Option<String> },
    MoreContext { id: u64, title: String },
    ReplyReport { id: u64, session: String, name: String },
    Message { session: String, name: String },
    NewRequest,
    Comment { block: usize, quote: String },
    General { send: bool },
}

pub struct Composer {
    pub target: Target,
    pub input: TextInput,
    pub chips: Vec<Chip>,
    pub form: Option<work::OptionsForm>,
}

impl Target {
    fn draft_key(&self) -> Option<String> {
        match self {
            Target::Answer { id, .. } => Some(format!("answer:{id}")),
            Target::MoreContext { id, .. } => Some(format!("context:{id}")),
            Target::ReplyReport { id, .. } => Some(format!("reply:{id}")),
            Target::PrChanges { id, .. } => Some(format!("pr:{id}")),
            Target::PrAsk { id, .. } => Some(format!("pr-ask:{id}")),
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
    Group { name: String, total: usize },
    Node(usize),
}

enum Msg {
    Data(Box<Overview>),
    Offline,
    SyncError(String),
    RemoteDiff { id: u64, result: Result<Value, String> },
    Posted { key: Option<Key>, result: Result<String, String> },
    Closed { name: String, result: Result<Closed, String> },
    File { path: String, result: Result<files::FileBody, String> },
    Found { query: String, result: Result<(String, Vec<String>), String> },
    Uploaded { seq: u64, result: Result<String, String> },
    Preview { seq: u64, image: image::DynamicImage },
    Options { project: Option<String>, result: Result<(Vec<String>, Option<String>, Vec<alondra_core::options::Spec>, Option<String>), String> },
    Issues(Result<(Vec<alondra_core::issues::Issue>, Option<String>), String>),
    /// A task's history, or whether alondrad has no route for it.
    TaskHistory { id: String, since: i64, history: Result<(Vec<alondra_core::tasks::Transition>, Vec<alondra_core::Report>, Option<String>), bool> },
    /// A cancel or reopen: what to flash, or why it failed and whether alondrad has no route for it.
    TaskAction(Result<String, (String, bool)>),
    /// An archive of what is finished, told like a task action.
    TasksArchived(Result<String, (String, bool)>),
    /// What each thread of a node does, from its own route; none from an alondrad without it.
    NodeThreads { session: String, threads: Option<Vec<crate::client::Activity>> },
}

/// How a close that alondrad accepted ended: a background job is gone; an interactive session
/// only left the fleet and its terminal is still open.
#[derive(Debug, PartialEq)]
enum Closed {
    Removed,
    LeftFleet,
}

pub enum Flow {
    Go,
    Quit,
    Attach(String),
    Review { id: u64, number: u64, dir: std::path::PathBuf },
    EditSchedule(Option<Value>),
}

pub struct App {
    pub data: Overview,
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
    activity_told: i64,
    hidden: HashMap<Key, i64>,
    pub fleet_sel: Option<String>,
    fleet_idx: usize,
    pub fleet_scroll: usize,
    pub fleet_detail_scroll: usize,
    pending_close: Option<(String, i64)>,
    pending_merge: Option<(u64, alondra_core::pr::MergeMethod, i64)>,
    /// What each post would have called and flashed on success, since tests never reach alondrad.
    #[cfg(test)]
    posted: std::cell::RefCell<Vec<(Vec<String>, String)>>,
    #[cfg(test)]
    posted_keys: std::cell::RefCell<Vec<Option<Key>>>,
    pub search: Option<Search>,
    pub composer: Option<Composer>,
    pub reader: Option<Reader>,
    pub reviews: HashMap<Key, Review>,
    drafts: HashMap<String, (String, Vec<Chip>)>,
    chip_seq: u64,
    pub files: HashMap<String, Result<files::FileBody, String>>,
    /// How images are drawn in this terminal: Kitty graphics in Ghostty and Kitty, iTerm2 or sixel
    /// where supported, colour half-blocks elsewhere. Found once, when the TUI starts.
    pub picker: Option<ratatui_image::picker::Picker>,
    /// Each image, encoded for the terminal, by stored path.
    pub images: HashMap<String, ratatui_image::protocol::StatefulProtocol>,
    /// The preview of each image attached in the composer, by chip.
    pub previews: HashMap<u64, ratatui_image::protocol::StatefulProtocol>,
    loading: HashSet<String>,
    pub flash: Option<Flash>,
    pub help: bool,
    pub help_scroll: usize,
    pub help_view: View,
    pub focus: Pane,
    pub detail_view: View,
    pub fleet_view: View,
    pub fleet_zoom: bool,
    pub task_sel: Option<tasks::TaskKey>,
    task_idx: usize,
    pub task_scroll: usize,
    pub task_detail_scroll: usize,
    pub task_view: View,
    pub task_zoom: bool,
    /// Each task's history by id, with the `state_since` it was fetched at.
    pub histories: HashMap<String, (i64, Vec<alondra_core::tasks::Transition>)>,
    pub task_reports: HashMap<String, Vec<alondra_core::Report>>,
    pub task_briefs: HashMap<String, String>,
    history_asked: HashSet<(String, i64)>,
    history_missing: bool,
    pub task_actions_missing: bool,
    pub archive_missing: bool,
    /// The Tasks list shows its archived section open.
    pub archived_open: bool,
    /// The Tasks list shows its finished section open.
    pub finished_open: bool,
    /// What each thread of the selected node does, as its own route last said.
    pub threads: Option<(String, Vec<crate::client::Activity>)>,
    /// The node, the tick and when its threads were last asked for.
    threads_asked: Option<(String, u64, i64)>,
    pending_cancel: Option<(String, i64)>,
    pub cmdline: Option<TextInput>,
    keymap: Keymap,
    last_clock: i64,
    last_task_animation: i64,
    tx: SyncSender<Msg>,
}

const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;
const PREVIEW_PIXELS: u32 = 480;
const ACTIVITY_EVERY_MS: i64 = 60_000;
/// Threads move with every tool call, which does not move the tick: the open node asks again this often.
const THREADS_EVERY_MS: i64 = 10_000;

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Msg>(64);
    let sync_tx = tx.clone();
    std::thread::Builder::new().name("alondra-sync".into()).spawn(move || sync_loop(sync_tx))?;
    crate::attach::reset_terminal_modes();
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let mut app = App::new(tx);
    app.picker = Some(ratatui_image::picker::Picker::from_query_stdio().unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks()));
    let res = main_loop(&mut terminal, app, rx);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::attach::reset_terminal_modes();
    res
}

fn background_sync_error(error: &anyhow::Error, prefix: &str) -> Msg {
    if error.is::<crate::account::AccountBusy>() { Msg::Offline }
    else { Msg::SyncError(format!("{prefix}{error}")) }
}

fn sync_loop(tx: SyncSender<Msg>) {
    let quick = if crate::service_client::target().is_some() { Client::for_tui(Duration::from_secs(15)) } else { Client::new(Duration::from_secs(5)) };
    let waiter = if crate::service_client::target().is_some() { Client::for_tui(Duration::from_secs(70)) } else { Client::new(Duration::from_secs(70)) };
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
            Err(error) => {
                if was_online && crate::service_client::target().is_some() { let _ = tx.send(background_sync_error(&error, "")); }
                if was_online && tx.send(Msg::Offline).is_err() {
                    return;
                }
                was_online = false;
                std::thread::sleep(Duration::from_secs(2));
                continue;
            }
        };
        if let Err(error) = waiter.get::<Value>(&format!("/v1/changes?after={tick}&wait=50")) {
            if crate::service_client::target().is_some() { let _ = tx.send(background_sync_error(&error, "Remote change wait failed: ")); }
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

/// Back from a node's session: its last frame is still on screen and ratatui does not know,
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
            app.note_activity();
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => match app.on_key(k) {
                        Flow::Quit => return Ok(()),
                        Flow::Attach(short) => {
                            if crate::service_client::target().is_some() {
                                app.fail(tr!("Remote terminal attachment is unavailable; open the terminal on the destination Mac", "El terminal remoto no está disponible; ábrelo en el Mac de destino"));
                                break;
                            }
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            let res = crate::attach::attach(&short, crate::attach::Over::Tui);
                            crossterm::terminal::enable_raw_mode()?;
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            if let Err(e) = res {
                                app.fail(trf!("Could not open its terminal: {e}", "No pude abrir su terminal: {e}"));
                            }
                            repaint(terminal, &mut app)?;
                            break;
                        }
                        Flow::Review { id, number, dir } => {
                            if crate::service_client::target().is_some() {
                                app.fail(tr!("Interactive tuicr review requires the destination Mac; use its PR link to review remotely", "La revisión interactiva con tuicr requiere el Mac de destino; usa el enlace del PR para revisar en remoto"));
                                break;
                            }
                            draw(terminal, &mut app)?;
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            let res = crate::attach::run_tool("tuicr", &["--stdout", "--no-update-check", "pr", &number.to_string()], &dir, crate::attach::Over::Tui);
                            crossterm::terminal::enable_raw_mode()?;
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            app.reviewed(id, number, res);
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
                                Ok(Some(())) => { app.flash(tr!("Schedule saved", "Programación guardada")); app.reload_schedules(); }
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

fn edit_schedule(existing: Option<Value>) -> Result<Option<()>> {
    let path = std::env::temp_dir().join(format!("alondra-schedule-{}-{}.json", std::process::id(), now_millis()));
    let mut body = existing.clone().unwrap_or_else(|| json!({
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
    body = serde_json::from_slice(&edited).map_err(|e| anyhow::anyhow!("Invalid schedule JSON: {e}"))?;
    let client = Client::for_tui(Duration::from_secs(15));
    if let Some(id) = existing.as_ref().and_then(|v| v["id"].as_str()) {
        client.put(&format!("/v1/schedules/{}", urlencode(id)), &body)?;
    } else {
        client.post("/v1/schedules", &body)?;
    }
    Ok(Some(()))
}

pub fn who(name: &Option<String>, session: &str) -> String {
    name.clone().unwrap_or_else(|| session.chars().take(8).collect())
}

pub fn said(n: &Node) -> String {
    n.last_message.clone().or(n.daemon_needs.clone()).or(n.daemon_detail.clone()).unwrap_or_default()
}

fn approval_of(q: &Question) -> Option<Approval> {
    let find = |pred: &dyn Fn(&str) -> bool| q.options.iter().find(|o| pred(&o.label.to_lowercase())).map(|o| o.label.clone());
    let approve = find(&|l| l.starts_with("aprob") || l.starts_with("acept") || l.starts_with("approv") || l.starts_with("accept"));
    let changes = find(&|l| ["cambi", "pedir", "rechaz", "change", "reject"].iter().any(|w| l.contains(w)));
    (q.origin == QuestionOrigin::Plan || (approve.is_some() && changes.is_some())).then(|| Approval {
        approve: approve.or_else(|| q.options.first().map(|o| o.label.clone())),
        changes: changes.or_else(|| q.options.get(1).map(|o| o.label.clone())),
    })
}

/// Options whose description asks for a reason open the composer instead of answering at once.
fn needs_text(o: &QuestionOption) -> bool {
    o.description.as_deref().is_some_and(|d| d.trim_start().to_lowercase().starts_with("write"))
}

/// A tuicr export carries numbered comments anchored to files (`1. \`src/a.rs:42\` - …`).
fn has_comments(export: &str) -> bool {
    export.lines().any(|l| {
        let t = l.trim_start();
        t.split_once(". ").is_some_and(|(n, rest)| n.parse::<u32>().is_ok() && rest.starts_with('`'))
    })
}

/// Where to open a PR's repo: the node's own checkout, else `~/Projects/<name>`.
/// The Mac's default app for a URL or file, through `open`. Tests never open one.
fn open_with_mac(target: &str) {
    if cfg!(test) {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open").arg(target).spawn();
}

/// Download remote attachments before invoking a local application. Never pass a remote
/// filesystem path to `open`, even when a same-named file happens to exist locally.
fn open_remote_file(path: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let bytes = Client::for_tui(Duration::from_secs(120)).get_bytes(&format!("/v1/file?path={}", urlencode(path)))?;
    let suffix: String = crate::service_client::random::<16>()?.iter().map(|b| format!("{b:02x}")).collect();
    let dir = std::env::temp_dir().join(format!("alondra-attachment-{suffix}"));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let name = std::path::Path::new(path).file_name().filter(|n| !n.is_empty()).unwrap_or(std::ffi::OsStr::new("attachment"));
    let local = dir.join(name);
    let mut file = std::fs::OpenOptions::new().create_new(true).write(true).mode(0o600).open(&local)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    open_with_mac(&local.to_string_lossy());
    Ok(())
}

fn clone_of(node_cwd: Option<&str>, repo: &str) -> Option<std::path::PathBuf> {
    let is_repo = |p: &std::path::Path| p.join(".git").exists();
    if let Some(cwd) = node_cwd.map(std::path::PathBuf::from).filter(|p| is_repo(p)) {
        return Some(cwd);
    }
    let name = repo.rsplit('/').next()?;
    let home = std::env::var("HOME").ok()?;
    Some(std::path::Path::new(&home).join("Projects").join(name)).filter(|p| is_repo(p))
}

fn answer_body(text: &str, option: Option<&str>, attachments: &[String]) -> Value {
    json!({"text": text, "option": option, "device": "tui", "attachments": attachments})
}

fn stored_ids(chips: &[Chip]) -> Vec<String> {
    chips.iter().filter_map(|c| match &c.upload {
        Upload::Stored(id) => Some(id.clone()),
        Upload::Sending => None,
    }).collect()
}

/// What each thread of the node does now, from its light route; an alondrad without that route
/// answers from the node's detail, cut to one timeline event. None when neither answers.
fn node_threads(get: impl Fn(&str) -> Result<Value>, session: &str) -> Option<Vec<crate::client::Activity>> {
    let key = urlencode(session);
    let (answer, at) = match get(&format!("/v1/nodes/{key}/activity")) {
        Err(e) if e.downcast_ref::<crate::client::NotFound>().is_some() => (get(&format!("/v1/nodes/{key}?limit=1")), "/node/activity"),
        light => (light, "/activity"),
    };
    answer.ok().and_then(|v| serde_json::from_value(v.pointer(at)?.clone()).ok())
}

impl App {
    fn new(tx: SyncSender<Msg>) -> Self {
        App {
            data: Overview::default(),
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
            activity_told: 0,
            hidden: HashMap::new(),
            fleet_sel: None,
            fleet_idx: 0,
            fleet_scroll: 0,
            fleet_detail_scroll: 0,
            pending_close: None,
            pending_merge: None,
            #[cfg(test)]
            posted: Default::default(),
            #[cfg(test)]
            posted_keys: Default::default(),
            search: None,
            composer: None,
            reader: None,
            reviews: HashMap::new(),
            drafts: HashMap::new(),
            chip_seq: 0,
            previews: HashMap::new(),
            files: HashMap::new(),
            picker: cfg!(test).then(ratatui_image::picker::Picker::halfblocks),
            images: HashMap::new(),
            loading: HashSet::new(),
            flash: None,
            help: false,
            help_scroll: 0,
            help_view: View::default(),
            focus: Pane::List,
            detail_view: View::default(),
            fleet_view: View::default(),
            fleet_zoom: false,
            task_sel: None,
            task_idx: 0,
            task_scroll: 0,
            task_detail_scroll: 0,
            task_view: View::default(),
            task_zoom: false,
            histories: HashMap::new(),
            task_reports: HashMap::new(),
            task_briefs: HashMap::new(),
            history_asked: HashSet::new(),
            history_missing: false,
            task_actions_missing: false,
            archive_missing: false,
            archived_open: false,
            finished_open: false,
            threads: None,
            threads_asked: None,
            pending_cancel: None,
            cmdline: None,
            keymap: Keymap::default(),
            last_clock: now_millis(),
            last_task_animation: now_millis() / 500,
            tx,
        }
    }

    /// Questions first, then each PR card followed by the questions and reports that belong to it,
    /// then reading.
    pub fn inbox_keys(&self) -> Vec<Key> {
        let q = self.data.open_questions.iter().filter(|q| self.card_of(q.pr_id).is_none()).map(|q| Key::Q(q.id));
        let mut prs: Vec<&crate::client::PrCard> = self.data.prs.iter().collect();
        prs.sort_by_key(|p| p.train_place());
        let p = prs.into_iter().flat_map(|p| {
            std::iter::once(Key::P(p.pr.id)).chain(self.asked_on(p.pr.id).map(|q| Key::Q(q.id))).chain(self.reported_on(p.pr.id).map(|r| Key::R(r.id)))
        });
        let r = self.data.news.iter().filter(|r| self.card_of(r.pr_id).is_none()).map(|r| Key::R(r.id));
        q.chain(p).chain(r).filter(|k| !self.hidden.contains_key(k) && self.parent(*k).is_none_or(|p| !self.hidden.contains_key(&Key::P(p)))).collect()
    }

    /// The inbox as the user counts it: a PR card and the questions inside it are one thing.
    pub fn inbox_count(&self) -> usize {
        self.inbox_keys().into_iter().filter(|k| self.parent(*k).is_none()).count()
    }

    /// The PR card a question or report is drawn inside, when that card is in the inbox.
    fn card_of(&self, pr_id: Option<u64>) -> Option<u64> {
        pr_id.filter(|id| self.data.prs.iter().any(|p| p.pr.id == *id))
    }

    pub fn parent(&self, key: Key) -> Option<u64> {
        match key {
            Key::Q(id) => self.data.open_questions.iter().find(|q| q.id == id).and_then(|q| self.card_of(q.pr_id)),
            Key::R(id) => self.data.news.iter().find(|r| r.id == id).and_then(|r| self.card_of(r.pr_id)),
            Key::P(_) => None,
        }
    }

    pub fn asked_on(&self, pr: u64) -> impl Iterator<Item = &crate::client::OpenQuestion> {
        self.data.open_questions.iter().filter(move |q| self.card_of(q.pr_id) == Some(pr))
    }

    pub fn reported_on(&self, pr: u64) -> impl Iterator<Item = &crate::client::NewsReport> {
        self.data.news.iter().filter(move |r| self.card_of(r.pr_id) == Some(pr))
    }

    pub fn item(&self, key: Key) -> Option<Item<'_>> {
        match key {
            Key::Q(id) => self.data.open_questions.iter().find(|q| q.id == id).map(|q| Item::Q(q)),
            Key::R(id) => self.data.news.iter().find(|r| r.id == id).map(|r| Item::R(&r.report)),
            Key::P(id) => self.data.prs.iter().find(|p| p.pr.id == id).map(Item::P),
        }
    }

    /// What tuicr left on stdout when David quit it: his comments go to the node as a change
    /// request; without comments nothing is sent.
    fn reviewed(&mut self, id: u64, number: u64, res: Result<String>) {
        match res {
            Ok(text) if has_comments(&text) => {
                self.post(Some(Key::P(id)), vec![(format!("/v1/prs/{id}/changes"), json!({"text": text.trim()}))], trf!("Review of PR #{number} sent to the node", "Revisión del PR #{number} enviada al nodo"));
            }
            Ok(_) => self.flash(trf!("PR #{number} reviewed without comments", "PR #{number} revisado sin comentarios")),
            Err(e) if e.to_string().contains("No such file") || e.to_string().contains("could not launch") || e.to_string().contains("no pude lanzar") => {
                self.fail(tr!("tuicr is not installed: brew install tuicr", "tuicr no está instalado: brew install tuicr"));
            }
            Err(e) => self.fail(trf!("tuicr: {e}", "tuicr: {e}")),
        }
    }

    fn selected_pr(&self) -> Option<crate::client::PrCard> {
        match self.sel.and_then(|k| self.item(k)) {
            Some(Item::P(p)) => Some(p.clone()),
            _ => None,
        }
    }

    /// Merging cannot be undone, so a method key takes a second press within a few seconds.
    fn merge_pr(&mut self, pr: &crate::client::PrCard, i: usize) {
        let method = match pr.actions().get(i) {
            Some(crate::client::PrAction::Merge(m)) => *m,
            Some(crate::client::PrAction::ResolveConflicts) => {
                let number = pr.pr.number;
                self.post(
                    Some(Key::P(pr.pr.id)),
                    vec![(format!("/v1/prs/{}/resolve-conflicts", pr.pr.id), json!({}))],
                    trf!("Asked its node to resolve the conflicts of PR #{number}", "Pedido a su nodo que resuelva los conflictos del PR #{number}"),
                );
                self.hide(Key::P(pr.pr.id));
                return;
            }
            None => {
                let n = i + 1;
                self.fail(trf!("This PR has no option {n}", "Este PR no tiene opción {n}"));
                return;
            }
        };
        if let Some(reason) = pr.merge_blocked() {
            self.fail(reason);
            return;
        }
        let now = now_millis();
        let label = i18n::merge_method(method);
        let number = pr.pr.number;
        if self.pending_merge.is_some_and(|(id, m, until)| id == pr.pr.id && m == method && now < until) {
            self.pending_merge = None;
            self.post(Some(Key::P(pr.pr.id)), vec![(format!("/v1/prs/{}/merge", pr.pr.id), json!({"method": method}))], trf!("PR #{number} merged ({label})", "PR #{number} fusionado ({label})"));
            self.hide(Key::P(pr.pr.id));
        } else {
            self.pending_merge = Some((pr.pr.id, method, now + 4000));
            let n = i + 1;
            self.flash(trf!("Press {n} again to merge PR #{number} ({label})", "Pulsa {n} otra vez para fusionar el PR #{number} ({label})"));
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

    /// Every call to alondrad runs here. Tests never reach the user's live alondrad.
    fn spawn(&self, job: impl FnOnce(&SyncSender<Msg>) + Send + 'static) {
        if cfg!(test) {
            return;
        }
        let tx = self.tx.clone();
        std::thread::spawn(move || job(&tx));
    }

    /// Tells alondrad the user is at the TUI, at most once a minute, so it holds back pushes.
    fn note_activity(&mut self) {
        let now = now_millis();
        if now - self.activity_told < ACTIVITY_EVERY_MS {
            return;
        }
        self.activity_told = now;
        self.spawn(|_| {
            let _ = Client::for_tui(Duration::from_secs(5)).post("/v1/activity", &json!({"client": "tui"}));
        });
    }

    fn post(&self, key: Option<Key>, calls: Vec<(String, Value)>, ok: String) {
        #[cfg(test)]
        self.posted.borrow_mut().push((calls.iter().map(|(p, _)| p.clone()).collect(), ok.clone()));
        #[cfg(test)]
        self.posted_keys.borrow_mut().push(key);
        self.spawn(move |tx| {
            let client = Client::for_tui(Duration::from_secs(15));
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
            let result = Client::for_tui(Duration::from_secs(10))
                .get_bytes(&format!("/v1/file?path={}", urlencode(&path)))
                .map(files::FileBody::classify)
                .map_err(|e| e.to_string());
            let _ = tx.send(Msg::File { path, result });
        });
    }

    fn reconcile(&mut self) {
        let now = now_millis();
        let present: HashSet<Key> =
            self.data.open_questions.iter().map(|q| Key::Q(q.id)).chain(self.data.news.iter().map(|r| Key::R(r.id))).chain(self.data.prs.iter().map(|p| Key::P(p.pr.id))).collect();
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
            Some(Item::P(_)) | None => vec![],
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
        self.reconcile_tasks();
        self.load_threads();
    }

    /// alondrad came back, maybe a newer one: the routes an older one lacked are tried again.
    fn forget_missing_routes(&mut self) {
        self.task_actions_missing = false;
        self.archive_missing = false;
        self.history_missing = false;
        self.history_asked.clear();
    }

    /// Asks the selected node's own route what each of its threads does, which the overview does
    /// not carry: when the node is opened, when the tick moves and every `THREADS_EVERY_MS`.
    fn load_threads(&mut self) {
        if self.screen != Screen::Fleet {
            return;
        }
        let Some(session) = self.fleet_sel.clone() else { return };
        let now = now_millis();
        if self.threads_asked.as_ref().is_some_and(|(s, tick, at)| *s == session && *tick == self.data.tick && now - at < THREADS_EVERY_MS) {
            return;
        }
        self.threads_asked = Some((session.clone(), self.data.tick, now));
        self.spawn(move |tx| {
            let client = Client::for_tui(Duration::from_secs(10));
            let threads = node_threads(|path| client.get::<Value>(path), &session);
            let _ = tx.send(Msg::NodeThreads { session, threads });
        });
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::Data(o) => {
                if self.offline || o.tick < self.data.tick {
                    self.forget_missing_routes();
                }
                self.data = *o;
                self.connected = true;
                self.loaded = true;
                self.offline = false;
            }
            Msg::SyncError(error) => self.fail(error),
            Msg::RemoteDiff { id, result } => match result {
                Err(error) => self.fail(error),
                Ok(value) => {
                    if self.sel == Some(Key::P(id)) {
                        self.open_reader();
                        if let Some(reader) = self.reader.as_mut() {
                            let diff = value["diff"].as_str().unwrap_or("");
                            let head = value["head_sha"].as_str().unwrap_or("unknown");
                            reader.parts.push(Part::Text(format!("## Diff at {head}\n\n```diff\n{diff}\n```")));
                            if value["truncated"] == true { reader.parts.push(Part::Text("Diff truncated by destination; open the PR for the complete diff.".into())); }
                        }
                        self.rebuild_reader();
                    }
                }
            },
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
                self.fail(trf!("Not sent: {e}", "No se envió: {e}"));
            }
            Msg::Closed { name, result } => self.closed(&name, result),
            Msg::Uploaded { seq, result } => self.uploaded(seq, result),
            Msg::Preview { seq, image } => {
                if let Some(p) = self.picker.as_ref() {
                    self.previews.insert(seq, p.new_resize_protocol(image));
                }
            }
            Msg::File { path, result } => {
                self.loading.remove(&path);
                if let (Ok(files::FileBody::Image(img)), Some(picker)) = (&result, &self.picker) {
                    self.images.insert(path.clone(), picker.new_resize_protocol((**img).clone()));
                }
                self.files.insert(path.clone(), result);
                if self.reader.as_ref().is_some_and(|r| r.parts.iter().any(|p| matches!(p, Part::File { path: fp, .. } if *fp == path))) {
                    self.rebuild_reader();
                }
            }
            Msg::Options { project, result } => match result {
                Ok((projects, suggested, specs, error)) => {
                    if let Some(f) = self.composer.as_mut().and_then(|c| c.form.as_mut()) {
                        f.suggested = suggested;
                        f.loaded(project, projects, specs, error);
                    }
                }
                Err(e) => self.fail(trf!("Could not load the options: {e}", "No se pudieron cargar las opciones: {e}")),
            },
            Msg::Issues(result) => {
                if let Some(f) = self.composer.as_mut().and_then(|c| c.form.as_mut()) {
                    f.issues_loaded(result);
                }
            }
            Msg::TaskHistory { id, since, history } => self.history_loaded(id, since, history),
            Msg::TaskAction(result) => self.task_action_done(result),
            Msg::TasksArchived(result) => self.archive_done(result),
            Msg::NodeThreads { session, threads: Some(threads) } => self.threads = Some((session, threads)),
            Msg::NodeThreads { threads: None, .. } => {}
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
        if self.screen == Screen::Tasks && self.data.tasks.iter().any(|task| task.state == TaskState::Running && task.archived_at.is_none()) && now / 500 != self.last_task_animation {
            self.last_task_animation = now / 500;
            dirty = true;
        }
        if self.hidden.values().any(|at| now - at >= HIDE_GRACE_MS) {
            dirty = true;
        }
        self.load_threads();
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
                let result = Client::for_tui(Duration::from_secs(120))
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
        if self.composer.is_some()
            && let Some(files) = dropped::dropped_files(s)
        {
            for f in files {
                self.attach(f);
            }
        } else if let Some(c) = self.composer.as_mut() {
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
                KeyCode::Char('s') if self.screen == Screen::Tasks => { self.schedules_open = true; self.reload_schedules(); },
                KeyCode::Char('M') => self.message_orchestrator(),
                _ => {
                    return match self.screen {
                        Screen::Inbox => self.inbox_key(k),
                        Screen::Tasks => self.tasks_key(k),
                        Screen::Fleet => self.fleet_key(k),
                    };
                }
            },
            _ => match self.screen {
                Screen::Inbox => self.inbox_cmd(cmd),
                Screen::Tasks => self.tasks_cmd(cmd),
                Screen::Fleet => self.fleet_cmd(cmd),
            },
        }
        Flow::Go
    }

    /// Inbox, Tasks, Fleet and round again.
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
        self.fleet_zoom = false;
        self.task_zoom = false;
        self.threads_asked = None;
        self.reconcile_tasks();
        self.load_threads();
    }

    fn reload_schedules(&mut self) {
        match Client::for_tui(Duration::from_secs(5)).get::<Value>("/v1/schedules") {
            Ok(value) => {
                self.schedules = value["schedules"].as_array().cloned().unwrap_or_default();
                self.schedule_idx = self.schedule_idx.min(self.schedules.len().saturating_sub(1));
            }
            Err(e) => self.fail(format!("Schedules: {e}")),
        }
    }

    fn schedule_key(&mut self, k: KeyEvent) -> Flow {
        let selected = self.schedules.get(self.schedule_idx).cloned();
        match k.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('s') => self.schedules_open = false,
            KeyCode::Char('j') | KeyCode::Down => { self.schedule_idx = (self.schedule_idx + 1).min(self.schedules.len().saturating_sub(1)); self.schedule_delete = None; },
            KeyCode::Char('k') | KeyCode::Up => { self.schedule_idx = self.schedule_idx.saturating_sub(1); self.schedule_delete = None; },
            KeyCode::Char('n') => return Flow::EditSchedule(None),
            KeyCode::Char('e') => if let Some(value) = selected { return Flow::EditSchedule(Some(value)); },
            KeyCode::Char('r') => self.reload_schedules(),
            KeyCode::Char('p') => if let Some(mut value) = selected {
                let id = value["id"].as_str().unwrap_or("").to_owned();
                let enabled = value["enabled"].as_bool().unwrap_or(false);
                value["enabled"] = json!(!enabled);
                match Client::for_tui(Duration::from_secs(15)).put(&format!("/v1/schedules/{}", urlencode(&id)), &value) {
                    Ok(_) => self.reload_schedules(), Err(e) => self.fail(e.to_string()),
                }
            },
            KeyCode::Enter => if let Some(value) = selected {
                let id = value["id"].as_str().unwrap_or("");
                match Client::for_tui(Duration::from_secs(15)).post(&format!("/v1/schedules/{}/run", urlencode(id)), &json!({})) {
                    Ok(_) => { self.flash(tr!("Task created", "Tarea creada")); self.reload_schedules(); },
                    Err(e) => { self.fail(e.to_string()); self.reload_schedules(); },
                }
            },
            KeyCode::Char('d') => if let Some(value) = selected {
                let id = value["id"].as_str().unwrap_or("");
                let now = now_millis();
                if self.schedule_delete.as_ref().is_some_and(|(pending, until)| pending == id && now < *until) {
                    self.schedule_delete = None;
                    match Client::for_tui(Duration::from_secs(15)).delete(&format!("/v1/schedules/{}", urlencode(id))) {
                        Ok(()) => self.reload_schedules(), Err(e) => self.fail(e.to_string()),
                    }
                } else {
                    self.schedule_delete = Some((id.to_string(), now + 4000));
                    self.flash(tr!("Press d again to delete this schedule", "Pulsa d otra vez para borrar esta programación"));
                }
            },
            _ => {}
        }
        Flow::Go
    }

    fn message_orchestrator(&mut self) {
        match self.orchestrator() {
            Some(n) => {
                let session = n.session_id.clone();
                self.compose(Target::Message { session, name: tr!("the orchestrator", "el orquestador").into() }, "");
            }
            None => self.fail(tr!("The orchestrator is not connected to Alondra yet", "El orquestador todavía no está conectado a Alondra")),
        }
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
            "fleet" | "f" => {
                self.reader = None;
                self.show(Screen::Fleet);
            }
            n if n.parse::<usize>().is_ok_and(|n| n > 0) && self.reader.is_none() => {
                let i = n.parse::<isize>().unwrap_or(1) - 1;
                match self.screen {
                    Screen::Inbox => self.move_inbox(i),
                    Screen::Tasks => self.move_tasks(i),
                    Screen::Fleet => self.move_fleet(i),
                }
            }
            other => self.fail(trf!("Not a command: {other}  (try :q, :help, :inbox, :tasks, :fleet, :<number>)", "No es un comando: {other}  (prueba :q, :help, :inbox, :tasks, :fleet, :<número>)")),
        }
        Flow::Go
    }

    fn compose(&mut self, target: Target, prefill: &str) {
        let (text, chips) = target.draft_key().and_then(|k| self.drafts.remove(&k)).unwrap_or_else(|| (prefill.to_string(), vec![]));
        let form = matches!(target, Target::NewRequest).then(work::OptionsForm::default);
        self.composer = Some(Composer { target, input: TextInput::with(&text), chips, form });
        if self.composer.as_ref().is_some_and(|c| c.form.is_some()) {
            self.load_options(None);
        }
    }

    /// Asks alondrad for the open issues, which it keeps for ten minutes unless `refresh`.
    fn load_issues(&self, refresh: bool) {
        self.spawn(move |tx| {
            let path = if refresh { "/v1/issues?refresh=1" } else { "/v1/issues" };
            let result = Client::for_tui(Duration::from_secs(40)).get::<Value>(path).map_err(|e| e.to_string()).and_then(|v| {
                let issues = serde_json::from_value(v["issues"].clone()).map_err(|e| e.to_string())?;
                Ok((issues, v["error"].as_str().map(str::to_string)))
            });
            let _ = tx.send(Msg::Issues(result));
        });
    }

    /// Fetches the options for "New" once, when it opens or its project changes.
    fn load_options(&self, project: Option<String>) {
        self.spawn(move |tx| {
            let client = Client::for_tui(Duration::from_secs(10));
            let query = project.as_deref().map(|p| format!("?project={}", urlencode(p))).unwrap_or_default();
            let result = client
                .get::<Value>(&format!("/v1/options{query}"))
                .map_err(|e| e.to_string())
                .and_then(|o| {
                    let specs = serde_json::from_value(o["options"].clone()).map_err(|e| e.to_string())?;
                    let listed = client.get::<Value>("/v1/projects").ok();
                    let projects = listed.as_ref().and_then(|p| p["projects"].as_array().cloned()).unwrap_or_default();
                    let names = projects.iter().filter_map(|p| p["name"].as_str().map(str::to_string)).collect();
                    let root = listed.as_ref().and_then(|p| p["root"].as_str().map(std::path::PathBuf::from));
                    let here = std::env::current_dir().ok();
                    let suggested = root.zip(here).and_then(|(root, here)| alondra_core::options::project_of(&root, &here));
                    Ok((names, suggested, specs, o["error"].as_str().map(str::to_string)))
                });
            let _ = tx.send(Msg::Options { project, result });
        });
    }

    fn cancel_composer(&mut self) {
        let Some(c) = self.composer.take() else { return };
        let text = c.input.text();
        if let Some(k) = c.target.draft_key()
            && (!text.trim().is_empty() || !c.chips.is_empty())
        {
            self.drafts.insert(k, (text, c.chips));
            self.flash(tr!("Draft saved: open it again to pick up where you left off", "Borrador guardado: vuelve a abrirlo y sigue donde lo dejaste"));
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
                self.fail(tr!("That node is no longer in the fleet", "Ese nodo ya no está en la flota"));
                Flow::Go
            }
        }
    }

    fn inbox_cmd(&mut self, cmd: Cmd) {
        let i = self.sel_idx as isize;
        let detail = self.focus == Pane::Detail && self.detail_view.shown;
        match cmd {
            Cmd::Move(m) if detail => self.detail_scroll = keys::scroll(self.detail_scroll, m, self.detail_view.h, self.detail_view.total),
            Cmd::Move(Motion::Down) => self.move_inbox(i + 1),
            Cmd::Move(Motion::Up) => self.move_inbox(i - 1),
            Cmd::Move(Motion::Top) => self.move_inbox(0),
            Cmd::Move(Motion::Bottom) => self.move_inbox(isize::MAX),
            Cmd::Move(m) => self.detail_scroll = keys::scroll(self.detail_scroll, m, self.detail_view.h, self.detail_view.total),
            Cmd::Right if self.detail_view.shown && self.sel.is_some() => self.focus = Pane::Detail,
            Cmd::Right | Cmd::Expand => self.open_reader(),
            Cmd::Left | Cmd::Back => self.focus = Pane::List,
            Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    fn inbox_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Char('J') => self.detail_scroll = keys::by(self.detail_scroll, 5, self.detail_view.h, self.detail_view.total),
            KeyCode::Char('K') => self.detail_scroll = keys::by(self.detail_scroll, -5, self.detail_view.h, self.detail_view.total),
            KeyCode::Enter => self.open_reader(),
            KeyCode::Char('a') => {
                let session = match self.sel.and_then(|k| self.item(k)) {
                    Some(Item::Q(q)) => q.session_id.clone(),
                    Some(Item::R(r)) => r.session_id.clone(),
                    Some(Item::P(p)) => p.pr.session_id.clone(),
                    None => return Flow::Go,
                };
                return self.attach_to(&session);
            }
            KeyCode::Char('r') => {
                if let Some(q) = self.selected_question() {
                    self.compose(Target::Answer { id: q.id, title: q.title, option: None }, "");
                } else if let Some(r) = self.selected_report() {
                    let name = who(&r.node_name, &r.session_id);
                    self.compose(Target::ReplyReport { id: r.id, session: r.session_id, name }, "");
                } else if let Some(p) = self.selected_pr() {
                    self.compose(Target::PrChanges { id: p.pr.id, title: p.pr.title }, "");
                }
            }
            KeyCode::Char('p') => {
                if let Some(p) = self.selected_pr() {
                    self.compose(Target::PrAsk { id: p.pr.id, title: p.pr.title }, "");
                }
            }
            KeyCode::Char('v') if self.selected_report().is_some_and(|r| !alondra_core::artifact::in_report(&r).is_empty()) => {
                if let Some(r) = self.selected_report() {
                    self.review(&r, ReviewVerdict::Approved);
                }
            }
            KeyCode::Char('v') => {
                let attachments = match self.sel.and_then(|k| self.item(k)) {
                    Some(Item::Q(q)) => q.attachments.clone(),
                    Some(Item::R(r)) => r.attachments.clone(),
                    _ => vec![],
                };
                let shown_elsewhere: Vec<String> = attachments
                    .iter()
                    .filter_map(|a| a.stored_path.clone())
                    .filter(|p| matches!(self.files.get(p), Some(Ok(files::FileBody::Image(_) | files::FileBody::Binary { .. }))))
                    .collect();
                if shown_elsewhere.is_empty() {
                    self.fail(tr!("No image or file to open here", "Aquí no hay imagen ni fichero que abrir"));
                } else {
                    if crate::service_client::target().is_some() {
                        for path in shown_elsewhere {
                            if let Err(error) = open_remote_file(&path) { self.fail(error.to_string()); return Flow::Go; }
                        }
                    } else { shown_elsewhere.iter().for_each(|p| open_with_mac(p)); }
                    self.flash(tr!("Opened with its app", "Abierto con su aplicación"));
                }
            }
            KeyCode::Char('b') => {
                if let Some(r) = self.selected_report() {
                    let artifacts = alondra_core::artifact::in_report(&r);
                    if artifacts.is_empty() {
                        self.fail(tr!("This report has no artifact to open", "Este informe no tiene ningún artefacto que abrir"));
                    } else {
                        artifacts.iter().for_each(|url| open_with_mac(url));
                        self.flash(tr!("Opened in the browser", "Abierto en el navegador"));
                    }
                }
            }
            KeyCode::Char('w') => {
                if let Some(p) = self.selected_pr() {
                    let _ = std::process::Command::new("open").arg(&p.pr.url).spawn();
                }
            }
            KeyCode::Char('d') => {
                if crate::service_client::target().is_some() {
                    if let Some(pr) = self.selected_pr() {
                        let id = pr.pr.id;
                        let tx = self.tx.clone();
                        std::thread::spawn(move || {
                            let result = Client::for_tui(Duration::from_secs(90)).get::<Value>(&format!("/v1/prs/{id}/diff-snapshot")).map_err(|e| e.to_string());
                            let _ = tx.send(Msg::RemoteDiff { id, result });
                        });
                        self.flash(tr!("Loading the destination's PR diff…", "Cargando el diff del PR desde el destino…"));
                    }
                    return Flow::Go;
                }
                if let Some(p) = self.selected_pr() {
                    let cwd = self.node(&p.pr.session_id).and_then(|n| n.cwd.clone());
                    match clone_of(cwd.as_deref(), &p.pr.repo) {
                        Some(dir) => {
                            let number = p.pr.number;
                            self.flash(trf!("Opening PR #{number} in tuicr…", "Abriendo el PR #{number} en tuicr…"));
                            return Flow::Review { id: p.pr.id, number, dir };
                        }
                        None => {
                            let repo = p.pr.repo.clone();
                            self.fail(trf!("No local copy of {repo} to review it in", "No hay copia local de {repo} donde revisarlo"));
                        }
                    }
                }
            }
            KeyCode::Char('c') => {
                if let Some(q) = self.selected_question() {
                    self.compose(Target::MoreContext { id: q.id, title: q.title }, "");
                } else if let Some(r) = self.selected_report() {
                    self.review(&r, ReviewVerdict::Comments);
                }
            }
            KeyCode::Char('x') => {
                if let Some(p) = self.selected_pr() {
                    let number = p.pr.number;
                    self.post(Some(Key::P(p.pr.id)), vec![(format!("/v1/prs/{}/dismiss", p.pr.id), json!({}))], trf!("PR #{number} set aside", "PR #{number} apartado"));
                    self.hide(Key::P(p.pr.id));
                } else if let Some(r) = self.selected_report() {
                    self.mark_read(&r);
                }
            }
            KeyCode::Char(c @ '1'..='9') if k.modifiers.is_empty() => {
                if let Some(q) = self.selected_question() {
                    self.choose(&q, c as usize - '1' as usize);
                } else if let Some(p) = self.selected_pr() {
                    self.merge_pr(&p, c as usize - '1' as usize);
                }
            }
            _ => {}
        }
        Flow::Go
    }

    fn choose(&mut self, q: &Question, i: usize) {
        if now_millis() < self.settle_until {
            self.fail(tr!("The selection just changed: check which one it is and press again", "La selección acaba de cambiar: mira cuál es y vuelve a pulsar"));
            return;
        }
        let Some(opt) = q.options.get(i) else {
            let n = i + 1;
            self.fail(trf!("This question has no option {n}", "Esta pregunta no tiene opción {n}"));
            return;
        };
        if needs_text(opt) {
            self.compose(Target::Answer { id: q.id, title: q.title.clone(), option: Some(opt.label.clone()) }, "");
            return;
        }
        let who = who(&q.node_name, &q.session_id);
        self.post(
            Some(Key::Q(q.id)),
            vec![(format!("/v1/questions/{}/answer", q.id), answer_body(&opt.label, Some(&opt.label), &[]))],
            format!("«{}» → {who}", opt.label),
        );
        self.reader = None;
        self.hide(Key::Q(q.id));
    }

    /// Closing a node cannot be undone, so it takes a second press within a few seconds.
    fn confirm_close(&mut self, session: &str, name: &str) -> bool {
        let now = now_millis();
        if self.pending_close.as_ref().is_some_and(|(s, until)| s == session && now < *until) {
            self.pending_close = None;
            return true;
        }
        self.pending_close = Some((session.to_string(), now + 4000));
        self.fail(trf!("Press x again to close {name} (claude rm, never discards work)", "Pulsa x otra vez para cerrar {name} (claude rm, nunca descarta trabajo)"));
        false
    }

    fn mark_read(&mut self, r: &Report) {
        let done = { let t = md::truncate(&r.title, 60); trf!("Read: {t}", "Leído: {t}") };
        self.mark_read_saying(r, done);
    }

    /// `done` is what the flash says once alondrad has it, which replaces whatever the flash
    /// said in between.
    fn mark_read_saying(&mut self, r: &Report, done: String) {
        self.post(Some(Key::R(r.id)), vec![(format!("/v1/reports/{}/read", r.id), json!({}))], done);
        self.reader = None;
        self.hide(Key::R(r.id));
    }

    /// `claude rm` plus its check can take several seconds, so the answer comes back as its own
    /// message and a refusal reads as a close that did not happen, not as a message not sent.
    fn close(&mut self, session: &str, name: &str, force: bool) {
        let path = format!("/v1/nodes/{}/close", urlencode(session));
        let name = name.to_string();
        self.flash(trf!("Closing {name}…", "Cerrando {name}…"));
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(90)).post(&path, &json!({ "force": force })).map_err(|e| e.to_string()).map(|v| {
                if v.get("interactive").and_then(Value::as_bool) == Some(true) { Closed::LeftFleet } else { Closed::Removed }
            });
            let _ = tx.send(Msg::Closed { name, result });
        });
    }

    fn closed(&mut self, name: &str, result: Result<Closed, String>) {
        match result {
            Ok(Closed::Removed) => self.flash(trf!("{name} closed", "{name} cerrado")),
            Ok(Closed::LeftFleet) => self.flash(trf!(
                "{name} left the fleet; it is an interactive session, so close its terminal with /exit",
                "{name} ha salido de la flota; es una sesión interactiva, así que cierra su terminal con /exit"
            )),
            Err(why) => self.fail(trf!("{name} was not closed: {why}", "{name} no se cerró: {why}")),
        }
    }

    /// David's verdict on the artifact a report sent: the node reads his comments on it, or learns
    /// it is approved. A report with no artifact has nothing to review.
    fn review(&mut self, r: &Report, verdict: ReviewVerdict) {
        if alondra_core::artifact::in_report(r).is_empty() {
            self.fail(tr!("This report has no artifact to review", "Este informe no tiene ningún artefacto que revisar"));
            return;
        }
        let name = who(&r.node_name, &r.session_id);
        let done = match verdict {
            ReviewVerdict::Comments => trf!("{name} will read your comments", "{name} leerá tus comentarios"),
            ReviewVerdict::Approved => trf!("Approved for {name}", "Aprobado para {name}"),
        };
        self.post(Some(Key::R(r.id)), vec![(format!("/v1/reports/{}/review", r.id), json!({ "verdict": verdict }))], done);
        self.reader = None;
        self.hide(Key::R(r.id));
    }

    fn composer_key(&mut self, k: KeyEvent) {
        let Some(c) = self.composer.as_mut() else { return };
        if let Some(form) = c.form.as_mut() {
            match form.key(k) {
                work::FormKey::Handled => return,
                work::FormKey::Reload(project) => return self.load_options(project),
                work::FormKey::LoadIssues(refresh) => return self.load_issues(refresh),
                work::FormKey::NotMine => {}
            }
        }
        match k.code {
            KeyCode::Esc => self.cancel_composer(),
            KeyCode::Enter if k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) => {
                c.input.handle(k);
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace if c.input.text().is_empty() && !c.chips.is_empty() => {
                c.chips.pop();
            }
            _ => {
                c.input.handle(k);
            }
        }
    }

    /// Copies a dropped file into Alondra's storage while the user keeps writing.
    fn attach(&mut self, path: std::path::PathBuf) {
        let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if bytes > MAX_ATTACHMENT_BYTES {
            let size = files::size(bytes as usize);
            self.fail(trf!("{name} is {size}: attachments go up to 20 MB", "{name} ocupa {size}: los adjuntos llegan hasta 20 MB"));
            return;
        }
        self.chip_seq += 1;
        let seq = self.chip_seq;
        let Some(c) = self.composer.as_mut() else { return };
        let chip = Chip { seq, name: name.clone(), bytes, upload: Upload::Sending };
        let preview = self.picker.is_some() && chip.image();
        c.chips.push(chip);
        self.spawn(move |tx| {
            if preview && let Ok(image) = image::open(&path) {
                let _ = tx.send(Msg::Preview { seq, image: image.thumbnail(PREVIEW_PIXELS, PREVIEW_PIXELS) });
            }
            let result = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| Client::for_tui(Duration::from_secs(120)).upload(&name, &bytes).map_err(|e| e.to_string()))
                .and_then(|v| v.get("id").and_then(Value::as_str).map(str::to_string).ok_or_else(|| "no id".to_string()));
            let _ = tx.send(Msg::Uploaded { seq, result });
        });
    }

    fn uploaded(&mut self, seq: u64, result: Result<String, String>) {
        let chips = self.composer.iter_mut().flat_map(|c| c.chips.iter_mut()).chain(self.drafts.values_mut().flat_map(|(_, c)| c.iter_mut()));
        let mut failed = None;
        for chip in chips.filter(|c| c.seq == seq) {
            match &result {
                Ok(id) => chip.upload = Upload::Stored(id.clone()),
                Err(e) => failed = Some((chip.name.clone(), e.clone())),
            }
        }
        if let Some((name, e)) = failed {
            if let Some(c) = self.composer.as_mut() {
                c.chips.retain(|c| c.seq != seq);
            }
            for (_, chips) in self.drafts.values_mut() {
                chips.retain(|c| c.seq != seq);
            }
            self.fail(trf!("Could not attach {name}: {e}", "No se pudo adjuntar {name}: {e}"));
        }
    }

    fn submit(&mut self) {
        let Some(c) = self.composer.take() else { return };
        let text = c.input.text().trim().to_string();
        let needs_text = matches!(c.target, Target::Answer { .. } | Target::ReplyReport { .. } | Target::Message { .. } | Target::NewRequest | Target::PrChanges { .. } | Target::PrAsk { .. });
        if needs_text && text.is_empty() && c.chips.is_empty() {
            self.composer = Some(c);
            return;
        }
        if c.chips.iter().any(|c| c.upload == Upload::Sending) {
            self.composer = Some(c);
            self.flash(tr!("Still copying the attachments: ⏎ again in a moment", "Aún se están copiando los adjuntos: ⏎ otra vez en un momento"));
            return;
        }
        let ids = stored_ids(&c.chips);
        if !matches!(c.target, Target::Comment { .. } | Target::General { .. }) {
            for chip in &c.chips {
                self.previews.remove(&chip.seq);
            }
        }
        match c.target {
            Target::PrChanges { id, .. } => {
                self.post(Some(Key::P(id)), vec![(format!("/v1/prs/{id}/changes"), json!({"text": text, "attachments": ids}))], tr!("Changes requested", "Cambios pedidos").to_string());
            }
            Target::PrAsk { id, .. } => {
                self.post(None, vec![(format!("/v1/prs/{id}/ask"), json!({"text": text, "attachments": ids}))], tr!("Asked its node", "Preguntado a su nodo").to_string());
            }
            Target::Answer { id, option, .. } => {
                self.post(Some(Key::Q(id)), vec![(format!("/v1/questions/{id}/answer"), answer_body(&text, option.as_deref(), &ids))], trf!("#{id} answered", "#{id} contestada"));
                self.hide(Key::Q(id));
            }
            Target::MoreContext { id, .. } => {
                self.post(
                    Some(Key::Q(id)),
                    vec![(format!("/v1/questions/{id}/more-context"), json!({"note": text, "attachments": ids}))],
                    trf!("#{id} sent back: it will ask you again with more context", "#{id} devuelta: te la volverá a preguntar con más contexto"),
                );
                self.hide(Key::Q(id));
            }
            Target::ReplyReport { id, session, name } => {
                self.post(
                    Some(Key::R(id)),
                    vec![
                        (format!("/v1/nodes/{}/messages", urlencode(&session)), json!({"text": text, "from": "david", "attachments": ids})),
                        (format!("/v1/reports/{id}/read"), json!({})),
                    ],
                    trf!("Sent to {name} and marked read", "Enviado a {name} y marcado leído"),
                );
                self.hide(Key::R(id));
            }
            Target::Message { session, name } => {
                let calls = vec![(format!("/v1/nodes/{}/messages", urlencode(&session)), json!({"text": text, "from": "david", "attachments": ids}))];
                self.post(None, calls, trf!("Sent to {name}", "Enviado a {name}"));
            }
            Target::NewRequest => {
                let body = c.form.as_ref().map(|f| f.body(&text, &ids)).unwrap_or_else(|| json!({"text": text, "attachments": ids}));
                self.post(None, vec![("/v1/requests".into(), body)], tr!("Sent to the orchestrator: it will prepare the mission and launch the node", "Enviado al orquestador: preparará la misión y lanzará el nodo").into())
            }
            Target::Comment { block, quote } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key) else { return };
                let review = self.reviews.entry(key).or_default();
                review.chips.extend(c.chips);
                review.comments.retain(|c| c.block != block);
                if !text.is_empty() {
                    review.comments.push(Comment { block, quote, text });
                    review.comments.sort_by_key(|c| c.block);
                }
            }
            Target::General { send } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key) else { return };
                let review = self.reviews.entry(key).or_default();
                review.general = text;
                review.chips.extend(c.chips);
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
                let look = draw::question_glyph(q);
                (new_reader(key, &q.session_id, &q.node_name, &q.title, look, approval_of(q), parts), attachment_paths(&q.attachments))
            }
            Some(Item::R(r)) => {
                let mut parts = vec![Part::Text(format!("# {}", r.title))];
                parts.extend(r.body.clone().map(Part::Text));
                parts.extend(r.attachments.iter().filter_map(|a| a.stored_path.clone().map(|path| Part::File { name: a.name.clone(), path })));
                parts.extend(links_part(&r.links));
                (new_reader(key, &r.session_id, &r.node_name, &r.title, draw::report_glyph(r), None, parts), attachment_paths(&r.attachments))
            }
            Some(Item::P(p)) => {
                let mut parts = vec![Part::Text(format!("# {}\n\n{}", p.pr.title, draw::pr_summary(p)))];
                parts.extend(p.pr.body.clone().map(Part::Text));
                parts.push(Part::Text(p.pr.url.clone()));
                let (glyph, _, color) = draw::pr_glyph();
                (new_reader(key, &p.pr.session_id, &p.pr.node_name, &p.pr.title, (glyph, tr!("PR", "PR"), color), None, parts), vec![])
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
                        Some(Ok(files::FileBody::Text(text))) => blocks.extend(md::parse(text)),
                        Some(Ok(body)) => blocks.push(Block::para(&files::not_text(body))),
                        Some(Err(e)) => blocks.push(Block::para(&trf!("Could not load the file: {e}", "No he podido cargar el fichero: {e}"))),
                        None => blocks.push(Block::para(tr!("Loading…", "Cargando…"))),
                    }
                }
            }
        }
        r.blocks = blocks;
        r.settle_cursor();
    }

    fn reader_cmd(&mut self, cmd: Cmd) {
        let Some(r) = self.reader.as_mut() else { return };
        match cmd {
            Cmd::Move(Motion::Down) => r.down(),
            Cmd::Move(Motion::Up) => r.up(),
            Cmd::Move(Motion::Top) => r.top(),
            Cmd::Move(Motion::Bottom) => r.bottom(),
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
        let key = r.key;
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
            None => label.clone().unwrap_or_else(|| "Approved".into()),
        };
        let node = r.node.clone();
        self.post(Some(r.key), vec![(format!("/v1/questions/{id}/answer"), answer_body(&text, label.as_deref(), &[]))], trf!("Approved → {node}", "Aprobado → {node}"));
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
        let ids = stored_ids(&review.chips);
        let n = review.count();
        let node = r.node.clone();
        let count = i18n::comments(n);
        match key {
            Key::Q(id) => {
                let option = r.approval.as_ref().and_then(|a| a.changes.clone());
                let done = match option {
                    Some(_) => trf!("Changes requested from {node} ({count})", "Cambios pedidos a {node} ({count})"),
                    None => trf!("Answer sent to {node} ({count})", "Respuesta enviada a {node} ({count})"),
                };
                self.post(Some(key), vec![(format!("/v1/questions/{id}/answer"), answer_body(&text, option.as_deref(), &ids))], done);
            }
            Key::R(id) => {
                self.post(
                    Some(key),
                    vec![
                        (format!("/v1/nodes/{}/messages", urlencode(&r.session)), json!({"text": text, "from": "david", "attachments": ids})),
                        (format!("/v1/reports/{id}/read"), json!({})),
                    ],
                    trf!("{count} sent to {node}; marked read", "{count} enviados a {node}; marcado leído"),
                );
            }
            Key::P(id) => {
                self.post(Some(key), vec![(format!("/v1/prs/{id}/changes"), json!({"text": text, "attachments": ids}))], trf!("Changes requested from {node} ({count})", "Cambios pedidos a {node} ({count})"));
            }
        }
        self.reader = None;
        self.hide(key);
    }

    pub fn node_matches(&self, n: &FleetNode, q: &str) -> bool {
        let tasks: Vec<&str> = self.tasks_on(&n.session_id).into_iter().map(|t| t.title.as_str()).collect();
        let hay = format!(
            "{} {} {} {} {} {} {} {} {}",
            n.display_name(),
            n.short_id,
            n.project.clone().unwrap_or_default(),
            n.state.label(),
            i18n::state(n.state),
            i18n::fleet_state(n),
            said(n),
            n.cwd.clone().unwrap_or_default(),
            tasks.join(" ")
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

    /// Regular nodes only, grouped by project in a stable, creation-time order (see
    /// `fleet_sessions`); the orchestrator is never a row here, so it can never end up inside a
    /// project's folder — it has its own fixed area above this list.
    pub fn fleet_rows(&self) -> Vec<FleetRow> {
        let indices: Vec<usize> = match self.fleet_filter() {
            Some((idx, _)) => idx,
            None => (0..self.data.nodes.len()).collect(),
        };
        let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for i in indices {
            let n = &self.data.nodes[i];
            if !n.is_orchestrator {
                groups.entry(n.project.clone().filter(|p| p != alondra_core::OTHERS_PROJECT).unwrap_or_else(|| tr!("others", "otros").into())).or_default().push(i);
            }
        }
        let mut sorted: Vec<(String, Vec<usize>)> = groups.into_iter().collect();
        sorted.sort_by_key(|(p, ..)| p.to_lowercase());
        let mut rows = vec![];
        for (name, nodes) in sorted {
            let total = self.data.nodes.iter().filter(|n| !n.is_orchestrator && n.project.as_deref() == Some(name.as_str())).count().max(nodes.len());
            rows.push(FleetRow::Group { name, total });
            rows.extend(nodes.into_iter().map(FleetRow::Node));
        }
        rows
    }

    /// Every selectable node, in keyboard-navigation order: the orchestrator first (its fixed
    /// area at the top is still reachable with the arrow keys), then the regular fleet list.
    fn fleet_sessions(&self) -> Vec<String> {
        self.orchestrator()
            .map(|n| n.session_id.clone())
            .into_iter()
            .chain(self.fleet_rows().iter().filter_map(|r| match r {
                FleetRow::Node(i) => Some(self.data.nodes[*i].session_id.clone()),
                FleetRow::Group { .. } => None,
            }))
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
        self.load_threads();
    }

    fn fleet_cmd(&mut self, cmd: Cmd) {
        let i = self.fleet_idx as isize;
        let detail = self.fleet_zoom || (self.focus == Pane::Detail && self.fleet_view.shown);
        match cmd {
            Cmd::Move(m) if detail => self.fleet_detail_scroll = keys::scroll(self.fleet_detail_scroll, m, self.fleet_view.h, self.fleet_view.total),
            Cmd::Move(Motion::Down) => self.move_fleet(i + 1),
            Cmd::Move(Motion::Up) => self.move_fleet(i - 1),
            Cmd::Move(Motion::Top) => self.move_fleet(0),
            Cmd::Move(Motion::Bottom) => self.move_fleet(isize::MAX),
            Cmd::Move(m) => self.fleet_detail_scroll = keys::scroll(self.fleet_detail_scroll, m, self.fleet_view.h, self.fleet_view.total),
            Cmd::Right if self.fleet_view.shown && !self.fleet_zoom => self.focus = Pane::Detail,
            Cmd::Right => self.fleet_zoom = self.fleet_sel.is_some(),
            Cmd::Expand => self.fleet_zoom = !self.fleet_zoom && self.fleet_sel.is_some(),
            Cmd::Left | Cmd::Back if self.fleet_zoom => self.fleet_zoom = false,
            Cmd::Left | Cmd::Back if self.focus == Pane::Detail => self.focus = Pane::List,
            Cmd::Back if self.search.is_some() => self.search = None,
            Cmd::Left | Cmd::Back | Cmd::Pending | Cmd::Other(_) => {}
        }
    }

    fn fleet_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Char('J') => self.fleet_detail_scroll = keys::by(self.fleet_detail_scroll, 5, self.fleet_view.h, self.fleet_view.total),
            KeyCode::Char('K') => self.fleet_detail_scroll = keys::by(self.fleet_detail_scroll, -5, self.fleet_view.h, self.fleet_view.total),
            KeyCode::Char('/') => match self.search.as_mut() {
                Some(s) => s.editing = true,
                None => self.search = Some(Search { input: TextInput::default(), editing: true, typed_at: now_millis(), remote: Remote::Idle }),
            },
            KeyCode::Char('x') => {
                if let Some(n) = self.selected_node() {
                    let (session, name) = (n.session_id.clone(), n.display_name().to_string());
                    if self.confirm_close(&session, &name) {
                        self.close(&session, &name, true);
                    }
                }
            }
            KeyCode::Char('r') | KeyCode::Char('m') => {
                if let Some(n) = self.selected_node() {
                    let target = Target::Message { session: n.session_id.clone(), name: n.display_name().to_string() };
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

fn attachment_paths(a: &[alondra_core::Attachment]) -> Vec<String> {
    a.iter().filter_map(|a| a.stored_path.clone()).collect()
}

fn links_part(links: &[alondra_core::Link]) -> Option<Part> {
    (!links.is_empty()).then(|| {
        let items: Vec<String> = links.iter().map(|l| format!("- {} {}", l.title.clone().unwrap_or_default(), l.url)).collect();
        Part::Text(format!("## {}\n\n{}", tr!("Links", "Enlaces"), items.join("\n")))
    })
}

fn new_reader(
    key: Key,
    session: &str,
    node: &Option<String>,
    title: &str,
    (glyph, kind, color): (&'static str, &'static str, ratatui::style::Color),
    approval: Option<Approval>,
    parts: Vec<Part>,
) -> Reader {
    Reader {
        key,
        session: session.to_string(),
        node: who(node, session),
        title: title.to_string(),
        kind,
        glyph,
        color,
        approval,
        parts,
        blocks: vec![],
        cursor: 0,
        scroll: 0,
        ranges: vec![],
        view_h: 0,
        total: 0,
        find: None,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #[test]
    fn background_contention_retries_but_real_errors_remain_visible() {
        let busy = anyhow::Error::new(crate::account::AccountBusy).context("request submission uncertain");
        assert!(matches!(super::background_sync_error(&busy, "wait: "), super::Msg::Offline));
        let denied = anyhow::anyhow!("relay authentication failed (HTTP 401)");
        assert!(matches!(super::background_sync_error(&denied, "wait: "),
            super::Msg::SyncError(text) if text.contains("HTTP 401")));
    }

    use super::*;
    use alondra_core::{QuestionOption, QuestionStatus, ReportKind};

    pub(super) fn question(id: u64, origin: QuestionOrigin, options: &[&str]) -> Question {
        Question {
            id,
            session_id: "s1".into(),
            node_name: Some("demo".into()),
            title: format!("Question {id}"),
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
            action: false,
            steps: vec![],
            mission: None,
            replaces: None, about_pr: None,
            task: None,
        }
    }

    pub(super) fn report(id: u64) -> Report {
        Report {
            id,
            session_id: "s1".into(),
            node_name: Some("demo".into()),
            title: format!("Report {id}"),
            body: None,
            links: vec![],
            attachments: vec![],
            kind: ReportKind::Notice,
            created_at: 0,
            read: false,
            to_orchestrator: false,
            mission: None,
            looks_like_question: false,
            task: None,
        }
    }

    pub(super) fn alive_node(session: &str) -> crate::client::FleetNode {
        crate::client::FleetNode { node: alondra_core::model::Node::new(session, 0), project: None }
    }

    pub(super) fn app_with(questions: Vec<Question>) -> (App, Receiver<Msg>) {
        i18n::set_for_this_thread(i18n::Lang::En);
        let (tx, rx) = mpsc::sync_channel(8);
        let mut app = App::new(tx);
        app.data.open_questions = questions.into_iter().map(Into::into).collect();
        app.reconcile();
        (app, rx)
    }

    #[test]
    fn plans_and_approve_or_change_questions_get_the_review_keys() {
        let plan = approval_of(&question(1, QuestionOrigin::Plan, &["Approve", "Request changes"])).unwrap();
        assert_eq!(plan.approve.as_deref(), Some("Approve"));
        assert_eq!(plan.changes.as_deref(), Some("Request changes"));
        let spanish = approval_of(&question(2, QuestionOrigin::Ask, &["Aprobar", "Cambiar"])).unwrap();
        assert_eq!(spanish.changes.as_deref(), Some("Cambiar"));
        assert!(approval_of(&question(3, QuestionOrigin::Ask, &["Yes", "No"])).is_none());
    }

    #[test]
    fn answering_advances_to_the_next_item_and_guards_against_a_double_press() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &["Yes"]), question(2, QuestionOrigin::Ask, &["Yes"])]);
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
        app.data.open_questions.insert(0, question(9, QuestionOrigin::Ask, &[]).into());
        app.reconcile();
        assert_eq!(app.sel, Some(Key::Q(2)));
        assert_eq!(app.sel_idx, 2);
    }

    #[test]
    fn a_failed_send_brings_the_item_back() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &[])]);
        app.hide(Key::Q(1));
        assert!(app.inbox_keys().is_empty());
        app.on_msg(Msg::Posted { key: Some(Key::Q(1)), result: Err("down".into()) });
        app.reconcile();
        assert_eq!(app.sel, Some(Key::Q(1)));
    }

    #[test]
    fn a_dropped_file_becomes_an_attachment_that_waits_for_its_copy_before_sending() {
        let dir = std::env::temp_dir().join(format!("alondra-tui-drop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("Captura de pantalla.png");
        std::fs::write(&shot, b"x").unwrap();
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &[])]);
        app.compose(Target::Answer { id: 1, title: "t".into(), option: None }, "");
        app.on_paste(&format!("{}/Captura\\ de\\ pantalla.png", dir.display()));
        let c = app.composer.as_ref().unwrap();
        assert_eq!(c.input.text(), "", "the path is not typed into the answer");
        assert_eq!(c.chips.iter().map(|c| (c.name.as_str(), c.bytes)).collect::<Vec<_>>(), [("Captura de pantalla.png", 1)]);

        app.submit();
        assert!(app.composer.is_some(), "an attachment still copying holds the send back");

        let seq = app.composer.as_ref().unwrap().chips[0].seq;
        app.on_msg(Msg::Uploaded { seq, result: Ok("0123456789abcdef".into()) });
        assert_eq!(app.composer.as_ref().unwrap().chips[0].upload, Upload::Stored("0123456789abcdef".into()));
        assert_eq!(stored_ids(&app.composer.as_ref().unwrap().chips), ["0123456789abcdef"]);
        app.cancel_composer();
        app.compose(Target::Answer { id: 1, title: "t".into(), option: None }, "");
        assert_eq!(app.composer.as_ref().unwrap().chips.len(), 1, "a draft keeps its attachments");

        app.on_paste("no es una ruta");
        app.composer.as_mut().unwrap().input = TextInput::default();
        app.composer_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        assert!(app.composer.as_ref().unwrap().chips.is_empty(), "backspace on an empty box removes the last attachment");

        app.on_paste(&shot.display().to_string());
        let seq = app.composer.as_ref().unwrap().chips[0].seq;
        app.on_msg(Msg::Uploaded { seq, result: Err("alondrad answered: the file is over 20 MB".into()) });
        assert!(app.composer.as_ref().unwrap().chips.is_empty(), "a failed copy is not left pretending to be attached");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cancelled_reply_comes_back_as_a_draft() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &[])]);
        app.compose(Target::Answer { id: 1, title: "t".into(), option: None }, "");
        app.composer.as_mut().unwrap().input.insert_str("half written");
        app.cancel_composer();
        assert!(app.composer.is_none());
        app.compose(Target::Answer { id: 1, title: "t".into(), option: None }, "");
        assert_eq!(app.composer.as_ref().unwrap().input.text(), "half written");
    }

    #[test]
    fn review_feedback_follows_document_order() {
        let mut rv = Review::default();
        rv.comments.push(Comment { block: 1, quote: "a".into(), text: "x".into() });
        rv.comments.push(Comment { block: 0, quote: "b".into(), text: "y".into() });
        rv.comments.sort_by_key(|c| c.block);
        assert!(rv.feedback().starts_with(&format!("Comments from {}:\n\n1. On \"b\":", crate::client::config().name)));
    }

    #[test]
    fn asking_on_a_pr_keeps_the_comments_drafted_in_its_reader() {
        let (mut app, _rx) = app_with(vec![]);
        app.reviews.insert(Key::P(4), Review { comments: vec![Comment { block: 0, quote: "a".into(), text: "why this?".into() }], ..Default::default() });
        app.compose(Target::PrAsk { id: 4, title: "t".into() }, "");
        app.composer.as_mut().unwrap().input.insert_str("Does it build?");
        app.submit();
        assert_eq!(app.posted.borrow().last().map(|(p, _)| p.clone()), Some(vec!["/v1/prs/4/ask".to_string()]));
        let key = app.posted_keys.borrow().last().cloned().flatten();
        app.on_msg(Msg::Posted { key, result: Ok("Asked its node".into()) });
        assert_eq!(app.reviews.get(&Key::P(4)).map(|r| r.comments.len()), Some(1), "the Ask sent none of them");
    }

    /// Round 2: the open node's threads were asked for once, when it was opened, and never again
    /// while it stayed open: the overview no longer carries them.
    #[test]
    fn the_open_nodes_threads_are_asked_again_when_the_tick_moves_and_every_few_seconds() {
        let (mut app, _rx) = app_with(vec![]);
        app.data.nodes = vec![alive_node("s")];
        app.data.tick = 1;
        app.show(Screen::Fleet);
        app.reconcile();
        let asked = |app: &App| app.threads_asked.clone().map(|(s, tick, _)| (s, tick));
        assert_eq!(asked(&app), Some(("s".to_string(), 1)));

        let mut moved = app.data.clone();
        moved.tick = 2;
        app.on_msg(Msg::Data(Box::new(moved)));
        app.reconcile();
        assert_eq!(asked(&app), Some(("s".to_string(), 2)), "a state changed: asked again");

        let before = app.threads_asked.as_ref().map(|(_, _, at)| *at).unwrap();
        app.on_timers();
        assert_eq!(app.threads_asked.as_ref().map(|(_, _, at)| *at), Some(before), "nothing moved: not asked again yet");
        if let Some((_, _, at)) = app.threads_asked.as_mut() {
            *at -= THREADS_EVERY_MS;
        }
        app.on_timers();
        assert!(app.threads_asked.as_ref().is_some_and(|(_, _, at)| *at >= before), "tool calls move threads without the tick: asked again");

        let main = crate::client::Activity { tool: Some("Bash".into()), summary: Some("make test".into()), since: 5, ..Default::default() };
        app.on_msg(Msg::NodeThreads { session: "s".into(), threads: Some(vec![main]) });
        app.on_msg(Msg::NodeThreads { session: "s".into(), threads: None });
        assert_eq!(app.threads.as_ref().map(|(s, t)| (s.as_str(), t.len())), Some(("s", 1)), "a failed ask keeps what was shown");
    }

    /// Round 3: every 10 s the open node's whole detail (53–350 KB) came over to read its threads.
    #[test]
    fn threads_come_from_the_light_route_and_from_the_detail_on_an_older_alondrad() {
        let asked = std::cell::RefCell::new(Vec::<String>::new());
        let newer = |path: &str| -> Result<Value> {
            asked.borrow_mut().push(path.to_string());
            Ok(json!({"activity": [{"agent_id": null, "tool": "Bash", "summary": "make test", "since": 5}]}))
        };
        assert_eq!(node_threads(newer, "hive/one").map(|t| t.len()), Some(1));
        assert_eq!(asked.take(), ["/v1/nodes/hive%2Fone/activity"], "the light route alone");

        let older = |path: &str| -> Result<Value> {
            asked.borrow_mut().push(path.to_string());
            if path.ends_with("/activity") {
                return Err(anyhow::Error::new(crate::client::NotFound));
            }
            Ok(json!({"node": {"activity": [{"tool": "Bash", "since": 5}, {"agent_id": "a1", "tool": "mav", "since": 6}]}, "timeline": []}))
        };
        assert_eq!(node_threads(older, "s").map(|t| t.len()), Some(2));
        assert_eq!(asked.take(), ["/v1/nodes/s/activity", "/v1/nodes/s?limit=1"], "an alondrad without the route: its detail, cut to one event");

        let down = |_: &str| -> Result<Value> { Err(anyhow::anyhow!("alondrad is not responding")) };
        assert!(node_threads(down, "s").is_none(), "a failed ask leaves what was shown");
    }

    #[test]
    fn what_an_older_alondrad_lacked_is_tried_again_once_it_is_back() {
        let (mut app, _rx) = app_with(vec![]);
        (app.task_actions_missing, app.archive_missing, app.history_missing) = (true, true, true);
        app.on_msg(Msg::Data(Box::new(app.data.clone())));
        assert!(app.task_actions_missing && app.archive_missing, "the same alondrad still lacks them");
        app.on_msg(Msg::Offline);
        app.on_msg(Msg::Data(Box::new(app.data.clone())));
        assert!(!app.task_actions_missing && !app.archive_missing && !app.history_missing);
    }

    pub(super) fn press(app: &mut App, keys: &str) -> Flow {
        let mut flow = Flow::Go;
        for c in keys.chars() {
            flow = app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        flow
    }

    pub(super) fn code(app: &mut App, c: KeyCode) -> Flow {
        app.on_key(KeyEvent::new(c, KeyModifiers::NONE))
    }

    pub(super) fn ctrl(app: &mut App, c: char) -> Flow {
        app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn long_question(id: u64) -> Question {
        let mut q = question(id, QuestionOrigin::Ask, &["Yes", "No"]);
        q.detail = Some((1..=80).map(|i| format!("Paragraph {i}.")).collect::<Vec<_>>().join("\n\n"));
        q
    }

    #[test]
    fn gg_and_g_jump_to_the_ends_of_the_list() {
        let (mut app, _rx) = app_with((1..=4).map(|i| question(i, QuestionOrigin::Ask, &[])).collect());
        press(&mut app, "G");
        assert_eq!(app.sel, Some(Key::Q(4)));
        press(&mut app, "g");
        assert_eq!(app.sel, Some(Key::Q(4)));
        press(&mut app, "g");
        assert_eq!(app.sel, Some(Key::Q(1)));
        press(&mut app, "jj");
        code(&mut app, KeyCode::Up);
        assert_eq!(app.sel, Some(Key::Q(2)));
    }

    #[test]
    fn ctrl_d_scrolls_the_detail_from_the_list_and_j_scrolls_it_once_focused() {
        let (mut app, _rx) = app_with(vec![long_question(1), question(2, QuestionOrigin::Ask, &[])]);
        app.detail_view = View { h: 20, total: 200, shown: true };
        ctrl(&mut app, 'd');
        assert_eq!(app.detail_scroll, 10);
        ctrl(&mut app, 'f');
        assert_eq!(app.detail_scroll, 28);
        ctrl(&mut app, 'u');
        assert_eq!(app.detail_scroll, 18);
        assert_eq!(app.sel, Some(Key::Q(1)));
        press(&mut app, "l");
        assert_eq!(app.focus, Pane::Detail);
        press(&mut app, "jjj");
        assert_eq!(app.detail_scroll, 21);
        assert_eq!(app.sel, Some(Key::Q(1)));
        press(&mut app, "G");
        assert_eq!(app.detail_scroll, 180);
        press(&mut app, "gg");
        assert_eq!(app.detail_scroll, 0);
        code(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Pane::List);
        press(&mut app, "j");
        assert_eq!(app.sel, Some(Key::Q(2)));
    }

    #[test]
    fn o_expands_the_item_and_o_again_collapses_it() {
        let (mut app, _rx) = app_with(vec![long_question(1)]);
        press(&mut app, "o");
        assert!(app.reader.as_ref().is_some_and(|r| r.key == Key::Q(1)));
        press(&mut app, "o");
        assert!(app.reader.is_none());
        code(&mut app, KeyCode::Enter);
        assert!(app.reader.is_some());
        code(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
    }

    #[test]
    fn the_reader_scrolls_through_a_block_taller_than_the_screen() {
        let mut q = question(1, QuestionOrigin::Ask, &[]);
        q.detail = Some("word ".repeat(2000));
        let (mut app, _rx) = app_with(vec![q]);
        press(&mut app, "o");
        let r = app.reader.as_mut().unwrap();
        r.view_h = 10;
        r.total = 60;
        r.ranges = vec![(0, 2), (3, 60)];
        r.cursor = 1;
        r.scroll = 2;
        press(&mut app, "jjjj");
        let r = app.reader.as_ref().unwrap();
        assert_eq!((r.cursor, r.scroll), (1, 6));
        ctrl(&mut app, 'd');
        assert_eq!(app.reader.as_ref().unwrap().scroll, 11);
        press(&mut app, "G");
        assert_eq!(app.reader.as_ref().unwrap().scroll, 50);
        press(&mut app, "gg");
        let r = app.reader.as_ref().unwrap();
        assert_eq!(r.scroll, 0);
    }

    #[test]
    fn slash_searches_the_reading_and_n_cycles_through_matches() {
        let mut q = question(1, QuestionOrigin::Ask, &[]);
        q.detail = Some("one\n\ntwo cat\n\nthree\n\nfour cat\n\nfive".into());
        let (mut app, _rx) = app_with(vec![q]);
        press(&mut app, "o/cat");
        assert!(app.reader.as_ref().unwrap().find.as_ref().is_some_and(|f| f.editing && f.input.text() == "cat"));
        code(&mut app, KeyCode::Enter);
        let r = app.reader.as_ref().unwrap();
        assert_eq!(r.blocks[r.cursor].plain(), "two cat");
        press(&mut app, "n");
        let r = app.reader.as_ref().unwrap();
        assert_eq!(r.blocks[r.cursor].plain(), "four cat");
        press(&mut app, "n");
        let r = app.reader.as_ref().unwrap();
        assert_eq!(r.blocks[r.cursor].plain(), "two cat");
        press(&mut app, "N");
        let r = app.reader.as_ref().unwrap();
        assert_eq!(r.blocks[r.cursor].plain(), "four cat");
        code(&mut app, KeyCode::Esc);
        assert!(app.reader.as_ref().is_some_and(|r| r.find.is_none()));
        code(&mut app, KeyCode::Esc);
        assert!(app.reader.is_none());
    }

    #[test]
    fn capitals_typed_with_shift_reach_every_text_field() {
        let shifted = |app: &mut App, c: char| app.on_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::SHIFT));
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &["Sí"])]);
        press(&mut app, "r");
        shifted(&mut app, 'H');
        press(&mut app, "ola ");
        shifted(&mut app, 'Ñ');
        assert_eq!(app.composer.as_ref().unwrap().input.text(), "Hola Ñ");
        code(&mut app, KeyCode::Esc);
        press(&mut app, "n");
        shifted(&mut app, 'A');
        assert_eq!(app.composer.as_ref().unwrap().input.text(), "A");
    }

    #[test]
    fn typing_in_the_reply_box_never_triggers_normal_mode_keys() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &["Yes"])]);
        press(&mut app, "r");
        assert!(app.composer.is_some());
        assert!(matches!(press(&mut app, "q:ggjko1?/nG"), Flow::Go));
        ctrl(&mut app, 'd');
        assert_eq!(app.composer.as_ref().unwrap().input.text(), "q:ggjko1?/nG");
        assert!(app.reader.is_none() && !app.help && app.cmdline.is_none());
        code(&mut app, KeyCode::Esc);
        assert!(app.composer.is_none());
        assert!(matches!(press(&mut app, "q"), Flow::Quit));
    }

    #[test]
    fn the_command_line_quits_and_navigates() {
        let (mut app, _rx) = app_with((1..=3).map(|i| question(i, QuestionOrigin::Ask, &[])).collect());
        press(&mut app, ":3");
        code(&mut app, KeyCode::Enter);
        assert_eq!(app.sel, Some(Key::Q(3)));
        press(&mut app, ":fleet");
        code(&mut app, KeyCode::Enter);
        assert!(app.screen == Screen::Fleet);
        press(&mut app, ":nope");
        code(&mut app, KeyCode::Enter);
        assert!(app.flash.as_ref().is_some_and(|f| f.error));
        press(&mut app, ":q");
        assert!(matches!(code(&mut app, KeyCode::Enter), Flow::Quit));
    }

    #[test]
    fn an_action_card_asks_why_when_it_cannot_be_done() {
        let mut card = question(1, QuestionOrigin::Ask, &[]);
        card.action = true;
        card.options = alondra_core::answer_option::for_action();
        let (mut app, _rx) = app_with(vec![card.clone(), question(2, QuestionOrigin::Ask, &[])]);
        app.settle_until = 0;
        press(&mut app, "2");
        let Some(Composer { target: Target::Answer { id: 1, option, .. }, .. }) = &app.composer else { panic!("no composer") };
        assert_eq!(option.as_deref(), Some(alondra_core::answer_option::CANNOT));
        assert!(!needs_text(&card.options[0]));
    }

    pub(super) fn pr_card(preferred: Option<alondra_core::pr::MergeMethod>) -> crate::client::PrCard {
        use alondra_core::pr::{MergeMethod, PullRequest};
        let mut pr = PullRequest::opened(4, "https://github.com/a/b/pull/12", "s1", Some("demo".into()), 0).unwrap();
        pr.title = "Menus become cards".into();
        pr.merge_methods = vec![MergeMethod::Merge, MergeMethod::Squash];
        crate::client::PrCard { pr, preferred_method: preferred, below_number: None, below_set_aside: false, stack_bottom: None, stack_opened_at: None, stack_position: 0, task: None }
    }

    /// Card `id` (PR #`id + 10`) stacked `position` cards up the train that starts at `bottom`.
    pub(super) fn train_card(id: u64, title: &str, bottom: u64, position: usize, opened_at: i64) -> crate::client::PrCard {
        let mut card = pr_card(None);
        card.pr.id = id;
        card.pr.number = id + 10;
        card.pr.title = title.into();
        card.pr.opened_at = opened_at;
        card.below_number = (position > 0).then(|| id + 9);
        (card.stack_bottom, card.stack_position, card.stack_opened_at) = (Some(bottom), position, Some(bottom as i64 * 100));
        card
    }

    #[test]
    fn a_pr_on_top_of_an_open_pr_cannot_be_merged_from_its_card() {
        let (mut app, _rx) = app_with(vec![]);
        app.data.prs = vec![train_card(2, "Upper", 1, 1, 50), train_card(1, "Lower", 1, 0, 100)];
        app.reconcile();
        app.settle_until = 0;
        assert_eq!(app.inbox_keys(), [Key::P(1), Key::P(2)], "the train bottom first");
        press(&mut app, "j");
        assert!(matches!(app.sel, Some(Key::P(2))));
        press(&mut app, "1");
        press(&mut app, "1");
        assert!(app.pending_merge.is_none(), "no merge started");
        assert!(!app.hidden.contains_key(&Key::P(2)), "nothing merged");
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("Merge PR #11 first")), "{:?}", app.flash.as_ref().map(|f| &f.text));

        app.data.prs = vec![crate::client::PrCard { below_set_aside: true, ..train_card(2, "Upper", 1, 1, 50) }];
        app.reconcile();
        press(&mut app, "1");
        assert!(app.pending_merge.is_none(), "a PR set aside in Alondra is still open on GitHub");
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("PR #11 is still open on GitHub: merge or close it first")), "{:?}", app.flash.as_ref().map(|f| &f.text));
    }

    #[test]
    fn a_pr_card_offers_the_repos_methods_with_the_last_used_first() {
        use alondra_core::pr::MergeMethod;
        assert_eq!(pr_card(None).methods(), [MergeMethod::Merge, MergeMethod::Squash]);
        assert_eq!(pr_card(Some(MergeMethod::Squash)).methods(), [MergeMethod::Squash, MergeMethod::Merge]);
        assert_eq!(pr_card(Some(MergeMethod::Rebase)).methods(), [MergeMethod::Merge, MergeMethod::Squash], "a method the repo no longer allows is not offered");
    }

    #[test]
    fn only_a_tuicr_export_with_comments_is_sent_back() {
        let export = "I reviewed your code and have the following comments. Please address them.\n\n1. `src/auth.rs` - Consider adding unit tests\n2. `src/auth.rs:42` - Magic number should be a named constant\n";
        assert!(has_comments(export));
        assert!(!has_comments(""));
        assert!(!has_comments("I reviewed your code.\n"));
    }

    #[test]
    fn opening_tuicr_says_so_before_the_screen_goes_to_it() {
        let repo = std::env::temp_dir().join(format!("alondra-review-{}", std::process::id()));
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let (mut app, _rx) = app_with(vec![]);
        let mut node = alondra_core::model::Node::new("s1", 0);
        node.cwd = Some(repo.display().to_string());
        app.data.nodes = vec![crate::client::FleetNode { node, project: None }];
        app.data.prs = vec![pr_card(None)];
        app.reconcile();
        app.settle_until = 0;
        let flow = app.on_key(KeyEvent::from(KeyCode::Char('d')));
        std::fs::remove_dir_all(&repo).unwrap();
        assert!(matches!(flow, Flow::Review { id: 4, number: 12, .. }));
        assert_eq!(app.flash.as_ref().map(|f| f.text.as_str()), Some("Opening PR #12 in tuicr…"));
    }

    #[test]
    fn merging_a_pr_takes_a_second_press() {
        let (mut app, _rx) = app_with(vec![]);
        app.data.prs = vec![pr_card(None)];
        app.reconcile();
        app.settle_until = 0;
        assert!(matches!(app.sel, Some(Key::P(4))));
        press(&mut app, "2");
        assert_eq!(app.pending_merge.map(|(id, m, _)| (id, m)), Some((4, alondra_core::pr::MergeMethod::Squash)));
        assert!(app.flash.as_ref().is_some_and(|f| f.text.contains("Press 2 again to merge PR #12 (squash)")), "{:?}", app.flash.as_ref().map(|f| &f.text));
        assert!(!app.hidden.contains_key(&Key::P(4)), "nothing merged yet");
    }

    #[test]
    fn a_pr_in_conflict_offers_resolving_it_instead_of_merging() {
        use crate::client::PrAction;
        let mut card = pr_card(None);
        card.pr.mergeable = Some("CONFLICTING".into());
        assert_eq!(card.actions(), [PrAction::ResolveConflicts]);
        let (mut app, _rx) = app_with(vec![]);
        app.data.prs = vec![card];
        app.reconcile();
        app.settle_until = 0;
        press(&mut app, "2");
        assert_eq!(app.flash.as_ref().map(|f| f.text.as_str()), Some("This PR has no option 2"));
        press(&mut app, "1");
        assert!(app.pending_merge.is_none(), "nothing to merge");
        assert!(app.hidden.contains_key(&Key::P(4)), "the card waits for the node's push");
    }

    fn asked_on_pr(id: u64, pr: u64) -> crate::client::OpenQuestion {
        let mut q = question(id, QuestionOrigin::Ask, &["Merge it", "Not yet"]);
        q.options[1].description = Some("Write what is missing".into());
        crate::client::OpenQuestion { question: q, pr_id: Some(pr), task: None, report: None, reports: vec![] }
    }

    #[test]
    fn the_overview_says_which_pr_card_a_question_belongs_to() {
        let mut value = serde_json::to_value(question(2, QuestionOrigin::Ask, &["Yes"])).unwrap();
        let plain: crate::client::OpenQuestion = serde_json::from_value(value.clone()).unwrap();
        value["pr_id"] = json!(4);
        let attached: crate::client::OpenQuestion = serde_json::from_value(value).unwrap();
        assert_eq!((plain.pr_id, attached.pr_id, attached.id), (None, Some(4), 2));
    }

    #[test]
    fn a_question_about_a_waiting_pr_sits_under_its_card_and_counts_with_it() {
        let (mut app, _rx) = app_with(vec![question(1, QuestionOrigin::Ask, &["Yes"])]);
        app.data.prs = vec![pr_card(None)];
        app.data.open_questions.extend([asked_on_pr(2, 4), asked_on_pr(3, 99)]);
        app.reconcile();
        assert_eq!(app.inbox_keys(), [Key::Q(1), Key::Q(3), Key::P(4), Key::Q(2)], "a question whose card is gone stays a card of its own");
        assert_eq!(app.inbox_count(), 3);
        app.hide(Key::P(4));
        assert_eq!(app.inbox_keys(), [Key::Q(1), Key::Q(3)], "the question leaves with its card");
    }

    #[test]
    fn a_report_about_a_waiting_pr_sits_under_its_card_and_counts_with_it() {
        let reported = |id: u64, pr: Option<u64>| crate::client::NewsReport { report: report(id), pr_id: pr, task: None, earlier: vec![] };
        let cases: [(&str, Vec<crate::client::NewsReport>, Vec<Key>, usize); 3] = [
            ("the report links the waiting PR", vec![reported(2, None), reported(1, Some(4))], vec![Key::P(4), Key::R(1), Key::R(2)], 2),
            ("the report's card is not in the inbox", vec![reported(2, None), reported(1, Some(99))], vec![Key::P(4), Key::R(2), Key::R(1)], 3),
            ("the report links no PR", vec![reported(2, None), reported(1, None)], vec![Key::P(4), Key::R(2), Key::R(1)], 3),
        ];
        for (what, news, keys, count) in cases {
            let (mut app, _rx) = app_with(vec![]);
            app.data.prs = vec![pr_card(None)];
            app.data.news = news;
            app.reconcile();
            assert_eq!(app.inbox_keys(), keys, "{what}");
            assert_eq!(app.inbox_count(), count, "{what}");
            assert_eq!(app.parent(Key::R(1)).is_some(), count == 2, "{what}");
        }
        let (mut app, _rx) = app_with(vec![]);
        app.data.prs = vec![pr_card(None)];
        app.data.news = vec![reported(1, Some(4))];
        app.reconcile();
        app.hide(Key::P(4));
        assert!(app.inbox_keys().is_empty(), "the report leaves with its card");
    }

    #[test]
    fn digits_on_a_question_inside_a_pr_card_answer_the_question() {
        let (mut app, _rx) = app_with(vec![]);
        app.data.prs = vec![pr_card(None)];
        app.data.open_questions.push(asked_on_pr(2, 4));
        app.reconcile();
        press(&mut app, "j");
        assert_eq!(app.sel, Some(Key::Q(2)));
        press(&mut app, "2");
        let Some(Composer { target: Target::Answer { id: 2, option, .. }, .. }) = &app.composer else { panic!("no composer") };
        assert_eq!(option.as_deref(), Some("Not yet"));
        assert!(app.pending_merge.is_none(), "the PR is not merged from its question");
    }

    #[test]
    fn modified_digits_do_not_answer_cards() {
        let (mut app, _rx) = app_with(vec![]);
        app.data.open_questions.push(asked_on_pr(2, 4));
        app.reconcile();
        app.on_key(KeyEvent::new(KeyCode::Char('2'), KeyModifiers::SUPER));
        assert!(app.composer.is_none());
        assert!(app.pending_merge.is_none());
        press(&mut app, "2");
        assert!(matches!(app.composer.as_ref().map(|c| &c.target), Some(Target::Answer { id: 2, .. })));
    }

    #[test]
    fn help_scrolls_with_j_and_closes_with_anything_else() {
        let (mut app, _rx) = app_with(vec![]);
        press(&mut app, "?");
        assert!(app.help);
        app.help_view = View { h: 10, total: 40, shown: true };
        press(&mut app, "jj");
        assert!(app.help);
        assert_eq!(app.help_scroll, 2);
        press(&mut app, "x");
        assert!(!app.help);
    }

    #[test]
    fn x_only_marks_a_report_read_and_never_closes_its_node() {
        let (mut app, _rx) = app_with(vec![]);
        app.data.news = vec![report(1).into()];
        app.data.nodes = vec![alive_node("s1")];
        app.reconcile();
        assert_eq!(app.sel, Some(Key::R(1)));
        press(&mut app, "x");
        assert!(app.hidden.contains_key(&Key::R(1)), "x marks the report read");
        assert!(app.pending_close.is_none(), "x alone never arms a close");
    }

    #[test]
    fn a_close_answer_says_in_one_line_what_happened() {
        let (mut app, _rx) = app_with(vec![]);
        app.on_msg(Msg::Closed { name: "demo".into(), result: Ok(Closed::Removed) });
        assert_eq!(app.flash.as_ref().map(|f| (f.text.as_str(), f.error)), Some(("demo closed", false)));
        app.on_msg(Msg::Closed { name: "demo".into(), result: Ok(Closed::LeftFleet) });
        assert!(app.flash.as_ref().is_some_and(|f| !f.error && f.text.contains("close its terminal with /exit")));
        app.on_msg(Msg::Closed { name: "demo".into(), result: Err(i18n::close_reason("busy", None)) });
        assert_eq!(app.flash.as_ref().map(|f| (f.text.as_str(), f.error)), Some(("demo was not closed: it is working; wait until it stops", true)));
    }

}
