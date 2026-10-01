use super::*;

fn app() -> App {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    App::new(tx)
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

#[test]
fn changed_view_action_requires_a_new_confirmed_command() {
    let mut app = app();
    let request = json!({"protocol":1,"command_id":"old-command","data_epoch":"epoch",
        "client":"tui","lang":"en","action_id":"changes:pr@1~oldhead",
        "input":{"text":"Please fix this"}});
    let reply = view_action_result(json!({"outcome":"changed",
        "message":"The PR changed", "confirm":{"action_id":"changes:pr@2~newhead",
        "button":"Send anyway"}}))
    .unwrap();
    app.on_msg(Msg::ViewAction {
        request,
        result: Ok(reply),
    });
    assert!(app.posted.borrow().is_empty());
    press(&mut app, KeyCode::Char('y'));
    let posted = app.posted.borrow();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].0, "/actions");
    assert_eq!(posted[0].1["action_id"], "changes:pr@2~newhead");
    assert_eq!(posted[0].1["input"]["text"], "Please fix this");
    assert_ne!(posted[0].1["command_id"], "old-command");
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
fn new_work_opens_the_server_form_before_the_sidebar_arrives() {
    let mut app = app();
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("new_request")
    );
    assert!(app.composer.is_none());
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
    })));
    assert!(!app.offline);
}
