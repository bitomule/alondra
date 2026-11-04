use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

struct Fault {
    point: &'static str,
    remaining: usize,
    hits: usize,
}
static FAULTS: LazyLock<Mutex<BTreeMap<PathBuf, Fault>>> = LazyLock::new(Mutex::default);
type Hook = Box<dyn FnOnce(&str) + Send>;
static SPAWNS: LazyLock<Mutex<BTreeMap<PathBuf, Hook>>> = LazyLock::new(Mutex::default);

pub(crate) struct Injection(PathBuf);
impl Injection {
    pub(crate) fn at(home: &Path, point: &'static str, occurrence: usize) -> Self {
        assert!(occurrence > 0);
        assert!(
            FAULTS
                .lock()
                .unwrap()
                .insert(
                    home.to_owned(),
                    Fault {
                        point,
                        remaining: occurrence,
                        hits: 0
                    }
                )
                .is_none()
        );
        Self(home.to_owned())
    }
    pub(crate) fn hits(&self) -> usize {
        FAULTS.lock().unwrap()[&self.0].hits
    }
}
impl Drop for Injection {
    fn drop(&mut self) {
        FAULTS.lock().unwrap().remove(&self.0);
    }
}

pub(crate) struct SpawnHook(PathBuf);
impl Drop for SpawnHook {
    fn drop(&mut self) {
        SPAWNS.lock().unwrap().remove(&self.0);
    }
}
pub(crate) fn on_spawn(home: &Path, hook: impl FnOnce(&str) + Send + 'static) -> SpawnHook {
    assert!(
        SPAWNS
            .lock()
            .unwrap()
            .insert(home.to_owned(), Box::new(hook))
            .is_none()
    );
    SpawnHook(home.to_owned())
}
pub(crate) fn spawned(home: &Path, bearer: &str) {
    let hook = SPAWNS.lock().unwrap().remove(home);
    if let Some(hook) = hook {
        hook(bearer);
    }
}

pub(crate) fn database_write(
    connection: &rusqlite::Connection,
    point: &'static str,
) -> std::io::Result<()> {
    if let Some(home) = connection.path().map(Path::new).and_then(Path::parent) {
        write(home, point)?;
    }
    Ok(())
}
pub(crate) fn owner_write(directory: &Path, point: &'static str) -> std::io::Result<()> {
    if let Some(home) = directory.parent().and_then(Path::parent) {
        write(home, point)?;
    }
    Ok(())
}

pub(crate) fn write(home: &Path, point: &'static str) -> std::io::Result<()> {
    if let Some(fault) = FAULTS
        .lock()
        .unwrap()
        .get_mut(home)
        .filter(|fault| fault.point == point)
    {
        fault.remaining = fault.remaining.saturating_sub(1);
        if fault.remaining == 0 {
            fault.hits += 1;
            return Err(std::io::Error::from_raw_os_error(libc::ENOSPC));
        }
    }
    Ok(())
}
