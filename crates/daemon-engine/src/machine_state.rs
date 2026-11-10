//! Whether the Mac has room for one more node. Memory never stops what already runs: it only
//! answers, for the admission gate, whether one more node may start or a paused one resume.
//!
//! There is room when, over the last `ROOM_WINDOW`, macOS never reported critical pressure and
//! free memory, less what the node that starts and each node admitted in the last 3 minutes cost
//! (`node_cost`), stayed at `FREE_FLOOR` or above, and at `SWAPPING_FLOOR` or above while macOS
//! swapped out `SWAPPING` or more. macOS's warning alone never holds a start: it lingers for hours
//! with a third or half of memory free, so it only spaces starts out (`WARNING_SPACING`). Available
//! bytes must also cover the floor and pending nodes. Swap growth holds starts regardless of the
//! apparent free percentage until a full minute of fresh measurements shows it stable.
//!
//! A daemon that just started judges the samples it has. The CPU load holds nothing.
use std::collections::{BTreeMap, VecDeque};
use alondra_core::domain::AgentId;

use alondra_core::{Machine, MemoryCompressor, MemoryPressure, Millis};

/// How long memory must have had room before one more node starts.
pub(crate) const ROOM_WINDOW: Millis = 60_000;
/// Under macOS's warning, one start every this long, so the memory of each shows before the next.
pub(crate) const WARNING_SPACING: Millis = 3 * 60_000;
const GIB: i64 = 1 << 30;
const MIB: u64 = 1 << 20;
/// Free memory no start may take the Mac under.
const FREE_FLOOR: u32 = 20;
/// Free memory no start may take the Mac under while macOS swaps out.
const SWAPPING_FLOOR: u32 = 30;
/// What macOS must swap out within `ROOM_WINDOW` to count as swapping.
const SWAPPING: u64 = 64 * MIB;

/// Why one more node may not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoRoom {
    Unmeasured,
    /// macOS reports critical pressure.
    Pressure(MemoryPressure),
    /// Free memory, and what it must be for `nodes` more at `cost` each to leave `floor` free.
    Free { free: u32, needed: u32, floor: u32, nodes: usize, cost: i64 },
    /// What macOS swapped out in the last minute, in bytes, with `free` memory, `needed` to start.
    Swapping { swapped: u64, free: u32, needed: u32 },
    Available { available: u64, needed: u64 },
    SwapGrowth { used: u64, allocated: u64, swapped: u64 },
}

impl std::fmt::Display for NoRoom {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoRoom::Unmeasured => write!(out, "waiting for a valid system memory measurement"),
            NoRoom::Available { available, needed } => write!(out, "{available} bytes available, {needed} bytes needed including the memory floor and pending nodes"),
            NoRoom::SwapGrowth { used, allocated, swapped } => write!(out, "positive swap growth of {used} bytes used and {allocated} bytes allocated with {swapped} bytes swapped out; waiting for a stable minute"),
            NoRoom::Pressure(pressure) => write!(out, "macOS reports {pressure:?} memory pressure"),
            NoRoom::Free { free, needed, floor, nodes, cost } => write!(out, "{free}% free, {needed}% needed to keep {floor}% free with {nodes} node(s) at {:.2} GiB each", *cost as f64 / GIB as f64),
            NoRoom::Swapping { swapped, free, needed } => write!(out, "macOS swapped out {} MiB in the last minute with {free}% free, {needed}% needed while it swaps", swapped / MIB),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    at: Millis,
    pressure: Option<MemoryPressure>,
    free: Option<u32>,
    /// What macOS has swapped out since it booted, in bytes.
    swapped_out: Option<u64>,
}

/// The Mac as the gate reads it: the last sample's gauges and the samples of the last minute. The
/// load is shown and recorded but never holds a start: builds spend the CPU, agents the memory.
/// Nothing of it is kept across restarts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MacState {
    pub free_percent: Option<u32>,
    /// A partial percentage cannot replace the byte floor used while swapping.
    has_budgeted_percentage: bool,
    pub pressure: Option<MemoryPressure>,
    pub compressor: Option<MemoryCompressor>,
    pub swap_rates: Option<(f64, f64)>,
    /// What the gate takes one node to cost, from what the nodes measured; `None` before the
    /// daemon's first measure.
    pub node_cost: Option<u64>,
    /// The Mac's memory, in bytes; 0 where the system does not say.
    pub memory_total: u64,
    pub memory_available: Option<u64>,
    available_samples: VecDeque<(Millis, u64)>,
    pub nodes: BTreeMap<AgentId, u64>,
    pub nodes_at: Option<Millis>,
    /// Node footprints taken before the current system-memory sample.
    pub observed_nodes: BTreeMap<AgentId, u64>,
    pub observed_nodes_at: Option<Millis>,
    /// The observed part of recent admissions, already charged to system memory.
    pub observed_in_flight: u64,
    swap_samples: VecDeque<(Millis, MemoryCompressor)>,
    swap_hold: Option<NoRoom>,
    swap_out_hold: Option<u64>,
    unbudgeted: bool,
    pub load: f64,
    pub cpus: usize,
    /// The samples of the last `ROOM_WINDOW` and the one before them, from which the swap-outs of
    /// the whole minute are counted.
    samples: VecDeque<Sample>,
}

impl MacState {
    /// Missing readings keep the last known budget; they cannot establish recovery. Returns
    /// whether this step took a valid byte budget, which can include newer node footprints.
    pub(crate) fn step(&mut self, machine: &Machine, now: Millis) -> bool {
        self.step_load(machine);
        let Some(available) = (machine.memory_total > 0 && machine.memory_available <= machine.memory_total)
            .then_some(machine.memory_available) else {
                self.restrict_without_budget(machine, now);
                return false;
            };
        if self.available_samples.back().is_some_and(|(at, _)| now == *at) { return false; }
        if let Some(last) = self.samples.back() {
            if now < last.at {
                let shift = last.at.saturating_sub(now);
                for sample in &mut self.samples { sample.at = sample.at.saturating_sub(shift); }
                for (at, _) in &mut self.available_samples { *at = at.saturating_sub(shift); }
                self.swap_samples.clear();
            }
        }
        let before = self.available_samples.back().map(|(at, _)| *at).zip(self.compressor);
        if std::mem::take(&mut self.unbudgeted) { self.swap_samples.clear(); }
        let free = machine.memory_free_percent.filter(|free| *free <= 100);
        self.has_budgeted_percentage |= free.is_some();
        self.memory_available = Some(available);
        self.memory_total = machine.memory_total;
        self.available_samples.push_back((now, available));
        while self.available_samples.get(1).is_some_and(|(at, _)| *at <= now - ROOM_WINDOW) {
            self.available_samples.pop_front();
        }
        self.sample_swap(machine.compressor, now);
        self.swap_rates = machine.compressor.zip(before).filter(|&(_, (at, _))| now > at).map(|(compressor, (at, before))| {
            let seconds = (now - at) as f64 / 1000.0;
            (compressor.swapins.saturating_sub(before.swapins) as f64 / seconds, compressor.swapouts.saturating_sub(before.swapouts) as f64 / seconds)
        });
        self.free_percent = free.or(self.free_percent);
        self.pressure = machine.memory_pressure.or(self.pressure);
        self.compressor = machine.compressor;
        let swapped_out = machine.compressor.map(|compressor| compressor.swapouts.saturating_mul(compressor.page_size));
        self.samples.push_back(Sample { at: now, pressure: self.pressure, free, swapped_out });
        while self.samples.get(1).is_some_and(|next| next.at <= now - ROOM_WINDOW) {
            self.samples.pop_front();
        }
        self.retain_swap_floor();
        true
    }

    fn restrict_without_budget(&mut self, machine: &Machine, now: Millis) {
        let pending = std::mem::replace(&mut self.unbudgeted, true);
        if let Some(free) = machine.memory_free_percent.filter(|free| *free <= 100) {
            self.free_percent = Some(self.free_percent.map_or(free, |before| before.min(free)));
        }
        match machine.memory_pressure {
            Some(MemoryPressure::Critical) => self.pressure = Some(MemoryPressure::Critical),
            Some(MemoryPressure::Warning) if self.pressure != Some(MemoryPressure::Critical) => self.pressure = Some(MemoryPressure::Warning),
            _ => {},
        }
        let (hold, floor) = (self.swap_hold, self.swap_out_hold);
        self.sample_swap(machine.compressor, now);
        self.swap_hold = hold.or(self.swap_hold);
        self.swap_out_hold = floor.or(self.swap_out_hold);
        let mut sample = self.samples.back().copied().unwrap_or(Sample { at: now, pressure: None, free: None, swapped_out: None });
        sample.at = now.max(sample.at);
        sample.pressure = self.pressure;
        sample.free = self.free_percent;
        if let Some(compressor) = machine.compressor.filter(|c| c.page_size > 0 && c.swap_total.is_none_or(|total| c.swap_used <= total)) {
            let swapped = compressor.swapouts.saturating_mul(compressor.page_size);
            sample.swapped_out = Some(sample.swapped_out.unwrap_or(0).max(swapped));
        }
        if pending { self.samples.pop_back(); }
        self.samples.push_back(sample);
        self.retain_swap_floor();
    }

    fn retain_swap_floor(&mut self) {
        if let Some((first, last)) = self.samples.front().and_then(|s| s.swapped_out)
            .zip(self.samples.back().and_then(|s| s.swapped_out)) {
            let swapped = last.saturating_sub(first);
            if swapped >= SWAPPING { self.swap_out_hold = Some(swapped); }
        }
    }

    /// What one more node is expected to take.
    pub(crate) fn node_cost(&self) -> i64 {
        i64::try_from(self.node_cost.unwrap_or(crate::node_cost::DEFAULT)).unwrap_or(i64::MAX)
    }

    /// Whether macOS reports its warning, which spaces starts out without holding them.
    pub(crate) fn warned(&self) -> bool {
        self.pressure == Some(MemoryPressure::Warning)
    }

    /// Whether one more node may start with `in_flight` nodes admitted in the last 3 minutes, from
    /// the `ROOM_WINDOW` ending at the last valid measurement. A gap in readings cannot age out
    /// a shortage. A system without a valid byte budget waits for its first measurement.
    pub(crate) fn room(&self, in_flight: usize, _now: Millis) -> Result<(), NoRoom> {
        let Some(last) = self.samples.back() else { return Err(NoRoom::Unmeasured) };
        let Some((at, _)) = self.available_samples.back() else { return Err(NoRoom::Unmeasured) };
        let recent: Vec<&Sample> = self.samples.iter().filter(|sample| sample.at > at - ROOM_WINDOW).collect();
        let judged = if recent.is_empty() { vec![last] } else { recent };
        if judged.iter().any(|sample| sample.pressure == Some(MemoryPressure::Critical)) {
            return Err(NoRoom::Pressure(MemoryPressure::Critical));
        }
        let available = self.available_samples.iter().map(|(_, available)| *available).min()
            .filter(|_| self.memory_total > 0).ok_or(NoRoom::Unmeasured)?;
        let (nodes, cost) = (1 + in_flight, self.node_cost());
        let pending = (cost as u64).saturating_mul(in_flight as u64).saturating_sub(self.observed_in_flight);
        let reserved_bytes = pending.saturating_add(cost as u64);
        let reserved = self.percent_of_memory(i64::try_from(reserved_bytes).unwrap_or(i64::MAX));
        let free = judged.iter().filter_map(|sample| sample.free).min().or(self.free_percent);
        let needed = FREE_FLOOR + reserved;
        if let Some(free) = free.filter(|free| *free < needed) {
            return Err(NoRoom::Free { free, needed, floor: FREE_FLOOR, nodes, cost });
        }
        let swapped = self.samples.front().and_then(|first| first.swapped_out).zip(last.swapped_out)
            .map_or(0, |(first, last)| last.saturating_sub(first)).max(self.swap_out_hold.unwrap_or(0));
        let needed = SWAPPING_FLOOR + reserved;
        if swapped >= SWAPPING && let Some(free) = free.filter(|free| *free < needed) {
            return Err(NoRoom::Swapping { swapped, free, needed });
        }
        if let Some(why) = self.swap_hold { return Err(why); }
        let floor = if !self.has_budgeted_percentage && swapped >= SWAPPING { SWAPPING_FLOOR } else { FREE_FLOOR };
        let floor = self.memory_total / 100 * u64::from(floor);
        let needed = floor.saturating_add(reserved_bytes);
        if available < needed { return Err(NoRoom::Available { available, needed }); }
        Ok(())
    }

    fn sample_swap(&mut self, compressor: Option<MemoryCompressor>, now: Millis) {
        let Some(current) = compressor.filter(|c| c.page_size > 0 && c.swap_total.is_none_or(|total| c.swap_used <= total)
            && (self.swap_hold.is_none() || c.swap_total.is_some())) else {
            self.swap_samples.clear();
            return;
        };
        if self.swap_samples.back().is_some_and(|(at, before)| now <= *at || now - at > 30_000
            || before.page_size != current.page_size || current.swapouts < before.swapouts) {
            self.swap_samples.clear();
        }
        self.swap_samples.push_back((now, current));
        while self.swap_samples.get(1).is_some_and(|(at, _)| *at <= now - ROOM_WINDOW) {
            self.swap_samples.pop_front();
        }
        let Some((at, first)) = self.swap_samples.front() else { return };
        let (used, allocated) = self.swap_samples.iter().zip(self.swap_samples.iter().skip(1))
            .fold((0u64, 0u64), |(used, allocated), ((_, before), (_, after))| {
                (used.saturating_add(after.swap_used.saturating_sub(before.swap_used)),
                    allocated.saturating_add(after.swap_total.zip(before.swap_total).map_or(0, |(after, before)| after.saturating_sub(before))))
            });
        let swapped = current.swapouts.saturating_sub(first.swapouts).saturating_mul(current.page_size);
        if swapped >= SWAPPING {
            self.swap_out_hold = Some(swapped);
        } else if now - at >= ROOM_WINDOW {
            self.swap_out_hold = None;
        }
        let activity = swapped.max(used).max(if current.swap_used > 0 { allocated } else { 0 });
        if activity >= SWAPPING && (used > 0 || allocated > 0) {
            self.swap_hold = Some(NoRoom::SwapGrowth { used, allocated, swapped });
        } else if now - at >= ROOM_WINDOW && used == 0 && allocated == 0 && swapped < SWAPPING {
            self.swap_hold = None;
        }
    }

    pub(crate) fn memory_detail(&self, in_flight: usize) -> String {
        let pending = (self.node_cost() as u64).saturating_mul(in_flight as u64).saturating_sub(self.observed_in_flight);
        let nodes = self.nodes.values().copied().fold(0u64, u64::saturating_add);
        let available = self.memory_available.map_or_else(|| "unknown".into(), |bytes| bytes.to_string());
        let swap = self.compressor.map_or_else(|| "swap unavailable".into(), |c|
            format!("swap used {} bytes, allocated {}", c.swap_used, c.swap_total.map_or_else(|| "unknown".into(), |bytes| format!("{bytes} bytes"))));
        let growth = self.swap_samples.front().zip(self.swap_samples.back()).map_or_else(|| "swap growth unavailable".into(), |((first_at, first), (last_at, last))| {
            let used = i128::from(last.swap_used) - i128::from(first.swap_used);
            let allocated = last.swap_total.zip(first.swap_total).map_or_else(|| "unknown".into(), |(last, first)| (i128::from(last) - i128::from(first)).to_string());
            format!("swap growth (net) used {used:+} bytes, allocated {allocated} bytes over {} ms", last_at - first_at)
        });
        format!("system memory {} bytes, available {available} bytes, nodes {nodes} bytes (measured at {:?}), pending {pending} bytes, next node {} bytes; {swap}; {growth}", self.memory_total, self.nodes_at, self.node_cost())
    }

    /// `bytes` as a share of the Mac's memory, in whole percent rounded up; 0 where the system
    /// does not report its memory.
    fn percent_of_memory(&self, bytes: i64) -> u32 {
        if self.memory_total == 0 {
            return 0;
        }
        let bytes = u128::try_from(bytes).unwrap_or_default();
        u32::try_from((bytes * 100).div_ceil(u128::from(self.memory_total))).unwrap_or(u32::MAX).min(100)
    }

    fn step_load(&mut self, machine: &Machine) {
        if machine.cpus == 0 {
            return;
        }
        self.load = machine.load[0];
        self.cpus = machine.cpus;
    }
}

/// A day file started by an older Alondra gains the current header, and its rows are moved to the
/// current columns by name, so a column added or dropped never shifts the ones after it.
fn rewrite_header(file: &std::path::Path) -> std::io::Result<()> {
    use std::io::BufRead;
    let mut header = String::new();
    std::io::BufReader::new(std::fs::File::open(file)?).read_line(&mut header)?;
    if header.trim_end() == SAMPLES_HEADER {
        return Ok(());
    }
    let text = std::fs::read_to_string(file)?;
    let mut lines = text.lines();
    let old: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();
    let mut rewritten = format!("{SAMPLES_HEADER}\n");
    for line in lines {
        let values: Vec<&str> = line.split(',').collect();
        let row: Vec<&str> = SAMPLES_HEADER.split(',')
            .map(|column| old.iter().position(|name| *name == column).and_then(|index| values.get(index).copied()).unwrap_or_default())
            .collect();
        rewritten.push_str(&row.join(","));
        rewritten.push('\n');
    }
    std::fs::write(file, rewritten)
}

/// Where every sample is kept for a week, one CSV file a day, to tune the thresholds with this
/// Mac's own numbers.
pub const SAMPLES_DIR: &str = "machine-samples";
const KEEP_DAYS: Millis = 7 * 24 * 60 * 60_000;

/// `health` says whether the gate had room for one more node with none in flight; the disk's usable
/// space and what the nodes at work may still take of it are in decimal GB.
const SAMPLES_HEADER: &str = "at,memory_free_percent,load_1m,cpus,health,working,memory_pressure,compressor_share,margin_gb,compressor_pages,uncompressed_pages,stored_pages,swap_used_mb,swapins_per_s,swapouts_per_s,node_cost_gb,disk_usable_gb,disk_reserved_gb";
const GB: f64 = (1u64 << 30) as f64;

pub(crate) fn record(dir: &std::path::Path, now: Millis, mac: &MacState, working: usize, disk: Option<(u64, u64)>) {
    use std::io::Write;
    let day = alondra_daemon_engine_model::local_day(now);
    let file = dir.join(format!("{day}.csv"));
    let new_day = !file.exists();
    let written = std::fs::create_dir_all(dir).and_then(|()| {
        if !new_day {
            rewrite_header(&file)?;
        }
        let mut out = std::fs::OpenOptions::new().create(true).append(true).open(&file)?;
        if new_day {
            writeln!(out, "{SAMPLES_HEADER}")?;
        }
        let free = mac.free_percent.map(|free| free.to_string()).unwrap_or_default();
        let room = if mac.room(0, now).is_ok() { "room" } else { "no_room" };
        let pressure = mac.pressure.and_then(|pressure| serde_json::to_value(pressure).ok()).and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        let compressor = mac.compressor.map(|c| {
            let swap = mac.swap_rates.map(|(ins, outs)| format!("{ins:.0},{outs:.0}")).unwrap_or_else(|| ",".into());
            format!("{:.3},{:.2},{},{},{},{},{swap}", c.share(), c.margin() as f64 / GB, c.compressor_pages, c.uncompressed_pages, c.stored_pages, c.swap_used >> 20)
        }).unwrap_or_else(|| ",,,,,,,".into());
        let node_cost = mac.node_cost.map(|cost| format!("{:.2}", cost as f64 / GB)).unwrap_or_default();
        let disk = disk.map(|(usable, reserved)| format!("{:.2},{:.2}", usable as f64 / 1e9, reserved as f64 / 1e9)).unwrap_or_else(|| ",".into());
        writeln!(out, "{now},{free},{:.2},{},{room},{working},{pressure},{compressor},{node_cost},{disk}", mac.load, mac.cpus)
    });
    if let Err(error) = written {
        log!("could not record the machine sample in {}: {error}", file.display());
        return;
    }
    if new_day {
        let oldest = format!("{}.csv", alondra_daemon_engine_model::local_day(now - KEEP_DAYS));
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
    const MIN: Millis = 60_000;
    const PAGE: u64 = 16_384;

    #[test]
    fn startup_admission_requires_a_coherent_memory_budget() {
        for machine in [
            Machine::default(),
            Machine { memory_free_percent: Some(100), ..Machine::default() },
            Machine { memory_free_percent: Some(101), ..Machine::default() },
            Machine { memory_available: 8 << 30, ..Machine::default() },
            Machine { memory_total: 16 << 30, memory_available: 17 << 30, ..Machine::default() },
            Machine { memory_pressure: Some(MemoryPressure::Normal), cpus: 8, ..Machine::default() },
        ] {
            let mut state = MacState::default();
            assert!(state.room(0, 0).is_err());
            for at in [0, SAMPLE, 2 * MIN] {
                state.step(&machine, at);
                assert!(state.room(0, at).is_err(), "invalid memory budget: {machine:?}");
            }
            state.step(&memory_sample(None), 2 * MIN + SAMPLE);
            assert_eq!(state.room(0, 2 * MIN + SAMPLE), Ok(()));
        }
    }

    #[test]
    fn startup_admission_invalid_memory_totals_cannot_reprice_a_retained_budget() {
        let mut state = MacState::default();
        state.step(&Machine { memory_total: 16 << 30, memory_available: 2 << 30, ..Machine::default() }, 0);
        assert!(state.room(0, 0).is_err());
        state.step(&Machine { memory_total: 1 << 30, memory_available: 2 << 30, memory_free_percent: Some(100), ..Machine::default() }, 2 * MIN);
        assert_eq!(state.memory_total, 16 << 30);
        assert!(state.room(0, 2 * MIN).is_err(), "an invalid pair cannot reduce the memory floor");
    }

    #[test]
    fn startup_admission_invalid_budgets_cannot_clear_optional_memory_holds() {
        for first in [pressed(5, MemoryPressure::Normal), pressed(60, MemoryPressure::Critical)] {
            let mut state = MacState::default();
            state.step(&first, 0);
            assert!(state.room(0, 0).is_err());
            let invalid = Machine { memory_free_percent: Some(100), memory_pressure: Some(MemoryPressure::Normal), ..Machine::default() };
            assert!(!state.step(&invalid, 2 * MIN));
            assert!(state.room(0, 2 * MIN).is_err(), "invalid bytes cannot clear a measured hold");
            state.step(&pressed(60, MemoryPressure::Normal), SAMPLE);
            assert!(!state.step(&invalid, 2 * MIN));
            assert!(state.room(0, 2 * MIN).is_err(), "a gap in valid readings cannot age out the measured hold");
            state.step(&pressed(60, MemoryPressure::Normal), 3 * MIN);
            assert_eq!(state.room(0, 3 * MIN), Ok(()));
        }
    }

    #[test]
    fn startup_admission_unbudgeted_signals_can_tighten_but_cannot_recover() {
        for signal in [
            Machine { memory_pressure: Some(MemoryPressure::Critical), ..Machine::default() },
            Machine { memory_free_percent: Some(3), ..Machine::default() },
        ] {
            let mut state = MacState::default();
            let healthy = Machine { memory_pressure: Some(MemoryPressure::Normal), ..memory_sample(None) };
            state.step(&healthy, 0);
            assert_eq!(state.room(0, 0), Ok(()));
            assert!(!state.step(&signal, 2 * MIN));
            assert!(state.room(0, 2 * MIN).is_err(), "a missing byte budget cannot hide an independent shortage: {signal:?}");
            assert_eq!((state.memory_total, state.memory_available), (healthy.memory_total, Some(healthy.memory_available)));
            let invalid = Machine { memory_free_percent: Some(100), memory_pressure: Some(MemoryPressure::Normal), ..Machine::default() };
            state.step(&invalid, 2 * MIN + SAMPLE);
            assert!(state.room(0, 2 * MIN + SAMPLE).is_err(), "an invalid healthy signal cannot clear the shortage");
            state.step(&healthy, 2 * MIN + 2 * SAMPLE);
            assert!(state.room(0, 2 * MIN + 2 * SAMPLE).is_err(), "a recent independent shortage keeps its actual age");
            state.step(&healthy, 4 * MIN);
            assert_eq!(state.room(0, 4 * MIN), Ok(()));
        }
        let mut state = MacState::default();
        state.step(&memory_sample(None), 0);
        state.step(&Machine { memory_pressure: Some(MemoryPressure::Warning), ..Machine::default() }, SAMPLE);
        assert!(state.warned(), "a warning still spaces admissions without a new byte budget");
        for valid_at in [2 * MIN, 2 * MIN + SAMPLE, MIN - SAMPLE] {
            let mut state = MacState::default();
            let healthy = Machine { memory_pressure: Some(MemoryPressure::Normal), ..memory_sample(None) };
            state.step(&healthy, 0);
            state.step(&Machine { memory_pressure: Some(MemoryPressure::Critical), ..Machine::default() }, 2 * MIN);
            assert!(state.step(&healthy, valid_at), "the first valid budget after an invalid reading is accepted, even at the same timestamp");
            assert!(state.room(0, valid_at).is_err());
            state.step(&healthy, valid_at + MIN);
            assert_eq!(state.room(0, valid_at + MIN), Ok(()));
        }

    }

    #[test]
    fn startup_admission_unbudgeted_percentages_cannot_replace_an_unknown_percentage() {
        let mut state = MacState::default();
        let budget = |compressor| Machine { memory_available: 5 << 30, memory_free_percent: None, ..memory_sample(compressor) };
        state.step(&budget(swap_sample(1 << 30, 2 << 30, 0)), 0);
        assert_eq!(state.room(0, 0), Ok(()));
        let swapping = swap_sample(1 << 30, 2 << 30, SWAPPING / PAGE);
        state.step(&Machine { memory_free_percent: Some(100), compressor: swapping, ..Machine::default() }, SAMPLE);
        assert!(matches!(state.room(0, SAMPLE), Err(NoRoom::Available { .. })), "a percentage without a budget cannot remove the byte floor for swapping");
        state.step(&budget(swapping), 2 * SAMPLE);
        assert!(state.room(0, 2 * SAMPLE).is_err(), "a later byte budget without a percentage cannot inherit the unbudgeted 100%");

        let mut state = MacState { node_cost: Some(6 << 30), ..MacState::default() };
        state.step(&Machine { memory_available: 12 << 30, memory_free_percent: None, ..memory_sample(None) }, 0);
        assert_eq!(state.room(0, 0), Ok(()));
        state.step(&Machine { memory_free_percent: Some(40), ..Machine::default() }, SAMPLE);
        assert!(matches!(state.room(0, SAMPLE), Err(NoRoom::Free { free: 40, .. })), "independent percentages still restrict a large pending cost");
        state.step(&Machine { memory_available: 12 << 30, memory_free_percent: None, ..memory_sample(None) }, 2 * MIN);
        assert!(matches!(state.room(0, 2 * MIN), Err(NoRoom::Free { free: 40, .. })), "missing percentages cannot erase the independent restriction");

        let mut state = MacState::default();
        state.step(&Machine { memory_free_percent: Some(3), ..Machine::default() }, 0);
        assert_eq!(state.room(0, 0), Err(NoRoom::Unmeasured));
        state.step(&Machine { memory_free_percent: None, ..memory_sample(None) }, SAMPLE);
        assert!(matches!(state.room(0, SAMPLE), Err(NoRoom::Free { free: 3, .. })), "the first budget cannot discard an earlier restrictive percentage");
    }

    #[test]
    fn startup_admission_preserves_partial_shortages_before_the_first_budget() {
        let healthy = Machine { memory_pressure: Some(MemoryPressure::Normal), ..memory_sample(None) };
        for shortage in [
            Machine { memory_free_percent: Some(3), ..Machine::default() },
            Machine { memory_pressure: Some(MemoryPressure::Critical), ..Machine::default() },
        ] {
            let mut state = MacState::default();
            state.step(&shortage, 0);
            assert!(state.room(0, 0).is_err());
            state.step(&healthy, SAMPLE);
            assert!(state.room(0, SAMPLE).is_err(), "the first byte budget cannot erase a recent independent shortage");
            state.step(&healthy, MIN + SAMPLE);
            assert_eq!(state.room(0, MIN + SAMPLE), Ok(()));
        }
    }

    #[test]
    fn startup_admission_unbudgeted_swap_growth_can_hold_but_cannot_recover() {
        let mut state = MacState::default();
        state.step(&memory_sample(swap_sample(1 << 30, 2 << 30, 0)), 0);
        let growing = Machine { compressor: swap_sample((1 << 30) + SWAPPING, (2 << 30) + SWAPPING, SWAPPING / PAGE), ..Machine::default() };
        assert!(!state.step(&growing, SAMPLE));
        assert!(state.room(0, SAMPLE).is_err(), "swap growth remains a restriction when available bytes are missing");
        for at in (2 * SAMPLE..=2 * MIN).step_by(SAMPLE as usize) {
            state.step(&growing, at);
            assert!(state.room(0, at).is_err(), "missing memory cannot establish swap recovery");
        }
        let healthy = memory_sample(growing.compressor);
        for at in (2 * MIN + SAMPLE..3 * MIN + SAMPLE).step_by(SAMPLE as usize) {
            state.step(&healthy, at);
            assert!(state.room(0, at).is_err(), "swap recovery needs a fresh minute of valid memory measurements");
        }
        state.step(&healthy, 3 * MIN + SAMPLE);
        assert_eq!(state.room(0, 3 * MIN + SAMPLE), Ok(()));
    }

    #[test]
    fn startup_admission_unbudgeted_swapouts_still_raise_the_memory_floor() {
        let mut state = MacState::default();
        let healthy = Machine { memory_free_percent: Some(31), ..memory_sample(swap_sample(1 << 30, 2 << 30, 0)) };
        state.step(&healthy, 0);
        let swapping = Machine { compressor: swap_sample(1 << 30, 2 << 30, SWAPPING / PAGE), ..Machine::default() };
        assert!(!state.step(&swapping, SAMPLE));
        assert!(matches!(state.room(0, SAMPLE), Err(NoRoom::Swapping { .. })));
        state.step(&Machine { compressor: healthy.compressor, ..Machine::default() }, 2 * MIN);
        assert!(state.room(0, 2 * MIN).is_err(), "a missing budget cannot recover the swap-out restriction");
        state.step(&healthy, 3 * MIN);
        assert!(state.room(0, 3 * MIN).is_err());
        for at in (3 * MIN + SAMPLE..=4 * MIN).step_by(SAMPLE as usize) { state.step(&healthy, at); }
        assert_eq!(state.room(0, 4 * MIN), Ok(()));
    }

    #[test]
    fn startup_admission_unbudgeted_swap_readings_cannot_clear_a_known_floor() {
        for budgeted in [false, true] {
            let mut state = MacState::default();
            let sample = |compressor| Machine { memory_free_percent: Some(31), ..memory_sample(compressor) };
            state.step(&sample(swap_sample(1 << 30, 2 << 30, 0)), 0);
            let mut swapping = sample(swap_sample(1 << 30, 2 << 30, SWAPPING / PAGE));
            if !budgeted { swapping.memory_total = 0; }
            state.step(&swapping, 2 * MIN);
            assert!(matches!(state.room(0, 2 * MIN), Err(NoRoom::Swapping { .. })));
            state.step(&sample(None), 2 * MIN + SAMPLE);
            assert!(matches!(state.room(0, 2 * MIN + SAMPLE), Err(NoRoom::Swapping { .. })), "a sampling gap cannot leave a known swapping floor unprotected");
        }
        for free in [31, 60] {
            for missing in [
                None,
                Some(MemoryCompressor { page_size: 0, ..MemoryCompressor::default() }),
                Some(MemoryCompressor { page_size: PAGE, swap_used: 1, swap_total: Some(0), ..MemoryCompressor::default() }),
            ] {
                let mut state = MacState::default();
                let sample = |compressor| Machine { memory_free_percent: Some(free), ..memory_sample(compressor) };
                state.step(&sample(swap_sample(1 << 30, 2 << 30, 0)), 0);
                let stable = swap_sample(1 << 30, 2 << 30, SWAPPING / PAGE);
                state.step(&sample(stable), SAMPLE);
                assert_eq!(state.room(0, SAMPLE).is_err(), free == 31);
                for at in (2 * SAMPLE..=2 * MIN).step_by(SAMPLE as usize) {
                    state.step(&sample(missing), at);
                    assert_eq!(state.room(0, at).is_err(), free == 31, "unknown swap counters cannot lower the floor or impose an unconditional hold");
                }
                for at in (2 * MIN + SAMPLE..3 * MIN + SAMPLE).step_by(SAMPLE as usize) {
                    state.step(&sample(stable), at);
                    assert_eq!(state.room(0, at).is_err(), free == 31);
                }
                state.step(&sample(stable), 3 * MIN + SAMPLE);
                assert_eq!(state.room(0, 3 * MIN + SAMPLE), Ok(()));
            }
        }
        let mut state = MacState::default();
        let sample = |outs| Machine { memory_free_percent: Some(31), ..memory_sample(swap_sample(1 << 30, 2 << 30, outs)) };
        state.step(&sample(0), 0);
        state.step(&sample(SWAPPING / PAGE), SAMPLE);
        for at in (2 * SAMPLE..MIN + 2 * SAMPLE).step_by(SAMPLE as usize) {
            state.step(&sample(0), at);
            assert!(state.room(0, at).is_err(), "reset counters need a fresh stable minute");
        }
        state.step(&sample(0), MIN + 2 * SAMPLE);
        assert_eq!(state.room(0, MIN + 2 * SAMPLE), Ok(()));
    }

    #[test]
    fn startup_admission_available_bytes_do_not_require_optional_percentage_or_swap() {
        let mut state = MacState::default();
        state.step(&Machine { memory_total: 16 << 30, memory_available: 8 << 30, ..Machine::default() }, 0);
        assert_eq!(state.room(0, 0), Ok(()));
    }

    #[test]
    fn regression_real_available_memory_holds_a_start_even_with_a_healthy_percentage() {
        for available in [0, 512 << 20] {
            let mut state = MacState::default();
            assert!(state.step(&Machine { memory_total: 18 << 30, memory_available: available,
                memory_free_percent: Some(60), ..Machine::default() }, 0));
            assert!(state.room(0, 0).is_err(), "a high percentage cannot stand in for available RAM");
        }
    }

    #[test]
    fn regression_growing_swap_holds_starts_with_plenty_of_memory_and_disk() {
        let mut state = MacState::default();
        for at in (0..=MIN).step_by(SAMPLE as usize) {
            let growth = at as u64 * (1024 << 20) / MIN as u64;
            state.step(&Machine { memory_total: 18 << 30, memory_available: 10 << 30,
                memory_free_percent: Some(60), disk_total: 500 << 30, disk_available: 100 << 30,
                compressor: Some(MemoryCompressor { page_size: 16384, swap_used: (1 << 30) + growth,
                    swap_total: Some((2 << 30) + growth), swapouts: growth / 16384,
                    ..MemoryCompressor::default() }), ..Machine::default() }, at);
        }
        assert!(state.room(0, MIN).is_err(), "one GiB of swap growth per minute holds a start despite free disk");
    }

    fn memory_sample(counters: Option<MemoryCompressor>) -> Machine {
        Machine { memory_total: 18 << 30, memory_available: 10 << 30,
            memory_free_percent: Some(60), compressor: counters, ..Machine::default() }
    }

    fn swap_sample(used: u64, allocated: u64, outs: u64) -> Option<MemoryCompressor> {
        Some(MemoryCompressor { page_size: PAGE, swap_used: used, swap_total: Some(allocated),
            swapouts: outs, ..MemoryCompressor::default() })
    }

    #[test]
    fn swap_allocation_growth_holds_until_a_fresh_stable_minute() {
        let mut state = MacState::default();
        state.step(&memory_sample(swap_sample(GIB as u64, 2 << 30, 0)), 0);
        state.step(&memory_sample(swap_sample(GIB as u64, (2 << 30) + SWAPPING, 0)), SAMPLE);
        assert!(matches!(state.room(0, SAMPLE), Err(NoRoom::SwapGrowth { allocated: SWAPPING, .. })));
        for at in (2 * SAMPLE..MIN + SAMPLE).step_by(SAMPLE as usize) {
            state.step(&memory_sample(swap_sample(GIB as u64, (2 << 30) + SWAPPING, 0)), at);
            assert!(state.room(0, at).is_err(), "not stable for a minute yet");
        }
        state.step(&memory_sample(swap_sample(GIB as u64, (2 << 30) + SWAPPING, 0)), MIN + SAMPLE);
        assert_eq!(state.room(0, MIN + SAMPLE), Ok(()));
    }

    #[test]
    fn invalid_stale_and_reset_swap_counters_do_not_prove_recovery() {
        for invalid in [None, swap_sample(3 << 30, 2 << 30, 99), swap_sample(1 << 30, 2 << 30, 0),
            Some(MemoryCompressor { page_size: PAGE, swap_used: 1 << 30, swapouts: 100 + SWAPPING / PAGE, ..Default::default() })] {
            let mut state = MacState::default();
            state.step(&memory_sample(swap_sample(1 << 30, 2 << 30, 100)), 0);
            state.step(&memory_sample(swap_sample((1 << 30) + SWAPPING, 2 << 30, 100 + SWAPPING / PAGE)), SAMPLE);
            assert!(state.room(0, SAMPLE).is_err());
            state.step(&memory_sample(invalid), 2 * SAMPLE);
            assert!(state.room(0, 10 * MIN).is_err(), "absence of fresh readings never releases a hold");
            for at in (10 * MIN..=11 * MIN).step_by(SAMPLE as usize) {
                state.step(&memory_sample(swap_sample(1 << 30, 2 << 30, 0)), at);
            }
            assert_eq!(state.room(0, 11 * MIN), Ok(()));
        }
    }

    #[test]
    fn stable_shrinking_and_small_swap_growth_have_room() {
        for growth in [-1i64, 0, 1] {
            let mut state = MacState::default();
            for at in (0..=MIN).step_by(SAMPLE as usize) {
                let used = (GIB + growth * at * 1024) as u64;
                state.step(&memory_sample(swap_sample(used, 2 << 30, 0)), at);
            }
            assert_eq!(state.room(0, MIN), Ok(()));
        }
    }

    #[test]
    fn a_swap_drop_cannot_hide_growth_later_in_the_same_window() {
        let mut state = MacState::default();
        state.step(&memory_sample(swap_sample(1 << 30, 4 << 30, 0)), 0);
        state.step(&memory_sample(swap_sample(1 << 30, (4 << 30) + SWAPPING, 0)), SAMPLE);
        state.step(&memory_sample(swap_sample(1 << 30, 2 << 30, 0)), 2 * SAMPLE);
        for at in (3 * SAMPLE..=MIN + SAMPLE).step_by(SAMPLE as usize) {
            state.step(&memory_sample(swap_sample(1 << 30, (2 << 30) + (at as u64 / SAMPLE as u64) * SWAPPING, 0)), at);
        }
        assert!(state.room(0, MIN + SAMPLE).is_err(), "recent growth matters even below the window's starting allocation");
    }

    #[test]
    fn missing_swap_allocation_for_a_minute_does_not_release_a_hold() {
        let mut state = MacState::default();
        state.step(&memory_sample(swap_sample(1 << 30, 2 << 30, 0)), 0);
        state.step(&memory_sample(swap_sample(1 << 30, (2 << 30) + SWAPPING, 0)), SAMPLE);
        for at in (2 * SAMPLE..=2 * MIN).step_by(SAMPLE as usize) {
            let counters = MemoryCompressor { page_size: PAGE, swap_used: 1 << 30, ..Default::default() };
            state.step(&memory_sample(Some(counters)), at);
        }
        assert!(state.room(0, 2 * MIN).is_err(), "unknown allocation is not a stable allocation");
    }

    #[test]
    fn bytes_work_without_a_percentage_and_missing_readings_keep_the_last_budget() {
        let mut state = MacState::default();
        state.step(&Machine { memory_total: 18 << 30, memory_available: 0, ..Machine::default() }, 0);
        assert!(matches!(state.room(0, 0), Err(NoRoom::Available { available: 0, .. })));
        state.step(&Machine::default(), 2 * MIN);
        assert!(state.room(0, 2 * MIN).is_err());
        let machine = memory_sample(None);
        for at in (3 * MIN..=4 * MIN).step_by(SAMPLE as usize) { state.step(&machine, at); }
        assert_eq!(state.room(0, 4 * MIN), Ok(()));
    }

    #[test]
    fn missing_bytes_do_not_age_out_a_low_reading_after_one_good_sample() {
        let mut state = MacState::default();
        state.step(&Machine { memory_available: 0, ..memory_sample(None) }, 0);
        state.step(&memory_sample(None), SAMPLE);
        state.step(&Machine { memory_free_percent: Some(60), ..Machine::default() }, 2 * MIN);
        assert!(state.room(0, 2 * MIN).is_err(), "missing bytes cannot establish recovery");
        state.step(&memory_sample(None), 2 * MIN + SAMPLE);
        assert_eq!(state.room(0, 2 * MIN + SAMPLE), Ok(()));
    }

    #[test]
    fn a_clock_step_back_cannot_hide_new_critical_pressure() {
        let mut state = MacState::default();
        state.step(&memory_sample(None), MIN);
        state.step(&Machine { memory_pressure: Some(MemoryPressure::Critical), ..memory_sample(None) }, MIN - SAMPLE);
        assert_eq!(state.room(0, MIN - SAMPLE), Err(NoRoom::Pressure(MemoryPressure::Critical)));
    }

    #[test]
    fn observed_node_memory_is_not_reserved_twice_but_unobserved_growth_is() {
        let mut state = MacState { node_cost: Some(1 << 30), ..MacState::default() };
        state.step(&Machine { memory_available: 5 << 30, ..memory_sample(None) }, 0);
        assert!(state.room(1, 0).is_err(), "two pending GiB would cross the floor");
        state.observed_in_flight = 1 << 30;
        assert_eq!(state.room(1, 0), Ok(()), "the existing node is already in system usage");
        state.observed_in_flight = 128 << 20;
        assert!(state.room(1, 0).is_err(), "a node still growing keeps the rest of its reserve");
    }
    const GIB_PAGES: u64 = (1 << 30) / PAGE;
    const WEIGHED: u64 = 900_000;

    fn machine(free: u32) -> Machine {
        Machine { memory_total: 18 << 30, memory_available: 8 << 30, memory_free_percent: Some(free), cpus: 11, load: [5.0, 5.0, 5.0], ..Machine::default() }
    }

    fn pressed(free: u32, pressure: MemoryPressure) -> Machine {
        Machine { memory_pressure: Some(pressure), ..machine(free) }
    }

    fn weighed(free: u32, pressure: MemoryPressure, uncompressed: u64, compressor: u64) -> Machine {
        let counters = MemoryCompressor { page_size: PAGE, uncompressed_pages: uncompressed, compressor_pages: compressor, ..MemoryCompressor::default() };
        Machine { compressor: Some(counters), ..pressed(free, pressure) }
    }

    /// The Mac with `margin_gib` left before macOS's warning, out of the same memory weighed.
    fn with_margin(margin_gib: f64, pressure: MemoryPressure) -> Machine {
        let compressor = ((WEIGHED as f64 - margin_gib * GIB_PAGES as f64) / 2.0) as u64;
        weighed(40, pressure, WEIGHED - compressor, compressor)
    }

    /// Feeds `machine` every 10 s from `from` until `to`, and returns `to`.
    fn feed(state: &mut MacState, machine: &Machine, from: Millis, to: Millis) -> Millis {
        let mut now = from;
        while now < to {
            state.step(machine, now);
            now += SAMPLE;
        }
        to
    }

    /// This Mac (18 GB) on 13 October from 17:54 to 18:33, as `machine-samples/2026-10-13.csv`
    /// recorded it, with no node at work and macOS at normal pressure the whole time, while the gate
    /// of the time held 11 tasks: seconds, uncompressed pages, compressor pages, % free.
    const IDLE: &[(Millis, u64, u64, u32)] = &[
    (0, 465262, 354772, 40), (10, 503376, 346690, 44), (20, 518309, 331084, 45), (30, 525430, 324537, 45),
    (40, 526690, 322826, 46), (50, 523381, 326069, 45), (60, 469871, 342098, 41), (70, 514776, 334502, 45),
    (80, 517311, 331700, 45), (90, 524307, 325105, 45), (100, 523534, 325539, 45), (110, 528288, 320790, 46),
    (120, 470172, 348590, 41), (130, 521053, 327704, 45), (140, 522852, 326228, 45), (150, 532130, 317082, 46),
    (160, 533551, 315642, 46), (170, 539270, 310161, 47), (180, 493608, 325965, 43), (190, 531540, 318129, 46),
    (200, 532397, 316495, 46), (210, 541838, 307474, 47), (220, 542191, 306794, 47), (230, 518888, 326280, 45),
    (240, 487329, 328779, 42), (250, 499327, 345490, 43), (260, 502250, 342468, 43), (270, 509367, 334880, 44),
    (280, 513636, 330913, 44), (290, 518642, 326846, 45), (300, 459215, 348199, 40), (310, 511317, 333886, 44),
    (320, 514569, 330757, 45), (330, 519626, 325218, 45), (340, 522451, 322466, 45), (350, 530359, 318096, 46),
    (360, 508077, 319820, 44), (370, 528971, 319738, 46), (380, 531239, 316995, 46), (390, 535385, 313275, 46),
    (400, 538930, 309568, 47), (410, 556082, 292527, 48), (420, 510813, 307869, 44), (430, 552082, 296705, 48),
    (440, 563170, 285543, 49), (450, 561524, 287337, 49), (460, 553480, 295479, 48), (470, 557337, 291386, 48),
    (480, 507469, 311939, 44), (490, 544915, 303706, 47), (500, 553787, 295074, 48), (510, 559891, 289310, 49),
    (520, 542834, 305896, 47), (530, 544721, 304406, 47), (540, 499353, 320093, 43), (550, 511923, 337225, 44),
    (560, 504761, 343821, 44), (570, 507901, 341146, 44), (580, 512591, 336208, 44), (590, 520424, 328850, 45),
    (600, 479018, 347353, 41), (610, 517069, 332227, 45), (620, 521517, 327773, 45), (630, 539021, 310361, 47),
    (640, 540986, 307970, 47), (650, 546674, 302918, 47), (660, 523055, 326250, 45), (670, 529986, 319712, 46),
    (680, 531122, 318168, 46), (690, 539769, 309717, 47), (700, 543801, 305183, 47), (710, 544686, 303835, 47),
    (720, 502093, 317005, 43), (730, 544111, 304867, 47), (740, 547248, 301402, 47), (750, 549686, 299441, 48),
    (760, 551308, 298088, 48), (770, 550239, 299002, 48), (780, 501289, 318267, 43), (790, 536802, 313011, 46),
    (800, 536234, 312622, 46), (810, 540876, 307768, 47), (820, 544476, 304822, 47), (830, 542343, 306891, 47),
    (841, 523259, 326062, 45), (851, 499496, 349871, 43), (861, 477502, 349989, 41), (871, 479821, 360104, 41),
    (881, 481391, 358552, 42), (891, 487259, 353423, 42), (901, 486028, 353377, 42), (911, 498553, 342719, 43),
    (921, 500892, 340306, 43), (931, 515438, 325450, 45), (941, 519138, 317532, 45), (951, 455582, 354279, 39),
    (961, 494327, 345916, 43), (971, 501312, 339300, 43), (981, 496478, 343828, 43), (991, 502514, 338549, 43),
    (1001, 499942, 339916, 43), (1011, 502781, 338539, 44), (1021, 483527, 357466, 42), (1031, 500053, 341028, 43),
    (1041, 505193, 336263, 44), (1051, 503358, 336705, 44), (1061, 501188, 340345, 43), (1071, 494797, 346636, 43),
    (1081, 479556, 360610, 41), (1091, 502429, 339246, 44), (1101, 506451, 334791, 44), (1111, 511892, 329782, 44),
    (1121, 509799, 331876, 44), (1131, 509703, 331403, 44), (1141, 483612, 357885, 42), (1151, 475960, 362954, 41),
    (1161, 511869, 334639, 44), (1171, 517990, 329742, 45), (1181, 529671, 319212, 46), (1191, 534029, 314085, 46),
    (1201, 540322, 308015, 47), (1211, 537756, 310195, 47), (1221, 543843, 304561, 47), (1221, 543843, 304561, 47),
    (1221, 543843, 304561, 47), (1231, 526583, 321652, 46), (1241, 541468, 307163, 47), (1251, 521316, 327447, 45),
    (1262, 511989, 337001, 44), (1262, 511989, 337001, 44), (1273, 520515, 328227, 45), (1283, 520736, 328004, 45),
    (1293, 524124, 323595, 45), (1293, 524124, 323595, 45), (1303, 517858, 321423, 45), (1313, 511212, 316344, 44),
    (1323, 511721, 318112, 44), (1333, 520887, 308111, 45), (1343, 518114, 309878, 45), (1353, 503848, 324873, 44),
    (1363, 508432, 322276, 44), (1373, 515545, 315473, 45), (1383, 532477, 315885, 46), (1393, 541305, 307829, 47),
    (1403, 558542, 290032, 48), (1413, 559661, 289100, 49), (1423, 571085, 278237, 50), (1433, 571616, 277474, 50),
    (1443, 576979, 272229, 50), (1453, 570429, 278987, 49), (1463, 571127, 278033, 50), (1473, 571869, 276740, 50),
    (1483, 573021, 276102, 50), (1493, 542260, 291174, 47), (1503, 550611, 298482, 48), (1513, 554525, 294765, 48),
    (1523, 558828, 290284, 48), (1533, 547195, 302208, 47), (1543, 549967, 299336, 48), (1553, 550461, 298351, 48),
    (1563, 555526, 294065, 48), (1573, 561127, 288120, 49), (1583, 562032, 287427, 49), (1593, 559892, 289458, 49),
    (1603, 533955, 315644, 46), (1613, 533327, 314948, 46), (1623, 531658, 318026, 46), (1633, 536574, 312763, 46),
    (1643, 544155, 305020, 47), (1653, 542538, 306701, 47), (1663, 542931, 306424, 47), (1673, 547969, 301508, 47),
    (1683, 551598, 297869, 48), (1693, 551946, 297357, 48), (1703, 552503, 296865, 48), (1713, 553789, 295905, 48),
    (1723, 553292, 296063, 48), (1733, 553835, 295687, 48), (1743, 555559, 293906, 48), (1753, 548061, 301580, 47),
    (1763, 549009, 300774, 48), (1773, 549849, 299775, 48), (1783, 550209, 299514, 48), (1793, 550621, 299380, 48),
    (1803, 551507, 298040, 48), (1813, 552172, 297878, 48), (1823, 554368, 290806, 48), (1833, 575250, 274484, 50),
    (1843, 575518, 273845, 50), (1853, 581604, 267590, 50), (1863, 583594, 266116, 51), (1873, 588966, 260439, 51),
    (1883, 590494, 259583, 51), (1893, 590663, 258835, 51), (1903, 594777, 254982, 52), (1913, 594664, 254893, 52),
    (1923, 594932, 255073, 52), (1933, 595245, 254505, 52), (1943, 595680, 254365, 52), (1953, 595445, 254197, 52),
    (1963, 595673, 254088, 52), (1973, 602282, 246970, 52), (1983, 602025, 247636, 52), (1993, 602913, 246892, 52),
    (2003, 603753, 246467, 52), (2013, 603784, 246148, 52), (2023, 601084, 248657, 52), (2033, 600936, 248391, 52),
    (2043, 592581, 256746, 51), (2053, 534629, 315269, 46), (2063, 539319, 309132, 47), (2073, 545570, 302898, 47),
    (2083, 548906, 300454, 48), (2093, 556281, 292376, 48), (2103, 546146, 302365, 47), (2113, 549082, 299676, 48),
    (2123, 550949, 298013, 48), (2133, 549408, 299216, 48), (2143, 550969, 297920, 48), (2153, 551261, 297241, 48),
    (2163, 555568, 293479, 48), (2173, 561074, 287332, 49), (2183, 562657, 286203, 49), (2193, 563552, 285338, 49),
    (2203, 549845, 298539, 48), (2223, 557207, 291357, 48), (2233, 538810, 309733, 47), (2243, 542973, 305922, 47),
    (2253, 544853, 304094, 47), (2264, 545860, 302608, 47), (2274, 559947, 288636, 49), (2284, 576941, 271636, 50),
    (2294, 580175, 268593, 50), (2304, 580186, 268448, 50), (2314, 581059, 267735, 50), (2324, 580911, 267611, 50),
    (2334, 582657, 266105, 51), (2344, 567265, 281495, 49), (2354, 558744, 289560, 48), (2364, 560820, 287827, 49),
    (2374, 552958, 294920, 48), (2384, 556892, 291383, 48), (2394, 567255, 281366, 49),
    ];

    /// This Mac (18 GB) on 9 October from 15:51 to 16:26, while up to six compilers ran at once,
    /// one sample every 10 s: seconds, uncompressed pages, compressor pages, macOS level, % free.
    const OCTOBER_9_BUILDS: &[(Millis, u64, u64, i32, u32)] = &[
    (0, 720502, 141579, 1, 63), (10, 705893, 139432, 1, 61), (20, 730621, 136066, 1, 63),
    (30, 711046, 135402, 1, 62), (40, 734582, 130451, 1, 64), (50, 731205, 118171, 1, 64),
    (60, 748278, 117429, 1, 65), (71, 733859, 111970, 1, 64), (81, 737338, 127128, 1, 64),
    (91, 721162, 126638, 1, 63), (101, 700780, 162606, 1, 61), (111, 662632, 185000, 1, 58),
    (121, 682373, 176044, 1, 59), (131, 681592, 162114, 1, 59), (141, 710458, 152531, 1, 62),
    (151, 691817, 151911, 1, 60), (161, 712128, 151114, 1, 62), (171, 707157, 138416, 1, 61),
    (181, 729338, 133884, 1, 63), (192, 711682, 132290, 1, 62), (202, 731485, 131702, 1, 64),
    (212, 715490, 126889, 1, 62), (222, 724380, 126263, 1, 63), (232, 716815, 125917, 1, 62),
    (242, 743586, 125412, 1, 65), (252, 739121, 108792, 1, 64), (262, 754677, 111733, 1, 66),
    (273, 737703, 111503, 1, 64), (283, 739079, 110346, 1, 64), (293, 737708, 114532, 1, 63),
    (303, 750279, 114447, 1, 65), (313, 740406, 114319, 1, 64), (323, 751940, 114263, 1, 65),
    (333, 740897, 114389, 1, 64), (343, 748240, 113983, 1, 65), (354, 742425, 113889, 1, 64),
    (364, 751144, 113538, 1, 65), (374, 743108, 113380, 1, 65), (384, 750212, 113159, 1, 65),
    (394, 747336, 113104, 1, 64), (404, 745169, 114612, 1, 65), (414, 743688, 124317, 1, 65),
    (424, 739853, 123406, 1, 64), (435, 744446, 121425, 1, 65), (445, 716712, 146931, 1, 62),
    (455, 540089, 325043, 1, 47), (465, 480433, 384584, 2, 41), (475, 463097, 400869, 1, 40),
    (485, 456348, 408175, 2, 39), (496, 413361, 439583, 2, 37), (508, 466930, 397327, 2, 40),
    (518, 369457, 476965, 2, 33), (532, 410919, 452736, 2, 36), (543, 432062, 431158, 2, 37),
    (554, 401411, 447532, 2, 35), (564, 633076, 230852, 1, 55), (574, 650753, 196979, 1, 57),
    (584, 671030, 186081, 1, 58), (594, 664502, 183899, 1, 58), (604, 683554, 179196, 1, 59),
    (615, 687812, 166925, 1, 59), (625, 565971, 298287, 1, 49), (637, 570485, 273602, 1, 49),
    (647, 617081, 242955, 1, 53), (657, 431629, 418787, 2, 37), (668, 452574, 391324, 2, 39),
    (679, 385241, 479842, 2, 33), (689, 383428, 480902, 2, 33), (699, 402389, 424534, 2, 39),
    (713, 416297, 449054, 2, 36), (725, 485922, 380857, 2, 42), (735, 337913, 502427, 2, 29),
    (745, 387102, 478389, 2, 33), (756, 357272, 498072, 2, 31), (766, 460833, 380398, 2, 40),
    (776, 603900, 246690, 1, 52), (786, 621286, 222078, 1, 54), (796, 635435, 229599, 1, 55),
    (809, 613661, 251785, 1, 53), (821, 608453, 253493, 1, 53), (831, 638219, 230165, 1, 55),
    (841, 675019, 189283, 1, 59), (851, 680979, 184155, 1, 59), (861, 692297, 172144, 1, 60),
    (871, 696046, 166438, 1, 60), (881, 699811, 164617, 1, 61), (891, 699570, 163346, 1, 61),
    (901, 704366, 160537, 1, 61), (911, 666505, 196085, 1, 58), (921, 678908, 184556, 1, 59),
    (931, 685030, 177301, 1, 60), (942, 643356, 183099, 1, 56), (952, 701107, 163847, 1, 61),
    (962, 603914, 258420, 1, 52), (974, 590025, 271809, 1, 51), (984, 601849, 253863, 1, 52),
    (994, 630737, 230670, 1, 55), (1004, 633018, 223347, 1, 55), (1014, 628393, 228754, 1, 55),
    (1024, 643555, 223498, 1, 56), (1035, 667850, 193186, 1, 58), (1045, 598940, 264856, 1, 52),
    (1055, 605393, 256873, 1, 53), (1065, 632147, 229070, 1, 55), (1075, 665755, 199172, 1, 58),
    (1085, 659178, 190477, 1, 57), (1095, 676734, 187337, 1, 59), (1105, 679812, 184293, 1, 59),
    (1115, 680611, 183735, 1, 59), (1126, 680611, 183248, 1, 59), (1136, 415299, 429456, 2, 36),
    (1146, 406020, 457455, 2, 35), (1158, 366022, 489168, 2, 29), (1168, 352443, 511433, 2, 30),
    (1179, 394614, 469906, 2, 35), (1191, 423782, 420291, 2, 37), (1202, 511666, 351597, 2, 44),
    (1214, 378212, 486777, 2, 33), (1226, 346640, 516616, 2, 30), (1238, 351283, 505590, 2, 30),
    (1250, 376870, 486321, 2, 31), (1261, 475454, 387571, 2, 39), (1273, 459454, 405215, 1, 40),
    (1284, 510218, 355207, 1, 44), (1294, 621318, 242643, 1, 54), (1305, 618313, 247780, 1, 54),
    (1315, 584433, 279002, 1, 51), (1325, 517987, 348540, 1, 45), (1335, 663588, 201745, 1, 57),
    (1345, 658799, 187415, 1, 57), (1355, 674579, 182716, 1, 59), (1366, 681191, 173922, 1, 59),
    (1376, 635510, 219712, 1, 55), (1386, 444251, 407405, 1, 39), (1399, 362169, 478086, 2, 32),
    (1410, 341269, 510031, 2, 29), (1420, 386189, 459951, 2, 33), (1430, 343351, 506834, 2, 29),
    (1441, 345507, 501874, 2, 30), (1453, 350703, 506102, 2, 30), (1464, 439416, 417644, 2, 38),
    (1474, 533894, 319653, 1, 46), (1484, 523663, 334211, 1, 45), (1494, 598271, 258094, 1, 52),
    (1506, 514333, 342389, 1, 44), (1522, 440063, 415261, 2, 38), (1532, 615435, 245686, 1, 53),
    (1542, 605634, 219944, 1, 52), (1552, 660759, 196831, 1, 57), (1563, 649385, 189163, 1, 56),
    (1573, 672526, 184325, 1, 58), (1583, 696629, 156590, 1, 61), (1593, 656450, 200211, 1, 57),
    (1603, 642189, 190312, 1, 56), (1613, 672911, 180573, 1, 58), (1623, 596437, 226743, 1, 52),
    (1633, 564892, 277091, 1, 49), (1643, 514762, 292390, 1, 45), (1654, 532147, 286684, 1, 46),
    (1664, 514649, 306969, 1, 45), (1674, 486853, 325984, 1, 42), (1684, 469263, 351291, 1, 41),
    (1694, 482156, 336242, 1, 42), (1704, 512724, 296735, 1, 44), (1714, 544041, 272450, 1, 47),
    (1724, 563386, 256425, 1, 49), (1735, 570198, 255467, 1, 49), (1745, 569320, 232554, 1, 49),
    (1755, 621707, 221280, 1, 54), (1765, 655046, 192778, 1, 57), (1775, 622259, 218750, 1, 54),
    (1785, 621957, 218474, 1, 54), (1795, 634249, 216919, 1, 55), (1805, 636940, 212602, 1, 55),
    (1816, 641515, 210960, 1, 56), (1826, 641922, 209872, 1, 56), (1836, 639754, 209444, 1, 56),
    (1846, 642650, 207799, 1, 56), (1856, 644835, 205945, 1, 56), (1866, 646844, 204285, 1, 56),
    (1876, 681659, 169318, 1, 59), (1886, 684455, 166894, 1, 59), (1896, 684687, 166658, 1, 59),
    (1906, 685454, 165562, 1, 60), (1916, 701261, 148866, 1, 61), (1926, 704311, 148437, 1, 61),
    (1936, 701249, 148164, 1, 61), (1946, 703752, 148002, 1, 61), (1957, 701715, 147810, 1, 61),
    (1967, 712440, 139680, 1, 62), (1977, 711547, 139662, 1, 62), (1987, 704476, 139318, 1, 61),
    (1997, 710437, 138585, 1, 62), (2007, 715161, 138468, 1, 62), (2017, 723315, 125316, 1, 63),
    (2027, 708411, 130499, 1, 62), (2038, 717837, 129900, 1, 62), (2048, 721098, 129063, 1, 63),
    (2058, 724674, 129109, 1, 63), (2068, 708692, 135431, 1, 61), (2078, 714189, 134358, 1, 62),
    (2088, 715984, 136252, 1, 62), (2098, 708067, 131954, 1, 61), (2108, 721043, 131543, 1, 63),
    (2118, 709701, 131379, 1, 62), (2128, 717411, 134018, 1, 62), (2139, 708471, 133471, 1, 62),
    ];

    #[test]
    fn todays_idle_40_minutes_with_macos_normal_have_room_for_one_more_node() {
        let mut state = MacState::default();
        let mut held = vec![];
        for &(seconds, uncompressed, compressor, free) in IDLE {
            let now = seconds * 1000;
            state.step(&weighed(free, MemoryPressure::Normal, uncompressed, compressor), now);
            if now >= ROOM_WINDOW && let Err(why) = state.room(0, now) {
                held.push((seconds, why));
            }
        }
        assert!(held.is_empty(), "held with no node at work and macOS normal: {held:?}");
    }

    /// Up to six compilers at once on this 18 GB Mac: macOS warned in 39 samples with 29–44 % free
    /// and no swap counted. Each of them leaves room for one more node and only spaces starts out.
    #[test]
    fn the_october_9_builds_bring_macos_to_warning_with_29_to_44_percent_free_which_only_spaces_starts() {
        let mut state = MacState { node_cost: Some(410_000_000), ..MacState::default() };
        let mut warned = vec![];
        for &(seconds, uncompressed, compressor, level, free) in OCTOBER_9_BUILDS {
            let now = seconds * 1000;
            let pressure = MemoryPressure::from_kernel_level(level).unwrap();
            state.step(&Machine { memory_total: 18 * GIB as u64, ..weighed(free, pressure, uncompressed, compressor) }, now);
            assert_eq!(state.room(0, now), Ok(()), "at {seconds} s, {free} % free, {pressure:?}");
            assert_eq!(state.warned(), pressure == MemoryPressure::Warning, "at {seconds} s");
            if state.warned() {
                warned.push(free);
            }
        }
        assert_eq!(warned.len(), 39, "the samples macOS warned in: {warned:?}");
        assert_eq!((warned.iter().min(), warned.iter().max()), (Some(&29), Some(&44)));
    }

    /// 08:39 on 22 October: 40 % free on this 18 GB Mac and nodes that measured 0.41 GB each. The
    /// node that starts and each one admitted in the last 3 minutes must leave 20 % free.
    #[test]
    fn the_free_memory_needed_is_what_a_node_measures_for_the_one_that_starts_and_each_in_flight() {
        let mut state = MacState::default();
        let mac = |free| Machine { memory_total: 18 * GIB as u64, ..machine(free) };
        let now = feed(&mut state, &mac(40), 0, 2 * MIN);
        let measured: i64 = 410_000_000;
        state.node_cost = Some(measured as u64);
        assert_eq!(state.room(8, now), Ok(()), "9 nodes at 0.41 GB take 19 % of 18 GiB");
        assert_eq!(state.room(9, now), Err(NoRoom::Free { free: 40, needed: 42, floor: 20, nodes: 10, cost: measured }));
        state.node_cost = Some(GIB as u64);
        assert_eq!(state.room(4, now), Err(NoRoom::Free { free: 40, needed: 48, floor: 20, nodes: 5, cost: GIB }),
            "the 1 GiB a node was taken to cost would hold this start");
        state.node_cost = None;
        assert_eq!(state.node_cost(), 512 << 20, "before the daemon measured its nodes, one costs 512 MiB");
        assert_eq!(state.room(6, now), Ok(()));
        assert!(matches!(state.room(7, now), Err(NoRoom::Free { nodes: 8, needed: 43, .. })));
        let now = feed(&mut state, &mac(22), now, now + SAMPLE);
        assert!(matches!(state.room(0, now), Err(NoRoom::Free { nodes: 1, needed: 23, .. })), "even with nobody starting, the node that starts must fit");
    }

    /// The work Mac from 12:19 to 14:20 on 28 October: macOS at warning for two hours with 36–51 %
    /// free and 1–3 nodes working, while the gate held every start. Now each sample has room.
    #[test]
    fn macos_warning_with_36_to_51_percent_free_on_the_work_mac_has_room_for_one_more_node() {
        let mut state = MacState { node_cost: Some(512 << 20), ..MacState::default() };
        let mut now = 0;
        for free in (36..=51).chain((36..=51).rev()).cycle().take(2 * 60 * 6) {
            state.step(&Machine { memory_total: 16 * GIB as u64, ..pressed(free, MemoryPressure::Warning) }, now);
            assert_eq!(state.room(2, now), Ok(()), "{free} % free, 2 nodes in flight");
            now += SAMPLE;
        }
        assert!(state.warned());
    }

    /// 11:37:50 to 11:38:50 on 22 October: a build took the memory a simulator had just freed, macOS
    /// warned with 31–35 % free and swapped out 1,000–4,500 pages a second. That is real pressure:
    /// from the first sample under 33 % free it holds starts by the swap, not by the warning.
    #[test]
    fn macos_swapping_out_with_a_third_of_memory_free_holds_starts() {
        const OCTOBER_22: &[(Millis, u64, u64, i32, u32)] = &[
            (0, 633955, 254778, 1, 55), (10, 556964, 308724, 1, 48), (20, 403023, 476013, 2, 35), (30, 365749, 522283, 2, 31),
            (40, 380928, 508170, 2, 33), (50, 396269, 493498, 2, 34), (60, 366324, 524757, 2, 31),
        ];
        let mut state = MacState { node_cost: Some(410_000_000), ..MacState::default() };
        let now = feed(&mut state, &Machine { memory_total: 18 * GIB as u64, ..with_margin(5.8, MemoryPressure::Normal) }, 0, 2 * MIN);
        assert_eq!(state.room(0, now), Ok(()));
        let mut swapouts = 0;
        for &(seconds, uncompressed, compressor, level, free) in OCTOBER_22 {
            let at = now + seconds * 1000;
            let mut machine = Machine { memory_total: 18 * GIB as u64, ..weighed(free, MemoryPressure::from_kernel_level(level).unwrap(), uncompressed, compressor) };
            if free < 40 {
                swapouts += 2_000 * 10;
            }
            machine.compressor.as_mut().unwrap().swapouts = swapouts;
            state.step(&machine, at);
            if seconds >= 30 {
                assert!(matches!(state.room(0, at), Err(NoRoom::Swapping { needed: 33, .. })), "at {seconds} s, {free} % free: {:?}", state.room(0, at));
            }
        }
    }

    /// An 8 GB Mac whose nodes have not been measured yet (512 MiB each): critical pressure, low free
    /// memory, or a third free while macOS swaps out hold starts; the same Mac calm has room.
    #[test]
    fn a_small_mac_under_real_pressure_holds_starts() {
        let small = |free, pressure, swapouts| {
            let counters = MemoryCompressor { page_size: PAGE, uncompressed_pages: 300_000, compressor_pages: 100_000, swapouts, ..MemoryCompressor::default() };
            Machine { memory_total: 8 * GIB as u64, compressor: Some(counters), ..pressed(free, pressure) }
        };
        let mut state = MacState::default();
        let now = feed(&mut state, &small(45, MemoryPressure::Normal, 0), 0, 2 * MIN);
        assert_eq!(state.room(0, now), Ok(()));

        let now = feed(&mut state, &small(45, MemoryPressure::Critical, 0), now, now + SAMPLE);
        assert_eq!(state.room(0, now), Err(NoRoom::Pressure(MemoryPressure::Critical)));

        let now = feed(&mut state, &small(26, MemoryPressure::Warning, 0), now, now + 2 * MIN);
        assert_eq!(state.room(0, now), Err(NoRoom::Free { free: 26, needed: 27, floor: 20, nodes: 1, cost: 512 << 20 }));

        let mut swapping = MacState::default();
        let mut swapouts = 0;
        let mut now = 0;
        while now < 2 * MIN {
            swapouts += 1_000;
            swapping.step(&small(35, MemoryPressure::Warning, swapouts), now);
            now += SAMPLE;
        }
        assert!(matches!(swapping.room(0, now), Err(NoRoom::Swapping { free: 35, needed: 37, .. })), "{:?}", swapping.room(0, now));
        let calm = feed(&mut swapping, &small(35, MemoryPressure::Warning, swapouts), now, now + MIN + SAMPLE);
        assert_eq!(swapping.room(0, calm), Ok(()), "a minute after the swap-outs stop, a third free has room");
        let now = feed(&mut swapping, &small(50, MemoryPressure::Warning, swapouts + 1_000), calm, calm + SAMPLE);
        assert_eq!(swapping.room(0, now), Ok(()), "half free has room while macOS swaps out a little");
    }

    #[test]
    fn without_compressor_counters_free_memory_and_critical_pressure_decide() {
        let mut state = MacState::default();
        let now = feed(&mut state, &machine(19), 0, 2 * MIN);
        assert_eq!(state.room(0, now), Err(NoRoom::Free { free: 19, needed: 23, floor: 20, nodes: 1, cost: 512 << 20 }));
        let now = feed(&mut state, &machine(23), now, now + 4 * SAMPLE);
        assert!(matches!(state.room(0, now), Err(NoRoom::Free { free: 19, .. })), "the last sample at 19 % is still within the minute");
        let now = feed(&mut state, &machine(23), now, now + 2 * SAMPLE);
        assert_eq!(state.room(0, now), Ok(()));
        let now = feed(&mut state, &pressed(50, MemoryPressure::Warning), now, now + SAMPLE);
        assert_eq!(state.room(0, now), Ok(()), "macOS's warning alone holds nothing");
        let now = feed(&mut state, &pressed(50, MemoryPressure::Critical), now, now + SAMPLE);
        assert_eq!(state.room(0, now), Err(NoRoom::Pressure(MemoryPressure::Critical)));
        assert_eq!(state.room(0, now + MIN), Err(NoRoom::Pressure(MemoryPressure::Critical)), "the last sample stands while no new one comes");
    }

    /// The work Mac's daemon held starts "measuring memory for its first minute" with 61–87 % free.
    /// A daemon that just started judges the samples it has.
    #[test]
    fn a_daemon_waits_for_memory_readings_and_has_room_at_once_with_a_healthy_budget() {
        let mut state = MacState::default();
        assert_eq!(state.room(5, 0), Err(NoRoom::Unmeasured));
        state.step(&Machine::default(), 0);
        assert_eq!(state.room(0, MIN), Err(NoRoom::Unmeasured), "an empty sample cannot establish room");
        for free in [61, 87] {
            let mut state = MacState::default();
            state.step(&Machine { memory_total: 16 * GIB as u64, ..pressed(free, MemoryPressure::Warning) }, MIN);
            assert_eq!(state.room(0, MIN), Ok(()), "{free} % free on the first sample");
        }
        let mut state = MacState::default();
        state.step(&machine(15), MIN);
        assert!(matches!(state.room(0, MIN), Err(NoRoom::Free { free: 15, .. })), "nor does it start a node into low memory");
    }

    #[test]
    fn the_pressure_level_goes_into_the_samples_file() {
        let dir = crate::test_support::Directory::new();
        let mut state = MacState::default();
        state.step(&pressed(31, MemoryPressure::Warning), 0);
        record(&dir, 1_791_500_000_000, &state, 6, Some((21_400_000_000, 8_000_000_000)));
        let day = std::fs::read_dir(&*dir).unwrap().next().unwrap().unwrap().path();
        let text = std::fs::read_to_string(day).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(SAMPLES_HEADER));
        assert_eq!(lines.next(), Some("1791500000000,31,5.00,11,room,6,warning,,,,,,,,,,21.40,8.00"), "the disk's usable space and what the nodes at work reserve of it");
    }

    #[test]
    fn the_compressor_counters_go_into_the_samples_file_and_an_older_file_moves_to_the_current_columns() {
        let dir = crate::test_support::Directory::new();
        let now = 1_791_500_000_000;
        let day = dir.join(format!("{}.csv", alondra_daemon_engine_model::local_day(now)));
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(&day, "at,memory_free_percent,load_1m,cpus,health,cpu_busy,working,memory_pressure,compressor_share,margin_gb,burst_gb,compressor_pages,uncompressed_pages,stored_pages,swap_used_mb,swapins_per_s,swapouts_per_s\n\
            1,40,1.00,11,tight,false,2,normal,0.318,5.00,0.89,286160,613840,0,6700,30,0\n").unwrap();
        let mut state = MacState { node_cost: Some(410_000_000), ..MacState::default() };
        let mut machine = with_margin(5.0, MemoryPressure::Normal);
        machine.compressor.as_mut().unwrap().swap_used = 6_700 << 20;
        state.step(&machine, 0);
        machine.compressor.as_mut().unwrap().swapins = 300;
        state.step(&machine, SAMPLE);
        record(&dir, now, &state, 6, None);
        let text = std::fs::read_to_string(day).unwrap();
        assert_eq!(text.lines().collect::<Vec<_>>(), [
            SAMPLES_HEADER,
            "1,40,1.00,11,tight,2,normal,0.318,5.00,286160,613840,0,6700,30,0,,,",
            "1791500000000,40,5.00,11,room,6,normal,0.318,5.00,286160,613840,0,6700,30,0,0.38,,",
        ]);
    }
}
