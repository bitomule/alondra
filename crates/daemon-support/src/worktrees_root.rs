//! Worktree storage pinned to a volume. No directory is created before its identity is verified.
use std::path::{Path, PathBuf};
use alondra_core::options::WorktreesRoot;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use crate::locks::Poisonable as _;

pub const UNAVAILABLE: &str = "worktrees_unavailable";
// Recover: the lock holds no state, and every existing namespace is validated on disk.
static CLAIM: std::sync::Mutex<()> = std::sync::Mutex::new(());
const RECORD: &str = ".alondra-repository.json";

pub trait Volumes {
    fn identity(&self, path: &Path) -> Option<String>;
    fn mount(&self, identity: &str) -> bool;
}

pub struct System;
impl Volumes for System {
    fn identity(&self, path: &Path) -> Option<String> {
        #[cfg(target_os = "macos")]
        {
            use std::io::Write;
            use std::process::{Command, Stdio};
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let path = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
            let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
            if unsafe { libc::statfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 { return None; }
            let stats = unsafe { stats.assume_init() };
            let mount = unsafe { std::ffi::CStr::from_ptr(stats.f_mntonname.as_ptr()) };
            let mount = std::ffi::OsString::from_vec(mount.to_bytes().to_vec());
            let info = Command::new("/usr/sbin/diskutil").args(["info", "-plist"]).arg(mount).output().ok()?;
            if !info.status.success() { return None; }
            let mut convert = Command::new("/usr/bin/plutil").args(["-convert", "json", "-o", "-", "-"])
                .stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().ok()?;
            convert.stdin.take()?.write_all(&info.stdout).ok()?;
            let out = convert.wait_with_output().ok()?;
            if !out.status.success() { return None; }
            let info: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
            info.get("VolumeUUID")?.as_str().map(str::to_owned)
        }
        #[cfg(not(target_os = "macos"))]
        {
            use std::os::unix::fs::MetadataExt;
            Some(format!("dev:{}", std::fs::metadata(path).ok()?.dev()))
        }
    }
    fn mount(&self, identity: &str) -> bool {
        #[cfg(target_os = "macos")]
        { std::process::Command::new("/usr/sbin/diskutil").args(["mount", identity]).status().is_ok_and(|status| status.success()) }
        #[cfg(not(target_os = "macos"))]
        { let _ = identity; false }
    }
}

/// Returns the existing folder on the expected volume, without creating a missing mount point.
pub fn available(root: &WorktreesRoot, volumes: &impl Volumes) -> Result<PathBuf, String> {
    let matches = || root.path.is_dir().then_some(root.path.as_path()).filter(|path| volumes.identity(path).as_deref() == Some(root.volume_id.as_str()));
    let mut existing = matches();
    if existing.is_none() && !root.path.is_dir()
        && root.path.ancestors().skip(1).find(|path| path.is_dir())
            .is_some_and(|path| volumes.identity(path).as_deref() == Some(root.volume_id.as_str())) {
        return Err(format!("Worktree folder unavailable: {}", root.path.display()));
    }
    if existing.is_none() && volumes.mount(&root.volume_id) {
        existing = matches();
    }
    let existing = existing.ok_or_else(|| format!("Worktree folder unavailable: {}", root.path.display()))?;
    if root.path.ancestors().any(|path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())) {
        return Err("The worktree folder contains a symbolic link".into());
    }
    Ok(existing.to_path_buf())
}

pub fn capture(path: &Path) -> Result<WorktreesRoot, String> {
    let path = path.canonicalize().map_err(|why| format!("Choose an existing worktree folder: {why}"))?;
    if !path.is_dir() || path.parent().is_none() { return Err("Choose a worktree folder below the volume root".into()); }
    let volume_id = System.identity(&path).ok_or("Could not identify the worktree volume")?;
    Ok(WorktreesRoot { path, volume_id })
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub repository: PathBuf,
    pub root: WorktreesRoot,
}

/// Claims a repository folder; another checkout with the same name gets a short suffix.
pub fn directory(root: &WorktreesRoot, repository: &Path) -> Result<PathBuf, String> {
    directory_with(root, repository, || {})
}

fn directory_with(root: &WorktreesRoot, repository: &Path, before_create: impl FnOnce()) -> Result<PathBuf, String> {
    let _claim = CLAIM.lock_or_recover("worktree repository folders");
    let storage = Folder::storage(root)?;
    let repository = repository.canonicalize().map_err(|why| why.to_string())?;
    let name = repository.file_name().and_then(|name| name.to_str()).ok_or("The repository has no name")?;
    let hash: String = Sha256::digest(repository.as_os_str().as_encoded_bytes()).iter().take(4).map(|byte| format!("{byte:02x}")).collect();
    before_create();
    for name in [name.to_owned(), format!("{name}-{}", &hash[..8])] {
        let created = match storage.mkdir(&name) {
            Ok(()) => true,
            Err(why) if why.kind() == std::io::ErrorKind::AlreadyExists => false,
            Err(why) => return Err(why.to_string()),
        };
        let folder = match storage.child(&name) {
            Ok(folder) => folder,
            Err(_) if !created => continue,
            Err(why) => return Err(why.to_string()),
        };
        let record = Repository { repository: repository.clone(), root: root.clone() };
        if created { folder.write_record(&record)?; }
        if created || folder.read_record().as_ref() == Some(&record) {
            storage.check()?;
            folder.check()?;
            return Ok(folder.path);
        }
    }
    Err("The worktree repository folder is occupied".into())
}

fn registered(folder: &Path) -> Option<Repository> {
    if std::fs::symlink_metadata(folder).ok()?.file_type().is_symlink() { return None; }
    serde_json::from_slice(&std::fs::read(folder.join(RECORD)).ok()?).ok()
}

/// Storage metadata retained by the daemon even while this volume is absent.
pub fn stored(worktree: &Path) -> Option<Repository> {
    let folder = worktree.parent()?;
    let record = registered(folder)?;
    (folder.parent()? == record.root.path).then_some(record)
}

pub async fn ready(root: &WorktreesRoot) -> Result<PathBuf, String> {
    let root = root.clone();
    tokio::task::spawn_blocking(move || available(&root, &System)).await.map_err(|why| why.to_string())?
}

pub fn repository(worktree: &Path) -> Option<PathBuf> {
    stored(worktree).map(|record| record.repository)
}

pub async fn ready_recorded(worktree: &Path) -> Result<(), String> {
    let Some(record) = stored(worktree) else { return Ok(()) };
    ready(&record.root).await.map(drop)
}

#[derive(Debug)]
pub(crate) struct Folder {
    handle: std::fs::File,
    path: PathBuf,
}

impl Folder {
    fn storage(root: &WorktreesRoot) -> Result<Self, String> {
        use std::os::unix::fs::OpenOptionsExt;
        available(root, &System)?;
        let handle = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&root.path).map_err(|why| why.to_string())?;
        if opened_identity(&handle).as_deref() != Some(root.volume_id.as_str()) {
            return Err("The worktree storage volume changed".into());
        }
        let folder = Self { handle, path: root.path.clone() };
        folder.check()?;
        Ok(folder)
    }

    fn relative_file(&self, name: &str, flags: libc::c_int) -> std::io::Result<std::fs::File> {
        use std::os::fd::FromRawFd;
        let name = std::ffi::CString::new(name)?;
        let fd = unsafe { libc::openat(self.descriptor(), name.as_ptr(), flags | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o666) };
        if fd < 0 { return Err(std::io::Error::last_os_error()); }
        Ok(unsafe { std::fs::File::from_raw_fd(fd) })
    }

    fn mkdir(&self, name: &str) -> std::io::Result<()> {
        let name = std::ffi::CString::new(name)?;
        if unsafe { libc::mkdirat(self.descriptor(), name.as_ptr(), 0o777) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn child(&self, name: &str) -> std::io::Result<Self> {
        use std::os::unix::fs::MetadataExt;
        let handle = self.relative_file(name, libc::O_RDONLY | libc::O_DIRECTORY)?;
        if handle.metadata()?.dev() != self.handle.metadata()?.dev() {
            return Err(std::io::Error::other("The repository folder is on another volume"));
        }
        Ok(Self { handle, path: self.path.join(name) })
    }

    fn read_record(&self) -> Option<Repository> {
        serde_json::from_reader(self.relative_file(RECORD, libc::O_RDONLY).ok()?).ok()
    }

    fn write_record(&self, record: &Repository) -> Result<(), String> {
        use std::io::Write;
        let mut file = self.relative_file(RECORD, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL).map_err(|why| why.to_string())?;
        file.write_all(&serde_json::to_vec(record).map_err(|why| why.to_string())?).map_err(|why| why.to_string())
    }

    pub(crate) fn descriptor(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd;
        self.handle.as_raw_fd()
    }

    pub(crate) fn check(&self) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt;
        let opened = self.handle.metadata().map_err(|why| why.to_string())?;
        let current = std::fs::symlink_metadata(&self.path).map_err(|why| why.to_string())?;
        if current.is_dir() && (opened.dev(), opened.ino()) == (current.dev(), current.ino()) {
            Ok(())
        } else {
            Err("The worktree repository folder changed during creation".into())
        }
    }
}

fn opened_identity(file: &std::fs::File) -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        use std::os::{fd::AsRawFd, unix::ffi::OsStringExt};
        let mut stats = std::mem::MaybeUninit::<libc::statfs>::uninit();
        if unsafe { libc::fstatfs(file.as_raw_fd(), stats.as_mut_ptr()) } != 0 { return None; }
        let stats = unsafe { stats.assume_init() };
        let mount = unsafe { std::ffi::CStr::from_ptr(stats.f_mntonname.as_ptr()) };
        System.identity(Path::new(&std::ffi::OsString::from_vec(mount.to_bytes().to_vec())))
    }
    #[cfg(not(target_os = "macos"))]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!("dev:{}", file.metadata().ok()?.dev()))
    }
}

/// Pins the validated directory while Git writes relative to it, without recreating any parent.
pub(crate) async fn hold(worktree: &Path, record: &Repository) -> Result<Folder, String> {
    let path = worktree.parent().ok_or("The worktree has no parent")?.to_path_buf();
    let record = record.clone();
    tokio::task::spawn_blocking(move || {
        use std::os::unix::fs::OpenOptionsExt;
        available(&record.root, &System)?;
        let handle = std::fs::OpenOptions::new().read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW).open(&path).map_err(|why| why.to_string())?;
        if opened_identity(&handle).as_deref() != Some(record.root.volume_id.as_str())
            || registered(&path).as_ref() != Some(&record) {
            return Err("The worktree repository folder is unavailable".into());
        }
        let folder = Folder { handle, path };
        folder.check()?;
        Ok(folder)
    }).await.map_err(|why| why.to_string())?
}

/// Restores ownership metadata from the daemon's durable record after the volume was checked.
pub fn restore(worktree: &Path, record: &Repository) -> Result<(), String> {
    restore_with(worktree, record, || {})
}

fn restore_with(worktree: &Path, record: &Repository, before_write: impl FnOnce()) -> Result<(), String> {
    let folder = worktree.parent().ok_or("The worktree has no parent")?;
    if folder.parent() != Some(record.root.path.as_path()) {
        return Err("The recorded worktree is outside its storage root".into());
    }
    let _claim = CLAIM.lock_or_recover("worktree repository folders");
    let storage = Folder::storage(&record.root)?;
    let name = folder.file_name().and_then(|name| name.to_str()).ok_or("The repository folder has no name")?;
    let folder = match storage.child(name) {
        Ok(folder) => folder,
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => {
            storage.mkdir(name).map_err(|why| why.to_string())?;
            storage.child(name).map_err(|why| why.to_string())?
        },
        Err(why) => return Err(why.to_string()),
    };
    if let Some(existing) = folder.read_record() {
        storage.check()?;
        folder.check()?;
        return if existing == *record { Ok(()) } else { Err("The worktree folder belongs to another repository".into()) };
    }
    before_write();
    folder.write_record(record)?;
    storage.check()?;
    folder.check()

}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn worktrees_root_replaced_during_namespace_creation_receives_no_writes() {
        let fixture = crate::test_support::Directory::new();
        let path = fixture.join("storage");
        let repo = fixture.join("checkout");
        std::fs::create_dir_all(&path).unwrap();
        std::fs::create_dir(&repo).unwrap();
        let root = capture(&path).unwrap();
        let result = directory_with(&root, &repo, || {
            std::fs::rename(&path, fixture.join("original-storage")).unwrap();
            std::fs::create_dir(&path).unwrap();
        });
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0, "the replacement storage must receive no namespace or ownership record");
        assert!(result.is_err());
    }

    #[test]
    fn worktrees_root_replaced_during_record_restoration_receives_no_writes() {
        let fixture = crate::test_support::Directory::new();
        std::fs::create_dir_all(&fixture).unwrap();
        let root = capture(&fixture).unwrap();
        let record = Repository { repository: fixture.join("checkout"), root };
        let path = fixture.join("repo");
        std::fs::create_dir(&path).unwrap();
        let result = restore_with(&path.join("task"), &record, || {
            std::fs::rename(&path, fixture.join("original-repo")).unwrap();
            std::fs::create_dir(&path).unwrap();
        });
        assert_eq!(std::fs::read_dir(&path).unwrap().count(), 0, "the replacement namespace must receive no ownership record");
        assert!(result.is_err());
    }

    struct Fake {
        mounted: Cell<bool>,
        succeeds: bool,
        calls: Cell<usize>,
    }
    impl Volumes for Fake {
        fn identity(&self, _: &Path) -> Option<String> {
            Some(if self.mounted.get() { "expected" } else { "system" }.into())
        }
        fn mount(&self, id: &str) -> bool {
            assert_eq!(id, "expected");
            self.calls.set(self.calls.get() + 1);
            self.mounted.set(self.succeeds);
            self.succeeds
        }
    }

    #[test]
    fn worktrees_root_mounts_and_rechecks_identity_without_creating_a_system_disk_folder() {
        let fixture = crate::test_support::Directory::new();
        std::fs::create_dir_all(&fixture).unwrap();
        let root = WorktreesRoot { path: fixture.join("missing/tasks"), volume_id: "expected".into() };
        let fake = Fake { mounted: Cell::new(false), succeeds: false, calls: Cell::new(0) };
        assert!(available(&root, &fake).is_err());
        assert_eq!(fake.calls.get(), 1);
        assert!(!root.path.exists());
        std::fs::create_dir_all(&root.path).unwrap();
        let fake = Fake { mounted: Cell::new(false), succeeds: true, calls: Cell::new(0) };
        assert_eq!(available(&root, &fake).unwrap(), root.path);
        assert_eq!(fake.calls.get(), 1);
        available(&root, &fake).unwrap();
        assert_eq!(fake.calls.get(), 1, "a mounted volume is not mounted again");
    }

    #[test]
    fn worktrees_root_restore_never_writes_through_a_replaced_repository_folder() {
        let fixture = crate::test_support::Directory::new();
        std::fs::create_dir_all(&fixture).unwrap();
        let root = capture(&fixture).unwrap();
        let foreign = fixture.join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        std::os::unix::fs::symlink(&foreign, fixture.join("repo")).unwrap();
        let record = Repository { repository: fixture.join("checkout"), root };
        assert!(restore(&fixture.join("repo/task"), &record).is_err());
        assert_eq!(std::fs::read_dir(&foreign).unwrap().count(), 0);
    }

    #[test]
    fn worktrees_root_missing_folder_on_the_expected_volume_is_never_created() {
        let fixture = crate::test_support::Directory::new();
        std::fs::create_dir_all(&fixture).unwrap();
        let mut root = capture(&fixture).unwrap();
        root.path = fixture.join("missing/tasks");
        assert!(directory(&root, &fixture).is_err());
        assert!(!fixture.join("missing").exists());
    }

    #[test]
    fn worktrees_root_never_trusts_a_successful_mount_without_the_expected_identity() {
        struct Wrong;
        impl Volumes for Wrong {
            fn identity(&self, _: &Path) -> Option<String> { Some("wrong".into()) }
            fn mount(&self, _: &str) -> bool { true }
        }
        let root = WorktreesRoot { path: "/not-a-worktree-volume/tasks".into(), volume_id: "expected".into() };
        assert!(available(&root, &Wrong).is_err());
        assert!(!root.path.exists());
    }
}
