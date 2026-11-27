//! A staged task hands over from its author to a new implementer once the user approved the
//! author's plan: the author ends only once it rests, the implementer starts from what Alondra
//! writes for it, and nothing reaches or relaunches the author meanwhile.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::provider_transition::Phase;
use crate::provider_transition::TransitionsOf as _;
use alondra_engine_admission::AdmissionOf as _;
use alondra_core::model_selection::{ModelKey, ModelProfile, TaskSelection};
use super::*;

const TOLD: &str = "Rename the release notes file.";

fn runtime(domain: &Arc<DomainRuntime>) -> Arc<dyn alondra_engine_ports::Runtime> {
    domain.clone()
}

fn gate(domain: &DomainRuntime, slots: u32) {
    domain.admission().tune(crate::admission::Tuning { spacing: 0, limit: Some(crate::admission::Limit::Fixed(slots)) });
}

fn ready(slice: &Slice, domain: &Arc<DomainRuntime>) {
    domain.enable_sdk(slice.backend.clone());
    domain.admission().ignore_mac();
    gate(domain, 6);
}

/// Every role of the task runs on Claude's fixture model, which the user chose for it.
fn on_claude() -> BTreeMap<ExecutionRole, TaskSelection> {
    let mut chosen = TaskSelection::automatic(ModelProfile::General);
    chosen.request_model(Some(ModelKey { provider: Provider::Claude, model: "claude-fixture".into() }));
    chosen.account = Some(AccountKey::native(Provider::Claude));
    TaskStages::role_selections(&chosen)
}

/// A staged task whose author asked the user to approve its plan, its turn waiting on that card.
fn presenting_its_plan(slice: &Slice) -> (Driver, String) {
    let mut accounts = alondra_core::accounts::AccountsConfig::fresh();
    accounts.connect(Provider::Claude);
    std::fs::write(alondra_core::options::path_in(&slice._home), accounts.to_toml()).unwrap();
    claude_ready(slice);
    ready(slice, &slice.domain);
    slice.run(&system(), "stages", Command::RecordTaskStages {
        task_id: TASK.into(), staged: true, reason: "It changes the release flow".into(), selections: on_claude(),
    });
    slice.asked("decide:plan")
}

/// The user approved the author's plan; the approval has not reached the author's turn yet.
fn approved_author(slice: &Slice) -> (Driver, AgentId) {
    let (driver, card) = presenting_its_plan(slice);
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    let author = driver.agent().clone();
    (driver, author)
}

fn request_stage(domain: &Arc<DomainRuntime>) -> bool {
    crate::provider_transition::request_stage(&runtime(domain), &TASK.into()).unwrap()
}

async fn look(domain: &Arc<DomainRuntime>) -> Result<Phase, DomainError> {
    crate::provider_transition::advance(&runtime(domain), &TASK.into()).await
}

fn stage_steps(slice: &Slice) -> Vec<String> {
    stage_steps_in(&slice.database)
}

fn stage_steps_in(database: &std::path::Path) -> Vec<String> {
    let observer = rusqlite::Connection::open(database).unwrap();
    let mut statement = observer.prepare("SELECT phase FROM provider_transition_steps WHERE task_id=?1 ORDER BY sequence").unwrap();
    statement.query_map([TASK], |row| row.get(0)).unwrap().map(Result::unwrap).collect()
}

/// Whether the change of role is done as the daemon sees it. The daemon reads the journal under the
/// writer lock it holds from a commit until its publication; a connection of its own sees the
/// commit before the domain publishes it. So the change is done once its last step is committed
/// and the published state holds every command committed with it.
fn handed(database: &std::path::Path, domain: &DomainRuntime) -> bool {
    let published = domain.read().state().revision;
    let (done, committed): (bool, Option<i64>) = rusqlite::Connection::open(database).unwrap().query_row(
        "SELECT EXISTS(SELECT 1 FROM provider_transition_steps WHERE task_id=?1 AND phase='done'), (SELECT max(revision) FROM command_audit)",
        [TASK], |row| Ok((row.get(0)?, row.get(1)?))).unwrap();
    done && committed.is_none_or(|committed| u64::try_from(committed).unwrap() <= published)
}

fn admissions(domain: &DomainRuntime) -> usize {
    domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().filter(|record| matches!(record, LifecycleRecord::Admitted { .. })).count()
}

/// Whether the execution admitted last has ended or been stopped since.
fn ended_since_admitted(domain: &DomainRuntime) -> bool {
    domain.state().unwrap().lifecycle_journal[&TaskId::from(TASK)].iter().rev()
        .take_while(|record| !matches!(record, LifecycleRecord::Admitted { .. }))
        .any(|record| matches!(record, LifecycleRecord::HostEnded { .. } | LifecycleRecord::HostStopped { .. }))
}

/// The author's turn ended and no card holds it.
fn rests(domain: &DomainRuntime) -> bool {
    let lifecycle = Slice::lifecycle(domain);
    lifecycle.turn().is_none() && lifecycle.cards().is_empty()
}

fn held(domain: &DomainRuntime) -> (bool, bool) {
    let task = TaskId::from(TASK);
    (crate::provider_transition::holds_execution(domain, &task), crate::provider_transition::moving(domain, &task))
}

/// What the implementer was first told: the context Alondra wrote for it and its first input.
fn implementer_prompt(slice: &Slice) -> String {
    slice.prompts().into_iter().find(|prompt| prompt.contains("You are the implementer of this task"))
        .unwrap_or_else(|| panic!("the implementer is told its stage: {:?}", slice.prompts()))
}

/// Wakes after every owner commit and every published change of the domain's state.
struct Changes {
    commits: tokio::sync::watch::Receiver<u64>,
    published: tokio::sync::watch::Receiver<u64>,
}

impl Changes {
    fn of(domain: &DomainRuntime) -> Self {
        Self::checking(domain, || {})
    }

    /// Also runs `check` after every owner commit, before the domain publishes it.
    fn checking(domain: &DomainRuntime, mut check: impl FnMut() + Send + 'static) -> Self {
        let (commit, commits) = tokio::sync::watch::channel(0_u64);
        domain.before_owner_publish(move || {
            check();
            commit.send_modify(|count| *count += 1);
        });
        Self { commits, published: domain.subscribe() }
    }

    async fn next(&mut self) {
        tokio::select! {
            changed = self.commits.changed() => changed.unwrap(),
            changed = self.published.changed() => changed.unwrap(),
        }
    }

    async fn until(&mut self, what: &str, done: impl Fn() -> bool) {
        let reached = async { while !done() { self.next().await } };
        if tokio::time::timeout(std::time::Duration::from_secs(60), reached).await.is_err() {
            panic!("{what} never happened");
        }
    }
}

/// The author of the task presented its plan, the user approved it, the change of role started and
/// the author's turn ended: the change of role waits only for the daemon's next look.
async fn handing_over(slice: &Slice) -> AgentId {
    let (driver, author) = approved_author(slice);
    assert!(request_stage(&slice.domain), "the approval owes the implementer");
    let mut changes = Changes::of(&slice.domain);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    changes.until("the author's rest", || rests(&slice.domain)).await;
    author
}

/// Looks at the change of role after every change, as the daemon's transition loop does, until
/// it is done. A look that fails or aborts it fails the test with the steps it reached.
async fn handed_over(slice: &Slice, domain: &Arc<DomainRuntime>) {
    handed_over_checking(slice, domain, Changes::of(domain)).await
}

async fn handed_over_checking(slice: &Slice, domain: &Arc<DomainRuntime>, mut changes: Changes) {
    let done = || handed(&slice.database, domain);
    let handing = async {
        while !done() {
            let looked = look(domain).await;
            if done() { return }
            match looked {
                Ok(Phase::Aborted) => panic!("the change of role was aborted after {:?}", stage_steps(slice)),
                Ok(_) => {}
                Err(error) => panic!("the change of role stopped after {:?}: {}", stage_steps(slice), error.message),
            }
            changes.next().await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(60), handing).await.is_err() {
        panic!("the change of role never ended: {:?}, {:?}", stage_steps(slice), Slice::lifecycle(&**domain).activity());
    }
}

/// Looks at the change of role until it stops at `stop`, past the author's drain.
async fn stopped_at(slice: &Slice, domain: &Arc<DomainRuntime>, stop: Phase) {
    let mut changes = Changes::of(domain);
    let stopped = async {
        loop {
            match look(domain).await {
                Ok(phase) if phase == stop => return,
                Ok(Phase::Draining) => changes.next().await,
                other => panic!("the change of role did not stop at {stop:?}: {other:?} after {:?}", stage_steps(slice)),
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(60), stopped).await.is_err() {
        panic!("the change of role never reached {stop:?}: {:?}", stage_steps(slice));
    }
}

#[tokio::test]
async fn a_staged_tasks_author_hands_over_to_a_new_implementer_once_it_rests() {
    let slice = Slice::new();
    let task: TaskId = TASK.into();
    let author = handing_over(&slice).await;
    handed_over(&slice, &slice.domain).await;
    assert_eq!(stage_steps(&slice), ["requested", "draining", "source_fenced", "context_ready", "target_admitted", "target_ready", "consumed", "done"]);
    let state = slice.domain.state().unwrap();
    let handed = &state.tasks[&task];
    let journal = &state.lifecycle_journal[&task];
    let ended = journal.iter().position(|record| matches!(record, LifecycleRecord::HostEnded { .. })).expect("the author's end is recorded");
    let admitted: Vec<usize> = journal.iter().enumerate().filter(|(_, record)| matches!(record, LifecycleRecord::Admitted { .. })).map(|(at, _)| at).collect();
    assert_eq!(admitted.len(), 2, "the author and then its implementer");
    assert!(admitted[1] > ended, "the implementer is admitted only once the author ended");
    assert_eq!(handed.stages.runs.iter().map(|run| run.role).collect::<Vec<_>>(), [Some(ExecutionRole::Author), Some(ExecutionRole::Implementer)]);
    assert_eq!(handed.stages.handover, None, "the implementer's admission ends the change of role");
    assert_ne!(handed.agent_id.as_ref(), Some(&author), "a new agent implements the plan");
    assert_eq!(handed.selection.as_ref().map(|selection| selection.profile), Some(ModelProfile::Implementation), "the implementer starts with its role's selection");
    assert!(handed.pending_selection.is_none());
    assert_eq!(held(&slice.domain), (false, false), "nothing holds the implementer");
    let prompt = implementer_prompt(&slice);
    assert!(prompt.contains("Change the fixture") && prompt.contains("Test the fixture"), "the implementer starts from the approved plan: {prompt}");
    assert!(prompt.contains("as its implementer"), "{prompt}");
    assert!(!prompt.contains("decide:plan"), "the author's own prompt stays in its conversation: {prompt}");
    assert!(!prompt.contains(alondra_domain_model::HANDED_OVER_PLAN), "what the author was told stays in its conversation: {prompt}");
    slice.domain.stop_sdk_owners();
}

/// The owner commits the change of role's last step with the command that clears the task's
/// pending selection, and publishes that state only after: a look between the two must not read
/// the change of role as done.
#[tokio::test]
async fn a_change_of_role_reads_as_done_only_once_its_selection_is_published() {
    let slice = Slice::new();
    let task: TaskId = TASK.into();
    handing_over(&slice).await;
    let between = Arc::new(std::sync::Mutex::new(Vec::new()));
    let (database, domain, looks) = (slice.database.clone(), Arc::downgrade(&slice.domain), Arc::clone(&between));
    let changes = Changes::checking(&slice.domain, move || {
        let Some(domain) = domain.upgrade() else { return };
        let committed = stage_steps_in(&database).iter().any(|step| step == "done");
        if committed && domain.read().state().tasks[&TaskId::from(TASK)].pending_selection.is_some() {
            looks.lock().unwrap().push(handed(&database, &domain));
        }
    });
    handed_over_checking(&slice, &slice.domain, changes).await;
    let between = between.lock().unwrap().clone();
    assert!(!between.is_empty(), "the last step is committed before the selection it clears is published");
    assert!(between.iter().all(|done| !done), "a look before the publication reads the change of role as done");
    assert!(slice.domain.state().unwrap().tasks[&task].pending_selection.is_none());
    slice.domain.stop_sdk_owners();
}

/// The owner that takes over the author's host hands it the user's approval only once the gate
/// gives its turn a slot: with none, the author's turn stays open for as long as the test looks.
#[tokio::test]
async fn an_author_still_in_its_turn_keeps_its_host_and_hands_over_once_it_rests() {
    let slice = Slice::new();
    let (driver, author) = approved_author(&slice);
    assert!(request_stage(&slice.domain));
    gate(&slice.domain, 0);
    for _ in 0..2 {
        assert_eq!(look(&slice.domain).await.unwrap(), Phase::Draining, "the author has not ended its turn");
    }
    let lifecycle = Slice::lifecycle(&*slice.domain);
    assert!(lifecycle.turn().is_some());
    assert!(lifecycle.answer_goes_on(), "the approval waits for a slot of the gate");
    assert_eq!(stage_steps(&slice), ["requested", "draining"]);
    assert!(!ended_since_admitted(&slice.domain), "its host is not stopped");
    assert!(slice.domain.owns_sdk_host(&author), "the change of role takes over the author's host so its turn can end");
    drop(driver);
    gate(&slice.domain, 6);
    let home = slice.domain.home().to_path_buf();
    crate::admission::pass(&*slice.domain, &home).await.unwrap();
    handed_over(&slice, &slice.domain).await;
    assert_eq!(admissions(&slice.domain), 2);
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn an_author_waiting_on_its_own_question_hands_over_only_once_the_user_answers() {
    let slice = Slice::new();
    let task: TaskId = TASK.into();
    let (driver, author) = approved_author(&slice);
    slice.run(&Actor::Agent { agent_id: author.clone() }, "author-asks", Command::AskQuestion {
        attachments: vec![], interaction_id: "q-notes-format".into(), scope: Scope::Task { task_id: task.clone() }, recipient: Recipient::User,
        body: "Which format do the release notes take?".into(), blocking: true,
        options: vec![QuestionOption { id: "keep".into(), label: "Keep a Changelog".into(), description: None },
            QuestionOption { id: "plain".into(), label: "Plain list".into(), description: None }],
        card: CardSpec::new(CardKind::Decide, "Choose the release notes format").why("The plan leaves it open"),
    });
    assert!(request_stage(&slice.domain));
    let mut changes = Changes::of(&slice.domain);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    changes.until("the end of the author's turn", || Slice::lifecycle(&*slice.domain).turn().is_none()).await;
    for _ in 0..2 {
        assert_eq!(look(&slice.domain).await.unwrap(), Phase::Draining, "the author waits on the user's answer to its question");
    }
    assert!(!ended_since_admitted(&slice.domain), "its host is not stopped");
    assert!(slice.domain.owns_sdk_host(&author));
    slice.run(&user(), "answer-format", Command::AnswerQuestion {
        attachments: vec![], interaction_id: "q-notes-format".into(), option_id: "plain".into(), inputs: BTreeMap::new(),
    });
    handed_over(&slice, &slice.domain).await;
    assert_eq!(admissions(&slice.domain), 2);
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn a_change_of_role_survives_a_daemon_restart_after_every_step_without_a_second_implementer() {
    for died in [Phase::Draining, Phase::SourceFenced, Phase::ContextReady] {
        let slice = Slice::new();
        let task: TaskId = TASK.into();
        handing_over(&slice).await;
        *slice.domain.selection_changes().crash_at.lock().unwrap() = Some(died);
        let mut changes = Changes::of(&slice.domain);
        let crashed = async {
            loop {
                match look(&slice.domain).await {
                    Err(error) => return error,
                    Ok(Phase::Draining) => changes.next().await,
                    Ok(other) => panic!("the change of role went past {died:?} to {other:?}"),
                }
            }
        };
        let error = tokio::time::timeout(std::time::Duration::from_secs(60), crashed).await.unwrap_or_else(|_| panic!("the daemon never died at {died:?}"));
        assert_eq!(error.message, "the daemon died");
        assert_eq!(stage_steps(&slice).last().map(String::as_str), Some(died.key()), "the daemon died right after {died:?}");
        slice.domain.stop_sdk_owners();
        slice.domain.selection_changes().die();
        let domain = slice.restart();
        ready(&slice, &domain);
        handed_over(&slice, &domain).await;
        let state = domain.state().unwrap();
        assert_eq!(admissions(&domain), 2, "a restart after {died:?} starts no second implementer");
        assert_eq!(state.tasks[&task].stages.runs.iter().map(|run| run.role).collect::<Vec<_>>(), [Some(ExecutionRole::Author), Some(ExecutionRole::Implementer)]);
        domain.stop_sdk_owners();
    }
}

#[tokio::test]
async fn a_daemon_restart_between_the_approval_and_the_first_step_still_hands_over_once() {
    for requested in [false, true] {
        let slice = Slice::new();
        let task: TaskId = TASK.into();
        let (driver, _) = approved_author(&slice);
        if requested {
            assert!(request_stage(&slice.domain));
            *slice.domain.selection_changes().crash_at.lock().unwrap() = Some(Phase::Requested);
            assert_eq!(look(&slice.domain).await.unwrap_err().message, "the daemon died");
            assert_eq!(stage_steps(&slice), ["requested"]);
        }
        drop(driver);
        slice.domain.stop_sdk_owners();
        slice.domain.selection_changes().die();
        let domain = slice.restart();
        ready(&slice, &domain);
        assert_eq!(request_stage(&domain), !requested, "only a change of role that never started is started after the restart");
        handed_over(&slice, &domain).await;
        assert_eq!(admissions(&domain), 2);
        assert_eq!(domain.state().unwrap().tasks[&task].stages.runs.iter().map(|run| run.role).collect::<Vec<_>>(), [Some(ExecutionRole::Author), Some(ExecutionRole::Implementer)]);
        domain.stop_sdk_owners();
    }
}

#[tokio::test]
async fn an_author_whose_host_dies_before_its_change_of_role_starts_still_hands_over() {
    let slice = Slice::new();
    let (driver, _) = approved_author(&slice);
    slice.kill_host(&driver);
    drop(driver);
    assert!(stage_steps(&slice).is_empty(), "the host died before the change of role started");
    assert!(request_stage(&slice.domain));
    handed_over(&slice, &slice.domain).await;
    assert_eq!(admissions(&slice.domain), 2);
    assert!(implementer_prompt(&slice).contains("Change the fixture"), "the implementer starts from the approved plan");
    slice.domain.stop_sdk_owners();
}

/// The change of role waits for the user: no look starts it, and its card offers to try again.
fn waits_for_the_user(domain: &Arc<DomainRuntime>, attempts: u8) {
    let state = domain.state().unwrap();
    let task: TaskId = TASK.into();
    let handover = state.tasks[&task].stages.handover.clone().expect("the task still owes its implementer");
    assert_eq!((handover.refused, handover.attempts, handover.transition), (true, attempts, None));
    let card = state.open_start_problem(&task).expect("the user is asked");
    assert!(matches!(alondra_domain_model::start_problem::from_body(&card.body), Some(StartProblem::StageStartRefused { role: ExecutionRole::Implementer, .. })), "{}", card.body);
    assert!(matches!(&card.payload, InteractionPayload::Question { options, .. }
        if options.iter().map(|option| option.id.as_str()).collect::<Vec<_>>() == [START_RETRY, START_CANCEL]), "{:?}", card.payload);
}

#[tokio::test]
async fn a_change_of_role_of_a_task_left_without_an_execution_asks_the_user() {
    let slice = Slice::new();
    let (driver, _) = approved_author(&slice);
    let mut changes = Changes::of(&slice.domain);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    changes.until("the author's rest", || rests(&slice.domain)).await;
    slice.domain.stop_sdk_owners();
    slice.run(&system(), "release", Command::ReleaseTask { task_id: TASK.into(), reason: "Its node is gone".into(), relaunch: Relaunch::ByItself, cwd: None });
    assert!(Slice::lifecycle(&*slice.domain).execution().is_none());
    slice.run(&system(), "node-closed", Command::RaiseStartProblem { task_id: TASK.into(), problem: StartProblem::NodeClosed });
    assert!(!request_stage(&slice.domain));
    let state = slice.domain.state().unwrap();
    assert!(!state.tasks[&TaskId::from(TASK)].stages.handover.as_ref().unwrap().refused, "the change of role waits while another start problem is open");
    let other = state.open_start_problem(&TASK.into()).unwrap().id.clone();
    slice.answer(&other.0, START_RETRY, None).unwrap();
    for _ in 0..2 {
        assert!(!request_stage(&slice.domain));
        waits_for_the_user(&slice.domain, 1);
    }
    assert!(stage_steps(&slice).is_empty());
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn an_implementer_whose_start_is_refused_for_good_asks_the_user_at_once() {
    let slice = Slice::new();
    handing_over(&slice).await;
    gate(&slice.domain, 0);
    stopped_at(&slice, &slice.domain, Phase::ContextReady).await;
    let state = slice.domain.state().unwrap();
    let mut accounts = state.accounts.clone();
    for account in accounts.iter_mut().filter(|account| account.provider == Provider::Claude && account.id.is_default()) {
        account.state = AccountState::SignedOut;
    }
    slice.run(&system(), "sign-out", Command::RecordAccounts { accounts, reserve: state.account_reserve });
    gate(&slice.domain, 6);
    let home = slice.domain.home().to_path_buf();
    crate::admission::pass(&*slice.domain, &home).await.unwrap();
    crate::provider_transition::pass(&runtime(&slice.domain)).await;
    assert_eq!(stage_steps(&slice).last().map(String::as_str), Some("aborted"), "{:?}", stage_steps(&slice));
    waits_for_the_user(&slice.domain, 1);
    assert_eq!(admissions(&slice.domain), 1);
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn the_admission_gate_holds_the_implementers_first_turn_until_it_lets_it_through() {
    let slice = Slice::new();
    handing_over(&slice).await;
    gate(&slice.domain, 0);
    stopped_at(&slice, &slice.domain, Phase::ContextReady).await;
    assert!(ended_since_admitted(&slice.domain), "the author ended");
    assert_eq!(admissions(&slice.domain), 1, "no implementer starts while the gate has no slot for its turn");
    let mut changes = Changes::of(&slice.domain);
    for _ in 0..3 {
        let _ = look(&slice.domain).await;
        if tokio::time::timeout(std::time::Duration::from_millis(200), changes.next()).await.is_err() { break }
    }
    assert!(!stage_steps(&slice).iter().any(|step| step == "consumed"), "the implementer takes no turn while the gate has no slot: {:?}", stage_steps(&slice));
    assert!(!slice.prompts().iter().any(|prompt| prompt.contains("You are the implementer of this task")), "nothing reaches the implementer yet");
    gate(&slice.domain, 6);
    let home = slice.domain.home().to_path_buf();
    crate::admission::pass(&*slice.domain, &home).await.unwrap();
    handed_over(&slice, &slice.domain).await;
    assert_eq!(admissions(&slice.domain), 2);
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn nothing_reaches_or_relaunches_the_author_from_the_approval_of_its_plan_on() {
    let slice = Slice::new();
    let task: TaskId = TASK.into();
    let (driver, card) = presenting_its_plan(&slice);
    assert_eq!(held(&slice.domain), (false, false), "an author whose plan waits for the user is held by nothing");
    slice.answer(&card, Hold::APPROVE, None).unwrap();
    assert_eq!(held(&slice.domain), (true, true), "the approval holds the author's execution");
    assert!(!to_take(&*slice.domain, &slice.domain.state().unwrap(), &task), "no takeover starts the author again");
    let starts = slice.starts().len();
    retake(&*slice.domain, task.clone());
    assert!(!crate::sdk_owner::being_retaken(&*slice.domain, &task), "a retake leaves the author alone");
    crate::domain_messages::tell_sdk_node(&*slice.domain, &task, crate::node_push::ALONDRA, TOLD, "tell-author", false).unwrap();
    let mut changes = Changes::of(&slice.domain);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    changes.until("the author's rest", || rests(&slice.domain)).await;
    assert_eq!(held(&slice.domain), (true, true), "the resting author stays held until its implementer starts");
    assert_eq!(slice.starts().len(), starts, "nothing started the author again");
    assert!(!slice.prompts().iter().any(|prompt| prompt.contains(TOLD)), "what Alondra told the task waits for the implementer: {:?}", slice.prompts());
    slice.domain.stop_sdk_owners();
}

#[tokio::test]
async fn an_author_that_rested_before_its_change_of_role_started_hands_the_implementer_its_stage_not_a_reminder() {
    let slice = Slice::new();
    let (driver, _) = approved_author(&slice);
    let mut changes = Changes::of(&slice.domain);
    slice.domain.add_sdk_owner(Owner::spawn(driver));
    changes.until("the author's rest", || rests(&slice.domain)).await;
    assert!(request_stage(&slice.domain), "the change of role starts on a later look");
    handed_over(&slice, &slice.domain).await;
    let prompt = implementer_prompt(&slice);
    assert!(prompt.contains("Current input:\nCarry on with this Alondra task as its implementer"), "the implementer's first input is its stage: {prompt}");
    assert!(!prompt.contains("Your turn ended without a result"), "the reminder meant for the author never reaches the implementer: {prompt}");
    slice.domain.stop_sdk_owners();
}
