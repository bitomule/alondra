//! The vertical slice through the real owner: the daemon's store, commands and `own`, the pinned
//! Node host with the official SDK, and a fake native Claude that answers each prompt with a
//! result. Each step waits on the host's next event, never on a clock.
use super::*;
use crate::test_support::Directory;
use rusqlite::Connection;
use std::collections::BTreeMap;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const TASK: &str = "sdk-slice";

fn runfile(name: &str) -> PathBuf {
    let path = PathBuf::from(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile")));
    std::fs::canonicalize(path).unwrap()
}

struct Slice {
    domain: Arc<DomainRuntime>,
    database: PathBuf,
    capture: PathBuf,
    worktree: PathBuf,
    backend: SdkBackend,
    _home: Directory,
    _state: Directory,
}

fn user() -> Actor {
    Actor::User { user_id: "slice-user".into() }
}

fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

impl Slice {
    fn new() -> Self {
        let home = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let state = Directory::for_unix_socket();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        let native = home.join("native");
        std::fs::copy(runfile("FIXTURE_NATIVE"), &native).unwrap();
        std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
        let capture = home.join("native-capture.json");
        let environment = BTreeMap::from([("HOME".to_owned(), home.to_str().unwrap().to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        let mut native_environment = environment.clone();
        native_environment.insert("FIXTURE_CAPTURE".into(), capture.to_str().unwrap().into());
        let (node, host) = (runfile("FIXTURE_NODE"), runfile("FIXTURE_HOST"));
        let backend = SdkBackend {
            node_sha256: backend::digest(&node).unwrap(),
            host_sha256: backend::digest(&host).unwrap(),
            native_sha256: backend::digest(&native).unwrap(),
            node,
            host,
            native,
            host_environment: environment,
            native_environment,
            confinement: None,
            state_root: state.join("h"),
            io_timeout_seconds: 30,
        };
        let database = home.join("domain.sqlite");
        let domain = Arc::new(DomainRuntime::open_at(&home, &database).unwrap());
        let slice = Self { domain, database, capture, worktree, backend, _home: home, _state: state };
        slice.run(&Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "sdk-slice-request".into(),
            body: "Write the release notes".into(),
            auto_merge: false,
            maintenance: false,
            message: false,
        });
        slice.run(&Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Release notes".into() });
        slice.run(&Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask {
            plan_items: None,
            task_id: TASK.into(),
            request_id: "sdk-slice-request".into(),
            title: "Write the release notes".into(),
            agent_id: None,
            review_prs: false,
            dependencies: BTreeSet::new(),
            stack: None,
            launch: Some(LaunchPlan::new("node", slice.worktree.to_str().unwrap())),
        });
        slice
    }

    fn run(&self, actor: &Actor, id: &str, command: Command) {
        let state = self.domain.state().unwrap();
        self.domain.execute_with(actor, &envelope(&state, id, command), None, None, || Ok(())).unwrap();
    }

    fn launch(&self) -> Driver {
        self.launch_with("Write the release notes")
    }

    fn launch_with(&self, prompt: &str) -> Driver {
        start(Arc::clone(&self.domain), &self.backend, Launch {
            task: TASK.into(),
            name: "Release notes".into(),
            directory: self.worktree.to_str().unwrap().into(),
            prompt: prompt.into(),
        })
        .unwrap()
    }

    /// Reads the host until the task's lifecycle no longer waits for it.
    fn settle(&self, driver: &mut Driver) {
        while driver.waits_for_host() {
            driver.observe().unwrap();
        }
    }

    fn prompts(&self) -> Vec<String> {
        let path = PathBuf::from(format!("{}.prompts", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn observer(&self) -> Connection {
        Connection::open(&self.database).unwrap()
    }

    /// Every turn-end card of the task, in the read the clients get, with whether it is answered.
    fn read_cards(domain: &DomainRuntime) -> Vec<(String, bool)> {
        let read = domain.read();
        read.state()
            .interactions
            .values()
            .filter(|card| card.id.0.starts_with(&format!("{TURN_ENDED_PREFIX}{TASK}-")))
            .map(|card| (card.id.0.clone(), matches!(&card.payload, InteractionPayload::Question { answer: Some(_), .. })))
            .collect()
    }

    fn stored_cards(&self) -> i64 {
        self.observer()
            .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0))
            .unwrap()
    }
}

fn committed_revision(database: &Path) -> u64 {
    Connection::open(database).unwrap().query_row("SELECT revision FROM domain_meta WHERE singleton=1", [], |row| row.get::<_, i64>(0)).unwrap() as u64
}

/// At every acknowledgement, what readers see is the revision SQL committed.
fn watch_acknowledgements(slice: &Slice) -> Arc<Mutex<Vec<(u64, u64)>>> {
    let seen = Arc::new(Mutex::new(vec![]));
    let (recorded, database, domain) = (Arc::clone(&seen), slice.database.clone(), Arc::downgrade(&slice.domain));
    slice.domain.on_acknowledge(move || {
        let (published, committed) = (domain.upgrade().unwrap().read().state().revision, committed_revision(&database));
        assert_eq!(published, committed, "readers see the committed revision when its observation is acknowledged");
        recorded.lock().unwrap().push((published, committed));
    });
    seen
}

// A1: a task launched through the SDK backend runs its first turn on the real host; the end of
// that turn commits one card with the raw observation, and every acknowledgement leaves only
// once readers see the commit it seals.
#[test]
fn a_task_launched_through_the_sdk_ends_its_turn_with_one_card_readers_see_before_the_host_is_acknowledged() {
    let slice = Slice::new();
    let acknowledged = watch_acknowledgements(&slice);
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes"], "the host's Claude got the launch prompt");
    let cards = Slice::read_cards(&slice.domain);
    assert_eq!(cards.len(), 1, "one turn-end card: {cards:?}");
    assert!(!cards[0].1, "the card waits for the user");
    assert_eq!(slice.stored_cards(), 1);
    let acknowledged = acknowledged.lock().unwrap().clone();
    assert!(!acknowledged.is_empty(), "the host's observations were acknowledged");
    for (published, committed) in acknowledged {
        assert_eq!(published, committed, "readers see the committed revision when its observation is acknowledged");
    }
    let raw: i64 = slice.observer().query_row("SELECT count(*) FROM provider_observations WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap();
    assert!(raw >= 2, "the raw observations are kept: {raw}");
    let offered = node_offers(&slice.domain, driver.agent());
    for claude_bg in ["attach", "msg:", "interrupt:", "close:"] {
        assert!(!offered.iter().any(|offer| offer.starts_with(claude_bg)), "an SDK node offers no {claude_bg}: {offered:?}");
    }
    driver.stop().unwrap();
}

/// What the TUI's Fleet detail of `agent` offers, given the live node the daemon gathers for it.
fn node_offers(domain: &DomainRuntime, agent: &AgentId) -> Vec<String> {
    use alondra_core::views::{Client, Item, RenderContext, Transport, build, live::{LiveNode, LiveSnapshot}};
    let read = domain.read();
    let live = LiveSnapshot { nodes: vec![LiveNode { agent_id: agent.0.clone(), short_id: Some(agent.0.chars().take(8).collect()), ..Default::default() }], ..Default::default() };
    let view = build(&format!("node:{agent}"), RenderContext {
        domain: read.state(), index: read.index(), live: &live, client: Client::Tui, lang: alondra_core::views::text::Lang::En,
        transport: Transport::Loopback, cursor: "test", rendered_at: "",
    }).unwrap();
    let mut offers = vec![];
    for section in &view.sections {
        if section.id == "attach" {
            offers.push("attach".to_owned());
        }
        for item in &section.items {
            if let Item::Actions(bar) = item {
                offers.extend(bar.actions.iter().map(|action| action.id.clone()));
            }
        }
    }
    offers
}

// A2: the user answers the card from a client, through the normal command path. The answer
// commits with its dispatch prepared, the owner claims and sends it, and the host's Claude gets
// what it is told; its next turn ends with a card of its own.
#[test]
fn carrying_on_from_a_client_commits_and_reaches_the_sdk() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let (card, _) = Slice::read_cards(&slice.domain).pop().unwrap();
    let state = slice.domain.state().unwrap();
    let carry_on = Command::AnswerQuestion {
        interaction_id: card.as_str().into(),
        option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Add the tests first".into()))]),
        attachments: vec![],
    };
    let outcome = slice.domain.execute_with(&user(), &envelope(&state, "client-carry-on", carry_on), None, None, || Ok(()));
    assert!(outcome.as_ref().is_ok_and(|outcome| outcome.changed), "the client's answer is confirmed: {outcome:?}");
    assert_eq!(driver.pending_permits(), 1, "its dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Go on with your task") && prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    let mut cards = Slice::read_cards(&slice.domain);
    cards.sort();
    assert_eq!(cards.len(), 2, "{cards:?}");
    assert_eq!(cards.iter().filter(|(_, answered)| *answered).count(), 1, "the first card is answered, the second waits: {cards:?}");
    let (second, _) = cards.iter().find(|(_, answered)| !answered).unwrap().clone();
    let state = slice.domain.state().unwrap();
    let written = Command::AnswerQuestion {
        interaction_id: second.as_str().into(),
        option_id: String::new(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text("Write the changelog too".into()))]),
        attachments: vec![],
    };
    let outcome = slice.domain.execute_with(&user(), &envelope(&state, "client-writes", written), None, None, || Ok(()));
    assert!(outcome.as_ref().is_ok_and(|outcome| outcome.changed), "what the user writes on the card is confirmed: {outcome:?}");
    assert_eq!(driver.pending_permits(), 1);
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts().last().map(String::as_str), Some("Write the changelog too"), "it reaches the host's Claude as written");
    let message: crate::domain_messages::MessageBody = serde_json::from_value(serde_json::json!({"text": "Also update the changelog"})).unwrap();
    let refused = tokio::runtime::Runtime::new().unwrap()
        .block_on(crate::domain_messages::deliver(&slice.domain, &slice._home, &user(), "David", driver.agent(), message, true))
        .unwrap_err();
    assert_eq!(refused.code, "unsupported", "a message outside its cards is refused with its reason: {refused:?}");
    assert_eq!(slice.prompts().len(), 3, "nothing more reached the host's Claude");
    driver.stop().unwrap();
}

// A3: the writer stops between the commit of the turn end and its publication. A runtime opened
// on the same database gets the host's unacknowledged event again: the exact replay publishes
// the committed card once and acknowledges it, with no second card.
#[test]
fn a_crash_between_commit_and_publication_reconciles_without_a_second_card() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    let database = slice.database.clone();
    slice.domain.before_owner_publish(move || {
        let cards: i64 = Connection::open(&database)
            .unwrap()
            .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0))
            .unwrap();
        if cards > 0 {
            panic!("the daemon stopped after the commit");
        }
    });
    let crash = loop {
        if let Err(crash) = catch_unwind(AssertUnwindSafe(|| driver.observe().unwrap())) {
            break crash;
        }
    };
    assert_eq!(crash.downcast_ref::<&str>(), Some(&"the daemon stopped after the commit"));
    assert_eq!(slice.stored_cards(), 1, "the turn end committed its card");
    assert!(Slice::read_cards(&slice.domain).is_empty(), "nothing of it was published");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let acknowledged = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&acknowledged);
    restarted.on_acknowledge(move || *counter.lock().unwrap() += 1);
    driver.rebind(Arc::clone(&restarted));
    assert_eq!(driver.observe().unwrap(), alondra_store::ObservationAcceptance::Duplicate,
        "the host's unacknowledged turn end comes again, and the store knows it already");
    assert_eq!(*acknowledged.lock().unwrap(), 1, "the replay acknowledges the event once");
    assert_eq!(Slice::read_cards(&restarted).len(), 1, "readers see the one card");
    assert_eq!(slice.stored_cards(), 1, "the replay adds no second card");
    assert!(!driver.waits_for_host(), "the turn is over");
    driver.stop().unwrap();
}

// A5: deterministic counts of the slice's work, never its time: per step, how many aggregates the
// store rebuilt from their journals, how many journal events it decoded, how many writing
// statements the daemon issued and how many read indexes publication built.
#[derive(Debug, PartialEq, Eq)]
struct Work {
    aggregate_loads: u64,
    event_decodes: u64,
    writes: u64,
    index_builds: u64,
}

fn measured<T>(domain: &DomainRuntime, step: impl FnOnce() -> T) -> (T, Work) {
    use std::sync::atomic::Ordering;
    let (store, (writes, _), builds) = (alondra_store::measure::counts(), crate::domain_store::statement_counts(), domain.index_builds.load(Ordering::SeqCst));
    let value = step();
    let done = alondra_store::measure::counts() - store;
    let (written, _) = crate::domain_store::statement_counts();
    (value, Work {
        aggregate_loads: done.aggregate_loads,
        event_decodes: done.event_decodes,
        writes: written - writes,
        index_builds: domain.index_builds.load(Ordering::SeqCst) - builds,
    })
}

#[test]
fn the_slice_counts_its_aggregate_loads_decodes_and_index_builds() {
    let slice = Slice::new();
    let (mut driver, launch) = measured(&slice.domain, || slice.launch());
    eprintln!("launch: {launch:?}");
    let mut observations = vec![];
    while driver.waits_for_host() {
        let (_, work) = measured(&slice.domain, || driver.observe().unwrap());
        eprintln!("observation: {work:?}");
        observations.push(work);
    }
    let turn_end = observations.last().unwrap();
    assert_eq!(turn_end.index_builds, 1, "the turn end publishes its card once");
    let (traces, facts): (Vec<&Work>, Vec<&Work>) = observations.iter().partition(|work| work.writes == 0);
    assert_eq!(traces.len(), 2, "the native trace of the first prompt, queued and started: {observations:?}");
    assert!(traces.iter().all(|work| work.index_builds == 0), "an observation that changes nothing for readers builds no index: {observations:?}");
    assert!(facts.iter().all(|work| work.index_builds == 1), "each observation with facts builds one index: {observations:?}");
    assert!(turn_end.aggregate_loads <= 14 && turn_end.event_decodes <= 38 && turn_end.writes <= 14,
        "the turn end's work grew past what this slice measured first: {turn_end:?}");
    let state = slice.domain.state().unwrap();
    let retitle = envelope(&state, "retitle", Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Notes for the release".into() });
    let (_, first) = measured(&slice.domain, || slice.domain.execute_with(&user(), &retitle, None, None, || Ok(())).unwrap());
    let (_, replay) = measured(&slice.domain, || slice.domain.execute_with(&user(), &retitle, None, None, || Ok(())).unwrap());
    eprintln!("command: {first:?}, exact replay: {replay:?}");
    assert_eq!(first.index_builds, 1);
    assert_eq!(replay, Work { aggregate_loads: 0, event_decodes: 0, writes: 0, index_builds: 0 }, "an exact replay rebuilds nothing");
    driver.stop().unwrap();
}

#[test]
fn stopping_the_owners_leaves_no_process_of_their_hosts() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let group = driver.worker.host_group().expect("the driver has its host");
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the host and its native Claude share the host's group: {processes:?}");
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.domain.stop_sdk_owners();
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the daemon shutdown: {left:?}");
}

/// The closer reaches an SDK node only through its owner: `claude rm` and the session listing are
/// never asked about one.
struct NoClaudeSessions;
impl crate::node_close::Sessions for NoClaudeSessions {
    fn claude<'a>(&'a self, args: Vec<String>) -> crate::node_close::Answer<'a, Result<String, String>> {
        panic!("the closer ran claude {args:?} for an SDK node")
    }
    fn leftovers<'a>(&'a self, session_id: &'a str) -> crate::node_close::Answer<'a, Option<String>> {
        panic!("the closer looked for leftovers of {session_id}, an SDK node")
    }
}

#[tokio::test]
async fn cancelling_an_sdk_task_with_background_work_stops_its_host_and_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("background");
    while !slice.domain.state().unwrap().lifecycle_journal[&TASK.into()].iter().any(|record| matches!(record, LifecycleRecord::WorkStarted { .. })) {
        driver.observe().unwrap();
    }
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    assert!(processes.len() >= 2, "the host and its native Claude share the host's group: {processes:?}");
    let agent = driver.agent().clone();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    slice.run(&user(), "cancel", Command::CancelTask { task_id: TASK.into() });
    let task = slice.domain.state().unwrap().tasks[&TaskId::from(TASK)].clone();
    assert!(task.ending(), "the cancelled task waits for the receipts of its node's end");
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    let task = &state.tasks[&TaskId::from(TASK)];
    assert_eq!((task.state, task.wrap_up.node), (TaskState::Cancelled, Some(NodeEnd::Closed)), "the cancelled task with background work ended");
    assert!(!slice.domain.owns_sdk_host(&agent));
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the cancellation: {left:?}");
}

#[tokio::test]
async fn cancelling_an_sdk_task_that_waits_on_its_card_stops_its_host_and_ends_the_task() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let group = driver.worker.host_group().unwrap();
    let processes = alondra_process_group::members(group).unwrap();
    slice.domain.sdk_owners.lock().unwrap().push(Owner::spawn(driver));
    let state = slice.domain.state().unwrap();
    let cancelled = slice.domain.execute_with(&user(), &envelope(&state, "client-cancel", Command::CancelTask { task_id: TASK.into() }), None, None, || Ok(()));
    assert!(cancelled.is_ok(), "the user's cancellation commits: {cancelled:?}");
    crate::task_closer::pass(&slice.domain, &NoClaudeSessions, &crate::task_closer::Timing::new(None), alondra_core::now_millis()).await.unwrap();
    let state = slice.domain.state().unwrap();
    assert_eq!(state.tasks[&TaskId::from(TASK)].state, TaskState::Cancelled);
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "SDK host processes survived the cancellation: {left:?}");
}

/// The owner generation currently fencing the slice's execution, read straight from SQL.
fn owner_generation(database: &Path) -> i64 {
    Connection::open(database).unwrap().query_row("SELECT owner_generation FROM provider_execution_owners", [], |row| row.get(0)).unwrap()
}

fn carry_on(domain: &DomainRuntime, note: &str) {
    let (card, _) = Slice::read_cards(domain).into_iter().find(|(_, answered)| !answered).expect("an open turn-end card");
    let state = domain.state().unwrap();
    let answer = Command::AnswerQuestion {
        interaction_id: card.as_str().into(),
        option_id: CARRY_ON.into(),
        inputs: BTreeMap::from([(ANSWER_TEXT.to_owned(), InputValue::Text(note.into()))]),
        attachments: vec![],
    };
    assert!(domain.execute_with(&user(), &envelope(&state, "client-carry-on", answer), None, None, || Ok(())).unwrap().changed);
}

// B1: the daemon crashes with the host still running. A fresh daemon on the same database adopts
// that live host with the next owner generation, and «Carry on» from a client reaches the same
// native Claude, which starts only once; its next turn ends with a card of its own.
#[test]
fn a_daemon_restart_adopts_the_live_host_and_the_same_claude_answers_the_next_turn() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    assert_eq!(slice.prompts(), ["Write the release notes"], "the launch prompt reached the host once");
    assert_eq!(owner_generation(&slice.database), 1, "the launch owned the execution as generation 1");
    let processes = alondra_process_group::members(driver.worker.host_group().unwrap()).unwrap();
    // The daemon dies: drop its driver without stopping the host, which keeps running.
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into()).unwrap();
    assert_eq!(owner_generation(&slice.database), 2, "adoption reserved the next owner generation");
    carry_on(&restarted, "Add the tests first");
    adopted.step(&|| false).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the same Claude got the launch and the carry-on, no restart: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the adopted turn ended with its own card");
    adopted.stop().unwrap();
    let left: Vec<String> =
        processes.iter().filter(|process| alondra_process_group::alive(process).unwrap()).map(ToString::to_string).collect();
    assert!(left.is_empty(), "the adopted host's processes survived its stop: {left:?}");
}

// B2: the writer crashes between the turn-end commit and its publication. A fresh daemon adopts the
// live host, which re-sends the unacknowledged event; the adopting owner sees the store already has
// it (Duplicate), acknowledges it once and publishes exactly one card.
#[test]
fn an_unacknowledged_turn_end_comes_back_to_the_adopting_owner_once() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    let database = slice.database.clone();
    slice.domain.before_owner_publish(move || {
        let cards: i64 = Connection::open(&database).unwrap()
            .query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1", [format!("{TURN_ENDED_PREFIX}{TASK}-%")], |row| row.get(0)).unwrap();
        if cards > 0 {
            panic!("the daemon stopped after the commit");
        }
    });
    let crash = loop {
        if let Err(crash) = catch_unwind(AssertUnwindSafe(|| driver.observe().unwrap())) {
            break crash;
        }
    };
    assert_eq!(crash.downcast_ref::<&str>(), Some(&"the daemon stopped after the commit"));
    assert_eq!(slice.stored_cards(), 1, "the turn end committed its card");
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let acknowledged = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&acknowledged);
    restarted.on_acknowledge(move || *counter.lock().unwrap() += 1);
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into()).unwrap();
    assert_eq!(adopted.observe().unwrap(), alondra_store::ObservationAcceptance::Duplicate, "the host re-sends the unacknowledged turn end and the store knows it");
    assert_eq!(*acknowledged.lock().unwrap(), 1, "the replay acknowledges the event once");
    assert_eq!(Slice::read_cards(&restarted).len(), 1, "readers see the one card");
    assert_eq!(slice.stored_cards(), 1, "the replay adds no second card");
    adopted.stop().unwrap();
}

// B3: a client's answer is claimed and recorded, then the daemon dies before the host receives it.
// A fresh daemon adopts and reconciles the unsent dispatch by re-sending its exact bytes: the host
// offers it to the same Claude exactly once.
#[test]
fn a_send_the_host_never_got_is_sent_once_by_the_adopting_owner() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_and_record_without_sending(), "the owner claimed and recorded the dispatch");
    drop(driver);
    assert_eq!(slice.prompts().len(), 1, "the host's Claude never got the answer");
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into()).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "reconciliation sent the answer to the same Claude exactly once: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    adopted.stop().unwrap();
}

// B4: the host offered a client's answer but the daemon crashed before recording its receipt. A
// fresh daemon adopts and reconciles it from the retained receipt: it is handed, and the host is
// not asked to offer it a second time.
#[test]
fn a_send_the_host_offered_is_not_offered_again_when_its_receipt_was_lost() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_record_and_offer_without_receipt(), "the host offered the answer");
    drop(driver);
    let restarted = Arc::new(DomainRuntime::open_at(&slice._home, &slice.database).unwrap());
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into()).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the answer reached the host once; reconciliation did not offer it again: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the reconciled turn ended with its own card");
    adopted.stop().unwrap();
}
