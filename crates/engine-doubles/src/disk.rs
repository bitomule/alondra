//! The disk of a test: every folder that exists is on the Mac's own disk, with the room a test
//! gives it. No disk of the Mac is read.
use alondra_engine_ports::disk::{Disk, Space};
use std::path::Path;
use std::sync::{Mutex, PoisonError};

const GB: u64 = 1_000_000_000;

/// Plenty of room: no admission line is near it.
pub const ROOMY: Space = Space { free: 400 * GB, purgeable: 0, total: 500 * GB };

pub struct Disks {
    own: Mutex<Space>,
}

impl Default for Disks {
    fn default() -> Self {
        Self { own: Mutex::new(ROOMY) }
    }
}

impl Disks {
    /// The space of the Mac's own disk from now on.
    pub fn set_own(&self, space: Space) {
        *self.own.lock().unwrap_or_else(PoisonError::into_inner) = space;
    }
}

impl Disk for Disks {
    fn space(&self, path: &Path) -> Option<Space> {
        path.exists().then(|| *self.own.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disks_keep_the_contract_of_the_macs() {
        crate::contract::disk(&Disks::default());
    }
}
