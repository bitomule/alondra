//! The card that asks the user to allow Alondra on this Mac's local network when macOS keeps
//! denying it: opened once the denial outlasts the seconds macOS takes to let an updated binary
//! through, withdrawn as soon as a check finds it allowed. "I've allowed it" checks again at once
//! and, if macOS still denies it, opens a card that says so.
use alondra_core::domain::SystemCard;
use std::sync::Arc;
use std::time::Duration;

use alondra_core::domain::{InteractionId, InteractionPayload, QuestionOption};
use alondra_core::local_network_card::{
    LOCAL_NETWORK_ALLOWED_LABEL, LOCAL_NETWORK_ALLOWED_OPTION, LOCAL_NETWORK_CAUSE, LOCAL_NETWORK_PREFIX, LocalNetworkCard,
};
use alondra_core::views::Lang;

use crate::domain_runtime::DomainRuntime;

/// Longer than the last check after start, so an update that macOS lets through by itself never
/// shows a card.
pub(crate) const GRACE: Duration = Duration::from_secs(45);
const TICK: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
pub(crate) enum Step {
    Raise { still: bool },
    Withdraw,
    Nothing,
}

/// What to do with the card, given whether one is open, whether the user answered it, the answer
/// macOS gives now and for how long it has denied it.
pub(crate) fn step(open: bool, answered: bool, found: &str, denied_for: Option<Duration>) -> Step {
    match (open, found) {
        (true, "allowed") => Step::Withdraw,
        (true, "denied") if answered => Step::Raise { still: true },
        (false, "denied") if denied_for.is_some_and(|denied| denied >= GRACE) => Step::Raise { still: false },
        _ => Step::Nothing,
    }
}

/// Whether launchd runs alondrad through the launcher that keeps the permission across updates.
pub(crate) fn under_launcher() -> bool {
    let mut path = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    let written = unsafe { libc::proc_pidpath(libc::getppid(), path.as_mut_ptr().cast(), path.len() as u32) };
    written > 0 && std::str::from_utf8(&path[..written as usize]).is_ok_and(|parent| parent.ends_with(&format!("/{}", alondra_core::launcher::NAME)))
}

pub(crate) async fn run(domain: Arc<DomainRuntime>) {
    let launcher = under_launcher();
    watch(domain, || (crate::local_network::current(), crate::local_network::denied_for()), || crate::local_network::check(), launcher, TICK).await
}

fn open_cards(domain: &DomainRuntime) -> Vec<InteractionId> {
    let Ok(state) = domain.state() else { return vec![] };
    state.interactions.values()
        .filter(|card| card.system_card == Some(SystemCard::LocalNetwork) && !card.withdrawn && matches!(card.payload, InteractionPayload::Question { answer: None, .. }))
        .map(|card| card.id.clone())
        .collect()
}

fn answered(domain: &DomainRuntime, id: &InteractionId) -> bool {
    domain.state().ok().is_some_and(|state| matches!(
        state.interactions.get(id).map(|card| &card.payload),
        Some(InteractionPayload::Question { answer: Some(_), .. })
    ))
}

fn raise(domain: &DomainRuntime, launcher: bool, still: bool) -> Option<InteractionId> {
    let id = format!("{LOCAL_NETWORK_PREFIX}{}", alondra_core::now_millis());
    let card = LocalNetworkCard { launcher, still };
    let (need, why) = card.text(Lang::En);
    let option = QuestionOption { id: LOCAL_NETWORK_ALLOWED_OPTION.into(), label: LOCAL_NETWORK_ALLOWED_LABEL.into(), description: None };
    match domain.raise_worded_problem(&id, Some(SystemCard::LocalNetwork), LOCAL_NETWORK_CAUSE, need, why, card.to_body(), vec![option], None) {
        Ok(_) => Some(InteractionId(id)),
        Err(error) => {
            log!("could not put the local network card in front of the user: {error:?}");
            None
        }
    }
}

async fn watch(
    domain: Arc<DomainRuntime>,
    read: impl Fn() -> (&'static str, Option<Duration>) + Send + Sync + 'static,
    recheck: impl Fn() -> &'static str + Send + Sync + 'static,
    launcher: bool,
    tick: Duration,
) {
    let (read, recheck) = (Arc::new(read), Arc::new(recheck));
    let starting = domain.clone();
    tokio::task::spawn_blocking(move || {
        for id in open_cards(&starting) {
            let _ = starting.withdraw_problem(&id);
        }
    }).await.expect("local network card worker failed");
    let mut open: Option<InteractionId> = None;
    loop {
        let (domain, read, recheck, card) = (domain.clone(), read.clone(), recheck.clone(), open.clone());
        open = tokio::task::spawn_blocking(move || {
            let answered = card.as_ref().is_some_and(|id| answered(&domain, id));
            let (found, denied_for) = if answered { (recheck(), read().1) } else { read() };
            match step(card.is_some(), answered, found, denied_for) {
                Step::Withdraw => {
                    if let Some(id) = &card {
                        let _ = domain.withdraw_problem(id);
                    }
                    None
                }
                Step::Raise { still } => {
                    if let Some(id) = &card {
                        let _ = domain.withdraw_problem(id);
                    }
                    raise(&domain, launcher, still)
                }
                Step::Nothing if answered => None,
                Step::Nothing => card,
            }
        }).await.expect("local network card worker failed");
        tokio::time::sleep(tick).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, PROTOCOL_VERSION};
    use std::sync::Mutex;

    #[test]
    fn a_denial_shorter_than_the_grace_opens_nothing() {
        assert_eq!(step(false, false, "denied", Some(Duration::from_secs(3))), Step::Nothing);
        assert_eq!(step(false, false, "denied", Some(GRACE)), Step::Raise { still: false });
        assert_eq!(step(false, false, "unknown", Some(GRACE * 2)), Step::Nothing);
        assert_eq!(step(false, false, "allowed", None), Step::Nothing);
    }

    #[test]
    fn an_open_card_closes_once_allowed_and_stays_while_denied() {
        assert_eq!(step(true, false, "allowed", None), Step::Withdraw);
        assert_eq!(step(true, false, "denied", Some(GRACE * 10)), Step::Nothing);
        assert_eq!(step(true, true, "allowed", None), Step::Withdraw);
        assert_eq!(step(true, true, "denied", Some(GRACE)), Step::Raise { still: true });
    }

    #[test]
    fn the_grace_outlasts_every_check_after_start() {
        assert!(crate::local_network::AFTER_START.iter().all(|at| *at < GRACE));
    }

    struct Home(crate::test_support::Directory);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn world() -> (Home, Arc<DomainRuntime>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        (Home(home), domain)
    }

    fn cards(domain: &DomainRuntime) -> Vec<alondra_core::domain::Interaction> {
        let state = domain.state().unwrap();
        let mut open: Vec<_> = state.interactions.values().filter(|card| card.system_card == Some(SystemCard::LocalNetwork) && !card.withdrawn
            && matches!(card.payload, InteractionPayload::Question { answer: None, .. })).cloned().collect();
        open.sort_by(|a, b| a.id.0.cmp(&b.id.0));
        open
    }

    async fn until(what: &str, check: impl Fn() -> bool) {
        for _ in 0..300 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("never happened: {what}");
    }

    #[tokio::test]
    async fn a_lasting_denial_opens_one_card_that_closes_when_macos_allows_it() {
        let (_home, domain) = world();
        let answer = Arc::new(Mutex::new(("denied", Some(GRACE))));
        let reading = answer.clone();
        let watcher = tokio::spawn(watch(domain.clone(), move || *reading.lock().unwrap(), || "denied", true, Duration::from_millis(10)));

        until("the card", || cards(&domain).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let open = cards(&domain);
        assert_eq!(open.len(), 1, "one card, not one per check");
        assert_eq!(open[0].card.need, "Allow Alondra to use this Mac's local network");
        assert_eq!(LocalNetworkCard::from_body(&open[0].body), Some(LocalNetworkCard { launcher: true, still: false }));

        *answer.lock().unwrap() = ("allowed", None);
        until("the card withdrawn", || cards(&domain).is_empty()).await;
        watcher.abort();
    }

    #[tokio::test]
    async fn i_have_allowed_it_checks_again_and_says_so_when_macos_still_denies_it() {
        let (_home, domain) = world();
        let rechecked = Arc::new(Mutex::new(0));
        let counting = rechecked.clone();
        let watcher = tokio::spawn(watch(domain.clone(), || ("denied", Some(GRACE)), move || { *counting.lock().unwrap() += 1; "denied" }, false, Duration::from_millis(10)));
        until("the card", || cards(&domain).len() == 1).await;
        let first = cards(&domain).remove(0);
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "device:phone".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "allowed".into(), expected_revision: state.revision,
            command: Command::AnswerQuestion { attachments: vec![], interaction_id: first.id.clone(), option_id: LOCAL_NETWORK_ALLOWED_OPTION.into(), inputs: Default::default() },
        }, None, || Ok(())).unwrap();
        until("a card that says it is still denied", || cards(&domain).iter().any(|card| card.id != first.id)).await;
        watcher.abort();
        assert!(*rechecked.lock().unwrap() >= 1, "the answer made it check again");
        let open = cards(&domain);
        assert_eq!(open.len(), 1);
        assert_eq!(LocalNetworkCard::from_body(&open[0].body), Some(LocalNetworkCard { launcher: false, still: true }));
    }

    #[tokio::test]
    async fn a_card_left_open_by_an_earlier_daemon_is_withdrawn_at_start() {
        let (_home, domain) = world();
        raise(&domain, true, false).unwrap();
        assert_eq!(cards(&domain).len(), 1);
        let watcher = tokio::spawn(watch(domain.clone(), || ("allowed", None), || "allowed", true, Duration::from_millis(10)));
        until("the stray withdrawn", || cards(&domain).is_empty()).await;
        watcher.abort();
    }
}
