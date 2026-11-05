//! A task never ends with its pull request open and nobody to answer for it. When the user cancels
//! it, Alondra closes the PR on GitHub with the reason. When the orchestrator or Alondra closes it,
//! Alondra reads the PR on GitHub and decides on what GitHub reports, never on words: a merged PR
//! finishes the task, a closed one lets it be cancelled, and an open one reaches the user as a
//! delivery, with what keeps it from merging, if anything.
use super::*;
use crate::open_pr_delivery::{OPEN_PR_DELIVERY_PREFIX, OpenPrDelivery, PrReadiness};

/// The comment Alondra closes the PR of a task the user cancelled with.
pub fn cancelled_pr_comment(title: &str) -> String {
    format!("Cancelled in Alondra: {title}")
}

impl DomainState {
    /// The first of the task's pull requests still open on GitHub.
    pub fn open_pr_of(&self, task: &TaskId) -> Option<&PullRequest> {
        self.open_prs(self.tasks.get(task)?).next()
    }

    /// Ends `task` for `actor`, who closes it without a delivery. With its PR open, the user's
    /// cancel also closes the PR on GitHub, and anyone else's leaves the task for Alondra to read
    /// the PR and deliver it.
    pub(in crate::domain) fn close_task(&mut self, actor: &Actor, task: &TaskId, result: Option<String>,
        effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<()> {
        if self.open_pr_of(task).is_none() {
            return self.cancel(task, effects, lifecycles);
        }
        if matches!(actor, Actor::User { .. }) {
            let t = self.tasks.get_mut(task).expect("the PR's task exists");
            t.open_pr = Some(OpenPrPending::Close { comment: cancelled_pr_comment(&t.title) });
            return self.cancel(task, effects, lifecycles);
        }
        self.delivered_pr_stays(actor, task, self.lifecycle(lifecycles, task)?.phase())?;
        let t = self.tasks.get_mut(task).expect("the PR's task exists");
        t.open_pr = Some(OpenPrPending::Deliver { result });
        self.invalidate(task);
        Ok(())
    }

    /// A PR the task delivered waits for the user's review: only the user accepts or cancels it.
    pub(in crate::domain) fn delivered_pr_stays(&self, actor: &Actor, task: &TaskId, phase: alondra_domain::TaskPhase) -> Result<()> {
        let Some(pr) = self.open_pr_of(task).filter(|_| !matches!(actor, Actor::User { .. }) && phase == alondra_domain::TaskPhase::Delivered) else {
            return Ok(());
        };
        refuse("open_pr_delivered", "task_id", &format!(
            "Task {task} delivered its pull request #{}, which waits for the user's review: only the user accepts or cancels it", pr.number))
    }

    /// What GitHub reports of the PR of a task closed while its PR was open settles it.
    pub(in crate::domain) fn settle_open_pr(&mut self, actor: &Actor, task: &TaskId, status: &OpenPrStatus,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        system(actor)?;
        head(&status.head)?;
        nonempty(&status.base)?;
        let Some(OpenPrPending::Deliver { result }) = self.task(task)?.open_pr.clone() else {
            return refuse("open_pr_settled", "task_id", &format!("Task {task} has no open pull request left to deliver"));
        };
        let pr = self.open_pr_of(task).or_else(|| self.task_prs(&self.tasks[task]).last()).cloned()
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
        self.insert_interaction(actor, &id, &Scope::Task { task_id: task.clone() }, &Recipient::User, &card.to_body(), false, &[],
            InteractionPayload::Delivery { targets, is_final: true, acceptance: None, feedback: vec![] }, &spec, Source::Delivery)?;
        self.invalidate(task);
        self.decide(lifecycles, task, LifecycleRecord::Presented { delivery: id.clone() })?;
        let t = self.tasks.get_mut(task).expect("found above");
        t.final_presentation = Some(id.clone());
        t.verdict = None;
        created.push(id.0);
        Ok(())
    }
}
