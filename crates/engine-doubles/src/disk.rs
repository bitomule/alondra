//! The disks of a test: every folder that exists is on the Mac's own disk, with the room a test
//! gives it, unless the test mounts another volume over a path. No disk of the Mac is read.
use alondra_engine_ports::disk::{Disk, Mounts, Space};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

const GB: u64 = 1_000_000_000;

/// Plenty of room: no admission line is near it.
pub const ROOMY: Space = Space { free: 400 * GB, purgeable: 0, total: 500 * GB };

pub struct Disks {
    own: Mutex<Space>,
    volumes: Mutex<Vec<Volume>>,
}

#[derive(Clone)]
struct Volume {
    mount: PathBuf,
    space: Space,
    held: u64,
}

impl Default for Disks {
    fn default() -> Self {
        Self { own: Mutex::new(ROOMY), volumes: Mutex::default() }
    }
}

impl Disks {
    /// The space of the Mac's own disk from now on.
    pub fn set_own(&self, space: Space) {
        *self.own.lock().unwrap_or_else(PoisonError::into_inner) = space;
    }

    /// A volume mounted at `mount`, holding `held` bytes of its own: every path under it is on it,
    /// whether or not the folder exists.
    pub fn mount(&self, mount: impl Into<PathBuf>, space: Space, held: u64) {
        let mount = mount.into();
        let mut volumes = self.volumes.lock().unwrap_or_else(PoisonError::into_inner);
        volumes.retain(|volume| volume.mount != mount);
        volumes.push(Volume { mount, space, held });
    }

    fn volume_of(&self, path: &Path) -> Option<Volume> {
        let volumes = self.volumes.lock().unwrap_or_else(PoisonError::into_inner);
        volumes.iter().filter(|volume| path.starts_with(&volume.mount)).max_by_key(|volume| volume.mount.components().count()).cloned()
    }
}

impl Disk for Disks {
    fn space(&self, path: &Path) -> Option<Space> {
        match self.volume_of(path) {
            Some(volume) => Some(volume.space),
            None => path.exists().then(|| *self.own.lock().unwrap_or_else(PoisonError::into_inner)),
        }
    }

    fn held(&self, path: &Path) -> Option<u64> {
        match self.volume_of(path) {
            Some(volume) => Some(volume.held),
            None => path.exists().then(|| {
                let own = self.own.lock().unwrap_or_else(PoisonError::into_inner);
                own.total.saturating_sub(own.free)
            }),
        }
    }

    fn mount_point(&self, path: &Path) -> Option<PathBuf> {
        match self.volume_of(path) {
            Some(volume) => Some(volume.mount),
            None => path.exists().then(|| PathBuf::from("/")),
        }
    }

    fn same_volume(&self, a: &Path, b: &Path) -> bool {
        match (self.mount_point(a), self.mount_point(b)) {
            (Some(a), Some(b)) => a == b,
            _ => false,
        }
    }

    fn mounted_volumes(&self) -> std::io::Result<Mounts> {
        let volumes = self.volumes.lock().unwrap_or_else(PoisonError::into_inner);
        let candidates: Vec<PathBuf> = volumes.iter().map(|volume| volume.mount.clone()).collect();
        let all: BTreeSet<PathBuf> = candidates.iter().cloned().chain([PathBuf::from("/")]).collect();
        Ok(Mounts { all, candidates })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_disks_keep_the_contract_of_the_macs() {
        crate::contract::disk(&Disks::default());
    }

    #[test]
    fn a_mounted_volume_holds_every_path_under_it_and_is_a_candidate() {
        let disks = Disks::default();
        let space = Space { free: GB, purgeable: 0, total: 40 * GB };
        disks.mount("/Volumes/Build", space, 3 * GB);
        let inside = Path::new("/Volumes/Build/repo");
        assert_eq!(disks.space(inside), Some(space));
        assert_eq!(disks.held(inside), Some(3 * GB));
        assert_eq!(disks.mount_point(inside), Some(PathBuf::from("/Volumes/Build")));
        assert!(!disks.same_volume(inside, Path::new("/")));
        assert_eq!(disks.mounted_volumes().unwrap().candidates, [PathBuf::from("/Volumes/Build")]);
    }
}
