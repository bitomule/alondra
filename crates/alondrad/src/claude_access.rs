//! When to probe Claude for the tasks it holds through the Agent SDK. While the API fails, one
//! held task goes on now and then; while an account's usage limit stands, one of that account's
//! tasks goes on when its limit resets. Its next turn proves Claude works, and the tasks the same
//! wait held go on, or it holds again. The clock only says when to look. A sign-in is probed by
//! the user's answer to its card, never here.
use crate::domain_runtime::DomainRuntime;
use alondra_core::accounts::AccountId;
use alondra_core::domain::{
    ClaudeHeld, ClaudeHold, Command, CommandEnvelope, Millis, OUTAGE_PROBE, PROTOCOL_VERSION, QUIET_RETRY,
    usage_resets_at,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// A wait the schedule probes: the API, or one account's usage limit.
type Wait = (ClaudeHold, Option<AccountId>);

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
            let usage = wait.0 == ClaudeHold::Usage;
            let since = *self.since.entry(wait.clone()).or_insert(now);
            let at = if usage {
                waiting
                    .iter()
                    .filter_map(|held| usage_resets_at(&held.card))
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

pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    let mut changes = domain.subscribe();
    let mut schedule = Schedule::default();
    loop {
        let now = alondra_core::now_millis();
        let held = domain.read().state().claude_held();
        let (due, next) = schedule.due(&held, now);
        for (hold, account) in due {
            probe(&domain, hold, account);
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

pub(crate) fn probe(domain: &DomainRuntime, hold: ClaudeHold, account: Option<AccountId>) {
    let state = domain.read();
    let named = account.as_ref().map_or_else(String::new, |account| format!("-{account}"));
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.state().data_epoch.clone(),
        command_id: format!("probe-claude-{hold:?}{named}-{}", state.state().revision).to_lowercase().as_str().into(),
        expected_revision: state.state().revision,
        command: Command::ProbeClaudeAccess { hold, account: account.clone() },
    };
    match domain.execute_as_system(envelope) {
        Ok(_) => log!("probing Claude with one task it held ({hold:?}{named})"),
        Err(error) => log!("could not probe Claude ({hold:?}{named}): {error}"),
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
        ClaudeHeld { task: TaskId::from(task), hold, card: InteractionId::from(card), account: AccountId::parse(account).unwrap() }
    }

    fn wait(hold: ClaudeHold, account: Option<&str>) -> Wait {
        (hold, account.map(|account| AccountId::parse(account).unwrap()))
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
        let known = [held("t", ClaudeHold::Usage, &format!("claude-usage-limit-until-{reset}-t-3"))];
        let at = Millis::try_from(reset * 1000).unwrap();
        assert_eq!(schedule.due(&known, 1_000), (vec![], Some(at)));
        assert_eq!(schedule.due(&known, at), (vec![wait(ClaudeHold::Usage, Some(AccountId::DEFAULT))], Some(at + QUIET_RETRY)));
        let mut schedule = Schedule::default();
        let unknown = [held("t", ClaudeHold::Usage, "claude-usage-limit-t-3")];
        assert_eq!(schedule.due(&unknown, 1_000), (vec![], Some(1_000 + QUIET_RETRY)));
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
            on("t", ClaudeHold::Usage, &format!("claude-usage-limit-until-{main_reset}-t-3"), "default"),
            on("u", ClaudeHold::Usage, &format!("claude-usage-limit-until-{work_reset}-u-4"), "work"),
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
