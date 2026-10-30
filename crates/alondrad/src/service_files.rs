//! Bounded, device-owned attachment transfers inside the encrypted service channel.
use crate::locks::Poisonable as _;
use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

pub const CHUNK: usize = 64 * 1024;
const TTL: Duration = Duration::from_secs(600);
const SLOTS: usize = 2;
const UNCERTAIN: &str = "transfer already finalized or interrupted; previous receipt unavailable";
/// A holder that panics leaves it whole, so it is recovered: an upload that panics while finishing
/// stays marked attempted without a result, which is refused as uncertain as after a restart.
#[derive(Default)]
pub struct Transfers(Mutex<HashMap<(String, String), Transfer>>);
struct Transfer {
    name: String,
    size: usize,
    hash: String,
    data: Vec<u8>,
    result: Option<Value>,
    finalization_attempted: bool,
    received: usize,
    touched: Instant,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    transfer_id: String,
    name: String,
    bytes: usize,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    #[serde(rename = "transfer_id")]
    _transfer_id: String,
    offset: usize,
    data: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Id {
    #[serde(rename = "transfer_id")]
    _transfer_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Download {
    path: PathBuf,
    offset: u64,
    length: usize,
    #[serde(default)]
    version: Option<String>,
}
fn valid_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, c)| {
            if [8, 13, 18, 23].contains(&i) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

fn upload_id(sender: &str, id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(sender.as_bytes());
    hash.update([0]);
    hash.update(id.as_bytes());
    hash.finalize()
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect()
}
impl Transfers {
    pub fn dispatch(&self, home: &Path, sender: &str, route: &str, body: Value) -> Result<Value> {
        let renamed;
        let route = match route.strip_prefix("/uploads/") {
            Some(step) => {
                renamed = format!("/v1/service/uploads/{step}");
                renamed.as_str()
            }
            None => route,
        };
        if route == "/v1/service/files/chunk" {
            return download(home, serde_json::from_value(body)?);
        }
        let mut transfers = self.0.lock_or_recover("the file transfers");
        transfers.retain(|_, t| t.touched.elapsed() < TTL);
        if route == "/v1/service/uploads/start" {
            let b: Start = serde_json::from_value(body)?;
            ensure!(valid_id(&b.transfer_id), "invalid transfer ID");
            ensure!(
                b.bytes > 0 && b.bytes <= crate::uploads::MAX_UPLOAD_BYTES,
                "invalid file size"
            );
            ensure!(!b.name.is_empty() && b.name.len() <= 1024, "invalid name");
            ensure!(
                b.sha256.len() == 64
                    && b.sha256
                        .bytes()
                        .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                "invalid SHA256"
            );
            let key = (sender.to_owned(), b.transfer_id);
            if let Some(t) = transfers.get_mut(&key) {
                ensure!(!t.finalization_attempted || t.result.is_some(), UNCERTAIN);
                ensure!(
                    t.name == b.name && t.size == b.bytes && t.hash == b.sha256,
                    "transfer ID already used with different metadata"
                );
                t.touched = Instant::now();
                return Ok(
                    json!({"offset":t.received,"chunk_bytes":CHUNK,"completed":t.result.is_some()}),
                );
            }
            let ledger = ledger(home)?;
            let seen: bool = ledger.query_row(
                "SELECT EXISTS(SELECT 1 FROM attempts WHERE sender=?1 AND transfer_id=?2)",
                rusqlite::params![sender, &key.1],
                |r| r.get(0),
            )?;
            ensure!(
                !seen,
                "transfer already finalized or interrupted; previous receipt unavailable"
            );
            ensure!(
                transfers.len() < 32
                    && transfers.values().filter(|t| !t.finalization_attempted).count() < SLOTS,
                "transfer capacity reached; cancel or finish an existing transfer"
            );
            transfers.insert(
                key,
                Transfer {
                    name: b.name,
                    size: b.bytes,
                    hash: b.sha256,
                    data: Vec::with_capacity(b.bytes),
                    result: None,
                    finalization_attempted: false,
                    received: 0,
                    touched: Instant::now(),
                },
            );
            return Ok(json!({"offset":0,"chunk_bytes":CHUNK,"completed":false}));
        }
        let id = body
            .get("transfer_id")
            .and_then(Value::as_str)
            .context("missing transfer ID")?;
        ensure!(valid_id(id), "invalid transfer ID");
        let key = (sender.to_owned(), id.to_owned());
        if route == "/v1/service/uploads/cancel" {
            let _: Id = serde_json::from_value(body)?;
            transfers.remove(&key);
            return Ok(json!({"cancelled":true}));
        }
        if !transfers.contains_key(&key) {
            ensure!(!attempted(home, sender, &key.1)?, UNCERTAIN);
        }
        let t = transfers
            .get_mut(&key)
            .context("transfer expired or missing; restart upload")?;
        ensure!(!t.finalization_attempted || t.result.is_some(), UNCERTAIN);
        if route == "/v1/service/uploads/chunk" {
            let b: Chunk = serde_json::from_value(body)?;
            ensure!(b.data.len() <= CHUNK.div_ceil(3) * 4, "chunk too large");
            let bytes = URL_SAFE_NO_PAD.decode(b.data)?;
            ensure!(
                !bytes.is_empty() && bytes.len() <= CHUNK,
                "invalid chunk size"
            );
            let end = b
                .offset
                .checked_add(bytes.len())
                .context("invalid offset")?;
            ensure!(
                end <= t.size && b.offset <= t.data.len(),
                "chunk outside file or out of order"
            );
            if b.offset < t.data.len() {
                ensure!(
                    end <= t.data.len() && t.data[b.offset..end] == bytes,
                    "retry differs from stored chunk"
                );
            } else {
                ensure!(t.result.is_none(), "transfer already completed");
                t.data.extend_from_slice(&bytes);
                t.received = t.data.len();
            }
            t.touched = Instant::now();
            return Ok(json!({"offset":t.data.len()}));
        }
        ensure!(
            route == "/v1/service/uploads/finish",
            "unknown transfer operation"
        );
        let _: Id = serde_json::from_value(body)?;
        if let Some(result) = &t.result {
            return Ok(result.clone());
        }
        ensure!(t.data.len() == t.size, "incomplete upload");
        let actual: String = Sha256::digest(&t.data)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        ensure!(actual == t.hash, "file digest mismatch");
        // Commit an attempt before the filesystem side effect. A crash leaves a
        // tombstone and refuses reuse, rather than silently duplicating a file.
        let mut ledger = ledger(home)?;
        let transaction = ledger.transaction()?;
        let seen: bool = transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM attempts WHERE sender=?1 AND transfer_id=?2)",
            rusqlite::params![sender, &key.1],
            |r| r.get(0),
        )?;
        ensure!(!seen, UNCERTAIN);
        let count: i64 =
            transaction.query_row("SELECT count(*) FROM attempts", [], |r| r.get(0))?;
        ensure!(count < 10000, "transfer ledger capacity reached");
        transaction.execute(
            "INSERT INTO attempts(sender,transfer_id) VALUES(?1,?2)",
            rusqlite::params![sender, &key.1],
        )?;
        t.finalization_attempted = true;
        let data = std::mem::take(&mut t.data);
        transaction.commit().context(UNCERTAIN)?;
        let (id, a) =
            crate::uploads::store_identified(home, &t.name, &data, &upload_id(sender, &key.1))
                .context(UNCERTAIN)?;
        let result = json!({"id":id,"name":a.name,"bytes":a.bytes,"kind":if crate::uploads::is_image(&a.name) {"image"} else {"file"},"path":a.stored_path});
        t.result = Some(result.clone());
        t.touched = Instant::now();
        Ok(result)
    }
}
impl Transfers {
    /// A step of an upload as the domain routes name it. What `finish` answers is the attachment
    /// a v2 command carries.
    pub fn dispatch_domain(&self, home: &Path, sender: &str, route: &str, body: Value) -> Result<Value> {
        let finished = self.dispatch(home, sender, route, body)?;
        let (Some(id), Some(name), Some(stored)) = (finished.get("id").and_then(Value::as_str),
            finished.get("name").and_then(Value::as_str), finished.get("path").and_then(Value::as_str)) else {
            return Ok(finished);
        };
        let attachment = attachment_ref(home, id.to_owned(), name.to_owned(), Path::new(stored))?;
        Ok(json!({"attachment": attachment}))
    }
}
pub fn error_code(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "transfer expired or missing; restart upload" => "attachment_transfer_unknown",
        "transfer already finalized or interrupted; previous receipt unavailable" => {
            "attachment_transfer_finalization_uncertain"
        }
        "transfer capacity reached; cancel or finish an existing transfer"
        | "transfer ledger capacity reached" => "attachment_transfer_capacity",
        "file changed; restart download" | "file changed during read" => "attachment_file_changed",
        _ => "attachment_transfer_rejected",
    }
}
fn attempted(home: &Path, sender: &str, id: &str) -> Result<bool> {
    Ok(ledger(home)?.query_row(
        "SELECT EXISTS(SELECT 1 FROM attempts WHERE sender=?1 AND transfer_id=?2)",
        rusqlite::params![sender, id],
        |r| r.get(0),
    )?)
}
fn ledger(home: &Path) -> Result<rusqlite::Connection> {
    let connection = rusqlite::Connection::open(home.join("service-upload-attempts.sqlite"))?;
    connection.execute_batch("CREATE TABLE IF NOT EXISTS attempts(sender TEXT NOT NULL, transfer_id TEXT NOT NULL, PRIMARY KEY(sender,transfer_id));")?;
    Ok(connection)
}
const READ_CACHE_BYTES: usize = 2 * crate::uploads::MAX_UPLOAD_BYTES;
#[derive(Default)]
pub(crate) struct StoredReads {
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
    pub(crate) fn attachment(&self, home: &Path, reference: &str, digest: &str,
        offset: u64, version: Option<String>) -> Result<Value> {
        self.chunk(home, reference, Some(digest), offset, version, alondra_core::domain::MAX_ATTACHMENT_BYTES).map(|(chunk, _)| chunk)
    }
    pub(crate) fn verified(&self, home: &Path, reference: &str, digest: &str,
        offset: u64, version: Option<String>) -> Result<Value> {
        self.chunk(home, reference, Some(digest), offset, version, crate::uploads::MAX_UPLOAD_BYTES as u64).map(|(chunk, _)| chunk)
    }
    pub(crate) fn stored(&self, home: &Path, reference: &str, offset: u64,
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
        ensure!(version.as_ref().is_none_or(|v| v == &current_version), "file changed; restart download");
        let key = (path, current_version.clone());
        let now = Instant::now();
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
            ensure!(file.read(&mut extra)? == 0, "file changed during read");
            let digest = hex_digest(Sha256::digest(&data).as_slice());
            self.hashed_bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
            ensure!(file_version(&file.metadata()?) == current_version, "file changed during read");
            ensure!(expected.is_none_or(|value| value == digest), "stored document digest does not match revision");
            let copy = std::sync::Arc::new(VerifiedCopy { data, digest, _reservation: reservation });
            let mut copies = self.copies.lock_or_recover("the stored read cache");
            while copies.len() >= SLOTS {
                let oldest = copies.iter().min_by_key(|(_, entry)| entry.touched).unwrap().0.clone();
                copies.remove(&oldest);
            }
            copies.insert(key, CachedCopy { copy: copy.clone(), touched: Instant::now() });
            copy
        } else {
            // Evicted copies may still be serving another request; their reservations remain live.
            return read_chunk(home, reference, expected, offset, version, Some(&self.hashed_bytes), maximum);
        };
        ensure!(expected.is_none_or(|value| value == copy.digest), "stored document digest does not match revision");
        ensure!(file_version(&file.metadata()?) == current_version, "file changed during read");
        let start = offset as usize;
        let end = start.saturating_add(CHUNK).min(copy.data.len());
        Ok((json!({"version":current_version,"data":URL_SAFE_NO_PAD.encode(&copy.data[start..end]),
            "offset":offset,"total":copy.data.len(),"next_offset":end,"eof":end==copy.data.len()}), copy.digest.clone()))
    }
}

/// The attachment a v2 command carries, for a file the daemon stored: its digest is
/// computed from the stored copy and its reference is relative to this HOME.
pub(crate) fn attachment_ref(home: &Path, id: String, name: String, stored: &Path) -> Result<alondra_core::domain::AttachmentRef> {
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
    Ok(alondra_core::domain::AttachmentRef {
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
    ensure!(version.as_ref().is_none_or(|v| v == &current_version), "file changed; restart download");
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
    ensure!(file_version(&after) == current_version, "file changed during read");
    let next = offset + bytes.len() as u64;
    Ok((json!({"version":current_version,"data":URL_SAFE_NO_PAD.encode(bytes),
        "offset":offset,"total":metadata.len(),"next_offset":next,"eof":next==metadata.len()}), actual))
}

fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn file_version(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}-{}-{}-{}-{}-{}-{}", metadata.dev(), metadata.ino(), metadata.len(),
        metadata.mtime(), metadata.mtime_nsec(), metadata.ctime(), metadata.ctime_nsec())
}

pub(crate) fn open_stored_file(home: &Path, path: &Path) -> Result<std::fs::File> {
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

fn download(home: &Path, b: Download) -> Result<Value> {
    ensure!(b.length > 0 && b.length <= CHUNK, "invalid chunk length");
    let root = home.join("files").canonicalize()?;
    let path = b.path.canonicalize()?;
    let relative = path
        .strip_prefix(&root)
        .context("only Alondra stored copies may be read")?;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let root = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(root)?;
    let mut directory: OwnedFd = root.into();
    let components: Vec<_> = relative.components().collect();
    ensure!(!components.is_empty(), "not a regular file");
    for (index, component) in components.iter().enumerate() {
        let std::path::Component::Normal(name) = component else {
            anyhow::bail!("invalid stored path")
        };
        let name = std::ffi::CString::new(name.as_bytes())?;
        let flags = libc::O_RDONLY
            | libc::O_NOFOLLOW
            | libc::O_CLOEXEC
            | libc::O_NONBLOCK
            | if index + 1 < components.len() {
                libc::O_DIRECTORY
            } else {
                0
            };
        // Relative descriptor traversal refuses symlinks at every component,
        // including a directory swapped after canonicalization.
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        ensure!(fd >= 0, "stored file unavailable");
        directory = unsafe { OwnedFd::from_raw_fd(fd) };
    }
    let mut file = std::fs::File::from(directory);
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "not a regular file");
    let total = metadata.len();
    let version = format!(
        "{}-{}-{}-{}-{}-{}-{}",
        metadata.dev(),
        metadata.ino(),
        total,
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    );
    ensure!(
        b.offset == 0 || b.version.is_some(),
        "version required after first chunk"
    );
    ensure!(
        b.version.as_ref().is_none_or(|v| v == &version),
        "file changed; restart download"
    );
    ensure!(
        total <= crate::uploads::MAX_UPLOAD_BYTES as u64 && b.offset <= total,
        "invalid size or offset"
    );
    file.seek(SeekFrom::Start(b.offset))?;
    let mut data = vec![0; b.length.min((total - b.offset) as usize)];
    file.read_exact(&mut data)?;
    let after = file.metadata()?;
    ensure!(
        metadata.len() == after.len()
            && metadata.mtime() == after.mtime()
            && metadata.mtime_nsec() == after.mtime_nsec()
            && metadata.ctime() == after.ctime()
            && metadata.ctime_nsec() == after.ctime_nsec(),
        "file changed during read"
    );
    let next = b.offset + data.len() as u64;
    Ok(
        json!({"version":version,"data":URL_SAFE_NO_PAD.encode(data),"offset":b.offset,"total":total,"next_offset":next,"eof":next==total}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "01234567-89ab-cdef-0123-456789abcdef";
    fn home() -> crate::test_support::Directory {
        let path = crate::test_support::Directory::new();
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    fn start(t: &Transfers, h: &Path, bytes: &[u8]) -> Result<Value> {
        t.dispatch(h,"phone-1","/v1/service/uploads/start",json!({"transfer_id":ID,"name":"../../file.bin","bytes":bytes.len(),"sha256":Sha256::digest(bytes).iter().map(|b|format!("{b:02x}")).collect::<String>()}))
    }
    fn send(t: &Transfers, h: &Path, offset: usize, bytes: &[u8]) -> Result<Value> {
        t.dispatch(
            h,
            "phone-1",
            "/v1/service/uploads/chunk",
            json!({"transfer_id":ID,"offset":offset,"data":URL_SAFE_NO_PAD.encode(bytes)}),
        )
    }
    fn finish(t: &Transfers, h: &Path) -> Result<Value> {
        t.dispatch(
            h,
            "phone-1",
            "/v1/service/uploads/finish",
            json!({"transfer_id":ID}),
        )
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
        file.set_len(alondra_core::domain::MAX_ATTACHMENT_BYTES + 1).unwrap();
        let mut grown_hash = Sha256::new();
        grown_hash.update(&bytes);
        let zeros = [0u8; CHUNK];
        let mut remaining = alondra_core::domain::MAX_ATTACHMENT_BYTES as usize + 1 - bytes.len();
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

    #[test]
    fn failed_store_after_tombstone_remains_uncertain_across_retries_and_restart() {
        let h = home();
        let t = Transfers::default();
        start(&t, &h, b"abc").unwrap();
        send(&t, &h, 0, b"abc").unwrap();
        // Force store_identified's create_dir to fail after the ledger commit.
        let destination = h.join("files/uploads").join(upload_id("phone-1", ID));
        std::fs::create_dir_all(&destination).unwrap();
        let sentinel = destination.join("existing.bin");
        std::fs::write(&sentinel, b"unchanged").unwrap();
        for _ in 0..2 {
            assert_eq!(
                error_code(&finish(&t, &h).unwrap_err()),
                "attachment_transfer_finalization_uncertain"
            );
        }
        assert_eq!(
            error_code(&start(&t, &h, b"abc").unwrap_err()),
            "attachment_transfer_finalization_uncertain"
        );
        drop(t);
        let restarted = Transfers::default();
        assert_eq!(
            error_code(&finish(&restarted, &h).unwrap_err()),
            "attachment_transfer_finalization_uncertain"
        );
        assert_eq!(
            error_code(&start(&restarted, &h, b"abc").unwrap_err()),
            "attachment_transfer_finalization_uncertain"
        );
        assert_eq!(std::fs::read(&sentinel).unwrap(), b"unchanged");
        assert_eq!(
            std::fs::read_dir(h.join("files/uploads")).unwrap().count(),
            1
        );
        assert_eq!(std::fs::read_dir(destination).unwrap().count(), 1);
        std::fs::remove_dir_all(h).unwrap();
    }
    #[test]
    fn irreversible_upload_errors_release_buffers_and_admit_new_transfers() {
        let h = home();
        let transfers = Transfers::default();
        let bytes = vec![23; 1024 * 1024];
        let metadata = json!({"transfer_id":ID,"name":"file.bin","bytes":bytes.len(),
            "sha256":hex_digest(Sha256::digest(&bytes).as_slice())});
        let upload = |sender: &str| {
            let started = transfers.dispatch(&h, sender, "/uploads/start", metadata.clone()).unwrap();
            assert_eq!(started["offset"], 0, "new and repeated empty starts retain their offset");
            for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
                transfers.dispatch(&h, sender, "/uploads/chunk", json!({
                    "transfer_id":ID,"offset":index * CHUNK,"data":URL_SAFE_NO_PAD.encode(chunk),
                })).unwrap();
            }
        };
        for sender in ["phone-1", "phone-2"] {
            upload(sender);
            let destination = h.join("files/uploads").join(upload_id(sender, ID));
            std::fs::create_dir_all(&destination).unwrap();
            let sentinel = destination.join("existing.bin");
            std::fs::write(&sentinel, b"untouched").unwrap();
            for _ in 0..2 {
                let error = transfers.dispatch(&h, sender, "/uploads/finish", json!({"transfer_id":ID})).unwrap_err();
                assert_eq!(error_code(&error), "attachment_transfer_finalization_uncertain");
            }
            assert_eq!(std::fs::read(sentinel).unwrap(), b"untouched");
        }
        let (entries, retained_bytes) = {
            let map = transfers.0.lock().unwrap();
            (map.len(), map.values().map(|transfer| transfer.data.capacity()).sum::<usize>())
        };
        let fresh_upload = transfers.dispatch(&h, "phone-3", "/uploads/start", metadata.clone());
        println!("upload_error_retention entries={entries} retained_buffer_bytes={retained_bytes} attempted_uploads=2 uploaded_bytes={} fresh_upload_admitted={}", 2 * bytes.len(), fresh_upload.is_ok());
        assert_eq!(entries, 2, "uncertain attempts retain their retry guard");
        assert_eq!(retained_bytes, 0, "irreversible failures have no consumer for the upload buffer");
        assert_eq!(ledger(&h).unwrap().query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0)).unwrap(), 2);
        fresh_upload.unwrap();
        upload("phone-3");
        let completed = transfers.dispatch(&h, "phone-3", "/uploads/finish", json!({"transfer_id":ID})).unwrap();
        assert_eq!(std::fs::read(completed["path"].as_str().unwrap()).unwrap(), bytes);
        assert_eq!(transfers.dispatch(&h, "phone-3", "/uploads/finish", json!({"transfer_id":ID})).unwrap(), completed);
        assert_eq!(ledger(&h).unwrap().query_row("SELECT count(*) FROM attempts", [], |r| r.get::<_, i64>(0)).unwrap(), 3);
        drop(transfers);
        let restarted = Transfers::default();
        for sender in ["phone-1", "phone-2", "phone-3"] {
            let error = restarted.dispatch(&h, sender, "/uploads/start", metadata.clone()).unwrap_err();
            assert_eq!(error_code(&error), "attachment_transfer_finalization_uncertain");
        }
    }

    #[test]
    fn binary_roundtrip_retry_restart_and_download_version() {
        let h = home();
        let t = Transfers::default();
        let bytes: Vec<u8> = (0..CHUNK + 17).map(|i| i as u8).collect();
        start(&t, &h, &bytes).unwrap();
        send(&t, &h, 0, &bytes[..CHUNK]).unwrap();
        send(&t, &h, 0, &bytes[..CHUNK]).unwrap();
        assert!(send(&t, &h, 0, &[42]).is_err());
        assert!(finish(&t, &h).is_err());
        send(&t, &h, CHUNK, &bytes[CHUNK..]).unwrap();
        let (first_finish, second_finish) = std::thread::scope(|scope| {
            let a = scope.spawn(|| finish(&t, &h).unwrap());
            let b = scope.spawn(|| finish(&t, &h).unwrap());
            (a.join().unwrap(), b.join().unwrap())
        });
        let result = first_finish;
        assert_eq!(second_finish, result);
        assert_eq!(
            std::fs::read_dir(h.join("files/uploads")).unwrap().count(),
            1
        );
        assert_eq!(result["name"], "file.bin");
        let path = result["path"].as_str().unwrap();
        assert_eq!(std::fs::read(path).unwrap(), bytes);
        let first = t
            .dispatch(
                &h,
                "phone-1",
                "/v1/service/files/chunk",
                json!({"path":path,"offset":0,"length":CHUNK}),
            )
            .unwrap();
        assert!(serde_json::to_vec(&first).unwrap().len() < 180 * 1024);
        assert_eq!(
            URL_SAFE_NO_PAD
                .decode(first["data"].as_str().unwrap())
                .unwrap(),
            bytes[..CHUNK]
        );
        let last = t
            .dispatch(
                &h,
                "phone-1",
                "/v1/service/files/chunk",
                json!({"path":path,"offset":CHUNK,"length":CHUNK,"version":first["version"]}),
            )
            .unwrap();
        assert_eq!(last["eof"], true);
        std::fs::write(path, b"changed").unwrap();
        assert!(
            t.dispatch(
                &h,
                "phone-1",
                "/v1/service/files/chunk",
                json!({"path":path,"offset":0,"length":1,"version":first["version"]})
            )
            .is_err()
        );
        drop(t);
        let restarted = Transfers::default();
        assert!(finish(&restarted, &h).is_err());
        assert!(start(&restarted, &h, &bytes).is_err());
        std::fs::remove_dir_all(h).unwrap();
    }
    #[test]
    fn rejects_owner_offset_digest_capacity_and_traversal() {
        let h = home();
        let t = Transfers::default();
        start(&t, &h, b"abc").unwrap();
        assert!(
            t.dispatch(
                &h,
                "phone-2",
                "/v1/service/uploads/finish",
                json!({"transfer_id":ID})
            )
            .is_err()
        );
        assert!(send(&t, &h, usize::MAX, b"abc").is_err());
        assert!(send(&t, &h, 1, b"a").is_err());
        assert!(send(&t, &h, 0, &vec![0; CHUNK + 1]).is_err());
        send(&t, &h, 0, b"xyz").unwrap();
        assert!(finish(&t, &h).is_err());
        assert!(start(&t, &h, b"different").is_err());
        std::fs::create_dir_all(h.join("files")).unwrap();
        std::fs::write(h.join("secret"), b"secret").unwrap();
        std::os::unix::fs::symlink(h.join("secret"), h.join("files/link")).unwrap();
        assert!(
            t.dispatch(
                &h,
                "phone-1",
                "/v1/service/files/chunk",
                json!({"path":h.join("files/link"),"offset":0,"length":1})
            )
            .is_err()
        );
        t.dispatch(
            &h,
            "phone-1",
            "/v1/service/uploads/cancel",
            json!({"transfer_id":ID}),
        )
        .unwrap();
        start(&t, &h, b"abc").unwrap();
        // Only two active buffers, across all approved devices.
        let metadata = json!({"transfer_id":ID,"name":"second","bytes":3,"sha256":Sha256::digest(b"abc").iter().map(|b|format!("{b:02x}")).collect::<String>()});
        t.dispatch(&h, "phone-2", "/v1/service/uploads/start", metadata.clone())
            .unwrap();
        assert!(
            t.dispatch(&h, "phone-3", "/v1/service/uploads/start", metadata)
                .is_err()
        );
        let mut map = t.0.lock().unwrap();
        map.get_mut(&("phone-1".into(), ID.into())).unwrap().touched = Instant::now() - TTL;
        drop(map);
        assert!(finish(&t, &h).is_err());
        std::fs::remove_dir_all(h).unwrap();
    }
    #[test]
    fn full_twenty_mib_is_supported_in_bounded_chunks() {
        let h = home();
        let t = Transfers::default();
        let bytes = vec![19; crate::uploads::MAX_UPLOAD_BYTES];
        start(&t, &h, &bytes).unwrap();
        for (index, chunk) in bytes.chunks(CHUNK).enumerate() {
            send(&t, &h, index * CHUNK, chunk).unwrap();
        }
        let result = finish(&t, &h).unwrap();
        assert_eq!(result["bytes"], bytes.len());
        assert_eq!(
            std::fs::read(result["path"].as_str().unwrap()).unwrap(),
            bytes
        );
        std::fs::remove_dir_all(h).unwrap();
    }
}
