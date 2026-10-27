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
    let mut last = last.lock().expect("options lock poisoned");
    let parsed = match crate::domain_store::provider_settings::read_options(home) {
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
