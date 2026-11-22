use crate::{Client, ClientCapabilities, Item, Lang, Link, LiveSnapshot, LocalLink, RenderContext, Transport, View};
use alondra_domain_rules::{DomainState, ReadSnapshot};
use std::sync::Arc;

#[test]
fn modern_model_forms_use_an_action_kind_a_legacy_daemon_cannot_misinterpret() {
    let domain = Arc::new(DomainState::new("epoch"));
    let snapshot = ReadSnapshot::build(domain.clone());
    let live = LiveSnapshot::default();
    let mut legacy = crate::build("new_request", RenderContext { domain: &domain, index: snapshot.index(), live: &live,
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
fn an_old_client_keeps_a_meaningful_disabled_account_action_instead_of_running_claude() {
    let read = ReadSnapshot::build(Arc::new(DomainState::new("epoch")));
    let live = LiveSnapshot::default();
    let mut view = crate::build("accounts", RenderContext {
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
fn an_old_client_is_not_offered_the_account_actions_of_a_provider_it_does_not_know() {
    let read = ReadSnapshot::build(Arc::new(DomainState::new("epoch")));
    let live = LiveSnapshot::default();
    let mut view = crate::build("accounts", RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: Client::Tui,
        lang: Lang::En, transport: Transport::Loopback, cursor: "", rendered_at: "",
    }).unwrap();
    let action = view.sections.iter_mut().flat_map(|section| &mut section.items).find_map(|item| match item {
        Item::Row(row) if !row.actions.is_empty() => Some(&mut row.actions[0]), _ => None,
    }).unwrap();
    let label = action.label.clone();
    action.enabled = true;
    action.id = crate::action_id::ActionId::new(crate::action_id::Verb::AccountEnable, "work", 1, Some("antigravity".into())).unwrap().to_string();
    let mut modern = view.clone();
    ClientCapabilities::parse("provider_accounts").unwrap().adapt_view(&mut modern, Lang::En);
    ClientCapabilities::default().adapt_view(&mut view, Lang::En);
    let find = |view: &View| view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item {
        Item::Row(row) => row.actions.iter().find(|action| action.label == label).cloned(), _ => None,
    }).unwrap();
    assert!(!find(&view).enabled);
    assert!(find(&view).disabled_reason.as_deref().unwrap().contains("Update Alondra"));
    assert!(find(&modern).enabled);
}
