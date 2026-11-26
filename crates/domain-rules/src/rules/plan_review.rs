//! A decision of the user on a Task that others wait for holds those that have not started, until
//! the orchestrator reviews the launch plan each was given before it. Only a settlement releases
//! one: the orchestrator's, or Alondra's once nobody reviewed it in time, which keeps the plan and
//! tells its node what was decided. Never the end of what it waits for.
use super::*;

/// The Tasks that wait for `source`, directly or through others, and have not started: the
/// plans they would start with were written before the decision.
pub(crate) fn unstarted_dependents(domain: &DomainState, source: &TaskId) -> BTreeSet<TaskId> {
    let waits_on = |t: &Task, up: &TaskId| {
        t.dependencies.contains(up) || t.delivered_dependencies.contains(up) || t.stack.as_ref().is_some_and(|s| &s.upstream_task == up)
    };
    let mut seen = BTreeSet::from([source.clone()]);
    let mut frontier = vec![source.clone()];
    let mut found = BTreeSet::new();
    while let Some(up) = frontier.pop() {
        for t in domain.tasks.values().filter(|t| waits_on(t, &up)) {
            if seen.insert(t.id.clone()) {
                frontier.push(t.id.clone());
                if unstarted(t) {
                    found.insert(t.id.clone());
                }
            }
        }
    }
    found
}
pub(crate) fn unstarted(t: &Task) -> bool {
    !t.started && t.agent_id.is_none() && !t.archived && t.final_presentation.is_none()
        && matches!(t.state, TaskState::NotStarted | TaskState::WaitingDependency)
}
/// The user's answer completed a node's decision on a Task others wait for: every dependent
/// that has not started is held in the same commit, and the orchestrator is told.
pub(crate) fn hold_for_plan_review(domain: &mut DomainState, answered: &InteractionId, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
    let i = &domain.interactions[answered];
    let (Author::Agent { .. }, Scope::Task { task_id }, Recipient::User) = (&i.author, &i.scope, &i.recipient) else { return Ok(()) };
    if !domain.group_complete(i) {
        return Ok(());
    }
    let decisions: Vec<InteractionId> = domain.group_members(i).into_iter().filter(|m| domain.decides(m)).map(|m| m.id.clone()).collect();
    if decisions.is_empty() {
        return Ok(());
    }
    let task_id = task_id.clone();
    let dependents = unstarted_dependents(domain, &task_id);
    if dependents.is_empty() {
        return Ok(());
    }
    for dependent in &dependents {
        domain.decide(lifecycles, dependent, LifecycleRecord::HeldForPlanReview { review: answered.clone() })?;
    }
    domain.plan_reviews.insert(answered.clone(), PlanReview {
        task_id: task_id.clone(),
        decisions,
        at_revision: domain.revision + 1,
        dependents: dependents.into_iter().map(|t| (t, None)).collect(),
    });
    effects.push(DomainEffect::ReviewDependentPlans { review_id: answered.clone(), task_id });
    Ok(())
}
/// No node is assigned to or starts a Task a decision holds.
pub(crate) fn not_held_for_plan_review(domain: &DomainState, task: &TaskId) -> Result<()> {
    let Some((_, review)) = domain.plan_reviews_holding(task).next() else { return Ok(()) };
    let source = domain.tasks.get(&review.task_id).map_or("", |t| t.title.as_str());
    refuse("plan_review_pending", "task_id", &format!(
        "Task {task} waits for the orchestrator to review its plan after the user's decision on «{source}»: it starts once settle_plan_review settles it"))
}
pub(crate) fn settle_plan_review(domain: &mut DomainState, actor: &Actor, review_id: &InteractionId, task_id: &TaskId, outcome: &PlanReviewOutcome,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    domain.fence(actor)?;
    if !matches!((actor, outcome), (Actor::Orchestrator { .. }, _) | (Actor::System, PlanReviewOutcome::Unchanged { .. })) {
        return fail("forbidden", "Only the orchestrator settles a plan review");
    }
    let Some(review) = domain.plan_reviews.get(review_id) else {
        return refuse("plan_review_not_found", "review_id", "No decision of the user is waiting for a plan review with this id: use the review_id the message gave you");
    };
    match review.dependents.get(task_id) {
        None => {
            let held: Vec<&str> = review.dependents.keys().map(|t| t.0.as_str()).collect();
            return refuse("not_in_plan_review", "task_id", &format!("This review holds {}, not {task_id}", held.join(", ")));
        }
        Some(Some(_)) => return refuse("plan_review_settled", "task_id", &format!("The plan review of {task_id} is already settled and its outcome is final")),
        Some(None) => {}
    }
    match outcome {
        PlanReviewOutcome::Unchanged { reason } | PlanReviewOutcome::Cancelled { reason } => nonempty(reason)?,
        PlanReviewOutcome::PlanChanged { launch, reason } => {
            nonempty(reason)?;
            if let Some(field) = launch.missing() {
                return refuse("launch_plan_required", field, "a launch plan needs a name and a folder to start the node in");
            }
        }
        PlanReviewOutcome::KeptByUser =>
            return refuse("invalid_input", "outcome.kind", "Settle it with unchanged, plan_changed (with the new launch plan) or cancelled, each with its reason"),
    }
    settle_held(domain, review_id, task_id, outcome.clone(), lifecycles)?;
    if matches!(outcome, PlanReviewOutcome::Cancelled { .. }) && !domain.task(task_id)?.state.terminal() {
        apply(domain, actor, &Command::CancelTask { task_id: task_id.clone(), reason: None }, effects, created, lifecycles)?;
    }
    Ok(())
}
/// Once a review holds nothing, the problem raised about it no longer asks anything.
pub(crate) fn settle_held(domain: &mut DomainState, review_id: &InteractionId, task_id: &TaskId, outcome: PlanReviewOutcome, lifecycles: &mut Lifecycles) -> Result<()> {
    let at_revision = domain.revision + 1;
    if domain.lifecycle(lifecycles, task_id)?.plan_reviews().iter().any(|(held, _)| *held == review_id) {
        let settled = match &outcome {
            PlanReviewOutcome::Unchanged { .. } => ReviewOutcome::Unchanged,
            PlanReviewOutcome::PlanChanged { .. } => ReviewOutcome::PlanChanged,
            PlanReviewOutcome::Cancelled { .. } => ReviewOutcome::Cancelled,
            PlanReviewOutcome::KeptByUser => ReviewOutcome::KeptByUser,
        };
        let plan_revision = domain.lifecycle(lifecycles, task_id)?.plan_revision();
        domain.decide(lifecycles, task_id, LifecycleRecord::PlanReviewSettled { review: review_id.clone(), plan_revision, outcome: settled })?;
    }
    let Some(review) = domain.plan_reviews.get_mut(review_id) else { return Ok(()) };
    if let Some(entry) = review.dependents.get_mut(task_id) {
        *entry = Some(PlanReviewSettlement { outcome, at_revision });
    }
    if review.open().next().is_some() {
        return Ok(());
    }
    let lead = format!("{PLAN_REVIEW_PROBLEM_PREFIX}{review_id}-");
    let moot: Vec<InteractionId> = domain.interactions.values()
        .filter(|i| i.author == Author::System && i.id.0.starts_with(&lead) && domain.open(i))
        .map(|i| i.id.clone()).collect();
    for problem in moot {
        domain.retire(&problem);
    }
    Ok(())
}
/// The user answered the problem about a review the orchestrator left unsettled.
pub(crate) fn settle_plan_review_problem(domain: &mut DomainState, card: &InteractionId, option: &str, lifecycles: &mut Lifecycles) -> Result<()> {
    let Some(i) = domain.interactions.get(card).filter(|i| matches!(i.system_card, Some(SystemCard::Inbox { about: InboxCard::PlanReviewProblem, .. })) && i.author == Author::System) else { return Ok(()) };
    let Some(problem) = PlanReviewProblem::from_body(&i.body) else { return Ok(()) };
    if option != PLAN_REVIEW_KEEP_PLANS {
        return Ok(());
    }
    let open: Vec<TaskId> = domain.plan_reviews.get(&problem.review_id).map(|r| r.open().cloned().collect()).unwrap_or_default();
    for task in open {
        settle_held(domain, &problem.review_id, &task, PlanReviewOutcome::KeptByUser, lifecycles)?;
    }
    Ok(())
}
