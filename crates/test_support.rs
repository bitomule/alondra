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
    pub fn new() -> Self {
        let owner = tempfile::Builder::new().prefix("at-").tempdir().expect("create test directory");
        let path = owner.path().canonicalize().expect("resolve test directory").join("work");
        Self { path, owner: Some(owner) }
    }
}

impl Deref for Directory {
    type Target = Path;
    fn deref(&self) -> &Path { &self.path }
}

impl AsRef<Path> for Directory {
    fn as_ref(&self) -> &Path { &self.path }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = self.owner.take().expect("test directory owner").close() {
            if std::thread::panicking() {
                eprintln!("Failed to remove test directory {}: {error}", self.path.display());
            } else {
                panic!("Failed to remove test directory {}: {error}", self.path.display());
            }
        }
    }
}


pub struct Owned<T> {
    value: T,
    _directory: Directory,
}
impl<T> Owned<T> {
    pub fn new(value: T, directory: Directory) -> Self { Self { value, _directory: directory } }
}
impl<T> Deref for Owned<T> {
    type Target = T;
    fn deref(&self) -> &T { &self.value }
}

impl AsRef<std::ffi::OsStr> for Directory {
    fn as_ref(&self) -> &std::ffi::OsStr { self.path.as_os_str() }
}
