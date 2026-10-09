//! How the Mac is doing, with hysteresis: memory decides between Fine, Tight and AtLimit, and the
//! CPU load only decides whether new nodes may start. Entering pressure is quick (30 s, or one
//! sample under 5 %); counting the Mac as recovered takes 20 % free without a break for
//! `RECOVERY_WINDOW`, so memory swinging between 9 % and 16 % or 22 % never looks like a recovery.
//! The pressure level macOS reports counts too, since it follows the compressor and swap and can
//! be at warning with a third of memory free: warning for 30 s is Tight, warning for 2 min or one
//! critical sample is AtLimit, and recovering also needs the pressure back to normal.
//! See docs/fleet-pacing.md.
use alondra_core::{Machine, MemoryPressure, Millis};
use serde::{Deserialize, Serialize};

const TIGHT_BELOW: u32 = 15;
const AT_LIMIT_BELOW: u32 = 10;
const AT_ONCE_BELOW: u32 = 5;
const FINE_FROM: u32 = 20;
const PRESSURE_HOLDS: Millis = 30_000;
const WARNING_HOLDS: Millis = 2 * 60_000;
pub(crate) const RECOVERY_WINDOW: Millis = 5 * 60_000;
const LOAD_BUSY_PER_CORE: f64 = 4.0;
const LOAD_CALM_PER_CORE: f64 = 3.0;
const LOAD_BUSY_HOLDS: Millis = 60_000;
const LOAD_CALM_HOLDS: Millis = 2 * 60_000;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Health {
    #[default]
    Fine,
    Tight,
    AtLimit,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub(crate) struct MacState {
    pub health: Health,
    /// No new node starts while the CPU load stays this high.
    pub cpu_busy: bool,
    pub free_percent: Option<u32>,
    #[serde(default)]
    pub pressure: Option<MemoryPressure>,
    pub load: f64,
    pub cpus: usize,
    below_tight: Option<Millis>,
    below_limit: Option<Millis>,
    from_limit: Option<Millis>,
    from_fine: Option<Millis>,
    #[serde(default)]
    warned: Option<Millis>,
    #[serde(default)]
    calm: Option<Millis>,
    load_high: Option<Millis>,
    load_low: Option<Millis>,
}

fn held(since: &mut Option<Millis>, holds: bool, now: Millis) -> Millis {
    if holds {
        now - *since.get_or_insert(now)
    } else {
        *since = None;
        -1
    }
}

impl MacState {
    /// Takes one sample and returns the change of health it caused, if any.
    pub(crate) fn step(&mut self, machine: &Machine, now: Millis) -> Option<(Health, Health)> {
        self.step_with(machine, now, RECOVERY_WINDOW)
    }

    pub(crate) fn step_with(&mut self, machine: &Machine, now: Millis, recovery: Millis) -> Option<(Health, Health)> {
        self.step_load(machine, now);
        let free = machine.memory_free_percent?;
        self.free_percent = Some(free);
        self.pressure = machine.memory_pressure;
        let pressed = matches!(machine.memory_pressure, Some(MemoryPressure::Warning | MemoryPressure::Critical));
        let warned = held(&mut self.warned, pressed, now);
        let calm = held(&mut self.calm, !pressed, now);
        let tight = held(&mut self.below_tight, free < TIGHT_BELOW, now).max(warned);
        let limit = held(&mut self.below_limit, free < AT_LIMIT_BELOW, now);
        let out_of_limit = held(&mut self.from_limit, free >= AT_LIMIT_BELOW, now).min(calm);
        let fine = held(&mut self.from_fine, free >= FINE_FROM, now).min(calm);
        let to_limit = free < AT_ONCE_BELOW
            || limit >= PRESSURE_HOLDS
            || machine.memory_pressure == Some(MemoryPressure::Critical)
            || warned >= WARNING_HOLDS;
        let next = match self.health {
            Health::Fine if to_limit => Health::AtLimit,
            Health::Fine if tight >= PRESSURE_HOLDS => Health::Tight,
            Health::Tight if to_limit => Health::AtLimit,
            Health::Tight if fine >= recovery => Health::Fine,
            Health::AtLimit if out_of_limit >= PRESSURE_HOLDS => if fine >= recovery { Health::Fine } else { Health::Tight },
            health => health,
        };
        let before = std::mem::replace(&mut self.health, next);
        (before != next).then_some((before, next))
    }

    /// How long memory has been at 20 % free or more, and macOS without pressure, without a break.
    pub(crate) fn fine_for(&self, now: Millis) -> Option<Millis> {
        Some(now - self.from_fine?.max(self.calm?))
    }

    fn step_load(&mut self, machine: &Machine, now: Millis) {
        if machine.cpus == 0 {
            return;
        }
        let per_core = machine.load[0] / machine.cpus as f64;
        self.load = machine.load[0];
        self.cpus = machine.cpus;
        let high = held(&mut self.load_high, per_core > LOAD_BUSY_PER_CORE, now);
        let low = held(&mut self.load_low, per_core < LOAD_CALM_PER_CORE, now);
        if !self.cpu_busy && high >= LOAD_BUSY_HOLDS {
            self.cpu_busy = true;
        } else if self.cpu_busy && low >= LOAD_CALM_HOLDS {
            self.cpu_busy = false;
        }
    }
}

/// Where every sample is kept for a week, one CSV file a day, to tune the thresholds with this
/// Mac's own numbers.
pub(crate) const SAMPLES_DIR: &str = "machine-samples";
const KEEP_DAYS: Millis = 7 * 24 * 60 * 60_000;

pub(crate) fn record(dir: &std::path::Path, now: Millis, mac: &MacState, working: usize) {
    use std::io::Write;
    let day = crate::node_reports::local_day(now);
    let file = dir.join(format!("{day}.csv"));
    let new_day = !file.exists();
    let written = std::fs::create_dir_all(dir).and_then(|()| {
        let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&file)?;
        if new_day {
            writeln!(out, "at,memory_free_percent,load_1m,cpus,health,cpu_busy,working,memory_pressure")?;
        }
        let free = mac.free_percent.map(|free| free.to_string()).unwrap_or_default();
        let health = serde_json::to_value(mac.health).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        let pressure = mac.pressure.and_then(|pressure| serde_json::to_value(pressure).ok()).and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        writeln!(out, "{now},{free},{:.2},{},{health},{},{working},{pressure}", mac.load, mac.cpus, mac.cpu_busy)
    });
    if let Err(error) = written {
        log!("could not record the machine sample in {}: {error}", file.display());
        return;
    }
    if new_day {
        let oldest = format!("{}.csv", crate::node_reports::local_day(now - KEEP_DAYS));
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".csv") && name < oldest {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: Millis = 10_000;

    fn machine(free: u32) -> Machine {
        Machine { memory_free_percent: Some(free), cpus: 11, load: [5.0, 5.0, 5.0], ..Machine::default() }
    }

    fn pressed(free: u32, pressure: MemoryPressure) -> Machine {
        Machine { memory_pressure: Some(pressure), ..machine(free) }
    }

    /// Feeds `free` under `pressure` every 10 s for `span` from `from`; returns the changes and the next time.
    fn feed_pressed(state: &mut MacState, free: u32, pressure: MemoryPressure, from: Millis, span: Millis) -> (Vec<(Health, Health)>, Millis) {
        let mut changes = vec![];
        let mut now = from;
        while now < from + span {
            changes.extend(state.step(&pressed(free, pressure), now));
            now += SAMPLE;
        }
        (changes, now)
    }

    /// Feeds `free` every 10 s for `span` from `from`; returns the changes and the next time.
    fn feed(state: &mut MacState, free: u32, from: Millis, span: Millis) -> (Vec<(Health, Health)>, Millis) {
        let mut changes = vec![];
        let mut now = from;
        while now < from + span {
            changes.extend(state.step(&machine(free), now));
            now += SAMPLE;
        }
        (changes, now)
    }

    #[test]
    fn memory_under_15_percent_for_30_seconds_is_tight() {
        let mut state = MacState::default();
        let (changes, _) = feed(&mut state, 14, 0, 30_000);
        assert!(changes.is_empty(), "20 s under 15 % is not yet tight");
        assert_eq!(state.step(&machine(14), 30_000), Some((Health::Fine, Health::Tight)));
    }

    #[test]
    fn under_10_percent_for_30_seconds_or_under_5_once_is_at_limit() {
        let mut state = MacState::default();
        feed(&mut state, 9, 0, 30_000);
        assert_eq!(state.step(&machine(9), 30_000), Some((Health::Fine, Health::AtLimit)));
        let mut state = MacState::default();
        assert_eq!(state.step(&machine(4), 0), Some((Health::Fine, Health::AtLimit)));
    }

    #[test]
    fn at_limit_goes_back_to_tight_after_30_seconds_at_10_percent() {
        let mut state = MacState::default();
        state.step(&machine(3), 0);
        let (changes, _) = feed(&mut state, 12, SAMPLE, 40_000);
        assert_eq!(changes, vec![(Health::AtLimit, Health::Tight)]);
    }

    #[test]
    fn fine_again_only_after_5_minutes_at_20_percent_without_a_break() {
        let mut state = MacState::default();
        state.step(&machine(3), 0);
        let (changes, now) = feed(&mut state, 25, SAMPLE, 4 * 60_000);
        assert_eq!(changes, vec![(Health::AtLimit, Health::Tight)]);
        let (_, now) = feed(&mut state, 18, now, SAMPLE);
        let (changes, now) = feed(&mut state, 25, now, 4 * 60_000 + 50_000);
        assert!(changes.is_empty(), "the dip to 18 % started the window again");
        let (changes, _) = feed(&mut state, 25, now, 20_000);
        assert_eq!(changes, vec![(Health::Tight, Health::Fine)]);
    }

    #[test]
    fn the_cpu_closes_starts_after_a_minute_over_4_per_core_and_opens_after_2_minutes_under_3() {
        let mut state = MacState::default();
        let busy = |load: f64| Machine { load: [load, load, load], ..machine(50) };
        let mut now = 0;
        while now < 60_000 {
            state.step(&busy(50.0), now);
            assert!(!state.cpu_busy);
            now += SAMPLE;
        }
        state.step(&busy(50.0), now);
        assert!(state.cpu_busy);
        now += SAMPLE;
        let calm_from = now;
        while now < calm_from + 120_000 {
            state.step(&busy(30.0), now);
            assert!(state.cpu_busy);
            now += SAMPLE;
        }
        state.step(&busy(30.0), now);
        assert!(!state.cpu_busy);
    }

    #[test]
    fn no_memory_reading_changes_nothing() {
        let mut state = MacState::default();
        state.step(&machine(3), 0);
        assert_eq!(state.step(&Machine { memory_free_percent: None, ..machine(0) }, SAMPLE), None);
        assert_eq!(state.health, Health::AtLimit);
    }

    #[test]
    fn a_macos_warning_with_31_percent_free_is_tight_after_30_seconds_and_at_limit_after_2_minutes() {
        let mut state = MacState::default();
        let (changes, now) = feed_pressed(&mut state, 31, MemoryPressure::Warning, 0, 30_000);
        assert!(changes.is_empty(), "20 s of warning is not yet tight");
        let (changes, now) = feed_pressed(&mut state, 31, MemoryPressure::Warning, now, SAMPLE);
        assert_eq!(changes, vec![(Health::Fine, Health::Tight)]);
        let (changes, now) = feed_pressed(&mut state, 31, MemoryPressure::Warning, now, WARNING_HOLDS - 40_000);
        assert!(changes.is_empty(), "under 2 min of warning stays tight");
        assert_eq!(state.step(&pressed(31, MemoryPressure::Warning), now), Some((Health::Tight, Health::AtLimit)));
        assert_eq!(state.pressure, Some(MemoryPressure::Warning));
    }

    #[test]
    fn one_critical_sample_is_at_limit_and_a_short_warning_changes_nothing() {
        let mut state = MacState::default();
        assert_eq!(state.step(&pressed(40, MemoryPressure::Critical), 0), Some((Health::Fine, Health::AtLimit)));
        let mut state = MacState::default();
        let (_, now) = feed_pressed(&mut state, 40, MemoryPressure::Warning, 0, 20_000);
        let (changes, _) = feed_pressed(&mut state, 40, MemoryPressure::Normal, now, 5 * 60_000);
        assert!(changes.is_empty());
        assert_eq!(state.health, Health::Fine);
    }

    #[test]
    fn plenty_of_memory_free_is_no_recovery_until_macos_has_no_pressure_for_5_minutes() {
        let mut state = MacState::default();
        state.step(&pressed(45, MemoryPressure::Critical), 0);
        let (changes, now) = feed_pressed(&mut state, 45, MemoryPressure::Warning, SAMPLE, 10 * 60_000);
        assert!(changes.is_empty(), "45 % free under warning stays at its limit: {changes:?}");
        assert_eq!(state.fine_for(now), None);
        let (changes, now) = feed_pressed(&mut state, 45, MemoryPressure::Normal, now, 4 * 60_000);
        assert_eq!(changes, vec![(Health::AtLimit, Health::Tight)]);
        let (_, now) = feed_pressed(&mut state, 45, MemoryPressure::Warning, now, SAMPLE);
        let (changes, now) = feed_pressed(&mut state, 45, MemoryPressure::Normal, now, 4 * 60_000 + 50_000);
        assert!(changes.is_empty(), "the warning started the window again");
        let (changes, _) = feed_pressed(&mut state, 45, MemoryPressure::Normal, now, 20_000);
        assert_eq!(changes, vec![(Health::Tight, Health::Fine)]);
    }

    #[test]
    fn the_pressure_level_goes_into_the_samples_file() {
        let dir = crate::test_support::Directory::new();
        let mut state = MacState::default();
        state.step(&pressed(31, MemoryPressure::Warning), 0);
        record(&dir, 1_791_500_000_000, &state, 6);
        let day = std::fs::read_dir(&*dir).unwrap().next().unwrap().unwrap().path();
        let text = std::fs::read_to_string(day).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("at,memory_free_percent,load_1m,cpus,health,cpu_busy,working,memory_pressure"));
        assert_eq!(lines.next(), Some("1791500000000,31,5.00,11,fine,false,6,warning"));
    }
}
