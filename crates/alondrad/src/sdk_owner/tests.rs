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
    plugin: PathBuf,
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
        let plugin = home.join("plugin");
        std::fs::DirBuilder::new().mode(0o700).create(&plugin).unwrap();
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
        let slice = Self { domain, database, capture, worktree, plugin, backend, _home: home, _state: state };
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
            plugin: self.plugin.clone(),
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

// A5: deterministic counts of the slice's work, never its time: per step, how many lifecycle
// transactions it ran, how many aggregates the store rebuilt from their journals, how many journal
// events it decoded, how many writing statements the daemon issued and how many read indexes
// publication built.
#[derive(Debug, PartialEq, Eq)]
struct Work {
    transactions: u64,
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
        transactions: done.transactions,
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
    assert!(launch.aggregate_loads <= launch.transactions, "the launch rebuilds its one task at most once per transaction: {launch:?}");
    assert!(observations.iter().all(|work| work.aggregate_loads <= work.transactions),
        "an observation rebuilds its one task at most once per transaction: {observations:?}");
    let journal: i64 = rusqlite::Connection::open(&slice.database).unwrap()
        .query_row("SELECT count(*) FROM lifecycle_events WHERE task_id=?1", [TASK], |row| row.get(0)).unwrap();
    assert!(turn_end.event_decodes <= (turn_end.transactions + turn_end.index_builds) * journal as u64,
        "the turn end decodes the task's {journal} journal events at most once per transaction and once per publication: {turn_end:?}");
    assert!(turn_end.writes <= 14, "the turn end's writes grew past what this slice measured first: {turn_end:?}");
    let state = slice.domain.state().unwrap();
    let retitle = envelope(&state, "retitle", Command::TitleRequest { request_id: "sdk-slice-request".into(), title: "Notes for the release".into() });
    let (_, first) = measured(&slice.domain, || slice.domain.execute_with(&user(), &retitle, None, None, || Ok(())).unwrap());
    let (_, replay) = measured(&slice.domain, || slice.domain.execute_with(&user(), &retitle, None, None, || Ok(())).unwrap());
    eprintln!("command: {first:?}, exact replay: {replay:?}");
    assert_eq!(first.index_builds, 1);
    assert_eq!((replay.aggregate_loads, replay.event_decodes, replay.writes, replay.index_builds), (0, 0, 0, 0), "an exact replay rebuilds nothing: {replay:?}");
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
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
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
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
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
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
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
    let mut adopted = adopt(Arc::clone(&restarted), &slice.backend, TASK.into(), Some(&slice.plugin)).unwrap();
    slice.settle(&mut adopted);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "the answer reached the host once; reconciliation did not offer it again: {prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the reconciled turn ended with its own card");
    adopted.stop().unwrap();
}

// 19 Oct: Claude's sign-in expired and nothing told the user. Through the SDK, the native
// Claude's 401 ends the turn with `authentication_failed`: the task waits on its card asking the
// user to sign in, committed and published before the host is acknowledged. Once the user signs
// in and says so, the task goes on and Claude's answer leaves no card open.
#[test]
fn a_signed_out_turn_waits_on_the_sign_in_card_until_claude_answers_again() {
    let slice = Slice::new();
    let acknowledged = watch_acknowledgements(&slice);
    let mut driver = slice.launch_with("Write the release notes signed-out");
    slice.settle(&mut driver);
    assert_eq!(Slice::read_cards(&slice.domain), [], "no card that tells the task to go on");
    let held: Vec<(String, ClaudeHold)> = slice.domain.read().state().claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect();
    assert_eq!(held, [(TASK.to_owned(), ClaudeHold::SignedOut)]);
    let listed = |domain: &DomainRuntime| -> Vec<InteractionId> {
        domain.read().index().waiting().iter().map(|item| { let InboxSubject::Interaction { interaction_id } = &item.subject; interaction_id.clone() })
            .filter(|id| id.0.starts_with(SIGNED_OUT_PREFIX)).collect()
    };
    let card = listed(&slice.domain).pop().expect("Waiting for You lists the sign-in card");
    let stored: i64 = slice.observer().query_row("SELECT count(*) FROM interactions WHERE id LIKE ?1 AND task_id=?2", [format!("{SIGNED_OUT_PREFIX}%"), TASK.to_owned()], |row| row.get(0)).unwrap();
    assert_eq!(stored, 1, "its card, committed");
    assert!(!acknowledged.lock().unwrap().is_empty(), "the host's observations were acknowledged after their commit");

    std::fs::write(format!("{}.recovered", slice.capture.display()), "").unwrap();
    let state = slice.domain.state().unwrap();
    let signed_in = Command::AnswerQuestion { interaction_id: card, option_id: SIGNED_IN.into(), inputs: BTreeMap::new(), attachments: vec![] };
    slice.domain.execute_with(&user(), &envelope(&state, "client-signed-in", signed_in), None, None, || Ok(())).unwrap();
    assert_eq!(driver.pending_permits(), 1, "the probe's dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Claude answers again"), "{}", prompts[1]);
    let read = slice.domain.read();
    assert!(read.state().claude_held().is_empty(), "Claude answered: nothing waits for it");
    assert_eq!(listed(&slice.domain), [], "no sign-in card is listed");
    driver.stop().unwrap();
}

// The API kept failing past Claude Code's retries: the task waits with no announcement, and the
// daemon's scheduled probe, run as the daemon runs it, sends exactly that task on once Claude
// answers again.
#[test]
fn an_overloaded_api_holds_the_task_until_the_scheduled_probe_finds_claude_answering() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("Write the release notes overloaded");
    slice.settle(&mut driver);
    let held: Vec<(String, ClaudeHold)> = slice.domain.read().state().claude_held().into_iter().map(|held| (held.task.0, held.hold)).collect();
    assert_eq!(held, [(TASK.to_owned(), ClaudeHold::Outage)]);
    assert_eq!(Slice::read_cards(&slice.domain), [], "no card that tells the task to go on");
    std::fs::write(format!("{}.recovered", slice.capture.display()), "").unwrap();
    crate::claude_access::probe(&slice.domain, ClaudeHold::Outage);
    assert_eq!(driver.pending_permits(), 1, "the probe's dispatch reached the owner");
    driver.step(&|| false).unwrap();
    slice.settle(&mut driver);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].starts_with("Claude answers again"), "{}", prompts[1]);
    assert!(slice.domain.read().state().claude_held().is_empty(), "Claude answered: nothing waits for it");
    driver.stop().unwrap();
}

// Each launch made a provider binding of its own while the store keeps one active binding per
// host: every task after the first was refused with BindingRevoked.
#[test]
fn a_second_task_starts_through_the_same_sdk_host() {
    let slice = Slice::new();
    slice.run(&Actor::Orchestrator { generation: 0 }, "second-task", Command::CreateTask {
        plan_items: None,
        task_id: "sdk-slice-second".into(),
        request_id: "sdk-slice-request".into(),
        title: "Write the upgrade guide".into(),
        agent_id: None,
        review_prs: false,
        dependencies: BTreeSet::new(),
        stack: None,
        launch: Some(LaunchPlan::new("node", slice.worktree.to_str().unwrap())),
    });
    let mut first = slice.launch();
    slice.settle(&mut first);
    let mut second = start(Arc::clone(&slice.domain), &slice.backend, Launch {
        task: "sdk-slice-second".into(),
        name: "Upgrade guide".into(),
        directory: slice.worktree.to_str().unwrap().into(),
        prompt: "Write the upgrade guide".into(),
        plugin: slice.plugin.clone(),
    })
    .expect("the second task starts too");
    slice.settle(&mut second);
    assert_eq!(slice.prompts(), ["Write the release notes", "Write the upgrade guide"]);
    first.stop().unwrap();
    second.stop().unwrap();
}

// The task's native Claude loads every settings source of the user's default profile, with no
// strict MCP that would hide the user's servers, and Alondra's node plugin, launch settings and a
// capability issued to its node on top: its Alondra MCP server reaches the daemon as that node.
#[test]
fn the_tasks_claude_loads_the_users_profile_and_alondras_plugin_settings_and_capability() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&slice.capture).unwrap()).unwrap();
    let args: Vec<&str> = capture["args"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    let after = |flag: &str| args.iter().position(|arg| *arg == flag).and_then(|at| args.get(at + 1)).copied()
        .or_else(|| args.iter().find_map(|arg| arg.strip_prefix(&format!("{flag}="))));
    assert_eq!(after("--setting-sources"), Some("user,project,local"), "{args:?}");
    assert!(!args.contains(&"--strict-mcp-config"), "the user's MCP servers load: {args:?}");
    assert_eq!(after("--plugin-dir"), slice.plugin.to_str(), "{args:?}");
    let settings: serde_json::Value = serde_json::from_str(after("--settings").expect("Alondra's launch settings")).unwrap();
    assert_eq!(settings, crate::plugins::launch_settings_with(None));
    let environment: Vec<&str> = capture["env_keys"].as_array().unwrap().iter().filter_map(serde_json::Value::as_str).collect();
    for alondra in ["ALONDRA_DOMAIN_CAPABILITY", "ALONDRA_DOMAIN_PROTOCOL", "ALONDRA_SESSION_ID", "ALONDRA_HOME", "ALONDRA_SDK_HOST"] {
        assert!(environment.contains(&alondra), "{alondra} reaches the node plugin's MCP server: {environment:?}");
    }
    for profile in ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"] {
        assert!(!environment.contains(&profile), "the user's default profile: {environment:?}");
    }
    let agent = driver.agent().0.clone();
    let session = capture["env_value_sha256"]["ALONDRA_SESSION_ID"].as_str().unwrap();
    use sha2::Digest;
    let expected: String = sha2::Sha256::digest(agent.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(session, expected, "the node the capability was issued to");
    driver.stop().unwrap();
}

/// The Mac goes down: every process of the host's group is killed, its native Claude included, and
/// the daemon with them.
fn kill_host_and_daemon(driver: Driver) {
    let group = driver.worker.host_group().unwrap();
    let mut exits = alondra_process_group::Exits::new().unwrap();
    for member in alondra_process_group::members(group).unwrap() {
        exits.watch(&member).unwrap();
    }
    assert!(std::process::Command::new("/bin/kill").args(["-KILL", "--", &format!("-{group}")]).status().unwrap().success());
    exits.wait().unwrap();
    drop(driver);
}

impl Slice {
    /// Each native Claude the hosts started: its session, the session it resumed and how many
    /// prompts that session's transcript already held.
    fn starts(&self) -> Vec<serde_json::Value> {
        let path = PathBuf::from(format!("{}.starts", self.capture.display()));
        std::fs::read_to_string(path).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    fn restart(&self) -> Arc<DomainRuntime> {
        Arc::new(DomainRuntime::open_at(&self._home, &self.database).unwrap())
    }

    fn take_over(&self, domain: &Arc<DomainRuntime>) -> Driver {
        adopt(Arc::clone(domain), &self.backend, TASK.into(), Some(&self.plugin)).unwrap()
    }

    /// Reads the host until its turn is running.
    fn until_working(&self, driver: &mut Driver) {
        while lifecycle(&self.domain).activity() != ActivityView::Working {
            driver.observe().unwrap();
        }
    }
}

fn lifecycle(domain: &DomainRuntime) -> alondra_domain::TaskLifecycle {
    let read = domain.read();
    replay(&TASK.into(), &read.state().lifecycle_journal[&TaskId::from(TASK)]).unwrap()
}

/// The open card of the task whose id starts with `prefix`.
fn open_card(domain: &DomainRuntime, prefix: &str) -> Option<Interaction> {
    domain.read().state().interactions.values()
        .find(|card| card.id.0.starts_with(prefix) && !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: None, .. }))
        .cloned()
}

fn answer(domain: &DomainRuntime, card: &InteractionId, option: &str) {
    let state = domain.state().unwrap();
    let answer = Command::AnswerQuestion { interaction_id: card.clone(), option_id: option.into(), inputs: BTreeMap::new(), attachments: vec![] };
    assert!(domain.execute_with(&user(), &envelope(&state, &format!("client-{option}"), answer), None, None, || Ok(())).unwrap().changed);
}

// R1: the Mac shuts down after the first turn's card: host, Claude and daemon are gone. The daemon
// that comes back finds no process holding the host's lock, launches a fresh host for the same
// execution with the next owner generation, and «Carry on» resumes the same native session with its
// transcript: one more Claude starts, with --resume of the stored session.
#[test]
fn a_dead_host_is_replaced_by_one_that_resumes_the_same_claude_session() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert_eq!(owner_generation(&slice.database), 2, "the replacement owns the execution with the next generation");
    assert_eq!(slice.starts().len(), 1, "no Claude starts before the user answers");
    let node = restarted.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap();
    assert!(!crate::node_revival::needed(&restarted, &node.0), "no claude respawn, and no card about it, for a node of the SDK");
    carry_on(&restarted, "Add the tests first");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let starts = slice.starts();
    assert_eq!(starts.len(), 2, "{starts:?}");
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "the same session is resumed: {starts:?}");
    assert_eq!(starts[1]["history"], 1, "with the transcript of the first turn");
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "the resumed turn ended with its own card");
    resumed.stop().unwrap();
}

// R2: the Mac shuts down mid-turn. The ended host's turn ended with it: the task asks how to go
// on instead of staying «working» forever, and the answer goes to the resumed session.
#[test]
fn a_turn_cut_by_the_end_of_its_host_asks_how_to_go_on_and_resumes_the_session() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let session = slice.starts()[0]["session"].as_str().unwrap().to_owned();
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert_eq!(lifecycle(&restarted).activity(), ActivityView::Resting, "nothing of the ended host runs");
    let card = open_card(&restarted, SESSION_ENDED_PREFIX).expect("the interrupted task asks the user");
    answer(&restarted, &card.id, CARRY_ON);
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let starts = slice.starts();
    assert_eq!(starts[1]["resume"].as_str(), Some(session.as_str()), "{starts:?}");
    assert!(slice.prompts()[1].starts_with("Go on with your task"), "{:?}", slice.prompts());
    resumed.stop().unwrap();
}

// R3: a normal shutdown stops the host (5d) mid-turn: what the stopping host wrote after the last
// acknowledgement (its Claude's exit, the end of its SDK iterator) is committed as evidence, and
// the task asks how to go on.
#[test]
fn a_host_stopped_mid_turn_by_a_normal_shutdown_leaves_evidence_the_next_daemon_commits() {
    let slice = Slice::new();
    let mut driver = slice.launch_with("consume-and-hold");
    slice.until_working(&mut driver);
    let mut owner = Owner::spawn(driver);
    owner.stop().unwrap();
    drop(owner);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert!(open_card(&restarted, SESSION_ENDED_PREFIX).is_some(), "the interrupted task asks the user");
    resumed.stop().unwrap();
}

/// The first turn ended, the user answered `note` and the host offered it to a Claude whose
/// session then ended before it showed it read it; the Mac went down before the receipt was kept.
fn doubtful(slice: &Slice) -> (Arc<DomainRuntime>, Driver, Interaction) {
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first unread");
    assert!(driver.claim_record_and_offer_without_receipt(), "the host offered the answer");
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    let card = open_card(&restarted, HANDOFF_DOUBT_PREFIX).expect("the doubtful answer asks the user");
    assert!(!resumed.waits_for_host(), "the replacement host runs no Claude yet: its owner waits for what the user decides, not on the host");
    (restarted, resumed, card)
}

// R4a: the doubtful answer is shown whole and nothing is sent until the user decides; «Send it
// again» gives the resumed session the same text once.
#[test]
fn a_doubtful_answer_is_sent_again_to_the_resumed_session_only_on_the_users_word() {
    let slice = Slice::new();
    let (restarted, mut resumed, card) = doubtful(&slice);
    assert!(card.card.content.as_ref().is_some_and(|content| content.body.ends_with("Add the tests first unread")), "{:?}", card.card.content);
    assert_eq!(resumed.pending_permits(), 0, "nothing is sent while the user decides");
    assert_eq!(slice.prompts().len(), 2, "the ended session got it once");
    answer(&restarted, &card.id, RESEND);
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 3, "{prompts:?}");
    assert_eq!(prompts[2], prompts[1], "the same words, once more");
    assert!(slice.starts()[1]["resume"].is_string(), "to the resumed session");
    assert_eq!(Slice::read_cards(&restarted).len(), 2, "its turn ended with its own card");
    resumed.stop().unwrap();
}

// R4b: «Take it as received» sends nothing and records no consumption; the task asks how to go on.
#[test]
fn a_doubtful_answer_taken_as_received_is_not_sent_again() {
    let slice = Slice::new();
    let (restarted, mut resumed, card) = doubtful(&slice);
    answer(&restarted, &card.id, TAKE_AS_RECEIVED);
    assert_eq!(resumed.pending_permits(), 0, "nothing is sent");
    assert_eq!(slice.prompts().len(), 2);
    assert!(lifecycle(&restarted).pending_inputs().is_empty(), "the answer left the queue by the user's word");
    assert!(open_card(&restarted, SESSION_ENDED_PREFIX).is_some(), "the task asks how to go on");
    resumed.stop().unwrap();
}

// R5: the answer was claimed and recorded, and the Mac went down before the host got it. The ended
// host's journal shows it never took the command, so it goes once to the resumed session, unasked.
#[test]
fn an_answer_the_ended_host_never_took_goes_to_the_resumed_session_unasked() {
    let slice = Slice::new();
    let mut driver = slice.launch();
    slice.settle(&mut driver);
    carry_on(&slice.domain, "Add the tests first");
    assert!(driver.claim_and_record_without_sending(), "the owner claimed and recorded the dispatch");
    kill_host_and_daemon(driver);
    let restarted = slice.restart();
    let mut resumed = slice.take_over(&restarted);
    assert!(open_card(&restarted, HANDOFF_DOUBT_PREFIX).is_none(), "nothing to ask: the journal proves it never left");
    resumed.step(&|| false).unwrap();
    slice.settle(&mut resumed);
    let prompts = slice.prompts();
    assert_eq!(prompts.len(), 2, "{prompts:?}");
    assert!(prompts[1].ends_with("Add the tests first"), "{}", prompts[1]);
    assert!(slice.starts()[1]["resume"].is_string());
    resumed.stop().unwrap();
}
