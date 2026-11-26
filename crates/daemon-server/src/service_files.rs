//! Bounded, device-owned attachment transfers inside the encrypted service channel.
use alondra_core::clock;
use alondra_core::clock::Instant;
use alondra_engine_ports::stored_files::{CHUNK, TransferError, attachment_ref};
use alondra_foundation::locks::Poisonable as _;
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
    time::{Duration},
};

const TTL: Duration = Duration::from_secs(600);
const SLOTS: usize = 2;

const UNCERTAIN: TransferError = TransferError::FinalizationUncertain;
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
        transfers.retain(|_, t| alondra_core::clock::since(t.touched) < TTL);
        if route == "/v1/service/uploads/start" {
            let b: Start = serde_json::from_value(body)?;
            ensure!(valid_id(&b.transfer_id), "invalid transfer ID");
            ensure!(
                b.bytes > 0 && b.bytes <= alondra_engine_ports::uploads::MAX_UPLOAD_BYTES,
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
                t.touched = clock::instant();
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
            ensure!(!seen, UNCERTAIN);
            ensure!(
                transfers.len() < 32
                    && transfers.values().filter(|t| !t.finalization_attempted).count() < SLOTS,
                TransferError::Capacity
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
                    touched: clock::instant(),
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
            .context(TransferError::Unknown)?;
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
            t.touched = clock::instant();
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
        ensure!(count < 10000, TransferError::Capacity);
        transaction.execute(
            "INSERT INTO attempts(sender,transfer_id) VALUES(?1,?2)",
            rusqlite::params![sender, &key.1],
        )?;
        t.finalization_attempted = true;
        let data = std::mem::take(&mut t.data);
        transaction.commit().context(UNCERTAIN)?;
        let (id, a) =
            alondra_engine_ports::uploads::store_identified(home, &t.name, &data, &upload_id(sender, &key.1))
                .context(UNCERTAIN)?;
        let result = json!({"id":id,"name":a.name,"bytes":a.bytes,"kind":if alondra_engine_ports::uploads::is_image(&a.name) {"image"} else {"file"},"path":a.stored_path});
        t.result = Some(result.clone());
        t.touched = clock::instant();
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
    match error.downcast_ref::<TransferError>() {
        Some(TransferError::Unknown) => "attachment_transfer_unknown",
        Some(TransferError::FinalizationUncertain) => "attachment_transfer_finalization_uncertain",
        Some(TransferError::Capacity) => "attachment_transfer_capacity",
        Some(TransferError::FileChanged) => "attachment_file_changed",
        None => "attachment_transfer_rejected",
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
        TransferError::FileChanged
    );
    ensure!(
        total <= alondra_engine_ports::uploads::MAX_UPLOAD_BYTES as u64 && b.offset <= total,
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
        TransferError::FileChanged
    );
    let next = b.offset + data.len() as u64;
    Ok(
        json!({"version":version,"data":URL_SAFE_NO_PAD.encode(data),"offset":b.offset,"total":total,"next_offset":next,"eof":next==total}),
    )
}

#[cfg(test)]
#[cfg(not(feature = "real-hosts"))]
mod tests {
    use super::*;
    use alondra_engine_ports::stored_files::hex_digest;
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
    fn transfer_codes_survive_context_added_to_the_error() {
        let h = home();
        let t = Transfers::default();
        let missing = send(&t, &h, 0, b"abc").unwrap_err();
        assert_eq!(error_code(&missing), "attachment_transfer_unknown");
        assert_eq!(error_code(&missing.context("upload")), "attachment_transfer_unknown");
        let invalid = t.dispatch(&h, "phone-1", "/v1/service/uploads/chunk", json!({"transfer_id":"../x"})).unwrap_err();
        assert_eq!(error_code(&invalid.context("upload")), "attachment_transfer_rejected");
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
