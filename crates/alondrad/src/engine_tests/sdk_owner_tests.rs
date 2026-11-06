//! The SDK owner's journeys that go through alondrad: an answer delivered to the node, a
//! button pressed in a client, the context a view shows.
use alondra_domain::ActivityView;
use crate::domain_store::StoreError;
use alondra_daemon_engine::sdk_owner::tests::codex_tests::*;
use alondra_daemon_engine::sdk_owner::tests::*;
use alondra_daemon_engine::sdk_owner::*;
use crate::domain_runtime::DomainRuntime;
use alondra_core::accounts::{AccountId, Provider};
use alondra_core::domain::*;
use std::sync::Arc;

// The node asks the user during its turn and the turn ends: the turn waits on that question and no
// card says it stopped; the user's answer reaches Claude as its next input and ends the wait.
#[test]
fn a_turn_that_ends_with_the_nodes_question_open_shows_only_that_question_and_the_answer_reaches_claude() {
    use alondra_core::domain::{CardKind, CardSpec, QuestionOption, Recipient, Scope};
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let options = ["Short", "Long"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
    slice.run(&Actor::Agent { agent_id: driver.agent().clone() }, "ask", Command::AskQuestion {
        attachments: vec![], interaction_id: "q-name".into(), scope: Scope::Task { task_id: TASK.into() }, recipient: Recipient::User,
        body: "Pick the name".into(), blocking: true, options, card: CardSpec::derived(CardKind::Decide, "Pick the name"),
    });
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Keep it short"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true))
        .unwrap();
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let inbox = |domain: &DomainRuntime| domain.read().index().waiting().iter().map(|item| item.id.clone()).collect::<Vec<_>>();
    assert_eq!(inbox(&slice.domain), ["interaction:q-name"], "only the node's question waits for the user");
    assert!(open_card(&slice.domain, TURN_ENDED_PREFIX).is_none() && open_card(&slice.domain, STALLED_PREFIX).is_none(), "no card says it stopped");
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), Some(&alondra_domain::Await::User("q-name".into())), "the turn waits on its question");

    answer(&slice.domain, &"q-name".into(), "short");
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&slice.domain), 1, "the answer goes to the node");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert!(prompts.last().is_some_and(|last| last.contains("Short.")), "{prompts:?}");
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), None, "the answer ended the wait");
    driver.stop().unwrap();
}

// The node delivers inside its turn and the user accepts: Alondra's request to clean up, a message
// of its outbox, is what the node's Claude is told next. A node of `claude --bg` takes it through its
// mod; nothing took it for a node of the SDK.
#[tokio::test(flavor = "multi_thread")]
async fn alondras_cleanup_request_reaches_the_sdk_node_as_its_next_input() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let agent = driver.agent().clone();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    let pending: Vec<_> = slice.domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
        .filter(|message| message.recipient_kind == "agent" && message.agent_id.as_deref() == Some(agent.0.as_str())).collect();
    assert_eq!(pending.len(), 1, "Alondra asks the node to clean up: {pending:?}");
    let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
    let push = crate::push::Push::open(&slice._home).unwrap();
    crate::domain_delivery::deliver_from_runtime(&slice.domain, &slice._home, &push, None, &client, pending[0].clone(), std::time::Duration::ZERO).await.unwrap();
    assert!(slice.domain.store.lock().unwrap().pending_messages().unwrap().iter().all(|message| message.id != pending[0].id), "the message is handed");
    assert_eq!(driver.pending_permits(), 1, "it leaves for the node's host");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let last = slice.prompts().last().cloned().unwrap();
    assert!(last.starts_with("[From Alondra · ") && last.contains("is finished and is closing"), "{last}");
    driver.stop().unwrap();
}

// The node was asked to clean up and its turn with that request ended: its host stays until the
// node says it cleaned up. Before, the closer stopped it as soon as the task rested, while the node
// was still answering the request.
#[tokio::test(flavor = "multi_thread")]
async fn an_sdk_node_asked_to_clean_up_is_closed_only_once_it_says_it_did() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let agent = driver.agent().clone();
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    slice.run(&user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets });
    let pending = slice.domain.store.lock().unwrap().pending_messages().unwrap().into_iter().find(|message| message.recipient_kind == "agent").unwrap();
    let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
    let push = crate::push::Push::open(&slice._home).unwrap();
    crate::domain_delivery::deliver_from_runtime(&slice.domain, &slice._home, &push, None, &client, pending, std::time::Duration::ZERO).await.unwrap();
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Resting, "the turn with the request ended");
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let timing = crate::task_closer::Timing::new(None);
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    assert!(slice.domain.owns_sdk_host(&agent), "the host stays until the node says it cleaned up");
    assert_eq!(slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].state, TaskState::Closing);
    slice.run(&Actor::Agent { agent_id: agent.clone() }, "cleaned", Command::ConfirmCleanup { task_id: TASK.into(), discard: Vec::new() });
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &timing, alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Done, Some(NodeEnd::Closed)));
    assert!(!slice.domain.owns_sdk_host(&agent));
}

// The same, with the task delivered: the user replied to the result and the node's turn on the
// reply left the command running. A delivery under review asked nothing of a resumed session, so
// nothing ever woke it: the task read «Answering you» for 12 hours.
#[test]
fn background_work_for_the_users_reply_that_its_replaced_host_ended_is_reported_to_the_resumed_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let node = Actor::Agent { agent_id: driver.agent().clone() };
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&node, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Wrap up"})).unwrap();
    let told = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true))
        .unwrap();
    assert_eq!(told["reaches"], "turn", "Claude reads it inside the turn that runs: {told}");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!((lifecycle(&slice.domain).phase(), lifecycle(&slice.domain).activity()), (alondra_domain::TaskPhase::Delivered, ActivityView::Resting));
    slice.run(&user(), "reply", Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "final".into(), targets,
        body: "background-shell".into(), instruction: alondra_core::domain::Instruction::Reply });
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&slice.domain), 1, "the reply is told to the node");
    driver.step(&|| false).unwrap();
    until_own_work(&slice, &mut driver);
    let (session, _) = strand_host(&slice, driver);
    let restarted = slice.restart();
    let (_, driver) = take_all(&restarted, &slice.backend).pop().expect("the daemon's start takes the execution");
    let driver = driver.unwrap_or_else(|Failed { why, .. }| panic!("the execution was left as it was: {why}"));
    assert_eq!(lifecycle(&restarted).phase(), alondra_domain::TaskPhase::Delivered, "the delivery stays the one the user reviews");
    assert_told_the_work_ended(&slice, &restarted, driver, &session);
}

// The user replied to a delivery and Claude, busy, never started on the reply: the task read
// «Answering you» although no turn ran. It reads that Claude has not started, until a turn does.
#[test]
fn a_reply_claude_has_not_started_reads_waiting_for_claude_not_answering_you() {
    use alondra_core::views::{Client, RenderContext, Transport, build, live::LiveSnapshot, text::Lang};
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let node = Actor::Agent { agent_id: driver.agent().clone() };
    let targets = vec![DeliveryTarget::Text { text: "The notes".into() }];
    slice.run(&node, "deliver", Command::PresentResult {
        checklist: Vec::new(), attachments: vec![], task_id: TASK.into(), interaction_id: "final".into(), body: "Done".into(),
        targets: targets.clone(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the notes", &[], true),
    });
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Wrap up"})).unwrap();
    tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "Alice", driver.agent(), message, true)).unwrap();
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    slice.run(&user(), "reply", Command::SubmitFeedback { merge_failure: None, attachments: vec![], presentation_id: "final".into(), targets,
        body: "stall-before-starting".into(), instruction: alondra_core::domain::Instruction::Reply });
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&slice.domain), 1);
    driver.step(&|| false).unwrap();
    assert_eq!(lifecycle(&slice.domain).activity(), ActivityView::Starting, "Claude holds the reply and starts no turn");
    let shown = |lang| {
        let read = slice.domain.read();
        let live = LiveSnapshot::default();
        let view = build(&format!("task:{TASK}"), RenderContext {
            domain: read.state(), index: read.index(), live: &live, client: Client::Tui, lang,
            transport: Transport::Loopback, cursor: "test", rendered_at: "",
        }).unwrap();
        serde_json::to_string(&view).unwrap()
    };
    let (english, spanish) = (shown(Lang::En), shown(Lang::Es));
    assert!(english.contains("Waiting for Claude to start") && !english.contains("Answering you"), "{english}");
    assert!(spanish.contains("Esperando a que Claude empiece") && !spanish.contains("Respondiéndote"), "{spanish}");
    driver.stop().unwrap();
}

/// The user presses «Interrupt» on the task of `agent`, through the action path the clients use.
fn press_interrupt(slice: &Slice, agent: &AgentId) -> serde_json::Value {
    press(slice, alondra_core::views::action_id::Verb::Interrupt, agent)
}

/// The user presses the node's `verb` button on the task of `agent`, through the action path the
/// clients use.
fn press(slice: &Slice, verb: alondra_core::views::action_id::Verb, agent: &AgentId) -> serde_json::Value {
    let state = slice.domain.state().unwrap();
    let action = alondra_core::views::action_id::ActionId::new(verb, &agent.0, state.revision, None).unwrap();
    let request = crate::views::actions::ActionRequest {
        protocol: alondra_core::views::VIEWS_PROTOCOL,
        command_id: format!("0b6f9a52-3c1e-4d7a-9f20-{:012x}", state.revision),
        data_epoch: state.data_epoch.0.clone(),
        client: "ios".into(),
        lang: "en".into(),
        action_id: action.to_string(),
        option_id: None,
        input: Default::default(),
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
        crate::views::actions::execute_action(&slice.domain, &client, &slice._home, "test-device", request, None, || Ok(())).await.unwrap()
    })
}

// Claude accepts the interrupt but its turn goes on, and it ignores SIGTERM. Once the interrupt is
// accepted, its task offers «Force stop» instead of «Interrupt»; pressing it ends Claude's process,
// which ends the turn as the user's interrupt, and «Carry on» reaches the same session in a new
// host. Before, the node stayed «working» with nothing that could stop it.
#[test]
fn a_turn_claude_keeps_running_after_its_interrupt_is_force_stopped_and_resumed() {
    use alondra_core::views::action_id::Verb;
    let slice = Slice::new();
    let mut driver = slice.launch_with("ignore-interrupt");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    let refused = slice.try_run(&user(), "force-too-soon", Command::ForceStopTurn { agent_id: driver.agent().clone() }).unwrap_err();
    let StoreError::Domain(refused) = refused else { panic!("{refused:?}") };
    assert_eq!(refused.refusal, Some(alondra_core::domain::Refusal::InterruptNotAccepted), "a turn is interrupted before it is forced");
    assert_eq!(press_interrupt(&slice, driver.agent())["outcome"], "applied");
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    assert_eq!(Slice::lifecycle(&slice.domain).turn_stop(), Some(alondra_domain::TurnStop::InterruptAccepted), "its receipt is the fact that Claude accepted it");
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Working, "and the turn goes on");
    let offered = node_offers(&slice.domain, driver.agent());
    assert!(offered.iter().any(|offer| offer.starts_with("force_stop:")), "the stop button is now Force stop: {offered:?}");
    assert!(!offered.iter().any(|offer| offer.starts_with("interrupt:")), "in place of Interrupt: {offered:?}");

    assert_eq!(press(&slice, Verb::ForceStop, driver.agent())["outcome"], "applied");
    let turn = Slice::lifecycle(&slice.domain).turn().unwrap().as_str().to_owned();
    assert_eq!(slice.control_rows().last().unwrap(), &("force_stop".to_owned(), Some(turn), "prepared".to_owned()));
    let offered = node_offers(&slice.domain, driver.agent());
    assert!(!offered.iter().any(|offer| offer.starts_with("force_stop:") || offer.starts_with("interrupt:")), "it is forced once, and nothing else offers to stop it: {offered:?}");
    let native = slice.starts()[0]["pid"].as_i64().unwrap();
    driver.step(&|| false).unwrap();
    assert_eq!(slice.control_rows().last().unwrap().2, "handed", "the host answered once Claude exited");
    // SAFETY: signal 0 only asks whether the process exists.
    assert_eq!(unsafe { libc::kill(native as libc::pid_t, 0) }, -1, "Claude's process is gone");
    while driver.observe().is_ok() {}
    assert!(driver.host_to_replace(), "Claude's exit ends its host for the owner");
    let mut resumed = driver.replace_ended_host(&slice.backend, Some(&slice.plugin)).unwrap();
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the forced turn ended");
    assert!(open_card(&slice.domain, SESSION_ENDED_PREFIX).is_none(), "the stopped task creates no card");
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), Some(&alondra_domain::Await::Instruction));
    slice.run(&Actor::System, "continue", Command::TellNode { task_id: TASK.into(), text: "Continue the release notes".into(), from_user: true, answers: None });
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    resumed.stop().unwrap();
}

// The domain refuses to record that Claude accepted an interrupt: the owner goes on, and the turn
// Claude ends waits for another instruction without a card.
#[test]
fn an_accepted_interrupt_alondra_cannot_record_leaves_the_owner_going() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    press_interrupt(&slice, driver.agent());
    crate::domain_store::refuse_next(crate::domain_store::Refuse::InterruptAccepted);
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the interrupted turn ended");
    assert!(Slice::read_cards(&slice.domain).is_empty(), "without a card");
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), Some(&alondra_domain::Await::Instruction));
    driver.stop().unwrap();
}

#[test]
fn cancelling_from_the_turn_end_card_after_a_failure_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("max-turns");
    slice.settle(&mut driver);
    let reply = cancel_from_turn_end_card(&slice);
    assert_cancelled_from_its_card(&slice, &reply);
    driver.stop().unwrap();
}

/// The user cancels the task from its open turn-end card, through the action path the clients use.
fn cancel_from_turn_end_card(slice: &Slice) -> serde_json::Value {
    let (card, _) = Slice::read_cards(&slice.domain).into_iter().find(|(card, answered)| !answered && card.starts_with(TURN_ENDED_PREFIX)).expect("an open turn-end card");
    let state = slice.domain.state().unwrap();
    let action = alondra_core::views::action_id::ActionId::new(alondra_core::views::action_id::Verb::Ans, &card, state.revision, None).unwrap();
    let request = crate::views::actions::ActionRequest {
        protocol: alondra_core::views::VIEWS_PROTOCOL,
        command_id: "5d2c8e41-7a93-4f06-b1e8-3c9a0d4f6e27".into(),
        data_epoch: state.data_epoch.0.clone(),
        client: "tui".into(),
        lang: "en".into(),
        action_id: action.to_string(),
        option_id: Some(alondra_core::domain::CANCEL_TASK.into()),
        input: Default::default(),
    };
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
        crate::views::actions::execute_action(&slice.domain, &client, &slice._home, "test-device", request, None, || Ok(())).await
    }).unwrap_or_else(|error| panic!("the cancellation failed in the store: {error:#}"))
}

// The user interrupts from the task while such a command runs: Claude Code stops it, and the turn
// that waited for it pauses without a card.
#[test]
fn interrupting_a_command_claude_code_tracks_as_work_pauses_without_a_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("foreground-command-hold");
    while slice.observations_with("\"subtype\":\"task_started\"") == 0 {
        driver.observe().unwrap();
    }
    assert!(node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "the running command offers Interrupt");
    let reply = press_interrupt(&slice, driver.agent());
    assert_eq!(reply["outcome"], "applied", "{reply}");
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    slice.read_turn_result(&mut driver);
    assert_eq!(slice.observations_with("\"status\":\"stopped\""), 1, "Claude Code stopped the command");
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the interrupted turn ended");
    let cards = Slice::read_cards(&slice.domain);
    assert!(cards.is_empty(), "the turn ended without a card: {cards:?}");
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), Some(&alondra_domain::Await::Instruction));
    assert!(!node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "nothing runs: no Interrupt");
    driver.stop().unwrap();
}

// C1: the user presses Interrupt on the task of a working SDK node. The control commits with the
// domain's decision, the owner sends it, the host hands it to Claude, and the interrupted turn
// pauses without a card.
#[test]
fn interrupting_a_working_sdk_node_from_its_task_reaches_claude_and_pauses_without_a_card() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let offered = node_offers(&slice.domain, driver.agent());
    assert!(offered.iter().any(|offer| offer.starts_with("interrupt:")), "a working SDK node offers Interrupt: {offered:?}");
    for claude_bg in ["attach", "close:"] {
        assert!(!offered.iter().any(|offer| offer.starts_with(claude_bg)), "an SDK node offers no {claude_bg}: {offered:?}");
    }
    let reply = press_interrupt(&slice, driver.agent());
    assert_eq!(reply["outcome"], "applied", "{reply}");
    let turn = Slice::lifecycle(&slice.domain).turn().unwrap().as_str().to_owned();
    assert_eq!(slice.control_rows(), [("interrupt".to_owned(), Some(turn), "prepared".to_owned())], "the interrupt committed with the decision");
    assert!(slice.controls().is_empty(), "nothing reached Claude before the owner sends it");
    assert_eq!(driver.pending_controls(), 1, "its permit reached the owner");
    driver.step(&|| false).unwrap();
    assert_eq!(subtypes(&slice.controls()), ["interrupt"], "Claude got the interrupt");
    assert_eq!(slice.control_rows()[0].2, "handed");
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).activity(), ActivityView::Resting, "the interrupted turn ended");
    let cards = Slice::read_cards(&slice.domain);
    assert!(cards.is_empty(), "the turn ended without a card: {cards:?}");
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), Some(&alondra_domain::Await::Instruction));
    assert!(!node_offers(&slice.domain, driver.agent()).iter().any(|offer| offer.starts_with("interrupt:")), "nothing runs: no Interrupt");
    let again = slice.try_run(&user(), "interrupt-again", Command::InterruptTurn { agent_id: driver.agent().clone() }).unwrap_err();
    let StoreError::Domain(refusal) = again else { panic!("{again:?}") };
    assert_eq!(alondra_core::views::refusal_text(&refusal.why(), alondra_core::views::text::Lang::En), "This node isn't running anything to interrupt.");
    driver.stop().unwrap();
}

/// The context the live view shows for the SDK task's node, launched told `prompt`.
fn shown_context(prompt: &str) -> (Option<u32>, Option<u64>) {
    let slice = Slice::new();
    let mut driver = slice.launch_with(prompt);
    slice.settle(&mut driver);
    let client = crate::client_routes::Client::open(Arc::clone(&slice.domain), &slice._home, Default::default());
    let state = slice.domain.state().unwrap();
    let live = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::views::live::gather(&client, &state, alondra_core::views::Lang::En, &[], false, false));
    let node = live.nodes.iter().find(|node| node.agent_id == driver.agent().0).expect("the task's node is live");
    let shown = (node.context_percent, node.context_window);
    driver.stop().unwrap();
    shown
}

// The node of an SDK task shows how much of its model's window its last answer used: 250 403
// tokens of the million Claude said its model has.
#[test]
fn an_sdk_node_shows_the_context_its_last_answer_used_of_its_models_window() {
    assert_eq!(shown_context("large-context"), (Some(25), Some(1_000_000)));
}

// Without a window Claude gave for the conversation's own model, the node shows no percent rather
// than one against a window nobody said.
#[test]
fn an_sdk_node_without_its_models_window_shows_no_context() {
    assert_eq!(shown_context("large-context-unsized"), (None, None), "Claude sized only another model");
    assert_eq!(shown_context("Write the release notes"), (None, None), "Claude sized no model");
}

#[tokio::test(flavor="multi_thread",worker_threads=4)]
#[ignore="opt-in: real pinned Codex, real Alondra MCP CLI and authenticated loopback daemon; no credentials or external inference"]
async fn an_actual_codex_cli_asks_enters_native_plan_and_presents_the_result_through_alondra(){
    let slice=Slice::with_database_name(HostRoot::new(),crate::domain_database_migration::DATABASE);configured(&slice,"complete");
    for args in [vec!["init","-b","main"],vec!["config","user.name","Alondra fixture"],vec!["config","user.email","fixture@example.invalid"],vec!["commit","--allow-empty","-m","Initial fixture"]] {
        let output=std::process::Command::new("/usr/bin/git").args(args).current_dir(&slice.worktree).env_clear().env("HOME",&*slice._home).env("PATH","/usr/bin:/bin").env("GIT_CONFIG_NOSYSTEM","1").env("GIT_CONFIG_GLOBAL","/dev/null").output().unwrap();
        assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    }
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let port=listener.local_addr().unwrap().port();
    let journey=slice._home.join("native-journey.json");std::fs::write(&journey,serde_json::to_vec(&serde_json::json!({"task":TASK,"expected_answer":"English."})).unwrap()).unwrap();
    let (_model,model_trace,code_home,selected)=native_model_fixture(&slice,Some((&journey,port)));
    let daemon=crate::domain_bootstrap::DomainDaemon::over_shared(&slice._home,Arc::clone(&slice.domain)).unwrap();
    let outbox_domain=Arc::clone(&slice.domain);let outbox_home=slice._home.to_path_buf();
    let push=Arc::new(crate::push::Push::open(&outbox_home).unwrap());assert!(!push.enabled());
    let outbox_client=daemon.client();let(stop_outbox,outbox_stopped)=tokio::sync::oneshot::channel();
    let outbox=tokio::spawn(async move {tokio::select! {
        result=crate::domain_delivery::run(outbox_domain,outbox_home,push,None,outbox_client)=>result,
        _=outbox_stopped=>Ok(()),
    }});
    let auth=Arc::clone(&daemon.auth);let app=crate::domain_bootstrap::router(daemon).layer(axum::middleware::from_fn_with_state(auth,crate::auth::guard));
    let (shutdown,closed)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,app.into_make_service_with_connect_info::<std::net::SocketAddr>()).with_graceful_shutdown(async{let _=closed.await;}).await.unwrap();});
    let domain=Arc::clone(&slice.domain);let fixture=Arc::new(tokio::sync::Mutex::new(slice));let scope=Arc::clone(&fixture);
    let scenario=tokio::spawn(async move {
        let slice=scope.lock().await;
        let (pulse,mut changed)=tokio::sync::watch::channel(0_u64);slice.on_acknowledge(move||pulse.send_modify(|count|*count+=1));
        let effective=selected.effective.clone().unwrap();let backend=slice.backend.clone();let owner_domain=Arc::clone(&slice.domain);
        let launch=Launch{task:TASK.into(),name:"Native report journey".into(),directory:slice.worktree.display().to_string(),prompt:"Write a report. Ask which language it should use, prepare a plan for approval, then present the completed report through Alondra.".into(),plugin:slice.plugin.clone(),options:LaunchOptions{provider:Provider::Codex,selection:Some(selected),model:Some(effective.model.model),effort:effective.reasoning_effort,disallowed_tools:vec![]},model:None,account:AccountId::default_account()};
        let driver=tokio::task::spawn_blocking(move||start(owner_domain,&backend,launch).unwrap()).await.unwrap();slice.domain.add_sdk_owner(Owner::spawn(driver));
        native_business_until(&slice,&mut changed,"language question",&model_trace,|state|state.interactions.contains_key(&InteractionId::from("native-language-decision"))).await;
        assert!(!slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].over());slice.answer("native-language-decision","english",None).unwrap();
        native_business_until(&slice,&mut changed,"native plan after answered question",&model_trace,|state|state.lifecycle_of(&TASK.into()).unwrap().cards().iter().any(|(id,_)|state.interactions.get(id).and_then(|interaction|interaction.card.content.as_ref()).is_some_and(|content|serde_json::to_value(&content.kind).unwrap()==serde_json::json!("plan")))).await;
        let state=slice.domain.state().unwrap();let card=state.lifecycle_of(&TASK.into()).unwrap().cards().iter().find(|(id,_)|state.interactions.get(id).and_then(|interaction|interaction.card.content.as_ref()).is_some_and(|content|serde_json::to_value(&content.kind).unwrap()==serde_json::json!("plan"))).unwrap().0.clone();
        assert!(slice.kept.lock().unwrap().values().any(|bytes|String::from_utf8_lossy(bytes).contains("\"type\":\"plan\"")),"the real Codex CLI emitted a native plan item");
        slice.answer(&card.0,Hold::APPROVE,None).unwrap();
        native_business_until(&slice,&mut changed,"final presentation after approved native plan",&model_trace,|state|state.tasks[&TaskId::from(TASK)].final_presentation.is_some() && state.lifecycle_of(&TASK.into()).unwrap().activity()==ActivityView::Resting && slice.kept.lock().unwrap().values().any(|bytes|{let raw=String::from_utf8_lossy(bytes);raw.contains("\"tool\":\"present_result\"") && raw.contains("\"status\":\"completed\"")})).await;
        let state=slice.domain.state().unwrap();let task=&state.tasks[&TaskId::from(TASK)];assert!(task.final_presentation.is_some());assert!(!task.over(),"canonical delivery still waits for the user's review");
        let requests=transcript(&model_trace);assert!(requests.len()>=6);
        let stages=transcript(&model_trace.with_extension("stages"));
        let plan_requests:Vec<_>=stages.iter().filter(|stage|stage["step"]==5 && stage["advanced"]==true).collect();
        assert_eq!(plan_requests.len(),1,"the model emitted exactly one complete native plan");
        let plan_index=usize::try_from(plan_requests[0]["request"].as_u64().unwrap().checked_sub(1).unwrap()).unwrap();
        let native_plan=requests[plan_index]["input"].as_array().unwrap().iter().filter(|message|message["role"]=="developer").flat_map(|message|message["content"].as_array().into_iter().flatten()).any(|part|part["type"]=="input_text" && part["text"].as_str().is_some_and(|text|text.contains("<collaboration_mode>") && text.contains("# Plan Mode (Conversational)") && text.contains("You are in **Plan Mode** until a developer message explicitly ends it.")));
        assert!(native_plan,"the plan turn used the actual pinned native Plan Mode developer instructions");
        let mut reply_messages=std::collections::BTreeSet::new();let mut first_reply=None;
        for (index,request) in requests.iter().enumerate(){
            for message in request["input"].as_array().unwrap().iter().filter(|message|message["role"]=="user"){
                if message["content"].as_array().is_some_and(|parts|parts.iter().any(|part|part["type"]=="input_text" && part["text"].as_str().is_some_and(|text|text.contains("English.")))){
                    reply_messages.insert(message["id"].as_str().unwrap().to_owned());first_reply.get_or_insert(index);
                }
            }
        }
        assert_eq!(reply_messages.len(),1,"the real outbox delivered one English answer into the same native session");
        assert!(first_reply.unwrap()<plan_index,"the native session received the chosen language before preparing its plan");
        assert!(slice.domain.store.lock().unwrap().pending_messages().unwrap().iter().all(|message|message.recipient_kind!="agent" || message.interaction_id.as_deref()!=Some("native-language-decision")),"the actual producer committed the answer handoff");
        assert_synthetic_native_auth(&code_home);
    });
    let result=scenario.await;
    let _=stop_outbox.send(());let delivered=outbox.await;
    domain.stop_sdk_owners();
    let _=shutdown.send(());server.await.unwrap();
    drop(fixture);
    delivered.unwrap().unwrap();
    if let Err(error)=result {if error.is_panic(){std::panic::resume_unwind(error.into_panic())}else{panic!("the native journey stopped: {error}")}}
}

// 3 November: the user answered a node's card in words only, and the node read
// «[From Alondra · 19:55] <text>» with nothing saying it answered its card.
#[test]
fn an_answer_reaches_the_sdk_node_from_the_user_with_its_card() {
    use alondra_core::domain::{CardKind, CardSpec, LifecycleRecord, QuestionOption, Recipient, Scope};
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let options = ["Short", "Long"].map(|label| QuestionOption { id: label.to_lowercase(), label: label.into(), description: None }).to_vec();
    slice.run(&Actor::Agent { agent_id: driver.agent().clone() }, "ask", Command::AskQuestion {
        attachments: vec![], interaction_id: "q-name".into(), scope: Scope::Task { task_id: TASK.into() }, recipient: Recipient::User,
        body: "Pick the name".into(), blocking: true, options, card: CardSpec::derived(CardKind::Decide, "Pick the name"),
    });
    slice.answer("q-name", "", Some("Call it Atlas")).unwrap();
    assert_eq!(crate::domain_delivery::tell_sdk_nodes(&slice.domain), 1, "the answer goes to the node");
    let told = slice.domain.read().state().lifecycle_journal[&TaskId::from(TASK)].iter().rev()
        .find_map(|record| match record {
            LifecycleRecord::Told { text, answers, .. } => Some((text.clone().unwrap_or_default(), answers.clone())),
            _ => None,
        })
        .expect("the node is told the answer");
    assert_eq!(told.1, Some("q-name".into()), "what it is told names the card it answers");
    assert!(!told.0.starts_with("[From Alondra"), "the user wrote it, not Alondra: {}", told.0);
    assert!(told.0.contains("q-name") && told.0.contains("Call it Atlas"), "{}", told.0);
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), None, "the answer ended the wait");
    driver.stop().unwrap();
}

#[test]
fn a_refused_interrupted_turn_end_keeps_the_host_without_waking_another_turn() {
    let slice = Slice::new();
    let mut driver = slice.launch_working();
    let group = driver.worker.host_group();
    let starts = slice.starts();
    let prompts = slice.prompts();
    assert_eq!(press_interrupt(&slice, driver.agent())["outcome"], "applied");
    driver.step(&|| false).unwrap();
    crate::domain_store::refuse_next(crate::domain_store::Refuse::TurnEnd);
    slice.settle(&mut driver);
    assert_eq!(driver.worker.host_group(), group);
    assert_eq!(slice.starts(), starts);
    assert_eq!(slice.prompts(), prompts, "a user interruption never becomes another inference turn");
    assert!(Slice::lifecycle(&slice.domain).turn().is_none());
    assert!(Slice::read_cards(&slice.domain).is_empty());
    assert_eq!(Slice::lifecycle(&slice.domain).awaits(), Some(&alondra_domain::Await::Instruction));
    assert!(!driver.host_to_replace());
    assert!(open_card(&slice.domain, HOST_LOST_PREFIX).is_none());
    driver.stop().unwrap();
}
