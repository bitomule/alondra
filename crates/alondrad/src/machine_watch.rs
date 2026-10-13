//! What the daemon does when the Mac stops nodes: the disk nearly full, or a node that declared
//! with `wait_for_machine` that it waits for the machine. One incident at a
//! time puts one Problem in front of the user, reminds once after
//! `REMIND_AFTER_MS` and, once the Mac recovers, withdraws the card. The nodes that waited are not
//! woken here: they wait paused at the fleet gate, which brings them back one at a time within the
//! limit (`fleet_pause`). A disk below `disk::low_line()` opens an incident before any node fails,
//! and while it is open no node starts. The disk watched is the Mac's own and every volume a node
//! was launched on, so a repository on an external disk is watched too.
//!
//! Memory opens no incident: the user can do nothing about it that Alondra does not do already.
//! `pace` takes each sample into `MacState`, from which the gate reads whether there is room for
//! one more node; memory never pauses a working node. The orchestrator hears of neither: it has
//! nothing to do about the machine, and `launch` says why a task is queued.
//!
//! With the disk full the domain may not be able to write the card, so the user then hears it by
//! a push, which writes nothing to disk.
use alondra_core::machine_incident::{MACHINE_FIXED_LABEL, MACHINE_FIXED_OPTION, MACHINE_PREFIX, MachineCard, MachineCause};
use alondra_core::views::Lang;
use alondra_core::{Machine, MemoryPressure, Millis, now_millis};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// One machine incident is open at a time, so its card has one cause.
const MACHINE_CAUSE: &str = "machine";
use std::sync::Mutex;
use std::time::Duration;

use crate::domain_runtime::DomainRuntime;
use crate::machine_state::{MacState, NoRoom};
use crate::usage_pause::Alerts;

pub(crate) const FILE: &str = "machine-incident.json";
const HEALTHY_SAMPLES: u32 = 2;
/// A disk that climbs back can fall again within minutes as builds come and go: its incident, and
/// its one card, stays open until the disk has been fine this long.
pub(crate) const DISK_SETTLE_MS: Millis = 10 * 60_000;
pub(crate) const REMIND_AFTER_MS: Millis = 30 * 60_000;
const TICK: Duration = Duration::from_secs(10);

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
    /// Since when the gauges say fine, while they do.
    #[serde(default)]
    pub fine_since: Option<Millis>,
}

#[derive(Default, Serialize, Deserialize)]
struct Record {
    incident: Option<Incident>,
    declared: BTreeMap<String, Declared>,
    /// Mount points of the volumes nodes were launched on, other than the Mac's own disk.
    #[serde(default)]
    volumes: BTreeSet<PathBuf>,
    /// Measured again for a minute after every start, so never saved.
    #[serde(skip)]
    mac: MacState,
    #[serde(default)]
    sampled_at: Millis,
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
    /// The incident is over: its declared waits are left to the fleet gate.
    Release(Vec<String>),
}

/// What a node hears when it declares a wait for the subscription's usage: pacing that is
/// Alondra's call (its gate and the usage card the user answers), and the real limit stops the
/// node by itself in Claude Code.
pub(crate) const USAGE_WAIT_REFUSED: &str = "wait_for_machine is only for the Mac's memory, disk or CPU. \
    Subscription usage (weekly or 5-hour limits, tokens, budget) is never a reason to wait: Alondra paces usage itself \
    and the user decides whether to keep going. Carry on with your task.";

/// What the gauges say is wrong. Once the disk was the cause it must climb past
/// `disk::recovered_line()` to count as fine, so a disk hovering at the line does not flap. Memory
/// and the CPU load open none: they only pace the fleet at the gate.
pub(crate) fn failing(machine: &Machine, previous: Option<MachineCause>) -> Option<MachineCause> {
    let line = if previous == Some(MachineCause::Disk) { crate::disk::recovered_line() } else { crate::disk::low_line() };
    (machine.disk_total > 0 && machine.disk_available < line).then_some(MachineCause::Disk)
}

fn card_for(cause: MachineCause, machine: &Machine, disk: Option<&str>, waiting: &[Waiter], minutes: Option<u64>) -> MachineCard {
    MachineCard {
        cause,
        disk_free: machine.disk_available,
        nodes: waiting.iter().map(|waiter| waiter.name.clone()).collect(),
        reason: (cause == MachineCause::Declared).then(|| waiting.first().map(|waiter| waiter.reason.clone())).flatten(),
        minutes,
        disk: disk.filter(|_| cause == MachineCause::Disk).map(str::to_owned),
        still: false,
    }
}

/// The machine with the disk gauge of its fullest volume: the Mac's own, or one a node works on,
/// named when it is not the Mac's own.
pub(crate) fn tightest(machine: Machine, volumes: &[(String, crate::disk::Space)]) -> (Machine, Option<String>) {
    match volumes.iter().min_by_key(|(_, space)| space.free) {
        Some((name, space)) if space.free < machine.disk_available || machine.disk_total == 0 =>
            (Machine { disk_available: space.free, disk_total: space.total, ..machine }, Some(name.clone())),
        _ => (machine, None),
    }
}

/// One sample of the incident: the gauges, the nodes waiting, and whether the user just said they
/// fixed it. Pure, so every turn of it is tested without a clock.
#[cfg(test)]
pub(crate) fn step(previous: Option<Incident>, machine: &Machine, waiting: &[Waiter], fixed: bool, now: Millis) -> (Option<Incident>, Vec<Action>) {
    step_on(previous, machine, None, waiting, fixed, now)
}

/// `step` with the name of the volume the disk gauge measured, when it is not the Mac's own.
pub(crate) fn step_on(previous: Option<Incident>, machine: &Machine, disk: Option<&str>, waiting: &[Waiter], fixed: bool, now: Millis) -> (Option<Incident>, Vec<Action>) {
    let measured = failing(machine, previous.as_ref().map(|incident| incident.cause));
    let Some(mut incident) = previous else {
        let Some(cause) = measured else { return (None, vec![]) };
        return open(cause, machine, disk, waiting, now);
    };
    let fine = measured.is_none();
    incident.healthy = if fine { incident.healthy + 1 } else { 0 };
    incident.fine_since = if fine { incident.fine_since.or(Some(now)) } else { None };
    let settled = incident.cause != MachineCause::Disk || incident.fine_since.is_some_and(|since| now - since >= DISK_SETTLE_MS);
    let declared_only = incident.cause == MachineCause::Declared;
    if declared_only || (fine && (fixed || (incident.healthy >= HEALTHY_SAMPLES && settled))) {
        let mut actions: Vec<Action> = incident.card.into_iter().map(Action::Withdraw).collect();
        if !waiting.is_empty() {
            actions.push(Action::Release(waiting.iter().map(|waiter| waiter.session.clone()).collect()));
        }
        return (None, actions);
    }
    let cause = measured.unwrap_or(incident.cause);
    let longest = waiting.iter().map(|waiter| now - waiter.at).max();
    let remind = !incident.reminded && longest.is_some_and(|waited| waited >= REMIND_AFTER_MS);
    let first_stop = incident.nodes_shown == 0 && !waiting.is_empty();
    let mut actions = vec![];
    if remind || first_stop || cause != incident.cause || fixed {
        let card = MachineCard { still: fixed, ..card_for(cause, machine, disk, waiting, remind.then(|| longest.unwrap_or(0) as u64 / 60_000)) };
        actions.extend(incident.card.take().map(Action::Withdraw));
        if remind {
            incident.reminded = true;
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

/// A problem the Mac's gauges measure opens an incident with its card. A wait a node declared
/// with no measured cause, such as a build queue, opens none: it is the node's state.
fn open(cause: MachineCause, machine: &Machine, disk: Option<&str>, waiting: &[Waiter], now: Millis) -> (Option<Incident>, Vec<Action>) {
    let card = card_for(cause, machine, disk, waiting, None);
    let id = format!("{MACHINE_PREFIX}{now}");
    let incident = Incident { since: now, cause, card: Some(id.clone()), cards: 1, nodes_shown: waiting.len(), reminded: false, healthy: 0, fine_since: None };
    (Some(incident), vec![Action::Raise { id, card }])
}

/// The volume the machine's own disk gauge measures.
fn own_disk() -> &'static Path {
    let data = Path::new(crate::machine::BOOT_DATA);
    if data.exists() { data } else { Path::new("/") }
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
    /// A memory incident an earlier daemon left open is dropped: `watch` then withdraws its card
    /// as a stray, and its declared waits stay with the fleet gate.
    pub(crate) fn persisted_in(home: &Path) -> Self {
        let file = home.join(FILE);
        let mut record: Record = std::fs::read(&file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
        if record.incident.take_if(|incident| incident.cause == MachineCause::Memory).is_some() {
            log!("dropping the memory incident an earlier daemon left open: memory no longer asks the user anything");
        }
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

    /// How the Mac is doing, as the gate reads it.
    pub(crate) fn mac(&self) -> MacState {
        self.record.lock().expect("machine watch poisoned").mac.clone()
    }

    /// Takes a new sample; returns whether there is room for one more node when that changed. A
    /// sample already taken changes nothing.
    fn sample(&self, machine: &Machine, now: Millis) -> Option<Result<(), NoRoom>> {
        let mut record = self.record.lock().expect("machine watch poisoned");
        if machine.at != 0 && machine.at == record.sampled_at {
            return None;
        }
        record.sampled_at = machine.at;
        let before = record.mac.room(0, now);
        record.mac.step(machine, now);
        let after = record.mac.room(0, now);
        (before.is_ok() != after.is_ok()).then_some(after)
    }

    fn home(&self) -> Option<&Path> {
        self.file.as_deref().and_then(Path::parent)
    }

    #[cfg(test)]
    pub(crate) fn set_disk_low(&self, low: bool) {
        let disk = Machine { disk_total: 500_000_000_000, disk_available: if low { 1_000_000_000 } else { 400_000_000_000 }, ..Machine::default() };
        self.set_incident(step(None, &disk, &[], false, 0).0);
    }

    pub(crate) fn disk_low(&self) -> bool {
        self.incident().is_some_and(|incident| incident.cause == MachineCause::Disk)
    }

    /// Why no node may start in `cwd` now, if the disk is nearly full there or on the Mac. The
    /// folder's volume is watched from now on, and measured at once so a launch never waits for
    /// the next sample; the watcher then puts the card in front of the user.
    pub(crate) fn refuse_launch(&self, cwd: &Path) -> Option<String> {
        self.watch_volume(cwd);
        let space = crate::disk::space(cwd);
        let low_here = space.is_some_and(|space| space.free < crate::disk::low_line());
        if !self.disk_low() && !low_here {
            return None;
        }
        if low_here {
            self.changed.notify_one();
        }
        let free = space.filter(|_| low_here).map(|space| format!(" ({} free where it works)", alondra_core::machine_incident::bytes_text(space.free, Lang::En))).unwrap_or_default();
        Some(format!("The disk is nearly full{free}: Alondra starts no node until there is space again, and {} has a card about it", crate::domain_messages::user_name()))
    }

    fn watch_volume(&self, dir: &Path) {
        let Some(mount) = crate::disk::mount_point(dir) else { return };
        if crate::disk::same_volume(&mount, own_disk()) {
            return;
        }
        let mut record = self.record.lock().expect("machine watch poisoned");
        if record.volumes.insert(mount) {
            self.save(&record);
        }
    }

    /// The free space of every watched volume still mounted, by name.
    fn volumes(&self) -> Vec<(String, crate::disk::Space)> {
        let volumes = self.record.lock().expect("machine watch poisoned").volumes.clone();
        volumes.iter().filter_map(|mount| {
            let space = crate::disk::space(mount)?;
            let name = mount.file_name().map_or_else(|| mount.display().to_string(), |name| name.to_string_lossy().into_owned());
            Some((name, space))
        }).collect()
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

/// Machine cards still open that are not the open incident's card, such as the ones an earlier
/// daemon raised for a wait no gauge measured: none of them asks the user anything real.
fn withdraw_strays(domain: &DomainRuntime) {
    let Ok(state) = domain.state() else { return };
    let current = domain.live.machine_watch.incident().filter(|incident| incident.cause != MachineCause::Declared).and_then(|incident| incident.card);
    for (id, card) in &state.interactions {
        let open = !card.withdrawn && matches!(card.payload, alondra_core::domain::InteractionPayload::Question { answer: None, .. });
        if open && id.0.starts_with(MACHINE_PREFIX) && current.as_deref() != Some(id.0.as_str()) {
            log!("withdrawing the stray machine card {}", id.0);
            let _ = domain.withdraw_problem(id);
        }
    }
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

/// Carries out one step's actions.
async fn act(domain: &DomainRuntime, alerts: &impl Alerts, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Raise { id, card } => {
                let (need, why) = card.text(Lang::En);
                let option = alondra_core::domain::QuestionOption { id: MACHINE_FIXED_OPTION.into(), label: MACHINE_FIXED_LABEL.into(), description: None, inputs: vec![] };
                if let Err(error) = domain.raise_worded_problem(&id, MACHINE_CAUSE, need, why, card.to_body(), vec![option], None) {
                    log!("could not put the machine's card in front of the user, pushing it instead: {error:?}");
                    alerts.tell(push_text(card), false).await;
                }
            }
            Action::Withdraw(id) => {
                let _ = domain.withdraw_problem(&alondra_core::domain::InteractionId(id));
            }
            Action::Push(card) => alerts.tell(push_text(card), false).await,
            Action::Release(sessions) => {
                log!("the machine recovered: {} node(s) that waited come back through the fleet gate", sessions.len());
                domain.live.machine_watch.forget(&sessions);
            }
        }
    }
}

/// A new sample of the Mac: kept in the samples file, and when memory gains or loses room for one
/// more node, one line in the log and a look of the gate.
pub(crate) fn pace(domain: &DomainRuntime, machine: &Machine) {
    let watch = &domain.live.machine_watch;
    let now = if machine.at == 0 { now_millis() } else { machine.at };
    let change = watch.sample(machine, now);
    let mac = watch.mac();
    let working = domain.state().map(|state| crate::fleet_gate::working_tasks(&state).len()).unwrap_or(0);
    if let Some(home) = watch.home() {
        crate::machine_state::record(&home.join(crate::machine_state::SAMPLES_DIR), now, &mac, working);
    }
    let Some(room) = change else { return };
    let free = mac.free_percent.map_or_else(|| "unknown".into(), |free| format!("{free}%"));
    match room {
        Ok(()) => log!("memory has room for one more node again ({free} free, {working} node(s) working)"),
        Err(why) => log!("memory has no room for one more node: {why} ({free} free, {working} node(s) working); running nodes go on"),
    }
    domain.live.gate.poke();
}

/// Looks at the machine on every sample and whenever a node declares a wait or the user answers.
pub(crate) async fn run(domain: std::sync::Arc<DomainRuntime>, gauges: crate::machine::Gauges, alerts: impl Alerts) {
    watch(domain, move || gauges.lock().expect("machine gauges poisoned").as_ref().map(|(machine, _)| machine.clone()), alerts, TICK).await
}

async fn watch(domain: std::sync::Arc<DomainRuntime>, sample: impl Fn() -> Option<Machine>, alerts: impl Alerts, tick: Duration) {
    withdraw_strays(&domain);
    let mut cards = domain.subscribe();
    loop {
        if let Some(machine) = sample() {
            let watch = &domain.live.machine_watch;
            pace(&domain, &machine);
            let (machine, disk) = tightest(machine, &watch.volumes());
            let previous = watch.incident();
            let fixed = fixed(&domain, previous.as_ref());
            let waiting = waiters(&domain);
            let now = now_millis();
            let (next, actions) = step_on(previous, &machine, disk.as_deref(), &waiting, fixed, now);
            watch.set_incident(next);
            if fixed {
                domain.live.gate.release_declared(&domain);
            }
            act(&domain, &alerts, actions).await;
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
        Machine { at: 0, memory_total: 100, memory_available: 50, memory_free_percent: Some(memory_free), memory_pressure: None, compressor: None, load: [1.0; 3], cpus: 8,
            cpu_usage: 10.0, disk_total: 500 * GB, disk_available: disk_free, unreliable: false }
    }

    fn waiter(session: &str, name: &str, at: Millis) -> Waiter {
        Waiter { session: session.into(), name: name.into(), reason: "disk full".into(), at }
    }

    fn raised(actions: &[Action]) -> Vec<&MachineCard> {
        actions.iter().filter_map(|action| match action { Action::Raise { card, .. } => Some(card), _ => None }).collect()
    }

    #[test]
    fn a_full_disk_with_a_stopped_node_raises_one_card() {
        let phase0 = [waiter("s1", "Fase 0: quitar versión de identificadores", 0)];
        let (incident, actions) = step(None, &machine(0, 50), &phase0, false, 1_000);
        let incident = incident.unwrap();
        assert_eq!((incident.cause, incident.card.as_deref()), (MachineCause::Disk, Some("machine-1000")));
        let cards = raised(&actions);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].text(Lang::Es).0, "Libera espacio en disco: quedan 0 B");
        assert_eq!(cards[0].nodes, ["Fase 0: quitar versión de identificadores"]);

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
    fn a_volume_nodes_work_on_running_low_opens_the_card_and_names_it_while_the_macs_own_disk_is_fine() {
        let external = [("Externo".to_owned(), crate::disk::Space { free: 3 * GB, total: 900 * GB }),
            ("Fotos".to_owned(), crate::disk::Space { free: 400 * GB, total: 900 * GB })];
        let (measured, disk) = tightest(machine(80 * GB, 50), &external);
        assert_eq!((measured.disk_available, disk.as_deref()), (3 * GB, Some("Externo")));
        let (incident, actions) = step_on(None, &measured, disk.as_deref(), &[], false, 0);
        assert_eq!(incident.unwrap().cause, MachineCause::Disk);
        assert_eq!(raised(&actions)[0].text(Lang::Es).0, "Libera espacio en disco: solo quedan 3,0 GB en Externo");

        let (measured, disk) = tightest(machine(2 * GB, 50), &external);
        assert_eq!((measured.disk_available, disk), (2 * GB, None), "the Mac's own disk is the fuller one: not named");
        assert_eq!(tightest(machine(80 * GB, 50), &[]).1, None);
    }

    #[test]
    fn no_node_starts_while_the_disk_is_low_and_one_does_once_it_recovers() {
        let watch = MachineWatch::default();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        assert_eq!(watch.refuse_launch(&here), None, "a healthy disk starts nodes");
        let (incident, _) = step(None, &machine(GB, 50), &[], false, 0);
        watch.set_incident(incident);
        let refused = watch.refuse_launch(&here).expect("refused while the disk is low");
        assert!(refused.starts_with("The disk is nearly full: Alondra starts no node until there is space again, and "), "{refused}");
        assert!(refused.ends_with(" has a card about it"), "{refused}");
        watch.set_incident(None);
        assert_eq!(watch.refuse_launch(&here), None);
    }

    #[test]
    fn a_launch_on_another_volume_watches_it_from_then_on() {
        let watch = MachineWatch::default();
        let here = crate::test_support::Directory::new();
        std::fs::create_dir(&here).unwrap();
        watch.refuse_launch(&here);
        let mount = crate::disk::mount_point(&here).unwrap();
        let watched = watch.record.lock().unwrap().volumes.clone();
        if crate::disk::same_volume(&mount, own_disk()) {
            assert!(watched.is_empty(), "the Mac's own disk already has its gauge: {watched:?}");
        } else {
            assert_eq!(watched.into_iter().collect::<Vec<_>>(), [mount]);
            assert_eq!(watch.volumes().len(), 1);
        }
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
            } else {
                assert!(actions.is_empty(), "nothing else at minute {minute}: {actions:?}");
            }
            incident = next;
        }
        assert_eq!(reminders, 1, "only once");
    }

    #[test]
    fn the_disk_must_stay_past_the_recovery_line_to_release_everyone() {
        let stopped = [waiter("s1", "Lumo", 0), waiter("s2", "Quill", 0)];
        let (incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, actions) = step(incident, &machine(6 * GB, 50), &stopped, false, MIN);
        assert!(actions.is_empty() && incident.is_some(), "6 GB is above the warning yet below the recovery line");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 2 * MIN);
        assert!(actions.is_empty() && incident.is_some(), "one healthy sample is not enough");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 3 * MIN);
        assert!(actions.is_empty() && incident.is_some(), "a disk fine for a minute may fall again");
        let (incident, actions) = step(incident, &machine(8 * GB, 50), &stopped, false, 2 * MIN + DISK_SETTLE_MS);
        assert!(incident.is_none());
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()));
        assert!(actions.contains(&Action::Release(vec!["s1".into(), "s2".into()])));
    }

    /// 8 Oct 2026: the disk climbed past the recovery line for two samples and fell again, and
    /// a second card («3.8 GB left») joined the first («4.8 GB left») three minutes later.
    #[test]
    fn a_disk_that_recovers_briefly_and_falls_again_keeps_its_one_card() {
        let (mut incident, actions) = step(None, &machine(4_800_000_000, 50), &[], false, 0);
        assert_eq!(raised(&actions).len(), 1);
        let samples = [(8 * GB, MIN), (8 * GB, MIN + 10_000), (8 * GB, MIN + 20_000), (3_800_000_000, 3 * MIN)];
        for (free, at) in samples {
            let (next, actions) = step(incident, &machine(free, 50), &[], false, at);
            assert!(actions.is_empty(), "no card withdrawn or raised at {at}: {actions:?}");
            incident = next;
        }
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-0"));
    }

    #[test]
    fn a_wait_no_gauge_measures_is_the_nodes_state_never_a_card() {
        let queued = [Waiter { reason: "build queue full".into(), ..waiter("s1", "Quill", 0) }];
        for minute in 0..60 {
            let (incident, actions) = step(None, &machine(100 * GB, 50), &queued, false, minute * MIN);
            assert!(incident.is_none() && actions.is_empty(), "a wait no gauge measures is the node's state, never a card: {actions:?}");
        }
    }

    /// The morning of 8 October: a node waiting in the Bazel queue declared wait_for_machine, the
    /// user answered "I've fixed it", the node was woken, declared again, and a new card came out,
    /// three times in a row. A wait the Mac's gauges do not measure now puts no card at all, and an
    /// incident left from before closes with its card withdrawn.
    #[test]
    fn a_node_declaring_again_after_i_have_fixed_it_puts_no_card_in_front_of_the_user() {
        let queued = [Waiter { reason: "waiting in the Bazel queue behind make ablation".into(), ..waiter("s1", "Ritmo 2", 0) }];
        let mut incident = None;
        let mut cards = 0;
        for (round, fixed) in [(0, false), (1, true), (2, false), (3, true), (4, false), (5, true)] {
            let (next, actions) = step(incident, &machine(100 * GB, 50), &queued, fixed, round * MIN + 1_000);
            cards += raised(&actions).len();
            incident = next;
        }
        assert_eq!((cards, incident), (0, None));

        let left = Incident { since: 0, cause: MachineCause::Declared, card: Some("machine-1791441349131".into()), cards: 3, nodes_shown: 1, reminded: false, healthy: 0, fine_since: None };
        let (incident, actions) = step(Some(left), &machine(100 * GB, 50), &queued, false, MIN);
        assert!(incident.is_none());
        assert_eq!(actions, [Action::Withdraw("machine-1791441349131".into()), Action::Release(vec!["s1".into()])]);
    }

    #[test]
    fn i_have_fixed_it_releases_at_once_when_the_mac_is_fine_and_says_it_still_is_when_it_is_not() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (incident, _) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, actions) = step(incident, &machine(GB / 2, 50), &stopped, true, MIN);
        assert_eq!(actions[0], Action::Withdraw("machine-0".into()), "the answered card goes");
        let cards = raised(&actions);
        assert_eq!(cards[0].text(Lang::Es).0, "Sigue: quedan 500 MB libres", "still full: a new card that says so");
        assert_eq!(cards[0].text(Lang::En).0, "Still: 500 MB free left");
        let (incident, actions) = step(incident, &machine(50 * GB, 50), &stopped, true, 2 * MIN);
        assert!(incident.is_none(), "no second sample needed once the user says so");
        assert!(actions.contains(&Action::Release(vec!["s1".into()])));
    }

    #[test]
    fn a_disk_that_fills_again_after_it_was_freed_gets_a_new_card() {
        let stopped = [waiter("s1", "Lumo", 0)];
        let (incident, first) = step(None, &machine(0, 50), &stopped, false, 0);
        let (incident, _) = step(incident, &machine(50 * GB, 50), &stopped, true, MIN);
        assert!(incident.is_none(), "freed and answered: the incident closes");
        let (incident, again) = step(None, &machine(GB, 50), &stopped, false, 60 * MIN);
        assert_eq!(raised(&first).len() + raised(&again).len(), 2);
        assert_eq!(incident.unwrap().card.as_deref(), Some("machine-3600000"), "its own card, though the last one was answered");
    }

    /// 9 Oct 2026: «Libera memoria: macOS avisa de presión de memoria (38% libre)», with
    /// «Responder al orquestador», asked the user for what Alondra already does by holding new
    /// starts. Memory, however long and however hard, opens no incident.
    #[test]
    fn memory_under_pressure_for_hours_with_nodes_waiting_opens_no_incident_and_no_card() {
        let stopped = [Waiter { reason: "killed for lack of memory".into(), ..waiter("s1", "Lumo", 0) }];
        let mut mac = MacState::default();
        let mut incident = None;
        for sample in 0..(3 * 60 * 6) {
            let now = sample * 10_000;
            let pressed = Machine { at: now, memory_pressure: Some(MemoryPressure::Critical), ..machine(100 * GB, 3) };
            mac.step(&pressed, now);
            let (next, actions) = step(incident, &pressed, &stopped, false, now);
            assert!(next.is_none() && actions.is_empty(), "no incident, card or push at {now}: {actions:?}");
            incident = next;
        }
        assert_eq!(mac.room(0, 3 * 60 * MIN), Err(NoRoom::Pressure(MemoryPressure::Critical)), "the gauges still see the pressure, and the gate holds new starts by them");
    }

    #[test]
    fn two_nodes_stopped_by_the_same_full_disk_share_one_card_that_names_both() {
        let (incident, _) = step(None, &machine(0, 50), &[waiter("s1", "Lumo", 0)], false, 0);
        let both = [waiter("s1", "Lumo", 0), waiter("s2", "Quill", 1_000)];
        let (incident, actions) = step(incident, &machine(0, 50), &both, false, 10_000);
        assert!(actions.is_empty(), "{actions:?}");
        let (_, actions) = step(incident, &machine(0, 50), &both, false, 31 * MIN);
        assert_eq!(raised(&actions)[0].nodes, ["Lumo", "Quill"], "the reminder names both");
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
    async fn a_full_disk_puts_one_card_tells_the_orchestrator_nothing_and_i_have_fixed_it_withdraws_it() {
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
        assert!(orchestrator_inbox(&domain).is_empty(), "the orchestrator has nothing to do about it: {:?}", orchestrator_inbox(&domain));
        assert!(told.0.lock().unwrap().is_empty(), "the card got through: no push in its place");

        *disk.lock().unwrap() = 50 * GB;
        let state = domain.state().unwrap();
        domain.execute(&Actor::User { user_id: "device:phone".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "fixed".into(), expected_revision: state.revision,
            command: Command::AnswerQuestion { attachments: vec![], interaction_id: cards[0].id.clone(), option_id: MACHINE_FIXED_OPTION.into(), inputs: Default::default() },
        }, None, || Ok(())).unwrap();
        until("the incident closed", || domain.live.machine_watch.incident().is_none()).await;
        watcher.abort();
        assert!(open_machine_cards(&domain).is_empty());
        assert!(node.texts().is_empty(), "the fleet gate brings the node back, not the machine watch");
        assert!(orchestrator_inbox(&domain).is_empty(), "{:?}", orchestrator_inbox(&domain));
    }

    #[tokio::test]
    async fn a_cpu_load_opens_no_incident_no_card_and_no_notice() {
        let (_home, domain) = world();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let watcher = tokio::spawn(watch(domain.clone(), || {
            let mut sample = machine(50 * GB, 50);
            sample.load = [90.0; 3];
            sample.unreliable = true;
            Some(sample)
        }, told.clone(), Duration::from_millis(20)));
        tokio::time::sleep(Duration::from_millis(200)).await;
        watcher.abort();
        assert!(domain.live.machine_watch.incident().is_none());
        assert!(open_machine_cards(&domain).is_empty());
        assert!(orchestrator_inbox(&domain).is_empty(), "{:?}", orchestrator_inbox(&domain));
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn when_the_card_cannot_be_written_the_user_hears_it_by_push() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("fase0", "disk full", 0, now_millis());
        let told = Arc::new(Told(Mutex::new(vec![])));
        let incident = Incident { since: 0, cause: MachineCause::Disk, card: None, cards: 1, nodes_shown: 0, reminded: false, healthy: 0, fine_since: None };
        domain.live.machine_watch.set_incident(Some(incident));
        // The card's id is taken already, so the domain refuses it as a full disk would.
        domain.raise_worded_problem("machine-0-2", MACHINE_CAUSE, "taken".into(), "taken".into(), "{}".into(), vec![
            alondra_core::domain::QuestionOption { id: "x".into(), label: "x".into(), description: None, inputs: vec![] },
        ], None).unwrap();
        let watcher = tokio::spawn(watch(domain.clone(), || Some(machine(0, 50)), told.clone(), Duration::from_millis(20)));
        until("the push", || !told.0.lock().unwrap().is_empty()).await;
        watcher.abort();
        assert_eq!(told.0.lock().unwrap()[0], "Libera espacio en disco: quedan 0 B. Disco casi lleno (0 B libres). Alondra no lanzará nodos nuevos hasta que haya espacio. Parados: fase0. Continúan solos cuando se recupere.");
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

    fn pressed(at: Millis, under: bool) -> Machine {
        let free = if under { 3 } else { 60 };
        Machine { at, memory_pressure: Some(if under { MemoryPressure::Critical } else { MemoryPressure::Normal }), ..machine(100 * GB, free) }
    }

    fn holds(domain: &DomainRuntime, now: Millis) -> bool {
        let head = crate::fleet_gate::Waiting { key: crate::fleet_gate::Key::Task("t".into()), priority: Default::default(), resuming: false, since: 0 };
        crate::fleet_gate::decide(&head, 0, 4, &domain.live.machine_watch.mac(), 0, None, 0, now).is_err()
    }

    #[tokio::test]
    async fn sustained_memory_pressure_puts_no_card_and_no_push_while_the_fleet_is_held_and_resumed() {
        let (_home, domain) = world();
        domain.live.machine_watch.declare("s1", "killed for lack of memory", 0, now_millis());
        let clock = Arc::new(Mutex::new((0 as Millis, true)));
        let reading = clock.clone();
        let told = Arc::new(Told(Mutex::new(vec![])));
        let watcher = tokio::spawn(watch(domain.clone(), move || {
            let mut clock = reading.lock().unwrap();
            clock.0 += 10_000;
            Some(pressed(clock.0, clock.1))
        }, told.clone(), Duration::from_millis(1)));

        let now = || clock.lock().unwrap().0;
        until("a minute of pressure", || now() >= 2 * MIN).await;
        assert!(holds(&domain, now()), "the gate holds starts while memory has no room");
        until("an hour of pressure", || now() >= 60 * MIN).await;
        assert!(holds(&domain, now()));
        assert!(domain.live.machine_watch.incident().is_none());
        assert!(open_machine_cards(&domain).is_empty(), "{:?}", open_machine_cards(&domain));
        assert!(told.0.lock().unwrap().is_empty(), "no push: {:?}", told.0.lock().unwrap());
        assert!(orchestrator_inbox(&domain).is_empty(), "pacing asks nothing of the orchestrator: {:?}", orchestrator_inbox(&domain));

        clock.lock().unwrap().1 = false;
        let back = now();
        until("a minute of room", || now() >= back + 2 * MIN).await;
        watcher.abort();
        assert!(!holds(&domain, now()), "the gate lets nodes start and resume once memory is back");
        assert!(orchestrator_inbox(&domain).is_empty(), "{:?}", orchestrator_inbox(&domain));
        let state = domain.state().unwrap();
        let cards = state.interactions.values().filter(|card| card.id.0.starts_with(MACHINE_PREFIX) && matches!(card.payload, InteractionPayload::Question { .. })).count();
        assert_eq!(cards, 0, "never a machine card, open or withdrawn");
        assert!(told.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_new_daemon_retires_the_memory_card_an_earlier_one_left_open() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let left = Incident { since: 1_000, cause: MachineCause::Memory, card: Some("machine-1000".into()), cards: 1, nodes_shown: 0, reminded: false, healthy: 0, fine_since: None };
        std::fs::write(home.join(FILE), serde_json::json!({"incident": left, "declared": {}}).to_string()).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let body = r#"{"cause":"memory","disk_free":0,"memory_free_percent":38,"memory_pressure":true,"single_node":true}"#;
        domain.raise_worded_problem("machine-1000", MACHINE_CAUSE, "Free up memory: macOS reports memory pressure (38% free) with a single node working".into(), "something outside Alondra uses the memory".into(), body.into(), vec![
            alondra_core::domain::QuestionOption { id: MACHINE_FIXED_OPTION.into(), label: MACHINE_FIXED_LABEL.into(), description: None, inputs: vec![] },
        ], None).unwrap();
        assert_eq!(open_machine_cards(&domain).len(), 1);
        assert!(domain.live.machine_watch.incident().is_none(), "the memory incident is dropped on load");

        let told = Arc::new(Told(Mutex::new(vec![])));
        let watcher = tokio::spawn(watch(domain.clone(), || Some(pressed(0, true)), told.clone(), Duration::from_millis(20)));
        until("the memory card withdrawn", || open_machine_cards(&domain).is_empty()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        watcher.abort();
        assert!(open_machine_cards(&domain).is_empty(), "no new card in its place");
        assert!(told.0.lock().unwrap().is_empty());
    }
}
