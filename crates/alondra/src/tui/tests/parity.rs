//! Views terminal contract tests against synthetic server documents.

use super::*;

fn fixtures_dir() -> std::path::PathBuf {
    views_fixtures("cards")
}

fn views_fixtures(folder: &str) -> std::path::PathBuf {
    match std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR")) {
        Ok(root) => std::path::PathBuf::from(root)
            .join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()))
            .join("fixtures/views")
            .join(folder),
        Err(_) => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/views").join(folder),
    }
}

/// The Metrics view as the daemon renders it, from `fixtures/views/metrics`.
pub(in crate::tui) fn metrics_fixture(name: &str) -> serde_json::Value {
    let path = views_fixtures("metrics").join(name);
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))).unwrap()
}

/// A view as a daemon newer than this terminal sends it, from `fixtures/views/newer-daemon`.
pub(in crate::tui) fn newer_daemon_fixture(name: &str) -> String {
    let path = views_fixtures("newer-daemon").join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

pub(in crate::tui) fn view_fixture(name: &str) -> serde_json::Value {
    let path = fixtures_dir().join(name);
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

/// The card as a view that is not a card's own shows it: without a kind, so its actions follow the
/// view's keys instead of 1-3, m and r.
fn untyped(view: &mut serde_json::Value) {
    view["view"] = serde_json::json!("task");
    view["sections"][0]["items"][0]
        .as_object_mut()
        .unwrap()
        .remove("kind");
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
fn changed_view_action_asks_in_a_modal_and_sends_the_fresh_action_on_enter() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let original = crate::client::Client::action_body(
        "epoch",
        "changes:pr@1-old",
        None,
        serde_json::json!({"text":"Review notes"}),
    );
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "message":"PR #12 has new commits",
        "result":{"kind":"changed", "title":"PR #12 has new commits",
            "body":"You reviewed an older version.",
            "next":{"id":"changes:pr@2-new", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false,
                "enabled":true}}
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction {
        request: original.clone(),
        result: Ok(changed),
    });
    assert!(posted(&app).is_empty());
    let drawn = screen(&mut app, 100, 30);
    assert!(drawn.contains("PR #12 has new commits"), "{drawn}");
    assert!(drawn.contains("Send anyway · Esc cancel"), "{drawn}");
    assert!(!app.flash.as_ref().is_some_and(|f| f.kind == FlashKind::Error));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["action_id"], "changes:pr@2-new");
    assert_eq!(sent[0].1["input"], original["input"]);
    assert_ne!(sent[0].1["command_id"], original["command_id"]);
    assert!(app.modal.is_none());
}

#[test]
fn escape_on_a_changed_modal_sends_nothing() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let request = crate::client::Client::action_body("epoch", "changes:pr@1-old", None, serde_json::json!({}));
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "result":{"kind":"changed", "title":"New commits",
            "next":{"id":"changes:pr@2-new", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
                "resolves_card":true}}
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction { request, result: Ok(changed) });
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Enter);
    assert!(app.modal.is_none());
    assert!(posted(&app).is_empty());
}

/// The merge action exactly as the server words it in `card-pr.json`, under this test's id.
fn server_merge_action(action_id: &str) -> serde_json::Value {
    let card = view_fixture("card-pr.json");
    let mut merge = card["sections"].as_array().unwrap().iter()
        .flat_map(|section| section["items"].as_array().unwrap())
        .flat_map(|item| item["actions"].as_array().into_iter().flatten())
        .find(|action| action["id"].as_str().is_some_and(|id| id.starts_with("merge:")))
        .expect("card-pr.json offers a merge")
        .clone();
    merge["id"] = action_id.into();
    merge
}

fn merge_card(app: &mut App, merge: impl FnOnce(&str) -> serde_json::Value) -> (String, String) {
    let id = "hf-weekly-2026-09-30";
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.sel = Some(Key(id.into()));
    let action_id = format!("merge:{id}@7~abcdef12");
    let mut card = view_fixture("card-pr.json");
    card["key"] = format!("card:{id}").into();
    card["sections"][0]["items"][0]["actions"] = serde_json::json!([merge(&action_id)]);
    app.on_msg(Msg::ViewCard {
        key: format!("card:{id}"),
        result: Ok(serde_json::from_value(card).unwrap()),
    });
    (id.into(), action_id)
}

fn served_merge_card(app: &mut App) -> (String, String) {
    merge_card(app, server_merge_action)
}

/// What "More" offers on `card-pr.json`, in order: the other ways to merge come first.
fn served_more() -> Vec<serde_json::Value> {
    view_fixture("card-pr.json")["sections"][0]["items"][0]["more"].as_array().cloned().unwrap_or_default()
}

fn applied(kind: &str, title: &str) -> Result<ViewActionReply, Delivery> {
    view_action_result(serde_json::json!({
        "outcome":"applied", "stamp":"x", "result":{"kind":kind, "title":title}
    }))
}

#[test]
fn merge_end_to_end_one_press_one_post_then_started() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (pr_id, action_id) = served_merge_card(&mut app);
    let before = screen(&mut app, 120, 30);
    assert!(before.contains("Squash and merge"), "the button says the method: {before}");

    press(&mut app, KeyCode::Char('1'));
    assert!(app.modal.is_none() && app.card_menu.is_none(), "merging asks nothing");
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "/actions");
    assert_eq!(sent[0].1["action_id"], action_id);
    assert_eq!(sent[0].1["option_id"], "squash");
    assert_eq!(flash(&app), "Merging…");
    assert!(app.optimistic.hides(&Key(pr_id.clone())));
    assert!(!app.inbox_keys().contains(&Key(pr_id)), "the card leaves the list at once");
    let after = screen(&mut app, 120, 30);
    assert!(!after.contains("Review the beta candidate"), "the card is not drawn: {after}");
    assert!(!after.contains("Merge PR #12?"), "{after}");
    assert!(after.contains("Merging…"), "{after}");

    app.on_msg(Msg::ViewAction {
        request: sent[0].1.clone(),
        result: applied("started", "PR #12 is merging. You'll see it in its task when GitHub confirms."),
    });
    assert_eq!(flash(&app), "PR #12 is merging. You'll see it in its task when GitHub confirms.");
    assert_eq!(app.flash.as_ref().map(|f| f.kind), Some(FlashKind::Pending));
    assert_eq!(posted(&app).len(), 1);
}

#[test]
fn escape_on_the_more_menu_sends_nothing() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Esc);
    assert!(app.card_menu.is_none());
    assert!(posted(&app).is_empty());
}

#[test]
fn a_refused_action_brings_the_card_back_with_the_servers_words() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (pr_id, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    assert!(app.optimistic.hides(&Key(pr_id.clone())));
    app.on_msg(Msg::ViewAction {
        request,
        result: view_action_result(serde_json::json!({
            "outcome":"rejected",
            "result":{"kind":"refused", "title":"PR #11 must merge first.", "body":"This one builds on it."}
        })),
    });
    assert!(!app.optimistic.hides(&Key(pr_id.clone())), "the card is back");
    assert!(app.inbox_keys().contains(&Key(pr_id)));
    let back = screen(&mut app, 120, 30);
    assert!(back.contains("Review the beta candidate"), "the card is drawn again: {back}");
    assert!(back.contains("PR #11 must merge first."), "{back}");
    assert_eq!(app.flash.as_ref().map(|f| f.kind), Some(FlashKind::Error));
    assert!(flash(&app).contains("PR #11 must merge first."));
    assert!(flash(&app).contains("This one builds on it."));
}

#[test]
fn an_answer_that_does_not_reach_the_mac_is_tried_again_in_silence_with_the_same_command() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (pr_id, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let shown = flash(&app).to_owned();
    for failure in [Delivery::Unsent, Delivery::Unanswered] {
        app.on_msg(Msg::ViewAction { request: request.clone(), result: Err(failure) });
        assert!(app.optimistic.hides(&Key(pr_id.clone())), "the card stays out while it is retried");
        assert_eq!(flash(&app), shown, "nothing says it failed");
        app.tick_answers(now_millis() + 20_000);
    }
    let sent = posted(&app);
    assert_eq!(sent.len(), 3, "sent again after each failure");
    assert!(sent.iter().all(|(_, body)| *body == request), "the very same request, so the daemon does it once");
}

/// 5 November: adding an account from the TUI took longer than one attempt; the retry found the
/// first attempt still running and the TUI said «Tu Mac no pudo hacerlo · outcome_unknown» while the
/// account was being added. Still running is retried in silence until the real reply arrives.
#[test]
fn an_action_the_mac_is_still_running_is_retried_in_silence_and_ends_with_its_real_reply() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (pr_id, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let shown = flash(&app).to_owned();
    app.on_msg(Msg::ViewAction { request: request.clone(), result: Err(Delivery::Running) });
    assert!(app.optimistic.hides(&Key(pr_id.clone())), "the card stays out while the Mac works on it");
    assert_eq!(flash(&app), shown, "nothing says it failed");
    app.tick_answers(now_millis() + 20_000);
    let sent = posted(&app);
    assert_eq!(sent.len(), 2, "asked again");
    assert_eq!(sent[1].1, request, "with the same command, so the Mac answers how the first one ended");
    let done = Outcome { kind: OutcomeKind::Done, title: "Merged".into(), body: None, next: None };
    app.on_msg(Msg::ViewAction { request, result: Ok(ViewActionReply::Outcome(done)) });
    assert_eq!(flash(&app), "Merged");
}

#[test]
fn an_action_whose_outcome_the_mac_cannot_tell_says_what_happened_and_what_to_do_without_codes() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let reason = alondra_core::views::action_outcome::UnknownReason::Interrupted;
    app.on_msg(Msg::ViewAction { request, result: Err(Delivery::Unknown(reason)) });
    let notice = flash(&app).to_owned();
    assert_eq!(notice, "Tu Mac no sabe si se hizo.\nAlondra se reinició en tu Mac mientras lo hacía. Mira si el cambio está; si no, vuelve a hacerlo.");
    assert!(!notice.contains("outcome_unknown"));
    assert_eq!(posted(&app).len(), 1, "not sent again: asking again gives the same answer");
}

#[test]
fn flash_kinds_draw_with_their_own_mark() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.flash("Saved");
    assert!(screen(&mut app, 80, 24).contains("✓ Saved"));
    app.pending("Merging…");
    assert!(screen(&mut app, 80, 24).contains("… Merging…"));
    app.fail("Not done");
    assert!(screen(&mut app, 80, 24).contains("✗ Not done"));
}

#[test]
fn no_waiting_card_fixture_has_an_unusable_action() {
    for name in std::fs::read_dir(fixtures_dir())
    .unwrap()
    .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
    .filter(|name| name.starts_with("card-") && name.ends_with(".json"))
    {
        let view: alondra_core::views::View = serde_json::from_value(view_fixture(&name)).unwrap();
        for action in crate::tui::view_render::actions(&view) {
            assert!(action.enabled && action.disabled_reason.is_none(), "{name}: {}", action.label);
        }
    }
}

#[test]
fn failed_views_feed_marks_cached_tasks_as_outdated_until_recovery() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-tui.json")).unwrap())));
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
        service: None,
    })));
    assert!(!app.offline);
}

#[test]
fn the_task_list_counts_running_time_instead_of_the_text_older_clients_read() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-tui.json")).unwrap())));
    let drawn = screen(&mut app, 200, 40);
    assert!(!drawn.contains("running since"), "{drawn}");
    assert!(drawn.lines().any(|line| line.contains("Count the retracted pushes") && line.contains(" d ")), "{drawn}");
}

#[test]
fn tasks_search_shows_pending_and_uses_the_typed_query() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-tui.json")).unwrap())));
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
fn tasks_search_paste_loads_the_pasted_query() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
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
            "8fa4034b-249e-4487-a89b-57de38c26372",
            "hf-weekly-2026-09-30",
            "f12d034b-249e-4487-a89b-57de38c26372",
            "cc2d034b-249e-4487-a89b-57de38c26372"
        ]
    );
    let list = screen(&mut app, 120, 26);
    for label in ["WAITING FOR YOU", "Choose the welcome copy", "Review the beta candidate"] {
        assert!(list.contains(label), "{label}: {list}");
    }

    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(serde_json::from_value(view_fixture("card-question.json")).unwrap()),
    });
    let detail = screen(&mut app, 120, 44);
    assert!(
        detail.contains("The new empty state is ready") && detail.contains("Concise: a short title"),
        "{detail}"
    );
}

#[test]
fn tasks_search_key_opens_the_server_search_view() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-tui.json")).unwrap())));

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
fn no_key_opens_the_orchestrator() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    press(&mut app, KeyCode::Char('M'));
    assert!(!app.view_navigation.iter().any(|key| key.contains("orchestrator")), "{:?}", app.view_navigation);
    assert!(app.composer.is_none());
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
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
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
    assert!(app.loading.contains(&diff_key(route, "head-154")));
    load_diff_at(&mut app, route, "head-154", "diff --git a/a b/a\n@@ -1 +1 @@\n-broken\n+fixed\n", false);
    let shown = screen(&mut app, 120, 30);
    assert!(shown.contains("fixed") && shown.contains("Changes · 1 file · +1 −1"), "{shown}");
}

const PR_DIFF: &str = include_str!("../../../../../fixtures/diffs/pull-request.diff");
const DELIVERY_DIFF: &str = "/pull-requests/github.com%2Fexample%2Frepo%2312/diff";
const DELIVERY_HEAD: &str = "abcdef12";

fn load_diff(app: &mut App, route: &str, text: &str, truncated: bool) {
    load_diff_at(app, route, DELIVERY_HEAD, text, truncated);
}

/// The diff the daemon answers for `route` at `head`.
fn load_diff_at(app: &mut App, route: &str, head: &str, text: &str, truncated: bool) {
    app.on_msg(Msg::File {
        route: diff_key(route, head),
        result: Ok(files::FileBody::Diff(std::sync::Arc::new(unified_diff::Diff::read(text, truncated)))),
    });
}

/// The screen's rows, and its buffer to read colours from.
fn diff_rows(app: &mut App, width: u16, height: u16) -> (Vec<String>, ratatui::buffer::Buffer) {
    let backend = ratatui::backend::TestBackend::new(width, height);
    let mut terminal = ratatui::Terminal::new(backend).unwrap();
    terminal.draw(|frame| draw::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let rows = (0..height)
        .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect();
    (rows, buffer)
}

fn row_with<'a>(rows: &'a [String], text: &str) -> Option<&'a String> {
    rows.iter().find(|row| row.contains(text))
}

/// The style of the cell where `text` starts on screen.
fn style_at(rows: &[String], buffer: &ratatui::buffer::Buffer, text: &str) -> ratatui::style::Style {
    let (y, row) = rows.iter().enumerate().find(|(_, row)| row.contains(text)).unwrap_or_else(|| panic!("no {text}"));
    let x = row[..row.find(text).unwrap()].chars().count();
    buffer[(x as u16, y as u16)].style()
}

/// The PR delivery card's detail, as the inbox shows it, with `d` offered for its diff.
fn delivery_detail() -> App {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let view: alondra_core::views::View = serde_json::from_value(view_fixture("card-delivery.json")).unwrap();
    app.view_navigation.push(view.key.clone());
    app.card_view = Some(view);
    app
}

fn cursor_kind(app: &App) -> md::Kind {
    let reader = app.reader.as_ref().unwrap();
    reader.blocks[reader.cursor].kind.clone()
}

#[test]
fn pr_detail_summarises_the_changes_and_leaves_the_diff_to_d() {
    let mut app = delivery_detail();
    load_diff(&mut app, DELIVERY_DIFF, PR_DIFF, false);
    let shown: String = view_render::document_lines(app.card_view.as_ref().unwrap(), 120, false, false)
        .iter()
        .map(|line| format!("{line}\n"))
        .collect();
    assert!(shown.contains("Changes · 4 files · +120 −30 · d view changes"), "{shown}");
    assert!(!shown.contains("compactRow") && !shown.contains("ScheduleEditor.swift"), "{shown}");
    assert!(draw::footer_keys(&app).iter().any(|(key, label)| key == "d" && label == "view changes"));
}

/// The PR delivery's detail opened as the daemon sends it, without the card's count of changes.
fn uncounted_detail() -> App {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let mut view = view_fixture("card-delivery.json");
    view["sections"][0]["items"][0]["content_row"] = serde_json::Value::Null;
    let key = view["key"].as_str().unwrap().to_owned();
    app.view_navigation.push(key.clone());
    app.on_msg(Msg::ViewCard { key, result: Ok(serde_json::from_value(view).unwrap()) });
    app
}

fn changes_label(app: &App) -> String {
    let changes = |route: &str, head: &str| match app.files.get(&diff_key(route, head)) {
        Some(Ok(files::FileBody::Diff(diff))) => Some(md::diff::diff_counts(diff)),
        _ => None,
    };
    view_render::document_lines_with(app.card_view.as_ref().unwrap(), 120, false, false, &changes)
        .iter()
        .map(|line| format!("{line}\n"))
        .collect()
}

#[test]
fn a_detail_without_the_cards_count_fetches_the_diff_and_counts_it() {
    let mut app = uncounted_detail();
    assert!(app.loading.contains(&diff_key(DELIVERY_DIFF, DELIVERY_HEAD)), "opening the detail asks for its diff");
    assert!(changes_label(&app).contains("Changes · d view changes"), "{}", changes_label(&app));
    load_diff(&mut app, DELIVERY_DIFF, PR_DIFF, false);
    assert!(changes_label(&app).contains("Changes · 6 files · +8 −6 · d view changes"), "{}", changes_label(&app));
}

#[test]
fn a_new_head_reads_the_diff_again() {
    let mut app = uncounted_detail();
    load_diff(&mut app, DELIVERY_DIFF, PR_DIFF, false);
    let mut pushed = serde_json::to_value(app.card_view.clone().unwrap()).unwrap();
    pushed["sections"][2]["items"][0]["digest"] = serde_json::json!("fedcba98");
    let key = pushed["key"].as_str().unwrap().to_owned();
    app.on_msg(Msg::ViewCard { key, result: Ok(serde_json::from_value(pushed).unwrap()) });
    assert!(app.loading.contains(&diff_key(DELIVERY_DIFF, "fedcba98")), "a new head reads the diff again");
    assert!(changes_label(&app).contains("Changes · d view changes"), "the old count is not shown for the new head: {}", changes_label(&app));
}

#[test]
fn d_on_a_card_detail_opens_its_pr_diff_by_file() {
    let mut app = delivery_detail();
    press(&mut app, KeyCode::Char('d'));
    assert!(app.loading.contains(&diff_key(DELIVERY_DIFF, DELIVERY_HEAD)), "the card's diff is fetched as the PR's diff");
    let reader = app.reader.as_ref().expect("d opens the reader");
    assert!(
        reader.parts.iter().any(|part| matches!(part,
            Part::Diff { route, id, head } if route == DELIVERY_DIFF && id == "github.com/example/repo#12" && head == "abcdef12")),
        "a card's diff is read as the PR's diff, not as a file"
    );
    load_diff(&mut app, DELIVERY_DIFF, PR_DIFF, false);
    let (rows, buffer) = diff_rows(&mut app, 110, 60);
    let screen = rows.join("\n");
    assert!(screen.contains("Changes · 6 files · +8 −6"), "{screen}");
    for entry in ["M  Sources/ScheduleEditor.swift  +3 −3", "A  Sources/CompactRow.swift  +4 −0",
        "D  Sources/LongForm.swift  +0 −2", "R  Old/Launch.swift → Sources/Launch/Launch.swift",
        "R  Sources/Settings.swift → Sources/Preferences.swift  +1 −1", "B  Assets/icon.png"] {
        assert!(screen.contains(entry), "{entry}: {screen}");
    }
    assert!(matches!(cursor_kind(&app), md::Kind::DiffEntry(header) if header.path == "Sources/ScheduleEditor.swift"),
        "d puts the cursor on the list of files");
    assert!(screen.contains("⌄ M  Sources/ScheduleEditor.swift  +3 −3"), "{screen}");
    assert!(screen.contains("@@ -10,5 +10,5 @@ struct ScheduleEditor: View {"), "{screen}");
    let context = row_with(&rows, "var body: some View {").unwrap();
    assert!(context.contains(" 10  10   "), "old and new numbers: {context}");
    let removed = row_with(&rows, "Form { longRows }").unwrap();
    assert!(removed.contains(" 11     -"), "a removed line has only its old number: {removed}");
    let added = row_with(&rows, "Form { compactRow }").unwrap();
    assert!(added.contains("      11 +"), "an added line has only its new number: {added}");
    use ratatui::style::Color;
    assert_eq!(style_at(&rows, &buffer, "Form { compactRow }").fg, Some(Color::Green));
    assert_eq!(style_at(&rows, &buffer, "Form { compactRow }").bg, Some(Color::Reset));
    assert_eq!(style_at(&rows, &buffer, "compactRow").bg, Some(Color::Indexed(22)), "what changed inside the line is marked");
    assert_eq!(style_at(&rows, &buffer, "longRows").bg, Some(Color::Indexed(52)));
    assert_eq!(style_at(&rows, &buffer, "Form { longRows }").fg, Some(Color::Red));
    assert!(screen.contains("› B  Assets/icon.png · Binary file"), "a binary starts folded: {screen}");
    assert!(screen.contains("› R  Old/Launch.swift → Sources/Launch/Launch.swift · Renamed, no changes"), "{screen}");
}

#[test]
fn files_fold_with_enter_and_brackets_jump_between_them() {
    let mut app = delivery_detail();
    press(&mut app, KeyCode::Char('d'));
    load_diff(&mut app, DELIVERY_DIFF, PR_DIFF, false);
    diff_rows(&mut app, 110, 60);
    let footer = draw::footer_keys(&app);
    assert!(footer.iter().any(|(key, label)| key == "]" && label == "next file"), "{footer:?}");
    assert!(footer.iter().any(|(key, label)| key == "[" && label == "previous file"), "{footer:?}");
    assert!(footer.iter().any(|(key, label)| key == "⏎" && label == "go to the file"), "{footer:?}");
    let header = |app: &App| match cursor_kind(app) {
        md::Kind::DiffFile(header) => header.path,
        other => panic!("not on a file: {other:?}"),
    };
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(header(&app), "Sources/ScheduleEditor.swift");
    let (shown, _) = diff_rows(&mut app, 110, 20);
    let top = shown.iter().position(|row| row.contains("⌄ M  Sources/ScheduleEditor.swift"));
    assert!(top.is_some_and(|row| row < 4), "the file comes to the top: {shown:#?}");
    assert!(draw::footer_keys(&app).iter().any(|(key, label)| key == "⏎" && label == "fold the file"));
    press(&mut app, KeyCode::Enter);
    let (shown, _) = diff_rows(&mut app, 110, 60);
    let screen = shown.join("\n");
    assert!(screen.contains("› M  Sources/ScheduleEditor.swift") && !screen.contains("Form { compactRow }"), "{screen}");
    press(&mut app, KeyCode::Enter);
    let (shown, _) = diff_rows(&mut app, 110, 60);
    assert!(shown.join("\n").contains("Form { compactRow }"), "enter unfolds it again");
    press(&mut app, KeyCode::Char(']'));
    assert_eq!(header(&app), "Sources/CompactRow.swift");
    press(&mut app, KeyCode::Char('['));
    assert_eq!(header(&app), "Sources/ScheduleEditor.swift");
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('c'));
    assert!(
        matches!(app.composer.as_ref().map(|composer| &composer.target),
            Some(Target::Comment { quote, .. }) if quote == "Sources/ScheduleEditor.swift:11 Form { longRows }"),
        "a comment on a diff line quotes its file and line"
    );
}

#[test]
fn large_files_start_folded_and_open_from_the_list_and_a_cut_diff_says_so() {
    let mut app = delivery_detail();
    press(&mut app, KeyCode::Char('d'));
    let lines: String = (0..=unified_diff::FOLDED_CHANGE_THRESHOLD).map(|n| format!("+generated line {n}\n")).collect();
    let text = format!("diff --git a/big.txt b/big.txt\nnew file mode 100644\n@@ -0,0 +1,401 @@\n{lines}");
    load_diff(&mut app, DELIVERY_DIFF, &text, true);
    let (shown, _) = diff_rows(&mut app, 110, 40);
    let screen = shown.join("\n");
    assert!(screen.contains("Part of the diff is not shown. Open the PR on GitHub to read the rest."), "{screen}");
    assert!(screen.contains("› A  big.txt  +401 −0 · Large file · ⏎ shows it"), "{screen}");
    assert!(!screen.contains("generated line 0"), "{screen}");
    press(&mut app, KeyCode::Enter);
    let (shown, _) = diff_rows(&mut app, 110, 40);
    let screen = shown.join("\n");
    assert!(screen.contains("⌄ A  big.txt") && screen.contains("generated line 0"), "{screen}");
}

/// The key bar with `view` open as the detail of what is selected.
fn footer_on(view: alondra_core::views::View) -> Vec<(String, String)> {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.view_navigation.push(view.key.clone());
    app.card_view = Some(view);
    crate::tui::draw::footer_keys(&app)
}

#[test]
fn diff_card_footer_names_the_view_changes_shortcut() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let mut view = view_fixture("card-question.json");
    view["key"] = serde_json::json!("pull_request:owner/repo#1");
    view["sections"] = serde_json::json!([{"id":"diff", "collapsed":false, "items":[{
        "type":"content_ref", "kind":"diff", "title":"Diff", "size":0,
        "digest":"head-1", "fetch":"/diff", "commentable":true
    }]}]);
    let keys = footer_on(serde_json::from_value(view).unwrap());
    assert!(keys.iter().any(|(key, label)| key == "d" && label == "view changes"));
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
                    {"id":"squash", "label":"Squash and merge", "style":"secondary",
                     "input":{"kind":"none"}},
                    {"id":"merge", "label":"Merge commit", "style":"secondary",
                     "input":{"kind":"none"}}
                 ]}
            ]
        }]
    }]);
    let keys = footer_on(serde_json::from_value(view).unwrap());
    assert_eq!(keys.iter().filter(|(_, label)| label == "Ask about the PR").count(), 1);
    assert!(keys.iter().any(|(key, label)| key == "1" && label == "Squash and merge"));
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
    delivery["sections"][0]["items"][0]["actions"] = serde_json::json!([]);
    delivery["sections"][0]["items"][0]["more"] = serde_json::json!([{
        "id":"", "label":"Review local clone", "style":"plain", "presentation":"button",
        "input":{"kind":"none"}, "attachments":false, "enabled":true,
        "link":{"kind":"local", "local":{"kind":"review_clone",
            "repo":"owner/alondra-no-clone-fixture-999", "branch":"head-154", "pr":154,
            "pr_id":pr_id}}
    }]);
    app.view_navigation.push("card:delivery-1".into());
    app.card_view = Some(serde_json::from_value(delivery).unwrap());
    press(&mut app, KeyCode::Char('m'));
    press(&mut app, KeyCode::Char('1'));
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
    assert!(app.reader.is_some() && app.loading.contains(&diff_key(route, "head-154")));
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
    untyped(&mut view);
    app.view_navigation.push(format!("card:{}", question_id()));
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    press(&mut app, KeyCode::Char('C'));
    typed(&mut app, "Tighter");
    press(&mut app, KeyCode::Enter);
    assert!(crate::tui::draw::footer_keys(&app).iter().any(|(key, _)| key == "2"), "every option carries the comments");
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
    view["sections"][0]["items"][0]["actions"] = serde_json::json!([{
        "id": format!("ans:{}@1", question_id()), "label": "Answer", "style": "primary", "presentation": "options",
        "attachments": false, "enabled": true, "input": {"kind": "none"},
        "options": [{"id":"changes", "label":"Request changes", "style":"secondary",
            "input":{"kind":"text", "placeholder":"Explain", "required":true,"multiline":true}, "key":"1"}]
    }]);
    app.view_navigation.push(format!("card:{}", question_id()));
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    press(&mut app, KeyCode::Char('c'));
    typed(&mut app, "Tighter");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('1'));
    let anchored = "1. On \"Choose the welcome copy\":\n   Tighter";
    assert!(app.composer.as_ref().is_some_and(|composer| composer.input.text().contains(anchored)));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent[0].1["option_id"], "changes");
    assert!(sent[0].1["input"]["text"].as_str().unwrap().contains(anchored));
}

#[test]
fn number_keys_press_the_buttons_and_answer_with_their_option_id() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture(
        "home-tui.json",
    ))
    .unwrap())));
    app.reconcile();
    let card = view_fixture("card-question.json");
    let buttons = card["sections"][0]["items"][0]["actions"].clone();
    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(serde_json::from_value(card).unwrap()),
    });

    press(&mut app, KeyCode::Char('2'));

    let requests = posted(&app);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/actions");
    assert_eq!(requests[0].1["option_id"], buttons[1]["option_id"]);
    assert_eq!(requests[0].1["action_id"], buttons[1]["id"]);
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
    untyped(&mut card);
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
        "New work", "Project", "Alondra picks the project", "Issue", "none", "PRs",
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
    assert_eq!(sent[0].1["action_id"], "new_request:view@6");
    assert_eq!(sent[0].1["data_epoch"], "test-epoch");
    assert_eq!(
        sent[0].1["input"],
        serde_json::json!({"fields": {
            "text": "Fix the crash", "project": "", "issue": "", "pr": "auto_merge", "effort": "auto", "account": "auto"
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

fn question_card() -> alondra_core::views::View {
    let mut card = view_fixture("card-question.json");
    card["sections"][0]["items"][0]["timestamps"] =
        serde_json::json!([{"role": "created", "at": screens::minutes_ago(2), "format": "relative"}]);
    serde_json::from_value(card).unwrap()
}

fn text_of(lines: &[ratatui::text::Line<'_>]) -> Vec<String> {
    lines.iter().map(|line| line.to_string()).collect()
}

#[test]
fn card_detail_starts_with_the_title_and_keeps_agent_and_age_metadata() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let card = question_card();
    let lines = text_of(&crate::tui::view_render::document_lines(&card, 80, false, false));
    assert_eq!(lines[0], "Choose the welcome copy");
    assert!(lines.iter().any(|line| line.starts_with("Restore the native review experience · Demo Garden · ")
        && (line.ends_with(" ago") || line.ends_with("Sep 30"))), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("DECIDE")), "the frame carries the kind: {lines:#?}");
    assert_eq!(
        lines.iter().filter(|line| line.contains("Choose the welcome copy")).count(),
        1,
        "{lines:#?}"
    );
    assert!(lines.iter().any(|line| line.starts_with("Task: Restore the native review")), "{lines:#?}");
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
    assert!(shown.contains(" decide "), "{shown}");
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
        shown.contains("? decide · Restore the native review experience · Choose the welcome copy"),
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

fn attachable(key: &str) -> alondra_core::views::View {
    let mut view = view_fixture("card-question.json");
    view["view"] = serde_json::json!("task");
    view["key"] = serde_json::json!(key);
    view["sections"] = serde_json::json!([{"id": "attach", "collapsed": false, "items": [{
        "type": "row", "id": "attach", "icon": "node", "title": "Attach", "depth": 0, "struck": false,
        "link": {"kind": "local", "local": {"kind": "attach", "session": "short-a-1"}}
    }]}]);
    serde_json::from_value(view).unwrap()
}

#[test]
fn footers_name_the_next_screen_and_never_offer_a_terminal() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let has = |app: &App, key: &str, label: &str| {
        crate::tui::draw::footer_keys(app).iter().any(|(k, l)| k == key && l == label)
    };
    let terminal = |app: &App| crate::tui::draw::footer_keys(app).iter().any(|(_, l)| l.contains("terminal"));
    app.home_view = Some(serde_json::from_value(view_fixture("home-tui.json")).unwrap());
    assert!(has(&app, "Tab", "tasks"));
    app.screen = Screen::Tasks;
    app.home_view = Some(serde_json::from_value(view_fixture("tasks-ios.json")).unwrap());
    app.view_navigation.push("task:t-1".into());
    app.card_view = Some(attachable("task:t-1"));
    assert!(has(&app, "⏎/o", "expand") && has(&app, "Tab", "inbox"));
    assert!(!terminal(&app), "an old daemon's attach link never offers a terminal");
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    assert!(has(&app, "Tab", "bandeja") && has(&app, "⏎/o", "expandir"));
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
fn enter_on_a_task_or_node_expands_it_and_an_old_attach_link_runs_nothing() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.view_navigation.push("task:t-1".into());
    app.card_view = Some(attachable("task:t-1"));
    assert!(matches!(app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Flow::Go));
    let attach: alondra_core::views::Action = serde_json::from_value(serde_json::json!({
        "id": "", "label": "Attach to node", "style": "plain", "presentation": "button",
        "input": {"kind": "none"}, "attachments": false, "enabled": true, "key": "a",
        "link": {"kind": "local", "local": {"kind": "attach", "session": "short-a-1"}}
    })).unwrap();
    assert!(matches!(app.run_view_action_confirmed(&attach, None, &Key("t-1".into()), "test".into()), Flow::Go));
    let flash = app.flash.as_ref().expect("the refusal is told");
    assert!(flash.text.contains("no longer opens a node's terminal"), "{}", flash.text);
}

pub(super) fn status_with(gauges: serde_json::Value) -> alondra_core::views::Status {
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
    assert!(!shown.contains("Fleet"), "{shown}");
    for expected in [" Alondra ", "Inbox ", "Tasks 1", "2 working", "RAM ▰▰▱▱ 42%  CPU ▰▱▱▱ 34%", "Disk ▰▰▰▱ 120 GB free"] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
    assert!(shown.contains("34%  Disk"), "the gauges are separated: {shown}");
    assert!(!shown.contains("Local"));
}

#[test]
fn a_banner_tops_the_screen_while_claude_usage_is_out_and_goes_with_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    let mut status = status_with(production_gauges());
    status.usage_exhausted = Some(alondra_core::views::UsageExhausted {
        scope: "this_mac".into(),
        until: "2099-10-05T14:30:00.000+02:00".into(),
        macs_exhausted: 1,
        macs_total: 1,
        paused_by_user: false,
    });
    app.view_status = Some(status);
    let shown = screen(&mut app, 120, 6);
    let first_row: String = shown.chars().take(120).collect();
    let rest: String = shown.chars().skip(120).collect();
    assert!(
        first_row.contains("Usage limit reached on this Mac until 14:30"),
        "{shown}"
    );
    assert!(rest.contains(" Alondra "), "the header sits under the banner: {shown}");

    app.view_status = Some(status_with(production_gauges()));
    let shown = screen(&mut app, 120, 6);
    assert!(!shown.contains("No margin"), "{shown}");
    assert!(shown.chars().take(120).collect::<String>().contains(" Alondra "), "{shown}");
}

#[test]
fn the_banner_says_work_waits_for_a_claude_session_while_no_account_has_one() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    let mut status = status_with(production_gauges());
    status.session_wait = Some(alondra_core::views::Notice {
        text: "Esperando una sesión de Claude · inicia sesión en una cuenta en Cuentas de Claude".into(),
        tone: alondra_core::views::Tone::Warning,
        action: None,
    });
    app.view_status = Some(status);
    let shown = screen(&mut app, 120, 6);
    let first_row: String = shown.chars().take(120).collect();
    assert!(first_row.contains("Esperando una sesión de Claude · inicia sesión en una cuenta en Cuentas de Claude"), "{shown}");
    assert!(!shown.contains("margen") && !shown.contains("min más"), "no quota and no countdown: {shown}");
}

pub(super) fn usage_gauges(lang: crate::i18n::Lang) -> serde_json::Value {
    let es = lang == crate::i18n::Lang::Es;
    let t = |es_text: &str, en_text: &str| if es { es_text.to_owned() } else { en_text.to_owned() };
    serde_json::json!([
        {"id": "usage", "label": "Claude", "value": 0.7, "text": t("por encima del ritmo", "over pace"), "level": "warm",
         "detail": t("Claude va por encima del ritmo de hoy: seguir gasta parte de los próximos días",
                     "Claude is over today's pace: going on spends part of the coming days"),
         "parts": [
            {"label": "Principal", "group": "Claude", "value": 0.7, "text": t("por encima del ritmo", "over pace"), "level": "warm",
             "lines": [t("Le queda el 70 % de la semana · se reinicia el lunes a las 02:00", "70% of the week left · resets Monday at 02:00"),
                       t("Hoy ya ha gastado su parte (18 % de 15 %)", "Today's share is spent (18% of 15%)")]},
            {"label": "Trabajo", "group": "Claude", "value": 0.0, "text": t("sin margen", "no room"), "level": "hot",
             "lines": [t("En su límite · vuelve en 2 h (14:30)", "At its limit · back in 2 h (14:30)")]},
            {"label": "Codex", "group": "Codex", "value": 0.8, "text": t("con margen", "with room"), "level": "calm",
             "lines": [t("Le queda el 80 % de la semana · se reinicia el jueves a las 09:00", "80% of the week left · resets Thursday at 09:00")]}
         ]},
        {"id": "memory", "label": "RAM", "value": 0.42, "text": "42%", "level": "calm"},
        {"id": "cpu", "label": "CPU", "value": 0.34, "text": "34%", "level": "calm"},
        {"id": "disk", "label": t("Disco", "Disk"), "value": 0.72, "text": t("120 GB libres", "120 GB free"), "level": "calm"}
    ])
}

fn rows(shown: &str, width: usize) -> Vec<String> {
    shown.chars().collect::<Vec<_>>().chunks(width).map(|row| row.iter().collect()).collect()
}

#[test]
fn the_usage_indicator_is_one_word_in_the_header_of_tasks_and_inbox() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-ios.json")).unwrap())));
    app.view_status = Some(status_with(usage_gauges(crate::i18n::Lang::Es)));
    for screen_shown in [Screen::Tasks, Screen::Inbox] {
        app.screen = screen_shown;
        let shown = screen(&mut app, 180, 12);
        let rows = rows(&shown, 180);
        assert!(rows[0].contains("● Claude: por encima del ritmo  RAM ▰▰▱▱ 42%"), "{shown}");
        assert!(!rows[1].contains("Principal"), "the accounts wait for the sheet: {shown}");
        assert!(shown.contains("u uso"), "the key bar offers the sheet: {shown}");
    }
    app.screen = Screen::Tasks;
    let shown = screen(&mut app, 120, 6);
    assert!(rows(&shown, 120)[0].trim_end().ends_with("● Claude: por encima del ritmo"), "a narrow header drops the other bars, not the word: {shown}");
}

#[test]
fn u_opens_the_usage_sheet_with_one_section_per_provider_and_any_key_closes_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("tasks-ios.json")).unwrap())));
    app.view_status = Some(status_with(usage_gauges(crate::i18n::Lang::Es)));
    press(&mut app, KeyCode::Char('u'));
    let shown = screen(&mut app, 120, 30);
    for expected in [
        " Uso · cualquier tecla cierra ",
        "Claude va por encima del ritmo de hoy: seguir gasta parte de los próximos días",
        "● Principal  ▰▰▰▱ por encima del ritmo",
        "Le queda el 70 % de la semana · se reinicia el lunes a las 02:00",
        "Hoy ya ha gastado su parte (18 % de 15 %)",
        "● Trabajo  ▱▱▱▱ sin margen",
        "En su límite · vuelve en 2 h (14:30)",
        "● Codex  ▰▰▰▱ con margen",
        "sin margen: el trabajo nuevo espera",
    ] {
        assert!(shown.contains(expected), "{expected}: {shown}");
    }
    let rows = rows(&shown, 120);
    let claude = rows.iter().position(|row| row.contains("│Claude ")).expect("a Claude section");
    let codex = rows.iter().position(|row| row.contains("│Codex ")).expect("a Codex section");
    let trabajo = rows.iter().position(|row| row.contains("● Trabajo")).unwrap();
    assert!(claude < trabajo && trabajo < codex, "each account under its provider: {shown}");
    press(&mut app, KeyCode::Esc);
    let shown = screen(&mut app, 120, 30);
    assert!(!shown.contains(" Uso · cualquier tecla cierra "), "{shown}");
}

#[test]
fn without_a_usage_reading_there_is_no_usage_key() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    app.view_status = Some(status_with(production_gauges()));
    press(&mut app, KeyCode::Char('u'));
    assert!(!app.usage_sheet);
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
    assert!(!shown.contains("Flota"), "{shown}");
    assert!(shown.contains("Tareas 1") && shown.contains("Disco ▰▰▰▱ 120 GB libres"), "{shown}");
}

#[test]
fn header_drops_the_gauge_names_before_it_overflows() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.loaded = true;
    app.view_status = Some(status_with(production_gauges()));
    let shown = screen(&mut app, 84, 6);
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
        Some("t-651c38484cd2f34e74d5107ba05f7f15")
    );
    let shown = screen(&mut app, 120, 20);
    assert!(shown.contains("Restore the native review experience") && shown.contains("Scheduled [s]"), "{shown}");
    assert!(!shown.contains("Scheduled Tasks"), "{shown}");
}

#[test]
fn help_lists_scheduled_tasks_the_disk_bar_and_no_fleet() {
    for lang in [crate::i18n::Lang::En, crate::i18n::Lang::Es] {
        crate::i18n::set_for_this_thread(lang);
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
        let mut app = App::new(tx);
        app.screen = Screen::Tasks;
        let help: String = crate::tui::draw::help_lines(&app)
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(help.contains("RAM") && help.contains("CPU"), "{help}");
        assert!(!help.to_lowercase().contains("agent"), "{help}");
        assert!(!help.contains("sesión") && !help.contains("session"), "{help}");
        assert!(!help.to_lowercase().contains("fleet") && !help.to_lowercase().contains("flota"), "{help}");
    }
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    let help: String = crate::tui::draw::help_lines(&app).iter().map(|line| line.to_string()).collect();
    assert!(help.contains("scheduled") && help.contains(":tasks"), "{help}");
}

fn held_plan_card() -> alondra_core::views::View {
    use alondra_core::domain::*;
    let mut state = DomainState::new("e");
    let mut run = |actor: Actor, command: Command| {
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("c{}", state.revision).into(), expected_revision: state.revision, command };
        state.execute(&actor, &envelope).unwrap();
    };
    let user = || Actor::User { user_id: "u".into() };
    run(user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
    run(Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into(), at: 0 });
    run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t".into(), request_id: "r".into(), title: "Build".into(),
        agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
    run(Actor::System, Command::HoldHook { interaction_id: "hold-1".into(), task_id: "t".into(),
        agent_id: "agent-a".into(), hold: Hold::Plan, body: "# Ship the parser\n\n- [ ] Split the lexer".into(),
        options: Hold::Plan.options(), card: alondra_core::domain::CardSpec::new(alondra_core::domain::CardKind::Review, "Ship the parser")
            .content(alondra_core::domain::ContentKind::Plan, "- [ ] Split the lexer") });
    let live = alondra_core::views::LiveSnapshot::default();
    alondra_core::views::build("card:hold-1", alondra_core::views::RenderContext {
        domain: &state, index: &alondra_core::domain::ReadIndex::build(&state), live: &live, client: alondra_core::views::Client::Tui, lang: alondra_core::views::Lang::En,
        transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "",
    }).unwrap()
}

#[test]
fn a_held_plan_is_approved_with_1_and_sent_back_from_its_field() {
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
    press(&mut app, KeyCode::Char('1'));
    assert!(posted(&app)[0].1["action_id"].as_str().is_some_and(|id| id.starts_with("accept:hold-1@")), "{:?}", posted(&app));
    let mut app = reader_on_plan();
    press(&mut app, KeyCode::Char('r'));
    assert!(matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { action_id, .. }) if action_id.starts_with("changes:hold-1@")));
    typed(&mut app, "Split it in two PRs");
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app)[0].1["input"]["text"], "Split it in two PRs");
}

fn task_view(id: &str, with_node: bool) -> alondra_core::views::View {
    let action = |action_id: &str, label: &str, key: &str| serde_json::json!({
        "id": action_id, "label": label, "style": "secondary", "presentation": "button",
        "input": {"kind": "none"}, "attachments": false, "enabled": true, "key": key
    });
    let mut actions = vec![action(&format!("cancel:{id}@1"), "Cancel Task", "x")];
    let mut sections = vec![];
    if with_node {
        actions.insert(0, action("msg:a-1@1", "Message", "M"));
        sections.push(serde_json::json!({"id": "attach", "collapsed": false, "items": [{
            "type": "row", "id": "attach", "icon": "node", "title": "Attach", "depth": 0, "struck": false,
            "link": {"kind": "local", "local": {"kind": "attach", "session": "abc123"}}, "actions": []
        }]}));
    }
    sections.push(serde_json::json!({"id": "actions", "collapsed": false,
        "items": [{"type": "actions", "actions": actions}]}));
    let mut view = view_fixture("card-question.json");
    view["key"] = serde_json::json!(format!("task:{id}"));
    view["sections"] = serde_json::json!(sections);
    serde_json::from_value(view).unwrap()
}

#[test]
fn task_footer_offers_only_the_keys_the_selected_task_answers() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.task_sel = Some(crate::tui::tasks::TaskKey::Task("t-1".into()));

    app.card_view = Some(task_view("t-1", true));
    let keys = crate::tui::draw::footer_keys(&app);
    assert!(keys.iter().any(|(key, label)| key == "Shift+M" && label == "Message"), "{keys:?}");
    assert!(keys.iter().any(|(key, label)| key == "⏎/o" && label == "expand"), "{keys:?}");
    assert!(keys.iter().any(|(key, label)| key == "x" && label == "Cancel Task"), "{keys:?}");
    assert!(!keys.iter().any(|(key, _)| key == "r"), "{keys:?}");

    app.card_view = Some(task_view("t-1", false));
    let keys = crate::tui::draw::footer_keys(&app);
    assert!(!keys.iter().any(|(key, _)| key == "Shift+M" || key == "⏎" || key == "r"), "{keys:?}");
    assert!(keys.iter().any(|(key, _)| key == "x"), "{keys:?}");
}

#[test]
fn a_changed_modal_hides_the_card_it_came_from_not_the_selected_one() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (origin, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "result":{"kind":"changed", "title":"New commits",
            "next":{"id":"merge:fresh", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
                "resolves_card":true}}
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction { request, result: Ok(changed) });
    assert!(app.flash.is_none(), "the merge is not running, so «Merging…» goes: {}", flash(&app));
    let other = Key(question_id());
    app.sel = Some(other.clone());
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app).len(), 2);
    assert!(app.optimistic.hides(&Key(origin)), "the card the action came from leaves");
    assert!(!app.optimistic.hides(&other), "the selected card stays");
}

#[test]
fn a_daemon_error_is_not_reported_as_nothing_sent() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (origin, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let error = anyhow::Error::new(crate::client::Answered { status: 500, message: "disk full".into() });
    app.on_msg(Msg::ViewAction { request, result: Err(crate::client::delivery(&error)) });
    assert_eq!(flash(&app), "Your Mac couldn't do it.\ndisk full");
    assert_eq!(app.flash.as_ref().map(|f| f.kind), Some(FlashKind::Error));
    assert!(app.inbox_keys().contains(&Key(origin)), "the card is back");
}

#[test]
fn an_answer_the_terminal_cannot_read_is_not_reported_as_nothing_sent() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let result = view_action_result(serde_json::json!({"something": "else"}));
    app.on_msg(Msg::ViewAction { request, result });
    assert_eq!(flash(&app), "Tu Mac no pudo hacerlo.");
}

#[test]
fn an_answer_unconfirmed_for_thirty_seconds_brings_its_card_back_saying_which_answer_did_not_arrive() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (origin, _) = served_merge_card(&mut app);
    let title = app.card_title(&Key(origin.clone()));
    press(&mut app, KeyCode::Char('1'));
    let first = posted(&app)[0].1.clone();
    let began = now_millis();
    app.on_msg(Msg::ViewAction { request: first.clone(), result: Err(Delivery::Unanswered) });
    app.tick_answers(began + optimistic::CONFIRMATION_MS - 1_000);
    assert!(!app.inbox_keys().contains(&Key(origin.clone())), "still out before thirty seconds");

    app.tick_answers(began + optimistic::CONFIRMATION_MS + 1);
    assert!(app.inbox_keys().contains(&Key(origin.clone())), "the card is back where it was");
    assert_eq!(app.sel, Some(Key(origin.clone())), "and selected again");
    assert_eq!(app.card_view.as_ref().map(|view| view.key.clone()), Some(format!("card:{origin}")),
        "with its detail, never waiting on the Mac that did not answer");
    let drawn = screen(&mut app, 120, 30);
    assert!(drawn.contains("Review the beta candidate"), "{drawn}");
    let notice = flash(&app);
    assert!(notice.starts_with("Tu Mac aún no ha confirmado tu respuesta «") && notice.contains(&format!("» a «{title}». La tarjeta ha vuelto")), "{notice}");
    assert_eq!(app.flash.as_ref().map(|f| (f.kind, f.ttl)), Some((FlashKind::Error, NOTICE_MS)));

    served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let sent = posted(&app);
    assert_eq!(sent.len(), 3, "the retry after the failure, then the answer again");
    assert_eq!(sent.last().unwrap().1, first, "answering again sends the same command, which the daemon does once");
    assert!(!app.inbox_keys().contains(&Key(origin)), "and the card leaves again");
}

/// 14 October: the user wrote their changes in the reply of the Redraft PR's delivery from the TUI and
/// the card stayed on screen. A reply the daemon says takes the card away leaves the queue and the
/// detail the moment it is sent, with nothing saying it is on its way.
#[test]
fn a_reply_written_on_a_delivery_leaves_the_queue_and_the_detail_at_once() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let id = "hf-weekly-2026-09-30";
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.sel = Some(Key(id.into()));
    let mut card = view_fixture("card-pr.json");
    card["key"] = format!("card:{id}").into();
    card["sections"][0]["items"][0]["reply"]["resolves_card"] = true.into();
    app.on_msg(Msg::ViewCard { key: format!("card:{id}"), result: Ok(serde_json::from_value(card).unwrap()) });
    assert!(screen(&mut app, 120, 30).contains("Review the beta candidate"));

    let waiting = app.attention();
    let listed = |app: &App| app.shown_home().and_then(|home| home.sections[0].count);
    let count = listed(&app);
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "No blurry photo found; make it a row like Albums");
    press(&mut app, KeyCode::Enter);
    assert_eq!(app.attention(), waiting - 1, "the inbox tab counts it out at once");
    assert_eq!(listed(&app), count.map(|count| count - 1), "and so does its section");
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("changes:"), "the field of a PR asks for changes: {sent:?}");
    assert!(!app.inbox_keys().contains(&Key(id.into())), "out of the queue before the Mac answers");
    let drawn = screen(&mut app, 120, 30);
    assert!(!drawn.contains("Review the beta candidate"), "out of the detail too: {drawn}");
    assert!(!drawn.contains("Sending") && !drawn.contains("sending"), "nothing says it is on its way: {drawn}");
}

#[test]
fn a_card_opened_from_a_link_leaves_the_detail_the_moment_it_is_answered() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    let key = format!("card:{}", question_id());
    let mut card = view_fixture("card-question.json");
    card["sections"][0]["items"][0]["actions"][0]["resolves_card"] = true.into();
    app.navigate_view(&key);
    app.on_msg(Msg::ViewCard { key: key.clone(), result: Ok(serde_json::from_value(card).unwrap()) });
    let need = app.card_view.as_ref().and_then(|view| view.title.clone()).unwrap_or_default();
    assert!(screen(&mut app, 120, 30).contains(&need), "the linked card is open");

    press(&mut app, KeyCode::Char('1'));
    assert_eq!(posted(&app).len(), 1);
    assert!(!app.view_navigation.contains(&key), "the view opened for it closes");
    assert!(app.card_view.as_ref().is_none_or(|view| view.key != key), "its detail is gone before the Mac answers");
}

#[test]
fn views_that_arrive_before_the_mac_confirms_never_bring_the_card_back() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (origin, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let mut home: ViewDocument = serde_json::from_value(view_fixture("home-tui.json")).unwrap();
    for version in ["same-as-before", "rendered-while-sending"] {
        home.version = version.into();
        app.on_msg(Msg::ViewHome(Ok(home.clone())));
        assert!(!app.inbox_keys().contains(&Key(origin.clone())), "{version}: the answer is still on its way");
    }
    home.version = "shown-when-confirmed".into();
    app.on_msg(Msg::ViewHome(Ok(home.clone())));
    app.on_msg(Msg::ViewAction { request, result: applied("done", "Merged") });
    app.on_msg(Msg::ViewHome(Ok(home.clone())));
    assert!(!app.inbox_keys().contains(&Key(origin.clone())), "the view the daemon had when it confirmed is older than the answer");
    let mut after = home.clone();
    after.version = "after-the-answer".into();
    for section in &mut after.sections {
        section.items.retain(|item| !matches!(item, alondra_core::views::Item::Card(card) if card.id == origin));
    }
    app.on_msg(Msg::ViewHome(Ok(after)));
    assert!(!app.inbox_keys().contains(&Key(origin)), "the daemon's newer view no longer lists it");
    assert!(app.optimistic.is_empty(), "nothing is left to remember");
}

#[test]
fn a_different_action_after_a_timeout_gets_its_own_command_id() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let first = posted(&app)[0].1.clone();
    app.on_msg(Msg::ViewAction { request: first.clone(), result: Err(Delivery::Unanswered) });
    let other = crate::client::Client::action_body("epoch", "other@1", None, serde_json::json!({}));
    app.submit_view_request(other, None);
    assert_ne!(posted(&app)[1].1["command_id"], first["command_id"]);
}

#[test]
fn a_changed_modal_waits_for_the_composer_and_never_takes_its_enter() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    open_question(&mut app);
    press(&mut app, KeyCode::Char('r'));
    assert!(app.composer.is_some());
    let request = crate::client::Client::action_body("epoch", "changes:pr@1-old", None, serde_json::json!({}));
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "result":{"kind":"changed", "title":"New commits",
            "next":{"id":"changes:pr@2-new", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
                "resolves_card":true}}
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction { request, result: Ok(changed) });
    assert!(app.modal.is_none(), "the composer keeps the keyboard");
    typed(&mut app, "hello");
    assert!(app.modal.is_none() && posted(&app).is_empty());
    press(&mut app, KeyCode::Esc);
    assert!(app.composer.is_none());
    assert!(app.modal.is_some(), "the question opens once the composer closes");
    assert!(
        posted(&app).iter().all(|(_, body)| body["action_id"] != "changes:pr@2-new"),
        "nothing was sent anyway"
    );
}

fn open_question(app: &mut App) {
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.sel = Some(Key(question_id()));
    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(serde_json::from_value(view_fixture("card-question.json")).unwrap()),
    });
}

#[test]
fn cancelling_a_composer_leaves_no_stale_stamp_for_the_next_action() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.sel = Some(Key(question_id()));
    let mut card = view_fixture("card-question.json");
    card["sections"][0]["items"][0]["reply"]["stamp"] = "Replying…".into();
    app.on_msg(Msg::ViewCard {
        key: format!("card:{}", question_id()),
        result: Ok(serde_json::from_value(card).unwrap()),
    });
    press(&mut app, KeyCode::Char('r'));
    assert!(app.composer.is_some());
    press(&mut app, KeyCode::Esc);
    let request = crate::client::Client::action_body("epoch", "changes:pr@1-old", None, serde_json::json!({}));
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "result":{"kind":"changed", "title":"New commits",
            "next":{"id":"changes:pr@2-new", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
                "resolves_card":true}}
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction { request, result: Ok(changed) });
    press(&mut app, KeyCode::Enter);
    assert_eq!(posted(&app).len(), 1);
    assert!(!flash(&app).contains("Replying…"), "not the cancelled answer's stamp: {}", flash(&app));
}

#[test]
fn the_cards_field_posts_the_questions_reply_with_the_text() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    open_question(&mut app);
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Neither");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    let sent = posted(&app);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "/actions");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("reply:"), "{sent:?}");
    assert_eq!(sent[0].1["input"]["text"], "Neither");
}

#[test]
fn the_pr_card_fixture_reads_in_the_terminal_with_the_servers_labels_order_and_outcome() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let fixture = view_fixture("card-pr.json");
    let item = &fixture["sections"][0]["items"][0];
    let merge = &item["actions"][0];
    let text = |value: &serde_json::Value| value.as_str().unwrap().to_owned();
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let origin = pr_card_fixture(&mut app);

    let drawn = screen(&mut app, 120, 40);
    let mut buttons: Vec<String> = item["actions"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, action)| format!(" {}  {}", index + 1, text(&action["label"])))
        .collect();
    buttons.push(format!(" m  More… ({})", item["more"].as_array().unwrap().len()));
    buttons.push(format!(" r  {}", text(&item["reply"]["label"])));
    assert_in_order(&drawn, &buttons);

    press(&mut app, KeyCode::Char('m'));
    let more: Vec<String> = item["more"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(index, action)| format!(" {}  {}", index + 1, text(&action["label"])))
        .collect();
    assert_in_order(&screen(&mut app, 120, 40), &more);
    press(&mut app, KeyCode::Esc);

    press(&mut app, KeyCode::Char('1'));
    assert!(merge.get("confirm").is_none() && merge.get("options").is_none(), "{merge}");
    assert!(app.modal.is_none() && app.card_menu.is_none(), "one press merges");
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["action_id"], merge["id"]);
    assert_eq!(sent[0].1["option_id"], merge["option_id"]);
    assert_eq!(flash(&app), text(&merge["stamp"]));
    assert!(!app.inbox_keys().contains(&Key(origin)));

    app.on_msg(Msg::ViewAction {
        request: sent[0].1.clone(),
        result: applied("started", "PR #12 is merging. You'll see it in its task when GitHub confirms."),
    });
    assert!(screen(&mut app, 120, 40).contains("PR #12 is merging. You'll see it in its task when GitHub confirms."));
}

fn assert_in_order(drawn: &str, texts: &[String]) {
    let mut at = 0;
    for text in texts {
        let found = drawn[at..].find(text.as_str()).unwrap_or_else(|| panic!("{text:?} after the previous one in:\n{drawn}"));
        at += found + text.len();
    }
}

fn pr_card_fixture(app: &mut App) -> String {
    let id = "hf-weekly-2026-09-30";
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
    app.reconcile();
    app.sel = Some(Key(id.into()));
    let mut card = view_fixture("card-pr.json");
    card["key"] = format!("card:{id}").into();
    app.on_msg(Msg::ViewCard {
        key: format!("card:{id}"),
        result: Ok(serde_json::from_value(card).unwrap()),
    });
    id.into()
}

#[test]
fn a_done_card_stays_away_and_comes_back_only_if_the_server_still_has_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (origin, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    app.on_msg(Msg::ViewAction { request, result: applied("started", "PR #12 is merging.") });
    assert!(!app.inbox_keys().contains(&Key(origin.clone())), "the server has not taken it away yet");
    assert!(!screen(&mut app, 120, 30).contains("Review the beta candidate"));

    let mut stale: ViewDocument = serde_json::from_value(view_fixture("home-tui.json")).unwrap();
    app.on_msg(Msg::ViewHome(Ok(stale.clone())));
    assert!(!app.inbox_keys().contains(&Key(origin.clone())), "a view rendered before the answer cannot bring it back");
    stale.version = "after-the-answer".into();
    app.on_msg(Msg::ViewHome(Ok(stale)));
    assert!(app.inbox_keys().contains(&Key(origin)), "a newer view still has it, so it is still there");
    assert!(screen(&mut app, 120, 30).contains("Review the beta candidate"));
}

#[test]
fn a_changed_modal_never_takes_the_enter_that_sends_the_composer() {
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    open_question(&mut app);
    press(&mut app, KeyCode::Char('r'));
    let request = crate::client::Client::action_body("epoch", "changes:pr@1-old", None, serde_json::json!({}));
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "result":{"kind":"changed", "title":"New commits",
            "next":{"id":"changes:pr@2-new", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
                "resolves_card":true}}
    }))
    .unwrap();
    app.on_msg(Msg::ViewAction { request, result: Ok(changed) });
    typed(&mut app, "Neither");
    app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL));
    let sent = posted(&app);
    assert_eq!(sent.len(), 1, "only the composer's own answer: {sent:?}");
    assert_eq!(sent[0].1["input"]["text"], "Neither");
    assert!(app.modal.is_some(), "the question opens now that the composer is closed");
    press(&mut app, KeyCode::Esc);
    assert_eq!(posted(&app).len(), 1, "Send anyway was never sent");
}

fn changed_from(request: serde_json::Value) -> Msg {
    let changed = view_action_result(serde_json::json!({
        "outcome":"changed", "result":{"kind":"changed", "title":"PR #12 has new commits",
            "next":{"id":"merge:fresh", "label":"Send anyway", "style":"primary",
                "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
                "resolves_card":true}}
    }))
    .unwrap();
    Msg::ViewAction { request, result: Ok(changed) }
}

#[test]
fn a_changed_question_waits_for_the_open_one_and_enter_answers_the_one_on_screen() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (a, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let merge_a = posted(&app)[0].1.clone();

    let b = question_id();
    app.sel = Some(Key(b.clone()));
    let mut card = view_fixture("card-question.json");
    card["sections"][0]["items"][0]["actions"][0]["confirm"] =
        serde_json::json!({"title": "Ship the concise copy?", "message": "The beta ships with it.", "button": "Ship it"});
    app.on_msg(Msg::ViewCard { key: format!("card:{b}"), result: Ok(serde_json::from_value(card).unwrap()) });
    press(&mut app, KeyCode::Char('1'));
    assert!(screen(&mut app, 120, 30).contains("Ship the concise copy?"));

    app.on_msg(changed_from(merge_a));
    let drawn = screen(&mut app, 120, 30);
    assert!(drawn.contains("Ship the concise copy?"), "B's question stays on screen: {drawn}");
    assert!(!drawn.contains("PR #12 has new commits"), "{drawn}");

    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 2);
    assert!(sent[1].1["action_id"].as_str().unwrap().starts_with("ans:"), "Enter answered B: {sent:?}");
    assert!(screen(&mut app, 120, 30).contains("PR #12 has new commits"), "A's question comes next");

    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 3);
    assert_eq!(sent[2].1["action_id"], "merge:fresh");
    assert!(app.optimistic.hides(&Key(a.clone())), "it is A that leaves");
}

#[test]
fn the_pairing_prompt_never_shows_over_a_question_that_holds_the_keys() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    merge_card(&mut app, |_| serde_json::json!({
        "id": "close_unmerged:hf-weekly-2026-09-30@7", "label": "Close without merging", "style": "destructive",
        "presentation": "button", "input": {"kind": "none"}, "attachments": false, "enabled": true,
        "confirm": {"title": "Close PR #12 without merging?", "message": "Nothing is merged.", "button": "Close PR"}
    }));
    app.pairing.push(PendingDevice {
        id: "d-1".into(),
        name: "Alice's iPhone".into(),
        code: "4821".into(),
        expires_at_ms: now_millis() + 600_000,
    });
    assert!(screen(&mut app, 120, 30).contains("Allow “Alice's iPhone” to use Alondra?"));

    press(&mut app, KeyCode::Char('1'));
    let drawn = screen(&mut app, 120, 30);
    assert!(drawn.contains("Close PR #12 without merging?"), "{drawn}");
    assert!(!drawn.contains("Allow “Alice's iPhone”"), "the prompt that does not get the keys is not shown: {drawn}");

    press(&mut app, KeyCode::Esc);
    assert_eq!(app.pairing.len(), 1, "Esc answered the close question, not the device");
    assert!(screen(&mut app, 120, 30).contains("Allow “Alice's iPhone” to use Alondra?"));
}

#[test]
fn a_card_that_comes_back_leaves_the_selection_where_the_user_moved_it() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (origin, _) = served_merge_card(&mut app);
    press(&mut app, KeyCode::Char('1'));
    let request = posted(&app)[0].1.clone();
    let landed = app.sel.clone();
    press(&mut app, KeyCode::Char('j'));
    let chosen = app.sel.clone();
    assert_ne!(chosen, landed, "the user moved");
    app.on_msg(Msg::ViewAction {
        request,
        result: view_action_result(serde_json::json!({
            "outcome":"rejected", "result":{"kind":"refused", "title":"PR #11 must merge first."}
        })),
    });
    assert!(app.inbox_keys().contains(&Key(origin)), "the card is back");
    assert_eq!(app.sel, chosen, "and the selection stays where the user put it");
}

fn plan_card() -> (serde_json::Value, String) {
    let mut view = view_fixture("card-question.json");
    let id = question_id();
    let changes = format!("changes:{id}@1");
    let card = &mut view["sections"][0]["items"][0];
    card["kind"] = "review".into();
    card["actions"] = serde_json::json!([
        {"id":format!("accept:{id}@1"), "label":"Approve", "style":"primary", "role":"advance",
         "presentation":"button", "input":{"kind":"none"}, "attachments":false, "enabled":true,
         "stamp":"Approved"},
        {"id":changes, "label":"Request changes", "style":"secondary", "role":"neutral",
         "presentation":"button",
         "input":{"kind":"text", "placeholder":"Describe the changes", "required":true, "multiline":true},
         "attachments":true, "comments":{"kind":"blocks"}, "enabled":true, "stamp":"Changes requested"}
    ]);
    (view, changes)
}

#[test]
fn reader_comments_on_a_plan_go_to_request_changes_by_its_button_number() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let (view, changes) = plan_card();
    app.view_navigation.push(format!("card:{}", question_id()));
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    press(&mut app, KeyCode::Char('C'));
    typed(&mut app, "Split step two");
    press(&mut app, KeyCode::Enter);

    press(&mut app, KeyCode::Char('2'));
    assert!(!flash(&app).contains("sends no words"), "{}", flash(&app));
    assert!(
        matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { action_id, option_id: None, .. }) if *action_id == changes),
        "the composer writes Request changes"
    );
    assert!(app.composer.as_ref().is_some_and(|composer| composer.input.text().contains("Split step two")));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "/actions");
    assert_eq!(sent[0].1["action_id"], changes.as_str());
    assert!(sent[0].1["input"]["text"].as_str().unwrap().contains("Split step two"));
}

#[test]
fn reader_comments_on_a_question_go_with_the_option_pressed_at_once() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.view_navigation.push(format!("card:{}", question_id()));
    app.card_view = Some(serde_json::from_value(view_fixture("card-question.json")).unwrap());
    app.open_reader();
    press(&mut app, KeyCode::Char('C'));
    typed(&mut app, "Only on the onboarding screen");
    press(&mut app, KeyCode::Enter);

    press(&mut app, KeyCode::Char('2'));
    assert!(app.composer.is_none(), "the option answers at once");
    let sent = posted(&app);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert!(sent[0].1["action_id"].as_str().unwrap().starts_with("ans:"), "{sent:?}");
    assert_eq!(sent[0].1["option_id"], "warm");
    assert!(sent[0].1["input"]["text"].as_str().unwrap().contains("Only on the onboarding screen"), "{sent:?}");
}

fn pull_request_detail(app: &mut App) -> String {
    let view = view_fixture("pull-request-mac.json");
    let key = view["key"].as_str().unwrap().to_owned();
    let action_id = view["sections"].as_array().unwrap().iter()
        .flat_map(|section| section["items"].as_array().unwrap())
        .filter_map(|item| item["actions"].as_array())
        .flatten()
        .find_map(|action| action["id"].as_str().filter(|id| id.starts_with("merge:")))
        .unwrap()
        .to_owned();
    app.view_navigation.push(key);
    app.card_view = Some(serde_json::from_value(view).unwrap());
    action_id
}

#[test]
fn merge_from_a_pull_request_detail_sends_the_method_its_button_says() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let action_id = pull_request_detail(&mut app);
    press(&mut app, KeyCode::Char('1'));
    assert!(app.card_menu.is_none(), "the method is set in Settings: no menu");
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1["action_id"], action_id.as_str());
    assert_eq!(sent[0].1["option_id"], "squash");
}

fn settled(card: &serde_json::Value, why: &str) -> ViewDocument {
    let mut view = card.clone();
    view["view"] = alondra_core::views::SETTLED_VIEW.into();
    view["sections"] = serde_json::json!([]);
    view["empty"] = serde_json::json!({"text": why, "tone": "neutral"});
    serde_json::from_value(view).unwrap()
}

#[test]
fn a_card_settled_elsewhere_closes_and_says_why_instead_of_drawing_it() {
    let card = view_fixture("card-question.json");
    let key = card["key"].as_str().unwrap().to_owned();
    let need = card["sections"][0]["items"][0]["need"].as_str().unwrap().to_owned();
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.navigate_view(&key);
    app.on_msg(Msg::ViewCard { key: key.clone(), result: Ok(serde_json::from_value(card.clone()).unwrap()) });
    assert!(app.card_view.is_some());
    app.on_msg(Msg::ViewCard { key: key.clone(), result: Ok(settled(&card, "Ya respondida")) });
    assert!(app.card_view.is_none());
    assert!(app.view_navigation.is_empty(), "{:?}", app.view_navigation);
    assert_eq!(flash(&app), "Ya respondida");
    assert!(!screen(&mut app, 120, 30).contains(&need));

    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Inbox;
    app.sel = Some(Key(question_id()));
    app.on_msg(Msg::ViewCard { key, result: Ok(settled(&card, "Ya no hace falta")) });
    assert!(app.card_view.is_none());
    assert_eq!(flash(&app), "Ya no hace falta");
    assert!(app.optimistic.hides(&Key(question_id())), "the inbox leaves the settled card");
}

#[test]
fn a_know_card_detail_shows_its_whole_body_with_the_command_block() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let view = serde_json::from_value(view_fixture("card-know-command-es-tui.json")).unwrap();
    let lines = text_of(&crate::tui::view_render::document_lines(&view, 100, false, false));
    let at = |text: &str| lines.iter().position(|line| line.contains(text)).unwrap_or_else(|| panic!("{text} missing: {lines:#?}"));
    let headline = at("Pega este comando en el terminal");
    let intro = at("Ejecútalo una vez en el terminal del Mac");
    let command = at("│ cp ~/.claude.json ~/.claude.json.bak-$(date +%s) && python3 - <<'PY'");
    let script = at("│ print(\"Confiados\")");
    let closing = at("Si clonas repos nuevos en ~/Projects, vuelve a ejecutarlo.");
    assert!(headline < intro && intro < command && command < script && script < closing, "{lines:#?}");
}

fn reader_with_a_comment(app: &mut App) -> String {
    app.loaded = true;
    let mut view = view_fixture("card-question.json");
    let id = question_id();
    view["sections"][0]["items"][0]["actions"] = serde_json::json!([
        {"id":format!("changes:{id}@1"), "label":"Request changes", "style":"secondary",
         "presentation":"button", "input":{"kind":"text","placeholder":"Describe", "required":true,"multiline":true},
         "attachments":true,"enabled":true,"key":"s","resolves_card":true}
    ]);
    let key = format!("card:{id}");
    app.view_navigation.push(key.clone());
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    press(app, KeyCode::Char('c'));
    typed(app, "Tighter");
    press(app, KeyCode::Enter);
    key
}

fn settled_card(key: &str) -> alondra_core::views::View {
    let card = view_fixture("card-question.json");
    serde_json::from_value(serde_json::json!({
        "protocol": card["protocol"], "data_epoch": "test-epoch", "view": alondra_core::views::SETTLED_VIEW,
        "key": key, "client": "tui", "version": "", "cursor": "", "sections": [],
        "empty": {"text": "Respondida", "tone": "neutral"}, "status": card["status"], "notices": []
    }))
    .unwrap()
}

#[test]
fn sent_reader_comments_close_the_reader_and_say_they_went_not_that_the_card_is_gone() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let key = reader_with_a_comment(&mut app);
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert!(sent[0].1["input"]["text"].as_str().unwrap().contains("Tighter"));

    assert!(app.reader.is_none(), "the reader closes the moment the comments leave");
    assert!(!app.view_navigation.contains(&key), "and so does the card they answer");
    app.on_msg(Msg::ViewCard { key: key.clone(), result: Ok(settled_card(&key)) });
    let sending = screen(&mut app, 140, 30);
    assert!(!sending.contains("enviando") && !sending.contains("ya no está en la bandeja"), "{sending}");

    app.on_msg(Msg::ViewAction { request: sent[0].1.clone(), result: applied("done", "Cambios solicitados") });
    assert!(app.reviews.is_empty(), "sent comments are not left as unsent");
    assert_eq!(flash(&app), "Comentarios enviados a Restore the native review experience");
    let after = screen(&mut app, 140, 30);
    assert!(!after.contains("ya no está en la bandeja") && !after.contains("sin enviar"), "{after}");
}

#[test]
fn reader_comments_the_mac_did_not_take_stay_in_the_reader() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    reader_with_a_comment(&mut app);
    press(&mut app, KeyCode::Char('s'));
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    app.on_msg(Msg::ViewAction { request: sent[0].1.clone(), result: applied("refused", "Ya no está disponible") });
    assert_eq!(app.reviews.values().map(|review| review.count()).sum::<usize>(), 1, "the comments wait for the user to open them again");
    assert!(app.review_sends.is_empty());
}

#[test]
fn comments_on_a_card_that_went_away_are_sent_to_its_tasks_node_with_their_quotes() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    let key = reader_with_a_comment(&mut app);
    app.on_msg(Msg::ViewCard { key: key.clone(), result: Ok(settled_card(&key)) });
    let gone = screen(&mut app, 260, 30);
    assert!(gone.contains("s: enviar tus comentarios a Restore the native review experience"), "{gone}");

    press(&mut app, KeyCode::Char('s'));
    let sent = posted(&app);
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].0, "task:t-651c38484cd2f34e74d5107ba05f7f15", "to the node of the card's task");
    let text = sent[0].1["text"].as_str().unwrap();
    assert!(text.starts_with("Mis comentarios sobre «"), "{text}");
    assert!(text.contains("Tighter"), "{text}");

    app.on_msg(Msg::Posted { key: Some(Key(key)), result: Ok("Comentarios enviados a Restore the native review experience".into()) });
    assert!(app.reader.is_none() && app.reviews.is_empty());
    assert_eq!(flash(&app), "Comentarios enviados a Restore the native review experience");
}

/// The TUI hides a card on the server's word alone: an action that takes it out hides it at
/// once, and one that leaves it waiting (asking to clarify, a message to its node) never does.
/// It used to hide every card it acted on and bring back, fifteen seconds later, those the
/// server still listed.
#[test]
fn a_card_leaves_the_list_only_when_its_action_says_it_takes_it_out() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    for resolves in [true, false] {
        let (tx, _rx) = std::sync::mpsc::sync_channel(8);
        let mut app = App::new(tx);
        app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(view_fixture("home-tui.json")).unwrap())));
        app.reconcile();
        app.sel = Some(Key(question_id()));
        let mut card = view_fixture("card-question.json");
        card["sections"][0]["items"][0]["actions"][0]["resolves_card"] = resolves.into();
        app.on_msg(Msg::ViewCard { key: format!("card:{}", question_id()), result: Ok(serde_json::from_value(card).unwrap()) });
        press(&mut app, KeyCode::Char('1'));
        assert_eq!(posted(&app).len(), 1, "the action is sent either way");
        assert_eq!(app.optimistic.hides(&Key(question_id())), resolves, "resolves_card {resolves}");
        assert_eq!(app.inbox_keys().contains(&Key(question_id())), !resolves);
    }
}

/// The user wrote «Hazlo tú» under the orchestrator's Decide card and the card stayed: its reply went
/// out as a new message to the orchestrator. The reply field of an open question answers it, so
/// the card leaves the list the moment it is sent.
#[test]
fn words_written_under_an_orchestrator_decide_answer_it_and_take_it_out() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let card = view_fixture("card-orchestrator-decide-tui.json");
    let decide = card["sections"][0]["items"][0].clone();
    let id = decide["id"].as_str().unwrap().to_owned();
    let mut home = view_fixture("home-tui.json");
    home["sections"][0]["items"].as_array_mut().unwrap().insert(0, decide);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::ViewHome(Ok(serde_json::from_value(home).unwrap())));
    app.reconcile();
    app.sel = Some(Key(id.clone()));
    app.on_msg(Msg::ViewCard { key: format!("card:{id}"), result: Ok(serde_json::from_value(card).unwrap()) });
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Hazlo tú");
    press(&mut app, KeyCode::Enter);
    let sent = posted(&app);
    assert_eq!(sent.len(), 1);
    let action = sent[0].1["action_id"].as_str().unwrap();
    assert!(action.starts_with(&format!("reply:{id}@")), "the words answer the card, never a new message: {action}");
    assert_eq!(sent[0].1["input"]["text"], "Hazlo tú");
    assert!(app.optimistic.hides(&Key(id.clone())), "the answered card leaves at once");
    assert!(!app.inbox_keys().contains(&Key(id)));
}

#[test]
fn comments_a_node_takes_after_its_turn_count_as_sent_and_an_uncertain_handoff_does_not() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let sent = || "Comments sent".to_owned();
    assert_eq!(comments_delivered(&serde_json::json!({"delivery_state": "handed"}), sent()), Ok(sent()));
    assert_eq!(comments_delivered(&serde_json::json!({"delivery_state": "dispatching"}), sent()), Ok(sent()),
        "a node of the Agent SDK takes them once its turn lets it: they are on their way, not lost");
    assert_eq!(comments_delivered(&serde_json::json!({}), sent()), Ok(sent()));
    assert!(comments_delivered(&serde_json::json!({"delivery_state": "uncertain"}), sent()).is_err());
}

#[test]
fn a_numbered_list_on_a_card_reads_as_a_list_and_other_facts_keep_their_label() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let tasks: ViewDocument = serde_json::from_value(view_fixture("card-outcome-tasks-es-tui.json")).unwrap();
    let lines = text_of(&crate::tui::view_render::document_lines(&tasks, 100, false, false));
    assert!(lines.iter().any(|line| line == "1. Guardar las notas con su versión · Prioridad alta"), "{lines:#?}");
    assert!(lines.iter().any(|line| line == "3. Sincronizar desde el Mac · Prioridad baja · después de 1"), "{lines:#?}");
    let done: ViewDocument = serde_json::from_value(view_fixture("card-outcome-done-es-tui.json")).unwrap();
    let lines = text_of(&crate::tui::view_render::document_lines(&done, 100, false, false));
    assert!(lines.iter().any(|line| line.starts_with("Cancelada: Documentar las cuentas")), "{lines:#?}");
}

#[test]
fn card_title_contract_tui_uses_fields_without_text_comparison() {
    let mut value = view_fixture("card-question.json");
    value["title"] = serde_json::json!("A wrapper title that must not be shown");
    value["sections"][0]["items"][0]["title"] = serde_json::json!("Release design");
    value["sections"][0]["items"][0]["need"] = serde_json::json!("Choose a design");
    value["sections"][0]["items"][0]["title_is_separate"] = serde_json::json!(true);
    let view = serde_json::from_value(value.clone()).unwrap();
    let kind = crate::tui::view_render::document_kind(&view).unwrap();
    assert!(!kind.is_empty());
    let lines = text_of(&crate::tui::view_render::document_lines(&view, 100, false, false)).join("\n");
    assert!(!lines.contains(&kind.to_uppercase()), "the kind belongs to the pane frame: {lines}");
    assert_eq!(lines.matches("Release design").count(), 1, "{lines}");
    assert_eq!(lines.matches("Choose a design").count(), 1, "{lines}");
    assert!(!lines.contains("A wrapper title"), "{lines}");
    let mut equal = value.clone();
    equal["sections"][0]["items"][0]["need"] = serde_json::json!("Release design");
    let equal = serde_json::from_value(equal).unwrap();
    let lines = text_of(&crate::tui::view_render::document_lines(&equal, 100, false, false)).join("\n");
    assert_eq!(lines.matches("Release design").count(), 2, "explicit fields remain even when their text is equal: {lines}");
    value["sections"][0]["items"][0]["need"] = serde_json::json!("Legacy metadata is not a request");
    for signal in [Some(false), None] {
        let fields = value["sections"][0]["items"][0].as_object_mut().unwrap();
        if let Some(signal) = signal {
            fields.insert("title_is_separate".into(), serde_json::json!(signal));
        } else {
            fields.remove("title_is_separate");
        }
        let view = serde_json::from_value(value.clone()).unwrap();
        assert!(!crate::tui::view_render::typed_card(&view).unwrap().title_is_separate);
        let lines = text_of(&crate::tui::view_render::document_lines(&view, 100, false, false)).join("\n");
        assert_eq!(lines.matches("Release design").count(), 1, "{lines}");
        assert!(!lines.contains("Legacy metadata"), "{lines}");
    }
    let view = serde_json::from_value(value).unwrap();
    let mut card = crate::tui::view_render::typed_card(&view).unwrap().clone();
    card.icon = alondra_core::views::Icon::PullRequest;
    card.state = Some(alondra_core::views::Label { text: "Ready for review".into(), tone: alondra_core::views::Tone::Info, elapsed: None });
    let lines = text_of(&crate::tui::view_render::card_lines(&card, 100));
    assert_eq!(lines[0], "Release design");
    assert!(lines.iter().any(|line| line.starts_with("Ready for review")), "{lines:#?}");
}
