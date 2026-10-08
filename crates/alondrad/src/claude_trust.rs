//! `claude --bg` refuses a git repository the user never trusted (Claude Code 2.1.281+), and a
//! trusted parent folder does not count. Alondra only reads that trust, never grants it: the user
//! does, and Alondra checks before it saves a Schedule or launches a session.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The repository `cwd` belongs to when Claude Code does not trust it, read from this user's config.
pub fn check(cwd: &Path) -> Option<PathBuf> {
    untrusted(&config_path()?, cwd)
}

#[cfg(not(test))]
fn config_path() -> Option<PathBuf> {
    let env = |key: &str| std::env::var(key).ok();
    let dir = env("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()).map(PathBuf::from);
    Some(dir.unwrap_or_else(|| alondra_core::config::home_dir(&env)).join(".claude.json"))
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

/// The repository `cwd` belongs to when Claude Code does not trust it. An unreadable config proves
/// nothing, so it does not block: Claude's own refusal still reaches the orchestrator.
pub fn untrusted(config: &Path, cwd: &Path) -> Option<PathBuf> {
    let cwd = cwd.canonicalize().ok()?;
    let repository = cwd.ancestors().find(|dir| dir.join(".git").exists()).unwrap_or(&cwd).to_path_buf();
    let document: Value = serde_json::from_str(&std::fs::read_to_string(config).ok()?).ok()?;
    let accepted = |dir: &Path| {
        dir.to_str().and_then(|dir| document.get("projects")?.get(dir)?.get("hasTrustDialogAccepted"))
            == Some(&Value::Bool(true))
    };
    let trusted = accepted(&repository) || main_checkout(&repository).is_some_and(|main| accepted(&main));
    (!trusted).then_some(repository)
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

/// What the user does to trust `repository`; Alondra never does it for them.
pub fn instructions(repository: &Path) -> String {
    format!(
        "Claude Code does not trust {repo}, so Alondra cannot start a session there. The user trusts it on the Mac: set \"hasTrustDialogAccepted\": true under projects[\"{repo}\"] in ~/.claude.json, or run `claude` in {repo} and accept the trust prompt.",
        repo = repository.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

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

        assert_eq!(untrusted(&config, &linked), Some(linked));
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

    #[test]
    fn a_missing_config_does_not_block() {
        let fixture = Fixture::new();
        let repository = fixture.repository("BlurCamera");

        assert_eq!(untrusted(&fixture.dir.join(".claude.json"), &repository), None);
    }
}
