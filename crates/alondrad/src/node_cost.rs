//! What one node costs in memory, measured: once a minute, the footprint of every node's process
//! group (its Agent SDK host, Claude and the commands Claude runs that stay in that group), kept for
//! six hours. The gate reserves the 95th percentile of those samples for each node it starts.
//!
//! Heavy, bursty work is not a node's cost: a build or a test run inside one node is a few of its
//! samples, which the percentile leaves out, and work that leaves the group (a build server, a
//! simulator) is the machine's. Both are seen as they happen, in macOS's pressure and the margin
//! to its warning. See docs/pacing.md §5.
use std::collections::VecDeque;

use alondra_core::Millis;

const MIB: u64 = 1 << 20;
/// What a node is taken to cost until there are enough samples.
pub(crate) const DEFAULT: u64 = 512 * MIB;
/// No estimate goes below this, whatever the samples say.
const FLOOR: u64 = 128 * MIB;
const ENOUGH: usize = 30;
const PERCENTILE: usize = 95;
pub(crate) const EVERY: Millis = 60_000;
const WINDOW: Millis = 6 * 60 * 60_000;
const MOST: usize = 20_000;

#[derive(Debug, Default)]
pub(crate) struct NodeCost {
    samples: VecDeque<(Millis, u64)>,
    last: Option<Millis>,
}

impl NodeCost {
    /// Whether a minute has passed since the last measure.
    pub(crate) fn due(&self, now: Millis) -> bool {
        self.last.is_none_or(|last| now - last >= EVERY)
    }

    /// Keeps the footprint of each node measured at `now`; a node already gone measures 0 and
    /// is not kept.
    pub(crate) fn record(&mut self, now: Millis, footprints: impl IntoIterator<Item = u64>) {
        self.last = Some(now);
        self.samples.extend(footprints.into_iter().filter(|bytes| *bytes > 0).map(|bytes| (now, bytes)));
        while self.samples.front().is_some_and(|(at, _)| *at <= now - WINDOW) || self.samples.len() > MOST {
            self.samples.pop_front();
        }
    }

    /// The bytes one more node is expected to take.
    pub(crate) fn estimate(&self) -> u64 {
        if self.samples.len() < ENOUGH {
            return DEFAULT;
        }
        let mut sorted: Vec<u64> = self.samples.iter().map(|(_, bytes)| *bytes).collect();
        sorted.sort_unstable();
        let rank = (sorted.len() * PERCENTILE).div_ceil(100);
        sorted[rank - 1].max(FLOOR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: Millis = 60_000;
    const GB: f64 = 1e9;

    /// The process groups of the eight nodes at work on David's 18 GB Mac, 22 Oct 2026 11:30.
    const MEASURED: [f64; 8] = [0.41, 0.30, 0.27, 0.26, 0.23, 0.21, 0.19, 0.13];

    fn bytes(gb: f64) -> u64 {
        (gb * GB) as u64
    }

    fn an_hour_of(nodes: &[f64], cost: &mut NodeCost, from: Millis) {
        for minute in 0..60 {
            cost.record(from + minute * MIN, nodes.iter().map(|gb| bytes(*gb)));
        }
    }

    #[test]
    fn with_too_few_samples_a_node_costs_the_default() {
        let mut cost = NodeCost::default();
        assert_eq!(cost.estimate(), DEFAULT);
        cost.record(0, MEASURED.map(bytes));
        cost.record(MIN, MEASURED.map(bytes));
        assert_eq!(cost.estimate(), DEFAULT, "16 samples are not enough");
    }

    #[test]
    fn the_estimate_is_the_95th_percentile_of_what_the_nodes_measured() {
        let mut cost = NodeCost::default();
        an_hour_of(&MEASURED, &mut cost, 0);
        assert_eq!(cost.estimate(), bytes(0.41));
        assert!(cost.estimate() < (1 << 30) / 2, "a node of today costs well under the 1 GiB the gate reserved");
    }

    #[test]
    fn a_build_inside_one_node_now_and_then_does_not_raise_what_every_node_costs() {
        let mut cost = NodeCost::default();
        for minute in 0..120 {
            let building = minute % 30 == 0;
            let mut nodes = MEASURED.map(bytes).to_vec();
            nodes[0] = if building { bytes(3.0) } else { bytes(0.41) };
            cost.record(minute * MIN, nodes);
        }
        assert_eq!(cost.estimate(), bytes(0.41), "4 samples of 960 at 3 GB stay above the 95th percentile");
    }

    #[test]
    fn nodes_that_grow_for_good_raise_the_estimate() {
        let mut cost = NodeCost::default();
        an_hour_of(&MEASURED, &mut cost, 0);
        an_hour_of(&[1.2; 8], &mut cost, 60 * MIN);
        assert_eq!(cost.estimate(), bytes(1.2));
    }

    #[test]
    fn samples_older_than_six_hours_are_forgotten() {
        let mut cost = NodeCost::default();
        an_hour_of(&[2.0; 8], &mut cost, 0);
        an_hour_of(&MEASURED, &mut cost, 7 * 60 * MIN);
        assert_eq!(cost.estimate(), bytes(0.41));
    }

    #[test]
    fn nodes_that_are_gone_are_not_samples_and_tiny_ones_cost_the_floor() {
        let mut cost = NodeCost::default();
        an_hour_of(&[0.0, 0.01], &mut cost, 0);
        assert_eq!(cost.estimate(), FLOOR);
    }

    #[test]
    fn the_nodes_are_measured_once_a_minute() {
        let mut cost = NodeCost::default();
        assert!(cost.due(0));
        cost.record(0, []);
        assert!(!cost.due(MIN - 1));
        assert!(cost.due(MIN));
    }
}
