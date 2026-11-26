//! A task never ends with its pull request open and nobody to answer for it. When the user cancels
//! it, Alondra closes the PR on GitHub with the reason. When the orchestrator or Alondra closes it,
//! the close itself delivers the PR to the user, who merges or closes it: nothing waits for GitHub.
//! A close an older build left waiting is settled on what GitHub reports: a merged PR finishes the
//! task, a closed one lets it be cancelled, and an open one reaches the user as a delivery.
use super::*;

/// A cancelled task whose PR Alondra still holds open, with no one closing it, ends up as if
/// the user's cancel had just closed it: Alondra closes the PR with the cancel's reason until
/// GitHub reports it closed. A PR Alondra knew closed is closed once only: reopened, it is the
/// user's to decide.
pub(crate) fn close_pr_of_cancelled_task(domain: &mut DomainState, actor: &Actor, task: &TaskId, pr_id: &PrId, branch: &str, status: &OpenPrStatus,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    system(actor)?;
    nonempty(branch)?;
    if domain.branch_has_live_task(branch) { return Ok(()); }
    head(&status.head)?;
    nonempty(&status.base)?;
    let t = domain.task(task)?;
    if t.state != TaskState::Cancelled {
        return refuse("open_pr_close", "task_id", &format!("Task {task} is not cancelled, so Alondra does not close its pull request"));
    }
    if status.state != PrState::Open {
        return refuse("open_pr_close", "status", &format!("GitHub does not report the pull request {pr_id} open"));
    }
    let Some(pr) = domain.task_prs(t).find(|pr| pr.id == *pr_id).cloned() else {
        return refuse("open_pr_close", "pr_id", &format!("Task {task} has no pull request {pr_id}"));
    };
    if pr.state != PrState::Open {
        return refuse("open_pr_close", "pr_id", &format!(
            "The pull request {pr_id} was closed and someone reopened it on GitHub: it goes to the user, Alondra does not close it again"));
    }
    match &t.open_pr {
        Some(OpenPrPending::Close { .. }) => return Ok(()),
        Some(OpenPrPending::Deliver { .. }) => {
            return refuse("open_pr_close", "task_id", &format!("Task {task} is already delivering its pull request"));
        }
        None => (),
    }
    let comment = cancelled_pr_comment(&t.title);
    if (&pr.head, &pr.base, pr.state, &pr.merged_head) != (&status.head, &status.base, PrState::Open, &None) {
        apply(domain, actor, &Command::ObservePrState { pr_id: pr.id.clone(), head: status.head.clone(), base: status.base.clone(),
            state: PrState::Open, merged_head: None }, effects, created, lifecycles)?;
    }
    domain.tasks.get_mut(task).expect("found above").open_pr = Some(OpenPrPending::Close { comment });
    domain.invalidate(task);
    Ok(())
}
/// What GitHub reports of the PR of a task closed while its PR was open settles it.
pub(crate) fn settle_open_pr(domain: &mut DomainState, actor: &Actor, task: &TaskId, status: &OpenPrStatus, selected: Option<&PrId>,
    effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
    system(actor)?;
    head(&status.head)?;
    nonempty(&status.base)?;
    let Some(OpenPrPending::Deliver { result }) = domain.task(task)?.open_pr.clone() else {
        return refuse("open_pr_settled", "task_id", &format!("Task {task} has no open pull request left to deliver"));
    };
    let pr = selected.and_then(|id| domain.pull_requests.get(id))
        .or_else(|| domain.open_pr_of(task)).or_else(|| domain.task_prs(&domain.tasks[task]).last()).cloned()
        .ok_or_else(|| DomainError::new("not_found", "The task has no pull request"))?;
    let merged_head = (status.state == PrState::Merged).then(|| status.head.clone());
    if (&pr.head, &pr.base, pr.state, &pr.merged_head) != (&status.head, &status.base, status.state, &merged_head) {
        apply(domain, actor, &Command::ObservePrState { pr_id: pr.id.clone(), head: status.head.clone(), base: status.base.clone(),
            state: status.state, merged_head }, effects, created, lifecycles)?;
    }
    domain.tasks.get_mut(task).expect("found above").open_pr = None;
    if status.state == PrState::Closed {
        if let Some(result) = result {
            domain.tasks.get_mut(task).expect("found above").stall_closing = Some(StallClosing::Settled { result });
        }
        return domain.cancel(task, effects, lifecycles);
    }
    domain.deliver_open_pr(task, &pr, status, result, created, lifecycles)
}
