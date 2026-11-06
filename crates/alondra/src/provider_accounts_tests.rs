use super::*;

fn row(id: &str, email: Option<&str>, state: AccountState) -> Row {
    Row {
        provider: Provider::Claude,
        id: AccountId::parse(id).unwrap(), name: id.to_uppercase(), enabled: true, default: id == "default",
        billing: AccountBilling::default(), state, email: email.map(str::to_owned), plan: Some("max".into()), profile: None,
    }
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

/// Serves `answers` one connection each, in order, and keeps the method and path of every request.
fn fake_daemon(answers: Vec<Value>) -> (Client, std::thread::JoinHandle<Vec<String>>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let client = Client::at(format!("http://{}", listener.local_addr().unwrap()));
    let server = std::thread::spawn(move || answers.into_iter().map(|answer| {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut bytes = Vec::new();
        let request = loop {
            let mut buffer = [0; 4096];
            let count = stream.read(&mut buffer).unwrap(); assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
            if let Some(end) = bytes.windows(4).position(|part| part == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]).into_owned();
                let size: usize = headers.lines().find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|size| size.trim().parse().unwrap())).unwrap_or(0);
                if bytes.len() >= end + 4 + size { break headers.lines().next().unwrap().rsplit_once(' ').unwrap().0.to_owned(); }
            }
        };
        let body = answer.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        request
    }).collect());
    (client, server)
}

#[test]
fn adding_an_account_registers_it_and_tells_the_providers_own_command_without_signing_in() {
    crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
    let config = |codex: Value| json!({"connected": ["claude", "codex"], "distribution": {"policy": "chain", "reserve": 0.9, "move_when_exhausted": false, "order": [{"provider": "claude", "account": "default"}, {"provider": "codex", "account": "default"}]}, "claude": [], "codex": codex});
    let account = |id: &str, name: &str, state: &str, profile: Option<&str>| json!({"provider": "codex", "id": id, "name": name, "enabled": true, "default": id == "default",
        "billing": {"kind": "subscription"}, "state": state, "email": null, "plan": null, "profile": profile});
    let before = json!({"version": "v1", "config": config(json!([])), "supported_providers": ["claude", "codex"],
        "accounts": [account("default", "Default", "signed_in", None)]});
    let after = json!({"version": "v2", "config": config(json!([{"id": "work", "name": "Work"}])), "supported_providers": ["claude", "codex"],
        "accounts": [account("default", "Default", "signed_in", None), account("work", "Work", "signed_out", Some("~/.alondra/provider-accounts/codex/work"))]});
    let (client, server) = fake_daemon(vec![before, after]);
    let lines = add_with(&client, Provider::Codex, "Work").unwrap();
    assert_eq!(server.join().unwrap(), ["GET /settings/accounts?capabilities=provider_accounts", "PUT /settings/accounts?capabilities=provider_accounts"],
        "the account is only written to the settings: no sign-in action reaches the daemon");
    assert_eq!(lines, ["Added Work: signed out", "On the Mac, run CODEX_HOME=~/.alondra/provider-accounts/codex/work codex login"]);
}

#[test]
fn reading_the_accounts_declares_the_capability_so_the_daemon_keeps_codex_rows() {
    let codex = json!({"provider": "codex", "id": "default", "name": "Default", "enabled": true, "default": true, "state": "signed_in"});
    let (client, server) = fake_daemon(vec![json!({"accounts": [codex.clone()]})]);
    assert_eq!(accounts(&client), Some(json!([codex])));
    assert_eq!(server.join().unwrap(), ["GET /settings/accounts?capabilities=provider_accounts"]);
}

#[test]
fn every_accounts_verb_parses_and_only_list_takes_json() {
    use clap::Parser as _;
    let parse = |args: &[&str]| crate::Cli::try_parse_from(std::iter::once("alondra").chain(args.iter().copied()));
    for verb in ["add", "enable", "disable", "remove"] {
        assert!(parse(&["accounts", verb, "My", "Work"]).is_ok(), "{verb}");
        assert!(parse(&["accounts", verb]).is_err(), "{verb} needs a name");
    }
    assert!(parse(&["accounts", "list"]).is_ok());
    assert!(parse(&["accounts", "folder"]).is_err());
    assert!(parse(&["accounts", "login", "Work"]).is_err(), "Alondra never signs in to a provider");
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
