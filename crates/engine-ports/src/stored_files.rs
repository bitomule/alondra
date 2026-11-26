//! The daemon's stored copies read back in bounded chunks: the verified-copy cache the runtime keeps,
//! the attachment a v2 command carries for a stored file, and the safe opening of a stored path.
use alondra_core::clock;
use alondra_core::clock::Instant;
use alondra_foundation::locks::Poisonable as _;
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

pub const CHUNK: usize = 64 * 1024;
const TTL: Duration = Duration::from_secs(600);
const SLOTS: usize = 2;
/// The transfer failures a client acts on; any other failure is a rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferError {
    Unknown,
    FinalizationUncertain,
    Capacity,
    FileChanged,
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unknown => "transfer expired or missing; restart upload",
            Self::FinalizationUncertain => "transfer already finalized or interrupted; previous receipt unavailable",
            Self::Capacity => "transfer capacity reached; cancel or finish an existing transfer",
            Self::FileChanged => "file changed; restart download",
        })
    }
}

impl std::error::Error for TransferError {}

const READ_CACHE_BYTES: usize = 2 * crate::uploads::MAX_UPLOAD_BYTES;
#[derive(Default)]
pub struct StoredReads {
    hashed_bytes: std::sync::atomic::AtomicU64,
    reserved: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// A holder that panics leaves it whole, so it is recovered: a cache of verified copies.
    copies: Mutex<HashMap<(PathBuf, String), CachedCopy>>,
}
struct CachedCopy {
    copy: std::sync::Arc<VerifiedCopy>,
    touched: Instant,
}
struct ReadReservation {
    bytes: usize,
    total: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for ReadReservation {
    fn drop(&mut self) {
        self.total.fetch_sub(self.bytes, std::sync::atomic::Ordering::Relaxed);
    }
}
struct VerifiedCopy {
    data: Vec<u8>,
    digest: String,
    _reservation: ReadReservation,
}
impl StoredReads {
    pub fn attachment(&self, home: &Path, reference: &str, digest: &str,
        offset: u64, version: Option<String>) -> Result<Value> {
        self.chunk(home, reference, Some(digest), offset, version, alondra_domain_model::MAX_ATTACHMENT_BYTES).map(|(chunk, _)| chunk)
    }
    pub fn verified(&self, home: &Path, reference: &str, digest: &str,
        offset: u64, version: Option<String>) -> Result<Value> {
        self.chunk(home, reference, Some(digest), offset, version, crate::uploads::MAX_UPLOAD_BYTES as u64).map(|(chunk, _)| chunk)
    }
    pub fn stored(&self, home: &Path, reference: &str, offset: u64,
        version: Option<String>) -> Result<Value> {
        let (mut chunk, digest) = self.chunk(home, reference, None, offset, version, crate::uploads::MAX_UPLOAD_BYTES as u64)?;
        chunk["content_digest"] = json!(digest);
        Ok(chunk)
    }
    fn chunk(&self, home: &Path, reference: &str, expected: Option<&str>,
        offset: u64, version: Option<String>, maximum: u64) -> Result<(Value, String)> {
        use std::sync::atomic::Ordering;
        let path = Path::new(reference);
        let path = if path.is_absolute() { path.to_path_buf() } else { home.join(path) };
        let mut file = open_stored_file(home, &path)?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file() && metadata.len() <= maximum,
            "invalid stored document");
        let current_version = file_version(&metadata);
        ensure!(offset <= metadata.len() && (offset == 0 || version.is_some()), "invalid offset");
        ensure!(version.as_ref().is_none_or(|v| v == &current_version), TransferError::FileChanged);
        let key = (path, current_version.clone());
        let now = clock::instant();
        let (cached, reservation) = {
            let mut copies = self.copies.lock_or_recover("the stored read cache");
            copies.retain(|_, copy| now.duration_since(copy.touched) < TTL);
            if let Some(entry) = copies.get_mut(&key) {
                entry.touched = now;
                (Some(entry.copy.clone()), None)
            } else {
                let size = metadata.len() as usize;
                while !copies.is_empty() && (copies.len() >= SLOTS
                    || self.reserved.load(Ordering::Relaxed).saturating_add(size) > READ_CACHE_BYTES) {
                    let oldest = copies.iter().min_by_key(|(_, copy)| copy.touched).unwrap().0.clone();
                    copies.remove(&oldest);
                }
                let reservation = self.reserved.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
                    |bytes| bytes.checked_add(size).filter(|total| *total <= READ_CACHE_BYTES)).ok()
                    .map(|_| ReadReservation { bytes: size, total: self.reserved.clone() });
                (None, reservation)
            }
        };
        let copy = if let Some(copy) = cached { copy } else if let Some(reservation) = reservation {
            let mut data = vec![0; metadata.len() as usize];
            file.read_exact(&mut data)?;
            let mut extra = [0];
            ensure!(file.read(&mut extra)? == 0, TransferError::FileChanged);
            let digest = hex_digest(Sha256::digest(&data).as_slice());
            self.hashed_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
            ensure!(file_version(&file.metadata()?) == current_version, TransferError::FileChanged);
            ensure!(expected.is_none_or(|value| value == digest), "stored document digest does not match revision");
            let copy = std::sync::Arc::new(VerifiedCopy { data, digest, _reservation: reservation });
            let mut copies = self.copies.lock_or_recover("the stored read cache");
            while copies.len() >= SLOTS {
                let oldest = copies.iter().min_by_key(|(_, entry)| entry.touched).unwrap().0.clone();
                copies.remove(&oldest);
            }
            copies.insert(key, CachedCopy { copy: copy.clone(), touched: clock::instant() });
            copy
        } else {
            // Evicted copies may still be serving another request; their reservations remain live.
            return read_chunk(home, reference, expected, offset, version, Some(&self.hashed_bytes), maximum);
        };
        ensure!(expected.is_none_or(|value| value == copy.digest), "stored document digest does not match revision");
        ensure!(file_version(&file.metadata()?) == current_version, TransferError::FileChanged);
        let start = offset as usize;
        let end = start.saturating_add(CHUNK).min(copy.data.len());
        Ok((json!({"version":current_version,"data":URL_SAFE_NO_PAD.encode(&copy.data[start..end]),
            "offset":offset,"total":copy.data.len(),"next_offset":end,"eof":end==copy.data.len()}), copy.digest.clone()))
    }
}

/// The attachment a v2 command carries, for a file the daemon stored: its digest is
/// computed from the stored copy and its reference is relative to this HOME.
pub fn attachment_ref(home: &Path, id: String, name: String, stored: &Path) -> Result<alondra_domain_model::AttachmentRef> {
    let mut file = open_stored_file(home, stored)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
        total += count as u64;
    }
    let reference = stored.strip_prefix(home).unwrap_or(stored);
    Ok(alondra_domain_model::AttachmentRef {
        id,
        name,
        content_digest: hex_digest(hasher.finalize().as_slice()),
        content_reference: reference.to_string_lossy().into_owned(),
        bytes: total,
    })
}

fn read_chunk(home: &Path, reference: &str, expected: Option<&str>,
    offset: u64, version: Option<String>, hashed: Option<&std::sync::atomic::AtomicU64>, maximum: u64) -> Result<(Value, String)> {
    let path = Path::new(reference);
    let path = if path.is_absolute() { path.to_path_buf() } else { home.join(path) };
    let mut file = open_stored_file(home, &path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file() && metadata.len() <= maximum,
        "invalid stored document");
    let current_version = file_version(&metadata);
    ensure!(offset <= metadata.len() && (offset == 0 || version.is_some()), "invalid offset");
    ensure!(version.as_ref().is_none_or(|v| v == &current_version), TransferError::FileChanged);
    let mut hash = Sha256::new();
    let mut position = 0u64;
    let mut bytes = Vec::with_capacity(CHUNK.min((metadata.len() - offset) as usize));
    let end = offset.saturating_add(CHUNK as u64).min(metadata.len());
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        hash.update(&buffer[..count]);
        let segment_end = position + count as u64;
        if segment_end > offset && position < end {
            let start = offset.saturating_sub(position) as usize;
            let stop = (end.min(segment_end) - position) as usize;
            bytes.extend_from_slice(&buffer[start..stop]);
        }
        position = segment_end;
        ensure!(position <= maximum, "stored document too large");
    }
    if let Some(hashed) = hashed { hashed.fetch_add(position, std::sync::atomic::Ordering::Relaxed); }
    let actual = hex_digest(hash.finalize().as_slice());
    ensure!(position == metadata.len() && expected.is_none_or(|digest| actual == digest),
        "stored document digest does not match revision");
    let after = file.metadata()?;
    ensure!(file_version(&after) == current_version, TransferError::FileChanged);
    let next = offset + bytes.len() as u64;
    Ok((json!({"version":current_version,"data":URL_SAFE_NO_PAD.encode(bytes),
        "offset":offset,"total":metadata.len(),"next_offset":next,"eof":next==metadata.len()}), actual))
}

pub fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn file_version(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}-{}-{}-{}-{}-{}-{}", metadata.dev(), metadata.ino(), metadata.len(),
        metadata.mtime(), metadata.mtime_nsec(), metadata.ctime(), metadata.ctime_nsec())
}

pub fn open_stored_file(home: &Path, path: &Path) -> Result<std::fs::File> {
    let root = home.join("files").canonicalize()?;
    let path = path.canonicalize()?;
    let relative = path.strip_prefix(&root).context("only Alondra stored copies may be read")?;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let root = std::fs::OpenOptions::new().read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY).open(root)?;
    let mut directory: OwnedFd = root.into();
    let components: Vec<_> = relative.components().collect();
    ensure!(!components.is_empty(), "not a regular file");
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else { anyhow::bail!("invalid stored path") };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let flags = libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK
            | if index + 1 < components.len() { libc::O_DIRECTORY } else { 0 };
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        ensure!(fd >= 0, "stored file unavailable");
        directory = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    Ok(std::fs::File::from(directory))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn home() -> crate::test_support::Directory {
        let path = crate::test_support::Directory::new();
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    #[test]
    fn attachments_above_the_upload_limit_download_without_growing_the_cache_budget() {
        let h = home();
        std::fs::create_dir(h.join("files")).unwrap();
        let bytes = vec![37u8; READ_CACHE_BYTES + 7];
        std::fs::write(h.join("files/video.mp4"), &bytes).unwrap();
        let digest = hex_digest(Sha256::digest(&bytes).as_slice());
        let reads = StoredReads::default();
        let first = reads.attachment(&h, "files/video.mp4", &digest, 0, None).unwrap();
        assert_eq!(first["total"], bytes.len());
        assert_eq!(URL_SAFE_NO_PAD.decode(first["data"].as_str().unwrap()).unwrap(), bytes[..CHUNK]);
        assert!(reads.verified(&h, "files/video.mp4", &digest, 0, None).is_err());
        assert_eq!(reads.reserved.load(std::sync::atomic::Ordering::Relaxed), 0);
        let end = reads.attachment(&h, "files/video.mp4", &digest, (bytes.len() - 7) as u64,
            Some(first["version"].as_str().unwrap().into())).unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(end["data"].as_str().unwrap()).unwrap(), bytes[bytes.len() - 7..]);
        assert_eq!(end["eof"], true);
        let file = std::fs::OpenOptions::new().write(true).open(h.join("files/video.mp4")).unwrap();
        file.set_len(alondra_domain_model::MAX_ATTACHMENT_BYTES + 1).unwrap();
        let mut grown_hash = Sha256::new();
        grown_hash.update(&bytes);
        let zeros = [0u8; CHUNK];
        let mut remaining = alondra_domain_model::MAX_ATTACHMENT_BYTES as usize + 1 - bytes.len();
        while remaining > 0 {
            let count = remaining.min(zeros.len());
            grown_hash.update(&zeros[..count]);
            remaining -= count;
        }
        let grown_digest = hex_digest(grown_hash.finalize().as_slice());
        assert!(reads.attachment(&h, "files/video.mp4", &grown_digest, 0, None).is_err());
    }

    #[test]
    #[ignore = "opt-in isolated stored-file profiling"]
    fn profile_verified_download() {
        fn cpu() -> f64 {
            let mut value = std::mem::MaybeUninit::<libc::rusage>::uninit();
            assert_eq!(unsafe { libc::getrusage(libc::RUSAGE_SELF, value.as_mut_ptr()) }, 0);
            let value = unsafe { value.assume_init() };
            value.ru_utime.tv_sec as f64 + value.ru_stime.tv_sec as f64
                + (value.ru_utime.tv_usec + value.ru_stime.tv_usec) as f64 / 1_000_000.0
        }
        let h = home();
        std::fs::create_dir(h.join("files")).unwrap();
        let bytes = vec![37u8; 2 * 1024 * 1024];
        std::fs::write(h.join("files/fixture.bin"), &bytes).unwrap();
        let digest = hex_digest(Sha256::digest(&bytes).as_slice());
        let mut samples = Vec::new();
        for _ in 0..10 {
            let reads = StoredReads::default();
            let start = Instant::now(); let cpu_start = cpu();
            let mut version = None; let mut offset = 0; let mut actual = Vec::new();
            loop {
                let reply = reads.verified(&h, "files/fixture.bin", &digest, offset, version.clone()).unwrap();
                actual.extend(URL_SAFE_NO_PAD.decode(reply["data"].as_str().unwrap()).unwrap());
                version = Some(reply["version"].as_str().unwrap().to_owned());
                offset = reply["next_offset"].as_u64().unwrap();
                if reply["eof"].as_bool().unwrap() { break; }
            }
            let cpu_ms = (cpu() - cpu_start) * 1000.0;
            let wall_ms = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(actual, bytes);
            samples.push(json!({"cpu_ms":cpu_ms,"wall_ms":wall_ms,
                "hashed_bytes":reads.hashed_bytes.load(std::sync::atomic::Ordering::Relaxed),
                "retained_snapshot_bytes":reads.reserved.load(std::sync::atomic::Ordering::Relaxed)}));
        }
        println!("{}",json!({"scope":"verified-file-download","bytes":bytes.len(),"chunks":bytes.len()/CHUNK,"errors":0,"samples":samples}));
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn cached_download_still_checks_digest_mutation_and_path_scope() {
        let h = home();
        std::fs::create_dir(h.join("files")).unwrap();
        let path = h.join("files/fixture.bin");
        let bytes = vec![37u8; CHUNK * 2];
        std::fs::write(&path, &bytes).unwrap();
        let digest = hex_digest(Sha256::digest(&bytes).as_slice());
        let reads = StoredReads::default();
        let first = reads.stored(&h, "files/fixture.bin", 0, None).unwrap();
        assert_eq!(first["content_digest"], digest);
        let version = Some(first["version"].as_str().unwrap().to_owned());
        assert!(reads.verified(&h, "files/fixture.bin", "wrong", CHUNK as u64, version.clone()).is_err());
        std::fs::write(&path, vec![91u8; bytes.len()]).unwrap();
        std::fs::File::options().write(true).open(&path).unwrap().set_times(
            std::fs::FileTimes::new().set_modified(std::time::SystemTime::UNIX_EPOCH)).unwrap();
        assert!(reads.verified(&h, "files/fixture.bin", &digest, CHUNK as u64, version).is_err());
        assert!(reads.verified(&h, "files/fixture.bin", &digest, 0, None).is_err());
        let replacement = vec![91u8; bytes.len()];
        let replacement_digest = hex_digest(Sha256::digest(&replacement).as_slice());
        let fresh = reads.verified(&h, "files/fixture.bin", &replacement_digest, 0, None).unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(fresh["data"].as_str().unwrap()).unwrap(), replacement[..CHUNK]);
        std::fs::rename(&path, h.join("outside.bin")).unwrap();
        assert!(reads.stored(&h, "outside.bin", 0, None).is_err());
        assert!(reads.stored(&h, "files/fixture.bin", 0, None).is_err());
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn read_cache_limits_include_evicted_copies_still_in_use() {
        use std::sync::atomic::Ordering;
        let h = home();
        std::fs::create_dir(h.join("files")).unwrap();
        let reads = StoredReads::default();
        let bytes = vec![1u8; crate::uploads::MAX_UPLOAD_BYTES];
        let digest = hex_digest(Sha256::digest(&bytes).as_slice());
        for name in ["a", "b", "c"] { std::fs::write(h.join("files").join(name), &bytes).unwrap(); }
        reads.verified(&h, "files/a", &digest, 0, None).unwrap();
        let pinned_a = reads.copies.lock().unwrap().values().next().unwrap().copy.clone();
        reads.verified(&h, "files/b", &digest, 0, None).unwrap();
        let pinned_b = reads.copies.lock().unwrap().values().find(|entry|
            !std::sync::Arc::ptr_eq(&entry.copy, &pinned_a)).unwrap().copy.clone();
        assert_eq!(reads.reserved.load(Ordering::Relaxed), READ_CACHE_BYTES);
        let fallback = reads.verified(&h, "files/c", &digest, 0, None).unwrap();
        assert_eq!(URL_SAFE_NO_PAD.decode(fallback["data"].as_str().unwrap()).unwrap(), bytes[..CHUNK]);
        assert!(reads.copies.lock().unwrap().is_empty());
        assert_eq!(reads.reserved.load(Ordering::Relaxed), READ_CACHE_BYTES);
        drop(pinned_a); drop(pinned_b);
        assert_eq!(reads.reserved.load(Ordering::Relaxed), 0);
        reads.verified(&h, "files/c", &digest, 0, None).unwrap();
        assert_eq!(reads.copies.lock().unwrap().len(), 1);
        assert_eq!(reads.reserved.load(Ordering::Relaxed), bytes.len());
        drop(reads);
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn failed_digest_read_releases_its_cache_reservation() {
        use std::sync::atomic::Ordering;
        let h = home();
        std::fs::create_dir(h.join("files")).unwrap();
        std::fs::write(h.join("files/fixture.bin"), b"contents").unwrap();
        let reads = StoredReads::default();
        assert!(reads.verified(&h, "files/fixture.bin", "wrong", 0, None).is_err());
        assert_eq!(reads.reserved.load(Ordering::Relaxed), 0);
        assert!(reads.copies.lock().unwrap().is_empty());
        std::fs::remove_dir_all(h).unwrap();
    }

    #[test]
    fn verified_download_hashes_a_stable_copy_once_not_once_per_chunk() {
        let h = home();
        std::fs::create_dir(h.join("files")).unwrap();
        let bytes = vec![37u8; 1024 * 1024];
        std::fs::write(h.join("files/fixture.bin"), &bytes).unwrap();
        let digest = hex_digest(Sha256::digest(&bytes).as_slice());
        let reads = StoredReads::default();
        let mut version = None;
        let mut offset = 0;
        let mut downloaded = Vec::new();
        loop {
            let reply = reads.verified(&h, "files/fixture.bin", &digest, offset, version.clone()).unwrap();
            downloaded.extend(URL_SAFE_NO_PAD.decode(reply["data"].as_str().unwrap()).unwrap());
            version = Some(reply["version"].as_str().unwrap().to_owned());
            offset = reply["next_offset"].as_u64().unwrap();
            if reply["eof"].as_bool().unwrap() { break; }
        }
        assert_eq!(downloaded, bytes);
        assert_eq!(reads.hashed_bytes.load(std::sync::atomic::Ordering::Relaxed), bytes.len() as u64,
            "a stable full download should not hash the same megabyte sixteen times");
        std::fs::remove_dir_all(h).unwrap();
    }
}
