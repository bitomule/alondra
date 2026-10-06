//! What the daemon does when the Mac stops nodes: the disk nearly full, memory or CPU at the limit,
//! or a node that declared with `wait_for_machine` that it waits for the machine. One incident at
//! a time puts one Problem in front of the user, tells the orchestrator, reminds once after
//! `REMIND_AFTER_MS` and, once the Mac recovers, withdraws the card and wakes the nodes that
//! waited: all at once after a full disk, one by one after memory or CPU, so they do not bring the
//! Mac back to its limit. A disk below `LOW_DISK_BYTES` opens an incident before any node fails.
//!
//! With the disk full the domain may not be able to write the card, so the user then hears it by
//! a push, which writes nothing to disk.
use alondra_core::machine_incident::{MACHINE_FIXED_LABEL, MACHINE_FIXED_OPTION, MACHINE_PREFIX, MachineCard, MachineCause};
use alondra_core::views::Lang;
use alondra_core::{Machine, Millis, STRAINED_FREE_PERCENT, now_millis};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use crate::domain_runtime::DomainRuntime;
use crate::usage_pause::Alerts;

pub(crate) const FILE: &str = "machine-incident.json";
pub(crate) const LOW_DISK_BYTES: u64 = 5_000_000_000;
const RECOVERED_DISK_BYTES: u64 = 7_000_000_000;
const HEALTHY_SAMPLES: u32 = 2;
/// A wait no gauge explains, such as a build queue, gets this long before a healthy Mac wakes it.
const DECLARED_GRACE_MS: Millis = 5 * 60_000;
pub(crate) const REMIND_AFTER_MS: Millis = 30 * 60_000;
const TICK: Duration = Duration::from_secs(10);
const STAGGER: Duration = Duration::from_secs(120);
pub(crate) const RESUME_MESSAGE: &str = "The Mac has recovered: disk, memory and CPU are fine again. Carry on with your task; if it still cannot go on, call wait_for_machine again with the reason.";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Declared {
    pub reason: String,
    pub at: Millis,
    /// The node's turn when it declared: a later turn means it went on.
    pub turn: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Incident {
    pub since: Millis,
    pub cause: MachineCause,
    /// The open card, if the domain could write it.
    pub card: Option<String>,
    pub cards: u32,
    pub nodes_shown: usize,
    pub reminded: bool,
    pub healthy: u32,
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    incident: Option<Incident>,
    declared: BTreeMap<String, Declared>,
}

/// A node stopped for the machine, as the card names it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Waiter {
    pub session: String,
    pub name: String,
    pub reason: String,
    pub at: Millis,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Action {
    Raise { id: String, card: MachineCard },
    Withdraw(String),
    Push(MachineCard),
    TellOrchestrator(String),
    Wake { sessions: Vec<String>, one_by_one: bool },
}

/// What the gauges say is wrong. Once the disk was the cause it must climb past
/// `RECOVERED_DISK_BYTES` to count as fine, so a disk hovering at the line does not flap.
pub(crate) fn failing(machine: &Machine, disk_was_low: bool) -> Option<MachineCause> {
    let line = if disk_was_low { RECOVERED_DISK_BYTES } else { LOW_DISK_BYTES };
    if machine.disk_total > 0 && machine.disk_available < line {
        Some(MachineCause::Disk)
    } else if machine.memory_free_percent.is_some_and(|free| free < STRAINED_FREE_PERCENT) {
        Some(MachineCause::Memory)
    } else if machine.unreliable {
        Some(MachineCause::Load)
    } else {
        None
    }
}

fn card_for(cause: MachineCause, machine: &Machine, waiting: &[Waiter], minutes: Option<u64>) -> MachineCard {
    MachineCard {
        cause,
        disk_free: machine.disk_available,
        memory_free_percent: machine.memory_free_percent,
        nodes: waiting.iter().map(|waiter| waiter.name.clone()).collect(),
        reason: (cause == MachineCause::Declared).then(|| waiting.first().map(|waiter| waiter.reason.clone())).flatten(),
        minutes,
    }
}

fn orchestrator_text(card: &MachineCard, opening: bool) -> String {
    let (_, why) = card.text(Lang::En);
    let lead = if opening { "Machine at its limit" } else { "The machine is still at its limit" };
    format!("{lead}: {why} Do not launch or wake nodes until Alondra says it recovered; {} has a card about it.", crate::domain_messages::user_name())
}

/// One sample of the incident: the gauges, the nodes waiting, and whether the user just said they
/// fixed it. Pure, so every turn of it is tested without a clock.
pub(crate) fn step(previous: Option<Incident>, machine: &Machine, waiting: &[Waiter], fixed: bool, now: Millis) -> (Option<Incident>, Vec<Action>) {
    let disk_was_low = previous.as_ref().is_some_and(|incident| incident.cause == MachineCause::Disk);
    let measured = failing(machine, disk_was_low);
    let Some(mut incident) = previous else {
        let Some(cause) = measured.or_else(|| (!waiting.is_empty()).then_some(MachineCause::Declared)) else { return (None, vec![]) };
        let card = card_for(cause, machine, waiting, None);
        let id = format!("{MACHINE_PREFIX}{now}");
        let incident = Incident { since: now, cause, card: Some(id.clone()), cards: 1, nodes_shown: waiting.len(), reminded: false, healthy: 0 };
        return (Some(incident), vec![Action::TellOrchestrator(orchestrator_text(&card, true)), Action::Raise { id, card }]);
    };
    let waited_enough = waiting.iter().all(|waiter| now - waiter.at >= DECLARED_GRACE_MS);
    let fine = measured.is_none() && (incident.cause != MachineCause::Declared || waited_enough || fixed);
    incident.healthy = if fine { incident.healthy + 1 } else { 0 };
    if fine && (fixed || incident.healthy >= HEALTHY_SAMPLES || (incident.cause == MachineCause::Declared && waiting.is_empty())) {
        let mut actions: Vec<Action> = incident.card.into_iter().map(Action::Withdraw).collect();
        let names = waiting.iter().map(|waiter| waiter.name.as_str()).collect::<Vec<_>>().join(", ");
        actions.push(Action::TellOrchestrator(if names.is_empty() {
            "The machine recovered.".into()
        } else {
            format!("The machine recovered: Alondra woke the nodes that waited for it ({names}).")
        }));
        if !waiting.is_empty() {
            actions.push(Action::Wake {
                sessions: waiting.iter().map(|waiter| waiter.session.clone()).collect(),
                one_by_one: matches!(incident.cause, MachineCause::Memory | MachineCause::Load),
            });
        }
        return (None, actions);
    }
    let cause = measured.unwrap_or(incident.cause);
    let longest = waiting.iter().map(|waiter| now - waiter.at).max();
    let remind = !incident.reminded && longest.is_some_and(|waited| waited >= REMIND_AFTER_MS);
    let first_stop = incident.nodes_shown == 0 && !waiting.is_empty();
    let mut actions = vec![];
    if remind || first_stop || cause != incident.cause || fixed {
        let card = card_for(cause, machine, waiting, remind.then(|| longest.unwrap_or(0) as u64 / 60_000));
        actions.extend(incident.card.take().map(Action::Withdraw));
        if remind {
            incident.reminded = true;
            actions.push(Action::TellOrchestrator(orchestrator_text(&card, false)));
            actions.push(Action::Push(card.clone()));
        }
        incident.cards += 1;
        let id = format!("{MACHINE_PREFIX}{}-{}", incident.since, incident.cards);
        incident.card = Some(id.clone());
        incident.cause = cause;
        incident.nodes_shown = waiting.len();
        actions.push(Action::Raise { id, card });
    }
    (Some(incident), actions)
}

pub(crate) struct MachineWatch {
    record: Mutex<Record>,
    file: Option<PathBuf>,
    changed: tokio::sync::Notify,
}

impl Default for MachineWatch {
    fn default() -> Self {
        Self { record: Mutex::default(), file: None, changed: tokio::sync::Notify::new() }
    }
}

impl MachineWatch {
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let record = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        Self { record: Mutex::new(record), file: Some(file), ..Self::default() }
    }

    fn save(&self, record: &Record) {
        let Some(file) = &self.file else { return };
        let next = file.with_extension("next");
        let written = serde_json::to_vec(record).map_err(std::io::Error::from)
            .and_then(|bytes| std::fs::write(&next, bytes))
            .and_then(|()| std::fs::rename(&next, file));
        if let Err(error) = written {
            log!("could not save {}: {error}", file.display());
        }
    }

    /// A node said it waits for the machine; the watcher looks at once.
    pub(crate) fn declare(&self, session: &str, reason: &str, turn: u64, now: Millis) {
        let mut record = self.record.lock().expect("machine watch poisoned");
        record.declared.insert(session.to_owned(), Declared { reason: reason.to_owned(), at: now, turn });
        self.save(&record);
        drop(record);
        self.changed.notify_one();
    }

    pub(crate) fn incident(&self) -> Option<Incident> {
        self.record.lock().expect("machine watch poisoned").incident.clone()
    }

    fn declared(&self) -> BTreeMap<String, Declared> {
        self.record.lock().expect("machine watch poisoned").declared.clone()
    }

    fn forget(&self, sessions: &[String]) {
        let mut record = self.record.lock().expect("machine watch poisoned");
        if sessions.iter().fold(false, |any, session| record.declared.remove(session).is_some() || any) {
            self.save(&record);
        }
    }

    fn set_incident(&self, incident: Option<Incident>) {
        let mut record = self.record.lock().expect("machine watch poisoned");
        if record.incident != incident {
            record.incident = incident;
            self.save(&record);
        }
    }
}

/// The nodes still waiting: a node that took another turn since it declared, or that the user
/// closed, went on and is forgotten.
fn waiters(domain: &DomainRuntime) -> Vec<Waiter> {
    let watch = &domain.live.machine_watch;
    let state = domain.state().ok();
    let mut gone = vec![];
    let mut waiting = vec![];
    for (session, declared) in watch.declared() {
        let moved_on = domain.live.node(&session).is_some_and(|node| node.turn > declared.turn);
        if moved_on || domain.live.is_closed(&session) {
            gone.push(session);
            continue;
        }
        let name = state.as_ref().and_then(|state| {
            let agent = alondra_core::domain::AgentId(session.clone());
            state.tasks.values().find(|task| task.agent_id.as_ref() == Some(&agent) && !task.archived).map(|task| task.title.clone())
                .or_else(|| state.agents.get(&agent).map(|agent| agent.name.clone()))
        }).unwrap_or_else(|| session.chars().take(8).collect());
        waiting.push(Waiter { session, name, reason: declared.reason, at: declared.at });
    }
    watch.forget(&gone);
    waiting
}

/// The answer "I've fixed it" on the open card, taken once.
fn fixed(domain: &DomainRuntime, incident: Option<&Incident>) -> bool {
    use alondra_core::domain::{InteractionId, InteractionPayload};
    let Some(card) = incident.and_then(|incident| incident.card.as_ref()) else { return false };
    let Ok(state) = domain.state() else { return false };
    matches!(state.interactions.get(&InteractionId(card.clone())).map(|card| &card.payload),
        Some(InteractionPayload::Question { answer: Some(answer), .. }) if answer.option_id == MACHINE_FIXED_OPTION)
}

fn push_text(card: MachineCard) -> impl Fn(bool) -> String + Send + Sync {
    move |spanish| {
        let (need, why) = card.text(if spanish { Lang::Es } else { Lang::En });
        format!("{need}. {why}")
    }
}

fn wake(domain: &DomainRuntime, session: &str) {
    if domain.live.pushes.message(session, RESUME_MESSAGE, true).is_err() {
        domain.live.losses.report(session);
    }
}

/// Carries out one step's actions.
async fn act(domain: &DomainRuntime, alerts: &impl Alerts, sample: impl Fn() -> Option<Machine>, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Raise { id, card } => {
                let (need, why) = card.text(Lang::En);
                let option = alondra_core::domain::QuestionOption { id: MACHINE_FIXED_OPTION.into(), label: MACHINE_FIXED_LABEL.into(), description: None, inputs: vec![] };
                if let Err(error) = domain.raise_worded_problem(&id, need, why, card.to_body(), vec![option]) {
                    log!("could not put the machine's card in front of the user, pushing it instead: {error:?}");
                    alerts.tell(push_text(card), false).await;
                }
            }
            Action::Withdraw(id) => {
                let _ = domain.withdraw_problem(&alondra_core::domain::InteractionId(id));
            }
            Action::Push(card) => alerts.tell(push_text(card), false).await,
            Action::TellOrchestrator(text) => {
                let id = format!("{MACHINE_PREFIX}note-{}", crate::auth::random_hex(6));
                if let Err(error) = domain.tell_orchestrator(&id, text) {
                    log!("could not tell the orchestrator about the machine: {error:?}");
                }
            }
            Action::Wake { sessions, one_by_one } => {
                log!("the machine recovered: waking {} node(s){}", sessions.len(), if one_by_one { " one by one" } else { "" });
                domain.live.machine_watch.forget(&sessions);
                for (index, session) in sessions.iter().enumerate() {
                    if one_by_one && index > 0 {
                        tokio::time::sleep(STAGGER).await;
                        if sample().is_some_and(|machine| failing(&machine, false).is_some()) {
                            log!("the machine is at its limit again: the other nodes keep waiting");
                            let rest = sessions[index..].to_vec();
                            let now = now_millis();
                            for session in rest {
                                domain.live.machine_watch.declare(&session, "the machine went back to its limit", domain.live.node(&session).map_or(0, |node| node.turn), now);
                            }
                            break;
                        }
                    }
                    wake(domain, session);
                }
            }
        }
    }
}

/// Looks at the machine on every sample and whenever a node declares a wait or the user answers.
pub(crate) async fn run(domain: std::sync::Arc<DomainRuntime>, gauges: crate::machine::Gauges, alerts: impl Alerts) {
    watch(domain, move || gauges.lock().expect("machine gauges poisoned").as_ref().map(|(machine, _)| machine.clone()), alerts, TICK).await
}

async fn watch(domain: std::sync::Arc<DomainRuntime>, sample: impl Fn() -> Option<Machine>, alerts: impl Alerts, tick: Duration) {
    let mut cards = domain.subscribe();
    loop {
        if let Some(machine) = sample() {
            let watch = &domain.live.machine_watch;
            let previous = watch.incident();
            let fixed = fixed(&domain, previous.as_ref());
            let (next, actions) = step(previous, &machine, &waiters(&domain), fixed, now_millis());
            watch.set_incident(next);
            act(&domain, &alerts, &sample, actions).await;
        }
        tokio::select! {
            _ = tokio::time::sleep(tick) => {}
            _ = domain.live.machine_watch.changed.notified() => {}
            changed = cards.changed() => if changed.is_err() { return },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_push::testing::Node;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, InteractionPayload, PROTOCOL_VERSION};
    use std::sync::Arc;

    const GB: u64 = 1_000_000_000;
    const MIN: Millis = 60_000;

    fn machine(disk_free: u64, memory_free: u32) -> Machine {
        Machine { at: 0, memory_total: 100, memory_available: 50, memory_free_percent: Some(memory_free), load: [1.0; 3], cpus: 8,
            cpu_usage: 10.0, disk_total: 500 * GB, disk_available: disk_free, unreliable: false }
    }

    fn waiter(session: &str, name: &str, at: Millis) -> Waiter {
        Waiter { session: session.into(), name: name.into(), reason: "disk full".into(), at }
    }

    fn raised(actions: &[Action]) -> Vec<&MachineCard> {
        actions.iter().filter_map(|action| match action { Action::Raise { card, .. } => Some(card), _ => None }).collect()
    }

    #[test]
    fn a_full_disk_with_a_stopped_node_raises_one_card_and_tells_the_orchestrator() {
        let phase0 = [waiter("s1", "Fase 0: quitar versión de identificadores", 0)];
        let (incident, actions) = step(None, &machine(0, 50), &phase0, false, 1_000);
        let incident = incident.unwrap();
        assert_eq!((incident.cause, incident.card.as_deref()), (MachineCause::Disk, Some("machine-1000")));
        let cards = raised(&actions);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].text(Lang::Es).0, "Libera espacio en disco: quedan 0 B");
        assert_eq!(cards[0].nodes, ["Fase 0: quitar versión de identificadores"]);
        assert!(actions.iter().any(|action| matches!(action, Action::TellOrchestrator(text) if text.starts_with("Machine at its limit: Disk nearly full (0 B free). Stopped: Fase 0"))));

        let both = [phase0[0].clone(), waiter("s2", "Lumo", 2_000)];
        let (still, actions) = step(Some(incident), &machine(0, 50), &both, false, 11_000);
        assert!(actions.is_empty(), "a second node stopping is the same incident, not another card: {actions:?}");
        assert_eq!(still.unwrap().card.as_deref(), Some("machine-1000"));
    }

    #[test]
    fn a_disk_running_low_warns_before_any_node_stops_and_the_first_stop_renews_the_card() {
        let (incident, actions) = step(None, &machine(4 * GB, 50), &[], false, 0);
        assert_eq!(raised(&actions)[0].text(Lang::En).0, "Free up disk space: only 4.0 GB left");
        let (incident, actions) = step(incident, &machine(4 * GB, 50), &[], false, 10_000);
        assert!(actions.is_empty(), "no card again on every sample");
        let (incident, actions) = step(incident, &machine(0, 50), &[waiter("s1", "Lumo", 15_000)], false, 20_000);
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()));
        assert_eq!(raised(&actions)[0].nodes, ["Lumo"]);
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-0-2"));
    }

    #[test]
    fn a_long_wait_reminds_once_at_thirty_minutes() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (mut incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let mut reminders = 0;
        for minute in 1..=240 {
            let (next, actions) = step(incident, &machine(0, 50), &stopped, false, minute * MIN);
            if actions.iter().any(|action| matches!(action, Action::Push(_))) {
                reminders += 1;
                assert_eq!(minute, 30, "the reminder comes at 30 minutes");
                assert_eq!(raised(&actions)[0].minutes, Some(30));
                assert!(actions.iter().any(|action| matches!(action, Action::TellOrchestrator(text) if text.starts_with("The machine is still at its limit"))));
            } else {
                assert!(actions.is_empty(), "nothing else at minute {minute}: {actions:?}");
            }
            incident = next;
        }
        assert_eq!(reminders, 1, "only once");
    }

    #[test]
    fn the_disk_must_climb_past_the_recovery_line_twice_to_wake_everyone_at_once() {
        let stopped = [waiter("s1", "Lumo", 0), waiter("s2", "Quill", 0)];
        let (incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, actions) = step(incident, &machine(6 * GB, 50), &stopped, false, MIN);
        assert!(actions.is_empty() && incident.is_some(), "6 GB is above the warning yet below the recovery line");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 2 * MIN);
        assert!(actions.is_empty() && incident.is_some(), "one healthy sample is not enough");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 3 * MIN);
        assert!(incident.is_none());
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()));
        assert!(actions.contains(&Action::Wake { sessions: vec!["s1".into(), "s2".into()], one_by_one: false }));
    }

    #[test]
    fn memory_wakes_one_by_one_and_a_declared_wait_gets_its_grace() {
        let stopped = [waiter("s1", "Lumo", 0), waiter("s2", "Quill", 0)];
        let (incident, _) = step(None, &machine(100 * GB, 5), &stopped, false, 0);
        assert_eq!(incident.as_ref().unwrap().cause, MachineCause::Memory);
        let (incident, _) = step(incident, &machine(100 * GB, 40), &stopped, false, MIN);
        let (_, actions) = step(incident, &machine(100 * GB, 40), &stopped, false, 2 * MIN);
        assert!(actions.contains(&Action::Wake { sessions: vec!["s1".into(), "s2".into()], one_by_one: true }));

        let queued = [Waiter { reason: "build queue full".into(), ..waiter("s1", "Quill", 0) }];
        let (mut incident, actions) = step(None, &machine(100 * GB, 50), &queued, false, 0);
        assert_eq!(raised(&actions)[0].cause, MachineCause::Declared);
        for minute in 1..5 {
            let (next, actions) = step(incident, &machine(100 * GB, 50), &queued, false, minute * MIN);
            assert!(actions.is_empty(), "a healthy Mac does not wake a declared wait before its grace");
            incident = next;
        }
        let (incident, _) = step(incident, &machine(100 * GB, 50), &queued, false, 5 * MIN);
        let (incident, actions) = step(incident, &machine(100 * GB, 50), &queued, false, 5 * MIN + 10_000);
        assert!(incident.is_none());
        assert!(actions.contains(&Action::Wake { sessions: vec!["s1".into()], one_by_one: false }));
    }

    #[test]
    fn i_have_fixed_it_wakes_at_once_when_the_mac_is_fine_and_renews_the_card_when_it_is_not() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, actions) = step(incident, &machine(GB / 2, 50), &stopped, true, MIN);
        let cards = raised(&actions);
        assert_eq!(cards[0].text(Lang::Es).0, "Libera espacio en disco: quedan 500 MB", "still full: a new card with the current numbers");
        let (incident, actions) = step(incident, &machine(50 * GB, 50), &stopped, true, 2 * MIN);
        assert!(incident.is_none(), "no second sample needed once the user says so");
        assert!(actions.contains(&Action::Wake { sessions: vec!["s1".into()], one_by_one: false }));
    }

    struct Home(PathBuf);
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn world() -> (Home, Arc<DomainRuntime>) {
        let home = std::env::temp_dir().join(format!("alondra-machine-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        (Home(home.clone()), Arc::new(DomainRuntime::open(&home).unwrap()))
    }

    struct Told(Mutex<Vec<String>>);
    impl Alerts for Arc<Told> {
        async fn tell(&self, text: impl Fn(bool) -> String + Send + Sync, _passive: bool) {
            self.0.lock().unwrap().push(text(true));
        }
    }

    fn open_machine_cards(domain: &DomainRuntime) -> Vec<alondra_core::domain::Interaction> {
        let state = domain.state().unwrap();
        state.interactions.values().filter(|card| card.id.0.starts_with(MACHINE_PREFIX) && !card.withdrawn
            && matches!(card.payload, InteractionPayload::Question { answer: None, .. })).cloned().collect()
    }

    fn orchestrator_inbox(domain: &DomainRuntime) -> Vec<String> {
        let state = domain.state().unwrap();
        domain.store.lock().unwrap().pending_messages().unwrap().iter()
            .filter(|message| message.recipient_kind == "orchestrator")
            .map(|message| crate::domain_transport::render(&state, message)).collect()
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
    async fn a_full_disk_puts_one_card_and_tells_the_orchestrator_and_i_have_fixed_it_wakes_the_node() {
        let (_home, domain) = world();
        let session = "fase0";
        domain.live.machine_watch.declare(session, "disk full, 0 bytes free", 0, now_millis());
        let node = Node::connect(&domain, session);
        let disk = Arc::new(Mutex::new(0u64));
        let reading = disk.clone();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let watcher = tokio::spawn(watch(domain.clone(), move || Some(machine(*reading.lock().unwrap(), 50)), told.clone(), Duration::from_millis(20)));

        until("the card", || open_machine_cards(&domain).len() == 1).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        let cards = open_machine_cards(&domain);
        assert_eq!(cards.len(), 1, "one card for the incident, not one per sample");
        assert_eq!(cards[0].card.need, "Free up disk space: 0 B left");
        assert!(orchestrator_inbox(&domain).iter().any(|text| text.starts_with("Machine at its limit: Disk nearly full (0 B free). Stopped: fase0.")),
            "{:?}", orchestrator_inbox(&domain));
        assert!(told.0.lock().unwrap().is_empty(), "the card got through: no push in its place");

        *disk.lock().unwrap() = 50 * GB;
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "device:phone".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: "fixed".into(), expected_revision: state.revision,
            command: Command::AnswerQuestion { attachments: vec![], interaction_id: cards[0].id.clone(), option_id: MACHINE_FIXED_OPTION.into(), inputs: Default::default() },
        }, None, || Ok(())).unwrap();
        until("the node woken", || node.texts() == [RESUME_MESSAGE]).await;
        watcher.abort();
        assert!(domain.live.machine_watch.incident().is_none());
        assert!(open_machine_cards(&domain).is_empty());
        assert!(orchestrator_inbox(&domain).iter().any(|text| text == "The machine recovered: Alondra woke the nodes that waited for it (fase0)."));
    }

    #[tokio::test]
    async fn when_the_card_cannot_be_written_the_user_hears_it_by_push() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("fase0", "disk full", 0, now_millis());
        let told = Arc::new(Told(Mutex::new(vec![])));
        let incident = Incident { since: 0, cause: MachineCause::Disk, card: None, cards: 1, nodes_shown: 0, reminded: false, healthy: 0 };
        domain.live.machine_watch.set_incident(Some(incident));
        // The card's id is taken already, so the domain refuses it as a full disk would.
        domain.raise_worded_problem("machine-0-2", "taken".into(), "taken".into(), "{}".into(), vec![
            alondra_core::domain::QuestionOption { id: "x".into(), label: "x".into(), description: None, inputs: vec![] },
        ]).unwrap();
        let watcher = tokio::spawn(watch(domain.clone(), || Some(machine(0, 50)), told.clone(), Duration::from_millis(20)));
        until("the push", || !told.0.lock().unwrap().is_empty()).await;
        watcher.abort();
        assert_eq!(told.0.lock().unwrap()[0], "Libera espacio en disco: quedan 0 B. Disco casi lleno (0 B libres). Parados: fase0. Continúan solos cuando se recupere.");
    }

    #[test]
    fn a_node_that_took_another_turn_no_longer_waits() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("s", "RAM", 0, now_millis());
        assert_eq!(waiters(&domain).len(), 1);
        domain.live.observe(&serde_json::json!({"session_id": "s", "hook_event_name": "UserPromptSubmit", "prompt": "go"}));
        assert!(waiters(&domain).is_empty());
        assert!(waiters(&domain).is_empty(), "and it is forgotten");
    }
}
