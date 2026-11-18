//! Digests shared by host discovery, launch and adoption.
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    size: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}
impl From<Metadata> for Identity {
    fn from(value: Metadata) -> Self {
        Self {
            device: value.dev(),
            inode: value.ino(),
            size: value.len(),
            modified: (value.mtime(), value.mtime_nsec()),
            changed: (value.ctime(), value.ctime_nsec()),
        }
    }
}
#[derive(Default)]
struct Cache {
    entries: Mutex<HashMap<PathBuf, (Identity, String)>>,
}
impl Cache {
    fn digest(
        &self,
        path: &Path,
        hash: impl FnOnce(&mut File) -> io::Result<String>,
    ) -> io::Result<String> {
        // Holding the lock through hashing shares the first stable read with concurrent callers.
        let mut entries = self
            .entries
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let canonical = path.canonicalize()?;
        let mut file = File::open(&canonical)?;
        let before = Identity::from(file.metadata()?);
        let digest = if let Some((identity, digest)) = entries.get(&canonical)
            && *identity == before
        {
            digest.clone()
        } else {
            hash(&mut file)?
        };
        if Identity::from(file.metadata()?) != before
            || Identity::from(std::fs::metadata(&canonical)?) != before
            || path.canonicalize()? != canonical
        {
            return Err(io::Error::other("host artifact changed while hashing"));
        }
        entries.insert(canonical, (before, digest.clone()));
        Ok(digest)
    }
}
/// Hash once per canonical path and file identity. Errors and unstable reads are never cached.
pub fn digest(path: &Path) -> io::Result<String> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Cache::default).digest(path, hash)
}
fn hash(file: &mut File) -> io::Result<String> {
    #[cfg(test)]
    HASH_READS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

#[cfg(test)]
static HASH_READS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
#[path = "artifact_digest_tests.rs"]
mod tests;
