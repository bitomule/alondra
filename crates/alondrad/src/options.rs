use std::path::{Path, PathBuf};

use alondra_core::options::{self, OptionsFile, Resolved};

use crate::hub::Hub;

pub struct Loaded {
    pub file: OptionsFile,
    pub error: Option<String>,
}

/// Reads `options.toml` on every call, so an edit applies at once. A broken file keeps the last
/// good one in use and says why; no file means only the built-ins.
pub fn load(hub: &Hub) -> Loaded {
    let path = options::path_in(&hub.home);
    let mut last = hub.options.lock().expect("options lock poisoned");
    let parsed = match std::fs::read_to_string(&path) {
        Ok(text) => OptionsFile::parse(&text).map_err(|e| format!("{}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(OptionsFile::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    };
    match parsed {
        Ok(file) => {
            *last = Some(file.clone());
            Loaded { file, error: None }
        }
        Err(error) => Loaded { file: last.clone().unwrap_or_default(), error: Some(error) },
    }
}

pub fn user_home() -> PathBuf {
    alondra_core::config::home_dir(&|k| std::env::var(k).ok())
}

pub fn project_of(file: &OptionsFile, cwd: &Path) -> Option<String> {
    options::project_of(&file.projects_root(&user_home()), cwd)
}

/// The `claude` arguments for a launch: its name, permission mode, the flags the options set,
/// and the prompt with the options section appended.
pub fn launch_args(name: &str, permission_mode: Option<&str>, explicit_model: Option<&str>, prompt: &str, resolved: &Resolved, request_id: Option<&str>) -> Vec<String> {
    let mut args: Vec<String> = ["--bg", "--name", name, "--permission-mode", permission_mode.unwrap_or("auto")].map(String::from).into();
    args.extend(resolved.launch_flags(explicit_model));
    args.push("--".into());
    args.push(format!("{}\n\n{}", prompt.trim_end(), resolved.mission_section(request_id)));
    args
}
