//! Swap shares the boot disk's container. Its existing use is already absent from free space;
//! only its recent growth needs room beside the builds' remaining peaks.
use alondra_core::{MemoryCompressor, Millis};
use std::collections::VecDeque;

const MINUTE: Millis = 60_000;
const WINDOW: Millis = 5 * MINUTE;
const HORIZON: Millis = 40 * MINUTE;
const MAX_GAP: Millis = 30_000;
const GROWTH_BYTES: u64 = 256 << 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    at: Millis,
    used: u64,
    total: u64,
}

#[derive(Default)]
pub(crate) struct SwapDisk {
    samples: VecDeque<Sample>,
    /// Net allocated swapfile change between consecutive valid samples, for the build attribution only.
    pub allocated_change: i128,
}

impl SwapDisk {
    pub(crate) fn line(&self, now: Millis) -> Option<String> {
        let last = self.samples.back()?;
        if now < last.at || now - last.at > MAX_GAP {
            return None;
        }
        let first = self.samples.front()?;
        let growth = i128::from(last.used) - i128::from(first.used);
        let gb = 1_000_000_000.0;
        Some(format!("swap {:.2}/{:.2} GB, growth {:+.3} GB over {} s, swap reserve {:.2} GB",
            last.used as f64 / gb, last.total as f64 / gb, growth as f64 / gb,
            (last.at - first.at) / 1000, self.reserve(now).unwrap_or(0) as f64 / gb))
    }

    pub(crate) fn sample(&mut self, compressor: Option<MemoryCompressor>, now: Millis) {
        self.allocated_change = 0;
        let Some((used, total)) = compressor.and_then(|c| c.swap_total.map(|total| (c.swap_used, total)))
            .filter(|(used, total)| used <= total) else {
                self.samples.clear();
                return;
            };
        if let Some(last) = self.samples.back() {
            if now <= last.at || now - last.at > MAX_GAP {
                self.samples.clear();
            } else {
                self.allocated_change = i128::from(total) - i128::from(last.total);
            }
        }
        self.samples.push_back(Sample { at: now, used, total });
        while self.samples.get(1).is_some_and(|next| next.at <= now - WINDOW) {
            self.samples.pop_front();
        }
    }

    pub(crate) fn reserve(&self, now: Millis) -> Option<u64> {
        let last = self.samples.back()?;
        if now < last.at || now - last.at > MAX_GAP {
            return None;
        }
        let first = self.samples.front()?;
        let millis = last.at - first.at;
        let recent = self.samples.iter().rev().find(|sample| sample.at <= last.at - MINUTE)?;
        let bytes = last.used.saturating_sub(first.used);
        if millis < WINDOW || bytes < GROWTH_BYTES || last.used <= recent.used {
            return None;
        }
        let reserve = (u128::from(bytes) * HORIZON as u128 / millis as u128).min(u128::from(u64::MAX)) as u64;
        Some(reserve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const GB: u64 = 1_000_000_000;
    fn counters(used: u64) -> Option<MemoryCompressor> {
        Some(MemoryCompressor { swap_used: used, swap_total: Some(30 * GB), ..MemoryCompressor::default() })
    }
    fn feed(swap: &mut SwapDisk, from: Millis, until: Millis, used: impl Fn(Millis) -> u64) {
        for now in (from..=until).step_by(10_000) {
            swap.sample(counters(used(now)), now);
        }
    }

    #[test]
    fn a_full_fresh_window_of_growth_needs_room_and_a_quiet_minute_releases_it() {
        let mut swap = SwapDisk::default();
        feed(&mut swap, 0, WINDOW - 10_000, |at| GB + at as u64 * 1000);
        assert_eq!(swap.reserve(WINDOW - 10_000), None);
        swap.sample(counters(GB + 300_000_000), WINDOW);
        assert_eq!(swap.reserve(WINDOW), Some(2_400_000_000));
        assert_eq!(swap.reserve(WINDOW + MAX_GAP + 1), None, "stale readings cannot hold starts");
        feed(&mut swap, WINDOW + 10_000, WINDOW + MINUTE, |_| GB + 300_000_000);
        assert_eq!(swap.reserve(WINDOW + MINUTE), None, "old growth does not hold a stable swap");
    }

    #[test]
    fn stable_shrinking_and_small_growth_reserve_nothing() {
        for change in [-1000i64, 0, 800] {
            let mut swap = SwapDisk::default();
            feed(&mut swap, 0, WINDOW, |at| (GB as i64 + at * change) as u64);
            assert_eq!(swap.reserve(WINDOW), None, "change {change}");
        }
    }

    #[test]
    fn missing_invalid_interrupted_or_reversed_samples_start_a_new_history() {
        for bad in [None, Some(MemoryCompressor::default()),
            Some(MemoryCompressor { swap_used: GB, swap_total: Some(0), ..MemoryCompressor::default() }),
            Some(MemoryCompressor { swap_used: 31 * GB, swap_total: Some(30 * GB), ..MemoryCompressor::default() })] {
            let mut swap = SwapDisk::default();
            feed(&mut swap, 0, WINDOW, |at| GB + at as u64 * 1000);
            assert!(swap.reserve(WINDOW).is_some());
            swap.sample(bad, WINDOW + 10_000);
            assert_eq!(swap.reserve(WINDOW + 10_000), None);
            swap.sample(counters(2 * GB), WINDOW + 20_000);
            assert_eq!(swap.allocated_change, 0, "an unknown interval is not attributed to builds");
            assert_eq!(swap.reserve(WINDOW + 20_000), None);
        }
        for next in [WINDOW - 1, WINDOW, WINDOW + MAX_GAP + 1] {
            let mut swap = SwapDisk::default();
            feed(&mut swap, 0, WINDOW, |at| GB + at as u64 * 1000);
            swap.sample(counters(2 * GB), next);
            assert_eq!(swap.reserve(next), None);
            assert_eq!(swap.allocated_change, 0);
        }
    }

    #[test]
    fn old_compressor_samples_decode_without_a_swap_total() {
        let old = serde_json::json!({ "page_size": 16384, "uncompressed_pages": 300_000,
            "compressor_pages": 100_000, "stored_pages": 200_000, "swap_used": GB,
            "swapins": 0, "swapouts": 0 });
        let compressor: MemoryCompressor = serde_json::from_value(old).unwrap();
        assert_eq!(compressor.swap_total, None);
        let mut swap = SwapDisk::default();
        for at in (0..=WINDOW).step_by(10_000) {
            swap.sample(Some(compressor), at);
        }
        assert_eq!(swap.reserve(WINDOW), None);
        assert_eq!(swap.line(WINDOW), None);
    }
}
