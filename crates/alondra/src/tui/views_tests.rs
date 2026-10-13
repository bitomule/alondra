use super::*;

fn app() -> App {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    App::new(tx)
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn fleet_search_types_and_pastes_into_the_views_key() {
    let mut app = app();
    app.screen = Screen::Fleet;
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
fn the_allow_always_button_says_what_it_allows_and_the_other_buttons_do_not() {
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
    assert!(under("Keep the warmer copy").is_some(), "{screen}");
    assert_eq!(under("Preserve the more conversational explanation."), None, "{screen}");
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
    app.screen = Screen::Fleet;
    press(&mut app, KeyCode::Char('a'));
    assert!(app.rules.is_some(), "a on the Fleet opens the rules");
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
