//! Whether the Mac has room for one more node. Memory never stops what already runs: it only
//! answers, for the fleet gate, whether one more node may start or a paused one resume.
//!
//! There is room when every sample of the last `ROOM_WINDOW` has macOS at normal pressure and a
//! margin to macOS's warning that fits what one node costs, measured (`node_cost`), for the node
//! that starts and for each node admitted in the last 3 minutes, whose memory does not show yet.
//! The margin is what XNU weighs: it warns once the
//! compressor holds more pages than all uncompressed memory, so the margin is uncompressed minus
//! compressor memory (`MemoryCompressor::margin`). Where the system gives no compressor counters,
//! 20 % free stands in for it. Over 33,145 samples of 10–13 Oct this rule keeps starts open 92 % of
//! the time with the Mac idle and macOS normal, where the hysteresis it replaced kept them open
//! 42 %; a start it lets through by mistake costs at most one macOS warning, since nothing is paused.
//!
//! The window must be covered: a daemon that just started measures for a minute before it admits.
//! The CPU load only holds new starts. See docs/fleet-pacing.md §1.
use std::collections::VecDeque;

use alondra_core::{Machine, MemoryCompressor, MemoryPressure, Millis};

/// How long memory must have had room before one more node starts.
pub(crate) const ROOM_WINDOW: Millis = 60_000;
const GIB: i64 = 1 << 30;
/// Free memory that counts as room where the system gives no compressor counters.
const ROOM_FREE: u32 = 20;
const LOAD_BUSY_PER_CORE: f64 = 4.0;
const LOAD_CALM_PER_CORE: f64 = 3.0;
const LOAD_BUSY_HOLDS: Millis = 60_000;
const LOAD_CALM_HOLDS: Millis = 2 * 60_000;

/// Why one more node may not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoRoom {
    /// The daemon has not seen a minute of samples yet.
    Measuring,
    Pressure(MemoryPressure),
    /// The margin to macOS's warning, in bytes, and what the node that starts and those in flight
    /// need, `nodes` of them at `cost` each.
    Margin { margin: i64, needed: i64, nodes: usize, cost: i64 },
    /// Free memory, without compressor counters.
    Free { free: u32 },
}

impl std::fmt::Display for NoRoom {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let gib = |bytes: i64| bytes as f64 / GIB as f64;
        match self {
            NoRoom::Measuring => write!(out, "the daemon is measuring memory for its first minute"),
            NoRoom::Pressure(pressure) => write!(out, "macOS reports {pressure:?} memory pressure"),
            NoRoom::Margin { margin, needed, nodes, cost } => write!(out, "{:.1} GiB left before macOS's memory warning, {:.1} GiB needed for {nodes} node(s) at {:.2} GiB each", gib(*margin), gib(*needed), gib(*cost)),
            NoRoom::Free { free } => write!(out, "{free}% free, {ROOM_FREE}% needed"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct Sample {
    at: Millis,
    pressure: Option<MemoryPressure>,
    free: u32,
    margin: Option<i64>,
}

impl Sample {
    fn room(&self, nodes: usize, cost: i64) -> Result<(), NoRoom> {
        if let Some(pressure) = self.pressure.filter(|pressure| *pressure != MemoryPressure::Normal) {
            return Err(NoRoom::Pressure(pressure));
        }
        let needed = cost * nodes as i64;
        match self.margin {
            Some(margin) if margin < needed => Err(NoRoom::Margin { margin, needed, nodes, cost }),
            None if self.free < ROOM_FREE => Err(NoRoom::Free { free: self.free }),
            _ => Ok(()),
        }
    }
}

/// The Mac as the gate reads it: the last sample's gauges, the samples of the last minute, and
/// whether the CPU holds new starts. Nothing of it is kept across restarts.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MacState {
    /// No new node starts while the CPU load stays this high.
    pub cpu_busy: bool,
    pub free_percent: Option<u32>,
    pub pressure: Option<MemoryPressure>,
    pub compressor: Option<MemoryCompressor>,
    pub swap_rates: Option<(f64, f64)>,
    /// What the gate takes one node to cost, from what the nodes measured; `None` before the
    /// daemon's first measure.
    pub node_cost: Option<u64>,
    pub load: f64,
    pub cpus: usize,
    /// The samples of the last `ROOM_WINDOW` and the one before them, which shows it is covered.
    samples: VecDeque<Sample>,
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
    /// Takes one sample; one without a memory reading only counts for the CPU.
    pub(crate) fn step(&mut self, machine: &Machine, now: Millis) {
        self.step_load(machine, now);
        let Some(free) = machine.memory_free_percent else { return };
        let before = self.samples.back().map(|sample| sample.at).zip(self.compressor);
        self.swap_rates = machine.compressor.zip(before).filter(|&(_, (at, _))| now > at).map(|(compressor, (at, before))| {
            let seconds = (now - at) as f64 / 1000.0;
            (compressor.swapins.saturating_sub(before.swapins) as f64 / seconds, compressor.swapouts.saturating_sub(before.swapouts) as f64 / seconds)
        });
        self.free_percent = Some(free);
        self.pressure = machine.memory_pressure;
        self.compressor = machine.compressor;
        self.samples.push_back(Sample { at: now, pressure: machine.memory_pressure, free, margin: machine.compressor.map(|compressor| compressor.margin()) });
        while self.samples.get(1).is_some_and(|next| next.at <= now - ROOM_WINDOW) {
            self.samples.pop_front();
        }
    }

    /// What one more node is expected to take.
    pub(crate) fn node_cost(&self) -> i64 {
        i64::try_from(self.node_cost.unwrap_or(crate::node_cost::DEFAULT)).unwrap_or(i64::MAX)
    }

    /// Whether one more node may start with `in_flight` nodes admitted in the last 3 minutes. A
    /// system that never reported memory always has room; one that did needs `ROOM_WINDOW` of it.
    /// macOS's warning holds every start whatever is free: the margin here only approximates what
    /// the kernel weighs, and half of the warnings start with it still positive.
    pub(crate) fn room(&self, in_flight: usize, now: Millis) -> Result<(), NoRoom> {
        let Some(first) = self.samples.front() else { return Ok(()) };
        if first.at > now - ROOM_WINDOW {
            return Err(NoRoom::Measuring);
        }
        let cost = self.node_cost();
        let recent: Vec<&Sample> = self.samples.iter().filter(|sample| sample.at > now - ROOM_WINDOW).collect();
        let judged = if recent.is_empty() { self.samples.back().into_iter().collect() } else { recent };
        judged.into_iter().rev().try_for_each(|sample| sample.room(1 + in_flight, cost))
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
pub(crate) const SAMPLES_DIR: &str = "machine-samples";
const KEEP_DAYS: Millis = 7 * 24 * 60 * 60_000;

/// `health` says whether the gate had room for one more node with none in flight.
const SAMPLES_HEADER: &str = "at,memory_free_percent,load_1m,cpus,health,cpu_busy,working,memory_pressure,compressor_share,margin_gb,compressor_pages,uncompressed_pages,stored_pages,swap_used_mb,swapins_per_s,swapouts_per_s,node_cost_gb";
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
        let room = if mac.room(0, now).is_ok() { "room" } else { "no_room" };
        let pressure = mac.pressure.and_then(|pressure| serde_json::to_value(pressure).ok()).and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default();
        let compressor = mac.compressor.map(|c| {
            let swap = mac.swap_rates.map(|(ins, outs)| format!("{ins:.0},{outs:.0}")).unwrap_or_else(|| ",".into());
            format!("{:.3},{:.2},{},{},{},{},{swap}", c.share(), c.margin() as f64 / GB, c.compressor_pages, c.uncompressed_pages, c.stored_pages, c.swap_used >> 20)
        }).unwrap_or_else(|| ",,,,,,,".into());
        let node_cost = mac.node_cost.map(|cost| format!("{:.2}", cost as f64 / GB)).unwrap_or_default();
        writeln!(out, "{now},{free},{:.2},{},{room},{},{working},{pressure},{compressor},{node_cost}", mac.load, mac.cpus, mac.cpu_busy)
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
    const MIN: Millis = 60_000;
    const PAGE: u64 = 16_384;
    const GIB_PAGES: u64 = (1 << 30) / PAGE;
    const WEIGHED: u64 = 900_000;

    fn machine(free: u32) -> Machine {
        Machine { memory_free_percent: Some(free), cpus: 11, load: [5.0, 5.0, 5.0], ..Machine::default() }
    }

    fn pressed(free: u32, pressure: MemoryPressure) -> Machine {
        Machine { memory_pressure: Some(pressure), ..machine(free) }
    }

    fn weighed(free: u32, pressure: MemoryPressure, uncompressed: u64, compressor: u64) -> Machine {
        let counters = MemoryCompressor { page_size: PAGE, uncompressed_pages: uncompressed, compressor_pages: compressor, ..MemoryCompressor::default() };
        Machine { compressor: Some(counters), ..pressed(free, pressure) }
    }

    fn margin_of(margin_gib: f64) -> i64 {
        with_margin(margin_gib, MemoryPressure::Normal).compressor.unwrap().margin()
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

    /// Up to six compilers at once: every sample macOS warned in, and the minute after it, has no
    /// room for one more node; once the builds calm down there is room again.
    #[test]
    fn a_build_that_brings_macos_to_warning_leaves_no_room_until_a_minute_after_it_clears() {
        let mut state = MacState::default();
        let mut warned = vec![];
        let mut last_warning = None;
        for &(seconds, uncompressed, compressor, level, free) in OCTOBER_9_BUILDS {
            let now = seconds * 1000;
            let pressure = MemoryPressure::from_kernel_level(level).unwrap();
            state.step(&weighed(free, pressure, uncompressed, compressor), now);
            let room = state.room(0, now);
            if pressure == MemoryPressure::Warning {
                assert_eq!(room, Err(NoRoom::Pressure(MemoryPressure::Warning)), "at {seconds} s");
                warned.push(seconds);
                last_warning = Some(now);
            } else if last_warning.is_some_and(|at| now - at < ROOM_WINDOW) {
                assert!(room.is_err(), "{seconds} s, under a minute after a warning");
            }
        }
        assert_eq!(warned.len(), 39, "the samples macOS warned in: {warned:?}");
        let end = OCTOBER_9_BUILDS.last().unwrap().0 * 1000;
        assert_eq!(state.room(0, end), Ok(()), "the builds are over");
    }

    /// 08:39 on 22 October: 4.8 GiB to macOS's warning and 4 nodes admitted in the last 3 minutes,
    /// while the nodes at work measured 0.13–0.41 GB each. 1 GiB per node held 31 tasks.
    #[test]
    fn the_margin_needed_is_what_a_node_measures_for_the_one_that_starts_and_each_in_flight() {
        let mut state = MacState::default();
        let now = feed(&mut state, &with_margin(4.8, MemoryPressure::Normal), 0, 2 * MIN);
        let measured: u64 = 410_000_000;
        state.node_cost = Some(measured);
        assert_eq!(state.room(4, now), Ok(()), "5 nodes at 0.41 GB fit in 4.8 GiB");
        let cost = measured as i64;
        assert_eq!(state.room(11, now), Ok(()), "12 of them fit");
        assert_eq!(state.room(12, now), Err(NoRoom::Margin { margin: margin_of(4.8), needed: 13 * cost, nodes: 13, cost }));
        state.node_cost = Some(GIB as u64);
        assert_eq!(state.room(4, now), Err(NoRoom::Margin { margin: margin_of(4.8), needed: 5 * GIB, nodes: 5, cost: GIB }),
            "the 1 GiB a node was taken to cost held this start");
        state.node_cost = None;
        assert_eq!(state.node_cost(), 512 << 20, "before the daemon measured its nodes, one costs 512 MiB");
        assert_eq!(state.room(8, now), Ok(()));
        assert!(matches!(state.room(9, now), Err(NoRoom::Margin { nodes: 10, .. })));
        let now = feed(&mut state, &with_margin(0.4, MemoryPressure::Normal), now, now + SAMPLE);
        assert!(matches!(state.room(0, now), Err(NoRoom::Margin { nodes: 1, .. })), "even with nobody starting, the node that starts must fit");
    }

    /// 11:37:50 to 11:38:50 on 22 October: a build took the memory a simulator had just freed and
    /// macOS warned with 31–35 % free, the compressor above all uncompressed memory; at 08:46 it
    /// warned with 1.9 GB of margin left. macOS's level is what holds, not the free figure.
    #[test]
    fn macos_warning_holds_every_start_with_a_third_of_memory_free_and_margin_left() {
        const OCTOBER_22: &[(Millis, u64, u64, i32, u32)] = &[
            (0, 633955, 254778, 1, 55), (10, 556964, 308724, 1, 48), (20, 403023, 476013, 2, 35), (30, 365749, 522283, 2, 31),
            (40, 380928, 508170, 2, 33), (50, 396269, 493498, 2, 34), (60, 366324, 524757, 2, 31),
        ];
        let mut state = MacState { node_cost: Some(410_000_000), ..MacState::default() };
        let now = feed(&mut state, &with_margin(5.8, MemoryPressure::Normal), 0, 2 * MIN);
        assert_eq!(state.room(0, now), Ok(()));
        for &(seconds, uncompressed, compressor, level, free) in OCTOBER_22 {
            let at = now + seconds * 1000;
            let pressure = MemoryPressure::from_kernel_level(level).unwrap();
            state.step(&weighed(free, pressure, uncompressed, compressor), at);
            if pressure == MemoryPressure::Warning {
                assert_eq!(state.room(0, at), Err(NoRoom::Pressure(MemoryPressure::Warning)), "at {seconds} s, {free} % free");
            }
        }
        let mut state = MacState { node_cost: Some(410_000_000), ..MacState::default() };
        let now = feed(&mut state, &weighed(44, MemoryPressure::Warning, 512_794, 387_515), 0, 2 * MIN);
        assert!(state.compressor.unwrap().margin() > GIB, "margin left: {}", state.compressor.unwrap().margin());
        assert_eq!(state.room(0, now), Err(NoRoom::Pressure(MemoryPressure::Warning)));
    }

    #[test]
    fn without_compressor_counters_a_minute_at_20_percent_free_and_normal_pressure_is_room() {
        let mut state = MacState::default();
        let now = feed(&mut state, &machine(19), 0, 2 * MIN);
        assert_eq!(state.room(0, now), Err(NoRoom::Free { free: 19 }));
        let now = feed(&mut state, &machine(20), now, now + 4 * SAMPLE);
        assert_eq!(state.room(0, now), Err(NoRoom::Free { free: 19 }), "the last sample at 19 % is still within the minute");
        let now = feed(&mut state, &machine(20), now, now + SAMPLE);
        assert_eq!(state.room(0, now), Ok(()));
        let now = feed(&mut state, &pressed(50, MemoryPressure::Warning), now, now + SAMPLE);
        assert_eq!(state.room(0, now), Err(NoRoom::Pressure(MemoryPressure::Warning)), "macOS's own level counts without counters too");
        assert_eq!(state.room(0, now + MIN), Err(NoRoom::Pressure(MemoryPressure::Warning)), "the last sample stands while no new one comes");
    }

    #[test]
    fn a_daemon_that_just_started_measures_for_a_minute_and_a_system_without_memory_readings_always_has_room() {
        let mut state = MacState::default();
        assert_eq!(state.room(5, 0), Ok(()));
        state.step(&Machine { memory_free_percent: None, ..machine(0) }, 0);
        assert_eq!(state.room(0, MIN), Ok(()), "a sample without a memory reading only counts for the CPU");
        let now = feed(&mut state, &machine(60), MIN, MIN + 50_000);
        assert_eq!(state.room(0, now), Err(NoRoom::Measuring));
        let now = feed(&mut state, &machine(60), now, now + SAMPLE);
        assert_eq!(state.room(0, now), Ok(()));
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
    fn the_pressure_level_goes_into_the_samples_file() {
        let dir = crate::test_support::Directory::new();
        let mut state = MacState::default();
        state.step(&pressed(31, MemoryPressure::Warning), 0);
        record(&dir, 1_791_500_000_000, &state, 6);
        let day = std::fs::read_dir(&*dir).unwrap().next().unwrap().unwrap().path();
        let text = std::fs::read_to_string(day).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some(SAMPLES_HEADER));
        assert_eq!(lines.next(), Some("1791500000000,31,5.00,11,no_room,false,6,warning,,,,,,,,,"));
    }

    #[test]
    fn the_compressor_counters_go_into_the_samples_file_and_an_older_file_moves_to_the_current_columns() {
        let dir = crate::test_support::Directory::new();
        let now = 1_791_500_000_000;
        let day = dir.join(format!("{}.csv", crate::node_reports::local_day(now)));
        std::fs::create_dir_all(&*dir).unwrap();
        std::fs::write(&day, "at,memory_free_percent,load_1m,cpus,health,cpu_busy,working,memory_pressure,compressor_share,margin_gb,burst_gb,compressor_pages,uncompressed_pages,stored_pages,swap_used_mb,swapins_per_s,swapouts_per_s\n\
            1,40,1.00,11,tight,false,2,normal,0.318,5.00,0.89,286160,613840,0,6700,30,0\n").unwrap();
        let mut state = MacState { node_cost: Some(410_000_000), ..MacState::default() };
        let mut machine = with_margin(5.0, MemoryPressure::Normal);
        machine.compressor.as_mut().unwrap().swap_used = 6_700 << 20;
        state.step(&machine, 0);
        machine.compressor.as_mut().unwrap().swapins = 300;
        state.step(&machine, SAMPLE);
        record(&dir, now, &state, 6);
        let text = std::fs::read_to_string(day).unwrap();
        assert_eq!(text.lines().collect::<Vec<_>>(), [
            SAMPLES_HEADER,
            "1,40,1.00,11,tight,false,2,normal,0.318,5.00,286160,613840,0,6700,30,0,",
            "1791500000000,40,5.00,11,room,false,6,normal,0.318,5.00,286160,613840,0,6700,30,0,0.38",
        ]);
    }
}
