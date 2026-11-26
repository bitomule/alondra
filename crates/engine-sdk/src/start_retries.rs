//! What keeps every open task in someone's hands once its start goes wrong, carried out: a task whose
//! start failed is put in front of the user or started again, as the domain decides
//! (`DomainState::react`). The failures themselves are recorded by `start_failures`.
use crate::engine::EngineOf as _;
use crate::locks::Poisonable as _;
use alondra_engine_ports::Runtime;
use alondra_domain_model::{DomainState, InteractionPayload, NodePhase, Reaction, START_RETRY, Start, StartProblem, TaskId};
use alondra_engine_kernel::start_failures::{failed, problem_of, raise};
use anyhow::Result;
use std::path::Path;

/// Carries out what must happen now so no open task is left in nobody's hands.
pub async fn react(domain: &dyn Runtime, home: &Path) -> Result<()> {
    settle_api_failure_cards(domain)?;
    let state = domain.state();
    record_plan_cancels(domain, &state)?;
    let facts = domain.engine().start_facts(&state)?;
    for reaction in state.react(&facts) {
        match reaction {
            Reaction::Ask(task, problem) => {
                raise(domain, &task, problem)?;
            }
            Reaction::Retry(task) => retry(domain, home, &task, facts.get(&task).is_some_and(|f| f.closed || f.start == Start::Launched)).await?,
        }
    }
    Ok(())
}

/// Carries out the user's answer on the card of a node Alondra stopped sending back after its Claude
/// API errors: "try again now" sends it back at once; any other end of the card (the task
/// cancelled, the card gone) only forgets the streak.
fn settle_api_failure_cards(domain: &dyn Runtime) -> Result<()> {
    let streaks = domain.store().lock_or_exit("the domain store").api_failures()?;
    let state = domain.state();
    for (agent, streak) in streaks {
        let Some(card) = streak.card.as_ref().and_then(|card| state.interactions.get(card)) else { continue };
        if state.open(card) {
            continue;
        }
        let retried = !card.withdrawn && matches!(&card.payload, InteractionPayload::Question { answer: Some(answer), .. } if answer.option_id == START_RETRY);
        let live = state.node_phase(&agent).is_some_and(NodePhase::live) && alondra_engine_ports::admission::open_task_of(&state, &agent.0).is_some();
        if retried && live {
            let text = match serde_json::from_str::<StartProblem>(&card.body) {
                Ok(StartProblem::ApiFailed { error, detail, .. }) => alondra_engine_ports::api::resume_text(&error, detail.as_deref()),
                _ => alondra_engine_ports::api::resume_text("unknown", None),
            };
            log!("the user asked to try {} again after its Claude API errors", agent.0);
            domain.engine().tell_session(&agent.0, &text, true);
        }
        domain.store().lock_or_exit("the domain store").set_api_failures(&agent, None)?;
    }
    Ok(())
}

/// The orchestrator cancelled a task whose plan was refused: the reason it wrote is the task's.
fn record_plan_cancels(domain: &dyn Runtime, state: &DomainState) -> Result<()> {
    let cancels: Vec<(TaskId, String)> = state.interactions.values()
        .filter(|i| alondra_domain_model::is_plan_refusal(i))
        .filter_map(|i| match (&i.scope, &i.payload) {
            (alondra_domain_model::Scope::Task { task_id }, alondra_domain_model::InteractionPayload::Question { answer: Some(answer), .. })
                if answer.option_id == alondra_domain_model::START_CANCEL && state.tasks.get(task_id).is_some_and(|task| task.cancelled()) =>
                answer.inputs.values().find_map(|value| match value {
                    alondra_domain_model::InputValue::Text(text) if !text.trim().is_empty() => Some((task_id.clone(), text.trim().to_owned())),
                    _ => None,
                }),
            _ => None,
        })
        .collect();
    if cancels.is_empty() {
        return Ok(());
    }
    let store = domain.store().lock_or_exit("the domain store");
    let recorded = store.cancel_reasons()?;
    for (task, reason) in cancels.into_iter().filter(|(task, _)| !recorded.contains_key(task)) {
        store.set_cancel_reason(&task, &reason)?;
    }
    Ok(())
}

/// The user asked to try again: a task whose node closed gets a new node in that node's folder;
/// any other goes back to the gate, which starts it with its plan, or relaunches it when Alondra
/// closed its node. A relaunch refused
/// for a reason of its own is a failed start like any other. Trying again a folder Claude Code
/// did not trust is the user trusting it, so Alondra records that first, or says why it could not.
async fn retry(domain: &dyn Runtime, home: &Path, task: &TaskId, closed: bool) -> Result<()> {
    let untrusted = domain.state().last_start_problem(task).and_then(|problem| serde_json::from_str::<StartProblem>(&problem.body).ok());
    if let Some(StartProblem::Untrusted { folder, .. }) = untrusted
        && let Err(error) = domain.ports().accounts().grant_trust(Path::new(&folder), &alondra_engine_kernel::accounts_file::profiles(home))
    {
        log!("could not trust {folder} in Claude Code for {task}: {error}");
        return failed(domain, task, StartProblem::Untrusted { folder, error: Some(error) }, alondra_core::now_millis());
    }
    {
        let mut store = domain.store().lock_or_exit("the domain store");
        store.forget_auto_launch(task)?;
        store.set_start_retry(task, None)?;
        store.set_launch_refusal(task, None, alondra_core::now_millis())?;
    }
    if !closed {
        return Ok(());
    }
    match domain.engine().relaunch(home, task, false).await {
        Err(error) if !matches!(error.code.as_str(), "launch_failed" | "workspace_untrusted") => {
            if let Some(problem) = problem_of(&*domain.ports().accounts(), &error.code, &error.message, None) {
                failed(domain, task, problem, alondra_core::now_millis())?;
            }
        }
        _ => {}
    }
    Ok(())
}
