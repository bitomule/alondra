//! Transcripts of the TUI on real domain projections: every scenario is built by executing
//! commands through the core rules, every key the user presses becomes an envelope that the
//! same rules execute as the user, and the screen is rendered at each step.

use std::collections::BTreeSet;
use std::path::PathBuf;

use alondra_core::domain_v2::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

use super::*;

pub(crate) struct World {
    pub state: DomainState,
    seq: u64,
}

mod parity;

fn actor(alias: &str) -> Actor {
    match alias {
        "system" => Actor::System,
        "orchestrator" => Actor::Orchestrator { generation: 1 },
        a if a.starts_with("agent") => Actor::Agent { agent_id: a.into() },
        u => Actor::User { user_id: u.into() },
    }
}

impl World {
    pub fn new() -> Self {
        World {
            state: DomainState::new("fixture-epoch-2"),
            seq: 0,
        }
    }

    pub fn run(&mut self, who: &str, command: Command) -> CommandOutcome {
        self.seq += 1;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: self.state.data_epoch.clone(),
            command_id: format!("world-{}", self.seq).into(),
            expected_revision: self.state.revision,
            command,
        };
        self.state
            .execute(&actor(who), &envelope)
            .unwrap_or_else(|e| panic!("{who}: {e}"))
    }

    /// A request, one connected agent and its running task.
    pub fn base(review_prs: bool) -> Self {
        let mut w = World::new();
        w.run(
            "user-a",
            Command::CreateRequest {
                request_id: "request-1".into(),
                body: "Synthetic request".into(),
                auto_merge: false,
            },
        );
        w.run(
            "system",
            Command::RegisterAgent {
                agent_id: "agent-a".into(),
                name: "Instance A".into(),
            },
        );
        w.run(
            "system",
            Command::ObserveAgent {
                agent_id: "agent-a".into(),
                connected: true,
            },
        );
        w.run(
            "user-a",
            Command::CreateTask {
                task_id: "task-1".into(),
                request_id: "request-1".into(),
                title: "Synthetic task".into(),
                agent_id: Some("agent-a".into()),
                review_prs,
                dependencies: BTreeSet::new(),
                stack: None,
            },
        );
        w.run(
            "agent-a",
            Command::StartTask {
                task_id: "task-1".into(),
            },
        );
        w
    }

    fn task() -> Scope {
        Scope::Task {
            task_id: "task-1".into(),
        }
    }

    pub fn information(&mut self, id: &str, body: &str) {
        self.run(
            "agent-a",
            Command::PublishInformation {
                attachments: vec![],
                interaction_id: id.into(),
                scope: Self::task(),
                recipient: Recipient::User,
                body: body.into(),
            },
        );
    }

    pub fn question(&mut self, id: &str, body: &str, options: &[(&str, bool)]) {
        let options = options
            .iter()
            .map(|(o, writes)| QuestionOption {
                id: (*o).into(),
                label: format!("Option {o}"),
                description: None,
                inputs: if *writes {
                    vec![InputSpec {
                        id: "why".into(),
                        kind: InputKind::Text,
                        required: true,
                    }]
                } else {
                    vec![]
                },
            })
            .collect();
        self.run(
            "agent-a",
            Command::AskQuestion {
                attachments: vec![],
                interaction_id: id.into(),
                scope: Self::task(),
                recipient: Recipient::User,
                body: body.into(),
                blocking: true,
                options,
            },
        );
    }

    pub fn pull_request(&mut self) {
        self.run(
            "agent-a",
            Command::RegisterPr {
                task_id: "task-1".into(),
                pr_id: "pr-1".into(),
                host: "github.com".into(),
                repository: "owner/repo".into(),
                number: 7,
                head: "head1".into(),
                base: "main".into(),
                allowed_methods: vec![MergeMethod::Squash, MergeMethod::Merge],
            },
        );
    }

    pub fn present(&mut self, id: &str, targets: Vec<DeliveryTarget>) {
        self.run(
            "agent-a",
            Command::PresentResult {
                attachments: vec![],
                task_id: "task-1".into(),
                interaction_id: id.into(),
                body: "Result ready".into(),
                targets,
                is_final: true,
            },
        );
    }

    pub fn text() -> Vec<DeliveryTarget> {
        vec![DeliveryTarget::Text {
            text: "The answer".into(),
        }]
    }

    pub fn pr() -> Vec<DeliveryTarget> {
        vec![DeliveryTarget::PullRequest {
            pr_id: "pr-1".into(),
            head: "head1".into(),
        }]
    }
}

pub(crate) fn app_on(world: &World) -> App {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let (tx, _rx) = mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::Data(Box::new(world.state.projection())));
    app.reconcile();
    app
}

fn refresh(app: &mut App, world: &World) {
    app.on_msg(Msg::Data(Box::new(world.state.projection())));
    app.reconcile();
}

fn press(app: &mut App, code: KeyCode) {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

fn sent(app: &App) -> Vec<CommandEnvelope> {
    app.sent.borrow().clone()
}

/// Executes the last envelope the TUI sent as the user, and tells the TUI how it ended.
fn deliver(app: &mut App, world: &mut World) -> Result<CommandOutcome, DomainError> {
    let envelope = sent(app).pop().expect("the TUI sent a command");
    let result = world.state.execute(&actor("user-a"), &envelope);
    let slot = app
        .sending
        .iter()
        .find(|(_, s)| s.envelope == envelope)
        .map(|(k, _)| k.clone())
        .expect("the TUI tracks what it sent");
    app.on_msg(Msg::Command {
        slot,
        result: result.clone().map_err(CommandFailure::Rejected),
    });
    refresh(app, world);
    result
}

fn select(app: &mut App, id: &str) {
    let i = app
        .inbox_keys()
        .iter()
        .position(|k| k.0 == id)
        .unwrap_or_else(|| panic!("{id} is not in the inbox"));
    app.move_inbox(i as isize);
}

fn screen(app: &mut App, w: u16, h: u16) -> String {
    let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
    t.draw(|f| draw::draw(f, app)).unwrap();
    let buf = t.backend().buffer().clone();
    (0..h)
        .map(|y| {
            (0..w)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn flash(app: &App) -> String {
    app.flash
        .as_ref()
        .map(|f| f.text.clone())
        .unwrap_or_default()
}

#[test]
fn marking_information_read_changes_only_that_item_and_never_replies() {
    let mut world = World::base(false);
    world.information("info-1", "Tests pass");
    world.information("info-2", "Docs updated");
    let mut app = app_on(&world);
    select(&mut app, "interaction:info-1");
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(sent(&app).len(), 1);
    assert_eq!(
        sent(&app)[0].command,
        Command::MarkRead {
            interaction_id: "info-1".into()
        }
    );
    let outcome = deliver(&mut app, &mut world).unwrap();
    assert!(outcome.effects.is_empty(), "reading notifies nobody");
    let read = |id: &str| matches!(&world.state.interactions[&InteractionId::from(id)].payload, InteractionPayload::Information { read_by } if !read_by.is_empty());
    assert!(read("info-1") && !read("info-2"));
    assert_eq!(
        world.state.tasks[&TaskId::from("task-1")].state,
        TaskState::Running
    );
    select(&mut app, "interaction:info-2");
    let shown = screen(&mut app, 120, 24);
    assert!(
        shown.contains("Docs updated") && shown.contains("x mark read"),
        "{shown}"
    );
}

fn pr_with_question_child() -> World {
    let mut world = World::base(true);
    world.pull_request();
    world.present("delivery-1", World::pr());
    world.question(
        "question-1",
        "Keep the flag?",
        &[("yes", false), ("no", false)],
    );
    world.run(
        "agent-a",
        Command::LinkInteractionSubject {
            interaction_id: "question-1".into(),
            subject: InteractionSubject::PullRequest {
                pr_id: "pr-1".into(),
            },
        },
    );
    world
}

#[test]
fn a_number_on_a_child_question_answers_the_child_and_never_authorizes_its_pr() {
    let mut world = pr_with_question_child();
    let mut app = app_on(&world);
    assert_eq!(
        app.inbox_keys()
            .iter()
            .map(|k| k.0.as_str())
            .collect::<Vec<_>>(),
        [
            "pr:pr-1",
            "interaction:delivery-1",
            "interaction:question-1"
        ]
    );
    let shown = screen(&mut app, 120, 30);
    assert!(
        shown.contains("↳ ✓ Result ready") && shown.contains("↳ ? Keep the flag?"),
        "{shown}"
    );
    select(&mut app, "pr:pr-1");
    press(&mut app, KeyCode::Char('1'));
    assert!(sent(&app).is_empty(), "a root only groups");
    assert!(flash(&app).contains("Select the item under it"));
    select(&mut app, "interaction:question-1");
    press(&mut app, KeyCode::Char('1'));
    assert_eq!(sent(&app).len(), 1);
    assert!(
        matches!(&sent(&app)[0].command, Command::AnswerQuestion { interaction_id, option_id, .. } if interaction_id.0 == "question-1" && option_id == "yes")
    );
    deliver(&mut app, &mut world).unwrap();
    assert!(world.state.merge_authorizations.is_empty());
    assert!(
        matches!(&world.state.interactions[&InteractionId::from("question-1")].payload, InteractionPayload::Question { answer: Some(a), .. } if a.option_id == "yes")
    );
}

#[test]
fn an_option_that_asks_for_text_opens_the_composer_and_sends_it_as_the_input() {
    let mut world = World::base(false);
    world.question("question-1", "Which name?", &[("custom", true)]);
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('1'));
    assert!(sent(&app).is_empty() && app.composer.is_some());
    typed(&mut app, "Lark");
    press(&mut app, KeyCode::Enter);
    let Command::AnswerQuestion {
        inputs, option_id, ..
    } = &sent(&app)[0].command
    else {
        panic!("not an answer")
    };
    assert_eq!(
        (option_id.as_str(), inputs.get("why")),
        ("custom", Some(&InputValue::Text("Lark".into())))
    );
    deliver(&mut app, &mut world).unwrap();
}

#[test]
fn feedback_goes_to_the_presented_delivery_and_a_conflict_keeps_the_text() {
    let mut world = World::base(false);
    world.present("delivery-1", World::text());
    let mut app = app_on(&world);
    select(&mut app, "interaction:delivery-1");
    press(&mut app, KeyCode::Char('r'));
    typed(&mut app, "Fix the intro");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        sent(&app)[0].command,
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "delivery-1".into(),
            targets: World::text(),
            body: "Fix the intro".into()
        }
    );
    world.run(
        "user-b",
        Command::AcceptDelivery {
            presentation_id: "delivery-1".into(),
            targets: World::text(),
        },
    );
    let refused = deliver(&mut app, &mut world).unwrap_err();
    assert_eq!(refused.code, "stale_revision");
    assert!(flash(&app).contains("Your text is kept"), "{}", flash(&app));
    assert_eq!(
        app.sel.as_ref().map(|k| k.0.as_str()),
        Some("interaction:delivery-1")
    );
    press(&mut app, KeyCode::Char('r'));
    assert_eq!(
        app.composer.as_ref().map(|c| c.input.text()),
        Some("Fix the intro".to_string())
    );
    press(&mut app, KeyCode::Enter);
    assert_eq!(sent(&app).len(), 2);
    assert_ne!(
        sent(&app)[0].command_id,
        sent(&app)[1].command_id,
        "a refused command is not replayed"
    );
    deliver(&mut app, &mut world).unwrap();
    assert!(
        matches!(&world.state.interactions[&InteractionId::from("delivery-1")].payload, InteractionPayload::Delivery { feedback, .. } if feedback.len() == 1)
    );
}

#[test]
fn an_unconfirmed_command_is_resent_as_the_same_envelope() {
    let mut world = World::base(false);
    world.present("delivery-1", World::text());
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('v'));
    let first = sent(&app)[0].clone();
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(
        sent(&app).len(),
        1,
        "nothing is sent twice while the first is on its way"
    );
    app.on_msg(Msg::Command {
        slot: "accept:delivery-1".into(),
        result: Err(CommandFailure::Uncertain("timed out".into())),
    });
    assert!(flash(&app).contains("cannot happen twice"));
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(sent(&app)[1], first);
    deliver(&mut app, &mut world).unwrap();
}

#[test]
fn authorizing_a_merge_takes_two_presses_records_it_and_never_says_merged() {
    let mut world = World::base(true);
    world.pull_request();
    world.present("delivery-1", World::pr());
    let mut app = app_on(&world);
    select(&mut app, "interaction:delivery-1");
    press(&mut app, KeyCode::Char('2'));
    assert!(sent(&app).is_empty());
    assert!(flash(&app).contains("Press 2 again"));
    press(&mut app, KeyCode::Char('2'));
    let Command::AuthorizeMerge {
        presentation_id,
        pr_id,
        expected_head,
        method,
        ..
    } = &sent(&app)[0].command
    else {
        panic!("not an authorization")
    };
    assert_eq!(
        (
            presentation_id.0.as_str(),
            pr_id.0.as_str(),
            expected_head.as_str(),
            *method
        ),
        ("delivery-1", "pr-1", "head1", MergeMethod::Merge)
    );
    deliver(&mut app, &mut world).unwrap();
    let said = flash(&app);
    assert!(
        said.contains("authorized") && !said.to_lowercase().contains("merged"),
        "{said}"
    );
    assert_eq!(
        world.state.pull_requests[&PrId::from("pr-1")].state,
        PrState::Open
    );
    assert!(
        app.item(&Key("interaction:delivery-1".into())).is_some(),
        "the PR stays until the agent merges it"
    );
    let shown = screen(&mut app, 140, 40);
    assert!(
        shown.contains("merge authorized (merge commit) · waiting for the agent"),
        "{shown}"
    );
    assert!(!shown.contains("merged by the agent"), "{shown}");
    app.show(Screen::Tasks);
    let tasks = screen(&mut app, 140, 30);
    assert!(
        tasks.contains("merge authorized · waiting for the agent") && !tasks.contains("done"),
        "{tasks}"
    );
}

fn text_completion_trace() -> Vec<(String, CommandEnvelope)> {
    let path = match std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR")) {
        Ok(root) => PathBuf::from(root)
            .join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()))
            .join("fixtures/domain-v2/text-completion.commands.json"),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/domain-v2/text-completion.commands.json"),
    };
    let trace: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap();
    trace["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["actor"].as_str().unwrap().to_string(),
                serde_json::from_value(s["envelope"].clone()).unwrap(),
            )
        })
        .collect()
}

#[test]
fn completion_shows_only_after_the_agent_declares_it_on_the_shared_trace() {
    let trace = text_completion_trace();
    let mut world = World::new();
    for (who, envelope) in &trace[..5] {
        world.state.execute(&actor(who), envelope).unwrap();
    }
    let mut app = app_on(&world);
    assert_eq!(
        app.inbox_keys()
            .iter()
            .map(|k| k.0.as_str())
            .collect::<Vec<_>>(),
        ["interaction:final-1"]
    );
    press(&mut app, KeyCode::Char('v'));
    assert_eq!(
        sent(&app)[0].command,
        trace[5].1.command,
        "the key sends what the shared trace accepts with"
    );
    deliver(&mut app, &mut world).unwrap();
    app.show(Screen::Tasks);
    let accepted = screen(&mut app, 140, 24);
    assert!(
        accepted.contains("presented · accepted · waiting for the agent"),
        "{accepted}"
    );
    assert!(!accepted.contains("done"), "{accepted}");
    assert_ne!(
        world.state.tasks[&TaskId::from("task-1")].state,
        TaskState::Done
    );
    let (who, complete) = &trace[6];
    world.state.execute(&actor(who), complete).unwrap();
    refresh(&mut app, &world);
    app.finished_open = true;
    app.reconcile();
    let done = screen(&mut app, 140, 24);
    assert!(
        done.contains("Finished · 1")
            && done.contains("✓ Synthetic answer")
            && done.contains("done"),
        "{done}"
    );
}

#[test]
fn a_child_keeps_its_selection_when_the_inbox_is_regrouped() {
    let mut world = World::base(true);
    world.question("question-1", "Keep the flag?", &[("yes", false)]);
    let mut app = app_on(&world);
    select(&mut app, "interaction:question-1");
    world.pull_request();
    world.run(
        "agent-a",
        Command::LinkInteractionSubject {
            interaction_id: "question-1".into(),
            subject: InteractionSubject::PullRequest {
                pr_id: "pr-1".into(),
            },
        },
    );
    refresh(&mut app, &world);
    assert_eq!(
        app.sel.as_ref().map(|k| k.0.as_str()),
        Some("interaction:question-1")
    );
    assert!(
        app.parent(&Key("interaction:question-1".into()))
            .is_some_and(|p| p.id == "pr:pr-1")
    );
}

#[test]
fn tasks_and_agents_come_from_the_projection() {
    let mut world = World::base(false);
    world.run(
        "system",
        Command::RegisterAgent {
            agent_id: "agent-b".into(),
            name: "Instance B".into(),
        },
    );
    world.run(
        "user-a",
        Command::CreateTask {
            task_id: "task-2".into(),
            request_id: "request-1".into(),
            title: "Second task".into(),
            agent_id: Some("agent-a".into()),
            review_prs: false,
            dependencies: BTreeSet::from(["task-1".into()]),
            stack: None,
        },
    );
    let mut app = app_on(&world);
    app.show(Screen::Tasks);
    let tasks = screen(&mut app, 140, 24);
    assert!(
        tasks.contains("0/2") && tasks.contains("Synthetic task") && tasks.contains("└"),
        "{tasks}"
    );
    app.show(Screen::Fleet);
    let agents = screen(&mut app, 140, 24);
    assert!(
        agents.contains("● Instance A") && agents.contains("○ Instance B"),
        "{agents}"
    );
    assert!(
        agents.contains("Second task"),
        "one agent holds several tasks: {agents}"
    );
}

#[test]
fn cancel_asks_twice_and_sends_the_typed_command() {
    let world = World::base(false);
    let mut app = app_on(&world);
    app.show(Screen::Tasks);
    press(&mut app, KeyCode::Char('x'));
    assert!(sent(&app).is_empty());
    press(&mut app, KeyCode::Char('x'));
    assert_eq!(
        sent(&app)[0].command,
        Command::CancelTask {
            task_id: "task-1".into()
        }
    );
}

#[test]
fn a_schedule_write_carries_only_the_draft_fields() {
    let listed = serde_json::json!({"id": "s", "title": "T", "text": "x", "project": null, "options": {}, "disallowed_tools": [], "frequency": "daily",
        "hour": 9, "minute": 0, "weekday": null, "interval": null, "start_at": null, "enabled": true, "revision": 2, "next_run": 5, "deleted_at": null,
        "status": "sent", "last_run": 1, "last_request": null, "last_error": null, "last_run_id": "r"});
    let draft = schedule_draft(&listed);
    for gone in [
        "id",
        "next_run",
        "deleted_at",
        "status",
        "last_run",
        "last_run_id",
    ] {
        assert!(draft.get(gone).is_none(), "{gone}");
    }
    assert_eq!(draft["revision"], 2);
}

#[test]
fn background_contention_retries_but_real_errors_remain_visible() {
    let busy =
        anyhow::Error::new(crate::account::AccountBusy).context("request submission uncertain");
    assert!(matches!(
        background_sync_error(&busy, "wait: "),
        Msg::Offline
    ));
    let denied = anyhow::anyhow!("relay authentication failed (HTTP 401)");
    assert!(
        matches!(background_sync_error(&denied, "wait: "), Msg::SyncError(text) if text.contains("HTTP 401"))
    );
}

#[test]
fn reader_navigation_search_and_comments_survive() {
    let mut world = World::base(false);
    world.present("delivery-1", World::text());
    let mut app = app_on(&world);
    press(&mut app, KeyCode::Char('o'));
    assert!(app.reader.is_some());
    let _ = screen(&mut app, 120, 30);
    press(&mut app, KeyCode::Char('c'));
    typed(&mut app, "Tighter");
    press(&mut app, KeyCode::Enter);
    assert_eq!(
        app.reviews
            .get(&Key("interaction:delivery-1".into()))
            .map(|r| r.count()),
        Some(1)
    );
    press(&mut app, KeyCode::Char('s'));
    assert!(
        matches!(&sent(&app)[0].command, Command::SubmitFeedback { body, .. } if body.contains("Tighter"))
    );
    deliver(&mut app, &mut world).unwrap();
    assert!(
        app.reviews.is_empty(),
        "sent comments are cleared once the daemon has them"
    );
}

/// What the sectioned reads deliver: no interactions, and a body the daemon cut to fit.
fn sectioned(world: &World, cut: &str) -> DomainProjection {
    let mut p = world.state.projection();
    p.interactions.clear();
    fn shorten(item: &mut InboxItem, cut: usize) {
        if let Some(body) = item.body.as_mut() {
            body.truncate(cut);
        }
        for child in &mut item.children {
            shorten(child, cut);
        }
    }
    for root in &mut p.inbox {
        shorten(root, cut.len());
    }
    p
}

#[test]
fn a_cut_body_is_replaced_by_the_interaction_once_it_is_read() {
    let mut world = World::base(false);
    world.information(
        "info-1",
        "Tests pass and the migration replays every fixture without a diff",
    );
    let (tx, _rx) = mpsc::sync_channel(8);
    let mut app = App::new(tx);
    app.on_msg(Msg::Data(Box::new(sectioned(&world, "Tests pass"))));
    app.reconcile();
    select(&mut app, "interaction:info-1");
    let before = screen(&mut app, 120, 24);
    assert!(
        before.contains("Tests pass") && !before.contains("without a diff"),
        "{before}"
    );
    let full = world.state.interactions[&InteractionId::from("info-1")].clone();
    let revision = full.revision;
    app.on_msg(Msg::Interaction {
        id: "info-1".into(),
        asked_at: revision,
        result: Ok(full),
    });
    let after = screen(&mut app, 120, 24);
    assert!(after.contains("without a diff"), "{after}");
    app.on_msg(Msg::Data(Box::new(sectioned(&world, "Tests pass"))));
    app.reconcile();
    assert!(
        app.interaction(&"info-1".into()).is_some(),
        "an unchanged item keeps its interaction"
    );
    world.run(
        "user-a",
        Command::MarkRead {
            interaction_id: "info-1".into(),
        },
    );
    app.on_msg(Msg::Data(Box::new(sectioned(&world, "Tests pass"))));
    app.reconcile();
    assert!(
        app.interaction(&"info-1".into()).is_none(),
        "a changed item drops the stale interaction"
    );
}
