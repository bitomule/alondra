//! Settings › MCP servers, the terminal's settings screen (`,` or `:settings` from any screen): per project, each server its nodes use with how it is, and the
//! projects no node runs in, each checked on demand. Its buttons are the view's: Connect opens
//! the sign-in as a card's does, Reconnect, Sign out and Check go to the daemon.

use std::time::Duration;

use alondra_core::views::{Action, Item, Row, View};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

use super::keys::Motion;
use super::{App, Client, Key, Msg};

pub const VIEW: &str = alondra_core::views::mcp_servers::KEY;
const ACCENT: Color = Color::LightCyan;

/// A line of the screen: a project's heading, what a section says of itself, or one of its rows.
pub enum Entry<'a> {
    Heading(&'a str),
    Note(&'a str),
    Row(&'a Row),
}

#[derive(Default)]
pub struct McpServers {
    pub view: Option<View>,
    pub failed: Option<String>,
    pub sel: usize,
    pub scroll: usize,
}

impl McpServers {
    pub fn title(&self) -> Option<&str> {
        self.view.as_ref()?.title.as_deref()
    }

    pub fn entries(&self) -> Vec<Entry<'_>> {
        let mut entries = vec![];
        for section in self.view.iter().flat_map(|view| &view.sections) {
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

    pub fn rows(&self) -> Vec<&Row> {
        self.entries().into_iter().filter_map(|entry| match entry {
            Entry::Row(row) => Some(row),
            Entry::Heading(_) | Entry::Note(_) => None,
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

    fn clamp(&mut self) {
        self.sel = self.sel.min(self.rows().len().saturating_sub(1));
    }
}

/// The buttons of a row, numbered from 1 in the order the view gives them.
pub fn actions(row: &Row) -> Vec<&Action> {
    row.actions.iter().filter(|action| action.enabled).take(9).collect()
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
            Span::raw(format!(" {}   ", action.label)),
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

    /// The selected row's button `index`, from 0: through the view's confirmation when it asks
    /// for one.
    pub(super) fn mcp_server_action(&mut self, index: usize) {
        let Some(screen) = self.mcp_servers.as_ref() else { return };
        let Some((row, action, epoch)) = screen.view.as_ref().and_then(|view| {
            let row = screen.selected()?;
            Some((row.id.clone(), (*actions(row).get(index)?).clone(), view.data_epoch.clone()))
        }) else {
            return;
        };
        self.run_view_action(&action, None, &Key(row), epoch);
    }
}
