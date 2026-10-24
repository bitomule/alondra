//! Settings, the terminal's settings screen (`,` or `:settings` from any screen). General first:
//! this Mac's settings, the `settings` section of the machine view (how PRs merge, nodes at once),
//! each picked from its menu. Then Claude accounts: each account with how it is and what it takes,
//! how new work is spread, and adding one, whose sign-in runs on this screen. Then MCP servers: per
//! project, each server its nodes use with how it is, and the projects no node runs in, each checked
//! on demand. Its buttons are the view's: Connect opens the sign-in as a card's does, Reconnect,
//! Sign out and Check go to the daemon.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use alondra_core::views::{Action, Item, Row, View};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

use super::keys::Motion;
use super::{App, Client, Flow, Key, Msg};

pub const VIEW: &str = alondra_core::views::mcp_servers::KEY;
pub const GENERAL: &str = "machine";
pub const ACCOUNTS: &str = alondra_core::views::accounts::KEY;
const GENERAL_SECTION: &str = "settings";
const ACCENT: Color = Color::LightCyan;

/// A line of the screen: a project's heading, what a section says of itself, or one of its rows.
pub enum Entry<'a> {
    Group(&'static str),
    Heading(&'a str),
    Note(&'a str),
    Row(&'a Row),
}

#[derive(Default)]
pub struct McpServers {
    pub general: Option<View>,
    pub accounts: Option<View>,
    pub view: Option<View>,
    pub failed: Option<String>,
    pub sel: usize,
    pub scroll: usize,
    /// What the user just picked on a General row, by the action's id: the row says it at once,
    /// until the Mac refuses it or a view of the Mac says how it is now.
    pub chosen: HashMap<String, Chosen>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Chosen {
    row: String,
    line: String,
    before: Option<String>,
    applied: bool,
}

impl McpServers {
    pub fn title(&self) -> Option<&str> {
        self.view.as_ref()?.title.as_deref()
    }

    pub fn entries(&self) -> Vec<Entry<'_>> {
        let mut entries = vec![];
        let general: Vec<&Row> = self.general.iter().flat_map(|view| &view.sections)
            .filter(|section| section.id == GENERAL_SECTION)
            .flat_map(|section| &section.items)
            .filter_map(|item| match item {
                Item::Row(row) => Some(row),
                _ => None,
            })
            .collect();
        if !general.is_empty() {
            entries.push(Entry::Group(tr!("General", "General")));
            entries.extend(general.into_iter().map(Entry::Row));
        }
        if self.accounts.as_ref().is_some_and(|view| !view.sections.is_empty()) {
            entries.push(Entry::Group(tr!("Claude accounts", "Cuentas de Claude")));
            entries.extend(sections(self.accounts.as_ref()));
        }
        if !entries.is_empty() && self.view.as_ref().is_some_and(|view| !view.sections.is_empty()) {
            entries.push(Entry::Group(tr!("MCP servers", "Servidores MCP")));
        }
        entries.extend(sections(self.view.as_ref()));
        entries
    }

    pub fn rows(&self) -> Vec<&Row> {
        self.entries().into_iter().filter_map(|entry| match entry {
            Entry::Row(row) => Some(row),
            Entry::Group(_) | Entry::Heading(_) | Entry::Note(_) => None,
        }).collect()
    }

    pub fn selected(&self) -> Option<&Row> {
        self.rows().get(self.sel).copied()
    }

    pub fn step(&mut self, motion: Motion) {
        let last = self.rows().len().saturating_sub(1);
        self.sel = match motion {
            Motion::Down => self.sel + 1,
            Motion::Up => self.sel.saturating_sub(1),
            Motion::Top => 0,
            Motion::Bottom => last,
            _ => self.sel,
        }
        .min(last);
    }

    /// `row` as the screen draws it: with what the user just picked in place of what it said.
    pub fn shown<'a>(&self, row: &'a Row) -> Cow<'a, Row> {
        match self.chosen.values().find(|chosen| chosen.row == row.id) {
            Some(chosen) => Cow::Owned(Row { subtitle: Some(chosen.line.clone()), ..row.clone() }),
            None => Cow::Borrowed(row),
        }
    }

    /// The user picked `option` of `action` on the General row `row`.
    pub fn choose(&mut self, row: &Row, action: &Action, option: &alondra_core::views::ActionOption) {
        let general = self.general.iter().flat_map(|view| &view.sections).filter(|section| section.id == GENERAL_SECTION)
            .flat_map(|section| &section.items).any(|item| matches!(item, Item::Row(general) if general.id == row.id));
        if !general {
            return;
        }
        let label = option.label.split(" · ").next().unwrap_or(&option.label);
        let line = format!("{}: {label}", row.title);
        self.chosen.insert(action.id.clone(), Chosen { row: row.id.clone(), line, before: row.subtitle.clone(), applied: false });
    }

    /// The data epoch the screen's buttons act on.
    pub fn epoch(&self) -> Option<String> {
        self.view.as_ref().or(self.accounts.as_ref()).or(self.general.as_ref()).map(|view| view.data_epoch.clone())
    }

    fn clamp(&mut self) {
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
    }
}

/// What `view` shows, section by section: its heading, its rows and what it says of itself.
fn sections(view: Option<&View>) -> Vec<Entry<'_>> {
    let mut entries = vec![];
    for section in view.iter().flat_map(|view| &view.sections) {
        if let Some(title) = &section.title {
            entries.push(Entry::Heading(title));
        }
        entries.extend(section.items.iter().filter_map(|item| match item {
            Item::Row(row) => Some(Entry::Row(row)),
            Item::Notice(notice) => Some(Entry::Note(&notice.text)),
            _ => None,
        }));
    }
    entries
}

/// Claude's sign-in for an account, run on this screen by `alondra accounts`.
#[derive(Clone, Debug, PartialEq)]
pub struct SignIn {
    pub args: Vec<String>,
}

/// The buttons of a row, numbered from 1 in the order the view gives them.
pub fn actions(row: &Row) -> Vec<&Action> {
    row.actions.iter().filter(|action| action.enabled).take(9).collect()
}

/// What a row's button says: a setting's menu is named like its row, so it reads «Change…».
pub fn button_label<'a>(row: &Row, action: &'a Action) -> &'a str {
    match action.label == row.title {
        true => tr!("Change…", "Cambiar…"),
        false => &action.label,
    }
}

fn badge(row: &Row) -> Option<Span<'static>> {
    let label = row.badges.first()?;
    let color = match label.tone {
        alondra_core::views::Tone::Success => Color::Green,
        alondra_core::views::Tone::Critical => Color::Red,
        alondra_core::views::Tone::YourTurn | alondra_core::views::Tone::Warning => Color::Yellow,
        alondra_core::views::Tone::Info | alondra_core::views::Tone::Accent => Color::Cyan,
        alondra_core::views::Tone::Neutral => Color::Gray,
    };
    Some(Span::styled(format!("  {}", label.text), Style::new().fg(color)))
}

/// A server's name and how it is, what it says of it, its buttons when selected, and below it what
/// a check found, indented.
pub fn row_lines(row: &Row, width: usize, selected: bool) -> Vec<Line<'static>> {
    let bar = Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT));
    let text_width = width.saturating_sub(2);
    let lead = vec![bar.clone(), Span::raw(" ")];
    let mut title = vec![Span::styled(row.title.clone(), if selected { Style::new().add_modifier(Modifier::BOLD) } else { Style::new() })];
    title.extend(badge(row));
    let mut lines = super::md::wrap(title, text_width, lead.clone(), lead.clone());
    for line in row.subtitle.iter().flat_map(|subtitle| subtitle.lines()).filter(|line| !line.is_empty()) {
        lines.extend(super::md::wrap(vec![Span::styled(line.to_owned(), Style::new().fg(Color::Gray))], text_width, lead.clone(), lead.clone()));
    }
    for child in &row.children {
        let indent = vec![bar.clone(), Span::raw("   ")];
        let mut spans = vec![Span::raw(child.title.clone())];
        spans.extend(badge(child));
        lines.extend(super::md::wrap(spans, text_width.saturating_sub(2), indent.clone(), indent));
    }
    if selected {
        let buttons: Vec<Span<'static>> = actions(row).into_iter().enumerate().flat_map(|(index, action)| [
            Span::styled(format!("{}", index + 1), Style::new().fg(ACCENT).bold()),
            Span::raw(format!(" {}   ", button_label(row, action))),
        ]).collect();
        if !buttons.is_empty() {
            lines.extend(super::md::wrap(buttons, text_width, lead.clone(), lead));
        }
    }
    lines
}

impl App {
    pub(super) fn open_mcp_servers(&mut self) {
        self.mcp_servers = Some(McpServers::default());
        self.load_mcp_servers();
        self.load_general();
        self.load_accounts();
    }

    fn load_accounts(&mut self) {
        self.spawn(|tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(ACCOUNTS, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key: ACCOUNTS.into(), result });
        });
    }

    pub(super) fn accounts_loaded(&mut self, view: &View) {
        let Some(screen) = self.mcp_servers.as_mut() else { return };
        screen.accounts = Some(view.clone());
        screen.clamp();
        self.sync_open_views();
    }

    /// Claude's sign-in for an account runs on this Mac's screen, never on another Mac's.
    pub(super) fn account_sign_in(&mut self, args: Vec<String>) -> Flow {
        if self.remote_mac().is_some() {
            let command = std::iter::once("alondra".to_owned()).chain(args.iter().map(|arg| match arg.contains(' ') {
                true => alondra_core::views::accounts::shell_word(arg),
                false => arg.clone(),
            })).collect::<Vec<_>>().join(" ");
            self.fail(trf!(
                "Claude's sign-in opens on the Mac it is for: run {command} there",
                "El inicio de sesión de Claude se abre en el Mac de la cuenta: ejecuta {command} allí"
            ));
            return Flow::Go;
        }
        Flow::AccountSignIn(SignIn { args })
    }

    pub(super) fn account_signed_in(&mut self, result: anyhow::Result<String>) {
        match result {
            Ok(words) if !words.is_empty() => self.flash(words),
            Ok(_) => self.flash(tr!("Done", "Hecho")),
            Err(error) => self.fail(format!("{error:#}")),
        }
        if self.mcp_servers.is_some() {
            self.load_accounts();
        }
    }

    fn load_general(&mut self) {
        self.spawn(|tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(GENERAL, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key: GENERAL.into(), result });
        });
    }

    pub(super) fn general_loaded(&mut self, view: &View) {
        let Some(screen) = self.mcp_servers.as_mut() else { return };
        screen.general = Some(view.clone());
        let rows: Vec<&Row> = view.sections.iter().flat_map(|section| &section.items)
            .filter_map(|item| match item {
                Item::Row(row) => Some(row),
                _ => None,
            }).collect();
        screen.chosen.retain(|_, chosen| !chosen.applied
            && rows.iter().any(|row| row.id == chosen.row && row.subtitle == chosen.before));
        screen.clamp();
        self.sync_open_views();
    }

    /// The Mac answered a pick on a General row: refused, the row says again how it is; done, the
    /// row is asked again.
    pub(super) fn setting_answered(&mut self, action_id: &str, applied: bool) {
        let Some(screen) = self.mcp_servers.as_mut() else { return };
        if !applied {
            screen.chosen.remove(action_id);
            return;
        }
        let Some(chosen) = screen.chosen.get_mut(action_id) else { return };
        chosen.applied = true;
        self.load_general();
    }

    pub(super) fn close_mcp_servers(&mut self) {
        self.mcp_servers = None;
        self.sync_open_views();
    }

    fn load_mcp_servers(&mut self) {
        self.spawn(|tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(VIEW, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key: VIEW.into(), result });
        });
    }

    pub(super) fn mcp_servers_loaded(&mut self, result: Result<&View, &str>) {
        let Some(screen) = self.mcp_servers.as_mut() else { return };
        match result {
            Ok(view) => {
                screen.view = Some(view.clone());
                screen.failed = None;
                screen.clamp();
                self.sync_open_views();
            }
            Err(error) if screen.view.is_none() => screen.failed = Some(error.to_owned()),
            Err(_) => {}
        }
    }

    /// The selected row's button `index`, from 0: its menu of options first when it has one, and
    /// through the view's confirmation when it asks for one.
    pub(super) fn mcp_server_action(&mut self, index: usize) -> Flow {
        let Some(screen) = self.mcp_servers.as_ref() else { return Flow::Go };
        let Some(epoch) = screen.epoch() else { return Flow::Go };
        let Some((row, action)) = screen.selected().and_then(|row| Some((row.id.clone(), (*actions(row).get(index)?).clone()))) else {
            return Flow::Go;
        };
        self.press_card_action(action, None, &Key(row), epoch)
    }
}
