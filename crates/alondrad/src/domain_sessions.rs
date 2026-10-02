//! How a background Claude session receives its domain v2 bearer. `claude --bg` keeps none of the
//! launcher's environment and chooses its own session id, which the session's mod reports with
//! the key of this file (`node_reports::Reports::wait_launched`). The protocol and the bearer travel
//! in a private settings file passed with `--settings`: its `env` block reaches the session. The
//! bearer is written into that file once the launched session's real id is known and bound;
//! `alondra mcp` reads it from the file the `env` block names. The file goes when the session ends.
use alondra_core::domain_v2::AgentId;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};

const DIR: &str = "runtime/sessions";
pub(crate) const SETTINGS_ENV: &str = "ALONDRA_DOMAIN_SETTINGS";
const AGENT_ENV: &str = "ALONDRA_DOMAIN_AGENT";

/// The settings a launch passes before its session exists: Alondra's launch settings, the v2
/// protocol and where the bearer will be written.
pub(crate) fn prepare(home: &Path, key: &str) -> Result<PathBuf> {
    anyhow::ensure!(!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "invalid session settings key");
    let dir = home.join(DIR);
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir)?;
    let path = dir.join(format!("{key}.json"));
    let mut settings: Value = serde_json::from_str(&alondra_core::launch_settings())?;
    let env = settings["env"].as_object_mut().context("launch settings have no env")?;
    env.insert("ALONDRA_DOMAIN_PROTOCOL".into(), json!("2"));
    env.insert(SETTINGS_ENV.into(), json!(path.display().to_string()));
    write_private(&path, &settings)?;
    Ok(path)
}

/// The launched session is bound: its bearer and Agent id join the file it already reads.
pub(crate) fn bind(path: &Path, agent: &AgentId, capability: &str) -> Result<()> {
    let mut settings: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let env = settings["env"].as_object_mut().context("session settings have no env")?;
    env.insert("ALONDRA_DOMAIN_CAPABILITY".into(), json!(capability));
    env.insert(AGENT_ENV.into(), json!(agent.0));
    write_private(path, &settings)
}

/// `--plugin-dir` for each plugin and the session's settings file.
pub(crate) fn flags(plugins: &[&Path], settings: &Path) -> Vec<String> {
    let mut out = vec![];
    for p in plugins {
        out.extend(["--plugin-dir".to_string(), p.display().to_string()]);
    }
    out.extend(["--settings".to_string(), settings.display().to_string()]);
    out
}

/// The files bound to an Agent whose session ended.
pub(crate) fn remove_for(home: &Path, agent: &AgentId) -> Result<()> {
    let entries = match std::fs::read_dir(home.join(DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries {
        let path = entry?.path();
        let bound = std::fs::read(&path).ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|settings| settings["env"][AGENT_ENV] == json!(agent.0));
        if bound {
            std::fs::remove_file(&path)?;
        }
    }
    Ok(())
}

pub(crate) fn remove(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn write_private(path: &Path, value: &Value) -> Result<()> {
    let next = path.with_extension("next");
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&next)?;
    file.write_all(&serde_json::to_vec(value)?)?;
    file.sync_all()?;
    std::fs::rename(&next, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_bearer_joins_a_private_settings_file_that_goes_with_its_agent() {
        let home = std::env::temp_dir().join(format!("alondra-session-settings-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let path = prepare(&home, "run-1").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(home.join(DIR)).unwrap().permissions().mode() & 0o777, 0o700);
        let settings: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(settings["env"]["ALONDRA_DOMAIN_PROTOCOL"], "2");
        assert_eq!(settings["env"][SETTINGS_ENV], json!(path.display().to_string()));
        assert_eq!(settings["env"][alondra_core::LAUNCHED_ENV], "1");
        assert!(settings["env"].get("ALONDRA_DOMAIN_CAPABILITY").is_none(), "no bearer before the session is bound");
        assert_eq!(flags(&[Path::new("/r/plugin")], &path), ["--plugin-dir", "/r/plugin", "--settings", path.to_str().unwrap()]);

        bind(&path, &"session-real".into(), &"c".repeat(64)).unwrap();
        let settings: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(settings["env"]["ALONDRA_DOMAIN_CAPABILITY"], json!("c".repeat(64)));
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        remove_for(&home, &"someone-else".into()).unwrap();
        assert!(path.exists());
        remove_for(&home, &"session-real".into()).unwrap();
        assert!(!path.exists());
        std::fs::remove_dir_all(home).unwrap();
    }
}
