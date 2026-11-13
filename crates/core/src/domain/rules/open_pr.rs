//! A task never ends with its pull request open and nobody to answer for it. When the user cancels
//! it, Alondra closes the PR on GitHub with the reason. When the orchestrator or Alondra closes it,
//! the close itself delivers the PR to the user, who merges or closes it: nothing waits for GitHub.
//! A close an older build left waiting is settled on what GitHub reports: a merged PR finishes the
//! task, a closed one lets it be cancelled, and an open one reaches the user as a delivery.
use super::*;
use crate::open_pr_delivery::{OPEN_PR_DELIVERY_PREFIX, OpenPrDelivery, PrReadiness};

/// The comment Alondra closes the PR of a task the user cancelled with.
pub fn cancelled_pr_comment(title: &str) -> String {
    format!("Cancelled in Alondra: {title}")
}

impl DomainState {
    pub fn branch_has_live_task(&self, branch: &str) -> bool {
        branch.strip_prefix("worktree-").is_some_and(|id|
            self.tasks.get(&TaskId::from(id)).is_some_and(|task| !task.state.terminal()))
    }


    /// The first of the task's pull requests still open on GitHub.
    pub fn open_pr_of(&self, task: &TaskId) -> Option<&PullRequest> {
        self.open_prs(self.tasks.get(task)?).next()
    }

    /// Ends `task` for `actor`, who closes it without a delivery. With its PR open, the user's
    /// cancel also closes the PR on GitHub, and anyone else's delivers the PR to the user in the
    /// same command: a task with its PR open ends only with the user's merge or close of it.
    pub(in crate::domain) fn close_task(&mut self, actor: &Actor, task: &TaskId, result: Option<String>,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        let explained = result.is_some() && !self.task(task)?.pending_user_inputs.is_empty();
        if !matches!(actor, Actor::User { .. }) && !self.task(task)?.pending_user_inputs.is_empty() && !explained {
            return fail("pending_user_message", "A user message still needs a final delivery before the task can be cancelled");
        }
        if matches!(actor, Actor::User { .. }) || explained {
            self.tasks.get_mut(task).unwrap().pending_user_inputs.clear();
        }
        let Some(pr) = self.open_pr_of(task).cloned() else {
            return self.cancel(task, effects, lifecycles);
        };
        if matches!(actor, Actor::User { .. }) {
            let t = self.tasks.get_mut(task).expect("the PR's task exists");
            t.open_pr = Some(OpenPrPending::Close { comment: cancelled_pr_comment(&t.title) });
            return self.cancel(task, effects, lifecycles);
        }
        if !explained {
            self.delivered_pr_stays(actor, task, self.lifecycle(lifecycles, task)?.phase())?;
        }
        let known = OpenPrStatus { state: pr.state, head: pr.head.clone(), base: pr.base.clone(),
            mergeable: PrMergeable::Computing, checks: PrChecks::None };
        self.deliver_open_pr(task, &pr, &known, result, created, lifecycles)
    }

    /// A PR the task delivered waits for the user's review: only the user accepts or cancels it.
    pub(in crate::domain) fn delivered_pr_stays(&self, actor: &Actor, task: &TaskId, phase: alondra_domain::TaskPhase) -> Result<()> {
        let Some(pr) = self.open_pr_of(task).filter(|_| !matches!(actor, Actor::User { .. }) && phase == alondra_domain::TaskPhase::Delivered) else {
            return Ok(());
        };
        refuse("open_pr_delivered", "task_id", &format!(
            "Task {task} delivered its pull request #{}, which waits for the user's review: only the user accepts or cancels it", pr.number))
    }

    /// A cancelled task whose PR is open on GitHub, with no one closing it, ends up as if the
    /// user's cancel had just closed it: Alondra closes the PR with the cancel's reason until
    /// GitHub reports it closed.
    pub(in crate::domain) fn close_pr_of_cancelled_task(&mut self, actor: &Actor, task: &TaskId, pr_id: &PrId, branch: &str, status: &OpenPrStatus,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        system(actor)?;
        nonempty(branch)?;
        if self.branch_has_live_task(branch) { return Ok(()); }
        head(&status.head)?;
        nonempty(&status.base)?;
        let t = self.task(task)?;
        if t.state != TaskState::Cancelled {
            return refuse("open_pr_close", "task_id", &format!("Task {task} is not cancelled, so Alondra does not close its pull request"));
        }
        if status.state != PrState::Open {
            return refuse("open_pr_close", "status", &format!("GitHub does not report the pull request {pr_id} open"));
        }
        let Some(pr) = self.task_prs(t).find(|pr| pr.id == *pr_id).cloned() else {
            return refuse("open_pr_close", "pr_id", &format!("Task {task} has no pull request {pr_id}"));
        };
        match &t.open_pr {
            Some(OpenPrPending::Close { .. }) => return Ok(()),
            Some(OpenPrPending::Deliver { .. }) => {
                return refuse("open_pr_close", "task_id", &format!("Task {task} is already delivering its pull request"));
            }
            None => (),
        }
        let comment = cancelled_pr_comment(&t.title);
        if (&pr.head, &pr.base, pr.state, &pr.merged_head) != (&status.head, &status.base, PrState::Open, &None) {
            self.apply(actor, &Command::ObservePrState { pr_id: pr.id.clone(), head: status.head.clone(), base: status.base.clone(),
                state: PrState::Open, merged_head: None }, effects, created, lifecycles)?;
        }
        self.tasks.get_mut(task).expect("found above").open_pr = Some(OpenPrPending::Close { comment });
        self.invalidate(task);
        Ok(())
    }

    /// What GitHub reports of the PR of a task closed while its PR was open settles it.
    pub(in crate::domain) fn settle_open_pr(&mut self, actor: &Actor, task: &TaskId, status: &OpenPrStatus, selected: Option<&PrId>,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        system(actor)?;
        head(&status.head)?;
        nonempty(&status.base)?;
        let Some(OpenPrPending::Deliver { result }) = self.task(task)?.open_pr.clone() else {
            return refuse("open_pr_settled", "task_id", &format!("Task {task} has no open pull request left to deliver"));
        };
        let pr = selected.and_then(|id| self.pull_requests.get(id))
            .or_else(|| self.open_pr_of(task)).or_else(|| self.task_prs(&self.tasks[task]).last()).cloned()
            .ok_or_else(|| DomainError::new("not_found", "The task has no pull request"))?;
        let merged_head = (status.state == PrState::Merged).then(|| status.head.clone());
        if (&pr.head, &pr.base, pr.state, &pr.merged_head) != (&status.head, &status.base, status.state, &merged_head) {
            self.apply(actor, &Command::ObservePrState { pr_id: pr.id.clone(), head: status.head.clone(), base: status.base.clone(),
                state: status.state, merged_head }, effects, created, lifecycles)?;
        }
        self.tasks.get_mut(task).expect("found above").open_pr = None;
        if status.state == PrState::Closed {
            if let Some(result) = result {
                self.tasks.get_mut(task).expect("found above").stall_closing = Some(StallClosing::Settled { result });
            }
            return self.cancel(task, effects, lifecycles);
        }
        self.deliver_open_pr(task, &pr, status, result, created, lifecycles)
    }

    /// Alondra's delivery of the open `pr` of `task` to the user, as `status` reports it.
    fn deliver_open_pr(&mut self, task: &TaskId, pr: &PullRequest, status: &OpenPrStatus, result: Option<String>,
        created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        let t = &self.tasks[task];
        let card = OpenPrDelivery { title: t.title.clone(), repository: pr.repository.clone(), number: pr.number,
            readiness: PrReadiness::of(status), result };
        let (need, why) = card.text(crate::views::Lang::En);
        let id = InteractionId(format!("{OPEN_PR_DELIVERY_PREFIX}{}-{}", pr.slug(), self.revision + 1));
        let kind = if status.state == PrState::Open { CardKind::Review } else { CardKind::Know };
        let content = CardContent { kind: ContentKind::Report, summary: None, body: card.detail(crate::views::Lang::En) };
        let spec = CardSpec { system: Some(SystemCard::OpenPrDelivery), need, why, content: Some(content),
            ..CardSpec::new(kind, clip(&t.title, CardSpec::TITLE_MAX)) };
        let targets = self.with_open_prs(task, &[DeliveryTarget::PullRequest { pr_id: pr.id.clone(), head: status.head.clone() }]);
        self.insert_interaction(&Actor::System, &id, &Scope::Task { task_id: task.clone() }, &Recipient::User, &card.to_body(), false, &[],
            InteractionPayload::Delivery { targets, is_final: true, acceptance: None, feedback: vec![] }, &spec, Source::Delivery)?;
        self.invalidate(task);
        let standing = self.lifecycle(lifecycles, task)?.delivery().cloned();
        let mut replacement_notice = None;
        if let Some(previous) = standing {
            replacement_notice = Some(self.next_input(task));
            self.hand_back(task, &previous, delivery_replaced(), lifecycles)?;
        }
        self.decide(lifecycles, task, LifecycleRecord::Presented { delivery: id.clone() })?;
        if let Some(notice) = replacement_notice {
            self.consume_replacement_notice(task, notice, lifecycles)?;
        }
        let t = self.tasks.get_mut(task).expect("found above");
        t.final_presentation = Some(id.clone());
        t.verdict = None;
        created.push(id.0);
        Ok(())
    }
}
