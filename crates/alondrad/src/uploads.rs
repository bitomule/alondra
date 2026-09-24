//! Files the user attaches from a client. Each one is copied into Alondra's own storage the
//! moment it arrives, under `files/uploads/<id>/<name>`, so the original can be deleted right
//! after attaching. A node receives the copy's absolute path and opens it with its Read tool,
//! which shows it images and PDFs as well as text.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use alondra_core::Attachment;
use sha2::{Digest, Sha256};

pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;

/// A file uploaded and never sent (a draft given up on) is dropped after this long.
pub const UNSENT_GRACE: Duration = Duration::from_secs(24 * 3600);

const ID_LEN: usize = 16;

/// Formats Claude reads as images. Others that are still images (HEIC from an iPhone, TIFF,
/// BMP) are converted to JPEG on arrival so the node can see them.
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];
const CONVERTIBLE_EXTENSIONS: [&str; 5] = ["heic", "heif", "tif", "tiff", "bmp"];

pub fn uploads_dir(home: &Path) -> PathBuf {
    home.join("files").join("uploads")
}

/// Messages too long to type into a node's terminal, each stored whole in its own folder.
pub fn messages_dir(home: &Path) -> PathBuf {
    home.join("files").join("messages")
}

fn extension(name: &str) -> String {
    Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default()
}

pub fn is_image(name: &str) -> bool {
    IMAGE_EXTENSIONS.contains(&extension(name).as_str())
}

/// The name a client sent, reduced to a plain file name that cannot leave its directory.
pub fn clean_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).collect::<String>().trim().trim_start_matches('.').to_string();
    let cleaned: String = cleaned.chars().take(120).collect();
    if cleaned.is_empty() { "attachment".to_string() } else { cleaned }
}

fn new_id(bytes: &[u8]) -> String {
    let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    let mut h = Sha256::new();
    h.update(nanos.to_le_bytes());
    h.update(std::process::id().to_le_bytes());
    h.update(bytes);
    h.finalize().iter().take(ID_LEN / 2).map(|b| format!("{b:02x}")).collect()
}

fn valid_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The id is the name of the upload's directory; the attachment carries the stored copy's path.
pub fn store(home: &Path, raw_name: &str, bytes: &[u8]) -> anyhow::Result<(String, Attachment)> {
    anyhow::ensure!(!bytes.is_empty(), "the file is empty");
    anyhow::ensure!(bytes.len() <= MAX_UPLOAD_BYTES, "the file is over {} MB", MAX_UPLOAD_BYTES / (1024 * 1024));
    let name = clean_name(raw_name);
    let id = new_id(bytes);
    let dir = uploads_dir(home).join(&id);
    std::fs::create_dir_all(&dir)?;
    let dest = dir.join(&name);
    std::fs::write(&dest, bytes)?;
    let dest = if CONVERTIBLE_EXTENSIONS.contains(&extension(&name).as_str()) { to_jpeg(&dest).unwrap_or(dest) } else { dest };
    Ok((id.clone(), attachment_at(&dest, raw_name)?))
}

fn attachment_at(path: &Path, source: &str) -> anyhow::Result<Attachment> {
    Ok(Attachment {
        name: path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
        source_path: source.to_string(),
        stored_path: Some(path.to_string_lossy().to_string()),
        bytes: Some(std::fs::metadata(path)?.len()),
    })
}

/// Replaces the stored file with a JPEG next to it; `None` leaves the original in place.
fn to_jpeg(path: &Path) -> Option<PathBuf> {
    let out = path.with_extension("jpg");
    let ok = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg"])
        .arg(path)
        .arg("--out")
        .arg(&out)
        .output()
        .is_ok_and(|o| o.status.success() && out.exists());
    if !ok {
        return None;
    }
    let _ = std::fs::remove_file(path);
    Some(out)
}

/// The uploads a send refers to by id, in order. An id that is not there (never uploaded, or
/// swept) refuses the whole send: the user must know the node would not get it.
pub fn resolve(home: &Path, ids: &[String]) -> Result<Vec<Attachment>, String> {
    ids.iter()
        .map(|id| {
            let missing = || format!("attachment {id} is not in Alondra's storage: attach it again");
            if !valid_id(id) {
                return Err(missing());
            }
            let file = std::fs::read_dir(uploads_dir(home).join(id))
                .map_err(|_| missing())?
                .filter_map(Result::ok)
                .map(|e| e.path())
                .find(|p| p.is_file())
                .ok_or_else(missing)?;
            attachment_at(&file, id).map_err(|_| missing())
        })
        .collect()
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b < 1024 => format!("{b} B"),
        b if b < 1024 * 1024 => format!("{} KB", b.div_ceil(1024)),
        b => format!("{:.1} MB", b as f64 / (1024.0 * 1024.0)),
    }
}

/// What the node reads after the user's text: every attachment as a path it can open, in
/// backticks because file names carry spaces.
pub fn delivery_note(user: &str, attachments: &[Attachment]) -> String {
    let files: Vec<String> = attachments
        .iter()
        .filter_map(|a| {
            let path = a.stored_path.as_ref()?;
            let kind = if is_image(&a.name) { "image" } else { "file" };
            Some(format!("`{path}` ({kind}, {})", human_size(a.bytes.unwrap_or(0))))
        })
        .collect();
    if files.is_empty() {
        return String::new();
    }
    format!("\n\n{user} attached {} (open with the Read tool): {}", if files.len() == 1 { "a file" } else { "files" }, files.join("; "))
}

/// Removes attachment directories nothing refers to any more: an item's files stay while the
/// item is kept, and go once retention drops it. Directories younger than `grace` are kept
/// even unreferenced, which covers an upload not sent yet and a node's copy made just before
/// its question or report is recorded.
pub fn sweep(home: &Path, referenced: &HashSet<PathBuf>, grace: Duration) -> usize {
    let files = home.join("files");
    let now = SystemTime::now();
    let old = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| now.duration_since(t).unwrap_or_default() > grace);
    let holds_a_reference = |dir: &Path| referenced.iter().any(|r| r.starts_with(dir));
    let node_dirs = std::fs::read_dir(&files).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).filter(|p| {
        p.file_name().is_some_and(|n| {
            let n = n.to_string_lossy();
            n.starts_with("q-") || n.starts_with("r-")
        })
    });
    let uploads = std::fs::read_dir(uploads_dir(home)).into_iter().flatten().filter_map(Result::ok).map(|e| e.path());
    let messages = std::fs::read_dir(messages_dir(home)).into_iter().flatten().filter_map(Result::ok).map(|e| e.path());
    let mut removed = 0;
    for dir in node_dirs.chain(uploads).chain(messages).filter(|p| p.is_dir()) {
        if !holds_a_reference(&dir) && old(&dir) && std::fs::remove_dir_all(&dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("alondra-uploads-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_stored_upload_outlives_the_original_file() {
        let home = home("outlives");
        let original = home.join("captura de pantalla.png");
        std::fs::write(&original, b"\x89PNG not really").unwrap();
        let (id, stored) = store(&home, &original.to_string_lossy(), &std::fs::read(&original).unwrap()).unwrap();
        std::fs::remove_file(&original).unwrap();
        let resolved = resolve(&home, std::slice::from_ref(&id)).unwrap();
        assert_eq!(resolved[0].stored_path, stored.stored_path);
        assert_eq!(resolved[0].name, "captura de pantalla.png");
        assert_eq!(std::fs::read(resolved[0].stored_path.as_ref().unwrap()).unwrap(), b"\x89PNG not really");
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn names_cannot_escape_their_directory() {
        assert_eq!(clean_name("../../etc/passwd"), "passwd");
        assert_eq!(clean_name("..\\..\\x.txt"), "x.txt");
        assert_eq!(clean_name(".."), "attachment");
        assert_eq!(clean_name(".hidden"), "hidden");
        assert_eq!(clean_name(""), "attachment");
    }

    #[test]
    fn an_unknown_or_malformed_id_refuses_the_send() {
        let home = home("unknown");
        assert!(resolve(&home, &["0123456789abcdef".into()]).is_err());
        assert!(resolve(&home, &["../../secrets".into()]).is_err());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn empty_and_oversized_files_are_refused() {
        let home = home("limits");
        assert!(store(&home, "a.txt", b"").is_err());
        assert!(store(&home, "a.bin", &vec![0u8; MAX_UPLOAD_BYTES + 1]).is_err());
        assert!(store(&home, "a.bin", &vec![0u8; MAX_UPLOAD_BYTES]).is_ok());
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_node_reads_each_path_with_its_kind_and_size() {
        let a = |name: &str, bytes| Attachment { name: name.into(), source_path: String::new(), stored_path: Some(format!("/h/files/uploads/x/{name}")), bytes: Some(bytes) };
        assert_eq!(delivery_note("David", &[]), "");
        assert_eq!(
            delivery_note("David", &[a("foto.jpg", 1_300_000), a("notas.txt", 900)]),
            "\n\nDavid attached files (open with the Read tool): `/h/files/uploads/x/foto.jpg` (image, 1.2 MB); `/h/files/uploads/x/notas.txt` (file, 900 B)"
        );
    }

    #[test]
    fn an_iphone_photo_arrives_as_a_jpeg_the_node_can_see() {
        let home = home("heic");
        let sample = std::process::Command::new("/usr/bin/sips")
            .args(["-s", "format", "heic", "-z", "8", "8", "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns", "--out"])
            .arg(home.join("IMG_0001.HEIC"))
            .output()
            .is_ok_and(|o| o.status.success());
        if !sample {
            std::fs::remove_dir_all(home).unwrap();
            return;
        }
        let (_, a) = store(&home, "IMG_0001.HEIC", &std::fs::read(home.join("IMG_0001.HEIC")).unwrap()).unwrap();
        assert_eq!(a.name, "IMG_0001.jpg");
        assert!(is_image(&a.name));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn retention_removes_only_old_files_nothing_refers_to() {
        let home = home("sweep");
        let (_, kept) = store(&home, "kept.png", b"k").unwrap();
        let (_, dropped) = store(&home, "dropped.png", b"d").unwrap();
        let node_copy = home.join("files").join("q-1").join("plan.md");
        std::fs::create_dir_all(node_copy.parent().unwrap()).unwrap();
        std::fs::write(&node_copy, b"p").unwrap();
        let referenced: HashSet<PathBuf> = [PathBuf::from(kept.stored_path.clone().unwrap())].into();
        assert_eq!(sweep(&home, &referenced, Duration::from_secs(3600)), 0, "young files are kept");
        assert_eq!(sweep(&home, &referenced, Duration::ZERO), 2);
        assert!(Path::new(kept.stored_path.as_ref().unwrap()).exists());
        assert!(!Path::new(dropped.stored_path.as_ref().unwrap()).exists());
        assert!(!node_copy.exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}
