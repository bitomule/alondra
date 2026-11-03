//! What a node of each project takes on the disk at its peak, measured from the Mac's samples of
//! its own disk, and the space the gate reserves for the nodes at work.
//!
//! A stretch of a node runs while the gate counts its task as working. Every sample splits what
//! the usable space (free plus purgeable, so macOS purging or Time Machine taking a local snapshot
//! moves nothing) fell since the one before evenly among the stretches that ran across both, and
//! takes what it rose off them in proportion to what each took, never under zero: what they took
//! together is never more than the disk lost, so what they reserve covers what they may still
//! take. A stretch's peak is the most it took. A stretch that ends keeps its peak, cut at
//! `PEAK_CEILING_BYTES`, as one of its project's latest `STRETCHES_KEPT`; a project is estimated at
//! the second highest of them, so one stray reading does not decide it. The stretches running and the
//! latest reading are kept across a restart, so a node at work goes on from what it took. A stretch
//! whose start went unseen (no reading kept) never knew what it used before: its build most likely
//! already took its space, which the free space shows, so it reserves `UNSEEN_START_BYTES` and keeps
//! nothing.
//!
//! Another volume builds write to is measured the same way, by what the volume itself holds, once a
//! stretch took at least `LEARNED_BYTES` from it; a project not measured there yet reserves what the heaviest one measured
//! there takes.
//!
//! On 27 October six nodes building at once took the disk from 34 GB to nothing: each start found
//! room for one build, none for the builds already running.
use alondra_core::Millis;
use alondra_core::domain::TaskId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const GB: u64 = 1_000_000_000;
/// A project measured fewer than `TRUSTED_STRETCHES` times reserves at least this: an iOS build
/// with a simulator and Bazel's test outputs.
pub(crate) const UNMEASURED_PEAK_BYTES: u64 = 8 * GB;
/// What a project measured enough reserves at the least.
pub(crate) const MEASURED_PEAK_FLOOR_BYTES: u64 = GB;
/// What a stretch whose start went unseen reserves at the most.
pub(crate) const UNSEEN_START_BYTES: u64 = MEASURED_PEAK_FLOOR_BYTES;
/// No stretch is kept as having used more, whatever else filled the disk while it ran.
pub(crate) const PEAK_CEILING_BYTES: u64 = 30 * GB;
pub(crate) const STRETCHES_KEPT: usize = 12;
const TRUSTED_STRETCHES: usize = 3;
/// What a stretch must take from another volume for it to count as one builds write to.
pub(crate) const LEARNED_BYTES: u64 = GB;
pub(crate) const PROJECTS_KEPT: usize = 64;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Measured {
    /// The peaks of its latest stretches, oldest first.
    pub peaks: Vec<u64>,
    pub at: Millis,
}

/// The peaks of each project, by `Task::project`: kept across restarts.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct Estimates(BTreeMap<String, Measured>);

impl Estimates {
    /// The space a node of `project` takes at its peak on the Mac's own disk.
    pub(crate) fn estimate(&self, project: Option<&str>) -> u64 {
        self.estimate_or(project, UNMEASURED_PEAK_BYTES)
    }

    /// The space a node of `project` takes at its peak on another volume builds write to.
    pub(crate) fn estimate_beside(&self, project: Option<&str>) -> u64 {
        let heaviest = self.0.values().map(second_highest).max().unwrap_or(0).max(MEASURED_PEAK_FLOOR_BYTES);
        self.estimate_or(project, heaviest)
    }

    fn estimate_or(&self, project: Option<&str>, unmeasured: u64) -> u64 {
        let Some(measured) = project.and_then(|project| self.0.get(project)) else { return unmeasured };
        let least = if measured.peaks.len() < TRUSTED_STRETCHES { unmeasured } else { MEASURED_PEAK_FLOOR_BYTES };
        second_highest(measured).max(least)
    }

    /// The most any stretch kept took.
    pub(crate) fn most(&self) -> u64 {
        self.0.values().flat_map(|measured| measured.peaks.iter().copied()).max().unwrap_or(0)
    }

    fn keep(&mut self, project: &str, peak: u64, at: Millis) {
        let measured = self.0.entry(project.to_owned()).or_default();
        measured.peaks.push(peak.min(PEAK_CEILING_BYTES));
        let over = measured.peaks.len().saturating_sub(STRETCHES_KEPT);
        measured.peaks.drain(..over);
        measured.at = at;
        while self.0.len() > PROJECTS_KEPT {
            let Some(oldest) = self.0.iter().min_by_key(|(name, measured)| (measured.at, *name)).map(|(name, _)| name.clone()) else { break };
            self.0.remove(&oldest);
        }
    }

    #[cfg(test)]
    pub(crate) fn seed(&mut self, project: &str, peaks: &[u64]) {
        for peak in peaks {
            self.keep(project, *peak, 0);
        }
    }

    #[cfg(test)]
    pub(crate) fn projects(&self) -> usize {
        self.0.len()
    }
}

/// The second highest peak kept, the highest while only one is.
fn second_highest(measured: &Measured) -> u64 {
    let mut peaks = measured.peaks.clone();
    peaks.sort_unstable_by(|a, b| b.cmp(a));
    peaks.get(1).or(peaks.first()).copied().unwrap_or(0)
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Stretch {
    project: Option<String>,
    used: u64,
    peak: u64,
    /// Measured from its start: false for one already running when the daemon started.
    whole: bool,
}

/// The stretches running now and the latest reading, kept across a restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct Ledger {
    usable: Option<u64>,
    stretches: BTreeMap<TaskId, Stretch>,
}

impl Ledger {
    /// Takes a sample of the usable space with the tasks working now and their project; returns
    /// whether a stretch ended or one whose start went unseen began, which frees what they reserved.
    pub(crate) fn sample(&mut self, estimates: &mut Estimates, usable: u64, working: &BTreeMap<TaskId, Option<String>>, now: Millis) -> bool {
        self.sample_excluding(estimates, usable, working, now, 0)
    }

    /// Swap already took space from the boot container, but is not part of a build's peak.
    pub(crate) fn sample_excluding(&mut self, estimates: &mut Estimates, usable: u64, working: &BTreeMap<TaskId, Option<String>>, now: Millis, swap_change: i128) -> bool {
        let ended: Vec<TaskId> = self.stretches.keys().filter(|task| !working.contains_key(*task)).cloned().collect();
        for task in &ended {
            let Some(stretch) = self.stretches.remove(task) else { continue };
            if let (true, Some(project)) = (stretch.whole, stretch.project.as_deref()) {
                estimates.keep(project, stretch.peak, now);
            }
        }
        if let Some(before) = self.usable {
            self.share(before as i128 - usable as i128 - swap_change);
        }
        let watched = self.usable.is_some();
        let mut unseen = false;
        for (task, project) in working {
            self.stretches.entry(task.clone()).or_insert_with(|| {
                unseen |= !watched;
                Stretch { project: project.clone(), used: 0, peak: 0, whole: watched }
            });
        }
        self.usable = Some(usable);
        !ended.is_empty() || unseen
    }

    /// Splits what the disk lost evenly among the stretches running, or takes what it gained off
    /// them in proportion to what each took.
    fn share(&mut self, lost: i128) {
        let count = self.stretches.len() as u128;
        if count == 0 {
            return;
        }
        if lost >= 0 {
            let lost = lost as u128;
            let (each, left) = (lost / count, lost % count);
            for (index, stretch) in self.stretches.values_mut().enumerate() {
                let part = each + u128::from((index as u128) < left);
                stretch.used = (u128::from(stretch.used) + part).min(u128::from(u64::MAX)) as u64;
                stretch.peak = stretch.peak.max(stretch.used);
            }
            return;
        }
        let gained = lost.unsigned_abs();
        let took: u128 = self.stretches.values().map(|stretch| u128::from(stretch.used)).sum();
        for stretch in self.stretches.values_mut() {
            stretch.used = if gained >= took { 0 } else { (u128::from(stretch.used) * (took - gained) / took) as u64 };
        }
    }

    /// What `task` may still take of its `estimate`: what its stretch did not take yet, all of it
    /// before its first sample, and at most `UNSEEN_START_BYTES` when its start went unseen.
    pub(crate) fn reserved(&self, task: &TaskId, estimate: u64) -> u64 {
        match self.stretches.get(task) {
            Some(stretch) if stretch.whole => estimate.saturating_sub(stretch.used),
            Some(stretch) => estimate.min(UNSEEN_START_BYTES).saturating_sub(stretch.used),
            None => estimate,
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.stretches.is_empty()
    }

    /// What `task`'s stretch has taken so far.
    pub(crate) fn used(&self, task: &TaskId) -> u64 {
        self.stretches.get(task).map_or(0, |stretch| stretch.used)
    }

    /// The most a stretch running now took.
    pub(crate) fn most(&self) -> u64 {
        self.stretches.values().map(|stretch| stretch.peak).max().unwrap_or(0)
    }
}

/// A task at work when the disk falls under its floor.
pub(crate) struct AtWork {
    pub task: TaskId,
    pub used: u64,
    /// Its next turn can wait: false for a node of `claude --bg`.
    pub can_wait: bool,
    pub held: bool,
}

/// The tasks whose next turn waits for room: every one at work but the one that took least, so the
/// work never stops whole. One already held stays as it is.
pub(crate) fn to_hold(at_work: &[AtWork]) -> Vec<TaskId> {
    let going: Vec<&AtWork> = at_work.iter().filter(|task| !task.held).collect();
    let Some(lightest) = going.iter().min_by_key(|task| (task.used, &task.task)) else { return vec![] };
    going.iter().filter(|task| task.task != lightest.task && task.can_wait).map(|task| task.task.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn working(tasks: &[(&str, &str)]) -> BTreeMap<TaskId, Option<String>> {
        tasks.iter().map(|(task, project)| (TaskId((*task).into()), Some((*project).into()))).collect()
    }

    fn task(id: &str) -> TaskId {
        TaskId(id.into())
    }

    /// Samples `usable` in turn with the same tasks working, then one with none.
    fn run(ledger: &mut Ledger, estimates: &mut Estimates, tasks: &[(&str, &str)], usable: &[u64]) {
        for (step, usable) in usable.iter().enumerate() {
            ledger.sample(estimates, *usable, &working(tasks), step as Millis);
        }
        ledger.sample(estimates, *usable.last().unwrap(), &BTreeMap::new(), usable.len() as Millis);
    }

    fn measured(peaks: &[u64]) -> Estimates {
        let mut estimates = Estimates::default();
        for (at, peak) in peaks.iter().enumerate() {
            estimates.keep("app", *peak, at as Millis);
        }
        estimates
    }

    #[test]
    fn a_node_alone_is_measured_at_the_most_the_disk_fell_while_it_worked() {
        let (mut ledger, mut estimates) = (Ledger::default(), Estimates::default());
        ledger.sample(&mut estimates, 100 * GB, &BTreeMap::new(), 0);
        run(&mut ledger, &mut estimates, &[("t1", "app")], &[100 * GB, 96 * GB, 88 * GB, 91 * GB, 99 * GB]);
        assert_eq!(estimates.0["app"].peaks, [12 * GB]);
    }

    #[test]
    fn what_the_disk_lost_while_three_nodes_built_is_split_among_them_and_adds_up() {
        let (mut ledger, mut estimates) = (Ledger::default(), Estimates::default());
        let tasks = [("t1", "app"), ("t2", "web"), ("t3", "docs")];
        ledger.sample(&mut estimates, 100 * GB, &working(&tasks), 0);
        ledger.sample(&mut estimates, 100 * GB, &working(&tasks), 1);
        ledger.sample(&mut estimates, 100 * GB - 9 * GB - 2, &working(&tasks), 2);
        let used: u64 = ["t1", "t2", "t3"].iter().map(|id| ledger.used(&task(id))).sum();
        assert_eq!(used, 9 * GB + 2, "every byte the disk lost is someone's, once");
        assert_eq!(ledger.used(&task("t1")), 3 * GB + 1);
        assert_eq!(ledger.used(&task("t3")), 3 * GB);
    }

    #[test]
    fn what_the_disk_gained_is_taken_off_in_proportion_to_what_each_took_and_never_more() {
        let (mut ledger, mut estimates) = (Ledger::default(), Estimates::default());
        let tasks = working(&[("t1", "app"), ("t2", "web")]);
        ledger.sample(&mut estimates, 100 * GB, &BTreeMap::new(), 0);
        ledger.sample(&mut estimates, 100 * GB, &working(&[("t1", "app")]), 1);
        ledger.sample(&mut estimates, 94 * GB, &tasks, 2);
        ledger.sample(&mut estimates, 90 * GB, &tasks, 3);
        assert_eq!((ledger.used(&task("t1")), ledger.used(&task("t2"))), (8 * GB, 2 * GB));
        ledger.sample(&mut estimates, 95 * GB, &tasks, 4);
        assert_eq!((ledger.used(&task("t1")), ledger.used(&task("t2"))), (4 * GB, GB), "half of what each took");
        ledger.sample(&mut estimates, 120 * GB, &tasks, 5);
        assert_eq!((ledger.used(&task("t1")), ledger.used(&task("t2"))), (0, 0), "never under zero");
        ledger.sample(&mut estimates, 120 * GB, &BTreeMap::new(), 6);
        assert_eq!((estimates.0["app"].peaks.as_slice(), estimates.0["web"].peaks.as_slice()), ([8 * GB].as_slice(), [2 * GB].as_slice()));
    }

    /// The release that first measured peaks found every node at work with no reading kept: each
    /// reserved its project's whole estimate, and 7 of them on 26.8 GB free left room for none.
    #[test]
    fn a_stretch_whose_start_went_unseen_reserves_little_and_keeps_nothing() {
        let (mut ledger, mut estimates) = (Ledger::default(), measured(&[4 * GB, 4 * GB, 4 * GB]));
        assert!(ledger.sample(&mut estimates, 50 * GB, &working(&[("t1", "app")]), 0), "the gate looks again: t1 reserves less than a new start");
        assert_eq!(ledger.reserved(&task("t1"), estimates.estimate(Some("app"))), UNSEEN_START_BYTES, "its build most likely already took its space");
        ledger.sample(&mut estimates, 49_600_000_000, &working(&[("t1", "app")]), 1);
        assert_eq!(ledger.reserved(&task("t1"), estimates.estimate(Some("app"))), 600_000_000, "less what it took since");
        ledger.sample(&mut estimates, 47 * GB, &working(&[("t1", "app")]), 2);
        assert_eq!(ledger.reserved(&task("t1"), estimates.estimate(Some("app"))), 0);
        assert_eq!(ledger.reserved(&task("t1"), 500_000_000), 0, "never more than its estimate");
        ledger.sample(&mut estimates, 47 * GB, &working(&[("t1", "app"), ("t2", "app")]), 3);
        ledger.sample(&mut estimates, 45 * GB, &working(&[("t1", "app"), ("t2", "app")]), 4);
        assert_eq!(ledger.reserved(&task("t2"), estimates.estimate(Some("app"))), 3 * GB, "started after: measured whole");
        assert!(!ledger.sample(&mut estimates, 45 * GB, &working(&[("t1", "app"), ("t2", "app"), ("t3", "app")]), 5), "a start seen frees nothing");
        ledger.sample(&mut estimates, 45 * GB, &BTreeMap::new(), 6);
        assert_eq!(estimates.0["app"].peaks, [4 * GB, 4 * GB, 4 * GB, GB, 0], "only the stretches measured whole are kept");
    }

    #[test]
    fn the_stretches_at_work_and_the_latest_reading_survive_a_restart() {
        let (mut ledger, mut estimates) = (Ledger::default(), measured(&[6 * GB, 6 * GB, 6 * GB]));
        ledger.sample(&mut estimates, 50 * GB, &BTreeMap::new(), 0);
        ledger.sample(&mut estimates, 50 * GB, &working(&[("t1", "app")]), 1);
        ledger.sample(&mut estimates, 46 * GB, &working(&[("t1", "app")]), 2);
        let mut reopened: Ledger = serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        assert_eq!(reopened.reserved(&task("t1"), estimates.estimate(Some("app"))), 2 * GB, "6 GB less the 4 it took before the restart");
        reopened.sample(&mut estimates, 45 * GB, &working(&[("t1", "app"), ("t2", "app")]), 3);
        assert_eq!(reopened.used(&task("t1")), 5 * GB, "what the disk lost while the daemon restarted is t1's, which ran across both readings");
        assert_eq!(reopened.reserved(&task("t2"), estimates.estimate(Some("app"))), 6 * GB, "t2 started after the reading kept: measured whole");
        assert!(serde_json::from_str::<Ledger>("{}").unwrap().is_empty(), "a file of an earlier daemon still reads");
    }

    #[test]
    fn macos_purging_and_time_machine_snapshots_are_no_nodes_use() {
        use crate::disk::Space;
        let (mut ledger, mut estimates) = (Ledger::default(), Estimates::default());
        let tasks = working(&[("t1", "app")]);
        let samples = [Space { free: 10 * GB, purgeable: 20 * GB, total: 500 * GB },
            Space { free: 25 * GB, purgeable: 5 * GB, total: 500 * GB },
            Space { free: 5 * GB, purgeable: 25 * GB, total: 500 * GB }];
        for (at, space) in samples.iter().enumerate() {
            ledger.sample(&mut estimates, space.usable(), &tasks, at as Millis);
        }
        assert_eq!(ledger.used(&task("t1")), 0);
    }

    #[test]
    fn a_node_that_ends_frees_what_it_reserved_and_keeps_its_peak() {
        let (mut ledger, mut estimates) = (Ledger::default(), measured(&[6 * GB, 6 * GB, 6 * GB]));
        ledger.sample(&mut estimates, 40 * GB, &BTreeMap::new(), 0);
        ledger.sample(&mut estimates, 40 * GB, &working(&[("t1", "app")]), 1);
        ledger.sample(&mut estimates, 35 * GB, &working(&[("t1", "app")]), 2);
        assert_eq!(ledger.reserved(&task("t1"), estimates.estimate(Some("app"))), GB, "6 GB less the 5 it already took");
        assert!(ledger.sample(&mut estimates, 35 * GB, &BTreeMap::new(), 3), "a stretch ended");
        assert_eq!(ledger.used(&task("t1")), 0);
        assert_eq!(estimates.0["app"].peaks.last(), Some(&(5 * GB)));
    }

    #[test]
    fn a_new_project_reserves_the_prudent_default_until_it_is_measured_three_times() {
        assert_eq!(Estimates::default().estimate(Some("new")), UNMEASURED_PEAK_BYTES);
        assert_eq!(Estimates::default().estimate(None), UNMEASURED_PEAK_BYTES, "a task without a project");
        assert_eq!(measured(&[2 * GB, 3 * GB]).estimate(Some("app")), UNMEASURED_PEAK_BYTES);
        assert_eq!(measured(&[12 * GB]).estimate(Some("app")), 12 * GB, "measured over the default");
        assert_eq!(measured(&[2 * GB, 3 * GB, 2 * GB]).estimate(Some("app")), 2 * GB);
        assert_eq!(measured(&[0, 0, 0]).estimate(Some("app")), MEASURED_PEAK_FLOOR_BYTES);
    }

    #[test]
    fn a_project_not_measured_on_another_volume_reserves_what_the_heaviest_one_takes_there() {
        let mut estimates = measured(&[3 * GB, 4 * GB, 4 * GB]);
        estimates.keep("web", GB / 2, 9);
        assert_eq!(estimates.estimate_beside(Some("docs")), 4 * GB, "never measured there");
        assert_eq!(estimates.estimate_beside(Some("web")), 4 * GB, "measured fewer than 3 times");
        assert_eq!(estimates.estimate_beside(Some("app")), 4 * GB);
        assert_eq!(Estimates::default().estimate_beside(None), MEASURED_PEAK_FLOOR_BYTES);
        assert_eq!(estimates.most(), 4 * GB);
    }

    #[test]
    fn a_stray_reading_is_cut_and_does_not_decide_the_estimate() {
        let estimates = measured(&[4 * GB, 5 * GB, 80 * GB, 3 * GB]);
        assert_eq!(estimates.0["app"].peaks[2], PEAK_CEILING_BYTES);
        assert_eq!(estimates.estimate(Some("app")), 5 * GB, "the second highest");
    }

    #[test]
    fn many_short_turns_without_a_build_do_not_lower_the_estimate_while_two_builds_are_recent() {
        let mut peaks = vec![9 * GB, 10 * GB];
        peaks.extend([GB / 10; 10]);
        assert_eq!(measured(&peaks).estimate(Some("app")), 9 * GB);
        peaks.push(GB / 10);
        assert_eq!(measured(&peaks).estimate(Some("app")), MEASURED_PEAK_FLOOR_BYTES, "the builds left the latest 12");
    }

    #[test]
    fn the_estimates_stay_small_and_survive_a_restart() {
        let mut estimates = Estimates::default();
        for project in 0..100 {
            for at in 0..20 {
                estimates.keep(&format!("project-{project:03}"), 9 * GB, project * 100 + at);
            }
        }
        assert_eq!(estimates.projects(), PROJECTS_KEPT);
        assert!(estimates.0.values().all(|measured| measured.peaks.len() == STRETCHES_KEPT));
        assert!(!estimates.0.contains_key("project-000"), "the project measured longest ago goes first");
        let saved = serde_json::to_vec(&estimates).unwrap();
        assert!(saved.len() < 16_000, "{} bytes", saved.len());
        assert_eq!(serde_json::from_slice::<Estimates>(&saved).unwrap(), estimates);
    }
}
