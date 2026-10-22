//! Whether Claude can work for the tasks that run through the Agent SDK. A turn a Claude API error
//! ended holds its task on a pause of its lifecycle, on a card of its own as every wait is, until
//! something outside the task changes: the user signs in to Claude again, the API answers, or the
//! usage limit resets. A sign-in and a usage limit belong to the provider account the task runs
//! on; the API is the same for every account. Waiting for You shows the cards of one hold (of one
//! account) as a single card: the oldest, and none while a probe runs. Only facts release them:
//! the model answering an SDK task, which lets go on the tasks its account's sign-in or limit
//! held and every task the API held, or the one held task the user's answer or the daemon's
//! schedule sent on as a probe.
use super::*;
use crate::accounts::{AccountId, AccountState};
use alondra_domain::WaitReason as LifecycleWait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The card of a task Claude's expired sign-in holds.
pub const SIGNED_OUT_PREFIX: &str = "claude-signed-out-";
/// The card of a task the failing API holds.
pub const OUTAGE_PREFIX: &str = "claude-outage-";
/// The card of a task the usage limit holds; with its reset, in Unix seconds, after `USAGE_UNTIL`.
pub const USAGE_PREFIX: &str = "claude-usage-limit-";
const USAGE_UNTIL: &str = "claude-usage-limit-until-";
/// The answers that send the task on as the probe of whether Claude works again.
pub const SIGNED_IN: &str = "signed_in";
pub const SIGNED_IN_LABEL: &str = "I signed in";
pub const TRY_NOW: &str = "try_now";
pub const TRY_NOW_LABEL: &str = "Try now";
const SIGNED_IN_MEANS: &str = "One waiting task goes on; when Claude answers it, the rest of its account go on too";
const TRY_NOW_MEANS: &str = "One waiting task tries again now; when Claude answers it, the rest go on too";
/// What a task Claude held is told when it goes on, on its account or on the one it moved to.
pub const GO_ON: &str ="Claude answers again: go on with your task from where it stopped.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaudeHold {
    SignedOut,
    Outage,
    Usage,
}

impl ClaudeHold {
    pub fn for_failure(kind: SdkFailureKind) -> Option<Self> {
        match kind {
            SdkFailureKind::SignedOut => Some(Self::SignedOut),
            SdkFailureKind::Outage => Some(Self::Outage),
            SdkFailureKind::Usage => Some(Self::Usage),
            SdkFailureKind::NeedsUser => None,
        }
    }
    pub fn of(card: &InteractionId) -> Option<Self> {
        let id = card.0.as_str();
        if id.starts_with(SIGNED_OUT_PREFIX) {
            Some(Self::SignedOut)
        } else if id.starts_with(OUTAGE_PREFIX) {
            Some(Self::Outage)
        } else if id.starts_with(USAGE_PREFIX) {
            Some(Self::Usage)
        } else {
            None
        }
    }
    fn option(self) -> QuestionOption {
        let (id, label, means) = match self {
            Self::SignedOut => (SIGNED_IN, SIGNED_IN_LABEL, SIGNED_IN_MEANS),
            Self::Outage | Self::Usage => (TRY_NOW, TRY_NOW_LABEL, TRY_NOW_MEANS),
        };
        QuestionOption { id: id.into(), label: label.into(), description: Some(means.into()) }
    }
    fn need(self) -> &'static str {
        match self {
            Self::SignedOut => "Sign in to Claude outside Alondra",
            Self::Outage => "Claude's API is not answering: tasks wait for it",
            Self::Usage => "Claude's usage limit is reached: tasks wait for it",
        }
    }
    fn why(self) -> &'static str {
        match self {
            Self::SignedOut => "Claude Code's sign-in expired, so the tasks that run through it wait. Open Terminal, run claude and sign in with /login, then press I signed in.",
            Self::Outage => "The Claude API kept failing after Claude Code's own retries. Alondra tries one task again every 2 minutes; when Claude answers, every waiting task goes on.",
            Self::Usage => "The tasks that run through Claude Code wait until the limit resets. Alondra tries one then; when Claude answers, every waiting task goes on.",
        }
    }
}

/// When the usage limit a card waits for resets, in Unix seconds, when Claude said.
pub fn usage_resets_at(card: &InteractionId) -> Option<u64> {
    card.0.strip_prefix(USAGE_UNTIL)?.split('-').next()?.parse().ok()
}

/// A task Claude holds, the card it waits on and the account it runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeHeld {
    pub task: TaskId,
    pub hold: ClaudeHold,
    pub card: InteractionId,
    pub account: AccountId,
}

impl ClaudeHeld {
    /// What the hold waits for: the API, for every account; a sign-in or a limit, of its account.
    pub fn waits_for(&self) -> (ClaudeHold, Option<AccountId>) {
        (self.hold, self.hold.of_account(&self.account))
    }
}

impl ClaudeHold {
    /// The account a hold of a task on `account` waits for; none for the API, which is shared.
    pub fn of_account(self, account: &AccountId) -> Option<AccountId> {
        (self != Self::Outage).then(|| account.clone())
    }
}

/// A task's last pause (true) or resume (false) on a Claude hold, and whether a turn ended after it.
fn last_hold(journal: &Journal) -> Option<(&InteractionId, bool, bool)> {
    let mut ended_since = false;
    for record in journal.iter().rev() {
        match record {
            LifecycleRecord::TurnEnded { .. } => ended_since = true,
            LifecycleRecord::Paused { card } if ClaudeHold::of(card).is_some() => return Some((card, true, ended_since)),
            LifecycleRecord::Resumed { card, .. } if ClaudeHold::of(card).is_some() => return Some((card, false, ended_since)),
            _ => {}
        }
    }
    None
}

impl DomainState {
    /// The account the task's current execution runs on.
    pub fn task_account(&self, task: &TaskId) -> AccountId {
        self.tasks.get(task).and_then(|task| task.account.clone()).unwrap_or_else(AccountId::default_account)
    }

    /// The open tasks Claude holds, by task id. A task's last pause on a Claude hold names it; its
    /// lifecycle confirms it still waits on that pause.
    pub fn claude_held(&self) -> Vec<ClaudeHeld> {
        let mut held = vec![];
        for (task, journal) in &self.lifecycle_journal {
            if self.tasks.get(task).is_none_or(Task::over) {
                continue;
            }
            let Some((card, true, _)) = last_hold(journal) else { continue };
            let Some(hold) = ClaudeHold::of(card) else { continue };
            let waits = self.lifecycle_of(task).is_ok_and(|lifecycle| {
                lifecycle.cards().iter().any(|(id, reason)| *id == card && *reason == LifecycleWait::Pause)
            });
            if waits {
                held.push(ClaudeHeld { task: task.clone(), hold, card: card.clone(), account: self.task_account(task) });
            }
        }
        held
    }

    /// What a hold's card waits for: its hold, and the account of its task unless it waits for the API.
    pub fn claude_card_waits_for(&self, card: &Interaction) -> Option<(ClaudeHold, Option<AccountId>)> {
        let hold = ClaudeHold::of(&card.id)?;
        let account = match &card.scope {
            Scope::Task { task_id } => self.task_account(task_id),
            _ => AccountId::default_account(),
        };
        Some((hold, hold.of_account(&account)))
    }

    /// The holds a probe runs for now: a task sent on from one whose next turn has not ended.
    fn probing(&self) -> BTreeSet<(ClaudeHold, Option<AccountId>)> {
        self.lifecycle_journal
            .iter()
            .filter(|(task, _)| self.tasks.get(*task).is_some_and(|t| !t.over()))
            .filter_map(|(task, journal)| match last_hold(journal) {
                Some((card, false, false)) => ClaudeHold::of(card).map(|hold| (hold, hold.of_account(&self.task_account(task)))),
                _ => None,
            })
            .collect()
    }

    /// The cards of Claude's holds Waiting for You shows: the oldest open one of each hold of each
    /// account, and none of one a probe runs for. The others wait in the queue, out of sight.
    pub fn shown_claude_cards(&self) -> BTreeSet<InteractionId> {
        let probing = self.probing();
        let mut oldest: BTreeMap<(ClaudeHold, Option<AccountId>), &Interaction> = BTreeMap::new();
        for card in self.interactions.values().filter(|i| i.author == Author::System && self.open(i)) {
            let Some(waits_for) = self.claude_card_waits_for(card) else { continue };
            if probing.contains(&waits_for) {
                continue;
            }
            let first = oldest.entry(waits_for).or_insert(card);
            if card.order < first.order {
                *first = card;
            }
        }
        oldest.into_values().map(|card| card.id.clone()).collect()
    }

    /// A card of Claude's holds that Waiting for You leaves out: another of its hold stands for it.
    pub(super) fn grouped_away(&self, i: &Interaction) -> bool {
        i.author == Author::System && ClaudeHold::of(&i.id).is_some() && !self.shown_claude_cards().contains(&i.id)
    }

    /// How many open cards wait for what `card` waits for: the tasks held with it.
    pub fn claude_waiting(&self, card: &Interaction) -> usize {
        let Some(waits_for) = self.claude_card_waits_for(card) else { return 0 };
        self.interactions
            .values()
            .filter(|i| i.author == Author::System && self.open(i) && self.claude_card_waits_for(i).as_ref() == Some(&waits_for))
            .count()
    }

    /// The task waits on `hold`, on a card of its own, before its failed turn ends. An expired
    /// sign-in leaves its account signed out, so no new work starts on it. The user hears of a
    /// sign-in only when no other task of its account waits for one; of the API and the limit, never.
    pub(super) fn hold_on_claude(&mut self, task: &TaskId, hold: ClaudeHold, resets_at: Option<u64>, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        let revision = self.revision + 1;
        let id = InteractionId(match (hold, resets_at) {
            (ClaudeHold::SignedOut, _) => format!("{SIGNED_OUT_PREFIX}{task}-{revision}"),
            (ClaudeHold::Outage, _) => format!("{OUTAGE_PREFIX}{task}-{revision}"),
            (ClaudeHold::Usage, Some(at)) => format!("{USAGE_UNTIL}{at}-{task}-{revision}"),
            (ClaudeHold::Usage, None) => format!("{USAGE_PREFIX}{task}-{revision}"),
        });
        let account = self.task_account(task);
        let announce = hold == ClaudeHold::SignedOut
            && !self.claude_held().iter().any(|held| held.waits_for() == (hold, Some(account.clone())));
        if hold == ClaudeHold::SignedOut {
            self.account_signed_out(&account);
        }
        self.interactions.insert(id.clone(), hold_card(id.clone(), task, hold, revision));
        if announce {
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: id.clone() });
        }
        self.decide(lifecycles, task, LifecycleRecord::Paused { card: id })
    }

    /// The daemon's schedule: the task of the oldest card of `hold` goes on, to probe whether
    /// Claude works again; for a limit, of `account` when it names one. Its card is withdrawn:
    /// nobody answered it.
    pub(super) fn probe_claude(&mut self, hold: ClaudeHold, account: Option<&AccountId>, lifecycles: &mut Lifecycles) -> Result<Option<TaskId>, DomainError> {
        let held = self.claude_held().into_iter().filter(|held| held.hold == hold && account.is_none_or(|account| held.account == *account));
        let Some(probe) = held.min_by_key(|held| self.interactions.get(&held.card).map(|i| i.order)) else {
            return Ok(None);
        };
        self.withdraw_hold(&probe.card);
        self.resume_held(&probe, lifecycles)?;
        Ok(Some(probe.task))
    }

    /// The model answered an SDK task on `account`: the tasks its account's sign-in or limit held
    /// and those the API held go on, each card withdrawn. Tasks held by another account's sign-in
    /// or limit wait on: this answer says nothing of it. An account Alondra saw signed out is
    /// signed in again, with its credential's next generation.
    pub(super) fn claude_answered(&mut self, account: &AccountId, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        for held in self.claude_held() {
            if held.hold == ClaudeHold::Outage || held.account == *account {
                self.withdraw_hold(&held.card);
                self.resume_held(&held, lifecycles)?;
            }
        }
        if let Some(status) = self.accounts.iter_mut().find(|status| status.id == *account && status.state == AccountState::SignedOut) {
            status.state = AccountState::SignedIn;
            status.sign_ins += 1;
        }
        Ok(())
    }

    /// The user answered a hold's card: its task goes on as the probe. False for any other card.
    pub(super) fn settle_claude_card(&mut self, card: &InteractionId, lifecycles: &mut Lifecycles) -> Result<bool, DomainError> {
        if ClaudeHold::of(card).is_none() {
            return Ok(false);
        }
        if let Some(held) = self.claude_held().into_iter().find(|held| held.card == *card) {
            self.resume_held(&held, lifecycles)?;
        }
        Ok(true)
    }

    /// The task's hold on its account's usage limit, when its session can go on with another
    /// account now: its turn ended on the limit, nothing else holds it and it runs through the
    /// SDK. The daemon moves only such a task (`move_when_exhausted`), and the admission that
    /// moves it is judged again.
    pub fn held_by_limit(&self, task: &TaskId) -> Option<ClaudeHeld> {
        let held = self.claude_held().into_iter().find(|held| held.task == *task && held.hold == ClaudeHold::Usage)?;
        let node = self.tasks.get(task)?.agent_id.clone()?;
        let mut lifecycle = self.lifecycle_of(task).ok()?;
        if lifecycle.execution()?.host().as_str() == alondra_domain::CLAUDE_BG_HOST {
            return None;
        }
        let placeholder = String::from("account-move");
        let identity = ExecutionIdentity { conversation: placeholder.clone(), instance: placeholder.clone(), host: placeholder };
        let records = [
            LifecycleRecord::Resumed { card: held.card.clone(), input: self.next_input(task), text: None },
            LifecycleRecord::ExecutionReleased { input: self.next_input(task) },
            LifecycleRecord::Admitted { node, execution: Some(identity) },
        ];
        for record in &records {
            self.judge(task, &mut lifecycle, record).ok()?;
        }
        Some(held)
    }

    /// The execution admitted for `task` on `to` leaves the usage limit its account holds it on
    /// with `card`: the card is withdrawn and the task goes on with an input of its own, whose
    /// words are the ones the new execution is launched with.
    pub(super) fn leave_limit(&mut self, task: &TaskId, card: &InteractionId, to: &AccountId, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        let Some(held) = self.claude_held().into_iter().find(|held| held.task == *task && held.card == *card && held.hold == ClaudeHold::Usage) else {
            return Err(DomainError::new("conflict", "The task no longer waits on that usage limit"));
        };
        if held.account == *to {
            return Err(DomainError::new("invalid_input", "A task leaves its account's usage limit only for another account"));
        }
        match self.accounts.iter().find(|status| status.id == *to) {
            Some(status) if matches!(status.state, AccountState::Disabled | AccountState::SignedOut) || status.billing == crate::accounts::AccountBilling::PayPerUse => {
                return Err(DomainError::new("conflict", "That account cannot take the task's work now"));
            }
            None if !to.is_default() || !self.accounts.is_empty() => {
                return Err(DomainError::new("not_found", "That account is not one of the accounts Alondra knows"));
            }
            _ => {}
        }
        self.withdraw_hold(&held.card);
        let record = LifecycleRecord::Resumed { card: held.card.clone(), input: self.next_input(task), text: None };
        self.decide(lifecycles, task, record)
    }

    fn account_signed_out(&mut self, account: &AccountId) {
        if let Some(status) = self.accounts.iter_mut().find(|status| status.id == *account && status.state != AccountState::Disabled) {
            status.state = AccountState::SignedOut;
        }
    }

    /// The accounts the daemon saw, as the domain keeps them: an account whose expired sign-in
    /// holds a task stays signed out until Claude answers on it, whatever its sign-in check says,
    /// and one seen signed in again after it was signed out counts its credential's next generation.
    pub(super) fn record_accounts(&mut self, seen: &[crate::accounts::AccountStatus]) {
        let held: BTreeSet<AccountId> = self.claude_held().into_iter().filter(|held| held.hold == ClaudeHold::SignedOut).map(|held| held.account).collect();
        let before = std::mem::take(&mut self.accounts);
        self.accounts = seen
            .iter()
            .map(|status| {
                let mut status = status.clone();
                let prior = before.iter().find(|prior| prior.provider == status.provider && prior.id == status.id);
                status.sign_ins = prior.map_or(0, |prior| prior.sign_ins);
                let was_out = prior.is_some_and(|prior| prior.state == AccountState::SignedOut);
                if status.state == AccountState::SignedIn && held.contains(&status.id) {
                    status.state = AccountState::SignedOut;
                } else if status.state == AccountState::SignedIn && was_out {
                    status.sign_ins += 1;
                }
                status
            })
            .collect();
    }

    fn withdraw_hold(&mut self, card: &InteractionId) {
        let revision = self.revision + 1;
        if let Some(card) = self.interactions.get_mut(card) {
            card.withdrawn = true;
            card.revision = revision;
        }
    }

    fn resume_held(&mut self, held: &ClaudeHeld, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        let record = LifecycleRecord::Resumed { card: held.card.clone(), input: self.next_input(&held.task), text: Some(GO_ON.into()) };
        self.decide(lifecycles, &held.task, record)
    }
}

fn hold_card(id: InteractionId, task: &TaskId, hold: ClaudeHold, revision: u64) -> Interaction {
    let mut card = CardSpec::new(CardKind::Problem, hold.need()).why(hold.why());
    let option = hold.option();
    card.recommended = Some(option.id.clone());
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: hold.need().into(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question { options: vec![option], answer: None },
        revision,
        order: revision,
        withdrawn: false,
        card,
        clarification: None,
    }
}
