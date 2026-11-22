//! The journal: where the domain's commands, their effects and the engine's own records are kept.
//! The runtime holds one writer and one reader and speaks to them only through [`Journal`]; the
//! adapter that keeps them on disk implements it and opens them through [`JournalFiles`].
pub mod account_move;
pub mod batch;
pub mod canonical;
pub mod decisions;
pub mod handover;
pub mod metrics;
pub mod owner;
pub mod provider_transition;
pub mod records;
pub mod settings;

use account_move::{AccountMove, Plan as AccountMovePlan};
use alondra_core::Millis;
use alondra_core::metrics::report::MetricsInputs;
use alondra_domain_rules::{
    AttachmentRef,
    Actor, AgentId, Command, CommandEnvelope, CommandOutcome, DataEpoch, DomainError, DomainState, InteractionId, LaunchPlan,
    RequestId, Revision, TaskId,
};
use alondra_store::AuthorityScope;
use anyhow::Result;
use batch::{BatchEffectRef, BatchReport, UnfinishedBatch};
use handover::{Handover, Process};
use metrics::{Decision, SessionStarted, Spent};
use owner::{Adoption, OwnerCommit};
use provider_transition::{Phase, Plan as TransitionPlan, Source};
use crate::agent_host::EndedHost;
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::ProviderBinding;
use alondra_store::{AdapterState, CommittedCommand, CommittedControl, ControlPermit, DispatchPermit, DispatchReceipt, InputPayload, OwnerGrant};
use alondra_domain::TaskLifecycle;
use provider_transition::Transition;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use crate::outbox::OutboxMessage;

pub const SCHEMA_VERSION: i64 = 5;
/// The schema before every task's lifecycle moved into the store's journal: opening it migrates it.
pub const PREVIOUS_SCHEMA_VERSION: i64 = 4;
pub const ORCHESTRATOR_MESSAGE_TTL_MS: i64 = 24 * 60 * 60 * 1_000;

/// The host every orchestrator batch is claimed for.
pub const BATCH_HOST: &str = "alondra-sdk-orchestrator";

/// The file the journal of a HOME is kept in.
pub const DATABASE: &str = "domain.db";

pub fn active_path(home: &Path) -> PathBuf {
    home.join(DATABASE)
}

/// How a HOME's journal is opened, migrated and checked on disk.
pub trait JournalFiles: Send + Sync {
    /// The writer, with the decisions the cards Claude asks for are built with.
    fn open(&self, database: &Path, decisions: Option<Arc<dyn decisions::SdkDecisions>>) -> Result<Box<dyn Journal>>;
    fn open_readonly(&self, database: &Path) -> Result<Box<dyn Journal>>;
    /// Moves an older database to [`DATABASE`]; true when it did. The fence it returns keeps the
    /// source closed to other writers while it is held.
    fn migrate_and_fence(&self, home: &Path) -> Result<(bool, Option<Box<dyn std::any::Any + Send>>)>;
    /// Refuses a HOME whose source database a preflight may not copy.
    fn validate_preflight_source(&self, home: &Path) -> Result<()>;
    /// The files of a migration in progress that a preflight copies with the database.
    fn preflight_staging_files(&self, home: &Path) -> Result<Vec<PathBuf>>;
    /// The name of the migration's marker in HOME.
    fn migration_marker(&self) -> &'static str;
    /// Removes the copies of an older database a migration committed before `now` left behind,
    /// once they are no longer a way back.
    fn remove_recovery_copies(&self, home: &Path, now: std::time::SystemTime) -> Result<()>;
}

/// What an identity proven for a native account says of the account the domain sees now.
pub trait KnownIdentity {
    /// What changed, `identity` or `billing policy`, or none.
    fn change(&self, current: &alondra_core::accounts::AccountStatus) -> Option<&'static str>;
    fn same_known_native_account(&self, provider: alondra_core::accounts::Provider, current: &alondra_core::accounts::AccountStatus) -> bool;
}

/// One connection to the journal: the writer commits commands and records, a reader reads them.
pub trait Journal: Send {
    /// The adapter behind the port, for the tests that read its tables.
    fn as_any(&self) -> &dyn std::any::Any;
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any;
    /// Stages commands and observations in one transaction and commits them together.
    fn own_with(&mut self, scope: AuthorityScope, stage: &mut dyn FnMut(&mut dyn OwnerJournal) -> std::result::Result<(), StoreError>) -> std::result::Result<OwnerCommit, StoreError>;
    fn launched_tasks_of(&self, tasks: &mut dyn Iterator<Item = &TaskId>) -> Result<std::collections::BTreeSet<TaskId>>;
    fn select_batch_value(&mut self, run: &str, selection: &Value) -> Result<()>;
    /// The retention policies, by name, in the order a pass runs them.
    fn retention_policies(&self) -> Vec<&'static str>;
    /// One pass of the retention policy `policy`: how many rows it changed, at most `limit`.
    fn retain(&mut self, policy: &str, now: i64, limit: usize) -> Result<usize>;
    /// Begins a read transaction: every getter until [`Journal::end_read`] reads one commit.
    fn begin_read(&self);
    fn end_read(&self);
    /// Whether the settings change `identity` committed.
    fn settings_committed(&self, identity: &settings::ProjectionIdentity) -> Result<bool>;
    /// Settles a settings change a crash left between the journal and the options file.
    fn recover_settings(&mut self, home: &Path) -> Result<()>;
    fn account_moves(&self) -> Result<Vec<AccountMove>>;
    fn allowed_rules(&self) -> Result<Vec<records::AllowedRule>>;
    fn api_failures(&self) -> Result<std::collections::BTreeMap<AgentId, records::Streak>>;
    fn attachments_kept_elsewhere(&self) -> Result<Vec<(String, String, String, String, String)>>;
    fn auto_launch(&self, task_id: &TaskId) -> Result<Option<AutoLaunch>>;
    fn auto_launches(&self) -> Result<Vec<AutoLaunch>>;
    fn batch_completed(&self, run: &str) -> Result<bool>;
    fn batch_effects(&self, run: &str) -> Result<Vec<BatchEffectRef>>;
    /// How many new Tasks the commands of `run` created.
    fn tasks_created_in(&mut self, run: &str) -> Result<usize>;
    fn batch_outcomes(&self, run: &str, epoch: &DataEpoch, after: &str, limit: usize) -> Result<Value>;
    fn batch_run_manifest(&self, epoch: &DataEpoch, after: &str, limit: usize) -> Result<Value>;
    fn batch_selection(&self, run: &str) -> Result<Option<records::BatchSelection>>;
    fn batch_stop(&self, run: &str) -> Result<Option<(alondra_domain_rules::orchestrator_batch::BatchStop, String)>>;
    fn begin_task_closing(&mut self, task_id: &TaskId, now: i64) -> Result<TaskClosing>;
    fn begin_tool_operation(&mut self, run: &str, call: &str, kind: &str, request: &Value) -> Result<Option<(String,Option<String>,Option<String>)>>;
    fn bind_batch_effects(&mut self, run: &str, epoch: &DataEpoch) -> Result<()>;
    fn cancel_reasons(&self) -> Result<std::collections::BTreeMap<TaskId, String>>;
    fn change_launch_plan(&mut self, task_id: &TaskId, plan: &LaunchPlan, changes: Option<&serde_json::Value>, now: i64) -> Result<()>;
    fn claim_auto_launch(&mut self, task_id: &TaskId, now: i64, retry: bool) -> Result<bool>;
    fn claim_message(&mut self, id: i64) -> Result<Option<DeliveryClaim>>;
    fn claim_orchestrator_batch(&mut self, ids: &[i64], run: &str, session: &str, epoch: alondra_domain_rules::DataEpoch) -> Result<(Vec<DeliveryClaim>, BatchOwner)>;
    fn claim_selected_orchestrator_batch(&mut self, ids: &[i64], run: &str, session: &str, epoch: alondra_domain_rules::DataEpoch, selection: Option<&records::BatchSelection>) -> Result<(Vec<DeliveryClaim>, BatchOwner)>;
    fn claim_task_removal(&mut self, task_id: &TaskId, now: i64) -> Result<bool>;
    fn complete_tool_operation(&mut self, run: &str, call: &str, answer: &std::result::Result<Value,DomainError>) -> Result<()>;
    fn conversation_directory(&self, task: &TaskId, conversation: &str) -> Result<Option<String>>;
    fn conversation_metadata(&self, task: &TaskId, conversation: &str) -> Result<Option<Vec<u8>>>;
    fn conversations_metadata(&self, task: &TaskId) -> Result<Vec<Vec<u8>>>;
    fn deferred_move(&self, task:&TaskId) ->Result<Option<Transition>>;
    fn deferred_moves(&self) ->Result<Vec<Transition>>;
    fn delivery_evidence(&self) -> Result<Vec<DeliveryEvidence>>;
    fn delivery_evidence_page(&self, after: i64) -> Result<Vec<DeliveryEvidenceItem>>;
    fn dequeue_launch(&mut self, task_id: &TaskId) -> Result<()>;
    fn discard_expired_orchestrator_messages(&mut self, cutoff: i64) -> Result<usize>;
    fn ended_tasks_compacted(&self) -> Result<()>;
    fn enqueue_launch(&mut self, task_id: &TaskId, body: &serde_json::Value, origin: &str, now: i64) -> Result<()>;
    fn execute_authenticated(&mut self, actor: &Actor, envelope: &CommandEnvelope, transport: Option<TransportIdentity<'_>>) -> std::result::Result<Commit, StoreError>;
    fn execute_filling_revision(&mut self, actor: &Actor, envelope: &CommandEnvelope) -> std::result::Result<Commit, StoreError>;
    fn execute_from_batch(&mut self, actor: &Actor, envelope: &CommandEnvelope, run: &str) -> std::result::Result<Commit, StoreError>;
    fn execute_launching(&mut self, actor: &Actor, envelope: &CommandEnvelope, transport: Option<TransportIdentity<'_>>, launch: &RequestLaunch) -> std::result::Result<Commit, StoreError>;
    fn fill_task_models(&mut self) -> Result<usize>;
    fn fill_task_projects(&mut self, root: &Path) -> Result<usize>;
    fn finish_batch(&mut self, run: &str) -> Result<()>;
    fn finish_message(&mut self, attempt_id: i64, version: u64, state: alondra_core::delivery::State, reason: Option<&str>) -> Result<bool>;
    fn fold_ended_tasks(&mut self, limit: usize) -> Result<usize>;
    fn forget_auto_launch(&mut self, task_id: &TaskId) -> Result<()>;
    fn forget_inherited_worktree(&mut self, agent_id: &str) -> Result<()>;
    fn forget_planless_blocks(&mut self) -> Result<usize>;
    fn forget_post_merge_check(&mut self, task_id: &TaskId) -> Result<()>;
    fn forget_task_closing(&mut self, task_id: &TaskId) -> Result<()>;
    fn hand_carried_message(&mut self, id: i64) -> Result<()>;
    fn handed_user_notices(&self, interaction_ids: &[InteractionId]) -> Result<Vec<(i64, InteractionId)>>;
    fn handovers(&self) -> Result<Vec<Handover>>;
    fn inherited_worktree(&self, agent_id: &str) -> Result<Option<String>>;
    fn integrity_check(&self) -> Result<()>;
    fn keep_attachment_at(&mut self, interaction: &str, id: &str, reference: &str) -> Result<()>;
    fn launch_plan(&self, task_id: &TaskId) -> Result<Option<LaunchPlan>>;
    fn launch_queue(&self) -> Result<Vec<QueuedLaunch>>;
    fn launch_refusals(&self) -> Result<Vec<(TaskId, String, Option<String>)>>;
    fn launched_by_alondra(&self, agent_id: &str) -> Result<bool>;
    fn load(&mut self) -> Result<Arc<DomainState>>;
    fn message_origin(&self, id: i64) -> Result<Option<(i64, String)>>;
    fn metrics_inputs(&self, now: Millis) -> Result<MetricsInputs>;
    fn orchestrator_delivery(&self, attempt_id: i64) -> Result<Option<String>>;
    fn orchestrator_notices_out(&self) -> Result<Vec<String>>;
    /// Whether a batch that ended with its result was handed the orchestrator notice of `interaction_id`.
    fn orchestrator_notice_read(&self, interaction_id: &str) -> Result<bool>;
    fn pending_messages(&self) -> Result<Vec<OutboxMessage>>;
    fn plan_account_move(&mut self, _task: &TaskId, _plan: &AccountMovePlan) -> Result<i64>;
    fn planned_tasks(&self) -> Result<std::collections::BTreeMap<TaskId, i64>>;
    fn post_merge_check(&self, task_id: &TaskId) -> Result<Option<PostMergeCheck>>;
    fn protect(&mut self, protected: crate::worktrees::WorktreeRules);
    fn protected(&self) -> &crate::worktrees::WorktreeRules;
    fn provider_transition(&self, task:&TaskId) ->Result<Option<Transition>>;
    fn latest_provider_transition(&self, task:&TaskId) ->Result<Option<Transition>>;
    fn provider_transition_finished(&self, id:&str) ->Result<bool>;
    fn provider_transitions(&self) ->Result<Vec<Transition>>;
    fn prs_without_size(&self, now: Millis, limit: usize) -> Result<Vec<(String, String)>>;
    fn public_task_context(&self, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> Result<alondra_store::ContextPage>;
    fn read_mark(&self) -> Result<(u64, Revision)>;
    /// Where this reader stands in what it reads; fails unless a read transaction is active.
    fn read_cursor(&self) -> Result<ReadCursor>;
    fn reclaim_space(&self, pages: u32) -> Result<()>;
    fn reconcile_orchestrator_batch(&mut self, run: &str, continuation: bool) -> Result<()>;
    fn record_allowed_rule(&self, rule: &records::AllowedRule) -> Result<()>;
    fn record_batch_report(&mut self, run: &str, cursor: u64, evidence: &[u8], report: &BatchReport) -> Result<()>;
    fn record_bg_stopped(&mut self, node: &AgentId, task: &TaskId, exited: Option<Process>) -> Result<()>;
    fn record_handover_done(&mut self, node: &AgentId, task: &TaskId) -> Result<()>;
    fn record_model_decision(&mut self, d: &Decision) -> Result<()>;
    fn record_move_done(&mut self, task: &TaskId, number: i64) -> Result<()>;
    fn record_move_host_stopped(&mut self, task: &TaskId, number: i64) -> Result<()>;
    fn record_move_undone(&mut self, task: &TaskId, number: i64, why: &str) -> Result<()>;
    fn record_pr_lines(&mut self, task_id: &str, lines: i64) -> Result<()>;
    fn record_release(&mut self, version: &str, at: Millis) -> Result<()>;
    fn record_spend(&mut self, spent: &Spent<'_>) -> Result<()>;
    fn record_session_start(&mut self, started: &SessionStarted<'_>) -> Result<()>;
    fn session_starts(&self, task_id: &str) -> Result<Vec<metrics::RecordedStart>>;
    fn refused_move(&self, task:&TaskId) ->Result<Option<Transition>>;
    fn refused_moves(&self) ->Result<Vec<Transition>>;
    fn request_launch(&mut self, request_id: &RequestId) -> Result<Option<RequestLaunch>>;
    fn request_launch_summaries(&self) -> Result<std::collections::BTreeMap<RequestId, (Option<String>, Option<String>)>>;
    fn request_times(&self) -> Result<std::collections::BTreeMap<RequestId, i64>>;
    fn resolve_uncertain(&mut self, attempt_id: i64, expected_version: u64, retry: bool) -> Result<bool>;
    fn resolve_uncertain_authenticated(&mut self, attempt_id: i64, expected_version: u64, epoch: &str, retry: bool, transport: TransportIdentity<'_>) -> std::result::Result<Value, StoreError>;
    fn retain_evidence(&mut self, limit: usize) -> std::result::Result<alondra_store::EvidenceRetention, StoreError>;
    fn revision_times(&self, revisions: &[Revision]) -> Result<std::collections::BTreeMap<Revision, i64>>;
    fn rows_written(&self) -> u64;
    fn save_task_closing(&mut self, row: &TaskClosing) -> Result<()>;
    fn schedule_request_tools(&mut self, request_id: &RequestId) -> Result<Option<Vec<String>>>;
    fn set_allowed_rule_state(&self, id: i64, state: records::RuleState) -> Result<()>;
    fn set_api_failures(&self, agent: &AgentId, streak: Option<&records::Streak>) -> Result<()>;
    fn set_auto_launch(&mut self, task_id: &TaskId, state: AutoLaunchState, reason: Option<AutoLaunchReason>, detail: Option<&str>, now: i64) -> Result<()>;
    fn set_cancel_reason(&self, task_id: &TaskId, reason: &str) -> Result<()>;
    fn set_inherited_worktree(&mut self, agent_id: &str, dir: &str, from_agent: &str, now: i64) -> Result<()>;
    fn set_launch_refusal(&mut self, task_id: &TaskId, refusal: Option<(&str, Option<&str>)>, now: i64) -> Result<()>;
    fn set_start_retry(&mut self, task_id: &TaskId, retry: Option<(u8, i64)>) -> Result<()>;
    fn set_stop_blocks(&self, task_id: &TaskId, blocks: u32, marker: i64) -> Result<()>;
    fn set_task_worktree(&mut self, task_id: &TaskId, dir: &str, now: i64) -> Result<()>;
    fn settle_orchestrator_batch(&mut self, run: &str, target: alondra_core::delivery::State, reason: Option<&str>) -> Result<bool>;
    fn start_retries(&self) -> Result<std::collections::BTreeMap<TaskId, (u8, i64)>>;
    fn stop_blocks(&self, task_id: &TaskId) -> Result<Option<(u32, i64)>>;
    fn task_closing(&self, task_id: &TaskId) -> Result<Option<TaskClosing>>;
    fn task_closings(&self) -> Result<Vec<TaskClosing>>;
    fn task_history(&self, task_id: &TaskId) -> Result<Vec<HistoryStep>>;
    fn task_roles(&self) -> Result<std::collections::BTreeMap<TaskId, String>>;
    fn task_spend(&self, task: &str) -> Result<(BTreeMap<String, i64>, Option<i64>)>;
    fn task_started_times(&self) -> Result<std::collections::BTreeMap<TaskId, i64>>;
    fn task_state_times(&self) -> Result<std::collections::BTreeMap<TaskId, i64>>;
    fn task_was_launched(&self, task_id: &TaskId) -> Result<bool>;
    fn task_worktree(&self, task_id: &TaskId) -> Result<Option<String>>;
    fn task_worktrees(&self) -> Result<Vec<(TaskId, String)>>;
    fn tool_operations(&self, run: &str) -> Result<Vec<(String,String,String,Option<String>,Option<String>)>>;
    fn transport_receipt(&mut self, identity: TransportIdentity<'_>) -> std::result::Result<Option<CommandOutcome>, StoreError>;
    fn unfinished_orchestrator_batches(&self, epoch: &DataEpoch, active: Option<&str>, retained: Option<&str>) -> Result<Vec<UnfinishedBatch>>;
    fn user_notices_in_open_passes(&self) -> Result<std::collections::BTreeSet<i64>>;
    fn user_push_handed(&self, interaction_ids: &[InteractionId]) -> Result<bool>;
    fn user_push_history(&self, message_id: i64) -> Result<UserPushHistory>;
    fn was_schedule_launch_failed(&self, request_id: &RequestId) -> Result<bool>;
    fn worktree_storage(&self, path: &str) -> Result<Option<records::Repository>>;
    fn worktree_storage_at(&self, path: &str) -> Result<Option<(std::path::PathBuf, records::Repository)>>;
    fn write_mark(&self) -> Result<(u64, Revision)>;
}

impl dyn Journal + '_ {
    /// Stages commands and observations in one transaction and commits them together.
    pub fn own<T>(&mut self, scope: AuthorityScope, stage: impl FnOnce(&mut dyn OwnerJournal) -> std::result::Result<T, StoreError>) -> std::result::Result<(T, OwnerCommit), StoreError> {
        let mut stage = Some(stage);
        let mut value = None;
        let commit = self.own_with(scope, &mut |owner| {
            let stage = stage.take().expect("an owner transaction stages once");
            value = Some(stage(owner)?);
            Ok(())
        })?;
        Ok((value.expect("a committed owner transaction staged its value"), commit))
    }

    pub fn launched_tasks<'a>(&self, tasks: impl IntoIterator<Item = &'a TaskId>) -> Result<std::collections::BTreeSet<TaskId>> {
        self.launched_tasks_of(&mut tasks.into_iter())
    }

    pub fn select_batch(&mut self, run: &str, selection: &impl Serialize) -> Result<()> {
        self.select_batch_value(run, &serde_json::to_value(selection)?)
    }
}

/// Opaque and local to one reader: two cursors are equal only when nothing this reader reads
/// changed between them, and a replaced connection never equals the one it replaced.
#[derive(Clone, Debug)]
pub struct ReadCursor {
    identity: std::sync::Arc<()>,
    mark: (u64, Revision),
    local_writes: u64,
}

impl ReadCursor {
    pub fn new(identity: std::sync::Arc<()>, mark: (u64, Revision), local_writes: u64) -> Self {
        Self { identity, mark, local_writes }
    }
}

impl PartialEq for ReadCursor {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.identity, &other.identity)
            && self.mark == other.mark && self.local_writes == other.local_writes
    }
}
impl Eq for ReadCursor {}

/// One read transaction on a reader connection: every getter called through it reads the same
/// committed state, never part of a commit. It ends when dropped.
pub struct ReadTransaction<'a>(std::sync::MutexGuard<'a, Box<dyn Journal>>);

impl<'a> ReadTransaction<'a> {
    pub fn begin(journal: std::sync::MutexGuard<'a, Box<dyn Journal>>) -> Self {
        journal.begin_read();
        Self(journal)
    }
}

impl std::ops::Deref for ReadTransaction<'_> {
    type Target = dyn Journal;
    fn deref(&self) -> &(dyn Journal + 'static) {
        &**self.0
    }
}

impl std::ops::DerefMut for ReadTransaction<'_> {
    fn deref_mut(&mut self) -> &mut (dyn Journal + 'static) {
        &mut **self.0
    }
}

impl Drop for ReadTransaction<'_> {
    fn drop(&mut self) {
        self.0.end_read();
    }
}

/// The transaction an owner stages its commands, observations and provider records in.
pub trait OwnerJournal {
    /// Runs `step` on the lifecycle journal this transaction stages in.
    fn with_lifecycle(&mut self, step: &mut dyn FnMut(&mut alondra_store::LifecycleTransaction<'_>) -> std::result::Result<(), StoreError>) -> std::result::Result<(), StoreError>;
    fn stage_observation_with_identity(&mut self, binding: &alondra_provider_contract::ProviderBinding, owner_generation: u64, batch: &alondra_provider_contract::OrderedObservation, normalizer: &dyn alondra_agent_application::ObservationNormalizer, home: &Path, probe: bool, identity: Option<&dyn KnownIdentity>) -> std::result::Result<alondra_store::ObservationAcceptance, StoreError>;
    fn discard_observation_by(&mut self, binding: &alondra_provider_contract::ProviderBinding, owner_generation: u64, batch: &alondra_provider_contract::OrderedObservation, normalizer: &dyn alondra_agent_application::ObservationNormalizer, reason: owner::DiscardReason) -> std::result::Result<alondra_store::ObservationAcceptance, StoreError>;
    fn launch_with(&mut self, task: &TaskId, text: &str, directory: &str, binding: &alondra_provider_contract::ProviderBinding, provider: &mut dyn owner::Preparer) -> std::result::Result<alondra_foundation::ExecutionStamp, StoreError>;
    fn record_command_with(&mut self, attempt: alondra_store::DispatchAttempt, owner_generation: u64, provider: &mut dyn owner::Preparer) -> std::result::Result<(), StoreError>;
    fn record_control_with(&mut self, attempt: alondra_store::ControlAttempt, owner_generation: u64, prepare: &mut dyn FnMut(&alondra_store::ControlAttempt) -> std::result::Result<alondra_provider_contract::PreparedCommand, alondra_provider_contract::AdapterError>) -> std::result::Result<(), StoreError>;
    fn adapter_state(&self, grant: &OwnerGrant) -> std::result::Result<AdapterState, StoreError>;
    fn admit_provider_transition(&mut self, id:&str, stamp:&alondra_foundation::ExecutionStamp) ->std::result::Result<(),StoreError>;
    fn adopt(&mut self, task: &TaskId) -> std::result::Result<Adoption, StoreError>;
    fn claim(&mut self, permit: DispatchPermit) -> std::result::Result<(), StoreError>;
    fn claim_control(&mut self, permit: ControlPermit) -> std::result::Result<bool, StoreError>;
    fn claim_control_for_identity(&mut self, permit:ControlPermit, directory:&std::path::Path) ->std::result::Result<bool,StoreError>;
    fn claim_is_open(&self, task: &TaskId, intent: i64) -> std::result::Result<bool, StoreError>;
    fn claims_left(&mut self, task: &TaskId) -> std::result::Result<bool, StoreError>;
    fn configure_transition_model(&mut self, plan:&TransitionPlan) ->std::result::Result<bool,StoreError>;
    fn domain_state(&self) -> &DomainState;
    fn execution_binding(&self, stamp: &ExecutionStamp) -> std::result::Result<ProviderBinding, StoreError>;
    fn lifecycle(&self, task: &TaskId) -> std::result::Result<std::sync::Arc<TaskLifecycle>, StoreError>;
    fn normalization_checkpoint(&self, stamp: &alondra_foundation::ExecutionStamp) -> std::result::Result<Option<alondra_store::NormalizationCheckpoint>, StoreError>;
    fn observe_provider_transition(&mut self, stamp:&alondra_foundation::ExecutionStamp, cursor:u64) ->std::result::Result<(),StoreError>;
    fn pending_handoffs(&self, task: &TaskId) -> std::result::Result<Vec<(InputPayload, String)>, StoreError>;
    fn pending_transition_selection(&mut self, id:&str) ->std::result::Result<Option<alondra_core::model_selection::PendingTaskSelection>,StoreError>;
    fn pin_context(&mut self, transfer: &str, through: &alondra_store::ContextRef) -> std::result::Result<(),StoreError>;
    fn provider_transition_blocks(&mut self, stamp:&alondra_foundation::ExecutionStamp) ->std::result::Result<bool,StoreError>;
    fn provider_transition_phase(&mut self, id:&str, expected:Phase, next:Phase, facts:serde_json::Value) ->std::result::Result<(),StoreError>;
    fn record_control_receipt(&mut self, control: CommittedControl, receipt: DispatchReceipt) -> std::result::Result<(), StoreError>;
    fn record_handoff_proof(&mut self, grant: &OwnerGrant, intent: i64, proof: &alondra_provider_contract::HandoffProof) -> std::result::Result<bool, StoreError>;
    fn record_move_admitted(&mut self, task: &TaskId, number: i64, agent: &AgentId) -> std::result::Result<(), StoreError>;
    fn record_receipt(&mut self, command: CommittedCommand, receipt: DispatchReceipt) -> std::result::Result<(), StoreError>;
    fn record_sdk_admitted(&mut self, node: &AgentId, task: &TaskId, agent: &AgentId) -> std::result::Result<(), StoreError>;
    fn record_settings_projection(&mut self, identity: &settings::ProjectionIdentity) -> std::result::Result<(),StoreError>;
    fn refusal(&self, envelope: &CommandEnvelope) -> Option<DomainError>;
    fn reissue_permit(&mut self, task: &TaskId) -> std::result::Result<(), StoreError>;
    fn release_context(&mut self, transfer: &str, task: &TaskId) -> std::result::Result<(),StoreError>;
    fn request_provider_transition(&mut self, plan:&TransitionPlan) ->std::result::Result<(),StoreError>;
    fn require_native_identity(&self, binding:&ProviderBinding, expected:Option<&dyn KnownIdentity>) ->std::result::Result<(),StoreError>;
    fn resolve(&mut self, stamp: &ExecutionStamp, intent: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError>;
    fn resolve_control(&mut self, stamp: &ExecutionStamp, id: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError>;
    fn retain_evidence(&mut self, limit: usize) -> std::result::Result<alondra_store::EvidenceRetention, StoreError>;
    fn retained_context(&self, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> std::result::Result<alondra_store::ContextPage,StoreError>;
    fn selection_source(&mut self, task:&TaskId, host_directory:std::path::PathBuf) ->std::result::Result<Source,StoreError>;
    fn source_fenced(&mut self, plan:&TransitionPlan, proof:&EndedHost) ->std::result::Result<(),StoreError>;
    fn source_inputs_unoffered(&mut self, task:&TaskId) ->std::result::Result<bool,StoreError>;
    fn stage_command(&mut self, actor: &Actor, envelope: &CommandEnvelope, attached: Attached<'_>) -> std::result::Result<&CommandOutcome, StoreError>;
    fn state(&self) -> &DomainState;
    fn transition_launch_text(&mut self, task:&TaskId, fallback:&str) ->std::result::Result<String,StoreError>;
    fn transition_model_control_status(&mut self, plan: &TransitionPlan) -> std::result::Result<Option<String>, StoreError>;
    fn unrecorded(&self) -> &[String];
    fn validate_selection_binding(&mut self, plan:&TransitionPlan) ->std::result::Result<(),StoreError>;
    fn validate_selection_source(&mut self, plan:&TransitionPlan) ->std::result::Result<(),StoreError>;
    fn work_is_over(&self, task: &alondra_domain_rules::TaskId) -> bool;
}

impl dyn OwnerJournal + '_ {
    pub fn stage_observation(&mut self, binding: &alondra_provider_contract::ProviderBinding, owner_generation: u64, batch: &alondra_provider_contract::OrderedObservation, normalizer: &impl alondra_agent_application::ObservationNormalizer, home: &Path, probe: bool) -> std::result::Result<alondra_store::ObservationAcceptance, StoreError> {
        self.stage_observation_with_identity(binding, owner_generation, batch, normalizer, home, probe, None)
    }

    pub fn discard_observation(&mut self, binding: &alondra_provider_contract::ProviderBinding, owner_generation: u64, batch: &alondra_provider_contract::OrderedObservation, normalizer: &impl alondra_agent_application::ObservationNormalizer, reason: owner::DiscardReason) -> std::result::Result<alondra_store::ObservationAcceptance, StoreError> {
        self.discard_observation_by(binding, owner_generation, batch, normalizer, reason)
    }

    pub fn launch(&mut self, task: &TaskId, text: &str, directory: &str, binding: &alondra_provider_contract::ProviderBinding, provider: &mut impl owner::Preparer) -> std::result::Result<alondra_foundation::ExecutionStamp, StoreError> {
        self.launch_with(task, text, directory, binding, provider)
    }

    pub fn record_command(&mut self, attempt: alondra_store::DispatchAttempt, owner_generation: u64, provider: &mut impl owner::Preparer) -> std::result::Result<(), StoreError> {
        self.record_command_with(attempt, owner_generation, provider)
    }

    pub fn record_control(&mut self, attempt: alondra_store::ControlAttempt, owner_generation: u64, prepare: impl FnOnce(&alondra_store::ControlAttempt) -> std::result::Result<alondra_provider_contract::PreparedCommand, alondra_provider_contract::AdapterError>) -> std::result::Result<(), StoreError> {
        let mut prepare = Some(prepare);
        self.record_control_with(attempt, owner_generation, &mut |attempt| (prepare.take().expect("a control is prepared once"))(attempt))
    }
}

/// A check an earlier daemon kept on work a node declared after the merge: the card it may have
/// raised asking the user whether to close the node.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostMergeCheck {
    pub problem_id: Option<String>,
}

/// What the closer remembers about closing one task's node, across restarts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskClosing {
    pub task_id: TaskId,
    /// Milliseconds: when the closer first saw the task due for closing.
    pub since: i64,
    pub failures: u32,
    pub last_error: Option<String>,
    /// Milliseconds: no attempt before this.
    pub next_attempt: i64,
    /// The task's `wrap_up.cleaned_at` seen at the last failed attempt.
    pub attempted_cleaned_at: Option<i64>,
    /// Milliseconds: `claude rm` was started and its result not recorded.
    pub rm_started_at: Option<i64>,
    /// Milliseconds: when the closer first found the node ready to close but still working.
    pub ready_since: Option<i64>,
    /// The Problem card raised after three failures.
    pub problem_id: Option<String>,
}

/// What a command carries into the transaction that commits it.
pub enum Attached<'a> {
    None,
    /// What the user chose for the launch of a new request or card reply.
    Request(&'a RequestLaunch),
    Batch(&'a batch::BatchCommand<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoLaunchState {
    Launching,
    Launched,
    Blocked,
}

impl AutoLaunchState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Launching => "launching",
            Self::Launched => "launched",
            Self::Blocked => "blocked",
        }
    }
    pub fn parse(text: &str) -> Self {
        match text {
            "launched" => Self::Launched,
            "blocked" => Self::Blocked,
            _ => Self::Launching,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AutoLaunchReason {
    UsageExhausted,
    WorkspaceUntrusted,
    LaunchFailed,
}

impl AutoLaunchReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UsageExhausted => "usage_exhausted",
            Self::WorkspaceUntrusted => "workspace_untrusted",
            Self::LaunchFailed => "launch_failed",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "usage_exhausted" => Self::UsageExhausted,
            "workspace_untrusted" => Self::WorkspaceUntrusted,
            "launch_failed" => Self::LaunchFailed,
            _ => return None,
        })
    }
}

/// A launch waiting at the gate: the launch as asked, who asked and since when.
#[derive(Clone, Debug, PartialEq)]
pub struct QueuedLaunch {
    pub task_id: TaskId,
    pub body: serde_json::Value,
    pub origin: String,
    pub enqueued_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AutoLaunch {
    pub task_id: TaskId,
    pub state: AutoLaunchState,
    pub reason: Option<AutoLaunchReason>,
    pub detail: Option<String>,
    pub at: i64,
}

pub type TransportIdentity<'a> = (&'a str, &'a str, &'a [u8; 32]);

/// What the user chose when asking for new work, kept with the Request it created.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RequestLaunch {
    pub project: Option<String>,
    pub options: alondra_core::options::Resolved,
    pub issue: Option<String>,
    pub issue_text: Option<String>,
    pub attachments: Vec<AttachmentRef>,
}

/// A full disk anywhere in the error's chain, or SQLite saying its database is full.
pub fn no_space(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(crate::disk::io_no_space) || alondra_store::disk_full(cause)
    })
}

#[derive(Debug)]
pub enum StoreError {
    Domain(DomainError),
    Storage(anyhow::Error),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Domain(e) => e.fmt(f),
            Self::Storage(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for StoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Domain(error) => Some(error),
            Self::Storage(error) => Some(error.as_ref()),
        }
    }
}

impl From<anyhow::Error> for StoreError {
    fn from(e: anyhow::Error) -> Self {
        Self::Storage(e)
    }
}

impl From<serde_json::Error> for StoreError {
    fn from(e: serde_json::Error) -> Self {
        Self::Storage(e.into())
    }
}

impl From<DomainError> for StoreError {
    fn from(e: DomainError) -> Self {
        Self::Domain(e)
    }
}

#[derive(Debug)]
pub struct Commit {
    pub state: Arc<DomainState>,
    pub outcome: CommandOutcome,
    pub replayed: bool,
    /// What the store issued with the commit: the dispatches an SDK execution's owner claims.
    pub sealed: Option<alondra_store::Committed>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UserPushHistory {
    pub card_pushed_at: Option<u64>,
    pub sibling_pushed: bool,
}

/// The owner of one orchestrator batch's SDK host. Only the claim of the batch's messages issues
/// it (`DomainStore::claim_orchestrator_batch`), in the same transaction; it is never cloned.
#[derive(Debug)]
pub struct BatchOwner {
    run: String,
    session: String,
    epoch: alondra_domain_rules::DataEpoch,
}

impl BatchOwner {
    pub fn issued(run: &str, session: &str, epoch: alondra_domain_rules::DataEpoch) -> Self {
        Self { run: run.into(), session: session.into(), epoch }
    }
    pub fn run(&self) -> &str {
        &self.run
    }
    pub fn session(&self) -> &str { &self.session }
    pub fn epoch(&self) -> &alondra_domain_rules::DataEpoch { &self.epoch }
    pub fn host_name(&self) -> &str { BATCH_HOST }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryClaim {
    pub attempt_id: i64,
    pub version: u64,
    pub message: OutboxMessage,
}

/// One step of a Task's history.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct HistoryStep {
    pub at: i64,
    pub state: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryEvidence {
    pub attempt_id: i64,
    pub message_id: i64,
    pub state: String,
    pub version: u64,
    pub reason: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeliveryEvidenceItem {
    pub attempt_id: i64,
    pub message_id: i64,
    pub state: String,
    pub current_state: String,
    pub version: u64,
    pub reason: Option<String>,
    pub recipient_kind: String,
    pub task_id: Option<String>,
    pub interaction_id: Option<String>,
}

pub fn generation_provider(state: &DomainState, account: &alondra_core::accounts::AccountKey) -> u64 {
    state.accounts.iter().find(|status| status.provider == account.provider && status.id == account.account).map_or(0, |status| status.sign_ins) + 1
}
pub fn schedule_command(command: &Command) -> bool {
    matches!(
        command,
        Command::CreateSchedule { .. }
            | Command::UpdateSchedule { .. }
            | Command::DeleteSchedule { .. }
            | Command::ClaimScheduleRun { .. }
    )
}
