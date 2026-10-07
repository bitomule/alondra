use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

pub const RESTORE_SCRIPT: &str = include_str!("../restore.sh");

/// Held while a file and its manifest line change, so a signal never restores halfway.
pub static WRITING: Mutex<()> = Mutex::new(());

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn file_digest(path: &Path) -> Result<String> {
    Ok(match std::fs::read(path) {
        Ok(bytes) => digest(&bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => "-".into(),
        Err(err) => return Err(err.into()),
    })
}

/// Replaces files of the working tree. Before touching one it saves it under `guard` and
/// records, in the manifest, its original digest and the digest about to be written; the
/// restore script copied into `guard` puts the branch back from there, even with no build.
pub struct Scratch {
    root: PathBuf,
    guard: PathBuf,
    entries: BTreeMap<String, (String, Vec<String>)>,
}

impl Scratch {
    pub fn new(root: &Path, guard: &Path) -> Result<Self> {
        if guard.join("manifest").exists() {
            bail!("{} holds an earlier run's backups; restore them first", guard.display());
        }
        std::fs::create_dir_all(guard.join("files"))?;
        std::fs::write(guard.join("restore.sh"), RESTORE_SCRIPT)?;
        Ok(Self { root: root.into(), guard: guard.into(), entries: BTreeMap::new() })
    }

    fn save_manifest(&self) -> Result<()> {
        let mut text = String::new();
        for (path, (original, written)) in &self.entries {
            text.push_str(&format!("{original}\t{}\t{path}\n", written.join(",")));
        }
        let tmp = self.guard.join("manifest.tmp");
        std::fs::write(&tmp, text)?;
        std::fs::rename(tmp, self.guard.join("manifest"))?;
        Ok(())
    }

    pub fn write(&mut self, path: &str, content: Option<&str>) -> Result<()> {
        let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let full = self.root.join(path);
        if !self.entries.contains_key(path) {
            let original = file_digest(&full)?;
            if original != "-" {
                let copy = self.guard.join("files").join(path);
                std::fs::create_dir_all(copy.parent().context("path")?)?;
                std::fs::copy(&full, &copy)?;
            }
            self.entries.insert(path.to_string(), (original, Vec::new()));
        }
        let written = content.map_or_else(|| "-".to_string(), |c| digest(c.as_bytes()));
        self.entries.get_mut(path).context("entry")?.1.push(written);
        self.save_manifest()?;
        match content {
            Some(text) => {
                std::fs::create_dir_all(full.parent().context("path")?)?;
                std::fs::write(&full, text)?;
            }
            None => {
                if full.is_file() {
                    std::fs::remove_file(&full)?;
                }
            }
        }
        Ok(())
    }
}

/// Runs the restore script kept in `guard`. Fails, keeping the backups, when a file changed
/// after the run wrote it.
pub fn restore(root: &Path, guard: &Path) -> Result<()> {
    if !guard.join("manifest").exists() {
        if guard.exists() {
            std::fs::remove_dir_all(guard)?;
        }
        return Ok(());
    }
    let script = guard.join("restore.sh");
    if !script.exists() {
        std::fs::write(&script, RESTORE_SCRIPT)?;
    }
    let status = Command::new("bash").arg(&script).arg(guard).arg(root).status()?;
    if !status.success() {
        bail!(
            "files changed after an interrupted ablation run were kept; their branch versions are in {}",
            guard.display()
        );
    }
    Ok(())
}

/// One ablation run per worktree at a time; a lock left by a process that is gone is taken over.
pub struct Lock {
    path: PathBuf,
}

impl Lock {
    pub fn acquire(path: &Path) -> Result<Self> {
        for _ in 0..2 {
            match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
                Ok(mut file) => {
                    write!(file, "{}", std::process::id())?;
                    return Ok(Self { path: path.into() });
                }
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                    let pid: i32 = std::fs::read_to_string(path)?.trim().parse().unwrap_or(0);
                    if pid > 0 && unsafe { libc::kill(pid, 0) } == 0 {
                        bail!("another ablation run (process {pid}) is using this worktree");
                    }
                    std::fs::remove_file(path)?;
                }
                Err(err) => return Err(err.into()),
            }
        }
        bail!("could not take {}", path.display())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        let guard = dir.path().join("guard");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/fix.rs"), "branch fix").unwrap();
        std::fs::write(root.join("src/new.rs"), "branch only").unwrap();
        (dir, root, guard)
    }

    fn read(root: &Path, path: &str) -> Option<String> {
        std::fs::read_to_string(root.join(path)).ok()
    }

    #[test]
    fn restore_puts_back_every_file_the_run_replaced() {
        let (_dir, root, guard) = tree();
        let mut scratch = Scratch::new(&root, &guard).unwrap();
        scratch.write("src/fix.rs", Some("base code")).unwrap();
        scratch.write("src/fix.rs", Some("base code, second round")).unwrap();
        scratch.write("src/new.rs", None).unwrap();
        scratch.write("src/gone.rs", Some("base only")).unwrap();
        assert_eq!(read(&root, "src/fix.rs").as_deref(), Some("base code, second round"));

        restore(&root, &guard).unwrap();
        assert_eq!(read(&root, "src/fix.rs").as_deref(), Some("branch fix"));
        assert_eq!(read(&root, "src/new.rs").as_deref(), Some("branch only"));
        assert_eq!(read(&root, "src/gone.rs"), None);
        assert!(!guard.exists());
    }

    #[test]
    fn a_file_edited_after_an_interrupted_run_is_kept_not_overwritten() {
        let (_dir, root, guard) = tree();
        let mut scratch = Scratch::new(&root, &guard).unwrap();
        scratch.write("src/fix.rs", Some("base code")).unwrap();
        scratch.write("src/new.rs", None).unwrap();
        drop(scratch);
        std::fs::write(root.join("src/fix.rs"), "edited after the interruption").unwrap();

        assert!(restore(&root, &guard).is_err());
        assert_eq!(read(&root, "src/fix.rs").as_deref(), Some("edited after the interruption"));
        assert_eq!(read(&root, "src/new.rs").as_deref(), Some("branch only"));
        let saved = std::fs::read_to_string(guard.join("files/src/fix.rs")).unwrap();
        assert_eq!(saved, "branch fix");
        let manifest = std::fs::read_to_string(guard.join("manifest")).unwrap();
        assert_eq!(manifest.lines().count(), 1);
        assert!(manifest.ends_with("\tsrc/fix.rs\n"));
        assert!(Scratch::new(&root, &guard).is_err());
    }

    #[test]
    fn a_run_stopped_between_manifest_and_write_restores_cleanly() {
        let (_dir, root, guard) = tree();
        let mut scratch = Scratch::new(&root, &guard).unwrap();
        scratch.write("src/fix.rs", Some("base code")).unwrap();
        scratch.entries.get_mut("src/fix.rs").unwrap().1.push(digest(b"second round, never written"));
        scratch.save_manifest().unwrap();
        assert_eq!(read(&root, "src/fix.rs").as_deref(), Some("base code"));

        restore(&root, &guard).unwrap();
        assert_eq!(read(&root, "src/fix.rs").as_deref(), Some("branch fix"));
    }

    #[test]
    fn a_second_run_is_refused_while_the_first_is_alive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let first = Lock::acquire(&path).unwrap();
        assert!(Lock::acquire(&path).is_err());
        drop(first);
        std::fs::write(&path, "999999").unwrap();
        let taken_over = Lock::acquire(&path).unwrap();
        assert_eq!(std::fs::read_to_string(taken_over.path()).unwrap(), std::process::id().to_string());
    }
}
