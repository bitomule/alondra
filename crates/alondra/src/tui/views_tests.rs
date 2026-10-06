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
    assert!(app.flash.as_ref().is_some_and(|f| f.error));
}

#[test]
fn the_switch_key_asks_for_the_next_mac_unless_typing() {
    let mut app = app();
    let key = KeyEvent::new(KeyCode::Char(']'), KeyModifiers::NONE);
    assert!(matches!(app.on_key(key), Flow::NextMac));
    app.cmdline = Some(TextInput::default());
    assert!(!matches!(app.on_key(key), Flow::NextMac));
}
