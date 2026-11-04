//! How a `claude --bg` session of an earlier release holds its domain v2 bearer: in the private
//! settings file it was launched with, whose `env` block reaches the session and `alondra mcp`
//! reads. A session that reconnects gets its bearer again; the file goes when the session ends.
use alondra_core::domain::AgentId;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const DIR: &str = "runtime/sessions";
const AGENT_ENV: &str = "ALONDRA_DOMAIN_AGENT";

/// The launched session is bound: its bearer and Agent id join the file it already reads.
pub(crate) fn bind(path: &Path, agent: &AgentId, capability: &str) -> Result<()> {
    let mut settings: Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let env = settings["env"].as_object_mut().context("session settings have no env")?;
    env.insert("ALONDRA_DOMAIN_CAPABILITY".into(), json!(capability));
    env.insert(AGENT_ENV.into(), json!(agent.0));
    write_private(path, &settings)
}

/// The files bound to an Agent whose session ended.
pub(crate) fn remove_for(home: &Path, agent: &AgentId) -> Result<()> {
    for path in files_bound_to(home, agent)? {
        std::fs::remove_file(&path)?;
    }
    Ok(())
}

/// The session ended while its Agent still has work: `claude respawn` and a restart pass the
/// same `--settings` path again, so the file stays and only the bearer, which was revoked, goes.
pub(crate) fn suspend_for(home: &Path, agent: &AgentId) -> Result<()> {
    for path in files_bound_to(home, agent)? {
        let mut settings: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
        if let Some(env) = settings["env"].as_object_mut() {
            env.remove("ALONDRA_DOMAIN_CAPABILITY");
        }
        write_private(&path, &settings)?;
    }
    Ok(())
}

/// A restarted session connected under a settings file that kept no bearer: it gets a new one.
/// True when it was issued.
pub(crate) fn reissue(domain: &crate::domain_runtime::DomainRuntime, key: &str, agent: &AgentId) -> Result<bool> {
    anyhow::ensure!(!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "invalid session settings key");
    let path = domain.home().join(DIR).join(format!("{key}.json"));
    let Ok(bytes) = std::fs::read(&path) else { return Ok(false) };
    let settings: Value = serde_json::from_slice(&bytes)?;
    if settings["env"][AGENT_ENV] != json!(agent.0) || settings["env"].get("ALONDRA_DOMAIN_CAPABILITY").is_some() {
        return Ok(false);
    }
    let capability = domain.bind_agent(&agent.0)?;
    bind(&path, agent, capability.expose_for_child_environment())?;
    Ok(true)
}

fn files_bound_to(home: &Path, agent: &AgentId) -> Result<Vec<PathBuf>> {
    let entries = match std::fs::read_dir(home.join(DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut bound = vec![];
    for entry in entries {
        let path = entry?.path();
        let ours = std::fs::read(&path).ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some_and(|settings| settings["env"][AGENT_ENV] == json!(agent.0));
        if ours {
            bound.push(path);
        }
    }
    Ok(bound)
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
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

    /// The settings file an earlier release launched a `claude --bg` session with.
    fn earlier_launch(home: &Path, key: &str) -> PathBuf {
        let dir = home.join(DIR);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).unwrap();
        let path = dir.join(format!("{key}.json"));
        write_private(&path, &json!({"env": {"ALONDRA_DOMAIN_PROTOCOL": "2", "ALONDRA_DOMAIN_SETTINGS": path.display().to_string()}})).unwrap();
        path
    }

    #[test]
    fn the_bearer_joins_a_private_settings_file_that_goes_with_its_agent() {
        let home = crate::test_support::Directory::new();
        let path = earlier_launch(&home, "run-1");
        bind(&path, &"session-real".into(), &"c".repeat(64)).unwrap();
        let settings: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(settings["env"]["ALONDRA_DOMAIN_CAPABILITY"], json!("c".repeat(64)));
        assert_eq!(settings["env"]["ALONDRA_DOMAIN_PROTOCOL"], "2", "what the session already read stays");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        remove_for(&home, &"someone-else".into()).unwrap();
        assert!(path.exists());
        remove_for(&home, &"session-real".into()).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_session_that_ended_with_work_left_keeps_its_file_without_its_bearer() {
        let home = crate::test_support::Directory::new();
        let path = earlier_launch(&home, "run-1");
        bind(&path, &"session-real".into(), &"c".repeat(64)).unwrap();
        suspend_for(&home, &"session-real".into()).unwrap();
        let settings: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(settings["env"].get("ALONDRA_DOMAIN_CAPABILITY").is_none(), "the revoked bearer goes");
        assert_eq!(settings["env"][AGENT_ENV], "session-real", "the file stays bound to its agent");
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
