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
    domain.execute_fixture(actor, envelope(&state, id, command))
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
            plan_items: None, continues: None, task_id: TASK.into(), request_id: "r".into(), title: "Write the release notes".into(), agent_id: None, review_prs: false,
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
            provider: Provider::Claude, id: AccountId::parse(id).unwrap(), name: id.into(), state, native_identity: None, email: None, plan: None, billing: Default::default(), sign_ins: 0,
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

    /// Waits until the task's execution of `generation` ended a turn with Claude's answer and
    /// rests on the card that turn ended on.
    async fn until_answered(&self, generation: u64) {
        self.until(|domain| domain.read().state().interactions.keys().any(|card| rested(card, generation))).await;
    }
}

/// Whether `card` is one a turn of the task's execution of `generation` ended on.
fn rested(card: &InteractionId, generation: u64) -> bool {
    [STALLED_PREFIX, TURN_ENDED_PREFIX].iter().any(|prefix| card.0.starts_with(&format!("{prefix}{TASK}-{generation}-")))
}

impl Drop for World {
    fn drop(&mut self) {
        self.domain.stop_sdk_owners();
    }
}

/// A quota hold keeps its original physical native process, account and pending mission.
async fn assert_stayed(world: &World, old: &AgentId) {
    assert_eq!(world.domain.state().unwrap().task_account(&TASK.into()),work());
    assert_eq!(world.node(),*old);
    assert_eq!(world.starts().len(),1,"no replacement native process");
    assert_eq!(world.prompts(),["Write the release notes rate-limited"]);
    assert!(world.domain.state().unwrap().held_by_limit(&TASK.into()).is_some());
    assert!(world.domain.owns_sdk_host(old));
}

// The old distribution flag cannot authorize changing an already-started legacy node.
#[tokio::test]
async fn an_already_started_legacy_node_keeps_its_account_when_the_old_move_flag_is_on() {
    let world=World::new().await;let old=world.node();
    assert!(world.domain.state().unwrap().tasks[&TaskId::from(TASK)].selection.is_none());
    tick(&world.domain).await;
    assert_stayed(&world,&old).await;
    assert!(world.journal().is_empty());
    let state=world.domain.state().unwrap();let held=state.held_by_limit(&TASK.into()).unwrap();
    let life=state.lifecycle_of(&TASK.into()).unwrap();let stamp=life.execution().unwrap();
    let forged=AccountMovePlan{from:work(),to:AccountId::default_account(),card:held.card,node:old,instance:stamp.instance().as_str().into(),session:world.starts()[0]["session"].as_str().unwrap().into(),cwd:world.worktree.to_str().unwrap().into(),until:i64::MAX};
    assert!(world.domain.store.lock().unwrap().plan_account_move(&TASK.into(),&forged).unwrap_err().to_string().contains("creating a node"));
}

// Old pre-admission journals remain readable and are safely cancelled after restart.
#[tokio::test]
async fn a_crash_after_each_pre_admission_step_undoes_the_old_move_without_changing_accounts() {
    for step in [AccountMoveStep::Planned,AccountMoveStep::HostStopped]{
        let mut world=World::new().await;let old=world.node();
        historical(&world,step,old.clone());
        world.restart().await;tick(&world.domain).await;world.domain.join_retakes();
        assert_eq!(world.step(),Some(AccountMoveStep::Undone));
        assert_stayed(&world,&old).await;
    }
}

// A daemon that comes back to a move that can no longer happen undoes it before its admission:
// with the setting turned off meanwhile, the task stays on its account's limit with its execution
// taken over again, and no Claude ran on the other account.
#[tokio::test]
async fn a_move_that_can_no_longer_happen_is_undone_and_the_task_waits_on_its_accounts_limit() {
    for step in [AccountMoveStep::Planned,AccountMoveStep::HostStopped]{
        let mut world=World::new().await;let old=world.node();
        historical(&world,step,old.clone());world.set_moving(false);world.restart().await;
        assert!(!world.domain.owns_sdk_host(&old),"pending historical journal fences startup until cancellation");
        tick(&world.domain).await;world.domain.join_retakes();
        assert_eq!(world.step(),Some(AccountMoveStep::Undone));
        assert!(world.journal()[0].undone.as_deref().unwrap().contains("creating a node"));
        assert_stayed(&world,&old).await;
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
    assert!(world.journal().is_empty(), "available alternatives do not change an already started node");
    assert_eq!(world.domain.state().unwrap().task_account(&TASK.into()),work());
}

// Capacity observations keep initial account choice separate from existing-session recovery.
#[tokio::test]
async fn fresh_capacity_does_not_change_the_account_of_an_existing_node() {
    let world=World::new().await;let old=world.node();tick(&world.domain).await;
    let reading=alondra_core::usage::UsageReading{windows:vec![],plan:None,complete:true};let now=alondra_core::now_millis();
    run(&world.domain,&Actor::System,"work-reset",Command::RecordAccountUsage{provider:Provider::Claude,account:work(),reading,at:now,day:crate::node_reports::local_day(now)}).unwrap();
    assert!(can_take(&world.domain.state().unwrap(),&work(),now));
    tick(&world.domain).await;
    assert!(world.journal().is_empty());assert_stayed(&world,&old).await;
}

#[tokio::test]
async fn a_typed_claude_node_never_enters_legacy_account_migration_even_when_enabled(){
    let world=World::new().await;let original=world.node();
    run(&world.domain,&Actor::System,"bind-modern-native-selection",Command::RecordTaskSelection{task_id:TASK.into(),selection:alondra_core::model_selection::TaskSelection::automatic(alondra_core::model_selection::ModelProfile::Implementation),pending:None}).unwrap();
    tick(&world.domain).await;
    assert!(world.journal().is_empty());assert_eq!(world.starts().len(),1);assert_eq!(world.node(),original);assert!(world.domain.owns_sdk_host(&original),"a held modern session is not stopped to migrate its cache");
    assert_eq!(world.domain.state().unwrap().tasks[&TaskId::from(TASK)].account_key(),alondra_core::accounts::AccountKey::new(Provider::Claude,work()));
    let state=world.domain.state().unwrap();let held=state.held_by_limit(&TASK.into()).unwrap();let stamp=state.lifecycle_of(&TASK.into()).unwrap().execution().unwrap().clone();
    let forged=AccountMovePlan{from:work(),to:AccountId::default_account(),card:held.card,node:original,instance:stamp.instance().as_str().into(),session:world.starts()[0]["session"].as_str().unwrap().into(),cwd:world.worktree.to_str().unwrap().into(),until:i64::MAX};
    let error=world.domain.store.lock().unwrap().plan_account_move(&TASK.into(),&forged).unwrap_err();assert!(error.to_string().contains("creating a node"));assert!(world.journal().is_empty(),"bypassing the planner still cannot create a durable transfer");
}


// Simulates rows written by an old release. Only the historical journal is seeded; native
// execution, account binding and lifecycle facts are created by the real owner.
fn historical(world:&World,step:AccountMoveStep,source_node:AgentId)->i64{
    let state=world.domain.state().unwrap();let life=state.lifecycle_of(&TASK.into()).unwrap();let stamp=life.execution().unwrap();
    let store=world.domain.store.lock().unwrap();
    let metadata=store.conversation_metadata(&TASK.into(),stamp.conversation().as_str()).unwrap().unwrap();
    let plan=AccountMovePlan{from:work(),to:AccountId::default_account(),card:state.held_by_limit(&TASK.into()).map(|held|held.card).unwrap_or_else(||"historical-limit".into()),node:source_node,instance:stamp.instance().as_str().into(),session:alondra_provider_claude::conversation_session(&metadata).unwrap(),cwd:world.worktree.to_str().unwrap().into(),until:i64::MAX};
    let conn=store.conn_for_test();let number:i64=conn.query_row("SELECT coalesce(max(number),0)+1 FROM account_move_steps WHERE task_id=?1",[TASK],|row|row.get(0)).unwrap();
    for (kind,facts) in [("planned",serde_json::to_value(&plan).unwrap()),("host_stopped",serde_json::json!({})),("admitted",serde_json::json!({"agent":world.node()})),("done",serde_json::json!({}))]{
        conn.execute("INSERT INTO account_move_steps(task_id,number,step,at,facts) VALUES(?1,?2,?3,0,?4)",rusqlite::params![TASK,number,kind,facts.to_string()]).unwrap();
        if (kind=="planned" && step==AccountMoveStep::Planned)||(kind=="host_stopped" && step==AccountMoveStep::HostStopped)||(kind=="admitted" && step==AccountMoveStep::Admitted){break}
    }number
}

#[tokio::test]
async fn historical_unstarted_destinations_are_parked_before_startup_or_explicit_recovery(){
    for step in [AccountMoveStep::Admitted,AccountMoveStep::Done]{
        let mut world=World::with_accounts(true);*world.domain.moves.crash_after.lock().unwrap()=Some(AccountMoveStep::Admitted);
        let launch=crate::sdk_owner::Launch{task:TASK.into(),name:"admitted-destination".into(),directory:world.worktree.to_str().unwrap().into(),prompt:"Write the release notes".into(),plugin:world.home.join(crate::plugins::NODE),options:Default::default(),model:None,account:AccountId::default_account()};
        assert!(crate::sdk_owner::launch(&world.domain,launch).await.unwrap_err().message.contains("admission's commit"));
        let destination=world.node();
        run(&world.domain,&Actor::System,"historical-source",Command::RegisterAgent{agent_id:"legacy-source".into(),name:"legacy-source".into(),at:0}).unwrap();
        historical(&world,step,"legacy-source".into());
        let state=world.domain.state().unwrap();let life=state.lifecycle_of(&TASK.into()).unwrap();
        std::fs::write(world.backend.state_root.join(life.execution().unwrap().instance().as_str()).join("host.lock"),b"").unwrap();
        *world.domain.moves.crash_after.lock().unwrap()=None;world.restart().await;
        assert!(crate::sdk_owner::launch_admitted(world.domain.clone(),&world.backend,TASK.into(),&world.home.join(crate::plugins::NODE)).err().unwrap().to_string().contains("never positively observed"));
        tick(&world.domain).await;
        assert_eq!(world.step(),Some(step));assert_eq!(world.node(),destination);
        assert_eq!(world.domain.state().unwrap().task_account(&TASK.into()),AccountId::default_account(),"no silent account rollback");
        assert!(world.starts().is_empty(),"a persisted lock and UUID must not cause a native launch");
        assert!(!world.domain.owns_sdk_host(&destination));
        assert!(world.domain.state().unwrap().interactions.values().any(|interaction|interaction.body.contains("never positively observed")));
    }
}

#[tokio::test]
async fn a_historical_started_destination_is_adopted_with_its_existing_physical_process_and_binding(){
    let mut world=World::with_accounts(true);
    let launch=crate::sdk_owner::Launch{task:TASK.into(),name:"running-destination".into(),directory:world.worktree.to_str().unwrap().into(),prompt:"Write the release notes".into(),plugin:world.home.join(crate::plugins::NODE),options:Default::default(),model:None,account:AccountId::default_account()};
    let mut driver=crate::sdk_owner::start(world.domain.clone(),&world.backend,launch).unwrap();
    while !world.domain.state().unwrap().interactions.keys().any(|card|rested(card,1)){driver.step(&||false).unwrap();}
    world.until_answered(1).await;
    let destination=world.node();let starts=world.starts();let native_pid=starts[0]["pid"].as_i64().unwrap() as i32;
    run(&world.domain,&Actor::System,"historical-source",Command::RegisterAgent{agent_id:"legacy-source".into(),name:"legacy-source".into(),at:0}).unwrap();
    historical(&world,AccountMoveStep::Admitted,"legacy-source".into());world.set_moving(false);
    drop(driver);
    world.domain=World::boot(&world.home,&world.database,&world.backend);
    let (domain,backend)=(world.domain.clone(),world.backend.clone());
    tokio::task::spawn_blocking(move||crate::sdk_owner::adopt_all(&domain,&backend)).await.unwrap();
    tick(&world.domain).await;
    assert_eq!(unsafe{libc::kill(native_pid,0)},0,"the original fixture native process remains alive");
    assert_eq!(world.step(),Some(AccountMoveStep::Done));assert_eq!(world.node(),destination);
    assert_eq!(world.domain.state().unwrap().task_account(&TASK.into()),AccountId::default_account());
    assert_eq!(world.starts(),starts,"same physical process, not just the same UUID");assert!(world.domain.owns_sdk_host(&destination));
}


#[tokio::test]
async fn manual_replacement_of_an_existing_legacy_sdk_node_is_refused_before_stop_or_workspace_effects(){
    let world=World::new().await;let original=world.node();let starts=world.starts();
    let pid=starts[0]["pid"].as_i64().unwrap() as i32;let revision=world.domain.state().unwrap().revision;
    let body=serde_json::from_value(serde_json::json!({"task":TASK,"replace":true,"provider":"codex","model":"opus","reason":"Change model","cwd":"/must-not-create-or-check-this-workspace"})).unwrap();
    let error=crate::domain_orchestrator_tools::launch(&world.domain,&world.home,body,|_|None).await.unwrap_err();
    assert_eq!(error.code,"active_account_immutable");assert_eq!(world.domain.state().unwrap().revision,revision);
    assert_eq!(world.node(),original);assert!(world.domain.owns_sdk_host(&original));assert_eq!(world.starts(),starts);
    assert_eq!(unsafe{libc::kill(pid,0)},0,"forbidden replacement never stops the original native process");
    assert!(world.journal().is_empty());
}
