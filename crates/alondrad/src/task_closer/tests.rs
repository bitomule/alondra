use super::*;
use crate::domain_store::model_metrics::{Decision, DecisionKind};
use crate::node_close::Answer;
use crate::node_push::testing::Node;
use alondra_core::domain::{
    Actor, CommandEnvelope, DeliveryTarget, Instruction, ModelChooser, PROTOCOL_VERSION, TaskState,
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
}

impl FakeSessions {
    fn listing(ids: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            listed: Mutex::new(ids.iter().map(|id| id.to_string()).collect()),
            leftovers: Mutex::new(None),
            calls: Default::default(),
            watch_while_removing: Mutex::new(None),
            during_rm: Mutex::new(None),
        })
    }
    fn removals(&self) -> usize {
        self.calls.lock().unwrap().iter().filter(|call| call[0] == "rm").count()
    }
}

impl Sessions for FakeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<String, String>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(args.clone());
            match args[0].as_str() {
                "agents" => Ok(json!(self.listed.lock().unwrap().iter().map(|id| json!({"id": id})).collect::<Vec<_>>()).to_string()),
                "rm" => {
                    if let Some(hook) = self.during_rm.lock().unwrap().take() {
                        hook();
                    }
                    if let Some((domain, seen)) = self.watch_while_removing.lock().unwrap().as_mut() {
                        *seen = Some(domain.live.begin_watch(SESSION));
                    }
                    self.listed.lock().unwrap().retain(|id| id != &args[1]);
                    Ok(format!("removed {}", args[1]))
                }
                _ => Err("unexpected".into()),
            }
        })
    }

    fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
        Box::pin(async move { self.leftovers.lock().unwrap().clone() })
    }
}

struct Home(std::path::PathBuf);
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
            model: None, effort: None, approach: None, chosen_by: ModelChooser::User, reason: None, title: "Result".into(),
        }).unwrap();
        rig
    }

    fn without_a_launch(wait: Option<Duration>) -> Self {
        let home = std::env::temp_dir().join(format!("alondra-closer-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        command(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: SESSION.into(), name: "Node".into() });
        command(&domain, user(), "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false });
        command(&domain, user(), "task", Command::CreateTask {
            task_id: "t".into(), request_id: "r".into(), title: "Result".into(), agent_id: Some(SESSION.into()),
            review_prs: false, dependencies: Default::default(), stack: None,
        });
        domain.live.apply(&Event::DaemonSeen(DaemonObservation {
            session_id: SESSION.into(), short_id: SHORT.into(), name: Some("Node".into()), ..Default::default()
        }));
        domain.live.observe(&hook("SessionStart"));
        domain.live.observe(&hook("Stop"));
        let node = Node::connect(&domain, SESSION);
        let sessions = FakeSessions::listing(&[SHORT]);
        let rig = Self { _home: Home(home), domain, sessions, node, timing: Timing::new(wait) };
        rig.deliver_and_accept();
        rig
    }

    fn deliver_and_accept(&self) {
        command(&self.domain, agent(), "present", Command::PresentResult {
            attachments: vec![], task_id: "t".into(), interaction_id: "final".into(), body: "Done".into(), targets: text(),
            is_final: true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
        });
        command(&self.domain, user(), "accept", Command::AcceptDelivery { presentation_id: "final".into(), targets: text() });
    }

    fn confirm(&self) {
        let id = format!("confirm-{}", crate::auth::random_hex(6));
        command(&self.domain, agent(), &id, Command::ConfirmCleanup { task_id: "t".into() });
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

#[tokio::test]
async fn a_refused_close_tells_the_node_why_and_tries_again_after_the_pause_or_its_confirmation() {
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
    assert_eq!(rig.told().len(), 2, "tried again after the pause");

    rig.confirm();
    rig.pass(T0 + 6 * MINUTE).await;
    assert_eq!(rig.told().len(), 3, "a new confirmation tries at once");
    assert_eq!(rig.closing().unwrap().failures, 3);
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
async fn a_cancelled_tasks_node_is_closed_once_idle_and_gets_no_message() {
    let rig = Rig::new(thirty_minutes());
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into() });
    let told = rig.told().len();
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    rig.pass(T0).await;
    assert_eq!(rig.sessions.removals(), 0, "not while it works");
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
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into() });
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
    command(&rig.domain, user(), "feedback", Command::SubmitFeedback {
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

#[tokio::test]
async fn a_node_that_is_busy_gets_the_margin_from_when_it_was_ready_not_from_when_the_task_was_seen() {
    let rig = Rig::new(thirty_minutes());
    rig.pass(T0).await;
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    rig.pass(T0 + 40 * MINUTE).await;
    assert_eq!(rig.closing().unwrap().failures, 0, "ready and busy for the first time: the margin starts now");
    rig.pass(T0 + 69 * MINUTE).await;
    assert_eq!(rig.closing().unwrap().failures, 0);
    assert_eq!(rig.sessions.removals(), 0);
    rig.pass(T0 + 71 * MINUTE).await;
    let row = rig.closing().unwrap();
    assert_eq!((row.failures, row.last_error.as_deref()), (1, Some("it is still working")));
    assert!(rig.told().last().unwrap().contains("it is still working"));
}

#[tokio::test]
async fn cancelling_a_task_withdraws_the_card_of_three_failed_closes_and_the_node_is_closed() {
    let rig = Rig::new(thirty_minutes());
    failed_three_times(&rig).await;
    let card = rig.cards()[0].id.clone();
    command(&rig.domain, user(), "cancel", Command::CancelTask { task_id: "t".into() });
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
        command(&domain, user(), "feedback", Command::SubmitFeedback {
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
    assert!(rig.domain.live.closed_task("t").is_some(), "what a relaunch needs is kept");
    let told: Vec<_> = rig.domain.state().unwrap().interactions.values().map(|i| i.body.clone()).collect();
    assert!(told.iter().any(|body| body.contains("closed while the user asked for changes") && body.contains("launch (task t)")), "{told:?}");
}

#[tokio::test]
async fn a_node_that_replaces_the_one_being_removed_is_not_unassigned() {
    let rig = Rig::new(thirty_minutes());
    rig.confirm();
    command(&rig.domain, Actor::System, "register-b", Command::RegisterAgent { agent_id: "agent-b".into(), name: "New node".into() });
    let domain = rig.domain.clone();
    *rig.sessions.during_rm.lock().unwrap() = Some(Box::new(move || {
        command(&domain, user(), "feedback", Command::SubmitFeedback {
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
        command(&self.domain, agent(), &id, Command::DeclarePostMergeWork { task_id: "t".into(), reason: reason.map(str::to_owned) });
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
    assert_eq!(rig.task().state, TaskState::Closing);
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

    fn answer_post_merge_card(&self, option: &str) {
        let card = self.post_merge_cards()[0].id.clone();
        command(&self.domain, user(), &format!("answer-{option}-{}", crate::auth::random_hex(4)), Command::AnswerQuestion {
            attachments: vec![], interaction_id: card, option_id: option.into(), inputs: Default::default(),
        });
    }

    fn asked_whether_it_still_runs(&self) -> usize {
        self.told().iter().filter(|text| text.contains("still running after the merge")).count()
    }
}

const HOUR: i64 = 60 * MINUTE;

#[tokio::test]
async fn after_three_hours_alondra_asks_the_node_and_an_idle_node_that_does_not_answer_goes_to_the_user() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0).await;
    rig.pass(T0 + 3 * HOUR - MINUTE).await;
    assert_eq!(rig.asked_whether_it_still_runs(), 0);
    rig.pass(T0 + 3 * HOUR).await;
    assert_eq!(rig.asked_whether_it_still_runs(), 1);
    rig.pass(T0 + 3 * HOUR + 29 * MINUTE).await;
    assert!(rig.post_merge_cards().is_empty());
    rig.pass(T0 + 3 * HOUR + 30 * MINUTE).await;
    let cards = rig.post_merge_cards();
    assert_eq!(cards.len(), 1);
    let card = serde_json::to_string(&cards[0]).unwrap();
    assert!(card.contains("“Result”") && card.contains("3 h 30 min") && card.contains("release.sh publishes 0.2.54")
        && card.contains("Close the node") && card.contains("Leave it"), "{card}");
    assert_eq!(rig.sessions.removals(), 0, "nothing is closed without the user");
    rig.pass(T0 + 5 * HOUR).await;
    assert_eq!((rig.sessions.removals(), rig.post_merge_cards().len(), rig.asked_whether_it_still_runs()), (0, 1, 1));
    rig.answer_post_merge_card("close");
    rig.pass(T0 + 5 * HOUR + MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1);
    rig.assert_done(NodeEnd::Closed);
}

#[tokio::test]
async fn a_node_that_declares_again_or_is_working_never_reaches_the_user_and_leave_it_starts_another_round() {
    let rig = Rig::new(thirty_minutes());
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0).await;
    rig.pass(T0 + 3 * HOUR).await;
    rig.declare_post_merge_work(Some("release.sh publishes 0.2.54"));
    rig.pass(T0 + 4 * HOUR).await;
    assert!(rig.post_merge_cards().is_empty(), "declaring again is the answer");
    rig.pass(T0 + 7 * HOUR - MINUTE).await;
    assert_eq!(rig.asked_whether_it_still_runs(), 1, "the next round counts from the new declaration");
    rig.pass(T0 + 7 * HOUR).await;
    assert_eq!(rig.asked_whether_it_still_runs(), 2);
    rig.domain.live.observe(&hook("UserPromptSubmit"));
    rig.pass(T0 + 8 * HOUR).await;
    assert!(rig.post_merge_cards().is_empty(), "a working node is not reported");
    rig.domain.live.observe(&hook("Stop"));
    rig.pass(T0 + 8 * HOUR + MINUTE).await;
    assert_eq!(rig.post_merge_cards().len(), 1);
    rig.answer_post_merge_card("leave");
    rig.pass(T0 + 8 * HOUR + 2 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    assert!(rig.post_merge_cards().is_empty());
    rig.pass(T0 + 11 * HOUR + 2 * MINUTE).await;
    assert_eq!(rig.asked_whether_it_still_runs(), 3);
    rig.declare_post_merge_work(None);
    rig.pass(T0 + 11 * HOUR + 3 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 0);
    rig.pass(T0 + 11 * HOUR + 34 * MINUTE).await;
    assert_eq!(rig.sessions.removals(), 1, "once the work ends the usual wait closes it");
}
