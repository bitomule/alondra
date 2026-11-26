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
use alondra_core::accounts::{AccountId, AccountKey, AccountState, Provider};
use alondra_domain::WaitReason as LifecycleWait;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The card of a task Claude's expired sign-in holds.
pub const SIGNED_OUT_PREFIX: &str = "claude-signed-out-";
/// The card of a task the failing API holds.
pub const OUTAGE_PREFIX: &str = alondra_domain::CLAUDE_OUTAGE_CARD_PREFIX;
/// The card of a task the usage limit holds; with its reset, in Unix seconds, after `USAGE_UNTIL`.
pub const USAGE_PREFIX: &str = alondra_domain::CLAUDE_USAGE_CARD_PREFIX;
pub(in crate) const USAGE_UNTIL: &str = "claude-usage-limit-until-";
/// The answers that send the task on as the probe of whether Claude works again.
pub const SIGNED_IN: &str = "signed_in";
pub const SIGNED_IN_LABEL: &str = "I signed in";
pub const TRY_NOW: &str = "try_now";
pub const TRY_NOW_LABEL: &str = "Try now";
const SIGNED_IN_MEANS: &str = "One waiting task goes on; when Claude answers it, the rest of its account go on too";
const TRY_NOW_MEANS: &str = "One waiting task tries again now; when Claude answers it, the rest go on too";
/// What a task Claude's sign-in or API held is told when it goes on.
pub const GO_ON: &str ="Claude answers again: go on with your task from where it stopped.";
macro_rules! alondra_waits {
    () => { "Alondra waits out usage limits for you: never wait for one with sleep, cron or a background loop; end your turn and Alondra wakes you when Claude answers again." };
}
/// What a task the usage limit held is told when Claude answers it again.
pub const USAGE_RENEWED: &str = concat!("Your usage limit has renewed: Claude answers again. Go on with your task now, from where it stopped. ", alondra_waits!());
/// What a task the usage limit held is told when it goes on on another account.
pub const MOVED_ON: &str = concat!("Your task now runs on another Claude account that has usage left: Claude answers again. Go on with your task now, from where it stopped. ", alondra_waits!());

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
    /// Who a task this hold pauses waits on: the user signs in; Alondra waits out the API and the limit.
    pub fn owner(self) -> alondra_domain::Owner {
        use alondra_domain::{Capacity, Owner};
        match self {
            Self::SignedOut => Owner::User,
            Self::Outage => Owner::Capacity(Capacity::ClaudeApi),
            Self::Usage => Owner::Capacity(Capacity::Usage),
        }
    }
    pub fn of(card: &Interaction) -> Option<Self> {
        match card.system_card.as_ref()? {
            SystemCard::ClaudeSignedOut => Some(Self::SignedOut),
            SystemCard::ClaudeOutage { .. } => Some(Self::Outage),
            SystemCard::ClaudeUsage { .. } => Some(Self::Usage),
            _ => None,
        }
    }
    /// The card of this hold after `failure`; one of the API is `quiet` until Alondra retried.
    pub fn system_card(self, failure: &SdkApiFailure, quiet: bool) -> SystemCard {
        match self {
            Self::SignedOut => SystemCard::ClaudeSignedOut,
            Self::Outage => SystemCard::ClaudeOutage { quiet, detail: failure.detail.clone(), said: failure.said.clone() },
            Self::Usage => SystemCard::ClaudeUsage { resets_at: failure.resets_at },
        }
    }
    /// What the task is told when Claude answers it again.
    pub fn go_on(self) -> &'static str {
        match self {
            Self::SignedOut | Self::Outage => GO_ON,
            Self::Usage => USAGE_RENEWED,
        }
    }
    pub fn option(self) -> QuestionOption {
        let (id, label, means) = match self {
            Self::SignedOut => (SIGNED_IN, SIGNED_IN_LABEL, SIGNED_IN_MEANS),
            Self::Outage | Self::Usage => (TRY_NOW, TRY_NOW_LABEL, TRY_NOW_MEANS),
        };
        QuestionOption { id: id.into(), label: label.into(), description: Some(means.into()) }
    }
    pub fn need(self) -> &'static str {
        match self {
            Self::SignedOut => "Sign in to Claude outside Alondra",
            Self::Outage => "Claude's API is not answering: tasks wait for it",
            Self::Usage => "Claude's usage limit is reached: tasks wait for it",
        }
    }
    pub fn why(self) -> &'static str {
        match self {
            Self::SignedOut => "Claude Code's sign-in expired, so the tasks that run through it wait. Open Terminal, run claude and sign in with /login, then press I signed in.",
            Self::Outage => "The Claude API kept failing after Claude Code's own retries. Alondra tries one task again every 2 minutes; when Claude answers, every waiting task goes on.",
            Self::Usage => "The tasks that run through Claude Code wait until the limit resets. Alondra tries one then; when Claude answers, every waiting task goes on.",
        }
    }
}


/// A task Claude holds, the card it waits on and the account it runs on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeHeld {
    pub provider: Provider,
    pub task: TaskId,
    pub hold: ClaudeHold,
    pub card: InteractionId,
    pub account: AccountId,
    /// When the usage limit resets, in Unix seconds, when Claude said.
    pub resets_at: Option<u64>,
}

impl ClaudeHeld {
    /// What the hold waits for: the API, for every account; a sign-in or a limit, of its account.
    pub fn waits_for(&self) -> (Provider, ClaudeHold, Option<AccountId>) {
        (self.provider, self.hold, self.hold.of_account(&self.account))
    }
}

impl ClaudeHold {
    /// The account a hold of a task on `account` waits for; none for the API, which is shared.
    pub fn of_account(self, account: &AccountId) -> Option<AccountId> {
        (self != Self::Outage).then(|| account.clone())
    }
}

/// A task's last pause (true) or resume (false) on a Claude hold, and whether a turn ended after it.
fn last_hold<'a>(journal: &'a Journal, cards: &BTreeMap<InteractionId, Interaction>) -> Option<(&'a InteractionId, bool, bool)> {
    let held = |card: &InteractionId| cards.get(card).and_then(ClaudeHold::of).is_some();
    let mut ended_since = false;
    for record in journal.iter().rev() {
        match record {
            LifecycleRecord::TurnEnded { .. } => ended_since = true,
            LifecycleRecord::Paused { card, .. } if held(card) => return Some((card, true, ended_since)),
            LifecycleRecord::Resumed { card, .. } if held(card) => return Some((card, false, ended_since)),
            _ => {}
        }
    }
    None
}

impl DomainState {
    /// The Claude hold `card` stands for, when it is one of Alondra's hold cards.
    pub fn claude_hold(&self, card: &InteractionId) -> Option<ClaudeHold> {
        self.interactions.get(card).and_then(ClaudeHold::of)
    }

    /// Whether `task` fails on its own: it went on from waiting out the API, its turn failed again
    /// before ending, and no other task of its provider still waits for the API.
    pub fn refused_alone(&self, task: &TaskId) -> bool {
        let Some(journal) = self.lifecycle_journal.get(task) else { return false };
        let back_from_outage = matches!(last_hold(journal, &self.interactions), Some((card, false, false)) if self.claude_hold(card) == Some(ClaudeHold::Outage));
        let provider = self.task_account_key(task).provider;
        back_from_outage && !self.claude_held().iter().any(|held| held.task != *task && held.provider == provider && held.hold == ClaudeHold::Outage)
    }

    /// Whether `task` went on from waiting out the API, as Alondra's retry or the user's, and its
    /// turn has not ended since: a failure now is the API failing again after a retry.
    pub fn retried_on_outage(&self, task: &TaskId) -> bool {
        self.lifecycle_journal.get(task).is_some_and(|journal| {
            matches!(last_hold(journal, &self.interactions), Some((card, false, false)) if self.claude_hold(card) == Some(ClaudeHold::Outage))
        })
    }

    /// When the usage limit `card` waits for resets, in Unix seconds, when Claude said.
    pub fn usage_resets_at(&self, card: &InteractionId) -> Option<u64> {
        match self.interactions.get(card)?.system_card {
            Some(SystemCard::ClaudeUsage { resets_at }) => resets_at,
            _ => None,
        }
    }

    /// The account the task's current execution runs on.
    pub fn task_account(&self, task: &TaskId) -> AccountId {
        self.tasks.get(task).and_then(|task| task.account.clone()).unwrap_or_else(AccountId::default_account)
    }

    pub fn task_account_key(&self, task: &TaskId) -> AccountKey {
        self.tasks.get(task).map(Task::account_key).unwrap_or_else(|| AccountKey::native(Provider::Claude))
    }

    /// The open tasks Claude holds, by task id. A task's last pause on a Claude hold names it; its
    /// lifecycle confirms it still waits on that pause.
    pub fn current_hold_credentials(&self, card: &Interaction) -> bool {
        let Scope::Task { task_id } = &card.scope else { return card.card.cause.is_none() };
        let key = self.task_account_key(task_id);
        if key.provider == Provider::Codex
            && let Some(selection) = self.tasks.get(task_id).and_then(|task| task.selection.as_ref())
        {
            let Some(current) = self.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account) else { return false; };
            // A spending grant does not identify a native API credential shared with another session.
            if selection.paid_account_grant.is_some() || matches!(current.billing, alondra_core::accounts::AccountBilling::PayPerUse) { return false; }
        }
        let generation = self.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account).map_or(1, |account| account.sign_ins + 1);
        card.card.cause.as_ref().is_none_or(|cause| *cause == credential_cause(&key, generation))
    }

    pub fn claude_held(&self) -> Vec<ClaudeHeld> {
        let mut held = vec![];
        for (task, journal) in &self.lifecycle_journal {
            if self.tasks.get(task).is_none_or(Task::over) {
                continue;
            }
            let Some((card, true, _)) = last_hold(journal, &self.interactions) else { continue };
            let Some(hold) = self.claude_hold(card) else { continue };
            let waits = self.lifecycle_of(task).is_ok_and(|lifecycle| {
                lifecycle.cards().iter().any(|(id, reason)| *id == card && *reason == LifecycleWait::Pause)
            });
            if waits && self.interactions.get(card).is_none_or(|card| self.current_hold_credentials(card)) {
                held.push(ClaudeHeld { provider: self.task_account_key(task).provider, task: task.clone(), hold, card: card.clone(), account: self.task_account(task), resets_at: self.usage_resets_at(card) });
            }
        }
        held
    }

    /// When the usage limit that holds each task comes back, in milliseconds: what the hold's
    /// provider said, else when its account's windows that ran out reset.
    pub fn usage_resets_by_task(&self, now: alondra_core::Millis) -> BTreeMap<TaskId, alondra_core::Millis> {
        self.claude_held().into_iter()
            .filter(|held| held.hold == ClaudeHold::Usage)
            .filter_map(|held| {
                let said = held.resets_at.and_then(|seconds| alondra_core::Millis::try_from(seconds).ok()?.checked_mul(1000));
                let account = || self.account_usage.iter().find(|usage| usage.provider == held.provider && usage.account == held.account)?.plan_exhausted_until(now);
                Some((held.task, said.or_else(account)?))
            })
            .collect()
    }

    /// What a hold's card waits for: its hold, and the account of its task unless it waits for the API.
    pub fn claude_card_waits_for(&self, card: &Interaction) -> Option<(ClaudeHold, Option<AccountId>)> {
        let hold = ClaudeHold::of(card)?;
        if !self.current_hold_credentials(card) { return None; }
        let account = match &card.scope {
            Scope::Task { task_id } => self.task_account(task_id),
            _ => AccountId::default_account(),
        };
        Some((hold, hold.of_account(&account)))
    }

    pub fn provider_card_waits_for(&self, card: &Interaction) -> Option<(Provider, ClaudeHold, Option<AccountId>)> {
        let (hold, account) = self.claude_card_waits_for(card)?;
        let provider = match &card.scope { Scope::Task { task_id } => self.task_account_key(task_id).provider, _ => Provider::Claude };
        Some((provider,hold,account))
    }

    /// The holds a probe runs for now: a task sent on from one whose next turn has not ended.
    pub fn probing(&self) -> BTreeSet<(Provider, ClaudeHold, Option<AccountId>)> {
        self.lifecycle_journal
            .iter()
            .filter(|(task, _)| self.tasks.get(*task).is_some_and(|t| !t.over()))
            .filter_map(|(task, journal)| match last_hold(journal, &self.interactions) {
                Some((card, false, false)) if self.interactions.get(card).is_none_or(|card| self.current_hold_credentials(card)) => self.claude_hold(card).map(|hold| (self.task_account_key(task).provider, hold, hold.of_account(&self.task_account(task)))),
                _ => None,
            })
            .collect()
    }

    /// The cards of Claude's holds Waiting for You shows: the oldest open one of each hold of each
    /// account, and none of one a probe runs for. A quiet card of the API is never one: until
    /// Alondra's retry fails too, the user hears nothing of it. The others wait in the queue, out
    /// of sight.
    pub fn shown_claude_cards(&self) -> BTreeSet<InteractionId> {
        let probing = self.probing();
        let mut oldest: BTreeMap<(Provider, ClaudeHold, Option<AccountId>), &Interaction> = BTreeMap::new();
        for card in self.interactions.values().filter(|i| i.author == Author::System && self.open(i)) {
            let Some(waits_for) = self.provider_card_waits_for(card) else { continue };
            if probing.contains(&waits_for) || matches!(card.system_card, Some(SystemCard::ClaudeOutage { quiet: true, .. })) {
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
    pub fn grouped_away(&self, i: &Interaction) -> bool {
        i.author == Author::System && self.provider_card_waits_for(i).is_some() && !self.shown_claude_cards().contains(&i.id)
    }

    /// How many open cards wait for what `card` waits for: the tasks held with it.
    pub fn claude_waiting(&self, card: &Interaction) -> usize {
        let Some(waits_for) = self.provider_card_waits_for(card) else { return 0 };
        self.interactions
            .values()
            .filter(|i| i.author == Author::System && self.open(i) && self.provider_card_waits_for(i).as_ref() == Some(&waits_for))
            .count()
    }

    /// The task waits on `hold`, on a card of its own, before its failed turn ends. An expired
    /// sign-in leaves its account signed out, so no new work starts on it. The user hears of a
    /// sign-in only when no other task of its account waits for one; of the API and the limit, never.
    ///
    /// Claude goes on by itself while its task waits (a subagent hands back, background work
    /// ends), so a turn can end on Claude again while a pause already holds the task. That never
    /// refuses the end of the turn: the same hold stays as it is, a hold on the API gives way to
    /// the sign-in or the limit, and any other pause (the user's, the machine's) stands, so the
    /// next turn meets the hold again if it still holds. Nor does a lifecycle that cannot wait now.
    pub fn hold_on_claude(&mut self, task: &TaskId, hold: ClaudeHold, failure: &SdkApiFailure, effects: &mut Vec<DomainEffect>, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        let resets_at = failure.resets_at;
        let standing = self.lifecycle(lifecycles, task)?.cards().into_iter()
            .find(|(_, reason)| *reason == LifecycleWait::Pause)
            .map(|(card, _)| card.clone());
        let replaced = match &standing {
            None => None,
            Some(card) if self.claude_hold(card) == Some(ClaudeHold::Outage) && hold != ClaudeHold::Outage => Some(card.clone()),
            Some(_) => return Ok(()),
        };
        let revision = self.revision + 1;
        let id = InteractionId(match (hold, resets_at) {
            (ClaudeHold::SignedOut, _) => format!("{SIGNED_OUT_PREFIX}{task}-{revision}"),
            (ClaudeHold::Outage, _) => format!("{OUTAGE_PREFIX}{task}-{revision}"),
            (ClaudeHold::Usage, Some(at)) => format!("{USAGE_UNTIL}{at}-{task}-{revision}"),
            (ClaudeHold::Usage, None) => format!("{USAGE_PREFIX}{task}-{revision}"),
        });
        let paused = LifecycleRecord::Paused { card: id.clone(), owner: Some(hold.owner().into()) };
        let records: Vec<LifecycleRecord> = replaced.iter().map(|card| LifecycleRecord::PauseEnded { card: card.clone() }).chain([paused]).collect();
        let mut probe = self.lifecycle_of(task)?;
        if records.iter().try_for_each(|record| self.judge(task, &mut probe, record)).is_err() {
            return Ok(());
        }
        let key = self.task_account_key(task);
        let account = key.account.clone();
        let announce = hold == ClaudeHold::SignedOut
            && !self.claude_held().iter().any(|held| held.waits_for() == (key.provider, hold, Some(account.clone())));
        if hold == ClaudeHold::SignedOut {
            self.account_signed_out(&key);
        }
        if let Some(card) = &replaced {
            self.withdraw_hold(card);
        }
        let generation = self.accounts.iter().find(|account| account.provider == key.provider && account.id == key.account).map_or(1, |account| account.sign_ins + 1);
        let mut card = hold_card(id.clone(), task, key.provider, hold, hold.system_card(failure, key.provider == Provider::Claude && !self.retried_on_outage(task)), revision);
        card.card.cause = Some(credential_cause(&key, generation));
        self.insert_system_card(card)?;
        if announce {
            effects.push(DomainEffect::NotifyUser { task_id: Some(task.clone()), interaction_id: id });
        }
        records.into_iter().try_for_each(|record| self.decide(lifecycles, task, record))
    }


    /// The model answered an SDK task on `account`: the tasks its account's sign-in or limit held
    /// and those the API held go on, each card withdrawn. Tasks held by another account's sign-in
    /// or limit wait on: this answer says nothing of it. An account Alondra saw signed out is
    /// signed in again, with its credential's next generation.
    pub fn claude_answered(&mut self, source: &TaskId, account: &AccountKey, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        for held in self.claude_held() {
            if account.provider == Provider::Codex && held.task != *source { continue; }
            if held.provider == account.provider && (held.hold == ClaudeHold::Outage || held.account == account.account) {
                self.withdraw_hold(&held.card);
                self.resume_held(&held, None, lifecycles)?;
            }
        }
        if let Some(status) = self.accounts.iter_mut().find(|status| status.provider == account.provider && status.id == account.account && status.state == AccountState::SignedOut) {
            status.state = AccountState::SignedIn;
            status.sign_ins += 1;
            self.model_catalogs.retain(|catalog| catalog.account != *account);
        }
        Ok(())
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




    pub fn account_signed_out(&mut self, account: &AccountKey) {
        if let Some(status) = self.accounts.iter_mut().find(|status| status.provider == account.provider && status.id == account.account && status.state != AccountState::Disabled) {
            status.state = AccountState::SignedOut;
        }
    }


    pub fn withdraw_hold(&mut self, card: &InteractionId) {
        let revision = self.revision + 1;
        if let Some(card) = self.interactions.get_mut(card) {
            card.withdrawn = true;
            card.revision = revision;
        }
    }

    pub fn resume_held(&mut self, held: &ClaudeHeld, written: Option<&str>, lifecycles: &mut Lifecycles) -> Result<(), DomainError> {
        let mut text = String::from(if held.provider == Provider::Claude { held.hold.go_on() } else { "Continue your existing task. Alondra manages provider waits; do not poll, sleep or schedule a retry yourself." });
        if let Some(written) = written {
            text.push_str("\n\nThe user wrote: ");
            text.push_str(written);
        }
        let record = LifecycleRecord::Resumed { card: held.card.clone(), input: self.next_input(&held.task), text: Some(text) };
        self.decide(lifecycles, &held.task, record)
    }
}

pub fn credential_cause(key: &AccountKey, generation: u64) -> String {
    let cause = format!("provider-credential:{}:{}", key.wire_id(), generation);
    debug_assert!(cause.len() <= 128);
    cause
}

fn hold_card(id: InteractionId, task: &TaskId, provider: Provider, hold: ClaudeHold, system: SystemCard, revision: u64) -> Interaction {
    let (need, why) = hold_text(provider, hold);
    let mut card = CardSpec::new(CardKind::Problem, need).why(why);
    let option = hold.option();
    card.recommended = Some(option.id.clone());
    Interaction {
        attachments: vec![],
        id,
        scope: Scope::Task { task_id: task.clone() },
        author: Author::System,
        recipient: Recipient::User,
        body: need.into(),
        blocking: true,
        subject: None,
        payload: InteractionPayload::Question { options: vec![option], answer: None },
        revision,
        order: revision,
        withdrawn: false,
        card,
        clarification: None, snoozed_until: None, resurfaced_at_revision: None, system_card: Some(system), shortened: false,
    }
}

/// What the card of `hold` on `provider` asks and why.
pub(crate) fn hold_text(provider: Provider, hold: ClaudeHold) -> (&'static str, &'static str) {
    match (provider, hold) {
        (Provider::Claude, _) => (hold.need(),hold.why()),
        (Provider::Codex, ClaudeHold::SignedOut)=>("Sign in to Codex outside Alondra","The Codex sign-in needs attention. On the Mac, run codex login, then press I signed in."),
        (Provider::Codex, ClaudeHold::Outage)=>("Codex is not answering: tasks wait for it","Alondra checks native account metadata before continuing an existing waiting task. No model prompt is sent as a health check."),
        (Provider::Codex, ClaudeHold::Usage)=>("The Codex usage limit is reached: tasks wait for it","Alondra checks the native account quota and continues waiting tasks when fresh account-wide capacity is available."),
        (Provider::Antigravity, ClaudeHold::SignedOut)=>("Sign in to Antigravity outside Alondra","The Antigravity account is signed out or its key was refused. In Terminal on the Mac, run the sign-in command Accounts shows for it (agy acp --login for the default account), or set its API key, then press I signed in."),
        (Provider::Antigravity, ClaudeHold::Outage)=>("Antigravity is not answering: tasks wait for it","Alondra checks the account with agy acp before continuing a waiting task. No model prompt is sent as a health check."),
        (Provider::Antigravity, ClaudeHold::Usage)=>("The Antigravity usage limit is reached: tasks wait for it","Alondra reads the account's usage with agy and continues waiting tasks once a group of models has room again."),
    }
}
