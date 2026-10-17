use super::*;

#[derive(Debug)]
pub(super) struct PreparedCommit {
    pub(super) commit: Commit,
    pub(super) version: i64,
    pub(super) remember: bool,
}

/// The owner must roll back the whole transaction on error; this stages no independent commit.
pub(super) fn stage(
    conn: &Connection,
    cached: &Option<Cached>,
    actor: &Actor,
    envelope: &CommandEnvelope,
    transport: Option<TransportIdentity<'_>>,
    fill_revision: bool,
    attached: Attached<'_>,
    journal: &mut alondra_store::LifecycleTransaction<'_>,
) -> std::result::Result<PreparedCommit, StoreError> {
    if conn.is_autocommit() {
        return Err(StoreError::Storage(anyhow::anyhow!(
            "domain command requires an active transaction"
        )));
    }
    let actor_key = String::from_utf8(canonical::bytes(&match actor {
        Actor::Orchestrator { .. } => serde_json::json!({"kind":"orchestrator"}),
        _ => serde_json::to_value(actor)?,
    })?)
    .context("actor encoding")?;
    let digest = canonical::digest(envelope)
        .map_err(|e| DomainError::new("invalid_input", e.to_string()))?;
    check_schema(conn)?;
    let (state, hit, version) = state_in(cached, conn)?;
    if envelope.protocol_version != PROTOCOL_VERSION {
        return Err(
            DomainError::new("incompatible_protocol", "domain protocol 2 is required").into(),
        );
    }
    if envelope.data_epoch != state.data_epoch {
        return Err(DomainError::new("stale_epoch", "domain epoch changed").into());
    }
    if let Actor::Orchestrator { generation } = actor {
        if *generation != state.orchestrator_generation {
            return Err(
                DomainError::new("stale_generation", "orchestrator generation changed").into(),
            );
        }
    }
    if let Some((sender, id, ciphertext_digest)) = transport {
        let stored: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT digest,result FROM local_command_results WHERE sender=?1 AND request_id=?2",
                params![sender, id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((previous, result)) = stored {
            if previous != ciphertext_digest {
                return Err(DomainError::new(
                    "idempotency_conflict",
                    "request ID reused with different encrypted bytes",
                )
                .into());
            }
            let outcome = serde_json::from_str(&result)?;
            return Ok(PreparedCommit {
                commit: Commit {
                    state,
                    outcome,
                    replayed: true,
                },
                version,
                remember: !hit,
            });
        }
    }
    let stored:Option<(Vec<u8>,String)>=conn.query_row("SELECT digest,result FROM semantic_receipts WHERE actor=?1 AND command_id=?2 AND data_epoch=?3",params![actor_key,envelope.command_id.0,envelope.data_epoch.0],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((previous, result)) = stored {
        if previous != digest {
            return Err(DomainError::new(
                "idempotency_conflict",
                "command ID reused with different payload",
            )
            .into());
        }
        let outcome = serde_json::from_str(&result)?;
        if let Some((sender, id, ciphertext_digest)) = transport {
            repository::exec(
                conn,
                "INSERT INTO local_command_results VALUES(?1,?2,?3,?4)",
                params![sender, id, ciphertext_digest.as_slice(), result],
            )?;
        }
        return Ok(PreparedCommit {
            commit: Commit {
                state,
                outcome,
                replayed: true,
            },
            version,
            remember: true,
        });
    }
    let filled;
    let envelope = if fill_revision && envelope.expected_revision == 0 {
        filled = CommandEnvelope {
            expected_revision: state.revision,
            ..envelope.clone()
        };
        &filled
    } else {
        envelope
    };
    let mut next = DomainState::clone(&state);
    let outcome = next.execute(actor, envelope)?;
    if outcome.changed {
        repository::save_changes(conn, journal, &state, &next)?;
        if let Command::RecordScheduleRunFailed { run_id, .. } = &envelope.command
            && let Some(request_id) = &next.schedule.runs[run_id].request_id
        {
            repository::remember_schedule_failed_launch(conn, request_id)?;
        }
        match (&attached, &envelope.command) {
            (Attached::Request(launch), Command::CreateRequest { request_id, .. }) => {
                repository::remember_request_launch(conn, request_id, launch)?;
            }
            (
                _,
                Command::RecordScheduleRunSent {
                    run_id,
                    request,
                    launch,
                    ..
                },
            ) => {
                if let Ok(options) =
                    serde_json::from_str::<alondra_core::options::Resolved>(&launch.options_json)
                {
                    let project = next
                        .schedule
                        .runs
                        .get(run_id)
                        .and_then(|run| run.definition_snapshot.as_ref())
                        .and_then(|definition| definition.project.clone());
                    repository::remember_request_launch(
                        conn,
                        &request.id,
                        &RequestLaunch {
                            project,
                            options,
                            issue: None,
                            issue_text: None,
                            attachments: vec![],
                        },
                    )?;
                }
            }
            _ => {}
        }
        if let Some((task_id, plan)) = plan_set_by(&envelope.command) {
            store_launch_plan(conn, task_id, plan)?;
        }
        if let Command::SettlePlanReview { task_id, outcome: PlanReviewOutcome::Cancelled { reason }, .. } = &envelope.command {
            repository::exec(conn,
                "INSERT INTO task_cancel_reason(task_id,reason) VALUES(?1,?2) ON CONFLICT(task_id) DO UPDATE SET reason=excluded.reason",
                params![task_id.0, reason])?;
        }
        for (id, task) in &next.tasks {
            if let Some(before) = state.tasks.get(id).filter(|before| needs_a_node_again(before, task)) {
                queue_for_a_node(conn, id, before.state.terminal())?;
            }
        }
        let at = alondra_core::now_millis();
        repository::exec(
            conn,
            "INSERT INTO command_audit(revision,actor,command_id,data_epoch,command,at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                outcome.revision as i64,
                actor_key,
                envelope.command_id.0,
                envelope.data_epoch.0,
                String::from_utf8(canonical::bytes(&envelope.command)?)
                    .context("command encoding")?,
                at
            ],
        )?;
        for request in next
            .requests
            .keys()
            .filter(|id| !state.requests.contains_key(*id))
        {
            repository::exec(
                conn,
                "UPDATE requests SET created_at=?1 WHERE id=?2",
                params![at, request.0],
            )?;
        }
        for (id, task) in next.tasks.iter().filter(|(id, task)| {
            state.tasks.get(*id).map(|before| before.state) != Some(task.state)
        }) {
            repository::exec(
                conn,
                "INSERT INTO task_history(revision,task_id,state,summary) VALUES(?1,?2,?3,?4)",
                params![
                    outcome.revision as i64,
                    id.0,
                    serde_json::to_value(task.state)?
                        .as_str()
                        .unwrap_or_default(),
                    history_summary(&envelope.command)
                ],
            )?;
        }
        for (index, effect) in outcome.effects.iter().enumerate() {
            let (kind, agent, task, interaction, message) = match effect {
                DomainEffect::NotifyUser {
                    task_id,
                    interaction_id,
                } => (
                    "orchestrator",
                    None,
                    task_id.as_ref().map(|x| x.0.as_str()),
                    Some(interaction_id.0.as_str()),
                    "A domain interaction is ready for your review.".to_owned(),
                ),
                DomainEffect::NotifyAgent {
                    agent_id,
                    task_id,
                    interaction_id,
                    message,
                } => (
                    "agent",
                    Some(agent_id.0.as_str()),
                    task_id.as_ref().map(|x| x.0.as_str()),
                    interaction_id.as_ref().map(|x| x.0.as_str()),
                    message.clone(),
                ),
                DomainEffect::NotifyOrchestrator {
                    interaction_id: Some(interaction_id),
                    ..
                } => (
                    "orchestrator",
                    None,
                    None,
                    Some(interaction_id.0.as_str()),
                    "A domain interaction requires your response.".to_owned(),
                ),
                DomainEffect::NotifyOrchestrator {
                    interaction_id: None,
                    request_id,
                } => (
                    "orchestrator",
                    None,
                    None,
                    None,
                    match request_id {
                        Some(request)
                            if next.requests.get(request).is_some_and(|r| r.maintenance) =>
                        {
                            format!(
                                "Maintenance message {request} (sent with alondra send): act on it, then close it with close_request; it gets no card for the user."
                            )
                        }
                        Some(request) => match next.request_plans.get(request) {
                            Some(approved) => {
                                let source = next
                                    .tasks
                                    .get(&approved.task_id)
                                    .map_or("", |task| task.title.as_str());
                                format!(
                                    "The user approved the plan that Task “{source}” ({}) proposed in its final result. Request {request} holds that plan: name it with title_request, then create one implementation Task under it, or one per phase when the plan is in phases: in the same project folder as that Task, with dependencies on {} and role implementation. The first takes the whole plan as its checklist; each later one takes its phase's points from the first with plan_items, with dependencies only on the phases it truly needs. Each starts with its points approved and the whole plan in its mission: never ask its node for a new plan.",
                                    approved.task_id, approved.task_id
                                )
                            }
                            None => format!(
                                "The user created Request {request}: turn it into Tasks, or answer it and close it."
                            ),
                        },
                        None => "The domain has something for you.".to_owned(),
                    },
                ),
                DomainEffect::ReviewDependentPlans { review_id, task_id } => (
                    "orchestrator",
                    None,
                    Some(task_id.0.as_str()),
                    None,
                    crate::plan_reviews::orchestrator_text(&next, review_id, &|task| stored_plan(conn, task).ok().flatten()),
                ),
                DomainEffect::ScheduleLaunchFailed {
                    task_id,
                    title,
                    reason,
                } => (
                    "orchestrator",
                    None,
                    Some(task_id.0.as_str()),
                    None,
                    format!(
                        "The scheduled Task “{title}” ({task_id}) could not start: {reason} It stays not started with no node and nothing retries it: tell the user why and ask before starting it again."
                    ),
                ),
            };
            repository::exec(
                conn,
                "INSERT INTO domain_messages(revision,ordinal,recipient_kind,agent_id,task_id,interaction_id,message) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![
                    outcome.revision as i64,
                    index as i64,
                    kind,
                    agent,
                    task,
                    interaction,
                    message
                ],
            )?;
            if matches!(effect, DomainEffect::NotifyUser { .. }) {
                repository::exec(
                    conn,
                    "INSERT INTO domain_message_user_targets(message_id) VALUES(?1)",
                    [conn.last_insert_rowid()],
                )?;
            }
            repository::exec(
                conn,
                "INSERT INTO domain_outbox(message_id,state) VALUES(?1,'pending')",
                [conn.last_insert_rowid()],
            )?;
        }
    } else {
        ensure_no_effects(&outcome)?;
    }
    let result = serde_json::to_string(&outcome)?;
    repository::exec(
        conn,
        "INSERT INTO semantic_receipts(actor,command_id,data_epoch,digest,result) VALUES(?1,?2,?3,?4,?5)",
        params![
            actor_key,
            envelope.command_id.0,
            envelope.data_epoch.0,
            digest.as_slice(),
            result
        ],
    )?;
    if let Some((sender, id, ciphertext_digest)) = transport {
        repository::exec(
            conn,
            "INSERT INTO local_command_results VALUES(?1,?2,?3,?4)",
            params![sender, id, ciphertext_digest.as_slice(), result],
        )?;
    }
    Ok(PreparedCommit {
        commit: Commit {
            state: Arc::new(next),
            outcome,
            replayed: false,
        },
        version,
        remember: true,
    })
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;

/// A task that was over, or was left in progress with no node, and now waits for one: the fleet
/// must start it, so it goes back to its queue.
fn needs_a_node_again(before: &Task, after: &Task) -> bool {
    (before.state.terminal() || before.started_without_node())
        && after.state == TaskState::NotStarted && after.agent_id.is_none() && !after.archived
}

/// The launch plan a command gives a task: a new task's, a suggestion that became one, or the new
/// plan of a reviewed one.
fn plan_set_by(command: &Command) -> Option<(&TaskId, &LaunchPlan)> {
    match command {
        Command::CreateTask { task_id, launch: Some(plan), .. }
        | Command::SettleSuggestion { decision: SuggestionDecision::NewTask { task_id, launch: plan, .. }, .. }
        | Command::SettlePlanReview { task_id, outcome: PlanReviewOutcome::PlanChanged { launch: plan, .. }, .. } => Some((task_id, plan)),
        _ => None,
    }
}

/// A new plan keeps the place its first one gave the task; a queued launch starts with it.
fn store_launch_plan(conn: &Connection, task: &TaskId, plan: &LaunchPlan) -> std::result::Result<(), StoreError> {
    repository::exec(conn,
        "INSERT INTO task_launch_plans(task_id,plan,role,at) VALUES(?1,?2,?3,?4) ON CONFLICT(task_id) DO UPDATE SET plan=excluded.plan,role=excluded.role",
        params![task.0, serde_json::to_string(plan)?, plan.role.map(|role| role.as_str()), alondra_core::now_millis()])?;
    let body = crate::domain_orchestrator_tools::LaunchBody::from_plan(task.clone(), plan).to_queue();
    repository::exec(conn, "UPDATE launch_queue SET body=?2 WHERE task_id=?1", params![task.0, serde_json::to_string(&body)?])?;
    Ok(())
}

fn stored_plan(conn: &Connection, task: &TaskId) -> Result<Option<LaunchPlan>> {
    let plan: Option<String> = conn.query_row("SELECT plan FROM task_launch_plans WHERE task_id=?1", [&task.0], |row| row.get(0)).optional()?;
    Ok(plan.map(|plan| serde_json::from_str(&plan)).transpose()?)
}

/// Puts the task in the fleet's queue with its launch plan, in the transaction that reopened it,
/// at the place its plan gave it, so being cancelled and reopened costs it no seniority. What its
/// previous starts left (an automatic launch, a retry, a refusal) belongs to that life and goes.
/// A reopened task with no plan could never start again, so its reopening is refused.
fn queue_for_a_node(conn: &Connection, task: &TaskId, reopened: bool) -> std::result::Result<(), StoreError> {
    let planned: Option<(String, i64)> = conn
        .query_row("SELECT plan,at FROM task_launch_plans WHERE task_id=?1", [&task.0], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?;
    let Some((plan, since)) = planned else {
        if reopened {
            return Err(DomainError::new("invalid_input", format!(
                "{task} has no launch plan, so nothing could start it again: create it again with its launch plan")).into());
        }
        return Ok(());
    };
    let plan: crate::launch_plans::LaunchPlan = serde_json::from_str(&plan).context("stored launch plan")?;
    let body = crate::domain_orchestrator_tools::LaunchBody::from_plan(task.clone(), &plan).to_queue();
    repository::exec(conn,
        "INSERT INTO launch_queue(task_id,body,origin,enqueued_at) VALUES(?1,?2,'reopen',?3) ON CONFLICT(task_id) DO UPDATE SET body=excluded.body,enqueued_at=min(enqueued_at,excluded.enqueued_at)",
        params![task.0, serde_json::to_string(&body)?, since])?;
    for table in ["auto_launches", "start_retries", "launch_refusals"] {
        repository::exec(conn, &format!("DELETE FROM {table} WHERE task_id=?1"), [&task.0])?;
    }
    Ok(())
}
