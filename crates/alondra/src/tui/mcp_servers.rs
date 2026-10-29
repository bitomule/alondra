//! Settings, the terminal's settings screen (`,` or `:settings` from any screen). General first:
//! this Mac's settings, the `settings` section of the machine view (how PRs merge, nodes at once),
//! each picked from its menu. Then provider accounts: each account with how it is and what it takes,
//! how new work is spread, and adding one, whose sign-in runs on this screen. Then MCP servers, one
//! row with how many there are, that opens their own screen (`:mcp` opens it directly): per
//! project, each server its nodes use with how it is, and the projects no node runs in, each checked
//! on demand. Then Metrics, one row that opens that screen (`M` and `:metrics` open it directly).
//! Its buttons are the view's: Connect opens the sign-in as a card's does, Reconnect,
//! Sign out, Delete and Check go to the daemon.

use std::borrow::Cow;
use std::collections::HashMap;
use std::time::Duration;

use alondra_core::views::{Action, Icon, Item, Row, View};
use alondra_core::accounts::AccountKey;
use serde_json::json;
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};

use super::keys::Motion;
use super::{App, Client, Flow, Key, Msg};

pub const VIEW: &str = alondra_core::views::mcp_servers::KEY;
pub const GENERAL: &str = "machine";
pub const ACCOUNTS: &str = alondra_core::views::accounts::KEY;
const GENERAL_SECTION: &str = "settings";
const SERVERS_ROW: &str = "mcp_servers";
const REPORT_ROW: &str = "crash_report";
const METRICS_ROW: &str = "metrics";
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
    /// The MCP servers' own screen, instead of Settings.
    pub detail: bool,
    pub account_detail: Option<Row>,
    pub order: Option<AccountOrderDraft>,
    servers: Option<Row>,
    report: Option<Row>,
    metrics: Option<Row>,
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

#[derive(Clone, Debug)]
pub struct AccountOrderDraft {
    pub rows: Vec<Row>,
    pub version: String,
    pub initial: Vec<AccountKey>,
    pub epoch: String,
    pub action: Action,
    pub conflict: bool,
    pub command: Option<String>,
}

impl AccountOrderDraft {
    pub fn keys(&self) -> Vec<AccountKey> { self.rows.iter().filter_map(|row| row.account.as_ref().map(|account| account.key.clone())).collect() }
    pub fn move_to(&mut self, selected: usize, target: usize) -> Option<usize> {
        if self.command.is_some() || self.conflict || selected >= self.rows.len() || target >= self.rows.len() { return None; }
        let row = self.rows.remove(selected);
        self.rows.insert(target, row);
        Some(target)
    }
}

impl McpServers {
    pub fn title(&self) -> Option<&str> {
        self.view.as_ref()?.title.as_deref()
    }

    pub fn entries(&self) -> Vec<Entry<'_>> {
        if let Some(order) = &self.order {
            let mut entries = vec![Entry::Group(tr!("Account order", "Orden de cuentas"))];
            entries.push(Entry::Note(if order.command.is_some() { tr!("Waiting for confirmation. This save keeps its original command.", "Esperando confirmación. Este guardado conserva su comando original.") }
                else if order.conflict { tr!("The account configuration changed. Reload to start a new draft.", "La configuración de cuentas cambió. Recarga para empezar un borrador nuevo.") }
                else { tr!("Changes stay here until Save. New work tries accounts from top to bottom.", "Los cambios se quedan aquí hasta Guardar. El trabajo nuevo prueba las cuentas de arriba abajo.") }));
            entries.extend(order.rows.iter().map(Entry::Row));
            return entries;
        }
        if let Some(row) = &self.account_detail { return vec![Entry::Row(row)]; }
        if self.detail {
            return sections(self.view.as_ref());
        }
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
            entries.push(Entry::Group(tr!("Accounts", "Cuentas")));
            entries.extend(sections(self.accounts.as_ref()));
        }
        if let Some(servers) = &self.servers {
            entries.push(Entry::Group(tr!("MCP servers", "Servidores MCP")));
            entries.push(Entry::Row(servers));
        }
        if let Some(metrics) = &self.metrics {
            entries.push(Entry::Group(tr!("Metrics", "Métricas")));
            entries.push(Entry::Row(metrics));
        }
        if let Some(report) = &self.report {
            entries.push(Entry::Group(tr!("Help", "Ayuda")));
            entries.push(Entry::Row(report));
        }
        entries
    }

    /// Whether the selected row is the one that opens a new crash report.
    pub fn on_report_row(&self) -> bool {
        !self.detail && self.selected().is_some_and(|row| row.id == REPORT_ROW)
    }

    fn report_row() -> Row {
        Row {
            id: REPORT_ROW.into(), link: None, icon: Icon::Warning, title: tr!("Send a crash report", "Enviar informe de fallo").into(),
            subtitle: Some(tr!(
                "The logs of this Mac with everything private left out; you see exactly what is sent before it leaves",
                "Los logs de este Mac con todo lo privado fuera; antes de que salga ves exactamente lo que se envía"
            ).into()),
            badges: vec![], trailing: None, depth: 0, struck: false, kind: None, children: vec![], folded: false, actions: vec![], account: None, model: None,
        }
    }

    /// Whether the selected row is the one that opens Metrics.
    pub fn on_metrics_row(&self) -> bool {
        !self.detail && self.selected().is_some_and(|row| row.id == METRICS_ROW)
    }

    /// Whether the selected row is the one that opens the MCP servers' screen.
    pub fn on_servers_row(&self) -> bool {
        !self.detail && self.selected().is_some_and(|row| row.id == SERVERS_ROW)
    }

    /// Opens the MCP servers' screen, or goes back from it to Settings, on its row.
    pub fn show_detail(&mut self, detail: bool) {
        self.detail = detail;
        self.scroll = 0;
        self.sel = match detail {
            true => 0,
            false => self.rows().iter().position(|row| row.id == SERVERS_ROW).unwrap_or(0),
        };
    }

    /// The row Settings leads to the MCP servers with: what their view says of itself.
    fn servers_row(view: Option<&View>) -> Row {
        let title = match view {
            Some(view) => view.summary.clone().unwrap_or_else(|| tr!("Open to see them", "Ábrelo para verlos").to_owned()),
            None => tr!("Loading…", "Cargando…").to_owned(),
        };
        Row {
        account: None, model: None,
            id: SERVERS_ROW.into(), link: None, icon: Icon::Machine, title, subtitle: None, badges: vec![], trailing: None, depth: 0,
            struck: false, kind: None, children: vec![], folded: false, actions: vec![],
        }
    }

    fn metrics_row() -> Row {
        Row {
            id: METRICS_ROW.into(), link: None, icon: Icon::History,
            title: tr!("Cost, tokens and how tasks went", "Coste, tokens y cómo fueron las tareas").to_owned(),
            subtitle: None, badges: vec![], trailing: None, depth: 0, struck: false, kind: None, children: vec![], folded: false, actions: vec![],
            account: None, model: None,
        }
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
        if let Some(order) = &self.order && let Some(position) = order.rows.iter().position(|item| item.id == row.id) {
            return Cow::Owned(Row { title: format!("{}. {}", position + 1, row.title), actions: vec![], ..row.clone() });
        }
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
    if selected {
        for action in row.actions.iter().filter(|action| !action.enabled) { if let Some(reason) = &action.disabled_reason {
            lines.extend(super::md::wrap(vec![Span::styled(format!("{}: {reason}", action.label), Style::new().fg(Color::Gray))], text_width, vec![bar.clone(), Span::raw(" ")], vec![bar.clone(), Span::raw(" ")]));
        } }
    }
    lines
}

impl App {
    pub(super) fn open_mcp_servers(&mut self, detail: bool) {
        self.mcp_servers = Some(McpServers {
            detail, servers: Some(McpServers::servers_row(None)), report: Some(McpServers::report_row()),
            metrics: Some(McpServers::metrics_row()), ..McpServers::default()
        });
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
        let selected = screen.selected().map(|row| row.id.clone());
        if let Some(order) = &mut screen.order {
            if order.epoch != view.data_epoch { order.command = None; }
            if order.epoch != view.data_epoch || (order.command.is_none() && view.account_configuration.as_ref().map(|configuration| &configuration.version) != Some(&order.version)) { order.conflict = true; }
        }
        if let Some(open) = &screen.account_detail {
            let key = open.account.as_ref().map(|account| &account.key);
            screen.account_detail = sections(Some(view)).into_iter().find_map(|entry| match entry {
                Entry::Row(row) if row.account.as_ref().map(|account| &account.key) == key => Some(account_detail(row)), _ => None,
            });
        }
        screen.accounts = Some(view.clone());
        if screen.order.is_none() && screen.account_detail.is_none() && let Some(selected) = selected && let Some(position) = screen.rows().iter().position(|row| row.id == selected) { screen.sel = position; }
        screen.clamp();
        self.sync_open_views();
    }

    /// Native sign-in runs on the Mac that holds this account's credentials.
    pub(super) fn account_sign_in(&mut self, args: Vec<String>) -> Flow {
        if self.remote_mac().is_some() {
            let command = std::iter::once("alondra").chain(args.iter().map(String::as_str))
                .map(alondra_core::views::accounts::shell_word).collect::<Vec<_>>().join(" ");
            let name = self.remote_mac().unwrap_or_else(|| "Mac".into());
            self.fail(trf!("Finish native sign-in on {name}: run {command} there", "Completa el inicio de sesión nativo en {name}: ejecuta {command} allí"));
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

    /// Back: from the MCP servers' screen to Settings, and from Settings out.
    pub(super) fn restore_settings_after_view(&mut self) {
        if !self.settings_after_view.as_ref().is_some_and(|(_, depth)| self.view_navigation.len() <= *depth) { return; }
        self.mcp_servers = self.settings_after_view.take().map(|(settings, _)| settings);
        self.load_mcp_servers();
        self.load_general();
        self.load_accounts();
    }

    pub(super) fn close_mcp_servers(&mut self) {
        match self.mcp_servers.as_mut() {
            Some(screen) if screen.order.as_ref().is_some_and(|order| order.command.is_some()) => {},
            Some(screen) if screen.order.is_some() => { screen.order = None; screen.sel = 0; },
            Some(screen) if screen.account_detail.is_some() => { screen.account_detail = None; screen.sel = 0; },
            Some(screen) if screen.detail => screen.show_detail(false),
            _ => {
                self.mcp_servers = None;
                self.sync_open_views();
            }
        }
    }

    pub(super) fn open_account_detail(&mut self) {
        let Some(screen) = &mut self.mcp_servers else { return };
        if let Some(row) = screen.selected().filter(|row| row.account.is_some()).cloned() { screen.account_detail = Some(account_detail(&row)); screen.sel = 0; screen.scroll = 0; }
    }

    pub(super) fn begin_account_order(&mut self) {
        let Some(screen) = &mut self.mcp_servers else { return };
        if screen.order.as_ref().is_some_and(|order| order.command.is_some()) { return; }
        let Some(view) = &screen.accounts else { return };
        let Some(configuration) = &view.account_configuration else { return };
        let Some(action) = view.sections.iter().flat_map(|section| &section.actions).find(|action| action.enabled && action.id.starts_with("account_order:")).cloned() else { return };
        let rows: Vec<Row> = sections(Some(view)).into_iter().filter_map(|entry| match entry { Entry::Row(row) if row.account.is_some() => Some(row.clone()), _ => None }).collect();
        let initial = rows.iter().filter_map(|row| row.account.as_ref().map(|account| account.key.clone())).collect();
        screen.order = Some(AccountOrderDraft { rows, initial, version: configuration.version.clone(), epoch: view.data_epoch.clone(), action, conflict: false, command: None });
        screen.sel = 0; screen.scroll = 0; screen.account_detail = None;
    }

    pub(super) fn move_account_order(&mut self, down: bool) {
        let Some(screen) = &mut self.mcp_servers else { return };
        let target = if down { screen.sel.saturating_add(1) } else { screen.sel.saturating_sub(1) };
        if let Some(order) = &mut screen.order && let Some(selected) = order.move_to(screen.sel, target) { screen.sel = selected; }
    }

    pub(super) fn account_order_position(&mut self) {
        if self.mcp_servers.as_ref().and_then(|screen| screen.order.as_ref()).is_some_and(|order| order.command.is_none() && !order.conflict) { self.compose(super::Target::AccountPosition, ""); }
    }

    pub(super) fn account_order_positioned(&mut self, text: &str) {
        let Some(screen) = &mut self.mcp_servers else { return };
        if let Ok(position) = text.trim().parse::<usize>() && let Some(order) = &mut screen.order && position > 0 && let Some(selected) = order.move_to(screen.sel, position - 1) { screen.sel = selected; return; }
        self.fail(tr!("Enter a position within the account list.", "Introduce una posición dentro de la lista de cuentas."));
    }

    pub(super) fn save_account_order(&mut self) {
        let Some(screen) = &mut self.mcp_servers else { return };
        let Some(order) = &mut screen.order else { return };
        if order.conflict || order.command.is_some() || order.keys() == order.initial { return; }
        let request = Client::action_body(&order.epoch, &order.action.id, None, json!({"fields": {"order": order.keys(), "expected_config_version": order.version}}));
        order.command = request["command_id"].as_str().map(str::to_owned);
        self.submit_view_request(request, None);
    }

    pub(super) fn account_order_answered(&mut self, command: &str, applied: bool) {
        let Some(screen) = &mut self.mcp_servers else { return };
        if !screen.order.as_ref().is_some_and(|order| order.command.as_deref() == Some(command)) { return; }
        if applied { screen.order = None; screen.sel = 0; self.load_accounts(); }
        else if let Some(order) = &mut screen.order { order.command = None; order.conflict = true; }
    }

    pub(super) fn open_mcp_detail(&mut self) {
        if let Some(screen) = self.mcp_servers.as_mut() {
            screen.show_detail(true);
        }
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
                screen.servers = Some(McpServers::servers_row(Some(view)));
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

fn account_detail(source: &Row) -> Row {
    let mut row = source.clone();
    let Some(account) = &source.account else { return row };
    let provider = alondra_core::views::accounts::provider_name(account.key.provider);
    let mut lines = vec![format!("{} · {} · {}", provider, account.key.account, account.availability.text), account.freshness.text.clone()];
    if let Some(email) = &account.email { lines.push(email.clone()); }
    if let Some(plan) = &account.plan { lines.push(plan.clone()); }
    if let Some(reason) = &account.reason { lines.push(reason.clone()); }
    let count = account.active_nodes;
    lines.push(trf!("{count} active nodes", "{count} nodos activos"));
    if account.windows.is_empty() { lines.push(tr!("Usage not reported", "Uso no comunicado").into()); }
    for window in &account.windows {
        let used = window.used_percent; let label = &window.label; let measured = account_time(window.measured_at);
        lines.push(trf!("{label}: {used}% used · reported {measured}", "{label}: {used}% usado · comunicado {measured}"));
        if let Some(reset) = window.resets_at { let reset = account_time(reset); lines.push(trf!("Resets {reset}", "Se restablece {reset}")); }
        if window.stale { lines.push(tr!("Waiting for a new reading", "Esperando una lectura nueva").into()); }
    }
    if let Some(catalog) = &account.model_catalog {
        lines.push(tr!("Available models", "Modelos disponibles").into());
        for model in &catalog.models {
            lines.push(format!("{} · {}", model.display_name, model.model));
            if !model.reasoning_efforts.is_empty() { let efforts = model.reasoning_efforts.join(", "); lines.push(trf!("Reasoning effort: {efforts}", "Esfuerzo de razonamiento: {efforts}")); }
            if let Some(context) = model.context_window { lines.push(trf!("Context: {context} tokens", "Contexto: {context} tokens")); }
        }
    }
    for action in source.actions.iter().filter(|action| !action.enabled) { if let Some(reason) = &action.disabled_reason { lines.push(format!("{}: {reason}", action.label)); } }
    row.subtitle = Some(lines.join("\n")); row
}

fn account_time(time: alondra_core::Millis) -> String {
    super::view_render::date_time_at_offset(time, 0).map(|(date, clock)| format!("{date} {clock} UTC")).unwrap_or_else(|| tr!("Unknown", "Desconocido").into())
}
