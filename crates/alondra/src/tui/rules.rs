//! A list of permissions, each with the one button that takes it back: this Mac's «Permissions
//! allowed always» (the `allowed_rules` rows of the machine view, with Remove) or what a task
//! allowed (the `allowed_in_task` rows of its view, with Revoke). One taken back leaves the list
//! at once and comes back if the Mac refuses.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use alondra_core::views::{Action, Item, Row, View};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use serde_json::json;

use super::keys::Motion;
use super::{Acting, App, Client, Key, Msg};

const ALWAYS_VIEW: &str = "machine";
const ALWAYS_SECTION: &str = "allowed_rules";
const TASK_SECTION: &str = "allowed_in_task";
const ACCENT: Color = Color::LightYellow;

/// Which list: what this Mac allows always, or what one task allowed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Always,
    Task(String),
}

impl Source {
    pub fn view(&self) -> String {
        match self {
            Source::Always => ALWAYS_VIEW.into(),
            Source::Task(task) => format!("task:{task}"),
        }
    }

    fn section(&self) -> &'static str {
        match self {
            Source::Always => ALWAYS_SECTION,
            Source::Task(_) => TASK_SECTION,
        }
    }
}

/// Whether a task's view lists something it allowed.
pub fn allows_something(view: &View) -> bool {
    view.sections.iter().any(|section| section.id == TASK_SECTION && !section.items.is_empty())
}

pub struct Rules {
    pub source: Source,
    pub view: Option<View>,
    pub failed: Option<String>,
    pub sel: usize,
    pub scroll: usize,
    hidden: HashSet<String>,
    /// The rule each Remove in flight takes away, by its command.
    removing: HashMap<String, String>,
}

impl Rules {
    pub fn new(source: Source) -> Self {
        Rules { source, view: None, failed: None, sel: 0, scroll: 0, hidden: HashSet::new(), removing: HashMap::new() }
    }

    pub fn title(&self) -> Option<&str> {
        self.view.as_ref()?.sections.iter().find(|section| section.id == self.source.section())?.title.as_deref()
    }

    pub fn rows(&self) -> Vec<&Row> {
        self.view
            .iter()
            .flat_map(|view| &view.sections)
            .filter(|section| section.id == self.source.section())
            .flat_map(|section| &section.items)
            .filter_map(|item| match item {
                Item::Row(row) if !self.hidden.contains(&row.id) => Some(row),
                _ => None,
            })
            .collect()
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

pub fn remove_action(row: &Row) -> Option<&Action> {
    row.actions.iter().find(|action| action.enabled && !action.id.is_empty())
}

/// A rule in words, wrapped with its code in colour, and under it its repo, day and task.
pub fn row_lines(row: &Row, width: usize, selected: bool) -> Vec<Line<'static>> {
    let bar = Span::styled(if selected { "▌" } else { " " }, Style::new().fg(ACCENT));
    let text_width = width.saturating_sub(2);
    let mut lines: Vec<Line<'static>> = super::md::render(&super::md::parse(&row.title), text_width)
        .lines
        .into_iter()
        .map(|line| {
            let mut spans = vec![bar.clone(), Span::raw(" ")];
            spans.extend(line.spans.into_iter().map(|span| {
                if selected { span.add_modifier(Modifier::BOLD) } else { span }
            }));
            Line::from(spans)
        })
        .collect();
    if let Some(subtitle) = row.subtitle.as_ref().filter(|subtitle| !subtitle.is_empty()) {
        let lead = vec![bar.clone(), Span::raw(" ")];
        lines.extend(super::md::wrap(
            vec![Span::styled(subtitle.clone(), Style::new().fg(Color::Gray))],
            text_width,
            lead.clone(),
            lead,
        ));
    }
    lines
}

impl App {
    pub(super) fn open_rules(&mut self, source: Source) {
        self.rules = Some(Rules::new(source));
        self.load_rules();
    }

    pub(super) fn close_rules(&mut self) {
        self.rules = None;
        self.sync_open_views();
    }

    fn load_rules(&mut self) {
        let Some(key) = self.rules.as_ref().map(|rules| rules.source.view()) else { return };
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25))
                .views(&key, None)
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key, result });
        });
    }

    /// Whether `key` is the view the open list reads.
    pub(super) fn rules_read(&self, key: &str) -> bool {
        self.rules.as_ref().is_some_and(|rules| rules.source.view() == key)
    }

    pub(super) fn rules_loaded(&mut self, result: Result<&View, &str>) {
        let Some(rules) = self.rules.as_mut() else { return };
        match result {
            Ok(view) => {
                rules.view = Some(view.clone());
                rules.failed = None;
                rules.clamp();
                self.sync_open_views();
            }
            Err(error) if rules.view.is_none() => rules.failed = Some(error.to_owned()),
            Err(_) => {}
        }
    }

    pub(super) fn remove_rule(&mut self) {
        let Some(rules) = self.rules.as_mut() else { return };
        let Some((id, action, epoch)) = rules.view.as_ref().and_then(|view| {
            let row = rules.selected()?;
            Some((row.id.clone(), remove_action(row)?.clone(), view.data_epoch.clone()))
        }) else {
            return;
        };
        let request = Client::action_body(&epoch, &action.id, None, json!({}));
        if let Some(command) = request["command_id"].as_str() {
            rules.removing.insert(command.to_owned(), id.clone());
        }
        rules.hidden.insert(id.clone());
        rules.clamp();
        let acting = Acting { origin: Key(id), stamp: action.stamp.clone(), hides: false, label: action.label.clone() };
        self.submit_view_request(request, Some(acting));
    }

    /// A Remove the Mac has not confirmed in time: the rule is back in the list while it goes on
    /// being sent, and leaves with the list asked again if it arrives.
    pub(super) fn rule_overdue(&mut self, command: &str) {
        let Some(rules) = self.rules.as_mut() else { return };
        if let Some(id) = rules.removing.get(command) {
            rules.hidden.remove(id);
        }
    }

    /// The Mac answered a Remove: done, the list is asked again; refused, the rule is back.
    pub(super) fn rule_answered(&mut self, command: &str, applied: bool) {
        let Some(rules) = self.rules.as_mut() else { return };
        let Some(id) = rules.removing.remove(command) else { return };
        if applied {
            self.load_rules();
        } else {
            rules.hidden.remove(&id);
        }
    }
}
