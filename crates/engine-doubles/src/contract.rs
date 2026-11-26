//! What the engine relies on from the `Worktrees`, `Disk`, `MachineProbe`, `Accounts`, `Processes`,
//! `Plugins` and `AgentHost` ports. The doubles of this crate pass it in their unit tests;
//! `crates/system` passes it with the Mac's own git, disks, gauges, files and processes, and
//! `crates/agent-runtime` with a host process of its own, so a double never drifts from what the
//! daemon really does.
use crate::test_support::Directory;
pub mod github;
use alondra_engine_ports::agent_host::{AdoptedEndpoint, AgentHost, EndedHost, HostFolder, HostLaunch, HostStop, LaunchIdentity, LaunchedHost, Retained, VerifiedArtifact};
use alondra_engine_ports::disk::Disk;
use alondra_engine_ports::machine::MachineProbe;
use alondra_core::secret_destination::SecretDestination;
use alondra_domain_model::{Actor, Command, CommandEnvelope, DomainRules, DomainState, LaunchPlan, PROTOCOL_VERSION, TaskId, WorktreeOrigin};
use alondra_engine_ports::worktrees::{LaunchError, Protected, TaskWorktree, Workplace, Worktrees, CHECK_LIMIT};
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::future::Future as _;
use std::io;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A missing path has no space; a real folder has sane space.
pub fn disk(disk: &dyn Disk) {
    let scratch = Directory::new();
    std::fs::create_dir_all(&scratch).unwrap();
    assert_eq!(disk.space(&scratch.join("missing")), None, "a missing path has no space");
    let space = disk.space(&scratch).expect("a real folder is on a volume");
    assert!(space.total > 0 && space.free <= space.total && space.usable() >= space.free, "{space:?}");
}

/// A sample of the Mac holds gauges that fit together: memory within what it has, the own disk's
/// free space within its size, a percentage under 100, and the cores and memory its size says.
pub fn machine(probe: &dyn MachineProbe) {
    let before = alondra_core::now_millis();
    let sample = probe.sample();
    assert!(sample.at >= before, "a sample is taken now: {} before {before}", sample.at);
    assert!(sample.memory_total > 0 && sample.memory_available <= sample.memory_total, "{sample:?}");
    assert!(sample.memory_free_percent.is_none_or(|free| free <= 100), "{sample:?}");
    assert!(sample.disk_total > 0 && sample.disk_available <= sample.disk_total, "the Mac's own disk is measured: {sample:?}");
    assert!(sample.load.iter().all(|load| load.is_finite() && *load >= 0.0), "{sample:?}");
    let (cores, memory) = probe.size();
    assert!(cores > 0, "a Mac has cores");
    assert_eq!(memory, sample.memory_total, "the size names the memory the samples measure");
    assert_eq!(probe.size(), (cores, memory), "a Mac's size stays put");
}

fn ready<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(future)
}

/// A task worktree made in a repository's main checkout, reused, protected, told apart from the
/// user's folders, kept while it holds work not saved and deleted once clean. `repository` makes
/// the folder it is given a repository's main checkout, with the files in it committed.
pub fn worktrees(worktrees: &dyn Worktrees, repository: &dyn Fn(&Path)) {
    let scratch = Directory::new();
    let plain = scratch.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "work\n").unwrap();
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    std::fs::write(repo.join("src/lib.txt"), "code\n").unwrap();
    repository(&repo);
    let repo = repo.canonicalize().unwrap();

    assert_eq!(ready(worktrees.isolate(&plain, "t", None, None, None)).unwrap(), Workplace::Given(plain.clone()), "a folder outside git is used as it is");
    let made = ready(worktrees.isolate(&repo, "t", None, None, None)).unwrap();
    let Workplace::Made(dir) = &made else { panic!("a main checkout gets a worktree of its own: {made:?}") };
    let dir = dir.canonicalize().unwrap();
    assert_eq!(dir, repo.join(".claude/worktrees/t"));
    assert!(dir.join("README.md").is_file(), "a worktree holds the repository's files");
    assert_eq!(worktrees.chosen(&made).map(|chosen| chosen.canonicalize().unwrap()), Some(dir.clone()));
    assert_eq!(worktrees.chosen(&Workplace::Given(plain.clone())), None);
    let again = ready(worktrees.isolate(&repo, "t", None, None, None)).unwrap();
    assert!(matches!(&again, Workplace::Reused(reused) if reused.canonicalize().unwrap() == dir), "{again:?}");
    assert_eq!(ready(worktrees.branch_in(&dir)).as_deref(), Some("worktree-t"), "a task's worktree works on a branch of its own");
    let admin = std::fs::read_to_string(dir.join(".git")).unwrap();
    let admin = Path::new(admin.trim().strip_prefix("gitdir: ").expect("a linked worktree's .git file names its folder in the repository")).to_path_buf();
    assert!(admin.starts_with(repo.join(".git/worktrees")) && admin.is_dir(), "{}", admin.display());
    assert!(ready(worktrees.recovery_directory_matches(&dir, &dir)));
    assert!(!ready(worktrees.recovery_directory_matches(&dir, &plain)));
    assert!(ready(worktrees.recovery_directory_matches(&repo, &dir)), "a node of the worktree is recovered from its repository's main checkout");
    assert!(ready(worktrees.recovery_directory_matches(&repo.join("src"), &dir.join("src"))), "at the same folder inside it");
    assert!(!ready(worktrees.recovery_directory_matches(&repo, &dir.join("src"))), "never another folder");
    assert!(!ready(worktrees.recovery_directory_matches(&repo, &repo)), "a main checkout is never a node's to recover");
    assert!(!ready(worktrees.recovery_directory_matches(&plain, &dir)));
    assert!(ready(worktrees.recovery_directory_matches(&plain, &plain)), "a folder outside git is recovered where it was");

    let elsewhere = Protected { home: scratch.join("home"), projects_root: scratch.join("home/Projects") };
    assert_eq!(worktrees.registrable(&dir, &elsewhere), Ok(()));
    assert!(worktrees.registrable(Path::new("relative/t"), &elsewhere).is_err(), "a relative path is never a worktree");
    assert!(worktrees.registrable(&plain, &elsewhere).is_err(), "a folder outside git is never a worktree");
    assert!(worktrees.registrable(&dir.join("src"), &elsewhere).is_err(), "only the top of a worktree is one");
    let mine = Protected { home: dir.clone(), projects_root: scratch.join("projects") };
    assert!(worktrees.registrable(&dir, &mine).is_err(), "the user's home is never a task's worktree");
    assert_eq!(worktrees.subagent_worktree(&dir, "a1"), Some(repo.join(".claude/worktrees/agent-a1")));

    let mut asked = worktrees.unsaved(&dir);
    let waited = ready(async move { asked.as_mut().poll(&mut std::task::Context::from_waker(std::task::Waker::noop())).is_pending() });
    assert!(waited, "git answers in a later poll than the one that asked");
    assert_eq!(ready(worktrees.unsaved(&dir)).unwrap(), None, "a worktree just made holds nothing unsaved");
    assert_eq!(ready(worktrees.release_task_worktree(&dir, "other", true)).unwrap(), TaskWorktree::NotAlondras, "only the worktree of the task's own name");
    assert_eq!(ready(worktrees.release_task_worktree(&plain, "plain", true)).unwrap(), TaskWorktree::NotAlondras, "never the user's folder");
    assert_eq!(worktrees.summary(&plain), format!("{} (not a readable git worktree)", plain.display()), "a folder outside git says so");
    let clean = worktrees.summary(&dir);
    std::fs::write(dir.join("notes.txt"), "not committed\n").unwrap();
    assert_ne!(worktrees.summary(&dir), clean, "what is not committed shows in the summary");
    assert!(ready(worktrees.unsaved(&dir)).unwrap().is_some(), "a new file is work not saved");
    assert_eq!(ready(worktrees.uncommitted_files_of_alondras_worktree(&dir)).unwrap(), Some(1));
    assert!(matches!(ready(worktrees.release_task_worktree(&dir, "t", true)).unwrap(), TaskWorktree::Unsaved(_)), "work not saved keeps it");
    assert!(dir.is_dir());
    std::fs::remove_file(dir.join("notes.txt")).unwrap();
    assert!(matches!(ready(worktrees.release_task_worktree(&dir, "t", false)).unwrap(), TaskWorktree::Removed { .. }), "a dry run says it would go");
    assert!(dir.is_dir(), "a dry run deletes nothing");
    assert!(matches!(ready(worktrees.release_task_worktree(&dir, "t", true)).unwrap(), TaskWorktree::Removed { .. }));
    assert!(!dir.exists());
    assert!(!admin.exists(), "the repository forgets a worktree that went");
    assert_eq!(ready(worktrees.release_task_worktree(&dir, "t", true)).unwrap(), TaskWorktree::Absent);

    let kept = ready(worktrees.isolate(&repo, "kept", None, None, None)).unwrap();
    let kept = kept.dir().to_path_buf();
    std::fs::write(kept.join("draft.txt"), "draft\n").unwrap();
    assert!(matches!(ready(worktrees.release_recorded_worktree(&kept, true)).unwrap(), TaskWorktree::Removed { .. }), "one given up goes as it is");
    assert!(!kept.exists());

    let shells = repo.join(".claude/worktrees");
    std::fs::create_dir_all(&shells).unwrap();
    let hollow = scratch.join("hollow");
    std::fs::create_dir_all(&hollow).unwrap();
    let hollow_link = shells.join("hollow");
    std::os::unix::fs::symlink(&hollow, &hollow_link).unwrap();
    assert!(worktrees.is_empty_shell(&hollow_link), "a link to a folder with nothing in it is what git leaves after removing a worktree");
    assert!(matches!(ready(worktrees.release_recorded_worktree(&hollow_link, false)).unwrap(), TaskWorktree::Removed { .. }), "nothing in it can be lost");
    assert!(std::fs::symlink_metadata(&hollow_link).is_err() && !hollow.exists(), "the link and the folder it points to go");
    let emptied = shells.join("emptied");
    std::fs::create_dir(&emptied).unwrap();
    assert!(matches!(ready(worktrees.release_recorded_worktree(&emptied, false)).unwrap(), TaskWorktree::Removed { .. }));
    assert!(!emptied.exists());
    let held = shells.join("held");
    std::fs::create_dir(&held).unwrap();
    std::fs::write(held.join("notes.txt"), "mine\n").unwrap();
    assert!(!worktrees.is_empty_shell(&held));
    assert_eq!(ready(worktrees.release_recorded_worktree(&held, true)).unwrap(), TaskWorktree::NotAlondras, "a folder with anything in it that is no worktree is never deleted");
    assert!(held.join("notes.txt").is_file());

    let root = worktrees.capture_root(&plain).expect("an existing folder can hold worktrees");
    assert_eq!(root.path, plain.canonicalize().unwrap());
    assert!(worktrees.capture_root(&scratch.join("missing")).is_err());

    let made = ready(worktrees.isolate(&repo.join("src"), "sub", None, None, Some(&root))).unwrap();
    let top = root.path.join("repo/sub");
    assert_eq!(made.dir().canonicalize().unwrap(), top.join("src"), "a launch in a folder of the checkout works in the same folder of its worktree");
    assert_eq!(worktrees.chosen(&made).map(|chosen| chosen.canonicalize().unwrap()), Some(top.clone()), "the whole worktree is the node's");
    let record = worktrees.stored(&top).expect("a worktree in a worktrees folder keeps the record of its repository");
    assert_eq!(record.repository.canonicalize().unwrap(), repo);
    let (reopened, recovered) = ready(worktrees.reopen(&repo.join("src"), "sub", &top.join("src"), None, Some(&root))).unwrap();
    assert!(recovered && reopened.dir().canonicalize().unwrap() == top.join("src"), "{reopened:?} {recovered}");
    assert!(matches!(ready(worktrees.discard_worktree(&top, true)).unwrap(), TaskWorktree::Removed { .. }));
    assert!(!top.exists());
    worktrees.restore(&top, &record).unwrap();
    let (reopened, recovered) = ready(worktrees.reopen(&top, "sub", &top, None, None)).unwrap();
    assert!(recovered && reopened.dir() == top, "a worktree whose branch stayed comes back where it was: {reopened:?} {recovered}");
    assert!(top.join("src/lib.txt").is_file());

    let linked = ready(worktrees.isolate(&repo, "away", None, None, None)).unwrap().dir().to_path_buf();
    let volume = scratch.join("volume");
    std::fs::create_dir_all(&volume).unwrap();
    let moved = volume.canonicalize().unwrap().join("away");
    std::fs::rename(&linked, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &linked).unwrap();
    std::fs::rename(&volume, scratch.join("unmounted")).unwrap();
    assert!(worktrees.unreachable_link(&linked).is_some());
    let kept = ready(worktrees.release_task_worktree(&linked, "away", true)).unwrap();
    assert!(matches!(&kept, TaskWorktree::Unavailable(_)), "a link whose volume is away stays: {kept:?}");
    std::fs::rename(scratch.join("unmounted"), &volume).unwrap();
    assert_eq!(worktrees.unreachable_link(&linked), None);
    assert!(matches!(ready(worktrees.release_task_worktree(&linked, "away", true)).unwrap(), TaskWorktree::Removed { .. }), "back, it goes with its link");
    assert!(std::fs::symlink_metadata(&linked).is_err() && !moved.exists());

    let unreadable = scratch.join("unreadable");
    std::fs::create_dir_all(unreadable.join(".git")).unwrap();
    let failed = ready(worktrees.isolate(&unreadable, "t", None, None, None));
    assert!(matches!(failed, Err(LaunchError::Workspace(_))), "a main checkout git cannot read gets no node: {failed:?}");

    let unnamed = scratch.join("App\n");
    std::fs::create_dir_all(&unnamed).unwrap();
    repository(&unnamed);
    assert!(!matches!(ready(worktrees.storage_for(&unnamed, "t", None, None, Some(&root))), Err(LaunchError::Storage(_))), "git failing is never the volume");
    for root in [None, Some(&root)] {
        let failed = ready(worktrees.isolate(&unnamed, "t", None, None, root));
        assert!(matches!(failed, Err(LaunchError::Workspace(_))), "a checkout git cannot name is a failure of the launch: {failed:?}");
    }

}

/// The checks of a final delivery in the worktree a task recorded as its own: clean passes, a file
/// not committed is refused, and a check slower than its limit is refused as not finished, never
/// taken as clean. `slow` makes every check in the worktree it is given take that long; `rules`
/// apply the commands that give the task its worktree.
pub fn final_checks(worktrees: &dyn Worktrees, repository: &dyn Fn(&Path), slow: &dyn Fn(&Path, Duration), rules: &dyn DomainRules) {
    let scratch = Directory::new();
    let repo = scratch.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("README.md"), "work\n").unwrap();
    repository(&repo);
    let dir = ready(worktrees.isolate(&repo, "t", None, None, None)).unwrap().dir().to_path_buf();
    let mut state = DomainState::new("e");
    let mut run = |actor: Actor, command: Command| {
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
            command_id: format!("c-{}", state.revision).into(), expected_revision: state.revision, command };
        state = rules.transition(&state, &actor, &envelope).unwrap().0;
    };
    run(Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { priority: Default::default(), plan_items: None, continues: None, task_id: "t".into(), request_id: "r".into(),
        title: "Task".into(), agent_id: None, review_prs: false, dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")) });
    run(Actor::System, Command::RecordTaskWorktree { task_id: "t".into(), path: dir.to_str().unwrap().into(), origin: WorktreeOrigin::Task, branch: None });
    let task = TaskId::from("t");
    let secret = |file: &str| worktrees.secret_refusal(&state, "sdk-0123abcd", &task, &SecretDestination { file: file.into(), variable: "KEY".into() });
    std::fs::create_dir_all(repo.join(".git/info")).unwrap();
    std::fs::write(repo.join(".git/info/exclude"), "local.env\n").unwrap();
    assert!(secret("local.env").is_none(), "a file the repository excludes can take a secret");
    assert!(secret("config.env").is_some_and(|refusal| refusal.subject.as_deref() == Some("destination")), "a file git keeps is refused");
    let check = |limit| ready(worktrees.final_refusal(&state, "sdk-0123abcd", &task, &[], limit)).map(|refusal| refusal.code);
    assert_eq!(check(CHECK_LIMIT), None, "a clean worktree passes");
    std::fs::write(dir.join("notes.txt"), "not committed\n").unwrap();
    assert_eq!(check(CHECK_LIMIT).as_deref(), Some("worktree_not_clean"));
    std::fs::remove_file(dir.join("notes.txt")).unwrap();
    slow(&dir, Duration::from_secs(2));
    assert_eq!(check(Duration::from_millis(100)).as_deref(), Some("worktree_check_timed_out"), "a check that did not finish is never taken as clean");
    assert_eq!(check(CHECK_LIMIT), None, "the same worktree passes once the check has the time it needs");
}

/// How a fixture of each implementation makes and saves work: `task_worktree` makes the worktree
/// of a task under a folder, in a repository with a remote, everything in it pushed.
pub struct Saving<'a> {
    pub task_worktree: &'a dyn Fn(&Path, &str) -> std::path::PathBuf,
    pub local_commit: &'a dyn Fn(&Path),
    pub push: &'a dyn Fn(&Path),
    pub branch_exists: &'a dyn Fn(&Path, &str) -> bool,
}

/// A commit the remote does not have is work not saved, kept until it is pushed; a worktree
/// discarded with its branch takes the branch only when nothing in it is lost.
pub fn saved_work(worktrees: &dyn Worktrees, saving: &Saving) {
    let scratch = Directory::new();
    let dir = (saving.task_worktree)(&scratch, "t");
    let repo = scratch.join("repo");
    assert_eq!(ready(worktrees.unsaved(&dir)).unwrap(), None, "a pushed worktree holds nothing unsaved");
    (saving.local_commit)(&dir);
    assert!(ready(worktrees.unsaved(&dir)).unwrap().is_some(), "a commit no remote has is unsaved");
    assert!(matches!(ready(worktrees.release_recorded_worktree(&dir, false)).unwrap(), TaskWorktree::Unsaved(_)));
    assert!(matches!(ready(worktrees.discard_worktree(&dir, false)).unwrap(), TaskWorktree::Removed { .. }), "one given up goes as it is");
    assert!((saving.branch_exists)(&repo, "worktree-t"), "a branch with commits no remote has stays");

    let pushed = repo.join(".claude/worktrees/u");
    let made = ready(worktrees.isolate(&repo, "u", None, None, None)).unwrap();
    assert_eq!(made.dir().canonicalize().unwrap(), pushed.canonicalize().unwrap());
    (saving.local_commit)(&pushed);
    (saving.push)(&pushed);
    assert_eq!(ready(worktrees.unsaved(&pushed)).unwrap(), None, "pushed work is saved");
    assert!(matches!(ready(worktrees.discard_worktree(&pushed, false)).unwrap(), TaskWorktree::Removed { .. }));
    assert!(!(saving.branch_exists)(&repo, "worktree-u"), "a branch whose commits a remote has goes with its worktree");
}

/// How a contract test makes a task's worktree in a repository with no remote, commits in it, and
/// reads whether a branch is still there.
pub struct Finishing<'a> {
    pub remoteless_worktree: &'a dyn Fn(&Path, &str) -> PathBuf,
    pub local_commit: &'a dyn Fn(&Path),
    pub branch_exists: &'a dyn Fn(&Path, &str) -> bool,
}

/// In a repository with no remote, a finished task's branch with commits of its own stays even when
/// a pull request of the task merged, and one with no commit of its own goes.
pub fn finished_branches(worktrees: &dyn Worktrees, finishing: &Finishing) {
    let scratch = Directory::new();
    let dir = (finishing.remoteless_worktree)(&scratch, "t");
    (finishing.local_commit)(&dir);
    assert!(matches!(ready(worktrees.release_finished_worktree(&dir, None, false, true)).unwrap(), TaskWorktree::Removed { .. }),
        "with no remote to push to, nothing in the worktree is unsaved");
    assert!((finishing.branch_exists)(&scratch.join("repo"), "worktree-t"), "its commits exist only in its branch, whatever a merged pull request says");

    let other = Directory::new();
    let empty = (finishing.remoteless_worktree)(&other, "u");
    assert!(matches!(ready(worktrees.release_finished_worktree(&empty, None, false, true)).unwrap(), TaskWorktree::Removed { .. }));
    assert!(!(finishing.branch_exists)(&other.join("repo"), "worktree-u"), "a branch the default branch holds goes");
}

/// An added Claude account's folder is private and links what the user set up; removing it keeps
/// what it linked. A named Codex account gets a private folder of its own, the native one none.
/// Two accounts share sessions only through the same `projects/`. A repository nothing trusts is
/// untrusted, a projects folder trusts what is in it on the spot, and the user's trust holds on
/// every account. `trust` writes the settings of a profile: which repositories it trusts.
pub fn accounts(accounts: &dyn alondra_engine_ports::accounts::Accounts, trust: &dyn Fn(&Path, &[(&Path, bool)])) {
    use alondra_core::accounts::{AccountId, AccountKey, Provider};
    use std::os::unix::fs::PermissionsExt as _;
    let scratch = Directory::new();
    let user_home = scratch.join("user");
    let claude = user_home.join(".claude");
    std::fs::create_dir_all(claude.join("projects")).unwrap();
    std::fs::create_dir_all(claude.join("skills/mine")).unwrap();
    std::fs::write(claude.join("CLAUDE.md"), "be brief").unwrap();
    let home = user_home.join(".alondra");
    let work = AccountId::parse("work").unwrap();
    let folder = accounts.prepare_claude(&home, &work, &claude).unwrap();
    assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700, "an account's folder is private");
    assert_eq!(std::fs::read_link(folder.join("skills")).unwrap(), claude.join("skills"));
    assert_eq!(std::fs::read_link(folder.join("CLAUDE.md")).unwrap(), claude.join("CLAUDE.md"));
    assert!(std::fs::symlink_metadata(folder.join("agents")).is_err(), "a name the user has not is not linked");
    assert!(accounts.prepare_claude(&home, &AccountId::default_account(), &claude).is_err(), "the default account is the user's own");
    assert!(accounts.shares_sessions(&home, &user_home, None, &AccountId::default_account(), &work));
    assert!(!accounts.shares_sessions(&home, &user_home, None, &work, &AccountId::parse("none").unwrap()), "an account with no folder");
    accounts.remove_claude(&home, &work).unwrap();
    assert!(!folder.exists());
    assert!(claude.join("skills/mine").is_dir() && claude.join("CLAUDE.md").is_file(), "removing an account keeps what it linked");
    accounts.remove_claude(&home, &work).unwrap();

    let codex = AccountKey::new(Provider::Codex, AccountId::parse("projects").unwrap());
    let profile = accounts.prepare_codex(&home, &codex).unwrap().expect("a named Codex account has a folder");
    assert_eq!(std::fs::metadata(&profile).unwrap().permissions().mode() & 0o777, 0o700);
    assert!(std::fs::read_to_string(profile.join("config.toml")).unwrap().contains("cli_auth_credentials_store = \"file\""));
    assert_eq!(accounts.prepare_codex(&home, &AccountKey::native(Provider::Codex)).unwrap(), None, "the native account has no folder of Alondra's");
    assert!(accounts.prepare_codex(&home, &AccountKey::new(Provider::Claude, work.clone())).is_err());
    accounts.remove_codex(&home, &codex).unwrap();
    assert!(!profile.exists());

    antigravity_keys(accounts, &home, &user_home);

    let projects = scratch.join("projects");
    let repository = projects.join("Lumo");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    std::fs::create_dir_all(repository.join("src")).unwrap();
    let repository = repository.canonicalize().unwrap();
    let account = scratch.join("accounts/work");
    std::fs::create_dir_all(&account).unwrap();
    assert_eq!(accounts.untrusted(Some(&account), &repository), None, "settings that cannot be read prove nothing");
    trust(&account, &[]);
    assert_eq!(accounts.untrusted(Some(&account), &repository.join("src")), Some(repository.clone()), "trust is kept under the repository's root");
    assert_eq!(accounts.ensure_trusted(Some(&account), &repository, Some(&scratch.join("elsewhere"))), Some(repository.clone()));
    assert_eq!(accounts.ensure_trusted(Some(&account), &repository, Some(&projects)), None, "inside the projects folder it is trusted on the spot");
    assert_eq!(accounts.untrusted(Some(&account), &repository), None);
    let fjord = projects.join("Fjord");
    std::fs::create_dir_all(fjord.join(".git")).unwrap();
    let fjord = fjord.canonicalize().unwrap();
    trust(&account, &[(&fjord, false)]);
    assert_eq!(accounts.untrusted(Some(&account), &fjord), Some(fjord.clone()));
    accounts.grant_trust(&fjord, std::slice::from_ref(&account)).unwrap();
    assert_eq!(accounts.untrusted(Some(&account), &fjord), None, "the user's trust holds on every account");
}

/// A named Antigravity account's key: written whole to a `0600` file of its private folder and
/// read back by the rule the Antigravity host reads it with (`fixtures/antigravity-credentials.json`
/// of `crates/core`, which the host's tests read too). A file that rule refuses is no key, and a
/// folder others may open keeps none.
fn antigravity_keys(accounts: &dyn alondra_engine_ports::accounts::Accounts, home: &Path, user_home: &Path) {
    use alondra_core::accounts::{AccountId, AccountKey, Provider};
    use alondra_core::antigravity_account::{KEY_FILE, Secret};
    use std::os::unix::fs::PermissionsExt as _;
    const RULES: &str = include_str!("../../core/fixtures/antigravity-credentials.json");
    let gemini = user_home.join(".gemini");
    std::fs::create_dir_all(&gemini).unwrap();
    let studio = AccountKey::new(Provider::Antigravity, AccountId::parse("studio").unwrap());
    let folder = accounts.prepare_antigravity(home, &studio, &gemini).unwrap().expect("a named account has a folder");
    assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
    let file = folder.join(KEY_FILE);
    accounts.set_antigravity_key(home, &studio, Some(&Secret::new("AIza-contract-key".into()))).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "AIza-contract-key");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600, "the key is the owner's alone");
    assert_eq!(std::fs::read_dir(&folder).unwrap().count(), 1, "no copy of the key is left beside it");
    assert_eq!(accounts.antigravity_key(home, &studio).unwrap().as_ref().map(Secret::expose), Some("AIza-contract-key"));
    assert!(accounts.antigravity_key_set(home, &studio));
    assert!(accounts.set_antigravity_key(home, &studio, Some(&Secret::new("AIza with a space".into()))).is_err(), "only a key is kept");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "AIza-contract-key", "a refused key leaves the old one");

    let rules: serde_json::Value = serde_json::from_str(RULES).unwrap();
    let mut written = vec![file.clone()];
    for kind in rules["key_files"].as_array().unwrap() {
        let name = kind["name"].as_str().unwrap();
        for path in written.drain(1..).chain([file.clone()]) {
            if let Err(error) = std::fs::remove_file(&path) { assert_eq!(error.kind(), std::io::ErrorKind::NotFound, "{name}"); }
        }
        let bytes = kind["hex"].as_str().map_or_else(|| kind["text"].as_str().unwrap().as_bytes().to_vec(),
            |hex| (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap()).collect());
        std::fs::write(&file, bytes).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(u32::from_str_radix(kind["mode"].as_str().unwrap(), 8).unwrap())).unwrap();
        for link in 1..kind["links"].as_u64().unwrap_or(1) {
            let path = folder.join(format!("{KEY_FILE}.link-{link}"));
            std::fs::hard_link(&file, &path).unwrap();
            written.push(path);
        }
        let read = accounts.antigravity_key(home, &studio);
        match kind["reads"].as_str().unwrap() {
            "key" => assert_eq!(read.unwrap().as_ref().map(Secret::expose), kind["key"].as_str(), "{name}"),
            "none" => assert!(matches!(read, Ok(None)), "{name}"),
            _ => assert!(read.is_err(), "{name}"),
        }
        assert_eq!(accounts.antigravity_key_set(home, &studio), kind["reads"] == "key", "{name}: the screens say Key set only for a key it reads");
    }
    for path in written { if let Err(error) = std::fs::remove_file(&path) { assert_eq!(error.kind(), std::io::ErrorKind::NotFound); } }

    accounts.set_antigravity_key(home, &studio, Some(&Secret::new("AIza-contract-key".into()))).unwrap();
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(accounts.antigravity_key(home, &studio).is_err(), "a folder others may open keeps no key");
    assert!(!accounts.antigravity_key_set(home, &studio));
    assert!(accounts.set_antigravity_key(home, &studio, Some(&Secret::new("AIza-other-key".into()))).is_err(), "nor takes one");
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "AIza-contract-key");
    accounts.set_antigravity_key(home, &studio, None).unwrap();
    assert!(!file.exists() && !accounts.antigravity_key_set(home, &studio), "removing the key removes its file");
    accounts.set_antigravity_key(home, &studio, None).unwrap();
    let native = AccountKey::native(Provider::Antigravity);
    assert!(accounts.set_antigravity_key(home, &native, Some(&Secret::new("AIza-contract-key".into()))).is_err(), "the default account is agy's own sign-in");
    assert!(matches!(accounts.antigravity_key(home, &native), Ok(None)));
    accounts.remove_antigravity(home, &studio).unwrap();
    assert!(!folder.exists());
}

/// What the Mac answers of its processes: a table with this process in it, under its parent.
pub fn processes(processes: &dyn alondra_engine_ports::processes::Processes) {
    let table = processes.table();
    let own = table.iter().find(|process| process.pid == std::process::id() as i32).expect("this process is in the table");
    assert!(table.iter().any(|process| process.pid == own.parent), "its parent too");
    processes.signal(&[], libc::SIGTERM);
}

/// Alondra's plugins: the copy a home holds, and the one the registered marketplace points to.
pub fn plugins(plugins: &dyn alondra_engine_ports::plugins::Plugins) {
    use alondra_engine_ports::plugins::{NODE, ORCHESTRATOR};
    let scratch = Directory::new();
    let home = scratch.join("home");
    std::fs::create_dir_all(home.join(NODE)).unwrap();
    assert!(plugins.node(&home).is_some(), "the copy in the home, or the one this daemon installed");
    assert_eq!(plugins.orchestrator(&home), None, "a copy without its manifest is not a plugin");
    std::fs::create_dir_all(home.join(ORCHESTRATOR).join(".claude-plugin")).unwrap();
    std::fs::write(home.join(ORCHESTRATOR).join(".claude-plugin/plugin.json"), "{}").unwrap();
    assert_eq!(plugins.orchestrator(&home), Some(home.join(ORCHESTRATOR)));
    let claude_home = scratch.join("claude");
    let marketplace = scratch.join("marketplace");
    std::fs::create_dir_all(marketplace.join(NODE).join(".claude-plugin")).unwrap();
    std::fs::write(marketplace.join(NODE).join(".claude-plugin/plugin.json"), "{}").unwrap();
    assert_eq!(plugins.registered(&claude_home, NODE), None, "nothing registered");
    std::fs::create_dir_all(claude_home.join("plugins")).unwrap();
    std::fs::write(claude_home.join("plugins/known_marketplaces.json"), serde_json::json!({"alondra": {"installLocation": marketplace}}).to_string()).unwrap();
    assert_eq!(plugins.registered(&claude_home, NODE), Some(marketplace.join(NODE)));
    assert_eq!(plugins.registered(&claude_home, ORCHESTRATOR), None);
}

/// How an implementation of `AgentHost` has a host to run, and how the contract speaks to the one
/// it reached.
pub trait HostFixture {
    /// The executable and the artifact a launch runs.
    fn artifact(&self) -> VerifiedArtifact;
    /// The environment of the host launched in `state_dir`.
    fn environment(&self, state_dir: &Path) -> BTreeMap<String, String>;
    /// The process id the host at the other end of `stream` answers with.
    fn process_id(&self, stream: UnixStream) -> u32;
    /// Tells the host at the other end of `stream` to end by itself, with success.
    fn exit(&self, stream: UnixStream);
}

/// One folder and the hosts the contract runs in it, one after another. Whatever it launched is
/// ended when it goes, also when an assertion failed with a host still running.
struct Hosted<'a> {
    hosts: &'a dyn AgentHost,
    fixture: &'a dyn HostFixture,
    state: PathBuf,
    launched: RefCell<Vec<HostStop>>,
}

impl Drop for Hosted<'_> {
    fn drop(&mut self) {
        for host in self.launched.borrow().iter() {
            let _ = host.kill();
        }
    }
}

impl Hosted<'_> {
    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(10)
    }

    fn make_folder(&self) {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new().mode(0o700).create(&self.state).unwrap();
    }

    fn start(&self, build: &mut dyn FnMut(&LaunchIdentity) -> io::Result<Vec<u8>>) -> io::Result<Box<dyn LaunchedHost>> {
        let environment = self.fixture.environment(&self.state);
        let launch = HostLaunch { artifact: self.fixture.artifact(), state_dir: &self.state, environment: &environment, confinement: None };
        let host = self.hosts.launch(launch, build)?;
        self.launched.borrow_mut().push(host.stopper());
        Ok(host)
    }

    /// A host launched in the folder with `bootstrap`, built once for that folder, and reached.
    fn launch(&self, bootstrap: &[u8]) -> Box<dyn LaunchedHost> {
        let mut built = Vec::new();
        let mut host = self
            .start(&mut |identity| {
                built.push(identity.state_dir().to_path_buf());
                Ok(bootstrap.to_vec())
            })
            .unwrap_or_else(|error| panic!("a free folder takes a host: {error}"));
        assert_eq!(built, [self.state.clone()], "a launch builds its bootstrap once, for its folder");
        assert_eq!(host.identity().state_dir(), self.state);
        assert_eq!(host.observed_exit().unwrap(), None, "a host just launched runs");
        let stream = host.wait_for_socket(Self::soon()).unwrap();
        assert_eq!(self.fixture.process_id(stream), host.process_id(), "its socket reaches the host launched");
        assert!(matches!(self.hosts.claim(&self.state).unwrap(), Retained::Held), "a live host holds its folder");
        host
    }

    /// Why a launch in the folder is refused, before any bootstrap is built.
    fn refusal(&self) -> io::ErrorKind {
        refused(self.start(&mut |_| panic!("a refused launch builds no bootstrap")))
    }

    /// The endpoint of the host in the folder and the bootstrap its adoption inspected, once.
    fn adopt(&self) -> (Box<dyn AdoptedEndpoint>, Vec<u8>) {
        let mut inspected = Vec::new();
        let endpoint = self
            .hosts
            .adopt(&self.state, &mut |folder, bootstrap| {
                assert_eq!(folder.state_dir(), self.state);
                inspected.push(bootstrap.to_vec());
                Ok(())
            })
            .unwrap_or_else(|error| panic!("a folder that retains a bootstrap is adopted: {error}"));
        assert_eq!(inspected.len(), 1, "an adoption inspects the bootstrap once");
        assert_eq!(endpoint.identity().state_dir(), self.state);
        (endpoint, inspected.remove(0))
    }

    /// The claim of the folder once its host ended, with the bootstrap it ran with.
    fn ended(&self, bootstrap: &[u8]) -> EndedHost {
        let Retained::Ended(ended) = self.hosts.claim(&self.state).unwrap() else { panic!("a host that ended leaves its folder to claim") };
        assert_eq!(ended.bootstrap(), bootstrap, "the claim holds the bootstrap the host ran with");
        assert_eq!(ended.state_dir(), self.state);
        ended
    }

    /// Retires the ended host as `name`, and makes the folder again for the next one.
    fn retire(&self, ended: EndedHost, name: &str) {
        let archive = self.state.with_file_name(name);
        let bootstrap = ended.bootstrap().to_vec();
        ended.retire(&archive).unwrap();
        assert!(matches!(self.hosts.claim(&self.state).unwrap(), Retained::Absent), "retiring frees the folder");
        assert_eq!(std::fs::read(HostFolder::at(archive).ownership_path()).unwrap(), bootstrap, "the ended host is kept as evidence");
        self.make_folder();
    }
}

fn refused<T>(result: io::Result<T>) -> io::ErrorKind {
    match result {
        Ok(_) => panic!("nothing was refused"),
        Err(error) => error.kind(),
    }
}

fn assert_running(host: &HostStop, process: u32) {
    let group = i32::try_from(process).expect("a process id fits a process group number");
    assert!(!host.gone().unwrap(), "a running host is not gone");
    assert_eq!(host.process_group(), Some(group), "a host leads the group its process id names");
    assert_eq!(host.process().unwrap().map(|host| host.pid), Some(group));
    assert!(host.tree().unwrap().iter().all(|under| under.pid != group), "a host is not one of the processes under it");
    assert!(host.footprint().unwrap().is_some(), "a running host's memory is measured");
}

fn assert_gone(host: &HostStop) {
    assert!(host.gone().unwrap(), "a host that ended is gone");
    assert_eq!(host.process().unwrap(), None);
    assert_eq!(host.tree().unwrap(), vec![], "nothing is under a host that ended");
    assert_eq!(host.native().unwrap(), None, "a host that ended runs no native agent");
}

/// A host's life in its folder as the engine relies on it: launched once with the bootstrap built
/// for it, held while it runs, adopted with that bootstrap and reached, stopped or ended and its
/// exit observed, claimed once ended, retired to free the folder for the next one. The handle that
/// stops a host answers for its processes the same way before and after it ends.
pub fn agent_host(hosts: &dyn AgentHost, fixture: &dyn HostFixture) {
    use std::os::unix::fs::DirBuilderExt as _;
    use std::os::unix::process::ExitStatusExt as _;
    let scratch = Directory::for_unix_socket();
    std::fs::DirBuilder::new().mode(0o700).create(&scratch).unwrap();
    let room = Hosted { hosts, fixture, state: scratch.join("host"), launched: RefCell::default() };
    let state = &room.state;
    let nothing_to_adopt = || refused(hosts.adopt(state, &mut |_, _| panic!("nothing to inspect where no host ran")));

    assert!(matches!(hosts.claim(state).unwrap(), Retained::Absent), "no folder, no host");
    assert_eq!(room.refusal(), io::ErrorKind::NotFound, "a host is launched in a folder that exists");
    assert_eq!(nothing_to_adopt(), io::ErrorKind::NotFound);
    assert_eq!(refused(hosts.end_holders(state)), io::ErrorKind::NotFound);
    room.make_folder();
    assert!(matches!(hosts.claim(state).unwrap(), Retained::Absent), "a folder no host wrote its bootstrap in");
    assert_eq!(nothing_to_adopt(), io::ErrorKind::NotFound);
    assert_eq!(refused(hosts.end_holders(state)), io::ErrorKind::NotFound);

    assert_eq!(refused(room.start(&mut |_| Err(io::ErrorKind::InvalidData.into()))), io::ErrorKind::InvalidData, "a launch fails as the build of its bootstrap did");
    assert_eq!(refused(room.start(&mut |_| Ok(vec![]))), io::ErrorKind::PermissionDenied, "an empty bootstrap starts no host");
    assert!(matches!(hosts.claim(state).unwrap(), Retained::Absent), "a launch that failed leaves nothing to claim");

    let mut first = room.launch(b"first bootstrap");
    let process = first.process_id();
    let stop = first.stopper();
    assert_running(&stop, process);
    assert_eq!(room.refusal(), io::ErrorKind::Other, "a live host's folder takes no other");
    let (adopted, bootstrap) = room.adopt();
    assert_eq!(bootstrap, b"first bootstrap", "an adoption reads the bootstrap the launch retained, byte for byte");
    let reached = adopted.stopper();
    assert_eq!(reached.process_group(), None, "an adopted host is not known before it is reached");
    assert!(!reached.gone().unwrap(), "and is taken as running");
    assert_eq!(fixture.process_id(adopted.connect(Hosted::soon()).unwrap()), process, "an adoption reaches the host that runs there");
    assert_running(&reached, process);
    assert_eq!(reached.process().unwrap(), stop.process().unwrap(), "both handles name the same process");
    assert_eq!(refused(hosts.adopt(state, &mut |_, _| Err(io::ErrorKind::InvalidData.into()))), io::ErrorKind::InvalidData, "an adoption fails as its inspection did");

    stop.request().unwrap();
    let status = first.wait().unwrap();
    assert_eq!(first.observed_exit().unwrap(), Some(status), "the exit waited for is the exit observed");
    assert_eq!(first.wait().unwrap(), status, "waiting again answers the same");
    assert_gone(&stop);
    assert_eq!(stop.process_group(), None);
    assert_eq!(stop.footprint().unwrap(), None);
    stop.request().unwrap();
    stop.kill().unwrap();
    assert!(stop.request_graceful_stop(Duration::ZERO).unwrap(), "a host that ended has stopped");
    assert_gone(&reached);
    assert_eq!(refused(first.wait_for_socket(Hosted::soon())), io::ErrorKind::ConnectionAborted, "no connection to a host that exited");
    assert!(adopted.connect(Hosted::soon()).is_err());

    let (unreachable, bootstrap) = room.adopt();
    assert_eq!(bootstrap, b"first bootstrap", "the bootstrap of a host that ended is still read");
    assert!(unreachable.connect(Hosted::soon()).is_err(), "but nothing is reached there");
    let claimed = room.ended(b"first bootstrap");
    assert!(matches!(hosts.claim(state).unwrap(), Retained::Held), "a claim keeps the folder until it is retired or dropped");
    assert_eq!(room.refusal(), io::ErrorKind::Other, "a claimed folder takes no host");
    drop(claimed);
    assert_eq!(room.refusal(), io::ErrorKind::PermissionDenied, "a bootstrap not retired never lets another host in");
    let Retained::Ended(ended) = hosts.end_holders(state).unwrap() else { panic!("ending the holders of a folder nothing holds claims it") };
    assert_eq!(ended.bootstrap(), b"first bootstrap");
    room.retire(ended, "first.ended");

    let mut second = room.launch(b"second bootstrap");
    fixture.exit(second.wait_for_socket(Hosted::soon()).unwrap());
    let status = second.wait().unwrap();
    assert!(status.success(), "a host that ends by itself says how");
    assert_eq!(second.observed_exit().unwrap(), Some(status));
    room.retire(room.ended(b"second bootstrap"), "second.ended");

    let mut third = room.launch(b"third bootstrap");
    third.stopper().kill().unwrap();
    assert_eq!(third.wait().unwrap().signal(), Some(libc::SIGKILL), "a killed host ends killed");
    room.retire(room.ended(b"third bootstrap"), "third.ended");

    let mut fourth = room.launch(b"fourth bootstrap");
    let (mut adopted, _) = room.adopt();
    adopted.stop().unwrap();
    assert_eq!(fourth.observed_exit().unwrap(), None, "an adopted host never reached is not stopped");
    drop(adopted.connect(Hosted::soon()).unwrap());
    adopted.stop().unwrap();
    let status = fourth.wait().unwrap();
    assert_eq!(fourth.observed_exit().unwrap(), Some(status), "the stop of an adopted host ends it");
    assert_gone(&adopted.stopper());
    room.retire(room.ended(b"fourth bootstrap"), "fourth.ended");

    let fifth = room.launch(b"fifth bootstrap");
    let process = fifth.process_id();
    drop(fifth);
    assert!(matches!(hosts.claim(state).unwrap(), Retained::Held), "a host outlives the handle that launched it");
    let (adopted, bootstrap) = room.adopt();
    assert_eq!(bootstrap, b"fifth bootstrap");
    assert_eq!(fixture.process_id(adopted.connect(Hosted::soon()).unwrap()), process, "and still answers");
    let Retained::Ended(ended) = hosts.end_holders(state).unwrap() else { panic!("ending the holders of a live host ends it and claims its folder") };
    assert_eq!(ended.bootstrap(), b"fifth bootstrap");
    assert_eq!(adopted.stopper().process().unwrap(), None, "the host whose holders were ended no longer runs");
    room.retire(ended, "fifth.ended");
}
