//! Metrics (`M`, `:metrics`, or its row in Settings): what ended tasks cost and how they went, the
//! daemon's `metrics` view drawn as it comes. Its period, comparison, filter and group rows each
//! open another key of that view. The row picked is marked at once while the figures of its key
//! arrive, and Esc goes back through the keys opened before closing.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use alondra_core::views::{Item, Link, Notice, Row, Tone, View};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::keys::Motion;
use super::{App, Client, Msg, view_render};

pub const KEY: &str = alondra_core::views::metrics::KEY;
const ACCENT: Color = Color::LightYellow;
/// The sections that choose what the figures cover: they stay on screen while a new key loads.
const CHOOSERS: [&str; 3] = ["period", "compare", "filter"];

pub fn reads(key: &str) -> bool {
    key == KEY || key.starts_with(&format!("{KEY}?"))
}

pub struct Metrics {
    pub key: String,
    views: HashMap<String, View>,
    back: Vec<String>,
    /// What was on screen when the user picked the row that opened `key`, and that row.
    from: Option<(View, String)>,
    pub failed: Option<String>,
    pub sel: usize,
    pub scroll: usize,
}

impl Metrics {
    fn new() -> Self {
        Metrics { key: KEY.into(), views: HashMap::new(), back: vec![], from: None, failed: None, sel: 0, scroll: 0 }
    }

    pub fn view(&self) -> Option<&View> {
        self.views.get(&self.key)
    }

    /// The view of `key`, or while it loads the choosers of the one before with the picked row
    /// marked as shown and a notice that the figures are on their way.
    pub fn shown(&self) -> Option<Cow<'_, View>> {
        if let Some(view) = self.view() {
            return Some(Cow::Borrowed(view));
        }
        let (from, picked) = self.from.as_ref()?;
        let mut view = from.clone();
        view.sections.retain(|section| CHOOSERS.contains(&section.id.as_str()));
        let mark = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            Item::Row(row) if row.link.is_none() => row.badges.first().cloned(),
            _ => None,
        });
        for section in &mut view.sections {
            if !section.items.iter().any(|item| matches!(item, Item::Row(row) if row.id == *picked)) {
                continue;
            }
            for item in &mut section.items {
                let Item::Row(row) = item else { continue };
                row.badges.clear();
                if row.id == *picked {
                    row.link = None;
                    row.badges.extend(mark.clone());
                }
            }
        }
        let text = match &self.failed {
            Some(error) => error.clone(),
            None => tr!("Loading…", "Cargando…").to_owned(),
        };
        let tone = if self.failed.is_some() { Tone::Critical } else { Tone::Neutral };
        view.notices = vec![Notice { text, tone, action: None }];
        Some(Cow::Owned(view))
    }

    pub fn rows(&self) -> Vec<Row> {
        self.shown().map_or_else(Vec::new, |view| rows(&view).into_iter().cloned().collect())
    }

    pub fn selected(&self) -> Option<Row> {
        self.rows().into_iter().nth(self.sel)
    }

    /// The key the selected row opens.
    pub fn target(&self) -> Option<String> {
        match self.selected()?.link? {
            Link::View { key } => Some(key),
            _ => None,
        }
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

    /// Keeps the selection on the row of the same id, wherever the new view puts it.
    fn reselect(&mut self, id: Option<String>) {
        let rows = self.rows();
        self.sel = id.and_then(|id| rows.iter().position(|row| row.id == id)).unwrap_or(self.sel).min(rows.len().saturating_sub(1));
    }
}

fn rows(view: &View) -> Vec<&Row> {
    view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Row(row) => Some(row),
        _ => None,
    }).collect()
}

/// The sections whose rows are picked as tabs on one line: the one shown and the others.
const TABS: [&str; 2] = ["period", "compare"];

/// The screen as lines, each as wide as `width`: what the view says of itself, then the period and
/// the comparison as tabs, each section under a ruled heading, and every figure in one table; and
/// which lines the selected row takes, and whether that is a whole line or a tab inside one.
pub fn lines(view: &View, sel: usize, width: usize) -> Rendered {
    let mut lines: Vec<Line<'static>> = view.notices.iter().map(view_render::notice_line).collect();
    let columns = view_render::FactColumns::of(view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Facts(facts) => Some(facts),
        _ => None,
    }));
    let tab_titles = view.sections.iter().filter(|section| TABS.contains(&section.id.as_str()))
        .filter_map(|section| section.title.as_deref()).map(super::md::str_width).max().unwrap_or(0);
    let mut rendered = Rendered { lines: vec![], selected: (0, 0), tab: false };
    let mut index = 0;
    let mut tabs_before = false;
    for section in view.sections.iter().filter(|section| !section.items.is_empty()) {
        let rows: Vec<&Row> = section.items.iter().filter_map(|item| match item {
            Item::Row(row) => Some(row),
            _ => None,
        }).collect();
        if TABS.contains(&section.id.as_str()) && rows.len() == section.items.len() {
            if !lines.is_empty() && !tabs_before {
                lines.push(Line::default());
            }
            let title = section.title.clone().unwrap_or_default();
            let lead = vec![Span::styled(format!("{title:<tab_titles$}  "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))];
            let blank = vec![Span::raw(" ".repeat(tab_titles + 2))];
            let start = lines.len();
            let picked = (index..index + rows.len()).contains(&sel).then(|| sel - index);
            let (tabs, on) = tab_lines(&rows, picked, width, lead, blank);
            if let Some(on) = on {
                rendered.selected = (start + on, start + on + 1);
                rendered.tab = true;
            }
            lines.extend(tabs);
            index += rows.len();
            tabs_before = true;
            continue;
        }
        tabs_before = false;
        if !lines.is_empty() {
            lines.push(Line::default());
        }
        if let Some(title) = &section.title {
            let rule = width.saturating_sub(super::md::str_width(title) + 1);
            lines.push(Line::from(vec![
                Span::styled(title.clone(), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                Span::styled(format!(" {}", "─".repeat(rule)), Style::new().fg(Color::DarkGray)),
            ]));
        }
        for item in &section.items {
            match item {
                Item::Row(row) => {
                    let start = lines.len();
                    lines.extend(view_render::row_lines(row, width, index == sel, false));
                    if index == sel {
                        rendered.selected = (start, lines.len());
                        rendered.tab = false;
                    }
                    index += 1;
                }
                Item::Facts(facts) => lines.extend(view_render::facts_lines(facts, width, columns)),
                Item::Notice(notice) => lines.push(view_render::notice_line(notice)),
                _ => {}
            }
        }
    }
    rendered.lines = lines.into_iter().map(|line| padded(line, width)).collect();
    rendered
}

pub struct Rendered {
    pub lines: Vec<Line<'static>>,
    pub selected: (usize, usize),
    /// The selection is one tab on its line, marked by itself rather than by the whole line.
    pub tab: bool,
}

/// Rows as tabs, wrapped under `lead`: the one shown (the one with a mark) in brackets and filled in, the one selected underlined and
/// bold. Returns the lines and which of them holds the selected tab.
fn tab_lines(rows: &[&Row], picked: Option<usize>, width: usize, lead: Vec<Span<'static>>, blank: Vec<Span<'static>>) -> (Vec<Line<'static>>, Option<usize>) {
    let indent: usize = lead.iter().map(Span::width).sum();
    let mut lines = vec![];
    let mut line = lead;
    let mut used = indent;
    let mut on = None;
    for (n, row) in rows.iter().enumerate() {
        let shown = !row.badges.is_empty();
        let text = match shown {
            true => format!("[{}]", row.title),
            false => format!(" {} ", row.title),
        };
        let tab = super::md::str_width(&text);
        if used > indent && used + 1 + tab > width {
            lines.push(Line::from(std::mem::replace(&mut line, blank.clone())));
            used = indent;
        }
        if used > indent {
            line.push(Span::raw(" "));
            used += 1;
        }
        let mut style = match shown {
            true => Style::new().fg(Color::Black).bg(ACCENT).add_modifier(Modifier::BOLD),
            false => Style::new().fg(Color::Gray),
        };
        if picked == Some(n) {
            style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            if !shown {
                style = style.fg(ACCENT);
            }
            on = Some(lines.len());
        }
        line.push(Span::styled(text, style));
        used += tab;
    }
    lines.push(Line::from(line));
    (lines, on)
}

/// `line` cut or filled with spaces to exactly `width` columns.
fn padded(line: Line<'static>, width: usize) -> Line<'static> {
    let mut spans = vec![];
    let mut left = width;
    for span in line.spans {
        if left == 0 {
            break;
        }
        let text = super::md::truncate(&span.content, left);
        left -= super::md::str_width(&text);
        spans.push(Span::styled(text, span.style));
    }
    if left > 0 {
        spans.push(Span::raw(" ".repeat(left)));
    }
    Line::from(spans)
}

impl App {
    pub(super) fn open_metrics(&mut self) {
        self.metrics = Some(Metrics::new());
        self.load_metrics(KEY.into());
    }

    /// Opens the key of the selected row, with the row marked at once.
    pub(super) fn metrics_open_row(&mut self) {
        let Some(screen) = self.metrics.as_mut() else { return };
        let (Some(key), Some(row), Some(view)) = (screen.target(), screen.selected(), screen.shown().map(Cow::into_owned)) else {
            return;
        };
        screen.back.push(std::mem::replace(&mut screen.key, key.clone()));
        screen.from = Some((view, row.id.clone()));
        screen.failed = None;
        screen.reselect(Some(row.id));
        self.load_metrics(key);
        self.sync_open_views();
    }

    /// Back to the key opened before, or out of the screen from the first.
    pub(super) fn metrics_back(&mut self) {
        let Some(screen) = self.metrics.as_mut() else { return };
        let Some(key) = screen.back.pop() else {
            self.metrics = None;
            self.sync_open_views();
            return;
        };
        let id = screen.selected().map(|row| row.id);
        screen.key = key.clone();
        screen.from = None;
        screen.failed = None;
        screen.reselect(id);
        self.load_metrics(key);
        self.sync_open_views();
    }

    fn load_metrics(&mut self, key: String) {
        self.spawn(move |tx, target| {
            let result = Client::for_target(target, Duration::from_secs(25)).views(&key, None).map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::ViewCard { key, result });
        });
    }

    pub(super) fn metrics_loaded(&mut self, key: &str, result: Result<&View, &str>) {
        let Some(screen) = self.metrics.as_mut() else { return };
        let current = screen.key == key;
        let id = screen.selected().map(|row| row.id);
        match result {
            Ok(view) => {
                screen.views.insert(key.to_owned(), view.clone());
                if current {
                    screen.failed = None;
                    screen.from = None;
                    screen.reselect(id);
                }
                self.sync_open_views();
            }
            Err(error) if current && screen.view().is_none() => screen.failed = Some(error.to_owned()),
            Err(_) => {}
        }
    }
}
