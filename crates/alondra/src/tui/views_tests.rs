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
    })));
    assert!(!app.offline);
}
