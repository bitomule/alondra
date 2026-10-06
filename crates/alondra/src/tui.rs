mod agents;
mod draw;
mod dropped;
mod files;
mod inbox;
mod input;
mod keys;
mod md;
mod tasks;
mod usage_banner;
mod view_feed;
mod view_render;
mod work;

#[cfg(test)]
#[path = "tui/tests/parity.rs"]
mod parity;
#[cfg(test)]
#[path = "tui/tests/screens.rs"]
mod screens;
#[cfg(test)]
mod views_tests;

#[derive(Debug)]
enum ViewActionReply {
    Applied(String),
    Changed {
        message: String,
        action_id: String,
        button: String,
    },
}

fn view_action_result(value: serde_json::Value) -> std::result::Result<ViewActionReply, String> {
    match value["outcome"].as_str() {
        Some("applied") => Ok(ViewActionReply::Applied(
            value["stamp"]
                .as_str()
                .unwrap_or(tr!("Done", "Hecho"))
                .to_owned(),
        )),
        Some("changed") => {
            let action_id = value["confirm"]["action_id"].as_str().ok_or_else(|| {
                tr!(
                    "The action changed; refresh the view",
                    "La acción ha cambiado; actualiza la vista"
                )
                .to_owned()
            })?;
            let button = value["confirm"]["button"]
                .as_str()
                .unwrap_or(tr!("Send anyway", "Enviar igualmente"));
            Ok(ViewActionReply::Changed {
                message: value["message"]
                    .as_str()
                    .unwrap_or(tr!("The item changed", "El elemento ha cambiado"))
                    .to_owned(),
                action_id: action_id.to_owned(),
                button: button.to_owned(),
            })
        }
        Some("rejected" | "no_longer_available") => Err(value["message"]
            .as_str()
            .unwrap_or(tr!(
                "Action could not be completed",
                "No se pudo completar la acción"
            ))
            .to_owned()),
        _ => Err(tr!(
            "Unexpected action response",
            "Respuesta de acción inesperada"
        )
        .to_owned()),
    }
}

fn diff_reader_text(diff: crate::client::PullRequestDiff, truncated_notice: &str) -> String {
    if diff.truncated {
        format!("{}\n\n{truncated_notice}", diff.text)
    } else {
        diff.text
    }
}

use std::collections::{HashMap, HashSet};
use std::io::stdout;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use alondra_core::domain::{
    AgentId, AttachmentRef, AvailableAction, Command, DeliveryTarget, Interaction, InteractionId,
    PrId, Revision, TaskId,
};
use alondra_core::now_millis;
use alondra_core::views::{Status as ViewStatus, View as ViewDocument};
use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use crate::client::Client;
use crate::i18n;
use crate::mcp::urlencode;
use crate::ops;
use crate::runtime::{Activity, Runtime, TaskHistory};
use dropped::{Chip, Upload};
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
const ACTIVITY_EVERY_MS: i64 = 60_000;
/// Threads move with every tool call, which does not move the domain: the open agent asks again this often.
const THREADS_EVERY_MS: i64 = 10_000;
const PREVIEW_PIXELS: u32 = 480;

/// An inbox item by the id the daemon gave it.
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
        let pairs: Vec<(String, String)> = self
            .comments
            .iter()
            .map(|c| (c.quote.clone(), c.text.clone()))
            .collect();
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

/// A piece of what the reader shows: Markdown, or a stored file loaded by its content route.
pub enum Part {
    Text(String),
    File { name: String, route: String },
}

pub struct Reader {
    pub key: Key,
    pub agent: String,
    pub title: String,
    pub kind: String,
    pub glyph: &'static str,
    pub color: ratatui::style::Color,
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
        if self
            .ranges
            .get(self.cursor)
            .is_some_and(|r| fits(r) || covers(r))
        {
            return;
        }
        let ok = |i: &usize| {
            self.blocks.get(*i).is_some_and(Block::commentable)
                && self.ranges.get(*i).is_some_and(|r| fits(r) || covers(r))
        };
        let n = self.ranges.len();
        let found = if d > 0 {
            (0..n).find(ok)
        } else {
            (0..n).rev().find(ok)
        };
        if let Some(i) = found {
            self.cursor = i;
        } else if let Some(i) = (0..n).find(|&i| {
            self.blocks.get(i).is_some_and(Block::commentable) && self.ranges[i].0 >= top
        }) {
            self.cursor = i;
        }
    }

    fn matches(&self) -> Vec<usize> {
        let Some(q) = self
            .find
            .as_ref()
            .map(Find::query)
            .filter(|q| !q.is_empty())
        else {
            return vec![];
        };
        (0..self.blocks.len())
            .filter(|&i| {
                self.blocks[i].commentable() && self.blocks[i].plain().to_lowercase().contains(&q)
            })
            .collect()
    }

    fn jump(&mut self, forward: bool, include_current: bool) -> bool {
        let hits = self.matches();
        let c = self.cursor;
        let next = if forward {
            hits.iter()
                .find(|&&i| i > c || (include_current && i == c))
                .or(hits.first())
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
    ViewAction {
        action_id: String,
        label: String,
        data_epoch: String,
        option_id: Option<String>,
        required: bool,
        allow_attachments: bool,
    },
    /// A message to an agent or the orchestrator; the text is filled in when it is sent.
    Message {
        key: Option<Key>,
        message: ops::Message,
        name: String,
        title: String,
    },
    /// A server form: new work, a schedule. `view` is the view its fields come from.
    Form {
        view: String,
    },
    Comment {
        block: usize,
        quote: String,
    },
    General {
        send: bool,
    },
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
            Target::ViewAction {
                action_id,
                option_id,
                ..
            } => Some(format!(
                "view-action:{action_id}:{}",
                option_id.as_deref().unwrap_or("")
            )),
            Target::Message { message: m, .. } => {
                let (agent, interaction, pr) = (
                    m.agent.as_ref().map_or("orchestrator", |a| a.0.as_str()),
                    m.interaction.as_ref().map_or("", |i| i.0.as_str()),
                    m.pr.as_ref().map_or("", |p| p.0.as_str()),
                );
                Some(format!("message:{:?}:{agent}:{interaction}:{pr}", m.kind))
            }
            Target::Form { view } => Some(format!("form:{view}")),
            Target::Comment { .. } | Target::General { .. } => None,
        }
    }
}

/// What the daemon found for a search nothing local matched.
pub enum Remote {
    Idle,
    Pending,
    Done {
        query: String,
        via: String,
        sessions: Vec<String>,
    },
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

enum Msg {
    ViewHome(Result<ViewDocument, String>),
    ViewCard {
        key: String,
        result: Result<ViewDocument, String>,
    },
    ViewFeed(Result<crate::client::ViewsFeed, String>),
    ViewAction {
        request: Value,
        result: Result<ViewActionReply, String>,
    },
    Offline,
    SyncError(String),
    TaskActions {
        task: TaskId,
        revision: Revision,
        result: Result<Vec<AvailableAction>, String>,
    },
    /// The daemon's runtime readings; `None` from one that does not serve them.
    Runtime(Option<Box<Runtime>>),
    /// The local daemon's own readings: machine, usage and update state.
    Daemon(Option<Box<Runtime>>),
    /// A request that is not a domain command: what to flash, or why it failed.
    Posted {
        key: Option<Key>,
        result: Result<String, String>,
    },
    Closed {
        name: String,
        result: Result<Closed, String>,
    },
    File {
        route: String,
        result: Result<files::FileBody, String>,
    },
    Uploaded {
        seq: u64,
        result: Result<AttachmentRef, String>,
    },
    Preview {
        seq: u64,
        image: image::DynamicImage,
    },
    FormView {
        view: String,
        project: Option<String>,
        result: Result<ViewDocument, String>,
    },
    Issues(Result<(Vec<alondra_core::issues::Issue>, Option<String>), String>),
    Threads {
        agent: AgentId,
        threads: Option<Vec<Activity>>,
    },
    History {
        task: TaskId,
        result: Result<TaskHistory, String>,
    },
}

/// How a close that alondrad accepted ended: a background job is gone; an interactive session
/// only left the fleet and its terminal is still open.
#[derive(Debug, PartialEq)]
pub enum Closed {
    Removed,
    LeftFleet,
}

pub enum Flow {
    Go,
    Quit,
    Attach(String),
    /// The PR in tuicr on its local clone; its comments come back as feedback on `key`.
    Review {
        key: Key,
        pr: PrId,
        number: u64,
        dir: std::path::PathBuf,
    },
}

pub struct App {
    pub home_view: Option<ViewDocument>,
    pub card_view: Option<ViewDocument>,
    /// The numbered list a card's `m` or a menu button opened over its buttons.
    pub card_menu: Option<view_render::Menu>,
    pub view_status: Option<ViewStatus>,
    pub view_data_epoch: Option<String>,
    pub connected: bool,
    pub loaded: bool,
    pub offline: bool,
    pub screen: Screen,
    pub schedules_open: bool,
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
    /// A step that takes a second press within a few seconds, until when, and the key that
    /// confirms it; any other key starts over.
    confirm: Option<(String, i64, KeyCode)>,
    pending_changed_action: Option<(Value, String)>,
    pressed: KeyCode,
    /// Every request that is not a domain command, as the tests read what it would have sent.
    #[cfg(test)]
    posted: std::cell::RefCell<Vec<(String, Value)>>,
    pub search: Option<Search>,
    pub composer: Option<Composer>,
    pub reader: Option<Reader>,
    pub reviews: HashMap<Key, Review>,
    drafts: HashMap<String, (String, Vec<Chip>)>,
    chip_seq: u64,
    /// What alondrad knows about the Mac and its sessions beyond the domain.
    pub runtime: Option<Runtime>,
    /// The local daemon's machine, usage and update readings from `/daemon`.
    pub daemon: Option<Runtime>,
    /// Stored files by content route: text to read, an image to draw, or bytes to leave to the Mac.
    pub files: HashMap<String, Result<files::FileBody, String>>,
    loading: HashSet<String>,
    /// How images are drawn in this terminal: Kitty graphics in Ghostty and Kitty, iTerm2 or sixel
    /// where supported, colour half-blocks elsewhere. Found once, when the TUI starts.
    pub picker: Option<ratatui_image::picker::Picker>,
    pub images: HashMap<String, ratatui_image::protocol::StatefulProtocol>,
    /// The preview of each image attached in the composer, by chip.
    pub previews: HashMap<u64, ratatui_image::protocol::StatefulProtocol>,
    /// What each thread of the selected agent does, as its own route last said.
    pub threads: Option<(AgentId, Vec<Activity>)>,
    threads_asked: Option<(AgentId, Revision, i64)>,
    pub histories: HashMap<TaskId, TaskHistory>,
    history_asked: HashSet<(TaskId, Revision)>,
    activity_told: i64,
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
    /// Interactions read one by one as the user opens them; the projection no longer carries them.
    pub interactions: HashMap<InteractionId, Interaction>,
    pub cmdline: Option<TextInput>,
    keymap: Keymap,
    last_clock: i64,
    tx: SyncSender<Msg>,
    open_views: std::sync::Arc<std::sync::Mutex<HashMap<String, String>>>,
    card_view_asked: Option<String>,
    screen_view_asked: Option<String>,
    view_navigation: Vec<String>,
    compose_orchestrator_on_load: bool,
    reader_after_view: Option<String>,
}

pub fn run() -> Result<()> {
    let (tx, rx) = mpsc::sync_channel::<Msg>(64);
    crate::attach::reset_terminal_modes();
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let mut app = App::new(tx);
    app.picker = Some(
        ratatui_image::picker::Picker::from_query_stdio()
            .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks()),
    );
    app.start_view_feed();
    let res = main_loop(&mut terminal, app, rx);
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::attach::reset_terminal_modes();
    res
}

fn background_sync_error(error: &anyhow::Error, prefix: &str) -> Msg {
    if error.is::<crate::account::AccountBusy>() {
        Msg::Offline
    } else {
        Msg::SyncError(format!("{prefix}{error}"))
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
            app.note_activity();
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
                                app.fail(trf!(
                                    "Could not open its terminal: {e}",
                                    "No pude abrir su terminal: {e}"
                                ));
                            }
                            repaint(terminal, &mut app)?;
                            break;
                        }
                        Flow::Review {
                            key,
                            pr,
                            number,
                            dir,
                        } => {
                            if crate::service_client::target().is_some() {
                                app.fail(tr!("tuicr runs on the destination Mac; the diff opens here instead", "tuicr funciona en el Mac de destino; aquí se abre el diff"));
                                break;
                            }
                            draw(terminal, &mut app)?;
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            let res = crate::attach::run_tool(
                                "tuicr",
                                &["--stdout", "--no-update-check", "pr", &number.to_string()],
                                &dir,
                                crate::attach::Over::Tui,
                            );
                            crossterm::terminal::enable_raw_mode()?;
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            app.reviewed(key, pr, number, res);
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

fn validate_view_fields(fields: &[alondra_core::views::Field], values: &Value) -> Result<()> {
    use alondra_core::views::FieldKind;
    for field in fields {
        let label = &field.label;
        if field
            .visible_when
            .as_ref()
            .is_some_and(|condition| {
                let actual = values.get(&condition.field);
                !condition.equals.as_ref().is_some_and(|expected| actual == Some(expected))
                    && !actual.is_some_and(|value| condition.any_of.contains(value))
            })
        {
            continue;
        }
        let value = values.get(&field.id);
        let missing = value.is_none_or(|value| match value {
            Value::Null => true,
            Value::String(text) => text.trim().is_empty(),
            Value::Array(items) => items.is_empty(),
            _ => false,
        });
        if field.required && missing {
            anyhow::bail!(trf!("{label} is required", "{label} es obligatorio"));
        }
        if missing {
            continue;
        }
        let value = value.expect("nonmissing value");
        match &field.kind {
            FieldKind::Choice { items }
                if !value
                    .as_str()
                    .is_some_and(|choice| items.iter().any(|item| item.id == choice)) =>
            {
                anyhow::bail!(trf!(
                    "{label}: choose one of the listed values",
                    "{label}: elige uno de los valores de la lista"
                ))
            }
            FieldKind::Stepper { min, max }
                if !value
                    .as_i64()
                    .is_some_and(|number| (*min..=*max).contains(&number)) =>
            {
                anyhow::bail!(trf!(
                    "{label}: expected a number from {min} to {max}",
                    "{label}: se espera un número entre {min} y {max}"
                ))
            }
            FieldKind::Toggle if !value.is_boolean() => anyhow::bail!(trf!(
                "{label}: expected true or false",
                "{label}: se espera true o false"
            )),
            FieldKind::Time
                if !value.as_str().is_some_and(|time| {
                    time.split_once(':').is_some_and(|(hour, minute)| {
                        hour.parse::<u8>().is_ok_and(|hour| hour <= 23)
                            && minute.parse::<u8>().is_ok_and(|minute| minute <= 59)
                    })
                }) =>
            {
                anyhow::bail!(trf!("{label}: expected HH:MM", "{label}: se espera HH:MM"))
            }
            FieldKind::Picker { source }
                if source == "issue_picker"
                    && !value
                        .as_str()
                        .is_some_and(|issue| alondra_core::issues::parse_ref(issue).is_some()) =>
            {
                anyhow::bail!(trf!(
                    "{label}: paste an issue link or owner/repo#number",
                    "{label}: pega un enlace de incidencia o owner/repo#número"
                ))
            }
            _ => {}
        }
    }
    Ok(())
}

/// The Mac's default app for a URL or file, through `open`. Tests never open one.
fn open_with_mac(target: &str) {
    if cfg!(test) {
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(target)
        .spawn();
}

/// A stored file handed to the Mac's app for it: always a private local copy, never a path of
/// the daemon's, so a remote destination and the local Mac read the same bytes.
fn open_stored(name: &str, route: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let bytes = Client::for_tui(Duration::from_secs(120)).content(route)?;
    let suffix: String = crate::service_client::random::<16>()?
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = std::env::temp_dir().join(format!("alondra-attachment-{suffix}"));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let name = std::path::Path::new(name)
        .file_name()
        .filter(|n| !n.is_empty())
        .unwrap_or(std::ffi::OsStr::new("attachment"));
    let local = dir.join(name);
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&local)?;
    std::io::Write::write_all(&mut file, &bytes)?;
    open_with_mac(&local.to_string_lossy());
    Ok(())
}

/// Where to review a PR: the agent's own checkout, else `~/Projects/<repo name>`.
fn clone_of(agent_cwd: Option<&str>, repository: &str) -> Option<std::path::PathBuf> {
    let is_repo = |p: &std::path::Path| p.join(".git").exists();
    if let Some(cwd) = agent_cwd
        .map(std::path::PathBuf::from)
        .filter(|p| is_repo(p))
    {
        return Some(cwd);
    }
    let name = repository.rsplit('/').next()?;
    let home = std::env::var("HOME").ok()?;
    Some(std::path::Path::new(&home).join("Projects").join(name)).filter(|p| is_repo(p))
}

/// A tuicr export carries numbered comments anchored to files (`1. \`src/a.rs:42\` - …`).
fn has_comments(export: &str) -> bool {
    export.lines().any(|l| {
        let t = l.trim_start();
        t.split_once(". ")
            .is_some_and(|(n, rest)| n.parse::<u32>().is_ok() && rest.starts_with('`'))
    })
}

impl App {
    fn new(tx: SyncSender<Msg>) -> Self {
        App {
            home_view: None,
            card_view: None,
            card_menu: None,
            view_status: None,
            view_data_epoch: None,
            connected: false,
            loaded: false,
            offline: false,
            screen: Screen::Inbox,
            schedules_open: false,
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
            pending_changed_action: None,
            pressed: KeyCode::Null,
            #[cfg(test)]
            posted: Default::default(),
            search: None,
            composer: None,
            reader: None,
            reviews: HashMap::new(),
            drafts: HashMap::new(),
            chip_seq: 0,
            runtime: None,
            daemon: None,
            files: HashMap::new(),
            loading: HashSet::new(),
            picker: cfg!(test).then(ratatui_image::picker::Picker::halfblocks),
            images: HashMap::new(),
            previews: HashMap::new(),
            threads: None,
            threads_asked: None,
            histories: HashMap::new(),
            history_asked: HashSet::new(),
            activity_told: 0,
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
            interactions: HashMap::new(),
            cmdline: None,
            keymap: Keymap::default(),
            last_clock: now_millis(),
            tx,
            open_views: std::sync::Arc::new(std::sync::Mutex::new(HashMap::new())),
            card_view_asked: None,
            screen_view_asked: None,
            view_navigation: vec![],
            compose_orchestrator_on_load: false,
            reader_after_view: None,
        }
    }

    fn start_view_feed(&self) {
        let tx = self.tx.clone();
        let open_views = self.open_views.clone();
        view_feed::start(tx, open_views);
    }

    fn load_selected_view(&mut self) {
        let Some(key) = self.selected_view_key() else {
            return;
        };
        if self.home_view.as_ref().is_some_and(|view| view.key == key)
            || self.card_view.as_ref().is_some_and(|view| view.key == key)
            || self.card_view_asked.as_deref() == Some(&key)
        {
            return;
        }
        self.card_view_asked = Some(key.clone());
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key, result });
        });
    }

    fn selected_view_key(&self) -> Option<String> {
        if let Some(key) = self.view_navigation.last() {
            return Some(key.clone());
        }
        if let (Some(selected), Some(view)) = (self.sel.as_ref(), self.home_view.as_ref()) {
            let entry = view_render::entries(view)
                .into_iter()
                .find(|(id, _, _)| *id == selected.0);
            if let Some((_, _, link)) = entry {
                return match link {
                    Some(alondra_core::views::Link::View { key }) => Some(key),
                    _ => None,
                };
            }
        }
        if self.screen == Screen::Tasks {
            return match self.task_sel.as_ref()? {
                tasks::TaskKey::Task(id) => Some(format!("task:{}", id.0)),
                tasks::TaskKey::Request(id) => Some(format!("request:{}", id.0)),
                tasks::TaskKey::Finished | tasks::TaskKey::Archived => None,
            };
        }
        let selected = self.sel.as_ref()?;
        (self.screen == Screen::Inbox).then(|| format!("card:{}", selected.0))
    }

    fn screen_view_key(&self) -> &str {
        match self.screen {
            Screen::Inbox => "home",
            Screen::Tasks if self.schedules_open => "schedules",
            Screen::Tasks => "tasks",
            Screen::Fleet => "fleet",
        }
    }

    fn load_screen_view(&mut self) {
        let key = self.screen_view_key().to_owned();
        if self.home_view.as_ref().is_some_and(|view| view.key == key)
            || self.screen_view_asked.as_deref() == Some(&key)
        {
            return;
        }
        self.screen_view_asked = Some(key.clone());
        self.home_view = None;
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewHome(result));
        });
    }

    fn sync_open_views(&self) {
        let mut open = self
            .open_views
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        open.clear();
        if let Some(home) = &self.home_view {
            open.insert(home.key.clone(), home.version.clone());
        }
        if let Some(card) = self.card_view.as_ref() {
            open.insert(card.key.clone(), card.version.clone());
        }
    }

    pub(super) fn navigate_view(&mut self, key: &str) {
        self.reader_after_view = None;
        self.view_navigation.push(key.to_owned());
        self.card_view = None;
        self.card_view_asked = None;
        self.detail_scroll = 0;
        self.load_selected_view();
        self.sync_open_views();
    }

    fn submit_view_request(&mut self, request: Value) {
        #[cfg(test)]
        self.posted
            .borrow_mut()
            .push(("/actions".into(), request.clone()));
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(30))
                .post("/actions", &request)
                .map_err(|error| format!("{error:#}"))
                .and_then(view_action_result);
            let _ = tx.send(Msg::ViewAction { request, result });
        });
    }

    fn confirm_changed_action(&mut self) {
        let Some((request, action_id)) = self.pending_changed_action.take() else {
            return;
        };
        let data_epoch = request["data_epoch"].as_str().unwrap_or_default();
        let option_id = request["option_id"].as_str();
        let input = request["input"].clone();
        let fresh = Client::action_body(data_epoch, &action_id, option_id, input);
        self.submit_view_request(fresh);
    }

    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            error: false,
            at: now_millis(),
        });
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            error: true,
            at: now_millis(),
        });
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
        if self
            .confirm
            .as_ref()
            .is_some_and(|(s, until, _)| s == step && now < *until)
        {
            self.confirm = None;
            return true;
        }
        self.confirm = Some((step.to_string(), now + CONFIRM_MS, self.pressed));
        self.fail(ask);
        false
    }

    /// Tells alondrad the user is at the TUI, at most once a minute, so it holds back pushes.
    fn note_activity(&mut self) {
        let now = now_millis();
        if now - self.activity_told < ACTIVITY_EVERY_MS {
            return;
        }
        self.activity_told = now;
        #[cfg(test)]
        self.posted
            .borrow_mut()
            .push((ops::PRESENCE.into(), json!({"client": "tui"})));
        self.spawn(|_| {
            let _ = Client::for_tui(Duration::from_secs(5))
                .post(ops::PRESENCE, &json!({"client": "tui"}));
        });
    }

    /// A request that is not a domain command. It answers as its own message; a route the
    /// daemon does not serve yet says so instead of failing silently.
    fn op(&mut self, key: Option<Key>, path: String, body: Value, ok: String) {
        #[cfg(test)]
        self.posted.borrow_mut().push((path.clone(), body.clone()));
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(15))
                .post(&path, &body)
                .map(|v| {
                    crate::delivery::response_notice(&v, crate::service_client::target())
                        .unwrap_or(ok)
                })
                .map_err(|e| {
                    if crate::client::missing(&e) {
                        trf!("{e} ({path})", "{e} ({path})")
                    } else {
                        format!("{e:#}")
                    }
                });
            let _ = tx.send(Msg::Posted { key, result });
        });
    }

    /// Sends a message to an agent or the orchestrator. Only a reply that reads the item takes it
    /// out of the inbox; anything else it is about stays until its agent acts.
    fn message(&mut self, key: Option<Key>, message: ops::Message, ok: String) {
        let (path, body) = (message.path(), message.body());
        if let Some(k) = key.clone().filter(|_| message.mark_read) {
            self.hide(k);
        }
        self.op(key, path, body, ok);
    }

    /// Loads a stored file once, by its content route.
    fn fetch(&mut self, route: &str) {
        if self.files.contains_key(route) || !self.loading.insert(route.to_string()) {
            return;
        }
        let route = route.to_string();
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(30))
                .content(&route)
                .map(files::FileBody::classify)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::File { route, result });
        });
    }

    fn fetch_diff(&mut self, route: &str, id: &str, expected_head: &str) {
        if self.files.contains_key(route) || !self.loading.insert(route.to_string()) {
            return;
        }
        let route = route.to_string();
        let id = id.to_string();
        let expected_head = expected_head.to_string();
        let truncated_notice = tr!(
            "Diff truncated. Open the PR on GitHub to read the rest.",
            "Diferencias incompletas. Abre la PR en GitHub para leer el resto."
        )
        .to_string();
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(30))
                .pull_request_diff(&id, &expected_head)
                .map(|diff| files::FileBody::Text(diff_reader_text(diff, &truncated_notice)))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::File { route, result });
        });
    }

    /// What tuicr left on stdout when the user quit it: the comments go to the agent as
    /// feedback on the delivery that presents the PR; without comments nothing is sent.
    fn reviewed(&mut self, key: Key, pr: PrId, number: u64, res: Result<String>) {
        match res {
            Ok(text) if has_comments(&text) => {
                if let Some(view) = self
                    .card_view
                    .as_ref()
                    .filter(|view| view.key == self.selected_view_key().unwrap_or_default())
                {
                    let action = view_render::actions(view).into_iter().find(|action| {
                        action.enabled
                            && alondra_core::views::action_id::ActionId::parse(&action.id)
                                .is_ok_and(|id| {
                                    id.verb == alondra_core::views::action_id::Verb::Changes
                                })
                    });
                    if let Some(action) = action {
                        let request = Client::action_body(
                            &view.data_epoch,
                            &action.id,
                            None,
                            json!({"text": text.trim()}),
                        );
                        self.submit_view_request(request);
                    } else {
                        self.fail(tr!(
                            "Request changes is unavailable for this PR",
                            "No se pueden solicitar cambios para esta PR"
                        ));
                    }
                } else {
                    self.fail(tr!(
                        "Request changes is unavailable for this PR",
                        "No se pueden solicitar cambios para esta PR"
                    ));
                }
            }
            Ok(_) => self.flash(trf!(
                "PR #{number} reviewed without comments",
                "PR #{number} revisado sin comentarios"
            )),
            Err(e)
                if e.to_string().contains("No such file")
                    || e.to_string().contains("could not launch")
                    || e.to_string().contains("no pude lanzar") =>
            {
                self.fail(tr!(
                    "tuicr is not installed: brew install tuicr",
                    "tuicr no está instalado: brew install tuicr"
                ));
            }
            Err(e) => self.fail(trf!("tuicr: {e}", "tuicr: {e}")),
        }
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

    /// Copies a dropped file into Alondra's storage while the user keeps writing.
    fn attach(&mut self, path: std::path::PathBuf) {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();
        let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if bytes > crate::client::MAX_ATTACHMENT_BYTES {
            let size = files::size(bytes as usize);
            self.fail(trf!(
                "{name} is {size}: attachments go up to 20 MB",
                "{name} ocupa {size}: los adjuntos llegan hasta 20 MB"
            ));
            return;
        }
        self.chip_seq += 1;
        let seq = self.chip_seq;
        let Some(c) = self.composer.as_mut() else {
            return;
        };
        let chip = Chip {
            seq,
            name: name.clone(),
            bytes,
            upload: Upload::Sending,
        };
        let preview = self.picker.is_some() && chip.image();
        c.chips.push(chip);
        self.spawn(move |tx| {
            if preview && let Ok(image) = image::open(&path) {
                let _ = tx.send(Msg::Preview {
                    seq,
                    image: image.thumbnail(PREVIEW_PIXELS, PREVIEW_PIXELS),
                });
            }
            let result = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    Client::for_tui(Duration::from_secs(120))
                        .upload(&name, &bytes)
                        .map_err(|e| {
                            let route = crate::client::UPLOADS;
                            if crate::client::missing(&e) {
                                trf!("{e} ({route})", "{e} ({route})")
                            } else {
                                format!("{e:#}")
                            }
                        })
                });
            let _ = tx.send(Msg::Uploaded { seq, result });
        });
    }

    fn uploaded(&mut self, seq: u64, result: Result<AttachmentRef, String>) {
        let chips = self
            .composer
            .iter_mut()
            .flat_map(|c| c.chips.iter_mut())
            .chain(self.drafts.values_mut().flat_map(|(_, c)| c.iter_mut()))
            .chain(self.reviews.values_mut().flat_map(|r| r.chips.iter_mut()));
        let mut failed = None;
        for chip in chips.filter(|c| c.seq == seq) {
            match &result {
                Ok(a) => chip.upload = Upload::Stored(a.clone()),
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
            for r in self.reviews.values_mut() {
                r.chips.retain(|c| c.seq != seq);
            }
            self.previews.remove(&seq);
            self.fail(trf!(
                "Could not attach {name}: {e}",
                "No se pudo adjuntar {name}: {e}"
            ));
        }
    }

    fn reconcile(&mut self) {
        if self.home_view.is_none() {
            self.load_screen_view();
            return;
        }
        let keys = self.inbox_keys();
        let previous = self.sel.clone();
        match self
            .sel
            .as_ref()
            .and_then(|selected| keys.iter().position(|key| key == selected))
        {
            Some(index) => self.sel_idx = index,
            None => {
                self.sel_idx = self.sel_idx.min(keys.len().saturating_sub(1));
                self.sel = keys.get(self.sel_idx).cloned();
            }
        }
        if self.sel != previous {
            self.detail_scroll = 0;
            self.card_view = None;
            self.card_view_asked = None;
        }
        self.load_selected_view();
        self.sync_open_views();
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::ViewHome(Ok(view)) => {
                if view.key != self.screen_view_key() {
                    return;
                }
                self.screen_view_asked = None;
                self.view_data_epoch = Some(view.data_epoch.clone());
                self.view_status = Some(view.status.clone());
                self.home_view = Some(view);
                self.loaded = true;
                self.connected = true;
                self.offline = false;
                self.reconcile();
            }
            Msg::ViewHome(Err(error)) => {
                self.connected = false;
                self.offline = true;
                if self.home_view.is_none() {
                    self.screen_view_asked = None;
                    self.fail(error);
                }
            }
            Msg::ViewCard {
                key,
                result: Ok(view),
            } => {
                if self.selected_view_key().as_deref() == Some(&key) {
                    if key.starts_with("search") {
                        if let Some(search) = self.search.as_mut() {
                            let query = search.query();
                            let sessions = view_render::rows(&view)
                                .into_iter()
                                .map(|(row, _)| row.id.clone())
                                .collect();
                            search.remote = Remote::Done {
                                query,
                                via: "Views".into(),
                                sessions,
                            };
                        }
                    }
                    self.card_view_asked = None;
                    self.view_data_epoch = Some(view.data_epoch.clone());
                    self.card_view = Some(view);
                    if self.reader_after_view.as_deref() == Some(&key) {
                        self.reader_after_view = None;
                        self.open_reader();
                    }
                    if key == "node:orchestrator" && self.compose_orchestrator_on_load {
                        self.compose_orchestrator_on_load = false;
                        let message = self.card_view.as_ref().and_then(|view| {
                            view_render::actions(view).into_iter().find(|action| {
                                action.id.starts_with("msg:")
                            })
                        });
                        if let Some(key) = message
                            .as_ref()
                            .and_then(|action| view_render::action_hint(action).chars().next())
                        {
                            let _ = self
                                .view_action_key(KeyEvent::new(KeyCode::Char(key), KeyModifiers::NONE));
                        }
                    }
                }
            }
            Msg::ViewCard {
                key,
                result: Err(error),
            } => {
                if self.card_view_asked.as_deref() == Some(&key) {
                    if key.starts_with("search") {
                        if let Some(search) = self.search.as_mut() {
                            search.remote = Remote::Failed(error.clone());
                        }
                    }
                    self.card_view_asked = None;
                    if self.reader_after_view.as_deref() == Some(&key) {
                        self.reader_after_view = None;
                    }
                    if key == "node:orchestrator" {
                        self.compose_orchestrator_on_load = false;
                    }
                    self.fail(error);
                }
            }
            Msg::ViewFeed(Ok(feed)) => {
                self.view_data_epoch = Some(feed.data_epoch);
                self.view_status = Some(feed.status);
                if feed.reset {
                    self.home_view = None;
                    self.card_view = None;
                    self.card_view_asked = None;
                    self.screen_view_asked = None;
                    self.load_screen_view();
                }
                self.connected = true;
                self.offline = false;
            }
            Msg::ViewFeed(Err(error)) => {
                self.connected = false;
                self.offline = true;
                if self.view_status.is_none() {
                    self.fail(error);
                }
            }
            Msg::ViewAction {
                request: _,
                result: Ok(ViewActionReply::Applied(message)),
            } => {
                self.pending_changed_action = None;
                self.flash(message);
            }
            Msg::ViewAction {
                request,
                result:
                    Ok(ViewActionReply::Changed {
                        message,
                        action_id,
                        button,
                    }),
            } => {
                self.pending_changed_action = Some((request, action_id));
                self.flash(trf!(
                    "{message} · press y to {button}",
                    "{message} · pulsa y para {button}"
                ));
            }
            Msg::ViewAction {
                request: _,
                result: Err(error),
            } => {
                self.pending_changed_action = None;
                self.fail(error);
            }
            Msg::SyncError(error) => self.fail(error),
            Msg::Offline => {
                self.connected = false;
                self.offline = true;
            }
            Msg::TaskActions {
                task,
                revision,
                result,
            } => match result {
                Ok(actions) => {
                    self.task_actions.insert(task, (revision, actions));
                }
                Err(error) => {
                    self.task_actions_asked.remove(&(task, revision));
                    self.fail(error);
                }
            },
            Msg::Runtime(runtime) => self.runtime = runtime.map(|r| *r),
            Msg::Daemon(daemon) => self.daemon = daemon.map(|d| *d),
            Msg::Posted { key, result } => match result {
                Ok(text) => {
                    if let Some(k) = key {
                        self.reviews.remove(&k);
                    }
                    self.flash(text);
                }
                Err(e) => {
                    if let Some(k) = key {
                        self.hidden.remove(&k);
                    }
                    self.fail(trf!(
                        "Not confirmed: {e}. Check before repeating.",
                        "Sin confirmar: {e}. Revísalo antes de repetir."
                    ));
                }
            },
            Msg::Closed { name, result } => self.closed(&name, result),
            Msg::File { route, result } => {
                self.loading.remove(&route);
                if let (Ok(files::FileBody::Image(img)), Some(picker)) = (&result, &self.picker) {
                    self.images
                        .insert(route.clone(), picker.new_resize_protocol((**img).clone()));
                }
                self.files.insert(route.clone(), result);
                if self.reader.as_ref().is_some_and(|r| {
                    r.parts
                        .iter()
                        .any(|p| matches!(p, Part::File { route: fr, .. } if *fr == route))
                }) {
                    self.rebuild_reader();
                }
            }
            Msg::Uploaded { seq, result } => self.uploaded(seq, result),
            Msg::Preview { seq, image } => {
                if let Some(p) = self.picker.as_ref() {
                    self.previews.insert(seq, p.new_resize_protocol(image));
                }
            }
            Msg::FormView {
                view,
                project,
                result,
            } => {
                let Some(form) = self
                    .composer
                    .as_mut()
                    .and_then(|c| c.form.as_mut())
                    .filter(|form| form.view == view && form.reload_value() == project)
                else {
                    return;
                };
                match result {
                    Ok(document) => {
                        let first = !form.loaded;
                        form.loaded(&document);
                        if first
                            && let Some(text) = form
                                .primary()
                                .and_then(|field| field.value.as_str())
                                .filter(|text| !text.is_empty())
                                .map(str::to_owned)
                            && let Some(composer) = self.composer.as_mut()
                            && composer.input.text().is_empty()
                        {
                            composer.input = TextInput::with(&text);
                        }
                    }
                    Err(e) => {
                        form.failed(e.clone());
                        self.fail(trf!(
                            "Could not load the form: {e}",
                            "No se pudo cargar el formulario: {e}"
                        ));
                    }
                }
            }
            Msg::Issues(result) => {
                if let Some(f) = self.composer.as_mut().and_then(|c| c.form.as_mut()) {
                    f.issues_loaded(result);
                }
            }
            Msg::Threads {
                agent,
                threads: Some(threads),
            } => self.threads = Some((agent, threads)),
            Msg::Threads { threads: None, .. } => {}
            Msg::History { task, result } => match result {
                Ok(history) => {
                    self.histories.insert(task, history);
                }
                Err(e) => self.fail(trf!("Task history: {e}", "Historia de la tarea: {e}")),
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
        if self.hidden.values().any(|at| now - at >= HIDE_GRACE_MS) {
            dirty = true;
        }
        self.load_threads();
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
            search.remote = Remote::Pending;
            self.update_search_view();
        }
    }

    fn update_search_view(&mut self) {
        let Some(query) = self.search.as_ref().map(Search::query) else {
            return;
        };
        self.agent_idx = 0;
        self.agent_sel = None;
        let key = if query.is_empty() {
            "search".to_owned()
        } else {
            format!(
                "search?{}",
                url::form_urlencoded::Serializer::new(String::new())
                    .append_pair("q", &query)
                    .finish()
            )
        };
        if let Some(current) = self.view_navigation.last_mut() {
            *current = key;
            self.card_view = None;
            self.card_view_asked = None;
            self.load_selected_view();
            self.sync_open_views();
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Flow {
        if self
            .confirm
            .as_ref()
            .is_some_and(|(_, _, key)| *key != k.code)
        {
            self.confirm = None;
        }
        self.pressed = k.code;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Flow::Quit;
        }
        if self.pending_changed_action.is_some() {
            if k.code == KeyCode::Char('y') && k.modifiers.is_empty() {
                self.confirm_changed_action();
                return Flow::Go;
            }
            self.pending_changed_action = None;
        }
        if self.help {
            self.help_key(k);
            return Flow::Go;
        }
        if self.composer.is_some() {
            self.composer_key(k);
            return Flow::Go;
        }
        if self.card_menu.is_some() {
            return self.card_menu_key(k);
        }
        if self.schedules_open {
            if let Some(flow) = self.schedule_row_key(k) {
                return flow;
            }
            if let Some(flow) = self.view_action_key(k) {
                return flow;
            }
            let flow = self.schedule_key(k);
            self.load_selected_view();
            self.sync_open_views();
            return flow;
        }
        if self.cmdline.is_some() {
            return self.cmdline_key(k);
        }
        if self
            .reader
            .as_ref()
            .is_some_and(|r| r.find.as_ref().is_some_and(|f| f.editing))
        {
            self.find_key(k);
            return Flow::Go;
        }
        if self.screen == Screen::Fleet
            && self.reader.is_none()
            && self.search.as_ref().is_some_and(|s| s.editing)
        {
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
        if matches!(cmd, Cmd::Back) && !self.view_navigation.is_empty() {
            self.view_navigation.pop();
            self.card_view = None;
            self.card_view_asked = None;
            self.load_selected_view();
            self.sync_open_views();
            return Flow::Go;
        }
        match cmd {
            Cmd::Pending => {}
            Cmd::Other(k) => match k.code {
                KeyCode::Char('q') => return Flow::Quit,
                KeyCode::Tab => self.switch_screen(),
                KeyCode::BackTab => self.switch_screen_back(),
                KeyCode::Char('f') => self.show(if self.screen == Screen::Fleet {
                    Screen::Inbox
                } else {
                    Screen::Fleet
                }),
                KeyCode::Char('n') => self.compose(
                    Target::Form {
                        view: "new_request".into(),
                    },
                    "",
                ),
                KeyCode::Char('s') if self.screen == Screen::Tasks => {
                    self.schedules_open = true;
                    self.home_view = None;
                    self.screen_view_asked = None;
                    self.load_screen_view();
                    self.load_selected_view();
                    self.sync_open_views();
                }
                KeyCode::Char('M') => {
                    if let Some(flow) = self.view_action_key(k) {
                        return flow;
                    }
                    self.compose_orchestrator_on_load = true;
                    self.navigate_view("node:orchestrator");
                }
                KeyCode::Char('/') if self.screen == Screen::Fleet => {
                    self.search = Some(Search {
                        input: TextInput::default(),
                        editing: true,
                        typed_at: now_millis(),
                        remote: Remote::Idle,
                    });
                    self.navigate_view("search");
                }
                _ => {
                    if let Some(flow) = self.view_action_key(k) {
                        return flow;
                    }
                    return self.inbox_key(k);
                }
            },
            _ => {
                if let Some(flow) = self.view_action_key(k) {
                    return flow;
                }
                self.inbox_cmd(cmd);
            }
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
        self.card_menu = None;
        self.view_navigation.clear();
        self.compose_orchestrator_on_load = false;
        self.reader_after_view = None;
        self.schedules_open = false;
        self.focus = Pane::List;
        self.agent_zoom = false;
        self.task_zoom = false;
        self.threads_asked = None;
        self.card_view = None;
        self.card_view_asked = None;
        self.home_view = None;
        self.screen_view_asked = None;
        self.sel = None;
        self.sel_idx = 0;
        self.load_screen_view();
        self.load_selected_view();
        self.sync_open_views();
    }

    fn message_orchestrator(&mut self) {
        let name = tr!("the orchestrator", "el orquestador").to_string();
        let message = ops::Message::to(None, ops::MessageKind::Message, String::new());
        self.compose(
            Target::Message {
                key: None,
                message,
                title: String::new(),
                name,
            },
            "",
        );
    }

    fn reload_schedules(&mut self) {
        self.home_view = None;
        self.screen_view_asked = None;
        self.load_screen_view();
    }

    fn schedule_key(&mut self, k: KeyEvent) -> Flow {
        match k.code {
            KeyCode::Esc | KeyCode::Tab | KeyCode::Char('s') => {
                self.schedules_open = false;
                self.reload_schedules();
            }
            KeyCode::Char('n') => self.compose(
                Target::Form {
                    view: "schedule:new".into(),
                },
                "",
            ),
            KeyCode::Char('e') => {
                if let Some(view) = self.selected_view_key() {
                    self.compose(Target::Form { view }, "");
                }
            }
            KeyCode::Char('r') => self.reload_schedules(),
            _ => {
                let cmd = self.keymap.read(k);
                self.inbox_cmd(cmd);
            }
        }
        Flow::Go
    }

    fn help_key(&mut self, k: KeyEvent) {
        match self.keymap.read(k) {
            Cmd::Move(m) => {
                self.help_scroll =
                    keys::scroll(self.help_scroll, m, self.help_view.h, self.help_view.total)
            }
            Cmd::Pending => {}
            _ => self.help = false,
        }
    }

    fn cmdline_key(&mut self, k: KeyEvent) -> Flow {
        let Some(input) = self.cmdline.as_mut() else {
            return Flow::Go;
        };
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
                self.move_inbox(i);
            }
            other => self.fail(trf!(
                "Not a command: {other}  (try :q, :help, :inbox, :tasks, :agents, :<number>)",
                "No es un comando: {other}  (prueba :q, :help, :inbox, :tasks, :agents, :<número>)"
            )),
        }
        Flow::Go
    }

    fn compose(&mut self, target: Target, prefill: &str) {
        let (text, chips) = target
            .draft_key()
            .and_then(|k| self.drafts.remove(&k))
            .unwrap_or_else(|| (prefill.to_string(), vec![]));
        let view = match &target {
            Target::Form { view } => Some(view.clone()),
            _ => None,
        };
        let form = view.as_deref().map(work::OptionsForm::new);
        self.composer = Some(Composer {
            target,
            input: TextInput::with(&text),
            chips,
            form,
        });
        if let Some(view) = view {
            self.load_form(&view, None);
        }
    }

    /// Asks alondrad for the open issues, which it keeps for ten minutes unless `refresh`.
    fn load_issues(&self, refresh: bool) {
        self.spawn(move |tx| {
            let key = if refresh {
                "issue_picker?refresh=1"
            } else {
                "issue_picker"
            };
            let result = Client::for_tui(Duration::from_secs(40))
                .views(key, None)
                .map_err(|e| format!("{e:#}"))
                .map(|view| {
                    let issues = view
                        .sections
                        .iter()
                        .flat_map(|section| &section.items)
                        .filter_map(|item| {
                            let alondra_core::views::Item::Row(row) = item else {
                                return None;
                            };
                            let (repo, number) = row.id.rsplit_once('#')?;
                            let number = number.parse().ok()?;
                            let updated_at = match &row.trailing {
                                Some(alondra_core::views::Trailing::Time { at, .. }) => {
                                    Some(at.clone())
                                }
                                _ => None,
                            };
                            Some(alondra_core::issues::Issue {
                                repo: repo.to_owned(),
                                number,
                                title: row.subtitle.clone().unwrap_or_else(|| row.title.clone()),
                                url: format!("https://github.com/{repo}/issues/{number}"),
                                author: None,
                                association: None,
                                labels: row.badges.iter().map(|badge| badge.text.clone()).collect(),
                                updated_at,
                                comments: 0,
                            })
                        })
                        .collect();
                    (issues, view.empty.map(|notice| notice.text))
                });
            let _ = tx.send(Msg::Issues(result));
        });
    }

    /// The server's form for `view`, with the fields it offers for `project`.
    fn load_form(&self, view: &str, project: Option<String>) {
        let view = view.to_owned();
        let key = match &project {
            Some(project) => format!("{view}?project={}", crate::mcp::urlencode(project)),
            None => view.clone(),
        };
        self.spawn(move |tx| {
            let result = Client::for_tui(Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::FormView {
                view,
                project,
                result,
            });
        });
    }

    fn cancel_composer(&mut self) {
        let Some(c) = self.composer.take() else {
            return;
        };
        let text = c.input.text();
        if let Some(k) = c.target.draft_key()
            && (!text.trim().is_empty() || !c.chips.is_empty())
        {
            self.drafts.insert(k, (text, c.chips));
            self.flash(tr!(
                "Draft saved: open it again to pick up where you left off",
                "Borrador guardado: vuelve a abrirlo y sigue donde lo dejaste"
            ));
        }
    }

    fn composer_key(&mut self, k: KeyEvent) {
        let Some(c) = self.composer.as_mut() else {
            return;
        };
        if let Some(form) = c.form.as_mut() {
            match form.key(k) {
                work::FormKey::Handled => return,
                work::FormKey::Reload(project) => {
                    let view = form.view.clone();
                    return self.load_form(&view, Some(project));
                }
                work::FormKey::LoadIssues(refresh) => return self.load_issues(refresh),
                work::FormKey::NotMine => {}
            }
        }
        match k.code {
            KeyCode::Esc => self.cancel_composer(),
            KeyCode::Enter
                if k.modifiers
                    .intersects(KeyModifiers::ALT | KeyModifiers::SHIFT) =>
            {
                c.input.handle(k);
            }
            KeyCode::Enter => self.submit(),
            KeyCode::Backspace if c.input.text().is_empty() && !c.chips.is_empty() => {
                if let Some(chip) = c.chips.pop() {
                    self.previews.remove(&chip.seq);
                }
            }
            _ => {
                c.input.handle(k);
            }
        }
    }

    fn submit(&mut self) {
        let Some(c) = self.composer.take() else {
            return;
        };
        let text = c.input.text().trim().to_string();
        let needs_text = match &c.target {
            Target::ViewAction { required, .. } => *required,
            Target::Message { message, .. } if message.kind == ops::MessageKind::MoreContext => {
                false
            }
            Target::Message { .. } => c.chips.is_empty(),
            Target::Form { .. } => false,
            Target::Comment { .. } | Target::General { .. } => false,
        };
        if needs_text && text.is_empty() {
            self.composer = Some(c);
            return;
        }
        if c.chips.iter().any(|c| c.upload == Upload::Sending) {
            self.composer = Some(c);
            self.flash(tr!(
                "Still copying the attachments: ⏎ again in a moment",
                "Aún se están copiando los adjuntos: ⏎ otra vez en un momento"
            ));
            return;
        }
        if matches!(c.target, Target::Form { .. }) {
            return self.submit_form(c, text);
        }
        let attachments = dropped::stored(&c.chips);
        if !matches!(c.target, Target::Comment { .. } | Target::General { .. }) {
            for chip in &c.chips {
                self.previews.remove(&chip.seq);
            }
        }
        match c.target {
            Target::ViewAction {
                action_id,
                data_epoch,
                option_id,
                allow_attachments,
                ..
            } => {
                let mut input = json!({});
                if !text.trim().is_empty() {
                    input["text"] = Value::String(text);
                }
                if allow_attachments && !attachments.is_empty() {
                    let ids = attachments
                        .iter()
                        .map(|attachment| attachment.id.clone())
                        .collect::<Vec<_>>();
                    input["attachments"] = json!(ids);
                }
                let request =
                    Client::action_body(&data_epoch, &action_id, option_id.as_deref(), input);
                self.submit_view_request(request);
            }
            Target::Message {
                key,
                mut message,
                name,
                ..
            } => {
                message.text = text;
                message.attachments = attachments;
                let ok = if message.mark_read {
                    trf!(
                        "Sent to {name} and marked read",
                        "Enviado a {name} y marcado leído"
                    )
                } else {
                    trf!("Sent to {name}", "Enviado a {name}")
                };
                self.message(key, message, ok);
            }
            Target::Form { .. } => {}
            Target::Comment { block, quote } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else {
                    return;
                };
                let review = self.reviews.entry(key).or_default();
                review.chips.extend(c.chips);
                review.comments.retain(|c| c.block != block);
                if !text.is_empty() {
                    review.comments.push(Comment { block, quote, text });
                    review.comments.sort_by_key(|c| c.block);
                }
            }
            Target::General { send } => {
                let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else {
                    return;
                };
                let review = self.reviews.entry(key).or_default();
                review.general = text;
                review.chips.extend(c.chips);
                if send {
                    self.send_review();
                }
            }
        }
    }

    fn submit_form(&mut self, c: Composer, text: String) {
        let attachments = dropped::stored(&c.chips);
        let ids: Vec<String> = attachments.iter().map(|a| a.id.clone()).collect();
        let Some((form, submit)) = c
            .form
            .as_ref()
            .and_then(|form| form.submit.as_ref().map(|submit| (form, submit)))
        else {
            self.composer = Some(c);
            self.flash(tr!("Still loading the form", "Aún se está cargando el formulario"));
            return;
        };
        let fields = form.submitted(&text, &ids);
        let checked = validate_view_fields(&form.fields, &fields);
        let request = Client::action_body(
            &submit.data_epoch,
            &submit.action_id,
            None,
            json!({"fields": fields}),
        );
        if let Err(error) = checked {
            self.composer = Some(c);
            self.fail(error.to_string());
            return;
        }
        for chip in &c.chips {
            self.previews.remove(&chip.seq);
        }
        self.submit_view_request(request);
    }

    fn open_reader(&mut self) {
        if let Some(view) = self
            .card_view
            .as_ref()
            .filter(|view| self.selected_view_key().as_deref() == Some(view.key.as_str()))
            .cloned()
        {
            let title = view.title.clone().unwrap_or_else(|| view.key.clone());
            let mut text = format!("# {title}");
            let mut parts = Vec::new();
            let mut diffs = HashMap::new();
            for section in &view.sections {
                for item in &section.items {
                    match item {
                        alondra_core::views::Item::Markdown(markdown) => {
                            if !markdown.text.is_empty() {
                                text.push_str(&format!("\n\n{}", markdown.text));
                            }
                        }
                        alondra_core::views::Item::ContentRef(content) => {
                            if content.kind == alondra_core::views::ContentKind::Diff {
                                let Some(id) = view.key.strip_prefix("pull_request:") else {
                                    continue;
                                };
                                if !alondra_core::views::id::valid(id)
                                    || content.fetch
                                        != format!(
                                            "/pull-requests/{}/diff",
                                            alondra_core::views::id::path_segment(id)
                                        )
                                {
                                    continue;
                                }
                                let Some(head) = content.digest.as_ref() else {
                                    continue;
                                };
                                diffs.insert(content.fetch.clone(), (id.to_string(), head.clone()));
                            }
                            parts.push(Part::File {
                                name: content.title.clone(),
                                route: content.fetch.clone(),
                            });
                        }
                        alondra_core::views::Item::Card(card) => {
                            text.push_str(&format!("\n\n## {}", card.title));
                            for context in &card.context {
                                text.push_str(&format!("\n\n{}", context.text));
                            }
                            if let Some(body) = &card.body {
                                text.push_str(&format!("\n\n{}", body.text));
                            }
                            parts.extend(card.attachments.iter().map(|attachment| Part::File {
                                name: attachment.name.clone(),
                                route: attachment.fetch.clone(),
                            }));
                        }
                        _ => {}
                    }
                }
            }
            parts.insert(0, Part::Text(text));
            for part in &parts {
                if let Part::File { route, .. } = part {
                    if let Some((id, head)) = diffs.get(route) {
                        self.fetch_diff(route, id, head);
                    } else {
                        self.fetch(route);
                    }
                }
            }
            let first_card = view
                .sections
                .iter()
                .flat_map(|section| section.items.iter())
                .find_map(|item| match item {
                    alondra_core::views::Item::Card(card) => Some(card),
                    _ => None,
                });
            let (glyph, kind, color) = match first_card {
                Some(card) => (
                    view_render::icon(card.icon),
                    view_render::document_kind(&view).unwrap_or_default(),
                    view_render::icon_color(card.icon),
                ),
                None => (
                    "▤",
                    tr!("document", "documento").to_owned(),
                    ratatui::style::Color::Cyan,
                ),
            };
            let agent = first_card
                .and_then(view_render::card_agent)
                .map(|line| line.text.clone())
                .unwrap_or_default();
            self.reader = Some(Reader {
                key: Key(view.key),
                agent,
                title,
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
            return;
        }
        if let Some(card) = self
            .card_view
            .as_ref()
            .and_then(view_render::detail_card)
            .cloned()
        {
            let key = self.sel.clone().unwrap_or_else(|| Key(card.id.clone()));
            let kind = card
                .eyebrow
                .as_ref()
                .map(|eyebrow| eyebrow.text.to_lowercase())
                .unwrap_or_default();
            let agent = view_render::card_agent(&card)
                .map(|line| line.text.clone())
                .unwrap_or_default();
            let (glyph, color) = (view_render::icon(card.icon), view_render::icon_color(card.icon));
            let mut text = format!("# {}", card.title);
            for context in &card.context {
                text.push_str(&format!("\n\n{}", context.text));
            }
            if let Some(body) = &card.body {
                text.push_str(&format!("\n\n{}", body.text));
            }
            let mut parts = vec![Part::Text(text)];
            parts.extend(card.attachments.iter().map(|attachment| Part::File {
                name: attachment.name.clone(),
                route: attachment.fetch.clone(),
            }));
            for part in &parts {
                if let Part::File { route, .. } = part {
                    self.fetch(route);
                }
            }
            self.reader = Some(Reader {
                key,
                agent,
                title: card.title,
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
            return;
        }
    }

    fn rebuild_reader(&mut self) {
        let Some(r) = self.reader.as_mut() else {
            return;
        };
        let mut blocks = vec![];
        for part in &r.parts {
            match part {
                Part::Text(t) => blocks.extend(md::parse(t)),
                Part::File { name, route } => {
                    blocks.push(Block::label(&format!("📎 {name}")));
                    match self.files.get(route) {
                        Some(Ok(files::FileBody::Text(text))) => blocks.extend(md::parse(text)),
                        Some(Ok(body)) => blocks.push(Block::para(&files::not_text(body))),
                        Some(Err(e)) => blocks.push(Block::para(&trf!(
                            "Could not load the file: {e}",
                            "No he podido cargar el fichero: {e}"
                        ))),
                        None => blocks.push(Block::para(tr!("Loading…", "Cargando…"))),
                    }
                }
            }
        }
        r.blocks = blocks;
        r.settle_cursor();
    }

    fn reader_cmd(&mut self, cmd: Cmd) {
        let Some(r) = self.reader.as_mut() else {
            return;
        };
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
        let Some(r) = self.reader.as_mut() else {
            return;
        };
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
        let Some(r) = self.reader.as_mut() else {
            return;
        };
        let key = r.key.clone();
        if self
            .card_view
            .as_ref()
            .is_some_and(|view| view.key == key.0)
        {
            match k.code {
                KeyCode::Char('s') => {
                    self.send_review();
                    return;
                }
                KeyCode::Char('v') | KeyCode::Char('x') => {
                    let _ = self.view_action_key(k);
                    return;
                }
                KeyCode::Char(c)
                    if !c.is_ascii_digit() && !"qcCdsnN/".contains(c)
                        && self.card_view.as_ref().is_some_and(|view| {
                            view_render::actions(view).iter().flat_map(|action| &action.options)
                                .any(|option| option.key.as_deref() == Some(c.encode_utf8(&mut [0; 4])))
                        }) =>
                {
                    let _ = self.view_action_key(k);
                    return;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let has_review = self
                        .reviews
                        .get(&key)
                        .is_some_and(|review| !review.is_empty());
                    if has_review {
                        let index = c as usize - '1' as usize;
                        let accepts_text = self.card_view.as_ref().is_some_and(|view| {
                            view_render::actions(view)
                                .iter()
                                .flat_map(|action| &action.options)
                                .nth(index)
                                .is_some_and(|option| {
                                    matches!(option.input, alondra_core::views::Input::Text { .. })
                                })
                        });
                        if !accepts_text {
                            self.fail(tr!("That option takes no text: your comments stay here. Pick one that asks you to write.",
                                "Esa opción no admite texto: tus comentarios se quedan aquí. Elige una que pida escribir."));
                            return;
                        }
                    }
                    let _ = self.view_action_key(k);
                    if has_review
                        && let Some(review) = self.reviews.get(&key)
                        && let Some(composer) = self.composer.as_mut()
                    {
                        composer.input = TextInput::with(&review.feedback());
                        composer.chips.extend(review.chips.clone());
                    }
                    return;
                }
                _ => {}
            }
        }
        match k.code {
            KeyCode::Char('q') => self.reader = None,
            KeyCode::Char('/') => {
                r.find = Some(Find {
                    input: TextInput::default(),
                    editing: true,
                })
            }
            KeyCode::Char(c @ ('n' | 'N')) if r.find.is_some() => {
                r.jump(c == 'n', false);
            }
            KeyCode::Char('c') | KeyCode::Enter => {
                let Some(b) = r.blocks.get(r.cursor).filter(|b| b.commentable()) else {
                    return;
                };
                let (block, quote) = (r.cursor, b.quote());
                let prefill = self
                    .reviews
                    .get(&key)
                    .and_then(|rv| rv.comments.iter().find(|c| c.block == block))
                    .map(|c| c.text.clone())
                    .unwrap_or_default();
                self.compose(Target::Comment { block, quote }, &prefill);
            }
            KeyCode::Char('C') => {
                let prefill = self
                    .reviews
                    .get(&key)
                    .map(|rv| rv.general.clone())
                    .unwrap_or_default();
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
            _ => {}
        }
    }

    /// The reader's comments go back to whoever the reading is about: feedback on a delivery, a
    /// reply to information, and on a question the option the user picks carries them.
    fn send_review(&mut self) {
        let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else {
            return;
        };
        if let Some(view) = self.card_view.as_ref().filter(|view| view.key == key.0) {
            let action = view_render::actions(view).into_iter().find(|action| {
                action.enabled
                    && alondra_core::views::action_id::ActionId::parse(&action.id).is_ok_and(|id| {
                        matches!(
                            id.verb,
                            alondra_core::views::action_id::Verb::Changes
                                | alondra_core::views::action_id::Verb::Reply
                        )
                    })
            });
            let Some(action) = action else {
                self.fail(tr!(
                    "Press the number of the option your comments go with",
                    "Pulsa el número de la opción con la que van tus comentarios"
                ));
                return;
            };
            let required = match &action.input {
                alondra_core::views::Input::Text { required, .. } => *required,
                _ => return,
            };
            let epoch = view.data_epoch.clone();
            let review = self.reviews.get(&key);
            let text = review
                .filter(|review| !review.is_empty())
                .map(Review::feedback)
                .unwrap_or_default();
            let chips = review
                .map(|review| review.chips.clone())
                .unwrap_or_default();
            self.compose(
                Target::ViewAction {
                    action_id: action.id,
                    label: action.label,
                    data_epoch: epoch,
                    option_id: None,
                    required,
                    allow_attachments: action.attachments,
                },
                &text,
            );
            if let Some(composer) = self.composer.as_mut() {
                composer.chips.extend(chips);
            }
            return;
        }
    }

    pub fn search_key(&mut self, k: KeyEvent) {
        let Some(s) = self.search.as_mut() else {
            return;
        };
        match k.code {
            KeyCode::Esc => {
                self.search = None;
                if self
                    .view_navigation
                    .last()
                    .is_some_and(|key| key.starts_with("search"))
                {
                    self.view_navigation.pop();
                    self.card_view = None;
                    self.card_view_asked = None;
                    self.load_selected_view();
                    self.sync_open_views();
                }
            }
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
                    s.remote = Remote::Pending;
                    self.update_search_view();
                }
            }
        }
    }

    /// The id `claude attach` takes for an agent: its short id when the runtime knows it.
    pub fn attach_id(&self, id: &AgentId) -> String {
        self.runtime
            .as_ref()
            .and_then(|r| r.node(id))
            .map_or_else(|| id.0.clone(), |n| n.short_id.clone())
    }
}

/// The daemon's reason an action is off, in the user's language where it is one it names.
pub fn unavailable(action: &AvailableAction) -> String {
    let reason = action.disabled_reason.clone().unwrap_or_default();
    match reason.as_str() {
        "already_read" => tr!("It is already read", "Ya está leído").into(),
        "task_not_active" => {
            tr!("The task is no longer active", "La tarea ya no está activa").into()
        }
        "invalid_action_for_kind" => tr!("It is already answered", "Ya está contestada").into(),
        "forbidden" => tr!("It is not addressed to you", "No va dirigida a ti").into(),
        _ => reason,
    }
}
