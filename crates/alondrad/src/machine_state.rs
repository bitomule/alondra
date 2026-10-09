//! How the Mac is doing, with hysteresis: memory decides between Fine, Tight and AtLimit, and the
//! CPU load only decides whether new nodes may start. Entering pressure is quick (30 s, or one
//! sample under 5 %); counting the Mac as recovered takes 20 % free without a break for
//! `RECOVERY_WINDOW`, so memory swinging between 9 % and 16 % or 22 % never looks like a recovery.
//! The pressure level macOS reports counts too, since it follows the compressor and swap and can
//! be at warning with a third of memory free: warning for 30 s is Tight, warning for 2 min or one
//! critical sample is AtLimit, and recovering also needs the pressure back to normal.
//! To stop before that warning, the counters macOS weighs count too: it warns when the compressor
//! passes half of the memory it weighs and clears below 40 %. At 40 %, or with less margin left than
//! the biggest one-minute burst of the last 30 minutes, the Mac is Tight after 20 s; at 45 % for
//! 30 s it is AtLimit.
//! See docs/fleet-pacing.md.
use std::collections::VecDeque;

use alondra_core::{Machine, MemoryCompressor, MemoryPressure, Millis};
use serde::{Deserialize, Serialize};

const TIGHT_BELOW: u32 = 15;
const AT_LIMIT_BELOW: u32 = 10;
const AT_ONCE_BELOW: u32 = 5;
const FINE_FROM: u32 = 20;
const PRESSURE_HOLDS: Millis = 30_000;
const WARNING_HOLDS: Millis = 2 * 60_000;
pub(crate) const RECOVERY_WINDOW: Millis = 5 * 60_000;
pub(crate) const NEAR_SHARE: f64 = 0.40;
const CROWDED_SHARE: f64 = 0.45;
const CLEAR_SHARE: f64 = 0.35;
const NEAR_HOLDS: Millis = 20_000;
const BURST_SPAN: Millis = 60_000;
const BURST_MEMORY: Millis = 30 * 60_000;
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
    #[serde(default)]
    pub compressor: Option<MemoryCompressor>,
    /// The biggest drop of margin to macOS's warning within a minute, over the last 30 minutes.
    #[serde(default)]
    pub burst: i64,
    /// Memory is near macOS's memory pressure, though macOS does not report it yet.
    #[serde(default)]
    pub near_pressure: bool,
    #[serde(default)]
    pub swap_rates: Option<(f64, f64)>,
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
    #[serde(default)]
    near: Option<Millis>,
    #[serde(default)]
    crowded: Option<Millis>,
    #[serde(default)]
    uncrowded: Option<Millis>,
    #[serde(default)]
    clear: Option<Millis>,
    #[serde(skip)]
    margins: VecDeque<Margin>,
    load_high: Option<Millis>,
    load_low: Option<Millis>,
}

#[derive(Clone, Debug, PartialEq)]
struct Margin {
    at: Millis,
    bytes: i64,
    /// How much margin was lost into this sample from any sample in the minute before.
    drop: i64,
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
        self.step_compressor(machine.compressor, now);
        let share = self.compressor.map(|compressor| compressor.share());
        let margin = self.compressor.map(|compressor| compressor.margin());
        let near = share.is_some_and(|share| share >= NEAR_SHARE) || margin.is_some_and(|margin| margin < self.burst);
        let near = held(&mut self.near, near, now);
        self.near_pressure = near >= NEAR_HOLDS;
        let crowded = held(&mut self.crowded, share.is_some_and(|share| share >= CROWDED_SHARE), now);
        let uncrowded = held(&mut self.uncrowded, share.is_none_or(|share| share < NEAR_SHARE), now);
        let clear = held(&mut self.clear, share.is_none_or(|share| share < CLEAR_SHARE) && margin.is_none_or(|margin| margin >= self.burst), now);
        let pressed = matches!(machine.memory_pressure, Some(MemoryPressure::Warning | MemoryPressure::Critical));
        let warned = held(&mut self.warned, pressed, now);
        let calm = held(&mut self.calm, !pressed, now);
        let tight = held(&mut self.below_tight, free < TIGHT_BELOW, now).max(warned);
        let limit = held(&mut self.below_limit, free < AT_LIMIT_BELOW, now);
        let out_of_limit = held(&mut self.from_limit, free >= AT_LIMIT_BELOW, now).min(calm).min(uncrowded);
        let fine = held(&mut self.from_fine, free >= FINE_FROM, now).min(calm).min(clear);
        let to_limit = free < AT_ONCE_BELOW
            || limit >= PRESSURE_HOLDS
            || crowded >= PRESSURE_HOLDS
            || machine.memory_pressure == Some(MemoryPressure::Critical)
            || warned >= WARNING_HOLDS;
        let next = match self.health {
            Health::Fine if to_limit => Health::AtLimit,
            Health::Fine if tight >= PRESSURE_HOLDS || self.near_pressure => Health::Tight,
            Health::Tight if to_limit => Health::AtLimit,
            Health::Tight if fine >= recovery => Health::Fine,
            Health::AtLimit if out_of_limit >= PRESSURE_HOLDS => if fine >= recovery { Health::Fine } else { Health::Tight },
            health => health,
        };
        let before = std::mem::replace(&mut self.health, next);
        (before != next).then_some((before, next))
    }

    /// How long memory has been at 20 % free or more, and macOS without pressure and far from it,
    /// without a break.
    pub(crate) fn fine_for(&self, now: Millis) -> Option<Millis> {
        Some(now - self.from_fine?.max(self.calm?).max(self.clear?))
    }

    fn step_compressor(&mut self, compressor: Option<MemoryCompressor>, now: Millis) {
        let previous = std::mem::replace(&mut self.compressor, compressor);
        let Some(compressor) = compressor else {
            self.margins.clear();
            self.burst = 0;
            self.swap_rates = None;
            return;
        };
        self.swap_rates = previous.zip(self.margins.back().map(|margin| margin.at)).filter(|&(_, at)| now > at).map(|(before, at)| {
            let seconds = (now - at) as f64 / 1000.0;
            (compressor.swapins.saturating_sub(before.swapins) as f64 / seconds, compressor.swapouts.saturating_sub(before.swapouts) as f64 / seconds)
        });
        let bytes = compressor.margin();
        let drop = self.margins.iter().filter(|margin| now - margin.at <= BURST_SPAN).map(|margin| margin.bytes - bytes).max().unwrap_or(0).max(0);
        self.margins.push_back(Margin { at: now, bytes, drop });
        while self.margins.front().is_some_and(|margin| now - margin.at > BURST_MEMORY) {
            self.margins.pop_front();
        }
        self.burst = self.margins.iter().map(|margin| margin.drop).max().unwrap_or(0);
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

/// A day file started by an older Alondra keeps its rows and gains the current header.
fn rewrite_header(file: &std::path::Path) -> std::io::Result<()> {
    use std::io::BufRead;
    let mut header = String::new();
    std::io::BufReader::new(std::fs::File::open(file)?).read_line(&mut header)?;
    if header.trim_end() == SAMPLES_HEADER {
        return Ok(());
    }
    let text = std::fs::read_to_string(file)?;
    let rows = text.split_once('\n').map_or("", |(_, rows)| rows);
    std::fs::write(file, format!("{SAMPLES_HEADER}\n{rows}"))
}

/// Where every sample is kept for a week, one CSV file a day, to tune the thresholds with this
/// Mac's own numbers.
pub(crate) const SAMPLES_DIR: &str = "machine-samples";
const KEEP_DAYS: Millis = 7 * 24 * 60 * 60_000;

const SAMPLES_HEADER: &str = "at,memory_free_percent,load_1m,cpus,health,cpu_busy,working,memory_pressure,compressor_share,margin_gb,burst_gb,compressor_pages,uncompressed_pages,stored_pages,swap_used_mb,swapins_per_s,swapouts_per_s";
const GB: f64 = (1u64 << 30) as f64;

pub(crate) fn record(dir: &std::path::Path, now: Millis, mac: &MacState, working: usize) {
    use std::io::Write;
    let day = crate::node_reports::local_day(now);
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
        let health = serde_json::to_value(mac.health).ok().and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        let pressure = mac.pressure.and_then(|pressure| serde_json::to_value(pressure).ok()).and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        let compressor = mac.compressor.map(|c| {
            let swap = mac.swap_rates.map(|(ins, outs)| format!("{ins:.0},{outs:.0}")).unwrap_or_else(|| ",".into());
            format!("{:.3},{:.2},{:.2},{},{},{},{},{swap}", c.share(), c.margin() as f64 / GB, mac.burst as f64 / GB, c.compressor_pages, c.uncompressed_pages, c.stored_pages, c.swap_used >> 20)
        }).unwrap_or_else(|| ",,,,,,,,".into());
        writeln!(out, "{now},{free},{:.2},{},{health},{},{working},{pressure},{compressor}", mac.load, mac.cpus, mac.cpu_busy)
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
        assert_eq!(lines.next(), Some(SAMPLES_HEADER));
        assert_eq!(lines.next(), Some("1791500000000,31,5.00,11,fine,false,6,warning,,,,,,,,,"));
    }

    const PAGE: u64 = 16_384;
    const GIB_PAGES: u64 = (1 << 30) / PAGE;
    const WEIGHED: u64 = 900_000;

    fn weighed(free: u32, pressure: MemoryPressure, uncompressed: u64, compressor: u64) -> Machine {
        let counters = MemoryCompressor { page_size: PAGE, uncompressed_pages: uncompressed, compressor_pages: compressor, ..MemoryCompressor::default() };
        Machine { compressor: Some(counters), ..pressed(free, pressure) }
    }

    /// The Mac with `margin_gib` left before macOS's warning, out of the same memory weighed.
    fn with_margin(margin_gib: f64, pressure: MemoryPressure) -> Machine {
        let compressor = ((WEIGHED as f64 - margin_gib * GIB_PAGES as f64) / 2.0) as u64;
        weighed(40, pressure, WEIGHED - compressor, compressor)
    }

    /// This Mac (18 GB) on 9 October from 15:51 to 16:26, while up to six compilers ran at once,
    /// one sample every 10 s: seconds, uncompressed pages, compressor pages, macOS level, % free.
    const TODAY: &[(Millis, u64, u64, i32, u32)] = &[
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
    fn todays_compile_bursts_find_the_mac_tight_before_macos_warns_from_the_second_one_on() {
        let mut state = MacState::default();
        let mut before_warnings = vec![];
        let mut healths = vec![];
        let mut previous = MemoryPressure::Normal;
        for &(seconds, uncompressed, compressor, level, free) in TODAY {
            let pressure = MemoryPressure::from_kernel_level(level).unwrap();
            if pressure != MemoryPressure::Normal && previous == MemoryPressure::Normal {
                before_warnings.push(state.health);
            }
            previous = pressure;
            state.step(&weighed(free, pressure, uncompressed, compressor), seconds * 1000);
            healths.push(state.health);
        }
        assert!(before_warnings.len() >= 5, "{before_warnings:?}");
        assert_eq!(before_warnings[0], Health::Fine, "nothing foretells the day's first burst");
        assert!(before_warnings[1..].iter().all(|&health| health != Health::Fine), "{before_warnings:?}");
        let first = healths.iter().position(|&health| health != Health::Fine).unwrap();
        assert!(healths[first..].iter().all(|&health| health != Health::Fine), "Fine again while the bursts went on");
        assert!(healths.contains(&Health::AtLimit));
    }

    #[test]
    fn last_nights_slow_climb_is_tight_and_then_at_its_limit_before_macos_warns() {
        let mut state = MacState::default();
        let mut tight_at = None;
        let mut limit_at = None;
        let mut swapped = 0;
        let mut now = 0;
        while now <= 20 * 60_000 {
            let share = 0.30 + 0.25 * now as f64 / (20 * 60_000) as f64;
            let compressor = (share * WEIGHED as f64) as u64;
            let pressure = if share > 0.5 { MemoryPressure::Warning } else { MemoryPressure::Normal };
            let free = 42 - (11 * now / (20 * 60_000)) as u32;
            swapped += 2_700 * 10;
            let mut machine = weighed(free, pressure, WEIGHED - compressor, compressor);
            machine.compressor.as_mut().unwrap().swapouts = swapped;
            match state.step(&machine, now) {
                Some((Health::Fine, Health::Tight)) => tight_at = Some((share, pressure)),
                Some((Health::Tight, Health::AtLimit)) => limit_at = Some((share, pressure)),
                _ => {}
            }
            now += SAMPLE;
        }
        let (share, pressure) = tight_at.unwrap();
        assert!((0.40..0.42).contains(&share) && pressure == MemoryPressure::Normal, "Tight at {share:.3} under {pressure:?}");
        let (share, pressure) = limit_at.unwrap();
        assert!((0.45..0.47).contains(&share) && pressure == MemoryPressure::Normal, "AtLimit at {share:.3} under {pressure:?}");
        let (_, outs) = state.swap_rates.unwrap();
        assert!((outs - 2_700.0).abs() < 1.0, "{outs}");
    }

    #[test]
    fn after_one_burst_the_mac_stays_tight_while_another_like_it_would_reach_the_warning() {
        let mut state = MacState::default();
        let mut now = 0;
        while now < 10 * 60_000 {
            assert_eq!(state.step(&with_margin(5.0, MemoryPressure::Normal), now), None);
            now += SAMPLE;
        }
        state.step(&with_margin(-1.0, MemoryPressure::Warning), now);
        now += SAMPLE;
        state.step(&with_margin(-1.0, MemoryPressure::Warning), now);
        now += SAMPLE;
        let burst_ended = now;
        let mut fine_at = None;
        while now < burst_ended + 40 * 60_000 {
            if state.step(&with_margin(5.0, MemoryPressure::Normal), now) == Some((Health::Tight, Health::Fine)) {
                fine_at = Some(now - burst_ended);
            }
            if now == burst_ended + 20_000 {
                assert_eq!(state.health, Health::Tight, "5 GB left is less than the 6 GB burst");
                assert!(state.near_pressure);
            }
            now += SAMPLE;
        }
        let fine_at = fine_at.unwrap();
        assert!((35 * 60_000..36 * 60_000).contains(&fine_at), "Fine {} s after the burst", fine_at / 1000);
    }

    #[test]
    fn the_compressor_counters_go_into_the_samples_file_and_an_old_header_is_replaced() {
        let dir = crate::test_support::Directory::new();
        let now = 1_791_500_000_000;
        let day = dir.join(format!("{}.csv", crate::node_reports::local_day(now)));
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(&day, "at,memory_free_percent,load_1m,cpus,health,cpu_busy,working\n1,40,1.00,11,fine,false,2\n").unwrap();
        let mut state = MacState::default();
        let mut machine = with_margin(5.0, MemoryPressure::Normal);
        machine.compressor.as_mut().unwrap().swap_used = 6_700 << 20;
        state.step(&machine, 0);
        machine.compressor.as_mut().unwrap().swapins = 300;
        state.step(&machine, SAMPLE);
        record(&dir, now, &state, 6);
        let text = std::fs::read_to_string(day).unwrap();
        assert_eq!(text.lines().collect::<Vec<_>>(), [
            SAMPLES_HEADER,
            "1,40,1.00,11,fine,false,2",
            "1791500000000,40,5.00,11,fine,false,6,normal,0.318,5.00,0.00,286160,613840,0,6700,30,0",
        ]);
    }
}
