//! The owner of an SDK execution, on the domain's writer lane: it admits the execution with
//! `AdmitExecution`, launches the host, sends each dispatch the store issued for its task,
//! stages what the host observes and acknowledges it only once readers see the commit.
//! Every step commits through `Runtime::own`; nothing here writes the store itself.
use alondra_engine_kernel::sdk_backend::SdkOf as _;
use alondra_engine_kernel::domain_runtime::RuntimeRecords as _;
use alondra_daemon_engine_model::{LaunchOptionsFile as _, WrittenFailure as _};
use alondra_engine_ports::gate::GateOf as _;
use alondra_engine_ports::Runtime;
use crate::engine::EngineOf as _;
use alondra_engine_ports::sdk::{SessionStart, StartKind};
use crate::account_move::MovesOf as _;
use alondra_engine_kernel::bg_sessions::HandoversOf as _;
use alondra_core::clock;
use alondra_core::clock::Instant;
use crate::locks::Poisonable as _;
pub use alondra_engine_kernel::sdk_backend as backend;
mod worker;
use alondra_engine_ports::{claude_history, conversation_file};
mod session_refresh;
mod node_environment;
pub use alondra_engine_kernel::untaken;
pub(crate) use alondra_daemon_engine_model::NativeAccountIdentity;
use worker::{Worker, Found};
use alondra_core::accounts::{AccountKey, Provider};
pub use backend::SdkBackend;
pub(crate) use crate::domain_runtime::seals::{self, Seals};

use crate::domain_store::{Attached, Preparer, StoreError};
use alondra_agent_application::InputMetadata;
use alondra_engine_ports::agent_host::{EndedHost, HostStop, VerifiedArtifact};
use alondra_domain_rules::*;
use alondra_domain::ActivityView;
use alondra_foundation::InputId;
use alondra_provider_claude::{ClaudeConfiguration, ClaudeWorker, Retained, WorkerProfile, answered_by_event, answered_control, context_event, native_exited};
use alondra_provider_contract::{AdapterError, Capability, Handoff, OrderedObservation, PreparedCommand, ProviderBinding};
use alondra_store::{AuthorityScope, Committed, ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub use alondra_engine_ports::hosts::{HOST, runs_through_sdk};
const DRIVER: &str = "claude-agent-sdk";
/// How long the owner waits for the host before it looks again for a control to send: a quiet
/// turn never holds back an interrupt.
const WAKE: Duration = Duration::from_millis(250);
/// How long a host handed an input may take to open its native session before another takes its
/// place. Opening starts the native process, which a loaded Mac makes slow; it has nothing to do
/// with the I/O timeout, which bounds one frame on the host's socket.
const OPENING_TIMEOUT: Duration = Duration::from_secs(120);
/// How long the owner waits before each new host it puts in place of a replacement that ended
/// without taking its input: a technical end never asks the user, and past the last wait every
/// new host waits the last again, so a host that keeps failing is never relaunched in a loop.
const RELAUNCH_WAITS: [Duration; 5] = [Duration::from_secs(2), Duration::from_secs(10), Duration::from_secs(30), Duration::from_secs(120), Duration::from_secs(300)];
/// Hosts in a row whose Claude never showed it ran before the task holds on the card that says its
/// session could not be resumed.
const SILENT_HOSTS: usize = 3;

/// The wait before the new host of relaunch `earlier` + 1.
fn relaunch_wait(earlier: usize) -> Duration {
    RELAUNCH_WAITS[earlier.min(RELAUNCH_WAITS.len() - 1)]
}

/// Tests relaunch at once: what they check is that a relaunch happens, never how long it waits.
fn pause_before_relaunch(earlier: usize) -> Duration {
    if alondra_foundation::under_test() { Duration::ZERO } else { relaunch_wait(earlier) }
}

/// What an SDK execution's binding lets the domain ask of it.
pub fn capabilities() -> BTreeSet<Capability> {
    BTreeSet::from([
        Capability::OfferInput,
        Capability::Interrupt,
        Capability::SetModel,
        Capability::SetPermissionMode,
        Capability::Compact,
        Capability::McpStatus,
        Capability::Decisions,
        Capability::McpReconnect,
        Capability::McpSignIn,
        Capability::ForceStop,
        Capability::McpClearAuth,
    ])
}

fn receipt(handoff: Handoff) -> DispatchReceipt {
    match handoff {
        Handoff::Offered => DispatchReceipt::Handed,
        Handoff::RefusedBeforeOffer => DispatchReceipt::Rejected,
        Handoff::Uncertain => DispatchReceipt::Uncertain,
    }
}

/// Why nothing starts on a daemon without an SDK backend, which only a broken install has.
pub use backend::NO_HOST;


/// What a launch through the SDK starts: the task, in its worktree, told `prompt`.
pub use alondra_engine_ports::hosts::Launch;

pub use alondra_daemon_engine_model::{Failed, LaunchOptions};
pub(crate) use alondra_daemon_engine_model::retain_selection_options;

fn failed(what: &str) -> impl Fn(StoreError) -> Failed + '_ {
    move |error| Failed {
        why: format!("{what}: {}", store_text(&error)),
        no_space: matches!(&error, StoreError::Storage(error) if alondra_engine_ports::journal::no_space(error)),
        refusal: match &error { StoreError::Domain(error) => Some(error.clone()), StoreError::Storage(_) => None },
    }
}
fn store_text(error: &StoreError) -> String {
    match error {
        StoreError::Domain(error) => error.message.clone(),
        StoreError::Storage(error) => format!("{error:#}"),
    }
}
fn adapter(what: &str) -> impl Fn(AdapterError) -> Failed + '_ {
    move |error| Failed::new(format!("{what}: {error}"))
}

impl Preparer for Worker {
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        Worker::stage_input(self, metadata)
    }
    fn prepare(&mut self, attempt: &DispatchAttempt, metadata: InputMetadata<'_, '_>) -> Result<PreparedCommand, AdapterError> {
        Worker::prepare(self, attempt, metadata)
    }
}

/// The one takeover of a task's execution that runs now. A takeover reserves the next owner and may
/// start a host: a second one at once fences the first, which then finds the second's host live,
/// refuses it and loses the session. Another takeover of the task waits for this one to end.
pub(crate) struct TakeoverTurn<'a> {
    domain: &'a dyn Runtime,
    task: TaskId,
}

impl<'a> TakeoverTurn<'a> {
    pub(crate) fn wait(domain: &'a dyn Runtime, task: &TaskId) -> Self {
        let mut taking = domain.sdk_owners().taking_over.lock_or_recover("the takeovers under way");
        while taking.contains(task) {
            domain.takeover_waited();
            taking = domain.sdk_owners().takeover_done.wait(taking).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        taking.insert(task.clone());
        Self { domain, task: task.clone() }
    }
}

impl Drop for TakeoverTurn<'_> {
    fn drop(&mut self) {
        self.domain.sdk_owners().taking_over.lock_or_recover("the takeovers under way").remove(&self.task);
        self.domain.sdk_owners().takeover_done.notify_all();
    }
}

/// Whether an owner of this daemon holds the host of the task's node.
fn owned_now(domain: &dyn Runtime, task: &TaskId) -> bool {
    domain.state().tasks.get(task).and_then(|task| task.agent_id.clone()).is_some_and(|agent| domain.owns_sdk_host(&agent))
}

/// Takes over the execution of a task no owner of this daemon holds, once no other takeover of it
/// runs: one that ended meanwhile left an owner, and nothing more is taken.
pub(crate) fn take_over_unowned(domain: &Arc<dyn Runtime>, backend: &SdkBackend, task: &TaskId, plugin: Option<&std::path::Path>) -> Result<(), Failed> {
    let _turn = TakeoverTurn::wait(&**domain, task);
    if owned_now(&**domain, task) {
        return Ok(());
    }
    let driver = adopt(Arc::clone(domain), backend, task.clone(), plugin)?;
    domain.add_sdk_owner(Owner::spawn(driver));
    Ok(())
}

/// One execution's owner: its worker and the task whose dispatches and controls only it claims.
/// The task's SDK execution is in hand while this lives.
pub(crate) struct InHand {
    domain: Arc<dyn Runtime>,
    task: TaskId,
}

impl InHand {
    fn new(domain: &Arc<dyn Runtime>, task: &TaskId) -> Self {
        *domain.sdk_owners().in_hand.lock_or_recover("the executions in hand").entry(task.clone()).or_default() += 1;
        Self { domain: Arc::clone(domain), task: task.clone() }
    }
}

impl Drop for InHand {
    fn drop(&mut self) {
        let mut held = self.domain.sdk_owners().in_hand.lock_or_recover("the executions in hand");
        if let Some(count) = held.get_mut(&self.task) {
            *count -= 1;
            if *count == 0 {
                held.remove(&self.task);
            }
        }
    }
}

struct PauseSubscription {
    snapshots: tokio::sync::watch::Receiver<u64>,
    paused: bool,
}

impl PauseSubscription {
    fn new(domain: &dyn Runtime, agent: &AgentId) -> Self {
        let mut snapshots = domain.subscribe_snapshots();
        // Mark before reading: a publication racing the read remains pending.
        drop(snapshots.borrow_and_update());
        let paused = domain.read().node_pause(agent).is_some_and(|pause| !pause.released);
        Self { snapshots, paused }
    }

    fn get(&mut self, domain: &dyn Runtime, agent: &AgentId) -> bool {
        if self.snapshots.has_changed().unwrap_or(false) {
            drop(self.snapshots.borrow_and_update());
            self.paused = domain.read().node_pause(agent).is_some_and(|pause| !pause.released);
        }
        self.paused
    }
}

pub struct Driver {
    pub in_hand: Option<InHand>,
    /// The capability its host was started or adopted with, which goes when the host ends.
    capability: Option<[u8; 32]>,
    domain: Arc<dyn Runtime>,
    pause: Mutex<PauseSubscription>,
    pub worker: Worker,
    scope: AuthorityScope,
    task: TaskId,
    agent: AgentId,
    permits: VecDeque<DispatchPermit>,
    controls: VecDeque<ControlPermit>,
    /// The input last handed to the host, until the task's lifecycle no longer waits for it.
    awaiting: Option<InputId>,
    /// The host was launched to replace one that ended and has been handed nothing yet: it runs no
    /// Claude, so it has nothing to say whatever the lifecycle still waits for.
    replacement: bool,
    /// Controls the host took whose answer is an event of its own, until that event is staged.
    results: Vec<alondra_foundation::CommandId>,
    /// The host closed its end while this daemon ran: the fact that ends this driver.
    pub host_closed: bool,
    /// A step of this owner failed: its host is stopped and replaced like one that closed.
    owner_failed: bool,
    native_account_refusal:Option<String>,
    /// Why the Codex session of this host ended on its own, as its host said it.
    native_failure: Option<alondra_provider_codex::SessionFailure>,
    /// The proof of the native account the host recorded is kept apart from its folder.
    identity_kept: bool,
    /// The Claude session the host last said it runs, so the log names each one once.
    claude_session: Option<String>,
    /// The node rests on a host a daemon of another release launched: a host of this release that
    /// resumes the same session takes its place.
    moves_to_this_release: bool,
    /// The node rests with nothing in its host: the host closes and no other takes its place until
    /// the task has something to do.
    pub releases: bool,
    /// This host resumes a released session and has not been handed its work yet: since when.
    resuming_since: Option<Instant>,
    /// This host resumes a released session that has not started a turn: what it was handed may
    /// not have been read.
    resumes_released: bool,
    /// Where this host's Claude keeps the conversation of its session. Each input it is handed
    /// first decides whether a new session takes that conversation over (`session_refresh`).
    conversation: Option<session_refresh::Conversation>,
    /// This host's session started empty and has not been handed an input yet: nothing of it is
    /// cached to expire.
    new_session: bool,
    /// This replacement was handed an input and did not take it: its end is watched and its place
    /// taken by another host after `relaunch_wait`.
    untaken: bool,
    /// The obligation watch gave this owner's process up: a host that resumes the same session
    /// takes its place, as it does for a first host.
    requeued: bool,
    /// Earlier replacements in this recovery chain.
    relaunches: usize,
    /// Hosts in a row, before this one, whose Claude never showed it ran: a session that cannot
    /// start would otherwise be resumed for ever, each new host taking an input and losing it.
    silent_hosts: usize,
    /// The node plugin this host was taken over with, which the host that replaces it takes too.
    plugin: Option<PathBuf>,
    /// How long a host handed an input may take to open the Claude session.
    opening_timeout: Duration,
    /// Past this moment, a host handed an input whose session has not opened is stuck: another
    /// host takes its place and gets what is pending again, since a session that never opened read
    /// nothing.
    opening_deadline: Option<Instant>,
    /// Inputs that are themselves the resend of one this host refused before offering it.
    refused: BTreeSet<InputId>,
    pub refuse_offers: usize,
    pub(crate) fail_next_step: bool,
    pub(crate) panic_next_step: bool,
    /// What this driver and whoever stops its owner agree on before a handoff to the host.
    handoffs: Arc<Handoffs>,
    /// Runs once a dispatch has recorded its command, just before the command goes to the host.
    pub(crate) before_handoff: Option<Box<dyn FnOnce(&Handoffs) + Send>>,
    /// Runs once the owner reads as ended after it closed the host of a resting node, just before its driver is dropped.
    pub(crate) before_release_ends: Option<Box<dyn FnOnce() + Send>>,
}

/// What an owner's thread and whoever stops its host agree on. The thread hands an input or a
/// control to the host only while nobody stops it, and a stop asks the host to end only once the
/// handoff under way has its receipt. A host told to stop while the owner writes to it closes
/// without reading the command; that command's receipt stays uncertain, nothing can prove whether
/// Claude got it, and the task's execution cannot end.
#[derive(Default)]
pub struct Handoffs {
    /// A holder that panics leaves it whole, so it is recovered: flags set or read.
    state: Mutex<HandoffState>,
    changed: std::sync::Condvar,
}

#[derive(Default)]
struct HandoffState {
    stopping: bool,
    handing: bool,
    stop_waits: bool,
}

/// A handoff under way; it ends when dropped, a panic's unwinding included.
struct Handing<'a>(&'a Handoffs);

impl Drop for Handing<'_> {
    fn drop(&mut self) {
        self.0.state.lock_or_recover("the SDK handoffs").handing = false;
        self.0.changed.notify_all();
    }
}

impl Handoffs {
    /// Begins a handoff, unless the owner is being stopped.
    fn begin(&self) -> Option<Handing<'_>> {
        let mut state = self.state.lock_or_recover("the SDK handoffs");
        if state.stopping {
            return None;
        }
        state.handing = true;
        Some(Handing(self))
    }

    /// No handoff begins from now on, and the one under way ends first.
    fn stop(&self) {
        let mut state = self.state.lock_or_recover("the SDK handoffs");
        state.stopping = true;
        while state.handing {
            state.stop_waits = true;
            state = self.changed.wait(state).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        state.stop_waits = false;
    }

    /// Whether a stop waits for the handoff under way to end.
    pub(crate) fn stop_waits(&self) -> bool {
        self.state.lock_or_recover("the SDK handoffs").stop_waits
    }
}

/// Admits the execution, starts its host and hands it the launch: once this returns, the task
/// runs through the SDK, and any failure before it is the launch's own, never a `claude --bg`.
pub fn start(domain: Arc<dyn Runtime>, backend: &SdkBackend, launch: Launch) -> Result<Driver, Failed> {
    let offered = provider_capabilities(launch.options.provider);
    start_bound(domain, backend, launch, offered, None, None)
}

struct NativeRecovery {
    admission: ExecutionIdentity,
    session: String,
    identity: Option<NativeAccountIdentity>,
}

enum NativeRecoveryStatus {
    Unstarted,
    Recoverable(NativeRecovery),
    Unavailable(Failed),
}

pub(crate) fn original_directory(domain: &dyn Runtime, task: &TaskId) -> Result<Option<String>, Failed> {
    let state = domain.state();
    let admission = state.lifecycle_journal.get(task).and_then(|records| records.iter().rev().find_map(|record| match record {
        LifecycleRecord::Admitted { execution: Some(execution), .. } if execution.host == HOST => Some(execution),
        _ => None,
    }));
    let Some(admission) = admission else { return Ok(None) };
    domain.store().lock_or_exit("the domain store").conversation_directory(task, &admission.conversation)
        .map_err(|error| Failed::new(format!("Reading the original native worktree: {error:#}")))
}

pub struct Reopening {
    task: TaskId,
    account: AccountKey,
    directory: PathBuf,
    input: String,
    recovery: Option<NativeRecovery>,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn prepare_reopening(
    domain: &dyn Runtime, backend: &SdkBackend, task: &TaskId, provider: Provider,
    account: &alondra_core::accounts::AccountId, directory: &std::path::Path,
    input: String, recover_directory: bool,
) -> Result<Reopening, Failed> {
    if domain.state().pending_reopening(task) != Some(input.as_str()) {
        return Err(Failed::new("The task's reopening changed before its node could start".into()));
    }
    let recovery = if recover_directory {
        match native_recovery_status(domain, backend, task, provider, account, &directory.to_string_lossy(), true)? {
            NativeRecoveryStatus::Recoverable(recovery) => Some(recovery),
            NativeRecoveryStatus::Unstarted => None,
            NativeRecoveryStatus::Unavailable(why) => {
                log!("reopened task {task} starts a new native session: {why}");
                None
            }
        }
    } else {
        log!("reopened task {task} starts a new native session: its original native directory cannot be recovered");
        None
    };
    Ok(Reopening { task: task.clone(), account: AccountKey::new(provider, account.clone()), directory: directory.to_path_buf(), input, recovery })
}

pub fn require_recoverable_session(domain: &dyn Runtime, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &alondra_core::accounts::AccountId, directory: &str) -> Result<(), Failed> {
    native_recovery(domain, backend, task, provider, account, directory).map(drop)
}

fn native_recovery(domain: &dyn Runtime, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &alondra_core::accounts::AccountId, directory: &str) -> Result<Option<NativeRecovery>, Failed> {
    match native_recovery_status(domain, backend, task, provider, account, directory, false)? {
        NativeRecoveryStatus::Unstarted => Ok(None),
        NativeRecoveryStatus::Recoverable(recovery) => Ok(Some(recovery)),
        NativeRecoveryStatus::Unavailable(why) => Err(why),
    }
}

fn native_recovery_status(domain: &dyn Runtime, backend: &SdkBackend, task: &TaskId, provider: Provider, account: &alondra_core::accounts::AccountId, directory: &str, require_history: bool) -> Result<NativeRecoveryStatus, Failed> {
    let state = domain.state();
    let records = state.lifecycle_journal.get(task).cloned().unwrap_or_default();
    let unavailable = |why: String| Ok(NativeRecoveryStatus::Unavailable(Failed::new(why)));
    let Some((at, admission)) = records.iter().enumerate().rev().find_map(|(at, record)| match record {
        LifecycleRecord::Admitted { execution: Some(execution), .. } if execution.host == HOST => Some((at, execution.clone())),
        _ => None,
    }) else {
        if state.tasks.get(task).is_some_and(|task| task.started) || records.iter().any(|record| matches!(record, LifecycleRecord::Admitted { .. } | LifecycleRecord::ResourceAvailable)) {
            return unavailable("This task has legacy native history without a verified SDK continuation. Recover the original legacy session before replacing its node; no fresh session was started".into());
        }
        return Ok(NativeRecoveryStatus::Unstarted);
    };
    let unproven = "The original native session cannot be proven. Keep this node on its original account and recover its retained native history; create a new node to start a fresh session";
    if records.iter().skip(at + 1).any(|record| matches!(record, LifecycleRecord::Admitted { .. }))
        || !records.iter().skip(at + 1).any(|record| matches!(record, LifecycleRecord::ResourceAvailable)) {
        return unavailable(unproven.into());
    }
    let task_state = state.tasks.get(task).ok_or_else(|| Failed::new(unproven.into()))?;
    if task_state.account_key() != AccountKey::new(provider, account.clone()) {
        return unavailable("Choose account and provider when creating a node. Recovery must use the original native account".into());
    }
    let (metadata, original_directory) = {
        let store = domain.store().lock_or_exit("the domain store");
        (store.conversation_metadata(task, &admission.conversation).map_err(|error| Failed::new(format!("Reading the original native conversation: {error:#}")))?,
         store.conversation_directory(task, &admission.conversation).map_err(|error| Failed::new(format!("Reading the original native worktree: {error:#}")))?)
    };
    if original_directory.as_deref() != Some(directory) {
        return unavailable("Native session recovery must use the original task worktree".into());
    }
    let Some(session) = metadata.as_deref().and_then(|metadata| match provider {
        Provider::Claude => alondra_provider_claude::conversation_session(metadata),
        Provider::Codex => alondra_provider_codex::conversation_session(metadata),
    }) else { return unavailable(unproven.into()); };
    let selected = backend.for_provider(provider, domain.home()).map_err(|error| Failed::new(format!("Locating the original native account: {error:#}")))?;
    let selected = on_account_provider(domain, &selected, provider, account)?;
    let identity = match NativeAccountIdentity::recover(&selected.state_root.join(&admission.instance), provider) {
        Ok(identity) => identity,
        Err(why) => return Ok(NativeRecoveryStatus::Unavailable(why)),
    };
    if let Some(change) = identity.as_ref().and_then(|identity| state.accounts.iter().find(|actual| actual.provider == provider && actual.id == *account).and_then(|actual| identity.change(actual))) {
        return unavailable(format!("The original native account {change} changed. Restore it before recovering this session"));
    }
    if provider == Provider::Codex {
        if !identity.as_ref().is_some_and(NativeAccountIdentity::can_resume_managed) {
            return unavailable("This Codex session has no retained managed-account proof. A fresh native process cannot resume an unverified or API-key identity".into());
        }
        let thread = match require_native_thread_mapping(&selected, &session, account) {
            Ok(thread) => thread,
            Err(why) => return Ok(NativeRecoveryStatus::Unavailable(why)),
        };
        if require_history {
            let selected = selected.for_start().map_err(|error| Failed::new(format!("Locating the original Codex executable: {error:#}")))?;
            let retained = crate::blocking_work::blocking(|| domain.ports().accounts().retained_codex_thread(
                &selected.native, &selected.native_environment, &thread, std::path::Path::new(directory)));
            if !retained.unwrap_or(false) {
                return unavailable("The original Codex thread is unavailable in its native profile".into());
            }
        }
    }
    if require_history && provider == Provider::Claude
        && !claude_history::available(&selected.native_environment, &session, std::path::Path::new(directory)) {
        return unavailable("The original Claude session history is unavailable in its native profile".into());
    }
    Ok(NativeRecoveryStatus::Recoverable(NativeRecovery { admission, session, identity }))
}

fn require_native_thread_mapping(backend:&SdkBackend,session:&str,account:&alondra_core::accounts::AccountId)->Result<String,Failed>{
    use std::os::unix::fs::{MetadataExt,OpenOptionsExt};use std::io::Read;use sha2::Digest;
    let directory=backend.state_root.join("native-threads");let path=directory.join(session);
    let fail=||Failed::new("The original private Codex thread mapping is unavailable. Restore the retained session instead of starting another thread".into());
    let parent=std::fs::symlink_metadata(&directory).map_err(|_|fail())?;let before=std::fs::symlink_metadata(&path).map_err(|_|fail())?;
    if !parent.is_dir() || parent.file_type().is_symlink() || parent.mode()&0o777!=0o700 || !before.is_file() || before.file_type().is_symlink() || before.mode()&0o777!=0o600 || before.uid()!=parent.uid() || before.nlink()!=1 || before.len()>1024{return Err(fail())}
    let mut file=std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW|libc::O_NONBLOCK|libc::O_CLOEXEC).open(path).map_err(|_|fail())?;
    let actual=file.metadata().map_err(|_|fail())?;if before.dev()!=actual.dev() || before.ino()!=actual.ino(){return Err(fail())}
    let mut bytes=Vec::new();file.by_ref().take(1025).read_to_end(&mut bytes).map_err(|_|fail())?;if bytes.len()>1024{return Err(fail())}
    let value:serde_json::Value=serde_json::from_slice(&bytes).map_err(|_|fail())?;
    let home=sha2::Sha256::digest(backend.native_environment.get("CODEX_HOME").map(String::as_str).unwrap_or("native_default").as_bytes()).iter().map(|byte|format!("{byte:02x}")).collect::<String>();
    if value["accountId"].as_str()!=Some(account.as_str()) || value["homeDigest"].as_str()!=Some(home.as_str()) || !value["threadId"].as_str().is_some_and(|id|!id.is_empty() && id.len()<=256){return Err(fail())}Ok(value["threadId"].as_str().expect("validated above").to_owned())
}

/// The existing Claude session an SDK execution continues instead of starting one.
pub enum Continuation {
    /// A `claude --bg` node's: its id is the session.
    Node(AgentId),
    Native { session: String },
    /// The task's own, which its execution on another account takes past `card`, the usage limit
    /// of the account it leaves, as the task's move `number`.
    Moved { session: String, card: InteractionId, number: i64 },
    Transition { id: String },
}

impl Continuation {
    fn session(&self) -> Option<&str> {
        match self {
            Self::Node(node) => Some(&node.0),
            Self::Native { session } => Some(session),
            Self::Moved { session, .. } => Some(session),
            Self::Transition { .. } => None,
        }
    }
}

/// Admits an SDK execution that continues the existing Claude session `continuation` names, in
/// the folder of the launch, and records the admission in the same transaction, in the journal
/// of the move it is part of. The launch's prompt is the first input: it opens that session with
/// `resume`.
pub(crate) fn continue_session(domain: Arc<dyn Runtime>, backend: &SdkBackend, launch: Launch, continuation: Continuation) -> Result<Driver, Failed> {
    let offered = provider_capabilities(launch.options.provider);
    start_bound(domain, backend, launch, offered, Some(continuation), None)
}

pub fn start_bound(domain: Arc<dyn Runtime>, backend: &SdkBackend, launch: Launch, capabilities: BTreeSet<Capability>, continuation: Option<Continuation>, mut reopening: Option<Reopening>) -> Result<Driver, Failed> {
    let _in_hand = InHand::new(&domain, &launch.task);
    domain.engine().require_initial_check().map_err(|error| Failed::new(error.message))?;
    let provider = launch.options.provider;
    if reopening.as_ref().is_some_and(|reopening| reopening.task != launch.task
        || reopening.account != AccountKey::new(provider, launch.account.clone())
        || reopening.directory != PathBuf::from(&launch.directory)) {
        return Err(Failed::new("The prepared reopening belongs to another node launch".into()));
    }
    let backend = &backend.for_provider(provider, domain.home()).map_err(|error| Failed::new(format!("{error:#}")))?;
    let backend = &backend.for_start().map_err(|error| Failed::new(format!("{error:#}")))?;
    let backend = &on_account_provider(&*domain, backend, provider, &launch.account)?;
    let suffix = crate::auth::random_hex(8);
    let agent = AgentId(format!("sdk-{suffix}"));
    let _preparing = alondra_engine_ports::agent_host::Preparing::hold(&backend.state_root.join(&agent.0));
    let state = domain.state();
    if reopening.as_ref().is_some_and(|reopening| state.pending_reopening(&launch.task) != Some(reopening.input.as_str())) {
        return Err(Failed::new("The task's reopening changed before admission; refresh the task".into()));
    }
    let recovery = if let Some(reopening) = &mut reopening {
        reopening.recovery.take()
    } else if continuation.is_none() {
        native_recovery(&*domain, backend, &launch.task, provider, &launch.account, &launch.directory)?
    } else { None };
    let continuation=continuation.or_else(||recovery.as_ref().map(|recovery|Continuation::Native{session:recovery.session.clone()}));
    let envelope = |name: &str, command: Command| CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("sdk-{name}-{}-{suffix}", launch.task).into(),
        expected_revision: 0,
        command,
    };
    let binding = binding_for(provider, backend, capabilities, &launch.account, crate::domain_store::generation_provider(&state, &AccountKey { provider, account: launch.account.clone() }))?;
    let mut configuration = configuration(&*domain, backend, &binding, &launch.directory, &agent.0, true, Some(worker_profile(&launch.plugin, &launch.options, &launch.directory)?))?;
    if let Some(Continuation::Transition { id }) = &continuation {
        let (prepared, conversation) = crate::provider_transition::retained_handoff(&*domain, &launch.task, id).map_err(|error|Failed::new(error.message))?;
        let profile = configuration.worker.as_mut().ok_or_else(||Failed::new("The transfer has no worker profile".into()))?;
        profile.handoff_context = Some(prepared.context);
        if let Some(folder) = conversation.as_deref().and_then(std::path::Path::parent) {
            read_also(&mut profile.settings, folder);
        }
    }
    launch.options.save(&configuration.state_directory)?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed::new(format!("{error:?}")))?;
    domain
        .execute_as_system(envelope("register", Command::RegisterAgent {
            agent_id: agent.clone(),
            name: launch.name.clone(),
            at: alondra_core::now_millis(),
        }))
        .map_err(failed("registering the node"))?;
    let capability = domain.bind_agent(&agent.0).map_err(failed("issuing the node's capability"))?;
    let unadmitted = Unadmitted { domain: Some(Arc::clone(&domain)), agent: agent.clone() };
    configuration.native_environment.extend(node_environment(domain.home(), &agent, capability.expose_for_child_environment()));
    let mut worker = Worker::configured(binding.clone(), configuration).map_err(adapter("configuring the SDK worker"))?;
    if let Some(session) = continuation.as_ref().and_then(Continuation::session) {
        worker.continue_session(session).map_err(adapter("continuing the node's session"))?;
    }
    let admit = envelope("admit", Command::AdmitExecution {
        task_id: launch.task.clone(),
        agent_id: agent.clone(),
        conversation: format!("sdk-conversation-{suffix}"),
        instance: agent.0.clone(),
        host: HOST.into(),
        provider,
        model: launch.model.clone(),
        account: Some(launch.account.clone()),
        leaves_limit: match &continuation {
            Some(Continuation::Moved { card, .. }) => Some(card.clone()),
            _ => None,
        },
    });
    let (first, _) = domain
        .own(scope.clone(), |owner| {
            if let Some(reopening) = &reopening
                && owner.state().pending_reopening(&launch.task) != Some(reopening.input.as_str()) {
                return Err(DomainError::new("conflict", "The task's reopening changed before admission; refresh the task").into());
            }
            let key=AccountKey::new(provider,launch.account.clone());
            let actual=owner.state().accounts.iter().find(|account|account.provider==provider && account.id==launch.account);
            if launch.options.selection.is_some() && actual.is_none_or(|account|account.state!=alondra_core::accounts::AccountState::SignedIn) {
                return Err(DomainError::new("account_unavailable","The selected provider account is no longer signed in; refresh the account before starting").into());
            }
            if actual.is_some_and(|account|matches!(account.billing,alondra_core::accounts::AccountBilling::PayPerUse)) && !launch.options.selection.as_ref().is_some_and(|selection|selection.account.as_ref()==Some(&key) && selection.paid_account_grant.as_ref()==Some(&key)) {
                return Err(DomainError::new("paid_authorization_required","This account is now paid per use; select it explicitly to authorize billable usage before starting").into());
            }
            if binding.credential_generation()!=crate::domain_store::generation_provider(owner.state(),&key) {
                return Err(DomainError::new("account_selection_changed","The selected account credentials changed before admission; refresh the selection").into());
            }
            if let Some(recovery)=&recovery{
                let latest=owner.state().lifecycle_journal.get(&launch.task).and_then(|records|records.iter().rev().find_map(|record|match record{LifecycleRecord::Admitted{execution:Some(execution),..}=>Some(execution),_=>None}));
                if latest!=Some(&recovery.admission){return Err(DomainError::new("native_session_changed","The original native session changed before recovery; refresh the task").into())}
                owner.require_native_identity(&binding,recovery.identity.as_ref().map(|identity| identity as &dyn crate::domain_store::KnownIdentity))?;
                if let Some(identity)=&recovery.identity{identity.retain(&backend.state_root.join(&agent.0)).map_err(|error|StoreError::Storage(anyhow::anyhow!("{error}")))?;}
            }else{NativeAccountIdentity::save(&backend.state_root.join(&agent.0),actual).map_err(|error|StoreError::Storage(anyhow::anyhow!("{error}")))?;}
            owner.stage_command(&Actor::System, &admit, Attached::None)?;
            if let Some(selection) = &launch.options.selection {
                let pending = match &continuation { Some(Continuation::Transition { id }) => owner.pending_transition_selection(id)?, _ => None };
                let mut selection=selection.clone();selection.native_tool_denials=Some(launch.options.disallowed_tools.clone());
                owner.stage_command(&Actor::System, &envelope("selection", Command::RecordTaskSelection {
                    task_id: launch.task.clone(), selection, pending,
                }), Attached::None)?;
            }
            let text = if matches!(continuation,Some(Continuation::Transition { .. })) { owner.transition_launch_text(&launch.task,&launch.prompt)? } else { launch.prompt.clone() };
            let admitted = owner.launch(&launch.task, &text, &launch.directory, &binding, &mut worker)?;
            match &continuation {
                Some(Continuation::Node(node)) => owner.record_sdk_admitted(node, &launch.task, &agent)?,
                Some(Continuation::Moved { number, .. }) => owner.record_move_admitted(&launch.task, *number, &agent)?,
                Some(Continuation::Transition { id }) => owner.admit_provider_transition(id, &admitted)?,
                Some(Continuation::Native { .. }) | None => {}
            }
            let lifecycle = owner.lifecycle(&launch.task)?;
            Ok(lifecycle.pending_inputs().first().map(|input| (*input).clone()))
        })
        .map_err(failed("admitting the execution"))?;
    unadmitted.admitted();
    if match &continuation {
        Some(Continuation::Node(_)) => *domain.handovers().crash_after.lock().unwrap() == Some(crate::domain_store::HandoverStep::SdkAdmitted),
        Some(Continuation::Moved { .. }) => *domain.moves().crash_after.lock().unwrap() == Some(crate::domain_store::AccountMoveStep::Admitted),
        None => *domain.moves().crash_after.lock().unwrap() == Some(crate::domain_store::AccountMoveStep::Admitted),
        Some(Continuation::Transition { .. }) | Some(Continuation::Native { .. }) => false,
    } {
        return Err(Failed::new("the daemon died after the admission's commit".into()));
    }
    let mut driver = Driver::new(domain, worker, scope, launch.task, agent);
    driver.conversation = Some(session_refresh::Conversation { native_environment: backend.native_environment.clone(), directory: launch.directory.clone() });
    driver.new_session = start_kind(continuation.as_ref()) != StartKind::Resumed;
    let mut sealed = driver.sealed();
    let grant = sealed.owners.pop().ok_or_else(|| Failed::new("the admission issued no owner grant".into()))?;
    let (state, _) = driver.domain.own(driver.scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    driver.sealed();
    driver.worker.install_owner(grant, state).map_err(adapter("starting the SDK host"))?;
    driver.log_host("started");
    driver.domain.engine().context_launched(&driver.task, start_kind(continuation.as_ref()));
    let permit = driver.permits.pop_front().ok_or_else(|| Failed::new("the admission issued no launch permit".into()))?;
    if let Err(failed) = driver.dispatch(permit) {
        let (directory, runtime) = (driver.worker.state_directory().to_path_buf(), Arc::clone(&driver.domain));
        drop(driver);
        return Err(end_launched(&*runtime, &directory, failed));
    }
    driver.awaiting = first;
    Ok(driver)
}

/// Lets Claude read `folder` besides its worktree, without asking.
fn read_also(settings: &mut serde_json::Map<String, serde_json::Value>, folder: &std::path::Path) {
    let permissions = settings.entry("permissions").or_insert_with(|| serde_json::json!({}));
    if let Some(permissions) = permissions.as_object_mut() {
        permissions.insert("additionalDirectories".into(), serde_json::json!([folder]));
    }
}

/// How a host that starts for `continuation` runs its conversation.
fn start_kind(continuation: Option<&Continuation>) -> StartKind {
    match continuation {
        Some(Continuation::Transition { .. }) => StartKind::Handoff,
        Some(_) => StartKind::Resumed,
        None => StartKind::New,
    }
}

/// The capability a host was started or adopted with. One a host that resumes the node was
/// issued since stays.
struct EndedCapability { domain: Arc<dyn Runtime>, agent: AgentId, digest: Option<[u8; 32]> }
impl EndedCapability {
    fn revoke(self) {
        if let Some(digest) = self.digest
            && let Err(error) = self.domain.revoke_capability(&digest) {
            log!("the capability of {}, whose host ended, could not be revoked: {error:#}", self.agent);
        }
    }
}

/// A host that ended under this daemon takes its node's capability with it: the host that resumes
/// the node is issued a new one, and a node no host resumes keeps none.
fn host_ended(domain: &dyn Runtime, agent: &AgentId) {
    if let Err(error) = domain.revoke_agent(&agent.0) {
        log!("the capability of {agent}, whose host ended, could not be revoked: {error:#}");
    }
}

/// The capability of a node whose execution was not admitted: a launch that stops before its
/// admission commits revokes it, so a start refused again and again leaves none behind.
struct Unadmitted { domain: Option<Arc<dyn Runtime>>, agent: AgentId }
impl Unadmitted {
    fn admitted(mut self) {
        self.domain = None;
    }
}
impl Drop for Unadmitted {
    fn drop(&mut self) {
        if let Some(domain) = &self.domain
            && let Err(error) = domain.revoke_agent(&self.agent.0) {
            log!("the capability of {} could not be revoked: {error:#}", self.agent.0);
        }
    }
}

/// One binding per SDK configuration, account and generation of its credential, named by them:
/// the store keeps one active binding per provider instance, so a binding of each launch would
/// revoke every task after the first, and a configuration that changes (a new release's host, the
/// account's folder) or an account signed in again binds the next executions while the earlier
/// ones keep the binding they were admitted under. Its credential is the account.
fn binding(backend: &SdkBackend, capabilities: BTreeSet<Capability>, account: &alondra_core::accounts::AccountId, generation: u64) -> Result<ProviderBinding, Failed> {
    binding_for(Provider::Claude, backend, capabilities, account, generation)
}
fn binding_for(provider: Provider, backend: &SdkBackend, capabilities: BTreeSet<Capability>, account: &alondra_core::accounts::AccountId, generation: u64) -> Result<ProviderBinding, Failed> {
    let configuration = backend.configuration_digest();
    let instance = if provider == Provider::Claude { format!("{HOST}-{}-{account}-{generation}", &configuration[..16]) }
        else { format!("{HOST}-codex-{}-{account}-{generation}", &configuration[..16]) };
    let driver = if provider == Provider::Claude { DRIVER } else { "codex-app-server" };
    ProviderBinding::new(
        format!("{driver}-{instance}"),
        driver.into(),
        instance,
        configuration,
        1,
        3,
        capabilities,
        account.to_string(),
        generation,
    )
    .map_err(|error| Failed::new(format!("the provider binding: {error:?}")))
}

/// The started backend one host of an execution on `account` runs with.
fn on_account(domain: &dyn Runtime, started: &SdkBackend, account: &alondra_core::accounts::AccountId) -> Result<SdkBackend, Failed> {
    started.for_account(domain.home(), account, &alondra_engine_kernel::accounts_file::registered(domain.home())).map_err(|error| Failed::new(format!("{error:#}")))
}
fn on_account_provider(domain: &dyn Runtime, started: &SdkBackend, provider: Provider, account: &alondra_core::accounts::AccountId) -> Result<SdkBackend, Failed> {
    started.for_account_provider(domain.home(), provider, account, &alondra_engine_kernel::accounts_file::registered_provider(domain.home(), provider)).map_err(|error| Failed::new(format!("{error:#}")))
}
fn provider_capabilities(provider: Provider) -> BTreeSet<Capability> {
    if provider == Provider::Claude { return capabilities(); }
    BTreeSet::from([Capability::OfferInput, Capability::Interrupt, Capability::ForceStop, Capability::SetModel, Capability::SetPermissionMode, Capability::McpStatus, Capability::McpSignIn, Capability::Decisions, Capability::BackgroundWork, Capability::DurableResume])
}


/// The account an execution's binding runs it on.
fn account_of(binding: &ProviderBinding) -> alondra_core::accounts::AccountId {
    alondra_daemon_engine_model::account_of(binding.credential())
}

/// The launch of a daemon with an SDK backend: the task runs through the SDK or does not
/// start, with the reason. It never falls back to `claude --bg`.
pub async fn launch(domain: &dyn Runtime, launch: Launch) -> Result<serde_json::Value, DomainError> {
    launch_prepared(domain, launch, None).await
}

pub(crate) async fn launch_prepared(domain: &dyn Runtime, launch: Launch, reopening: Option<Reopening>) -> Result<serde_json::Value, DomainError> {
    let Some((backend, runtime)) = domain.sdk() else {
        return Err(DomainError::new("unavailable", NO_HOST));
    };
    let backend = backend.clone();
    let task = launch.task.clone();
    let driver = tokio::task::spawn_blocking(move || {
        if let Some(reopening) = reopening {
            let offered = provider_capabilities(launch.options.provider);
            start_bound(runtime, &backend, launch, offered, None, Some(reopening))
        } else {
            start(runtime, &backend, launch)
        }
    })
        .await
        .map_err(|error| DomainError::new("launch_failed", format!("the SDK launch stopped: {error}")))?
        .map_err(|Failed { why, refusal, .. }| match refusal {
            Some(refusal) if refusal.code == alondra_domain_rules::capability_capacity_card::CAPABILITY_CAPACITY_CODE => refusal,
            _ => DomainError::new("launch_failed", format!("the task did not start through the Agent SDK: {why}")),
        })?;
    let agent = driver.agent().clone();
    domain.add_sdk_owner(Owner::spawn(driver));
    Ok(serde_json::json!({"agent_id": agent, "short_id": agent.0.chars().take(8).collect::<String>(), "task": task}))
}

/// Takes over the execution of `task` a previous daemon left: it reserves the next owner
/// generation and finds the execution's host. A host that still runs is adopted with the same
/// session, and every dispatch a crash left unresolved is reconciled against the receipt it
/// retained. A host that ended (no process holds its lock, as after the Mac restarts) is replaced:
/// the evidence its journal kept is committed, each unresolved dispatch is resolved from it, and a
/// fresh host is launched that resumes the same native session on the next input. `plugin` is
/// Alondra's node plugin, which that fresh host's Claude loads.
pub(crate) fn adopt_for_transition(domain: Arc<dyn Runtime>, backend:&SdkBackend,task:TaskId,plugin:Option<&std::path::Path>)->Result<Option<Driver>,Failed>{take_host(domain,backend,task,plugin,true,false,false)}

pub fn adopt(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>) -> Result<Driver, Failed> {
    let mut driver = take(domain, backend, task, plugin, true)?.ok_or_else(|| Failed::new("the ended host was not replaced".into()))?;
    driver.plugin = plugin.map(std::path::Path::to_path_buf);
    Ok(driver)
}

/// Launches the first host of the task's SDK execution, which a daemon that ended right after
/// admitting it never started (its folder holds no host): the launch it never sent goes now, with
/// a new owner generation.
pub(crate) fn launch_admitted(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId, plugin: &std::path::Path) -> Result<Driver, Failed> {
    let _in_hand = InHand::new(&domain, &task);
    crate::account_move::require_started_destination(&*domain,&task).map_err(Failed::new)?;
    let state = domain.state();
    let provider = state.tasks.get(&task).map(|task| task.provider).ok_or_else(|| Failed::new("the task is missing".into()))?;
    let backend = &backend.for_provider(provider, domain.home()).and_then(|selected| selected.for_start()).map_err(|error| Failed::new(format!("{error:#}")))?;
    let agent = state.tasks.get(&task).and_then(|t| t.agent_id.clone()).ok_or_else(|| Failed::new("the task has no node".into()))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed::new(format!("{error:?}")))?;
    let (adoption, _) = domain
        .own(scope.clone(), |owner| {
            let adoption = owner.adopt(&task)?;
            owner.reissue_permit(&task)?;
            Ok(adoption)
        })
        .map_err(failed("reserving the next owner"))?;
    let mut sealed = domain.take_seals(&task, &agent);
    let grant = sealed.owners.pop().ok_or_else(|| Failed::new("the reservation issued no owner grant".into()))?;
    let permit = sealed.permits.pop().ok_or_else(|| Failed::new("the reservation issued no launch permit".into()))?;
    let instance = adoption.stamp.instance().as_str().to_owned();
    let mut options = LaunchOptions::load(&backend.state_root.join(&instance))?;
    options.provider = provider;
    let backend = &on_account_provider(&*domain, backend, Provider::from_driver(grant.binding().driver()).ok_or_else(|| Failed::new("unknown execution provider".into()))?, &account_of(grant.binding()))?;
    let mut configuration = configuration(&*domain, backend, grant.binding(), &adoption.directory, &instance, false, Some(worker_profile(plugin, &options, &adoption.directory)?))?;
    let capability = domain.bind_agent(&agent.0).map_err(failed("issuing the node's capability"))?;
    let unadmitted = Unadmitted { domain: Some(Arc::clone(&domain)), agent: agent.clone() };
    configuration.native_environment.extend(node_environment(domain.home(), &agent, capability.expose_for_child_environment()));
    let mut worker = Worker::configured(grant.binding().clone(), configuration).map_err(adapter("configuring the SDK worker"))?;
    let identity=NativeAccountIdentity::load(worker.state_directory())?;
    domain.own(scope.clone(),|owner|owner.require_native_identity(grant.binding(),identity.as_ref().map(|identity| identity as &dyn crate::domain_store::KnownIdentity))).map_err(failed("Alondra cannot resume this session"))?;
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    domain.take_seals(&task, &agent);
    worker.install_owner(grant, adapter_state).map_err(adapter("starting the SDK host"))?;
    unadmitted.admitted();
    let mut driver = Driver::new(domain, worker, scope, task.clone(), agent);
    driver.conversation = Some(session_refresh::Conversation { native_environment: backend.native_environment.clone(), directory: adoption.directory.clone() });
    driver.new_session = true;
    driver.log_host("started, as its admission's first host");
    driver.domain.engine().context_launched(&driver.task, StartKind::New);
    if let Err(failed) = driver.dispatch(permit) {
        let (directory, runtime) = (driver.worker.state_directory().to_path_buf(), Arc::clone(&driver.domain));
        drop(driver);
        return Err(end_launched(&*runtime, &directory, failed));
    }
    let read = driver.domain.read();
    driver.awaiting = read.state().lifecycle_of(&task).ok()
        .and_then(|lifecycle| lifecycle.pending_inputs().first().map(|input| (*input).clone()));
    drop(read);
    Ok(driver)
}

/// Whether the host of the task's current SDK execution was ever started: its folder holds the
/// lock every host takes, which stays after it ends.
pub(crate) fn host_started(backend: &SdkBackend, domain: &dyn Runtime, task: &TaskId) -> bool {
    let read = domain.read();
    let instance = read.state().lifecycle_of(task).ok()
        .and_then(|lifecycle| lifecycle.execution().map(|stamp| stamp.instance().as_str().to_owned()));
    let provider = read.state().tasks.get(task).map(|task| task.provider).unwrap_or_default();
    let selected = backend.for_provider(provider, domain.home());
    instance.is_none_or(|instance| selected.as_ref().is_ok_and(|backend| backend.state_root.join(instance).join("host.lock").exists()))
}

/// Ends the task's host, which no owner of this daemon holds (its owner stopped it, or the daemon
/// that owned it is gone), and commits what became of it: the evidence its journal kept, each
/// dispatch resolved from it, and the host's end, which closes its open requests and their cards.
/// A host that still runs is ended by the lock it holds. No host takes its place and no Claude
/// session is resumed: the task's next execution, if any, is a new one.
pub(crate) fn end_unowned_host(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId) -> Result<(), Failed> {
    take(domain, backend, task, None, false).map(drop)
}

/// `end_unowned_host`, keeping the ended host's folder locked from the moment its end was read:
/// the proof that nothing of the host writes any more is never released and taken again.
pub(crate) fn end_and_claim_unowned_host(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId) -> Result<Option<EndedHost>, Failed> {
    let mut claimed = None;
    take_host_into(domain, backend, task, None, false, true, false, &mut claimed)?;
    Ok(claimed)
}

fn take(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>, relaunch: bool) -> Result<Option<Driver>, Failed> {
    take_host(domain, backend, task, plugin, relaunch, true,false)
}

/// A live host that refuses its adoption (its owner's socket is gone, it runs a release this one
/// cannot carry on with, its bootstrap no longer matches) can never be reached again: with
/// `end_refused`, it is ended by the lock it holds and the execution is taken again, now with a
/// host that ended. A live host of a release this one can carry on with is adopted as it runs.
fn take_host(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>, relaunch: bool, end_refused: bool, explicit_account_retry:bool) -> Result<Option<Driver>, Failed> {
    take_host_into(domain, backend, task, plugin, relaunch, end_refused, explicit_account_retry, &mut None)
}

/// `take_host`, which without `relaunch` leaves in `claimed` the ended host's folder it claimed, still locked.
#[allow(clippy::too_many_arguments)]
fn take_host_into(domain: Arc<dyn Runtime>, backend: &SdkBackend, task: TaskId, plugin: Option<&std::path::Path>, relaunch: bool, end_refused: bool, explicit_account_retry:bool, claimed: &mut Option<EndedHost>) -> Result<Option<Driver>, Failed> {
    let _in_hand = InHand::new(&domain, &task);
    let taken_at = clock::instant();
    if relaunch { crate::account_move::require_started_destination(&*domain,&task).map_err(Failed::new)?; }
    let base_backend = backend;
    let state = domain.state();
    let provider = state.tasks.get(&task).map(|task| task.provider).ok_or_else(|| Failed::new("the task is missing".into()))?;
    let backend = &backend.for_provider(provider, domain.home()).and_then(|selected| selected.for_start()).map_err(|error| Failed::new(format!("{error:#}")))?;
    let agent = state.tasks.get(&task).and_then(|t| t.agent_id.clone()).ok_or_else(|| Failed::new("the task has no node to adopt".into()))?;
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed::new(format!("{error:?}")))?;
    let (adoption, _) = domain
        .own(scope.clone(), |owner| {
            let adoption = owner.adopt(&task)?;
            if relaunch {
                owner.reissue_permit(&task)?;
            }
            Ok(adoption)
        })
        .map_err(failed("reserving the next owner"))?;
    domain.owner_reserved();
    let mut reserved = domain.take_seals(&task, &agent);
    let grant = reserved.owners.pop().ok_or_else(|| Failed::new("the reservation issued no owner grant".into()))?;
    let owner_generation = grant.generation();
    let stamp = adoption.stamp;
    let mut options = LaunchOptions::load(&backend.state_root.join(stamp.instance().as_str()))?;
    options.provider = provider;
    if let Some(model) = state.last_model_asked(&task) {
        options.model = Some(model);
    }
    let profile = plugin.map(|plugin| worker_profile(plugin, &options, &adoption.directory)).transpose()?;
    let on_account = &on_account_provider(&*domain, backend, Provider::from_driver(grant.binding().driver()).ok_or_else(|| Failed::new("unknown execution provider".into()))?, &account_of(grant.binding()))?;
    let mut worker = Worker::configured(grant.binding().clone(), configuration(&*domain, on_account, grant.binding(), &adoption.directory, stamp.instance().as_str(), false, profile)?)
        .map_err(adapter("configuring the SDK worker"))?;
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    domain.take_seals(&task, &agent);
    let commands: Vec<&[u8]> = adoption.receipt_candidates.iter().map(|candidate| candidate.command.as_slice()).collect();
    let retain = |proofs: Vec<alondra_provider_contract::HandoffProof>| -> Result<(), Failed> {
        if proofs.is_empty() { return Ok(()) }
        domain.own(scope.clone(), |owner| {
            for proof in &proofs {
                let candidate = adoption.receipt_candidates.iter().find(|candidate| candidate.command == proof.command())
                    .ok_or(alondra_store::Error::CommandConflict)?;
                owner.record_handoff_proof(&grant, candidate.intent_id, proof)?;
            }
            Ok(())
        }).map(drop).map_err(failed("retaining handoff receipts"))
    };
    retain(worker.archived_handoffs(&grant, &adapter_state, &commands).map_err(adapter("reading archived handoff receipts"))?)?;
    let stopped_by_alondra = worker.state_directory().join(STOPPED_BY_ALONDRA);
    let journal = match worker.find_host(&grant, &adapter_state).map_err(adapter("finding the SDK host"))? {
        Found::Live => None,
        Found::Ended(journal) => Some(journal),
    };
    if journal.is_some() {
        retain(worker.ended_handoffs(&grant, &adapter_state, &commands).map_err(adapter("reading the ended host's handoff receipts"))?)?;
    }
    let by_alondra = stopped_by_alondra.exists();
    if journal.is_none() {
        remove_stop_mark(&stopped_by_alondra);
    }
    let Some(journal) = journal else {
        if !relaunch {
            if !end_refused {
                return Err(Failed::new("the host still runs".into()));
            }
            log!("the SDK host of {task} still runs with no owner: it is stopped without resuming its Claude session");
            drop(worker);
            mark_stopped_by_alondra(&backend.state_root.join(stamp.instance().as_str()));
            alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts| hosts.end_holders(&backend.state_root.join(stamp.instance().as_str())))
                .map_err(|error| Failed::new(format!("its host did not stop: {error}")))?;
            return take_host_into(domain, base_backend, task, plugin, relaunch, false,explicit_account_retry, claimed);
        }
        let codex_checkpoint=if worker.provider()==Provider::Codex{adapter_state.checkpoint().map(|checkpoint|checkpoint.bytes().to_vec())}else{None};
        if let Err(error) = worker.adopt_owner(grant, adapter_state) {
            if !end_refused {
                return Err(adapter("adopting the SDK host")(error));
            }
            log!("the live SDK host of {task} refused its adoption ({error:?}): it is stopped and a host that resumes the same Claude session takes its place");
            drop(worker);
            mark_stopped_by_alondra(&backend.state_root.join(stamp.instance().as_str()));
            alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts| hosts.end_holders(&backend.state_root.join(stamp.instance().as_str())))
                .map_err(|error| Failed::new(format!("its host refused to be taken over and did not stop: {error}")))?;
            return take_host_into(domain, base_backend, task, plugin, relaunch, false,explicit_account_retry, claimed);
        }
        if worker.native_liveness().map_err(adapter("checking the adopted native process"))? == Some(alondra_provider_codex::NativeLiveness::Lost) {
            worker.stop().map_err(adapter("stopping the adopted host without its app-server"))?;
            drop(worker);
            return take_host_into(domain, base_backend, task, plugin, relaunch, false,explicit_account_retry, claimed);
        }
        if adopted_without_process(worker.stopper()).map_err(|error| Failed::new(format!("checking the adopted host's process: {error}")))? {
            log!("the SDK host of {task} answered its adoption but its process is gone: its end is recorded");
            drop(worker);
            mark_stopped_by_alondra(&backend.state_root.join(stamp.instance().as_str()));
            alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts| hosts.end_holders(&backend.state_root.join(stamp.instance().as_str())))
                .map_err(|error| Failed::new(format!("the leftovers of its host did not stop: {error}")))?;
            return take_host_into(domain, base_backend, task, plugin, relaunch, false,explicit_account_retry, claimed);
        }
        if worker.runs_another_release() {
            log!("the live SDK host of {task} runs the release before: it goes on, and once the node rests a host of this release takes its place");
        }
        let mut driver = Driver::new(domain, worker, scope, task.clone(), agent);
        driver.conversation = Some(session_refresh::Conversation { native_environment: on_account.native_environment.clone(), directory: adoption.directory.clone() });
        if let Some(checkpoint)=codex_checkpoint{driver.domain.engine().restore_codex_scope(&driver.task,&driver.agent,&checkpoint);}
        driver.log_host("was adopted");
        driver.receive_permits(reserved.permits);
        driver.controls.extend(reserved.control_permits);
        for command in adoption.commands {
            let handoff = driver.worker.reconcile(&command.command).map_err(adapter("reconciling a dispatch"))?;
            let intent = command.intent_id;
            driver.domain.own(driver.scope.clone(), |owner| owner.resolve(&stamp, intent, owner_generation, receipt(handoff))).map_err(failed("resolving a dispatch"))?;
        }
        for control in adoption.controls {
            let handoff = driver.worker.reconcile(&control.command).map_err(adapter("reconciling a control"))?;
            driver.domain.own(driver.scope.clone(), |owner| owner.resolve_control(&stamp, control.id, owner_generation, receipt(handoff))).map_err(failed("resolving a control"))?;
            if handoff == Handoff::Offered {
                driver.results.extend(answered_by_event(&control.command));
            }
        }
        driver.sealed();
        let read = driver.domain.read();
        driver.awaiting = read.state().lifecycle_of(&task).ok()
            .filter(|lifecycle| !lifecycle.held_by_pause())
            .and_then(|lifecycle| lifecycle.pending_inputs().first().map(|input| (*input).clone()));
        drop(read);
        if driver.awaiting.is_some() && driver.unopened()? {
            driver.opening_deadline = Some(clock::instant() + driver.opening_timeout);
        }
        return Ok(Some(driver));
    };
    let relaunch = relaunch && !crate::provider_transition::holds_execution(&*domain,&task);
    if relaunch && plugin.is_none() {
        return Err(Failed::new("the host ended and Alondra's node plugin, which its replacement loads, was not found".into()));
    }
    let identity=NativeAccountIdentity::recover(worker.state_directory(),worker.provider())?;
    if let Some(identity)=&identity{identity.keep(worker.state_directory())?;}
    for event in journal.events() {
        let batch = OrderedObservation { owner_generation, execution: stamp.clone(), cursor: event.cursor, evidence: event.payload.clone() };
        let binding = grant.binding().clone();
        let (normalizer, home) = (worker.normalizer(), domain.home());
        commit_observation(&*domain, &scope, &binding, owner_generation, &batch, normalizer, home,identity.as_ref())
            .map_err(failed(&format!("committing what the ended host observed at cursor {}", batch.cursor)))?;
        domain.forget_sealed_observations(&task);
        feed(worker.provider(), &*domain, &agent, &batch);
    }
    let retained = |command: &[u8]| -> Result<DispatchReceipt, Failed> {
        Ok(match journal.retained(command).map_err(adapter("reading the ended host's receipt"))? {
            Retained::Offered => DispatchReceipt::Handed,
            Retained::Refused | Retained::Absent => DispatchReceipt::Rejected,
            Retained::AdmittedOnly => DispatchReceipt::Uncertain,
        })
    };
    for command in adoption.commands {
        let receipt = retained(&command.command)?;
        domain.own(scope.clone(), |owner| owner.resolve(&stamp, command.intent_id, owner_generation, receipt)).map_err(failed("resolving a dispatch"))?;
    }
    for control in adoption.controls {
        let receipt = retained(&control.command)?;
        domain.own(scope.clone(), |owner| owner.resolve_control(&stamp, control.id, owner_generation, receipt)).map_err(failed("resolving a control"))?;
    }
    let generation = stamp.generation();
    let retaken = {
        let read = domain.read();
        relaunch && read.state().moving_to(&task).is_none() && read.state().lifecycle_of(&task).is_ok_and(|lifecycle| lifecycle.host_released())
    };
    let ended_work = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?.0
        .checkpoint().map(|checkpoint| worker.background_left(checkpoint.bytes())).unwrap_or_default();
    domain.own(scope.clone(), |owner| {
        // A claim whose preparation failed has no command the ended host could have received.
        // Keep these claims untouched when adopting a live host, whose requests still matter.
        for id in adoption.unrecorded_controls {
            owner.resolve_control(&stamp, id, owner_generation, DispatchReceipt::Rejected)?;
        }
        for id in adoption.unrecorded_commands {
            owner.resolve(&stamp, id, owner_generation, DispatchReceipt::Rejected)?;
        }
        let envelope = |name: String, command: Command| CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("host-ended:{task}:{generation}:{owner_generation}:{name}").as_str().into(),
            expected_revision: 0,
            command,
        };
        let ended = if retaken {
            Command::RecordHostRetaken { task_id: task.clone(), generation }
        } else {
            Command::RecordHostEnded { task_id: task.clone(), generation, resumes: relaunch, ended_work: ended_work.clone(), by_alondra }
        };
        owner.stage_command(&Actor::System, &envelope("ended".into(), ended), Attached::None)?;
        let doubted: Vec<InputId> = owner.lifecycle(&task)?.doubts().into_iter().map(|(_, input)| input.clone()).collect();
        for (input, status) in owner.pending_handoffs(&task)? {
            let (id, text) = (input.id().as_str().to_owned(), input.text().to_owned());
            let command = match status.as_str() {
                "rejected" => Command::ResendUntakenInput { task_id: task.clone(), generation, input: id.clone(), text },
                _ if doubted.contains(input.id()) => continue,
                _ => Command::RecordDoubtfulHandoff { task_id: task.clone(), generation, input: id.clone(), text, resumes: relaunch, by_alondra },
            };
            owner.stage_command(&Actor::System, &envelope(id, command), Attached::None)?;
        }
        Ok(())
    })
    .map_err(failed("recording the end of the host"))?;
    if !relaunch{*claimed=worker.take_ended();return Ok(None)}
    let (adapter_state, _) = domain.own(scope.clone(), |owner| owner.adapter_state(&grant)).map_err(failed("reading the provider state"))?;
    let refused=adapter_state.checkpoint().and_then(|checkpoint|alondra_provider_codex::blocked_policy(checkpoint.bytes()))
        .or_else(||journal.events().iter().find_map(|event|alondra_provider_codex::account_refusal(&OrderedObservation{owner_generation,execution:stamp.clone(),cursor:event.cursor,evidence:event.payload.clone()})).map(str::to_owned));
    if !explicit_account_retry && let Some(reason)=refused{return Err(Failed::new(reason))}
    let base = adapter_state.checkpoint().map(|checkpoint| checkpoint.cursor()).unwrap_or(0);
    let resume = domain.read().state().lifecycle_of(&task).ok()
        .is_some_and(|lifecycle| lifecycle.resource() == Some(alondra_domain::ResourceEvidence::Available));
    let capability = domain.bind_agent(&agent.0).map_err(failed("issuing the node's capability"))?;
    let unadmitted = Unadmitted { domain: Some(Arc::clone(&domain)), agent: agent.clone() };
    let environment = node_environment(domain.home(), &agent, capability.expose_for_child_environment());
    domain.own(scope.clone(),|owner|owner.require_native_identity(grant.binding(),identity.as_ref().map(|identity| identity as &dyn crate::domain_store::KnownIdentity))).map_err(failed("Alondra cannot resume this session"))?;
    worker.relaunch(grant, adapter_state, base, resume, retaken, environment).map_err(|error| Failed::new(format!("its new host could not start ({error})")))?;
    domain.engine().context_launched(&task, if resume { StartKind::Resumed } else { StartKind::New });
    if retaken && adoption.receipt_candidates.is_empty() && let Some(archive) = worker.retired_host() && let Err(error) = std::fs::remove_dir_all(&archive) {
        log!("could not remove the folder of the released host of {task} ({}): {error}", archive.display());
    }
    // Continued hosts open the native session lazily, on the first input after these files are synced.
    if let Err(failed)=options.save(worker.state_directory()).and_then(|()|identity.as_ref().map_or(Ok(()),|identity|identity.retain(worker.state_directory()))){
        let directory=worker.state_directory().to_path_buf();
        drop(worker);
        return Err(end_launched(&*domain, &directory,failed));
    }
    unadmitted.admitted();
    let mut driver = Driver::new(domain, worker, scope, task, agent);
    driver.conversation = Some(session_refresh::Conversation { native_environment: on_account.native_environment.clone(), directory: adoption.directory.clone() });
    driver.new_session = !resume;
    driver.replacement = true;
    driver.resuming_since = retaken.then_some(taken_at);
    driver.resumes_released = retaken;
    driver.log_host(if resume { "started anew; its Claude resumes the same session on its next input" } else { "started anew; its Claude starts the session on its next input" });
    driver.sealed();
    Ok(Some(driver))
}

/// At startup, takes over every task that was running through the SDK and is not terminal: a live
/// host is adopted with the same session, and one that ended or refuses its adoption is replaced by
/// a host that resumes it.
pub fn adopt_all(domain: &Arc<dyn Runtime>, backend: &SdkBackend) {
    end_hosts_of_ended_tasks(&**domain, backend);
    for (task, taken) in take_all(domain, backend) {
        match taken {
            Ok(driver) => {
                domain.add_sdk_owner(Owner::spawn(driver));
                domain.sdk_owners().host_losses_told.lock_or_recover("the tasks told of a lost host").remove(&task);
                close_host_lost_card(&**domain, &task);
                log!("the SDK execution of {task} was taken over: the same Claude session goes on");
            }
            Err(failed) => not_taken(&**domain, &task, failed),
        }
    }
}

/// A node's host that still runs in a folder no task that goes on claims (its task ended, or
/// moved to another node) has no owner, and no daemon would ever take it over: it is ended by the
/// lock it holds.
fn end_hosts_of_ended_tasks(domain: &dyn Runtime, backend: &SdkBackend) {
    let state = domain.state();
    let Ok(hosts) = alondra_engine_ports::agent_host::of(domain.ports()) else { return };
    let claimed: BTreeSet<&str> = state.tasks.iter()
        .filter(|(id, _)| !state.lifecycle_of(id).is_ok_and(|lifecycle| matches!(lifecycle.phase(), alondra_domain::TaskPhase::Completed | alondra_domain::TaskPhase::Cancelled)))
        .filter_map(|(_, task)| task.agent_id.as_ref().map(|agent| agent.0.as_str()))
        .collect();
    let roots: BTreeSet<PathBuf> = [Provider::Claude, Provider::Codex].into_iter()
        .filter_map(|provider| backend.for_provider(provider, domain.home()).ok().map(|selected| selected.state_root.clone()))
        .collect();
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else { continue };
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name();
            let Some(node) = name.to_str().filter(|name| name.starts_with("sdk-") && !name.contains('.')) else { continue };
            if claimed.contains(node) || domain.owns_sdk_host(&AgentId(node.to_owned())) { continue }
            let directory = entry.path();
            if !matches!(hosts.claim(&directory), Ok(alondra_engine_ports::agent_host::Retained::Held)) { continue }
            mark_stopped_by_alondra(&directory);
            match hosts.end_holders(&directory) {
                Ok(ended) => {
                    log!("the SDK host of {node}, a node no task goes on with, still ran with no owner: it was stopped");
                    drop(ended);
                    crate::host_folders::host_ended(domain, &directory, &|agent| domain.owns_sdk_host(agent));
                }
                Err(error) => log!("the SDK host of {node}, a node no task goes on with, still runs with no owner and did not stop: {error}"),
            }
        }
    }
}

/// An execution whose takeover found the disk full waits for space: nothing about the task
/// failed. Any other failure is lost, with its card.
fn not_taken(domain: &dyn Runtime, task: &TaskId, failed: Failed) {
    if !failed.no_space {
        return lost(domain, task, &failed.why);
    }
    log!("the SDK execution of {task} waits for disk space to be taken over: {failed}");
    domain.sdk_owners().waiting_for_space.lock_or_recover("the tasks waiting for space").insert(task.clone());
}

/// The machine watch measured space on the disk: each execution whose takeover found it full is
/// taken over again, unless it ended or another owner took it meanwhile.
pub fn retake_waiting_for_space(domain: &dyn Runtime) {
    let waiting = std::mem::take(&mut *domain.sdk_owners().waiting_for_space.lock_or_recover("the tasks waiting for space"));
    if waiting.is_empty() {
        return;
    }
    let state = domain.state();
    for task in waiting.into_iter().filter(|task| unowned(domain, &state, task)) {
        log!("there is disk space again: the SDK execution of {task} is taken over");
        retake(domain, task);
    }
}

/// A host runs the task again, so the card of the outage that held it is answered as the user's
/// «Try again» would: the task goes on with its pending input instead of waiting for a click.
fn close_host_lost_card(domain: &dyn Runtime, task: &TaskId) {
    let state = domain.state();
    let open = state.interactions.values().find(|card| {
        !card.withdrawn && retakes(card)
            && matches!(&card.payload, InteractionPayload::Question { answer: None, .. })
            && card.scope == (Scope::Task { task_id: task.clone() })
    });
    let Some(card) = open else { return };
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("host-found-{task}-{}", crate::auth::random_hex(8)).into(),
        expected_revision: 0,
        command: Command::AnswerQuestion { interaction_id: card.id.clone(), option_id: RETAKE.into(), inputs: BTreeMap::new(), attachments: vec![] },
    };
    if let Err(error) = domain.execute_as_system(envelope) {
        log!("the host-lost card of {task} stays open: {}", store_text(&error));
    }
}

/// The task's execution could not be taken over and no host runs it: its card tells the user why
/// and offers to try again or cancel the task. It is never left as it is. A task the gate holds
/// paused gets no card: its resume takes it over (`take_if_unowned`), and puts the card if that
/// fails too. The user hears of an outage once: until a takeover works, another failure leaves its
/// card without a push.
pub fn lost(domain: &dyn Runtime, task: &TaskId, why: &str) {
    log!("the SDK execution of {task} was not taken over: {why}");
    end_dead_host_claims(domain, task);
    let state = domain.state();
    if let Some(agent) = state.tasks.get(task).and_then(|task| task.agent_id.as_ref())
        && !domain.owns_sdk_host(agent) {
        host_ended(domain, agent);
    }
    if node_paused(&state, task) {
        log!("{task} is paused: a host that resumes its session takes it over when the pause ends");
        return;
    }
    let quiet = !domain.sdk_owners().host_losses_told.lock_or_recover("the tasks told of a lost host").insert(task.clone());
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: format!("host-lost-{task}-{}", crate::auth::random_hex(8)).into(),
        expected_revision: 0,
        command: Command::RecordHostLost { task_id: task.clone(), reason: why.to_owned(), quiet },
    };
    if let Err(error) = domain.execute_as_system(envelope) {
        log!("could not put the card of {task}, whose execution was not taken over: {}", store_text(&error));
    }
}

/// Whether the task's current execution still has something claimed for a host that never
/// answered for it.
pub(crate) fn claims_left(domain: &dyn Runtime, task: &TaskId) -> Result<bool, Failed> {
    let state = domain.state();
    let scope = AuthorityScope::new(state.data_epoch.clone()).map_err(|error| Failed::new(format!("{error:?}")))?;
    domain.own(scope, |owner| owner.claims_left(task)).map(|(left, _)| left).map_err(failed("reading the task's claims"))
}

/// A host given up as lost answers for nothing it claimed: its end is committed from its journal,
/// so what it never got goes back to the queue and what it may have got is resent or asked about,
/// and no claim is left holding the task's next execution.
fn end_dead_host_claims(domain: &dyn Runtime, task: &TaskId) {
    let Some((backend, runtime)) = domain.sdk() else { return };
    match claims_left(domain, task) {
        Ok(true) => {}
        Ok(false) => return,
        Err(failed) => return log!("could not read what the lost host of {task} claimed: {failed}"),
    }
    if !host_started(backend, domain, task) {
        return;
    }
    let backend = backend.clone();
    match end_unowned_host(runtime, &backend, task.clone()) {
        Ok(()) => log!("the lost host of {task} ended: what it claimed was settled from its journal"),
        Err(failed) => log!("the lost host of {task} left claims that could not be settled: {failed}"),
    }
}

fn node_paused(state: &DomainState, task: &TaskId) -> bool {
    state.lifecycle_of(task).ok().is_some_and(|lifecycle| lifecycle.cards().into_iter()
        .any(|(card, reason)| reason == alondra_domain::WaitReason::Pause && state.node_pause_card(card)))
}

/// The gate ended the pause of a task whose execution no owner of this daemon holds, as one whose
/// host was lost while it was paused: a host that resumes the same session takes it over and is
/// handed what the task goes on with.
pub fn take_if_unowned(domain: &dyn Runtime, task: &TaskId) {
    let state = domain.state();
    if to_take(domain, &state, task) {
        log!("{task} was resumed with no SDK host of its own: one that resumes its session takes it over");
        retake(domain, task.clone());
    }
}

/// How long the gate waits before it tries again to take over a task it found working unowned.
const UNOWNED_TAKE_EVERY: alondra_core::Millis = 60_000;

/// A task whose lifecycle reads anything but rest (a turn or work of its own at work, a turn to
/// start, a notification to take up) while no owner of this daemon holds its execution: its host
/// ended and nobody told the domain, as when an owner stops on a broken connection. It is taken
/// over, which records the host's end, so a task never reads as running or starting with no node;
/// one held by a pause waits for it, and is told to go on when the pause ends. A task the user
/// holds on a pause, as when its host was lost, waits for their answer, and one a pause holds
/// after it was taken over is taken over again when that pause ends.
pub fn take_working_unowned(domain: &dyn Runtime, state: &DomainState, now: alondra_core::Millis) {
    let working: Vec<(TaskId, u64)> = reads_as_working(state).into_iter()
        .filter(|task| to_take(domain, state, task))
        .filter_map(|task| Some((task.clone(), state.lifecycle_of(&task).ok()?.revision())))
        .collect();
    let due = due_unowned(&mut domain.sdk_owners().unowned_takes.lock_or_recover("the tasks taken over unowned"), working, now);
    for task in due {
        log!("{task} reads as working with no SDK host of its own: one that resumes its session takes it over");
        retake(domain, task);
    }
}

/// Every host this daemon owns whose node Alondra already recorded as closed or gone is closed:
/// a closed task never keeps a host, whatever started it after its close. A node the user kept is
/// theirs and stays.
pub fn close_hosts_of_ended_nodes(domain: &dyn Runtime, state: &DomainState) {
    for (task, agent) in ended_nodes_with_hosts(domain, state) {
        let Some((_, runtime)) = domain.sdk() else { return };
        let mut retakes = domain.sdk_owners().retakes.lock_or_recover("the retakes");
        retakes.retain(|(_, retake)| !retake.is_finished());
        if retakes.iter().any(|(taking, _)| *taking == task) {
            continue;
        }
        log!("the node {agent} ended with its task, but its SDK host still runs: it is closed");
        retakes.push((task, Retake::spawn(runtime, move |runtime| {
            if let Err(failed) = runtime.close_sdk_host(&agent) {
                log!("the SDK host of the ended node {agent} did not close: {failed}");
            }
        })));
    }
}

/// The nodes whose host this daemon still owns although Alondra recorded them closed or gone.
fn ended_nodes_with_hosts(domain: &dyn Runtime, state: &DomainState) -> Vec<(TaskId, AgentId)> {
    ended_node_tasks(state).into_iter()
        .filter_map(|task| state.tasks[&task].agent_id.clone().map(|agent| (task, agent)))
        .filter(|(_, agent)| domain.owns_sdk_host(agent))
        .collect()
}

/// The tasks whose node Alondra recorded as closed or gone: no host of theirs may still run.
pub(crate) fn ended_node_tasks(state: &DomainState) -> Vec<TaskId> {
    state.tasks.values()
        .filter(|task| matches!(task.wrap_up.node, Some(NodeEnd::Closed | NodeEnd::Gone)))
        .map(|task| task.id.clone())
        .collect()
}

/// The open tasks that read as working, starting or woken, but for one a pause holds on the user,
/// or after a takeover: those a host of their own must run.
pub(crate) fn reads_as_working(state: &DomainState) -> Vec<TaskId> {
    state.tasks.values().filter(|task| !task.over() && !state.node_closing(task))
        .filter(|task| state.lifecycle_of(&task.id).is_ok_and(|lifecycle| matches!(lifecycle.activity(),
            ActivityView::Working | ActivityView::OwnWork | ActivityView::Starting | ActivityView::Continuation)
            && !lifecycle.cards().into_iter().any(|(card, reason)| reason == alondra_domain::WaitReason::Pause
                && (lifecycle.card_owner(card) == Some(alondra_domain::Owner::User) || lifecycle.takeover() != alondra_domain::Takeover::None))))
        .map(|task| task.id.clone()).collect()
}

/// Whether an adopted host's process is gone. A host whose process this daemon could not learn is
/// taken as running.
fn adopted_without_process(host: Option<HostStop>) -> std::io::Result<bool> {
    if ADOPTED_PROCESS_GONE.with(|gone| gone.replace(false)) {
        return Ok(true);
    }
    host.map_or(Ok(false), |host| host.gone())
}
thread_local! {
    /// The next host this thread adopts reads as having no process, as a host whose process died
    /// while its endpoint still answered.
    pub static ADOPTED_PROCESS_GONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// The tasks of `working` to take over now: each at most every `UNOWNED_TAKE_EVERY`, and never
/// again while its lifecycle stands where the last take over left it, since a take over that
/// changed nothing would change nothing again.
pub(crate) fn due_unowned(taken: &mut std::collections::BTreeMap<TaskId, (alondra_core::Millis, u64)>, working: Vec<(TaskId, u64)>, now: alondra_core::Millis) -> Vec<TaskId> {
    taken.retain(|task, _| working.iter().any(|(working, _)| working == task));
    let due: Vec<(TaskId, u64)> = working.into_iter()
        .filter(|(task, revision)| taken.get(task).is_none_or(|(at, seen)| seen != revision && now - at >= UNOWNED_TAKE_EVERY))
        .collect();
    for (task, revision) in &due {
        taken.insert(task.clone(), (now, *revision));
    }
    due.into_iter().map(|(task, _)| task).collect()
}

/// A control reached a task whose host closed while its node rested: only a host can answer it.
pub(crate) fn wake_released(domain: &dyn Runtime, task: &TaskId) {
    let state = domain.state();
    let released = state.lifecycle_of(task).is_ok_and(|lifecycle| lifecycle.host_released());
    if released && unowned(domain, &state, task) {
        log!("a control reached {task}, whose host closed while it rested: one that resumes its session takes it over");
        retake(domain, task.clone());
    }
}

/// The user chose «Try again» on the card of a task whose execution was lost: it is taken over
/// again, on a thread of its own, as at the daemon's start; another failure puts a new card.
pub fn retake(domain:&dyn Runtime,task:TaskId){retake_with_policy(domain,task,false)}
pub(crate) fn retake_manually(domain:&dyn Runtime,task:TaskId){retake_with_policy(domain,task,true)}
fn retake_with_policy(domain: &dyn Runtime, task: TaskId,explicit_account_retry:bool) {
    if crate::provider_transition::holds_execution(domain,&task){return}

    let Some((backend, runtime)) = domain.sdk() else {
        return lost(domain, &task, NO_HOST);
    };
    let backend = backend.clone();
    let mut retakes = domain.sdk_owners().retakes.lock_or_recover("the retakes");
    if domain.sdk_owners().stopping_owners.load(Ordering::SeqCst) {
        return log!("{task} is not taken over: every SDK host of this daemon is stopping");
    }
    retakes.retain(|(_, retake)| !retake.is_finished());
    if retakes.iter().any(|(taking, _)| *taking == task) {
        return;
    }
    let refusal = crate::domain_store::carried_refusal();
    retakes.push((task.clone(), Retake::spawn(runtime, move |runtime| {
        refusal();
        take_again(runtime, &backend, &task, explicit_account_retry);
    })));
}

/// Takes the execution of `task` over with a new host, and records it. A takeover whose host never
/// answered the last one changes nothing: the domain refuses it, the last takeover is recorded as
/// unanswered, and a fresh host takes the same input the execution waits for, any host still
/// running ended first.
fn take_again(runtime: &Arc<dyn Runtime>, backend: &SdkBackend, task: &TaskId, explicit_account_retry: bool) {
    let _turn = TakeoverTurn::wait(&**runtime, task);
    if owned_now(&**runtime, task) {
        return;
    }
    let record = |fact: RuntimeObservation| -> Result<(), StoreError> {
        let state = runtime.state();
        let generation = state.lifecycle_of(task).map_err(StoreError::Domain)?.execution().map(|stamp| stamp.generation())
            .ok_or_else(|| StoreError::Domain(DomainError::new("conflict", format!("Task {task} has no execution to take over"))))?;
        runtime.execute_as_system(CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("takeover-{task}-{}", crate::auth::random_hex(8)).into(),
            expected_revision: 0,
            command: Command::RecordRuntimeFacts { task_id: task.clone(), generation, facts: vec![fact] },
        }).map(drop)
    };
    let earlier = runtime.state().lifecycle_of(task).ok().map(|lifecycle| lifecycle.takeover());
    let fresh = match record(RuntimeObservation::TakenOver) {
        Ok(()) => earlier == Some(alondra_domain::Takeover::Unanswered),
        Err(StoreError::Domain(error)) if error.code == REPEATED_TAKEOVER => {
            log!("the host that last took {task} over showed nothing: a fresh host takes what it waits for");
            if let Err(error) = record(RuntimeObservation::TakeoverUnanswered).and_then(|()| record(RuntimeObservation::TakenOver)) {
                log!("could not record the unanswered takeover of {task}: {}", store_text(&error));
            }
            true
        }
        Err(StoreError::Domain(error)) if error.code == CLOSING => {
            return log!("{task} is closing, so no host takes it over: {}", error.message);
        }
        Err(error) => {
            log!("the takeover of {task} was not recorded: {}", store_text(&error));
            false
        }
    };
    if fresh && host_started(backend, &**runtime, task) {
        let directory = {
            let read = runtime.read();
            let instance = read.state().lifecycle_of(task).ok().and_then(|lifecycle| lifecycle.execution().map(|stamp| stamp.instance().as_str().to_owned()));
            let provider = read.state().tasks.get(task).map(|task| task.provider).unwrap_or_default();
            instance.zip(backend.for_provider(provider, runtime.home()).ok()).map(|(instance, backend)| backend.state_root.join(instance))
        };
        if let Some(directory) = directory {
            mark_stopped_by_alondra(&directory);
            if let Err(error) = alondra_engine_ports::agent_host::of(runtime.ports()).and_then(|hosts| hosts.end_holders(&directory)) {
                return not_taken(&**runtime, task, Failed::new(format!("its host, which never took what it was handed, did not stop: {error}")));
            }
        }
    }
    let plugin = runtime.ports().plugins().node(runtime.home());
    let taken = if host_started(backend, &**runtime, task) {
        if explicit_account_retry{take_host(Arc::clone(runtime),backend,task.clone(),plugin.as_deref(),true,true,true).and_then(|driver|driver.ok_or_else(||Failed::new("The ended host was not replaced".into())))}else{adopt(Arc::clone(runtime), backend, task.clone(), plugin.as_deref())}
    } else {
        plugin.as_deref().ok_or_else(|| Failed::new("Alondra's node plugin, which its host loads, was not found".into()))
            .and_then(|plugin| launch_admitted(Arc::clone(runtime), backend, task.clone(), plugin))
    };
    match taken {
        Ok(driver) => {
            runtime.add_sdk_owner(Owner::spawn(driver));
            runtime.sdk_owners().host_losses_told.lock_or_recover("the tasks told of a lost host").remove(task);
            close_host_lost_card(&**runtime, task);
            log!("the SDK execution of {task} was taken over again: the same Claude session goes on");
        }
        Err(failed) => not_taken(&**runtime, task, failed),
    }
}

/// Stops, on a thread of its own, the host of the paused node `agent` and every process under it,
/// and puts in its place a host whose Claude opens only on its next input, which the pause holds:
/// with no Claude running nothing wakes a turn, and the end of the host settles what the task's
/// lifecycle waited for. A task an owner of this daemon does not hold has nothing to stop.
pub fn halt(domain: &dyn Runtime, task: TaskId, agent: AgentId) {
    if !domain.owns_sdk_host(&agent) || crate::provider_transition::holds_execution(domain, &task) || crate::account_move::holds_execution(domain, &task) {
        return;
    }
    let Some((backend, runtime)) = domain.sdk() else { return };
    let backend = backend.clone();
    let mut retakes = domain.sdk_owners().retakes.lock_or_recover("the retakes");
    retakes.retain(|(_, retake)| !retake.is_finished());
    if retakes.iter().any(|(taking, _)| *taking == task) {
        return;
    }
    retakes.push((task.clone(), Retake::spawn(runtime, move |runtime| {
        match runtime.halt_sdk_host(&agent) {
            None => return,
            Some(Ok(())) => log!("the SDK host of {task} and every process under it stopped for its pause"),
            Some(Err(failed)) => log!("the SDK host of {task} did not stop cleanly for its pause: {failed}"),
        }
        take_again(runtime, &backend, &task, false);
    })));
}

/// A takeover, a halt or a close of a host, on a thread of its own. It reads as finished once its
/// work is done and before it wakes the waiters on the domain: one that looks again then never
/// finds it under way only because its thread has not returned yet, with nothing left to wake it.
pub struct Retake {
    done: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

impl Retake {
    fn spawn(runtime: Arc<dyn Runtime>, work: impl FnOnce(&Arc<dyn Runtime>) + Send + 'static) -> Self {
        let done = Arc::new(AtomicBool::new(false));
        let finished = Arc::clone(&done);
        let thread = alondra_core::clock::spawn_thread(move || {
            work(&runtime);
            finished.store(true, Ordering::SeqCst);
            runtime.wake_subscribers();
            runtime.sdk_owners().retakes_held_after_waking.wait();
        });
        Self { done, thread }
    }

    pub fn is_finished(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    pub fn join(self) -> std::thread::Result<()> {
        self.thread.join()
    }
}

/// Whether a retake of `task`, or the halt that replaces its host for a pause, is under way.
pub(crate) fn being_retaken(domain: &dyn Runtime, task: &TaskId) -> bool {
    domain.sdk_owners().retakes.lock_or_recover("the retakes").iter().any(|(taking, retake)| taking == task && !retake.is_finished())
}

/// Whether the task runs through the SDK, has not ended, and no owner of this daemon, launch,
/// takeover or retake under way, or move to another account holds its execution.
fn to_take(domain: &dyn Runtime, state: &DomainState, task: &TaskId) -> bool {
    unowned(domain, state, task) && !closing_without_folder(domain, state, task) && state.lifecycle_of(task).ok().is_none_or(|lifecycle| !lifecycle.host_released()
        || !lifecycle.pending_inputs().is_empty() && !domain.gate().waits_for_slot(state, task))
}

/// A task whose node is closing and whose folder is gone: no host can start there, so none is
/// given to it; the closer ends it.
pub(crate) fn closing_without_folder(domain: &dyn Runtime, state: &DomainState, task: &TaskId) -> bool {
    state.tasks.get(task).is_some_and(|task| state.in_cleanup(task) || state.node_closing(task))
        && domain.task_worktree(task).ok().flatten().is_some_and(|dir| !std::path::Path::new(&dir).exists())
}

fn unowned(domain: &dyn Runtime, state: &DomainState, task: &TaskId) -> bool {
    state.tasks.get(task).and_then(|task| task.agent_id.as_ref()).is_some_and(|agent| !domain.owns_sdk_host(agent))
        && !domain.sdk_owners().in_hand.lock_or_recover("the executions in hand").contains_key(task)
        && !domain.sdk_owners().retakes.lock_or_recover("the retakes").iter().any(|(taking, retake)| taking == task && !retake.is_finished())
        && !crate::account_move::holds_execution(domain, task)
        && !crate::provider_transition::holds_execution(domain, task)
        && takeable(state, task)
}

/// Whether a host may take `task`'s execution over: it runs through the Agent SDK, is still open,
/// and its node does not only close now.
pub(crate) fn takeable(state: &DomainState, task: &TaskId) -> bool {
    runs_on_sdk_host(state, task) && state.tasks.get(task).is_some_and(|task| !state.node_closing(task))
}

/// Whether `task`'s current execution is one of the Agent SDK host and still open.
pub(crate) fn runs_on_sdk_host(state: &DomainState, task: &TaskId) -> bool {
    state.lifecycle_of(task).ok().is_some_and(|lifecycle| {
        lifecycle.execution().is_some_and(|stamp| stamp.host().as_str() == HOST)
            && !matches!(lifecycle.phase(), alondra_domain::TaskPhase::Completed | alondra_domain::TaskPhase::Cancelled)
    })
}

/// Takes each execution on a thread of its own, so a host that is slow to stop holds back only
/// its own task.
pub fn take_all(domain: &Arc<dyn Runtime>, backend: &SdkBackend) -> Vec<(TaskId, Result<Driver, Failed>)> {
    let state = domain.state();
    let tasks: Vec<TaskId> = state.tasks.keys().filter(|task| to_take(&**domain, &state, task)).cloned().collect();
    let plugin = domain.ports().plugins().node(domain.home());
    alondra_core::clock::scope(|scope| {
        let taking: Vec<_> = tasks
            .into_iter()
            .map(|task| {
                let plugin = plugin.as_deref();
                let taking = task.clone();
                let thread = scope.spawn(move || {
                    if host_started(backend, &**domain, &taking) {
                        adopt(Arc::clone(domain), backend, taking, plugin)
                    } else {
                        plugin.ok_or_else(|| Failed::new("Alondra's node plugin, which its host loads, was not found".into()))
                            .and_then(|plugin| launch_admitted(Arc::clone(domain), backend, taking, plugin))
                    }
                });
                (task, thread)
            })
            .collect();
        taking.into_iter().map(|(task, thread)| (task, thread.join().unwrap_or_else(|_| Err(Failed::new("taking it over panicked".into()))))).collect()
    })
}

/// The host's folder is `state_root/<instance>`, so the execution's own identity names it and a
/// daemon that comes back finds it without searching. A launch creates it; an adoption reuses it.
fn configuration(domain: &dyn Runtime, backend: &SdkBackend, binding: &ProviderBinding, directory: &str, instance: &str, create: bool, worker: Option<WorkerProfile>) -> Result<ClaudeConfiguration, Failed> {
    let state_directory = backend.state_root.join(instance);
    if create {
        std::fs::create_dir_all(&backend.state_root).map_err(|error| Failed::written(format!("creating the SDK state root: {error}"), &error))?;
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&state_directory)
            .map_err(|error| Failed::written(format!("creating the host's folder: {error}"), &error))?;
    }
    let mut native_environment = backend.native_environment.clone();
    if let Some(provider) = Provider::from_driver(binding.driver()) {
        native_environment.insert("ALONDRA_PROVIDER".into(), provider.as_str().into());
        if backend.installed_native && let (Some(alondra_home), Some(user_home)) = (backend.state_root.parent(), backend.native_environment.get("HOME")) {
            let daemon = |name: &str| std::env::var(name).ok();
            native_environment.extend(node_environment::granted(alondra_home, std::path::Path::new(user_home), std::path::Path::new(directory), provider, &daemon));
        }
    }
    if binding.driver() == "codex-app-server" {
        if let Some(plugin) = worker.as_ref().and_then(|profile| profile.plugins.first()) {
            native_environment.insert("ALONDRA_NODE_PLUGIN".into(), plugin.display().to_string());
        }
    }
    Ok(ClaudeConfiguration {
        artifact: VerifiedArtifact::verify(&backend.node, &backend.host, &backend.node_sha256, &backend.host_sha256)
            .map_err(|error| Failed::new(format!("verifying the SDK host: {error}")))?,
        state_directory,
        host_environment: backend.host_environment.clone(),
        native_executable: backend.native.clone(),
        native_directory: directory.into(),
        native_environment,
        profile_id: "alondra".into(),
        account_id: binding.credential().into(),
        host_release: backend.release.clone(),
        io_timeout: Duration::from_secs(backend.io_timeout_seconds),
        confinement: backend.confinement.clone(),
        hosts: alondra_engine_ports::agent_host::of(domain.ports()).map_err(|error| Failed::new(format!("starting the SDK host: {error}")))?,
        worker,
    })
}

/// Alondra's node plugin and launch settings, which a task's Claude loads on top of the user's own.
/// Its Claude runs in the default permission mode, so the settings allow editing inside the node's
/// own worktree: an edit there never puts a permission card to the user.
fn worker_profile(plugin: &std::path::Path, options: &LaunchOptions, worktree: &str) -> Result<WorkerProfile, Failed> {
    if options.provider == Provider::Codex {
        if !options.disallowed_tools.is_empty() { return Err(Failed::new("The selected Codex runtime cannot enforce this task's per-tool deny list; choose a supported task tool policy before launching".into())); }
        let instructions = std::fs::read_to_string(plugin.join("skills/alondra/SKILL.md")).map_err(|_| Failed::new("Alondra's Codex node mission is missing; run alondra doctor --fix before starting this task".into()))?;
        if instructions.trim().is_empty() || instructions.len() > 128 * 1024 { return Err(Failed::new("Alondra's Codex node mission is empty or too large; run alondra doctor --fix".into())); }
        let codex = serde_json::json!({
            "developerInstructions": instructions,
            "mcp_servers": {"alondra": {"command": plugin.join("bin/alondra"), "args": ["mcp-node"], "tool_timeout_sec": 3900}},
            "nativeAccountPolicy": {"paidAccountId": options.selection.as_ref().and_then(|selection|selection.paid_account_grant.as_ref().filter(|grant|grant.provider==Provider::Codex && selection.account.as_ref()==Some(*grant))).map(|grant|grant.account.as_str())},
            "reasoning_effort": options.effort,
            "initial_mode": if options.selection.as_ref().is_some_and(|selection| selection.profile == alondra_core::model_selection::ModelProfile::Design) { "plan" } else { "default" },
        });
        return Ok(WorkerProfile { plugins: vec![plugin.to_path_buf()], settings: serde_json::Map::from_iter([("codex".into(), codex)]), model: options.model.clone(), effort: None, disallowed_tools: Vec::new(), handoff_context: None });
    }
    let permissions = serde_json::json!({"allow": own_worktree_edits(std::path::Path::new(worktree))});
    Ok(WorkerProfile {
        plugins: vec![plugin.to_path_buf()],
        settings: crate::plugins::launch_settings_with(Some(permissions)).as_object().cloned().unwrap_or_default(),
        model: options.model.clone(),
        effort: options.effort(),
        disallowed_tools: options.disallowed_tools.clone(),
        handoff_context: None,
    })
}

/// Claude Code's `Edit` rules cover Edit, Write and NotebookEdit; `//` anchors a rule at the root
/// of the disk. The worktree is named as given and as its real path, when they differ. A path a
/// rule cannot spell literally gets no rule, and edits there ask as before.
pub(crate) fn own_worktree_edits(worktree: &std::path::Path) -> Vec<String> {
    let mut paths = vec![worktree.to_path_buf()];
    if let Ok(real) = worktree.canonicalize() {
        if real != worktree {
            paths.push(real);
        }
    }
    paths
        .iter()
        .filter_map(|path| path.to_str())
        .filter(|path| path.starts_with('/') && !path.contains(['*', '?', '[', ']', '{', '}', '(', ')', '!', '\\']))
        .map(|path| format!("Edit(/{}/**)", path.trim_end_matches('/')))
        .collect()
}

/// How the task's Claude reaches Alondra through the node plugin's MCP server: as its node, with
/// the capability issued to it, by the daemon's own address. The plugin's hooks stay inert: the
/// host reports what Claude does. Outside a terminal Claude Code offers no task list unless
/// `CLAUDE_CODE_ENABLE_TODO_TOOLS` asks for it; with it the node plans in steps, as it did as a
/// `claude --bg` session, and the task's detail shows them.
///
/// The conversation's prompt cache lasts an hour, also once an account pays for usage beyond its
/// plan, where Claude Code would drop it to five minutes: a node often waits minutes between
/// requests. A subagent's lasts five minutes, which costs 1.25 times the input to write against
/// twice for an hour, and a subagent seldom waits (code.claude.com/docs/en/prompt-caching).
fn node_environment(home: &std::path::Path, agent: &AgentId, capability: &str) -> BTreeMap<String, String> {
    let mut environment = BTreeMap::from([
        ("ALONDRA_HOME".to_owned(), home.display().to_string()),
        ("ALONDRA_SESSION_ID".to_owned(), agent.0.clone()),
        ("ALONDRA_DOMAIN_CAPABILITY".to_owned(), capability.to_owned()),
        ("ALONDRA_DOMAIN_PROTOCOL".to_owned(), "2".to_owned()),
        ("ALONDRA_SDK_HOST".to_owned(), "1".to_owned()),
        ("CLAUDE_CODE_ENABLE_TODO_TOOLS".to_owned(), "1".to_owned()),
        ("CLAUDE_CODE_PROMPT_CACHE_TTL".to_owned(), "1h".to_owned()),
        ("CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL".to_owned(), "5m".to_owned()),
    ]);
    for name in ["ALONDRA_PORT", "ALONDRA_SANDBOX"] {
        if let Ok(value) = std::env::var(name) {
            environment.insert(name.to_owned(), value);
        }
    }
    environment
}

impl Driver {
    /// Keeps what starting this host wrote to the prompt cache, by the task it works for.
    fn record_start(&self, start: &SessionStart) {
        let started = alondra_engine_ports::journal::metrics::SessionStarted {
            task_id: self.task.0.as_str(), session_id: &self.agent.0, kind: start.kind.as_str(), requested_at: start.requested_at,
            since_previous: start.since_previous, tokens: start.tokens, written_1h: start.written_1h, written_5m: start.written_5m, read: start.read,
        };
        log!("the {} host of {} wrote {} tokens to the prompt cache for an hour and {} for five minutes, and read {}, {}",
            start.kind.as_str(), self.task, start.written_1h, start.written_5m, start.read,
            start.since_previous.map_or_else(|| "with no earlier request timed".to_owned(), |since| format!("{} s after the request before it", since / 1000)));
        if let Err(error) = self.domain.record_session_start(&started) {
            log!("could not record what starting the host of {} wrote to the prompt cache: {error:#}", self.task);
        }
    }

    fn new(domain: Arc<dyn Runtime>, worker: Worker, scope: AuthorityScope, task: TaskId, agent: AgentId) -> Self {
        let pause = Mutex::new(PauseSubscription::new(&*domain, &agent));
        Self { pause, in_hand: Some(InHand::new(&domain, &task)), capability: domain.agent_capability(&agent.0), domain, worker, scope, task, agent, permits: VecDeque::new(), controls: VecDeque::new(), awaiting: None, replacement: false, results: vec![], host_closed: false, owner_failed: false, native_account_refusal:None, native_failure: None, identity_kept: false, claude_session: None, moves_to_this_release: false, releases: false, resuming_since: None, resumes_released: false, conversation: None, new_session: false, untaken: false, requeued: false, relaunches: 0, silent_hosts: 0, plugin: None, opening_timeout: OPENING_TIMEOUT, opening_deadline: None, refused: BTreeSet::new(), refuse_offers: 0, fail_next_step: false, panic_next_step: false, handoffs: Arc::default(), before_handoff: None, before_release_ends: None }
    }

    /// The runtime of the daemon that owns this host.
    pub fn domain(&self) -> &Arc<dyn Runtime> {
        &self.domain
    }

    /// Alondra's node plugin, which the host that replaces this one loads.
    pub fn replaced_with(&mut self, plugin: PathBuf) {
        self.plugin = Some(plugin);
    }

    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// The capability of the host that ended, revoked once the driver lets go of its execution, so
    /// a takeover of the task never waits on the revocation.
    fn ended_capability(&mut self) -> EndedCapability {
        EndedCapability { domain: Arc::clone(&self.domain), agent: self.agent.clone(), digest: self.capability.take() }
    }

    /// The driver no longer holds its execution, as one whose owner is gone though its value lives.
    pub fn let_go(&mut self) {
        self.in_hand = None;
    }

    fn log_host(&self, what: &str) {
        let pid = self.worker.host_group().map_or_else(|| "no pid".to_owned(), |pid| format!("pid {pid}"));
        log!("the SDK host of {} {what} ({}, {pid})", self.task, self.agent);
    }

    /// The store issues a permit with every commit that touches the task, and claims only one of
    /// the task's current revision and next offer: the last one issued replaces every one before.
    fn receive_permits(&mut self, permits: Vec<DispatchPermit>) {
        if let Some(newest) = permits.into_iter().last() {
            self.permits.clear();
            self.permits.push_back(newest);
        }
    }

    /// What the store issued for this task since the last look; its permits wait their turn.
    fn sealed(&mut self) -> Committed {
        let mut sealed = self.domain.take_seals(&self.task, &self.agent);
        self.receive_permits(std::mem::take(&mut sealed.permits));
        self.controls.extend(std::mem::take(&mut sealed.control_permits));
        sealed
    }

    /// Claims the control, records its exact command under the owner, hands it to the host with
    /// no SQL held, and records how the host answered. An interrupt whose turn or work already
    /// ended is revoked by its claim and never sent.
    fn control(&mut self, permit: ControlPermit) -> Result<(), Failed> {
        let handoffs = Arc::clone(&self.handoffs);
        let Some(_handing) = handoffs.begin() else {
            self.controls.push_front(permit);
            return Ok(());
        };
        let directory=self.worker.state_directory();
        match self.domain.own(self.scope.clone(), |owner| owner.claim_control_for_identity(permit,directory)) {
            Ok((true, _)) => {}
            Ok((false, _)) => return Ok(()),
            Err(StoreError::Storage(error)) if stale(&error) => return Ok(()),
            Err(error) => return Err(failed("claiming the control")(error)),
        }
        let attempt = self.sealed().control_attempts.pop().ok_or_else(|| Failed::new("the claim issued no control attempt".into()))?;
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let worker = &mut self.worker;
        self.domain
            .own(self.scope.clone(), |owner| owner.record_control(attempt, generation, |attempt| worker.prepare_control(attempt)))
            .map_err(failed("recording the control"))?;
        let control = self.sealed().control_commands.pop().ok_or_else(|| Failed::new("the control left no seal".into()))?;
        let handoff = self.worker.submit_control(&control).unwrap_or(Handoff::Uncertain);
        let awaited = (handoff == Handoff::Offered).then(|| answered_by_event(control.bytes())).flatten();
        let accepted = match (handoff, control.attempt().operation()) {
            (Handoff::Offered, alondra_store::ControlOperation::Control(alondra_domain::Control::Interrupt { turn: Some(turn) })) => {
                Some((control.attempt().execution().clone(), control.attempt().control().clone(), turn.clone()))
            }
            _ => None,
        };
        let epoch = self.scope.epoch().clone();
        let recorded = self.domain
            .own(self.scope.clone(), |owner| {
                owner.record_control_receipt(control, receipt(handoff))?;
                let Some((stamp, id, turn)) = accepted else { return Ok(()) };
                let lifecycle = owner.lifecycle(stamp.task())?;
                if lifecycle.turn() != Some(&turn) || lifecycle.turn_stop() != Some(alondra_domain::TurnStop::Running) {
                    return Ok(());
                }
                let envelope = CommandEnvelope {
                    protocol_version: PROTOCOL_VERSION,
                    data_epoch: epoch,
                    command_id: format!("interrupt-accepted:{}", id.as_str()).as_str().into(),
                    expected_revision: 0,
                    command: Command::RecordInterruptAccepted { task_id: stamp.task().clone(), generation: stamp.generation(), turn: turn.as_str().into() },
                };
                if let Some(refusal) = owner.refusal(&envelope) {
                    log!("the interrupt Claude accepted on {} is left out: {}", stamp.task(), refusal.message);
                    return Ok(());
                }
                owner.stage_command(&Actor::System, &envelope, Attached::None).map(drop)
            });
        match recorded {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => log!("the handoff of a control to the SDK host of {} is no longer the store's to record", self.task),
            Err(error) => return Err(failed("recording the control's handoff")(error)),
        }
        self.sealed();
        self.results.extend(awaited);
        Ok(())
    }

    /// Claims the dispatch, records the exact command under the owner, hands it to the host
    /// with no SQL held, and records how the handoff went.
    fn dispatch(&mut self, permit: DispatchPermit) -> Result<(), Failed> {
        let handoffs = Arc::clone(&self.handoffs);
        let Some(_handing) = handoffs.begin() else {
            self.permits.push_front(permit);
            return Ok(());
        };
        let input = permit.input().clone();
        if !self.new_session && let Some(conversation) = &self.conversation
            && session_refresh::refresh(&*self.domain, &self.task, self.worker.provider(), self.worker.state_directory(), conversation) {
            log!("input {} of {} waits for the new session that takes its conversation over", input.as_str(), self.task);
        }
        let mut retained = Some(permit);
        let identity=NativeAccountIdentity::load(self.worker.state_directory())?;
        let binding=self.worker.binding().clone();
        match self.domain.own(self.scope.clone(), |owner| {
            owner.require_native_identity(&binding,identity.as_ref().map(|identity| identity as &dyn crate::domain_store::KnownIdentity))?;
            let execution=retained.as_ref().unwrap().execution().clone();
            if owner.provider_transition_blocks(&execution)? { return Ok(Some(owner.lifecycle(execution.task())?.execution()==Some(&execution))); }
            owner.claim(retained.take().unwrap())?; Ok(None)
        }) {
            Ok((None, _)) => {}
            Ok((Some(true), _)) => { self.permits.push_front(retained.unwrap()); return Ok(()); }
            Ok((Some(false), _)) => { log!("input {} of {} belongs to an execution that ended: it goes to the next one", input.as_str(), self.task); return Ok(()); }
            Err(StoreError::Storage(error)) if stale(&error) || out_of_turn(&error) => { log!("input {} of {} was not handed to its SDK host ({}): {error}", input.as_str(), self.task, self.agent); return Ok(()) }
            Err(error) => return Err(failed("claiming the dispatch")(error)),
        }
        let attempt = self.sealed().attempts.pop().ok_or_else(|| Failed::new("the claim issued no attempt".into()))?;
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let worker = &mut self.worker;
        let (intent, stamp) = (attempt.intent_id(), attempt.execution().clone());
        match self.domain.own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker)) {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => return self.claim_went_stale(intent, &stamp, generation, &input),
            Err(error) => return Err(failed("recording the command")(error)),
        }
        let command = self.sealed().commands.pop().ok_or_else(|| Failed::new("the command left no seal".into()))?;
        self.domain.engine().context_handed(&self.task, alondra_core::now_millis());
        if let Some(hook) = self.before_handoff.take() {
            hook(&handoffs);
        }
        let handoff = self.submit(&command);
        if handoff == Handoff::Uncertain && self.worker.host_exited() {
            self.host_closed = true;
        }
        match self.domain.own(self.scope.clone(), |owner| owner.record_receipt(command, receipt(handoff))) {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => log!("the handoff of an input to the SDK host of {} is no longer the store's to record", self.task),
            Err(error) => return Err(failed("recording the handoff")(error)),
        }
        log!("input {} of {} went to its SDK host ({}): {handoff:?}", input.as_str(), self.task, self.agent);
        self.sealed();
        if handoff != Handoff::RefusedBeforeOffer && self.unopened()? {
            self.opening_deadline.get_or_insert(clock::instant() + self.opening_timeout);
        }
        if handoff == Handoff::Offered {
            if let Some(since) = self.resuming_since.take() {
                log!("{} resumed its released session: its host took the input {} ms after the work arrived", self.task, alondra_core::clock::since(since).as_millis());
            }
            self.awaiting = Some(input.clone());
            self.new_session = false;
            self.replacement = false;
            self.relaunches = 0;
        } else if self.replacement {
            self.untaken = true;
        }
        if handoff == Handoff::RefusedBeforeOffer {
            return self.offer_refused(&input);
        }
        Ok(())
    }

    fn submit(&mut self, command: &alondra_store::CommittedCommand) -> Handoff {
        if self.refuse_offers > 0 {
            self.refuse_offers -= 1;
            self.worker.discard_prepared();
            return Handoff::RefusedBeforeOffer;
        }
        self.worker.submit(command).unwrap_or(Handoff::Uncertain)
    }

    /// Something else changed the task between the claim and the command's record, so no host was
    /// offered the input: the claim is rejected and the input goes again, as a refused one does.
    fn claim_went_stale(&mut self, intent: i64, stamp: &alondra_foundation::ExecutionStamp, generation: u64, input: &InputId) -> Result<(), Failed> {
        log!("input {} of {} was claimed, but the task changed before its command was recorded: it goes again", input.as_str(), self.task);
        let rejected = self.domain.own(self.scope.clone(), |owner| owner.resolve(stamp, intent, generation, DispatchReceipt::Rejected));
        match rejected {
            Ok(_) => {}
            Err(StoreError::Storage(error)) if stale(&error) => {
                let task = self.task.clone();
                let (open, _) = self.domain.own(self.scope.clone(), |owner| owner.claim_is_open(&task, intent)).map_err(failed("reading the stale claim"))?;
                if open {
                    return Err(Failed::new(format!("the claim of input {} could not be rejected and nothing resolved it", input.as_str())));
                }
                return Ok(());
            }
            Err(error) => return Err(failed("rejecting the stale claim")(error)),
        }
        self.sealed();
        let task = self.task.clone();
        self.domain.own(self.scope.clone(), |owner| untaken::resend_refused(owner, &task, input)).map_err(failed("sending the stale input again"))?;
        self.sealed();
        Ok(())
    }

    /// The host of an open session refused `input` before offering it, which leaves nothing for a
    /// provider event to end: the same text goes again at once. When the host refuses that one
    /// too, the owner fails: a host that resumes the same session takes this one's place, and the
    /// input waits for a slot of the gate. A session that never opened keeps the start it has.
    fn offer_refused(&mut self, input: &InputId) -> Result<(), Failed> {
        let task = self.task.clone();
        if self.unopened()? {
            return Ok(());
        }
        if self.refused.remove(input) {
            log!("the SDK host of {task} refused input {} again before offering it: the task gives up its slot and a host that resumes the same session gets the input when the gate gives it one", input.as_str());
            return Err(Failed::new("the host refused the same input twice before offering it".into()));
        }
        let (resent, _) = self.domain.own(self.scope.clone(), |owner| untaken::resend_refused(owner, &task, input)).map_err(failed("sending the refused input again"))?;
        if let Some(resent) = resent {
            log!("the SDK host of {task} refused input {} before offering it: it goes again as {}", input.as_str(), resent.as_str());
            self.refused.insert(resent);
        }
        Ok(())
    }

    fn journal_len(&self) -> usize {
        self.domain.read().state().lifecycle_journal.get(&self.task).map_or(0, |journal| journal.len())
    }

    /// The provider explicitly refused an input: the domain already sent the same text again as a
    /// new input, with no card. When the provider refuses that one too, the owner fails: a host
    /// that resumes the same session takes this one's place.
    fn provider_refusals_since(&mut self, journaled: usize) -> Result<(), Failed> {
        let resent: Vec<(String, String)> = self.domain.read().state().lifecycle_journal.get(&self.task).into_iter().flat_map(|journal| journal.since(journaled))
            .filter_map(|record| match record {
                LifecycleRecord::HandoffResent { input, resent, .. } => Some((input.clone(), resent.clone())),
                _ => None,
            })
            .collect();
        for (input, resent) in resent {
            let (Ok(input), Ok(resent)) = (InputId::new(input), InputId::new(resent)) else { continue };
            if self.refused.remove(&input) {
                log!("the provider of {} refused input {} again: the task gives up its slot and a host that resumes the same session gets it", self.task, input.as_str());
                return Err(Failed::new("the provider refused the same input twice".into()));
            }
            log!("the provider of {} refused input {}: it goes again as {}", self.task, input.as_str(), resent.as_str());
            self.refused.insert(resent);
        }
        Ok(())
    }

    /// Whether the task's Claude session has not opened yet: it has read nothing it was handed.
    fn unopened(&self) -> Result<bool, Failed> {
        let read = self.domain.read();
        let lifecycle = read.state().lifecycle_of(&self.task).map_err(|error| Failed::new(format!("{error:#}")))?;
        Ok(lifecycle.resource() == Some(alondra_domain::ResourceEvidence::Reserved)
            || self.resumes_released && lifecycle.activity() == ActivityView::Starting)
    }

    /// Whether the host was handed an input and its session has not opened by its deadline.
    fn opening_overdue(&mut self, now: Instant) -> Result<bool, Failed> {
        let Some(deadline) = self.opening_deadline else { return Ok(false) };
        if !self.unopened()? {
            self.opening_deadline = None;
            self.resumes_released = false;
            return Ok(false);
        }
        Ok(now >= deadline)
    }

    /// `observe`, once the host has written something, however long it stays silent first: the
    /// I/O timeout bounds the read of what the host wrote, never the time its Claude takes to
    /// answer on a loaded Mac. The owner's own steps read only a readable host the same way; a
    /// host that ends makes its connection readable, and the read then fails.
    pub fn observe_written(&mut self) -> Result<alondra_store::ObservationAcceptance, Failed> {
        while !self.worker.readable(WAKE).map_err(adapter("waiting for the SDK host"))? {}
        self.observe()
    }

    /// The next thing the host observed, committed with the domain's decision, and acknowledged
    /// only with the seal of a commit readers already see.
    pub fn observe(&mut self) -> Result<alondra_store::ObservationAcceptance, Failed> {
        let batch = match self.worker.poll() {
            Ok(Some(batch)) => batch,
            Ok(None) => {
                self.host_closed = true;
                return Err(Failed::new("the SDK host closed".into()));
            }
            Err(error) => {
                self.host_closed = true;
                return Err(Failed::new(format!("the connection to the SDK host failed: {error:?}")));
            }
        };
        if self.worker.native_exited(&batch) {
            self.host_closed = true;
            return Err(Failed::new("the SDK host's native process exited".into()));
        }
        let generation = self.worker.owner_generation().map_err(adapter("reading the owner generation"))?;
        let binding = self.worker.binding().clone();
        let identity=NativeAccountIdentity::load_observed(self.worker.state_directory(),self.worker.provider())?;
        if !self.identity_kept && let Some(identity)=identity.as_ref().filter(|identity|identity.has_proof()) {
            match identity.keep(self.worker.state_directory()) {
                Ok(()) => self.identity_kept = true,
                Err(error) => log!("could not keep the native account identity of {}: {error}", self.task),
            }
        }
        let (worker, home) = (&self.worker, self.domain.home());
        self.domain.engine().note_output(&self.task);
        let journaled = self.journal_len();
        let (acceptance, unrecorded) = commit_observation(&*self.domain, &self.scope, &binding, generation, &batch, worker.normalizer(), home,identity.as_ref())
            .map_err(failed(&format!("committing the observation at cursor {}", batch.cursor)))?;
        self.provider_refusals_since(journaled)?;
        if let Some(answered) = self.worker.answered_control(&batch) {
            self.results.retain(|awaited| *awaited != answered);
        }
        if let Some(start) = self.domain.engine().context_observed(&self.task, self.worker.provider(), &batch, alondra_core::now_millis()) {
            self.record_start(&start);
        }
        feed(self.worker.provider(), &*self.domain, &self.agent, &batch);
        if let Some(line) = alondra_provider_claude::sdk_message(&batch).and_then(|message| claude_said(&message, &mut self.claude_session)) {
            log!("{line} ({}, task {})", self.agent, self.task);
        }
        for observation in self.sealed().observations {
            self.domain.before_acknowledge();
            self.worker.acknowledge(&observation).map_err(adapter("acknowledging the observation"))?;
        }
        if self.worker.provider()==Provider::Codex && let Some(failure)=alondra_provider_codex::session_failure(&batch){
            log!("the Codex session of {} ended on its own: {failure}", self.task);
            self.native_failure=Some(failure);
        }
        if self.worker.provider()==Provider::Codex && let Some(reason)=alondra_provider_codex::account_refusal(&batch){self.native_account_refusal=Some(reason.into());return Err(Failed::new(reason.into()))}
        if !unrecorded.is_empty() {
            log!("the SDK owner of {} keeps its host after an observation omitted lifecycle reactions: {}", self.task, unrecorded.join("; "));
        }
        Ok(acceptance)
    }

    /// Codex opens no turn for its own work that ended: the node is told at once. Claude opens one
    /// by itself, and the one clock tells the node only when it did not (`obligation_watch`).
    fn wake_completed_codex_work(&mut self) -> Result<(), Failed> {
        if self.worker.provider() != Provider::Codex || self.paused() {
            return Ok(());
        }
        let (task, agent) = (self.task.clone(), self.agent.clone());
        self.domain.own(self.scope.clone(), |owner| untaken::wake(owner, &task, &agent)).map_err(failed("queuing the completed native work"))?;
        Ok(())
    }

    pub fn paused(&self) -> bool {
        self.pause.lock_or_recover("the SDK pause subscription").get(&*self.domain, &self.agent)
    }

    /// An input that starts a turn waits here for a slot of the gate, with no card: it stays the
    /// task's, and the gate wakes this owner when its turn comes.
    fn may_start_turn(&self) -> bool {
        let starts = {
            let read = self.domain.read();
            let state = read.state();
            state.turn_passes_gate(&self.task) && state.lifecycle_of(&self.task).is_ok_and(|lifecycle| lifecycle.input_starts_turn())
        };
        !starts || self.domain.gate().may_take_turn(&self.task, alondra_core::now_millis())
    }

    /// The user's answer to a decision lets a turn go on that held no slot while it waited on them.
    /// An answer Alondra's policy gives goes at once: its turn never stopped holding its slot.
    fn may_answer(&self) -> bool {
        let goes_on = {
            let read = self.domain.read();
            let state = read.state();
            state.turn_passes_gate(&self.task) && state.lifecycle_of(&self.task).is_ok_and(|lifecycle| lifecycle.answer_goes_on())
        };
        !goes_on || self.domain.gate().may_take_turn(&self.task, alondra_core::now_millis())
    }

    /// Whether the task's lifecycle waits for the host: an input it was handed and has not taken
    /// yet, a turn or work of its own still going, or the answer of a control it took. Facts
    /// decide it, never a clock.
    fn expects_host(&mut self) -> Result<bool, Failed> {
        if self.replacement {
            return Ok(false);
        }
        let state = Arc::clone(self.domain.read().state());
        let lifecycle = state.lifecycle_of(&self.task).map_err(|error| Failed::new(format!("{error:#}")))?;
        if self.awaiting.as_ref().is_some_and(|input| !lifecycle.pending_inputs().contains(&input)) {
            self.awaiting = None;
        }
        Ok(self.awaiting.is_some()
            || !self.results.is_empty()
            || matches!(lifecycle.activity(), ActivityView::Starting | ActivityView::Working | ActivityView::OwnWork | ActivityView::Continuation))
    }

    fn legacy_plan_supersessions(&self) -> Result<BTreeSet<alondra_foundation::CallbackId>, Failed> {
        if self.worker.provider() != Provider::Codex || self.worker.supports_plan_supersession() {
            return Ok(BTreeSet::new());
        }
        let read = self.domain.read();
        let state = read.state();
        let lifecycle = state.lifecycle_of(&self.task).map_err(|error| Failed::new(format!("{error:#}")))?;
        Ok(state.lifecycle_journal.get(&self.task).into_iter().flatten().rev()
            .take_while(|record| !matches!(record, LifecycleRecord::Admitted { .. }))
            .filter_map(|record| match record {
                LifecycleRecord::Decided { epoch, decision: DecisionRecord::Superseded, .. } => Some(alondra_domain::decision_callback(lifecycle.generation(), *epoch)),
                _ => None,
            })
            .filter(|callback| lifecycle.callback(callback) == Some(alondra_domain::CallbackView::Answered))
            .collect())
    }

    /// A late observation can change whether an old host is idle. Read it before considering any
    /// unsent input, then reconsider the lifecycle on the next step. Once its observations are
    /// drained, move the idle host to this release before handing it another instruction.
    fn advance_idle_host_of_another_release(&mut self) -> Result<bool, Failed> {
        let deferred = self.legacy_plan_supersessions()?;
        let supersession_only = !deferred.is_empty() && self.awaiting.is_none() && self.results.is_empty()
            && self.domain.read().state().lifecycle_of(&self.task)
                .map_err(|error| Failed::new(format!("{error:#}")))?.quiescent_except_callbacks(&deferred);
        if !deferred.is_empty() {
            if !supersession_only { return Ok(false); }
        } else if !self.worker.runs_another_release() || (self.expects_host()? && !self.only_unsent_input()?) {
            return Ok(false);
        }
        if self.worker.readable(Duration::ZERO).unwrap_or(true) {
            self.observe()?;
        } else {
            self.moves_to_this_release = true;
        }
        Ok(true)
    }

    /// `idle_release` asked to close this host. What the host said last can move the task on, so
    /// all of it is read first: `false` until nothing is left to read and the answer is known. A
    /// host whose connection broke says nothing more here; the host that resumes the session reads
    /// what it journaled.
    pub fn settle_release(&mut self) -> Result<bool, Failed> {
        self.sealed();
        if !self.host_closed && self.worker.readable(Duration::ZERO).unwrap_or(true) {
            match self.observe() {
                Ok(_) => return Ok(false),
                Err(_) if self.host_closed => {}
                Err(failed) => return Err(failed),
            }
        }
        if let Some(permit) = self.controls.pop_front() {
            self.control(permit)?;
            return Ok(false);
        }
        let held = crate::provider_transition::holds_execution(&*self.domain, &self.task) || crate::account_move::holds_execution(&*self.domain, &self.task);
        self.releases = !held && self.awaiting.is_none() && self.results.is_empty() && self.permits.is_empty() && self.controls.is_empty()
            && self.domain.read().state().host_releasable(&self.task);
        Ok(true)
    }

    /// Ends the host and every process under it, as a pause does, and records that the session
    /// waits with no host. Refused because the task moved on meanwhile, the caller replaces it.
    pub fn release_host(&mut self) -> Result<(), Failed> {
        let host = self.worker.stopper();
        let tree = host.as_ref().map_or(Ok(vec![]), HostStop::tree)
            .map_err(|error| Failed::new(format!("listing the processes of the host: {error}")))?;
        mark_stopped_by_alondra(self.worker.state_directory());
        self.stop()?;
        host.map_or(Ok(()), |host| host.end_tree(&tree, HALT_GRACE)).map_err(|error| Failed::new(format!("the processes of the host did not end: {error}")))?;
        let read = self.domain.read();
        let state = read.state();
        let generation = state.lifecycle_of(&self.task).ok().and_then(|lifecycle| lifecycle.execution().map(|stamp| stamp.generation()))
            .ok_or_else(|| Failed::new("the task has no execution".into()))?;
        let epoch = state.data_epoch.clone();
        drop(read);
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: epoch,
            command_id: format!("host-released-{}-{}", self.task, crate::auth::random_hex(8)).into(),
            expected_revision: 0,
            command: Command::RecordHostReleased { task_id: self.task.clone(), generation },
        };
        self.domain.execute_as_system(envelope).map(drop).map_err(|error| Failed::new(store_text(&error)))
    }

    /// Whether all the lifecycle waits for is an input this owner has not handed to its host.
    fn only_unsent_input(&self) -> Result<bool, Failed> {
        if self.awaiting.is_some() || !self.results.is_empty() {
            return Ok(false);
        }
        let read = self.domain.read();
        let lifecycle = read.state().lifecycle_of(&self.task).map_err(|error| Failed::new(format!("{error:#}")))?;
        Ok(lifecycle.activity() == ActivityView::Starting && !lifecycle.pending_inputs().is_empty())
    }

    /// One step: send a waiting control first, then a waiting dispatch unless a pause holds it,
    /// else read the host when the lifecycle waits for it or the host has something, else wait a
    /// moment for what the store issues for this task.
    pub fn step(&mut self, stopped: &dyn Fn() -> bool) -> Result<(), Failed> {
        self.step_at(stopped, clock::instant())
    }

    /// `step`, as at `now`.
    fn step_at(&mut self, stopped: &dyn Fn() -> bool, now: Instant) -> Result<(), Failed> {
        let stepped = self.step_once(stopped, now);
        if stepped.is_err() {
            self.owner_failed = true;
        }
        stepped
    }

    fn step_once(&mut self, stopped: &dyn Fn() -> bool, now: Instant) -> Result<(), Failed> {
        if std::mem::take(&mut self.fail_next_step) {
            return Err(Failed::new("a step of the owner failed".into()));
        }
        assert!(!std::mem::take(&mut self.panic_next_step), "a step of the owner panicked");
        if self.worker.native_liveness().map_err(adapter("checking the native process"))? == Some(alondra_provider_codex::NativeLiveness::Lost) {
            self.host_closed = true;
            return Err(Failed::new("the Codex host lost its app-server".into()));
        }
        self.sealed();
        if self.opening_overdue(now)? {
            // What the host already wrote may be its Claude opening the session: an owner that
            // looks again only past the deadline reads it before it gives the host up.
            if self.worker.readable(Duration::ZERO).unwrap_or(false) {
                return self.observe().map(drop);
            }
            log!("the SDK host of {} did not open its Claude session within {} s of its input: another host takes its place", self.task, self.opening_timeout.as_secs());
            return Err(Failed::new("the host did not open its Claude session in time".into()));
        }
        if self.domain.sdk_owners().requeues.lock_or_recover("the requeued tasks").remove(&self.task) {
            self.requeued = true;
            log!("the process of {} wrote nothing since it was handed an input and no event named it: it is stopped and a host that resumes the same session gets the input again, flagged as possibly received, when the gate gives it a slot", self.task);
            return Err(Failed::new("the process wrote nothing since it was handed an input".into()));
        }
        if self.advance_idle_host_of_another_release()? {
            return Ok(());
        }
        let deferred = self.legacy_plan_supersessions()?;
        let held_answers = self.controls.iter().any(ControlPermit::responds) && !self.may_answer();
        if let Some(index) = self.controls.iter().position(|permit| !(held_answers && permit.responds()) && !deferred.iter()
            .any(|callback| callback.as_str() == permit.control().as_str())) {
            let permit = self.controls.remove(index).expect("the control was just found");
            return self.control(permit);
        }
        self.wake_completed_codex_work()?;
        self.sealed();
        let paused = self.paused() || crate::provider_transition::holds_source_inputs(&*self.domain,&self.task,&self.agent.0);
        if !paused && !self.permits.is_empty() && self.may_start_turn() && let Some(permit) = self.permits.pop_front() {
            return self.dispatch(permit);
        }
        if self.expects_host()? {
            if !self.worker.readable(WAKE).map_err(adapter("waiting for the SDK host"))? {
                return Ok(());
            }
            return self.observe().map(drop);
        }
        // Claude goes on by itself while the task rests, as when a subagent hands its report back
        // after the turn ended: what its host says is read as it comes.
        if (!self.replacement || self.untaken) && self.worker.readable(Duration::ZERO).unwrap_or(true) {
            return self.observe().map(drop);
        }
        let sealed = self.domain.wait_for_seals(&self.task, &self.agent, WAKE, &|| {
            stopped() || self.pause.lock_or_recover("the SDK pause subscription").snapshots.has_changed().unwrap_or(false)
        });
        self.receive_permits(sealed.permits);
        self.controls.extend(sealed.control_permits);
        Ok(())
    }

    pub(crate) fn rebind(&mut self, domain: Arc<dyn Runtime>) {
        self.pause = Mutex::new(PauseSubscription::new(&*domain, &self.agent));
        self.domain = domain;
    }

    /// Claims and records the next waiting dispatch but does not send it: the crash state a daemon
    /// leaves when it dies after recording a command and before the host receives it.
    pub(crate) fn claim_and_record_without_sending(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.permits.pop_front() else {
            return false;
        };
        self.domain.own(self.scope.clone(), |owner| owner.claim(permit)).unwrap();
        let attempt = self.sealed().attempts.pop().expect("the claim issued an attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker)).unwrap();
        self.sealed();
        true
    }

    /// Claims, records and sends the next waiting dispatch to the host, but does not record its
    /// receipt: the crash state a daemon leaves when it dies after the host offered the input.
    pub(crate) fn claim_record_and_offer_without_receipt(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.permits.pop_front() else {
            return false;
        };
        self.domain.own(self.scope.clone(), |owner| owner.claim(permit)).unwrap();
        let attempt = self.sealed().attempts.pop().expect("the claim issued an attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker)).unwrap();
        let command = self.sealed().commands.pop().expect("the command left a seal");
        assert_eq!(self.worker.submit(&command).unwrap(), Handoff::Offered, "the host offered the input");
        true
    }

    /// Claims, records and sends the next waiting dispatch, and records its receipt as uncertain: an
    /// owner stopped while the host read the input never learns that the host took it.
    pub(crate) fn claim_record_and_offer_with_an_uncertain_receipt(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.permits.pop_front() else {
            return false;
        };
        self.domain.own(self.scope.clone(), |owner| owner.claim(permit)).unwrap();
        let attempt = self.sealed().attempts.pop().expect("the claim issued an attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_command(attempt, generation, worker)).unwrap();
        let command = self.sealed().commands.pop().expect("the command left a seal");
        assert_eq!(self.worker.submit(&command).unwrap(), Handoff::Offered, "the host offered the input");
        self.domain.own(self.scope.clone(), |owner| owner.record_receipt(command, receipt(Handoff::Uncertain))).unwrap();
        self.sealed();
        true
    }

    /// Claims and records the next waiting control but does not send it: the crash state a daemon
    /// leaves when it dies after recording a control and before the host receives it.
    pub(crate) fn claim_and_record_control_without_sending(&mut self) -> bool {
        self.sealed();
        let Some(permit) = self.controls.pop_front() else {
            return false;
        };
        assert!(self.domain.own(self.scope.clone(), |owner| owner.claim_control(permit)).unwrap().0, "the control was claimed");
        let attempt = self.sealed().control_attempts.pop().expect("the claim issued a control attempt");
        let generation = self.worker.owner_generation().unwrap();
        let worker = &mut self.worker;
        self.domain.own(self.scope.clone(), |owner| owner.record_control(attempt, generation, |attempt| worker.prepare_control(attempt))).unwrap();
        self.sealed();
        true
    }

    pub fn pending_controls(&mut self) -> usize {
        self.sealed();
        self.controls.len()
    }

    pub fn pending_permits(&mut self) -> usize {
        self.sealed();
        self.permits.len()
    }

    pub fn waits_for_host(&mut self) -> bool {
        self.expects_host().unwrap()
    }

    pub fn stop(&mut self) -> Result<(), Failed> {
        self.worker.stop().map_err(adapter("stopping the SDK host"))
    }

    /// The host this driver owns closed while the daemon runs, as a crash of Node or of the host
    /// does: what is left of its group ends, and a host that resumes the same Claude session takes
    /// its place, as a daemon that comes back does (`adopt`). The ended host's journal says what it
    /// took; the task asks the user how to go on, and what it is told next reaches that session.
    pub fn replace_ended_host(mut self, backend: &SdkBackend, plugin: Option<&std::path::Path>) -> Result<Driver, Failed> {
        if self.owner_failed && !self.host_closed || self.moves_to_this_release {
            mark_stopped_by_alondra(self.worker.state_directory());
        }
        self.stop()?;
        let (domain, task, handoffs) = (Arc::clone(&self.domain), self.task.clone(), Arc::clone(&self.handoffs));
        drop(self);
        let mut next = adopt(domain, backend, task, plugin)?;
        next.handoffs = handoffs;
        Ok(next)
    }

    /// Whether `SILENT_HOSTS` hosts in a row ended without their Claude showing it ran, in a
    /// session that had opened before: another one would end the same way. A Codex host is bounded
    /// by its own relaunches.
    fn gave_up_starting(&self) -> bool {
        self.worker.provider() == Provider::Claude && self.claude_session.is_none() && self.silent_hosts >= SILENT_HOSTS && !self.unopened().unwrap_or(false)
    }

    /// Whether the owner should put a host that resumes the session in place of its own: its host
    /// closed or its connection to it failed, which a host that is dying shows before its exit, or
    /// a step of the owner itself failed. A replacement positively observed dead, or one that
    /// ended without taking its input, is replaced again after `relaunch_wait`, however many ended
    /// before. The
    /// replacement stops what is left of the host and waits for its exit before it reads the host's
    /// lock, so whether the host ended is a fact, never a guess.
    pub fn host_to_replace(&self) -> bool {
        (self.host_closed || self.owner_failed || self.moves_to_this_release) && self.native_account_refusal.is_none() && (!self.replacement || self.untaken || self.requeued || self.host_closed && self.worker.provider() == alondra_core::accounts::Provider::Codex) && !self.gave_up_starting() && !crate::provider_transition::holds_execution(&*self.domain,&self.task)
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.domain.gate().owner_ended(&self.task);
        if !self.releases {
            self.domain.engine().forget_context(&self.task);
        }
        self.domain.engine().host_ended(&self.agent);
        crate::host_folders::host_ended(&*self.domain, self.worker.state_directory(), &|agent| self.domain.owns_sdk_host(agent));
    }
}

fn feed(provider: Provider, domain: &dyn Runtime, agent: &AgentId, batch: &OrderedObservation) {
    domain.engine().feed(provider, agent, batch);
}

/// Marks, in the host's folder, that Alondra itself ends the host: the daemon stops it, or its
/// owner failed. The daemon that takes the ended host over reads it, so the end does not count as
/// one of a session that keeps closing; the folder, and the mark with it, is archived when a new
/// host takes its place. A mark that cannot be written leaves the end counted as the session's.
const STOPPED_BY_ALONDRA: &str = "stopped-by-alondra";
const HALT_GRACE: Duration = Duration::from_secs(10);
fn mark_stopped_by_alondra(state_directory: &std::path::Path) {
    if let Err(error) = std::fs::write(state_directory.join(STOPPED_BY_ALONDRA), b"") {
        log!("could not mark the stop of the SDK host in {} as Alondra's own: {error}", state_directory.display());
    }
}
/// A host this daemon launched that no owner will hold, because what came after its launch
/// failed: it is ended by the lock it holds, so no host runs with no owner.
fn end_launched(domain: &dyn Runtime, directory: &std::path::Path, failed: Failed) -> Failed {
    mark_stopped_by_alondra(directory);
    match alondra_engine_ports::agent_host::of(domain.ports()).and_then(|hosts| hosts.end_holders(directory)) {
        Ok(_) => failed,
        Err(error) => Failed { why: format!("{}; its new host did not stop: {error}", failed.why), ..failed },
    }
}
/// A live host goes on: a mark left by a stop that did not end it no longer applies.
fn remove_stop_mark(mark: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(mark)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        log!("could not remove the mark of an SDK host stop that did not end it ({}): {error}", mark.display());
    }
}

/// Commits what the host observed. What the domain refuses never stops the node: the observation
/// commits again without it, and the second value says what of the task's lifecycle it left out.
#[allow(clippy::too_many_arguments)]
pub(crate) fn commit_observation<N: alondra_agent_application::ObservationNormalizer>(domain: &dyn Runtime, scope: &AuthorityScope, binding: &ProviderBinding,
    generation: u64, batch: &OrderedObservation, normalizer: &N, home: &std::path::Path,identity:Option<&NativeAccountIdentity>) -> Result<(alondra_store::ObservationAcceptance, Vec<String>), StoreError> {
    let stage = |probe: bool| domain
        .own(scope.clone(), |owner| {
            let acceptance = owner.stage_observation_with_identity(binding, generation, batch, normalizer, home, probe,identity.map(|identity| identity as &dyn crate::domain_store::KnownIdentity))?;
            if probe && !owner.unrecorded().is_empty() && owner.work_is_over(batch.execution.task()) {
                return Err(StoreError::Domain(alondra_domain_rules::DomainError::new("work_is_over", owner.unrecorded().join("; "))));
            }
            Ok((acceptance, owner.unrecorded().to_vec()))
        })
        .map(|(staged, _)| staged);
    let staged = match stage(false) {
        Err(StoreError::Domain(refusal)) => {
            log!("the domain refused part of what an SDK host observed ({}): it commits without it", refusal.message);
            stage(true)
        }
        staged => staged,
    };
    let error = match staged {
        Err(error) => error,
        staged => return staged,
    };
    let Some(reason) = crate::domain_store::DiscardReason::of(&error) else { return Err(error) };
    log!("observation {} of {} could not be settled ({}: {}): it is discarded and the task goes on", batch.cursor, batch.execution.task(), reason.code(), store_text(&error));
    domain
        .own(scope.clone(), |owner| Ok((owner.discard_observation(binding, generation, batch, normalizer, reason)?, Vec::new())))
        .map(|(staged, _)| staged)
}

/// The log line of what the host's Claude did, for the steps of its life: the session it runs,
/// once per session, every API error it reports, with its own words, and the end of each of its
/// turns. Everything else it says is not logged.
fn claude_said(message: &serde_json::Value, session: &mut Option<String>) -> Option<String> {
    match (message["type"].as_str(), message["subtype"].as_str()) {
        (Some("system"), Some("init")) => {
            let id = message["session_id"].as_str()?;
            if session.as_deref() == Some(id) {
                return None;
            }
            *session = Some(id.to_owned());
            Some(format!("the SDK host's Claude runs session {id} with {}", message["model"].as_str().unwrap_or("its default model")))
        }
        (Some("result"), subtype) => Some(match message["is_error"].as_bool() {
            Some(true) => format!("a turn of the SDK host's Claude failed ({}, {}{}){}", subtype.unwrap_or("no subtype"), message["terminal_reason"].as_str().unwrap_or("no reason given"),
                status_of(message), said_text(message["result"].as_str())),
            _ => format!("a turn of the SDK host's Claude ended ({})", message["terminal_reason"].as_str().or(subtype).unwrap_or("completed")),
        }),
        (Some("assistant"), _) => {
            let error = message["error"].as_str()?;
            let detail = message["api_error"].as_str().map_or_else(String::new, |detail| format!(", {detail}"));
            let text = message.pointer("/message/content").and_then(serde_json::Value::as_array)
                .and_then(|blocks| blocks.iter().find_map(|block| block["text"].as_str()));
            Some(format!("the SDK host's Claude got an API error ({error}{detail}{}){}", status_of(message), said_text(text)))
        }
        _ => None,
    }
}

/// The HTTP status of an API error Claude reported, when it gave one.
fn status_of(message: &serde_json::Value) -> String {
    message["api_error_status"].as_u64().map_or_else(String::new, |status| format!(", HTTP {status}"))
}

/// What Claude Code wrote of an error, on one line and at most 300 characters.
fn said_text(text: Option<&str>) -> String {
    let Some(text) = text.map(str::trim).filter(|text| !text.is_empty()) else { return String::new() };
    let line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let clipped: String = line.chars().take(300).collect();
    format!(": {clipped}{}", if clipped.len() < line.len() { "…" } else { "" })
}

/// Another input goes to the host first: the store issues this one's permit again once it may.
fn out_of_turn(error: &anyhow::Error) -> bool {
    matches!(error.downcast_ref::<alondra_store::Error>(), Some(alondra_store::Error::OfferOrder))
}

fn stale(error: &anyhow::Error) -> bool {
    matches!(
        error.downcast_ref::<alondra_store::Error>(),
        Some(alondra_store::Error::StalePermit)
    )
}

fn failed_step(driver: &Driver, error: &Failed) {
    log!("the SDK owner of {} stopped: {error}", driver.task);
    driver.domain.owner_failed(error.to_string());
}

/// Runs the owner on a thread of its own until it is told to stop or the host goes.
pub struct Owner {
    /// Set before its host is asked to stop: the end of the host that follows is the owner's own.
    stopped: Arc<AtomicBool>,
    /// The owner ended on its own, its host stopped: it owns nothing any more.
    ended: Arc<AtomicBool>,
    /// The stop it was told is a close: Claude's running turn ends as an interrupted turn first.
    interrupt: Arc<AtomicBool>,
    /// `idle_release` asks the owner to close its host, which it does between two steps.
    release: Arc<AtomicBool>,
    domain: Arc<dyn Runtime>,
    /// A holder that panics leaves it whole, so it is recovered: one assignment or a read.
    host: Arc<Mutex<Option<HostStop>>>,
    agent: AgentId,
    /// The host's folder, where its stop is marked as Alondra's own.
    state_directory: std::path::PathBuf,
    thread: Option<std::thread::JoinHandle<Result<(), Failed>>>,
    /// The daemon went as a crash takes it: the owner lets go of its host without stopping it.
    crashed: Arc<AtomicBool>,
    /// Shared with the driver of each host the owner runs.
    handoffs: Arc<Handoffs>,
}

impl Owner {
    pub fn spawn(mut driver: Driver) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let interrupt = Arc::new(AtomicBool::new(false));
        let interrupting = Arc::clone(&interrupt);
        let release = Arc::new(AtomicBool::new(false));
        let releasing = Arc::clone(&release);
        let domain = Arc::clone(&driver.domain);
        let host = Arc::new(Mutex::new(driver.worker.stopper()));
        let current = Arc::clone(&host);
        let agent = driver.agent.clone();
        let state_directory = driver.worker.state_directory().to_path_buf();
        let flag = Arc::clone(&stopped);
        let domain_of_owner = Arc::clone(&domain);
        let ended = Arc::new(AtomicBool::new(false));
        let ending = Arc::clone(&ended);
        let crashed = Arc::new(AtomicBool::new(false));
        let crashing = Arc::clone(&crashed);
        let handoffs = Arc::clone(&driver.handoffs);
        let thread = alondra_core::clock::spawn_named_thread(Some(format!("sdk-owner-{}", driver.task)), move || {
            let told = || flag.load(Ordering::SeqCst);
            let mut lost_because = None;
            loop {
                while !told() && !driver.moves_to_this_release && !driver.releases {
                    if releasing.load(Ordering::SeqCst) {
                        match driver.settle_release() {
                            Ok(settled) => if settled { releasing.store(false, Ordering::SeqCst) },
                            Err(error) => {
                                releasing.store(false, Ordering::SeqCst);
                                log!("could not read what the host of {} said before closing it: {error}", driver.task);
                            }
                        }
                    }
                    if driver.releases {
                        break;
                    }
                    // A request to close the host ends the owner's wait at once, as a stop does:
                    // the host closes at the instant `idle_release` asks, not on a later wake.
                    let woken = || told() || releasing.load(Ordering::SeqCst);
                    if let Err(error) = driver.step(&woken) {
                        // Told to stop, the owner ended its host itself: what the host's end
                        // breaks in the step it was reading is that end, not a failure.
                        if !told() {
                            failed_step(&driver, &error);
                        }
                        break;
                    }
                }
                if told() {
                    break;
                }
                if driver.releases {
                    match driver.release_host() {
                        Ok(()) => {
                            driver.let_go();
                            ending.store(true, Ordering::SeqCst);
                            if let Some(hook) = driver.before_release_ends.take() {
                                hook();
                            }
                            log!("{} rests with nothing in its host: the host closed and its session waits for its next input", driver.task);
                            let capability = driver.ended_capability();
                            drop(driver);
                            capability.revoke();
                            return Ok(());
                        }
                        Err(error) => {
                            log!("the host of {} closed, but the node did not stay without one: {error}", driver.task);
                            driver.releases = false;
                            driver.replacement = false;
                            driver.host_closed = true;
                        }
                    }
                }
                let backend = driver.domain.sdk().map(|(backend, _)| backend.clone());
                let Some(backend) = backend.filter(|_| driver.host_to_replace()) else {
                    lost_because = driver.native_account_refusal.clone().or_else(|| Some(if driver.gave_up_starting() {
                        format!("Claude did not start in its session in {} new hosts in a row", driver.silent_hosts + 1)
                    } else {
                        "its new host stopped before it could take the task's next message".to_owned()
                    }));
                    break;
                };
                let relaunches = if driver.replacement { driver.relaunches + 1 } else { 0 };
                let silent_hosts = if driver.claude_session.is_some() { 0 } else { driver.silent_hosts + 1 };
                if driver.replacement {
                    driver.domain.pause_for_seals(pause_before_relaunch(driver.relaunches), &told);
                    if told() {
                        break;
                    }
                } else if driver.silent_hosts > 0 {
                    driver.domain.pause_for_seals(pause_before_relaunch(driver.silent_hosts - 1), &told);
                    if told() {
                        break;
                    }
                }
                let task = driver.task.clone();
                let plugin = driver.plugin.clone().or_else(|| driver.domain.ports().plugins().node(driver.domain.home()));
                let moved = driver.moves_to_this_release;
                let ended = driver.native_failure.take();
                let waited = relaunch_wait(driver.relaunches);
                let controls = std::mem::take(&mut driver.controls);
                match driver.replace_ended_host(&backend, plugin.as_deref()) {
                    Ok(mut next) => {
                        *current.lock_or_recover("the SDK host stop") = next.worker.stopper();
                        next.relaunches = relaunches;
                        next.silent_hosts = silent_hosts;
                        if relaunches > 0 {
                            let why = ended.map_or_else(|| "before taking its input".to_owned(), |failure| format!("before taking its input ({failure})"));
                            log!("the new SDK host of {task} ended {why}: after {}s a new host that resumes the same session took its place (relaunch {relaunches})", waited.as_secs());
                        } else if moved {
                            next.controls.extend(controls);
                            log!("{task} rested on the SDK host of the release before: a host of this release that resumes the same Claude session took its place");
                        } else {
                            log!("the SDK host of {task} ended under this daemon: a host that resumes the same Claude session took its place");
                        }
                        driver = next;
                    }
                    Err(Failed { why, .. }) => {
                        ending.store(true, Ordering::SeqCst);
                        lost(&*domain_of_owner, &task, &why);
                        return Ok(());
                    }
                }
            }
            if crashing.load(Ordering::SeqCst) {
                return Ok(());
            }
            // Told to stop, or ended by its own failure, the owner stops its host and waits for
            // it: no host is left running with no owner. Only a daemon that exits whole leaves
            // its hosts running, for the daemon that comes back to adopt.
            if interrupting.load(Ordering::SeqCst)
                && let Some(host) = driver.worker.stopper()
                && let Err(error) = host.request_after_interrupt()
            {
                log!("could not ask the SDK host of {} to interrupt its turn and stop: {error}", driver.task);
            }
            let stopped = driver.stop();
            if let Err(error) = &stopped {
                log!("the SDK host of {} did not stop cleanly: {error}", driver.task);
            }
            if let Some(why) = lost_because {
                ending.store(true, Ordering::SeqCst);
                lost(&*driver.domain, &driver.task, &why);
            }
            let capability = driver.ended_capability();
            drop(driver);
            capability.revoke();
            stopped
        })
        .expect("spawning the SDK owner");
        Self { stopped, ended, interrupt, release, domain, host, agent, state_directory, thread: Some(thread), crashed, handoffs }
    }

    /// Asks the owner to close its host once its task rests with nothing in it.
    pub(crate) fn ask_to_release(&self) {
        self.release.store(true, Ordering::SeqCst);
        self.domain.wake_seals();
    }

    pub(crate) fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// The owner goes as a daemon that crashes takes it: its connection to the host closes and
    /// nothing is recorded or stopped, so the host runs on for the daemon that comes back.
    fn crash(mut self) {
        self.crashed.store(true, Ordering::SeqCst);
        self.stopped.store(true, Ordering::SeqCst);
        self.domain.wake_seals();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    /// The owner ended on its own, or its thread is gone, a panic included: it owns nothing.
    pub(crate) fn ended(&self) -> bool {
        self.ended.load(Ordering::SeqCst) || self.thread.as_ref().is_some_and(std::thread::JoinHandle::is_finished)
    }

    /// The node's host, while the owner holds one.
    fn host(&self) -> Option<HostStop> {
        if self.ended() {
            return None;
        }
        self.host.lock_or_recover("the SDK host stop").clone()
    }

    /// Tells the owner to stop, lets an input or control it is handing to the host get its receipt,
    /// asks its host to stop so a read blocked on it returns, and waits until the owner has seen the
    /// host exit and its whole process group end. An error says the group did not empty, or the
    /// owner could not see it end.
    pub(crate) fn stop(&mut self) -> Result<(), Failed> {
        self.end()
    }

    /// Stops the host as `stop` does, once a turn Claude is running has ended as an interrupted
    /// turn: the close of a node whose task is over.
    pub(crate) fn close(&mut self) -> Result<(), Failed> {
        self.interrupt.store(true, Ordering::SeqCst);
        self.end()
    }

    /// Stops the host as `close` does and ends every process under it. Each command Claude runs
    /// has a group of its own, so the end of the host's group leaves those running.
    pub fn halt(&mut self) -> Result<(), Failed> {
        let host = self.host();
        let tree = host.as_ref().map_or(Ok(vec![]), HostStop::tree);
        let closed = self.close();
        let ended = tree.and_then(|tree| host.map_or(Ok(()), |host| host.end_tree(&tree, HALT_GRACE)));
        closed?;
        ended.map_err(|error| Failed::new(format!("the processes of the SDK host of {} did not end: {error}", self.agent)))
    }

    fn end(&mut self) -> Result<(), Failed> {
        self.stopped.store(true, Ordering::SeqCst);
        mark_stopped_by_alondra(&self.state_directory);
        self.domain.wake_seals();
        self.handoffs.stop();
        let interrupt = self.interrupt.load(Ordering::SeqCst);
        if let Some(host) = self.host.lock_or_recover("the SDK host stop").as_ref()
            && let Err(error) = if interrupt { host.request_after_interrupt() } else { host.request() }
        {
            log!("could not ask the SDK host of {} to stop: {error}", self.agent);
        }
        match self.thread.take().map(std::thread::JoinHandle::join) {
            None => Ok(()),
            Some(Ok(stopped)) => stopped,
            Some(Err(_)) => Err(Failed::new(format!("the SDK owner of {} panicked", self.agent))),
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if !self.crashed.load(Ordering::SeqCst) {
            let _ = self.stop();
        }
    }
}

#[cfg(all(test, feature = "whole-engine"))]
pub use crate::engine_fixtures::owner as fixtures;
#[cfg(all(test, feature = "whole-engine"))]
#[path = "sdk_owner/tests.rs"]
mod tests;

/// A door the retake threads of a test wait at after waking the domain's waiters, closed only
/// while the test holds it.
#[derive(Default)]
pub struct HoldAfterWaking {
    held: Mutex<bool>,
    opened: std::sync::Condvar,
}

pub struct HeldAfterWaking<'a>(&'a HoldAfterWaking);

impl Drop for HeldAfterWaking<'_> {
    fn drop(&mut self) {
        *self.0.held.lock_or_recover("the retake hold") = false;
        self.0.opened.notify_all();
    }
}

impl HoldAfterWaking {
    /// Holds the door until what it returns is dropped, a failing test included.
    pub fn hold(&self) -> HeldAfterWaking<'_> {
        *self.held.lock_or_recover("the retake hold") = true;
        HeldAfterWaking(self)
    }

    fn wait(&self) {
        let mut held = self.held.lock_or_recover("the retake hold");
        while *held {
            held = self.opened.wait(held).unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// What this daemon keeps about the SDK hosts it owns and the executions it takes over.
#[derive(Default)]
pub struct Owners {
    /// A holder that panics leaves it whole, so it is recovered: `add_sdk_owner` takes, partitions
    /// and puts back the list with only field and atomic reads in between; the hosts stop outside
    /// it.
    pub held: Mutex<Vec<Owner>>,
    /// The executions being taken over again, each with its task, while that runs.
    /// A holder that panics leaves it whole, so it is recovered: a retain and then one push of a
    /// thread already spawned.
    pub retakes: Mutex<Vec<(TaskId, Retake)>>,
    /// Set, under `retakes`, while every owner stops: a takeover asked for then, as when a host the
    /// stop ends leaves its task resumed with no owner, would start a host nothing stops.
    stopping_owners: AtomicBool,
    /// Holds each retake's thread after it woke the waiters on the domain, until the test lets it
    /// return: what reads a retake as under way must not need its thread to have returned.
    pub retakes_held_after_waking: HoldAfterWaking,
    /// The tasks whose SDK execution is in hand, with how many hold it: a launch or a takeover
    /// under way, or the driver of its host. Nothing takes over an execution in hand.
    /// A holder that panics leaves it whole, so it is recovered: one count up or down.
    pub in_hand: Mutex<BTreeMap<TaskId, usize>>,
    /// The tasks whose execution a takeover is taking now: another takeover of the same task waits
    /// for it on `takeover_done`, so two never reserve an owner and start a host at once.
    /// A holder that panics leaves it whole, so it is recovered: one insert or one remove.
    pub(crate) taking_over: Mutex<BTreeSet<TaskId>>,
    pub(crate) takeover_done: std::sync::Condvar,
    /// The executions whose takeover found the disk full: taken over again once the machine watch
    /// measures space, never given up for it.
    /// A holder that panics leaves it whole, so it is recovered: one insert or one take.
    pub waiting_for_space: Mutex<BTreeSet<TaskId>>,
    /// The tasks whose lost host the user already heard of, until a takeover works again.
    pub(crate) host_losses_told: Mutex<BTreeSet<TaskId>>,
    /// When the gate last took over each task it found working with no owner of this daemon.
    pub unowned_takes: Mutex<BTreeMap<TaskId, (alondra_core::Millis, u64)>>,
    /// The tasks whose session took no turn on the input offered again: their owner puts a host
    /// that resumes the same session in place of its own, and the input waits for a slot.
    pub requeues: Mutex<BTreeSet<TaskId>>,
    after_owner_reserved: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    while_owners_stop: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    takeover_waits: Mutex<Option<Box<dyn FnMut() + Send>>>,
    owner_failures: Mutex<Option<Box<dyn Fn(String) + Send>>>,
    /// The nodes whose driver a test steps by hand, with no owner thread: they count as owned.
    pub driven_by_hand: Mutex<BTreeSet<AgentId>>,
}

alondra_engine_kernel::extension!(Owners);

pub trait SdkOwners {
    fn sdk_owners(&self) -> &Owners;

    /// Stops every SDK owner and waits for each host's exit; nothing of an SDK host outlives the
    /// daemon that owns it.
    fn stop_sdk_owners(&self);

    /// Waits until every execution being taken over again is taken, or has its card.
    fn join_retakes(&self);

    /// Whether this daemon owns the SDK host of `agent`: an owner of it runs and has not ended.
    fn owns_sdk_host(&self, agent: &AgentId) -> bool;

    /// The bytes the process group of every node host this daemon owns now holds, which is what a
    /// node costs. A group two owners name is measured once.
    fn node_footprints(&self) -> std::io::Result<BTreeMap<AgentId, u64>>;

    fn node_groups(&self) -> Vec<i32>;

    /// Every owner goes as a crash of the daemon takes it, leaving its host running.
    fn crash_sdk_owners(&self);

    /// Whether a takeover, retake or halt of an SDK execution is under way on a thread of its own.
    fn retaking(&self) -> bool;

    /// Keeps `owner` as the owner of its node's host, in place of any of that node that ended on
    /// its own. Those are joined once the list is released: their end may read it.
    fn add_sdk_owner(&self, owner: Owner);

    /// Asks the owner of `agent`'s host, if it holds one, to close it.
    fn ask_to_release_host(&self, agent: &AgentId);

    /// Stops the SDK host of `agent` and waits until it and its whole process group exited. `Ok`
    /// with nothing to stop when this daemon owns no host for it.
    fn stop_sdk_host(&self, agent: &AgentId) -> Result<(), Failed>;

    /// Stops the host of a node whose task is over, once the turn its Claude runs, if any, has
    /// ended as an interrupted turn.
    fn close_sdk_host(&self, agent: &AgentId) -> Result<(), Failed>;

    /// Stops the host of a paused node as `close_sdk_host` does, and every process under it. `None`
    /// when this daemon owns no host for it.
    fn halt_sdk_host(&self, agent: &AgentId) -> Option<Result<(), Failed>>;

    /// Runs `hook` once, as the next takeover has reserved its owner and not yet looked for the host.
    fn after_owner_reserved(&self, hook: impl FnOnce() + Send + 'static);

    /// Runs `hook` once, as the next stop of every owner has joined the takeovers under way and
    /// not yet stopped the owners.
    fn while_owners_stop(&self, hook: impl FnOnce() + Send + 'static);

    fn owner_reserved(&self);

    /// Runs `hook` each time a takeover waits for another of the same task.
    fn on_takeover_wait(&self, hook: impl FnMut() + Send + 'static);

    fn takeover_waited(&self);

    /// Every failure an SDK owner reports from now on.
    fn owner_failures(&self) -> std::sync::mpsc::Receiver<String>;

    /// Calls `hook` with every failure an SDK owner reports from now on.
    fn on_owner_failure(&self, hook: impl Fn(String) + Send + 'static);

    fn owner_failed(&self, why: String);
}

impl<R: alondra_engine_ports::Runtime + ?Sized> SdkOwners for R {
    fn sdk_owners(&self) -> &Owners {
        alondra_engine_ports::extension(self.as_runtime())
    }

    fn stop_sdk_owners(&self) {
        let owners_of = self.sdk_owners();
        let retakes = {
            let mut retakes = owners_of.retakes.lock_or_recover("the retakes");
            owners_of.stopping_owners.store(true, Ordering::SeqCst);
            std::mem::take(&mut *retakes)
        };
        for (_, retake) in retakes {
            let _ = retake.join();
        }
        let hook = owners_of.while_owners_stop.lock_or_recover("the owners stop hook").take();
        if let Some(hook) = hook {
            hook();
        }
        let owners = std::mem::take(&mut *owners_of.held.lock_or_recover("the SDK owners"));
        log!("stopping {} SDK host(s)", owners.len());
        for mut owner in owners {
            match owner.stop() {
                Ok(()) => log!("the SDK host of {} and its process group exited", owner.agent()),
                Err(error) => log!("the SDK host of {} did not stop with the daemon: {error}", owner.agent()),
            }
        }
        owners_of.stopping_owners.store(false, Ordering::SeqCst);
    }

    fn join_retakes(&self) {
        let retakes = std::mem::take(&mut *self.sdk_owners().retakes.lock_or_recover("the retakes"));
        for (_, retake) in retakes {
            let _ = retake.join();
        }
    }

    fn owns_sdk_host(&self, agent: &AgentId) -> bool {
        if self.sdk_owners().driven_by_hand.lock().expect("driven by hand poisoned").contains(agent) {
            return true;
        }
        self.sdk_owners().held.lock_or_recover("the SDK owners").iter().any(|owner| owner.agent() == agent && !owner.ended())
    }

    fn node_footprints(&self) -> std::io::Result<BTreeMap<AgentId, u64>> {
        let hosts: Vec<(AgentId, HostStop)> = self.sdk_owners().held.lock_or_recover("the SDK owners").iter()
            .filter_map(|owner| owner.host().map(|host| (owner.agent().clone(), host))).collect();
        let mut seen = BTreeSet::new();
        let mut footprints = BTreeMap::new();
        for (agent, host) in hosts {
            if host.process_group().is_some_and(|group| seen.insert(group))
                && let Some(bytes) = host.footprint()?
            {
                footprints.insert(agent, bytes);
            }
        }
        Ok(footprints)
    }

    fn node_groups(&self) -> Vec<i32> {
        self.sdk_owners().held.lock_or_recover("the SDK owners").iter()
            .filter_map(|owner| owner.host().and_then(|host| host.process_group())).collect()
    }

    fn retaking(&self) -> bool {
        self.sdk_owners().retakes.lock_or_recover("the retakes").iter().any(|(_, retake)| !retake.is_finished())
    }

    fn crash_sdk_owners(&self) {
        self.join_retakes();
        let owners = std::mem::take(&mut *self.sdk_owners().held.lock_or_recover("the SDK owners"));
        for owner in owners {
            owner.crash();
        }
    }

    fn add_sdk_owner(&self, owner: Owner) {
        let ended: Vec<Owner> = {
            let mut owners = self.sdk_owners().held.lock_or_recover("the SDK owners");
            let (ended, kept) = std::mem::take(&mut *owners).into_iter().partition(|held| held.agent() == owner.agent() && held.ended());
            *owners = kept;
            owners.push(owner);
            ended
        };
        drop(ended);
    }

    fn ask_to_release_host(&self, agent: &AgentId) {
        if let Some(owner) = self.sdk_owners().held.lock_or_recover("the SDK owners").iter().find(|owner| owner.agent() == agent && !owner.ended()) {
            owner.ask_to_release();
        }
    }

    fn stop_sdk_host(&self, agent: &AgentId) -> Result<(), Failed> {
        take_sdk_owner(self.as_runtime(), agent).map_or(Ok(()), |mut owner| owner.stop())
    }

    fn close_sdk_host(&self, agent: &AgentId) -> Result<(), Failed> {
        take_sdk_owner(self.as_runtime(), agent).map_or(Ok(()), |mut owner| owner.close())
    }

    fn halt_sdk_host(&self, agent: &AgentId) -> Option<Result<(), Failed>> {
        take_sdk_owner(self.as_runtime(), agent).map(|mut owner| owner.halt())
    }

    fn after_owner_reserved(&self, hook: impl FnOnce() + Send + 'static) {
        *self.sdk_owners().after_owner_reserved.lock().unwrap() = Some(Box::new(hook));
    }

    fn while_owners_stop(&self, hook: impl FnOnce() + Send + 'static) {
        *self.sdk_owners().while_owners_stop.lock_or_recover("the owners stop hook") = Some(Box::new(hook));
    }

    fn owner_reserved(&self) {
        let hook = self.sdk_owners().after_owner_reserved.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    fn on_takeover_wait(&self, hook: impl FnMut() + Send + 'static) {
        *self.sdk_owners().takeover_waits.lock().unwrap() = Some(Box::new(hook));
    }

    fn takeover_waited(&self) {
        if let Some(hook) = self.sdk_owners().takeover_waits.lock().unwrap().as_mut() {
            hook();
        }
    }

    fn owner_failures(&self) -> std::sync::mpsc::Receiver<String> {
        let (sender, failures) = std::sync::mpsc::channel();
        self.on_owner_failure(move |why| drop(sender.send(why)));
        failures
    }

    fn on_owner_failure(&self, hook: impl Fn(String) + Send + 'static) {
        *self.sdk_owners().owner_failures.lock().unwrap() = Some(Box::new(hook));
    }

    fn owner_failed(&self, why: String) {
        if let Some(hook) = self.sdk_owners().owner_failures.lock().unwrap().as_ref() {
            hook(why);
        }
    }
}

fn take_sdk_owner(domain: &dyn Runtime, agent: &AgentId) -> Option<Owner> {
    let mut owners = domain.sdk_owners().held.lock_or_recover("the SDK owners");
    owners.iter().position(|owner| owner.agent() == agent && !owner.ended())
        .or_else(|| owners.iter().position(|owner| owner.agent() == agent))
        .map(|at| owners.remove(at))
}

/// Takes an SDK execution over again when a committed command asks for it.
pub struct Retakes;

impl crate::domain_runtime::CommitObserver for Retakes {
    fn committed(&self, domain: &dyn Runtime, command: &Command, state: &DomainState) {
        retake_lost(domain, command, state);
        retake_told(domain, command);
    }
}

/// «Try again», chosen on the card of a task whose SDK execution could not be taken over and
/// committed as its answer: the daemon takes the execution over again.
fn retake_lost(domain: &dyn Runtime, command: &Command, state: &DomainState) {
    let Command::AnswerQuestion { interaction_id, option_id, .. } = command else { return };
    if option_id != alondra_domain_rules::RETAKE {
        return;
    }
    let Some(Scope::Task { task_id }) = state.interactions.get(interaction_id).map(|card| &card.scope) else { return };
    retake_manually(domain, task_id.clone());
}

/// Something was told to the node of an SDK task whose execution no owner of this daemon
/// holds: a host that resumes its session takes it over, so what it was told reaches it.
fn retake_told(domain: &dyn Runtime, command: &Command) {
    if let Command::TellNode { task_id, .. } = command {
        take_if_unowned(domain, task_id);
    }
    if let Command::SetModel { task_id, .. } | Command::SetPermissionMode { task_id, .. } | Command::ReadMcpStatus { task_id }
        | Command::ReconnectMcp { task_id, .. } | Command::StartMcpSignIn { task_id, .. } | Command::ClearMcpSignIn { task_id, .. } = command {
        wake_released(domain, task_id);
    }
}

/// The SDK host of `agent` is one a test plays by hand, with no owner thread: it counts as owned.
pub fn drive_by_hand(domain: &dyn Runtime, agent: &AgentId) {
    domain.sdk_owners().driven_by_hand.lock().expect("driven by hand poisoned").insert(agent.clone());
}

/// What the rest of the engine asks of the owners of the SDK hosts.
pub struct Hosts;

impl alondra_engine_ports::hosts::SdkHosts for Hosts {
    fn owns(&self, runtime: &dyn Runtime, agent: &AgentId) -> bool {
        runtime.owns_sdk_host(agent)
    }

    fn node_footprints(&self, runtime: &dyn Runtime) -> std::io::Result<BTreeMap<AgentId, u64>> {
        runtime.node_footprints()
    }

    fn stop(&self, runtime: &dyn Runtime, agent: &AgentId) -> Result<(), Failed> {
        runtime.stop_sdk_host(agent)
    }

    fn requeue(&self, runtime: &dyn Runtime, task: &TaskId) {
        runtime.sdk_owners().requeues.lock_or_recover("the requeued tasks").insert(task.clone());
        runtime.wake_seals();
    }

    fn halt(&self, runtime: &dyn Runtime, task: TaskId, agent: AgentId) {
        halt(runtime, task, agent);
    }

    fn take_if_unowned(&self, runtime: &dyn Runtime, task: &TaskId) {
        take_if_unowned(runtime, task);
    }

    fn take_working_unowned(&self, runtime: &dyn Runtime, state: &DomainState, now: alondra_core::Millis) {
        take_working_unowned(runtime, state, now);
    }

    fn close_hosts_of_ended_nodes(&self, runtime: &dyn Runtime, state: &DomainState) {
        close_hosts_of_ended_nodes(runtime, state);
    }

    fn retake_waiting_for_space(&self, runtime: &dyn Runtime) {
        retake_waiting_for_space(runtime);
    }

    fn original_directory(&self, runtime: &dyn Runtime, task: &TaskId) -> Result<Option<String>, Failed> {
        original_directory(runtime, task)
    }

    fn require_recoverable_session(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, account: &alondra_core::accounts::AccountId, directory: &str) -> Result<(), Failed> {
        let (backend, _) = runtime.sdk().ok_or_else(|| Failed::new(crate::sdk_backend::NO_HOST.into()))?;
        require_recoverable_session(runtime, backend, task, provider, account, directory)
    }

    fn prepare_reopening(&self, runtime: &dyn Runtime, task: &TaskId, provider: Provider, account: &alondra_core::accounts::AccountId, directory: &std::path::Path, input: String, recover_directory: bool) -> Result<alondra_engine_ports::hosts::Reopening, Failed> {
        let (backend, _) = runtime.sdk().ok_or_else(|| Failed::new(crate::sdk_backend::NO_HOST.into()))?;
        prepare_reopening(runtime, backend, task, provider, account, directory, input, recover_directory).map(|reopening| alondra_engine_ports::hosts::Reopening(Box::new(reopening)))
    }

    fn end_unowned_host(&self, runtime: &dyn Runtime, task: TaskId) -> Result<(), Failed> {
        let (backend, domain) = runtime.sdk().ok_or_else(|| Failed::new(crate::sdk_backend::NO_HOST.into()))?;
        end_unowned_host(domain, backend, task)
    }

    fn launch<'a>(&'a self, runtime: &'a dyn Runtime, launch: Launch, reopening: Option<alondra_engine_ports::hosts::Reopening>) -> alondra_engine_ports::hosts::Launched<'a> {
        let reopening = reopening.map(|reopening| *reopening.0.downcast::<Reopening>().expect("a reopening the hosts' owner prepared"));
        Box::pin(launch_prepared(runtime, launch, reopening))
    }

    fn moving(&self, runtime: &dyn Runtime, task: &TaskId) -> bool {
        crate::provider_transition::moving(runtime, task)
    }
}

#[cfg(all(test, feature = "whole-engine", not(feature = "real-hosts")))]
mod pause_tests;
