use super::*;

fn app() -> App {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    App::new(tx)
}

#[test]
fn codex_login_verification_button_opens_only_the_provider_page() {
    let mut app = app();
    let mut action: alondra_core::views::Action = serde_json::from_value(json!({
        "id":"", "label":"Open verification page", "style":"primary", "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
        "link":{"kind":"url","url":"https://auth.openai.com/codex/device"}
    })).unwrap();
    OPENED.with(|opened| opened.borrow_mut().clear());
    app.run_view_action_confirmed(&action, None, &Key("accounts".into()), "epoch".into());
    OPENED.with(|opened| assert_eq!(&*opened.borrow(), &["https://auth.openai.com/codex/device"]));
    action.link = Some(alondra_core::views::Link::Url { url: "https://auth.openai.com.evil.test/codex/device".into() });
    app.run_view_action_confirmed(&action, None, &Key("accounts".into()), "epoch".into());
    OPENED.with(|opened| assert_eq!(opened.borrow().len(), 1));
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn tasks_search_types_and_pastes_into_the_views_key() {
    let mut app = app();
    app.screen = Screen::Tasks;
    press(&mut app, KeyCode::Char('/'));
    assert!(app.search.as_ref().is_some_and(|search| search.editing));
    press(&mut app, KeyCode::Char('N'));
    app.on_paste("ode One");
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("search?q=node+one")
    );
    assert!(matches!(
        app.search.as_ref().map(|search| &search.remote),
        Some(Remote::Pending)
    ));
}

#[test]
fn terminal_presence_is_recorded_once_per_minute() {
    let mut app = app();
    app.activity_told = 0;
    app.note_activity();
    app.note_activity();
    assert_eq!(
        app.posted
            .borrow()
            .iter()
            .filter(|(path, _)| path == crate::ops::PRESENCE)
            .count(),
        1
    );
}

#[test]
fn views_form_validates_choices_and_visible_required_fields() {
    let fields: Vec<alondra_core::views::Field> = serde_json::from_value(json!([
        {"id":"project","label":"Project","kind":{"kind":"choice","items":[{"id":"App","label":"App"}]},
         "value":"App","required":true,"reload_on_change":true},
        {"id":"repeat","label":"Repeat","kind":{"kind":"choice","items":[{"id":"daily","label":"Daily"},{"id":"weekly","label":"Weekly"}]},
         "value":"daily","required":true,"reload_on_change":false},
        {"id":"weekday","label":"Weekday","kind":{"kind":"stepper","min":0,"max":6},
         "value":null,"required":true,"visible_when":{"field":"repeat","equals":"weekly"},"reload_on_change":false}
    ])).unwrap();
    assert!(validate_view_fields(&fields, &json!({"project":"App","repeat":"daily"})).is_ok());
    assert!(validate_view_fields(&fields, &json!({"project":"Other","repeat":"daily"})).is_err());
    assert!(validate_view_fields(&fields, &json!({"project":"App","repeat":"weekly"})).is_err());

    let interval_fields: Vec<alondra_core::views::Field> = serde_json::from_value(json!([{
        "id":"interval","label":"Interval","kind":{"kind":"stepper","min":1,"max":365},
        "value":1,"required":true,
        "visible_when":{"field":"repeat","any_of":["every_hours","every_days","every_months"]},
        "reload_on_change":false
    }])).unwrap();
    assert!(validate_view_fields(&interval_fields, &json!({"repeat":"every_hours","interval":2})).is_ok());
    assert!(validate_view_fields(&interval_fields, &json!({"repeat":"every_months"})).is_err());
    assert!(validate_view_fields(&interval_fields, &json!({"repeat":"daily"})).is_ok());
}

#[test]
fn failed_views_feed_marks_cached_status_offline_until_recovery() {
    let mut app = app();
    app.view_status = Some(
        serde_json::from_value(json!({
            "version":"1", "summary":"cached", "badge":0, "counts":[], "gauges":[],
            "orchestrator":{"text":"Idle","tone":"neutral"}, "update":null
        }))
        .unwrap(),
    );
    app.on_msg(Msg::ViewFeed(Err("offline".into())));
    assert!(app.offline);
    assert!(app.view_status.is_some());
    let status = app.view_status.clone().unwrap();
    app.on_msg(Msg::ViewFeed(Ok(crate::client::ViewsFeed {
        protocol: alondra_core::views::VIEWS_PROTOCOL,
        cursor: "1".into(),
        data_epoch: "epoch".into(),
        status,
        stale: vec![],
        reset: false,
        superseded: false,
        service: None,
    })));
    assert!(!app.offline);
}

fn direct_mac(id: &str) -> MacTarget {
    let mac: crate::direct_client::DirectMac = serde_json::from_value(json!({
        "mac_id": id, "name": id, "mac_signing_key": "k", "mac_agreement_key": "k",
        "device_id": "d", "signing_key": "k", "agreement_key": "k", "routes": ["192.168.1.20:47801"]
    }))
    .unwrap();
    MacTarget::Direct(std::sync::Arc::new(crate::direct_client::Connection::new(mac)))
}

#[test]
fn each_mac_instance_sends_its_requests_to_its_own_mac() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut here = App::with_target(tx.clone(), MacTarget::Local);
    let mut studio = App::with_target(tx.clone(), direct_mac("mac-studio"));
    let mut mini = App::with_target(tx, direct_mac("mac-mini"));
    for app in [&mut here, &mut studio, &mut mini] {
        app.load_screen_view();
        app.fetch("/content/report.md");
    }
    assert_eq!(*here.spawned.borrow(), vec![None, None]);
    assert_eq!(*studio.spawned.borrow(), vec![Some("mac-studio".to_owned()); 2]);
    assert_eq!(*mini.spawned.borrow(), vec![Some("mac-mini".to_owned()); 2]);
    assert_eq!(here.remote_mac(), None);
    assert_eq!(studio.remote_mac().as_deref(), Some("mac-studio"));
}

fn waiting(id: &str) -> PendingDevice {
    PendingDevice {
        id: id.into(),
        name: format!("Device {id}"),
        code: "482913".into(),
        expires_at_ms: now_millis() + 60_000,
    }
}

#[test]
fn p_allows_and_d_denies_the_device_the_prompt_shows() {
    let mut app = app();
    app.on_msg(Msg::PendingDevices(vec![waiting("p-1"), waiting("p-2")]));
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.pairing_prompt().map(|d| d.id.as_str()), Some("p-2"));
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.pairing_prompt(), None);
    assert_eq!(
        *app.posted.borrow(),
        vec![
            ("/devices/pending/p-1/approve".to_owned(), json!({})),
            ("/devices/pending/p-2/deny".to_owned(), json!({})),
        ]
    );

    app.on_msg(Msg::PendingDevices(vec![waiting("p-1"), waiting("p-2")]));
    assert_eq!(app.pairing_prompt(), None, "an answered device stays answered");

    app.on_msg(Msg::PendingDevices(vec![waiting("p-3")]));
    app.composer = Some(Composer {
        target: Target::Form { view: "new_request".into() },
        input: TextInput::default(),
        chips: vec![],
        form: None,
    });
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.posted.borrow().len(), 2, "p while typing is a letter");
    assert_eq!(app.pairing_prompt().map(|d| d.id.as_str()), Some("p-3"));
}

#[test]
fn a_failed_answer_brings_the_prompt_back() {
    let mut app = app();
    app.on_msg(Msg::PendingDevices(vec![waiting("p-1")]));
    press(&mut app, KeyCode::Char('p'));
    assert_eq!(app.pairing_prompt(), None);
    app.on_msg(Msg::PairingAnswered {
        device: waiting("p-1"),
        allow: true,
        result: Err("refused".into()),
    });
    assert_eq!(app.pairing_prompt().map(|d| d.id.as_str()), Some("p-1"));
    assert!(app.flash.as_ref().is_some_and(|f| f.kind == FlashKind::Error));
}

#[test]
fn the_switch_key_asks_for_the_next_mac_unless_typing() {
    let mut app = app();
    let key = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE);
    assert!(matches!(app.on_key(key), Flow::NextMac));
    app.cmdline = Some(TextInput::default());
    assert!(!matches!(app.on_key(key), Flow::NextMac));
}

#[test]
fn an_item_kind_from_a_newer_daemon_shows_as_a_row_asking_to_update() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut json = super::parity::view_fixture("home-tui.json");
    let section = json["sections"].as_array_mut().unwrap().iter_mut().find(|s| !s["items"].as_array().unwrap().is_empty()).unwrap();
    section["items"].as_array_mut().unwrap().push(serde_json::json!({"type": "gauge_wall"}));
    let mut view: alondra_core::views::View = serde_json::from_value(json).unwrap();

    view_render::placeholders(&mut view);

    let rows: Vec<_> = view.sections.iter().flat_map(|s| &s.items).filter_map(|item| match item {
        alondra_core::views::Item::Row(row) if row.id.starts_with("unknown-") => Some(row.title.as_str()),
        _ => None,
    }).collect();
    assert_eq!(rows, ["Update the TUI to see this"]);
}

#[test]
fn a_card_from_a_newer_daemon_loads_with_what_the_tui_cannot_use_disabled_or_left_out() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(super::parity::view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    let card: ViewDocument = serde_json::from_str(&super::parity::newer_daemon_fixture("card.json")).unwrap();
    let key = card.key.clone();
    app.sel = Some(Key(key.trim_start_matches("card:").to_owned()));
    app.on_msg(Msg::ViewCard { key: key.clone(), result: Ok(card) });

    let view = app.card_view.clone().expect("the card loads");
    let actions = view_render::actions(&view);
    let action = |label: &str| actions.iter().find(|action| action.label == label).unwrap_or_else(|| panic!("{label}: {actions:?}"));
    for label in ["Sign in with a passkey", "Pick a time"] {
        assert!(!action(label).enabled, "{label}");
        assert_eq!(action(label).disabled_reason.as_deref(), Some("Update the TUI to use this"));
        assert_eq!(action(label).link, None);
    }
    assert!(action("Go on without the calendar").enabled);
    let items: Vec<_> = view.sections.iter().flat_map(|section| &section.items).collect();
    let alondra_core::views::Item::Card(card) = items[0] else { panic!("{items:?}") };
    assert_eq!(card.kind, None);
    assert!(card.context.iter().all(|line| line.link.is_none()));
    let Some(alondra_core::views::Item::Facts(facts)) = items.iter().find(|item| matches!(item, alondra_core::views::Item::Facts(_))) else { panic!() };
    assert!(facts.items[0].link.is_some());
    assert_eq!(facts.items[1].link, None);
    let Some(alondra_core::views::Item::Form(form)) = items.iter().find(|item| matches!(item, alondra_core::views::Item::Form(_))) else { panic!() };
    assert_eq!(form.fields.iter().map(|field| field.id.as_str()).collect::<Vec<_>>(), ["note"]);
    assert!(!form.submit.enabled);
    assert_eq!(form.submit.disabled_reason.as_deref(), Some("Update the TUI to use this"));

    let screen = drawn(&mut app, 160, 60);
    assert!(screen.contains("Sign in to the calendar: 1 task waits for it"), "{screen}");
    assert!(screen.contains("Update the TUI to see this"), "{screen}");
    assert!(screen.contains("Sign in with a passkey — Update the TUI to use this"), "{screen}");
    assert!(screen.contains("Pick a time — Update the TUI to use this"), "{screen}");
    assert!(screen.contains(" 2  Go on without the calendar"), "{screen}");
    assert!(screen.contains("Save — Update the TUI to use this"), "{screen}");
}

#[test]
fn an_outcome_kind_from_a_newer_daemon_shows_the_daemons_title_instead_of_a_failure() {
    let reply = super::view_action_result(json!({"result": {"kind": "queued", "title": "Queued for the Mac"}}));
    let Ok(super::ViewActionReply::Outcome(outcome)) = reply else { panic!("not an outcome") };
    assert_eq!(outcome.kind, OutcomeKind::Changed);
    assert_eq!(outcome.title, "Queued for the Mac");
}

fn drawn(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    buffer
        .content()
        .chunks(buffer.area.width as usize)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

const ALLOWS: &str = "Allows this now and stops asking in `alondra` for:\n· `make build` with any arguments\nYou can remove it in Settings › Permissions.";

#[test]
fn a_button_shows_the_words_the_server_puts_under_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(super::parity::view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    let mut card = super::parity::view_fixture("card-question.json");
    let key = card["key"].as_str().unwrap().to_owned();
    let buttons = &mut card["sections"][0]["items"][0]["actions"];
    buttons[0]["label"] = json!("Allow always");
    buttons[0]["option_id"] = json!("always");
    buttons[0]["description"] = json!(ALLOWS);
    buttons[1]["description"] = json!("Preserve the more conversational explanation.");
    app.sel = Some(Key(key.trim_start_matches("card:").to_owned()));
    app.on_msg(Msg::ViewCard { key, result: Ok(serde_json::from_value(card).unwrap()) });

    let screen = drawn(&mut app, 180, 50);
    let under = |text: &str| screen.lines().position(|line| line.contains(text));
    let button = under("Allow always").expect(&screen);
    assert_eq!(under("Allows this now and stops asking in alondra for:"), Some(button + 1), "{screen}");
    assert_eq!(under("· make build with any arguments"), Some(button + 2), "{screen}");
    assert_eq!(under("You can remove it in Settings › Permissions."), Some(button + 3), "{screen}");
    let warm = under("Keep the warmer copy").expect(&screen);
    assert_eq!(under("Preserve the more conversational explanation."), Some(warm + 1), "{screen}");
}

fn rule(id: &str, title: &str, subtitle: &str) -> Value {
    json!({"type": "row", "id": id, "icon": "check", "title": title, "subtitle": subtitle, "depth": 0, "struck": false,
        "actions": [{"id": format!("remove_rule:{id}@6"), "label": "Remove", "style": "destructive", "presentation": "button",
            "input": {"kind": "none"}, "attachments": false, "enabled": true, "stamp": "Removed", "resolves_card": false}]})
}

fn machine(rules: Vec<Value>) -> ViewDocument {
    let mut view = super::parity::view_fixture("machine-ios.json");
    view["key"] = json!("machine");
    view["sections"] = json!([{"id": "gauges", "collapsed": false, "items": [
        {"type": "facts", "items": [{"label": "Memory", "value": "6.1 GB of 16 GB"}]}]}]);
    if !rules.is_empty() {
        view["sections"].as_array_mut().unwrap().push(json!({
            "id": "allowed_rules", "title": "Permissions allowed always", "collapsed": false, "items": rules}));
    }
    serde_json::from_value(view).unwrap()
}

fn rules_open(rules: Vec<Value>) -> App {
    let mut app = app();
    app.screen = Screen::Tasks;
    press(&mut app, KeyCode::Char('a'));
    assert!(app.rules.is_some(), "a on the Tasks opens the rules");
    app.on_msg(Msg::ViewCard { key: "machine".into(), result: Ok(machine(rules)) });
    app
}

fn shown_rules(app: &App) -> Vec<String> {
    app.rules.as_ref().unwrap().rows().iter().map(|row| row.id.clone()).collect()
}

#[test]
fn the_rules_panel_lists_each_rule_of_the_machine_view_and_esc_closes_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = rules_open(vec![
        rule("rule-3", "`make build` with any arguments", "alondra · added 29 Sep · task «Build the importer»"),
        rule("rule-4", "`cargo test` with any arguments", "alondra · added 30 Sep"),
    ]);
    let screen = drawn(&mut app, 120, 30);
    assert!(screen.contains("Permissions allowed always"), "{screen}");
    assert!(screen.contains("make build with any arguments"), "{screen}");
    assert!(screen.contains("alondra · added 29 Sep · task «Build the importer»"), "{screen}");
    assert!(screen.contains("cargo test with any arguments"), "{screen}");
    assert!(screen.contains("x Remove"), "{screen}");
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.rules.as_ref().unwrap().selected().map(|row| row.id.as_str()), Some("rule-4"));
    press(&mut app, KeyCode::Esc);
    assert!(app.rules.is_none());

    let mut empty = rules_open(vec![]);
    let screen = drawn(&mut empty, 120, 30);
    assert!(screen.contains("No command is allowed always yet"), "{screen}");
}

#[test]
fn remove_hides_the_rule_at_once_and_a_refusal_brings_it_back_with_its_message() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = rules_open(vec![
        rule("rule-3", "`make build` with any arguments", "alondra · added 29 Sep"),
        rule("rule-4", "`cargo test` with any arguments", "alondra · added 30 Sep"),
    ]);
    press(&mut app, KeyCode::Char('x'));
    let request = app.posted.borrow().last().cloned().unwrap();
    assert_eq!(request.0, "/actions");
    assert_eq!(request.1["action_id"], "remove_rule:rule-3@6");
    assert_eq!(shown_rules(&app), ["rule-4"], "the rule leaves before the Mac answers");

    let refused = view_action_result(json!({"outcome": "rejected", "message": "That rule was already removed"})).unwrap();
    app.on_msg(Msg::ViewAction { request: request.1, result: Ok(refused) });
    assert_eq!(shown_rules(&app), ["rule-3", "rule-4"]);
    let flash = app.flash.as_ref().unwrap();
    assert!(flash.kind == FlashKind::Error);
    assert_eq!(flash.text, "That rule was already removed");

    press(&mut app, KeyCode::Char('x'));
    let request = app.posted.borrow().last().cloned().unwrap();
    let asked = app.spawned.borrow().len();
    let applied = view_action_result(json!({"outcome": "applied", "stamp": "Removed"})).unwrap();
    app.on_msg(Msg::ViewAction { request: request.1, result: Ok(applied) });
    assert_eq!(shown_rules(&app), ["rule-4"]);
    assert_eq!(app.spawned.borrow().len(), asked + 1, "the list is asked again once it is gone");
}

fn grant(card: &str, title: &str, subtitle: &str) -> Value {
    json!({"type": "row", "id": format!("grant-{card}"), "icon": "check", "title": title, "subtitle": subtitle, "depth": 0, "struck": false,
        "actions": [{"id": format!("revoke_grant:t-7@9~{card}"), "label": "Revoke", "style": "destructive", "presentation": "button",
            "input": {"kind": "none"}, "attachments": false, "enabled": true, "stamp": "Revoked", "resolves_card": false}]})
}

fn task_allowing(grants: Vec<Value>) -> ViewDocument {
    let mut view = super::parity::view_fixture("machine-ios.json");
    view["key"] = json!("task:t-7");
    view["sections"] = json!([{"id": "allowed_in_task", "title": "Allowed in this task", "collapsed": false, "items": grants}]);
    serde_json::from_value(view).unwrap()
}

#[test]
fn v_on_a_task_that_allowed_something_lists_it_and_revoke_takes_it_back_at_once() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    app.screen = Screen::Tasks;
    press(&mut app, KeyCode::Char('v'));
    assert!(app.rules.is_none(), "a task that allowed nothing has no list to open");
    let grants = vec![
        grant("decision-t-7-1-3", "git reset --hard origin/main", "exact command, from the task's worktree · used once"),
        grant("decision-t-7-1-4", "bazelisk test …", "any arguments, inside the task's folders · not used yet"),
    ];
    app.card_view = Some(task_allowing(grants.clone()));
    press(&mut app, KeyCode::Char('v'));
    assert!(app.rules.as_ref().is_some_and(|rules| rules.source == super::rules::Source::Task("t-7".into())));
    app.on_msg(Msg::ViewCard { key: "task:t-7".into(), result: Ok(task_allowing(grants)) });
    let screen = drawn(&mut app, 120, 30);
    assert!(screen.contains("Allowed in this task"), "{screen}");
    assert!(screen.contains("git reset --hard origin/main"), "{screen}");
    assert!(screen.contains("exact command, from the task's worktree · used once"), "{screen}");
    assert!(screen.contains("x Revoke"), "{screen}");
    press(&mut app, KeyCode::Char('x'));
    let request = app.posted.borrow().last().cloned().unwrap();
    assert_eq!(request.1["action_id"], "revoke_grant:t-7@9~decision-t-7-1-3");
    assert_eq!(shown_rules(&app), ["grant-decision-t-7-1-4"], "it leaves before the Mac answers");
    press(&mut app, KeyCode::Esc);
    assert!(app.rules.is_none());
}

fn sign_in_page(returns_to_claude: bool) -> Result<Value, String> {
    Ok(json!({"server": "linear", "url": "https://linear.app/oauth/authorize?state=s1", "returns_to_claude": returns_to_claude}))
}

#[test]
fn connect_on_the_daemon_s_mac_opens_the_browser_and_the_return_needs_nothing_more() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    app.mcp_connect("mcp-sign-in-t-7", "linear");
    assert_eq!(app.posted.borrow().last().map(|(path, _)| path.clone()).as_deref(), Some("/mcp-sign-ins/mcp-sign-in-t-7/connect"));
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), sign_in_page(true));
    assert!(app.composer.is_none(), "the browser comes back to Claude on this Mac by itself");
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), Ok(json!({"server": "linear", "connected": true})));
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), Ok(json!({"server": "linear", "url": "http://evil.example.com/", "returns_to_claude": true})));
    assert!(app.composer.is_none(), "only an https page, or one on this Mac, is opened");
}

#[test]
fn connect_away_from_the_mac_asks_for_where_the_browser_came_back_to_and_keeps_it_nowhere() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::with_target(tx, direct_mac("mac-studio"));
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), sign_in_page(true));
    assert!(matches!(&app.composer, Some(c) if matches!(&c.target, Target::McpReturn { server, .. } if server == "linear")));
    let screen = drawn(&mut app, 160, 40);
    assert!(screen.contains("Sign in to linear in the browser, then paste the address of the page that did not load"), "{screen}");
    let address = "http://localhost:49200/callback?code=one-time&state=s1";
    app.on_paste(address);
    assert!(app.to_restore().composer.is_none(), "a restart never hands the address to the next process");
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), sign_in_page(true));
    app.on_paste(address);
    press(&mut app, KeyCode::Esc);
    assert!(app.composer.is_none(), "Esc drops it");
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), sign_in_page(true));
    app.on_paste(address);
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.posted.borrow().last().cloned(), Some(("/mcp-sign-ins/mcp-sign-in-t-7/return".to_owned(), json!({"address": address}))));
    app.mcp_opened("mcp-sign-in-t-7".into(), "linear".into(), sign_in_page(false));
    assert!(app.composer.is_none(), "a claude.ai connector is connected on claude.ai, then reconnected");
}

fn mcp_servers_open() -> App {
    let mut app = app();
    app.screen = Screen::Inbox;
    press(&mut app, KeyCode::Char(','));
    assert!(app.mcp_servers.is_some(), ", opens Settings from any screen");
    let view: ViewDocument = serde_json::from_value(super::parity::view_fixture("mcp-servers-ios.json")).unwrap();
    app.on_msg(Msg::ViewCard { key: "mcp_servers".into(), result: Ok(view) });
    app
}

#[test]
fn settings_open_on_general_where_the_merge_method_set_in_settings_is_picked_with_one_post() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = mcp_servers_open();
    let machine: ViewDocument = serde_json::from_value(super::parity::view_fixture("machine-settings-ios.json")).unwrap();
    app.on_msg(Msg::ViewCard { key: "machine".into(), result: Ok(machine) });
    let screen = drawn(&mut app, 140, 60);
    for shown in ["GENERAL", "PR merges", "Squash and merge · if the repository allows it", "1 Change…", "Nodes at once", "MCP SERVERS", "9 servers · 3 need sign-in"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    assert!(!screen.contains("linear"), "the servers themselves are on their own screen: {screen}");
    assert!(screen.find("PR merges") < screen.find("MCP SERVERS"), "General comes first: {screen}");
    assert_eq!(app.mcp_servers.as_ref().unwrap().selected().map(|row| row.id.as_str()), Some("merge_method"));

    press(&mut app, KeyCode::Enter);
    assert!(app.posted.borrow().is_empty(), "Enter opens the menu");
    let menu = drawn(&mut app, 140, 60);
    for shown in [" 1  Squash and merge · now", "One commit on the base branch", " 2  Merge", " 3  Rebase and merge", "1-3 pick"] {
        assert!(menu.contains(shown), "{shown}: {menu}");
    }
    press(&mut app, KeyCode::Esc);
    assert!(app.card_menu.is_none() && app.mcp_servers.is_some(), "Esc closes the menu only");
    assert!(app.posted.borrow().is_empty());

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('3'));
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "/actions");
    assert!(sent[0].1["action_id"].as_str().is_some_and(|id| id.starts_with("set_merge_method:")), "{sent:?}");
    assert_eq!(sent[0].1["option_id"], "rebase");
    assert!(app.mcp_servers.is_some(), "Settings stay open");
}

#[test]
fn how_a_prs_branch_is_updated_with_main_is_below_the_merge_method_and_picked_with_one_post() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let mut app = mcp_servers_open();
    let machine: ViewDocument = serde_json::from_value(super::parity::view_fixture("machine-settings-ios-es.json")).unwrap();
    app.on_msg(Msg::ViewCard { key: "machine".into(), result: Ok(machine) });
    let screen = drawn(&mut app, 140, 60);
    for shown in ["Actualizar la rama de un PR con main", "Rebase · cuando la PR tiene conflictos"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    assert!(screen.find("Merge de las PR") < screen.find("Actualizar la rama") && screen.find("Actualizar la rama") < screen.find("Nodos a la vez"), "{screen}");

    press(&mut app, KeyCode::Down);
    assert_eq!(app.mcp_servers.as_ref().unwrap().selected().map(|row| row.id.as_str()), Some("branch_update"));
    press(&mut app, KeyCode::Enter);
    let menu = drawn(&mut app, 140, 60);
    for shown in [" 1  Rebase · ahora", "--force-with-lease", " 2  Merge", "Un commit de merge trae main a la rama"] {
        assert!(menu.contains(shown), "{shown}: {menu}");
    }
    press(&mut app, KeyCode::Char('2'));
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().is_some_and(|id| id.starts_with("set_branch_update:")), "{sent:?}");
    assert_eq!(sent[0].1["option_id"], "merge");
}

#[test]
fn the_mcp_servers_screen_lists_each_project_s_servers_and_runs_the_selected_one_s_buttons() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = mcp_servers_open();
    assert!(app.mcp_servers.as_ref().unwrap().on_servers_row(), "with nothing else loaded, the MCP servers' row is the one selected");
    let settings = drawn(&mut app, 140, 60);
    assert!(settings.contains("Enter open") && !settings.contains("Settings ›"), "{settings}");
    press(&mut app, KeyCode::Enter);
    assert!(app.mcp_servers.as_ref().unwrap().detail, "Enter on the row opens the MCP servers' screen");
    let screen = app.mcp_servers.as_mut().unwrap();
    screen.sel = screen.rows().iter().position(|row| row.title == "linear").expect("linear");
    let screen = drawn(&mut app, 140, 60);
    for shown in ["Settings › MCP servers", "alondra", "quill", "Other projects", "Check reads the state of their servers without starting a task.",
        "linear  Needs sign-in", "notion  Signing in", "sentry  Sign-in failed", "revenuecat  Needs sign-in", "1 Connect"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    let target = app.mcp_servers.as_ref().unwrap().selected().unwrap().id.clone();
    assert!(target.starts_with("mcp-server-"), "{target}");
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(app.posted.borrow().last().map(|(path, _)| path.clone()), Some(format!("/mcp-sign-ins/{target}/connect")));
    assert!(!screen.contains("1 Connect   2 Reconnect"), "one thing to do per server: {screen}");

    let screen = app.mcp_servers.as_mut().unwrap();
    screen.sel = screen.rows().iter().position(|row| row.actions.iter().any(|action| action.label == "Sign out")).expect("a server to sign out of");
    let sign_out = app.mcp_servers.as_ref().unwrap().selected().unwrap().clone();
    let index = super::mcp_servers::actions(&sign_out).iter().position(|action| action.label == "Sign out").unwrap();
    press(&mut app, KeyCode::Char(char::from_digit(index as u32 + 1, 10).unwrap()));
    assert_eq!(app.modal.as_ref().map(|modal| modal.title.as_str()), Some("Sign out of github?"), "signing out asks first");
    press(&mut app, KeyCode::Esc);
    assert!(app.mcp_servers.is_some());

    select(&mut app, "linear");
    let deletes = drawn(&mut app, 140, 60);
    assert!(deletes.contains("1 Connect   2 Delete"), "{deletes}");
    press(&mut app, KeyCode::Char('2'));
    let modal = app.modal.as_ref().expect("deleting asks first");
    assert_eq!((modal.title.as_str(), modal.destructive), ("Delete linear?", true));
    assert!(modal.body.contains("Tasks that already use it keep it until they start again"), "{}", modal.body);
    press(&mut app, KeyCode::Enter);
    let sent = app.posted.borrow().last().cloned().unwrap();
    assert_eq!(sent.0, "/actions");
    assert!(sent.1["action_id"].as_str().is_some_and(|id| id.starts_with("mcp_remove:mcp-server-")), "{sent:?}");

    press(&mut app, KeyCode::Esc);
    let screen = app.mcp_servers.as_ref().expect("Esc goes back to Settings");
    assert!(!screen.detail && screen.on_servers_row(), "on the row it came from");
    press(&mut app, KeyCode::Esc);
    assert!(app.mcp_servers.is_none());
}

#[test]
fn colon_mcp_opens_the_mcp_servers_screen_directly() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let mut app = app();
    app.screen = Screen::Inbox;
    app.run_command("mcp");
    let view: ViewDocument = serde_json::from_value(super::parity::view_fixture("mcp-servers-ios-es.json")).unwrap();
    app.on_msg(Msg::ViewCard { key: "mcp_servers".into(), result: Ok(view) });
    let screen = drawn(&mut app, 140, 60);
    assert!(screen.contains("Ajustes › Servidores MCP") && screen.contains("linear  Necesita iniciar sesión"), "{screen}");
    press(&mut app, KeyCode::Esc);
    let settings = drawn(&mut app, 140, 60);
    assert!(settings.contains("SERVIDORES MCP") && settings.contains("9 servidores · 3 piden iniciar sesión"), "{settings}");
}

fn accounts_open(fixture: &str) -> App {
    let mut app = app();
    app.screen = Screen::Inbox;
    press(&mut app, KeyCode::Char(','));
    let view: ViewDocument = serde_json::from_value(super::parity::view_fixture(fixture)).unwrap();
    app.on_msg(Msg::ViewCard { key: "accounts".into(), result: Ok(view) });
    app
}

fn select(app: &mut App, title: &str) {
    let screen = app.mcp_servers.as_mut().unwrap();
    screen.sel = screen.rows().iter().position(|row| row.title == title).unwrap_or_else(|| panic!("{title}"));
}

#[test]
fn settings_list_the_claude_accounts_between_general_and_mcp_servers_and_run_their_buttons() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = accounts_open("accounts-ios.json");
    let servers: ViewDocument = serde_json::from_value(super::parity::view_fixture("mcp-servers-ios.json")).unwrap();
    app.on_msg(Msg::ViewCard { key: "mcp_servers".into(), result: Ok(servers) });
    assert!(app.open_views.lock().unwrap().contains_key("accounts"), "a change to the accounts reaches the open screen");
    select(&mut app, "Work");
    let screen = drawn(&mut app, 140, 80);
    for shown in ["Settings", "ACCOUNTS", "Default  Signed in", "someone@work.example · max_20x · weight 8, set by you",
        "5 h 61 % · week 48 % · 1 node", "Personal  Signed out", "Old  Off", "Add a Claude account", "New work", "Most room first",
        "Move a node when its account runs out  Off", "Continue with the next eligible account in your order when quota runs out",
        "1 Weight   2 Move up   3 Move down   4 Turn off", "MCP SERVERS"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    assert!(screen.find("Add a Claude account") < screen.find("MCP SERVERS"), "the accounts come before the MCP servers: {screen}");
    assert!(screen.contains("Open tasks still run on Work"), "a disabled remove explains why: {screen}");

    press(&mut app, KeyCode::Char('1'));
    let menu = drawn(&mut app, 140, 80);
    assert!(menu.contains(" 1  By plan (10)") && menu.contains(" 4  3"), "the weight's options: {menu}");
    press(&mut app, KeyCode::Char('4'));
    let (path, body) = app.posted.borrow().last().cloned().unwrap();
    assert_eq!(path, "/actions");
    assert!(body["action_id"].as_str().unwrap().starts_with("account_weight:work@"), "{body}");
    assert_eq!(body["option_id"], "3");

    select(&mut app, "Most room first");
    press(&mut app, KeyCode::Char('1'));
    let (_, body) = app.posted.borrow().last().cloned().unwrap();
    assert!(body["action_id"].as_str().unwrap().starts_with("account_policy:policy@"), "the one other way runs at once: {body}");
    assert_eq!(body["option_id"], "chain");

    select(&mut app, "Move a node when its account runs out");
    press(&mut app, KeyCode::Char('1'));
    let (_, body) = app.posted.borrow().last().cloned().unwrap();
    assert!(body["action_id"].as_str().unwrap().starts_with("account_moves:moves@"), "{body}");
    assert_eq!(body["option_id"], "on");

    select(&mut app, "Old");
    let remove = super::mcp_servers::actions(app.mcp_servers.as_ref().unwrap().selected().unwrap()).iter().position(|action| action.label == "Remove").unwrap();
    press(&mut app, KeyCode::Char(char::from_digit(remove as u32 + 1, 10).unwrap()));
    assert_eq!(app.modal.as_ref().map(|modal| modal.title.as_str()), Some("Remove Old?"), "removing asks first");
    press(&mut app, KeyCode::Esc);
}

#[test]
fn adding_an_account_asks_its_name_then_signs_it_in_on_this_screen() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = accounts_open("accounts-ios.json");
    select(&mut app, "Add a Claude account");
    press(&mut app, KeyCode::Char('1'));
    assert!(matches!(&app.composer, Some(c) if matches!(c.target, Target::AccountName { .. })), "it asks only for a name");
    app.on_paste("Side project");
    let flow = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(&flow, Flow::AccountSignIn(sign_in) if sign_in.args == ["accounts", "add", "Side project"]),
        "the sign-in runs on the screen: {:?}", matches!(flow, Flow::Go));

    select(&mut app, "Personal");
    let flow = app.on_key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::NONE));
    assert!(matches!(&flow, Flow::AccountSignIn(sign_in) if sign_in.args == ["accounts", "login", "Personal"]));
    app.account_signed_in(Err(anyhow::anyhow!("That Claude account is already in Alondra as Default")));
    assert!(drawn(&mut app, 140, 50).contains("That Claude account is already in Alondra as Default"));
}

/// A Mac that takes 22 nodes offers 23 choices: the first nine have a number, ↑↓ reach the rest.
#[test]
fn every_node_limit_the_mac_offers_is_reached_with_the_arrows_and_picked_with_enter() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = mcp_servers_open();
    let mut machine = super::parity::view_fixture("machine-settings-ios.json");
    let limit = machine["sections"].as_array_mut().unwrap().iter_mut()
        .flat_map(|section| section["items"].as_array_mut().unwrap().iter_mut())
        .flat_map(|item| item["actions"].as_array_mut().into_iter().flatten())
        .find(|action| action["id"].as_str().is_some_and(|id| id.starts_with("set_limit:"))).unwrap();
    let template = limit["options"][1].clone();
    let options = limit["options"].as_array_mut().unwrap();
    options.truncate(1);
    options.extend((1..=22).map(|count| {
        let mut option = template.clone();
        option["id"] = json!(count.to_string());
        option["label"] = json!(count.to_string());
        option
    }));
    let machine: ViewDocument = serde_json::from_value(machine).unwrap();
    app.on_msg(Msg::ViewCard { key: "machine".into(), result: Ok(machine) });
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    assert_eq!(app.mcp_servers.as_ref().unwrap().selected().map(|row| row.id.as_str()), Some("node_limit"));
    press(&mut app, KeyCode::Enter);
    let menu = drawn(&mut app, 140, 40);
    assert!(menu.contains(" 1  Automatic (6) · now") && menu.contains("↑↓ 1 of 23 · ⏎ pick") && !menu.contains(" 22"), "{menu}");
    for _ in 0..30 {
        press(&mut app, KeyCode::Down);
    }
    let menu = drawn(&mut app, 140, 40);
    assert!(menu.contains("22") && menu.contains("↑↓ 23 of 23 · ⏎ pick") && !menu.contains("Automatic (6)"), "{menu}");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    let sent = app.posted.borrow().clone();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().is_some_and(|id| id.starts_with("set_limit:")), "{sent:?}");
    assert_eq!(sent[0].1["option_id"], "21");
    let picked = drawn(&mut app, 140, 40);
    assert!(picked.contains("Nodes at once: 21") && !picked.contains("Limit: automatic (6)"), "the row says the pick at once: {picked}");
    app.on_msg(Msg::ViewAction { request: sent[0].1.clone(), result: Err(crate::client::Delivery::Answered(Some("Choose an option".into()))) });
    let refused = drawn(&mut app, 140, 40);
    assert!(refused.contains("Limit: automatic (6)") && !refused.contains("Nodes at once: 21"), "refused, the row says how it is: {refused}");

    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('9'));
    let sent = app.posted.borrow().last().unwrap().1.clone();
    assert_eq!(sent["option_id"], "8", "9 picks the ninth choice");
    let done = alondra_core::views::Outcome { kind: alondra_core::views::OutcomeKind::Done, title: "Done".into(), body: None, next: None };
    app.on_msg(Msg::ViewAction { request: sent, result: Ok(ViewActionReply::Outcome(done)) });
    assert!(drawn(&mut app, 140, 40).contains("Nodes at once: 8"), "done, the pick stays until the Mac's view says how it is");
    let mut machine = super::parity::view_fixture("machine-settings-ios.json");
    let text = serde_json::to_string(&machine).unwrap().replace("Limit: automatic (6)", "Limit: 8, set by you · automatic would be 6");
    machine = serde_json::from_str(&text).unwrap();
    let machine: ViewDocument = serde_json::from_value(machine).unwrap();
    app.on_msg(Msg::ViewCard { key: "machine".into(), result: Ok(machine) });
    let confirmed = drawn(&mut app, 140, 40);
    assert!(confirmed.contains("Limit: 8, set by you · automatic would be 6") && !confirmed.contains("Nodes at once: 8"), "{confirmed}");
}

fn mixed_provider_accounts() -> ViewDocument {
    let mut view: ViewDocument = serde_json::from_value(super::parity::view_fixture("accounts-ios.json")).unwrap();
    let template = view.sections.iter().flat_map(|section| &section.items).find_map(|item| match item { alondra_core::views::Item::Row(row) => Some(row.clone()), _ => None }).unwrap();
    view.account_configuration = Some(alondra_core::views::AccountConfigurationPresentation { version: "cfg-one".into() });
    view.sections.truncate(1);
    view.sections[0].items = ["codex", "claude"].into_iter().enumerate().map(|(position, provider)| {
        let mut row = template.clone(); row.id = format!("{provider}-work"); row.title = "Work".into(); row.actions.clear();
        row.account = Some(serde_json::from_value(json!({"key":{"provider":provider,"account":"work"},"position":position+1,
            "availability":{"text":"Signed in","tone":"success"},"active_nodes":2,"freshness":{"text":"Last reported usage","tone":"neutral"},
            "windows":[{"kind":"codex:account:primary:300","label":"Primary · 300 min","used_percent":42,"measured_at":1000,"resets_at":18000000,"stale":false,"exhausted":false}]})).unwrap());
        alondra_core::views::Item::Row(row)
    }).collect();
    view.sections[0].actions = vec![serde_json::from_value(json!({"id":"account_order:accounts@0~cfg-one","label":"Reorder accounts","style":"secondary","presentation":"button","input":{"kind":"none"},"attachments":false,"enabled":true})).unwrap()];
    view
}

#[test]
fn provider_account_details_and_order_keep_identity_and_send_one_atomic_save_until_its_own_answer() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = accounts_open("accounts-ios.json");
    let view = mixed_provider_accounts(); app.accounts_loaded(&view);
    select(&mut app, "Work");
    press(&mut app, KeyCode::Enter);
    assert!(app.mcp_servers.as_ref().unwrap().account_detail.is_some());
    let detail = drawn(&mut app, 140, 45);
    assert!(detail.contains("Codex · work · Signed in") && detail.contains("42% used") && detail.contains("1970-01-01"), "{detail}");
    press(&mut app, KeyCode::Esc); press(&mut app, KeyCode::Char('o'));
    press(&mut app, KeyCode::Char('J'));
    let draft = app.mcp_servers.as_ref().unwrap().order.as_ref().unwrap();
    assert_eq!(draft.keys().iter().map(|key| key.wire_id()).collect::<Vec<_>>(), ["claude/work", "codex/work"]);
    assert_eq!(app.mcp_servers.as_ref().unwrap().sel, 1, "the moved account remains selected by identity");
    assert!(app.posted.borrow().is_empty(), "moving only changes the local draft");
    press(&mut app, KeyCode::Enter);
    let request = app.posted.borrow().last().unwrap().1.clone();
    assert_eq!(request["input"]["fields"]["order"], json!([{"provider":"claude","account":"work"},{"provider":"codex","account":"work"}]));
    assert_eq!(request["input"]["fields"]["expected_config_version"], "cfg-one");
    app.view_action_done(request.clone(), Err(crate::client::Delivery::Unanswered));
    press(&mut app, KeyCode::Enter); press(&mut app, KeyCode::Esc);
    assert_eq!(app.posted.borrow().len(), 1, "an uncertain save cannot become a second mutation");
    assert!(app.mcp_servers.as_ref().unwrap().order.as_ref().unwrap().command.is_some());
    let mut unrelated = request.clone(); unrelated["command_id"] = json!("another-command");
    let done = || Ok(ViewActionReply::Outcome(alondra_core::views::Outcome { kind: alondra_core::views::OutcomeKind::Done, title: "Saved".into(), body: None, next: None }));
    app.view_action_done(unrelated, done());
    assert!(app.mcp_servers.as_ref().unwrap().order.is_some(), "another receipt cannot acknowledge this draft");
    app.view_action_done(request, done());
    assert!(app.mcp_servers.as_ref().unwrap().order.is_none());
}

#[test]
fn provider_order_version_conflict_retains_the_draft_and_requires_explicit_reload() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = accounts_open("accounts-ios.json"); let mut view = mixed_provider_accounts(); app.accounts_loaded(&view);
    press(&mut app, KeyCode::Char('o')); press(&mut app, KeyCode::Char('J'));
    view.account_configuration.as_mut().unwrap().version = "cfg-two".into(); app.accounts_loaded(&view);
    press(&mut app, KeyCode::Enter);
    let draft = app.mcp_servers.as_ref().unwrap().order.as_ref().unwrap();
    assert!(draft.conflict); assert_eq!(draft.keys()[0].wire_id(), "claude/work"); assert!(app.posted.borrow().is_empty());
    press(&mut app, KeyCode::Char('r'));
    let draft = app.mcp_servers.as_ref().unwrap().order.as_ref().unwrap();
    assert!(!draft.conflict); assert_eq!(draft.version, "cfg-two"); assert_eq!(draft.keys()[0].wire_id(), "codex/work");
    press(&mut app, KeyCode::Esc); assert!(app.mcp_servers.as_ref().unwrap().order.is_none());
}

#[test]
fn a_restored_account_composer_cannot_execute_another_cli_command() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = accounts_open("accounts-ios.json");
    app.compose(Target::AccountName { provider: alondra_core::accounts::Provider::Codex, argv: vec!["accounts".into(), "remove".into()] }, "");
    app.on_paste("Keep this name");
    let flow = app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(flow, Flow::Go));
    assert!(app.composer.is_some());
    assert_eq!(app.flash.as_ref().unwrap().text, "Reload Accounts before adding this account");
    let screen = drawn(&mut app, 140, 50);
    assert!(screen.contains("Reload Accounts before adding this account"), "{screen}");
}

#[test]
fn a_settings_profile_link_opens_the_generic_view_and_returns_to_the_selected_role() {
    let mut app = accounts_open("accounts-ios.json");
    app.home_view = Some(serde_json::from_value(super::parity::view_fixture("home-tui.json")).unwrap());
    app.loaded = true;
    select(&mut app, "Claude · Orchestrator");
    let row = app.mcp_servers.as_mut().unwrap().accounts.as_mut().unwrap().sections.iter_mut()
        .flat_map(|section| &mut section.items).find_map(|item| match item { alondra_core::views::Item::Row(row) if row.title == "Claude · Orchestrator" => Some(row), _ => None }).unwrap();
    let navigation: alondra_core::views::Action = serde_json::from_value(serde_json::json!({
        "id":"", "label":"Edit ordered candidates", "style":"plain", "presentation":"button", "input":{"kind":"none"},
        "attachments":false, "enabled":true, "link":{"kind":"view","key":"model_profile:claude-orchestrator"}
    })).unwrap();
    row.actions = vec![navigation];
    let before = app.mcp_servers.as_ref().unwrap().selected().unwrap().id.clone();
    press(&mut app, KeyCode::Char('1'));
    assert!(app.mcp_servers.is_none(), "the specialized Settings overlay must not hide the generic form");
    assert_eq!(app.view_navigation.last().map(String::as_str), Some("model_profile:claude-orchestrator"));
    assert!(app.posted.borrow().is_empty(), "opening a form is navigation, not a mutation");
    let mut form: ViewDocument = serde_json::from_value(super::parity::view_fixture("new-request-ios.json")).unwrap();
    form.key = "model_profile:claude-orchestrator".into(); form.view = "model_profile".into(); form.title = Some("Orchestrator profile".into());
    app.on_msg(Msg::ViewCard { key: form.key.clone(), result: Ok(form) });
    for width in [72, 144] {
        let shown = drawn(&mut app, width, 40);
        assert!(shown.contains("Orchestrator profile"), "{width}: {shown}");
    }
    press(&mut app, KeyCode::Esc);
    assert!(app.settings_after_view.is_none());
    assert_eq!(app.mcp_servers.as_ref().unwrap().selected().unwrap().id, before, "Back preserves the selected role");
}

#[test]
fn a_settings_only_model_form_offers_save_and_only_its_declared_inputs() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    app.home_view = Some(serde_json::from_value(super::parity::view_fixture("home-tui.json")).unwrap()); app.loaded = true;
    let view: ViewDocument = serde_json::from_value(super::parity::view_fixture("model-profile-codex-tui.json")).unwrap();
    let mut form = work::OptionsForm::new(&view.key); form.loaded(&view);
    app.composer = Some(Composer { target: Target::Form { view: view.key.clone() }, input: TextInput::default(), chips: vec![], form: Some(form) });
    for width in [72, 144] {
        let screen = drawn(&mut app, width, 40);
        assert!(screen.contains("Tab chooses settings"), "{width}: {screen}");
        assert!(!screen.contains("Type here"), "{width}: {screen}");
        assert!(!screen.contains("project, issue and options"), "{width}: {screen}");
    }
    press(&mut app, KeyCode::Char('x')); app.on_paste("undeclared body");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
    assert!(app.composer.as_ref().unwrap().input.text().is_empty());
    app.attach(std::path::PathBuf::from("a-file-not-read"));
    assert!(app.composer.as_ref().unwrap().chips.is_empty());
    assert_eq!(app.flash.as_ref().unwrap().text, "This form does not take attachments");
    press(&mut app, KeyCode::Tab);
    assert!(app.composer.as_ref().unwrap().form.as_ref().unwrap().focus.is_some(), "declared settings remain editable");
    press(&mut app, KeyCode::Esc); assert!(app.composer.as_ref().unwrap().form.as_ref().unwrap().focus.is_none());
    press(&mut app, KeyCode::Esc); assert!(app.composer.is_none());
}

fn built_report(repo: Option<&str>, passed: bool) -> Value {
    json!({
        "schema": "alondra.report/1",
        "id": "r-20261027-0a1b2c3d",
        "dir": "/Users/someone/.alondra/reports/r-20261027-0a1b2c3d",
        "compressed_bytes": 48_000,
        "manifest": {
            "window": {"hours": 72},
            "description": {"bytes": 31, "sha256": "d"},
            "sections": [
                {"name": "versions.json", "bytes": 210, "sha256": "a"},
                {"name": "daemon.log", "bytes": 180_000, "sha256": "b"},
                {"name": "sdk-host.jsonl", "empty": "no SDK host ran"},
            ],
            "omitted": {"daemon_log_lines_without_template": 4},
        },
        "check": {"passed": passed, "hits": if passed { json!([]) } else { json!([{"section": "daemon.log", "line": 7, "what": "a path of this Mac"}]) }},
        "repo": repo,
        "failure": "session-ended",
    })
}

fn type_and_enter(app: &mut App, text: &str) {
    app.on_paste(text);
    press(app, KeyCode::Enter);
}

#[test]
fn a_crash_report_asks_what_happened_shows_every_file_and_asks_once_whether_to_send_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    press(&mut app, KeyCode::Char(':'));
    for c in "report".chars() {
        press(&mut app, KeyCode::Char(c));
    }
    press(&mut app, KeyCode::Enter);
    assert!(matches!(&app.composer, Some(c) if matches!(&c.target, Target::ReportDescription { failure } if failure == "other")),
        ":report asks what happened, as «other»");
    type_and_enter(&mut app, "The node stopped after the Mac slept");
    let report = app.crash_report.as_ref().expect("the report is being built");
    assert!(matches!(report.stage, crash_report::Stage::Building));
    assert_eq!(report.description.as_deref(), Some("The node stopped after the Mac slept"));

    app.on_msg(Msg::Report(crash_report::Answer::Built(Ok(built_report(Some("owner/reports"), true)))));
    let screen = drawn(&mut app, 120, 40);
    for shown in ["Crash report r-20261027-0a1b2c3d · last 72 hours", "What happened? “The node stopped after the Mac slept”",
        "manifest.json", "description.txt", "daemon.log", "no SDK host ran", "The final check found nothing private.",
        "This is exactly what is sent. Anyone with the link of its issue will be able to read it",
        "Send it to owner/reports? Anyone who sees its issue will be able to read it.", "Yes, send it", "No, keep it on this Mac"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    for hidden in ["recipient", "encrypt"] {
        assert!(!screen.to_lowercase().contains(hidden), "{hidden}: {screen}");
    }

    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Enter);
    let viewing = app.crash_report.as_ref().unwrap().viewing.as_ref().expect("Enter shows the file");
    assert_eq!(viewing.name, "daemon.log", "the empty section is not a file to open");
    app.on_msg(Msg::Report(crash_report::Answer::File { id: "r-20261027-0a1b2c3d".into(), name: "daemon.log".into(),
        result: Ok(json!({"text": "2026-10-27T10:00:00Z alondrad: tarea-1 started\n"})) }));
    assert!(drawn(&mut app, 120, 40).contains("2026-10-27T10:00:00Z alondrad: tarea-1 started"));
    press(&mut app, KeyCode::Esc);
    assert!(app.crash_report.as_ref().unwrap().viewing.is_none(), "Esc goes back to the report");

    press(&mut app, KeyCode::Char('n'));
    let screen = drawn(&mut app, 120, 40);
    assert!(screen.contains("It stays on this Mac. To send it later: alondra report send r-20261027-0a1b2c3d"), "{screen}");
}

#[test]
fn a_file_dropped_on_what_happened_stays_text_because_the_report_attaches_nothing() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let dir = crate::test_support::Directory::new();
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("notes.txt");
    std::fs::write(&file, b"x").unwrap();
    let mut app = app();
    app.open_crash_report("other");
    assert!(!drawn(&mut app, 120, 40).contains("drop a file"), "the keys offer no attachment");
    app.on_paste(&file.display().to_string());
    let composer = app.composer.as_ref().expect("still asking what happened");
    assert!(composer.chips.is_empty(), "nothing is attached");
    assert_eq!(composer.input.text(), file.display().to_string());
}

#[test]
fn yes_sends_the_report_as_the_failure_it_was_opened_for() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    let action = alondra_core::views::Action {
        link: Some(alondra_core::views::Link::Local { local: alondra_core::views::LocalLink::CrashReport { failure: "not-resumed".into() } }),
        ..serde_json::from_value(json!({"id": "", "label": "Send crash report", "style": "secondary", "presentation": "button",
            "input": {"kind": "none"}, "attachments": false, "enabled": true})).unwrap()
    };
    app.run_view_action(&action, None, &Key("task:t-1".into()), "epoch".into());
    assert!(matches!(&app.composer, Some(c) if matches!(&c.target, Target::ReportDescription { failure } if failure == "not-resumed")));
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.crash_report.as_ref().unwrap().description, None, "«What happened?» is optional");
    app.on_msg(Msg::Report(crash_report::Answer::Built(Ok(built_report(Some("owner/reports"), true)))));
    press(&mut app, KeyCode::Char('y'));
    assert!(matches!(app.crash_report.as_ref().unwrap().stage, crash_report::Stage::Sending(_)));
    assert!(drawn(&mut app, 120, 40).contains("Sending it to owner/reports…"));
    app.on_msg(Msg::Report(crash_report::Answer::Sent { id: "r-20261027-0a1b2c3d".into(),
        result: Ok(json!({"state": "sent", "message": "Sent: https://github.com/owner/reports/issues/4."})) }));
    assert!(drawn(&mut app, 120, 40).contains("Sent: https://github.com/owner/reports/issues/4."));
}

#[test]
fn a_report_with_nowhere_to_go_or_something_private_offers_no_send() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    for (repo, passed, says) in [(None, true, "This Mac knows no report repository, so it stays here. To save it as a file: alondra report save r-20261027-0a1b2c3d"),
        (Some("owner/reports"), false, "The report cannot leave this Mac; the final check found:")] {
        let mut app = app();
        app.open_crash_report("session-ended");
        press(&mut app, KeyCode::Enter);
        app.on_msg(Msg::Report(crash_report::Answer::Built(Ok(built_report(repo, passed)))));
        let screen = drawn(&mut app, 140, 40);
        assert!(screen.contains(says), "{screen}");
        assert!(!screen.contains("Yes, send it") && !screen.contains("Send it to"), "{screen}");
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(app.crash_report.as_ref().unwrap().stage, crash_report::Stage::Ready(_)), "y sends nothing");
    }
}

#[test]
fn settings_open_a_crash_report_as_other() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = mcp_servers_open();
    assert!(drawn(&mut app, 140, 60).contains("Send a crash report"));
    select(&mut app, "Send a crash report");
    press(&mut app, KeyCode::Enter);
    assert!(matches!(&app.composer, Some(c) if matches!(&c.target, Target::ReportDescription { failure } if failure == "other")));
}

#[test]
fn tuicr_is_not_installed_only_when_it_is_not_found_and_another_launch_failure_shows_as_it_is() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let root = crate::test_support::Directory::new();
    std::fs::create_dir_all(&*root).unwrap();
    let blocked = root.join("tuicr");
    std::fs::write(&blocked, "#!/bin/sh\n").unwrap();
    let launch = |program: &std::path::Path| {
        crate::pty::run_tool(program.to_str().unwrap(), &["--stdout"], &root).unwrap_err()
    };
    let shown = |error: anyhow::Error| {
        let mut app = app();
        app.reviewed(Key("i-1".into()), PrId("pr-1".into()), 7, Err(error));
        app.flash.map(|flash| flash.text).unwrap_or_default()
    };
    let without_permission = shown(launch(&blocked));
    assert!(!without_permission.contains("not installed"), "{without_permission}");
    assert!(without_permission.starts_with("tuicr: could not launch"), "{without_permission}");
    assert_eq!(shown(launch(&root.join("missing/tuicr"))), "tuicr is not installed: brew install tuicr");
}

const FORM_CARD: &str = "a98d034b-249e-4487-a89b-57de38c26372";

fn app_on_a_form_card() -> App {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = app();
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(super::parity::view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    let mut card = super::parity::view_fixture("card-elicitation-form-ios.json");
    card["key"] = json!(format!("card:{FORM_CARD}"));
    card["sections"][0]["items"][0]["id"] = json!(FORM_CARD);
    card["sections"][0]["items"][0]["form"]["fields"].as_array_mut().unwrap().push(json!(
        {"name": "calendar", "title": "Calendar", "required": true, "kind": {"kind": "choice", "options": [
            {"value": "work", "title": "Work"}, {"value": "home", "title": "Home"}]}}
    ));
    app.sel = Some(Key(FORM_CARD.into()));
    app.on_msg(Msg::ViewCard { key: format!("card:{FORM_CARD}"), result: Ok(serde_json::from_value(card).unwrap()) });
    app
}

fn type_text(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

#[test]
fn accepting_a_card_with_a_form_fills_it_by_field_and_sends_typed_values() {
    let mut app = app_on_a_form_card();
    let posted = |app: &App| app.posted.borrow().iter().filter(|(path, _)| path == "/actions").count();
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(posted(&app), 0, "Accept opens the form instead of answering");
    let screen = drawn(&mut app, 140, 50);
    assert!(screen.contains("The server asks you to fill in:"), "{screen}");
    assert!(screen.contains("whole number · at least 1"), "{screen}");
    assert!(screen.contains("Remember me *"), "{screen}");

    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "5 min");
    press(&mut app, KeyCode::Enter);
    assert!(drawn(&mut app, 140, 50).contains("minutes takes a whole number"));
    for _ in 0..4 {
        press(&mut app, KeyCode::Backspace);
    }
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Char(' '));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app), 0);
    assert!(drawn(&mut app, 140, 50).contains("Fill in Calendar"), "a required field left empty is named");
    press(&mut app, KeyCode::Up);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Enter);
    assert!(drawn(&mut app, 140, 50).contains("Home"));
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);

    let (_, request) = app.posted.borrow().last().cloned().unwrap();
    assert_eq!(request["action_id"], "ans:i-1@1");
    assert_eq!(request["option_id"], "accept");
    assert_eq!(request["input"], json!({"content": {"minutes": 5, "remember": true, "calendar": "home"}}));
    assert!(app.elicitation.is_none());
}

#[test]
fn a_form_value_outside_its_limits_is_not_sent_and_esc_goes_back_to_the_buttons() {
    let mut app = app_on_a_form_card();
    press(&mut app, KeyCode::Char('1'));
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "0");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(drawn(&mut app, 140, 50).contains("minutes is outside its limits"));
    assert!(app.posted.borrow().iter().all(|(path, _)| path != "/actions"));
    press(&mut app, KeyCode::Esc);
    assert!(app.elicitation.is_none());
    assert!(drawn(&mut app, 140, 50).contains(" 2  Decline"));
}

#[test]
fn declining_a_card_with_a_form_answers_at_once_without_values() {
    let mut app = app_on_a_form_card();
    press(&mut app, KeyCode::Char('2'));
    assert!(app.elicitation.is_none());
    let (_, request) = app.posted.borrow().last().cloned().unwrap();
    assert_eq!(request["option_id"], "decline");
    assert!(request["input"].get("content").is_none());
}
