//! The orchestrator executor: one durable inbox batch per print-mode
//! Claude invocation of `mcp-orchestrator`, with a bearer bound to this
//! execution. The batch is claimed from the domain outbox before Claude runs
//! and finished as handed, retryable or uncertain afterwards; finishing an
//! invocation never completes a Task.
use alondra_core::domain::SystemCard;
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::OutboxMessage;
use crate::orchestrator_process::{self as process, BatchEnd, Ownership};
use alondra_core::delivery::State;
use alondra_core::domain::{Actor, Author, Command, CommandEnvelope, DomainError, DomainState, InteractionId, InteractionPayload, PROTOCOL_VERSION, QuestionOption, Recipient, RequestId, Scope};
use alondra_core::now_millis;
use alondra_core::orchestrator_batch::{BATCH_CONTINUE, BATCH_CONTINUE_LABEL, BATCH_PROBLEM_PREFIX, BATCH_RETRY, BATCH_RETRY_LABEL, BatchProblem, BatchStop};
use alondra_core::orchestrator_cards::{STUCK_AFTER, STUCK_PROBLEM_PREFIX, STUCK_RETRY, STUCK_RETRY_LABEL, StuckProblem};
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const MAX_BATCH_MESSAGES: usize = 32;
const MAX_BATCH_BYTES: usize = 128 * 1024;
pub const FILE: &str = "orchestrator.json";
/// The runtime record's name before it was renamed; startup moves it to `FILE`.
pub(crate) const PREVIOUS_FILE: &str = "orchestrator-v2.json";
const SUMMARY_LINE: usize = 200;
const MAX_SUMMARY_BYTES: usize = 32 * 1024;
/// The most of its final words a batch hands to the next one.
const MAX_NOTE_BYTES: usize = 4 * 1024;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Runtime {
    /// The conversation of the latest batch. Every batch starts its own: none is resumed.
    pub session_id: Option<String>,
    pub active_run: Option<Run>,
    pub last_run_id: Option<String>,
    pub last_error: Option<String>,
    /// What kind of failure `last_error` describes, so the user reads it in their language.
    #[serde(default)]
    pub failure: Option<Failure>,
    pub retry_at: Option<i64>,
    pub consecutive_failures: u32,
    /// The turns the orchestrator ended, handled, failed or cut short: each one is a moment it
    /// rested, when Alondra hands it what it still has to re-plan.
    #[serde(default)]
    pub rests: u64,
    pub cancel_requested: bool,
    /// Batches whose execution was interrupted or given up: the next run is
    /// told never to replay them.
    pub pending_resolutions: Vec<Resolution>,
    #[serde(default,skip_serializing_if="Vec::is_empty")]
    pub batch_handoffs: Vec<crate::orchestrator_provider::BatchHandoff>,
    /// The open recovery question after repeated unsuccessful attempts.
    #[serde(default)]
    pub stuck_problem: Option<String>,
    /// What the last handled batch said at its end, for the next batch, which starts a new
    /// conversation and has only what Alondra hands it.
    #[serde(default)]
    pub note: Option<String>,
    /// The context the latest batch read.
    #[serde(default)]
    pub last_context: Option<BatchContext>,
}

/// Tokens the model read in a batch's first call and in its last one: its context at the start
/// of the turn and at its end.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct BatchContext {
    pub first: u64,
    pub last: u64,
    pub window: Option<u64>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    /// Claude did not start, or ended without changing anything; the batch is offered again on its own.
    Launch,
    /// Claude ran but did not finish; some of its actions may have happened.
    Interrupted,
    /// One of Alondra's own tools was refused to it.
    Denied,
    /// The user stopped the run.
    Cancelled,
    /// Its saved conversation was gone; the next run starts a new one. Only a daemon that
    /// resumed conversations wrote it.
    SessionMissing,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Run {
    pub id: String,
    pub message_ids: Vec<i64>,
    pub attempts: Vec<(i64, u64)>,
    pub started_at: i64,
    pub spawned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Resolution {
    pub run_id: String,
    pub message_ids: Vec<i64>,
}

pub fn load(home: &Path) -> Result<Runtime> {
    match std::fs::read(home.join(FILE)) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("invalid orchestrator runtime record"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Runtime::default()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn save(home: &Path, runtime: &Runtime) -> Result<()> {
    let next = home.join(format!("{FILE}.next"));
    #[cfg(any(test, feature = "test-hooks"))]
    faults::write(home, "runtime_write")?;
    std::fs::write(&next, serde_json::to_vec(runtime)?)?;
    #[cfg(any(test, feature = "test-hooks"))]
    faults::write(home, "runtime_rename")?;
    std::fs::rename(&next, home.join(FILE))?;
    Ok(())
}

#[cfg(any(test, feature = "test-hooks"))]
pub use alondra_daemon_engine_model::faults;
#[path = "domain_orchestrator/recovery.rs"]
mod recovery;
#[path = "domain_orchestrator/watchdog.rs"]
mod watchdog;
pub(crate) use watchdog::HostGuard;
#[cfg(test)]
#[path = "domain_orchestrator/tests/recovery.rs"]
mod recovery_tests;
#[cfg(test)]
#[path = "domain_orchestrator/tests/recovery_decisions.rs"]
mod recovery_decisions;
#[cfg(test)]
#[path = "domain_orchestrator/tests/deadline.rs"]
mod deadline_tests;

/// A failed batch's Problem is `problem-orchestrator-<run id>`; its options are the decisions.
const PROBLEM_PREFIX: &str = BATCH_PROBLEM_PREFIX;
/// The answer to cards raised before "continue" and "stop" became one choice.
const PROBLEM_STOP: &str = "stop";
const SUBJECT_LINE: usize = 120;
/// A newer batch card replaces an open one.
const BATCH_CAUSE: &str = "orchestrator_batch";
const STUCK_CAUSE: &str = "orchestrator_stuck";

pub struct Executor {
    pub domain: Arc<DomainRuntime>,
    pub home: PathBuf,
    pub claude_home: PathBuf,
}

/// A notice for the orchestrator is read once Alondra handed it over (or gave it up): it no
/// longer waits for anyone.
pub(crate) fn read_handed_notices(domain: &DomainRuntime) {
    read_handed(domain, |_| {});
}

/// The startup sweep finds every notice an older daemon never read: 443 on the real data, each a
/// command that holds the domain store. Back to back they kept the store from the HTTP server's
/// two workers for seconds, so the sweep hands it back after each notice for as long as it held it.
pub fn read_handed_notices_in_background(domain: &DomainRuntime) {
    read_handed(domain, |held| std::thread::sleep(held.max(SWEEP_PAUSE)));
}

const SWEEP_PAUSE: Duration = Duration::from_millis(2);

fn read_handed(domain: &DomainRuntime, pause: impl Fn(Duration)) {
    let out = match domain.store.lock_or_exit("the domain store").orchestrator_notices_out() {
        Ok(out) => out,
        Err(error) => return log!("could not list the orchestrator's handed notices: {error:#}"),
    };
    for id in out {
        let Ok(state) = domain.state() else { return };
        let unread = state.interactions.get(&InteractionId::from(id.as_str())).is_some_and(|i| {
            i.recipient == Recipient::Orchestrator && matches!(&i.payload, InteractionPayload::Information { read_by } if read_by.is_empty())
        });
        if !unread {
            continue;
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("orchestrator-read-{id}").into(), expected_revision: state.revision,
            command: Command::MarkRead { interaction_id: id.as_str().into() },
        };
        let started = std::time::Instant::now();
        if let Err(error) = domain.execute_as_system(envelope) {
            log!("could not mark the orchestrator's notice {id} read: {error:?}");
        }
        pause(started.elapsed());
    }
}

impl Executor {
    fn pending(&self) -> Result<Vec<OutboxMessage>> {
        let store = self.domain.store.lock_or_exit("the domain store");
        Ok(store.pending_messages()?.into_iter()
            .filter(|message| message.recipient_kind == "orchestrator").collect())
    }

    /// An execution the daemon did not see finish is resolved at startup: its
    /// attempts stay uncertain evidence, the batch is never replayed, and the
    /// next run is told what was interrupted.
    pub(crate) fn recover(&self) -> Result<Runtime> {
        let mut runtime = load(&self.home)?;
        if let Some(run) = runtime.active_run.clone() {
            let admitted = self.domain.end_execution(&run.id);
            let stopped = process::end_recorded(&self.home.join("runtime/orchestrator"), &run.id);
            let confirmed = self.domain.confirmed_batch_effects(&run.id,&admitted)?;
            if runtime.last_error.is_some() || stopped.is_err() || !confirmed {
                let why = stopped.err().map(|error| format!("The previous process could not be fenced: {error:#}"))
                    .unwrap_or_else(|| "The daemon restarted with an unresolved operation; reconcile its durable result before continuing".into());
                log!("orchestrator: batch {} was running when the daemon restarted and is not confirmed ({why}); admitted {:?}; unconfirmed {:?}",
                    run.id, admitted, self.domain.unconfirmed_operations(&run.id));
                let problem = BatchProblem { stop:if runtime.failure==Some(Failure::Denied) {BatchStop::Denied} else {BatchStop::Interrupted},calls:admitted,subjects:vec![],error:why.clone() };
                if runtime.last_error.is_none() {
                    self.failed(runtime,&run,why,Some(problem))?;
                } else {
                    runtime.failure.get_or_insert(Failure::Interrupted);
                    save(&self.home,&runtime)?;
                    self.raise_batch_problem(&run.id,problem,&why);
                }
                return load(&self.home);
            }
            let (effects,operations,selection) = {
                let store = self.domain.store.lock_or_exit("the domain store");
                (store.batch_effects(&run.id)?,store.tool_operations(&run.id)?,store.batch_selection(&run.id)?)
            };
            if (!effects.is_empty() || !operations.is_empty()) && let Some(selection) = selection {
                self.finish(&run,State::Retryable,Some("continue_confirmed_batch_after_restart"))?;
                runtime.batch_handoffs.push(crate::orchestrator_provider::BatchHandoff { run_id:run.id.clone(),selection,reason:"Daemon restarted after confirmed effects; inspect current state and continue remaining goals".into() });
            } else {
                self.give_up(&run, "daemon_restarted_during_execution")?;
                runtime.pending_resolutions.push(Resolution { run_id: run.id.clone(), message_ids: run.message_ids.clone() });
            }
            runtime.active_run = None;
            runtime.last_error = None;
            runtime.failure = None;
            runtime.retry_at = None;
            runtime.consecutive_failures = 0;
            runtime.cancel_requested = false;
            save(&self.home, &runtime)?;
        }
        Ok(runtime)
    }

    fn give_up(&self, run: &Run, reason: &str) -> Result<()> {
        let mut store = self.domain.store.lock_or_exit("the domain store");
        for (attempt, version) in &run.attempts {
            store.finish_message(*attempt, *version, State::Uncertain, Some(reason))?;
            store.resolve_uncertain(*attempt, *version + 1, false)?;
        }
        Ok(())
    }

    fn finish(&self, run: &Run, state: State, reason: Option<&str>) -> Result<()> {
        let mut store = self.domain.store.lock_or_exit("the domain store");
        for (attempt, version) in &run.attempts {
            store.finish_message(*attempt, *version, state, reason)?;
        }
        Ok(())
    }

    /// The user acknowledged an ambiguous execution: its batch is not replayed.
    pub(crate) fn acknowledge(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.take().filter(|run| run.id == run_id) else {
            bail!("no ambiguous execution {run_id}");
        };
        let mut store = self.domain.store.lock_or_exit("the domain store");
        if !store.settle_orchestrator_batch(&run.id, State::Discarded, Some("explicit_user_reconciliation"))? {
            for (attempt, version) in &run.attempts {
                store.resolve_uncertain(*attempt, *version + 1, false)?;
            }
        }
        drop(store);
        {
            let mut pending = self.domain.live.orchestrator_stop.lock_or_recover("the orchestrator stop");
            if pending.as_ref().is_some_and(|(stopped, _)| stopped.id == run.id) { *pending = None; }
        }
        self.domain.live.orchestrator_runs.lock_or_recover("the orchestrator runs").remove(&run.id);
        runtime.pending_resolutions.push(Resolution { run_id: run.id, message_ids: run.message_ids });
        runtime.last_error = None;
        runtime.failure = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        runtime.cancel_requested = false;
        save(&self.home, &runtime)
    }

    /// The user chose to replay an ambiguous execution's batch.
    pub(crate) fn retry(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.take().filter(|run| run.id == run_id) else {
            bail!("no ambiguous execution {run_id}");
        };
        let mut store = self.domain.store.lock_or_exit("the domain store");
        if !store.settle_orchestrator_batch(&run.id, State::Retryable, Some("explicit_user_reconciliation"))? {
            for (attempt, version) in &run.attempts {
                store.resolve_uncertain(*attempt, *version + 1, true)?;
            }
        }
        drop(store);
        {
            let mut pending = self.domain.live.orchestrator_stop.lock_or_recover("the orchestrator stop");
            if pending.as_ref().is_some_and(|(stopped, _)| stopped.id == run.id) { *pending = None; }
        }
        self.domain.live.orchestrator_runs.lock_or_recover("the orchestrator runs").remove(&run.id);
        runtime.last_error = None;
        runtime.failure = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        runtime.cancel_requested = false;
        save(&self.home, &runtime)
    }

    pub(crate) fn cancel(&self, run_id: &str) -> Result<()> {
        let mut runtime = load(&self.home)?;
        if runtime.active_run.as_ref().is_none_or(|run| run.id != run_id) {
            bail!("no running execution {run_id}");
        }
        if runtime.last_error.is_some() {
            return self.acknowledge(run_id);
        }
        runtime.cancel_requested = true;
        save(&self.home, &runtime)
    }

    fn prompt(&self, runtime: &Runtime, projects_root: Option<&Path>, messages: &[OutboxMessage]) -> Result<String> {
        let state = self.domain.state()?;
        let mut text = String::from("You are Alondra's orchestrator. Load the alondra-orchestrator skill. Process this inbox batch with the Alondra tools; use Alondra ask for user questions. Every open Request below marked (no task of its own) leaves this batch with its Tasks, an answer given with close_request, folded into the open Task that already covers it, or a question to the user: an answer written only in this conversation reaches nobody. Before create_task, compare each such Request with the open Tasks listed below: when one already covers what it asks, create no Task; send what is new to that Task's node and close the Request with close_request and folded_into naming that Task. Never two Tasks for the same work. A Request that lists its tasks already has them: never give it Tasks again unless every one of them has ended. After processing the batch, finish this invocation. Do not wait or poll for future work.\n");
        text.push_str("This is a new conversation, as every batch is: nothing of an earlier batch is in it but what Alondra writes below, which is current. Call tasks_in_progress or node only for what it does not say; do not infer that existing tasks need to be started again. End with a short note, a few sentences at most, of what the next batch needs that Alondra does not already hold: it reaches the next batch and nobody else.\n");
        if let Some(note) = &runtime.note {
            text.push_str(&format!("\nYOUR NOTE FROM THE PREVIOUS BATCH:\n{note}\n"));
        }
        if !runtime.pending_resolutions.is_empty() {
            text.push_str("\nDURABLE RESOLUTION: Previous interrupted inbox batches have already been resolved and are NOT replayed. Do not repeat or continue any of them unless a NEW User message explicitly authorizes it. This does not mean their business actions succeeded. An open Request below with no Task of its own was not turned into work: handle it now, unless tasks_in_progress shows an open question about it.\n");
        }
        if let Some(root) = projects_root {
            text.push_str(&format!("Projects root, readable through the role's file tools: {}\n", root.display()));
        }
        let mut transfer = String::from("\nRead batch_outcomes with list_runs=true and paginate with after to discover all retained batches, including references omitted below. Read a selected run_id before continuing its remaining goals; never replay its calls.\n");
        for handoff in runtime.batch_handoffs.iter().rev() {
            let header = format!("\nCONFIRMED BATCH HANDOFF {}: {}. Continue pending GOALS from current committed state; never replay previous tool calls. Use batch_outcomes for its durable references.\n",handoff.run_id,handoff.reason);
            if transfer.len()+header.len()>8192 {transfer.push_str("More batch references were omitted from this bounded prompt.\n");break;}
            transfer.push_str(&header);
            let store = self.domain.store.lock_or_exit("the domain store");
            for effect in store.batch_effects(&handoff.run_id)?.into_iter().take(8) {
                let line = format!("Confirmed domain command {} ({}) at revision {}; receipt {}.\n",effect.command_id,effect.kind,effect.revision,effect.receipt_ref.command_id);
                if transfer.len()+line.len()>8192 {break;}
                transfer.push_str(&line);
            }
        }
        text.push_str(&transfer);
        let user = crate::domain_messages::user_name();
        let opened = self.domain.store.lock_or_exit("the domain store").request_times()?;
        text.push_str(&summary(&state, &Ages { now: now_millis(), opened }, |request| crate::domain_orchestrator_tools::request_choices(&self.domain, request).ok().flatten()
            .map(|choices| crate::domain_orchestrator_tools::choices_text(&user, &self.home, &choices))));
        for message in messages {
            text.push_str(&format!("\n{}\n", crate::domain_transport::render(&state, message)));
            if let Some(case) = message.interaction_id.as_ref().and_then(|id| state.interactions.get(&InteractionId(id.clone()))) {
                text.push_str(&crate::stall_context::context(&self.domain, &state, case));
            }
        }
        Ok(text)
    }

    /// The domain names the orchestrator by this session; a failure never holds back the run.
    fn bind_session(&self, session: &str) {
        if let Err(error) = self.domain.bind_orchestrator_session(session) {
            log!("cannot record the orchestrator's session {session}: {error}");
        }
    }

    /// Records for the screens that no batch starts because no Claude account has a session, or
    /// that one has it again, and the line the log says it with: only when that changes.
    fn account_wait_change(&self) -> Option<String> {
        let checked = crate::accounts::none_signed_in(&self.domain.read().state().accounts);
        let wait = &self.domain.live.account_wait;
        match checked {
            Some(checked) if wait.start() => {
                Some(format!("orchestrator waits: no Claude account has a session ({checked}); requests stay unprocessed until one is signed in"))
            }
            None if wait.end() => Some("orchestrator: a Claude account has a session again; it still waits for Claude usage".to_owned()),
            _ => None,
        }
    }

    /// One batch: claim, bind a bearer to this execution, run Claude, finish.
    #[cfg(any(test, feature = "test-hooks"))]
    pub(crate) async fn tick(&self) -> Result<()> {
        let (_hold, stop) = tokio::sync::watch::channel(false);
        self.tick_with_stop(stop).await
    }

    async fn tick_with_stop(&self, stop: tokio::sync::watch::Receiver<bool>) -> Result<()> {
        self.tick_with_clock(stop, None).await
    }

    async fn tick_with_clock(
        &self,
        stop: tokio::sync::watch::Receiver<bool>,
        now: Option<i64>,
    ) -> Result<()> {
        let now = now.unwrap_or_else(now_millis);
        crate::orchestrator_inbox::settle_overdue(&self.domain, now);
        self.repair_incomplete().await?;
        if *stop.borrow() { return Ok(()); }
        self.apply_problem_answers()?;
        let mut runtime = load(&self.home)?;
        self.domain.live.orchestrator_rests.store(runtime.rests, std::sync::atomic::Ordering::Relaxed);
        crate::usage_probe::follow(&self.domain, now);
        if self.domain.live.usage_pause.current().is_some_and(|pause|pause.by_user) { return Ok(()); }
        if runtime.active_run.is_none() {
            crate::orchestrator_inbox::hand_replans(&self.domain);
        }
        if runtime.active_run.is_some() || runtime.retry_at.is_some_and(|at| at > now) {
            return Ok(());
        }
        let selection = match crate::orchestrator_provider::selection(&self.domain, &self.home, now) {
            Ok(selection) => selection,
            Err(error) => {
                if self.domain.live.account_wait.wait_for(&error.message) { log!("orchestrator waits for account/model selection: {}", error.message); }
                return Ok(());
            }
        };
        if self.domain.live.account_wait.end() { log!("orchestrator resumes with {}", selection.account.wire_id()); }
        if self.domain.live.usage_pause.is_active() { return Ok(()); }
        {
            let mut store = self.domain.store.lock_or_exit("the domain store");
            store.discard_expired_orchestrator_messages(
                now.saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS),
            )?;
        }
        let mut size = 0;
        let pending: Vec<OutboxMessage> = self.pending()?.into_iter().take(MAX_BATCH_MESSAGES)
            .take_while(|message| {
                let fits = size == 0 || size + message.message.len() <= MAX_BATCH_BYTES;
                if fits { size += message.message.len(); }
                fits
            }).collect();
        if pending.is_empty() && runtime.pending_resolutions.is_empty() {
            crate::orchestrator_inbox::hand_replans(&self.domain);
            if runtime.last_error.is_none() {
                crate::orchestrator_inbox::settle(&self.domain);
            }
            return Ok(());
        }
        let (Some(orchestrator_plugin), Some(node_plugin)) = (
            crate::plugins::orchestrator_in(&self.home).or_else(|| crate::plugins::dir(&self.claude_home, crate::plugins::ORCHESTRATOR)),
            crate::plugins::node_in(&self.home).or_else(|| crate::plugins::dir(&self.claude_home, crate::plugins::NODE)),
        ) else {
            bail!("Required Alondra plugins are not installed; run alondra doctor --fix");
        };
        let run_id = process::uuid()?;
        let session = process::uuid()?;
        let state = self.domain.state()?;
        let epoch = state.data_epoch.clone();
        let asked = crate::orchestrator_inbox::asked(&state);
        drop(state);
        let ids: Vec<i64> = pending.iter().map(|message| message.id).collect();
        let (claims, owner) = self.domain.store.lock_or_exit("the domain store")
            .claim_selected_orchestrator_batch(&ids, &run_id, &session, epoch, Some(&selection))?;
        self.domain.live.orchestrator_runs.lock_or_recover("the orchestrator runs").insert(run_id.clone());
        let attempts: Vec<(i64, u64)> = claims.iter().map(|claim| (claim.attempt_id, claim.version)).collect();
        let claimed: Vec<OutboxMessage> = claims.into_iter().map(|claim| claim.message).collect();
        if claimed.is_empty() && runtime.pending_resolutions.is_empty() {
            return Ok(());
        }
        crate::accounts::log_choice(&self.domain, "orchestrator batch", &selection.account, now);
        let run = Run {
            id: run_id.clone(), message_ids: claimed.iter().map(|m| m.id).collect(),
            attempts, started_at: now, spawned: false,
        };
        runtime.active_run = Some(run.clone());
        runtime.last_run_id = Some(run_id.clone());
        runtime.session_id = Some(session.clone());
        runtime.cancel_requested = false;
        runtime.last_error = None;
        runtime.failure = None;
        runtime.retry_at = None;
        self.domain.orchestrator_progress();
        save(&self.home, &runtime)?;
        self.bind_session(&session);
        let projects_root = crate::domain_schedule_runner::options_at(&self.home).ok()
            .map(|file| file.projects_root(&crate::options::user_home()))
            .filter(|root| root.is_dir());
        let prepared = self.domain.bind_orchestrator(&run_id)
            .map_err(|error| anyhow::anyhow!("cannot bind the execution's capability: {error}"))
            .and_then(|capability| Ok((capability, self.prompt(&runtime, projects_root.as_deref(), &claimed)?)));
        let (capability, prompt) = match prepared {
            Ok(prepared) => prepared,
            Err(error) => return self.failed(runtime, &run, format!("{error:#}"), None),
        };
        let directory = self.home.join("runtime/orchestrator");
        let home = self.home.clone();
        let spawned_domain = Arc::clone(&self.domain);
        #[cfg(any(test, feature = "test-hooks"))]
        let spawned_capability = capability.expose_for_child_environment().to_owned();
        let mark_spawned = move || {
            let mut runtime = load(&home)?;
            if let Some(run) = runtime.active_run.as_mut() { run.spawned = true; }
            save(&home, &runtime)?;
            spawned_domain.orchestrator_progress();
            #[cfg(any(test, feature = "test-hooks"))]
            faults::spawned(&home, &spawned_capability);
            Ok(())
        };
        let mut stopped = stop.clone();
        let cancel = async {
            loop {
                if *stopped.borrow() || load(&self.home).is_ok_and(|runtime| runtime.cancel_requested) { break; }
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                    changed = stopped.changed() => {
                        if changed.is_err() { std::future::pending::<()>().await; }
                    }
                }
            }
        };
        let output = match self.domain.sdk() {
            Some((backend, _)) => crate::orchestrator_provider::execute(crate::orchestrator_provider::Batch {
                owner, domain: Arc::clone(&self.domain), backend: backend.clone(), home: self.home.clone(), directory: directory.clone(),
                node_plugin, orchestrator_plugin, projects_root: projects_root.clone(),
                capability: capability.expose_for_child_environment().to_owned(),
                session: session.clone(), prompt, selection: selection.clone(),
            }, mark_spawned, cancel).await,
            None => {
                drop(owner);
                process::Output::not_started(crate::sdk_owner::NO_HOST)
            }
        };
        let changes = self.domain.end_execution(&run_id);
        let logical_stop = if output.stream.denied { Some(BatchStop::Denied) }
            else if output.stream.native_request_rejected { Some(BatchStop::Interrupted) }
            else { None };
        if let Some(stop) = logical_stop {
            self.remember_stop(&run, BatchProblem { stop, calls: changes.clone(), subjects: self.subjects(&claimed),
                error: output.stream.error.clone().unwrap_or_else(|| "The provider requires explicit reconciliation before retrying".into()) });
        }
        #[cfg(any(test, feature = "test-hooks"))]
        watchdog::tests::draining(&run_id);
        self.domain.drain_batch_operations(&run_id).await?;
        let effects_confirmed = self.domain.confirmed_batch_effects(&run_id,&changes)?;
        let restarted = *stop.borrow();
        let mut runtime = load(&self.home)?;
        if !runtime.cancel_requested && (output.stream.storage_failure || restarted) && effects_confirmed && !output.stream.denied
            && !output.stream.native_request_rejected {
            log!("orchestrator: batch {run_id} stopped before its end was recorded (storage failure: {}, restarted: {restarted}): {}",
                output.stream.storage_failure, output.stream.error.as_deref().unwrap_or("no error was reported"));
            self.repair_incomplete().await?;
            return Ok(());
        }
        let Some(run) = runtime.active_run.clone() else { return Ok(()) };
        let end = if runtime.cancel_requested {
            let has_effects = !changes.is_empty() || !self.domain.store.lock_or_exit("the domain store").tool_operations(&run_id)?.is_empty();
            BatchEnd::Cancelled { effects: has_effects.then_some(changes.clone()) }
        } else if output.stream.denied {
            BatchEnd::Denied { calls:changes.clone(),why:output.stream.error.clone().unwrap_or_else(|| "The provider denied an Alondra tool".into()) }
        } else if output.stream.native_request_rejected {
            BatchEnd::Interrupted { calls: changes.clone(), why: output.stream.error.clone().unwrap_or_else(|| "The native request was rejected; correct it before explicitly retrying".into()) }
        } else if let Some(reset) = output.stream.limit() && effects_confirmed {
            BatchEnd::Limited { reset }
        } else if !effects_confirmed {
            BatchEnd::Interrupted { calls: changes.clone(), why: "Some admitted mutations have no confirmed durable result; reconcile them before continuing".into() }
        } else {
            match output.end() {
                BatchEnd::NotStarted { why } if !changes.is_empty() => BatchEnd::Interrupted { calls: changes.clone(),why },
                other => other,
            }
        };
        crate::metrics_capture::orchestrator_provider_batch(&self.domain, &session, selection.account.provider, &output.stream.spent, now_millis());
        if let Some(context) = output.stream.context() {
            log!("orchestrator: batch {} read {} tokens in its first call and {} in its last (window {})",
                run.id, context.first, context.last, context.window.map_or("unknown".to_owned(), |window| window.to_string()));
            runtime.last_context = Some(context);
        }
        if !matches!(end, BatchEnd::Handled | BatchEnd::Cancelled { .. })
            && let Some(reset) = output.stream.limit()
        {
            crate::orchestrator_provider::record_limit(&self.domain, &selection, reset, now_millis());
        }
        if !matches!(end, BatchEnd::Handled) {
            log!("orchestrator: batch {} did not end handled ({:?}); result received: {}, success: {}, denied: {}, storage failure: {}, native request rejected: {}, restarted: {}; admitted {:?}; unconfirmed {:?}",
                run.id, end, output.stream.result, output.stream.success, output.stream.denied, output.stream.storage_failure,
                output.stream.native_request_rejected, restarted, changes, self.domain.unconfirmed_operations(&run.id));
        }
        let problem = |stop: BatchStop, calls: Vec<String>| BatchProblem { stop, calls, subjects: self.subjects(&claimed), error: String::new() };
        match end {
            BatchEnd::Cancelled { effects: None } => self.acknowledge(&run.id),
            BatchEnd::Cancelled { effects: Some(calls) } => {
                self.failed(runtime, &run, "Orchestrator execution was cancelled; check its effects before retrying".into(), Some(problem(BatchStop::Cancelled, calls)))
            }
            BatchEnd::Limited { .. } => {
                if !changes.is_empty() {
                    runtime.batch_handoffs.push(crate::orchestrator_provider::BatchHandoff { run_id: run.id.clone(), selection: selection.clone(), reason: "The selected account exhausted its quota after confirmed effects".into() });
                }
                self.finish(&run, State::Retryable, Some("orchestrator_usage_exhausted"))?;
                runtime.active_run = None;
                runtime.last_error = None;
                runtime.failure = None;
                runtime.retry_at = None;
                self.rested(&mut runtime);
                save(&self.home, &runtime)
            }
            BatchEnd::Handled => {
                runtime.note = output.stream.note.as_deref().map(|note| bounded_note(note, MAX_NOTE_BYTES)).filter(|note| !note.is_empty());
                self.handled(runtime, &run, &asked)
            }
            BatchEnd::NotStarted { why } => self.failed(runtime, &run, why, None),
            BatchEnd::Interrupted { why, .. } if effects_confirmed && !output.stream.denied && !output.stream.native_request_rejected => {
                runtime.batch_handoffs.push(crate::orchestrator_provider::BatchHandoff {
                    run_id: run.id.clone(), selection: selection.clone(),
                    reason: "The native turn ended after its effects were confirmed. Inspect durable results and continue only unfinished goals.".into(),
                });
                save(&self.home, &runtime)?;
                self.failed(runtime, &run, why, None)
            }
            BatchEnd::Interrupted { calls, why } => self.failed(runtime, &run, why, Some(problem(BatchStop::Interrupted, calls))),
            BatchEnd::Denied { calls, why } => self.failed(runtime, &run, why, Some(problem(BatchStop::Denied, calls))),
        }
    }

    /// The batch's turn ended with its result: its messages were handed over. When nothing else
    /// waits for the orchestrator, whatever it left without an outcome is settled now, in this
    /// step; a message still waiting is the next batch's, and settling before it would close or
    /// remind about a request the orchestrator has not seen yet.
    fn handled(&self, mut runtime: Runtime, run: &Run, (requests, suggestions): &(Vec<RequestId>, Vec<InteractionId>)) -> Result<()> {
        self.domain.store.lock_or_exit("the domain store").finish_batch(&run.id)?;
        self.domain.live.orchestrator_runs.lock_or_recover("the orchestrator runs").remove(&run.id);
        let settled = self.domain.store.lock_or_exit("the domain store").settle_orchestrator_batch(&run.id, State::Handed, None)?;
        if !settled { self.finish(run, State::Handed, None)?; }
        read_handed_notices(&self.domain);
        if let Err(error) = self.domain.announce_request_tasks() {
            log!("orchestrator: batch {} could not announce its tasks: {error}", run.id);
        }
        runtime.active_run = None;
        runtime.last_error = None;
        runtime.failure = None;
        runtime.retry_at = None;
        runtime.consecutive_failures = 0;
        runtime.pending_resolutions.clear();
        self.rested(&mut runtime);
        let prior_handoffs = std::mem::take(&mut runtime.batch_handoffs);
        if let Some(problem) = runtime.stuck_problem.take() {
            let _ = self.domain.withdraw_problem(&InteractionId(problem));
        }
        for handoff in prior_handoffs {
            self.domain.store.lock_or_exit("the domain store").finish_batch(&handoff.run_id)?;
            self.domain.live.orchestrator_runs.lock_or_recover("the orchestrator runs").remove(&handoff.run_id);
        }
        save(&self.home, &runtime)?;
        self.domain.orchestrator_progress();
        crate::orchestrator_inbox::hand_replans(&self.domain);
        if !self.pending()?.is_empty() {
            return Ok(());
        }
        crate::orchestrator_inbox::settle(&self.domain);
        let state = self.domain.state()?;
        let left = crate::orchestrator_inbox::unresolved(&state, requests, suggestions);
        if !left.is_empty() {
            log!("orchestrator: batch {} left without an outcome: {}", run.id, left.join(", "));
        }
        Ok(())
    }

    /// What a batch was about, as the user knows it: the cards and tasks its messages came from.
    fn subjects(&self, messages: &[OutboxMessage]) -> Vec<String> {
        let Ok(state) = self.domain.state() else { return vec![] };
        let mut subjects: Vec<String> = vec![];
        for message in messages {
            let interaction = message.interaction_id.as_ref().and_then(|id| state.interactions.get(&InteractionId(id.clone())));
            let task_id = message.task_id.clone().or_else(|| match interaction.map(|interaction| &interaction.scope) {
                Some(Scope::Task { task_id }) => Some(task_id.0.clone()),
                _ => None,
            });
            let task = task_id.and_then(|id| state.tasks.values().find(|task| task.id.0 == id));
            let subject = match (interaction, task) {
                (Some(interaction), Some(task)) => format!("{}: {}", task.title, interaction.card.effective_title()),
                (Some(interaction), None) => interaction.card.effective_title().to_owned(),
                (None, Some(task)) => task.title.clone(),
                (None, None) => message.message.lines().next().unwrap_or_default().chars().take(SUBJECT_LINE).collect(),
            };
            if !subject.trim().is_empty() && !subjects.contains(&subject) {
                subjects.push(subject);
            }
        }
        subjects
    }

    /// A run that changed nothing (`problem` is None) is offered again with a fresh prompt; one
    /// that may have changed something is the user's call.
    /// A turn of the orchestrator ended, whatever it did: it rests, and what it still has to
    /// re-plan is handed to it again.
    fn rested(&self, runtime: &mut Runtime) {
        runtime.rests = runtime.rests.saturating_add(1);
        self.domain.live.orchestrator_rests.store(runtime.rests, std::sync::atomic::Ordering::Relaxed);
    }

    fn failed(&self, mut runtime: Runtime, run: &Run, error: String, problem: Option<BatchProblem>) -> Result<()> {
        self.rested(&mut runtime);
        let done = self.failed_batch(runtime, run, error, problem);
        crate::orchestrator_inbox::hand_replans(&self.domain);
        done
    }

    fn failed_batch(&self, mut runtime: Runtime, run: &Run, error: String, problem: Option<BatchProblem>) -> Result<()> {
        runtime.consecutive_failures = runtime.consecutive_failures.saturating_add(1);
        runtime.last_error = Some(error);
        let Some(problem) = problem else {
            self.finish(run, State::Retryable, Some("orchestrator_launch_failed"))?;
            runtime.active_run = None;
            let delay = 30_000_i64.saturating_mul(1_i64 << runtime.consecutive_failures.min(5));
            runtime.retry_at = Some(now_millis().saturating_add(delay));
            runtime.failure = Some(Failure::Launch);
            if runtime.consecutive_failures >= STUCK_AFTER && runtime.stuck_problem.is_none() {
                runtime.stuck_problem = self.raise_stuck_problem(&run.id, runtime.consecutive_failures, runtime.last_error.as_deref().unwrap_or_default());
            }
            return save(&self.home, &runtime);
        };
        let mut problem = problem;
        problem.error = runtime.last_error.clone().unwrap_or_default();
        self.remember_stop(run, problem.clone());
        runtime.retry_at = None;
        runtime.cancel_requested = false;
        runtime.failure = Some(match problem.stop { BatchStop::Denied => Failure::Denied, BatchStop::Cancelled => Failure::Cancelled, BatchStop::Interrupted => Failure::Interrupted });
        save(&self.home, &runtime)?;
        self.finish(run, State::Uncertain, Some("orchestrator_execution_ambiguous"))?;
        self.raise_batch_problem(&run.id, problem, runtime.last_error.as_deref().unwrap_or_default());
        *self.domain.live.orchestrator_stop.lock_or_recover("the orchestrator stop") = None;
        Ok(())
    }

    fn remember_stop(&self, run: &Run, problem: BatchProblem) {
        *self.domain.live.orchestrator_stop.lock_or_recover("the orchestrator stop") = Some((run.clone(), problem));
    }

    fn resume_stopped(&self) -> Result<()> {
        let pending = self.domain.live.orchestrator_stop.lock_or_recover("the orchestrator stop").clone();
        if let Some((run, mut problem)) = pending {
            let mut runtime = load(&self.home)?;
            if runtime.cancel_requested {
                problem.stop = BatchStop::Cancelled;
                problem.error = "Orchestrator execution was cancelled; check its effects before retrying".into();
            }
            runtime.active_run = Some(run.clone());
            self.failed(runtime, &run, problem.error.clone(), Some(problem))?;
        }
        Ok(())
    }

    /// Repeated unsuccessful attempts leave one recovery question while automatic retries continue.
    fn raise_stuck_problem(&self, run_id: &str, failures: u32, error: &str) -> Option<String> {
        let id = format!("{STUCK_PROBLEM_PREFIX}{run_id}");
        let problem = StuckProblem { failures, error: error.to_owned() };
        let (need, why) = problem.text(alondra_core::views::Lang::En);
        let option = QuestionOption { id: STUCK_RETRY.into(), label: STUCK_RETRY_LABEL.into(), description: None };
        match self.domain.raise_worded_problem(&id, Some(SystemCard::StuckProblem), STUCK_CAUSE, need, why, problem.to_body(), vec![option], Some(STUCK_RETRY)) {
            Ok(_) => Some(id),
            Err(error) => {
                log!("orchestrator: could not tell the user Claude does not start: {error:?}");
                None
            }
        }
    }

    /// A batch whose effects are unknown is the user's call: a Problem card with the ways out,
    /// which `apply_problem_answers` carries out.
    fn raise_batch_problem(&self, run_id: &str, mut problem: BatchProblem, error: &str) {
        let option = |id: &str, label: &str| QuestionOption { id: id.into(), label: label.into(), description: None };
        problem.error = error.to_owned();
        let (need, why) = problem.text(alondra_core::views::Lang::En);
        let raised = self.domain.raise_worded_problem(
            &format!("{PROBLEM_PREFIX}{run_id}"), Some(SystemCard::BatchProblem { run: run_id.to_string() }),
            BATCH_CAUSE,
            need,
            why,
            problem.to_body(),
            vec![option(BATCH_CONTINUE, BATCH_CONTINUE_LABEL), option(BATCH_RETRY, BATCH_RETRY_LABEL)],
            Some(BATCH_CONTINUE),
        );
        if let Err(error) = raised {
            log!("orchestrator: could not raise the failed batch's Problem: {error:?}");
        }
    }

    /// The user's answer to a failed batch's Problem is the decision on that execution; a
    /// Problem whose execution was settled some other way stops waiting.
    pub(crate) fn apply_problem_answers(&self) -> Result<()> {
        let state = self.domain.state()?;
        let mut runtime = load(&self.home)?;
        if let Some(problem) = runtime.stuck_problem.clone()
            && let Some(interaction) = state.interactions.get(&InteractionId(problem))
            && (interaction.withdrawn || matches!(&interaction.payload, InteractionPayload::Question { answer: Some(_), .. }))
        {
            runtime.stuck_problem = None;
            if !interaction.withdrawn {
                runtime.retry_at = None;
            }
            save(&self.home, &runtime)?;
        }
        let active = runtime.active_run.map(|run| run.id);
        for (id, interaction) in &state.interactions {
            let Some(SystemCard::BatchProblem { run: run_id }) = &interaction.system_card else { continue };
            let run_id = run_id.as_str();
            let InteractionPayload::Question { answer, .. } = &interaction.payload else { continue };
            if interaction.withdrawn {
                continue;
            }
            match answer {
                Some(_) if active.as_deref() != Some(run_id) => {}
                Some(answer) => {
                    let applied = match answer.option_id.as_str() {
                        BATCH_RETRY => self.retry(run_id),
                        BATCH_CONTINUE => self.acknowledge(run_id),
                        PROBLEM_STOP => self.cancel(run_id),
                        other => Err(anyhow::anyhow!("unknown option {other}")),
                    };
                    if let Err(error) = applied {
                        log!("orchestrator: answer {} to {id} no longer applies: {error:#}", answer.option_id);
                    }
                }
                None if active.as_deref() != Some(run_id) => {
                    let _ = self.domain.withdraw_problem(id);
                }
                None => {}
            }
        }
        Ok(())
    }
}

/// When each Request was made, and now, so the prompt can say how long every open one has waited.
struct Ages {
    now: i64,
    opened: std::collections::BTreeMap<alondra_core::domain::RequestId, i64>,
}

impl Ages {
    fn of(&self, request: &alondra_core::domain::RequestId) -> Option<String> {
        const HOUR: i64 = 60 * 60 * 1_000;
        let waited = self.now.saturating_sub(*self.opened.get(request).filter(|at| **at > 0)?).max(0);
        Some(match waited / HOUR {
            0 => "opened less than an hour ago".to_owned(),
            hours if hours < 48 => format!("opened {hours} hours ago"),
            hours => format!("opened {} days ago", hours / 24),
        })
    }
}

/// The durable domain as the orchestrator needs it to act on a batch, which starts a new
/// conversation: every open Request, every question it asked that waits for its answer, and
/// every Task not archived travel in the prompt.
fn summary(state: &DomainState, ages: &Ages, choices: impl Fn(&alondra_core::domain::RequestId) -> Option<String>) -> String {
    let mut text = String::from("\nDURABLE DOMAIN (open Requests and Tasks not archived):\n");
    let one_line = |body: &str| body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(SUMMARY_LINE).collect::<String>();
    let state_name = |task: &alondra_core::domain::Task| serde_json::to_value(task.state).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
    let open: Vec<_> = state.requests.values().filter(|request| !request.closed).collect();
    let whole: usize = open.iter().map(|request| request.body.len()).sum();
    let tight = whole > MAX_SUMMARY_BYTES;
    for request in open {
        let tasks: Vec<_> = state.tasks.values().filter(|task| task.request_id == request.id).collect();
        let unsplit = tasks.is_empty();
        let body = if unsplit && !tight { request.body.trim().to_owned() } else { one_line(&request.body) };
        let age = ages.of(&request.id).map(|age| format!(" ({age})")).unwrap_or_default();
        let marker = if unsplit {
            " (no task of its own)".to_owned()
        } else {
            let listed = tasks.iter().map(|task| format!("{} {}", task.id, state_name(task))).collect::<Vec<_>>().join(", ");
            let ended = if tasks.iter().all(|task| task.over()) { "; every task of it has ended: close it with close_request, or create its next tasks" } else { "" };
            format!(" (tasks: {listed}{ended})")
        };
        text.push_str(&format!("request {}{age}{marker}: {body}\n", request.id));
        if tight && !unsplit {
            continue;
        }
        let schedule = state.schedule.runs.values().find(|run| run.request_id.as_ref() == Some(&request.id))
            .and_then(|run| state.schedule.schedules.get(&run.schedule_id));
        if let Some(schedule) = schedule {
            text.push_str(&format!("  made by the schedule “{}”: its options below are the plan of every task you create for it\n", one_line(&schedule.title)));
        }
        if let Some(chosen) = choices(&request.id) {
            text.push_str(&chosen);
            text.push('\n');
        }
    }
    for card in state.interactions.values().filter(|card| matches!(card.author, Author::Orchestrator { .. }) && !card.withdrawn) {
        let open = matches!(&card.payload, InteractionPayload::Question { answer: None, .. } | InteractionPayload::ActionRequest { answer: None, .. });
        if !open {
            continue;
        }
        let about = match &card.scope {
            Scope::Orchestrator { request_id: Some(request) } => format!(" about request {request}"),
            Scope::Task { task_id } => format!(" about task {task_id}"),
            _ => String::new(),
        };
        text.push_str(&format!("your question {}{about}, still unanswered: {}\n", card.id, one_line(&card.body)));
    }
    let tasks_start = text.len();
    for task in state.tasks.values().filter(|task| !task.archived) {
        let agent = task.agent_id.as_ref().map_or("none".to_owned(), |agent| agent.0.clone());
        let pr = task.pr_id.as_ref().map(|pr| format!(" pr={pr}")).unwrap_or_default();
        text.push_str(&format!("task {} of request {}: \"{}\" state={} agent={agent}{pr}\n",
            task.id, task.request_id, one_line(&task.title), state_name(task)));
        if text.len() - tasks_start > MAX_SUMMARY_BYTES {
            text.push_str("(more Tasks not listed)\n");
            break;
        }
    }
    text
}

/// At most `limit` bytes of `note`, cut at a character.
fn bounded_note(note: &str, limit: usize) -> String {
    let note = note.trim();
    let end = note.char_indices().map(|(at, c)| at + c.len_utf8()).take_while(|end| *end <= limit).last().unwrap_or(0);
    note[..end].to_owned()
}

/// The orchestrator phase and the actions available to the user.
pub fn status(executor: &Executor) -> Result<serde_json::Value> {
    let runtime = load(&executor.home)?;
    let queue_depth = executor.domain.live_read().pending_messages()?.into_iter()
        .filter(|message| message.recipient_kind == "orchestrator").count();
    let ambiguous = runtime.active_run.is_some() && runtime.last_error.is_some();
    let phase = match (&runtime.active_run, &runtime.last_error) {
        (Some(_), None) => "running",
        (_, Some(_)) => "failed",
        (None, None) => "idle",
    };
    Ok(serde_json::json!({
        "phase": phase, "can_retry": ambiguous, "can_cancel": runtime.active_run.is_some() && !ambiguous,
        "cancel_requested": runtime.cancel_requested,
        "active_run_id": runtime.active_run.as_ref().map(|run| run.id.clone()),
        "queue_depth": queue_depth, "last_error": runtime.last_error, "failure": runtime.failure, "retry_at": runtime.retry_at,
        "waits_for_sign_in": executor.domain.live.account_wait.is_waiting(),
    }))
}

pub fn executor(domain: Arc<DomainRuntime>, home: &Path) -> Executor {
    Executor { domain, home: home.to_owned(), claude_home: crate::watcher::claude_home() }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision { Cancel, Acknowledge, Retry }

/// `POST /orchestrator/{cancel,acknowledge,retry}`, the user's decision on an execution.
pub fn decision_route(path: &str) -> Option<Decision> {
    match path {
        "/orchestrator/cancel" => Some(Decision::Cancel),
        "/orchestrator/acknowledge" => Some(Decision::Acknowledge),
        "/orchestrator/retry" => Some(Decision::Retry),
        _ => None,
    }
}

/// The decision as a local response: `{}` once applied, a conflict when no such execution waits.
pub fn decide(executor: &Executor, decision: Decision, body: Option<&serde_json::Value>) -> serde_json::Value {
    let Some(run_id) = body.and_then(|body| body.get("run_id")).and_then(serde_json::Value::as_str) else {
        return serde_json::json!({"status": 422, "body": DomainError::new("invalid_input", "run_id is required")});
    };
    let applicable = load(&executor.home).and_then(|runtime| {
        anyhow::ensure!(decision == Decision::Cancel || runtime.last_error.is_some(), "the running execution is not awaiting reconciliation");
        Ok(())
    });
    let applied = applicable.and_then(|()| match decision {
        Decision::Cancel => executor.cancel(run_id),
        Decision::Acknowledge => executor.acknowledge(run_id),
        Decision::Retry => executor.retry(run_id),
    });
    match applied {
        Ok(()) => serde_json::json!({"status": 200, "body": {}}),
        Err(error) => serde_json::json!({"status": 409, "body": DomainError::new("conflict", error.to_string())}),
    }
}

pub async fn run(domain: Arc<DomainRuntime>, home: PathBuf, claude_home: PathBuf) -> Result<()> {
    let executor = Arc::new(Executor { domain, home: home.clone(), claude_home });
    let directory = home.join("runtime/orchestrator");
    let _ownership = loop {
        match Ownership::acquire(&directory) {
            Ok(Some(lock)) => break lock,
            Ok(None) => {
                let run = load(&home).ok().and_then(|runtime| runtime.active_run.map(|run| run.id));
                if let Err(error) = process::terminate_orphan(&directory, run.as_deref()) {
                    log!("waiting for previous orchestrator ownership: {error:#}");
                }
            }
            Err(error) => log!("orchestrator ownership: {error}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    };
    loop {
        match executor.startup_recovery().await {
            Ok(_) => break,
            Err(error) => log!("orchestrator recovery will retry: {error:#}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    if let Some(session) = load(&home).ok().and_then(|runtime| runtime.session_id) {
        executor.bind_session(&session);
    }
    let mut changed = executor.domain.subscribe();
    let mut usage = executor.domain.live.usage_pause.subscribe();
    let mut watchdog = watchdog::Watchdog::default();
    loop {
        if let Err(error) = executor.supervised_tick(&mut watchdog).await {
            log!("orchestrator: {error:#}");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        tokio::select! {
            _ = changed.changed() => {},
            _ = usage.changed() => {},
            _ = tokio::time::sleep(Duration::from_secs(10)) => {},
        }
    }
}

/// An executor whose batches run through the pinned Node host with the official SDK and the fake
/// orchestrator Claude (`agent-host/orchestrator-fake-claude.py`), which answers each prompt as its
/// marker says. A wrapper around the fake records each start with the capability it got, and,
/// from files beside its capture, puts a marker in front of every prompt (`.marker`, or
/// `.marker-once` for the next start only), holds the first prompt before Claude sees it
/// (`.hold-before`) or adds fields to every result (`.result-extra`). A held turn waits on a FIFO
/// the test writes, never on a clock.
#[cfg(any(test, feature = "test-hooks"))]
pub mod testing {
    use super::*;
    use crate::orchestrator_process::Ownership;
    use crate::test_support::Directory;
    use alondra_core::domain::{CardKind, CardSpec, LaunchPlan};
    use std::collections::{BTreeMap, BTreeSet};
    use std::io::{Read, Write};
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    const WRAPPER: &str = r#"#!/usr/bin/python3
import json
import os
import sys

capture = os.environ['FIXTURE_CAPTURE']
with open(capture + '.starts', 'a') as stream:
    stream.write(os.environ.get('ALONDRA_DOMAIN_CAPABILITY', '') + '\n')
marker = None
for name in ('.marker', '.marker-once'):
    if os.path.exists(capture + name):
        with open(capture + name) as stream:
            marker = stream.read().strip()
        if name == '.marker-once':
            os.remove(capture + name)
        break
hold_before = os.path.exists(capture + '.hold-before')
extra = json.load(open(capture + '.result-extra')) if os.path.exists(capture + '.result-extra') else {}


class Inputs:
    def __iter__(self):
        for raw in sys.__stdin__:
            message = json.loads(raw)
            if message.get('type') == 'user':
                if hold_before:
                    with open(os.environ['FIXTURE_HELD'], 'w') as held:
                        held.write('held\n')
                    with open(os.environ['FIXTURE_RELEASE']) as release:
                        release.read(1)
                if marker:
                    message['message']['content'] = marker + ' ' + message['message']['content']
            yield json.dumps(message) + '\n'


class Outputs:
    def write(self, text):
        for line in text.splitlines(True):
            if extra and line.strip():
                value = json.loads(line)
                if value.get('type') == 'result':
                    value.update(extra)
                    line = json.dumps(value, ensure_ascii=False) + '\n'
            sys.__stdout__.write(line)

    def flush(self):
        sys.__stdout__.flush()


sys.stdin = Inputs()
sys.stdout = Outputs()
fake = FAKE
sys.argv = [fake] + sys.argv[1:]
exec(compile(open(fake).read(), fake, 'exec'), {'__name__': '__main__'})
"#;

    fn runfile(name: &str) -> PathBuf {
        std::fs::canonicalize(std::env::var(name).unwrap_or_else(|_| panic!("{name} names a runfile"))).unwrap()
    }

    fn fifo(path: &Path) {
        assert!(std::process::Command::new("/usr/bin/mkfifo").arg(path).status().unwrap().success());
    }

    /// Alondra's node and orchestrator plugins, registered in the returned Claude home.
    pub(crate) fn registered_plugins(root: &Path) -> PathBuf {
        let claude_home = root.join("claude-home");
        let repo = root.join("assets");
        for plugin in [crate::plugins::NODE, crate::plugins::ORCHESTRATOR] {
            std::fs::create_dir_all(repo.join(plugin).join(".claude-plugin")).unwrap();
            std::fs::write(repo.join(plugin).join(".claude-plugin/plugin.json"), "{}").unwrap();
        }
        std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
        std::fs::write(claude_home.join("plugins/known_marketplaces.json"), serde_json::json!({"alondra":{"installLocation":repo}}).to_string()).unwrap();
        claude_home
    }

    pub struct Fixture {
        pub(crate) executor: Executor,
        _ownership: Ownership,
        pub(crate) capture: PathBuf,
        pub(crate) held: PathBuf,
        pub(crate) release: PathBuf,
        pub(crate) claude_home: PathBuf,
        _state: crate::sdk_owner::tests::HostRoot,
        pub(crate) root: Directory,
    }

    /// A daemon home of its own whose orchestrator runs through the SDK.
    pub(crate) fn fixture() -> Fixture {
        let root = Directory::new();
        std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        Fixture::over(root, domain, home)
    }

    fn seed_command(domain:&DomainRuntime,id:&str,command:Command) {
        let state=domain.state().unwrap();
        domain.execute_as_system(CommandEnvelope {protocol_version:PROTOCOL_VERSION,data_epoch:state.data_epoch.clone(),command_id:id.into(),expected_revision:state.revision,command}).unwrap();
    }

    pub(crate) fn seed_native_metadata(domain:&DomainRuntime, home:&std::path::Path) {
        let option_path = alondra_core::options::path_in(&home);
            if !option_path.exists() {
                std::fs::write(&option_path, "[accounts]\nconnected=[\"claude\"]\n[model_profiles.claude.orchestrator]\nmode=\"ordered_candidates\"\ncandidates=[{model=\"opus\"}]\n").unwrap();
            }
            let configured = alondra_core::options::OptionsFile::parse(&std::fs::read_to_string(&option_path).unwrap()).unwrap();
            if !configured.accounts.is_connected(alondra_core::accounts::Provider::Claude) { return; }
            if configured.model_profiles.get(&alondra_core::accounts::Provider::Claude).is_none_or(|profiles| !profiles.contains_key(&alondra_core::model_selection::ModelProfile::Orchestrator)) {
                let mut profiles = configured.model_profiles.clone();
                profiles.entry(alondra_core::accounts::Provider::Claude).or_default().insert(alondra_core::model_selection::ModelProfile::Orchestrator,
                    alondra_core::model_selection::RoleModelPolicy::OrderedCandidates { candidates: vec![alondra_core::model_selection::ModelCandidate { model:"opus".into(),reasoning_effort:None }] });
                let text=std::fs::read_to_string(&option_path).unwrap();
                std::fs::write(&option_path,alondra_core::options::with_model_profiles(&text,&profiles).unwrap()).unwrap();
            }
            {
                use alondra_core::accounts::{AccountBilling,AccountId,AccountKey,AccountState,AccountStatus,Provider};
                use alondra_core::model_selection::{AccountModelCatalog,CatalogModel};
                let key = AccountKey::new(Provider::Claude,AccountId::default_account());
                if domain.read().state().accounts.is_empty() {
                seed_command(&domain,"fixture-account",Command::RecordAccounts { accounts: vec![AccountStatus { provider: Provider::Claude,id: key.account.clone(),name:"Default".into(),state:AccountState::SignedIn,native_identity: None, email:None,plan:None,billing:AccountBilling::default(),sign_ins: 0, profile: None }],reserve:alondra_core::usage::default_reserve() });
                }
                seed_command(&domain,"fixture-account-order",Command::RecordAccountConfiguration { order:vec![key.clone()],version:"fixture-accounts".into() });
                let file = alondra_core::options::OptionsFile::parse(&std::fs::read_to_string(&option_path).unwrap()).unwrap();
                let version = alondra_core::model_selection::profiles_version(&file.model_profiles);
                seed_command(&domain,"fixture-model-profiles",Command::RecordModelProfiles { profiles:file.model_profiles,version });
                seed_command(&domain,"fixture-model-catalog",Command::RecordModelCatalog { catalog:AccountModelCatalog { account:key,revision:"fixture-models".into(),models:vec![CatalogModel { model:"opus".into(),display_name:"Native fixture model".into(),reasoning_efforts:vec![],default_reasoning_effort:None,context_window:Some(200000),input_modalities:vec!["text".into()],is_default:false,hidden:false }],native_default:None,measured_at:0 } });
            }
    }

    impl Fixture {
        /// The orchestrator of `domain`, whose daemon home is `home`, through the SDK; its fakes live in `root`.
        pub fn over(root: Directory, domain: Arc<DomainRuntime>, home: PathBuf) -> Self {
            let claude_home = registered_plugins(&root);
            let fake = root.join("orchestrator-fake-claude.py");
            std::fs::copy(runfile("FIXTURE_ORCHESTRATOR_NATIVE"), &fake).unwrap();
            let native = root.join("native");
            std::fs::write(&native, WRAPPER.replace("FAKE", &format!("{:?}", fake.to_str().unwrap()))).unwrap();
            std::fs::set_permissions(&native, std::fs::Permissions::from_mode(0o700)).unwrap();
            let (capture, held, release) = (root.join("capture.json"), root.join("held"), root.join("release"));
            fifo(&held);
            fifo(&release);
            let environment = BTreeMap::from([("HOME".to_owned(), root.to_str().unwrap().to_owned()), ("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
            let mut native_environment = environment.clone();
            for (name, path) in [("FIXTURE_CAPTURE", &capture), ("FIXTURE_HELD", &held), ("FIXTURE_RELEASE", &release)] {
                native_environment.insert(name.into(), path.to_str().unwrap().into());
            }
            let (node, host) = (runfile("FIXTURE_NODE"), runfile("FIXTURE_HOST"));
            let state = crate::sdk_owner::tests::HostRoot::new();
            let backend = crate::sdk_owner::SdkBackend {
                node_sha256: crate::sdk_owner::backend::digest(&node).unwrap(),
                host_sha256: crate::sdk_owner::backend::digest(&host).unwrap(),
                native_sha256: crate::sdk_owner::backend::digest(&native).unwrap(),
                node,
                host,
                native,
                host_environment: environment,
                native_environment,
                confinement: None,
                state_root: state.join("h"),
                io_timeout_seconds: 30,
                installed_native: false,
            provider: alondra_core::accounts::Provider::Claude, origin: None,
            };
            domain.enable_sdk(backend);
            seed_native_metadata(&domain,&home);
            let ownership = Ownership::acquire(&home.join("runtime/orchestrator")).unwrap().unwrap();
            Self { executor: Executor { domain, home, claude_home: claude_home.clone() }, _ownership: ownership, capture, held, release, claude_home, _state: state, root }
        }

        pub(crate) fn domain(&self) -> &DomainRuntime {
            &self.executor.domain
        }

        pub(crate) fn run(&self, actor: Actor, id: &str, command: Command) {
            let state = self.domain().state().unwrap();
            self.domain().execute(&actor, &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
                command_id: id.into(), expected_revision: state.revision, command,
            }, None, || Ok(())).unwrap();
        }

        /// A node working on task `t` asks the orchestrator `body`: one orchestrator inbox message.
        pub(crate) fn ask(&self, n: u32, body: &str) {
            if self.domain().state().unwrap().tasks.is_empty() {
                self.run(Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
                self.run(Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
                self.run(Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
                    task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
                    review_prs: false, dependencies: BTreeSet::new(), stack: None,
                    launch: Some(LaunchPlan::new("node", "/tmp")),
                });
            }
            self.run(Actor::Agent { agent_id: "a".into() }, &format!("question-{n}"), Command::AskQuestion {
                attachments: vec![], interaction_id: format!("q{n}").into(), scope: Scope::Task { task_id: "t".into() },
                recipient: Recipient::Orchestrator, body: body.into(), blocking: false,
                options: vec![QuestionOption { id: "here".into(), label: "Here".into(), description: None }],
                card: CardSpec::derived(CardKind::Provide, "Answer the question"),
            });
        }

        /// A message from the user: once the orchestrator handled it, `settle` closes it.
        pub(crate) fn tell(&self, id: &str, body: &str) {
            self.run(Actor::User { user_id: "u".into() }, id, Command::CreateRequest { request_id: id.into(), body: body.into(), auto_merge: false, maintenance: false, message: true });
        }

        fn lines(&self, suffix: &str) -> Vec<String> {
            match std::fs::read_to_string(format!("{}{suffix}", self.capture.display())) {
                Ok(text) => text.lines().map(str::to_owned).collect(),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
                Err(error) => panic!("{error}"),
            }
        }

        /// Every prompt the fake Claude got, in order.
        pub fn prompts(&self) -> Vec<String> {
            self.lines(".prompts").iter().map(|line| serde_json::from_str(line).unwrap()).collect()
        }

        /// The capability each start of the fake Claude got, one per start.
        pub(crate) fn starts(&self) -> Vec<String> {
            self.lines(".starts")
        }

        /// What the last start of the fake Claude was launched with.
        pub(crate) fn arguments(&self) -> Vec<String> {
            let capture: serde_json::Value = serde_json::from_slice(&std::fs::read(&self.capture).unwrap()).unwrap();
            serde_json::from_value(capture["args"].clone()).unwrap()
        }

        /// Every prompt from now on starts with `marker` until `clear_marker`.
        pub(crate) fn mark(&self, marker: &str) {
            std::fs::write(format!("{}.marker", self.capture.display()), marker).unwrap();
        }

        /// Only the next start's prompts start with `marker`.
        pub(crate) fn mark_once(&self, marker: &str) {
            std::fs::write(format!("{}.marker-once", self.capture.display()), marker).unwrap();
        }

        pub(crate) fn clear_marker(&self) {
            std::fs::remove_file(format!("{}.marker", self.capture.display())).unwrap();
        }

        /// The next starts hold their first prompt before Claude sees it, until `release`.
        pub(crate) fn hold_before_claude(&self) {
            std::fs::write(format!("{}.hold-before", self.capture.display()), "").unwrap();
        }

        /// Every result the fake Claude reports carries `fields` too.
        pub(crate) fn add_to_results(&self, fields: serde_json::Value) {
            std::fs::write(format!("{}.result-extra", self.capture.display()), fields.to_string()).unwrap();
        }

        pub(crate) fn pending(&self) -> usize {
            self.domain().store.lock().unwrap().pending_messages().unwrap().into_iter().filter(|m| m.recipient_kind == "orchestrator").count()
        }

        pub(crate) fn attempts(&self) -> Vec<String> {
            self.domain().store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
                .filter(|i| i.recipient_kind == "orchestrator").map(|i| i.state).collect()
        }

        pub(crate) fn cards(&self, prefix: &str) -> Vec<String> {
            self.domain().state().unwrap().interactions.keys().filter(|id| id.0.starts_with(prefix)).map(|id| id.0.clone()).collect()
        }

        pub(crate) fn unresolved(&self) -> Vec<String> {
            let state = self.domain().state().unwrap();
            let requests: Vec<RequestId> = state.requests.keys().cloned().collect();
            let suggestions: Vec<InteractionId> = state.interactions.keys().cloned().collect();
            crate::orchestrator_inbox::unresolved(&state, &requests, &suggestions)
        }

        pub async fn tick(&self) {
            self.executor.tick().await.unwrap();
        }

        /// Blocks until the fake Claude holds its turn.
        pub(crate) fn wait_held(&self) {
            let mut text = String::new();
            std::fs::File::open(&self.held).unwrap().read_to_string(&mut text).unwrap();
            assert_eq!(text, "held\n");
        }

        /// Waits, off the runtime, until the fake Claude holds its turn.
        pub(crate) async fn held(&self) {
            let held = self.held.clone();
            tokio::task::spawn_blocking(move || {
                let mut text = String::new();
                std::fs::File::open(held).unwrap().read_to_string(&mut text).unwrap();
            }).await.unwrap();
        }

        pub(crate) fn release(&self) {
            std::fs::OpenOptions::new().write(true).open(&self.release).unwrap().write_all(b"go").unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{Fixture, fixture};
    use super::*;
    use alondra_core::domain::*;

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// A question from an Agent to the orchestrator: one orchestrator inbox message.
    fn ask_orchestrator(f: &Fixture, n: u32) {
        f.ask(n, &format!("Where do I put change {n}?"));
    }

    fn outbox(domain: &DomainRuntime) -> Vec<(i64, String)> {
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator").map(|m| (m.id, m.state)).collect()
    }

    /// The reminders `settle` sent back to the orchestrator when a batch left something open.
    fn reminders(domain: &DomainRuntime) -> Vec<String> {
        use alondra_core::orchestrator_cards::{UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX};
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator")
            .filter_map(|m| m.interaction_id)
            .filter(|id| [UNANSWERED_NOTICE_PREFIX, UNSETTLED_NOTICE_PREFIX, UNSPLIT_NOTICE_PREFIX].iter().any(|prefix| id.starts_with(prefix)))
            .collect()
    }

    /// The orchestrator's inbox without those reminders.
    fn unhandled(domain: &DomainRuntime) -> Vec<(i64, String)> {
        let reminders = reminders(domain);
        domain.store.lock().unwrap().pending_messages().unwrap().into_iter()
            .filter(|m| m.recipient_kind == "orchestrator" && m.interaction_id.as_ref().is_none_or(|id| !reminders.contains(id)))
            .map(|m| (m.id, m.state)).collect()
    }

    fn attempts(domain: &DomainRuntime) -> Vec<(i64, String, Option<String>)> {
        domain.store.lock().unwrap().delivery_evidence_page(0).unwrap().into_iter()
            .filter(|i| i.recipient_kind == "orchestrator").map(|i| (i.message_id, i.state, i.reason)).collect()
    }

    fn last_prompt(f: &Fixture) -> String {
        f.prompts().pop().expect("a batch reached Claude")
    }

    fn again(f: &Fixture) {
        let mut runtime = load(&f.executor.home).unwrap();
        runtime.retry_at = None;
        save(&f.executor.home, &runtime).unwrap();
    }

    #[tokio::test]
    async fn a_new_request_is_weighed_against_the_open_tasks_before_it_gets_one() {
        let f = fixture();
        f.tell("r-waiting", "A node that waits holds no memory");
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("(no task of its own): A node that waits holds no memory"), "{prompt}");
        assert!(prompt.contains("Before create_task, compare each such Request with the open Tasks listed below")
            && prompt.contains("close the Request with close_request and folded_into naming that Task"), "{prompt}");
    }

    #[tokio::test]
    async fn a_disk_full_finishing_a_batch_does_not_silently_stall_the_next_request() {
        let f = fixture();
        f.tell("before-full", "First batch");
        let fault = faults::Injection::at(&f.executor.home, "runtime_write", 3);
        let error = f.executor.tick().await.unwrap_err();
        assert_eq!(error.downcast_ref::<std::io::Error>().unwrap().raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(fault.hits(), 1);
        assert!(load(&f.executor.home).unwrap().active_run.is_some());
        drop(fault);
        f.tell("after-full", "The request after disk space returns");
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "writes recovered but active_run silently blocked the next batch");
        let evidence = f.domain().store.lock().unwrap().delivery_evidence_page(0).unwrap();
        assert_eq!(evidence.iter().filter(|attempt| attempt.recipient_kind == "orchestrator").count(), 2, "a finished batch must not be replayed");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());
        assert!(f.cards(PROBLEM_PREFIX).is_empty());
    }

    #[tokio::test]
    async fn a_batch_reaches_one_invocation_with_a_bound_capability_and_is_handed_once() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        ask_orchestrator(&f, 2);
        assert_eq!(outbox(domain).len(), 2);
        f.tick().await;
        assert_eq!(f.starts().len(), 1);
        let prompt = last_prompt(&f);
        assert!(!prompt.contains("data_epoch=") && prompt.contains("Where do I put change 1?")
            && prompt.contains("Where do I put change 2?") && !prompt.contains("inbox message")
            && !prompt.contains("interaction_id=") && prompt.contains("new conversation"), "{prompt}");
        assert!(prompt.contains(" (tasks: t not_started): Work\n") && prompt.contains("task t of request r: \"Task\" state=not_started agent=a\n"), "{prompt}");
        assert!(unhandled(domain).is_empty());
        assert!(reminders(domain).is_empty(), "the questions it left unanswered wait for their deadline, not for the batch's end");
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "handed"));
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.session_id.is_some() && runtime.last_error.is_none());
        let session = runtime.session_id.clone().unwrap();
        assert_eq!(domain.state().unwrap().orchestrator_session, Some(session.as_str().into()), "the domain names the orchestrator by its session");
        let state = domain.state().unwrap();
        assert!(state.orchestrator_batch_base.is_some_and(|base| state.tasks.values().all(|task| task.created_revision < base)),
            "the batch starts after every task it was shown: {:?}", state.orchestrator_batch_base);
        {
            let store = domain.store.lock().unwrap();
            for item in store.delivery_evidence_page(0).unwrap().into_iter().filter(|i| i.recipient_kind == "orchestrator") {
                assert_eq!(store.orchestrator_delivery(item.attempt_id).unwrap().as_deref(), Some(session.as_str()), "each message records the session it was handed to");
            }
        }
        let token = f.starts()[0].clone();
        assert_eq!(token.len(), 64);
        assert!(domain.resolve_bearer(&token).is_err(), "the execution's bearer is revoked when it ends");
        for n in [1, 2] {
            command(domain, Actor::Orchestrator { generation: 0 }, &format!("answer-{n}"), Command::AnswerQuestion {
                attachments: vec![], interaction_id: format!("q{n}").into(), option_id: "here".into(), inputs: Default::default(),
            });
        }
        f.tick().await;
        assert_eq!(f.starts().len(), 1, "nothing pending, no launch");
        ask_orchestrator(&f, 3);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("new conversation") && prompt.contains("Where do I put change 3?")
            && !prompt.contains("Where do I put change 1?") && !prompt.contains("inbox message"), "{prompt}");
        assert_ne!(load(&f.executor.home).unwrap().session_id, runtime.session_id, "every batch is a conversation of its own");
    }

    /// Recorded from the 2026-10-03 run e8ab8f90: Read on the projects root was refused, the
    /// orchestrator went on to create and launch its task, and Claude reported success.
    #[tokio::test]
    async fn a_run_that_worked_around_a_refused_read_is_handed_and_frees_the_orchestrator() {
        let f = fixture();
        let domain = &f.executor.domain;
        let projects = f.root.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(alondra_core::options::path_in(&f.executor.home), format!("projects_root = {:?}\n", projects.to_str().unwrap())).unwrap();
        f.add_to_results(serde_json::json!({"permission_denials": [{"tool_name": "Read", "tool_use_id": "toolu_01RqEHPzQNkeM7Upm8Mc7Yqu",
            "tool_input": {"file_path": "/Users/example/Projects"}}]}));
        ask_orchestrator(&f, 1);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains(&format!("Projects root, readable through the role's file tools: {}\n", projects.display())), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert_eq!(runtime.last_error, None);
        assert_eq!(runtime.consecutive_failures, 0);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "handed"));
        ask_orchestrator(&f, 2);
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "the next batch runs at once");
    }

    #[tokio::test]
    async fn a_run_that_cannot_be_prepared_is_released_for_retry_instead_of_left_running() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let blocked = f.executor.home.join("domain-capabilities.json.next");
        std::fs::create_dir_all(blocked.join("occupied")).unwrap();
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none(), "{runtime:?}");
        assert!(runtime.last_error.as_deref().is_some_and(|error| error.contains("capability")), "{runtime:?}");
        assert!(runtime.retry_at.is_some());
        assert!(f.starts().is_empty(), "Claude is not started");
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "retryable" && reason.as_deref() == Some("orchestrator_launch_failed")));
        assert_eq!(status(&f.executor).unwrap()["phase"], "failed");
        assert_eq!(status(&f.executor).unwrap()["failure"], "launch");

        std::fs::remove_dir_all(&blocked).unwrap();
        again(&f);
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
        assert!(unhandled(domain).is_empty());
    }

    #[tokio::test]
    async fn stale_orchestrator_messages_expire_before_execution_but_boundary_messages_remain() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        ask_orchestrator(&f, 2);
        command(domain, Actor::System, "user-info", Command::PublishInformation {
            interaction_id: "info-user".into(), scope: Scope::Task { task_id: "t".into() },
            recipient: Recipient::User, body: "User notification".into(), attachments: vec![], card: CardSpec::derived(CardKind::Know, "Read the update"),
        });
        let cutoff = now_millis().saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS);
        {
            let mut store = domain.store.lock().unwrap();
            store.set_message_created_at_for_test(1, cutoff - 1).unwrap();
            store.set_message_created_at_for_test(2, cutoff).unwrap();
            let user_message = store.pending_messages().unwrap().into_iter()
                .find(|message| message.recipient_kind == "user").unwrap();
            store.set_message_created_at_for_test(user_message.id, cutoff - 1).unwrap();
            store.discard_expired_orchestrator_messages(cutoff).unwrap();
            assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "discarded");
            assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "pending");
            assert_eq!(store.message_outbox_state_for_test(user_message.id).unwrap(), "pending");
            store.set_message_created_at_for_test(
                2,
                now_millis().saturating_sub(crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS) + 60_000,
            ).unwrap();
        }

        f.tick().await;

        let store = domain.store.lock().unwrap();
        assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "discarded");
        assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "handed");
        assert_eq!(store.pending_messages().unwrap().iter().filter(|message| message.recipient_kind == "user").count(), 1,
            "user notifications retain their separate delivery policy");
        assert!(store.pending_messages().unwrap().iter().all(|message| message.id != 1));
        drop(store);
        assert_eq!(f.starts().len(), 1, "the fresh message remains eligible for execution");
    }

    #[test]
    fn orchestrator_claim_rechecks_expiry_and_keeps_the_exact_cutoff_eligible() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        ask_orchestrator(&f, 2);
        let created_at = 1_000;
        let mut store = domain.store.lock().unwrap();
        store.set_message_created_at_for_test(1, created_at).unwrap();
        store.set_message_created_at_for_test(2, created_at).unwrap();

        let exact_cutoff = store.claim_orchestrator_message_at_for_test(
            1,
            created_at + crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS,
        ).unwrap().expect("a message exactly 24 hours old remains eligible");
        assert_eq!(store.message_outbox_state_for_test(1).unwrap(), "dispatching");
        assert!(store.finish_message(exact_cutoff.attempt_id, exact_cutoff.version, State::Handed, None).unwrap());

        assert!(store.claim_orchestrator_message_at_for_test(
            2,
            created_at + crate::domain_store::ORCHESTRATOR_MESSAGE_TTL_MS + 1,
        ).unwrap().is_none(), "a message older than 24 hours is not claimed");
        assert_eq!(store.message_outbox_state_for_test(2).unwrap(), "discarded");
        assert!(!store.delivery_evidence().unwrap().iter().any(|item| item.message_id == 2),
            "discarding an unclaimed message does not invent a delivery attempt");
    }

    fn answer_problem(domain: &DomainRuntime, run_id: &str, option: &str) {
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "device:phone".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("answer-{option}").into(),
            expected_revision: state.revision,
            command: Command::AnswerQuestion {
                attachments: vec![], interaction_id: format!("{PROBLEM_PREFIX}{run_id}").into(), option_id: option.into(),
                inputs: Default::default(),
            },
        }, None, || Ok(())).unwrap();
    }

    /// The native stream announced tool intent, then ended without a result.
    const CREATED_THEN_EXIT: &str = "fixture:tool-then-exit";
    /// Claude died before it answered: the batch changed nothing.
    const DIED_BEFORE_ANSWERING: &str = "fixture:exit-before-init";

    #[test]
    fn an_unconfirmed_batch_is_a_problem_card_and_its_answer_is_the_decision() {
        for (option, replays) in [("retry", true), ("continue", false)] {
            let f = fixture();
            let domain = &f.executor.domain;
            ask_orchestrator(&f, 1);
            let pending = unhandled(domain)[0].0;
            let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
            let epoch = domain.state().unwrap().data_epoch.clone();
            let run = Run { id: "unconfirmed-decision".into(), message_ids: vec![claim.message.id],
                attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true };
            {
                let mut store = domain.store.lock().unwrap();
                store.bind_batch_effects(&run.id, &epoch).unwrap();
                store.begin_tool_operation(&run.id, "call", "interrupt", &serde_json::json!({"session_id":"a"})).unwrap();
            }
            let mut runtime = load(&f.executor.home).unwrap();
            runtime.active_run = Some(run.clone());
            save(&f.executor.home, &runtime).unwrap();
            f.executor.recover().unwrap();
            let id = format!("{PROBLEM_PREFIX}{}", run.id);
            let problem = domain.state().unwrap().interactions[&id.clone().into()].clone();
            assert_eq!(problem.card.kind, CardKind::Problem);
            assert_eq!(problem.author, Author::System);
            let InteractionPayload::Question { options, .. } = &problem.payload else { panic!("a Problem is asked like a question") };
            assert_eq!(options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(), ["continue", "retry"]);
            assert_eq!(problem.card.recommended.as_deref(), Some("continue"));
            assert_eq!(BatchProblem::from_body(&problem.body).unwrap().stop, BatchStop::Interrupted);
            assert_eq!(domain.store.lock().unwrap().tool_operations(&run.id).unwrap().len(), 1);
            answer_problem(domain, &run.id, option);
            f.executor.apply_problem_answers().unwrap();
            let runtime = load(&f.executor.home).unwrap();
            assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{option}: {runtime:?}");
            assert_eq!(!outbox(domain).is_empty(), replays, "{option}: only retry offers the batch again");
            assert_eq!(runtime.pending_resolutions.len(), usize::from(!replays), "{option}: the others are resolved without replay");
        }
    }

    #[tokio::test]
    async fn a_run_that_changed_nothing_runs_the_batch_again_with_its_prompt_and_asks_nobody() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)),
            "a run that changed nothing is not a decision for the user");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some(), "{runtime:?}");
        assert_eq!(runtime.failure, Some(Failure::Launch));
        assert!(runtime.last_error.is_some(), "{runtime:?}");
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "retryable"));

        f.clear_marker();
        again(&f);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Where do I put change 1?") && !prompt.contains(DIED_BEFORE_ANSWERING), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none(), "{runtime:?}");
        assert!(unhandled(domain).is_empty());
    }

    #[tokio::test]
    async fn claude_failing_to_start_three_times_in_a_row_tells_the_user_once_and_a_good_run_takes_it_back() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let stuck = || domain.state().unwrap().interactions.values()
            .filter(|i| i.id.0.starts_with(STUCK_PROBLEM_PREFIX) && !i.withdrawn).map(|i| i.id.clone()).collect::<Vec<_>>();
        for failure in 1..=4 {
            f.tick().await;
            assert_eq!(stuck().len(), usize::from(failure >= STUCK_AFTER), "after {failure} failed starts");
            again(&f);
        }
        let problem = stuck().remove(0);
        let state = domain.state().unwrap();
        let card = &state.interactions[&problem];
        assert_eq!(card.card.kind, CardKind::Problem);
        assert_eq!(card.card.recommended.as_deref(), Some(STUCK_RETRY));
        assert!(StuckProblem::from_body(&card.body).is_some_and(|body| body.failures == STUCK_AFTER), "{}", card.body);

        f.clear_marker();
        f.tick().await;
        assert!(stuck().is_empty(), "the orchestrator works again: nothing is left for the user");
        assert!(load(&f.executor.home).unwrap().stuck_problem.is_none());
    }

    #[tokio::test]
    async fn trying_again_now_from_the_card_starts_the_orchestrator_without_waiting_for_its_backoff() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        for _ in 0..STUCK_AFTER {
            again(&f);
            f.tick().await;
        }
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.retry_at.is_some(), "{runtime:?}");
        let problem = runtime.stuck_problem.clone().expect("the user was told");
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "u".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "retry-now".into(),
            expected_revision: state.revision,
            command: Command::AnswerQuestion { attachments: vec![], interaction_id: InteractionId(problem), option_id: STUCK_RETRY.into(), inputs: Default::default() },
        }, None, || Ok(())).unwrap();
        f.executor.apply_problem_answers().unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.retry_at.is_none() && runtime.stuck_problem.is_none(), "{runtime:?}");
    }

    fn plan_sync(f: &Fixture) {
        let orchestrator = Actor::Orchestrator { generation: 0 };
        f.run(orchestrator.clone(), "title-sync", Command::TitleRequest { request_id: "sync".into(), title: "Sync notes".into() });
        for (task, after) in [("model", vec![]), ("mac", vec!["model"]), ("ios", vec!["model"])] {
            f.run(orchestrator.clone(), &format!("create-{task}"), Command::CreateTask { plan_items: None, continues: None,
                task_id: task.into(), request_id: "sync".into(), title: format!("Sync {task}"), agent_id: None,
                review_prs: false, dependencies: after.into_iter().map(TaskId::from).collect(), stack: None,
                launch: Some(LaunchPlan::new("node", "/tmp")),
            });
        }
        f.run(orchestrator, "priority-ios", Command::SetPriority { task_id: "ios".into(), priority: TaskPriority::High });
    }

    fn announced(f: &Fixture) -> Vec<Vec<String>> {
        use alondra_core::request_outcome::TasksCard;
        let state = f.domain().state().unwrap();
        f.cards(alondra_core::request_outcome::TASKS_PREFIX).iter()
            .map(|id| TasksCard::from_body(&state.interactions[&InteractionId::from(id.as_str())].body).unwrap())
            .map(|card| card.tasks.into_iter().map(|task| task.id).collect())
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_tasks_a_turn_made_reach_the_user_once_when_it_ends() {
        let f = fixture();
        f.run(Actor::User { user_id: "u".into() }, "sync", Command::CreateRequest {
            request_id: "sync".into(), body: "Sync my notes between iPhone and Mac".into(), auto_merge: false, maintenance: false, message: false,
        });
        f.hold_before_claude();
        let turn = async {
            f.held().await;
            plan_sync(&f);
            assert!(announced(&f).is_empty(), "nothing is told while the turn runs");
            f.release();
        };
        let ((), ()) = tokio::join!(f.tick(), turn);
        assert_eq!(announced(&f), [["model", "ios", "mac"]], "one card, in the order the tasks run");
        assert!(f.domain().state().unwrap().unannounced_tasks().is_empty());
        f.tell("later", "Anything else?");
        let ((), ()) = tokio::join!(f.tick(), async { f.held().await; f.release(); });
        assert_eq!(announced(&f).len(), 1, "the next turn tells nothing new");
    }

    /// A task waits for a cancelled one and the orchestrator keeps failing: every turn it ends, it
    /// is handed the open item again, and nobody else is asked.
    #[tokio::test]
    async fn a_failing_orchestrator_is_handed_what_it_still_has_to_re_plan_at_every_turn_it_ends() {
        let f = fixture();
        f.mark(DIED_BEFORE_ANSWERING);
        ask_orchestrator(&f, 1);
        let orchestrator = Actor::Orchestrator { generation: 0 };
        f.run(Actor::User { user_id: "u".into() }, "pair", Command::CreateRequest {
            request_id: "pair".into(), body: "Two tasks".into(), auto_merge: false, maintenance: false, message: false,
        });
        f.run(orchestrator.clone(), "pair-title", Command::TitleRequest { request_id: "pair".into(), title: "Pair".into() });
        for (id, after) in [("first", vec![]), ("second", vec!["first"])] {
            f.run(orchestrator.clone(), id, Command::CreateTask {
                task_id: id.into(), request_id: "pair".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
                dependencies: after.into_iter().map(|d: &str| TaskId::from(d)).collect(), stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), plan_items: None, continues: None,
            });
        }
        f.run(Actor::User { user_id: "u".into() }, "cancel-first", Command::CancelTask { task_id: "first".into(), reason: None });
        let notices = || f.cards(alondra_core::orchestrator_cards::DEPENDENCY_CANCELLED_NOTICE_PREFIX).len();
        for failure in 1..=3 {
            f.tick().await;
            assert!(load(&f.executor.home).unwrap().last_error.is_some() || load(&f.executor.home).unwrap().retry_at.is_some(), "the turn failed");
            assert_eq!(notices(), failure + 1, "handed at the start and again when the turn ended, {failure} turns in");
            again(&f);
        }
        let state = f.executor.domain.state().unwrap();
        assert!(state.open_start_problem(&"second".into()).is_none(), "the user is never asked");
    }

    #[tokio::test]
    async fn a_turn_that_fails_leaves_its_tasks_for_the_next_turn_to_tell() {
        let f = fixture();
        f.mark(CREATED_THEN_EXIT);
        f.run(Actor::User { user_id: "u".into() }, "sync", Command::CreateRequest {
            request_id: "sync".into(), body: "Sync my notes between iPhone and Mac".into(), auto_merge: false, maintenance: false, message: false,
        });
        plan_sync(&f);
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some());
        assert!(f.cards(PROBLEM_PREFIX).is_empty());
        assert!(announced(&f).is_empty(), "a failed turn tells nothing");
        f.clear_marker();
        again(&f);
        f.tell("later", "Anything else?");
        f.tick().await;
        assert_eq!(announced(&f), [["model", "ios", "mac"]]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_stopped_before_it_changed_anything_is_settled_without_a_card() {
        let f = fixture();
        f.hold_before_claude();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let canceller = async {
            f.held().await;
            let run = load(&f.executor.home).unwrap().active_run.unwrap().id;
            f.executor.cancel(&run).unwrap();
        };
        let ((), ()) = tokio::join!(f.tick(), canceller);
        assert!(f.prompts().is_empty(), "Claude never saw the batch");
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)));
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && !runtime.cancel_requested, "{runtime:?}");
        assert_eq!(runtime.pending_resolutions.len(), 1, "the orchestrator hears it was stopped");
        assert!(outbox(domain).is_empty(), "a stopped batch is not replayed");
    }

    #[tokio::test]
    async fn a_batch_settled_elsewhere_withdraws_its_problem() {
        let f = fixture();
        f.mark("fixture:denied");
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        f.executor.acknowledge(&run.id).unwrap();
        f.executor.apply_problem_answers().unwrap();
        assert!(domain.state().unwrap().interactions[&format!("{PROBLEM_PREFIX}{}", run.id).into()].withdrawn);
    }

    #[tokio::test]
    async fn a_failed_execution_is_ambiguous_until_acknowledged_and_resolved_at_restart() {
        let f = fixture();
        f.mark("fixture:denied");
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        let runtime = load(&f.executor.home).unwrap();
        let run = runtime.active_run.clone().expect("an ambiguous execution stays claimed");
        assert!(runtime.last_error.is_some());
        assert_eq!(runtime.failure, Some(Failure::Denied));
        assert!(attempts(domain).iter().all(|(_, state, reason)| state == "uncertain" && reason.as_deref() == Some("orchestrator_execution_ambiguous")));
        assert!(outbox(domain).is_empty(), "an ambiguous batch is not offered again");
        f.tick().await;
        assert_eq!(f.starts().len(), 1);
        assert!(f.executor.acknowledge("other-run").is_err());
        f.executor.acknowledge(&run.id).unwrap();
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none());
        assert_eq!(runtime.pending_resolutions, vec![Resolution { run_id: run.id.clone(), message_ids: run.message_ids.clone() }]);
        assert!(attempts(domain).iter().all(|(_, state, _)| state == "discarded"));

        f.clear_marker();
        ask_orchestrator(&f, 2);
        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Previous interrupted inbox batches have already been resolved")
            && !prompt.contains(&run.id) && !prompt.contains("inbox message IDs"), "{prompt}");
        assert!(prompt.contains("Where do I put change 2?") && !prompt.contains("Where do I put change 1?")
            && !prompt.contains("inbox message"), "{prompt}");
        assert!(load(&f.executor.home).unwrap().pending_resolutions.is_empty());

        // A daemon restart during an execution resolves it without the user.
        let mut runtime = load(&f.executor.home).unwrap();
        ask_orchestrator(&f, 3);
        let pending = unhandled(domain)[0].0;
        let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
        runtime.active_run = Some(Run { id: "interrupted".into(), message_ids: vec![claim.message.id],
            attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true });
        save(&f.executor.home, &runtime).unwrap();
        let recovered = f.executor.recover().unwrap();
        assert!(recovered.active_run.is_none());
        assert_eq!(recovered.pending_resolutions[0].run_id, "interrupted");
        assert!(unhandled(domain).is_empty(), "the interrupted batch is not replayed");
        assert!(attempts(domain).iter().any(|(id, state, _)| *id == claim.message.id && state == "discarded"));
    }

    #[test]
    fn a_native_billing_change_cannot_turn_a_selected_batch_into_automatic_paid_use() {
        let f=fixture();
        let domain=&f.executor.domain;
        let selected=crate::orchestrator_provider::selection(domain,&f.executor.home,now_millis()).unwrap();
        let before=domain.state().unwrap();
        let mut accounts=before.accounts.clone();
        accounts[0].billing=alondra_core::accounts::AccountBilling::PayPerUse;
        f.run(Actor::System,"native-billing-changed",Command::RecordAccounts {accounts,reserve:before.account_reserve});
        let mut store=domain.store.lock().unwrap();
        assert!(store.claim_selected_orchestrator_batch(&[],"paid-race","native-session",before.data_epoch.clone(),Some(&selected)).is_err());
        assert!(store.batch_selection("paid-race").unwrap().is_none(),"No run or provider offer was created");
    }

    #[test]
    fn a_restart_keeps_an_unconfirmed_physical_operation_ambiguous() {
        let f = fixture();
        ask_orchestrator(&f,1);
        let domain = &f.executor.domain;
        let pending = unhandled(domain)[0].0;
        let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
        let epoch = domain.state().unwrap().data_epoch.clone();
        {
            let mut store = domain.store.lock().unwrap();
            store.bind_batch_effects("uncertain-restart",&epoch).unwrap();
            store.begin_tool_operation("uncertain-restart","call","interrupt",&serde_json::json!({"session_id":"a"})).unwrap();
        }
        let mut runtime = load(&f.executor.home).unwrap();
        runtime.active_run = Some(Run { id:"uncertain-restart".into(),message_ids:vec![claim.message.id],attempts:vec![(claim.attempt_id,claim.version)],started_at:1,spawned:true });
        save(&f.executor.home,&runtime).unwrap();
        for _ in 0..2 {
            let recovered = f.executor.recover().unwrap();
            assert_eq!(recovered.active_run.as_ref().map(|run| run.id.as_str()),Some("uncertain-restart"));
            assert_eq!(recovered.failure,Some(Failure::Interrupted));
            assert!(unhandled(domain).is_empty(),"An unknown physical result is never reoffered");
        }
        assert!(attempts(domain).iter().any(|(id,state,_)| *id==claim.message.id && state=="uncertain"));
    }

    #[tokio::test]
    async fn an_interrupted_batch_is_not_replayed_but_one_fresh_run_sees_its_open_request() {
        let f = fixture();
        let domain = &f.executor.domain;
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
            request_id: "bc2466f2".into(), body: "Make the editor readable".into(), auto_merge: false, maintenance: false, message: false,
        });
        let pending = outbox(domain)[0].0;
        let claim = domain.store.lock().unwrap().claim_message(pending).unwrap().unwrap();
        save(&f.executor.home, &Runtime {
            session_id: Some("session".into()),
            active_run: Some(Run { id: "interrupted".into(), message_ids: vec![claim.message.id],
                attempts: vec![(claim.attempt_id, claim.version)], started_at: 1, spawned: true }),
            ..Runtime::default()
        }).unwrap();
        f.executor.recover().unwrap();
        assert!(outbox(domain).is_empty(), "the interrupted batch is not replayed");

        f.tick().await;
        assert_eq!(f.starts().len(), 1);
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Previous interrupted inbox batches have already been resolved")
            && prompt.contains("An open Request below with no Task of its own was not turned into work")
            && prompt.contains("request bc2466f2 (") && prompt.contains("(no task of its own): Make the editor readable\n")
            && !prompt.contains("The user created Request"), "{prompt}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.pending_resolutions.is_empty() && runtime.active_run.is_none() && runtime.last_error.is_none());

        assert_eq!(reminders(domain), ["request-unsplit-bc2466f2-1"], "the request it left without a task goes back once, in the same step");
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "the reminder is a batch of its own");
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "then it is the user's question: one fresh run, not a loop");
    }

    /// 21 October: the user accepted a design whose plan has six phases and asked for them before the
    /// orchestrator's batch, already creating them, had finished.
    #[tokio::test]
    async fn an_accepted_plan_in_phases_reaches_the_orchestrator_at_once_and_one_left_without_tasks_is_flagged() {
        use alondra_core::orchestrator_cards::{UNSPLIT_NOTICE_PREFIX, UNSPLIT_PROBLEM_PREFIX};
        let f = fixture();
        let domain = &f.executor.domain;
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into(), at: 0 });
        command(domain, Actor::Orchestrator { generation: 0 }, "request", Command::CreateRequest {
            request_id: "r".into(), body: "Design several Claude accounts".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(domain, Actor::Orchestrator { generation: 0 }, "task", Command::CreateTask { plan_items: None, continues: None,
            task_id: "design".into(), request_id: "r".into(), title: "Design several Claude accounts".into(), agent_id: Some("a".into()),
            review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        let plan = "# Several accounts\n\n## Phase 1\n\n- [ ] Accounts in options\n\n## Phase 2\n\n- [ ] Usage per account\n";
        let targets = vec![DeliveryTarget::Text { text: "Design".into() }];
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            checklist: vec![], attachments: vec![], task_id: "design".into(), interaction_id: "design-result".into(),
            body: "Read the design".into(), card: CardSpec::delivery("Read the design", &targets, true).plan(plan),
            targets: targets.clone(), is_final: true, nothing_to_review: false,
        });
        command(domain, Actor::User { user_id: "u".into() }, "implement", Command::DecideProposedPlan {
            presentation_id: "design-result".into(), targets, choice: PlanChoice::Implement,
        });
        assert_eq!(outbox(domain).len(), 1, "the acceptance alone puts the plan in the orchestrator's inbox");

        f.tick().await;
        let prompt = last_prompt(&f);
        assert!(prompt.contains("Request implement-design-result holds that plan") && prompt.contains("one per phase"), "{prompt}");

        let reminder = format!("{UNSPLIT_NOTICE_PREFIX}implement-design-result-1");
        assert_eq!(reminders(domain), [reminder.clone()], "a batch that left the plan without tasks is told so");
        let text = domain.state().unwrap().interactions[&InteractionId::from(reminder.as_str())].body.clone();
        assert!(text.contains("holds a plan the user approved and still has no implementation Task")
            && text.contains("one per phase") && !text.contains("answer it and close it"), "{text}");

        f.tick().await;
        assert_eq!(f.cards(UNSPLIT_PROBLEM_PREFIX), [format!("{UNSPLIT_PROBLEM_PREFIX}implement-design-result-1")],
            "left without tasks again, it is the user's problem");
    }

    #[test]
    fn each_open_request_carries_the_choices_it_was_asked_with() {
        let f = fixture();
        ask_orchestrator(&f, 1);
        let state = f.executor.domain.state().unwrap();
        let text = summary(&state, &no_ages(), |request| (request.0 == "r").then(|| "  options: project App · Effort: hard".to_owned()));
        assert!(text.contains("request r (tasks: t not_started): Work\n  options: project App · Effort: hard\ntask t"), "{text}");
    }

    fn no_ages() -> Ages {
        Ages { now: 0, opened: Default::default() }
    }

    fn ages(now: i64, opened: &[(&str, i64)]) -> Ages {
        Ages { now, opened: opened.iter().map(|(id, at)| ((*id).into(), *at)).collect() }
    }

    #[test]
    fn each_open_request_says_how_long_ago_it_was_made() {
        let hour = 60 * 60 * 1_000;
        let f = fixture();
        ask_orchestrator(&f, 1);
        command(&f.executor.domain, Actor::User { user_id: "u".into() }, "old", Command::CreateRequest {
            request_id: "old".into(), body: "Split the public repo".into(), auto_merge: false, maintenance: false, message: false,
        });
        command(&f.executor.domain, Actor::User { user_id: "u".into() }, "recent", Command::CreateRequest {
            request_id: "recent".into(), body: "Fix the badge".into(), auto_merge: false, maintenance: false, message: false,
        });
        let now = 100 * 24 * hour;
        let text = summary(&f.executor.domain.state().unwrap(), &ages(now, &[("old", now - 73 * hour), ("recent", now - 5 * hour), ("r", now - 10)]), |_| None);
        assert!(text.contains("request old (opened 3 days ago) (no task of its own): Split the public repo"), "{text}");
        assert!(text.contains("request recent (opened 5 hours ago) (no task of its own): Fix the badge"), "{text}");
        assert!(text.contains("request r (opened less than an hour ago) (tasks: t not_started): Work"), "{text}");
    }

    #[test]
    fn a_request_whose_tasks_all_ended_is_told_to_be_closed() {
        let f = fixture();
        ask_orchestrator(&f, 1);
        let before = summary(&f.executor.domain.state().unwrap(), &no_ages(), |_| None);
        assert!(!before.contains("every task of it has ended"), "{before}");
        command(&f.executor.domain, Actor::Orchestrator { generation: 0 }, "cancel", Command::CancelTask { task_id: "t".into(), reason: None });
        let after = summary(&f.executor.domain.state().unwrap(), &no_ages(), |_| None);
        assert!(after.contains("request r (tasks: t closing; every task of it has ended: close it with close_request, or create its next tasks): Work"), "{after}");
    }

    #[test]
    fn every_open_request_says_which_tasks_it_has_however_many_requests_are_open() {
        let f = fixture();
        ask_orchestrator(&f, 1);
        let body = "A request that already has its tasks and waits for them to end. ".repeat(20);
        for n in 0..60 {
            let request = format!("busy-{n}");
            command(&f.executor.domain, Actor::User { user_id: "u".into() }, &request, Command::CreateRequest {
                request_id: request.as_str().into(), body: body.clone(), auto_merge: false, maintenance: false, message: false,
            });
            command(&f.executor.domain, Actor::Orchestrator { generation: 0 }, &format!("title-{n}"), Command::TitleRequest {
                request_id: request.as_str().into(), title: format!("Busy {n}"),
            });
            command(&f.executor.domain, Actor::Orchestrator { generation: 0 }, &format!("task-{n}"), Command::CreateTask {
                task_id: format!("t-busy-{n}").into(), request_id: request.as_str().into(), title: format!("Busy {n}"), agent_id: None,
                review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp/project")),
                plan_items: None, continues: None,
            });
        }
        command(&f.executor.domain, Actor::User { user_id: "u".into() }, "fresh", Command::CreateRequest {
            request_id: "fresh".into(), body: "A node that waits holds no memory".into(), auto_merge: false, maintenance: false, message: false,
        });
        let text = summary(&f.executor.domain.state().unwrap(), &no_ages(), |_| None);
        assert!(body.len() * 60 > MAX_SUMMARY_BYTES, "the open requests alone outgrow the bound");
        for n in 0..60 {
            assert!(text.contains(&format!("request busy-{n} (tasks: t-busy-{n} not_started): ")), "busy-{n} lost its tasks:\n{text}");
        }
        assert_eq!(text.matches("(no task of its own)").count(), 1, "{text}");
        assert!(text.contains("request fresh (no task of its own): A node that waits holds no memory"), "{text}");
    }

    #[test]
    fn a_request_not_split_yet_reaches_the_orchestrator_whole() {
        let f = fixture();
        let long = format!("{} end of the message", "word ".repeat(80));
        command(&f.executor.domain, Actor::User { user_id: "u".into() }, "long", Command::CreateRequest {
            request_id: "long".into(), body: long.clone(), auto_merge: false, maintenance: false, message: false,
        });
        let text = summary(&f.executor.domain.state().unwrap(), &no_ages(), |_| None);
        assert!(text.contains("end of the message"), "{text}");
    }

    #[tokio::test]
    async fn the_user_decides_on_an_ambiguous_execution_through_its_status() {
        let f = fixture();
        f.mark("fixture:denied");
        let domain = &f.executor.domain;
        assert_eq!(status(&f.executor).unwrap()["phase"], "idle");
        ask_orchestrator(&f, 1);
        assert_eq!(status(&f.executor).unwrap()["queue_depth"], 1);
        f.tick().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        let shown = status(&f.executor).unwrap();
        assert_eq!((shown["phase"].as_str(), shown["can_retry"].as_bool(), shown["can_cancel"].as_bool()), (Some("failed"), Some(true), Some(false)));
        assert_eq!(shown["active_run_id"], serde_json::json!(run.id));
        assert!(shown["last_error"].is_string());
        let missing = decide(&f.executor, Decision::Acknowledge, Some(&serde_json::json!({})));
        assert_eq!((missing["status"].as_u64(), missing["body"]["code"].as_str()), (Some(422), Some("invalid_input")));
        let other = decide(&f.executor, Decision::Acknowledge, Some(&serde_json::json!({"run_id": "other"})));
        assert_eq!((other["status"].as_u64(), other["body"]["code"].as_str()), (Some(409), Some("conflict")));
        let cancel = decide(&f.executor, Decision::Cancel, Some(&serde_json::json!({"run_id": "other"})));
        assert_eq!(cancel["status"], 409);
        let acknowledged = decide(&f.executor, Decision::Acknowledge, Some(&serde_json::json!({"run_id": run.id})));
        assert_eq!(acknowledged, serde_json::json!({"status": 200, "body": {}}));
        assert_eq!(status(&f.executor).unwrap()["phase"], "idle");
        assert_eq!(decision_route("/orchestrator/retry"), Some(Decision::Retry));
        assert_eq!(decision_route("/orchestrator/messages"), None);
        assert!(domain.state().unwrap().interactions.keys().any(|id| id.0 == format!("{PROBLEM_PREFIX}{}", run.id)));
    }

    #[tokio::test]
    async fn a_retry_offers_the_batch_again_and_a_cancel_is_ambiguous() {
        let f = fixture();
        f.mark("fixture:denied");
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        f.tick().await;
        let run = load(&f.executor.home).unwrap().active_run.unwrap();
        f.executor.retry(&run.id).unwrap();
        assert_eq!(outbox(domain).len(), 1);
        assert_eq!(outbox(domain)[0].1, "retryable");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());
        assert!(f.executor.cancel("nothing").is_err(), "cancel needs a running execution");
    }

    fn accounts_seen(f: &Fixture, id: &str, state: alondra_core::accounts::AccountState) {
        use alondra_core::accounts::{AccountBilling, AccountId, AccountStatus, Provider};
        let accounts = vec![AccountStatus {
            provider: Provider::Claude, id: AccountId::default_account(), name: "default".into(), state,
            native_identity: None, email: None, plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None,
        }];
        command(&f.executor.domain, Actor::System, id, Command::RecordAccounts { accounts, reserve: alondra_core::usage::default_reserve() });
        if state == alondra_core::accounts::AccountState::SignedIn {
            let catalog=alondra_core::model_selection::AccountModelCatalog { account:alondra_core::accounts::AccountKey::native(Provider::Claude),revision:"fixture-models".into(),native_default:None,measured_at:now_millis(),models:vec![alondra_core::model_selection::CatalogModel { model:"opus".into(),display_name:"Native fixture model".into(),reasoning_efforts:vec![],default_reasoning_effort:None,context_window:Some(200000),input_modalities:vec!["text".into()],is_default:false,hidden:false }] };
            command(&f.executor.domain,Actor::System,&format!("{id}-models"),Command::RecordModelCatalog {catalog});
        }
    }

    #[tokio::test]
    async fn without_a_signed_in_account_work_waits_for_a_session_and_resumes_the_moment_one_has_it() {
        use alondra_core::accounts::AccountState;
        let f = fixture();
        let domain = &f.executor.domain;
        accounts_seen(&f, "accounts-out", AccountState::SignedOut);
        ask_orchestrator(&f, 1);
        f.tick().await;
        assert_eq!(f.starts().len(), 0, "no account can run the batch");
        assert_eq!(status(&f.executor).unwrap()["waits_for_sign_in"], true, "the screens hear why the request sits");
        assert!(domain.live.usage_pause.waits_for_session(), "launches wait for a session, not for usage: {:?}", domain.live.usage_pause.current());

        f.tick().await;
        assert_eq!(f.starts().len(), 0);
        assert!(domain.live.usage_pause.waits_for_session(), "nothing but a session ends the wait");
        assert_eq!(f.executor.account_wait_change(), None, "the log says it once, not on every tick");

        domain.live.account_wait.end();
        let line = f.executor.account_wait_change().expect("a new wait is said");
        assert!(line.contains("no Claude account has a session (default (signed out))") && line.contains("until one is signed in"), "{line}");
        assert!(!line.contains("next try"), "no time in the wait: {line}");

        accounts_seen(&f, "accounts-in", AccountState::SignedIn);
        f.tick().await;
        assert_eq!(f.starts().len(), 1, "the account check seeing a session runs the batch at once");
        assert!(!domain.live.usage_pause.is_active(), "and nothing holds new work any more");
        assert_eq!(status(&f.executor).unwrap()["waits_for_sign_in"], false, "and the wait is gone from the screens");
    }

    #[tokio::test]
    async fn a_request_on_a_fresh_install_says_each_reason_it_waits_and_runs_once_claude_is_connected() {
        use alondra_core::accounts::{AccountKey, AccountsConfig, Provider};
        let f = fixture();
        let domain = &f.executor.domain;
        let options = alondra_core::options::path_in(&f.executor.home);
        let connected = std::fs::read_to_string(&options).unwrap();
        std::fs::write(&options, AccountsConfig::fresh().to_toml()).unwrap();
        crate::accounts::begin_initial_check(domain);
        ask_orchestrator(&f, 1);

        f.tick().await;
        assert_eq!(f.starts().len(), 0);
        let checking = domain.live.account_wait.reason().expect("the request waits");
        assert!(checking.contains("Accounts are being checked"), "{checking}");

        crate::accounts::check(domain, &f.executor.home);
        f.tick().await;
        assert_eq!(f.starts().len(), 0, "no provider is connected");
        let unconnected = domain.live.account_wait.reason().expect("the request still waits");
        assert_ne!(unconnected, checking, "the wait says why it waits now, not why it waited at startup");
        assert!(unconnected.contains("open Accounts to connect"), "{unconnected}");

        std::fs::write(&options, connected).unwrap();
        let profiles = alondra_core::options::OptionsFile::parse(&std::fs::read_to_string(&options).unwrap()).unwrap().model_profiles;
        let version = alondra_core::model_selection::profiles_version(&profiles);
        command(domain, Actor::System, "connected-profiles", Command::RecordModelProfiles { profiles, version });
        accounts_seen(&f, "connected", alondra_core::accounts::AccountState::SignedIn);
        command(domain, Actor::System, "connected-order", Command::RecordAccountConfiguration { order: vec![AccountKey::native(Provider::Claude)], version: "connected".into() });
        f.tick().await;
        assert_eq!(f.starts().len(), 1, "connecting Claude runs the request that waited");
        assert_eq!(domain.live.account_wait.reason(), None);
    }

    #[tokio::test]
    async fn claude_answering_while_no_account_has_a_session_checks_the_accounts_at_once() {
        use alondra_core::accounts::AccountState;
        let f = fixture();
        let domain = &f.executor.domain;
        let checked = || async { tokio::time::timeout(Duration::ZERO, domain.live.account_check.notified()).await.is_ok() };
        crate::accounts::answered(domain);
        assert!(!checked().await, "with sessions to spare an answer asks for nothing");

        accounts_seen(&f, "accounts-out", AccountState::SignedOut);
        assert!(crate::accounts::choose_for_launch(domain, &f.executor.home, now_millis()).is_none());
        crate::accounts::answered(domain);
        assert!(checked().await, "an answer while waiting for a session checks the accounts now");
    }

    #[tokio::test]
    async fn no_batch_starts_while_usage_is_exhausted_and_it_runs_once_when_usage_is_back() {
        let f = fixture();
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        domain.live.usage_pause.exhaust(Some(now_millis() + 3_600_000), now_millis());
        f.tick().await;
        f.tick().await;
        assert_eq!(f.starts().len(), 0, "paused: Claude is not started");
        assert_eq!(outbox(domain).len(), 1, "and the batch waits, untouched");
        assert!(load(&f.executor.home).unwrap().active_run.is_none());

        domain.live.usage_pause.resume(now_millis());
        f.tick().await;
        assert_eq!(f.starts().len(), 1, "resumed: the pending batch runs");
        assert!(unhandled(domain).is_empty());
        f.tick().await;
        assert_eq!(f.starts().len(), 1, "and only once");
    }

    #[tokio::test]
    async fn a_run_that_hits_the_limit_before_working_pauses_everything_and_is_retried_after_the_reset() {
        let f = fixture();
        f.mark_once("fixture:limited");
        let domain = &f.executor.domain;
        ask_orchestrator(&f, 1);
        let before = now_millis() / 1000;
        f.tick().await;
        let after = now_millis() / 1000;
        assert_eq!(f.starts().len(), 1);
        let until = domain.live.usage_pause.current().map(|e| e.until).expect("usage pauses everything");
        assert!(until % 1000 == 0 && ((before + 3_600) * 1000..=(after + 3_600) * 1000).contains(&until),
            "until is the reset Claude reported, an hour after the run: {until}");
        let runtime = load(&f.executor.home).unwrap();
        assert!(runtime.active_run.is_none() && runtime.last_error.is_none() && runtime.failure.is_none() && runtime.consecutive_failures == 0 && runtime.retry_at.is_none(), "{runtime:?}");
        assert_eq!(outbox(domain).iter().map(|(_, state)| state.as_str()).collect::<Vec<_>>(), ["retryable"]);
        assert!(!domain.state().unwrap().interactions.keys().any(|id| id.0.starts_with(PROBLEM_PREFIX)), "a limit is not a Problem for the user to decide");

        f.tick().await;
        assert_eq!(f.starts().len(), 1, "nothing runs until the reset");
        let usage = domain.read().state().account_usage.clone();
        assert!(usage.iter().any(|usage| usage.account.is_default() && usage.exhausted_until(now_millis()) == Some(until)),
            "the orchestrator's account ran out until the reset: {usage:?}");

        crate::usage_probe::record(domain, &alondra_core::accounts::AccountId::default_account(), alondra_core::usage::UsageReading {
            windows: vec![alondra_core::usage::WindowReading { kind: alondra_core::usage::WindowKind::Session, used: alondra_core::usage::Share::of(0.1), resets_at: None, exhausted: false }],
            plan: None, complete: true,
        }, now_millis());
        domain.live.usage_pause.resume(now_millis());
        f.tick().await;
        assert_eq!(f.starts().len(), 2, "after the reset the probe finds room: the same batch is retried");
        assert!(unhandled(domain).is_empty());
        assert!(attempts(domain).iter().any(|(_, state, _)| state == "handed"));
    }

    #[tokio::test]
    async fn without_an_sdk_backend_a_batch_does_not_start_and_is_offered_again() {
        let root = crate::test_support::Directory::new();
        let home = root.join("home");
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let claude_home = super::testing::registered_plugins(&root);
        let _ownership = crate::orchestrator_process::Ownership::acquire(&home.join("runtime/orchestrator")).unwrap().unwrap();
        let executor = Executor { domain: domain.clone(), home: home.clone(), claude_home };
        super::testing::seed_native_metadata(&domain,&home);
        command(&domain, Actor::User { user_id: "u".into() }, "m-1", Command::CreateRequest {
            request_id: "m-1".into(), body: "Where do I put change 1?".into(), auto_merge: false, maintenance: false, message: true,
        });
        executor.tick().await.unwrap();
        let runtime = load(&home).unwrap();
        assert_eq!(runtime.failure, Some(Failure::Launch), "{runtime:?}");
        assert_eq!(runtime.last_error.as_deref(), Some(crate::sdk_owner::NO_HOST));
        assert!(runtime.active_run.is_none() && runtime.retry_at.is_some(), "{runtime:?}");
        assert!(attempts(&domain).iter().all(|(_, state, _)| state == "retryable"));
    }
}
