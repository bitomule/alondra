use super::*;
use crate::test_support::Directory;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn a_status_says_the_sign_in_its_email_and_plan_and_fails_closed() {
    let status = parse_status(true, br#"{"loggedIn":true,"email":" me@example.org ","subscriptionType":"max","orgId":"x"}"#).unwrap();
    assert_eq!(status, AuthStatus { logged_in: true, email: Some("me@example.org".into()), subscription: Some("max".into()), pay_per_use: false });
    assert_eq!(parse_status(false, br#"{"loggedIn":false}"#), Some(AuthStatus::default()));
    for (ok, bytes) in [(true, b"corrupt".as_slice()), (true, b"{}".as_slice()), (false, br#"{"loggedIn":true}"#.as_slice())] {
        assert_eq!(parse_status(ok, bytes), None);
    }
    assert_eq!(parse_status(true, &vec![b' '; 70_000]), None);
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
fn the_folder_of_an_added_account_is_never_the_default_profile() {
    let of = |pairs: &[(&str, &str)]| {
        let pairs: Vec<(String, String)> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        configured_profile(&|key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone()))
    };
    assert_eq!(of(&[("HOME", "/u"), ("CLAUDE_CONFIG_DIR", "/u/.alondra/accounts/work"), ("CLAUDE_HOME", "/u/.alondra/accounts/work")]), None);
    assert_eq!(of(&[("HOME", "/u"), ("CLAUDE_HOME", "/u/.alondra/accounts/work/")]), None);
    assert_eq!(of(&[("ALONDRA_HOME", "/s"), ("CLAUDE_CONFIG_DIR", "/s/accounts/work")]), None);
    assert_eq!(of(&[("ALONDRA_HOME", "/s"), ("HOME", "/u"), ("CLAUDE_CONFIG_DIR", "/u/.alondra/accounts/work")]), Some(PathBuf::from("/u/.alondra/accounts/work")), "another home's folder is no account of this one");
    assert_eq!(of(&[("HOME", "/u"), ("CLAUDE_CONFIG_DIR", "/u/.alondra/accounts-old")]), Some(PathBuf::from("/u/.alondra/accounts-old")));
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


#[test]
fn native_api_key_helper_and_cloud_auth_are_not_subscription_sign_ins() {
    for method in ["api_key", "api_key_helper", "third_party"] {
        let bytes = serde_json::to_vec(&serde_json::json!({"loggedIn":true,"authMethod":method,"apiProvider":"firstParty"})).unwrap();
        assert!(parse_status(true,&bytes).unwrap().pay_per_use,"{method}");
    }
    let signed_in = parse_status(true,br#"{"loggedIn":true,"authMethod":"claude.ai","subscriptionType":"max"}"#).unwrap();
    assert!(!signed_in.pay_per_use);
    assert_eq!(signed_in.subscription.as_deref(),Some("max"));
    assert!(!parse_status(true,br#"{"loggedIn":true}"#).unwrap().pay_per_use,"older JSON stays compatible");
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
    let user = "ana";
    let user_home = scratch.join("user");
    std::fs::create_dir_all(&user_home).unwrap();
    let account = user_home.join(".alondra/accounts/work");
    std::fs::create_dir_all(&account).unwrap();
    let status = |folder: Option<&Path>| {
        let output = status_command(&script, &BTreeMap::new(), &user_home, &path, user, folder).output().unwrap();
        read_status(output.status.success(), &output.stdout).unwrap()
    };
    let default = status(None);
    assert_eq!(default.email.as_deref(), Some("me@example.org"));
    assert_eq!(default.subscription.as_deref(), Some("pro"));
    assert!(!status(Some(&account)).logged_in);
    std::fs::write(account.join("in"), "").unwrap();
    assert_eq!(status(Some(&account)).email.as_deref(), Some("work@example.org"));
    let vars: Vec<String> = command(&script, &BTreeMap::new(), &user_home, &path, user, None).get_envs().map(|(k, _)| k.to_string_lossy().into_owned()).collect();
    assert_eq!(vars.len(), 4, "{vars:?}");
}

