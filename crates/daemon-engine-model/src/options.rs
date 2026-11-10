use std::fs::File;
use std::io::Read;
use anyhow::{Result, ensure};
use crate::locks::Poisonable as _;
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
