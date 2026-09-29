//! Bounded, device-owned attachment transfers inside the encrypted service channel.
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
        let route = match route.strip_prefix("/v2/domain/uploads/") {
            Some(step) => {
                renamed = format!("/v1/service/uploads/{step}");
                renamed.as_str()
            }
            None => route,
        };
        if route == "/v1/service/files/chunk" {
            return download(home, serde_json::from_value(body)?);
        }
        let mut transfers = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("transfer lock poisoned"))?;
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
                    && transfers.values().filter(|t| t.result.is_none()).count() < SLOTS,
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
        transaction.commit().context(UNCERTAIN)?;
        let (id, a) =
            crate::uploads::store_identified(home, &t.name, &t.data, &upload_id(sender, &key.1))
                .context(UNCERTAIN)?;
        let result = json!({"id":id,"name":a.name,"bytes":a.bytes,"kind":if crate::uploads::is_image(&a.name) {"image"} else {"file"},"path":a.stored_path});
        t.result = Some(result.clone());
        t.data = Vec::new();
        t.touched = Instant::now();
        Ok(result)
    }
}
impl Transfers {
    /// A step of an upload as the v2 routes name it. What `finish` answers is the attachment
    /// a v2 command carries.
    pub fn dispatch_v2(&self, home: &Path, sender: &str, route: &str, body: Value) -> Result<Value> {
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
pub(crate) fn read_verified_domain_chunk(home: &Path, reference: &str, digest: &str,
    offset: u64, version: Option<String>) -> Result<Value> {
    read_chunk(home, reference, Some(digest), offset, version).map(|(chunk, _)| chunk)
}

/// A chunk of a stored copy with the digest of the whole file, which the daemon computes.
pub(crate) fn read_stored_chunk(home: &Path, reference: &str, offset: u64, version: Option<String>) -> Result<Value> {
    let (mut chunk, digest) = read_chunk(home, reference, None, offset, version)?;
    chunk["content_digest"] = json!(digest);
    Ok(chunk)
}

/// The attachment a v2 command carries, for a file the daemon stored: its digest is
/// computed from the stored copy and its reference is relative to this HOME.
pub(crate) fn attachment_ref(home: &Path, id: String, name: String, stored: &Path) -> Result<alondra_core::domain_v2::AttachmentRef> {
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
    Ok(alondra_core::domain_v2::AttachmentRef {
        id,
        name,
        content_digest: hex_digest(hasher.finalize().as_slice()),
        content_reference: reference.to_string_lossy().into_owned(),
        bytes: total,
    })
}

fn read_chunk(home: &Path, reference: &str, expected: Option<&str>,
    offset: u64, version: Option<String>) -> Result<(Value, String)> {
    let path = Path::new(reference);
    let path = if path.is_absolute() { path.to_path_buf() } else { home.join(path) };
    let mut file = open_stored_file(home, &path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file() && metadata.len() <= crate::uploads::MAX_UPLOAD_BYTES as u64,
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
        ensure!(position <= crate::uploads::MAX_UPLOAD_BYTES as u64, "stored document too large");
    }
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
    fn home() -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "alondra-transfer-{}-{nonce}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }
    use std::time::SystemTime;
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
