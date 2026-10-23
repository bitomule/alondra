use super::*;

fn app() -> App {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    App::new(tx)
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
    assert!(app.mcp_servers.is_some(), ", opens Settings › MCP servers from any screen");
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
    for shown in ["GENERAL", "PR merges", "Squash and merge · if the repository allows it", "1 Change…", "Nodes at once", "MCP SERVERS", "linear  Needs sign-in"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    assert!(screen.find("PR merges") < screen.find("linear"), "General comes first: {screen}");
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
fn the_mcp_servers_screen_lists_each_project_s_servers_and_runs_the_selected_one_s_buttons() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut app = mcp_servers_open();
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
    press(&mut app, KeyCode::Esc);
    assert!(app.mcp_servers.is_none());
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
    for shown in ["Settings", "CLAUDE ACCOUNTS", "Default  Signed in", "someone@work.example · max_20x · weight 8, set by you",
        "5 h 61 % · week 48 % · 1 node", "Personal  Signed out", "Old  Off", "Add an account", "New work", "Most room first",
        "Move a node when its account runs out  Off", "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed.",
        "1 Weight   2 Move up   3 Move down   4 Turn off", "MCP SERVERS"] {
        assert!(screen.contains(shown), "{shown}: {screen}");
    }
    assert!(screen.find("Add an account") < screen.find("linear"), "the accounts come before the MCP servers: {screen}");
    assert!(!screen.contains("Remove"), "a node runs on Work, so it can't be removed: {screen}");

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
    select(&mut app, "Add an account");
    press(&mut app, KeyCode::Char('1'));
    assert!(matches!(&app.composer, Some(c) if matches!(c.target, Target::AccountName)), "it asks only for a name");
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
