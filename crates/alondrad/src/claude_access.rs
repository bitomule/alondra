//! When to probe Claude for the tasks it holds through the Agent SDK. While the API fails or the
//! usage limit stands, one held task goes on now and then; its next turn proves Claude works, and
//! every held task goes on, or holds it again. The clock only says when to look. A sign-in is
//! probed by the user's answer to its card, never here.
use crate::domain_runtime::DomainRuntime;
use alondra_core::domain::{
    ClaudeHeld, ClaudeHold, Command, CommandEnvelope, Millis, OUTAGE_PROBE, PROTOCOL_VERSION, QUIET_RETRY,
    usage_resets_at,
};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const PROBED: [ClaudeHold; 2] = [ClaudeHold::Outage, ClaudeHold::Usage];

/// Since when each hold has waited without a probe.
#[derive(Default)]
pub(crate) struct Schedule {
    since: BTreeMap<ClaudeHold, Millis>,
}

impl Schedule {
    /// The holds to probe at `now`, and when to look next. A hold no task waits on any more is
    /// forgotten: the next one waits a whole interval again.
    pub(crate) fn due(&mut self, held: &[ClaudeHeld], now: Millis) -> (Vec<ClaudeHold>, Option<Millis>) {
        let mut due = vec![];
        let mut next: Option<Millis> = None;
        for hold in PROBED {
            let waiting: Vec<&ClaudeHeld> = held.iter().filter(|held| held.hold == hold).collect();
            if waiting.is_empty() {
                self.since.remove(&hold);
                continue;
            }
            let since = *self.since.entry(hold).or_insert(now);
            let at = match hold {
                ClaudeHold::Usage => waiting
                    .iter()
                    .filter_map(|held| usage_resets_at(&held.card))
                    .filter_map(|seconds| Millis::try_from(seconds).ok()?.checked_mul(1000))
                    .max()
                    .filter(|reset| *reset > since)
                    .unwrap_or(since + QUIET_RETRY),
                _ => since + OUTAGE_PROBE,
            };
            let at = if at <= now {
                due.push(hold);
                self.since.insert(hold, now);
                now + if hold == ClaudeHold::Usage { QUIET_RETRY } else { OUTAGE_PROBE }
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
        for hold in due {
            probe(&domain, hold);
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

pub(crate) fn probe(domain: &DomainRuntime, hold: ClaudeHold) {
    let state = domain.read();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.state().data_epoch.clone(),
        command_id: format!("probe-claude-{hold:?}-{}", state.state().revision).to_lowercase().as_str().into(),
        expected_revision: state.state().revision,
        command: Command::ProbeClaudeAccess { hold },
    };
    match domain.execute_as_system(envelope) {
        Ok(_) => log!("probing Claude with one task it held ({hold:?})"),
        Err(error) => log!("could not probe Claude ({hold:?}): {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{InteractionId, TaskId};

    fn held(task: &str, hold: ClaudeHold, card: &str) -> ClaudeHeld {
        ClaudeHeld { task: TaskId::from(task), hold, card: InteractionId::from(card) }
    }

    #[test]
    fn an_outage_is_probed_once_per_interval_and_forgotten_when_nothing_waits() {
        let mut schedule = Schedule::default();
        let outage = [held("t", ClaudeHold::Outage, "claude-outage-t-3"), held("u", ClaudeHold::Outage, "claude-outage-u-4")];
        assert_eq!(schedule.due(&outage, 1_000), (vec![], Some(1_000 + OUTAGE_PROBE)));
        assert_eq!(schedule.due(&outage, 1_000 + OUTAGE_PROBE - 1), (vec![], Some(1_000 + OUTAGE_PROBE)));
        let at = 1_000 + OUTAGE_PROBE;
        assert_eq!(schedule.due(&outage, at), (vec![ClaudeHold::Outage], Some(at + OUTAGE_PROBE)));
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
        assert_eq!(schedule.due(&known, at), (vec![ClaudeHold::Usage], Some(at + QUIET_RETRY)));
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
}
