//! A Claude account is a Claude Code configuration folder (`CLAUDE_CONFIG_DIR`): its sign-in lives
//! in the Keychain under a name of that folder's own, so two folders hold two sign-ins. The default
//! account is the user's own profile, run with no `CLAUDE_CONFIG_DIR` at all, since naming even the
//! default folder selects another sign-in. Every other account's folder links what the user set up
//! in `~/.claude` (skills, agents, commands, plugins, rules, instructions, settings and the
//! conversations), so a node works the same on any account; its sign-in and `.claude.json` are its
//! own. Alondra never reads or keeps a token: `claude auth` does all of it.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use serde::Deserialize;

use crate::accounts::{self, AccountId};

/// What every account shares with the user's own `~/.claude`.
pub const SHARED: [&str; 8] = ["skills", "agents", "commands", "plugins", "rules", "CLAUDE.md", "settings.json", "projects"];
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// The most of an answer Claude Code is read for.
pub const OUTPUT_LIMIT: u64 = 65536;

/// The user's own Claude Code folder, which every account links and Alondra never changes.
pub fn default_folder(user_home: &Path) -> PathBuf {
    user_home.join(".claude")
}

/// The Claude profile an environment names: `CLAUDE_CONFIG_DIR`, else `CLAUDE_HOME`; `None` is
/// Claude's own default profile. The daemon's environment names the default account's profile:
/// the one the user set Alondra up with, which nodes, setup, doctor, trust and the watcher share.
/// A folder of Alondra's accounts is never it: a node on an added account has that folder in its
/// environment, and whatever it runs must not make that account the default one too.
pub fn configured_profile(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let added = accounts::folders(&crate::config::alondra_home(env));
    ["CLAUDE_CONFIG_DIR", "CLAUDE_HOME"].into_iter().find_map(|key| env(key).filter(|value| !value.trim().is_empty())).map(PathBuf::from)
        .filter(|profile| !profile.starts_with(&added))
}

/// The `CLAUDE_CONFIG_DIR` Claude runs with for `account`: the folder Alondra keeps for an added
/// account, `default_profile` for the default one.
pub fn profile(home: &Path, account: &AccountId, default_profile: Option<&Path>) -> Option<PathBuf> {
    accounts::folder(home, account).or_else(|| default_profile.map(Path::to_path_buf))
}

/// The folder Claude keeps its jobs, sessions and plugins in for `profile`.
pub fn config_folder(profile: Option<&Path>, user_home: &Path) -> PathBuf {
    profile.map_or_else(|| default_folder(user_home), Path::to_path_buf)
}

/// The `.claude.json` Claude keeps its trust and account settings in for `profile`: inside the
/// profile's folder, or in HOME for the default one.
pub fn settings_file(profile: Option<&Path>, user_home: &Path) -> PathBuf {
    profile.unwrap_or(user_home).join(".claude.json")
}

/// What `claude auth status --json` says of a folder's sign-in. Never logged: it names the user.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AuthStatus {
    pub logged_in: bool,
    pub email: Option<String>,
    pub subscription: Option<String>,
    pub pay_per_use: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawStatus {
    logged_in: Option<bool>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    subscription_type: Option<String>,
    #[serde(default)]
    auth_method: Option<String>,
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
        true if success => Some(AuthStatus { logged_in: true, email: clean(raw.email), subscription: clean(raw.subscription_type),
            pay_per_use: matches!(raw.auth_method.as_deref(), Some("api_key" | "api_key_helper" | "third_party")) }),
        true => None,
    }
}

/// How Claude is run for an account: with the `environment` nodes run Claude with and nothing else
/// inherited but HOME, PATH and the user, in HOME, with `CLAUDE_CONFIG_DIR` only for an account
/// that is not the default one.
pub fn command(executable: &Path, environment: &BTreeMap<String, String>, user_home: &Path, path: &str, username: &str, folder: Option<&Path>) -> Command {
    let mut command = Command::new(executable);
    command.env_clear().envs(environment).env("HOME", user_home).env("PATH", path).env("USER", username).env("LOGNAME", username).current_dir(user_home);
    match folder {
        Some(folder) => command.env("CLAUDE_CONFIG_DIR", folder).env("CLAUDE_HOME", folder),
        None => command.env_remove("CLAUDE_CONFIG_DIR").env_remove("CLAUDE_HOME"),
    };
    command
}

/// The `claude auth status --json` that tells the sign-in of the account whose folder is `folder`.
pub fn status_command(executable: &Path, environment: &BTreeMap<String, String>, user_home: &Path, path: &str, username: &str, folder: Option<&Path>) -> Command {
    let mut command = command(executable, environment, user_home, path, username, folder);
    command.args(["auth", "status", "--json"]);
    command
}

/// The `claude auth logout` that signs the account whose folder is `folder` out of Claude, which
/// removes its sign-in from the Keychain.
pub fn logout_command(executable: &Path, environment: &BTreeMap<String, String>, user_home: &Path, path: &str, username: &str, folder: &Path) -> Command {
    let mut command = command(executable, environment, user_home, path, username, Some(folder));
    command.args(["auth", "logout"]);
    command
}

pub fn read_status(success: bool, bytes: &[u8]) -> Result<AuthStatus, String> {
    parse_status(success, bytes).ok_or_else(|| "Claude Code's answer about its sign-in could not be read".to_owned())
}

#[cfg(test)]
#[path = "claude_account_tests.rs"]
mod tests;
