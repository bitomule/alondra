//! What the terminal did before the domain v2 cutover and still does: every scenario runs on a
//! projection built by the core rules, the runtime and route fixtures in `fixtures/client-v2`,
//! and checks the exact command or request the key sends.

use super::*;
use crate::runtime::{Runtime, fixture};

fn runtime() -> Runtime {
    serde_json::from_value(fixture("runtime.json")).unwrap()
}

fn view_fixture(name: &str) -> serde_json::Value {
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

fn with_runtime(app: &mut App, runtime: Runtime) {
    app.on_msg(Msg::Runtime(Some(Box::new(runtime))));
    app.reconcile();
}

fn posted(app: &App) -> Vec<(String, serde_json::Value)> {
    app.posted.borrow().clone()
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
    assert_eq!(rows, ["i-1", "i-3", "pr-1", "i-5", "i-6", "i-2"]);
    let list = screen(&mut app, 120, 26);
    for label in [
        "WAITING FOR YOU",
        "PULL REQUESTS",
        "TO READ",
        "Build a feature",
    ] {
        assert!(list.contains(label), "{label}: {list}");
    }

    app.on_msg(Msg::ViewCard {
        key: "card:i-1".into(),
        result: Ok(serde_json::from_value(view_fixture("card-question.json")).unwrap()),
    });
    let detail = screen(&mut app, 120, 26);
    assert!(
        detail.contains("Body 0") && detail.contains("Need more context"),
        "{detail}"
    );
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
    assert!(app.schedules.is_empty());
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
    view["sections"][0]["items"][0]["actions"] = serde_json::json!([
        {"id":"changes:i-1@1", "label":"Request changes", "style":"secondary",
         "presentation":"button", "input":{"kind":"text","placeholder":"Describe", "required":true,"multiline":true},
         "attachments":true,"enabled":true,"key":"s"},
        {"id":"read:i-1@1", "label":"Mark as read", "style":"secondary",
         "presentation":"button", "input":{"kind":"none"},
         "attachments":false,"enabled":true,"key":"x"}
    ]);
    app.view_navigation.push("card:i-1".into());
    app.card_view = Some(serde_json::from_value(view).unwrap());
    app.open_reader();
    assert!(app.reader.is_some());
    press(&mut app, KeyCode::Char('s'));
    assert!(matches!(app.composer.as_ref().map(|composer| &composer.target),
        Some(Target::ViewAction { action_id, .. }) if action_id == "changes:i-1@1"));
    app.composer = None;
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(posted(&app)[0].1["action_id"], "read:i-1@1");
}

#[test]
fn view_number_key_posts_the_matching_option_id_and_epoch() {
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
        key: "card:i-1".into(),
        result: Ok(serde_json::from_value(card).unwrap()),
    });

    press(&mut app, KeyCode::Char('2'));

    let requests = posted(&app);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/actions");
    assert_eq!(requests[0].1["option_id"], "keep-existing");
    assert_eq!(requests[0].1["action_id"], "ans:i-1@1");
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
    assert_eq!(requests[0].1["action_id"], "ans:i-1@1");
    assert_eq!(requests[0].1["data_epoch"], "test-epoch");
}

#[test]
fn view_form_opens_editor_with_its_bound_action_and_epoch() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = std::sync::mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.screen = Screen::Tasks;
    app.task_sel = Some(tasks::TaskKey::Task(TaskId("t-1".into())));
    let mut card = view_fixture("card-question.json");
    card["key"] = serde_json::json!("task:t-1");
    let mut submit = card["sections"][0]["items"][0]["actions"][0].clone();
    submit["id"] = serde_json::json!("sched_save:s-new@1");
    submit["label"] = serde_json::json!("Save schedule");
    submit["key"] = serde_json::json!("e");
    submit["attachments"] = serde_json::json!(true);
    card["sections"][0]["items"].as_array_mut().unwrap().push(serde_json::json!({
        "type": "form",
        "id": "schedule",
        "fields": [
            {"id": "title", "label": "Title", "kind": {"kind": "text", "placeholder": null}, "value": "Daily check", "required": true, "reload_on_change": false},
            {"id": "uploads", "label": "Attachments", "kind": {"kind": "attachments"}, "value": [], "required": false, "reload_on_change": false}
        ],
        "submit": submit
    }));
    app.on_msg(Msg::ViewCard {
        key: "task:t-1".into(),
        result: Ok(serde_json::from_value(card).unwrap()),
    });

    assert!(matches!(
        app.on_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE)),
        Flow::EditViewForm { action_id, data_epoch, fields, attachment_field, .. }
            if action_id == "sched_save:s-new@1"
                && data_epoch == "test-epoch"
                && fields.len() == 2
                && attachment_field.as_deref() == Some("uploads")
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

fn attachment(id: &str, name: &str) -> AttachmentRef {
    AttachmentRef {
        id: id.into(),
        name: name.into(),
        content_digest: "d".repeat(64),
        content_reference: format!("files/{id}"),
        bytes: 3,
    }
}

fn composing(app: &App) -> Option<&Target> {
    app.composer.as_ref().map(|c| &c.target)
}

struct Scratch(std::path::PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch(tag: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("alondra-parity-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

#[test]
fn a_reply_to_information_goes_to_its_agent_and_reads_it_in_one_request() {
    let mut world = World::base(false);
    world.information("info-1", "Tests pass");
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('r'));
    assert!(
        matches!(composing(&app), Some(Target::Message { message, .. }) if message.mark_read),
        "r on information opens a reply"
    );
    typed(&mut app, "Thanks, ship it");
    press(&mut app, KeyCode::Enter);
    assert!(sent(&app).is_empty(), "a reply is not a domain command");
    assert_eq!(
        posted(&app),
        [(
            "/agents/agent-a/messages".to_string(),
            serde_json::json!({"text": "Thanks, ship it", "attachments": [], "kind": "reply", "mark_read": true, "interaction_id": "info-1"})
        )]
    );
    assert!(
        app.inbox_keys().is_empty(),
        "the replied information leaves the inbox while the daemon reads it"
    );
}

#[test]
fn a_question_without_a_text_option_is_replied_to_by_message_and_stays_open() {
    let mut world = World::base(false);
    world.question("question-1", "Keep the flag?", &[("yes", false)]);
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Only on beta");
    press(&mut app, KeyCode::Enter);
    assert!(sent(&app).is_empty(), "no option is chosen for the user");
    assert_eq!(posted(&app)[0].1["kind"], "reply");
    assert_eq!(posted(&app)[0].1["interaction_id"], "question-1");
    assert_eq!(posted(&app)[0].1["mark_read"], false);
    assert_eq!(
        app.inbox_keys().len(),
        1,
        "the question stays until its agent asks again"
    );
}

#[test]
fn more_context_sends_the_question_back_and_takes_an_empty_note() {
    let mut world = World::base(false);
    world.question("question-1", "Which name?", &[("yes", false)]);
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('c'));
    assert!(
        matches!(composing(&app), Some(Target::Message { message, .. }) if message.kind == ops::MessageKind::MoreContext)
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        posted(&app),
        [(
            "/agents/agent-a/messages".to_string(),
            serde_json::json!({"text": "", "attachments": [], "kind": "more_context", "mark_read": false, "interaction_id": "question-1"})
        )]
    );
}

#[test]
fn the_inbox_lists_what_waits_then_pull_requests_then_what_to_read() {
    let mut world = World::base(true);
    world.information("info-1", "Docs updated");
    world.question("question-2", "Keep the flag?", &[("yes", false)]);
    world.pull_request();
    world.present("delivery-1", World::pr());
    let mut app = app_on(&world);
    assert_eq!(
        app.inbox_keys()
            .iter()
            .map(|k| k.0.as_str())
            .collect::<Vec<_>>(),
        [
            "interaction:question-2",
            "pr:pr-1",
            "interaction:delivery-1",
            "interaction:info-1"
        ]
    );
    let shown = screen(&mut app, 120, 30);
    for header in ["WAITING FOR YOU 1", "PULL REQUESTS 1", "TO READ 1"] {
        assert!(shown.contains(header), "{header}: {shown}");
    }
}

#[test]
fn a_question_answered_from_another_device_leaves_the_inbox() {
    let mut world = World::base(false);
    world.question("question-1", "Keep the flag?", &[("yes", false)]);
    let mut app = app_on(&world);
    assert_eq!(
        app.inbox_keys()
            .iter()
            .map(|k| k.0.as_str())
            .collect::<Vec<_>>(),
        ["interaction:question-1"]
    );
    world.run(
        "device:phone",
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "question-1".into(),
            option_id: "yes".into(),
            inputs: Default::default(),
        },
    );
    refresh(&mut app, &world);
    assert!(app.inbox_keys().is_empty(), "{:?}", app.inbox_keys());
    let shown = screen(&mut app, 120, 30);
    assert!(
        !shown.contains("WAITING FOR YOU") && !shown.contains("Keep the flag?"),
        "{shown}"
    );
}

#[test]
fn a_pr_root_takes_changes_and_questions_but_never_an_authorization() {
    let mut world = super::pr_with_question_child();
    let mut app = app_on(&world);
    select(&mut app, "pr:pr-1");
    press(&mut app, KeyCode::Char('r'));
    assert!(
        matches!(composing(&app), Some(Target::Feedback { presentation, .. }) if presentation.0 == "delivery-1"),
        "changes go as feedback on the delivery that presents it"
    );
    typed(&mut app, "Rename the flag");
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(&sent(&app)[0].command, Command::SubmitFeedback { presentation_id, body, .. } if presentation_id.0 == "delivery-1" && body == "Rename the flag")
    );
    deliver(&mut app, &mut world).unwrap();
    select(&mut app, "pr:pr-1");
    press(&mut app, KeyCode::Char('p'));
    typed(&mut app, "Why squash?");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        posted(&app)[0],
        (
            "/agents/agent-a/messages".to_string(),
            serde_json::json!({"text": "Why squash?", "attachments": [], "kind": "pr_question", "mark_read": false, "pr_id": "pr-1"})
        )
    );
    select(&mut app, "pr:pr-1");
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(posted(&app).len(), 1, "a PR has nothing to mark read");
    assert!(app.inbox_keys().contains(&Key("pr:pr-1".into())));
    assert_eq!(
        sent(&app).len(),
        1,
        "none of these is a merge authorization"
    );
    assert!(world.state.merge_authorizations.is_empty());
}

#[test]
fn conflicts_the_runtime_reports_are_resolved_from_the_pr_root_by_its_agent() {
    let world = super::pr_with_question_child();
    let mut app = app_on(&world);
    select(&mut app, "pr:pr-1");
    press(&mut app, KeyCode::Char('1'));
    assert!(
        posted(&app).is_empty() && flash(&app).contains("Select the item under it"),
        "without the runtime the root only groups"
    );
    with_runtime(&mut app, runtime());
    select(&mut app, "pr:pr-1");
    let shown = screen(&mut app, 140, 34);
    assert!(
        shown.contains("Conflicts with main") && shown.contains("resolve conflicts"),
        "{shown}"
    );
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(
        posted(&app),
        [(
            "/pull-requests/pr-1/resolve-conflicts".to_string(),
            serde_json::json!({})
        )]
    );
    assert!(sent(&app).is_empty());
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
        Key(pr.into()),
        PrId(pr.into()),
        154,
        Ok("Reviewed.\n".into()),
    );
    assert!(posted(&app).is_empty() && flash(&app).contains("without comments"));
    app.reviewed(
        Key(pr.into()),
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
fn without_a_local_clone_the_diff_opens_in_the_reader() {
    let mut world = World::base(true);
    world.run(
        "agent-a",
        Command::RegisterPr {
            task_id: "task-1".into(),
            pr_id: "pr-1".into(),
            host: "github.com".into(),
            repository: "owner/alondra-no-clone-here".into(),
            number: 7,
            head: "head1".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    );
    world.present("delivery-1", World::pr());
    let mut app = app_on(&world);
    select(&mut app, "interaction:delivery-1");
    assert!(matches!(
        app.on_key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE)),
        Flow::Go
    ));
    assert!(flash(&app).contains("Loading the PR diff"));
    app.on_msg(Msg::Diff {
        key: Key("interaction:delivery-1".into()),
        result: Ok(serde_json::json!({"diff": "+fn main() {}", "head": "head1"})),
    });
    let shown = screen(&mut app, 120, 30);
    assert!(shown.contains("fn main()"), "{shown}");
}

#[test]
fn attachments_are_listed_loaded_and_drawn_in_the_detail() {
    let mut world = World::base(false);
    let options = vec![QuestionOption {
        id: "yes".into(),
        label: "Yes".into(),
        description: None,
        inputs: vec![],
    }];
    world.run(
        "agent-a",
        Command::AskQuestion {
            attachments: vec![
                attachment("att-1", "plan.md"),
                attachment("att-2", "shot.png"),
            ],
            interaction_id: "question-1".into(),
            scope: Scope::Task {
                task_id: "task-1".into(),
            },
            recipient: Recipient::User,
            body: "Approve the plan?".into(),
            blocking: true,
            options,
        },
    );
    let mut app = app_on(&world);
    let loading = screen(&mut app, 140, 40);
    assert!(
        loading.contains("📎 2") && loading.contains("plan.md") && loading.contains("Loading…"),
        "{loading}"
    );
    app.on_msg(Msg::File {
        route: crate::client::attachment_content(&attachment("att-1", "plan.md")),
        result: Ok(files::FileBody::classify(b"# Plan\n\nStep one".to_vec())),
    });
    app.on_msg(Msg::File {
        route: crate::client::attachment_content(&attachment("att-2", "shot.png")),
        result: Ok(files::FileBody::classify(files::png(4, 3))),
    });
    let loaded = screen(&mut app, 140, 40);
    assert!(
        loaded.contains("Step one") && loaded.contains("Image 4×3"),
        "{loaded}"
    );
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(
        flash(&app),
        "Opened with its app",
        "v opens the image, the text is already on screen"
    );
}

#[test]
fn a_dropped_file_waits_for_its_copy_and_goes_with_the_feedback() {
    let mut world = World::base(false);
    world.present("delivery-1", World::text());
    let dir = scratch("drop");
    let file = dir.0.join("shot.png");
    std::fs::write(&file, files::png(4, 3)).unwrap();
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('r'));
    app.on_paste(&file.to_string_lossy());
    assert_eq!(app.composer.as_ref().map(|c| c.chips.len()), Some(1));
    typed(&mut app, "See the screenshot");
    press(&mut app, KeyCode::Enter);
    assert!(
        sent(&app).is_empty() && flash(&app).contains("Still copying"),
        "nothing leaves before the copy is stored"
    );
    app.on_msg(Msg::Uploaded {
        seq: 1,
        result: Ok(attachment("att-9", "shot.png")),
    });
    press(&mut app, KeyCode::Enter);
    assert!(
        matches!(&sent(&app)[0].command, Command::SubmitFeedback { attachments, .. } if *attachments == [attachment("att-9", "shot.png")])
    );
    deliver(&mut app, &mut world).unwrap();
    assert!(
        matches!(&world.state.interactions[&InteractionId::from("delivery-1")].payload, InteractionPayload::Delivery { feedback, .. } if feedback[0].attachments.len() == 1)
    );
    app.compose(Target::General { send: false }, "");
    app.on_paste(&file.to_string_lossy());
    app.on_msg(Msg::Uploaded {
        seq: 2,
        result: Err("disk full".into()),
    });
    assert!(
        app.composer.as_ref().is_some_and(|c| c.chips.is_empty())
            && flash(&app).contains("Could not attach shot.png")
    );
}

#[test]
fn new_work_opens_the_server_owned_request_form() {
    let world = World::base(false);
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("new_request")
    );
    assert!(app.composer.is_none());
    assert!(posted(&app).is_empty());
}

#[test]
fn new_work_before_the_sidebar_arrives_still_uses_the_server_form() {
    let world = World::base(false);
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('n'));
    assert_eq!(
        app.view_navigation.last().map(String::as_str),
        Some("new_request")
    );
    assert!(app.composer.is_none());
    assert!(posted(&app).is_empty());
}

#[test]
fn the_header_shows_runtime_gauges_and_update_without_the_delivery_journal() {
    let world = World::base(false);
    let mut app = app_on(&world);
    let bare = screen(&mut app, 200, 12);
    assert!(
        !bare.contains("working") && !bare.contains("alondra update"),
        "{bare}"
    );
    let local = fixture("runtime.json");
    let daemon = serde_json::json!({"version": "0.1.11", "update": local["update"], "machine": local["machine"], "usage": local["usage"], "preflight_rejected": null});
    app.on_msg(Msg::Daemon(Some(Box::new(
        serde_json::from_value(daemon).unwrap(),
    ))));
    let from_daemon = screen(&mut app, 200, 12);
    for text in ["↑ 0.1.12 · alondra update", "0 working", "Claude", "RAM"] {
        assert!(
            from_daemon.contains(text),
            "the local /daemon alone fills the header: {text}: {from_daemon}"
        );
    }
    with_runtime(&mut app, runtime());
    let shown = screen(&mut app, 200, 12);
    for text in [
        "2 working",
        "Claude",
        "RAM",
        "CPU",
        "↑ 0.1.12 · alondra update",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
    assert!(
        !shown.contains("not confirmed") && !shown.contains(":deliveries"),
        "{shown}"
    );
}

#[test]
fn the_orchestrator_strip_recovers_only_on_a_second_press_of_the_same_key() {
    let world = World::base(false);
    let mut app = app_on(&world);
    with_runtime(&mut app, runtime());
    app.show(Screen::Fleet);
    typed(&mut app, "gg");
    assert!(app.orchestrator_selected(), "the orchestrator comes first");
    let shown = screen(&mut app, 140, 30);
    assert!(
        shown.contains("Orchestrator")
            && shown.contains("needs attention")
            && shown.contains(" App"),
        "{shown}"
    );
    press(&mut app, KeyCode::Char('R'));
    assert!(flash(&app).contains("press R again"));
    press(&mut app, KeyCode::Char('j'));
    press(&mut app, KeyCode::Char('k'));
    press(&mut app, KeyCode::Char('R'));
    assert!(posted(&app).is_empty(), "moving away starts over");
    press(&mut app, KeyCode::Char('R'));
    assert_eq!(
        posted(&app),
        [(
            "/orchestrator/retry".to_string(),
            serde_json::json!({"run_id": "run-7"})
        )]
    );
    press(&mut app, KeyCode::Char('x'));
    assert!(
        flash(&app).contains("manages the orchestrator"),
        "the orchestrator is never closed as a worker"
    );
}

#[test]
fn closing_an_agents_session_takes_two_presses() {
    let world = World::base(false);
    let mut app = app_on(&world);
    with_runtime(&mut app, runtime());
    app.show(Screen::Fleet);
    typed(&mut app, "ggj");
    assert_eq!(
        app.agent_sel.as_ref().map(|a| a.0.as_str()),
        Some("agent-a")
    );
    press(&mut app, KeyCode::Char('x'));
    assert!(posted(&app).is_empty() && flash(&app).contains("Press x again to close Instance A"));
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        posted(&app),
        [(
            "/agents/agent-a/close".to_string(),
            serde_json::json!({"force": true})
        )]
    );
    assert!(
        matches!(app.on_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)), Flow::Attach(short) if short == "a1b2c3"),
        "claude attach takes the session's short id"
    );
}

#[test]
fn agents_tasks_and_the_orchestrator_each_take_a_message() {
    let world = World::base(false);
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('M'));
    typed(&mut app, "Plan the week");
    press(&mut app, KeyCode::Enter);
    app.show(Screen::Tasks);
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Use the new API");
    press(&mut app, KeyCode::Enter);
    app.show(Screen::Fleet);
    press(&mut app, KeyCode::Char('m'));
    typed(&mut app, "Status?");
    press(&mut app, KeyCode::Enter);
    let paths: Vec<(String, serde_json::Value)> = posted(&app)
        .into_iter()
        .map(|(p, b)| (p, b["text"].clone()))
        .collect();
    assert_eq!(
        paths,
        [
            (
                "/orchestrator/messages".to_string(),
                serde_json::json!("Plan the week")
            ),
            (
                "/agents/agent-a/messages".to_string(),
                serde_json::json!("Use the new API")
            ),
            (
                "/agents/agent-a/messages".to_string(),
                serde_json::json!("Status?")
            ),
        ]
    );
    assert!(sent(&app).is_empty());
}

#[test]
fn the_task_detail_shows_its_brief_history_dependents_stack_and_options() {
    let mut world = World::base(true);
    world.run(
        "user-a",
        Command::CreateTask {
            task_id: "task-2".into(),
            request_id: "request-1".into(),
            title: "Second task".into(),
            agent_id: Some("agent-a".into()),
            review_prs: false,
            dependencies: std::collections::BTreeSet::from(["task-1".into()]),
            stack: None,
        },
    );
    let mut app = app_on(&world);
    with_runtime(&mut app, runtime());
    app.show(Screen::Tasks);
    let history = fixture("requests.json")["history"]["response"].clone();
    app.on_msg(Msg::History {
        task: "task-1".into(),
        result: Ok(serde_json::from_value(history).unwrap()),
    });
    let shown = screen(&mut app, 180, 60);
    for text in [
        "App",
        "ASSIGNMENT",
        "Cover the editor text",
        "HISTORY",
        "Presented PR #7",
        "waiting on it",
        "Second task",
        "OPTIONS",
        "review each one",
        "hard",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
}

#[test]
fn an_agents_threads_and_local_details_come_from_the_runtime() {
    let world = World::base(false);
    let mut app = app_on(&world);
    with_runtime(&mut app, runtime());
    app.show(Screen::Fleet);
    typed(&mut app, "ggj");
    let activity = fixture("requests.json")["activity"]["response"]["activity"].clone();
    app.on_msg(Msg::Threads {
        agent: "agent-a".into(),
        threads: serde_json::from_value(activity).ok(),
    });
    let shown = screen(&mut app, 160, 40);
    for text in [
        "project App · a1b2c3",
        "/Users/me/Projects/repo",
        "NOW",
        "bazel test",
        "subagent of “Synthetic task”",
        "Explore the parser",
        "A decision on the flag",
        "Running the suite",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
    press(&mut app, KeyCode::Char('/'));
    typed(&mut app, "projects/repo");
    assert_eq!(
        app.agent_list().len(),
        1,
        "the session's directory matches locally"
    );
    press(&mut app, KeyCode::Esc);
    press(&mut app, KeyCode::Char('/'));
    typed(&mut app, "parser");
    assert!(app.agent_list().is_empty());
    app.on_msg(Msg::Found {
        query: "parser".into(),
        result: Ok(("session transcripts".into(), vec!["agent-a".into()])),
    });
    assert_eq!(
        app.agent_list().len(),
        1,
        "the daemon's search fills in what nothing local matched"
    );
    assert!(screen(&mut app, 160, 40).contains("1 according to session transcripts"));
}

#[test]
fn a_on_an_orchestrator_item_opens_its_activity() {
    let mut world = World::base(false);
    world.run(
        "system",
        Command::AdvanceOrchestratorGeneration { generation: 1 },
    );
    world.run(
        "orchestrator",
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info-o".into(),
            scope: Scope::Orchestrator {
                request_id: Some("request-1".into()),
            },
            recipient: Recipient::User,
            body: "Split into two tasks".into(),
        },
    );
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('a'));
    assert!(app.screen == Screen::Inbox && flash(&app).contains("manages the orchestrator"));
    with_runtime(&mut app, runtime());
    press(&mut app, KeyCode::Char('a'));
    assert!(app.screen == Screen::Fleet && app.orchestrator_selected() && app.agent_zoom);
}

#[test]
fn comments_on_a_question_go_with_the_option_picked_when_it_takes_text() {
    let mut world = World::base(false);
    world.question(
        "question-1",
        "Approve the plan?",
        &[("changes", true), ("approve", false)],
    );
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('o'));
    let _ = screen(&mut app, 120, 30);
    press(&mut app, KeyCode::Char('c'));
    typed(&mut app, "Tighter");
    press(&mut app, KeyCode::Enter);
    press(&mut app, KeyCode::Char('s'));
    assert!(sent(&app).is_empty() && flash(&app).contains("Press the number"));
    press(&mut app, KeyCode::Char('2'));
    assert!(
        sent(&app).is_empty() && flash(&app).contains("takes no text"),
        "{}",
        flash(&app)
    );
    press(&mut app, KeyCode::Char('1'));
    let Command::AnswerQuestion {
        option_id, inputs, ..
    } = &sent(&app)[0].command
    else {
        panic!("not an answer")
    };
    assert!(
        option_id == "changes"
            && matches!(inputs.get("why"), Some(InputValue::Text(t)) if t.contains("Tighter"))
    );
    deliver(&mut app, &mut world).unwrap();
}

#[test]
fn a_presented_document_is_read_from_its_revision_content() {
    let mut world = World::base(false);
    world.run(
        "agent-a",
        Command::CreateDocument {
            task_id: "task-1".into(),
            document_id: "doc-1".into(),
            title: "Plan".into(),
            revision_id: "rev-1".into(),
            content_digest: "d".repeat(64),
            content_reference: "files/plan.md".into(),
            role: "design".into(),
        },
    );
    world.present(
        "delivery-1",
        vec![DeliveryTarget::DocumentRevision {
            document_id: "doc-1".into(),
            revision_id: "rev-1".into(),
        }],
    );
    let mut app = app_on(&world);
    let item = app
        .item(&Key("interaction:delivery-1".into()))
        .cloned()
        .unwrap();
    assert_eq!(
        app.item_files(&item)
            .into_iter()
            .map(|(_, r)| r)
            .collect::<Vec<_>>(),
        [crate::client::document_content("rev-1")]
    );
    app.on_msg(Msg::File {
        route: crate::client::document_content("rev-1"),
        result: Ok(files::FileBody::classify(
            b"## Steps\n\nMigrate the store".to_vec(),
        )),
    });
    press(&mut app, KeyCode::Char('o'));
    let shown = screen(&mut app, 120, 30);
    assert!(shown.contains("Migrate the store"), "{shown}");
    app.reader = None;
    app.show(Screen::Tasks);
    let tasks = screen(&mut app, 180, 40);
    assert!(
        tasks.contains("Plan · version") && tasks.contains("· unread"),
        "{tasks}"
    );
}

#[test]
fn a_stacked_task_lists_its_prs_in_merge_order_and_its_row_says_what_it_goes_on() {
    let mut world = World::base(true);
    world.pull_request();
    world.present("delivery-1", World::pr());
    let stack = StackDependency {
        upstream_task: "task-1".into(),
        presentation_id: "delivery-1".into(),
        pr_id: "pr-1".into(),
        head: "head1".into(),
    };
    world.run(
        "user-a",
        Command::CreateTask {
            task_id: "task-2".into(),
            request_id: "request-1".into(),
            title: "Album on top".into(),
            agent_id: Some("agent-a".into()),
            review_prs: true,
            dependencies: Default::default(),
            stack: Some(stack),
        },
    );
    let mut app = app_on(&world);
    app.show(Screen::Tasks);
    let second = app
        .task_keys()
        .into_iter()
        .position(|k| k == tasks::TaskKey::Task("task-2".into()))
        .unwrap();
    app.move_tasks(second as isize);
    let shown = screen(&mut app, 180, 50);
    for text in [
        "PRS IN MERGE ORDER",
        "#7 Synthetic task",
        "Album on top · no PR yet  this task",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
}

#[test]
fn tasks_group_by_project_and_a_taskless_request_says_how_it_closed() {
    let mut world = World::base(false);
    world.run(
        "user-a",
        Command::CreateRequest {
            request_id: "request-2".into(),
            body: "Is it broken?".into(),
            auto_merge: false,
        },
    );
    world.run(
        "user-a",
        Command::CloseRequest {
            request_id: "request-2".into(),
            answer: None,
            withdrawn: false,
            title: None,
        },
    );
    world.run(
        "user-a",
        Command::CancelTask {
            task_id: "task-1".into(),
        },
    );
    let mut app = app_on(&world);
    with_runtime(&mut app, runtime());
    app.show(Screen::Tasks);
    app.finished_open = true;
    app.reconcile();
    assert!(
        app.task_rows()
            .iter()
            .any(|r| matches!(r, tasks::TaskListRow::Project(p) if p == "App"))
    );
    let taskless = app
        .task_keys()
        .into_iter()
        .position(|k| k == tasks::TaskKey::Request("request-2".into()))
        .unwrap();
    app.move_tasks(taskless as isize);
    let shown = screen(&mut app, 180, 40);
    for text in [
        "Finished · 2",
        "the last one",
        "answered",
        "The orchestrator answered it: Nothing to change",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
}

#[test]
fn a_wide_inbox_shows_the_agents_beside_it_and_an_empty_detail_the_fleet() {
    let world = World::base(false);
    let mut app = app_on(&world);
    with_runtime(&mut app, runtime());
    let shown = screen(&mut app, 180, 20);
    for text in [
        "The fleet right now",
        "1 busy",
        "1 free",
        "★ Orchestrator",
        "Instance A",
    ] {
        assert!(shown.contains(text), "{text}: {shown}");
    }
}

#[test]
fn the_user_at_the_terminal_is_told_once_a_minute() {
    let world = World::base(false);
    let mut app = app_on(&world);
    app.note_activity();
    app.note_activity();
    assert_eq!(
        posted(&app),
        [(
            "/activity".to_string(),
            serde_json::json!({"client": "tui"})
        )]
    );
}

#[test]
fn an_attached_images_preview_goes_with_its_chip() {
    let mut world = World::base(false);
    world.present("delivery-1", World::text());
    let dir = scratch("preview");
    let file = dir.0.join("shot.png");
    std::fs::write(&file, files::png(4, 3)).unwrap();
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('r'));
    app.on_paste(&file.to_string_lossy());
    app.on_msg(Msg::Preview {
        seq: 1,
        image: image::load_from_memory(&files::png(4, 3)).unwrap(),
    });
    assert!(app.previews.contains_key(&1));
    let shown = screen(&mut app, 120, 30);
    assert!(
        shown.contains("shot.png") && shown.contains("copying…"),
        "{shown}"
    );
    press(&mut app, KeyCode::Backspace);
    assert!(
        app.composer.as_ref().is_some_and(|c| c.chips.is_empty()) && app.previews.is_empty(),
        "⌫ on an empty box drops the last attachment and its preview"
    );
}
