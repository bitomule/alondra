//! The keys of the state on screen, in one table: the key bar, the reader's panel, the help screen
//! and the key handler all read it, so a key is shown exactly when it does something.

use alondra_core::views::{Action, ActionOption, ContentKind, Item, View};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::keys::{Cmd, Motion};
use super::{App, Flow, Key, Pane, Screen, Target, view_render};

#[derive(Clone, Debug)]
pub(super) enum Act {
    Move,
    Focus(Pane),
    Back,
    Toggle,
    ToggleChecks,
    Open,
    Diff,
    Documents,
    Form,
    NextScreen,
    NewWork,
    Schedules,
    CloseSchedules,
    NewSchedule,
    Rules,
    TaskRules(String),
    CloseRules,
    RemoveRule,
    McpServers,
    CloseMcpServers,
    OpenMcpDetail,
    McpAction(usize),
    OpenAccountDetail,
    BeginAccountOrder,
    MoveAccountOrder(bool),
    AccountOrderPosition,
    SaveAccountOrder,
    CrashReport,
    ReportOpen,
    ReportSend,
    ReportKeep,
    CloseReport,
    Search,
    Quit,
    Help,
    CmdLine,
    NextMac,
    Button,
    More,
    Reply,
    Pick,
    CloseMenu,
    Server(Box<Action>, Option<Box<ActionOption>>),
    ScreenAction(Box<Action>, Option<String>),
    RowAction(Box<Action>),
    ReaderClose,
    ReaderFind,
    ReaderFindNext,
    ReaderFindPrev,
    ReaderFindClose,
    ReaderComment,
    ReaderGeneral,
    ReaderDelete,
    ReaderSend,
    ReaderAnswer,
    ReaderOpen,
    ReaderPick,
    ReaderPickClose,
    ModalConfirm,
    ModalCancel,
    Pairing(bool),
}

/// Where an entry is listed: on the key bar, or only on the help screen.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Shown {
    Bar,
    Help,
}

#[derive(Clone, Debug)]
pub(super) struct Binding {
    pub keys: Vec<KeyEvent>,
    pub hint: String,
    pub label: String,
    pub shown: Shown,
    pub act: Act,
}

/// A terminal sends Shift with a capital and with Shift+Tab; the table names them without it.
pub(super) fn same(a: &KeyEvent, b: &KeyEvent) -> bool {
    let strip = |key: &KeyEvent| match key.code {
        KeyCode::Char(_) | KeyCode::BackTab => key.modifiers - KeyModifiers::SHIFT,
        _ => key.modifiers,
    };
    a.code == b.code && strip(a) == strip(b)
}

pub(super) fn plain(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn char(c: char) -> KeyEvent {
    plain(KeyCode::Char(c))
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

fn digit(index: usize) -> Option<KeyEvent> {
    char::from_digit(index as u32 + 1, 10).filter(|_| index < 9).map(char)
}

/// The key a server hint names: one character, `enter`, `space` or `esc`.
fn hinted(hint: &str) -> Option<KeyEvent> {
    let mut chars = hint.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(char(c)),
        _ => match hint.to_ascii_lowercase().as_str() {
            "enter" | "return" => Some(plain(KeyCode::Enter)),
            "space" => Some(char(' ')),
            "esc" | "escape" => Some(plain(KeyCode::Esc)),
            _ => None,
        },
    }
}

fn selection_keys() -> Vec<KeyEvent> {
    vec![char('j'), char('k'), plain(KeyCode::Down), plain(KeyCode::Up), ctrl('e'), ctrl('y'),
        char('G'), plain(KeyCode::End), plain(KeyCode::Home)]
}

fn page_keys() -> Vec<KeyEvent> {
    vec![ctrl('d'), ctrl('u'), ctrl('f'), ctrl('b'), plain(KeyCode::PageDown), plain(KeyCode::PageUp), char(' ')]
}

fn motion(key: &KeyEvent) -> Option<Motion> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    Some(match key.code {
        KeyCode::Char('j') | KeyCode::Down if !ctrl => Motion::Down,
        KeyCode::Char('k') | KeyCode::Up if !ctrl => Motion::Up,
        KeyCode::Char('e') if ctrl => Motion::Down,
        KeyCode::Char('y') if ctrl => Motion::Up,
        KeyCode::Char('G') | KeyCode::End => Motion::Bottom,
        KeyCode::Home => Motion::Top,
        KeyCode::Char('d') if ctrl => Motion::HalfDown,
        KeyCode::Char('u') if ctrl => Motion::HalfUp,
        KeyCode::Char('f') if ctrl => Motion::PageDown,
        KeyCode::Char('b') if ctrl => Motion::PageUp,
        KeyCode::PageDown | KeyCode::Char(' ') => Motion::PageDown,
        KeyCode::PageUp => Motion::PageUp,
        _ => return None,
    })
}

/// Entries in order of precedence: a key an earlier entry takes is not offered again further down.
#[derive(Default)]
pub(super) struct Table(pub Vec<Binding>);

impl Table {
    fn add(&mut self, keys: Vec<KeyEvent>, hint: &str, label: impl Into<String>, act: Act) {
        self.put(keys, hint, label, Shown::Bar, act);
    }

    fn help(&mut self, keys: Vec<KeyEvent>, hint: &str, label: impl Into<String>, act: Act) {
        self.put(keys, hint, label, Shown::Help, act);
    }

    fn put(&mut self, keys: Vec<KeyEvent>, hint: &str, label: impl Into<String>, shown: Shown, act: Act) {
        let free: Vec<KeyEvent> = keys.into_iter()
            .filter(|key| !self.0.iter().any(|taken| taken.keys.iter().any(|other| same(other, key))))
            .collect();
        if free.is_empty() {
            return;
        }
        let hint = if hint.is_empty() { name(&free[0]) } else { hint.to_owned() };
        self.0.push(Binding { keys: free, hint, label: label.into(), shown, act });
    }

    pub fn find(&self, key: &KeyEvent) -> Option<&Binding> {
        self.0.iter().find(|binding| binding.keys.iter().any(|other| same(other, key)))
    }
}

/// How the key bar names a key: `⏎`, `Esc`, `Shift+R` for a capital, `^d` for Ctrl.
pub(super) fn name(key: &KeyEvent) -> String {
    let base = match key.code {
        KeyCode::Enter => "⏎".to_owned(),
        KeyCode::Esc => "Esc".to_owned(),
        KeyCode::Tab => "Tab".to_owned(),
        KeyCode::BackTab => "Shift+Tab".to_owned(),
        KeyCode::Char(' ') => tr!("Space", "Espacio").to_owned(),
        KeyCode::Char(c) if c.is_ascii_uppercase() => format!("Shift+{c}"),
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}"),
    };
    if key.modifiers.contains(KeyModifiers::CONTROL) { format!("^{base}") } else { base }
}

fn has_diff(view: &View) -> bool {
    view.sections.iter().flat_map(|section| &section.items)
        .any(|item| matches!(item, Item::ContentRef(content) if content.kind == ContentKind::Diff))
}

fn has_form(view: &View) -> Option<&Action> {
    view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Form(form) if form.submit.enabled => Some(&form.submit),
        _ => None,
    })
}

impl App {
    /// The detail document of what is selected, when it is the one loaded.
    pub(super) fn current_view(&self) -> Option<&View> {
        let selected = self.selected_view_key()?;
        self.card_view.as_ref().filter(|view| view.key == selected)
    }

    fn selected_fold(&self) -> Option<bool> {
        let id = &self.sel.as_ref()?.0;
        let home = self.shown_home()?;
        view_render::rows(&home).iter().any(|(row, _)| row.id == *id && row.folded)
            .then(|| self.open_rows.contains(id))
    }

    fn header_selected(&self) -> Option<bool> {
        let id = view_render::header_section(&self.sel.as_ref()?.0)?;
        Some(self.open_sections.contains(id))
    }

    fn detail_overflows(&self) -> bool {
        self.detail_view.shown && self.detail_view.total > self.detail_view.h
    }

    pub(super) fn bindings(&self) -> Table {
        let mut t = Table::default();
        let finding = self.reader.as_ref().is_some_and(|r| r.find.as_ref().is_some_and(|find| find.editing));
        let searching = self.screen == Screen::Tasks && self.reader.is_none() && !self.schedules_open
            && self.search.as_ref().is_some_and(|search| search.editing);
        if (self.composer.is_some() || self.cmdline.is_some() || finding || searching) && self.modal.is_none() {
            return t;
        }
        if let Some(modal) = &self.modal {
            t.add(vec![plain(KeyCode::Enter)], "⏎", modal.button.clone(), Act::ModalConfirm);
            t.add(vec![plain(KeyCode::Esc)], "Esc", tr!("cancel", "cancelar"), Act::ModalCancel);
            return t;
        }
        if self.pairing_answers_keys() && self.target.is_local() && self.pairing_prompt().is_some() {
            t.add(vec![char('p')], "p", tr!("allow the device", "permitir el dispositivo"), Act::Pairing(true));
            t.add(vec![char('d')], "d", tr!("deny it", "denegarlo"), Act::Pairing(false));
        }
        if let Some(menu) = &self.card_menu {
            let keys: Vec<KeyEvent> = (0..menu.entries.len()).filter_map(digit).collect();
            let hint = if keys.len() > 1 { format!("1-{}", keys.len()) } else { "1".into() };
            t.add(keys, &hint, tr!("pick", "elige"), Act::Pick);
            if menu.scrolls() {
                t.add(vec![plain(KeyCode::Up), plain(KeyCode::Down), char('k'), char('j')], "↑↓", tr!("move", "mover"), Act::Pick);
                t.add(vec![plain(KeyCode::Enter)], "⏎", tr!("pick the marked one", "elige la marcada"), Act::Pick);
            }
            t.add(vec![plain(KeyCode::Esc)], "Esc", tr!("back", "volver"), Act::CloseMenu);
            return t;
        }
        if self.crash_report.is_some() {
            self.crash_report_bindings(&mut t);
        } else if self.reader.is_some() {
            self.reader_bindings(&mut t);
        } else if self.rules.is_some() {
            self.rules_bindings(&mut t);
        } else if self.mcp_servers.is_some() {
            self.mcp_servers_bindings(&mut t);
        } else if self.schedules_open {
            self.schedule_bindings(&mut t);
        } else {
            self.list_bindings(&mut t);
        }
        t.help(vec![char('?')], "?", tr!("help", "ayuda"), Act::Help);
        t.help(vec![char(':')], ":", tr!("command line: :q :inbox :tasks :settings :report :<number>",
            "línea de órdenes: :q :inbox :tasks :ajustes :informe :<número>"), Act::CmdLine);
        t.help(vec![char(']')], "]", tr!("next Mac, when this terminal is connected to several",
            "siguiente Mac, si este terminal está conectado a varios"), Act::NextMac);
        t
    }

    fn reader_bindings(&self, t: &mut Table) {
        let Some(r) = self.reader.as_ref() else { return };
        let targets = r.blocks.get(r.cursor).map_or(0, |b| b.targets.len());
        if r.picking && targets > 1 {
            let keys: Vec<KeyEvent> = (0..targets.min(9)).filter_map(digit).collect();
            let hint = format!("1-{}", keys.len());
            t.add(keys, &hint, tr!("open", "abrir"), Act::ReaderPick);
            t.add(vec![plain(KeyCode::Esc)], "Esc", tr!("back", "volver"), Act::ReaderPickClose);
            return;
        }
        let mut moves = selection_keys();
        moves.extend(page_keys());
        t.add(moves, "j/k", tr!("block", "bloque"), Act::Move);
        if r.find.is_some() && r.matches().len() > 1 {
            t.add(vec![char('n')], "n", tr!("next match", "siguiente"), Act::ReaderFindNext);
            t.add(vec![char('N')], "Shift+N", tr!("previous match", "anterior"), Act::ReaderFindPrev);
        }
        if r.find.is_some() {
            t.add(vec![plain(KeyCode::Esc)], "Esc", tr!("close the search", "cerrar la búsqueda"), Act::ReaderFindClose);
        }
        t.add(vec![char('/')], "/", tr!("search", "buscar"), Act::ReaderFind);
        if targets > 0 {
            t.add(vec![char('v')], "v", tr!("open", "abrir"), Act::ReaderOpen);
        }
        if r.blocks.get(r.cursor).is_some_and(|block| block.commentable()) {
            t.add(vec![char('c'), plain(KeyCode::Enter)], "c", tr!("comment", "comentar"), Act::ReaderComment);
        }
        t.add(vec![char('C')], "Shift+C", tr!("general comment", "comentario general"), Act::ReaderGeneral);
        let review = self.reviews.get(&r.key).filter(|review| !review.is_empty());
        if review.is_some_and(|review| review.comments.iter().any(|comment| comment.block == r.cursor)) {
            t.add(vec![char('d')], "d", tr!("delete comment", "borrar comentario"), Act::ReaderDelete);
        }
        let view = self.card_view.as_ref().filter(|view| view.key == r.key.0);
        let sends = match view {
            Some(view) => self.review_action(view).is_some(),
            None => review.is_some() && r.task_id.is_some(),
        };
        if sends {
            t.add(vec![char('s')], "s", tr!("send your comments", "enviar tus comentarios"), Act::ReaderSend);
        }
        if let Some(view) = view {
            self.answer_bindings(t, view, true);
        }
        let mut close = vec![char('o'), char('q'), char('h'), plain(KeyCode::Left)];
        if r.find.is_none() {
            close.insert(0, plain(KeyCode::Esc));
        }
        t.add(close, if r.find.is_none() { "Esc/q" } else { "q" }, tr!("back", "volver"), Act::ReaderClose);
    }

    /// The buttons and actions of the document on screen. In the reader the digits answer, so the
    /// comments go with the option, and letters the reader uses for itself stay its own.
    fn answer_bindings(&self, t: &mut Table, view: &View, reading: bool) {
        let answer = if reading { Act::ReaderAnswer } else { Act::Button };
        if let Some(card) = view_render::typed_card(view) {
            for (index, action) in card.actions.iter().take(view_render::BUTTONS).enumerate() {
                if action.enabled && let Some(key) = digit(index)
                    && (!reading || self.takes_the_comments(view, key))
                {
                    t.add(vec![key], "", super::md::inline_text(&action.label), answer.clone());
                }
            }
            if !card.more.is_empty() && !reading {
                t.add(vec![char('m')], "m", tr!("more", "más"), Act::More);
            }
            if card.reply.as_ref().is_some_and(|reply| reply.enabled) {
                t.add(vec![char('r')], "r", tr!("reply", "responder"), Act::Reply);
            }
            if reading {
                for action in card.actions.iter().chain(&card.more).filter(|action| action.enabled) {
                    let action = Action::from(action.clone());
                    if let Some(key) = action.key.as_deref().and_then(hinted)
                        && key.code.as_char().is_some_and(|c| c.is_ascii_alphabetic())
                    {
                        let option = super::inbox::only_option(&action).map(Box::new);
                        t.add(vec![key], "", super::md::inline_text(&action.label), Act::Server(Box::new(action), option));
                    }
                }
            }
            return;
        }
        for action in view_render::actions(view).into_iter().filter(|action| action.enabled) {
            if let Some(key) = hinted(view_render::action_hint(&action)) {
                let option = super::inbox::only_option(&action).map(Box::new);
                t.add(vec![key], "", super::md::inline_text(&action.label), Act::Server(Box::new(action.clone()), option));
            }
            for (index, option) in action.options.iter().enumerate() {
                let key = match option.key.as_deref() {
                    Some(hint) => hinted(hint),
                    None => digit(index),
                };
                if let Some(key) = key.filter(|key| !reading || self.takes_the_comments(view, *key)) {
                    let act = if reading && key.code.as_char().is_some_and(|c| c.is_ascii_digit()) {
                        Act::ReaderAnswer
                    } else {
                        Act::Server(Box::new(action.clone()), Some(Box::new(option.clone())))
                    };
                    t.add(vec![key], "", option.label.clone(), act);
                }
            }
        }
    }

    /// With comments written, a key is offered only when the comments can go with it: an option or a text action.
    fn takes_the_comments(&self, view: &View, key: KeyEvent) -> bool {
        let Some(reader) = self.reader.as_ref() else { return true };
        if self.reviews.get(&reader.key).is_none_or(|review| review.is_empty()) {
            return true;
        }
        super::inbox::view_key_choice(view, key).is_some_and(|(action, option)| super::inbox::carries_words(&action, option.as_ref()))
    }

    fn schedule_bindings(&self, t: &mut Table) {
        if self.inbox_keys().len() > 1 {
            t.add(selection_keys(), "j/k", tr!("move", "mover"), Act::Move);
        }
        if let Some(home) = self.home_view.as_ref().filter(|view| view.key == "schedules")
            && let Some(selected) = self.sel.as_ref()
            && let Some(row) = view_render::rows(home).into_iter().find_map(|(row, _)| (row.id == selected.0).then_some(row))
        {
            for action in &row.actions {
                if let Some(key) = action.key.as_deref().and_then(hinted) {
                    t.add(vec![key], "", super::md::inline_text(&action.label), Act::RowAction(Box::new(action.clone())));
                }
            }
        }
        if let Some(view) = self.current_view() {
            if has_form(view).is_some() {
                t.add(vec![char('e'), plain(KeyCode::Enter)], "e", tr!("edit", "editar"), Act::Form);
            }
            self.answer_bindings(t, view, false);
        }
        self.screen_action_bindings(t);
        t.add(vec![char('n')], "n", tr!("create", "crear"), Act::NewSchedule);
        t.add(vec![plain(KeyCode::Esc), plain(KeyCode::Tab), char('s')], "Esc", tr!("back", "volver"), Act::CloseSchedules);
        t.help(vec![char('q')], "q", tr!("quit", "salir"), Act::Quit);
    }

    fn rules_bindings(&self, t: &mut Table) {
        let Some(rules) = self.rules.as_ref() else { return };
        if rules.rows().len() > 1 {
            t.add(selection_keys(), "j/k", tr!("move", "mover"), Act::Move);
        }
        if let Some(action) = rules.selected().and_then(super::rules::remove_action) {
            t.add(vec![char('x'), plain(KeyCode::Delete), plain(KeyCode::Backspace)], "x", action.label.clone(), Act::RemoveRule);
        }
        let toggle = match rules.source {
            super::rules::Source::Always => 'a',
            super::rules::Source::Task(_) => 'v',
        };
        t.add(vec![plain(KeyCode::Esc), char(toggle)], "Esc", tr!("back", "volver"), Act::CloseRules);
        t.help(vec![char('q')], "q", tr!("quit", "salir"), Act::Quit);
    }

    fn crash_report_bindings(&self, t: &mut Table) {
        let Some(report) = self.crash_report.as_ref() else { return };
        if let Some(viewing) = &report.viewing {
            if matches!(&viewing.text, Some(Ok(lines)) if lines.len() > 1) {
                let mut moves = selection_keys();
                moves.extend(page_keys());
                t.add(moves, "j/k", tr!("scroll", "desplazar"), Act::Move);
            }
            t.add(vec![plain(KeyCode::Esc), char('q'), char('h'), plain(KeyCode::Left)], "Esc", tr!("back to the report", "volver al informe"), Act::CloseReport);
            return;
        }
        if let Some(built) = report.built() {
            if built.openable().len() > 1 {
                t.add(selection_keys(), "j/k", tr!("move", "mover"), Act::Move);
            }
            t.add(vec![plain(KeyCode::Enter), char('v')], "Enter", tr!("show the file", "ver el fichero"), Act::ReportOpen);
            if built.sendable() {
                t.add(vec![char('y')], "y", tr!("Yes, send it", "Sí, enviarlo"), Act::ReportSend);
                t.add(vec![char('n')], "n", tr!("No, keep it on this Mac", "No, que se quede en este Mac"), Act::ReportKeep);
            }
        }
        t.add(vec![plain(KeyCode::Esc), char('q')], "Esc", tr!("close", "cerrar"), Act::CloseReport);
    }

    fn mcp_servers_bindings(&self, t: &mut Table) {
        let Some(screen) = self.mcp_servers.as_ref() else { return };
        if screen.rows().len() > 1 {
            t.add(selection_keys(), "j/k", tr!("move", "mover"), Act::Move);
        }
        if let Some(order) = &screen.order {
            if order.command.is_none() {
                if !order.conflict {
                    t.add(vec![char('K')], "K", tr!("move up", "subir"), Act::MoveAccountOrder(false));
                    t.add(vec![char('J')], "J", tr!("move down", "bajar"), Act::MoveAccountOrder(true));
                    t.add(vec![char('p')], "p", tr!("position", "posición"), Act::AccountOrderPosition);
                    if order.keys() != order.initial { t.add(vec![plain(KeyCode::Enter)], "Enter", tr!("save", "guardar"), Act::SaveAccountOrder); }
                } else { t.add(vec![char('r')], "r", tr!("reload draft", "recargar borrador"), Act::BeginAccountOrder); }
                t.add(vec![plain(KeyCode::Esc)], "Esc", tr!("cancel", "cancelar"), Act::CloseMcpServers);
            }
            return;
        }
        if screen.account_detail.is_none() && screen.selected().is_some_and(|row| row.account.is_some()) {
            t.add(vec![plain(KeyCode::Enter)], "Enter", tr!("account details", "detalles de cuenta"), Act::OpenAccountDetail);
        }
        if !screen.detail && screen.accounts.as_ref().is_some_and(|view| view.sections.iter().flat_map(|section| &section.actions).any(|action| action.enabled && action.id.starts_with("account_order:"))) {
            t.add(vec![char('o')], "o", tr!("reorder accounts", "ordenar cuentas"), Act::BeginAccountOrder);
        }
        if screen.on_report_row() {
            t.add(vec![plain(KeyCode::Enter), char('1')], "Enter", tr!("open", "abrir"), Act::CrashReport);
        } else if screen.on_servers_row() {
            t.add(vec![plain(KeyCode::Enter), char('1')], "Enter", tr!("open", "abrir"), Act::OpenMcpDetail);
        } else if let Some(row) = screen.selected() {
            for (index, action) in super::mcp_servers::actions(row).into_iter().enumerate() {
                let mut keys: Vec<KeyEvent> = digit(index).into_iter().collect();
                if index == 0 && (row.account.is_none() || screen.account_detail.is_some()) {
                    keys.push(plain(KeyCode::Enter));
                }
                t.add(keys, &(index + 1).to_string(), super::mcp_servers::button_label(row, action).to_owned(), Act::McpAction(index));
            }
        }
        t.add(vec![plain(KeyCode::Esc), char(',')], "Esc", tr!("back", "volver"), Act::CloseMcpServers);
        t.help(vec![char('q')], "q", tr!("quit", "salir"), Act::Quit);
    }

    fn screen_action_bindings(&self, t: &mut Table) {
        let Some(home) = self.shown_home() else { return };
        for (section, action) in super::draw::screen_actions(&home) {
            if let Some(key) = hinted(view_render::action_hint(&action)) {
                t.add(vec![key], "", super::md::inline_text(&action.label), Act::ScreenAction(Box::new(action), section));
            }
        }
    }

    fn list_bindings(&self, t: &mut Table) {
        let loaded = self.home_view.is_some();
        let detail = self.focus == Pane::Detail && self.detail_view.shown;
        if loaded && detail && self.detail_overflows() {
            let mut moves = selection_keys();
            moves.extend(page_keys());
            t.add(moves, "j/k", tr!("scroll", "desplazar"), Act::Move);
        } else if loaded && !detail && self.inbox_keys().len() > 1 {
            t.add(selection_keys(), "j/k", tr!("move", "mover"), Act::Move);
        }
        if !self.view_navigation.is_empty() {
            t.add(vec![plain(KeyCode::Esc)], "Esc", tr!("back", "volver"), Act::Back);
        }
        if detail {
            t.add(vec![char('h'), plain(KeyCode::Left), plain(KeyCode::Esc)], "h/Esc", tr!("list", "lista"), Act::Focus(Pane::List));
        }
        let open_close = |open: bool, opened: &'static str, closed: &'static str| if open { opened } else { closed };
        if let Some(open) = self.header_selected() {
            t.add(vec![char('o'), plain(KeyCode::Enter)], "o/⏎",
                open_close(open, tr!("hide", "ocultar"), tr!("show", "mostrar")), Act::Toggle);
        } else if let Some(open) = self.selected_fold() {
            t.add(vec![char('o'), plain(KeyCode::Enter)], "o/⏎",
                open_close(open, tr!("fold", "plegar"), tr!("unfold", "desplegar")), Act::Toggle);
        } else if let Some(view) = self.current_view() {
            if has_form(view).is_some() {
                t.add(vec![char('e'), plain(KeyCode::Enter)], "e", tr!("edit", "editar"), Act::Form);
            }
            if has_diff(view) {
                t.add(vec![char('d')], "d", tr!("diff and comments", "diff y comentarios"), Act::Diff);
            }
            if view_render::documents_menu(view).is_some() {
                t.add(vec![char('f')], "f", tr!("documents", "documentos"), Act::Documents);
            }
            if view_render::summarized_checks(view) {
                t.add(vec![char('c')], "c",
                    open_close(self.open_checks.contains(&view.key), tr!("hide checks", "ocultar checks"), tr!("show checks", "ver checks")),
                    Act::ToggleChecks);
            }
            self.answer_bindings(t, view, false);
            if !detail && self.detail_view.shown {
                t.add(vec![char('l'), plain(KeyCode::Right)], "l", tr!("detail", "detalle"), Act::Focus(Pane::Detail));
            }
            match self.screen {
                Screen::Inbox => {
                    let mut keys = vec![plain(KeyCode::Enter), char('o')];
                    if !self.detail_view.shown {
                        keys.extend([char('l'), plain(KeyCode::Right)]);
                    }
                    t.add(keys, "⏎/o", tr!("open", "abrir"), Act::Open);
                }
                Screen::Tasks => {
                    let mut keys = vec![plain(KeyCode::Enter), char('o')];
                    if !self.detail_view.shown {
                        keys.extend([char('l'), plain(KeyCode::Right)]);
                    }
                    t.add(keys, "⏎/o", tr!("expand", "expandir"), Act::Open);
                }
            }
        }
        if !detail && self.detail_overflows() {
            t.add(page_keys(), "^d/^u", tr!("scroll detail", "desplazar detalle"), Act::Move);
        }
        self.screen_action_bindings(t);
        match self.screen {
            Screen::Inbox => {}
            Screen::Tasks => {
                t.add(vec![char('s')], "s", tr!("scheduled", "programadas"), Act::Schedules);
                t.add(vec![char('/')], "/", tr!("search", "buscar"), Act::Search);
                t.add(vec![char('a')], "a", tr!("allowed always", "permitidos siempre"), Act::Rules);
                if let Some(task) = self.card_view.as_ref().filter(|view| super::rules::allows_something(view))
                    .and_then(|view| view.key.strip_prefix("task:"))
                {
                    t.add(vec![char('v')], "v", tr!("allowed in this task", "permitido en esta tarea"), Act::TaskRules(task.to_owned()));
                }
            }
        }
        t.add(vec![char(',')], ",", tr!("settings", "ajustes"), Act::McpServers);
        let next = match self.screen {
            Screen::Inbox => tr!("tasks", "tareas"),
            Screen::Tasks => tr!("inbox", "bandeja"),
        };
        t.add(vec![plain(KeyCode::Tab), plain(KeyCode::BackTab)], "Tab", next, Act::NextScreen);
        t.add(vec![char('n')], "n", tr!("new", "nuevo"), Act::NewWork);
        t.add(vec![char('q')], "q", tr!("quit", "salir"), Act::Quit);
    }

    /// Runs what the table says `key` does now; a key the table does not list does nothing.
    pub(super) fn press(&mut self, key: KeyEvent) -> Flow {
        let table = self.bindings();
        let Some(binding) = table.find(&key) else { return Flow::Go };
        let act = binding.act.clone();
        let flow = self.run(act, key);
        if self.schedules_open {
            self.load_selected_view();
            self.sync_open_views();
        }
        flow
    }

    fn run(&mut self, act: Act, key: KeyEvent) -> Flow {
        match act {
            Act::Move => {
                let Some(motion) = motion(&key) else { return Flow::Go };
                if let Some(report) = self.crash_report.as_mut() {
                    report.step(motion);
                } else if self.reader.is_some() {
                    self.reader_cmd(Cmd::Move(motion));
                } else if let Some(rules) = self.rules.as_mut() {
                    rules.step(motion);
                } else if let Some(screen) = self.mcp_servers.as_mut() {
                    screen.step(motion);
                } else {
                    self.inbox_cmd(Cmd::Move(motion));
                }
            }
            Act::Focus(pane) => self.focus = pane,
            Act::Back => {
                self.view_navigation.pop();
                self.restore_settings_after_view();
                self.card_view = None;
                self.card_view_asked = None;
                self.load_selected_view();
                self.sync_open_views();
            }
            Act::Toggle => {
                let _ = self.toggle_selected_section() || self.toggle_selected_fold();
            }
            Act::ToggleChecks => {
                if let Some(key) = self.current_view().map(|view| view.key.clone())
                    && !self.open_checks.remove(&key)
                {
                    self.open_checks.insert(key);
                }
            }
            Act::Open => self.open_reader(),
            Act::Diff => self.open_reader(),
            Act::Documents => self.card_menu = self.current_view().and_then(view_render::documents_menu),
            Act::Form => {
                if let Some(view) = self.current_view().map(|view| view.key.clone()) {
                    self.compose(Target::Form { view }, "");
                }
            }
            Act::NextScreen => self.switch_screen(),
            Act::NewWork => self.compose(Target::Form { view: "new_request".into() }, ""),
            Act::Schedules => {
                self.schedules_open = true;
                self.home_view = None;
                self.screen_view_asked = None;
                self.load_screen_view();
                self.load_selected_view();
                self.sync_open_views();
            }
            Act::CloseSchedules => {
                self.schedules_open = false;
                self.reload_schedules();
            }
            Act::NewSchedule => self.compose(Target::Form { view: "schedule:new".into() }, ""),
            Act::Rules => self.open_rules(super::rules::Source::Always),
            Act::TaskRules(task) => self.open_rules(super::rules::Source::Task(task)),
            Act::CloseRules => self.close_rules(),
            Act::RemoveRule => self.remove_rule(),
            Act::McpServers => self.open_mcp_servers(false),
            Act::CloseMcpServers => self.close_mcp_servers(),
            Act::OpenMcpDetail => self.open_mcp_detail(),
            Act::McpAction(index) => return self.mcp_server_action(index),
            Act::OpenAccountDetail => self.open_account_detail(),
            Act::BeginAccountOrder => self.begin_account_order(),
            Act::MoveAccountOrder(down) => self.move_account_order(down),
            Act::AccountOrderPosition => self.account_order_position(),
            Act::SaveAccountOrder => self.save_account_order(),
            Act::CrashReport => self.open_crash_report("other"),
            Act::ReportOpen => self.open_report_file(),
            Act::ReportSend => self.send_crash_report(),
            Act::ReportKeep => self.keep_crash_report(),
            Act::CloseReport => self.close_crash_report(),
            Act::Search => {
                self.search = Some(super::Search {
                    input: super::TextInput::default(),
                    editing: true,
                    typed_at: alondra_core::now_millis(),
                    remote: super::Remote::Idle,
                });
                self.navigate_view("search");
            }
            Act::Quit => return Flow::Quit,
            Act::Help => {
                self.help = true;
                self.help_scroll = 0;
            }
            Act::CmdLine => self.cmdline = Some(super::TextInput::default()),
            Act::NextMac => return Flow::NextMac,
            Act::Button | Act::More | Act::Reply => {
                return self.view_action_key(key).unwrap_or(Flow::Go);
            }
            Act::Pick | Act::CloseMenu => return self.card_menu_key(key),
            Act::Server(action, option) => {
                let Some(view) = self.current_view().or_else(|| {
                    let reading = self.reader.as_ref()?.key.0.clone();
                    self.card_view.as_ref().filter(|view| view.key == reading)
                }) else {
                    return Flow::Go;
                };
                let epoch = view.data_epoch.clone();
                let selected = self.sel.clone().unwrap_or_else(|| Key(view.key.clone()));
                return self.press_card_action(*action, option.map(|option| *option), &selected, epoch);
            }
            Act::ScreenAction(action, section) => {
                let Some(home) = self.home_view.as_ref() else { return Flow::Go };
                let epoch = home.data_epoch.clone();
                let selected = Key(section.map_or_else(|| home.key.clone(), |id| view_render::section_key(&id)));
                return self.press_card_action(*action, None, &selected, epoch);
            }
            Act::RowAction(action) => {
                let Some(home) = self.home_view.as_ref() else { return Flow::Go };
                let epoch = home.data_epoch.clone();
                let selected = self.sel.clone().unwrap_or_else(|| Key(home.key.clone()));
                return self.run_view_action(&action, None, &selected, epoch);
            }
            Act::ReaderClose => self.reader = None,
            Act::ReaderFind => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.find = Some(super::Find { input: super::TextInput::default(), editing: true });
                }
            }
            Act::ReaderFindNext | Act::ReaderFindPrev => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.jump(matches!(act, Act::ReaderFindNext), false);
                }
            }
            Act::ReaderFindClose => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.find = None;
                }
            }
            Act::ReaderComment => self.comment_block(),
            Act::ReaderGeneral => self.comment_general(),
            Act::ReaderDelete => self.delete_comment(),
            Act::ReaderSend => self.send_review(),
            Act::ReaderAnswer => self.answer_with_comments(key),
            Act::ReaderOpen => self.open_block(),
            Act::ReaderPick => self.pick_target(key),
            Act::ReaderPickClose => {
                if let Some(reader) = self.reader.as_mut() {
                    reader.picking = false;
                }
            }
            Act::ModalConfirm => self.run_modal(),
            Act::ModalCancel => self.modal = None,
            Act::Pairing(allow) => {
                if let Some(device) = self.pairing_prompt().cloned() {
                    self.answer_pairing(device, allow);
                }
            }
        }
        Flow::Go
    }
}
