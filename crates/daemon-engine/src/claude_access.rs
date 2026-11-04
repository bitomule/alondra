//! When to probe Claude for the tasks it holds through the Agent SDK. While the API fails, one
//! held task goes on now and then; while an account's usage limit stands, one of that account's
//! tasks goes on when its limit resets. Its next turn proves Claude works, and the tasks the same
//! wait held go on, or it holds again. The clock only says when to look. A sign-in is probed by
//! the user's answer to its card, never here.
use crate::domain_runtime::DomainRuntime;
use alondra_core::accounts::{AccountId, AccountKey, AccountState, AccountBilling, Provider};
use alondra_core::domain::{
    ClaudeHeld, ClaudeHold, Command, CommandEnvelope, Millis, OUTAGE_PROBE, PROTOCOL_VERSION, QUIET_RETRY,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// A wait the schedule probes: the API, or one account's usage limit.
type Wait = (Provider, ClaudeHold, Option<AccountId>);

/// Since when each wait has gone without a probe.
#[derive(Default)]
pub(crate) struct Schedule {
    since: BTreeMap<Wait, Millis>,
}

impl Schedule {
    /// The waits to probe at `now`, and when to look next. A wait no task is held on any more is
    /// forgotten: the next one waits a whole interval again.
    pub(crate) fn due(&mut self, held: &[ClaudeHeld], now: Millis) -> (Vec<Wait>, Option<Millis>) {
        let mut waits: BTreeMap<Wait, Vec<&ClaudeHeld>> = BTreeMap::new();
        for held in held.iter().filter(|held| held.hold != ClaudeHold::SignedOut) {
            waits.entry(held.waits_for()).or_default().push(held);
        }
        self.since.retain(|wait, _| waits.contains_key(wait));
        let mut due = vec![];
        let mut next: Option<Millis> = None;
        for (wait, waiting) in waits {
            let usage = wait.1 == ClaudeHold::Usage;
            let since = *self.since.entry(wait.clone()).or_insert(now);
            let at = if usage {
                waiting
                    .iter()
                    .filter_map(|held| held.resets_at)
                    .filter_map(|seconds| Millis::try_from(seconds).ok()?.checked_mul(1000))
                    .max()
                    .filter(|reset| *reset > since)
                    .unwrap_or(since + QUIET_RETRY)
            } else {
                since + OUTAGE_PROBE
            };
            let at = if at <= now {
                self.since.insert(wait.clone(), now);
                due.push(wait);
                now + if usage { QUIET_RETRY } else { OUTAGE_PROBE }
            } else {
                at
            };
            next = Some(next.map_or(at, |next| next.min(at)));
        }
        (due, next)
    }
}

pub async fn run(domain: Arc<DomainRuntime>) {
    let mut changes = domain.subscribe();
    let mut schedule = Schedule::default();
    loop {
        let now = alondra_core::now_millis();
        let held = domain.read().state().claude_held();
        let (due, next) = schedule.due(&held, now);
        for (provider, hold, account) in due {
            if provider == Provider::Claude { probe(&domain, hold, account); }
            else {
                let domain=domain.clone();
                if tokio::task::spawn_blocking(move || probe_for(&domain,provider,hold,account)).await.is_err() { log!("the provider metadata probe stopped"); }
            }
        }
        match next {
            Some(at) => tokio::select! {
                _ = changes.changed() => {}
                _ = tokio::time::sleep(Duration::from_millis(u64::try_from(at - now).unwrap_or(0))) => {}
            },
            None => {
                if changes.changed().await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Claude usage is back: each account whose usage limit holds tasks is probed now, whatever its
/// cards say of when it resets. A limit that still stands holds its probe again.
pub(crate) fn probe_usage_holds(domain: &DomainRuntime) {
    let waits: std::collections::BTreeSet<Wait> = domain.read().state().claude_held().into_iter()
        .filter(|held| held.hold == ClaudeHold::Usage)
        .map(|held| held.waits_for())
        .collect();
    for (provider, hold, account) in waits {
        probe_for(domain, provider, hold, account);
    }
}

pub(crate) fn probe(domain: &DomainRuntime, hold: ClaudeHold, account: Option<AccountId>) {
    probe_for(domain,Provider::Claude,hold,account)
}

pub(crate) fn native_capacity_ready(state: &alondra_core::domain::DomainState, key: &AccountKey, since: Millis) -> bool {
    if !state.accounts.iter().any(|account| account.provider==key.provider && account.id==key.account && account.state==AccountState::SignedIn && !matches!(account.billing,AccountBilling::PayPerUse)) { return false; }
    if !alondra_core::usage::eligible_account_keys(std::slice::from_ref(key),&state.accounts,&state.account_usage,since).contains(key) { return false; }
    native_capacity_for_existing(state, key, since)
}

/// Window evidence only: the current-task probe supplies native identity and original billing authority.
pub(crate) fn native_capacity_for_existing(state: &alondra_core::domain::DomainState, key: &AccountKey, since: Millis) -> bool {
    if key.provider != Provider::Codex { return false; }
    let Some(usage) = state.account_usage.iter().find(|usage| usage.provider == key.provider && usage.account == key.account) else { return false; };
    if usage.plan_exhausted_until(since).is_some() { return false; }
    let positive = |prefix: &str| usage.windows.iter().any(|window| window.measured_at >= since
        && matches!(&window.reading.kind, alondra_core::usage::WindowKind::Other(name) if name.starts_with(prefix))
        && !window.reading.exhausted && window.reading.used < alondra_core::usage::Share::FULL && window.reading.resets_at.is_none_or(|reset| reset > since));
    positive("codex:account:primary:") && positive("codex:account:secondary:")
}

pub(crate) fn probe_for(domain: &DomainRuntime, provider: Provider, hold: ClaudeHold, account: Option<AccountId>) {
    if provider == Provider::Codex {
        let read = domain.read();
        let candidate = read.state().claude_held().into_iter()
            .filter(|held| held.provider == provider && held.hold == hold && account.as_ref().is_none_or(|account| held.account == *account))
            .min_by_key(|held| read.state().interactions.get(&held.card).map(|card| card.order));
        let Some(candidate) = candidate else { return; };
        drop(read);
        match crate::accounts::probe_current_codex(domain, &candidate.task, Some(hold)) {
            Ok(true) => log!("continuing {} on its existing Codex account after its native metadata probe", candidate.task),
            Ok(false) => {},
            Err(why) => log!("the current Codex task's metadata probe could not recover it: {why}"),
        }
        return;
    }
    let state = domain.read();
    let named = account.as_ref().map_or_else(String::new, |account| format!("-{account}"));
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.state().data_epoch.clone(),
        command_id: format!("probe-{provider:?}-{hold:?}{named}-{}", state.state().revision).to_lowercase().as_str().into(),
        expected_revision: state.state().revision,
        command: Command::ProbeClaudeAccess { provider, hold, account: account.clone() },
    };
    match domain.execute_as_system(envelope) {
        Ok(_) => log!("continuing one task held on {provider:?} after its metadata probe ({hold:?}{named})"),
        Err(error) => log!("could not probe {provider:?} ({hold:?}{named}): {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{InteractionId, TaskId};

    fn held(task: &str, hold: ClaudeHold, card: &str) -> ClaudeHeld {
        on(task, hold, card, AccountId::DEFAULT)
    }

    fn on(task: &str, hold: ClaudeHold, card: &str, account: &str) -> ClaudeHeld {
        ClaudeHeld { provider: Provider::Claude, task: TaskId::from(task), hold, card: InteractionId::from(card), account: AccountId::parse(account).unwrap(), resets_at: None }
    }

    fn wait(hold: ClaudeHold, account: Option<&str>) -> Wait {
        (Provider::Claude,hold, account.map(|account| AccountId::parse(account).unwrap()))
    }

    #[test]
    fn an_outage_is_probed_once_per_interval_and_forgotten_when_nothing_waits() {
        let mut schedule = Schedule::default();
        let outage = [held("t", ClaudeHold::Outage, "claude-outage-t-3"), held("u", ClaudeHold::Outage, "claude-outage-u-4")];
        assert_eq!(schedule.due(&outage, 1_000), (vec![], Some(1_000 + OUTAGE_PROBE)));
        assert_eq!(schedule.due(&outage, 1_000 + OUTAGE_PROBE - 1), (vec![], Some(1_000 + OUTAGE_PROBE)));
        let at = 1_000 + OUTAGE_PROBE;
        assert_eq!(schedule.due(&outage, at), (vec![wait(ClaudeHold::Outage, None)], Some(at + OUTAGE_PROBE)));
        assert_eq!(schedule.due(&outage[1..], at + 1), (vec![], Some(at + OUTAGE_PROBE)), "one probe per interval");
        assert_eq!(schedule.due(&[], at + 2), (vec![], None));
        assert_eq!(schedule.due(&outage, at + 3), (vec![], Some(at + 3 + OUTAGE_PROBE)), "a new wait starts over");
    }

    #[test]
    fn the_usage_limit_is_probed_when_it_resets_or_quietly_when_unknown() {
        let mut schedule = Schedule::default();
        let reset = 4_102_444_800_u64;
        let known = [ClaudeHeld { resets_at: Some(reset), ..held("t", ClaudeHold::Usage, "claude-usage-limit-t-3") }];
        let at = Millis::try_from(reset * 1000).unwrap();
        assert_eq!(schedule.due(&known, 1_000), (vec![], Some(at)));
        assert_eq!(schedule.due(&known, at), (vec![wait(ClaudeHold::Usage, Some(AccountId::DEFAULT))], Some(at + QUIET_RETRY)));
        let mut schedule = Schedule::default();
        let unknown = [held("t", ClaudeHold::Usage, "claude-usage-limit-t-3")];
        assert_eq!(schedule.due(&unknown, 1_000), (vec![], Some(1_000 + QUIET_RETRY)));
    }

    #[test]
    fn codex_probe_requires_fresh_positive_native_capacity_not_only_a_sign_in() {
        use alondra_core::accounts::AccountStatus;
        use alondra_core::usage::{AccountUsage,UsageWindow,WindowReading,WindowKind,Share};
        let key=AccountKey::native(Provider::Codex); let mut state=alondra_core::domain::DomainState::new("epoch");
        state.accounts.push(AccountStatus {provider:key.provider,id:key.account.clone(),name:"Personal".into(),state:AccountState::SignedIn,email:None,plan:None,billing:AccountBilling::default(),sign_ins: 0, profile: None,native_identity:None});
        assert!(!native_capacity_ready(&state,&key,100));
        let window=|name:&str|UsageWindow {measured_at:100,reading:WindowReading {kind:WindowKind::Other(format!("codex:account:{name}:300")),used:Share::NONE,resets_at:Some(200),exhausted:false}};
        state.account_usage.push(AccountUsage {provider:key.provider,account:key.account.clone(),plan:None,day_start:None,windows:vec![window("primary")]});
        assert!(!native_capacity_ready(&state,&key,100)); state.account_usage[0].windows.push(window("secondary"));
        assert!(native_capacity_ready(&state,&key,100)); assert!(!native_capacity_ready(&state,&key,101),"old capacity is not a new metadata proof");
        state.account_usage[0].windows.push(UsageWindow {measured_at:100,reading:WindowReading {kind:WindowKind::Other("codex:account:limit".into()),used:Share::NONE,resets_at:None,exhausted:true}});
        assert!(!native_capacity_ready(&state,&key,100),"an unknown-reset limit remains authoritative until recovery cleared it");
    }

    #[test]
    fn existing_capacity_evidence_does_not_invent_new_work_or_paid_authority() {
        use alondra_core::accounts::AccountStatus;
        use alondra_core::usage::{AccountUsage, Share, UsageWindow, WindowKind, WindowReading};
        let key = AccountKey::native(Provider::Codex);
        let mut state = alondra_core::domain::DomainState::new("epoch");
        state.accounts.push(AccountStatus { provider: key.provider, id: key.account.clone(), name: "Personal".into(),
            state: AccountState::Disabled, email: Some("same@example.invalid".into()), plan: None, billing: AccountBilling::default(), sign_ins: 0, profile: None, native_identity: None });
        let window = |name: &str| UsageWindow { measured_at: 100, reading: WindowReading { kind: WindowKind::Other(format!("codex:account:{name}:300")),
            used: Share::NONE, resets_at: Some(200), exhausted: false } };
        state.account_usage.push(AccountUsage { provider: key.provider, account: key.account.clone(), windows: vec![window("primary"), window("secondary")], plan: None, day_start: None });
        assert!(native_capacity_for_existing(&state, &key, 100), "capacity is separate from the disabled new-work policy");
        assert!(!native_capacity_ready(&state, &key, 100), "metadata alone never enables new work");
        state.accounts[0].billing = AccountBilling::PayPerUse;
        assert!(native_capacity_for_existing(&state, &key, 100), "this is capacity evidence, never a paid grant");
        assert!(!native_capacity_ready(&state, &key, 100));
        assert!(!native_capacity_for_existing(&state, &key, 101), "the existing task still needs a fresh probe");
    }

    #[test]
    fn equal_default_slugs_and_outages_are_scheduled_per_provider() {
        let mut schedule=Schedule::default();
        let claude=held("claude-task",ClaudeHold::Outage,"claude-outage-claude-1");
        let mut codex=held("codex-task",ClaudeHold::Outage,"claude-outage-codex-1");codex.provider=Provider::Codex;
        let holds=[claude,codex]; assert!(schedule.due(&holds,100).0.is_empty());
        let due=schedule.due(&holds,100+OUTAGE_PROBE).0;
        assert_eq!(due,vec![(Provider::Claude,ClaudeHold::Outage,None),(Provider::Codex,ClaudeHold::Outage,None)]);
    }

    #[test]
    fn a_sign_in_is_never_probed_on_a_schedule() {
        let mut schedule = Schedule::default();
        let signed_out = [held("t", ClaudeHold::SignedOut, "claude-signed-out-t-7")];
        assert_eq!(schedule.due(&signed_out, 1_000), (vec![], None));
        assert_eq!(schedule.due(&signed_out, 1_000 + 10 * QUIET_RETRY), (vec![], None));
    }

    #[test]
    fn each_accounts_limit_is_probed_when_it_resets_and_the_api_once_for_all() {
        let mut schedule = Schedule::default();
        let (main_reset, work_reset) = (4_102_444_800_u64, 4_102_448_400_u64);
        let held = [
            ClaudeHeld { resets_at: Some(main_reset), ..on("t", ClaudeHold::Usage, "claude-usage-limit-t-3", "default") },
            ClaudeHeld { resets_at: Some(work_reset), ..on("u", ClaudeHold::Usage, "claude-usage-limit-u-4", "work") },
            on("v", ClaudeHold::Outage, "claude-outage-v-5", "default"),
            on("w", ClaudeHold::Outage, "claude-outage-w-6", "work"),
        ];
        let (main_at, work_at) = (Millis::try_from(main_reset * 1000).unwrap(), Millis::try_from(work_reset * 1000).unwrap());
        assert_eq!(schedule.due(&held, 1_000), (vec![], Some(1_000 + OUTAGE_PROBE)));
        let (due, _) = schedule.due(&held, 1_000 + OUTAGE_PROBE);
        assert_eq!(due, vec![wait(ClaudeHold::Outage, None)], "the API is one wait for every account");
        assert_eq!(schedule.due(&held, main_at).0, vec![wait(ClaudeHold::Outage, None), wait(ClaudeHold::Usage, Some("default"))],
            "the main account's reset probes only its tasks");
        assert_eq!(schedule.due(&held[1..], work_at).0, vec![wait(ClaudeHold::Outage, None), wait(ClaudeHold::Usage, Some("work"))],
            "the main account's task went on as its probe; the work account's reset probes its own");
    }
}
