//! The provider accounts as this Mac keeps them: the folder Alondra makes for an added account, the
//! Codex app-server it asks for an account's sign-in, usage and models, the Codex sign-ins it
//! leaves half published, and the trust Claude Code keeps for each repository. What the engine
//! decides of them (which account a node runs on, what the user sees) stays in the engine.
use alondra_core::accounts::{AccountId, AccountKey};
use alondra_core::codex_account::ProbeReport;
use alondra_foundation::Millis;
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Whether a Codex probe goes on to read the account's models and usage once it knows who signed in.
pub type Accept<'a> = Box<dyn FnOnce(&ProbeReport) -> bool + 'a>;

pub trait Accounts: Send + Sync {
    /// Makes the folder of the added Claude account `account`, private to the user, with a link to
    /// each shared name `shared_from` has; running it again only adds what `shared_from` gained.
    fn prepare_claude(&self, home: &Path, account: &AccountId, shared_from: &Path) -> io::Result<PathBuf>;
    /// Deletes the Claude account's folder: its links go, never what they point to.
    fn remove_claude(&self, home: &Path, account: &AccountId) -> io::Result<()>;
    /// Makes the private folder of a named Codex account, which keeps its credential in a file;
    /// none for the native account.
    fn prepare_codex(&self, home: &Path, key: &AccountKey) -> Result<Option<PathBuf>, String>;
    /// Deletes the private folder of a named Codex account.
    fn remove_codex(&self, home: &Path, key: &AccountKey) -> io::Result<()>;
    /// Whether a session one Claude account started resumes with the other: both keep their
    /// conversations in the same `projects/` folder.
    fn shares_sessions(&self, home: &Path, user_home: &Path, default_profile: Option<&Path>, one: &AccountId, other: &AccountId) -> bool;
    /// Asks the Codex `command` runs who is signed in for `account` and, when `accept` takes that
    /// account, its models and usage, without running a turn.
    fn probe_codex(&self, command: Command, account: AccountKey, now: Millis, accept: Accept<'_>) -> Result<ProbeReport, String>;
    /// Whether the Codex at `executable` still keeps `thread`, started in `directory`.
    fn retained_codex_thread(&self, executable: &Path, environment: &BTreeMap<String, String>, thread: &str, directory: &Path) -> Result<bool, String>;
    /// Removes what Codex sign-in `attempt` left half published in `profile`.
    fn recover_codex_sign_in(&self, profile: &Path, attempt: &str);
    /// Deletes the staging folders of Codex sign-ins an older daemon left in `staging`.
    fn clear_sign_in_staging(&self, staging: &Path);
    /// The bytes of the private file a native Codex session in `directory` records its account in.
    fn native_session_identity(&self, directory: &Path) -> Result<Vec<u8>, String>;
    /// The repository Claude Code would have to trust for `cwd` and does not, for the account whose
    /// folder is `profile` (none: the user's own).
    fn untrusted(&self, profile: Option<&Path>, cwd: &Path) -> Option<PathBuf>;
    /// Like `untrusted`, but a repository inside `projects_root` is trusted on the spot: choosing
    /// that folder at setup is the user's trust for what is in it. What stays untrusted comes back.
    fn ensure_trusted(&self, profile: Option<&Path>, cwd: &Path, projects_root: Option<&Path>) -> Option<PathBuf>;
    /// Records that the user trusts `repository`, for their own profile and for each of `profiles`.
    fn grant_trust(&self, repository: &Path, profiles: &[PathBuf]) -> Result<(), String>;
}
