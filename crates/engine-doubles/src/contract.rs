//! What the engine relies on from the `Worktrees`, `Disk`, `MachineProbe`, `Accounts`, `Processes`
//! and `Plugins` ports. The doubles of this crate pass it in their unit tests; `crates/system` passes
//! it with the Mac's own git, disks, gauges, files and processes, so a double never drifts from what
//! the daemon really does.
use crate::test_support::Directory;
pub mod github;
use alondra_engine_ports::disk::Disk;
use alondra_engine_ports::machine::MachineProbe;
use alondra_core::secret_destination::SecretDestination;
use alondra_domain_rules::{Actor, Command, CommandEnvelope, DomainState, LaunchPlan, PROTOCOL_VERSION, TaskId, WorktreeOrigin};
use alondra_engine_ports::worktrees::{LaunchError, Protected, TaskWorktree, Workplace, Worktrees, CHECK_LIMIT};
use std::future::Future as _;
use std::path::Path;
use std::time::Duration;

/// A missing path has no volume; a real folder is on one, with sane space.
pub fn disk(disk: &dyn Disk) {
    let scratch = Directory::new();
    std::fs::create_dir_all(&scratch).unwrap();
    let missing = scratch.join("missing");
    assert_eq!(disk.space(&missing), None, "a missing path has no space");
    assert_eq!(disk.mount_point(&missing), None, "a missing path has no volume");
    assert_eq!(disk.held(&missing), None);
    let space = disk.space(&scratch).expect("a real folder is on a volume");
    assert!(space.total > 0 && space.free <= space.total && space.usable() >= space.free, "{space:?}");
    let mount = disk.mount_point(&scratch).expect("a real folder has a mount point");
    assert!(scratch.canonicalize().unwrap().starts_with(&mount), "{} is not under {}", scratch.display(), mount.display());
    assert!(disk.same_volume(&scratch, &mount));
    assert!(!disk.same_volume(&scratch, &missing), "a missing path shares no volume");
    let mounts = disk.mounted_volumes().unwrap();
    assert!(mounts.candidates.iter().all(|candidate| mounts.all.contains(candidate)), "{mounts:?}");
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
    std::fs::write(dir.join("notes.txt"), "not committed\n").unwrap();
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
/// taken as clean. `slow` makes every check in the worktree it is given take that long.
pub fn final_checks(worktrees: &dyn Worktrees, repository: &dyn Fn(&Path), slow: &dyn Fn(&Path, Duration)) {
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
        state.execute(&actor, &envelope).unwrap();
    };
    run(Actor::User { user_id: "u".into() }, Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
    run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, continues: None, task_id: "t".into(), request_id: "r".into(),
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
