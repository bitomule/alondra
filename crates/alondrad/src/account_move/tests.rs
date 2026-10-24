//! A task's move to another account through the real owner: the daemon's store and commands, the
//! pinned Node host with the official SDK, and the fake native Claude, which records every start
//! with the session it resumed, its folder and the account folder it ran with, and refuses a turn
//! with Claude's usage limit when told. Every wait is on a fact the domain publishes, never on a
//! clock.
use super::*;
use crate::domain_store::StoreError;
use crate::test_support::Directory;
use alondra_core::accounts::{AccountState, AccountStatus};
use alondra_core::claude_account;
use std::collections::BTreeSet;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

const TASK: &str = "t-release-notes";
const WORK: &str = "work";

fn work() -> AccountId {
    AccountId::parse(WORK).unwrap()
}

struct World {
    domain: Arc<DomainRuntime>,
    backend: crate::sdk_owner::SdkBackend,
    capture: PathBuf,
    worktree: PathBuf,
    database: PathBuf,
    home: Directory,
    _state: crate::sdk_owner::tests::HostRoot,
}

fn envelope(state: &DomainState, id: &str, command: Command) -> CommandEnvelope {
    CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(), expected_revision: state.revision, command }
}

fn run(domain: &DomainRuntime, actor: &Actor, id: &str, command: Command) -> Result<CommandOutcome, StoreError> {
    let state = domain.state().unwrap();
    domain.execute_with(actor, &envelope(&state, id, command), None, None, || Ok(()))
}

impl World {
    /// The user's own account and a work account, both signed in, moving on; a task launched on
    /// the work account whose first turn Claude refuses with its usage limit.
    async fn new() -> Self {
        let world = Self::with_accounts(true);
        world.launch_limited().await;
        world
    }

    fn with_accounts(moving: bool) -> Self {
        let home = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&home).unwrap();
        let state = crate::sdk_owner::tests::HostRoot::new();
        let worktree = home.join("worktree");
        std::fs::DirBuilder::new().mode(0o700).create(&worktree).unwrap();
        std::fs::DirBuilder::new().mode(0o700).create(home.join(crate::plugins::NODE)).unwrap();
        std::fs::create_dir_all(home.join(".claude/projects")).unwrap();
        claude_account::prepare(&home, &work(), &home.join(".claude")).unwrap();
        let (backend, capture) = crate::sdk_owner::tests::fixture_backend(&home, &state);
        let database = home.join("domain.sqlite");
        let domain = Self::boot(&home, &database, &backend);
        let world = Self { domain, backend, capture, worktree, database, home, _state: state };
        world.set_moving(moving);
        world.accounts_seen("seen", AccountState::SignedIn, AccountState::SignedIn);
        let orchestrator = Actor::Orchestrator { generation: 0 };
        run(&world.domain, &orchestrator, "request", Command::CreateRequest { request_id: "r".into(), body: "Write the release notes".into(), auto_merge: false, maintenance: false, message: false }).unwrap();
        run(&world.domain, &orchestrator, "title", Command::TitleRequest { request_id: "r".into(), title: "Release notes".into() }).unwrap();
        run(&world.domain, &orchestrator, "task", Command::CreateTask {
            plan_items: None, task_id: TASK.into(), request_id: "r".into(), title: "Write the release notes".into(), agent_id: None, review_prs: false,
            dependencies: BTreeSet::new(), stack: None, launch: Some(LaunchPlan::new("node", world.worktree.to_str().unwrap())),
        }).unwrap();
        world
    }

    fn boot(home: &Path, database: &Path, backend: &crate::sdk_owner::SdkBackend) -> Arc<DomainRuntime> {
        let domain = Arc::new(DomainRuntime::open_at(home, database).unwrap());
        domain.enable_sdk(backend.clone());
        domain
    }

    fn set_moving(&self, on: bool) {
        let text = format!("[accounts.distribution]\nmove_when_exhausted = {on}\n\n[[accounts.claude]]\nid = \"{WORK}\"\nname = \"Work\"\n");
        std::fs::write(alondra_core::options::path_in(&self.home), text).unwrap();
    }

    fn accounts_seen(&self, id: &str, default: AccountState, work: AccountState) {
        let accounts = [(AccountId::DEFAULT, default), (WORK, work)].map(|(id, state)| AccountStatus {
            provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.into(), state, email: None, plan: None, billing: Default::default(), sign_ins: 0,
        }).to_vec();
        run(&self.domain, &Actor::System, id, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() }).unwrap();
    }

    /// The task starts on the work account and Claude refuses its turn with the usage limit: it
    /// waits on its limit card.
    async fn launch_limited(&self) {
        let launch = crate::sdk_owner::Launch {
            task: TASK.into(), name: "release-notes".into(), directory: self.worktree.to_str().unwrap().into(),
            prompt: "Write the release notes rate-limited".into(), plugin: self.home.join(crate::plugins::NODE),
            options: crate::sdk_owner::LaunchOptions::default(), model: None, account: work(),
        };
        crate::sdk_owner::launch(&self.domain, launch).await.unwrap();
        self.until(|domain| domain.read().state().held_by_limit(&TASK.into()).is_some()).await;
        assert_eq!(self.domain.read().state().task_account(&TASK.into()), work());
    }

    async fn until(&self, done: impl Fn(&DomainRuntime) -> bool) {
        let mut changed = self.domain.subscribe();
        while !done(&self.domain) {
            changed.changed().await.unwrap();
        }
    }

    /// A daemon that died: what it ran is gone, a new one opens the same database and, as at
    /// startup, takes over the SDK executions before it goes on with the moves.
    async fn restart(&mut self) {
        self.domain.stop_sdk_owners();
        self.domain = Self::boot(&self.home, &self.database, &self.backend);
        let (domain, backend) = (Arc::clone(&self.domain), self.backend.clone());
        tokio::task::spawn_blocking(move || crate::sdk_owner::adopt_all(&domain, &backend)).await.unwrap();
    }

    fn journal(&self) -> Vec<AccountMove> {
        moves(&self.domain)
    }

    fn step(&self) -> Option<AccountMoveStep> {
        latest(&self.domain, &TASK.into()).map(|moving| moving.step)
    }

    fn starts(&self) -> Vec<serde_json::Value> {
        std::fs::read_to_string(format!("{}.starts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
    }

    /// The prompts Claude got, besides the wake Alondra sends after a turn that ended empty.
    fn prompts(&self) -> Vec<String> {
        crate::sdk_owner::tests::not_wakes(std::fs::read_to_string(format!("{}.prompts", self.capture.display())).unwrap_or_default().lines().map(|line| serde_json::from_str(line).unwrap()))
    }

    fn node(&self) -> AgentId {
        self.domain.state().unwrap().tasks[&TaskId::from(TASK)].agent_id.clone().unwrap()
    }

    /// Waits until the task's execution of `generation` ended a turn with Claude's answer.
    async fn until_answered(&self, generation: u64) {
        let prefix = format!("{TURN_ENDED_PREFIX}{TASK}-{generation}-");
        self.until(|domain| domain.read().state().interactions.keys().any(|card| card.0.starts_with(&prefix))).await;
    }
}

impl Drop for World {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

/// What a finished move leaves: the work account's session resumed once with the user's own
/// account, in the task's folder, told to go on; the limit card withdrawn and the old node closed.
async fn assert_moved(world: &World, old: &AgentId) {
    assert_eq!(world.step(), Some(AccountMoveStep::Done), "{:?}", world.journal());
    world.until_answered(2).await;
    let starts = world.starts();
    assert_eq!(starts.len(), 2, "Claude starts once on each account: {starts:?}");
    let folder = alondra_core::accounts::folder(world.home.as_ref(), &work()).unwrap();
    assert_eq!(starts[0]["config"].as_str().map(PathBuf::from), Some(folder), "the first turn ran on the work account");
    assert_eq!(starts[1]["config"], serde_json::Value::Null, "the move runs on the user's own account");
    assert_eq!(starts[1]["resume"], starts[0]["session"], "with --resume of the same session: {starts:?}");
    assert!(starts[1]["history"].as_u64().unwrap() > 0, "its transcript is the one the work account left");
    assert_eq!(starts[1]["cwd"].as_str().map(PathBuf::from), Some(world.worktree.canonicalize().unwrap()), "in the task's folder");
    assert_eq!(world.prompts(), ["Write the release notes rate-limited", MOVED_ON], "the new execution is told to go on, once");
    let state = world.domain.state().unwrap();
    assert!(state.claude_held().is_empty(), "nothing holds the task");
    assert_eq!(state.task_account(&TASK.into()), AccountId::default_account(), "the task runs on the user's own account");
    let card = world.journal().last().unwrap().plan.card.clone();
    assert!(state.interactions[&card].withdrawn, "its limit card is withdrawn");
    let agent = world.node();
    assert_ne!(agent, *old);
    assert!(world.domain.owns_sdk_host(&agent), "the new node's host runs");
    assert!(!world.domain.owns_sdk_host(old), "the old one's does not");
    assert_eq!(state.node_phase(old), Some(&NodePhase::Closed), "the node that hit the limit is closed");
}

// A task whose account reached its usage limit goes on with the other account between turns: its
// host stops, a new execution on the other account resumes the same Claude session, and the
// journal records each step.
#[tokio::test]
async fn a_task_whose_account_ran_out_goes_on_with_another_account_in_the_same_session() {
    let world = World::new().await;
    let old = world.node();
    tick(&world.domain).await;
    assert_moved(&world, &old).await;
    let journal = world.journal();
    assert_eq!(journal.len(), 1);
    assert_eq!((journal[0].plan.from.clone(), journal[0].plan.to.clone()), (work(), AccountId::default_account()));
    assert_eq!(journal[0].plan.session, world.starts()[0]["session"].as_str().unwrap());
}

// A daemon that dies right after any step of a move comes back and goes on from the step it
// recorded: Claude starts once on each account, the same session goes on, and it is told to go on
// once.
#[tokio::test]
async fn a_crash_after_each_step_of_the_move_goes_on_from_that_step_and_repeats_nothing() {
    for step in [AccountMoveStep::Planned, AccountMoveStep::HostStopped, AccountMoveStep::Admitted] {
        let mut world = World::new().await;
        let old = world.node();
        *world.domain.moves.crash_after.lock().unwrap() = Some(step);
        tick(&world.domain).await;
        assert_eq!(world.step(), Some(step), "the move stopped at {step:?}");
        assert_eq!(world.starts().len(), 1, "no Claude ran on the other account before the crash at {step:?}");
        world.restart().await;
        tick(&world.domain).await;
        assert_moved(&world, &old).await;
    }
}

// A daemon that comes back to a move that can no longer happen undoes it before its admission:
// with the setting turned off meanwhile, the task stays on its account's limit with its execution
// taken over again, and no Claude ran on the other account.
#[tokio::test]
async fn a_move_that_can_no_longer_happen_is_undone_and_the_task_waits_on_its_accounts_limit() {
    for step in [AccountMoveStep::Planned, AccountMoveStep::HostStopped] {
        let mut world = World::new().await;
        let old = world.node();
        *world.domain.moves.crash_after.lock().unwrap() = Some(step);
        tick(&world.domain).await;
        world.set_moving(false);
        world.restart().await;
        assert!(!world.domain.owns_sdk_host(&old), "nothing takes the execution over while the move holds it");
        tick(&world.domain).await;
        world.domain.join_retakes();
        assert_eq!(world.step(), Some(AccountMoveStep::Undone), "after {step:?}");
        assert!(world.journal()[0].undone.as_deref().is_some_and(|why| why.contains("turned off")));
        let state = world.domain.state().unwrap();
        assert_eq!(state.claude_held().into_iter().map(|held| (held.task.0, held.account)).collect::<Vec<_>>(), [(TASK.to_owned(), work())],
            "it waits on the work account's limit");
        assert_eq!(world.node(), old, "on its own node");
        assert!(world.domain.owns_sdk_host(&old), "whose execution is taken over again");
        assert_eq!(world.starts().len(), 1, "no Claude ran on the other account");
    }
}

// With the setting off, a task whose account ran out waits on its limit as it always did.
#[tokio::test]
async fn with_the_setting_off_a_task_waits_on_its_accounts_limit() {
    let world = World::with_accounts(false);
    world.launch_limited().await;
    tick(&world.domain).await;
    assert!(world.journal().is_empty(), "nothing moves");
    assert_eq!(world.domain.state().unwrap().claude_held().len(), 1);
}

// Only a real limit keeps an account from taking the task: when the other account is signed out
// too, every account is at its limit and the task waits on its own.
#[tokio::test]
async fn when_every_account_is_at_its_limit_the_task_waits() {
    let world = World::new().await;
    world.accounts_seen("signed-out", AccountState::SignedOut, AccountState::SignedIn);
    tick(&world.domain).await;
    assert!(world.journal().is_empty(), "no account can take it");
    assert_eq!(world.domain.state().unwrap().claude_held().len(), 1, "it waits on its limit");
    world.accounts_seen("signed-in", AccountState::SignedIn, AccountState::SignedIn);
    tick(&world.domain).await;
    assert_eq!(world.step(), Some(AccountMoveStep::Done), "an account that can take it again takes it");
}

// A task moves once per limit it hits: on the account it moved to, a limit before the one it left
// has reset leaves it waiting there, even with the account it left free again.
#[tokio::test]
async fn a_task_moves_once_until_the_limit_it_left_resets() {
    let world = World::new().await;
    tick(&world.domain).await;
    assert_eq!(world.step(), Some(AccountMoveStep::Done), "{:?}", world.journal());
    world.until_answered(2).await;
    let reading = alondra_core::usage::UsageReading { windows: vec![], plan: None, complete: true };
    let now = alondra_core::now_millis();
    run(&world.domain, &Actor::System, "work-reset", Command::RecordAccountUsage {
        provider: Provider::Claude, account: work(), reading, at: now, day: crate::node_reports::local_day(now),
    }).unwrap();
    assert!(can_take(&world.domain.state().unwrap(), &work(), now), "the work account can take work again");
    run(&world.domain, &Actor::System, "limited-again", Command::TellNode { task_id: TASK.into(), text: "Go on rate-limited".into() }).unwrap();
    world.until(|domain| domain.read().state().held_by_limit(&TASK.into()).is_some()).await;
    tick(&world.domain).await;
    assert_eq!(world.journal().len(), 1, "no second move before the work account's limit resets");
    assert_eq!(world.domain.state().unwrap().task_account(&TASK.into()), AccountId::default_account(), "it waits on the account it moved to");
}
