use super::*;
use crate::test_support::Directory;
use alondra_core::accounts::AccountId;
use alondra_core::claude_account::default_folder;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

fn seed_claude(user_home: &Path) -> PathBuf {
    let claude = default_folder(user_home);
    for dir in ["skills/mine", "agents", "plugins", "projects/-repo"] {
        std::fs::create_dir_all(claude.join(dir)).unwrap();
    }
    std::fs::write(claude.join("CLAUDE.md"), "be brief").unwrap();
    std::fs::write(claude.join("settings.json"), "{}").unwrap();
    std::fs::write(claude.join("projects/-repo/session.jsonl"), "{}\n").unwrap();
    std::fs::write(claude.join(".credentials.json"), "secret").unwrap();
    claude
}

#[test]
fn an_account_folder_is_private_and_links_what_claude_has_of_the_shared_names() {
    let scratch = Directory::new();
    let user_home = scratch.join("user");
    let claude = seed_claude(&user_home);
    let home = user_home.join(".alondra");
    let id = AccountId::parse("work").unwrap();
    let folder = prepare(&home, &id, &claude).unwrap();
    assert_eq!(folder, home.join("accounts/work"));
    assert_eq!(std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777, 0o700);
    assert_eq!(std::fs::metadata(home.join("accounts")).unwrap().permissions().mode() & 0o777, 0o700);
    let mut names: Vec<String> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["CLAUDE.md", "agents", "plugins", "projects", "settings.json", "skills"], "only what ~/.claude has, never its sign-in");
    for name in &names {
        assert_eq!(std::fs::read_link(folder.join(name)).unwrap(), claude.join(name));
    }
    assert_eq!(std::fs::read_to_string(folder.join("projects/-repo/session.jsonl")).unwrap(), "{}\n");

    std::fs::create_dir_all(claude.join("rules")).unwrap();
    std::fs::write(folder.join(".claude.json"), "{\"own\":true}").unwrap();
    prepare(&home, &id, &claude).unwrap();
    assert_eq!(std::fs::read_link(folder.join("rules")).unwrap(), claude.join("rules"), "preparing again adds what appeared");
    assert_eq!(std::fs::read_to_string(folder.join(".claude.json")).unwrap(), "{\"own\":true}");
}

#[test]
fn removing_an_account_deletes_its_folder_and_never_what_it_links() {
    let scratch = Directory::new();
    let user_home = scratch.join("user");
    let claude = seed_claude(&user_home);
    let home = user_home.join(".alondra");
    let id = AccountId::parse("work").unwrap();
    let folder = prepare(&home, &id, &claude).unwrap();
    remove(&home, &id).unwrap();
    assert!(!folder.exists());
    for name in ["skills/mine", "agents", "plugins", "projects/-repo/session.jsonl", "CLAUDE.md", "settings.json", ".credentials.json"] {
        assert!(claude.join(name).exists(), "{name} is still in ~/.claude");
    }
    remove(&home, &id).unwrap();
    assert!(remove(&home, &AccountId::default_account()).is_err());
    assert!(prepare(&home, &AccountId::default_account(), &claude).is_err());
}

// A session moves between accounts only while both read the user's own conversations: an account
// whose folder holds a `projects/` of its own would resume nothing of it.
#[test]
fn accounts_share_sessions_only_through_the_linked_projects_folder() {
    let scratch = Directory::new();
    let user_home = scratch.join("user");
    let claude = seed_claude(&user_home);
    let home = user_home.join(".alondra");
    let (default, work, own, none) = (AccountId::default_account(), AccountId::parse("work").unwrap(), AccountId::parse("own").unwrap(), AccountId::parse("none").unwrap());
    prepare(&home, &work, &claude).unwrap();
    let separate = prepare(&home, &own, &claude).unwrap();
    std::fs::remove_file(separate.join("projects")).unwrap();
    std::fs::create_dir(separate.join("projects")).unwrap();
    assert!(shares_sessions(&home, &user_home, None, &default, &work));
    assert!(shares_sessions(&home, &user_home, None, &work, &default));
    assert!(!shares_sessions(&home, &user_home, None, &default, &own), "its own projects folder");
    assert!(!shares_sessions(&home, &user_home, None, &work, &none), "an account with no folder");
    let profile = user_home.join(".claude-work");
    std::fs::create_dir_all(profile.join("projects")).unwrap();
    let linked = prepare(&home, &AccountId::parse("linked").unwrap(), &profile).unwrap();
    assert!(shares_sessions(&home, &user_home, Some(&profile), &default, &AccountId::parse("linked").unwrap()), "the default profile the daemon was set up with");
    assert!(!shares_sessions(&home, &user_home, Some(&profile), &default, &work), "an account linked to ~/.claude while the default profile is another");
    assert!(linked.join("projects").exists());
}

