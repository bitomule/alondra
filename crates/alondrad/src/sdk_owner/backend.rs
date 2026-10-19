//! The SDK backend exists only in a sandbox daemon: its configuration names the pinned Node, the
//! host bundle and the native Claude, each with its SHA256, and the exact environments they get.
//! A daemon outside a sandbox that finds it refuses to start; one without it never builds one.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub(crate) const FILE: &str = "sdk-backend.json";
/// Setting either selects another credential namespace than the user's default profile, even when
/// it names the default folder.
const DEFAULT_PROFILE: [&str; 2] = ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"];

#[derive(Clone, Debug, Deserialize)]
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

pub(crate) fn load(home: &Path, sandbox: bool) -> Result<Option<SdkBackend>> {
    let path = home.join(FILE);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if !sandbox {
        bail!("{} configures the SDK backend, which runs only in a sandbox daemon (ALONDRA_SANDBOX=1): remove it", path.display());
    }
    let backend: SdkBackend = serde_json::from_slice(&bytes).with_context(|| format!("reading {}", path.display()))?;
    for (file, sha256) in [(&backend.node, &backend.node_sha256), (&backend.host, &backend.host_sha256), (&backend.native, &backend.native_sha256)] {
        ensure!(file.is_absolute(), "{} must be an absolute path", file.display());
        ensure!(digest(file)? == *sha256, "{} does not have the SHA256 {sha256} the SDK backend names", file.display());
    }
    for profile in DEFAULT_PROFILE {
        ensure!(!backend.native_environment.contains_key(profile),
            "the SDK backend's native environment sets {profile}: Claude Code runs with the user's default profile, which leaves it unset");
    }
    ensure!(backend.state_root.is_absolute(), "the SDK state root must be an absolute path");
    ensure!((1..=300).contains(&backend.io_timeout_seconds), "the SDK host's I/O timeout is 1 to 300 seconds");
    Ok(Some(backend))
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

    #[test]
    fn a_daemon_outside_a_sandbox_refuses_an_sdk_backend_and_builds_none_without_one() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        assert!(load(&home, false).unwrap().is_none(), "no configuration, no SDK backend");
        assert!(load(&home, true).unwrap().is_none(), "a sandbox without the configuration has none either");
        std::fs::write(home.join(FILE), b"{}").unwrap();
        let refused = load(&home, false).unwrap_err().to_string();
        assert!(refused.contains("runs only in a sandbox daemon"), "{refused}");
    }

    // Claude Code keeps the user's sign-in per profile: naming the default folder in either
    // variable selects another credential namespace than the one the user signed in to.
    #[test]
    fn a_native_environment_that_names_a_claude_profile_is_refused() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&home).unwrap();
        let file = home.join("artifact");
        std::fs::write(&file, b"artifact").unwrap();
        let sha256 = digest(&file).unwrap();
        for profile in DEFAULT_PROFILE {
            let backend = serde_json::json!({
                "node": file, "node_sha256": sha256, "host": file, "host_sha256": sha256, "native": file, "native_sha256": sha256,
                "host_environment": {}, "native_environment": {"HOME": home.join("home"), profile: home.join("home/.claude")},
                "state_root": home.join("state"), "io_timeout_seconds": 30,
            });
            std::fs::write(home.join(FILE), backend.to_string()).unwrap();
            let refused = load(&home, true).unwrap_err().to_string();
            assert!(refused.contains(&format!("sets {profile}")), "{refused}");
        }
    }
}
