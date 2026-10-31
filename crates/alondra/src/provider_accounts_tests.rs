use super::*;
use crate::test_support::Directory;
use std::os::unix::fs::PermissionsExt as _;

#[test]
fn codex_login_error_exit_cancels_only_its_own_attempt() {
    use std::io::{Read, Write};
    use alondra_core::accounts::AccountKey;
    use alondra_core::domain::{AccountLogin, AccountLoginPhase};
    use alondra_core::views::action_id::{ActionId, Verb};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = Vec::new();
        let body = loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap(); assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let size: usize = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|size| size.trim().parse().unwrap())).unwrap();
                if bytes.len() >= end + 4 + size { break serde_json::from_slice::<Value>(&bytes[end + 4..end + 4 + size]).unwrap(); }
            }
        };
        assert_eq!(body["data_epoch"], "fixture-epoch");
        let action = ActionId::parse(body["action_id"].as_str().unwrap()).unwrap();
        assert_eq!(action.verb, Verb::AccountLoginCancel);
        assert_eq!(action.extra.as_deref(), Some("owned-attempt"));
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
    });
    let fail = || -> Result<()> {
        let _pending = PendingCodexLogin { client: &client, login: AccountLogin {
            account: AccountKey::native(Provider::Codex), attempt: "owned-attempt".into(), started_at: 1,
            phase: AccountLoginPhase::Pending, challenge: None, error: None,
        }, epoch: "fixture-epoch".into(), revision: 3, finished: false };
        bail!("synthetic transport failure")
    };
    assert!(fail().is_err());
    server.join().unwrap();
}

fn row(id: &str, email: Option<&str>, state: AccountState) -> Row {
    Row {
        provider: Provider::Claude,
        id: AccountId::parse(id).unwrap(), name: id.to_uppercase(), enabled: true, default: id == "default",
        billing: AccountBilling::default(), state, email: email.map(str::to_owned), plan: Some("max".into()),
    }
}

#[test]
fn an_email_another_account_has_is_found_regardless_of_case() {
    let rows = [
        row("default", Some("Me@Example.org"), AccountState::SignedIn),
        row("work", Some("me@EXAMPLE.org"), AccountState::SignedIn),
        row("team", Some("team@example.org"), AccountState::SignedIn),
        row("new", None, AccountState::SignedOut),
    ];
    assert_eq!(same_email(&rows, &rows[1].id).unwrap().id.as_str(), "default");
    assert!(same_email(&rows, &rows[2].id).is_none());
    assert!(same_email(&rows, &rows[3].id).is_none(), "an account without a sign-in repeats nobody");
}

#[test]
fn the_list_shows_name_email_plan_and_state_and_never_a_folder() {
    let mut payg = row("api", Some("api@example.org"), AccountState::SignedIn);
    payg.billing = AccountBilling::PayPerUse;
    let listed = lines(&[row("default", Some("me@example.org"), AccountState::SignedIn), row("work", None, AccountState::SignedOut), payg]);
    let english = ["DEFAULT  me@example.org · max · signed in", "WORK     max · signed out", "API      api@example.org · pay per use · signed in"];
    let spanish = ["DEFAULT  me@example.org · max · con sesión", "WORK     max · sin sesión", "API      api@example.org · pago por uso · con sesión"];
    assert!(listed == english || listed == spanish, "{listed:?}");
    let mut unnamed = row("default", None, AccountState::SignedOut);
    unnamed.name = "Default".into();
    let shown = lines(&[unnamed, row("work", None, AccountState::SignedOut)]);
    assert!(shown[0].starts_with("Default  ") || shown[0].starts_with("Principal  "), "{shown:?}");
    assert!(shown[1].starts_with("WORK "), "{shown:?}");
    assert!(listed.iter().all(|line| !line.contains('/')));
}

#[test]
fn the_daemons_listing_is_read_with_the_file_section_as_it_is() {
    let body = json!({
        "version": "abc",
        "config": {"distribution": {"policy": "margin", "reserve": 0.9, "move_when_exhausted": false},
            "claude": [{"id": "work", "name": "Work"}, {"id": "default", "name": "Personal"}]},
        "accounts": [
            {"provider": "claude", "id": "work", "name": "Work", "enabled": true, "default": false, "billing": {"kind": "subscription"}, "state": "signed_out", "email": null, "plan": null},
            {"provider": "claude", "id": "default", "name": "Personal", "enabled": true, "default": true, "billing": {"kind": "subscription"}, "state": "signed_in", "email": "me@example.org", "plan": "max"},
        ],
    });
    let listing: Listing = serde_json::from_value(body).unwrap();
    assert_eq!(listing.find("personal").unwrap().id.as_str(), "default");
    assert_eq!(listing.find("work").unwrap().state, AccountState::SignedOut);
    assert!(listing.find("nobody").is_err());
    let without = listing.without(&AccountId::parse("work").unwrap());
    assert_eq!(without.claude.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["default"], "the default keeps its place in the file");
}

#[test]
fn claude_runs_for_an_account_with_its_folder_and_for_the_default_with_the_daemons_profile() {
    let scratch = Directory::new();
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("claude"), "#!/bin/sh\nprintf '%s' \"${CLAUDE_CONFIG_DIR:-none}\" > \"$HOME/seen\"\n").unwrap();
    std::fs::set_permissions(bin.join("claude"), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = scratch.join("alondra");
    let explicit = scratch.join("user/.claude-work");
    let work = claude_for(&home, &AccountId::parse("work").unwrap(), Some(&explicit));
    assert_eq!(work.config_dir.as_deref(), Some(home.join("accounts/work").to_str().unwrap()));
    assert_eq!(claude_for(&home, &AccountId::default_account(), None).config_dir, None, "Claude's default profile names no folder");
    assert_eq!(claude_for(&home, &AccountId::default_account(), Some(&explicit)).config_dir.as_deref(), explicit.to_str(),
        "the default account signs in to the profile the daemon's nodes run with");
    let context = crate::claude_auth::ContextEnv { home: scratch.join("user"), path: bin.display().to_string(), config_dir: work.config_dir.clone() };
    std::fs::create_dir_all(&context.home).unwrap();
    context.sign_out().unwrap();
    assert_eq!(std::fs::read_to_string(context.home.join("seen")).unwrap(), home.join("accounts/work").display().to_string());
}

#[test]
fn every_accounts_verb_parses_and_only_list_takes_json() {
    use clap::Parser as _;
    let parse = |args: &[&str]| crate::Cli::try_parse_from(std::iter::once("alondra").chain(args.iter().copied()));
    for verb in ["add", "login", "enable", "disable", "remove"] {
        assert!(parse(&["accounts", verb, "My", "Work"]).is_ok(), "{verb}");
        assert!(parse(&["accounts", verb]).is_err(), "{verb} needs a name");
    }
    assert!(parse(&["accounts", "list"]).is_ok());
    assert!(parse(&["accounts", "folder"]).is_err());
    assert!(crate::takes_json(parse(&["accounts", "list"]).unwrap().command.as_ref()));
    assert!(!crate::takes_json(parse(&["accounts", "add", "Work"]).unwrap().command.as_ref()));
}

#[test]
fn moving_to_another_account_says_whether_it_is_on_and_its_risk() {
    let english = ["Move a node to another account when its own reaches its limit: off",
        "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed."];
    let spanish = ["Mover un nodo a otra cuenta cuando la suya llega al límite: desactivado",
        "Anthropic no dice si cambiar de cuenta al llegar al límite está permitido."];
    let off = moving_lines(false);
    assert!(off == english || off == spanish, "{off:?}");
    let on = moving_lines(true);
    assert!(on[0].ends_with(": on") || on[0].ends_with(": activado"), "{on:?}");
}
