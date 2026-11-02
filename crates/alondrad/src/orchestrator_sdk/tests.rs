//! Orchestrator batches through the real SDK host: the pinned Node host with the official SDK and
//! a fake native Claude that answers each prompt as its marker says. A held turn waits on a FIFO
//! the test writes, never on a clock.
use super::*;
use crate::domain_orchestrator::testing::{Fixture, fixture};
use crate::domain_orchestrator::{Failure, Run, Runtime, load, save};
use alondra_core::domain::*;
use alondra_core::orchestrator_cards::{UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX, UNSPLIT_PROBLEM_PREFIX};
use alondra_provider_contract::Handoff;
use std::collections::BTreeSet;
use std::io::{Read, Write};

#[tokio::test]
async fn a_batch_through_the_sdk_is_one_turn_and_is_handed_once() {
    let f = fixture();
    f.tell("m-1", "Where do I put change 1?");
    f.tell("m-2", "Where do I put change 2?");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 1, "one batch, one prompt: {prompts:?}");
    assert!(prompts[0].contains("Where do I put change 1?") && prompts[0].contains("Where do I put change 2?") && prompts[0].contains("new conversation"), "{}", prompts[0]);
    assert_eq!(f.attempts(), ["handed", "handed"]);
    assert_eq!(f.pending(), 0);
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
    let session = runtime.session_id.clone().unwrap();
    assert!(f.arguments().contains(&format!("--session-id={session}")), "{:?}", f.arguments());
    assert!(f.arguments().contains(&"--permission-mode=auto".to_owned()), "{:?}", f.arguments());
    {
        let store = f.domain().store.lock().unwrap();
        for item in store.delivery_evidence_page(0).unwrap().into_iter().filter(|i| i.recipient_kind == "orchestrator") {
            assert_eq!(store.orchestrator_delivery(item.attempt_id).unwrap().as_deref(), Some(session.as_str()), "each message records the session it was handed to");
        }
    }
    assert!(!f.executor.home.join("runtime/orchestrator/owner.json").exists(), "the host's record goes with it");
    f.tell("m-3", "Where do I put change 3?");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 2);
    assert!(prompts[1].contains("Where do I put change 3?") && !prompts[1].contains("Where do I put change 1?"), "{}", prompts[1]);
}

/// The context of a batch is what Alondra writes in its prompt: each starts its own conversation,
/// with the note the previous one left and the questions the orchestrator still waits on, and
/// nothing of the conversations before it.
#[tokio::test]
async fn every_batch_is_a_new_conversation_and_carries_what_the_next_needs() {
    let f = fixture();
    let mut sessions = vec![];
    for n in 1..=5 {
        f.tell(&format!("m-{n}"), &format!("Where do I put change {n}?"));
        f.tick().await;
        let arguments = f.arguments();
        assert!(!arguments.iter().any(|argument| argument.starts_with("--resume")), "batch {n} resumes a conversation: {arguments:?}");
        let session = load(&f.executor.home).unwrap().session_id.unwrap();
        assert!(arguments.contains(&format!("--session-id={session}")), "{arguments:?}");
        sessions.push(session);
        if n == 1 {
            f.run(Actor::Orchestrator { generation: 0 }, "question", Command::AskQuestion {
                attachments: vec![], interaction_id: "q-waits".into(), scope: Scope::Orchestrator { request_id: None },
                recipient: Recipient::User, body: "Which repository is change 1 for?".into(), blocking: false,
                options: vec![QuestionOption { id: "web".into(), label: "Web".into(), description: None }, QuestionOption { id: "ios".into(), label: "iOS".into(), description: None }],
                card: CardSpec::derived(CardKind::Decide, "Choose the repository"),
            });
        }
    }
    assert_eq!(sessions.iter().collect::<BTreeSet<_>>().len(), 5, "five batches, five conversations: {sessions:?}");
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 5, "{prompts:?}");
    for prompt in &prompts[1..] {
        assert!(prompt.contains("YOUR NOTE FROM THE PREVIOUS BATCH:\nBatch handled\n"), "the note of the batch before: {prompt}");
        assert!(prompt.contains("your question q-waits, still unanswered: Which repository is change 1 for?"), "{prompt}");
    }
    let lengths: Vec<usize> = prompts[1..].iter().map(String::len).collect();
    assert!(lengths.windows(2).all(|pair| pair[0] == pair[1]), "a batch does not grow with the batches before it: {lengths:?}");
    let context = load(&f.executor.home).unwrap().last_context;
    assert_eq!(context, Some(crate::domain_orchestrator::BatchContext { first: 25, last: 25, window: None }), "each batch records the context it read");
}

// The first daemon with an SDK backend finds the claude -p batch the daemon it replaced left
// running. That batch is not run again, as any interrupted batch: what it may have done is
// uncertain. Since every batch is a conversation of its own, no orchestrator session moves to the
// SDK: the next batch simply runs through the SDK host, and claude -p never runs again.
#[tokio::test]
async fn after_the_update_a_claude_p_batch_left_running_is_not_repeated_and_the_next_runs_through_the_sdk() {
    let f = fixture();
    f.tell("m-1", "Where do I put change 1?");
    let pending = f.domain().store.lock().unwrap().pending_messages().unwrap().into_iter().find(|message| message.recipient_kind == "orchestrator").unwrap().id;
    let claim = f.domain().store.lock().unwrap().claim_message(pending).unwrap().unwrap();
    save(&f.executor.home, &Runtime {
        session_id: Some("0f0f0f0f-0000-4000-8000-000000000001".into()),
        active_run: Some(Run { id: "claude-p-run".into(), message_ids: vec![claim.message.id], attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true }),
        ..Runtime::default()
    }).unwrap();
    f.executor.recover().unwrap();
    assert_eq!(f.pending(), 0, "the interrupted claude -p batch is not offered again");
    assert_eq!(load(&f.executor.home).unwrap().pending_resolutions[0].run_id, "claude-p-run", "the next batch is told it was resolved");
    f.tell("m-2", "Where do I put change 2?");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 1, "one batch ran, through the SDK host: {prompts:?}");
    assert!(prompts[0].contains("Where do I put change 2?") && prompts[0].contains("Previous interrupted inbox batches have already been resolved and are NOT replayed"), "{}", prompts[0]);
    let arguments = f.arguments();
    assert!(!arguments.iter().any(|argument| argument.starts_with("--resume")), "the claude -p conversation is not resumed: {arguments:?}");
}

#[tokio::test]
async fn a_message_that_arrives_during_a_turn_waits_for_the_next_batch() {
    let f = fixture();
    f.ask(1, "fixture:hold Where do I put change 1?");
    let during = async {
        let held = f.held.clone();
        tokio::task::spawn_blocking(move || {
            let mut text = String::new();
            std::fs::File::open(held).unwrap().read_to_string(&mut text).unwrap();
        }).await.unwrap();
        f.tell("m-2", "Where do I put change 2?");
        let release = f.release.clone();
        tokio::task::spawn_blocking(move || std::fs::OpenOptions::new().write(true).open(release).unwrap().write_all(b"go").unwrap()).await.unwrap();
    };
    let ((), ()) = tokio::join!(f.tick(), during);
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 1, "{prompts:?}");
    assert!(prompts[0].contains("change 1") && !prompts[0].contains("change 2"), "the message that came during the turn is not in it: {}", prompts[0]);
    assert_eq!(f.attempts(), ["handed"]);
    assert_eq!(f.pending(), 1, "it waits for the next batch");
    assert!(!f.domain().state().unwrap().requests[&"m-2".into()].closed, "nothing settles a message the orchestrator has not seen");
    f.tick().await;
    let prompts = f.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].contains("change 2") && !prompts[1].contains("change 1"), "{}", prompts[1]);
    assert_eq!(f.attempts()[..2], ["handed", "handed"]);
}

#[test]
fn the_host_of_a_batch_takes_one_prompt_and_refuses_another() {
    let f = fixture();
    let session = "00000000-0000-4000-8000-00000000c0de";
    let epoch = f.domain().state().unwrap().data_epoch.clone();
    let (claims, owner) = f.domain().store.lock().unwrap().claim_orchestrator_batch(&[], "run-refuses", session, epoch).unwrap();
    assert!(claims.is_empty());
    let (backend, _) = f.domain().sdk().unwrap();
    let directory = f.executor.home.join("runtime/orchestrator");
    let plugin = |name| crate::plugins::dir(&f.claude_home, name).unwrap();
    let batch = Batch {
        owner, backend: backend.clone(), home: f.executor.home.clone(), directory,
        node_plugin: plugin(crate::plugins::NODE), orchestrator_plugin: plugin(crate::plugins::ORCHESTRATOR),
        projects_root: None, capability: "c".repeat(64), session: session.into(), prompt: String::new(),
        domain: Arc::clone(&f.executor.domain), selection: fixture_selection(),
    };
    let (configuration, profile) = (configuration(&batch).unwrap(), profile(&batch));
    let mut host = BatchHost::start(batch.owner, &configuration, session, &profile, "fixture:hold the first batch").unwrap();
    f.wait_held();
    assert_eq!(host.offer("the second batch").unwrap(), Handoff::RefusedBeforeOffer, "one batch, one turn");
    f.release();
    let mut stream = Stream::for_input(host.input());
    loop {
        match host.next().unwrap() {
            HostEvent::Sdk(message) => assert!(!stream.accept(&serde_json::to_vec(&message).unwrap(), session).is_err()),
            HostEvent::Diagnostic { text, .. } => panic!("{text}"),
            HostEvent::UnexpectedDecision => panic!("unexpected native decision"),
            HostEvent::Exited { code, .. } => {
                assert_eq!(code, Some(0));
                break;
            }
        }
    }
    host.stop().unwrap();
    assert_eq!(stream.end(true, ""), process::BatchEnd::Handled);
    assert_eq!(f.prompts(), ["fixture:hold the first batch"], "the refused prompt never reached Claude");
}

#[test]
fn the_projects_root_is_readable_and_never_writable() {
    let f = fixture();
    let epoch = f.domain().state().unwrap().data_epoch.clone();
    let batch = |root: Option<&Path>| {
        let (_, owner) = f.domain().store.lock().unwrap().claim_orchestrator_batch(&[], &crate::auth::random_hex(8), "00000000-0000-4000-8000-00000000c0de", epoch.clone()).unwrap();
        let plugin = |name| crate::plugins::dir(&f.claude_home, name).unwrap();
        Batch {
            owner, backend: f.domain().sdk().unwrap().0.clone(), home: f.executor.home.clone(), directory: f.executor.home.join("runtime/orchestrator"),
            node_plugin: plugin(crate::plugins::NODE), orchestrator_plugin: plugin(crate::plugins::ORCHESTRATOR),
            projects_root: root.map(Path::to_path_buf), capability: "c".repeat(64), session: "00000000-0000-4000-8000-00000000c0de".into(), prompt: String::new(),
            domain: Arc::clone(&f.executor.domain), selection: fixture_selection(),
        }
    };
    let rooted = profile(&batch(Some(Path::new("/Users/someone/Projects"))));
    assert_eq!(rooted.tools, ["Skill", "Read", "Glob", "Grep"]);
    assert_eq!(rooted.settings["permissions"], serde_json::json!({
        "additionalDirectories": ["/Users/someone/Projects"],
        "allow": ["Read(//Users/someone/Projects/**)", OWN_TOOLS],
        "deny": ["Edit(//Users/someone/Projects/**)"],
    }));
    assert_eq!(rooted.settings["env"], serde_json::from_str::<serde_json::Value>(&alondra_core::launch_settings()).unwrap()["env"]);
    let unrooted = profile(&batch(None));
    assert_eq!(unrooted.settings["permissions"], serde_json::json!({"allow": [OWN_TOOLS]}), "nothing outside its folder is readable");
}

#[tokio::test]
async fn every_request_of_a_handled_batch_ends_with_a_task_an_answer_or_a_question() {
    let f = fixture();
    let user = || Actor::User { user_id: "u".into() };
    for id in ["r-1", "r-2", "r-3"] {
        f.run(user(), id, Command::CreateRequest { request_id: id.into(), body: format!("Do {id}"), auto_merge: false, maintenance: false, message: false });
    }
    f.run(Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
    f.run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None,
        task_id: "t-2".into(), request_id: "r-2".into(), title: "Task".into(), agent_id: Some("a".into()), review_prs: false,
        dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
    });
    f.run(Actor::Orchestrator { generation: 0 }, "answer", Command::CloseRequest { request_id: "r-3".into(), withdrawn: false, answer: Some("Already done".into()), title: None, headline: None });
    f.run(Actor::Agent { agent_id: "a".into() }, "start", Command::StartTask { task_id: "t-2".into() });
    f.run(Actor::Agent { agent_id: "a".into() }, "suggest", Command::AskQuestion {
        attachments: vec![], interaction_id: "s-1".into(), scope: Scope::Task { task_id: "t-2".into() },
        recipient: Recipient::Orchestrator, body: "The badge is stale too".into(), blocking: false,
        options: suggestion_options(),
        card: CardSpec::new(CardKind::Propose, "Fix the stale badge").why("Not this task"),
    });
    assert!(f.pending() > 0, "the batch path runs, not the idle one");
    f.tick().await;
    assert_eq!(f.unresolved(), Vec::<String>::new(), "petición sin resultado");
    assert_eq!(f.cards(UNSPLIT_NOTICE_PREFIX), [format!("{UNSPLIT_NOTICE_PREFIX}r-1-1")], "r-1 goes back to the orchestrator once");
    assert_eq!(f.cards(UNSETTLED_NOTICE_PREFIX), [format!("{UNSETTLED_NOTICE_PREFIX}s-1")], "the suggestion goes back once");
    f.tick().await;
    assert_eq!(f.prompts().len(), 2, "the reminders are the next batch, a turn of their own");
    assert_eq!(f.unresolved(), Vec::<String>::new(), "petición sin resultado");
    assert_eq!(f.cards(UNSPLIT_PROBLEM_PREFIX), [format!("{UNSPLIT_PROBLEM_PREFIX}r-1-1")], "then it is a question to the user");
}

async fn ended(marker: &str) -> Fixture {
    let f = fixture();
    f.ask(1, &format!("{marker} Where do I put change 1?"));
    f.tick().await;
    f
}

#[tokio::test]
async fn a_batch_claude_never_answered_is_offered_again_without_a_card() {
    let f = ended("fixture:exit-before-init").await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Launch), "{runtime:?}");
    assert!(runtime.active_run.is_none() && runtime.retry_at.is_some());
    assert_eq!(f.attempts(), ["retryable"]);
    assert!(f.cards("problem-orchestrator-").is_empty());
}

#[tokio::test]
async fn a_batch_that_called_an_alondra_tool_and_died_is_the_users_call() {
    let f = ended("fixture:tool-then-exit").await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Interrupted), "{runtime:?}");
    assert_eq!(f.attempts(), ["uncertain"]);
    assert_eq!(f.cards("problem-orchestrator-").len(), 1);
}

#[tokio::test]
async fn a_batch_with_an_alondra_tool_refused_is_the_users_call() {
    let f = ended("fixture:denied").await;
    assert_eq!(load(&f.executor.home).unwrap().failure, Some(Failure::Denied));
    assert_eq!(f.attempts(), ["uncertain"]);
    assert_eq!(f.cards("problem-orchestrator-").len(), 1);
}

#[tokio::test]
async fn a_result_of_another_input_is_not_the_batch_handled() {
    let f = ended("fixture:another-input").await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Interrupted), "{runtime:?}");
    assert!(runtime.last_error.as_deref().is_some_and(|error| error.contains("another input")), "{runtime:?}");
    assert_eq!(f.attempts(), ["uncertain"]);
}

#[tokio::test]
async fn a_batch_that_ran_out_of_usage_before_answering_pauses_and_waits_without_a_card() {
    let f = ended("fixture:limited").await;
    assert!(f.domain().live.usage_pause.is_active(), "usage pauses everything");
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none() && runtime.failure.is_none(), "{runtime:?}");
    assert_eq!(f.attempts(), ["retryable"]);
    assert!(f.cards("problem-orchestrator-").is_empty());
}

/// Cancels the batch's run once the fake Claude holds its turn, after `meanwhile` got what it
/// told the test.
async fn cancelled_while_held(marker: &str, meanwhile: impl FnOnce(&Fixture, &str)) -> Fixture {
    let f = fixture();
    f.ask(1, &format!("{marker} Where do I put change 1?"));
    let during = async {
        let held = f.held.clone();
        let said = tokio::task::spawn_blocking(move || {
            let mut text = String::new();
            std::fs::File::open(held).unwrap().read_to_string(&mut text).unwrap();
            text
        }).await.unwrap();
        meanwhile(&f, said.trim_end());
        let run = load(&f.executor.home).unwrap().active_run.unwrap().id;
        f.executor.cancel(&run).unwrap();
    };
    let ((), ()) = tokio::join!(f.tick(), during);
    f
}

/// Claude answered but changed nothing in Alondra: whether the run read that answer before the
/// cancel or not, the batch is settled without a card.
#[tokio::test]
async fn a_turn_cancelled_after_claude_answered_without_changing_anything_is_settled_without_a_card() {
    let f = cancelled_while_held("fixture:hold", |_, _| {}).await;
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.failure.is_none() && runtime.active_run.is_none(), "{runtime:?}");
    assert_eq!(runtime.pending_resolutions.len(), 1, "the orchestrator hears it was stopped: {runtime:?}");
    assert!(f.cards("problem-orchestrator-").is_empty());
}

#[tokio::test]
async fn a_no_effect_cancellation_retires_its_remembered_logical_stop() {
    use alondra_core::orchestrator_batch::{BatchProblem, BatchStop};
    let f = fixture();
    f.tell("cancelled-stop", "Cancel this batch without repeating its decision");
    f.mark_once("fixture:quiet-hold");
    let during = async {
        f.held().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        *f.domain().live.orchestrator_stop.lock().unwrap() = Some((run.clone(), BatchProblem {
            stop: BatchStop::Denied,
            calls: vec![],
            subjects: vec![],
            error: "A logical refusal remembered before cancellation won".into(),
        }));
        f.executor.cancel(&run.id).unwrap();
    };
    let (result, ()) = tokio::join!(f.executor.tick(), during);
    result.unwrap();
    assert!(f.domain().live.orchestrator_stop.lock().unwrap().is_none(),
        "a completed cancellation cannot leave a logical stop to resurrect");
    let runtime = load(&f.executor.home).unwrap();
    assert!(runtime.active_run.is_none() && runtime.failure.is_none());
    assert_eq!(runtime.pending_resolutions.len(), 1);
    assert_eq!(f.attempts(), ["discarded"]);
    f.executor.repair_incomplete().await.unwrap();
    assert!(f.cards("problem-orchestrator-").is_empty());
    f.tell("after-cancelled-stop", "Process the next request");
    f.tick().await;
    assert_eq!(f.starts().len(), 2);
    assert!(f.domain().read().state().requests[&"after-cancelled-stop".into()].closed);
    assert!(f.cards("problem-orchestrator-").is_empty());
}

/// One of Claude's Alondra tools changed something before anything Claude said reached the run,
/// as when its call ran while the message announcing it was on its way: the daemon knows, so it
/// is the user's call.
#[tokio::test]
async fn a_turn_cancelled_after_a_change_the_run_never_read_is_the_users_call() {
    let f = cancelled_while_held("fixture:quiet-hold", |f, capability| {
        let state = f.domain().state().unwrap();
        f.domain().execute_bearer(capability, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: "tool-call".into(), expected_revision: state.revision,
            command: Command::CreateRequest { request_id: "r-2".into(), body: "Change 2".into(), auto_merge: false, maintenance: false, message: false },
        }).unwrap();
    }).await;
    let runtime = load(&f.executor.home).unwrap();
    assert_eq!(runtime.failure, Some(Failure::Cancelled), "{runtime:?}");
    assert!(runtime.pending_resolutions.is_empty(), "the batch is not given up as not done: {runtime:?}");
    assert_eq!(f.attempts(), ["uncertain"]);
    let cards = f.cards("problem-orchestrator-");
    assert_eq!(cards.len(), 1);
    let body = &f.domain().state().unwrap().interactions[&cards[0].clone().into()].body;
    assert_eq!(alondra_core::orchestrator_batch::BatchProblem::from_body(body).unwrap().calls, ["create_request"]);
}

fn fixture_selection() -> crate::orchestrator_provider::BatchSelection {
    use alondra_core::accounts::{AccountKey, Provider};
    use alondra_core::model_selection::{ModelKey, ModelSelection};
    crate::orchestrator_provider::BatchSelection {
        account: AccountKey::native(Provider::Claude), account_status: None,
        model: ModelSelection { model: ModelKey { provider: Provider::Claude, model: "opus".into() }, reasoning_effort: None, catalog_revision: "fixture-catalog".into() },
        policy_revision: "fixture-policy".into(), reason: "This test explicitly selects the native alias it advertises".into(), rejected_candidates: vec![],
    }
}

#[tokio::test]
async fn claude_batch_reports_are_durable_before_the_prompt_is_settled(){
    let f=fixture();f.tell("report-proof","Where should the release note go?");f.tick().await;
    assert_eq!(f.attempts(),["handed"]);
    let db=rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.executor.home)).unwrap();
    let (count,first,last):(i64,i64,i64)=db.query_row("SELECT count(*),min(cursor),max(cursor) FROM orchestrator_batch_reports",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert!(count>0);assert_eq!(first,1);assert_eq!(last,count);
    let terminal:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM orchestrator_batch_reports WHERE json_extract(report,'$.kind')='result' AND json_extract(report,'$.input_consumed')=1 AND json_extract(report,'$.turn_completed')=1)",[],|row|row.get(0)).unwrap();assert!(terminal);
}
#[tokio::test]
async fn claude_batch_report_commit_failure_retries_after_reports_can_commit(){
    let f=fixture();let db=rusqlite::Connection::open(crate::domain_database_migration::active_path(&f.executor.home)).unwrap();
    db.execute_batch("CREATE TRIGGER refuse_native_report BEFORE INSERT ON orchestrator_batch_reports BEGIN SELECT RAISE(ABORT,'injected report refusal'); END;").unwrap();
    f.tell("report-refusal","Where should the release note go?");f.tick().await;
    assert_ne!(f.attempts(),["handed"],"without the required report transaction the native prompt cannot settle");
    assert_eq!(f.attempts(),["retryable"]);
    assert!(load(&f.executor.home).unwrap().active_run.is_none());
    assert!(f.cards("problem-orchestrator-").is_empty());
    assert_eq!(db.query_row("SELECT count(*) FROM orchestrator_batch_reports",[],|row|row.get::<_,i64>(0)).unwrap(),0);
    db.execute_batch("DROP TRIGGER refuse_native_report").unwrap();
    f.tick().await;
    assert_eq!(f.attempts(),["retryable","handed"]);
    assert!(f.domain().read().state().requests[&"report-refusal".into()].closed);
    assert!(f.cards("problem-orchestrator-").is_empty());
}
