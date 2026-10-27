use super::{Action, Item, Label, Lang, Link, LocalLink, Row, Tone, View};
use super::text::t;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClientCapabilities(u8);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidCapabilities;

impl ClientCapabilities {
    pub const SUPPORTED: [&'static str; 3] = ["account_presentation", "model_selection", "provider_accounts"];

    pub fn parse(value: &str) -> Result<Self, InvalidCapabilities> {
        if value.len() > 256 { return Err(InvalidCapabilities); }
        let mut result = Self::default();
        if value.is_empty() { return Ok(result); }
        for name in value.split(',') {
            if name.is_empty() || name.len() > 64 || !name.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'_') {
                return Err(InvalidCapabilities);
            }
            if let Some(at) = Self::SUPPORTED.iter().position(|known| *known == name) { result.0 |= 1 << at; }
        }
        Ok(result)
    }

    pub fn contains(self, name: &str) -> bool {
        Self::SUPPORTED.iter().position(|known| *known == name).is_some_and(|at| self.0 & (1 << at) != 0)
    }

    pub fn query(self) -> String {
        Self::SUPPORTED.iter().filter(|name| self.contains(name)).copied().collect::<Vec<_>>().join(",")
    }

    pub fn is_empty(self) -> bool { self.0 == 0 }

    pub fn supports_link(self, link: &Link) -> bool {
        match link {
            Link::View { key } if key.starts_with("task_selection:") || key.starts_with("model_profile:") => self.contains("model_selection"),
            Link::Local { local: LocalLink::AccountAdd { provider, .. } | LocalLink::AccountSignIn { provider, .. } } => {
                match provider.as_deref() {
                    None | Some("claude") => true,
                    Some("codex") => self.contains("provider_accounts"),
                    Some(_) => false,
                }
            }
            _ => true,
        }
    }

    pub fn adapt_view(self, view: &mut View, lang: Lang) {
        for section in &mut view.sections {
            for action in &mut section.actions { self.adapt_action(action, lang); }
            for item in &mut section.items { self.adapt_item(item, lang); }
        }
        for card in &mut view.first_cards { self.adapt_view(card, lang); }
        if !self.is_empty() {
            view.status.version = super::version(&(view.status.clone(), self.query()));
        }
        view.version = super::version(view);
    }

    fn adapt_action(self, action: &mut Action, lang: Lang) {
        let unsupported_account_action = super::action_id::ActionId::parse(&action.id).ok().is_some_and(|id| {
            matches!(id.verb, super::action_id::Verb::AccountOrder) && !self.contains("account_presentation")
                || matches!(id.verb, super::action_id::Verb::ModelProfile | super::action_id::Verb::TaskSelection) && !self.contains("model_selection")
                || matches!(id.verb, super::action_id::Verb::AccountEnable | super::action_id::Verb::AccountDisable | super::action_id::Verb::AccountRemove
                    | super::action_id::Verb::AccountUp | super::action_id::Verb::AccountDown | super::action_id::Verb::AccountWeight | super::action_id::Verb::AccountRename | super::action_id::Verb::AccountModels)
                    && id.extra.as_deref() == Some("codex") && !self.contains("provider_accounts")
        });
        if unsupported_account_action || action.link.as_ref().is_some_and(|link| !self.supports_link(link)) {
            action.enabled = false;
            action.link = None;
            action.disabled_reason = Some(update_reason(lang).into());
        }
    }

    fn adapt_row(self, row: &mut Row, lang: Lang) {
        if !self.contains("account_presentation") { row.account = None; }
        if !self.contains("model_selection") { row.model = None; }
        let unsupported_link = row.link.as_ref().is_some_and(|link| !self.supports_link(link));
        if unsupported_link {
            row.link = None;
            row.badges.push(Label { text: update_reason(lang).into(), tone: Tone::Warning, elapsed: None });
        }
        let needs_update = unsupported_link || row.actions.iter().any(|action| action.link.as_ref().is_some_and(|link| !self.supports_link(link)));
        for action in &mut row.actions { self.adapt_action(action, lang); }
        if needs_update {
            let reason = update_reason(lang);
            row.subtitle = Some(row.subtitle.take().map_or_else(|| reason.into(), |subtitle| format!("{subtitle}\n{reason}")));
        }
        for child in &mut row.children { self.adapt_row(child, lang); }
    }

    fn adapt_item(self, item: &mut Item, lang: Lang) {
        match item {
            Item::Row(row) => self.adapt_row(row, lang),
            Item::Actions(bar) => for action in &mut bar.actions { self.adapt_action(action, lang); },
            Item::Form(form) => {
                if !self.contains("model_selection") {
                    form.fields.retain(|field| !matches!(field.id.as_str(), "model" | "reasoning_effort" | "allow_model_substitution"));
                }
                if self.contains("model_selection") {
                    if form.fields.iter().any(|field| field.id == "model") { form.fields.retain(|field| field.id != "effort"); }
                    if let Ok(mut id) = super::action_id::ActionId::parse(&form.submit.id) {
                        id.verb = match id.verb {
                            super::action_id::Verb::NewRequest => super::action_id::Verb::NewRequestSelection,
                            super::action_id::Verb::SchedSave if form.fields.iter().any(|field| field.id == "model") => super::action_id::Verb::ScheduleSelection,
                            verb => verb,
                        };
                        form.submit.id = id.to_string();
                    }
                }
                self.adapt_action(&mut form.submit, lang);
            },
            Item::Card(card) => {
                if card.link.as_ref().is_some_and(|link| !self.supports_link(link)) { card.link = None; }
                for action in &mut card.actions { self.adapt_action(action.on_client(), lang); }
                for action in &mut card.more { self.adapt_action(action.on_client(), lang); }
                if let Some(action) = &mut card.reply { self.adapt_action(action.on_client(), lang); }
            }
            _ => {}
        }
    }
}

fn update_reason(lang: Lang) -> &'static str {
    t!(lang, "Update Alondra to use this feature", "Actualiza Alondra para usar esta función")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{DomainState, ReadSnapshot};
    use crate::views::{Client, LiveSnapshot, RenderContext, Transport};
    use std::sync::Arc;

    #[test]
    fn modern_model_forms_use_an_action_kind_a_legacy_daemon_cannot_misinterpret() {
        let domain = Arc::new(DomainState::new("epoch"));
        let snapshot = ReadSnapshot::build(domain.clone());
        let live = LiveSnapshot::default();
        let mut legacy = crate::views::build("new_request", RenderContext { domain: &domain, index: snapshot.index(), live: &live,
            client: Client::Ios, lang: Lang::En, transport: Transport::Loopback, cursor: "", rendered_at: "" }).unwrap();
        let mut modern = legacy.clone();
        ClientCapabilities::default().adapt_view(&mut legacy, Lang::En);
        ClientCapabilities::parse("model_selection").unwrap().adapt_view(&mut modern, Lang::En);
        let submit = |view: &View| view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Form(form) => Some(form.submit.id.clone()), _ => None }).unwrap();
        assert!(submit(&legacy).starts_with("new_request:"));
        assert!(submit(&modern).starts_with("new_request_selection:"));
        let fields = |view: &View| view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { Item::Form(form) => Some(form.fields.iter().map(|field| field.id.clone()).collect::<Vec<_>>()), _ => None }).unwrap();
        assert!(!fields(&legacy).iter().any(|id| id == "model"));
        assert!(fields(&modern).iter().any(|id| id == "model"));
    }

    #[test]
    fn capability_order_duplicates_and_unknown_names_do_not_change_identity() {
        let first = ClientCapabilities::parse("provider_accounts,account_presentation,provider_accounts,future_feature").unwrap();
        let second = ClientCapabilities::parse("account_presentation,provider_accounts").unwrap();
        assert_eq!(first, second);
        assert_eq!(first.query(), "account_presentation,provider_accounts");
        assert!(ClientCapabilities::parse("provider_accounts,../shell").is_err());
        assert!(ClientCapabilities::parse("provider_accounts,").is_err());
    }

    #[test]
    fn an_old_client_keeps_a_meaningful_disabled_account_action_instead_of_running_claude() {
        let read = ReadSnapshot::build(Arc::new(DomainState::new("epoch")));
        let live = LiveSnapshot::default();
        let mut view = crate::views::build("accounts", RenderContext {
            domain: read.state(), index: read.index(), live: &live, client: Client::Tui,
            lang: Lang::En, transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap();
        let row = view.sections.iter_mut().flat_map(|section| &mut section.items).find_map(|item| match item {
            Item::Row(row) if !row.actions.is_empty() => Some(row), _ => None,
        }).unwrap();
        let action = &mut row.actions[0];
        action.link = Some(Link::Local { local: LocalLink::AccountAdd {
            command: "alondra accounts add --provider codex".into(), provider: Some("codex".into()),
            argv: Some(vec!["accounts".into(), "add".into(), "--provider".into(), "codex".into()]),
        } });
        let label = action.label.clone();
        let mut modern = view.clone();
        ClientCapabilities::parse("provider_accounts").unwrap().adapt_view(&mut modern, Lang::En);
        ClientCapabilities::default().adapt_view(&mut view, Lang::En);
        let action = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
            Item::Row(row) => row.actions.iter().find(|action| action.label == label), _ => None,
        }).unwrap();
        assert!(!action.enabled);
        assert!(action.link.is_none());
        assert!(action.disabled_reason.as_deref().unwrap().contains("Update Alondra"));
        assert_ne!(view.version, modern.version);
    }

    #[test]
    fn legacy_clients_keep_typed_claude_links_and_block_codex_links() {
        let capabilities = ClientCapabilities::default();
        let link = |provider: &str| Link::Local { local: LocalLink::AccountSignIn {
            provider: Some(provider.into()), argv: Some(vec!["accounts".into(), "login".into(), "--provider".into(), provider.into(), "work".into()]),
            account: "Work".into(), command: "alondra accounts login 'Work'".into(),
        } };
        assert!(capabilities.supports_link(&link("claude")));
        assert!(!capabilities.supports_link(&link("codex")));
        assert!(!capabilities.supports_link(&link("future")));
    }

    #[test]
    fn a_legacy_account_link_remains_usable_without_negotiation() {
        let legacy = Link::Local { local: LocalLink::AccountSignIn {
            account: "Personal".into(), command: "alondra accounts login Personal".into(), provider: None, argv: None,
        } };
        assert!(ClientCapabilities::default().supports_link(&legacy));
        let unknown = Link::Local { local: LocalLink::AccountSignIn {
            account: "Other".into(), command: "untrusted".into(), provider: Some("future".into()), argv: None,
        } };
        assert!(!ClientCapabilities::parse("provider_accounts").unwrap().supports_link(&unknown));
    }
}
