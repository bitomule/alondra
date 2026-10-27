mod agents;
mod bindings;
mod crash_report;
mod draw;
mod dropped;
mod files;
mod identity;
mod inbox;
mod input;
mod keys;
mod mcp_sign_in;
pub(crate) mod md;
mod optimistic;
mod restore;
mod rules;
mod mcp_servers;
mod tasks;
mod title;
mod updated;
mod usage_banner;
mod version_watch;
mod view_feed;
mod view_render;
mod work;

#[cfg(test)]
#[path = "tui/tests/keyhints.rs"]
mod keyhints;
#[cfg(test)]
#[path = "tui/tests/parity.rs"]
mod parity;
#[cfg(test)]
#[path = "tui/tests/screens.rs"]
mod screens;
#[cfg(test)]
#[path = "tui/tests/shots.rs"]
mod shots;
#[cfg(test)]
mod views_tests;

/// The card an action came from and what it says while it runs.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Acting {
    pub origin: Key,
    pub stamp: Option<String>,
    /// The server's `resolves_card`: the card leaves the list the moment the action is sent.
    pub hides: bool,
    /// The action's label, which the notice names when the answer does not arrive.
    #[serde(default)]
    pub label: String,
}

#[derive(Debug)]
enum ViewActionReply {
    Outcome(Outcome),
}

fn view_action_result(value: serde_json::Value) -> std::result::Result<ViewActionReply, Delivery> {
    if let Some(outcome) = value
        .get("result")
        .and_then(|result| serde_json::from_value::<Outcome>(result.clone()).ok())
    {
        return Ok(ViewActionReply::Outcome(outcome));
    }
    let message = |fallback: &str| value["message"].as_str().unwrap_or(fallback).to_owned();
    let outcome = match value["outcome"].as_str() {
        Some("applied") => Outcome {
            kind: OutcomeKind::Done,
            title: value["stamp"]
                .as_str()
                .unwrap_or(tr!("Done", "Hecho"))
                .to_owned(),
            body: None,
            next: None,
        },
        Some("rejected" | "no_longer_available") => Outcome {
            kind: OutcomeKind::Refused,
            title: message(tr!(
                "Action could not be completed",
                "No se pudo completar la acción"
            )),
            body: None,
            next: None,
        },
        _ => {
            return Err(Delivery::Answered(None));
        }
    };
    Ok(ViewActionReply::Outcome(outcome))
}

fn diff_reader_text(diff: crate::client::PullRequestDiff, truncated_notice: &str) -> String {
    if diff.truncated {
        format!("{}\n\n{truncated_notice}", diff.text)
    } else {
        diff.text
    }
}

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::stdout;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::Duration;

use alondra_core::domain::{
    AgentId, AttachmentRef, AvailableAction, Command, DeliveryTarget, Interaction, InteractionId,
    PrId, Revision, TaskId,
};
use alondra_core::now_millis;
use crate::client::Delivery;
use alondra_core::views::{Outcome, OutcomeKind, Status as ViewStatus, View as ViewDocument};
use anyhow::Result;
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::DefaultTerminal;
use serde_json::{Value, json};

use crate::client::{Client, Target as MacTarget};
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
/// How long a notice that an answer did not arrive stays on screen.
const NOTICE_MS: i64 = 12_000;
const CLOCK_MS: i64 = 20_000;
const IDLE_POLL: Duration = Duration::from_millis(250);
const ACTIVITY_EVERY_MS: i64 = 60_000;
/// Threads move with every tool call, which does not move the domain: the open agent asks again this often.
const THREADS_EVERY_MS: i64 = 10_000;
const PREVIEW_PIXELS: u32 = 480;
const PAIRING_POLL: Duration = Duration::from_secs(2);

/// An inbox item by the id the daemon gave it.
#[derive(Clone, PartialEq, Eq, Hash, Debug, serde::Serialize, serde::Deserialize)]
pub struct Key(pub String);

#[derive(Clone, Copy, PartialEq)]
pub enum Screen {
    Inbox,
    Tasks,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FlashKind {
    Success,
    Pending,
    Error,
}

pub struct Flash {
    pub text: String,
    pub kind: FlashKind,
    pub at: i64,
    pub ttl: i64,
}

/// A question the user answers with ⏎ or Esc before an action runs.
#[derive(Clone, Debug)]
pub struct Modal {
    pub title: String,
    pub body: String,
    pub button: String,
    pub destructive: bool,
    then: ModalThen,
}

#[derive(Clone, Debug)]
enum ModalThen {
    Action {
        action: Box<alondra_core::views::Action>,
        option_id: Option<String>,
        selected: Key,
        epoch: String,
    },
    Changed {
        request: Value,
        action_id: String,
        hides: bool,
        origin: Option<Key>,
        review: Option<Key>,
        label: String,
    },
}

/// A device asking to use Alondra on this Mac, as `/devices/pending` lists it.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct PendingDevice {
    pub id: String,
    pub name: String,
    pub code: String,
    pub expires_at_ms: i64,
}

#[derive(serde::Deserialize)]
struct PendingDevices {
    pending: Vec<PendingDevice>,
}

/// One Mac this terminal is connected to, as its tab in the header shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct MacTab {
    pub name: String,
    pub status: MacStatus,
    pub attention: usize,
    pub active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MacStatus {
    Here,
    Lan,
    Tailscale,
    Direct,
    Relay,
    Connecting,
    Offline,
}

impl MacStatus {
    pub fn label(self) -> Option<&'static str> {
        match self {
            MacStatus::Here => None,
            MacStatus::Lan => Some(tr!("On the local network", "En la red local")),
            MacStatus::Tailscale => Some(tr!("Through Tailscale", "Por Tailscale")),
            MacStatus::Direct => Some(tr!("Direct connection", "Conexión directa")),
            MacStatus::Relay => Some(tr!("Through the relay", "Por el relay")),
            MacStatus::Connecting => Some(tr!("Connecting…", "Conectando…")),
            MacStatus::Offline => Some(tr!("Offline", "Sin conexión")),
        }
    }

    fn direct(offline: bool, connected: bool, route: Option<crate::direct_client::RouteKind>) -> Self {
        use crate::direct_client::RouteKind;
        match (offline, connected, route) {
            (true, _, _) => MacStatus::Offline,
            (false, true, Some(RouteKind::Lan)) => MacStatus::Lan,
            (false, true, Some(RouteKind::Tailscale)) => MacStatus::Tailscale,
            (false, true, Some(RouteKind::Direct)) => MacStatus::Direct,
            (false, true, Some(RouteKind::Relay)) => MacStatus::Relay,
            _ => MacStatus::Connecting,
        }
    }
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
    /// The task the card came from, so comments still reach its node if the card goes away.
    task_id: Option<alondra_core::domain::TaskId>,
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
    /// The numbered list of what the selected block opens, when it opens more than one thing.
    pub picking: bool,
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
#[derive(serde::Serialize, serde::Deserialize)]
pub enum Target {
    ViewAction {
        action_id: String,
        label: String,
        data_epoch: String,
        option_id: Option<String>,
        required: bool,
        allow_attachments: bool,
        acting: Option<Acting>,
        /// The reader whose comments this sends.
        #[serde(default)]
        review: Option<Key>,
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
    /// Where the browser came back to after signing in to an MCP server away from the daemon's
    /// Mac: it carries a one-time code, so it never travels to a restarted process.
    McpReturn {
        card: String,
        server: String,
        url: String,
    },
    /// The name to append to a validated native account command.
    AccountName { provider: alondra_core::accounts::Provider, argv: Vec<String> },
    AccountPosition,
    /// «What happened?» for a crash report whose issue goes as `failure`; the report is built once
    /// it is sent, empty or not.
    ReportDescription {
        failure: String,
    },
}

pub struct Composer {
    pub target: Target,
    pub input: TextInput,
    pub chips: Vec<Chip>,
    pub form: Option<work::OptionsForm>,
}

impl Target {
    /// A crash report carries only what Alondra collects, so a file dropped on it is pasted as text.
    pub fn takes_files(&self) -> bool {
        !matches!(self, Target::ReportDescription { .. })
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
    /// The rows of a folded section, asked for when the user opened it.
    ViewSection {
        section: String,
        result: Result<ViewDocument, String>,
    },
    ViewCard {
        key: String,
        result: Result<ViewDocument, String>,
    },
    ViewFeed(Result<crate::client::ViewsFeed, String>),
    /// The daemon said which version it is and which views protocols it serves.
    Service(alondra_core::views::Service),
    /// What the binary a restart would run says it is, for that daemon version.
    DiskVersion {
        version: String,
        disk: version_watch::Disk,
    },
    ViewAction {
        request: Value,
        result: Result<ViewActionReply, Delivery>,
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
    /// What the daemon answered to Connect on the card of the tasks waiting for `server`.
    McpOpened {
        card: String,
        server: String,
        result: Result<Value, String>,
    },
    /// A request that is not a domain command: what to flash, or why it failed.
    Posted {
        key: Option<Key>,
        result: Result<String, String>,
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
    PendingDevices(Vec<PendingDevice>),
    DaemonIdentity(identity::Answer),
    PairingAnswered {
        device: PendingDevice,
        allow: bool,
        result: Result<(), String>,
    },
    Report(crash_report::Answer),
}

pub enum Flow {
    Go,
    Quit,
    NextMac,
    /// Claude's sign-in for an account, run on the screen by `alondra accounts`.
    AccountSignIn(mcp_servers::SignIn),
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
    /// This Mac's permissions allowed always, open over the Tasks.
    pub rules: Option<rules::Rules>,
    /// The MCP servers of the nodes and projects, open over the list screens.
    pub mcp_servers: Option<mcp_servers::McpServers>,
    /// Keeps Settings' selected row while a server-owned form uses the normal view navigator.
    pub settings_after_view: Option<(mcp_servers::McpServers, usize)>,
    pub crash_report: Option<crash_report::CrashReport>,
    /// What a key's work leaves the screen to do once it returns, such as an account's sign-in.
    pub pending_flow: Option<Flow>,
    pub sel: Option<Key>,
    sel_idx: usize,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    settle_until: i64,
    /// What the user did that no view shows yet: one rule for every action this terminal sends.
    optimistic: optimistic::Optimistic,
    /// The detail each answered card had when it left, so it comes back whole if the answer does not arrive.
    left_views: HashMap<Key, ViewDocument>,
    /// Reader comments on their way, by command: they stay until the Mac confirms them.
    review_sends: HashMap<String, Key>,
    /// The card whose comments were just sent, so its closing does not hide that they went.
    review_sent: Option<Key>,
    /// Who the comments of a reader closed on sending go to, for the word that they arrived.
    review_agents: HashMap<Key, String>,
    pub agent_sel: Option<AgentId>,
    agent_idx: usize,
    pub agent_scroll: usize,
    pub agent_detail_scroll: usize,
    pub agent_view: View,
    pub agent_zoom: bool,
    /// A step that takes a second press within a few seconds, until when, and the key that
    /// answers it; Esc or any other key cancels.
    modal: Option<Modal>,
    /// Where the selection landed when each card left: if it is still there when the card comes
    /// back, the user has not moved and the card is selected again.
    left_on: HashMap<Key, Option<Key>>,
    /// A question that arrived while the user was typing or answering another; it opens when
    /// they are done.
    deferred_modal: Option<Modal>,
    pressed: KeyCode,
    /// Every request that is not a domain command, as the tests read what it would have sent.
    #[cfg(test)]
    posted: std::cell::RefCell<Vec<(String, Value)>>,
    /// The direct Mac each background request would have gone to; `None` is this Mac.
    #[cfg(test)]
    spawned: std::cell::RefCell<Vec<Option<String>>>,
    target: MacTarget,
    /// Every Mac this terminal is connected to, when there is more than one.
    pub macs: Vec<MacTab>,
    /// Devices waiting for this Mac to let them in; only the instance for this Mac asks.
    pub pairing: Vec<PendingDevice>,
    pairing_answered: HashSet<String>,
    pub search: Option<Search>,
    pub composer: Option<Composer>,
    pub reader: Option<Reader>,
    pub reviews: HashMap<Key, Review>,
    chip_seq: u64,
    /// What alondrad knows about the Mac and its sessions beyond the domain.
    pub runtime: Option<Runtime>,
    /// The local daemon's machine, usage and update readings from `/daemon`.
    pub daemon: Option<Runtime>,
    /// Where this Mac's instance looks for its daemon; `None` draws no mark.
    pub here: Option<identity::Here>,
    /// Which daemon answered there, by its own word.
    pub daemon_identity: Option<identity::Answer>,
    /// Stored files by content route: text to read, an image to draw, or bytes to leave to the Mac.
    pub files: HashMap<String, Result<files::FileBody, String>>,
    loading: HashSet<String>,
    /// How images are drawn in this terminal: Kitty graphics in Ghostty and Kitty, iTerm2 or sixel
    /// where supported, colour half-blocks elsewhere. Found once, when the TUI starts.
    pub picker: Option<ratatui_image::picker::Picker>,
    /// Whether the reader writes its links as terminal hyperlinks.
    pub hyperlinks: bool,
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
    /// Sections the daemon sends folded that the user opened.
    pub open_sections: BTreeSet<String>,
    /// Rows the daemon sends folded, such as a chain of waiting tasks, that the user opened.
    pub open_rows: BTreeSet<String>,
    /// The rows of each opened section, fetched apart: the list view only says how many there are.
    pub section_pages: BTreeMap<String, alondra_core::views::Section>,
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
    reader_after_view: Option<String>,
    /// Whether this terminal is behind its daemon, and what to do about it.
    pub updated: updated::Watch,
}

pub fn run() -> Result<()> {
    let restored = restore::take();
    crate::pty::reset_terminal_modes();
    let mut terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    let picker = ratatui_image::picker::Picker::from_query_stdio()
        .unwrap_or_else(|_| ratatui_image::picker::Picker::halfblocks());
    let (direct, unreadable) = match crate::direct_client::load() {
        Ok(macs) => (macs, None),
        Err(e) => (Vec::new(), Some(e)),
    };
    let targets = std::iter::once(MacTarget::Local).chain(direct.into_iter().map(|mac| {
        MacTarget::Direct(std::sync::Arc::new(crate::direct_client::Connection::new(mac)))
    }));
    let mut macs: Vec<(App, Receiver<Msg>)> = targets
        .map(|target| {
            let (tx, rx) = mpsc::sync_channel::<Msg>(64);
            let mut app = App::with_target(tx, target);
            app.picker = Some(picker.clone());
            app.hyperlinks = std::env::var("TERM").map_or(true, |term| term != "dumb");
            app.start_view_feed();
            (app, rx)
        })
        .collect();
    let local = &mut macs[0].0;
    if let Some(state) = restored {
        local.restore(state);
    }
    local.start_pairing_feed();
    local.here = Some(identity::Here::of_this_process());
    local.start_identity_feed();
    if let Some(e) = unreadable {
        local.fail(trf!(
            "Could not read the Macs this terminal paired with: {e}",
            "No pude leer los Macs emparejados con este terminal: {e}"
        ));
    }
    let res = main_loop(&mut terminal, macs);
    title::clear(&mut stdout());
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::pty::reset_terminal_modes();
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

fn mac_tabs(macs: &[(App, Receiver<Msg>)], active: usize) -> Vec<MacTab> {
    if macs.len() < 2 {
        return Vec::new();
    }
    macs.iter()
        .enumerate()
        .map(|(i, (app, _))| app.mac_tab(i == active))
        .collect()
}

/// Every Mac's instance stays alive and takes its own messages; only the active one is drawn
/// and gets the keys. Without an answer from this Mac's daemon, the first direct Mac opens.
fn main_loop(terminal: &mut DefaultTerminal, mut macs: Vec<(App, Receiver<Msg>)>) -> Result<()> {
    let mut active = 0;
    let mut chosen = false;
    let mut dirty = true;
    let mut tab_title = title::TerminalTitle::default();
    loop {
        if dirty {
            let tabs = mac_tabs(&macs, active);
            let app = &mut macs[active].0;
            app.macs = tabs;
            app.reconcile();
            draw(terminal, app)?;
            let attention: Vec<Option<usize>> =
                macs.iter().map(|(app, _)| (!app.offline).then(|| app.attention())).collect();
            tab_title.show(&mut stdout(), title::text(&attention))?;
            dirty = false;
        }
        if event::poll(IDLE_POLL)? {
            let app = &mut macs[active].0;
            app.note_activity();
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => match app.on_key(k) {
                        Flow::Quit => return Ok(()),
                        Flow::NextMac => {
                            active = (active + 1) % macs.len();
                            chosen = true;
                            dirty = true;
                            break;
                        }
                        Flow::Review {
                            key,
                            pr,
                            number,
                            dir,
                        } => {
                            if app.remote_mac().is_some() {
                                app.fail(tr!("tuicr runs on the destination Mac; the diff opens here instead", "tuicr funciona en el Mac de destino; aquí se abre el diff"));
                                break;
                            }
                            draw(terminal, app)?;
                            let _ = execute!(stdout(), DisableBracketedPaste);
                            let res = crate::pty::run_tool(
                                "tuicr",
                                &["--stdout", "--no-update-check", "pr", &number.to_string()],
                                &dir,
                            );
                            crossterm::terminal::enable_raw_mode()?;
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            app.reviewed(key, pr, number, res);
                            tab_title.forget();
                            repaint(terminal, app)?;
                            break;
                        }
                        Flow::AccountSignIn(sign_in) => {
                            draw(terminal, app)?;
                            let _ = execute!(stdout(), DisableBracketedPaste, crossterm::terminal::Clear(crossterm::terminal::ClearType::All), crossterm::cursor::MoveTo(0, 0));
                            println!("{}\r", tr!("Signing in to Claude: follow the steps below; the browser may open.",
                                "Iniciando sesión en Claude: sigue los pasos de abajo; puede abrirse el navegador."));
                            let args: Vec<&str> = sign_in.args.iter().map(String::as_str).collect();
                            let home = alondra_core::config::home_dir(&|key| std::env::var(key).ok());
                            let res = std::env::current_exe().map_err(anyhow::Error::from)
                                .and_then(|alondra| crate::pty::run_in_terminal(&alondra, &args, &home));
                            crossterm::terminal::enable_raw_mode()?;
                            let _ = execute!(stdout(), EnableBracketedPaste);
                            app.account_signed_in(res);
                            tab_title.forget();
                            repaint(terminal, app)?;
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
        for (app, rx) in &mut macs {
            while let Ok(m) = rx.try_recv() {
                app.on_msg(m);
                dirty = true;
            }
            dirty |= app.on_timers();
        }
        if macs[0].0.updated.restart && !macs.iter().any(|(app, _)| app.holds_restart()) {
            restart(terminal, &mut macs[0].0)?;
            tab_title.forget();
            dirty = true;
        }
        let local = &macs[0].0;
        if !chosen && macs.len() > 1 && local.offline && !local.loaded {
            active = 1;
            chosen = true;
            dirty = true;
        }
    }
}

/// Hands the screen and the open composer to the new binary and becomes it. Only comes back when `exec`
/// failed, with the terminal and the composer where they were.
fn restart(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let state = app.to_restore();
    let file = match restore::save(&state) {
        Ok(file) => file,
        Err(error) => {
            app.exec_failed(state, &error);
            return Ok(());
        }
    };
    title::clear(&mut stdout());
    let _ = execute!(stdout(), DisableBracketedPaste);
    ratatui::restore();
    crate::pty::reset_terminal_modes();
    let error = updated::exec(&updated::stable_path(), &file);
    let _ = std::fs::remove_file(&file);
    *terminal = ratatui::init();
    let _ = execute!(stdout(), EnableBracketedPaste);
    app.exec_failed(state, &error);
    repaint(terminal, app)
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
                if !value.as_str().is_some_and(|choice| items.iter().any(|item| {
                    item.id == choice && item.disabled_reason.is_none() && item.available_when.as_ref().is_none_or(|condition| {
                        let actual = values.get(&condition.field);
                        condition.equals.as_ref().is_some_and(|expected| actual == Some(expected)) || actual.is_some_and(|actual| condition.any_of.contains(actual))
                    })
                })) =>
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

/// The Mac's default app for a URL or file, through `open`. Tests never open one, and a sandbox
/// TUI given `ALONDRA_SANDBOX_OPENED` writes it there instead, for a journey to follow.
#[cfg(test)]
thread_local! {
    static OPENED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn open_with_mac(target: &str) {
    #[cfg(test)]
    OPENED.with(|opened| opened.borrow_mut().push(target.to_owned()));
    if cfg!(test) {
        return;
    }
    if let Some(opened) = std::env::var_os("ALONDRA_SANDBOX_OPENED").filter(|_| std::env::var("ALONDRA_SANDBOX").as_deref() == Ok("1")) {
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(opened) {
            let _ = writeln!(file, "{target}");
        }
        return;
    }
    let _ = std::process::Command::new("/usr/bin/open")
        .arg(target)
        .spawn();
}

/// A stored file handed to the Mac's app for it: always a private local copy, never a path of
/// the daemon's, so a remote destination and the local Mac read the same bytes.
#[expect(clippy::disallowed_methods, reason = "User-opened attachments must outlive the CLI process.")]
fn open_stored(target: &MacTarget, name: &str, route: &str) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    let bytes = Client::for_target(target, Duration::from_secs(120)).content(route)?;
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
        Self::with_target(tx, MacTarget::Local)
    }

    fn with_target(tx: SyncSender<Msg>, target: MacTarget) -> Self {
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
            rules: None,
            mcp_servers: None,
            settings_after_view: None,
            crash_report: None,
            pending_flow: None,
            sel: None,
            sel_idx: 0,
            list_scroll: 0,
            detail_scroll: 0,
            settle_until: 0,
            optimistic: Default::default(),
            left_views: HashMap::new(),
            review_sends: HashMap::new(),
            review_sent: None,
            review_agents: HashMap::new(),
            agent_sel: None,
            agent_idx: 0,
            agent_scroll: 0,
            agent_detail_scroll: 0,
            agent_view: View::default(),
            agent_zoom: false,
            modal: None,
            left_on: HashMap::new(),
            deferred_modal: None,
            pressed: KeyCode::Null,
            #[cfg(test)]
            posted: Default::default(),
            #[cfg(test)]
            spawned: Default::default(),
            target,
            macs: Vec::new(),
            pairing: Vec::new(),
            pairing_answered: HashSet::new(),
            search: None,
            composer: None,
            reader: None,
            reviews: HashMap::new(),
            chip_seq: 0,
            runtime: None,
            daemon: None,
            here: None,
            daemon_identity: None,
            files: HashMap::new(),
            loading: HashSet::new(),
            picker: cfg!(test).then(ratatui_image::picker::Picker::halfblocks),
            hyperlinks: false,
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
            open_sections: BTreeSet::new(),
            open_rows: BTreeSet::new(),
            section_pages: BTreeMap::new(),
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
            reader_after_view: None,
            updated: updated::Watch::default(),
        }
    }

    fn start_view_feed(&self) {
        let tx = self.tx.clone();
        let open_views = self.open_views.clone();
        view_feed::start(tx, open_views, self.target.clone());
    }

    /// Asks this Mac's daemon every few seconds which daemon it is, since another can take its port.
    fn start_identity_feed(&self) {
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new()
            .name("alondra-daemon-identity".into())
            .spawn(move || {
                let client = Client::new(Duration::from_secs(5));
                let mut last = None;
                loop {
                    let answer = client.get::<Value>("/daemon").ok().and_then(|body| identity::Answer::from_daemon(&body));
                    if let Some(answer) = answer.filter(|answer| last.as_ref() != Some(answer)) {
                        if tx.send(Msg::DaemonIdentity(answer.clone())).is_err() {
                            return;
                        }
                        last = Some(answer);
                    }
                    std::thread::sleep(PAIRING_POLL);
                }
            });
    }

    /// Asks this Mac's daemon every few seconds which devices wait to be let in.
    fn start_pairing_feed(&self) {
        let tx = self.tx.clone();
        let _ = std::thread::Builder::new()
            .name("alondra-pairing-feed".into())
            .spawn(move || {
                let client = Client::for_user(Duration::from_secs(5));
                let mut last = None;
                loop {
                    let pending = client
                        .get::<PendingDevices>("/devices/pending")
                        .map(|p| p.pending)
                        .unwrap_or_default();
                    if last.as_ref() != Some(&pending) {
                        if tx.send(Msg::PendingDevices(pending.clone())).is_err() {
                            return;
                        }
                        last = Some(pending);
                    }
                    std::thread::sleep(PAIRING_POLL);
                }
            });
    }

    /// The Mac this instance reaches from afar, by id; `None` for this Mac over loopback.
    fn remote_mac(&self) -> Option<String> {
        match &self.target {
            MacTarget::Local => crate::service_client::target().map(str::to_owned),
            MacTarget::Direct(connection) => Some(connection.mac().mac_id.clone()),
        }
    }

    fn mac_tab(&self, active: bool) -> MacTab {
        let (name, status) = match &self.target {
            MacTarget::Local => (
                crate::service_client::target()
                    .map_or_else(|| tr!("This Mac", "Este Mac").to_owned(), str::to_owned),
                if self.offline {
                    MacStatus::Offline
                } else {
                    MacStatus::Here
                },
            ),
            MacTarget::Direct(connection) => (
                connection.mac().name.clone(),
                MacStatus::direct(
                    self.offline,
                    self.connected,
                    connection.route().map(|(_, kind)| kind),
                ),
            ),
        };
        MacTab {
            name,
            status,
            attention: self.attention() + self.pairing.len(),
            active,
        }
    }

    pub fn pairing_prompt(&self) -> Option<&PendingDevice> {
        self.pairing.first()
    }

    fn answer_pairing(&mut self, device: PendingDevice, allow: bool) {
        let verb = if allow { "approve" } else { "deny" };
        let path = format!("/devices/pending/{}/{verb}", urlencode(&device.id));
        #[cfg(test)]
        self.posted.borrow_mut().push((path.clone(), json!({})));
        self.pairing_answered.insert(device.id.clone());
        self.pairing.retain(|d| d.id != device.id);
        self.spawn(move |tx, _| {
            let result = Client::for_user(Duration::from_secs(10))
                .post(&path, &json!({}))
                .map(|_| ())
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::PairingAnswered {
                device,
                allow,
                result,
            });
        });
    }

    /// The pairing prompt shows only when its keys reach it: never over an open question or text
    /// being typed, which keep the keyboard.
    pub fn pairing_answers_keys(&self) -> bool {
        self.modal.is_none() && !self.typing()
    }


    fn typing(&self) -> bool {
        self.composer.is_some()
            || self.cmdline.is_some()
            || self
                .reader
                .as_ref()
                .is_some_and(|r| r.find.as_ref().is_some_and(|f| f.editing))
            || (self.screen == Screen::Tasks
                && self.reader.is_none()
                && self.search.as_ref().is_some_and(|s| s.editing))
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
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key, result });
        });
    }

    fn selected_view_key(&self) -> Option<String> {
        if let Some(key) = self.view_navigation.last() {
            return Some(key.clone());
        }
        if let (Some(selected), Some(view)) = (self.sel.as_ref(), self.shown_home()) {
            let entry = view_render::entries(&view, &self.open_sections, &self.open_rows)
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
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewHome(result));
        });
    }

    /// Asks for the rows of a folded section of the list on screen.
    pub(super) fn load_section(&mut self, id: &str) {
        let Some(key) = self.home_view.as_ref()
            .and_then(|home| home.sections.iter().find(|section| section.id == id))
            .filter(|section| section.collapsed && section.items.is_empty())
            .and_then(|section| section.more.as_ref())
            .map(|more| more.key.clone())
        else {
            self.section_pages.remove(id);
            return;
        };
        let section = id.to_owned();
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewSection { section, result });
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
        if let Some(view) = self.rules.as_ref().and_then(|rules| rules.view.as_ref()) {
            open.insert(view.key.clone(), view.version.clone());
        }
        for view in self.mcp_servers.iter().flat_map(|screen| [&screen.view, &screen.general, &screen.accounts]).flatten() {
            open.insert(view.key.clone(), view.version.clone());
        }
    }

    pub(super) fn navigate_view(&mut self, key: &str) {
        if let Some(settings) = self.mcp_servers.take() {
            self.settings_after_view = Some((settings, self.view_navigation.len()));
            self.focus = Pane::Detail;
        }
        self.reader_after_view = None;
        self.view_navigation.push(key.to_owned());
        self.card_view = None;
        self.card_view_asked = None;
        self.detail_scroll = 0;
        self.load_selected_view();
        self.sync_open_views();
    }

    /// Every action this terminal sends goes out here. Its card leaves at once when the daemon says
    /// the action takes it away, from the list and from the detail, and nothing says it is on its
    /// way: the answer is tried again in silence until the daemon confirms it (`optimistic`).
    fn submit_view_request(&mut self, request: Value, acting: Option<Acting>) {
        let target = acting.as_ref().filter(|acting| acting.hides).map(|acting| acting.origin.clone());
        let now = now_millis();
        let request = match self.optimistic.same_action(&request) {
            Some(earlier) => {
                if let (Some(review), Some(command)) = (request["command_id"].as_str().and_then(|command| self.review_sends.remove(command)), Some(earlier.clone())) {
                    self.review_sends.insert(command, review);
                }
                self.optimistic.again(&earlier, now);
                self.optimistic.request(&earlier).cloned().unwrap_or(request)
            }
            None => {
                let answer = optimistic::Answer {
                    card: acting.as_ref().map(|acting| self.card_title(&acting.origin)).unwrap_or_default(),
                    label: acting.as_ref().map(|acting| acting.label.clone()).unwrap_or_default(),
                };
                self.optimistic.begin(request.clone(), target.clone(), answer, now);
                request
            }
        };
        if let Some(stamp) = acting.and_then(|acting| acting.stamp) {
            self.flash(stamp);
        }
        if let Some(key) = target {
            if let Some(view) = self.card_view.clone().filter(|view| view.key == format!("card:{}", key.0)) {
                self.left_views.insert(key.clone(), view);
            }
            self.leave_detail_of(&key);
            self.reconcile();
            self.left_on.insert(key, self.sel.clone());
        }
        self.post_action(request);
    }

    fn post_action(&mut self, request: Value) {
        #[cfg(test)]
        self.posted
            .borrow_mut()
            .push(("/actions".into(), request.clone()));
        self.action_sent();
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, optimistic::ATTEMPT_TIMEOUT)
                .post("/actions", &request)
                .map_err(|error| crate::client::delivery(&error))
                .and_then(view_action_result);
            let _ = tx.send(Msg::ViewAction { request, result });
        });
    }

    /// The card just answered leaves the detail too: a view opened over the list for it closes, and
    /// so does the reader, whose comments stay until the daemon confirms they arrived.
    fn leave_detail_of(&mut self, key: &Key) {
        let card_key = format!("card:{}", key.0);
        let acted_on = |view: &String| *view == card_key || self.card_view.as_ref().is_some_and(|open| open.key == *view);
        if let Some(reader) = self.reader.take_if(|reader| acted_on(&reader.key.0)) {
            self.review_agents.insert(reader.key, reader.agent);
        }
        if self.view_navigation.last().is_some_and(acted_on) {
            self.view_navigation.pop();
            self.card_view = None;
            self.card_view_asked = None;
            self.detail_scroll = 0;
            self.load_selected_view();
            self.sync_open_views();
        }
    }

    /// What the user reads the card as: its title in the queue, else the open view's.
    fn card_title(&self, key: &Key) -> String {
        let from_home = self.home_view.as_ref().and_then(|home| {
            home.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
                alondra_core::views::Item::Card(card) if card.id == key.0 => card.need.clone(),
                _ => None,
            })
        });
        from_home
            .or_else(|| self.card_view.as_ref().and_then(|view| view.title.clone()))
            .map(|title| md::one_line(&title))
            .unwrap_or_default()
    }

    fn run_modal(&mut self) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal.then {
            ModalThen::Action {
                action,
                option_id,
                selected,
                epoch,
            } => {
                let option = option_id
                    .as_deref()
                    .and_then(|id| action.options.iter().find(|option| option.id == id));
                self.run_view_action_confirmed(&action, option, &selected, epoch);
            }
            ModalThen::Changed {
                request,
                action_id,
                hides,
                origin,
                review,
                label,
            } => {
                let data_epoch = request["data_epoch"].as_str().unwrap_or_default();
                let option_id = request["option_id"].as_str();
                let input = request["input"].clone();
                let fresh = Client::action_body(data_epoch, &action_id, option_id, input);
                if let (Some(key), Some(command)) = (review, fresh["command_id"].as_str()) {
                    self.review_sends.insert(command.to_owned(), key);
                }
                let acting = origin.map(|origin| Acting {
                    origin,
                    stamp: None,
                    hides,
                    label,
                });
                self.submit_view_request(fresh, acting);
            }
        }
    }

    pub(super) fn ask(&mut self, modal: Modal) {
        self.modal = Some(modal);
    }


    pub fn flash(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            kind: FlashKind::Success,
            at: now_millis(),
            ttl: FLASH_MS,
        });
    }

    pub fn pending(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            kind: FlashKind::Pending,
            at: now_millis(),
            ttl: FLASH_MS,
        });
    }

    pub fn fail(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            kind: FlashKind::Error,
            at: now_millis(),
            ttl: FLASH_MS,
        });
    }

    /// A card that no longer waits for the user is never drawn: its pane closes and the flash
    /// says why it went.
    fn close_settled_card(&mut self, key: &str, view: ViewDocument) {
        self.card_view = None;
        self.card_view_asked = None;
        if self.view_navigation.last().map(String::as_str) == Some(key) {
            self.view_navigation.pop();
            self.load_selected_view();
        } else if let Some(id) = key.strip_prefix("card:") {
            self.hide(Key(id.to_owned()));
        }
        self.sync_open_views();
        if self.review_sent.as_ref().is_some_and(|sent| sent.0 == key) {
            return;
        }
        self.flash(view.empty.map(|notice| notice.text).unwrap_or_default());
    }

    /// A notice the user must not miss: it stays longer than a flash.
    pub fn notice(&mut self, text: impl Into<String>) {
        self.flash = Some(Flash {
            text: text.into(),
            kind: FlashKind::Error,
            at: now_millis(),
            ttl: NOTICE_MS,
        });
    }

    /// The daemon already settled the card while the queue still lists it: it stays out until a
    /// newer view says where it is.
    fn hide(&mut self, key: Key) {
        let (version, epoch) = self.home_view.as_ref()
            .map(|home| (Some(home.version.clone()), home.data_epoch.clone()))
            .unwrap_or_default();
        self.optimistic.hold(key, version, epoch);
        self.reconcile();
    }

    /// Every call to alondrad runs here. Tests never reach the user's live alondrad.
    fn spawn(&self, job: impl FnOnce(&SyncSender<Msg>, &MacTarget) + Send + 'static) {
        #[cfg(test)]
        self.spawned.borrow_mut().push(match &self.target {
            MacTarget::Local => None,
            MacTarget::Direct(connection) => Some(connection.mac().mac_id.clone()),
        });
        if cfg!(test) {
            return;
        }
        let tx = self.tx.clone();
        let target = self.target.clone();
        std::thread::spawn(move || job(&tx, &target));
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
        self.spawn(|_, target| {
            let _ = Client::for_target(target, Duration::from_secs(5))
                .post(ops::PRESENCE, &json!({"client": "tui"}));
        });
    }

    /// A request that is not a domain command. It answers as its own message; a route the
    /// daemon does not serve yet says so instead of failing silently.
    fn op(&mut self, key: Option<Key>, path: String, body: Value, ok: String) {
        #[cfg(test)]
        self.posted.borrow_mut().push((path.clone(), body.clone()));
        let mac = self.remote_mac();
        self.action_sent();
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(15))
                .post(&path, &body)
                .map(|v| crate::delivery::response_notice(&v, mac.as_deref()).unwrap_or(ok))
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
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(30))
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
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(30))
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
                        self.submit_view_request(request, None);
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

    /// Copies a dropped file into Alondra's storage while the user keeps writing.
    fn attach(&mut self, path: std::path::PathBuf) {
        if self.composer.as_ref().and_then(|composer| composer.form.as_ref()).is_some_and(|form| form.loaded && !form.submit.as_ref().is_some_and(|submit| submit.attachments)) {
            self.fail(tr!("This form does not take attachments", "Este formulario no admite adjuntos"));
            return;
        }
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
        self.spawn(move |tx, target| {
            if preview && let Ok(image) = image::open(&path) {
                let _ = tx.send(Msg::Preview {
                    seq,
                    image: image.thumbnail(PREVIEW_PIXELS, PREVIEW_PIXELS),
                });
            }
            let result = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|bytes| {
                    Client::for_target(target, Duration::from_secs(120))
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

    fn on_msg(&mut self, mut m: Msg) {
        if let Msg::ViewHome(Ok(view)) | Msg::ViewCard { result: Ok(view), .. } | Msg::ViewSection { result: Ok(view), .. }
        | Msg::FormView { result: Ok(view), .. } = &mut m
        {
            view_render::placeholders(view);
        }
        if matches!(m, Msg::Posted { .. }) {
            self.action_answered();
        }
        match m {
            Msg::ViewHome(Ok(view)) => {
                if view.key != self.screen_view_key() {
                    return;
                }
                self.screen_view_asked = None;
                self.optimistic.reconcile(&view.version, &view.data_epoch);
                self.view_data_epoch = Some(view.data_epoch.clone());
                self.view_status = Some(view.status.clone());
                let changed = self.home_view.as_ref().is_none_or(|home| home.key != view.key || home.version != view.version);
                self.home_view = Some(view);
                if changed {
                    for section in self.open_sections.clone() {
                        self.load_section(&section);
                    }
                }
                self.loaded = true;
                self.connected = true;
                self.offline = false;
                self.reconcile();
            }
            Msg::ViewSection { section, result } => match result {
                Ok(page) => {
                    if let Some(rows) = page.sections.into_iter().find(|part| part.id == section) {
                        self.section_pages.insert(section, rows);
                        self.reconcile();
                    }
                }
                Err(error) => self.fail(error),
            },
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
                if self.rules_read(&key) {
                    self.rules_loaded(Ok(&view));
                }
                if key == mcp_servers::VIEW {
                    self.mcp_servers_loaded(Ok(&view));
                }
                if key == mcp_servers::GENERAL {
                    self.general_loaded(&view);
                }
                if key == mcp_servers::ACCOUNTS {
                    self.accounts_loaded(&view);
                }
                if view.view == alondra_core::views::SETTLED_VIEW {
                    if self.selected_view_key().as_deref() == Some(&key) {
                        self.close_settled_card(&key, view);
                    }
                } else if self.selected_view_key().as_deref() == Some(&key) {
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
                }
            }
            Msg::ViewCard {
                key,
                result: Err(error),
            } => {
                if self.rules_read(&key) {
                    self.rules_loaded(Err(&error));
                }
                if key == mcp_servers::VIEW {
                    self.mcp_servers_loaded(Err(&error));
                }
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
            Msg::ViewAction { request, result } => {
                self.action_answered();
                self.view_action_done(request, result);
            }
            Msg::Service(service) => self.service_seen(service),
            Msg::DiskVersion { version, disk } => self.disk_checked(version, disk),
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
            Msg::DaemonIdentity(answer) => self.daemon_identity = Some(answer),
            Msg::PendingDevices(pending) => {
                let now = now_millis();
                self.pairing_answered
                    .retain(|id| pending.iter().any(|d| &d.id == id));
                self.pairing = pending
                    .into_iter()
                    .filter(|d| d.expires_at_ms > now && !self.pairing_answered.contains(&d.id))
                    .collect();
            }
            Msg::Report(answer) => self.crash_report_answered(answer),
            Msg::PairingAnswered {
                device,
                allow,
                result,
            } => {
                let name = device.name.clone();
                match result {
                    Ok(()) if allow => self.flash(trf!(
                        "“{name}” can use Alondra now",
                        "«{name}» ya puede usar Alondra"
                    )),
                    Ok(()) => self.flash(trf!("“{name}” was denied", "«{name}» denegado")),
                    Err(e) => {
                        self.pairing_answered.remove(&device.id);
                        self.pairing.insert(0, device);
                        self.fail(trf!(
                            "Could not answer “{name}”: {e}",
                            "No pude responder a «{name}»: {e}"
                        ));
                    }
                }
            }
            Msg::Posted { key, result } => match result {
                Ok(text) => {
                    if let Some(k) = key {
                        self.reviews.remove(&k);
                        if self.reader.as_ref().is_some_and(|reader| reader.key == k) {
                            self.reader = None;
                        }
                    }
                    self.flash(text);
                }
                Err(e) => {
                    self.fail(trf!(
                        "Not confirmed: {e}. Check before repeating.",
                        "Sin confirmar: {e}. Revísalo antes de repetir."
                    ));
                }
            },
            Msg::McpOpened { card, server, result } => self.mcp_opened(card, server, result),
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

    fn view_action_done(&mut self, request: Value, result: Result<ViewActionReply, Delivery>) {
        let command = request["command_id"].as_str().unwrap_or_default();
        let outcome = match result {
            Ok(ViewActionReply::Outcome(outcome)) => outcome,
            Err(Delivery::Unsent | Delivery::Unanswered) => {
                self.optimistic.failed(command, now_millis());
                return;
            }
            Err(Delivery::Answered(message)) => Outcome {
                kind: OutcomeKind::Failed,
                title: tr!("Your Mac couldn't do it.", "Tu Mac no pudo hacerlo.").to_owned(),
                body: message,
                next: None,
            },
        };
        let applied = matches!(outcome.kind, OutcomeKind::Done | OutcomeKind::Started);
        let shown = self.home_view.as_ref().map(|home| home.version.clone());
        let settled = self.optimistic.settle(command, applied, shown)
            .unwrap_or(optimistic::Settled { target: None, was_overdue: false, comes_back: false });
        let origin = settled.target.clone();
        let review = self.review_sends.remove(command);
        let text = match &outcome.body {
            Some(body) => format!("{}\n{body}", outcome.title),
            None => outcome.title.clone(),
        };
        self.account_order_answered(command, applied);
        self.rule_answered(command, applied);
        self.setting_answered(request["action_id"].as_str().unwrap_or_default(), applied);
        match outcome.kind {
            OutcomeKind::Done | OutcomeKind::Started => {
                if let Some(key) = &origin {
                    self.left_on.remove(key);
                    self.left_views.remove(key);
                    if settled.was_overdue {
                        self.reconcile();
                    }
                }
                let text = match review {
                    Some(key) => self.review_delivered(key),
                    None => text,
                };
                if outcome.kind == OutcomeKind::Done {
                    self.flash(text);
                } else {
                    self.pending(text);
                }
            }
            OutcomeKind::Refused | OutcomeKind::Failed => {
                if settled.comes_back {
                    self.show_again(origin);
                }
                self.fail(text);
            }
            OutcomeKind::Changed => {
                self.flash = None;
                if settled.comes_back {
                    self.show_again(origin.clone());
                }
                let Some(next) = outcome.next else {
                    self.fail(text);
                    return;
                };
                let modal = Modal {
                    title: outcome.title,
                    body: outcome.body.unwrap_or_default(),
                    button: next.label.clone(),
                    destructive: false,
                    then: ModalThen::Changed {
                        request,
                        action_id: next.id.clone(),
                        hides: next.resolves_card,
                        origin,
                        review,
                        label: next.label.clone(),
                    },
                };
                if self.typing() || self.modal.is_some() {
                    self.deferred_modal = Some(modal);
                } else {
                    self.ask(modal);
                }
            }
        }
    }

    /// Sends again what is due and brings back the cards whose answer is overdue.
    fn tick_answers(&mut self, now: i64) -> bool {
        for request in self.optimistic.due(now) {
            self.post_action(request);
        }
        let overdue = self.optimistic.expire(now);
        let any = !overdue.is_empty();
        for overdue in overdue {
            self.answer_overdue(overdue);
        }
        any
    }

    /// An answer the daemon has not confirmed in time: its card is back where it was, and the user
    /// is told which answer did not arrive. It goes on being sent.
    fn answer_overdue(&mut self, overdue: optimistic::Overdue) {
        self.rule_overdue(&overdue.command);
        self.show_again(overdue.target);
        let optimistic::Answer { card, label } = overdue.answer;
        self.notice(match (card.is_empty(), label.is_empty()) {
            (false, false) => trf!(
                "Your Mac has not confirmed your answer “{label}” to “{card}”. The card is back; Alondra keeps sending it.",
                "Tu Mac aún no ha confirmado tu respuesta «{label}» a «{card}». La tarjeta ha vuelto; Alondra sigue enviándola."
            ),
            (false, true) => trf!(
                "Your Mac has not confirmed your answer to “{card}”. The card is back; Alondra keeps sending it.",
                "Tu Mac aún no ha confirmado tu respuesta a «{card}». La tarjeta ha vuelto; Alondra sigue enviándola."
            ),
            (true, false) => trf!(
                "Your Mac has not confirmed your answer “{label}”. Alondra keeps sending it.",
                "Tu Mac aún no ha confirmado tu respuesta «{label}». Alondra sigue enviándola."
            ),
            (true, true) => tr!(
                "Your Mac has not confirmed an answer. Alondra keeps sending it.",
                "Tu Mac aún no ha confirmado una respuesta. Alondra sigue enviándola."
            )
            .to_owned(),
        });
    }

    /// The comments reached the Mac: they leave the reader, which closes, and the user is told
    /// where they went.
    fn review_delivered(&mut self, key: Key) -> String {
        self.reviews.remove(&key);
        let agent = self
            .reader
            .as_ref()
            .filter(|reader| reader.key == key)
            .map(|reader| reader.agent.clone())
            .or_else(|| self.review_agents.remove(&key))
            .unwrap_or_default();
        if self.reader.as_ref().is_some_and(|reader| reader.key == key) {
            self.reader = None;
        }
        self.review_sent = Some(key);
        let sent = if agent.is_empty() {
            tr!("Comments sent", "Comentarios enviados").to_owned()
        } else {
            trf!("Comments sent to {agent}", "Comentarios enviados a {agent}")
        };
        sent
    }

    fn show_again(&mut self, origin: Option<Key>) {
        let Some(key) = origin else {
            return;
        };
        let left_on = self.left_on.remove(&key);
        let view = self.left_views.remove(&key);
        if left_on.is_none_or(|left_on| left_on == self.sel) {
            self.sel = Some(key);
            self.card_view = view;
            self.card_view_asked = None;
        }
        self.reconcile();
    }

    /// The home view as the user sees it: cards whose action is in flight are not there.
    pub(super) fn shown_home(&self) -> Option<std::borrow::Cow<'_, ViewDocument>> {
        let home = self.home_view.as_ref()?;
        let paged = |section: &alondra_core::views::Section| section.collapsed && section.items.is_empty()
            && self.open_sections.contains(&section.id) && self.section_pages.contains_key(&section.id);
        let blocks = home.sections.iter().any(view_render::has_blocks);
        if self.optimistic.is_empty() && !home.sections.iter().any(paged) && !blocks {
            return Some(std::borrow::Cow::Borrowed(home));
        }
        let mut shown = home.clone();
        view_render::lift_blocks(&mut shown.sections);
        for section in &mut shown.sections {
            if paged(section) && let Some(page) = self.section_pages.get(&section.id) {
                section.items = page.items.clone();
                section.more = page.more.clone();
            }
            if self.optimistic.hides(&Key(view_render::section_key(&section.id))) {
                section.items.clear();
                section.more = None;
                section.actions.clear();
                section.count = section.count.map(|_| 0);
                continue;
            }
            let before = section.items.len();
            section.items.retain(|item| match item {
                alondra_core::views::Item::Card(card) => !self.optimistic.hides(&Key(card.id.clone())),
                _ => true,
            });
            let gone = (before - section.items.len()) as u32;
            if let Some(count) = section.count.as_mut() {
                *count = count.saturating_sub(gone);
            }
        }
        Some(std::borrow::Cow::Owned(shown))
    }

    fn on_timers(&mut self) -> bool {
        let now = now_millis();
        let mut dirty = false;
        if self.flash.as_ref().is_some_and(|f| now - f.at > f.ttl) {
            self.flash = None;
            dirty = true;
        }
        if now - self.last_clock >= CLOCK_MS {
            self.last_clock = now;
            dirty = true;
        }
        dirty |= self.tick_answers(now);
        let waiting = self.pairing.len();
        self.pairing.retain(|d| d.expires_at_ms > now);
        dirty |= self.pairing.len() != waiting;
        dirty |= self.version_tick();
        self.load_threads();
        dirty
    }

    fn on_paste(&mut self, s: &str) {
        if let Some(form) = self.composer.as_mut().and_then(|composer| composer.form.as_mut()).filter(|form| form.loaded && form.primary().is_none()) {
            if form.popup.is_some() {
                for character in md::one_line(s).chars() { let _ = form.key(KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)); }
                return;
            }
            if dropped::dropped_files(s).is_none() { return; }
        }
        if self.composer.as_ref().is_some_and(|c| c.target.takes_files())
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
        let flow = self.on_key_pressed(k);
        let flow = match self.pending_flow.take() {
            Some(pending) if matches!(flow, Flow::Go) => pending,
            _ => flow,
        };
        if self.modal.is_none() && !self.typing() {
            self.modal = self.deferred_modal.take();
        }
        flow
    }

    fn on_key_pressed(&mut self, k: KeyEvent) -> Flow {
        self.pressed = k.code;
        if k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c') {
            return Flow::Quit;
        }
        if let Some(flow) = self.version_key(&k) {
            return flow;
        }
        if self.modal.is_some() {
            return self.press(k);
        }
        if self.help {
            self.help_key(k);
            return Flow::Go;
        }
        if self.composer.is_some() {
            self.composer_key(k);
            return Flow::Go;
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
        if self.screen == Screen::Tasks
            && self.reader.is_none()
            && !self.schedules_open
            && self.search.as_ref().is_some_and(|s| s.editing)
        {
            self.search_key(k);
            return Flow::Go;
        }
        let key = match self.keymap.read(k) {
            Cmd::Pending => return Flow::Go,
            Cmd::Move(keys::Motion::Top) if k.code == KeyCode::Char('g') => KeyEvent::new(KeyCode::Home, KeyModifiers::NONE),
            _ => k,
        };
        self.press(key)
    }

    fn switch_screen(&mut self) {
        self.show(match self.screen {
            Screen::Inbox => Screen::Tasks,
            Screen::Tasks => Screen::Inbox,
        });
    }

    fn show(&mut self, screen: Screen) {
        self.screen = screen;
        self.card_menu = None;
        self.view_navigation.clear();
        self.settings_after_view = None;
        self.reader_after_view = None;
        self.schedules_open = false;
        self.rules = None;
        self.mcp_servers = None;
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

    fn reload_schedules(&mut self) {
        self.home_view = None;
        self.screen_view_asked = None;
        self.load_screen_view();
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
            "settings" | "ajustes" | "mcp" => {
                self.reader = None;
                self.open_mcp_servers(line == "mcp");
            }
            "report" | "informe" => {
                self.reader = None;
                self.open_crash_report("other");
            }
            n if n.parse::<usize>().is_ok_and(|n| n > 0) && self.reader.is_none() => {
                let i = n.parse::<isize>().unwrap_or(1) - 1;
                self.move_inbox(i);
            }
            other => self.fail(trf!(
                "Not a command: {other}  (try :q, :help, :inbox, :tasks, :settings, :report, :<number>)",
                "No es un comando: {other}  (prueba :q, :help, :inbox, :tasks, :ajustes, :informe, :<número>)"
            )),
        }
        Flow::Go
    }

    fn compose(&mut self, target: Target, prefill: &str) {
        let view = match &target {
            Target::Form { view } => Some(view.clone()),
            _ => None,
        };
        let form = view.as_deref().map(work::OptionsForm::new);
        self.composer = Some(Composer {
            target,
            input: TextInput::with(prefill),
            chips: vec![],
            form,
        });
        if let Some(view) = view {
            self.load_form(&view, None);
        }
    }

    /// Asks alondrad for the open issues, which it keeps for ten minutes unless `refresh`.
    fn load_issues(&self, refresh: bool) {
        self.spawn(move |tx, target| {
            let key = if refresh {
                "issue_picker?refresh=1"
            } else {
                "issue_picker"
            };
            let result = Client::for_target(target, Duration::from_secs(40))
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
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::FormView {
                view,
                project,
                result,
            });
        });
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
        if c.form.as_ref().is_some_and(|form| form.loaded && form.primary().is_none())
            && (!matches!(k.code, KeyCode::Esc | KeyCode::Enter) || k.code == KeyCode::Enter && k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::SHIFT)) { return; }
        match k.code {
            KeyCode::Esc => self.composer = None,
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
        let Some(mut c) = self.composer.take() else {
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
            Target::McpReturn { .. } | Target::AccountName { .. } | Target::AccountPosition => true,
            Target::ReportDescription { .. } => false,
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
                acting,
                review,
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
                if let (Some(key), Some(command)) = (review, request["command_id"].as_str()) {
                    self.review_sends.insert(command.to_owned(), key);
                }
                self.submit_view_request(request, acting);
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
            Target::McpReturn { card, server, .. } => self.mcp_give_back(&card, &server, text),
            Target::ReportDescription { failure } => self.build_crash_report(failure, text),
            Target::AccountPosition => self.account_order_positioned(&text),
            Target::AccountName { provider, mut argv } => {
                let legacy = provider == alondra_core::accounts::Provider::Claude && argv == ["accounts", "add"];
                if !legacy && alondra_core::views::accounts::account_arguments(Some(provider.as_str()), Some(&argv), None, true).is_none() {
                    c.target = Target::AccountName { provider, argv };
                    self.composer = Some(c);
                    self.fail(tr!("Reload Accounts before adding this account", "Vuelve a cargar Cuentas antes de añadir esta cuenta"));
                    return;
                }
                if argv.iter().any(|arg| arg == "--provider") || text.starts_with('-') { argv.push("--".into()); }
                argv.push(text);
                let flow = self.account_sign_in(argv);
                self.pending_flow = Some(flow).filter(|flow| !matches!(flow, Flow::Go));
            }
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
        if let Some(reason) = form.intent_error() { self.composer = Some(c); self.fail(reason); return; }
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
        self.submit_view_request(request, None);
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
                            if let Some(why) = card.why.as_ref().filter(|why| !why.is_empty()) {
                                text.push_str(&format!("\n\n{why}"));
                            }
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
            let task_id = first_card.and_then(view_render::card_task_id);
            self.reader = Some(Reader {
                key: Key(view.key),
                agent,
                task_id,
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
                picking: false,
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
            let task_id = view_render::card_task_id(&card);
            let (glyph, color) = (view_render::icon(card.icon), view_render::icon_color(card.icon));
            let mut text = format!("# {}", card.title);
            if let Some(why) = card.why.as_ref().filter(|why| !why.is_empty()) {
                text.push_str(&format!("\n\n{why}"));
            }
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
                task_id,
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
                picking: false,
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
                        Some(Ok(body)) => blocks.push(Block::para(&files::not_text(body)).opening(md::Target {
                            kind: md::TargetKind::Stored,
                            url: route.clone(),
                            label: name.clone(),
                        })),
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
            Cmd::Back | Cmd::Left | Cmd::Expand | Cmd::Right | Cmd::Pending | Cmd::Other(_) => {}
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

    /// A digit in the reader answers with that option, and the comments go with it.
    fn answer_with_comments(&mut self, k: KeyEvent) {
        let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else {
            return;
        };
        let has_review = self
            .reviews
            .get(&key)
            .is_some_and(|review| !review.is_empty());
        if has_review {
            let accepts_text = self
                .card_view
                .as_ref()
                .and_then(|view| inbox::view_key_choice(view, k))
                .is_some_and(|(action, option)| inbox::carries_words(&action, option.as_ref()));
            if !accepts_text {
                self.fail(tr!("That key sends no words: your comments stay here. Press an option to send them with it.",
                    "Esa tecla no envía texto: tus comentarios se quedan aquí. Pulsa una opción para enviarlos con ella."));
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
            if let Target::ViewAction { review, .. } = &mut composer.target {
                *review = Some(key);
            }
        }
    }

    fn open_block(&mut self) {
        let Some(r) = self.reader.as_mut() else {
            return;
        };
        let Some(b) = r.blocks.get(r.cursor) else {
            return;
        };
        match b.targets.as_slice() {
            [] => {}
            [target] => {
                let target = target.clone();
                self.open_target(&target);
            }
            _ => {
                r.picking = true;
                self.flash = None;
            }
        }
    }

    fn pick_target(&mut self, key: KeyEvent) {
        let Some(r) = self.reader.as_mut() else {
            return;
        };
        r.picking = false;
        let KeyCode::Char(c @ '1'..='9') = key.code else {
            return;
        };
        let target = r
            .blocks
            .get(r.cursor)
            .and_then(|b| b.targets.get(c as usize - '1' as usize))
            .cloned();
        if let Some(target) = target {
            self.open_target(&target);
        }
    }

    fn open_target(&mut self, target: &md::Target) {
        let label = md::one_line(&target.label);
        if let Some(route) = target.stored() {
            let name = match crate::doc_images::stored_image(route) {
                Some(reference) => reference.rsplit('/').next().unwrap_or(reference).to_owned(),
                None => target.label.clone(),
            };
            let route = route.to_owned();
            self.pending(trf!("Opening “{label}”…", "Abriendo «{label}»…"));
            self.spawn(move |tx, mac| {
                if let Err(e) = open_stored(mac, &name, &route) {
                    let _ = tx.send(Msg::SyncError(trf!(
                        "Could not open {name}: {e}",
                        "No he podido abrir {name}: {e}"
                    )));
                }
            });
        } else if target.openable() {
            open_with_mac(&target.url);
            self.flash(trf!("Opened “{label}”", "Abierto «{label}»"));
        } else if target.local() {
            self.fail(tr!(
                "This image is a file on the Mac that wrote the document: it cannot be opened from here",
                "Esta imagen es un fichero del Mac que escribió el documento: no se puede abrir desde aquí"
            ));
        } else {
            let url = &target.url;
            self.fail(trf!(
                "Only web and mail links open from a document: {url}",
                "Desde un documento solo se abren enlaces web y de correo: {url}"
            ));
        }
    }

    fn comment_block(&mut self) {
        let Some(r) = self.reader.as_ref() else {
            return;
        };
        let Some(b) = r.blocks.get(r.cursor).filter(|b| b.commentable()) else {
            return;
        };
        let (block, quote) = (r.cursor, b.quote());
        let prefill = self
            .reviews
            .get(&r.key)
            .and_then(|rv| rv.comments.iter().find(|c| c.block == block))
            .map(|c| c.text.clone())
            .unwrap_or_default();
        self.compose(Target::Comment { block, quote }, &prefill);
    }

    fn comment_general(&mut self) {
        let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else {
            return;
        };
        let prefill = self
            .reviews
            .get(&key)
            .map(|rv| rv.general.clone())
            .unwrap_or_default();
        self.compose(Target::General { send: false }, &prefill);
    }

    fn delete_comment(&mut self) {
        let Some((key, block)) = self.reader.as_ref().map(|r| (r.key.clone(), r.cursor)) else {
            return;
        };
        if let Some(rv) = self.reviews.get_mut(&key) {
            let before = rv.comments.len();
            rv.comments.retain(|c| c.block != block);
            if rv.comments.len() < before {
                self.flash(tr!("Comment deleted", "Comentario borrado"));
            }
        }
    }

    /// The action the reader's comments are sent with: feedback on a delivery, a reply otherwise.
    fn review_action(&self, view: &ViewDocument) -> Option<alondra_core::views::Action> {
        view_render::actions(view).into_iter().find(|action| {
            action.enabled
                && matches!(action.input, alondra_core::views::Input::Text { .. })
                && alondra_core::views::action_id::ActionId::parse(&action.id).is_ok_and(|id| {
                    matches!(
                        id.verb,
                        alondra_core::views::action_id::Verb::Changes
                            | alondra_core::views::action_id::Verb::Reply
                    )
                })
        })
    }

    /// The reader's comments go back to whoever the reading is about: feedback on a delivery, a
    /// reply to information, and on a question the option the user picks carries them.
    fn send_review(&mut self) {
        let Some(key) = self.reader.as_ref().map(|r| r.key.clone()) else {
            return;
        };
        if let Some(view) = self.card_view.as_ref().filter(|view| view.key == key.0) {
            let Some(action) = self.review_action(view) else {
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
            let acting = Acting {
                origin: self.sel.clone().unwrap_or_else(|| Key(view.key.clone())),
                stamp: action.stamp.clone(),
                hides: action.resolves_card,
                label: action.label.clone(),
            };
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
                    acting: Some(acting),
                    review: Some(key.clone()),
                },
                &text,
            );
            if let Some(composer) = self.composer.as_mut() {
                composer.chips.extend(chips);
            }
            return;
        }
        self.send_review_to_node(key);
    }

    /// The card went (its node asked again about a newer revision, or withdrew it) while the user
    /// was commenting: the comments go to that node as a message, about what the user read.
    fn send_review_to_node(&mut self, key: Key) {
        let Some(review) = self.reviews.get(&key).filter(|review| !review.is_empty()) else {
            self.fail(tr!(
                "No comments to send",
                "No hay comentarios que enviar"
            ));
            return;
        };
        let Some((task, name, title)) = self.reader.as_ref().and_then(|reader| {
            Some((reader.task_id.clone()?, reader.agent.clone(), reader.title.clone()))
        }) else {
            self.fail(tr!(
                "This card is gone and has no node to send your comments to: they stay here.",
                "Esta tarjeta ya no está y no tiene nodo al que enviar tus comentarios: se quedan aquí."
            ));
            return;
        };
        let title = md::one_line(&title);
        let text = format!(
            "{}\n\n{}",
            trf!(
                "My comments on “{title}”, on the version I was reading (its card was gone when I sent them):",
                "Mis comentarios sobre «{title}», sobre la versión que estaba leyendo (su tarjeta ya no estaba cuando los envié):"
            ),
            review.feedback()
        );
        let mut message = ops::Message::to(None, ops::MessageKind::Message, text);
        message.attachments = dropped::stored(&review.chips);
        message.task = Some(task.clone());
        let ok = if name.is_empty() {
            tr!("Comments sent", "Comentarios enviados").to_owned()
        } else {
            trf!("Comments sent to {name}", "Comentarios enviados a {name}")
        };
        self.pending(tr!("Sending…", "Enviando…"));
        #[cfg(test)]
        self.posted.borrow_mut().push((format!("task:{task}"), message.body()));
        self.action_sent();
        self.spawn(move |tx, target| {
            let client = Client::for_target(target, Duration::from_secs(30));
            let result = task_node(&client, &task)
                .map(|agent| ops::Message { agent: Some(agent), ..message })
                .and_then(|message| client.post(&message.path(), &message.body()).map_err(|e| format!("{e:#}")))
                .and_then(|reply| comments_delivered(&reply, ok));
            let _ = tx.send(Msg::Posted { key: Some(key), result });
        });
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

}

/// The node working on a task, read from the message action its detail offers.
fn task_node(client: &Client, task: &alondra_core::domain::TaskId) -> Result<AgentId, String> {
    let view = client.views(&format!("task:{task}"), None).map_err(|e| format!("{e:#}"))?;
    crate::cli::view_action(&view, |action| action.id.starts_with("msg:") && !action.id.starts_with("msg:view@"))
        .and_then(|action| alondra_core::views::action_id::ActionId::parse(&action.id).ok())
        .map(|id| AgentId(id.subject))
        .ok_or_else(|| tr!("This task has no node to send your comments to: they stay here.",
            "Esta tarea no tiene nodo al que enviar tus comentarios: se quedan aquí.").to_owned())
}

/// Whether comments sent to a node reached it: handed to its session, or queued for it, as a
/// node of the Agent SDK takes them while a turn of its runs.
fn comments_delivered(reply: &serde_json::Value, ok: String) -> Result<String, String> {
    use alondra_core::delivery::State;
    match reply.get("delivery_state").map(|state| serde_json::from_value::<State>(state.clone())) {
        None | Some(Ok(State::Handed | State::Dispatching)) => Ok(ok),
        Some(Ok(state)) => Err(crate::delivery::state_text(state).to_owned()),
        Some(Err(_)) => Err(tr!("Unknown delivery outcome", "Resultado de entrega desconocido").to_owned()),
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
