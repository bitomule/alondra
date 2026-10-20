//! The inbox: the daemon's items, each key mapped to the one semantic command
//! the item offers. Reading is not replying, and authorizing a merge is not a merge.

use std::collections::BTreeMap;

use alondra_core::domain::PrId;
use alondra_core::now_millis;
use crossterm::event::{KeyCode, KeyEvent};

use super::keys::{self, Cmd, Motion};
use super::{App, Flow, Key, Msg, Pane, Part, Screen, Target, md, unavailable};
use crate::ops::{self, MessageKind};

pub(super) fn review_clone_flow(
    selected: &Key,
    pr_id: &str,
    number: u64,
    dir: std::path::PathBuf,
) -> Option<Flow> {
    alondra_core::views::id::valid(pr_id).then(|| Flow::Review {
        key: selected.clone(),
        pr: PrId(pr_id.to_string()),
        number,
        dir,
    })
}

impl App {
    pub fn inbox_keys(&self) -> Vec<Key> {
        if let Some(view) = self.shown_home() {
            return super::view_render::entries(&view, &self.open_sections, &self.open_rows)
                .into_iter()
                .map(|(id, _, _)| Key(id))
                .collect();
        }
        Vec::new()
    }

    pub fn inbox_count(&self) -> usize {
        if let Some(view) = self.shown_home() {
            return super::view_render::entries(&view, &self.open_sections, &self.open_rows)
                .into_iter()
                .filter(|(id, nested, _)| !nested && super::view_render::header_section(id).is_none())
                .count();
        }
        0
    }

    /// Items waiting for the user, as the daemon counts them, else as the inbox shows them.
    /// The server's count of what waits for the user, less the cards answered here that it still lists.
    pub fn attention(&self) -> usize {
        let answered = self.home_view.iter().flat_map(|home| &home.sections).flat_map(|section| &section.items)
            .filter(|item| matches!(item, alondra_core::views::Item::Card(card) if self.optimistic.hides(&Key(card.id.clone()))))
            .count();
        self.view_status
            .as_ref()
            .and_then(|s| s.counts.iter().find(|c| c.id == "inbox"))
            .map_or_else(|| self.inbox_count(), |c| (c.value as usize).saturating_sub(answered))
    }

    pub(super) fn move_inbox(&mut self, to: isize) {
        let keys = self.inbox_keys();
        if keys.is_empty() {
            return;
        }
        let idx = to.clamp(0, keys.len() as isize - 1) as usize;
        if self.sel.as_ref() != Some(&keys[idx]) {
            self.detail_scroll = 0;
        }
        self.sel_idx = idx;
        self.card_menu = None;
        self.sel = Some(keys[idx].clone());
        self.settle_until = 0;
    }

    /// Enter or `o` on a folded section's header opens it, and on an open one folds it again.
    pub(super) fn toggle_selected_section(&mut self) -> bool {
        let Some(id) = self
            .sel
            .as_ref()
            .and_then(|key| super::view_render::header_section(&key.0))
            .map(str::to_string)
        else {
            return false;
        };
        if !self.open_sections.remove(&id) {
            if !self.section_pages.contains_key(&id) {
                self.load_section(&id);
            }
            self.open_sections.insert(id);
        }
        true
    }

    /// Enter or `o` on a row the daemon sends folded shows the rows under it, and hides them again.
    pub(super) fn toggle_selected_fold(&mut self) -> bool {
        let Some(id) = self.sel.as_ref().map(|key| key.0.clone()) else {
            return false;
        };
        let folds = self.shown_home().is_some_and(|view| {
            super::view_render::rows(&view).iter().any(|(row, _)| row.id == id && row.folded)
        });
        if folds && !self.open_rows.remove(&id) {
            self.open_rows.insert(id);
        }
        folds
    }

    pub(super) fn inbox_cmd(&mut self, cmd: Cmd) {
        let i = self.sel_idx as isize;
        let detail = self.focus == Pane::Detail && self.detail_view.shown;
        match cmd {
            Cmd::Move(m) if detail => {
                self.detail_scroll = keys::scroll(
                    self.detail_scroll,
                    m,
                    self.detail_view.h,
                    self.detail_view.total,
                )
            }
            Cmd::Move(Motion::Down) => self.move_inbox(i + 1),
            Cmd::Move(Motion::Up) => self.move_inbox(i - 1),
            Cmd::Move(Motion::Top) => self.move_inbox(0),
            Cmd::Move(Motion::Bottom) => self.move_inbox(isize::MAX),
            Cmd::Move(m) => {
                self.detail_scroll = keys::scroll(
                    self.detail_scroll,
                    m,
                    self.detail_view.h,
                    self.detail_view.total,
                )
            }
            Cmd::Right if self.detail_view.shown && self.sel.is_some() => self.focus = Pane::Detail,
            Cmd::Right | Cmd::Expand => self.open_reader(),
            Cmd::Left | Cmd::Back => self.focus = Pane::List,
            Cmd::Pending | Cmd::Other(_) => {}
        }
    }



    pub(super) fn view_action_key(&mut self, key: KeyEvent) -> Option<Flow> {
        self.document_action_key(key)
            .or_else(|| self.screen_action_key(key))
    }

    fn screen_action_key(&mut self, key: KeyEvent) -> Option<Flow> {
        let home = self.home_view.as_ref()?;
        let action = super::draw::screen_actions(home)
            .into_iter()
            .find(|action| hint_matches(super::view_render::action_hint(action), key))?;
        let epoch = home.data_epoch.clone();
        let selected = Key(home.key.clone());
        Some(self.press_card_action(action, None, &selected, epoch))
    }

    fn document_action_key(&mut self, key: KeyEvent) -> Option<Flow> {
        let selected = self.selected_view_key()?;
        let document = self
            .card_view
            .as_ref()
            .filter(|view| view.key == selected)?;
        if let Some(card) = super::view_render::typed_card(document)
            && (self.reader.is_none() || card_button_index(key).is_some())
        {
            let (card, epoch) = (card.clone(), document.data_epoch.clone());
            return self.card_button_key(key, &card, epoch);
        }
        if let Some(form) = document
            .sections
            .iter()
            .flat_map(|section| &section.items)
            .find_map(|item| {
                if let alondra_core::views::Item::Form(form) = item {
                    Some(form)
                } else {
                    None
                }
            })
        {
            let submit = &form.submit;
            let hint = super::view_render::action_hint(submit);
            if key.code == KeyCode::Enter
                || key.code == KeyCode::Char('e')
                || (!hint.is_empty() && hint_matches(hint, key))
            {
                let view = document.key.clone();
                self.compose(super::Target::Form { view }, "");
                return Some(Flow::Go);
            }
        }
        let (action, option) = view_key_choice(document, key)?;
        let epoch = document.data_epoch.clone();
        let local_key = self.sel.clone().unwrap_or_else(|| Key(selected.clone()));
        Some(self.press_card_action(action, option, &local_key, epoch))
    }

    /// A card's keys never change: `1`-`3` press its buttons, `m` lists the rest, `r` replies.
    fn card_button_key(
        &mut self,
        key: KeyEvent,
        card: &alondra_core::views::Card,
        epoch: String,
    ) -> Option<Flow> {
        if !(key.modifiers - crossterm::event::KeyModifiers::SHIFT).is_empty() {
            return None;
        }
        let KeyCode::Char(character) = key.code else {
            return None;
        };
        let selected = self.sel.clone().unwrap_or_else(|| Key(card.id.clone()));
        if let Some(index) = card_button_index(key) {
            let action = alondra_core::views::Action::from(card.actions.get(index)?.clone());
            return Some(self.press_card_action(action, None, &selected, epoch));
        }
        match character {
            'm' if !card.more.is_empty() => {
                self.card_menu = Some(super::view_render::more_menu(card));
                Some(Flow::Go)
            }
            'r' => {
                let reply = card.reply.clone()?;
                Some(self.run_view_action(&reply, None, &selected, epoch))
            }
            _ => None,
        }
    }

    /// An action that offers options asks which one first; the rest run at once.
    pub(super) fn press_card_action(
        &mut self,
        action: alondra_core::views::Action,
        option: Option<alondra_core::views::ActionOption>,
        selected: &Key,
        epoch: String,
    ) -> Flow {
        let option = option.or_else(|| only_option(&action));
        if action.enabled && option.is_none() && action.option_id.is_none() && !action.options.is_empty() {
            self.card_menu = Some(super::view_render::option_menu(&action));
            return Flow::Go;
        }
        self.run_view_action(&action, option.as_ref(), selected, epoch)
    }

    pub(super) fn card_menu_key(&mut self, key: KeyEvent) -> Flow {
        let Some(menu) = self.card_menu.clone() else {
            return Flow::Go;
        };
        match key.code {
            KeyCode::Esc => self.card_menu = None,
            KeyCode::Char(character @ '1'..='9') if key.modifiers.is_empty() => {
                let Some((action, option)) = menu.entries.get(character as usize - '1' as usize) else {
                    return Flow::Go;
                };
                let (action, option) = (action.clone(), option.clone());
                self.card_menu = None;
                let Some(view) = self.card_view.as_ref().or(self.home_view.as_ref()) else {
                    return Flow::Go;
                };
                let epoch = view.data_epoch.clone();
                let selected = self.sel.clone().unwrap_or_else(|| Key(view.key.clone()));
                return self.press_card_action(action, option, &selected, epoch);
            }
            _ => {}
        }
        Flow::Go
    }


    pub(super) fn run_view_action(
        &mut self,
        action: &alondra_core::views::Action,
        option: Option<&alondra_core::views::ActionOption>,
        selected: &Key,
        epoch: String,
    ) -> Flow {
        if action.id.is_empty() || !action.enabled {
            return self.run_view_action_confirmed(action, option, selected, epoch);
        }
        if let Some(confirm) = &action.confirm {
            self.ask(super::Modal {
                title: confirm.title.clone(),
                body: confirm.message.clone(),
                button: confirm.button.clone(),
                destructive: action.style == alondra_core::views::Style::Destructive,
                then: super::ModalThen::Action {
                    action: Box::new(action.clone()),
                    option_id: option.map(|option| option.id.clone()),
                    selected: selected.clone(),
                    epoch,
                },
            });
            return Flow::Go;
        }
        self.run_view_action_confirmed(action, option, selected, epoch)
    }

    pub(super) fn run_view_action_confirmed(
        &mut self,
        action: &alondra_core::views::Action,
        option: Option<&alondra_core::views::ActionOption>,
        selected: &Key,
        epoch: String,
    ) -> Flow {
        if action.id.is_empty()
            && let Some(link) = action.link.as_ref()
        {
            match link {
                alondra_core::views::Link::Url { url } => {
                    if safe_github_url(url) {
                        super::open_with_mac(url);
                    } else {
                        self.fail(tr!(
                            "Only HTTPS GitHub links can be opened from this view",
                            "Solo se pueden abrir enlaces HTTPS de GitHub desde esta vista"
                        ));
                    }
                }
                alondra_core::views::Link::View { key } => self.navigate_view(key),
                alondra_core::views::Link::Local { local } => match local {
                    alondra_core::views::LocalLink::Attach { .. } => {
                        self.fail(tr!(
                            "Alondra no longer opens a node's terminal: its detail has its state and its cards",
                            "Alondra ya no abre el terminal de un nodo: su detalle tiene su estado y sus tarjetas"
                        ));
                    }
                    alondra_core::views::LocalLink::OpenPath { path } => {
                        self.flash(trf!(
                            "Local file reference: {path}",
                            "Referencia de archivo local: {path}"
                        ));
                    }
                    alondra_core::views::LocalLink::ReviewClone {
                        repo, pr, pr_id, ..
                    } => {
                        if !alondra_core::views::id::valid(pr_id) {
                            self.fail(tr!(
                                "Invalid PR review target",
                                "Destino de revisión de PR no válido"
                            ));
                            return Flow::Go;
                        }
                        if let Some(dir) = super::clone_of(None, repo) {
                            if let Some(flow) = review_clone_flow(selected, pr_id, *pr, dir) {
                                return flow;
                            }
                        }
                        let key = format!("pull_request:{pr_id}");
                        self.navigate_view(&key);
                        self.reader_after_view = Some(key);
                        self.flash(tr!(
                            "Opening the PR diff…",
                            "Abriendo las diferencias de la PR…"
                        ));
                    }
                },
            }
            return Flow::Go;
        }
        if !action.enabled {
            self.fail(action.disabled_reason.clone().unwrap_or_else(|| {
                tr!(
                    "This action is unavailable",
                    "Esta acción no está disponible"
                )
                .into()
            }));
            return Flow::Go;
        }
        let option_id = option.map(|option| option.id.clone()).or_else(|| action.option_id.clone());
        let input = match option {
            Some(option)
                if matches!(option.input, alondra_core::views::Input::None)
                    && matches!(action.input, alondra_core::views::Input::Text { required: false, .. }) =>
            {
                &action.input
            }
            Some(option) => &option.input,
            None => &action.input,
        };
        let acting = super::Acting {
            origin: selected.clone(),
            stamp: action.stamp.clone(),
            hides: action.resolves_card,
            label: option.map_or_else(|| action.label.clone(), |option| option.label.clone()),
        };
        if option.is_none() && action.option_id.is_some() {
            let (text, chips, review) = self.take_card_words();
            let mut input = serde_json::json!({});
            if !text.trim().is_empty() {
                input["text"] = serde_json::Value::String(text.trim().to_owned());
            }
            let attachments: Vec<String> = super::dropped::stored(&chips).into_iter().map(|attachment| attachment.id).collect();
            if !attachments.is_empty() {
                input["attachments"] = serde_json::json!(attachments);
            }
            let request = crate::client::Client::action_body(&epoch, &action.id, option_id.as_deref(), input);
            if let (Some(key), Some(command)) = (review, request["command_id"].as_str()) {
                self.review_sends.insert(command.to_owned(), key);
            }
            self.submit_view_request(request, Some(acting));
            return Flow::Go;
        }
        match input {
            alondra_core::views::Input::Text { required, .. } => {
                self.compose(
                    super::Target::ViewAction {
                        action_id: action.id.clone(),
                        label: action.label.clone(),
                        data_epoch: epoch,
                        option_id,
                        required: *required,
                        allow_attachments: action.attachments,
                        acting: Some(acting.clone()),
                        review: None,
                    },
                    "",
                );
            }
            alondra_core::views::Input::None => {
                let request = crate::client::Client::action_body(&epoch, &action.id, option_id.as_deref(), serde_json::json!({}));
                self.submit_view_request(request, Some(acting));
            }
        }
        Flow::Go
    }

    /// What the user wrote on the open card, which goes with the option they press: the comments
    /// left in the reader, or else the draft of the card's own field. Taken, so it is sent once.
    fn take_card_words(&mut self) -> (String, Vec<super::Chip>, Option<Key>) {
        let Some(view) = self.card_view.as_ref() else { return (String::new(), vec![], None) };
        if let Some(key) = self.reader.as_ref().map(|reader| reader.key.clone()).filter(|key| key.0 == view.key)
            && let Some(review) = self.reviews.get(&key).filter(|review| !review.is_empty())
        {
            return (review.feedback(), review.chips.clone(), Some(key));
        }
        let draft = super::view_render::card_reply(view).and_then(|reply| {
            self.drafts.remove(&format!("view-action:{}:", reply.id))
        });
        draft.map_or((String::new(), vec![], None), |(text, chips)| (text, chips, None))
    }
}

/// Whether what the user wrote can go with this key: every option carries it, and so does an
/// action that takes text.
pub(super) fn carries_words(action: &alondra_core::views::Action, option: Option<&alondra_core::views::ActionOption>) -> bool {
    let text = |input: &alondra_core::views::Input| matches!(input, alondra_core::views::Input::Text { .. });
    action.option_id.is_some() || text(&action.input) || option.is_some_and(|option| text(&option.input))
}

pub(super) fn safe_github_url(value: &str) -> bool {
    let Ok(url) = url::Url::parse(value) else {
        return false;
    };
    let Some(host) = url.host_str() else {
        return false;
    };
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && (host == "github.com" || host.ends_with(".github.com"))
}

/// An action with a single option needs no menu: pressing it picks that option.
pub(super) fn only_option(action: &alondra_core::views::Action) -> Option<alondra_core::views::ActionOption> {
    match action.options.as_slice() {
        [option] if action.option_id.is_none() => Some(option.clone()),
        _ => None,
    }
}

fn card_button_index(key: KeyEvent) -> Option<usize> {
    match key.code {
        KeyCode::Char(character @ '1'..='3') if (key.modifiers - crossterm::event::KeyModifiers::SHIFT).is_empty() => {
            Some(character as usize - '1' as usize)
        }
        _ => None,
    }
}

/// The action, and the option when the key names one, that a key presses on a view's detail.
pub(super) fn view_key_choice(
    document: &alondra_core::views::View,
    key: KeyEvent,
) -> Option<(alondra_core::views::Action, Option<alondra_core::views::ActionOption>)> {
    if let Some(card) = super::view_render::typed_card(document)
        && let Some(index) = card_button_index(key)
    {
        let action = alondra_core::views::Action::from(card.actions.get(index)?.clone());
        let option = only_option(&action);
        return Some((action, option));
    }
    for action in super::view_render::actions(document) {
        if hint_matches(super::view_render::action_hint(&action), key) {
            let option = only_option(&action);
            return Some((action, option));
        }
        let option = action.options.iter().enumerate().find(|(index, option)| {
            option.key.as_deref().map_or_else(
                || {
                    key.code == KeyCode::Char(char::from_digit((index + 1) as u32, 10).unwrap_or('\0'))
                        && key.modifiers.is_empty()
                },
                |hint| hint_matches(hint, key),
            )
        });
        if let Some((_, option)) = option {
            let option = option.clone();
            return Some((action, Some(option)));
        }
    }
    None
}

fn hint_matches(hint: &str, key: KeyEvent) -> bool {
    if hint.chars().count() == 1 {
        return key.code == KeyCode::Char(hint.chars().next().unwrap_or('\0'))
            && (key.modifiers - crossterm::event::KeyModifiers::SHIFT).is_empty();
    }
    match hint.to_ascii_lowercase().as_str() {
        "enter" | "return" => key.code == KeyCode::Enter,
        "space" => key.code == KeyCode::Char(' '),
        "esc" | "escape" => key.code == KeyCode::Esc,
        _ => false,
    }
}
