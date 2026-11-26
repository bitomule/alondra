use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::io::Read;
use anyhow::{Context as _, Result, ensure};
use alondra_foundation::locks::Poisonable as _;
use std::path::{Path, PathBuf};

use alondra_core::options::{self, OptionsFile};

pub struct Loaded {
    pub file: OptionsFile,
    pub error: Option<String>,
}

/// Reads `options.toml` on every call, so an edit applies at once. A broken file keeps the last
/// good one in use and says why; no file means only the built-ins.
pub fn load_in(home: &Path, last: &std::sync::Mutex<Option<OptionsFile>>) -> Loaded {
    let path = options::path_in(home);
    let mut last = last.lock_or_recover("the options file");
    let parsed = match read_options(home) {
        Ok(Some(text)) => OptionsFile::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Ok(None) => Ok(OptionsFile::fresh()),
        Err(why) => Err(format!("{}: {why}", path.display())),
    };
    match parsed {
        Ok(file) => {
            *last = Some(file.clone());
            Loaded { file, error: None }
        }
        Err(error) => Loaded { file: last.clone().unwrap_or_else(OptionsFile::fresh), error: Some(error) },
    }
}

pub fn user_home() -> PathBuf {
    alondra_core::config::home_dir(&|k| std::env::var(k).ok())
}

pub fn project_of(file: &OptionsFile, cwd: &Path) -> Option<String> {
    options::project_of(&file.projects_root(&user_home()), cwd)
}

pub const PENDING: &str = "options.pending";
pub const OPTIONS_LIMIT: u64 = 4 * 1024 * 1024;

pub fn pending(home: &Path) -> bool { home.join(PENDING).symlink_metadata().is_ok() }

/// The options file in `home`, or a fresh one when there is none.
pub fn options_at(home: &Path) -> Result<OptionsFile> {
    match read_options(home) {
        Ok(Some(contents)) => OptionsFile::parse(&contents).map_err(anyhow::Error::msg),
        Ok(None) => Ok(OptionsFile::fresh()),
        Err(error) => Err(anyhow::anyhow!(error)),
    }
}

pub fn read_options(home: &Path) -> Result<Option<String>, String> {
    if pending(home) { return Err("An accounts or model settings change is being reconciled; try again when it finishes".into()); }
    let text = read_optional(&alondra_core::options::path_in(home)).map_err(|why| why.to_string())?;
    if pending(home) { return Err("The settings changed while they were being read; refresh and try again".into()); }
    Ok(text)
}

pub fn read_optional(path: &Path) -> Result<Option<String>> {
    match File::open(path) {
        Ok(file) => {
            let mut text = String::new();
            file.take(OPTIONS_LIMIT + 1).read_to_string(&mut text)?;
            ensure!(text.len() as u64 <= OPTIONS_LIMIT, "the options file exceeds its size limit");
            Ok(Some(text))
        }
        Err(why) if why.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(why) => Err(why.into()),
    }
}

fn temporary(path: &Path) -> PathBuf {
    path.with_file_name(format!(".{}.{}.next", path.file_name().expect("settings file name").to_string_lossy(), alondra_foundation::random_hex(16)))
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<PathBuf> {
    let next = temporary(path);
    let result = (|| {
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&next)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok::<_, std::io::Error>(())
    })();
    if let Err(why) = result { let _ = fs::remove_file(&next); return Err(why.into()); }
    Ok(next)
}

pub fn sync_parent(path: &Path) -> Result<()> { File::open(path.parent().context("settings parent missing")?)?.sync_all()?; Ok(()) }

pub fn replace(path: &Path, bytes: &[u8]) -> Result<()> {
    let next = write_private(path, bytes)?;
    if let Err(why) = fs::rename(&next, path) { let _ = fs::remove_file(&next); return Err(why.into()); }
    sync_parent(path)
}

/// Writes `options.pending` in HOME, refusing when another settings change still waits there.
pub fn prepare_pending(home: &Path, bytes: &[u8]) -> Result<()> {
    let path = home.join(PENDING);
    let next = write_private(&path, bytes)?;
    let linked = fs::hard_link(&next, &path);
    let _ = fs::remove_file(&next);
    linked.context("another settings change is awaiting reconciliation")?;
    sync_parent(&path)
}

