//! A Claude account is a Claude Code configuration folder (`CLAUDE_CONFIG_DIR`): its sign-in lives
//! in the Keychain under a name of that folder's own, so two folders hold two sign-ins. The default
//! account is the user's own profile, run with no `CLAUDE_CONFIG_DIR` at all, since naming even the
//! default folder selects another sign-in. Every other account's folder links what the user set up
//! in `~/.claude` (skills, agents, commands, plugins, rules, instructions, settings and the
//! conversations), so a node works the same on any account; its sign-in and `.claude.json` are its
//! own. Alondra never reads or keeps a token: `claude auth` does all of it.
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::accounts::{self, AccountId};

/// What every account shares with the user's own `~/.claude`.
pub const SHARED: [&str; 8] = ["skills", "agents", "commands", "plugins", "rules", "CLAUDE.md", "settings.json", "projects"];
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const OUTPUT_LIMIT: u64 = 65536;

/// The user's own Claude Code folder, which every account links and Alondra never changes.
pub fn default_folder(user_home: &Path) -> PathBuf {
    user_home.join(".claude")
}

/// Creates the account's folder, private to the user, with a link to each of `SHARED` that
/// `shared_from` has. A name already in the folder is left as it is, so running it again only
/// adds what `shared_from` gained since.
pub fn prepare(home: &Path, id: &AccountId, shared_from: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let folder = accounts::folder(home, id).ok_or_else(|| std::io::Error::other("the default account has no folder of Alondra's"))?;
    let parent = folder.parent().expect("an account folder has a parent");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    match std::fs::symlink_metadata(&folder) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(std::io::Error::other(format!("{} is not a folder", folder.display())));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new().mode(0o700).create(&folder)?,
        Err(e) => return Err(e),
    }
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))?;
    for name in SHARED {
        let target = shared_from.join(name);
        let link = folder.join(name);
        if std::fs::symlink_metadata(&target).is_err() || std::fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        std::os::unix::fs::symlink(&target, &link)?;
    }
    Ok(folder)
}

/// Deletes the account's folder: the links go, never what they point to.
pub fn remove(home: &Path, id: &AccountId) -> std::io::Result<()> {
    let Some(folder) = accounts::folder(home, id) else {
        return Err(std::io::Error::other("the default account is the user's own and is never deleted"));
    };
    match std::fs::symlink_metadata(&folder) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&folder),
        Ok(_) => std::fs::remove_dir_all(&folder),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// What `claude auth status --json` says of a folder's sign-in. Never logged: it names the user.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub email: Option<String>,
    pub subscription: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStatus {
    logged_in: Option<bool>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
}

/// `None` when the answer does not say: a corrupt answer, or one that claims a sign-in while the
/// command failed.
pub fn parse_status(success: bool, bytes: &[u8]) -> Option<AuthStatus> {
    if bytes.len() as u64 > OUTPUT_LIMIT {
        return None;
    }
    let raw: RawStatus = serde_json::from_slice(bytes).ok()?;
    let clean = |text: Option<String>| text.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty());
    match raw.logged_in? {
        false => Some(AuthStatus::default()),
        true if success => Some(AuthStatus { logged_in: true, email: clean(raw.email), subscription: clean(raw.subscription_type) }),
        true => None,
    }
}

/// The `claude` the daemon's PATH finds first.
pub fn executable(path: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::split_paths(path).map(|p| p.join("claude")).find(|p| p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// The user the process runs as, which Claude's Keychain lookup goes by, never an inherited
/// `USER` or `LOGNAME`.
pub fn effective_username() -> Option<String> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    let code = unsafe { libc::getpwuid_r(libc::geteuid(), record.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
    if code != 0 || result.is_null() {
        return None;
    }
    let record = unsafe { record.assume_init() };
    if record.pw_name.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(record.pw_name) }.to_str().ok()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// How Claude is run for an account: nothing inherited but HOME, PATH and the user, in HOME, with
/// `CLAUDE_CONFIG_DIR` only for an account that is not the default one.
pub fn command(executable: &Path, user_home: &Path, path: &str, username: &str, folder: Option<&Path>) -> Command {
    let mut command = Command::new(executable);
    command.env_clear().env("HOME", user_home).env("PATH", path).env("USER", username).env("LOGNAME", username).current_dir(user_home);
    if let Some(folder) = folder {
        command.env("CLAUDE_CONFIG_DIR", folder).env("CLAUDE_HOME", folder);
    }
    command
}

/// Runs `command` with no input and waits up to `timeout` for it, keeping what it printed.
pub fn output(mut command: Command, timeout: Duration) -> Result<(ExitStatus, Vec<u8>), String> {
    let mut child = command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(|e| format!("could not run Claude Code: {e}"))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.take(OUTPUT_LIMIT + 1).read_to_end(&mut bytes).map(|_| bytes)
    });
    let until = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("Claude Code did not answer in time".into());
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let bytes = reader.join().map_err(|_| "could not read Claude Code's answer".to_owned())?.map_err(|e| e.to_string())?;
    Ok((status, bytes))
}

/// The sign-in of the account whose folder is `folder` (`None`: the default one).
pub fn status(executable: &Path, user_home: &Path, path: &str, username: &str, folder: Option<&Path>) -> Result<AuthStatus, String> {
    let mut command = command(executable, user_home, path, username, folder);
    command.args(["auth", "status", "--json"]);
    let (status, bytes) = output(command, STATUS_TIMEOUT)?;
    parse_status(status.success(), &bytes).ok_or_else(|| "Claude Code's answer about its sign-in could not be read".to_owned())
}

/// Signs the account out of Claude, which removes its sign-in from the Keychain.
pub fn logout(executable: &Path, user_home: &Path, path: &str, username: &str, folder: &Path) -> Result<(), String> {
    let mut command = command(executable, user_home, path, username, Some(folder));
    command.args(["auth", "logout"]);
    let (status, _) = output(command, STATUS_TIMEOUT)?;
    if status.success() { Ok(()) } else { Err(format!("claude auth logout ended with {status}")) }
}

#[cfg(test)]
#[path = "claude_account_tests.rs"]
mod tests;
