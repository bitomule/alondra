//! Views terminal contract tests against synthetic server documents.

use super::*;

pub(in crate::tui) fn view_fixture(name: &str) -> serde_json::Value {
    let path = match std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR")) {
        Ok(root) => std::path::PathBuf::from(root)
            .join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()))
            .join("fixtures/views")
            .join(name),
        Err(_) => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/views")
            .join(name),
    };
    serde_json::from_str(
        &std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{}: {error}", path.display())),
    )
    .unwrap()
}

fn posted(app: &App) -> Vec<(String, serde_json::Value)> {
    app.posted.borrow().clone()
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn typed(app: &mut App, text: &str) {
    for character in text.chars() {
        press(app, KeyCode::Char(character));
    }
}

fn flash(app: &App) -> &str {
    app.flash.as_ref().map_or("", |flash| flash.text.as_str())
}

fn question_id() -> String {
    view_fixture("card-question.json")["sections"][0]["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn screen(app: &mut App, width: u16, height: u16) -> String {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn changed_view_action_requires_a_fresh_explicit_confirmation() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let original = crate::client::Client::action_body(
        "epoch",
        "changes:pr@1-old",
        None,
        serde_json::json!({"text":"Review notes"}),
    );
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "message":"New commits", "confirm":{
            "action_id":"changes:pr@2-new", "button":"Send anyway"
        }
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction {
        request: original.clone(),
        result: Ok(changed),
    });
    assert!(app.pending_changed_action.is_some());
    assert!(posted(&app).is_empty());
    press(&mut app, KeyCode::Char('y'));
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["action_id"], "changes:pr@2-new");
    assert_eq!(sent[0].1["input"], original["input"]);
    assert_ne!(sent[0].1["command_id"], original["command_id"]);
    assert!(app.pending_changed_action.is_none());
}

#[test]
fn merge_needs_two_presses() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let pr_id = "github.com/example/repo#12";
    app.sel = Some(Key(pr_id.into()));
    let key = format!("card:{pr_id}");
    let mut card = view_fixture("card-pr.json");
    card["sections"][0]["items"][0]["actions"] = serde_json::json!([{
        "id": format!("merge:{pr_id}@7~abcdef12"),
        "label": "Merge",
        "style": "primary",
        "presentation": "button",
        "input": {"kind": "none"},
        "attachments": false,
        "enabled": true,
        "confirm": {"title": "Merge", "message": "Merge this pull request?", "button": "Merge"},
        "key": "m"
    }]);
    app.on_msg(Msg::ViewCard {
        key,
        result: Ok(serde_json::from_value(card).unwrap()),
    });

    press(&mut app, KeyCode::Char('m'));
    assert!(posted(&app).is_empty(), "the first press only arms confirmation");
    press(&mut app, KeyCode::Char('m'));
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "/actions");
    assert_eq!(sent[0].1["action_id"], format!("merge:{pr_id}@7~abcdef12"));
}

#[test]
fn failed_views_feed_marks_cached_fleet_as_outdated_until_recovery() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Fleet;
    let mut fleet = view_fixture("home-tui.json");
    fleet["view"] = serde_json::json!("fleet");
    fleet["key"] = serde_json::json!("fleet");
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(fleet).unwrap())));
    app.on_msg(Msg::ViewFeed(Err("offline".into())));
    assert!(app.offline);
    assert!(screen(&mut app, 120, 26).contains("saved data · connection lost"));
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

#[test]
fn fleet_search_shows_pending_and_uses_the_typed_query() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Fleet;
    let mut fleet = view_fixture("home-tui.json");
    fleet["view"] = serde_json::json!("fleet");
    fleet["key"] = serde_json::json!("fleet");
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(fleet).unwrap())));
    press(&mut app, KeyCode::Char('/'));
    assert!(app.search.as_ref().is_some_and(|search| search.editing));
    press(&mut app, KeyCode::Char('N'));
    press(&mut app, KeyCode::Char('o'));
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("search?q=no")
    );
    assert!(screen(&mut app, 100, 24).contains("Searching"));
}

#[test]
fn fleet_search_paste_loads_the_pasted_query() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Fleet;
    press(&mut app, KeyCode::Char('/'));
    app.on_paste("Issue 42");
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("search?q=issue+42")
    );
    assert!(matches!(
        app.search.as_ref().map(|search| &search.remote),
        Some(Remote::Pending)
    ));
}

#[test]
fn view_form_checks_project_choice_and_conditional_weekday() {
    let fields: Vec<alondra_core::views::Field> = serde_json::from_value(serde_json::json!([
        {"id":"project","label":"Project","kind":{"kind":"choice","items":[{"id":"App","label":"App"}]},
         "value":"App","required":true,"reload_on_change":true},
        {"id":"repeat","label":"Repeat","kind":{"kind":"choice","items":[{"id":"daily","label":"Daily"},{"id":"weekly","label":"Weekly"}]},
         "value":"daily","required":true,"reload_on_change":false},
        {"id":"weekday","label":"Weekday","kind":{"kind":"stepper","min":0,"max":6},
         "value":null,"required":true,"visible_when":{"field":"repeat","equals":"weekly"},"reload_on_change":false}
    ])).unwrap();
    assert!(
        validate_view_fields(
            &fields,
            &serde_json::json!({"project":"App","repeat":"daily"})
        )
        .is_ok()
    );
    assert!(
        validate_view_fields(
            &fields,
            &serde_json::json!({"project":"Other","repeat":"daily"})
        )
        .is_err()
    );
    assert!(
        validate_view_fields(
            &fields,
            &serde_json::json!({"project":"App","repeat":"weekly"})
        )
        .is_err()
    );
    assert!(
        validate_view_fields(
            &fields,
            &serde_json::json!({"project":"App","repeat":"weekly","weekday":2})
        )
        .is_ok()
    );
}

#[test]
fn views_home_fixture_renders_server_rows_and_selected_card() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture(
        "home-tui.json",
    ))
    .unwrap())));
    app.reconcile();
    let rows = app
        .inbox_keys()
        .into_iter()
        .map(|key| key.0)
        .collect::<Vec<_>>();
    assert_eq!(
        rows,
        [
            "a98d034b-249e-4487-a89b-57de38c26372",
            "hf-weekly-2026-09-30",
            "github.com/example/repo#12",
            "8fa4034b-249e-4487-a89b-57de38c26372",
            "cc2d034b-249e-4487-a89b-57de38c26372",
            "f12d034b-249e-4487-a89b-57de38c26372"
        ]
    );
    let list = screen(&mut app, 120, 26);
    for label in [
        "WAITING FOR YOU",
        "PULL REQUESTS",
        "TO READ",
        "Restore the native review experience",
    ] {
        assert!(list.contains(label), "{label}: {list}");
    }

    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(serde_json::from_value(view_fixture("card-question.json")).unwrap()),
    });
    let detail = screen(&mut app, 120, 26);
    assert!(
        detail.contains("The new empty state is ready.") && detail.contains("Need more context"),
        "{detail}"
    );
}

#[test]
fn fleet_node_view_renders_facts_activity_and_actions() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Fleet;
    let id = "550e8400-e29b-41d4-a716-446655440000";
    let mut fleet = view_fixture("home-tui.json");
    fleet["view"] = serde_json::json!("fleet");
    fleet["key"] = serde_json::json!("fleet");
    fleet["sections"] = serde_json::json!([{
        "id": "nodes", "title": "App", "collapsed": false, "items": [{
            "type": "row", "id": id, "icon": "node", "title": "Instance A",
            "subtitle": "Working", "depth": 0, "struck": false,
            "link": {"kind": "view", "key": format!("node:{id}")}
        }]
    }]);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(fleet).unwrap())));
    app.sel = Some(Key(id.into()));
    let mut node = view_fixture("card-question.json");
    node["view"] = serde_json::json!("node");
    node["key"] = serde_json::json!(format!("node:{id}"));
    node["title"] = serde_json::json!("Instance A");
    node["sections"] = serde_json::json!([
        {"id": "header", "collapsed": false, "items": [{
            "type": "facts", "items": [
                {"label": "State", "value": "Working"},
                {"label": "Tasks", "value": "2"}
            ]
        }]},
        {"id": "activity", "title": "Activity", "collapsed": false, "items": [{
            "type": "row", "id": "activity-1", "icon": "history",
            "title": "Running the suite", "depth": 0, "struck": false
        }]},
        {"id": "actions", "collapsed": false, "items": [{
            "type": "actions", "actions": [{
                "id": format!("msg:{id}@1"), "label": "Message",
                "style": "primary", "presentation": "button",
                "input": {"kind": "text", "placeholder": "Write to Instance A…", "required": true, "multiline": true},
                "attachments": true, "enabled": true, "key": "m"
            }]
        }]}
    ]);
    app.on_msg(Msg::ViewCard {
        key: format!("node:{id}"),
        result: Ok(serde_json::from_value(node).unwrap()),
    });

    let shown = screen(&mut app, 150, 35);
    for text in [
        "Instance A",
        "State: Working",
        "Tasks: 2",
        "Running the suite",
        "Message",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
    press(&mut app, KeyCode::Char('m'));
    typed(&mut app, "Status?");
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app)[0].0, "/actions");
    assert_eq!(posted(&app)[0].1["action_id"], format!("msg:{id}@1"));
    assert_eq!(posted(&app)[0].1["input"]["text"], "Status?");
}

#[test]
fn fleet_search_key_opens_the_server_search_view() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Fleet;
    let mut fleet = view_fixture("home-tui.json");
    fleet["view"] = serde_json::json!("fleet");
    fleet["key"] = serde_json::json!("fleet");
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(fleet).unwrap())));

    press(&mut app, KeyCode::Char('/'));

    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("search")
    );
    assert_eq!(app.card_view_asked.as_deref(), Some("search"));
}

#[test]
fn views_mode_schedules_loads_the_server_screen() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.home_view = Some(serde_json::from_value(view_fixture("home-tui.json")).unwrap());

    press(&mut app, KeyCode::Char('s'));

    assert!(app.schedules_open);
    assert_eq!(app.screen_view_asked.as_deref(), Some("schedules"));
    assert!(app.home_view.is_none());
}

#[test]
fn schedule_row_keys_post_the_selected_views_action() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.schedules_open = true;
    let mut view = view_fixture("home-tui.json");
    view["key"] = serde_json::json!("schedules");
    view["sections"] = serde_json::json!([{
        "id": "schedules", "collapsed": false, "items": [{
            "type": "row", "id": "hf-weekly-2026-09-30",
            "link": {"kind": "view", "key": "schedule:hf-weekly-2026-09-30"},
            "icon": "schedule", "title": "Weekly", "depth": 0, "struck": false,
            "actions": [{
                "id": "sched_run:s-hf-weekly-2026-09-30@1", "label": "Run Now",
                "style": "secondary", "presentation": "button", "input": {"kind": "none"},
                "attachments": false, "enabled": true, "key": "enter"
            }]
        }]
    }]);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view).unwrap())));
    let rendered = screen(&mut app, 100, 24);
    assert!(
        rendered.contains("Weekly") && rendered.contains("Run Now"),
        "{rendered}"
    );
    press(&mut app, KeyCode::Enter);
    let requests = posted(&app);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1["action_id"],
        "sched_run:s-hf-weekly-2026-09-30@1"
    );
    assert_eq!(requests[0].1["data_epoch"], "test-epoch");
}

#[test]
fn global_message_key_opens_the_orchestrator_views_composer() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    press(&mut app, KeyCode::Char('M'));
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("node:orchestrator")
    );
    let mut view = view_fixture("card-question.json");
    view["key"] = serde_json::json!("node:orchestrator");
    view["sections"][0]["items"][0]["actions"] = serde_json::json!([{
        "id": "msg:view@1", "label": "Message", "style": "primary",
        "presentation": "button", "input": {"kind": "text", "placeholder": "Write", "required": true, "multiline": true},
        "attachments": true, "enabled": true, "key": "M"
    }]);
    app.on_msg(Msg::ViewCard {
        key: "node:orchestrator".into(),
        result: Ok(serde_json::from_value(view).unwrap()),
    });
    assert!(
        matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { action_id, .. }) if action_id == "msg:view@1")
    );
}

#[test]
fn reader_review_and_read_keys_use_views_actions() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let mut view = view_fixture("card-question.json");
    let id = question_id();
    view["sections"][0]["items"][0]["actions"] = serde_json::json!([
        {"id":format!("changes:{id}@1"), "label":"Request changes", "style":"secondary",
         "presentation":"button", "input":{"kind":"text","placeholder":"Describe", "required":true,"multiline":true},
         "attachments":true,"enabled":true,"key":"s"},
        {"id":format!("read:{id}@1"), "label":"Mark as read", "style":"secondary",
         "presentation":"button", "input":{"kind":"none"},
         "attachments":false,"enabled":true,"key":"x"}
    ]);
    app.view_navigation.push(format!("card:{id}"));
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    assert!(app.reader.is_some());
    press(&mut app, KeyCode::Char('s'));
    assert!(
        matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { action_id, .. }) if action_id == &format!("changes:{id}@1"))
    );
    app.composer = None;
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(posted(&app)[0].1["action_id"], format!("read:{id}@1"));
}

#[test]
fn canonical_pr_diff_content_ref_opens_in_the_views_reader() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let key = "pull_request:github.com/bitomule/alondra#154";
    let route = "/pull-requests/github.com%2Fbitomule%2Falondra%23154/diff";
    let mut view = view_fixture("card-question.json");
    view["key"] = serde_json::json!(key);
    view["sections"] = serde_json::json!([{
        "id":"diff", "collapsed":false, "items":[{
            "type":"content_ref", "kind":"diff", "title":"Diff", "size":0,
            "digest":"head-154", "fetch":route, "commentable":true
        }]
    }]);
    app.view_navigation.push(key.into());
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    assert!(app.loading.contains(route));
    app.on_msg(Msg::File {
        route: route.into(),
        result: Ok(files::FileBody::Text("diff --git a/a b/a\n+fixed".into())),
    });
    let shown = screen(&mut app, 120, 30);
    assert!(shown.contains("fixed"), "{shown}");
    let cut = diff_reader_text(
        crate::client::PullRequestDiff {
            text: "partial diff".into(),
            truncated: true,
        },
        "Diff truncated",
    );
    assert!(cut.contains("partial diff\n\nDiff truncated"));
}

#[test]
fn diff_card_footer_names_the_diff_and_comments_shortcut() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut view = view_fixture("card-question.json");
    view["key"] = serde_json::json!("pull_request:owner/repo#1");
    view["sections"] = serde_json::json!([{"id":"diff", "collapsed":false, "items":[{
        "type":"content_ref", "kind":"diff", "title":"Diff", "size":0,
        "digest":"head-1", "fetch":"/diff", "commentable":true
    }]}]);
    let rendered: alondra_core::views::View = serde_json::from_value(view).unwrap();
    let keys = crate::tui::draw::view_footer_actions(&rendered);
    assert!(keys.iter().any(|(key, label)| key == "d" && label == "diff and comments"));
}

#[test]
fn view_footer_deduplicates_actions_and_names_merge_methods_for_people() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut view = view_fixture("card-question.json");
    view["sections"] = serde_json::json!([{
        "id":"actions", "collapsed":false, "items":[{
            "type":"actions", "actions":[
                {"id":"ask:one", "label":"Ask about the PR", "style":"secondary",
                 "presentation":"button", "input":{"kind":"none"}, "attachments":false,
                 "enabled":true, "key":"p"},
                {"id":"ask:two", "label":"Ask about the PR", "style":"secondary",
                 "presentation":"button", "input":{"kind":"none"}, "attachments":false,
                 "enabled":true, "key":"p"},
                {"id":"merge:one", "label":"Merge", "style":"primary",
                 "presentation":"menu", "input":{"kind":"none"}, "attachments":false,
                 "enabled":true, "key":"m", "options":[
                    {"id":"squash", "label":"squash", "style":"secondary",
                     "input":{"kind":"none"}},
                    {"id":"merge", "label":"merge", "style":"secondary",
                     "input":{"kind":"none"}}
                 ]}
            ]
        }]
    }]);
    let rendered: alondra_core::views::View = serde_json::from_value(view).unwrap();
    let keys = crate::tui::draw::view_footer_actions(&rendered);
    assert_eq!(keys.iter().filter(|(_, label)| label == "Ask about the PR").count(), 1);
    assert!(keys.iter().any(|(key, label)| key == "1" && label == "Squash merge"));
    assert!(keys.iter().any(|(key, label)| key == "2" && label == "Merge commit"));
}

#[test]
fn d_on_a_selected_delivery_opens_the_views_diff_without_a_clone() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let pr_id = "github.com/bitomule/alondra#154";
    let key = format!("pull_request:{pr_id}");
    let route = "/pull-requests/github.com%2Fbitomule%2Falondra%23154/diff";
    let mut delivery = view_fixture("card-question.json");
    delivery["key"] = serde_json::json!("card:delivery-1");
    delivery["sections"][0]["items"][0]["actions"] = serde_json::json!([{
        "id":"", "label":"Review local clone", "style":"plain", "presentation":"button",
        "input":{"kind":"none"}, "attachments":false, "enabled":true, "key":"d",
        "link":{"kind":"local", "local":{"kind":"review_clone",
            "repo":"owner/alondra-no-clone-fixture-999", "branch":"head-154", "pr":154,
            "pr_id":pr_id}}
    }]);
    app.view_navigation.push("card:delivery-1".into());
    app.card_view = Some(serde_json::from_value(delivery).unwrap());
    press(&mut app, KeyCode::Char('d'));
    assert_eq!(app.view_navigation.last(), Some(&key));
    assert_eq!(app.reader_after_view.as_deref(), Some(key.as_str()));

    let mut detail = view_fixture("card-question.json");
    detail["key"] = serde_json::json!(key);
    detail["sections"] = serde_json::json!([{"id":"diff", "collapsed":false, "items":[{
        "type":"content_ref", "kind":"diff", "title":"Diff", "size":0,
        "digest":"head-154", "fetch":route, "commentable":true
    }]}]);
    app.on_msg(Msg::ViewCard {
        key,
        result: Ok(serde_json::from_value(detail).unwrap()),
    });
    assert!(app.reader.is_some() && app.loading.contains(route));
}

#[test]
fn reader_comments_follow_the_selected_views_text_option() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let mut view = view_fixture("card-question.json");
    view["sections"][0]["items"][0]["actions"][0]["options"] = serde_json::json!([
        {"id":"changes", "label":"Request changes", "style":"secondary",
         "input":{"kind":"text", "placeholder":"Explain", "required":true,"multiline":true}, "key":"1"},
        {"id":"approve", "label":"Approve", "style":"primary",
         "input":{"kind":"none"}, "key":"2"}
    ]);
    app.view_navigation.push(format!("card:{}", question_id()));
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    press(&mut app, KeyCode::Char('C'));
    typed(&mut app, "Tighter");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('2'));
    assert!(posted(&app).is_empty() && flash(&app).contains("takes no text"));
    press(&mut app, KeyCode::Char('1'));
    assert!(
        matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { option_id: Some(option), .. }) if option == "changes")
    );
    assert!(
        app.composer
            .as_ref()
            .is_some_and(|composer| composer.input.text().contains("Tighter"))
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app)[0].1["option_id"], "changes");
    assert!(
        posted(&app)[0].1["input"]["text"]
            .as_str()
            .unwrap()
            .contains("Tighter")
    );
}

#[test]
fn reader_comments_are_sent_as_anchored_comments() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let mut view = view_fixture("card-question.json");
    view["sections"][0]["items"][0]["actions"][0]["options"] = serde_json::json!([
        {"id":"changes", "label":"Request changes", "style":"secondary",
         "input":{"kind":"text", "placeholder":"Explain", "required":true,"multiline":true}, "key":"1"}
    ]);
    app.view_navigation.push(format!("card:{}", question_id()));
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    press(&mut app, KeyCode::Char('c'));
    typed(&mut app, "Tighter");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('1'));
    let anchored = "1. On \"Restore the native review experience\":\n   Tighter";
    assert!(app.composer.as_ref().is_some_and(|composer| composer.input.text().contains(anchored)));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent[0].1["option_id"], "changes");
    assert!(sent[0].1["input"]["text"].as_str().unwrap().contains(anchored));
}

#[test]
fn number_keys_answer_with_the_option_id() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture(
        "home-tui.json",
    ))
    .unwrap())));
    app.reconcile();
    let mut card = view_fixture("card-question.json");
    card["sections"][0]["items"][0]["actions"][0]["options"] = serde_json::json!([
        {"id": "first", "label": "First", "style": "secondary", "input": {"kind": "none"}, "key": "1"},
        {"id": "keep-existing", "label": "Keep existing", "style": "primary", "input": {"kind": "none"}, "key": "2"}
    ]);
    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(serde_json::from_value(card).unwrap()),
    });

    press(&mut app, KeyCode::Char('2'));

    let requests = posted(&app);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/actions");
    assert_eq!(requests[0].1["option_id"], "keep-existing");
    assert_eq!(
        requests[0].1["action_id"],
        format!("ans:{}@1", question_id())
    );
    assert_eq!(requests[0].1["data_epoch"], "test-epoch");
    assert_eq!(requests[0].1["input"], serde_json::json!({}));
}

#[test]
fn task_detail_view_is_loaded_and_its_action_posts_with_the_document_epoch() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.task_sel = Some(tasks::TaskKey::Task(TaskId("t-1".into())));
    let mut card = view_fixture("card-question.json");
    card["key"] = serde_json::json!("task:t-1");
    card["sections"][0]["items"][0]["actions"][0]["key"] = serde_json::json!("z");
    card["sections"][0]["items"][0]["actions"][0]["input"] = serde_json::json!({"kind": "none"});
    app.on_msg(Msg::ViewCard {
        key: "task:t-1".into(),
        result: Ok(serde_json::from_value(card).unwrap()),
    });

    assert_eq!(
        app.card_view.as_ref().map(|view| view.key.as_str()),
        Some("task:t-1")
    );
    press(&mut app, KeyCode::Char('z'));

    let requests = posted(&app);
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].1["action_id"],
        format!("ans:{}@1", question_id())
    );
    assert_eq!(requests[0].1["data_epoch"], "test-epoch");
}

fn form_msg(view: &str, fixture: &str) -> Msg {
    Msg::FormView {
        view: view.into(),
        project: None,
        result: Ok(serde_json::from_value(view_fixture(fixture)).unwrap()),
    }
}

#[test]
fn n_opens_the_new_work_composer_with_the_servers_rows() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(
        app.composer.as_ref().map(|c| &c.target),
        Some(Target::Form { view }) if view == "new_request"
    ));
    assert!(app.view_navigation.is_empty(), "the form is the composer, not a view to edit elsewhere");
    assert!(screen(&mut app, 100, 24).contains("Loading the form…"));

    app.on_msg(form_msg("new_request", "new-request-ios.json"));
    let shown = screen(&mut app, 100, 24);
    for expected in [
        "New work", "Project", "The orchestrator picks the project", "Issue", "none", "PRs",
        "Review each one", "Effort", "Auto", "Tab",
    ] {
        assert!(shown.contains(expected), "{expected} missing from {shown}");
    }
}

#[test]
fn new_work_sends_the_typed_text_and_the_chosen_rows_by_action_id() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    press(&mut app, KeyCode::Char('n'));
    app.on_msg(form_msg("new_request", "new-request-ios.json"));
    typed(&mut app, "Fix the crash");
    for code in [
        KeyCode::Tab, KeyCode::Down, KeyCode::Down, KeyCode::Enter, KeyCode::Down, KeyCode::Enter,
        KeyCode::Esc,
    ] {
        press(&mut app, code);
    }
    assert!(screen(&mut app, 100, 24).contains("Auto-merge"));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "/actions");
    assert_eq!(sent[0].1["action_id"], "new_request:view@0");
    assert_eq!(sent[0].1["data_epoch"], "test-epoch");
    assert_eq!(
        sent[0].1["input"],
        serde_json::json!({"fields": {
            "text": "Fix the crash", "project": "", "issue": "", "pr": "auto_merge", "effort": "auto"
        }})
    );
}

#[test]
fn new_work_without_text_stays_open_and_says_what_is_missing() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    press(&mut app, KeyCode::Char('n'));
    app.on_msg(form_msg("new_request", "new-request-ios.json"));
    press(&mut app, KeyCode::Enter);
    assert!(posted(&app).is_empty());
    assert!(app.composer.is_some());
    assert!(flash(&app).contains("What do you want done?"), "{}", flash(&app));
}

#[test]
fn schedule_create_is_a_composer_whose_rows_follow_the_repeat() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.schedules_open = true;
    app.home_view = Some(serde_json::from_value(view_fixture("schedules-ios.json")).unwrap());
    press(&mut app, KeyCode::Char('n'));
    assert!(matches!(
        app.composer.as_ref().map(|c| &c.target),
        Some(Target::Form { view }) if view == "schedule:new"
    ));
    app.on_msg(form_msg("schedule:new", "schedule-new-ios.json"));
    let shown = screen(&mut app, 100, 30);
    for expected in ["Save Schedule", "Name", "Repeat", "Daily", "Time", "09:00", "Active", "yes"] {
        assert!(shown.contains(expected), "{expected} missing from {shown}");
    }
    assert!(!shown.contains("Weekday"));

    typed(&mut app, "Check the beta");
    press(&mut app, KeyCode::Enter);
    assert!(posted(&app).is_empty());
    assert!(flash(&app).contains("Name"), "{}", flash(&app));

    for code in [KeyCode::Tab, KeyCode::Down, KeyCode::Enter] {
        press(&mut app, code);
    }
    typed(&mut app, "Beta check");
    for code in [KeyCode::Enter, KeyCode::Esc, KeyCode::Enter] {
        press(&mut app, code);
    }
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["input"]["fields"]["name"], "Beta check");
    assert_eq!(sent[0].1["input"]["fields"]["mission"], "Check the beta");
    assert_eq!(sent[0].1["input"]["fields"]["time"], "09:00");
    assert_eq!(sent[0].1["input"]["fields"]["active"], true);
    assert!(sent[0].1["input"]["fields"].get("weekday").is_none());
}

#[test]
fn enter_on_a_form_view_opens_it_as_a_composer_not_an_external_editor() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.task_sel = Some(tasks::TaskKey::Task(TaskId("t-1".into())));
    let mut card = view_fixture("new-request-ios.json");
    card["key"] = serde_json::json!("task:t-1");
    app.on_msg(Msg::ViewCard {
        key: "task:t-1".into(),
        result: Ok(serde_json::from_value(card).unwrap()),
    });
    assert!(matches!(app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)), Flow::Go));
    assert!(matches!(
        app.composer.as_ref().map(|c| &c.target),
        Some(Target::Form { view }) if view == "task:t-1"
    ));
}

#[test]
fn view_url_links_only_open_https_github_hosts() {
    assert!(inbox::safe_github_url(
        "https://github.com/bitomule/alondra"
    ));
    assert!(inbox::safe_github_url("https://docs.github.com/en"));
    for url in [
        "http://github.com/bitomule/alondra",
        "https://github.com.evil.test/",
        "https://evil.test@github.com/",
        "file:///etc/passwd",
    ] {
        assert!(!inbox::safe_github_url(url), "{url}");
    }
}
#[test]
fn tuicr_comments_use_the_views_changes_action() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let pr = "github.com/bitomule/alondra#154";
    let key = format!("pull_request:{pr}");
    let mut view = view_fixture("card-question.json");
    view["key"] = serde_json::json!(key);
    view["sections"][0]["items"][0]["actions"] = serde_json::json!([{
        "id": format!("changes:{pr}@1~abcdef12"),
        "label": "Request changes", "style": "secondary", "presentation": "button",
        "input": {"kind": "text", "placeholder": "Describe the changes", "required": false, "multiline": true},
        "attachments": true, "enabled": true
    }]);
    app.view_navigation.push(key.clone());
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.reviewed(
        Key("interaction:delivery-1".into()),
        PrId(pr.into()),
        154,
        Ok("Reviewed.\n".into()),
    );
    assert!(posted(&app).is_empty() && flash(&app).contains("without comments"));
    app.reviewed(
        Key("interaction:delivery-1".into()),
        PrId(pr.into()),
        154,
        Ok("1. `src/a.rs:4` - rename it\n".into()),
    );
    let requests = posted(&app);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/actions");
    assert_eq!(requests[0].1["data_epoch"], "test-epoch");
    assert_eq!(
        requests[0].1["action_id"],
        format!("changes:{pr}@1~abcdef12")
    );
    assert_eq!(
        requests[0].1["input"]["text"],
        "1. `src/a.rs:4` - rename it"
    );
}

#[test]
fn review_clone_flow_keeps_delivery_selection_and_uses_canonical_pr_id() {
    let selected = Key("interaction:delivery-1".into());
    let dir = std::path::PathBuf::from("/private/tmp/synthetic-clone");
    let flow = inbox::review_clone_flow(
        &selected,
        "github.com/bitomule/alondra#154",
        154,
        dir.clone(),
    )
    .unwrap();
    assert!(
        matches!(flow, Flow::Review { key, pr, number: 154, dir: path }
        if key == selected && pr.0 == "github.com/bitomule/alondra#154" && path == dir)
    );
    assert!(inbox::review_clone_flow(&selected, "../wrong", 154, dir).is_none());
}

#[test]
fn schedules_screen_has_its_own_footer_with_the_create_hint() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.schedules_open = true;
    app.home_view = Some(serde_json::from_value(view_fixture("schedules-ios.json")).unwrap());

    let keys = crate::tui::draw::footer_keys(&app);

    assert!(keys.iter().any(|(key, label)| key == "n" && label == "create"));
    assert!(keys.iter().any(|(key, label)| key == "Esc" && label == "back"));
    assert!(!keys.iter().any(|(key, _)| key == "Shift+A" || key == "Tab"));
}

#[test]
fn detail_hides_the_disabled_accept_action() {
    let mut view = view_fixture("card-delivery.json");
    let actions = view["sections"][0]["items"][0]["actions"].as_array().unwrap();
    assert!(actions.iter().any(|action| action["id"].as_str().is_some_and(|id| id.starts_with("accept:"))
        && action["enabled"] == serde_json::json!(false)));
    let document: alondra_core::views::View = serde_json::from_value(view.take()).unwrap();

    let visible = crate::tui::draw::visible_detail_actions(&document);

    assert!(!visible.iter().any(|action| action.id.starts_with("accept:")));
    assert!(visible.iter().any(|action| action.id.starts_with("reply:")));
}

fn question_card() -> alondra_core::views::View {
    let mut card = view_fixture("card-question.json");
    card["sections"][0]["items"][0]["timestamps"] =
        serde_json::json!([{"role": "created", "at": "2026-09-30T09:58:00Z", "format": "relative"}]);
    serde_json::from_value(card).unwrap()
}

fn text_of(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

#[test]
fn card_detail_says_the_title_once_under_a_kind_agent_and_age_header() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let card = question_card();
    let lines = text_of(&crate::tui::view_render::document_lines(&card, 80));
    let header = &lines[0];
    assert!(
        header.starts_with("? Question · Alondra · iOS · Demo Garden · ") && header.ends_with(" ago"),
        "{header}"
    );
    assert_eq!(lines[1], "Restore the native review experience");
    assert_eq!(
        lines.iter().filter(|line| line.contains("Restore the native review experience")).count(),
        1,
        "{lines:#?}"
    );
    assert!(lines.iter().any(|line| line == "Mission · Polish the Alondra beta"));
}

#[test]
fn detail_pane_is_titled_with_the_kind_of_the_card() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(question_card()),
    });
    let shown = screen(&mut app, 120, 26);
    assert!(shown.contains(" question "), "{shown}");
    assert!(!shown.contains(" Detail "), "{shown}");
}

#[test]
fn reader_header_names_the_real_kind_and_the_agent() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let id = question_id();
    app.view_navigation.push(format!("card:{id}"));
    app.card_view = Some(question_card());
    app.open_reader();
    let shown = screen(&mut app, 120, 20);
    assert!(
        shown.contains("? question · Alondra · iOS · Restore the native review experience"),
        "{shown}"
    );
    assert!(!shown.contains("item"), "{shown}");
}

#[test]
fn before_the_first_answer_the_screen_says_it_is_connecting_or_not_answering() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let shown = screen(&mut app, 100, 20);
    assert!(shown.contains("Connecting to alondrad…") && shown.contains(" Inbox "), "{shown}");
    app.offline = true;
    assert!(screen(&mut app, 100, 20).contains("alondrad is not responding · retrying…"));
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    assert!(screen(&mut app, 100, 20).contains("alondrad no responde · reintentando…"));
}

#[test]
fn an_empty_inbox_says_all_caught_up_and_offers_the_next_step() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let mut home = view_fixture("home-tui.json");
    home["sections"] = serde_json::json!([]);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(home).unwrap())));
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let shown = screen(&mut app, 100, 20);
    for expected in [
        "✓ All caught up", "Nobody is waiting for you and there is",
        "n order new work", "Tab see the tasks",
    ] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let shown = screen(&mut app, 100, 20);
    assert!(shown.contains("✓ Todo al día") && shown.contains("Tab mira las tareas"), "{shown}");
}

#[test]
fn footers_name_the_next_screen_the_pane_key_and_the_terminal() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let has = |app: &App, key: &str, label: &str| {
        crate::tui::draw::footer_keys(app).iter().any(|(k, l)| k == key && l == label)
    };
    app.home_view = Some(serde_json::from_value(view_fixture("home-tui.json")).unwrap());
    assert!(has(&app, "Tab", "tasks") && has(&app, "^d/^u", "scroll detail") && has(&app, "o", "expand"));
    app.screen = Screen::Tasks;
    app.home_view = Some(serde_json::from_value(view_fixture("tasks-ios.json")).unwrap());
    assert!(has(&app, "h/l", "pane") && has(&app, "⏎", "its terminal") && has(&app, "Tab", "fleet"));
    app.screen = Screen::Fleet;
    app.home_view = Some(serde_json::from_value(view_fixture("fleet-ios.json")).unwrap());
    assert!(has(&app, "h/l", "pane") && has(&app, "⏎", "its terminal (← returns)") && has(&app, "Tab", "inbox"));
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    assert!(has(&app, "Tab", "bandeja") && has(&app, "⏎", "su terminal (← vuelve)"));
}

#[test]
fn a_narrow_footer_keeps_tab_new_and_quit() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.home_view = Some(serde_json::from_value(view_fixture("home-tui.json")).unwrap());
    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(question_card()),
    });
    app.sel = Some(Key(question_id()));
    let shown = screen(&mut app, 70, 20);
    let foot: String = shown.chars().rev().take(70 * 2).collect::<Vec<_>>().into_iter().rev().collect();
    assert!(foot.contains("Tab tasks") && foot.contains("n new") && foot.contains("q quit"), "{foot}");
}

#[test]
fn enter_on_a_task_or_node_opens_its_terminal() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.view_navigation.push("task:t-1".into());
    let mut task = view_fixture("card-question.json");
    task["view"] = serde_json::json!("task");
    task["key"] = serde_json::json!("task:t-1");
    task["sections"] = serde_json::json!([{"id": "attach", "collapsed": false, "items": [{
        "type": "row", "id": "attach", "icon": "node", "title": "Attach", "depth": 0, "struck": false,
        "link": {"kind": "local", "local": {"kind": "attach", "session": "short-a-1"}}
    }]}]);
    app.card_view = Some(serde_json::from_value(task).unwrap());
    assert!(matches!(
        app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        Flow::Attach(session) if session == "short-a-1"
    ));
    app.card_view = None;
    assert!(matches!(app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Flow::Go));
    assert!(flash(&app).contains("no terminal"), "{}", flash(&app));
}

fn status_with(gauges: serde_json::Value) -> alondra_core::views::Status {
    serde_json::from_value(serde_json::json!({
        "version": "1", "summary": "", "badge": 0,
        "counts": [
            {"id": "inbox", "label": "Inbox", "value": 4}, {"id": "tasks", "label": "Tasks", "value": 1},
            {"id": "agents", "label": "Fleet", "value": 1}, {"id": "working", "label": "working", "value": 2},
            {"id": "waiting", "label": "waiting", "value": 1}
        ],
        "gauges": gauges,
        "orchestrator": {"text": "idle", "tone": "neutral"}
    }))
    .unwrap()
}

fn production_gauges() -> serde_json::Value {
    serde_json::json!([
        {"id": "memory", "label": "RAM", "value": 0.42, "text": "42%", "level": "calm"},
        {"id": "cpu", "label": "CPU", "value": 0.34, "text": "34%", "level": "calm"},
        {"id": "disk", "label": "Disk", "value": 0.72, "text": "120 GB free", "level": "calm"}
    ])
}

#[test]
fn header_has_the_alondra_badge_the_bars_and_how_many_are_working() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    app.view_status = Some(status_with(production_gauges()));
    let shown = screen(&mut app, 140, 6);
    for expected in [" Alondra ", "Fleet 1", "2 working", "RAM ▰▰▱▱ 42%  CPU ▰▱▱▱ 34%", "Disk ▰▰▰▱ 120 GB free"] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
    assert!(shown.contains("34%  Disk"), "the gauges are separated: {shown}");
    assert!(!shown.contains("Local"));
}

#[test]
fn header_gauges_come_in_the_servers_language() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    app.view_status = Some(status_with(serde_json::json!([
        {"id": "disk", "label": "Disco", "value": 0.72, "text": "120 GB libres", "level": "calm"}
    ])));
    let shown = screen(&mut app, 140, 6);
    assert!(shown.contains("Flota 1") && shown.contains("Disco ▰▰▰▱ 120 GB libres"), "{shown}");
}

#[test]
fn header_drops_the_gauge_names_before_it_overflows() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    app.view_status = Some(status_with(production_gauges()));
    let shown = screen(&mut app, 92, 6);
    assert!(shown.contains("▰▰▱▱ 42% ▰▱▱▱ 34%") && !shown.contains("RAM"), "{shown}");
    let shown = screen(&mut app, 50, 6);
    assert!(!shown.contains("▰"), "{shown}");
}

#[test]
fn tasks_open_on_the_first_task_and_leave_scheduled_to_the_s_key() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-ios.json")).unwrap())));
    assert_eq!(
        app.sel.as_ref().map(|key| key.0.as_str()),
        Some("r-9f2156b5a8894e53901f0af576dff6a2")
    );
    let shown = screen(&mut app, 120, 20);
    assert!(shown.contains("Polish the Alondra beta") && shown.contains("Scheduled [s]"), "{shown}");
    assert!(!shown.contains("Scheduled Tasks"), "{shown}");
}

#[test]
fn an_empty_fleet_detail_summarises_the_fleet_from_the_servers_counts() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Fleet;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("fleet-ios.json")).unwrap())));
    app.sel = None;
    app.card_view_asked = None;
    let shown = screen(&mut app, 120, 20);
    for expected in ["The fleet right now", "0 working", "1 waiting"] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
}

#[test]
fn help_lists_scheduled_tasks_the_disk_bar_and_says_node_and_fleet() {
    for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Es] {
        crate::i18n::set_for_this_thread(lang);
        let help: String = crate::tui::draw::help_lines()
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(help.contains("RAM") && help.contains("CPU"), "{help}");
        assert!(!help.to_lowercase().contains("agent"), "{help}");
        assert!(!help.contains("sesión") && !help.contains("session"), "{help}");
    }
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let help: String = crate::tui::draw::help_lines().iter().map(|line| line.to_string()).collect();
    assert!(help.contains("scheduled tasks: n creates one") && help.contains(":fleet"));
}

fn held_plan_card() -> alondra_core::views::View {
    use alondra_core::domain_v2::*;
    let mut state = DomainState::new("e");
    let mut run = |actor: Actor, command: Command| {
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("c{}", state.revision).into(), expected_revision: state.revision, command };
        state.execute(&actor, &envelope).unwrap();
    };
    let user = || Actor::User { user_id: "u".into() };
    run(user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false });
    run(Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into() });
    run(user(), Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Build".into(),
        agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None });
    run(Actor::System, Command::HoldHook { interaction_id: "hold-1".into(), task_id: "t".into(),
        agent_id: "agent-a".into(), hold: Hold::Plan, body: "# Ship the parser\n\n1. Split the lexer".into(),
        options: Hold::Plan.options() });
    let live = alondra_core::views::LiveSnapshot::default();
    alondra_core::views::build("card:hold-1", alondra_core::views::RenderContext {
        domain: &state, live: &live, client: alondra_core::views::Client::Tui, lang: alondra_core::views::Lang::En,
        transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "",
    }).unwrap()
}

#[test]
fn a_held_plan_is_approved_with_a_and_sent_back_with_p_like_8ccd131() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let reader_on_plan = || {
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
        let mut app = App::new(tx);
        app.view_navigation.push("card:hold-1".into());
        app.card_view = Some(held_plan_card());
        app.open_reader();
        app
    };
    let mut app = reader_on_plan();
    press(&mut app, KeyCode::Char('a'));
    assert_eq!(posted(&app)[0].1["option_id"], "approve");
    let mut app = reader_on_plan();
    press(&mut app, KeyCode::Char('p'));
    assert!(matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { option_id: Some(option), .. }) if option == "request_changes"));
    typed(&mut app, "Split it in two PRs");
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app)[0].1["option_id"], "request_changes");
    assert_eq!(posted(&app)[0].1["input"]["text"], "Split it in two PRs");
}
