//! The Mac as the engine reaches it: git and the task worktrees, the disks, its memory and load,
//! the provider accounts' folders, sign-ins and trust, its processes, Alondra's plugins, Claude's
//! sign-in listeners, and the files and checks only the daemon itself makes. The engine asks for
//! them through the ports of `engine-ports`; the root gives the registry this crate's
//! implementations.
#[cfg(test)]
#[path = "../../test_support.rs"]
mod test_support;

#[macro_use]
extern crate alondra_foundation;

pub mod antigravity_account;
pub mod claude_account;
pub mod claude_trust;
pub mod codex_account;
pub mod codex_credentials;
pub mod disk;
pub mod hook_socket;
pub mod launch_worktree;
pub mod local_network;
pub mod machine;
pub mod plugins;
pub mod rehearsal;
#[cfg(target_os = "macos")]
pub mod schedule_clock;
pub mod secret_files;
pub mod sign_in_returns;
pub mod spec_worktree;
pub mod worktree;
pub mod worktrees_root;

use alondra_core::options::WorktreesRoot;
use alondra_core::secret_destination::SecretDestination;
use alondra_domain_model::{ChecklistMark, DeliveryTarget, DomainError, DomainState, Interaction, TaskId};
use std::time::Duration;
use alondra_core::accounts::{AccountId, AccountKey};
use alondra_core::codex_account::ProbeReport;
use alondra_engine_ports::Registry;
use alondra_engine_ports::accounts::{Accept, Accounts};
use alondra_engine_ports::plugins::Plugins;
use alondra_engine_ports::processes::{Process, Processes};
use alondra_engine_ports::sign_in_returns::SignInReturns;
use alondra_engine_ports::disk::{Disk, Space};
use alondra_engine_ports::machine::MachineProbe;
use alondra_engine_ports::worktrees::{Git, LaunchError, Protected, Repository, TaskWorktree, WorktreeOrigin, Workplace, Worktrees};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Gives the registry the Mac's disks, gauges, accounts, processes, plugins, sign-in listeners and
/// its git, which finds the sessions' worktrees in the Claude folder `claude_home` names.
pub fn wire(registry: &Registry, claude_home: fn() -> PathBuf) {
    wire_with(registry, claude_home, rehearsal::requested());
}

/// `wire`, with the Mac of an upgrade rehearsal (`rehearsal`) in place of the real disk and gauges.
pub fn wire_with(registry: &Registry, claude_home: fn() -> PathBuf, rehearsal: bool) {
    registry.provide::<dyn Worktrees>(Arc::new(GitWorktrees { claude_home }));
    registry.provide::<dyn alondra_engine_ports::spec_worktrees::SpecWorktrees>(Arc::new(spec_worktree::GitSpecWorktrees));
    if rehearsal {
        registry.provide::<dyn Disk>(Arc::new(rehearsal::RoomyDisk));
        registry.provide::<dyn MachineProbe>(Arc::new(rehearsal::RoomyMac));
    } else {
        registry.provide::<dyn Disk>(Arc::new(Disks));
        registry.provide::<dyn MachineProbe>(Arc::new(machine::Mac::new(Arc::new(Disks))));
    }
    registry.provide::<dyn Accounts>(Arc::new(MacAccounts));
    registry.provide::<dyn Processes>(Arc::new(MacProcesses));
    registry.provide::<dyn Plugins>(Arc::new(MacPlugins));
    registry.provide::<dyn SignInReturns>(Arc::new(Listeners));
    #[cfg(target_os = "macos")]
    registry.provide::<dyn alondra_engine_ports::schedule_clock::ScheduleClock>(Arc::new(schedule_clock::MacClock));
}

pub struct Listeners;

impl SignInReturns for Listeners {
    fn hand_back(&self, port: u16, target: &str) -> Result<u16, String> {
        sign_in_returns::hand_back(port, target)
    }
    fn listening(&self, port: u16) -> bool {
        sign_in_returns::listening(port)
    }
}

pub struct MacPlugins;

impl Plugins for MacPlugins {
    fn node(&self, home: &Path) -> Option<PathBuf> {
        plugins::node_in(home)
    }
    fn orchestrator(&self, home: &Path) -> Option<PathBuf> {
        plugins::orchestrator_in(home)
    }
    fn registered(&self, claude_home: &Path, name: &str) -> Option<PathBuf> {
        plugins::dir(claude_home, name)
    }
}

pub struct MacProcesses;

impl Processes for MacProcesses {
    fn table(&self) -> Vec<Process> {
        alondra_process_group::table().unwrap_or_default()
    }
    fn signal(&self, pids: &[u32], signal: i32) {
        for pid in pids {
            unsafe { libc::kill(*pid as libc::pid_t, signal) };
        }
    }
}

pub struct MacAccounts;

impl Accounts for MacAccounts {
    fn prepare_claude(&self, home: &Path, account: &AccountId, shared_from: &Path) -> std::io::Result<PathBuf> {
        claude_account::prepare(home, account, shared_from)
    }
    fn remove_claude(&self, home: &Path, account: &AccountId) -> std::io::Result<()> {
        claude_account::remove(home, account)
    }
    fn prepare_codex(&self, home: &Path, key: &AccountKey) -> Result<Option<PathBuf>, String> {
        codex_account::prepare(home, key)
    }
    fn remove_codex(&self, home: &Path, key: &AccountKey) -> std::io::Result<()> {
        codex_account::remove(home, key)
    }
    fn prepare_antigravity(&self, home: &Path, key: &AccountKey, shared_from: &Path) -> Result<Option<PathBuf>, String> {
        antigravity_account::prepare(home, key, shared_from)
    }
    fn remove_antigravity(&self, home: &Path, key: &AccountKey) -> std::io::Result<()> {
        antigravity_account::remove(home, key)
    }
    fn set_antigravity_key(&self, home: &Path, key: &AccountKey, secret: Option<&alondra_core::antigravity_account::Secret>) -> Result<(), String> {
        antigravity_account::set_key(home, key, secret)
    }
    fn antigravity_key(&self, home: &Path, key: &AccountKey) -> Result<Option<alondra_core::antigravity_account::Secret>, String> {
        antigravity_account::key(home, key)
    }
    fn antigravity_key_set(&self, home: &Path, key: &AccountKey) -> bool {
        antigravity_account::key_set(home, key)
    }
    fn check_antigravity(&self, probe: &alondra_core::antigravity_account::Probe) -> alondra_core::antigravity_account::CheckReport {
        antigravity_account::check(probe)
    }
    fn antigravity_usage(&self, probe: &alondra_core::antigravity_account::Probe) -> Result<alondra_core::antigravity_account::UsageOutcome, String> {
        antigravity_account::usage(probe)
    }
    fn shares_sessions(&self, home: &Path, user_home: &Path, default_profile: Option<&Path>, one: &AccountId, other: &AccountId) -> bool {
        claude_account::shares_sessions(home, user_home, default_profile, one, other)
    }
    fn probe_codex(&self, command: std::process::Command, account: AccountKey, now: alondra_foundation::Millis, accept: Accept<'_>) -> Result<ProbeReport, String> {
        codex_account::probe_matching(command, account, now, accept)
    }
    fn retained_codex_thread(&self, executable: &Path, environment: &std::collections::BTreeMap<String, String>, thread: &str, directory: &Path) -> Result<bool, String> {
        codex_account::retained_thread(executable, environment, thread, directory)
    }
    fn recover_codex_sign_in(&self, profile: &Path, attempt: &str) {
        codex_credentials::recover_pending(profile, attempt)
    }
    fn clear_sign_in_staging(&self, staging: &Path) {
        codex_credentials::clear_staging(staging)
    }
    fn native_session_identity(&self, directory: &Path) -> Result<Vec<u8>, String> {
        codex_account::native_session_identity(directory)
    }
    fn untrusted(&self, profile: Option<&Path>, cwd: &Path) -> Option<PathBuf> {
        claude_trust::untrusted(&claude_trust::config_of(profile)?, cwd)
    }
    fn ensure_trusted(&self, profile: Option<&Path>, cwd: &Path, projects_root: Option<&Path>) -> Option<PathBuf> {
        claude_trust::ensure_for(profile, cwd, projects_root)
    }
    fn grant_trust(&self, repository: &Path, profiles: &[PathBuf]) -> Result<(), String> {
        claude_trust::grant(repository, profiles)
    }
}

pub struct Disks;

impl Disk for Disks {
    fn space(&self, path: &Path) -> Option<Space> {
        disk::space(path)
    }
}

pub struct GitWorktrees {
    claude_home: fn() -> PathBuf,
}

impl Worktrees for GitWorktrees {
    fn worktree_of(&self, session_id: &str) -> Option<PathBuf> {
        worktree::worktree_of(&(self.claude_home)(), session_id)
    }
    fn origin(&self, session_id: &str) -> Option<WorktreeOrigin> {
        worktree::origin_in(&(self.claude_home)(), session_id)
    }
    fn subagent_worktree(&self, own: &Path, agent: &str) -> Option<PathBuf> {
        worktree::subagent_worktree(own, agent)
    }
    fn registrable(&self, path: &Path, protected: &Protected) -> Result<(), String> {
        worktree::registrable(path, protected)
    }
    fn branch_in<'a>(&'a self, dir: &'a Path) -> Git<'a, Option<String>> {
        Box::pin(worktree::branch_in(dir))
    }
    fn summary(&self, dir: &Path) -> String {
        worktree::summary(dir)
    }
    fn leftovers<'a>(&'a self, session_id: &'a str) -> Git<'a, Option<String>> {
        Box::pin(async move { worktree::leftovers(&(self.claude_home)(), session_id).await })
    }
    fn release_task_worktree<'a>(&'a self, dir: &'a Path, slug: &'a str, remove: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::release_task_worktree(dir, slug, remove))
    }
    fn release_finished_worktree<'a>(&'a self, dir: &'a Path, slug: Option<&'a str>, give_up: bool, pr_merged: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::release_finished_worktree(dir, slug, give_up, pr_merged))
    }
    fn release_recorded_worktree<'a>(&'a self, dir: &'a Path, give_up: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::release_recorded_worktree(dir, give_up))
    }
    fn discard_worktree<'a>(&'a self, dir: &'a Path, keep_branch: bool) -> Git<'a, Result<TaskWorktree, String>> {
        Box::pin(worktree::discard_worktree(dir, keep_branch))
    }
    fn exists(&self, dir: &Path) -> bool {
        dir.exists()
    }
    fn unreachable_link(&self, dir: &Path) -> Option<String> {
        worktree::unreachable_link(dir)
    }
    fn uncommitted_files_of_alondras_worktree<'a>(&'a self, dir: &'a Path) -> Git<'a, Result<Option<usize>, String>> {
        Box::pin(worktree::uncommitted_files_of_alondras_worktree(dir))
    }
    fn unsaved<'a>(&'a self, top: &'a Path) -> Git<'a, Result<Option<String>, String>> {
        Box::pin(worktree::unsaved(top))
    }
    fn isolate<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<Workplace, LaunchError>> {
        Box::pin(launch_worktree::isolate_in(cwd, slug, known, pr_head, root))
    }
    fn reopen<'a>(&'a self, cwd: &'a Path, slug: &'a str, original: &'a Path, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<(Workplace, bool), LaunchError>> {
        Box::pin(launch_worktree::reopen_in(cwd, slug, original, pr_head, root))
    }
    fn storage_for<'a>(&'a self, cwd: &'a Path, slug: &'a str, known: Option<&'a Path>, pr_head: Option<&'a str>, root: Option<&'a WorktreesRoot>) -> Git<'a, Result<PathBuf, LaunchError>> {
        Box::pin(launch_worktree::storage_for(cwd, slug, known, pr_head, root))
    }
    fn recovery_directory_matches<'a>(&'a self, cwd: &'a Path, original: &'a Path) -> Git<'a, bool> {
        Box::pin(launch_worktree::recovery_directory_matches(cwd, original))
    }
    fn chosen(&self, workplace: &Workplace) -> Option<PathBuf> {
        launch_worktree::chosen(workplace).map(Path::to_path_buf)
    }
    fn stored(&self, worktree: &Path) -> Option<Repository> {
        worktrees_root::stored(worktree)
    }
    fn restore(&self, worktree: &Path, record: &Repository) -> Result<(), String> {
        worktrees_root::restore(worktree, record)
    }
    fn capture_root(&self, path: &Path) -> Result<WorktreesRoot, String> {
        worktrees_root::capture(path)
    }
    fn final_refusal<'a>(&'a self, state: &'a DomainState, session_id: &'a str, task: &'a TaskId, targets: &'a [DeliveryTarget], limit: Duration) -> Git<'a, Option<DomainError>> {
        Box::pin(async move { worktree::final_refusal(&(self.claude_home)(), state, session_id, task, targets, limit).await })
    }
    fn evidence_refusal<'a>(&'a self, state: &'a DomainState, session_id: &'a str, task: &'a TaskId, checklist: &'a [ChecklistMark], limit: Duration) -> Git<'a, Option<DomainError>> {
        Box::pin(async move { worktree::evidence_refusal(&(self.claude_home)(), state, session_id, task, checklist, limit).await })
    }
    fn secret_refusal(&self, state: &DomainState, session_id: &str, task: &TaskId, destination: &SecretDestination) -> Option<DomainError> {
        secret_files::ask_refusal(&(self.claude_home)(), state, session_id, task, destination)
    }
    fn write_secret(&self, state: &DomainState, interaction: &Interaction, value: &str) -> Result<String, String> {
        secret_files::write(&(self.claude_home)(), state, interaction, value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::OnceLock;

    static CLAUDE_HOME: OnceLock<PathBuf> = OnceLock::new();

    fn claude_home() -> PathBuf {
        CLAUDE_HOME.get().cloned().unwrap_or_default()
    }

    // spec: antigravity.key-file/one-rule-for-daemon-and-host
    // spec: antigravity.key-file/refused-file-reads-as-no-key
    #[test]
    fn the_macs_accounts_keep_the_contract_the_doubles_keep() {
        let scratch = crate::test_support::Directory::new();
        std::fs::create_dir_all(&scratch).unwrap();
        claude_trust::use_config_for_test(&scratch.join(".claude.json"));
        alondra_engine_doubles::contract::accounts(&MacAccounts, &|profile, repositories| {
            let projects: serde_json::Map<String, serde_json::Value> = repositories.iter()
                .map(|(repository, accepted)| (repository.display().to_string(), serde_json::json!({"hasTrustDialogAccepted": accepted}))).collect();
            std::fs::write(profile.join(".claude.json"), serde_json::json!({"projects": projects}).to_string()).unwrap();
        });
    }

    #[test]
    fn the_macs_processes_and_plugins_keep_the_contract_the_doubles_keep() {
        alondra_engine_doubles::contract::processes(&MacProcesses);
        alondra_engine_doubles::contract::plugins(&MacPlugins);
    }

    #[test]
    fn the_macs_disks_keep_the_contract_the_doubles_keep() {
        alondra_engine_doubles::contract::disk(&Disks);
    }

    fn git_repository(path: &std::path::Path) {
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(path).args(args).output().unwrap().status.success(), "git {args:?}");
        git(&["init", "-q", "-b", "main"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "t"]);
        git(&["add", "-A"]);
        git(&["commit", "-q", "--allow-empty", "-m", "base"]);
    }

    #[test]
    fn the_macs_git_keeps_the_contract_the_doubles_keep() {
        alondra_engine_doubles::contract::worktrees(&GitWorktrees { claude_home: PathBuf::new }, &git_repository);
        alondra_engine_doubles::contract::final_checks(&GitWorktrees { claude_home: PathBuf::new }, &git_repository, &slow_status, &alondra_domain_rules::Rules);
        let git = |dir: &std::path::Path, args: &[&str]| std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        alondra_engine_doubles::contract::saved_work(&GitWorktrees { claude_home: PathBuf::new }, &alondra_engine_doubles::contract::Saving {
            task_worktree: &worktree::testing::task_worktree,
            local_commit: &|dir| assert!(git(dir, &["commit", "-q", "--allow-empty", "-m", "fix"]).status.success()),
            push: &|dir| assert!(git(dir, &["push", "-q", "-u", "origin", "HEAD"]).status.success()),
            branch_exists: &|repo, branch| git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).status.success(),
        });
        alondra_engine_doubles::contract::finished_branches(&GitWorktrees { claude_home: PathBuf::new }, &alondra_engine_doubles::contract::Finishing {
            remoteless_worktree: &worktree::testing::remoteless_task_worktree,
            local_commit: &|dir| assert!(git(dir, &["commit", "-q", "--allow-empty", "-m", "fix"]).status.success()),
            branch_exists: &|repo, branch| git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).status.success(),
        });
    }

    /// Every `git status` in `dir` takes `delay`: git asks a file-system monitor hook that sleeps
    /// before it answers.
    fn slow_status(dir: &std::path::Path, delay: std::time::Duration) {
        use std::os::unix::fs::PermissionsExt as _;
        let hook = dir.parent().unwrap().join(format!("{}-slow-fsmonitor", dir.file_name().unwrap().to_string_lossy()));
        std::fs::write(&hook, format!("#!/bin/sh\nsleep {}\nexit 1\n", delay.as_secs())).unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let git = std::process::Command::new("git").arg("-C").arg(dir).args(["config", "core.fsmonitor", hook.to_str().unwrap()]).output().unwrap();
        assert!(git.status.success());
    }

    #[test]
    fn the_wired_worktrees_find_a_sessions_worktree_in_the_claude_folder_the_root_names() {
        let scratch = crate::test_support::Directory::new();
        let made = scratch.join("repo/.claude/worktrees/t");
        std::fs::create_dir_all(&made).unwrap();
        let home = scratch.join("claude");
        let job = home.join("jobs/0123abcd");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("state.json"), serde_json::json!({"worktreePath": made}).to_string()).unwrap();
        CLAUDE_HOME.set(home).unwrap();
        let registry = Registry::default();
        wire(&registry, claude_home);
        let worktrees = registry.worktrees();
        assert_eq!(worktrees.worktree_of("0123abcd-session"), Some(made.clone()));
        assert_eq!(worktrees.origin("0123abcd-session"), Some(WorktreeOrigin::Made(made)));
        assert_eq!(worktrees.worktree_of("ffffffff-other"), None, "a session without a job has none");
        assert!(registry.missing().iter().all(|port| !matches!(*port, "Accounts" | "Disk" | "MachineProbe" | "Plugins" | "Processes" | "ScheduleClock" | "SignInReturns" | "Worktrees")), "{:?}", registry.missing());
    }
}
