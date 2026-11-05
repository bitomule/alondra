#![allow(dead_code)]

use std::ops::Deref;
use std::path::{Path, PathBuf};

/// Owns the entire temporary tree. The exposed child starts absent so fixtures can
/// exercise creation and missing-HOME behavior without detaching cleanup ownership.
#[derive(Debug)]
pub struct Directory {
    path: PathBuf,
    owner: Option<tempfile::TempDir>,
}

impl Directory {
    /// The spawning test owns cleanup while child processes retain this guard.
    pub fn for_process_fixture(path: &Path, token: &str) -> std::io::Result<Self> {
        use std::io::{Error, ErrorKind};
        let invalid = || Error::new(ErrorKind::InvalidInput, "invalid process fixture directory");
        if !path.is_absolute()
            || token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(invalid());
        }
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(invalid());
        }
        let canonical = path.canonicalize()?;
        let marker = canonical.join(".alondra-process-fixture");
        let metadata = std::fs::symlink_metadata(&marker)?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || std::fs::read(marker)? != format!("alondra-process-fixture:{token}\n").as_bytes()
        {
            return Err(invalid());
        }
        Ok(Self {
            path: canonical,
            owner: None,
        })
    }

    pub fn new() -> Self {
        let owner = tempfile::Builder::new()
            .prefix("at-")
            .tempdir()
            .expect("create test directory");
        let path = owner
            .path()
            .canonicalize()
            .expect("resolve test directory")
            .join("work");
        Self {
            path,
            owner: Some(owner),
        }
    }
}

impl Deref for Directory {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for Directory {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let Some(owner) = self.owner.take() else {
            return;
        };
        make_folders_writable(owner.path());
        if let Err(error) = owner.close() {
            if error.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            if std::thread::panicking() {
                eprintln!(
                    "Failed to remove test directory {}: {error}",
                    self.path.display()
                );
            } else {
                panic!(
                    "Failed to remove test directory {}: {error}",
                    self.path.display()
                );
            }
        }
    }
}

/// Tests leave read-only folders behind (snapshots, fake caches); their files cannot be
/// unlinked until the folder holding them is writable again.
fn make_folders_writable(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(metadata) = std::fs::symlink_metadata(path) else {
            return;
        };
        if !metadata.is_dir() {
            return;
        }
        let mode = metadata.permissions().mode();
        if mode & 0o700 != 0o700 {
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o700));
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            make_folders_writable(&entry.path());
        }
    }
}

pub struct Owned<T> {
    value: T,
    _directory: Directory,
}
impl<T> Owned<T> {
    pub fn new(value: T, directory: Directory) -> Self {
        Self {
            value,
            _directory: directory,
        }
    }
}
impl<T> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.value
    }
}

impl AsRef<std::ffi::OsStr> for Directory {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.path.as_os_str()
    }
}

/// The longest temporary folder that leaves room for a socket deep inside a fixture.
#[cfg(unix)]
const SOCKET_ROOT_LIMIT: usize = 32;

#[cfg(unix)]
pub fn socket_root(temporary: &Path) -> PathBuf {
    match temporary.canonicalize() {
        Ok(root) if root.as_os_str().len() <= SOCKET_ROOT_LIMIT => root,
        _ => std::fs::canonicalize("/tmp").expect("resolve short fixture root"),
    }
}

#[cfg(unix)]
impl Directory {
    /// Keeps macOS socket names within sockaddr_un limits while retaining fixture cleanup. Under the
    /// test's own TMPDIR when that is short: whatever removes it, also when the test was cut, ends
    /// the hosts running here, which end themselves once their lock is gone.
    pub fn for_unix_socket() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let root = socket_root(Path::new(&std::env::var_os("TMPDIR").unwrap_or_default()));
        let owner = tempfile::Builder::new()
            .prefix("as-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(root)
            .expect("create private socket fixture");
        let path = owner
            .path()
            .canonicalize()
            .expect("resolve socket fixture")
            .join("work");
        Self {
            path,
            owner: Some(owner),
        }
    }
}
