use super::*;
use crate::test_support::Directory;
use std::os::unix::fs::PermissionsExt as _;

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

#[test]
fn a_status_says_the_sign_in_its_email_and_plan_and_fails_closed() {
    let status = parse_status(true, br#"{"loggedIn":true,"email":" me@example.org ","subscriptionType":"max","orgId":"x"}"#).unwrap();
    assert_eq!(status, AuthStatus { logged_in: true, email: Some("me@example.org".into()), subscription: Some("max".into()) });
    assert_eq!(parse_status(false, br#"{"loggedIn":false}"#), Some(AuthStatus::default()));
    for (ok, bytes) in [(true, b"corrupt".as_slice()), (true, b"{}".as_slice()), (false, br#"{"loggedIn":true}"#.as_slice())] {
        assert_eq!(parse_status(ok, bytes), None);
    }
    assert_eq!(parse_status(true, &vec![b' '; 70_000]), None);
}

fn fake_claude(dir: &Path, body: &str) -> PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let script = dir.join("claude");
    std::fs::write(&script, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    script
}

#[test]
fn claude_runs_for_an_account_with_a_clean_environment_and_its_folder_only_when_not_default() {
    let scratch = Directory::new();
    let bin = scratch.join("bin");
    let script = fake_claude(&bin, r#"
if [ "$1 $2 $3" != "auth status --json" ] || [ "$PWD" != "$HOME" ] || [ -n "$ANTHROPIC_API_KEY" ]; then exit 3; fi
if [ -z "$CLAUDE_CONFIG_DIR" ]; then printf '{"loggedIn":true,"email":"me@example.org","subscriptionType":"pro"}'; exit 0; fi
if [ "$CLAUDE_HOME" = "$CLAUDE_CONFIG_DIR" ] && [ -f "$CLAUDE_CONFIG_DIR/in" ]; then printf '{"loggedIn":true,"email":"work@example.org"}'; exit 0; fi
printf '{"loggedIn":false}'; exit 1"#);
    let path = format!("{}:/usr/bin:/bin", bin.display());
    assert_eq!(executable(&path).unwrap(), script);
    let user = effective_username().unwrap();
    let user_home = scratch.join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let account = user_home.join(".alondra/accounts/work");
    std::fs::create_dir_all(&account).unwrap();
    let default = status(&script, &BTreeMap::new(), &user_home, &path, &user, None).unwrap();
    assert_eq!(default.email.as_deref(), Some("me@example.org"));
    assert_eq!(default.subscription.as_deref(), Some("pro"));
    assert!(!status(&script, &BTreeMap::new(), &user_home, &path, &user, Some(&account)).unwrap().logged_in);
    std::fs::write(account.join("in"), "").unwrap();
    assert_eq!(status(&script, &BTreeMap::new(), &user_home, &path, &user, Some(&account)).unwrap().email.as_deref(), Some("work@example.org"));
    let vars: Vec<String> = command(&script, &BTreeMap::new(), &user_home, &path, &user, None).get_envs().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
    assert_eq!(vars.len(), 4, "{vars:?}");
}

#[test]
fn a_claude_that_does_not_answer_is_stopped() {
    let scratch = Directory::new();
    let script = fake_claude(&scratch.join("bin"), "exec sleep 30");
    let mut command = Command::new(&script);
    command.current_dir(scratch.join("bin"));
    assert!(output(command, Duration::from_millis(100)).unwrap_err().contains("did not answer"));
}

#[test]
fn the_profile_an_environment_names_is_claude_config_dir_else_claude_home_else_none() {
    let of = |pairs: &[(&str, &str)]| {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        configured_profile(&|key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    };
    assert_eq!(of(&[]), None);
    assert_eq!(of(&[("CLAUDE_CONFIG_DIR", " ")]), None, "an empty variable is Claude's default profile");
    assert_eq!(of(&[("CLAUDE_CONFIG_DIR", "/u/.claude-work"), ("CLAUDE_HOME", "/u/.claude-work")]), Some(PathBuf::from("/u/.claude-work")));
    assert_eq!(of(&[("CLAUDE_HOME", "/u/.claude-work")]), Some(PathBuf::from("/u/.claude-work")));
}

#[test]
fn the_default_account_runs_with_the_configured_profile_and_an_added_one_with_its_own_folder() {
    let home = Path::new("/u/.alondra");
    let user_home = Path::new("/u");
    let work = AccountId::parse("work").unwrap();
    let explicit = Path::new("/u/.claude-work");
    assert_eq!(profile(home, &AccountId::default_account(), None), None);
    assert_eq!(profile(home, &AccountId::default_account(), Some(explicit)).as_deref(), Some(explicit));
    assert_eq!(profile(home, &work, Some(explicit)), Some(home.join("accounts/work")));
    assert_eq!(config_folder(None, user_home), PathBuf::from("/u/.claude"));
    assert_eq!(config_folder(Some(explicit), user_home), explicit);
    assert_eq!(settings_file(None, user_home), PathBuf::from("/u/.claude.json"));
    assert_eq!(settings_file(Some(explicit), user_home), PathBuf::from("/u/.claude-work/.claude.json"));
}
