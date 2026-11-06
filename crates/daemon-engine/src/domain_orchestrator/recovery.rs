use super::*;

impl Executor {
    pub(super) async fn startup_recovery(&self) -> Result<Runtime> {
        self.repair_incomplete().await?;
        let runtime = load(&self.home)?;
        let Some(run) = runtime.active_run.as_ref() else {
            return Ok(runtime);
        };
        let epoch = self.domain.read().state().data_epoch.clone();
        let current = self
            .domain
            .store
            .lock_or_exit("the domain store")
            .unfinished_orchestrator_batches(&epoch, Some(&run.id), None)?
            .iter()
            .any(|batch| batch.run == run.id);
        if current {
            return Ok(runtime);
        }
        self.recover()
    }

    /// Only called between invocations, after their provider worker has stopped. The database
    /// also finds claims whose runtime file could not be written after the claim committed.
    pub(crate) async fn repair_incomplete(&self) -> Result<()> {
        self.resume_stopped()?;
        'discover: loop {
            let mut runtime = load(&self.home)?;
            let epoch = self.domain.read().state().data_epoch.clone();
            let directory = self.home.join("runtime/orchestrator");
            let retained = process::recorded_run(&directory)?;
            let batches = self
                .domain
                .store
                .lock_or_exit("the domain store")
                .unfinished_orchestrator_batches(
                    &epoch,
                    runtime.active_run.as_ref().map(|run| run.id.as_str()),
                    retained.as_deref(),
                )?;
            if let Some(retained) = retained {
                anyhow::ensure!(
                    batches.iter().any(|batch| batch.run == retained)
                        || runtime
                            .active_run
                            .as_ref()
                            .is_some_and(|active| active.id == retained),
                    "the retained orchestrator host has no batch in the current epoch"
                );
                process::fence_recorded(&directory, &retained)?;
            }
            for batch in batches {
                let active = runtime
                    .active_run
                    .as_ref()
                    .is_some_and(|run| run.id == batch.run);
                if batch.ended && !active {
                    continue;
                }
                let decision = batch.decision.as_deref().or_else(|| {
                    batch
                        .attempts
                        .iter()
                        .find(|attempt| {
                            attempt.reason.as_deref() == Some("explicit_user_reconciliation")
                        })
                        .map(|attempt| attempt.state.as_str())
                });
                if let Some(decision) = decision {
                    if active {
                        match decision {
                            "discarded" => self.acknowledge(&batch.run)?,
                            "retryable" => self.retry(&batch.run)?,
                            _ => anyhow::bail!("the batch has an invalid user reconciliation"),
                        }
                        self.domain
                            .store
                            .lock_or_exit("the domain store")
                            .finish_batch(&batch.run)?;
                        continue 'discover;
                    }
                    self.domain
                        .store
                        .lock_or_exit("the domain store")
                        .finish_batch(&batch.run)?;
                    continue;
                }
                let unresolved = batch
                    .attempts
                    .iter()
                    .any(|attempt| matches!(attempt.state.as_str(), "dispatching" | "uncertain"));
                if !unresolved
                    && runtime
                        .batch_handoffs
                        .iter()
                        .any(|handoff| handoff.run_id == batch.run)
                {
                    if active {
                        runtime.active_run = None;
                        runtime.last_error = None;
                        runtime.failure = None;
                        runtime.cancel_requested = false;
                        save(&self.home, &runtime)?;
                    }
                    continue;
                }
                if !unresolved && !active {
                    self.domain
                        .store
                        .lock_or_exit("the domain store")
                        .finish_batch(&batch.run)?;
                    self.domain
                        .live
                        .orchestrator_runs
                        .lock_or_recover("the orchestrator runs")
                        .remove(&batch.run);
                    continue;
                }
                let run = Run {
                    id: batch.run.clone(),
                    message_ids: batch
                        .attempts
                        .iter()
                        .map(|attempt| attempt.message_id)
                        .collect(),
                    attempts: batch
                        .attempts
                        .iter()
                        .map(|attempt| {
                            (
                                attempt.attempt_id,
                                if attempt.state == "uncertain" {
                                    attempt.version.saturating_sub(1)
                                } else {
                                    attempt.version
                                },
                            )
                        })
                        .collect(),
                    started_at: 0,
                    spawned: true,
                };
                let handed = !batch.attempts.is_empty()
                    && batch
                        .attempts
                        .iter()
                        .all(|attempt| attempt.state == "handed");
                if (handed || batch.ended) && active {
                    let asked = crate::orchestrator_inbox::asked(self.domain.read().state());
                    self.handled(runtime, &run, &asked)?;
                    continue 'discover;
                }
                if active
                    && runtime.last_error.is_some()
                    && !matches!(
                        runtime.failure,
                        Some(Failure::Launch | Failure::SessionMissing)
                    )
                {
                    self.finish(
                        &run,
                        State::Uncertain,
                        Some("orchestrator_execution_ambiguous"),
                    )?;
                    let stop = match runtime.failure {
                        Some(Failure::Denied) => BatchStop::Denied,
                        Some(Failure::Cancelled) => BatchStop::Cancelled,
                        _ => BatchStop::Interrupted,
                    };
                    let error = runtime.last_error.clone().unwrap_or_default();
                    if !self
                        .domain
                        .read()
                        .state()
                        .interactions
                        .contains_key(&InteractionId(format!("{PROBLEM_PREFIX}{}", run.id)))
                    {
                        self.raise_batch_problem(
                            &run.id,
                            BatchProblem {
                                stop,
                                calls: vec![],
                                subjects: vec![],
                                error: error.clone(),
                            },
                            &error,
                        );
                    }
                    continue;
                }
                let admitted = self.domain.end_execution(&run.id);
                self.domain.drain_batch_operations(&run.id).await?;
                if !self.domain.confirmed_batch_effects(&run.id, &admitted)? {
                    runtime.active_run = Some(run.clone());
                    let why = "An interrupted batch has an operation without a confirmed durable result; reconcile its effects before continuing".to_owned();
                    let problem = BatchProblem {
                        stop: BatchStop::Interrupted,
                        calls: admitted,
                        subjects: vec![],
                        error: why.clone(),
                    };
                    self.failed(runtime, &run, why, Some(problem))?;
                    return Ok(());
                }
                let (effects, operations, selection) = {
                    let store = self.domain.store.lock_or_exit("the domain store");
                    (
                        store.batch_effects(&run.id)?,
                        store.tool_operations(&run.id)?,
                        store.batch_selection(&run.id)?,
                    )
                };
                let recorded_stop = self
                    .domain
                    .store
                    .lock_or_exit("the domain store")
                    .batch_stop(&run.id)?;
                let automatic = batch.attempts.iter().any(|attempt| {
                    attempt.state == "retryable"
                        && matches!(
                            attempt.reason.as_deref(),
                            Some("orchestrator_launch_failed" | "orchestrator_usage_exhausted")
                        )
                });
                let cold_batch = !automatic
                    && selection.is_some()
                    && !batch.ended
                    && !runtime
                        .batch_handoffs
                        .iter()
                        .any(|handoff| handoff.run_id == run.id)
                    && !self
                        .domain
                        .live
                        .orchestrator_runs
                        .lock_or_recover("the orchestrator runs")
                        .contains(&run.id)
                    && !self
                        .domain
                        .store
                        .lock_or_exit("the domain store")
                        .batch_completed(&run.id)?;
                let recorded_stop = recorded_stop.or_else(|| cold_batch.then(|| (BatchStop::Interrupted,
                "The daemon restarted without a durable terminal report from the native batch; reconcile its possible native work before retrying".into())));
                if active && runtime.cancel_requested {
                    if effects.is_empty() && operations.is_empty() && recorded_stop.is_none() {
                        self.acknowledge(&run.id)?;
                    } else {
                        self.failed(runtime, &run, "Orchestrator execution was cancelled; check its effects before retrying".into(), Some(BatchProblem { stop: BatchStop::Cancelled, calls: admitted, subjects: vec![], error: String::new() }))?;
                    }
                    return Ok(());
                }
                if let Some((stop, error)) = recorded_stop {
                    runtime.active_run = Some(run.clone());
                    self.failed(
                        runtime,
                        &run,
                        error.clone(),
                        Some(BatchProblem {
                            stop,
                            calls: admitted,
                            subjects: vec![],
                            error,
                        }),
                    )?;
                    return Ok(());
                }
                let continuation = !handed && (!effects.is_empty() || !operations.is_empty());
                let legacy_resolution = continuation && selection.is_none();
                if continuation {
                    if let Some(selection) = selection {
                        if !runtime
                            .batch_handoffs
                            .iter()
                            .any(|handoff| handoff.run_id == run.id)
                        {
                            runtime.batch_handoffs.push(crate::orchestrator_provider::BatchHandoff {
                        run_id: run.id.clone(), selection,
                        reason: "Storage interrupted batch completion; inspect confirmed results and continue pending goals without repeating calls".into(),
                    });
                        }
                    }
                    if !runtime
                        .pending_resolutions
                        .iter()
                        .any(|resolution| resolution.run_id == run.id)
                    {
                        runtime.pending_resolutions.push(Resolution {
                            run_id: run.id.clone(),
                            message_ids: run.message_ids.clone(),
                        });
                    }
                }
                if runtime
                    .active_run
                    .as_ref()
                    .is_some_and(|active| active.id == run.id)
                {
                    runtime.active_run = None;
                    runtime.last_error = None;
                    runtime.failure = None;
                    runtime.retry_at = None;
                    runtime.cancel_requested = false;
                }
                if continuation {
                    // Keep remaining goals durable before their old claims disappear from
                    // discovery, including when a different run owns the runtime record.
                    save(&self.home, &runtime)?;
                }
                self.domain
                    .store
                    .lock_or_exit("the domain store")
                    .reconcile_orchestrator_batch(&run.id, continuation)?;
                if !continuation {
                    save(&self.home, &runtime)?;
                }
                if !continuation || legacy_resolution {
                    self.domain
                        .store
                        .lock_or_exit("the domain store")
                        .finish_batch(&run.id)?;
                    self.domain
                        .live
                        .orchestrator_runs
                        .lock_or_recover("the orchestrator runs")
                        .remove(&run.id);
                }
                self.domain.orchestrator_progress();
                log!(
                    "orchestrator: recovered incomplete batch {}{}",
                    run.id,
                    if legacy_resolution {
                        "; legacy claims resolved without replay"
                    } else if continuation {
                        "; confirmed results passed to a fresh batch"
                    } else {
                        ""
                    }
                );
            }
            return Ok(());
        }
    }
}
