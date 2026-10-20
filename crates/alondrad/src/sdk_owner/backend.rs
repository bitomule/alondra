//! The SDK backend's configuration names the pinned Node, the host bundle and the native Claude,
//! each with its SHA256, and the exact environments they get. A daemon without it never builds an
//! SDK backend. Outside a sandbox the native Claude runs as the user's own: in their real HOME and
//! with their default profile, so it signs in as the user does; Seatbelt confinement stays optional.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) const FILE: &str = "sdk-backend.json";
/// Setting either selects another credential namespace than the user's default profile, even when
/// it names the default folder.
const DEFAULT_PROFILE: [&str; 2] = ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"];

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SdkBackend {
    pub(crate) node: PathBuf,
    pub(crate) node_sha256: String,
    pub(crate) host: PathBuf,
    pub(crate) host_sha256: String,
    pub(crate) native: PathBuf,
    pub(crate) native_sha256: String,
    pub(crate) host_environment: BTreeMap<String, String>,
    pub(crate) native_environment: BTreeMap<String, String>,
    /// A Seatbelt profile the whole host runs under, Claude included.
    #[serde(default)]
    pub(crate) confinement: Option<PathBuf>,
    /// Short enough for the host's Unix socket: each execution gets a private folder in it.
    pub(crate) state_root: PathBuf,
    pub(crate) io_timeout_seconds: u64,
}

/// The configuration in `home`, checked: `None` without one. `user_home` is the HOME the native
/// Claude of a daemon outside a sandbox runs in, whatever the configuration leaves out.
pub(crate) fn load(home: &Path, sandbox: bool, user_home: &Path) -> Result<Option<SdkBackend>> {
    let path = home.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut backend: SdkBackend = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
    for (file, sha256) in [(&backend.node, &backend.node_sha256), (&backend.host, &backend.host_sha256), (&backend.native, &backend.native_sha256)] {
        ensure!(file.is_absolute(), "{} must be an absolute path", file.display());
        ensure!(digest(file)? == *sha256, "{} does not have the SHA256 {sha256} the SDK backend names", file.display());
    }
    for profile in DEFAULT_PROFILE {
        ensure!(!backend.native_environment.contains_key(profile),
            "the SDK backend's native environment sets {profile}: Claude Code runs with the user's default profile, which leaves it unset");
    }
    if !sandbox {
        let home = backend.native_environment.entry("HOME".into()).or_insert_with(|| user_home.display().to_string());
        ensure!(Path::new(home) == user_home,
            "the SDK backend's native environment sets HOME to {home}: outside a sandbox Claude Code runs in the user's HOME, {}", user_home.display());
    }
    if let Some(confinement) = &backend.confinement {
        ensure!(confinement.is_absolute(), "the SDK backend's Seatbelt profile must be an absolute path");
    }
    ensure!(backend.state_root.is_absolute(), "the SDK state root must be an absolute path");
    ensure!((1..=300).contains(&backend.io_timeout_seconds), "the SDK host's I/O timeout is 1 to 300 seconds");
    Ok(Some(backend))
}

impl SdkBackend {
    /// The SHA256 of the configuration as it runs: every artifact's digest, both environments, the
    /// confinement and the host's limits. The provider binding is named by it, so executions keep
    /// the configuration they were admitted under and a changed one binds the next ones.
    pub(crate) fn configuration_digest(&self) -> String {
        use sha2::Digest;
        let canonical = serde_json::to_vec(self).expect("the SDK backend serializes");
        sha2::Sha256::digest(canonical).iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

pub(crate) fn digest(file: &Path) -> Result<String> {
    use sha2::Digest;
    use std::io::Read;
    let mut hasher = sha2::Sha256::new();
    let mut reader = std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
    let mut buffer = vec![0; 1 << 20];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configuration(home: &Path, native_environment: serde_json::Value) -> serde_json::Value {
        let file = home.join("artifact");
        std::fs::write(&file, b"artifact").unwrap();
        let sha256 = digest(&file).unwrap();
        serde_json::json!({
            "node": file, "node_sha256": sha256, "host": file, "host_sha256": sha256, "native": file, "native_sha256": sha256,
            "host_environment": {}, "native_environment": native_environment,
            "state_root": home.join("state"), "io_timeout_seconds": 30,
        })
    }

    #[test]
    fn a_daemon_without_a_configuration_builds_no_sdk_backend() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        assert!(load(&home, false, &home.join("user")).unwrap().is_none(), "no configuration, no SDK backend");
        assert!(load(&home, true, &home.join("user")).unwrap().is_none(), "a sandbox without the configuration has none either");
    }

    #[test]
    fn a_daemon_outside_a_sandbox_runs_claude_in_the_users_home_with_or_without_seatbelt() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let user = home.join("user");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"PATH": "/usr/bin"})).to_string()).unwrap();
        let backend = load(&home, false, &user).unwrap().expect("a production daemon admits the configuration");
        assert_eq!(backend.native_environment.get("HOME").map(String::as_str), Some(user.to_str().unwrap()), "Claude runs in the user's real HOME");
        assert!(backend.confinement.is_none(), "Seatbelt is optional");
        let mut confined = configuration(&home, serde_json::json!({"HOME": user}));
        confined["confinement"] = serde_json::json!(home.join("profile.sb"));
        std::fs::write(home.join(FILE), confined.to_string()).unwrap();
        assert!(load(&home, false, &user).unwrap().unwrap().confinement.is_some(), "and allowed");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"HOME": home.join("synthetic")})).to_string()).unwrap();
        let refused = load(&home, false, &user).unwrap_err().to_string();
        assert!(refused.contains("runs in the user's HOME"), "{refused}");
        assert!(load(&home, true, &user).unwrap().is_some(), "a sandbox daemon keeps its synthetic HOME");
    }

    #[test]
    fn the_configuration_digest_changes_with_what_runs() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let user = home.join("user");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({})).to_string()).unwrap();
        let first = load(&home, false, &user).unwrap().unwrap();
        assert_eq!(first.configuration_digest(), load(&home, false, &user).unwrap().unwrap().configuration_digest(), "the same configuration, the same digest");
        std::fs::write(home.join(FILE), configuration(&home, serde_json::json!({"PATH": "/bin"})).to_string()).unwrap();
        assert_ne!(first.configuration_digest(), load(&home, false, &user).unwrap().unwrap().configuration_digest(), "another environment, another digest");
    }

    // Claude Code keeps the user's sign-in per profile: naming the default folder in either
    // variable selects another credential namespace than the one the user signed in to.
    #[test]
    fn a_native_environment_that_names_a_claude_profile_is_refused() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        for (profile, sandbox) in DEFAULT_PROFILE.into_iter().flat_map(|profile| [(profile, true), (profile, false)]) {
            let backend = configuration(&home, serde_json::json!({"HOME": home.join("home"), profile: home.join("home/.claude")}));
            std::fs::write(home.join(FILE), backend.to_string()).unwrap();
            let refused = load(&home, sandbox, &home.join("home")).unwrap_err().to_string();
            assert!(refused.contains(&format!("sets {profile}")), "{refused}");
        }
    }
}
