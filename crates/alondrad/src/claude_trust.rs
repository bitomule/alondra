//! `claude --bg` refuses a git repository the user never trusted (Claude Code 2.1.281+). Claude
//! Code looks for trust from the folder up to the repository's root and stops there, so a trusted
//! parent folder does not count, and it keeps it in `~/.claude.json` under the repository's root,
//! the main checkout for a linked worktree; an account Alondra keeps in a folder of its own keeps it
//! in that folder's `.claude.json`. Alondra reads that trust before it saves a Schedule or
//! launches a session, and writes it the way Claude Code's trust dialog does in two cases only: the
//! user chose to trust the folder on its card, or the repository is inside the projects folder
//! the user chose at setup.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use serde_json::Value;

/// The repository Claude Code would have to trust for `cwd`, when it does not, read from this
/// user's config.
pub fn check(cwd: &Path) -> Option<PathBuf> {
    untrusted(&config_path()?, cwd)
}

/// Like `check`, but a repository inside `projects_root` is trusted on the spot: choosing that
/// folder at setup is the user's trust for what is in it. What is still untrusted comes back.
pub fn ensure(cwd: &Path, projects_root: Option<&Path>) -> Option<PathBuf> {
    ensure_for(None, cwd, projects_root)
}

/// `ensure` for the Claude account whose configuration is in `profile`, the folder Alondra keeps
/// for it; none for the user's own.
pub fn ensure_for(profile: Option<&Path>, cwd: &Path, projects_root: Option<&Path>) -> Option<PathBuf> {
    let config = config_of(profile)?;
    ensure_in(&config, cwd, projects_root, &crate::options::user_home())
}

/// Records that the user trusts `repository`, as Claude Code's trust dialog does: in their own
/// settings and in those of every account in `profiles`, so a session starts there on any of them.
pub fn grant(repository: &Path, profiles: &[PathBuf]) -> Result<(), String> {
    let config = config_path().ok_or("Alondra could not find Claude Code's settings file (~/.claude.json)")?;
    grant_in(&config, repository)?;
    for profile in profiles.iter().filter(|profile| profile.is_dir()) {
        grant_in(&profile.join(CONFIG), repository)?;
    }
    Ok(())
}

const CONFIG: &str = ".claude.json";

fn config_of(profile: Option<&Path>) -> Option<PathBuf> {
    match profile {
        Some(profile) => Some(profile.join(CONFIG)),
        None => config_path(),
    }
}

#[cfg(not(test))]
fn config_path() -> Option<PathBuf> {
    Some(config_in(&|key: &str| std::env::var(key).ok()))
}

/// The settings file of the profile the daemon's nodes run with by default.
fn config_in(env: &impl Fn(&str) -> Option<String>) -> PathBuf {
    use alondra_core::claude_account::{configured_profile, settings_file};
    settings_file(configured_profile(env).as_deref(), &alondra_core::config::home_dir(env))
}

#[cfg(test)]
thread_local! {
    static TEST_CONFIG: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// Tests never read the real `~/.claude.json`: a test that checks trust names its own config.
#[cfg(test)]
fn config_path() -> Option<PathBuf> {
    TEST_CONFIG.with(|config| config.borrow().clone())
}

#[cfg(test)]
pub fn use_config_for_test(config: &Path) {
    TEST_CONFIG.with(|current| *current.borrow_mut() = Some(config.to_path_buf()));
}

/// The repository Claude Code would have to trust for `cwd` when it does not: the repository's
/// root, or for a linked worktree its main checkout. An unreadable config proves nothing, so it
/// does not block: Claude's own refusal still reaches the orchestrator.
pub fn untrusted(config: &Path, cwd: &Path) -> Option<PathBuf> {
    let cwd = cwd.canonicalize().ok()?;
    let repository = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(&cwd).to_path_buf();
    let document: Value = serde_json::from_str(&std::fs::read_to_string(config).ok()?).ok()?;
    let accepted = |dir: &Path| {
        dir.to_str().and_then(|dir| document.get("projects")?.get(dir)?.get("hasTrustDialogAccepted"))
            == Some(&Value::Bool(true))
    };
    let main = main_checkout(&repository);
    let trusted = accepted(&repository) || main.as_deref().is_some_and(accepted);
    (!trusted).then(|| main.unwrap_or(repository))
}

/// A projects folder that holds the home folder would trust the whole disk, so it trusts nothing.
fn ensure_in(config: &Path, cwd: &Path, projects_root: Option<&Path>, home: &Path) -> Option<PathBuf> {
    let repository = untrusted(config, cwd)?;
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let inside = projects_root
        .and_then(|root| root.canonicalize().ok())
        .is_some_and(|root| !home.starts_with(&root) && repository.starts_with(&root));
    if !inside {
        return Some(repository);
    }
    match grant_in(config, &repository) {
        Ok(()) => {
            log!("Claude Code now trusts {}: it is inside the projects folder", repository.display());
            None
        }
        Err(why) => {
            log!("could not trust {} in Claude Code: {why}", repository.display());
            Some(repository)
        }
    }
}

fn grant_in(config: &Path, folder: &Path) -> Result<(), String> {
    let folder = folder.canonicalize().map_err(|error| format!("{} cannot be opened ({error})", folder.display()))?;
    let repository = main_checkout(&folder).unwrap_or(folder);
    let key = repository.to_str().ok_or_else(|| format!("{} is not a path Claude Code can store", repository.display()))?;
    let _lock = ConfigLock::take(config)?;
    let target = config.canonicalize().unwrap_or_else(|_| config.to_path_buf());
    let shown = target.display();
    let mut document: Value = match std::fs::read_to_string(&target) {
        Ok(text) => serde_json::from_str(&text)
            .map_err(|error| format!("Claude Code's settings file {shown} is not valid JSON ({error}), so Alondra left it untouched"))?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Value::Object(Default::default()),
        Err(error) => return Err(format!("Alondra could not read Claude Code's settings file {shown}: {error}")),
    };
    let unexpected = || format!("Claude Code's settings file {shown} does not have the shape Alondra expects, so Alondra left it untouched");
    let projects = document.as_object_mut().ok_or_else(unexpected)?
        .entry("projects").or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut().ok_or_else(unexpected)?;
    projects.entry(key).or_insert_with(new_project)
        .as_object_mut().ok_or_else(unexpected)?
        .insert("hasTrustDialogAccepted".into(), Value::Bool(true));
    let text = serde_json::to_string_pretty(&document).map_err(|error| error.to_string())?;
    replace(&target, &text).map_err(|error| format!("Alondra could not write Claude Code's settings file {shown}: {error}"))
}

/// The entry Claude Code creates for a project it has not seen yet.
fn new_project() -> Value {
    serde_json::json!({
        "allowedTools": [], "mcpContextUris": [], "mcpServers": {}, "enabledMcpjsonServers": [],
        "disabledMcpjsonServers": [], "hasTrustDialogAccepted": false,
        "hasClaudeMdExternalIncludesApproved": false, "hasClaudeMdExternalIncludesWarningShown": false,
    })
}

/// Written beside it and renamed over it, readable only by the user, as Claude Code writes it.
fn replace(target: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let temporary = PathBuf::from(format!("{}.tmp.{}.{}", target.display(), std::process::id(), crate::auth::random_hex(6)));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temporary)
        .and_then(|mut file| file.write_all(text.as_bytes()).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temporary, target));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Claude Code's own lock on its settings file: a `<file>.lock` folder, given up as abandoned once
/// it is older than ten seconds, like the proper-lockfile library Claude Code takes it with.
struct ConfigLock(PathBuf);

const LOCK_ABANDONED: Duration = Duration::from_secs(10);
const LOCK_WAIT: Duration = Duration::from_millis(if cfg!(test) { 300 } else { 5000 });

impl ConfigLock {
    fn take(config: &Path) -> Result<Self, String> {
        let lock = PathBuf::from(format!("{}.lock", config.display()));
        let started = Instant::now();
        loop {
            match std::fs::create_dir(&lock) {
                Ok(()) => return Ok(Self(lock)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let abandoned = std::fs::metadata(&lock).and_then(|meta| meta.modified()).ok()
                        .and_then(|at| SystemTime::now().duration_since(at).ok())
                        .is_some_and(|age| age > LOCK_ABANDONED);
                    if abandoned {
                        let _ = std::fs::remove_dir(&lock);
                    } else if started.elapsed() > LOCK_WAIT {
                        return Err(format!("another Claude Code session kept {} locked; try again in a moment", config.display()));
                    } else {
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
                Err(error) => return Err(format!("Alondra could not lock {}: {error}", config.display())),
            }
        }
    }
}

impl Drop for ConfigLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir(&self.0);
    }
}

/// The repository a linked worktree belongs to, which Claude Code trusts it through. Its links
/// must agree both ways, as Claude Code checks them: a `.git` file naming
/// `<main>/.git/worktrees/<name>`, whose `commondir` is `<main>/.git` and whose `gitdir` names
/// this worktree back, so a crafted repository cannot borrow another one's trust.
fn main_checkout(worktree: &Path) -> Option<PathBuf> {
    let link = std::fs::read_to_string(worktree.join(".git")).ok()?;
    let admin = worktree.join(link.trim().strip_prefix("gitdir:")?.trim()).canonicalize().ok()?;
    let common = admin.join(std::fs::read_to_string(admin.join("commondir")).ok()?.trim()).canonicalize().ok()?;
    let named_back = PathBuf::from(std::fs::read_to_string(admin.join("gitdir")).ok()?.trim()).canonicalize().ok()?;
    let linked = admin.parent()? == common.join("worktrees") && named_back == worktree.join(".git");
    (linked && common.file_name()? == ".git").then(|| common.parent().map(Path::to_path_buf))?
}

/// Why a session cannot start in `repository`, for the orchestrator and the nodes.
pub fn instructions(repository: &Path) -> String {
    format!(
        "Claude Code does not trust {repo}, so Alondra cannot start a session there. The user trusts it from the card Alondra shows them, or on the Mac by running `claude` in {repo} and accepting the trust prompt.",
        repo = repository.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trust_is_read_from_the_settings_of_the_profile_nodes_run_with() {
        let set_up = |profile: Option<&'static str>| move |key: &str| match key {
            "HOME" => Some("/Users/ana".to_owned()),
            "CLAUDE_CONFIG_DIR" | "CLAUDE_HOME" => profile.map(str::to_owned),
            _ => None,
        };
        assert_eq!(config_in(&set_up(None)), PathBuf::from("/Users/ana/.claude.json"));
        assert_eq!(config_in(&set_up(Some("/Users/ana/.claude-work"))), PathBuf::from("/Users/ana/.claude-work/.claude.json"));
    }

    struct Fixture {
        dir: crate::test_support::Directory,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = crate::test_support::Directory::new();
            std::fs::create_dir_all(&dir).unwrap();
            Self { dir }
        }

        fn repository(&self, name: &str) -> PathBuf {
            let repository = self.dir.join(name);
            std::fs::create_dir_all(repository.join(".git")).unwrap();
            repository
        }

        fn config(&self, projects: Value) -> PathBuf {
            let config = self.dir.join(".claude.json");
            std::fs::write(&config, serde_json::json!({"projects": projects}).to_string()).unwrap();
            config
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    #[test]
    fn a_repository_without_accepted_trust_is_untrusted() {
        let fixture = Fixture::new();
        let parcel = fixture.repository("Parcel");
        let fjord = fixture.repository("Fjord");
        let config = fixture.config(serde_json::json!({parcel.to_str().unwrap(): {"hasTrustDialogAccepted": false}}));

        assert_eq!(untrusted(&config, &parcel), Some(parcel));
        assert_eq!(untrusted(&config, &fjord), Some(fjord));
    }

    #[test]
    fn a_trusted_repository_passes_from_any_folder_inside_it() {
        let fixture = Fixture::new();
        let lumo = fixture.repository("Lumo");
        let nested = lumo.join("App/Sources");
        std::fs::create_dir_all(&nested).unwrap();
        let config = fixture.config(serde_json::json!({lumo.to_str().unwrap(): {"hasTrustDialogAccepted": true}}));

        assert_eq!(untrusted(&config, &lumo), None);
        assert_eq!(untrusted(&config, &nested), None);
    }

    #[test]
    fn a_folder_inside_an_untrusted_repository_names_the_repository() {
        let fixture = Fixture::new();
        let quill = fixture.repository("Quill");
        let nested = quill.join("Docs");
        std::fs::create_dir_all(&nested).unwrap();
        let config = fixture.config(serde_json::json!({nested.to_str().unwrap(): {"hasTrustDialogAccepted": true}}));

        assert_eq!(untrusted(&config, &nested), Some(quill));
    }

    /// A linked worktree as `git worktree add` lays it out, under `<main>/.claude/worktrees/<name>`.
    fn worktree(main: &Path, name: &str) -> PathBuf {
        let dir = main.join(".claude/worktrees").join(name);
        let admin = main.join(".git/worktrees").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(dir.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(admin.join("commondir"), "../..\n").unwrap();
        std::fs::write(admin.join("gitdir"), format!("{}\n", dir.join(".git").display())).unwrap();
        dir
    }

    #[test]
    fn a_worktree_of_a_trusted_repository_is_trusted_as_claude_code_does() {
        let fixture = Fixture::new();
        let alondra = fixture.repository("alondra");
        let linked = worktree(&alondra, "t-cards-3-tui");
        let nested = linked.join("crates");
        std::fs::create_dir_all(&nested).unwrap();
        let config = fixture.config(serde_json::json!({alondra.to_str().unwrap(): {"hasTrustDialogAccepted": true}}));

        assert_eq!(untrusted(&config, &linked), None);
        assert_eq!(untrusted(&config, &nested), None);
    }

    #[test]
    fn a_worktree_of_an_untrusted_repository_is_untrusted() {
        let fixture = Fixture::new();
        let lumo = fixture.repository("Lumo");
        let linked = worktree(&lumo, "feature");
        let config = fixture.config(serde_json::json!({}));

        assert_eq!(untrusted(&config, &linked), Some(lumo), "Claude Code keeps a worktree's trust under its main checkout");
    }

    #[test]
    fn a_crafted_worktree_link_does_not_borrow_another_repositorys_trust() {
        let fixture = Fixture::new();
        let trusted = fixture.repository("Trusted");
        let real = worktree(&trusted, "real");
        let config = fixture.config(serde_json::json!({trusted.to_str().unwrap(): {"hasTrustDialogAccepted": true}}));
        let cloned = fixture.dir.join("Cloned");
        std::fs::create_dir_all(&cloned).unwrap();
        std::fs::write(cloned.join(".git"), format!("gitdir: {}\n", trusted.join(".git/worktrees/real").display())).unwrap();
        assert_eq!(untrusted(&config, &cloned), Some(cloned.clone()), "the trusted worktree does not name it back");

        let admin = cloned.join("admin");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::write(cloned.join(".git"), format!("gitdir: {}\n", admin.display())).unwrap();
        std::fs::write(admin.join("commondir"), format!("{}\n", trusted.join(".git").display())).unwrap();
        std::fs::write(admin.join("gitdir"), format!("{}\n", cloned.join(".git").display())).unwrap();
        assert_eq!(untrusted(&config, &cloned), Some(cloned), "a commondir pointing at a trusted repository is not enough");
        assert_eq!(untrusted(&config, &real), None);
    }

    fn read(config: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(config).unwrap()).unwrap()
    }

    fn accepted(config: &Path, repository: &Path) -> Value {
        read(config)["projects"][repository.to_str().unwrap()]["hasTrustDialogAccepted"].clone()
    }

    #[test]
    fn granting_trust_writes_what_claude_codes_dialog_writes_and_keeps_the_rest() {
        let fixture = Fixture::new();
        let blur_camera = fixture.repository("BlurCamera");
        let lumo = fixture.repository("Lumo");
        let config = fixture.config(serde_json::json!({lumo.to_str().unwrap(): {"hasTrustDialogAccepted": true, "lastCost": 2.5}}));
        let mut document = read(&config);
        document["numStartups"] = 41.into();
        std::fs::write(&config, document.to_string()).unwrap();

        grant_in(&config, &blur_camera).unwrap();

        let document = read(&config);
        assert_eq!(document["numStartups"], 41);
        assert_eq!(document["projects"][lumo.to_str().unwrap()], serde_json::json!({"hasTrustDialogAccepted": true, "lastCost": 2.5}));
        assert_eq!(document["projects"][blur_camera.to_str().unwrap()], serde_json::json!({
            "allowedTools": [], "mcpContextUris": [], "mcpServers": {}, "enabledMcpjsonServers": [],
            "disabledMcpjsonServers": [], "hasTrustDialogAccepted": true,
            "hasClaudeMdExternalIncludesApproved": false, "hasClaudeMdExternalIncludesWarningShown": false,
        }));
        assert_eq!(untrusted(&config, &blur_camera), None);
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(std::fs::metadata(&config).unwrap().permissions().mode() & 0o777, 0o600);
        let left: Vec<_> = std::fs::read_dir(&*fixture.dir).unwrap().map(|entry| entry.unwrap().file_name()).collect();
        assert!(left.iter().all(|name| !name.to_string_lossy().starts_with(".claude.json.")), "no lock or temporary file is left: {left:?}");
    }

    #[test]
    fn granting_trust_turns_an_entry_claude_code_left_false_true() {
        let fixture = Fixture::new();
        let indie_apps = fixture.repository("indie-apps");
        let config = fixture.config(serde_json::json!({indie_apps.to_str().unwrap(): {"hasTrustDialogAccepted": false, "lastCost": 1}}));

        grant_in(&config, &indie_apps).unwrap();

        assert_eq!(read(&config)["projects"][indie_apps.to_str().unwrap()], serde_json::json!({"hasTrustDialogAccepted": true, "lastCost": 1}));
    }

    #[test]
    fn granting_trust_waits_for_a_live_claude_code_lock_and_takes_an_abandoned_one() {
        let fixture = Fixture::new();
        let quill = fixture.repository("Quill");
        let config = fixture.config(serde_json::json!({}));
        let lock = fixture.dir.join(".claude.json.lock");
        std::fs::create_dir(&lock).unwrap();

        let refused = grant_in(&config, &quill).unwrap_err();
        assert!(refused.contains("another Claude Code session kept"), "{refused}");
        assert_eq!(accepted(&config, &quill), Value::Null);
        assert!(lock.is_dir(), "a live lock is someone else's to remove");

        std::fs::File::open(&lock).unwrap().set_modified(SystemTime::now() - Duration::from_secs(60)).unwrap();
        grant_in(&config, &quill).unwrap();
        assert_eq!(accepted(&config, &quill), true);
        assert!(!lock.exists());
    }

    #[test]
    fn a_settings_file_alondra_cannot_parse_is_left_untouched_with_the_reason() {
        let fixture = Fixture::new();
        let fjord = fixture.repository("Fjord");
        let config = fixture.dir.join(".claude.json");
        std::fs::write(&config, "{ not json").unwrap();

        let refused = grant_in(&config, &fjord).unwrap_err();

        assert!(refused.contains("is not valid JSON") && refused.contains("left it untouched"), "{refused}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "{ not json");
    }

    #[test]
    fn a_repository_inside_the_projects_folder_is_trusted_by_choosing_that_folder() {
        let fixture = Fixture::new();
        let projects = fixture.dir.join("Projects");
        let blur_camera = projects.join("indie-apps");
        std::fs::create_dir_all(blur_camera.join(".git")).unwrap();
        let elsewhere = fixture.repository("Downloads-clone");
        let config = fixture.config(serde_json::json!({
            projects.to_str().unwrap(): {"hasTrustDialogAccepted": true},
            blur_camera.to_str().unwrap(): {"hasTrustDialogAccepted": false},
        }));
        let home = fixture.dir.join("home");
        std::fs::create_dir_all(&home).unwrap();

        assert_eq!(untrusted(&config, &blur_camera), Some(blur_camera.clone()), "a trusted parent does not count for Claude Code");
        assert_eq!(ensure_in(&config, &blur_camera, Some(&projects), &home), None);
        assert_eq!(accepted(&config, &blur_camera), true);
        assert_eq!(ensure_in(&config, &elsewhere, Some(&projects), &home), Some(elsewhere.clone()));
        assert_eq!(ensure_in(&config, &elsewhere, None, &home), Some(elsewhere.clone()));
        assert_eq!(accepted(&config, &elsewhere), Value::Null);
    }

    #[test]
    fn a_worktree_inside_the_projects_folder_trusts_its_main_checkout() {
        let fixture = Fixture::new();
        let projects = fixture.dir.join("Projects");
        std::fs::create_dir_all(projects.join("Lumo/.git")).unwrap();
        let lumo = projects.join("Lumo");
        let linked = worktree(&lumo, "t-paywall");
        let config = fixture.config(serde_json::json!({}));

        assert_eq!(ensure_in(&config, &linked, Some(&projects), &fixture.dir.join("home")), None);

        assert_eq!(accepted(&config, &lumo), true);
        assert_eq!(accepted(&config, &linked), Value::Null);
    }

    #[test]
    fn a_link_out_of_the_projects_folder_or_a_folder_holding_home_trusts_nothing() {
        let fixture = Fixture::new();
        let projects = fixture.dir.join("Projects");
        std::fs::create_dir_all(&projects).unwrap();
        let outside = fixture.repository("Outside");
        std::os::unix::fs::symlink(&outside, projects.join("Linked")).unwrap();
        let home = fixture.dir.join("home");
        let at_home = home.join("Code");
        std::fs::create_dir_all(at_home.join(".git")).unwrap();
        let config = fixture.config(serde_json::json!({}));

        assert_eq!(ensure_in(&config, &projects.join("Linked"), Some(&projects), &home), Some(outside.clone()));
        assert_eq!(ensure_in(&config, &at_home, Some(&home), &home), Some(at_home.clone()));
        assert_eq!(ensure_in(&config, &at_home, Some(&fixture.dir), &home), Some(at_home));
        assert_eq!(read(&config)["projects"], serde_json::json!({}));
    }

    #[test]
    fn a_missing_config_does_not_block() {
        let fixture = Fixture::new();
        let repository = fixture.repository("BlurCamera");

        assert_eq!(untrusted(&fixture.dir.join(".claude.json"), &repository), None);
    }

    #[test]
    fn an_accounts_trust_is_read_and_written_in_its_own_folder() {
        let fixture = Fixture::new();
        let lumo = fixture.repository("Lumo");
        let own = fixture.config(serde_json::json!({}));
        use_config_for_test(&own);
        let account = fixture.dir.join("accounts/work");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(account.join(CONFIG), r#"{"oauthAccount": {}}"#).unwrap();
        assert_eq!(ensure_for(Some(&account), &lumo, None), Some(lumo.canonicalize().unwrap()), "nothing trusts it yet");
        assert_eq!(ensure_for(Some(&account), &lumo, Some(&fixture.dir)), None, "inside the projects folder it is trusted on the spot");
        assert_eq!(untrusted(&account.join(CONFIG), &lumo), None, "in the account's own settings");
        assert!(untrusted(&own, &lumo).is_some(), "the user's own settings are not touched");
        let fjord = fixture.repository("Fjord");
        grant(&fjord, std::slice::from_ref(&account)).unwrap();
        assert_eq!((untrusted(&own, &fjord), untrusted(&account.join(CONFIG), &fjord)), (None, None), "the user's trust holds on every account");
    }
}
