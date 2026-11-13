use super::*;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use crate::node_close::Answer;
use crate::node_push::testing::Node;
use alondra_core::domain::{
    Actor, CardSpec, CommandEnvelope, DeliveryTarget, Instruction, ModelChooser, POST_MERGE_CLOSE, POST_MERGE_PROBLEM_PREFIX,
    PROTOCOL_VERSION, QuestionOption, TaskState,
};
use alondra_core::{DaemonObservation, Event};
use serde_json::{Value, json};
use std::sync::Mutex;

const SESSION: &str = "ab1234cd-0000-4000-8000-000000000000";
const SHORT: &str = "ab1234cd";
const MINUTE: i64 = 60_000;
const T0: i64 = 1_000_000;

struct FakeSessions {
    listed: Mutex<Vec<String>>,
    leftovers: Mutex<Option<String>>,
    calls: Mutex<Vec<Vec<String>>>,
    watch_while_removing: Mutex<Option<(Arc<DomainRuntime>, Option<bool>)>>,
    during_rm: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    rm_answers: Mutex<std::collections::VecDeque<crate::node_close::Ran>>,
}

impl FakeSessions {
    fn listing(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            listed: Mutex::new(ids.iter().map(|id| id.to_string()).collect()),
            leftovers: Mutex::new(None),
            calls: Default::default(),
            watch_while_removing: Mutex::new(None),
            during_rm: Mutex::new(None),
            rm_answers: Default::default(),
        })
    }
    fn removals(&self) -> usize {
        self.calls.lock().unwrap().iter().filter(|call| call[0] == "rm").count()
    }
}

impl Sessions for FakeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<crate::node_close::Ran, String>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(args.clone());
            match args[0].as_str() {
                "agents" => Ok(crate::node_close::Ran::ok(json!(self.listed.lock().unwrap().iter().map(|id| json!({"id": id})).collect::<Vec<_>>()).to_string())),
                "rm" => {
                    if let Some(hook) = self.during_rm.lock().unwrap().take() {
                        hook();
                    }
                    if let Some((domain, seen)) = self.watch_while_removing.lock().unwrap().as_mut() {
                        *seen = Some(domain.live.begin_watch(SESSION));
                    }
                    let ran = self.rm_answers.lock().unwrap().pop_front().unwrap_or_else(|| crate::node_close::Ran::ok(format!("removed {}", args[1])));
                    if ran.success {
                        self.listed.lock().unwrap().retain(|id| id != &args[1]);
                    }
                    Ok(ran)
                }
                _ => Err("unexpected".into()),
            }
        })
    }

    fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
        Box::pin(async move { self.leftovers.lock().unwrap().clone() })
    }
}

struct Home(crate::test_support::Directory);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Rig {
    _home: Home,
    domain: Arc<DomainRuntime>,
    sessions: Arc<FakeSessions>,
    node: Node,
    timing: Timing,
}

fn user() -> Actor {
    Actor::User { user_id: "u".into() }
}
fn agent() -> Actor {
    Actor::Agent { agent_id: SESSION.into() }
}

fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
    let state = domain.state().unwrap();
    domain.execute(&actor, &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
        expected_revision: state.revision, command,
    }, None, || Ok(())).unwrap();
}

fn hook(event: &str) -> Value {
    json!({"session_id": SESSION, "hook_event_name": event})
}

fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text { text: "The result".into() }]
}

impl Rig {
    /// A node Alondra launched, with its text delivery accepted: the task is closing and its node
    /// has been asked to clean up.
    fn new(wait: Option<Duration>) -> Self {
        let rig = Self::without_a_launch(wait);
        rig.domain.record_model_decision(&Decision {
            task_id: "t".into(), request_id: "r".into(), agent_id: Some(SESSION.into()), at: T0, kind: DecisionKind::Launch,
            model: None, effort: None, approach: None, chosen_by: ModelChooser::User, reason: None, title: "Result".into(), provider: None, tags: None,
        }).unwrap();
        rig
    }

    fn without_a_launch(wait: Option<Duration>) -> Self {
        let rig = Self::undelivered(wait);
        rig.deliver_and_accept();
        rig
    }

    fn undelivered(wait: Option<Duration>) -> Self {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        command(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: SESSION.into(), name: "Node".into(), at: 0 });
        command(&domain, user(), "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        command(&domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "t".into(), request_id: "r".into(), title: "Result".into(), agent_id: Some(SESSION.into()),
            review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        domain.live.apply(&Event::DaemonSeen(DaemonObservation {
            session_id: SESSION.into(), short_id: SHORT.into(), name: Some("Node".into()), ..Default::default()
        }));
        domain.live.observe(&hook("SessionStart"));
        domain.live.observe(&hook("Stop"));
        let node = Node::connect(&domain, SESSION);
        let sessions = FakeSessions::listing(&[SHORT]);
        Self { _home: Home(home), domain, sessions, node, timing: Timing::new(wait) }
    }

    fn deliver_and_accept(&self) {
        command(&self.domain, agent(), "present", Command::PresentResult {
            checklist: Vec::new(),
            attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(), targets: text(),
            is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        command(&self.domain, user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets: text() });
    }

    fn confirm(&self) {
        let id = format!("confirm-{}", crate::auth::random_hex(6));
        command(&self.domain, agent(), &id, Command::ConfirmCleanup { task_id: "t".into(), discard: Vec::new() });
    }

    async fn pass(&self, now: i64) {
        pass(&self.domain, self.sessions.as_ref(), &self.timing, now).await.unwrap();
        for _ in 0..3 {
            tokio::task::yield_now().await;
        }
    }

    fn task(&self) -> Task {
        self.domain.state().unwrap().tasks[&TaskId::from("t")].clone()
    }

    fn closing(&self) -> Option<TaskClosing> {
        self.domain.store.lock().unwrap().task_closing(&"t".into()).unwrap()
    }

    fn told(&self) -> Vec<String> {
        self.node.texts()
    }

    fn cards(&self) -> Vec<alondra_core::domain::Interaction> {
        self.domain.state().unwrap().interactions.values().filter(|i| i.id.0.starts_with("problem-close-")).cloned().collect()
    }

    fn assert_done(&self, end: NodeEnd) {
        let task = self.task();
        assert_eq!(task.wrap_up.node, Some(end));
        assert_eq!(task.state, TaskState::Done);
    }
}

fn thirty_minutes() -> Option<Duration> {
    Some(Duration::from_secs(30 * 60))
}

#[tokio::test]
async fn recording_a_closed_node_failure_keeps_its_receipt_and_stops_after_three_attempts() {
    let rig = Rig::new(thirty_minutes());
    rig.confirm();
    command(&rig.domain, Actor::System, "other-agent", Command::RegisterAgent { agent_id: "other".into(), name: "Other".into(), at: 0 });
    command(&rig.domain, user(), "other-request", Command::CreateRequest { request_id: "other".into(), body: "Other work".into(), auto_merge: false, maintenance: false, message: false });
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "other-task", Command::CreateTask {
        plan_items: None, continues: None, task_id: "z".into(), request_id: "other".into(), title: "Other task".into(), agent_id: Some("other".into()),
        review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("other", "/tmp")),
    });
    command(&rig.domain, Actor::Agent { agent_id: "other".into() }, "other-result", Command::PresentResult {
        checklist: vec![], attachments: vec![], task_id: "z".into(), interaction_id: "other-final".into(), body: "Done".into(),
        targets: text(), is_final: true, nothing_to_review: false, card: CardSpec::delivery("Review the other result", &[], true),
    });
    command(&rig.domain, user(), "accept-other", Command::AcceptDelivery { presentation_id: "other-final".into(), targets: text() });
    let database = crate::domain_database_migration::active_path(&rig._home.0);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.execute_batch(r#"CREATE TRIGGER refuse_node_end BEFORE INSERT ON task_history
        WHEN NEW.task_id = 't' AND NEW.state = 'done'
        BEGIN SELECT RAISE(ABORT, 'node end storage unavailable'); END;"#).unwrap();
    for attempt in 0..3 {
        let now = T0 + attempt * 5 * MINUTE;
        let result = pass(&rig.domain, rig.sessions.as_ref(), &rig.timing, now).await;
        assert!(result.is_err(), "the failed receipt is propagated");
        assert_eq!(rig.domain.state().unwrap().tasks[&TaskId::from("z")].state, TaskState::Done, "a failed close does not block other tasks");
        let row = rig.closing().expect("the failed receipt stays tracked");
        assert_eq!(row.failures, attempt as u32 + 1);
        assert_eq!(row.next_attempt, now + 5 * MINUTE);
        let calls = rig.sessions.calls.lock().unwrap().len();
        rig.pass(now + 1).await;
        assert_eq!(rig.sessions.calls.lock().unwrap().len(), calls, "domain notifications cannot bypass retry timing");
    }
    assert_eq!(rig.cards().len(), 1);
    let calls = rig.sessions.calls.lock().unwrap().len();
    rig.pass(T0 + 600 * MINUTE).await;
    assert_eq!(rig.sessions.calls.lock().unwrap().len(), calls, "a permanent failure waits for the user");
    assert_eq!(rig.task().wrap_up.node, None);
    connection.execute_batch("DROP TRIGGER refuse_node_end").unwrap();
    answer(&rig, "retry");
    rig.pass(T0 + 601 * MINUTE).await;
    rig.assert_done(NodeEnd::Gone);
    assert!(rig.closing().is_none());
    rig.pass(T0 + 602 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1, "the removed session is never removed again");
}

// 3 November: the user chose «Leave it open» on tasks that could not close. Recording that choice
// failed, the closer dropped his answer and asked him again at once, and again after the daemon
// restarted. An answered «Leave it open» stays the answer: its receipt is tried again quietly on the
// usual wait, across restarts, until it is recorded.
#[tokio::test]
async fn an_answered_leave_survives_a_failed_receipt_and_a_restart_without_asking_again() {
    let mut rig = Rig::new(thirty_minutes());
    failed_three_times(&rig).await;
    let database = crate::domain_database_migration::active_path(&rig._home.0);
    let connection = rusqlite::Connection::open(database).unwrap();
    connection.execute_batch(r#"CREATE TRIGGER refuse_node_end BEFORE INSERT ON command_audit
        WHEN json_extract(NEW.command, '$.kind') = 'record_node_end'
        BEGIN SELECT RAISE(ABORT, 'node end storage unavailable'); END;"#).unwrap();
    answer(&rig, "leave");
    let card = rig.closing().unwrap().problem_id;
    assert!(pass(&rig.domain, rig.sessions.as_ref(), &rig.timing, T0 + 11 * MINUTE).await.is_err(), "the failed receipt is propagated");
    let row = rig.closing().unwrap();
    assert_eq!(row.problem_id, card, "the answered card stays the task's answer");
    rig.pass(T0 + 11 * MINUTE + 1).await;
    assert_eq!(rig.closing().unwrap(), row, "the receipt waits for the usual retry");
    rig.domain = Arc::new(DomainRuntime::open(&rig._home.0).unwrap());
    for at in [T0 + 17 * MINUTE, T0 + 600 * MINUTE] {
        assert!(pass(&rig.domain, rig.sessions.as_ref(), &rig.timing, at).await.is_err());
        assert_eq!(rig.closing().unwrap().problem_id, card);
        assert_eq!(rig.cards().len(), 1, "nobody is asked again");
    }
    connection.execute_batch("DROP TRIGGER refuse_node_end").unwrap();
    rig.pass(T0 + 700 * MINUTE).await;
    rig.assert_done(NodeEnd::KeptByUser);
    assert!(rig.closing().is_none());
    assert_eq!(rig.sessions.removals(), 0, "the node the user kept is never removed");
}

#[tokio::test]
async fn a_node_that_confirms_its_cleanup_is_closed_and_its_task_is_done() {
    let rig = Rig::new(thirty_minutes());
    assert_eq!(rig.task().state, TaskState::Closing);
    rig.confirm();
    rig.pass(T0).await;
    assert_eq!(rig.sessions.calls.lock().unwrap().iter().filter(|call| call[0] == "rm" && call[1] == SHORT).count(), 1);
    rig.assert_done(NodeEnd::Closed);
    assert!(rig.domain.live.begin_watch(SESSION), "the watch is released");
    rig.domain.live.end_watch(SESSION);
    assert_eq!(rig.closing(), None);
}

#[tokio::test]
async fn a_node_that_never_answers_is_closed_once_the_wait_runs_out() {
    let rig = Rig::new(thirty_minutes());
    rig.pass(T0).await;
    rig.pass(T0 + 29 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    assert_eq!(rig.task().state, TaskState::Closing);
    rig.pass(T0 + 30 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn with_the_wait_off_only_the_nodes_confirmation_closes_it() {
    let rig = Rig::new(None);
    rig.pass(T0).await;
    rig.pass(T0 + 24 * 60 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    assert_eq!(rig.task().state, TaskState::Closing);
    rig.confirm();
    rig.pass(T0 + 25 * 60 * MINUTE).await;
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_working_node_is_not_closed_until_it_is_idle() {
    let rig = Rig::new(thirty_minutes());
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    assert!(rig.domain.live.node(SESSION).unwrap().busy);
    rig.confirm();
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 0);
    assert_eq!(rig.task().state, TaskState::Closing);
    rig.domain.live.observe(&hook("Stop"));
    rig.pass(T0 + MINUTE).await;
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_node_waiting_for_a_permission_is_never_closed_and_counts_no_failure() {
    let rig = Rig::new(thirty_minutes());
    rig.domain.live.observe(&hook("PermissionRequest"));
    rig.confirm();
    rig.pass(T0).await;
    rig.pass(T0 + 120 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    assert_eq!(rig.closing().unwrap().failures, 0);
    assert!(rig.told().iter().all(|text| !text.contains("could not close")));
}

// 21 October: a close that kept failing for the same reason told the node so on every attempt, and
// each message was one more turn of its Claude. The node is told once per reason, and again only
// after it confirms its cleanup anew.
#[tokio::test]
async fn a_refused_close_tells_the_node_why_once_and_again_only_after_its_new_confirmation() {
    let rig = Rig::new(thirty_minutes());
    *rig.sessions.leftovers.lock().unwrap() = Some("a.txt is not committed".into());
    rig.confirm();
    rig.pass(T0).await;
    let reason = format!("{SHORT} still has something pending, so it was not closed: a.txt is not committed");
    let expected = close_failed_text(&reason, &"t".into(), 5);
    let told = rig.told();
    assert_eq!(told.last().unwrap(), &expected);
    assert_eq!(told.len(), 1);
    assert_eq!(rig.sessions.removals(), 0);

    rig.pass(T0 + MINUTE).await;
    assert_eq!(rig.told().len(), 1, "no new attempt before the pause is over");
    rig.pass(T0 + 5 * MINUTE).await;
    assert_eq!(rig.closing().unwrap().failures, 2, "tried again after the pause");
    assert_eq!(rig.told().len(), 1, "the same reason is not told again");

    rig.confirm();
    rig.pass(T0 + 6 * MINUTE).await;
    assert_eq!(rig.closing().unwrap().failures, 3, "a new confirmation tries at once");
    assert_eq!(rig.told().len(), 2, "and is answered with why it still does not close");
}

#[tokio::test]
async fn three_failed_closes_raise_one_card_with_try_again_and_leave_it_open() {
    let rig = Rig::new(thirty_minutes());
    *rig.sessions.leftovers.lock().unwrap() = Some("work".into());
    rig.confirm();
    for attempt in 0..3 {
        rig.pass(T0 + attempt * 5 * MINUTE).await;
    }
    let cards = rig.cards();
    assert_eq!(cards.len(), 1);
    let InteractionPayload::Question { options, answer: None } = &cards[0].payload else { panic!("a question card") };
    assert_eq!(options.iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(),
        vec![("retry", "Try again"), ("leave", "Leave it open")]);
    let card = alondra_core::close_problem::CloseProblem::from_body(&cards[0].body).expect("the body holds what clients word");
    assert_eq!((card.stop, card.detail.as_str(), card.cancelled), (alondra_core::close_problem::CloseStop::Unsaved, "work", false));
    assert_eq!(cards[0].card.recommended.as_deref(), Some("leave"), "closing would lose the unsaved work");
    assert_eq!(rig.closing().unwrap().problem_id.as_deref(), Some(cards[0].id.0.as_str()));
    let told = rig.told().len();
    rig.pass(T0 + 15 * MINUTE).await;
    rig.pass(T0 + 600 * MINUTE).await;
    assert_eq!(rig.told().len(), told, "no fourth attempt while the user decides");
    assert_eq!(rig.cards().len(), 1);
}

fn answer(rig: &Rig, option: &str) {
    let card = rig.cards()[0].id.clone();
    command(&rig.domain, user(), &format!("answer-{option}"), Command::AnswerQuestion {
        attachments: vec![], interaction_id: card, option_id: option.into(), inputs: Default::default(),
    });
}

async fn failed_three_times(rig: &Rig) {
    *rig.sessions.leftovers.lock().unwrap() = Some("work".into());
    rig.confirm();
    for attempt in 0..3 {
        rig.pass(T0 + attempt * 5 * MINUTE).await;
    }
}

#[tokio::test]
async fn try_again_resets_the_count_and_closes_the_node() {
    let rig = Rig::new(thirty_minutes());
    failed_three_times(&rig).await;
    answer(&rig, "retry");
    *rig.sessions.leftovers.lock().unwrap() = None;
    rig.pass(T0 + 11 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn leave_it_open_finishes_the_task_and_records_that_the_user_kept_the_node() {
    let rig = Rig::new(thirty_minutes());
    failed_three_times(&rig).await;
    answer(&rig, "leave");
    rig.pass(T0 + 11 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    rig.assert_done(NodeEnd::KeptByUser);
}

#[tokio::test]
async fn a_node_already_gone_from_claude_agents_finishes_the_task_as_gone() {
    let rig = Rig::new(thirty_minutes());
    rig.sessions.listed.lock().unwrap().clear();
    rig.confirm();
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 0);
    rig.assert_done(NodeEnd::Gone);
}

#[tokio::test]
async fn a_restart_after_claude_rm_and_before_recording_finishes_as_closed() {
    let rig = Rig::new(thirty_minutes());
    let mut row = rig.domain.store.lock().unwrap().begin_task_closing(&"t".into(), T0).unwrap();
    row.rm_started_at = Some(T0 + MINUTE);
    rig.domain.store.lock().unwrap().save_task_closing(&row).unwrap();
    rig.sessions.listed.lock().unwrap().clear();
    rig.confirm();
    rig.pass(T0 + 2 * MINUTE).await;
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn the_wait_is_not_restarted_by_a_daemon_restart() {
    let rig = Rig::new(thirty_minutes());
    rig.pass(T0).await;
    let restarted = FakeSessions::listing(&[SHORT]);
    pass(&rig.domain, restarted.as_ref(), &Timing::new(thirty_minutes()), T0 + 20 * MINUTE).await.unwrap();
    assert_eq!(restarted.removals(), 0);
    assert_eq!(rig.closing().unwrap().since, T0);
    pass(&rig.domain, restarted.as_ref(), &Timing::new(thirty_minutes()), T0 + 30 * MINUTE).await.unwrap();
    assert_eq!(restarted.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_cancelled_tasks_busy_node_has_its_turn_cut_is_closed_once_idle_and_gets_no_message() {
    let rig = Rig::new(thirty_minutes());
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    let told = rig.told().len();
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 0, "not while it works");
    assert_eq!(rig.node.interrupts(), 1, "its turn is cut at once: the task no longer exists");
    rig.domain.live.observe(&hook("Stop"));
    rig.pass(T0 + MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1);
    let task = rig.task();
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)));
    assert_eq!(rig.told().len(), told, "nothing is sent after the cancellation");
}

#[tokio::test]
async fn a_task_cancelled_before_this_change_is_left_alone() {
    let rig = Rig::new(thirty_minutes());
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    rig.domain.store.lock().unwrap().conn_for_test().execute("DELETE FROM task_wrap_ups", []).unwrap();
    rig.domain.reload();
    assert!(!rig.task().wrap_up.close_cancelled);
    rig.pass(T0).await;
    rig.pass(T0 + 600 * MINUTE).await;
    assert!(rig.sessions.calls.lock().unwrap().is_empty());
    assert_eq!(rig.closing(), None);
}

#[tokio::test]
async fn a_session_alondra_did_not_launch_is_never_closed() {
    let rig = Rig::without_a_launch(thirty_minutes());
    rig.confirm();
    rig.pass(T0).await;
    assert!(rig.sessions.calls.lock().unwrap().is_empty(), "no claude call");
    rig.assert_done(NodeEnd::UsersOwn);
}

#[tokio::test]
async fn the_orchestrator_is_never_closed() {
    let rig = Rig::new(thirty_minutes());
    rig.domain.live.apply(&Event::MarkedOrchestrator { session_id: SESSION.into() });
    rig.confirm();
    rig.pass(T0).await;
    assert!(rig.sessions.calls.lock().unwrap().is_empty());
    rig.assert_done(NodeEnd::UsersOwn);
}

#[tokio::test]
async fn a_task_that_leaves_cleanup_forgets_its_closing_and_withdraws_its_card() {
    let rig = Rig::new(thirty_minutes());
    failed_three_times(&rig).await;
    let card = rig.cards()[0].id.clone();
    command(&rig.domain, user(), "feedback", Command::SubmitFeedback { merge_failure: None,
        attachments: vec![], presentation_id: "final".into(), targets: text(), body: "One more thing".into(),
        instruction: Instruction::Changes,
    });
    assert_eq!(rig.task().state, TaskState::Running);
    rig.pass(T0 + 20 * MINUTE).await;
    assert_eq!(rig.closing(), None);
    assert!(rig.domain.state().unwrap().interactions[&card].withdrawn);
}

#[tokio::test]
async fn closing_holds_the_sessions_watch() {
    let rig = Rig::new(thirty_minutes());
    *rig.sessions.watch_while_removing.lock().unwrap() = Some((rig.domain.clone(), None));
    rig.confirm();
    rig.pass(T0).await;
    let seen = rig.sessions.watch_while_removing.lock().unwrap().as_ref().unwrap().1;
    assert_eq!(seen, Some(false), "a revival cannot take the watch while claude rm runs");
}

const HOUR: i64 = 60 * MINUTE;

/// A finished task's node that keeps working is never cut nor reported, however long its turn
/// runs: it is closed between turns, once its turn ends.
#[tokio::test]
async fn a_busy_node_of_a_finished_task_is_never_cut_and_closes_once_its_turn_ends() {
    let rig = Rig::new(thirty_minutes());
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    for hours in [0, 1, 6, 24, 72] {
        rig.pass(T0 + hours * HOUR).await;
    }
    assert_eq!((rig.node.interrupts(), rig.sessions.removals(), rig.closing().unwrap().failures), (0, 0, 0));
    assert!(rig.cards().is_empty());
    rig.domain.live.observe(&hook("Stop"));
    rig.pass(T0 + 72 * HOUR + MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_busy_node_that_declared_work_after_the_merge_never_has_its_turn_cut() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.58"));
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    for minutes in [0, 31, 62, 120, 179] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    assert_eq!(rig.node.interrupts(), 0);
    assert_eq!(rig.sessions.removals(), 0);
}

#[tokio::test]
async fn cancelling_a_task_withdraws_the_card_of_three_failed_closes_and_the_node_is_closed() {
    let rig = Rig::new(thirty_minutes());
    failed_three_times(&rig).await;
    let card = rig.cards()[0].id.clone();
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    assert!(rig.domain.state().unwrap().interactions[&card].withdrawn, "the card no longer waits for an answer");
    let state = rig.domain.state().unwrap();
    assert_eq!(state.interactions[&card].revision, state.revision, "withdrawing it moves its revision, so clients refresh");
    *rig.sessions.leftovers.lock().unwrap() = None;
    rig.pass(T0 + 20 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1, "the closer does not wait for a withdrawn card");
    let task = rig.task();
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)));
}

#[tokio::test]
async fn a_task_that_goes_back_to_work_while_its_session_is_removed_is_released_to_be_relaunched() {
    let rig = Rig::new(thirty_minutes());
    rig.confirm();
    let domain = rig.domain.clone();
    *rig.sessions.during_rm.lock().unwrap() = Some(Box::new(move || {
        command(&domain, user(), "feedback", Command::SubmitFeedback { merge_failure: None,
            attachments: vec![], presentation_id: "final".into(), targets: text(), body: "One more thing".into(),
            instruction: Instruction::Changes,
        });
    }));
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 1);
    let task = rig.task();
    assert_eq!((task.state, task.agent_id.clone()), (TaskState::NotStarted, None), "ready to be relaunched, not working without a node");
    assert!(task.wrap_up.is_default());
    assert_eq!(rig.closing(), None);
    assert_eq!(rig.domain.live.closed_task("t").map(|node| node.relaunch), Some(crate::live::Relaunch::ByItself),
        "what a relaunch needs is kept, and Alondra starts it by itself");
    let told: Vec<_> = rig.domain.state().unwrap().interactions.values().map(|i| i.body.clone()).collect();
    assert!(!told.iter().any(|body| body.contains("closed while the user asked for changes")), "the orchestrator is not asked to launch it: {told:?}");
}

#[tokio::test]
async fn a_node_that_replaces_the_one_being_removed_is_not_unassigned() {
    let rig = Rig::new(thirty_minutes());
    rig.confirm();
    command(&rig.domain, Actor::System, "register-b", Command::RegisterAgent { agent_id: "agent-b".into(), name: "New node".into(), at: 0 });
    let domain = rig.domain.clone();
    *rig.sessions.during_rm.lock().unwrap() = Some(Box::new(move || {
        command(&domain, user(), "feedback", Command::SubmitFeedback { merge_failure: None,
            attachments: vec![], presentation_id: "final".into(), targets: text(), body: "One more thing".into(),
            instruction: Instruction::Changes,
        });
        command(&domain, Actor::Orchestrator { generation: 0 }, "assign-b", Command::AssignTask { task_id: "t".into(), agent_id: "agent-b".into(), model: None });
    }));
    rig.pass(T0).await;
    assert_eq!(rig.task().agent_id, Some("agent-b".into()), "the node that arrived during claude rm keeps the task");
}

#[tokio::test]
async fn when_closing_the_session_fails_inside_alondra_the_started_remove_is_forgotten() {
    let rig = Rig::new(thirty_minutes());
    rig.confirm();
    let runtime = rig._home.0.join("runtime");
    std::fs::create_dir_all(&runtime).unwrap();
    std::fs::write(runtime.join("sessions"), "not a folder").unwrap();
    let failed = pass(&rig.domain, rig.sessions.as_ref(), &rig.timing, T0).await;
    assert!(failed.is_err(), "the session's settings could not be looked at");
    assert_eq!(rig.sessions.removals(), 1, "claude rm did run");
    let row = rig.closing().unwrap();
    assert_eq!(row.rm_started_at, None, "nothing is left saying a remove is in flight");
}

impl Rig {
    fn declare_post_merge_work(&self, reason: Option<&str>) {
        let id = format!("post-merge-{}", crate::auth::random_hex(6));
        command(&self.domain, agent(), &id, Command::DeclarePostMergeWork { task_id: "t".into(), reason: reason.map(str::to_owned), waits_for: vec![] });
    }
}

#[tokio::test]
async fn an_idle_node_that_declares_work_after_the_merge_is_not_closed_when_the_wait_runs_out() {
    let rig = Rig::new(thirty_minutes());
    rig.pass(T0).await;
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0 + 30 * MINUTE).await;
    rig.pass(T0 + 4 * 60 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    assert_eq!(rig.task().state, TaskState::Running, "the task stays in progress while its work after the merge runs");
    assert!(rig.cards().is_empty());
}

#[tokio::test]
async fn the_wait_runs_again_in_full_once_the_node_stops_declaring_its_work_after_the_merge() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0).await;
    rig.pass(T0 + 60 * MINUTE).await;
    rig.declare_post_merge_work(None);
    rig.pass(T0 + 61 * MINUTE).await;
    rig.pass(T0 + 89 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    rig.pass(T0 + 91 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_busy_node_working_after_the_merge_counts_no_failure() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("deploy, brew and release.sh"));
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    for minutes in [0, 31, 62, 93, 124, 155] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    assert_eq!(rig.sessions.removals(), 0);
    assert_eq!(rig.closing().unwrap().failures, 0);
    assert!(rig.cards().is_empty());
}

#[tokio::test]
async fn cleanup_done_ends_the_work_after_the_merge_and_closes_the_node() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0).await;
    rig.confirm();
    assert_eq!(rig.task().wrap_up.post_merge_work, None);
    rig.pass(T0 + MINUTE).await;
    rig.assert_done(NodeEnd::Closed);
}

impl Rig {
    fn post_merge_cards(&self) -> Vec<alondra_core::domain::Interaction> {
        self.domain.state().unwrap().interactions.values()
            .filter(|i| i.id.0.starts_with("problem-post-merge-") && !i.withdrawn
                && matches!(i.payload, InteractionPayload::Question { answer: None, .. }))
            .cloned().collect()
    }

    fn node_event(&self, event: alondra_core::domain::NodeEvent) {
        crate::domain_agent_lifecycle::node_event(&self.domain, &SESSION.into(), event).unwrap();
    }
}

/// Hours of work declared after the merge and nothing new from the node: nobody asks the node,
/// no card reaches the user, nothing is closed.
#[tokio::test]
async fn hours_of_work_after_the_merge_ask_nothing_raise_no_card_and_close_nothing() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    let told = rig.told().len();
    for hours in [0, 3, 4, 8, 24, 72] {
        rig.pass(T0 + hours * HOUR).await;
    }
    assert!(rig.domain.state().unwrap().post_merge_owed(&rig.task()), "only a fact ends it");
    assert_eq!((rig.sessions.removals(), rig.node.interrupts(), rig.told().len()), (0, 0, told));
    assert!(rig.post_merge_cards().is_empty() && rig.cards().is_empty());
}

#[tokio::test]
async fn a_node_whose_work_after_the_merge_the_user_ended_on_any_card_closes_without_waiting_for_its_cleanup() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0).await;
    let card = format!("{POST_MERGE_PROBLEM_PREFIX}t-raised");
    command(&rig.domain, Actor::System, "raise", Command::RaiseProblem {
        attachments: vec![], interaction_id: card.as_str().into(), scope: Scope::Task { task_id: "t".into() }, body: "Its session closed".into(),
        options: vec![QuestionOption { id: POST_MERGE_CLOSE.into(), label: "Close the task".into(), description: None }],
        card: CardSpec { system: Some(alondra_core::domain::SystemCard::PostMergeProblem), ..CardSpec::new(alondra_core::domain::CardKind::Problem, "Its work after the merge stopped")
            .why("Its session closed while it ran its work after the merge.").content(alondra_core::domain::ContentKind::Diagnosis, "Its session closed") },
    });
    command(&rig.domain, user(), "close", Command::AnswerQuestion {
        attachments: vec![], interaction_id: card.as_str().into(), option_id: POST_MERGE_CLOSE.into(), inputs: Default::default(),
    });
    rig.pass(T0 + MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1, "the user's choice closes the node now, not after the 30 minutes its cleanup gets");
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn closing_the_node_deletes_the_worktree_alondra_made_for_its_task_which_claude_rm_leaves() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    rig.confirm();
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 1);
    assert!(!dir.exists(), "the task's worktree is gone with its session");
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_worktree_with_work_not_saved_keeps_the_task_closing_until_it_can_go() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    std::fs::write(dir.join("a.txt"), "x").unwrap();
    rig.confirm();
    rig.pass(T0).await;
    assert!(dir.join("a.txt").exists());
    assert_eq!(rig.task().state, TaskState::Closing, "a close that leaves the worktree behind is not a close");
    let closing = rig.closing().unwrap();
    assert_eq!(closing.failures, 1);
    assert!(closing.last_error.unwrap().contains("1 uncommitted file(s)"));

    std::fs::remove_file(dir.join("a.txt")).unwrap();
    rig.pass(T0 + 5 * MINUTE).await;
    assert!(!dir.exists(), "the next attempt deletes it although the session is already gone");
    assert_eq!(rig.task().state, TaskState::Done);
}

/// The job Claude Code keeps for the session, naming the worktree `claude rm` deletes with it.
fn job_with_worktree(dir: &std::path::Path) -> crate::test_support::Directory {
    let claude_home = crate::test_support::Directory::new();
    let job = claude_home.join("jobs").join(SHORT);
    std::fs::create_dir_all(&job).unwrap();
    let branch = format!("worktree-{}", dir.file_name().unwrap().to_str().unwrap());
    std::fs::write(job.join("state.json"), json!({"worktreePath": dir, "worktreeBranch": branch}).to_string()).unwrap();
    crate::watcher::use_claude_home_for_test(&claude_home);
    claude_home
}

// D-03: the close decides on the exit status of `claude rm`, the listing of `claude agents --json`
// and `git worktree list --porcelain -z`, never on what `claude rm` printed.
#[tokio::test]
async fn claude_rm_exiting_with_zero_closes_the_node_whatever_it_prints() {
    let rig = Rig::new(thirty_minutes());
    rig.sessions.rm_answers.lock().unwrap().push_back(crate::node_close::Ran::ok(format!("Deleted session {SHORT}")));
    rig.confirm();
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_failed_claude_rm_whose_worktree_git_no_longer_knows_deletes_it_if_identical_and_closes() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "job");
    let _claude_home = job_with_worktree(&dir);
    std::fs::remove_dir_all(root.join("repo/.git/worktrees/job")).unwrap();
    rig.sessions.rm_answers.lock().unwrap().push_back(crate::node_close::Ran::failed("something went wrong"));
    rig.confirm();
    rig.pass(T0).await;
    assert!(!dir.exists(), "its files were its pushed branch, so Alondra deleted the worktree");
    assert_eq!(rig.sessions.removals(), 2, "and ran claude rm again");
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_failed_claude_rm_whose_worktree_git_still_lists_fails_and_deletes_nothing() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "job");
    let _claude_home = job_with_worktree(&dir);
    rig.sessions.rm_answers.lock().unwrap().push_back(crate::node_close::Ran::failed("Git couldn't remove it"));
    rig.confirm();
    rig.pass(T0).await;
    assert!(dir.exists(), "a worktree git still lists is never deleted by hand");
    assert_eq!(rig.sessions.removals(), 1);
    assert_eq!(rig.task().state, TaskState::Closing);
    let closing = rig.closing().unwrap();
    assert_eq!((closing.failures, closing.last_error.as_deref()), (1, Some("Git couldn't remove it")));
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

// The user's rule, 19 October: a task owns every worktree its node's subagents make. One with work
// not committed and one with a commit no remote has hold the close, and the node is told which
// and what to do; a clean one goes. The one the node gives up goes as it is, the one it pushes
// goes once pushed, and only then is the task done.
#[tokio::test]
async fn a_dirty_subagent_worktree_holds_the_close_and_tells_the_node() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let own = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), own.to_str().unwrap()).unwrap();
    let repo = root.join("repo");
    let subagent = |name: &str| {
        let dir = repo.join(".claude/worktrees").join(name);
        git(&repo, &["worktree", "add", "-q", "-b", &format!("worktree-{name}"), dir.to_str().unwrap()]);
        command(&rig.domain, Actor::System, &format!("record-{name}"), Command::RecordTaskWorktree {
            task_id: "t".into(), path: dir.to_str().unwrap().into(), origin: alondra_core::domain::WorktreeOrigin::Subagent, branch: None,
        });
        dir
    };
    let given_up = subagent("agent-a0000000000000001");
    let unpushed = subagent("agent-a0000000000000002");
    let clean = subagent("agent-a0000000000000003");
    std::fs::write(given_up.join("draft.txt"), "x").unwrap();
    git(&unpushed, &["commit", "-q", "--allow-empty", "-m", "fix"]);
    rig.confirm();
    rig.pass(T0).await;
    assert_eq!(rig.task().state, TaskState::Closing, "a task with work left in a worktree of its own does not close");
    let told = rig.told().join("\n");
    for dir in [&given_up, &unpushed] {
        assert!(told.contains(dir.to_str().unwrap()), "the node is told which worktree holds work: {told}");
    }
    assert!(!told.contains(clean.to_str().unwrap()), "a clean worktree is not named: {told}");
    assert!(told.contains("discard"), "and how to give one up: {told}");
    assert!(given_up.join("draft.txt").exists() && unpushed.exists(), "nothing with work is deleted");

    git(&unpushed, &["push", "-q", "origin", "worktree-agent-a0000000000000002"]);
    command(&rig.domain, agent(), "give-up", Command::ConfirmCleanup { task_id: "t".into(), discard: vec![given_up.to_str().unwrap().into()] });
    rig.pass(T0 + 5 * MINUTE).await;
    for dir in [&own, &given_up, &unpushed, &clean] {
        assert!(!dir.exists(), "{} is deleted once nothing in it is lost", dir.display());
    }
    let task = rig.task();
    assert_eq!(task.state, TaskState::Done);
    assert!(task.worktrees.values().all(|worktree| worktree.end == Some(alondra_core::domain::WorktreeEnd::Removed)), "{:?}", task.worktrees);
}

fn pr() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }]
}

impl Rig {
    /// A node Alondra launched whose PR was delivered, accepted and merged, after it said it has
    /// `work` left for after the merge, if any.
    fn merged(work: Option<&str>) -> Self {
        let rig = Self::undelivered(thirty_minutes());
        rig.domain.record_model_decision(&Decision {
            task_id: "t".into(), request_id: "r".into(), agent_id: Some(SESSION.into()), at: T0, kind: DecisionKind::Launch,
            model: None, effort: None, approach: None, chosen_by: ModelChooser::User, reason: None, title: "Result".into(), provider: None, tags: None,
        }).unwrap();
        command(&rig.domain, agent(), "register-pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
            head: "head1".into(), base: "main".into(), allowed_methods: vec![alondra_core::domain::MergeMethod::Squash],
        });
        if work.is_some() {
            rig.declare_post_merge_work(work);
        }
        command(&rig.domain, agent(), "present-pr", Command::PresentResult {
            checklist: Vec::new(), attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(),
            targets: pr(), is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        command(&rig.domain, user(), "accept-pr", Command::AcceptDelivery { presentation_id: "final".into(), targets: pr() });
        command(&rig.domain, Actor::System, "merged", Command::ObservePrState {
            pr_id: "pr".into(), head: "head1".into(), base: "main".into(), state: alondra_core::domain::PrState::Merged,
            merged_head: Some("head1".into()),
        });
        rig
    }

    /// The node's session ends on its own, as Claude Code's SessionEnd hook reports it.
    fn session_ends(&self) {
        self.sessions.listed.lock().unwrap().clear();
        self.domain.live.observe(&hook("SessionEnd"));
        let agent = alondra_core::domain::AgentId(SESSION.into());
        crate::domain_agent_lifecycle::node_event(&self.domain, &agent, alondra_core::domain::NodeEvent::SessionClosed).unwrap();
        crate::domain_transport::session_ended(&self.domain, &agent).unwrap();
    }

    fn with_worktree(&self) -> (crate::test_support::Directory, std::path::PathBuf) {
        let root = crate::test_support::Directory::new();
        let dir = crate::worktree::testing::task_worktree(&root, "t");
        self.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
        (root, dir)
    }
}

#[tokio::test]
async fn a_node_whose_session_ends_after_its_pr_merged_is_closed_at_once_and_its_task_is_done() {
    let rig = Rig::merged(None);
    let (_root, dir) = rig.with_worktree();
    assert_eq!(rig.task().state, TaskState::Closing);
    rig.session_ends();
    rig.pass(T0).await;
    assert!(!dir.exists(), "the task's worktree is gone with its session");
    rig.assert_done(NodeEnd::Gone);
    assert!(rig.closing().is_none());
}

#[tokio::test]
async fn a_node_whose_session_ends_during_its_work_after_the_merge_is_closed_and_its_task_is_done() {
    let rig = Rig::merged(Some("publish the release"));
    let (_root, dir) = rig.with_worktree();
    assert_eq!(rig.task().state, TaskState::Running, "the task stays in progress while its work after the merge runs");
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 0);
    rig.session_ends();
    rig.pass(T0 + MINUTE).await;
    assert!(!dir.exists(), "the task's worktree is gone with its session");
    rig.assert_done(NodeEnd::Gone);
    assert!(rig.post_merge_cards().is_empty(), "nobody is asked about work that cannot run any more");
}

/// A task cancelled before any node took it owes nothing but its place in the launch queue:
/// Alondra takes it out and records the receipt, which is what makes the task cancelled.
#[tokio::test]
async fn a_cancelled_task_without_a_node_is_cancelled_by_alondras_cleanup_receipt() {
    let rig = Rig::undelivered(thirty_minutes());
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "task-u", Command::CreateTask { plan_items: None, continues: None,
        task_id: "u".into(), request_id: "r".into(), title: "Later".into(), agent_id: None,
        review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
    });
    command(&rig.domain, user(), "cancel-u", Command::CancelTask { task_id: "u".into(), reason: None });
    let task = rig.domain.state().unwrap().tasks[&TaskId::from("u")].clone();
    assert_eq!((task.state, task.ending, task.agent_id), (TaskState::Cancelled, None, None));
    assert!(rig.sessions.calls.lock().unwrap().is_empty(), "no node to close");
    assert!(rig.domain.store.lock().unwrap().launch_queue().unwrap().iter().all(|row| row.task_id.0 != "u"));
    assert_eq!(rig.task().state, TaskState::NotStarted, "the other task is untouched");
    let journal: i64 = rig.domain.store.lock().unwrap().conn_for_test()
        .query_row("SELECT count(*) FROM lifecycle_events WHERE task_id='u'", [], |row| row.get(0)).unwrap();
    assert_eq!(journal, 2, "finished and drained after the plan: the receipt came with the commit that cancelled it");
}

/// The task moved to another provider: a task of the same request goes on in the worktree of
/// task `t`, which has work not committed yet, and `t` is cancelled.
fn moved(continues: Option<&str>) -> (Rig, crate::test_support::Directory, std::path::PathBuf) {
    let rig = Rig::undelivered(thirty_minutes());
    rig.domain.record_model_decision(&Decision {
        task_id: "t".into(), request_id: "r".into(), agent_id: Some(SESSION.into()), at: T0, kind: DecisionKind::Launch,
        model: None, effort: None, approach: None, chosen_by: ModelChooser::User, reason: None, title: "Result".into(), provider: None, tags: None,
    }).unwrap();
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    std::fs::write(dir.join("a.txt"), "x").unwrap();
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "continuation", Command::CreateTask {
        task_id: "t-claude".into(), request_id: "r".into(), title: "Result on Claude".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", dir.to_str().unwrap())),
        plan_items: None, continues: continues.map(TaskId::from),
    });
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    (rig, root, dir)
}

fn worktree_end(rig: &Rig, task: &str, dir: &std::path::Path) -> Option<Option<alondra_core::domain::WorktreeEnd>> {
    rig.domain.state().unwrap().tasks[&TaskId::from(task)].worktrees.get(dir.to_str().unwrap()).map(|worktree| worktree.end)
}

#[tokio::test]
async fn a_cancelled_task_whose_worktree_its_continuation_works_in_closes_without_a_card_and_leaves_it() {
    let (rig, _root, dir) = moved(Some("t"));
    for minutes in [0, 5, 10, 20] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    assert!(dir.join("a.txt").exists(), "the continuation's work is still there");
    assert_eq!(rig.task().state, TaskState::Cancelled);
    assert!(rig.cards().is_empty(), "no card about a session Alondra cannot close");
    assert_eq!(worktree_end(&rig, "t", &dir), Some(Some(alondra_core::domain::WorktreeEnd::HandedOver)));
    assert_eq!(worktree_end(&rig, "t-claude", &dir), Some(None), "the continuation holds it now");
}

#[tokio::test]
async fn a_task_launched_in_the_worktree_without_naming_the_task_it_carries_on_takes_it_over() {
    let (rig, _root, dir) = moved(None);
    assert_eq!(worktree_end(&rig, "t-claude", &dir), None, "nothing recorded it yet");
    rig.pass(T0).await;
    assert!(dir.join("a.txt").exists());
    assert_eq!(rig.task().state, TaskState::Cancelled);
    assert!(rig.cards().is_empty());
    assert_eq!(worktree_end(&rig, "t-claude", &dir), Some(None), "it takes the worktree it starts in");
}

#[tokio::test]
async fn a_cancelled_task_whose_continuation_is_already_over_leaves_its_unsaved_worktree_to_the_users_card() {
    let (rig, _root, dir) = moved(Some("t"));
    command(&rig.domain, user(), "cancel-continuation", Command::CancelTask { task_id: "t-claude".into(), reason: None });
    for minutes in [0, 5, 10, 20] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    assert!(dir.join("a.txt").exists(), "work no remote has is never deleted unasked");
    assert_eq!(rig.task().state, TaskState::Cancelled, "the unsaved file no longer keeps the task from closing");
    assert_eq!(worktree_end(&rig, "t", &dir), Some(Some(alondra_core::domain::WorktreeEnd::KeptByUser)), "nobody else goes on in it, so it waits for the user");
    assert_eq!(rig.discard_cards().len(), 1);
    assert!(rig.cards().is_empty());
}

#[tokio::test]
async fn several_continuations_of_one_task_keep_its_worktree_until_the_last_one_ends() {
    let (rig, _root, dir) = moved(Some("t"));
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "second", Command::CreateTask {
        task_id: "t-sonnet".into(), request_id: "r".into(), title: "Result on Sonnet".into(), agent_id: None, review_prs: false,
        dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", dir.to_str().unwrap())),
        plan_items: None, continues: Some("t".into()),
    });
    command(&rig.domain, user(), "cancel-continuation", Command::CancelTask { task_id: "t-claude".into(), reason: None });
    for minutes in [0, 5, 10, 20] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    assert!(dir.join("a.txt").exists());
    assert_eq!(rig.task().state, TaskState::Cancelled);
    assert!(rig.cards().is_empty());
    assert_eq!(worktree_end(&rig, "t-claude", &dir), Some(Some(alondra_core::domain::WorktreeEnd::HandedOver)));
    assert_eq!(worktree_end(&rig, "t-sonnet", &dir), Some(None), "the continuation still working keeps it");
}

use alondra_core::discard_worktrees::DiscardWorktrees;
use alondra_core::domain::SystemCard;

fn branch_exists(repository: &std::path::Path, branch: &str) -> bool {
    let out = std::process::Command::new("git").arg("-C").arg(repository)
        .args(["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).output().unwrap();
    out.status.success()
}

impl Rig {
    fn discard_cards(&self) -> Vec<alondra_core::domain::Interaction> {
        self.domain.state().unwrap().interactions.values().filter(|i| i.system_card == Some(SystemCard::DiscardWorktrees)).cloned().collect()
    }

    fn answer_discard(&self, option: &str) {
        let card = self.discard_cards()[0].id.clone();
        command(&self.domain, user(), &format!("answer-discard-{option}"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: card, option_id: option.into(), inputs: Default::default(),
        });
    }

    fn answer_discard_ticking(&self, tasks: &[&str]) {
        let card = self.discard_cards()[0].id.clone();
        let form = json!({"delete": tasks}).as_object().unwrap().clone();
        command(&self.domain, user(), "answer-discard-ticked", Command::AnswerQuestion {
            attachments: vec![], interaction_id: card, option_id: "accept".into(),
            inputs: [(alondra_core::domain::ANSWER_CONTENT.to_string(), alondra_core::domain::InputValue::Form(form))].into(),
        });
    }

    /// A task already cancelled that has no node, with a worktree of its own left behind.
    fn cancelled_with_worktree(&self, id: &str, unsaved: &[&str]) -> (crate::test_support::Directory, std::path::PathBuf) {
        command(&self.domain, user(), &format!("request-{id}"), Command::CreateRequest {
            request_id: format!("r-{id}").into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(&self.domain, Actor::Orchestrator { generation: 0 }, &format!("create-{id}"), Command::CreateTask {
            plan_items: None, continues: None, task_id: id.into(), request_id: format!("r-{id}").into(), title: format!("Task {id}"), agent_id: None,
            review_prs: false, dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
        command(&self.domain, user(), &format!("cancel-{id}"), Command::CancelTask { task_id: id.into(), reason: None });
        let root = crate::test_support::Directory::new();
        let dir = crate::worktree::testing::task_worktree(&root, id);
        for file in unsaved {
            std::fs::write(dir.join(file), "x").unwrap();
        }
        self.domain.store.lock().unwrap().set_task_worktree(&id.into(), dir.to_str().unwrap(), T0).unwrap();
        (root, dir)
    }
}

#[tokio::test]
async fn cancelling_a_task_with_unsaved_files_asks_once_and_deletes_nothing_until_the_user_says_delete() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    std::fs::write(dir.join("a.txt"), "x").unwrap();
    std::fs::write(dir.join("b.txt"), "y").unwrap();
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    rig.pass(T0).await;
    rig.pass(T0 + MINUTE).await;

    let task = rig.task();
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)), "the cancelled task closes; its files wait for the user");
    assert!(dir.join("a.txt").exists() && dir.join("b.txt").exists(), "nothing is deleted before the user answers");
    assert!(rig.cards().is_empty(), "no card about a session that cannot close");
    let cards = rig.discard_cards();
    assert_eq!(cards.len(), 1);
    let card = DiscardWorktrees::from_body(&cards[0].body).expect("the body holds what clients word");
    assert_eq!((card.tasks.len(), card.tasks[0].task_id.as_str(), card.tasks[0].files), (1, "t", 2));
    let InteractionPayload::Question { options, answer: None } = &cards[0].payload else { panic!("a question card") };
    assert_eq!(options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["discard", "keep"]);
    rig.pass(T0 + 60 * MINUTE).await;
    assert_eq!(rig.discard_cards().len(), 1, "asked once");
    assert!(dir.exists());

    rig.answer_discard("discard");
    rig.pass(T0 + 61 * MINUTE).await;
    assert!(!dir.exists(), "Delete removes the worktree");
    assert!(!branch_exists(&root.join("repo"), "worktree-t"), "and its local branch, which has no open pull request");
    assert_eq!(rig.discard_cards().len(), 1);
}

#[tokio::test]
async fn keeping_the_unsaved_files_of_a_cancelled_task_touches_nothing_and_never_asks_again() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    std::fs::write(dir.join("a.txt"), "x").unwrap();
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    rig.pass(T0).await;
    rig.answer_discard("keep");
    for minutes in [1, 60, 600] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    assert!(dir.join("a.txt").exists());
    assert!(branch_exists(&root.join("repo"), "worktree-t"));
    assert_eq!(rig.discard_cards().len(), 1, "the answered card is why nobody asks again");
}

#[tokio::test]
async fn cancelling_a_task_without_unsaved_files_deletes_its_worktree_and_branch_without_a_card() {
    let rig = Rig::new(thirty_minutes());
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    rig.pass(T0).await;
    rig.pass(T0 + MINUTE).await;
    assert!(!dir.exists());
    assert!(!branch_exists(&root.join("repo"), "worktree-t"));
    assert!(rig.discard_cards().is_empty() && rig.cards().is_empty());
    assert_eq!(rig.task().state, TaskState::Cancelled);
}

#[tokio::test]
async fn a_cancelled_task_with_an_open_pull_request_keeps_its_branch_when_its_worktree_goes() {
    let rig = Rig::new(thirty_minutes());
    command(&rig.domain, agent(), "register-pr", Command::RegisterPr {
        task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(), number: 1,
        head: "head1".into(), base: "main".into(), allowed_methods: vec![alondra_core::domain::MergeMethod::Squash],
    });
    let root = crate::test_support::Directory::new();
    let dir = crate::worktree::testing::task_worktree(&root, "t");
    rig.domain.set_task_worktree(&"t".into(), dir.to_str().unwrap()).unwrap();
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    rig.pass(T0).await;
    rig.pass(T0 + MINUTE).await;
    assert!(!dir.exists());
    assert!(branch_exists(&root.join("repo"), "worktree-t"), "the open pull request still needs its branch");
}

#[tokio::test]
async fn the_cancelled_worktrees_left_before_this_rule_are_settled_with_one_card() {
    let rig = Rig::undelivered(thirty_minutes());
    let (_a_root, a) = rig.cancelled_with_worktree("t-a", &["one.txt", "two.txt"]);
    let (b_root, b) = rig.cancelled_with_worktree("t-b", &[]);
    let (_c_root, c) = rig.cancelled_with_worktree("t-c", &["three.txt"]);
    for task in ["t-a", "t-b", "t-c"] {
        assert_eq!(rig.domain.state().unwrap().tasks[&TaskId::from(task)].state, TaskState::Cancelled);
    }

    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    assert!(!b.exists(), "a worktree with nothing unsaved goes without asking");
    assert!(!branch_exists(&b_root.join("repo"), "worktree-t-b"));
    assert!(a.join("one.txt").exists() && c.join("three.txt").exists(), "the others wait for the user");
    let cards = rig.discard_cards();
    assert_eq!(cards.len(), 1, "one card for all of them");
    let card = DiscardWorktrees::from_body(&cards[0].body).unwrap();
    assert_eq!(card.tasks.iter().map(|task| (task.task_id.as_str(), task.files)).collect::<Vec<_>>(), [("t-a", 2), ("t-c", 1)]);
    let (options, _) = card.options();
    assert_eq!(options.iter().map(|o| o.label.as_str()).collect::<Vec<_>>(), ["Delete the ticked", "Delete all", "Keep all"]);

    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    assert_eq!(rig.discard_cards().len(), 1, "a second start does not repeat the card");

    rig.answer_discard("discard");
    rig.pass(T0).await;
    assert!(!a.exists() && !c.exists(), "Delete all removes the listed worktrees");
}

#[tokio::test]
async fn keeping_all_the_existing_cancelled_worktrees_deletes_none_and_no_later_start_asks_again() {
    let rig = Rig::undelivered(thirty_minutes());
    let (_a_root, a) = rig.cancelled_with_worktree("t-a", &["one.txt"]);
    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    rig.answer_discard("keep");
    rig.pass(T0).await;
    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    assert!(a.join("one.txt").exists());
    assert_eq!(rig.discard_cards().len(), 1);
}

#[tokio::test]
async fn deciding_the_existing_worktrees_one_by_one_deletes_only_the_ticked_and_leaves_the_others_untouched() {
    let rig = Rig::undelivered(thirty_minutes());
    let (_a_root, a) = rig.cancelled_with_worktree("t-a", &["one.txt"]);
    let (b_root, b) = rig.cancelled_with_worktree("t-b", &["two.txt"]);
    let (_c_root, c) = rig.cancelled_with_worktree("t-c", &["three.txt"]);
    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    let cards = rig.discard_cards();
    assert_eq!(cards.len(), 1);
    let spec = &cards[0].card;
    let form = spec.form.as_ref().expect("the card lets the user tick the worktrees to delete");
    let alondra_core::requested_form::RequestedFieldKind::Choices { options, .. } = &form.fields[0].kind else { panic!("a multiple choice") };
    assert_eq!(options.iter().map(|o| o.value.as_str()).collect::<Vec<_>>(), ["t-a", "t-b", "t-c"]);

    rig.answer_discard_ticking(&["t-a", "t-b"]);
    rig.pass(T0).await;
    assert!(!a.exists() && !b.exists(), "the ticked worktrees are deleted");
    assert!(!branch_exists(&b_root.join("repo"), "worktree-t-b"));
    assert!(c.join("three.txt").exists(), "the one left unticked is intact");
    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    rig.pass(T0 + MINUTE).await;
    assert!(c.join("three.txt").exists(), "and nobody asks about it again");
    assert_eq!(rig.discard_cards().len(), 1);
}

/// A home and a projects root of the test's own, the projects root a git repository and each with
/// a file of the user's, which the domain keeps from ever being a task's worktree.
struct UsersFolders {
    _scratch: crate::test_support::Directory,
    home: std::path::PathBuf,
    projects: std::path::PathBuf,
    plain: std::path::PathBuf,
}

impl UsersFolders {
    fn protecting(domain: &DomainRuntime) -> Self {
        let scratch = crate::test_support::Directory::new();
        let (home, plain) = (scratch.join("home"), scratch.join("plain"));
        let projects = home.join("Projects");
        for dir in [&home, &projects, &plain] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("notes.txt"), "mine").unwrap();
        }
        git(&projects, &["init", "-q"]);
        domain.store.lock().unwrap().protect(crate::worktree::Protected { home: home.clone(), projects_root: projects.clone() });
        Self { _scratch: scratch, home, projects, plain }
    }

    fn untouched(&self) -> bool {
        [&self.home, &self.projects, &self.plain].iter().all(|dir| dir.join("notes.txt").exists())
    }

    /// A row a daemon before this check wrote, straight into the store.
    fn stored_as_worktree_of(&self, domain: &DomainRuntime, task: &str, path: &std::path::Path) {
        domain.store.lock().unwrap().conn.execute(
            "INSERT INTO task_worktree_records(task_id,path,origin,branch,discard,end) VALUES(?1,?2,'inherited',NULL,0,NULL)",
            rusqlite::params![task, path.to_str().unwrap()],
        ).unwrap();
        domain.reload();
    }
}

fn record_worktree(domain: &DomainRuntime, task: &str, path: &std::path::Path) -> Result<(), crate::domain_store::StoreError> {
    crate::domain_github::execute_system(domain, Command::RecordTaskWorktree {
        task_id: task.into(), path: path.to_str().unwrap().into(), origin: alondra_core::domain::WorktreeOrigin::Entered, branch: None,
    })
}

#[tokio::test]
async fn the_domain_refuses_home_the_projects_root_and_a_folder_git_does_not_know_as_a_worktree() {
    let rig = Rig::new(thirty_minutes());
    let folders = UsersFolders::protecting(&rig.domain);
    for path in [&folders.home, &folders.projects, &folders.plain] {
        let refused = record_worktree(&rig.domain, "t", path).expect_err("only a git worktree is recorded");
        assert!(matches!(&refused, crate::domain_store::StoreError::Domain(error) if error.code == "invalid_input"), "{}: {refused:?}", path.display());
    }
    assert!(rig.domain.set_task_worktree(&"t".into(), folders.projects.to_str().unwrap()).is_err(), "the task's own folder is no exception");
    let root = crate::test_support::Directory::new();
    let worktree = crate::worktree::testing::task_worktree(&root, "t");
    record_worktree(&rig.domain, "t", &worktree).unwrap();
    assert_eq!(rig.task().worktrees.keys().cloned().collect::<Vec<_>>(), [worktree.to_str().unwrap().to_owned()]);
    assert!(folders.untouched());
}

#[tokio::test]
async fn closing_a_task_launched_in_the_projects_root_hands_it_to_no_other_task_launched_there_and_deletes_nothing() {
    let rig = Rig::new(thirty_minutes());
    let folders = UsersFolders::protecting(&rig.domain);
    rig.domain.store.lock().unwrap().set_task_worktree(&"t".into(), folders.projects.to_str().unwrap(), T0).unwrap();
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "r".into(), title: "Work".into() });
    command(&rig.domain, Actor::Orchestrator { generation: 0 }, "other", Command::CreateTask {
        plan_items: None, continues: None, task_id: "t-other".into(), request_id: "r".into(), title: "Other".into(), agent_id: None,
        review_prs: false, dependencies: Default::default(), stack: None,
        launch: Some(alondra_core::domain::LaunchPlan::new("node", folders.projects.to_str().unwrap())),
    });
    rig.confirm();
    rig.pass(T0).await;
    rig.assert_done(NodeEnd::Closed);
    assert!(rig.domain.state().unwrap().tasks[&TaskId::from("t-other")].worktrees.is_empty(), "the projects root is nobody's worktree");
    assert!(folders.untouched());
}

#[tokio::test]
async fn stored_records_of_folders_that_are_no_worktree_are_ignored_and_the_task_closes_deleting_nothing() {
    let rig = Rig::new(thirty_minutes());
    let folders = UsersFolders::protecting(&rig.domain);
    folders.stored_as_worktree_of(&rig.domain, "t", &folders.plain);
    folders.stored_as_worktree_of(&rig.domain, "t", &folders.projects);
    let state = rig.domain.state().unwrap();
    let id = format!("confirm-discard-{}", crate::auth::random_hex(6));
    let discard = rig.domain.execute(&agent(), &CommandEnvelope {
        protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.as_str().into(), expected_revision: state.revision,
        command: Command::ConfirmCleanup { task_id: "t".into(), discard: vec![folders.plain.to_str().unwrap().into()] },
    }, None, || Ok(()));
    assert!(discard.is_err(), "a folder without git is read as no worktree of the task, so the node cannot give it up");
    assert!(!rig.task().worktrees.contains_key(folders.plain.to_str().unwrap()), "the folder without git is ignored when read");
    rig.confirm();
    rig.pass(T0).await;
    rig.assert_done(NodeEnd::Closed);
    assert_eq!(rig.task().worktrees[folders.projects.to_str().unwrap()].end, Some(alondra_core::domain::WorktreeEnd::UsersOwn),
        "the projects root, a git repository, is left as the user's own");
    let rows: i64 = rig.domain.store.lock().unwrap().conn
        .query_row("SELECT count(*) FROM task_worktree_records WHERE task_id='t'", [], |row| row.get(0)).unwrap();
    assert_eq!(rows, 2, "no stored row is deleted for them");
    assert!(folders.untouched());
}

#[tokio::test]
async fn cancelling_a_task_whose_stored_worktrees_are_home_and_the_projects_root_deletes_nothing_and_asks_nothing() {
    let rig = Rig::new(thirty_minutes());
    let folders = UsersFolders::protecting(&rig.domain);
    git(&folders.home, &["init", "-q"]);
    folders.stored_as_worktree_of(&rig.domain, "t", &folders.home);
    folders.stored_as_worktree_of(&rig.domain, "t", &folders.projects);
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
    for minutes in [0, 5, 10] {
        rig.pass(T0 + minutes * MINUTE).await;
    }
    crate::cancelled_worktrees::sweep(&rig.domain).await.unwrap();
    assert_eq!(rig.task().state, TaskState::Cancelled);
    assert!(rig.discard_cards().is_empty(), "nobody is asked to delete the home or the projects root");
    for dir in [&folders.home, &folders.projects] {
        assert_eq!(rig.task().worktrees[dir.to_str().unwrap()].end, Some(alondra_core::domain::WorktreeEnd::UsersOwn));
    }
    assert!(folders.untouched());
}
