//! The inbox: the daemon's roots and their children, each key mapped to the one semantic command
//! the item offers. Reading is not replying, and authorizing a merge is not a merge.

use std::collections::BTreeMap;

use alondra_core::domain_v2::PrId;
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
        if let Some(view) = &self.home_view {
            return super::view_render::rows(view)
                .into_iter()
                .map(|(row, _)| Key(row.id.clone()))
                .collect();
        }
        Vec::new()
    }

    pub fn inbox_count(&self) -> usize {
        if let Some(view) = &self.home_view {
            return super::view_render::rows(view)
                .into_iter()
                .filter(|(_, nested)| !nested)
                .count();
        }
        0
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
        self.sel = Some(keys[idx].clone());
        self.settle_until = 0;
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

    pub(super) fn inbox_key(&mut self, k: KeyEvent) -> Flow {
        self.view_inbox_key(k)
    }

    fn view_inbox_key(&mut self, key: KeyEvent) -> Flow {
        if key.code == KeyCode::Enter {
            if self.screen != Screen::Inbox && !self.schedules_open {
                return self.open_terminal();
            }
            self.open_reader();
            return Flow::Go;
        }
        if key.code == KeyCode::Char('d')
            && self.card_view.as_ref().is_some_and(|view| view.sections.iter()
                .flat_map(|section| &section.items)
                .any(|item| matches!(item, alondra_core::views::Item::ContentRef(content)
                    if content.kind == alondra_core::views::ContentKind::Diff)))
        {
            self.open_reader();
            return Flow::Go;
        }
        if key.code == KeyCode::Char('J') {
            self.detail_scroll = keys::by(
                self.detail_scroll,
                5,
                self.detail_view.h,
                self.detail_view.total,
            );
            return Flow::Go;
        }
        if key.code == KeyCode::Char('K') {
            self.detail_scroll = keys::by(
                self.detail_scroll,
                -5,
                self.detail_view.h,
                self.detail_view.total,
            );
            return Flow::Go;
        }

        self.view_action_key(key).unwrap_or(Flow::Go)
    }

    /// Enter on a task or a node opens its session's terminal.
    fn open_terminal(&mut self) -> Flow {
        let selected = self.selected_view_key();
        let session = self
            .card_view
            .as_ref()
            .filter(|view| selected.as_deref() == Some(view.key.as_str()))
            .and_then(super::view_render::attach_session);
        match session {
            Some(session) => Flow::Attach(session),
            None => {
                self.flash(tr!(
                    "It has no terminal to open",
                    "No tiene terminal que abrir"
                ));
                Flow::Go
            }
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
        Some(self.run_view_action(&action, None, &selected, epoch))
    }

    fn document_action_key(&mut self, key: KeyEvent) -> Option<Flow> {
        let selected = self.selected_view_key()?;
        let document = self
            .card_view
            .as_ref()
            .filter(|view| view.key == selected)?;
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
        for action in super::view_render::actions(document) {
            if hint_matches(super::view_render::action_hint(&action), key) {
                let local_key = self.sel.clone().unwrap_or_else(|| Key(selected.clone()));
                return Some(self.run_view_action(
                    &action,
                    None,
                    &local_key,
                    document.data_epoch.clone(),
                ));
            }
            for (index, option) in action.options.iter().enumerate() {
                let matches = option.key.as_deref().map_or_else(
                    || {
                        key.code
                            == KeyCode::Char(
                                char::from_digit((index + 1) as u32, 10).unwrap_or('\0'),
                            )
                            && key.modifiers.is_empty()
                    },
                    |hint| hint_matches(hint, key),
                );
                if matches {
                    let local_key = self.sel.clone().unwrap_or_else(|| Key(selected.clone()));
                    return Some(self.run_view_action(
                        &action,
                        Some(option),
                        &local_key,
                        document.data_epoch.clone(),
                    ));
                }
            }
        }
        None
    }

    pub(super) fn schedule_row_key(&mut self, key: KeyEvent) -> Option<Flow> {
        let document = self
            .home_view
            .as_ref()
            .filter(|view| view.key == "schedules")?;
        let selected = self.sel.as_ref()?;
        let row = super::view_render::rows(document)
            .into_iter()
            .find_map(|(row, _)| (row.id == selected.0).then_some(row))?;
        let action = row
            .actions
            .iter()
            .find(|action| {
                action
                    .key
                    .as_deref()
                    .is_some_and(|hint| hint_matches(hint, key))
            })?
            .clone();
        let epoch = document.data_epoch.clone();
        let selected = selected.clone();
        Some(self.run_view_action(&action, None, &selected, epoch))
    }

    fn run_view_action(
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
                    alondra_core::views::LocalLink::Attach { session } => {
                        return Flow::Attach(session.clone());
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
        let option_id = option.map(|option| option.id.clone());
        if let Some(confirm) = &action.confirm {
            let step = format!("{}:{}", action.id, option_id.as_deref().unwrap_or(""));
            if !self.confirmed(&step, confirm.message.clone()) {
                return Flow::Go;
            }
        }
        let input = option.map_or(&action.input, |option| &option.input);
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
                    },
                    "",
                );
            }
            alondra_core::views::Input::None => {
                let action_id = action.id.clone();
                let data_epoch = epoch;
                let option_id = option_id.clone();
                let request = crate::client::Client::action_body(
                    &data_epoch,
                    &action_id,
                    option_id.as_deref(),
                    serde_json::json!({}),
                );
                self.submit_view_request(request);
            }
        }
        Flow::Go
    }
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
