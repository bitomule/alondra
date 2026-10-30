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
mod tests {
    use super::*;
    use crate::test_support;
    use std::sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    #[ignore = "explicit benchmark with ALONDRA_HASH_BENCH_ARTIFACT and ALONDRA_HASH_BENCH_ROUNDS"]
    fn benchmark_host_discovery_and_verification() {
        let path = PathBuf::from(std::env::var_os("ALONDRA_HASH_BENCH_ARTIFACT").unwrap());
        let rounds: usize = std::env::var("ALONDRA_HASH_BENCH_ROUNDS")
            .unwrap()
            .parse()
            .unwrap();
        let reads = HASH_READS.load(Ordering::Relaxed);
        let started = std::time::Instant::now();
        for _ in 0..rounds {
            let expected = digest(&path).unwrap();
            std::hint::black_box(digest(&path).unwrap());
            std::hint::black_box(
                crate::owned_host::VerifiedArtifact::verify(&path, &path, &expected, &expected)
                    .unwrap(),
            );
        }
        eprintln!(
            "artifact_bytes={} rounds={rounds} verifications={} sha256_reads={} elapsed_ms={}",
            std::fs::metadata(&path).unwrap().len(),
            rounds * 4,
            HASH_READS.load(Ordering::Relaxed) - reads,
            started.elapsed().as_millis()
        );
    }

    #[test]
    fn concurrent_discovery_and_verification_hash_one_identity_once() {
        let directory = test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        let path = directory.join("host");
        std::fs::write(&path, b"host").unwrap();
        let cache = Arc::new(Cache::default());
        let count = Arc::new(AtomicUsize::new(0));
        let barrier = Arc::new(Barrier::new(8));
        std::thread::scope(|scope| {
            for _ in 0..8 {
                let (cache, count, barrier, path) =
                    (cache.clone(), count.clone(), barrier.clone(), path.clone());
                scope.spawn(move || {
                    barrier.wait();
                    cache
                        .digest(&path, |file| {
                            count.fetch_add(1, Ordering::SeqCst);
                            hash(file)
                        })
                        .unwrap();
                });
            }
        });
        assert_eq!(count.load(Ordering::SeqCst), 1);
        let alias = directory.join("host-alias");
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        cache
            .digest(&alias, |_| {
                panic!("canonical aliases reuse the same digest")
            })
            .unwrap();
    }

    #[test]
    fn replacement_and_preserved_mtime_invalidate_expected_digest() {
        let directory = test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        let path = directory.join("host");
        std::fs::write(&path, b"good").unwrap();
        let expected = digest(&path).unwrap();
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, b"evil").unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
        assert_ne!(digest(&path).unwrap(), expected);
        assert!(
            crate::owned_host::VerifiedArtifact::verify(&path, &path, &expected, &expected)
                .is_err()
        );
        let replacement = directory.join("replacement");
        std::fs::write(&replacement, b"good").unwrap();
        std::fs::rename(&replacement, &path).unwrap();
        assert_eq!(digest(&path).unwrap(), expected);
    }

    #[test]
    fn failed_or_unstable_reads_are_not_cached() {
        let directory = test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        let path = directory.join("host");
        std::fs::write(&path, b"good").unwrap();
        let cache = Cache::default();
        assert!(
            cache
                .digest(&path, |_| Err(io::Error::other("read failure")))
                .is_err()
        );
        assert!(
            cache
                .digest(&path, |file| {
                    let result = hash(file)?;
                    std::fs::write(&path, b"changed length").unwrap();
                    Ok(result)
                })
                .is_err()
        );
        assert!(cache.entries.lock().unwrap().is_empty());
        assert_eq!(cache.digest(&path, hash).unwrap(), digest(&path).unwrap());
    }
}
