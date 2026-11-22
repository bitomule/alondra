//! The SQLite journal behind the engine's `Journal` port.
use super::*;
use alondra_engine_ports::journal::account_move::Plan as AccountMovePlan;
use alondra_engine_ports::journal::batch::{BatchEffectRef, BatchReport, UnfinishedBatch};
use alondra_engine_ports::journal::handover::{Handover, Process};
use alondra_engine_ports::journal::metrics::{Decision, Spent};
use alondra_engine_ports::journal::owner::{Adoption, DiscardReason, OwnerCommit, Preparer};
use alondra_engine_ports::journal::provider_transition::{Phase, Plan as TransitionPlan, Source, Transition};
use alondra_engine_ports::journal::{records, settings, Journal, JournalFiles, KnownIdentity, OwnerJournal};
use alondra_engine_ports::agent_host::EndedHost;
use alondra_agent_application::ObservationNormalizer;
use alondra_core::metrics::report::MetricsInputs;
use alondra_domain::TaskLifecycle;
use alondra_foundation::ExecutionStamp;
use alondra_provider_contract::{AdapterError, OrderedObservation, PreparedCommand, ProviderBinding};
use alondra_store::{AdapterState, AuthorityScope, CommittedCommand, CommittedControl, ControlAttempt, ControlPermit, DispatchAttempt, DispatchPermit, DispatchReceipt, InputPayload, ObservationAcceptance, OwnerGrant};
use std::collections::BTreeMap;

/// The SQLite store behind a journal, for the tests that read its tables or drive its test hooks.
pub trait AsSqlite {
    fn sqlite(&self) -> &DomainStore;
    fn sqlite_mut(&mut self) -> &mut DomainStore;
}

impl AsSqlite for dyn Journal + '_ {
    fn sqlite(&self) -> &DomainStore {
        self.as_any().downcast_ref().expect("the journal is the SQLite store")
    }
    fn sqlite_mut(&mut self) -> &mut DomainStore {
        self.as_any_mut().downcast_mut().expect("the journal is the SQLite store")
    }
}

/// The journal of a HOME kept in SQLite.
pub struct Sqlite;

impl JournalFiles for Sqlite {
    fn open(&self, database: &Path, decisions: Option<Arc<dyn wording::SdkDecisions>>) -> Result<Box<dyn Journal>> {
        let store = DomainStore::open(database)?;
        Ok(Box::new(match decisions {
            Some(decisions) => store.with_decisions(decisions),
            None => store,
        }))
    }
    fn open_readonly(&self, database: &Path) -> Result<Box<dyn Journal>> {
        Ok(Box::new(DomainStore::open_readonly(database)?))
    }
    fn migrate_and_fence(&self, home: &Path) -> Result<(bool, Option<Box<dyn std::any::Any + Send>>)> {
        let (migrated, fence) = domain_database_migration::migrate_and_fence(home)?;
        Ok((migrated, fence.map(|fence| Box::new(fence) as Box<dyn std::any::Any + Send>)))
    }
    fn validate_preflight_source(&self, home: &Path) -> Result<()> {
        domain_database_migration::validate_preflight_source(home)
    }
    fn preflight_staging_files(&self, home: &Path) -> Result<Vec<std::path::PathBuf>> {
        domain_database_migration::preflight_staging_files(home)
    }
    fn migration_marker(&self) -> &'static str {
        domain_database_migration::MARKER
    }
    fn remove_recovery_copies(&self, home: &Path, now: std::time::SystemTime) -> Result<()> {
        recovery_copies::remove_recovery_copies(home, now)
    }
}

impl Journal for DomainStore {
    fn as_any(&self) -> &dyn std::any::Any { self }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any { self }
    fn own_with(&mut self, scope: AuthorityScope, stage: &mut dyn FnMut(&mut dyn OwnerJournal) -> std::result::Result<(), StoreError>) -> std::result::Result<OwnerCommit, StoreError> {
        DomainStore::own(self, scope, |owner| stage(owner)).map(|((), commit)| commit)
    }
    fn launched_tasks_of(&self, tasks: &mut dyn Iterator<Item = &TaskId>) -> Result<std::collections::BTreeSet<TaskId>> { DomainStore::launched_tasks(self, tasks) }
    fn select_batch_value(&mut self, run: &str, selection: &Value) -> Result<()> { DomainStore::select_batch(self, run, selection) }
    fn retention_policies(&self) -> Vec<&'static str> { retention::POLICIES.iter().map(|policy| policy.name).collect() }
    fn retain(&mut self, policy: &str, now: i64, limit: usize) -> Result<usize> {
        let policy = retention::POLICIES.iter().find(|known| known.name == policy).with_context(|| format!("no retention policy {policy}"))?;
        DomainStore::retain(self, policy, now, limit)
    }
    fn begin_read(&self) { DomainStore::begin_read(self); }
    fn end_read(&self) { DomainStore::end_read(self) }
    fn settings_committed(&self, identity: &settings::ProjectionIdentity) -> Result<bool> { provider_settings::committed(self, identity) }
    fn recover_settings(&mut self, home: &Path) -> Result<()> { provider_settings::recover_store(self, home) }
    fn account_moves(&self) -> Result<Vec<AccountMove>> { DomainStore::account_moves(self) }
    fn allowed_rules(&self) -> Result<Vec<records::AllowedRule>> { DomainStore::allowed_rules(self) }
    fn api_failures(&self) -> Result<std::collections::BTreeMap<AgentId, records::Streak>> { DomainStore::api_failures(self) }
    fn attachments_kept_elsewhere(&self) -> Result<Vec<(String, String, String, String, String)>> { DomainStore::attachments_kept_elsewhere(self) }
    fn auto_launch(&self, task_id: &TaskId) -> Result<Option<AutoLaunch>> { DomainStore::auto_launch(self, task_id) }
    fn auto_launches(&self) -> Result<Vec<AutoLaunch>> { DomainStore::auto_launches(self) }
    fn batch_completed(&self, run: &str) -> Result<bool> { DomainStore::batch_completed(self, run) }
    fn batch_effects(&self, run: &str) -> Result<Vec<BatchEffectRef>> { DomainStore::batch_effects(self, run) }
    fn batch_outcomes(&self, run: &str, epoch: &DataEpoch, after: &str, limit: usize) -> Result<Value> { DomainStore::batch_outcomes(self, run, epoch, after, limit) }
    fn batch_run_manifest(&self, epoch: &DataEpoch, after: &str, limit: usize) -> Result<Value> { DomainStore::batch_run_manifest(self, epoch, after, limit) }
    fn batch_selection(&self, run: &str) -> Result<Option<records::BatchSelection>> { DomainStore::batch_selection(self, run) }
    fn batch_stop(&self, run: &str) -> Result<Option<(alondra_domain_rules::orchestrator_batch::BatchStop, String)>> { DomainStore::batch_stop(self, run) }
    fn begin_task_closing(&mut self, task_id: &TaskId, now: i64) -> Result<TaskClosing> { DomainStore::begin_task_closing(self, task_id, now) }
    fn begin_tool_operation(&mut self, run: &str, call: &str, kind: &str, request: &Value) -> Result<Option<(String,Option<String>,Option<String>)>> { DomainStore::begin_tool_operation(self, run, call, kind, request) }
    fn bind_batch_effects(&mut self, run: &str, epoch: &DataEpoch) -> Result<()> { DomainStore::bind_batch_effects(self, run, epoch) }
    fn cancel_reasons(&self) -> Result<std::collections::BTreeMap<TaskId, String>> { DomainStore::cancel_reasons(self) }
    fn change_launch_plan(&mut self, task_id: &TaskId, plan: &LaunchPlan, changes: Option<&serde_json::Value>, now: i64) -> Result<()> { DomainStore::change_launch_plan(self, task_id, plan, changes, now) }
    fn claim_auto_launch(&mut self, task_id: &TaskId, now: i64, retry: bool) -> Result<bool> { DomainStore::claim_auto_launch(self, task_id, now, retry) }
    fn claim_message(&mut self, id: i64) -> Result<Option<DeliveryClaim>> { DomainStore::claim_message(self, id) }
    fn claim_orchestrator_batch(&mut self, ids: &[i64], run: &str, session: &str, epoch: alondra_domain_rules::DataEpoch) -> Result<(Vec<DeliveryClaim>, BatchOwner)> { DomainStore::claim_orchestrator_batch(self, ids, run, session, epoch) }
    fn claim_selected_orchestrator_batch(&mut self, ids: &[i64], run: &str, session: &str, epoch: alondra_domain_rules::DataEpoch, selection: Option<&records::BatchSelection>) -> Result<(Vec<DeliveryClaim>, BatchOwner)> { DomainStore::claim_selected_orchestrator_batch(self, ids, run, session, epoch, selection) }
    fn claim_task_removal(&mut self, task_id: &TaskId, now: i64) -> Result<bool> { DomainStore::claim_task_removal(self, task_id, now) }
    fn complete_tool_operation(&mut self, run: &str, call: &str, answer: &std::result::Result<Value,DomainError>) -> Result<()> { DomainStore::complete_tool_operation(self, run, call, answer) }
    fn conversation_directory(&self, task: &TaskId, conversation: &str) -> Result<Option<String>> { DomainStore::conversation_directory(self, task, conversation) }
    fn conversation_metadata(&self, task: &TaskId, conversation: &str) -> Result<Option<Vec<u8>>> { DomainStore::conversation_metadata(self, task, conversation) }
    fn conversations_metadata(&self, task: &TaskId) -> Result<Vec<Vec<u8>>> { DomainStore::conversations_metadata(self, task) }
    fn deferred_move(&self, task:&TaskId) ->Result<Option<Transition>> { DomainStore::deferred_move(self, task) }
    fn deferred_moves(&self) ->Result<Vec<Transition>> { DomainStore::deferred_moves(self) }
    fn delivery_evidence(&self) -> Result<Vec<DeliveryEvidence>> { DomainStore::delivery_evidence(self) }
    fn delivery_evidence_page(&self, after: i64) -> Result<Vec<DeliveryEvidenceItem>> { DomainStore::delivery_evidence_page(self, after) }
    fn dequeue_launch(&mut self, task_id: &TaskId) -> Result<()> { DomainStore::dequeue_launch(self, task_id) }
    fn discard_expired_orchestrator_messages(&mut self, cutoff: i64) -> Result<usize> { DomainStore::discard_expired_orchestrator_messages(self, cutoff) }
    fn ended_tasks_compacted(&self) -> Result<()> { Ok(DomainStore::ended_tasks_compacted(self)?) }
    fn enqueue_launch(&mut self, task_id: &TaskId, body: &serde_json::Value, origin: &str, now: i64) -> Result<()> { DomainStore::enqueue_launch(self, task_id, body, origin, now) }
    fn execute_authenticated(&mut self, actor: &Actor, envelope: &CommandEnvelope, transport: Option<TransportIdentity<'_>>) -> std::result::Result<Commit, StoreError> { DomainStore::execute_authenticated(self, actor, envelope, transport) }
    fn execute_filling_revision(&mut self, actor: &Actor, envelope: &CommandEnvelope) -> std::result::Result<Commit, StoreError> { DomainStore::execute_filling_revision(self, actor, envelope) }
    fn execute_from_batch(&mut self, actor: &Actor, envelope: &CommandEnvelope, run: &str) -> std::result::Result<Commit, StoreError> { DomainStore::execute_from_batch(self, actor, envelope, run) }
    fn execute_launching(&mut self, actor: &Actor, envelope: &CommandEnvelope, transport: Option<TransportIdentity<'_>>, launch: &RequestLaunch) -> std::result::Result<Commit, StoreError> { DomainStore::execute_launching(self, actor, envelope, transport, launch) }
    fn fill_task_models(&mut self) -> Result<usize> { DomainStore::fill_task_models(self) }
    fn fill_task_projects(&mut self, root: &Path) -> Result<usize> { DomainStore::fill_task_projects(self, root) }
    fn finish_batch(&mut self, run: &str) -> Result<()> { DomainStore::finish_batch(self, run) }
    fn finish_message(&mut self, attempt_id: i64, version: u64, state: alondra_core::delivery::State, reason: Option<&str>) -> Result<bool> { DomainStore::finish_message(self, attempt_id, version, state, reason) }
    fn fold_ended_tasks(&mut self, limit: usize) -> Result<usize> { DomainStore::fold_ended_tasks(self, limit) }
    fn forget_auto_launch(&mut self, task_id: &TaskId) -> Result<()> { DomainStore::forget_auto_launch(self, task_id) }
    fn forget_inherited_worktree(&mut self, agent_id: &str) -> Result<()> { DomainStore::forget_inherited_worktree(self, agent_id) }
    fn forget_planless_blocks(&mut self) -> Result<usize> { DomainStore::forget_planless_blocks(self) }
    fn forget_post_merge_check(&mut self, task_id: &TaskId) -> Result<()> { DomainStore::forget_post_merge_check(self, task_id) }
    fn forget_task_closing(&mut self, task_id: &TaskId) -> Result<()> { DomainStore::forget_task_closing(self, task_id) }
    fn hand_carried_message(&mut self, id: i64) -> Result<()> { DomainStore::hand_carried_message(self, id) }
    fn handed_user_notices(&self, interaction_ids: &[InteractionId]) -> Result<Vec<(i64, InteractionId)>> { DomainStore::handed_user_notices(self, interaction_ids) }
    fn handovers(&self) -> Result<Vec<Handover>> { DomainStore::handovers(self) }
    fn inherited_worktree(&self, agent_id: &str) -> Result<Option<String>> { DomainStore::inherited_worktree(self, agent_id) }
    fn integrity_check(&self) -> Result<()> { DomainStore::integrity_check(self) }
    fn keep_attachment_at(&mut self, interaction: &str, id: &str, reference: &str) -> Result<()> { DomainStore::keep_attachment_at(self, interaction, id, reference) }
    fn launch_plan(&self, task_id: &TaskId) -> Result<Option<LaunchPlan>> { DomainStore::launch_plan(self, task_id) }
    fn launch_queue(&self) -> Result<Vec<QueuedLaunch>> { DomainStore::launch_queue(self) }
    fn launch_refusals(&self) -> Result<Vec<(TaskId, String, Option<String>)>> { DomainStore::launch_refusals(self) }
    fn launched_by_alondra(&self, agent_id: &str) -> Result<bool> { DomainStore::launched_by_alondra(self, agent_id) }
    fn load(&mut self) -> Result<Arc<DomainState>> { DomainStore::load(self) }
    fn message_origin(&self, id: i64) -> Result<Option<(i64, String)>> { DomainStore::message_origin(self, id) }
    fn metrics_inputs(&self, now: Millis) -> Result<MetricsInputs> { DomainStore::metrics_inputs(self, now) }
    fn orchestrator_delivery(&self, attempt_id: i64) -> Result<Option<String>> { DomainStore::orchestrator_delivery(self, attempt_id) }
    fn orchestrator_notices_out(&self) -> Result<Vec<String>> { DomainStore::orchestrator_notices_out(self) }
    fn pending_messages(&self) -> Result<Vec<OutboxMessage>> { DomainStore::pending_messages(self) }
    fn plan_account_move(&mut self, _task: &TaskId, _plan: &AccountMovePlan) -> Result<i64> { DomainStore::plan_account_move(self, _task, _plan) }
    fn planned_tasks(&self) -> Result<std::collections::BTreeMap<TaskId, i64>> { DomainStore::planned_tasks(self) }
    fn post_merge_check(&self, task_id: &TaskId) -> Result<Option<PostMergeCheck>> { DomainStore::post_merge_check(self, task_id) }
    fn protect(&mut self, protected: alondra_engine_ports::worktrees::WorktreeRules) { DomainStore::protect(self, protected) }
    fn protected(&self) -> &alondra_engine_ports::worktrees::WorktreeRules { DomainStore::protected(self) }
    fn provider_transition(&self, task:&TaskId) ->Result<Option<Transition>> { DomainStore::provider_transition(self, task) }
    fn latest_provider_transition(&self, task:&TaskId) ->Result<Option<Transition>> { DomainStore::latest_provider_transition(self, task) }
    fn provider_transition_finished(&self, id:&str) ->Result<bool> { DomainStore::provider_transition_finished(self, id) }
    fn provider_transitions(&self) ->Result<Vec<Transition>> { DomainStore::provider_transitions(self) }
    fn prs_without_size(&self, now: Millis, limit: usize) -> Result<Vec<(String, String)>> { DomainStore::prs_without_size(self, now, limit) }
    fn public_task_context(&self, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> Result<alondra_store::ContextPage> { DomainStore::public_task_context(self, task, after, limit) }
    fn read_mark(&self) -> Result<(u64, Revision)> { DomainStore::read_mark(self) }
    fn read_cursor(&self) -> Result<alondra_engine_ports::journal::ReadCursor> { DomainStore::read_cursor(self) }
    fn reclaim_space(&self, pages: u32) -> Result<()> { DomainStore::reclaim_space(self, pages) }
    fn reconcile_orchestrator_batch(&mut self, run: &str, continuation: bool) -> Result<()> { DomainStore::reconcile_orchestrator_batch(self, run, continuation) }
    fn record_allowed_rule(&self, rule: &records::AllowedRule) -> Result<()> { DomainStore::record_allowed_rule(self, rule) }
    fn record_batch_report(&mut self, run: &str, cursor: u64, evidence: &[u8], report: &BatchReport) -> Result<()> { DomainStore::record_batch_report(self, run, cursor, evidence, report) }
    fn record_bg_stopped(&mut self, node: &AgentId, task: &TaskId, exited: Option<Process>) -> Result<()> { DomainStore::record_bg_stopped(self, node, task, exited) }
    fn record_handover_done(&mut self, node: &AgentId, task: &TaskId) -> Result<()> { DomainStore::record_handover_done(self, node, task) }
    fn record_model_decision(&mut self, d: &Decision) -> Result<()> { DomainStore::record_model_decision(self, d) }
    fn record_move_done(&mut self, task: &TaskId, number: i64) -> Result<()> { DomainStore::record_move_done(self, task, number) }
    fn record_move_host_stopped(&mut self, task: &TaskId, number: i64) -> Result<()> { DomainStore::record_move_host_stopped(self, task, number) }
    fn record_move_undone(&mut self, task: &TaskId, number: i64, why: &str) -> Result<()> { DomainStore::record_move_undone(self, task, number, why) }
    fn record_pr_lines(&mut self, task_id: &str, lines: i64) -> Result<()> { DomainStore::record_pr_lines(self, task_id, lines) }
    fn record_release(&mut self, version: &str, at: Millis) -> Result<()> { DomainStore::record_release(self, version, at) }
    fn record_spend(&mut self, spent: &Spent<'_>) -> Result<()> { DomainStore::record_spend(self, spent) }
    fn record_session_start(&mut self, started: &alondra_engine_ports::journal::metrics::SessionStarted<'_>) -> Result<()> { DomainStore::record_session_start(self, started) }
    fn session_starts(&self, task_id: &str) -> Result<Vec<alondra_engine_ports::journal::metrics::RecordedStart>> { DomainStore::session_starts(self, task_id) }
    fn refused_move(&self, task:&TaskId) ->Result<Option<Transition>> { DomainStore::refused_move(self, task) }
    fn refused_moves(&self) ->Result<Vec<Transition>> { DomainStore::refused_moves(self) }
    fn request_launch(&mut self, request_id: &RequestId) -> Result<Option<RequestLaunch>> { DomainStore::request_launch(self, request_id) }
    fn request_launch_summaries(&self) -> Result<std::collections::BTreeMap<RequestId, (Option<String>, Option<String>)>> { DomainStore::request_launch_summaries(self) }
    fn request_times(&self) -> Result<std::collections::BTreeMap<RequestId, i64>> { DomainStore::request_times(self) }
    fn resolve_uncertain(&mut self, attempt_id: i64, expected_version: u64, retry: bool) -> Result<bool> { DomainStore::resolve_uncertain(self, attempt_id, expected_version, retry) }
    fn resolve_uncertain_authenticated(&mut self, attempt_id: i64, expected_version: u64, epoch: &str, retry: bool, transport: TransportIdentity<'_>) -> std::result::Result<Value, StoreError> { DomainStore::resolve_uncertain_authenticated(self, attempt_id, expected_version, epoch, retry, transport) }
    fn retain_evidence(&mut self, limit: usize) -> std::result::Result<alondra_store::EvidenceRetention, StoreError> { DomainStore::retain_evidence(self, limit) }
    fn revision_times(&self, revisions: &[Revision]) -> Result<std::collections::BTreeMap<Revision, i64>> { DomainStore::revision_times(self, revisions) }
    fn rows_written(&self) -> u64 { DomainStore::rows_written(self) }
    fn save_task_closing(&mut self, row: &TaskClosing) -> Result<()> { DomainStore::save_task_closing(self, row) }
    fn schedule_request_tools(&mut self, request_id: &RequestId) -> Result<Option<Vec<String>>> { DomainStore::schedule_request_tools(self, request_id) }
    fn set_allowed_rule_state(&self, id: i64, state: records::RuleState) -> Result<()> { DomainStore::set_allowed_rule_state(self, id, state) }
    fn set_api_failures(&self, agent: &AgentId, streak: Option<&records::Streak>) -> Result<()> { DomainStore::set_api_failures(self, agent, streak) }
    fn set_auto_launch(&mut self, task_id: &TaskId, state: AutoLaunchState, reason: Option<AutoLaunchReason>, detail: Option<&str>, now: i64) -> Result<()> { DomainStore::set_auto_launch(self, task_id, state, reason, detail, now) }
    fn set_cancel_reason(&self, task_id: &TaskId, reason: &str) -> Result<()> { DomainStore::set_cancel_reason(self, task_id, reason) }
    fn set_inherited_worktree(&mut self, agent_id: &str, dir: &str, from_agent: &str, now: i64) -> Result<()> { DomainStore::set_inherited_worktree(self, agent_id, dir, from_agent, now) }
    fn set_launch_refusal(&mut self, task_id: &TaskId, refusal: Option<(&str, Option<&str>)>, now: i64) -> Result<()> { DomainStore::set_launch_refusal(self, task_id, refusal, now) }
    fn set_start_retry(&mut self, task_id: &TaskId, retry: Option<(u8, i64)>) -> Result<()> { DomainStore::set_start_retry(self, task_id, retry) }
    fn set_stop_blocks(&self, task_id: &TaskId, blocks: u32, marker: i64) -> Result<()> { DomainStore::set_stop_blocks(self, task_id, blocks, marker) }
    fn set_task_worktree(&mut self, task_id: &TaskId, dir: &str, now: i64) -> Result<()> { DomainStore::set_task_worktree(self, task_id, dir, now) }
    fn settle_orchestrator_batch(&mut self, run: &str, target: alondra_core::delivery::State, reason: Option<&str>) -> Result<bool> { DomainStore::settle_orchestrator_batch(self, run, target, reason) }
    fn start_retries(&self) -> Result<std::collections::BTreeMap<TaskId, (u8, i64)>> { DomainStore::start_retries(self) }
    fn stop_blocks(&self, task_id: &TaskId) -> Result<Option<(u32, i64)>> { DomainStore::stop_blocks(self, task_id) }
    fn task_closing(&self, task_id: &TaskId) -> Result<Option<TaskClosing>> { DomainStore::task_closing(self, task_id) }
    fn task_closings(&self) -> Result<Vec<TaskClosing>> { DomainStore::task_closings(self) }
    fn task_history(&self, task_id: &TaskId) -> Result<Vec<HistoryStep>> { DomainStore::task_history(self, task_id) }
    fn task_roles(&self) -> Result<std::collections::BTreeMap<TaskId, String>> { DomainStore::task_roles(self) }
    fn task_spend(&self, task: &str) -> Result<(BTreeMap<String, i64>, Option<i64>)> { DomainStore::task_spend(self, task) }
    fn task_started_times(&self) -> Result<std::collections::BTreeMap<TaskId, i64>> { DomainStore::task_started_times(self) }
    fn task_state_times(&self) -> Result<std::collections::BTreeMap<TaskId, i64>> { DomainStore::task_state_times(self) }
    fn task_was_launched(&self, task_id: &TaskId) -> Result<bool> { DomainStore::task_was_launched(self, task_id) }
    fn task_worktree(&self, task_id: &TaskId) -> Result<Option<String>> { DomainStore::task_worktree(self, task_id) }
    fn task_worktrees(&self) -> Result<Vec<(TaskId, String)>> { DomainStore::task_worktrees(self) }
    fn tool_operations(&self, run: &str) -> Result<Vec<(String,String,String,Option<String>,Option<String>)>> { DomainStore::tool_operations(self, run) }
    fn transport_receipt(&mut self, identity: TransportIdentity<'_>) -> std::result::Result<Option<CommandOutcome>, StoreError> { DomainStore::transport_receipt(self, identity) }
    fn unfinished_orchestrator_batches(&self, epoch: &DataEpoch, active: Option<&str>, retained: Option<&str>) -> Result<Vec<UnfinishedBatch>> { DomainStore::unfinished_orchestrator_batches(self, epoch, active, retained) }
    fn user_notices_in_open_passes(&self) -> Result<std::collections::BTreeSet<i64>> { DomainStore::user_notices_in_open_passes(self) }
    fn user_push_handed(&self, interaction_ids: &[InteractionId]) -> Result<bool> { DomainStore::user_push_handed(self, interaction_ids) }
    fn user_push_history(&self, message_id: i64) -> Result<UserPushHistory> { DomainStore::user_push_history(self, message_id) }
    fn was_schedule_launch_failed(&self, request_id: &RequestId) -> Result<bool> { DomainStore::was_schedule_launch_failed(self, request_id) }
    fn worktree_storage(&self, path: &str) -> Result<Option<records::Repository>> { DomainStore::worktree_storage(self, path) }
    fn worktree_storage_at(&self, path: &str) -> Result<Option<(std::path::PathBuf, records::Repository)>> { DomainStore::worktree_storage_at(self, path) }
    fn write_mark(&self) -> Result<(u64, Revision)> { DomainStore::write_mark(self) }
}

impl OwnerJournal for OwnerTransaction<'_> {
    fn with_lifecycle(&mut self, step: &mut dyn FnMut(&mut alondra_store::LifecycleTransaction<'_>) -> std::result::Result<(), StoreError>) -> std::result::Result<(), StoreError> {
        step(&mut self.journal)
    }
    fn stage_observation_with_identity(&mut self, binding: &ProviderBinding, owner_generation: u64, batch: &OrderedObservation, normalizer: &dyn ObservationNormalizer, home: &Path, probe: bool, identity: Option<&dyn KnownIdentity>) -> std::result::Result<ObservationAcceptance, StoreError> {
        OwnerTransaction::stage_observation_with_identity(self, binding, owner_generation, batch, &normalizer, home, probe, identity)
    }
    fn discard_observation_by(&mut self, binding: &ProviderBinding, owner_generation: u64, batch: &OrderedObservation, normalizer: &dyn ObservationNormalizer, reason: DiscardReason) -> std::result::Result<ObservationAcceptance, StoreError> {
        OwnerTransaction::discard_observation(self, binding, owner_generation, batch, &normalizer, reason)
    }
    fn launch_with(&mut self, task: &TaskId, text: &str, directory: &str, binding: &ProviderBinding, provider: &mut dyn Preparer) -> std::result::Result<ExecutionStamp, StoreError> {
        OwnerTransaction::launch(self, task, text, directory, binding, &mut &mut *provider)
    }
    fn record_command_with(&mut self, attempt: DispatchAttempt, owner_generation: u64, provider: &mut dyn Preparer) -> std::result::Result<(), StoreError> {
        OwnerTransaction::record_command(self, attempt, owner_generation, &mut &mut *provider)
    }
    fn record_control_with(&mut self, attempt: ControlAttempt, owner_generation: u64, prepare: &mut dyn FnMut(&ControlAttempt) -> std::result::Result<PreparedCommand, AdapterError>) -> std::result::Result<(), StoreError> {
        OwnerTransaction::record_control(self, attempt, owner_generation, prepare)
    }
    fn adapter_state(&self, grant: &OwnerGrant) -> std::result::Result<AdapterState, StoreError> { OwnerTransaction::adapter_state(self, grant) }
    fn admit_provider_transition(&mut self, id:&str, stamp:&alondra_foundation::ExecutionStamp) ->std::result::Result<(),StoreError> { OwnerTransaction::admit_provider_transition(self, id, stamp) }
    fn adopt(&mut self, task: &TaskId) -> std::result::Result<Adoption, StoreError> { OwnerTransaction::adopt(self, task) }
    fn claim(&mut self, permit: DispatchPermit) -> std::result::Result<(), StoreError> { OwnerTransaction::claim(self, permit) }
    fn claim_control(&mut self, permit: ControlPermit) -> std::result::Result<bool, StoreError> { OwnerTransaction::claim_control(self, permit) }
    fn claim_control_for_identity(&mut self, permit:ControlPermit, directory:&std::path::Path) ->std::result::Result<bool,StoreError> { OwnerTransaction::claim_control_for_identity(self, permit, directory) }
    fn claim_is_open(&self, task: &TaskId, intent: i64) -> std::result::Result<bool, StoreError> { OwnerTransaction::claim_is_open(self, task, intent) }
    fn claims_left(&mut self, task: &TaskId) -> std::result::Result<bool, StoreError> { OwnerTransaction::claims_left(self, task) }
    fn configure_transition_model(&mut self, plan:&TransitionPlan) ->std::result::Result<bool,StoreError> { OwnerTransaction::configure_transition_model(self, plan) }
    fn domain_state(&self) -> &DomainState { OwnerTransaction::domain_state(self) }
    fn execution_binding(&self, stamp: &ExecutionStamp) -> std::result::Result<ProviderBinding, StoreError> { OwnerTransaction::execution_binding(self, stamp) }
    fn lifecycle(&self, task: &TaskId) -> std::result::Result<std::sync::Arc<TaskLifecycle>, StoreError> { OwnerTransaction::lifecycle(self, task) }
    fn normalization_checkpoint(&self, stamp: &alondra_foundation::ExecutionStamp) -> std::result::Result<Option<alondra_store::NormalizationCheckpoint>, StoreError> { OwnerTransaction::normalization_checkpoint(self, stamp) }
    fn observe_provider_transition(&mut self, stamp:&alondra_foundation::ExecutionStamp, cursor:u64) ->std::result::Result<(),StoreError> { OwnerTransaction::observe_provider_transition(self, stamp, cursor) }
    fn pending_handoffs(&self, task: &TaskId) -> std::result::Result<Vec<(InputPayload, String)>, StoreError> { OwnerTransaction::pending_handoffs(self, task) }
    fn pending_transition_selection(&mut self, id:&str) ->std::result::Result<Option<alondra_core::model_selection::PendingTaskSelection>,StoreError> { OwnerTransaction::pending_transition_selection(self, id) }
    fn pin_context(&mut self, transfer: &str, through: &alondra_store::ContextRef) -> std::result::Result<(),StoreError> { OwnerTransaction::pin_context(self, transfer, through) }
    fn provider_transition_blocks(&mut self, stamp:&alondra_foundation::ExecutionStamp) ->std::result::Result<bool,StoreError> { OwnerTransaction::provider_transition_blocks(self, stamp) }
    fn provider_transition_phase(&mut self, id:&str, expected:Phase, next:Phase, facts:serde_json::Value) ->std::result::Result<(),StoreError> { OwnerTransaction::provider_transition_phase(self, id, expected, next, facts) }
    fn record_control_receipt(&mut self, control: CommittedControl, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> { OwnerTransaction::record_control_receipt(self, control, receipt) }
    fn record_handoff_proof(&mut self, grant: &OwnerGrant, intent: i64, proof: &alondra_provider_contract::HandoffProof) -> std::result::Result<bool, StoreError> { OwnerTransaction::record_handoff_proof(self, grant, intent, proof) }
    fn record_move_admitted(&mut self, task: &TaskId, number: i64, agent: &AgentId) -> std::result::Result<(), StoreError> { OwnerTransaction::record_move_admitted(self, task, number, agent) }
    fn record_receipt(&mut self, command: CommittedCommand, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> { OwnerTransaction::record_receipt(self, command, receipt) }
    fn record_sdk_admitted(&mut self, node: &AgentId, task: &TaskId, agent: &AgentId) -> std::result::Result<(), StoreError> { OwnerTransaction::record_sdk_admitted(self, node, task, agent) }
    fn record_settings_projection(&mut self, identity: &settings::ProjectionIdentity) -> std::result::Result<(),StoreError> { OwnerTransaction::record_settings_projection(self, identity) }
    fn refusal(&self, envelope: &CommandEnvelope) -> Option<DomainError> { OwnerTransaction::refusal(self, envelope) }
    fn reissue_permit(&mut self, task: &TaskId) -> std::result::Result<(), StoreError> { OwnerTransaction::reissue_permit(self, task) }
    fn release_context(&mut self, transfer: &str, task: &TaskId) -> std::result::Result<(),StoreError> { OwnerTransaction::release_context(self, transfer, task) }
    fn request_provider_transition(&mut self, plan:&TransitionPlan) ->std::result::Result<(),StoreError> { OwnerTransaction::request_provider_transition(self, plan) }
    fn require_native_identity(&self, binding:&ProviderBinding, expected:Option<&dyn KnownIdentity>) ->std::result::Result<(),StoreError> { OwnerTransaction::require_native_identity(self, binding, expected) }
    fn resolve(&mut self, stamp: &ExecutionStamp, intent: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> { OwnerTransaction::resolve(self, stamp, intent, owner_generation, receipt) }
    fn resolve_control(&mut self, stamp: &ExecutionStamp, id: i64, owner_generation: u64, receipt: DispatchReceipt) -> std::result::Result<(), StoreError> { OwnerTransaction::resolve_control(self, stamp, id, owner_generation, receipt) }
    fn retain_evidence(&mut self, limit: usize) -> std::result::Result<alondra_store::EvidenceRetention, StoreError> { OwnerTransaction::retain_evidence(self, limit) }
    fn retained_context(&self, task: &TaskId, after: Option<&alondra_store::ContextRef>, limit: usize) -> std::result::Result<alondra_store::ContextPage,StoreError> { OwnerTransaction::retained_context(self, task, after, limit) }
    fn selection_source(&mut self, task:&TaskId, host_directory:std::path::PathBuf) ->std::result::Result<Source,StoreError> { OwnerTransaction::selection_source(self, task, host_directory) }
    fn source_fenced(&mut self, plan:&TransitionPlan, proof:&EndedHost) ->std::result::Result<(),StoreError> { OwnerTransaction::source_fenced(self, plan, proof) }
    fn source_inputs_unoffered(&mut self, task:&TaskId) ->std::result::Result<bool,StoreError> { OwnerTransaction::source_inputs_unoffered(self, task) }
    fn stage_command(&mut self, actor: &Actor, envelope: &CommandEnvelope, attached: Attached<'_>) -> std::result::Result<&CommandOutcome, StoreError> { OwnerTransaction::stage_command(self, actor, envelope, attached) }
    fn state(&self) -> &DomainState { OwnerTransaction::state(self) }
    fn transition_launch_text(&mut self, task:&TaskId, fallback:&str) ->std::result::Result<String,StoreError> { OwnerTransaction::transition_launch_text(self, task, fallback) }
    fn transition_model_control_status(&mut self, plan: &TransitionPlan) -> std::result::Result<Option<String>, StoreError> { OwnerTransaction::transition_model_control_status(self, plan) }
    fn unrecorded(&self) -> &[String] { OwnerTransaction::unrecorded(self) }
    fn validate_selection_binding(&mut self, plan:&TransitionPlan) ->std::result::Result<(),StoreError> { OwnerTransaction::validate_selection_binding(self, plan) }
    fn validate_selection_source(&mut self, plan:&TransitionPlan) ->std::result::Result<(),StoreError> { OwnerTransaction::validate_selection_source(self, plan) }
    fn work_is_over(&self, task: &alondra_domain_rules::TaskId) -> bool { OwnerTransaction::work_is_over(self, task) }
}

