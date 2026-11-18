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
        std::hint::black_box(digest(&path).unwrap());
        std::hint::black_box(digest(&path).unwrap());
    }
    eprintln!(
        "artifact_bytes={} rounds={rounds} verifications={} sha256_reads={} elapsed_ms={}",
        std::fs::metadata(&path).unwrap().len(),
        rounds * 2,
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
