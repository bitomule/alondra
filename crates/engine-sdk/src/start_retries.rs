//! What keeps every open task in someone's hands once its start goes wrong, carried out: a task whose
//! start failed is put in front of the user or started again, as the domain decides
//! (`DomainState::react`). The failures themselves are recorded by `start_failures`.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use crate::start_failures::{facts, failed, problem_of, raise};
use alondra_domain_rules::{DomainState, Reaction, StartProblem, TaskId};
use anyhow::Result;
use std::path::Path;

/// Carries out what must happen now so no open task is left in nobody's hands.
pub async fn react(domain: &DomainRuntime, home: &Path) -> Result<()> {
    crate::api_failures::settle_cards(domain)?;
    let state = domain.state()?;
    record_plan_cancels(domain, &state)?;
    let facts = facts(domain, &state)?;
    for reaction in state.react(&facts) {
        match reaction {
            Reaction::Ask(task, problem) => {
                raise(domain, &task, problem)?;
            }
            Reaction::Retry(task) => retry(domain, home, &task, facts.get(&task).is_some_and(|f| f.closed)).await?,
        }
    }
    Ok(())
}

/// The orchestrator cancelled a task whose plan was refused: the reason it wrote is the task's.
fn record_plan_cancels(domain: &DomainRuntime, state: &DomainState) -> Result<()> {
    let cancels: Vec<(TaskId, String)> = state.interactions.values()
        .filter(|i| alondra_domain_rules::is_plan_refusal(i))
        .filter_map(|i| match (&i.scope, &i.payload) {
            (alondra_domain_rules::Scope::Task { task_id }, alondra_domain_rules::InteractionPayload::Question { answer: Some(answer), .. })
                if answer.option_id == alondra_domain_rules::START_CANCEL && state.tasks.get(task_id).is_some_and(|task| task.cancelled()) =>
                answer.inputs.values().find_map(|value| match value {
                    alondra_domain_rules::InputValue::Text(text) if !text.trim().is_empty() => Some((task_id.clone(), text.trim().to_owned())),
                    _ => None,
                }),
            _ => None,
        })
        .collect();
    if cancels.is_empty() {
        return Ok(());
    }
    let store = domain.store.lock_or_exit("the domain store");
    let recorded = store.cancel_reasons()?;
    for (task, reason) in cancels.into_iter().filter(|(task, _)| !recorded.contains_key(task)) {
        store.set_cancel_reason(&task, &reason)?;
    }
    Ok(())
}

/// The user asked to try again: a task whose node they closed gets a new node in that node's
/// folder; any other goes back to the gate, which starts it with its plan, or relaunches it when
/// Alondra closed its node. A relaunch refused
/// for a reason of its own is a failed start like any other. Trying again a folder Claude Code
/// did not trust is the user trusting it, so Alondra records that first, or says why it could not.
async fn retry(domain: &DomainRuntime, home: &Path, task: &TaskId, closed: bool) -> Result<()> {
    let untrusted = domain.state()?.last_start_problem(task).and_then(|problem| serde_json::from_str::<StartProblem>(&problem.body).ok());
    if let Some(StartProblem::Untrusted { folder, .. }) = untrusted
        && let Err(error) = crate::claude_trust::grant(Path::new(&folder), &crate::accounts::profiles(home))
    {
        log!("could not trust {folder} in Claude Code for {task}: {error}");
        return failed(domain, task, StartProblem::Untrusted { folder, error: Some(error) }, alondra_core::now_millis());
    }
    {
        let mut store = domain.store.lock_or_exit("the domain store");
        store.forget_auto_launch(task)?;
        store.set_start_retry(task, None)?;
        store.set_launch_refusal(task, None, alondra_core::now_millis())?;
    }
    if !closed {
        return Ok(());
    }
    match crate::domain_orchestrator_tools::relaunch(domain, home, task).await {
        Err(error) if !matches!(error.code.as_str(), "launch_failed" | "workspace_untrusted") => {
            if let Some(problem) = problem_of(&error.code, &error.message, None) {
                failed(domain, task, problem, alondra_core::now_millis())?;
            }
        }
        _ => {}
    }
    Ok(())
}
