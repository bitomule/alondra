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
    pub fn close_task(&mut self, actor: &Actor, task: &TaskId, result: Option<String>,
        effects: &mut Vec<DomainEffect>, created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        let explained = result.is_some() && !self.task(task)?.pending_user_inputs.is_empty();
        if !matches!(actor, Actor::User { .. }) && !self.task(task)?.pending_user_inputs.is_empty() && !explained {
            return fail("pending_user_message", "A user message still needs a final delivery before the task can be cancelled");
        }
        if matches!(actor, Actor::User { .. }) || explained {
            self.tasks.get_mut(task).unwrap().pending_user_inputs.clear();
        }
        self.tasks.get_mut(task).unwrap().paused_in_queue = None;
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
    pub fn delivered_pr_stays(&self, actor: &Actor, task: &TaskId, phase: alondra_domain::TaskPhase) -> Result<()> {
        let Some(pr) = self.open_pr_of(task).filter(|_| !matches!(actor, Actor::User { .. }) && phase == alondra_domain::TaskPhase::Delivered) else {
            return Ok(());
        };
        refuse("open_pr_delivered", "task_id", &format!(
            "Task {task} delivered its pull request #{}, which waits for the user's review: only the user accepts or cancels it", pr.number))
    }



    /// Alondra's delivery of the open `pr` of `task` to the user, as `status` reports it.
    pub fn deliver_open_pr(&mut self, task: &TaskId, pr: &PullRequest, status: &OpenPrStatus, result: Option<String>,
        created: &mut Vec<String>, lifecycles: &mut Lifecycles) -> Result<()> {
        let t = &self.tasks[task];
        let card = OpenPrDelivery { title: t.title.clone(), repository: pr.repository.clone(), number: pr.number,
            readiness: PrReadiness::of(status), result };
        let (need, why) = card.text(alondra_core::text::Lang::En);
        let id = InteractionId(format!("{OPEN_PR_DELIVERY_PREFIX}{}-{}", pr.slug(), self.revision + 1));
        let kind = if status.state == PrState::Open { CardKind::Review } else { CardKind::Know };
        let content = CardContent { kind: ContentKind::Report, summary: None, body: card.detail(alondra_core::text::Lang::En) };
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
