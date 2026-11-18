use super::*;
use crate::client_routes::tests::Fixture;
use serde_json::Value;

struct Mac {
    alondra: crate::test_support::Directory,
    user: crate::test_support::Directory,
}

impl Mac {
    fn new() -> Self {
        let (alondra, user) = (crate::test_support::Directory::new(), crate::test_support::Directory::new());
        std::fs::create_dir(&*alondra).unwrap();
        std::fs::create_dir(&*user).unwrap();
        for name in ["a", "b", "c"] {
            std::fs::create_dir_all(user.join("Projects").join(name).join(".git")).unwrap();
        }
        std::fs::create_dir_all(user.join("Developer/x/.git")).unwrap();
        std::fs::create_dir_all(user.join("Notes/today")).unwrap();
        std::fs::create_dir_all(user.join("Documents/repo/.git")).unwrap();
        Self { alondra, user }
    }

    fn options(&self) -> String {
        std::fs::read_to_string(alondra_core::options::path_in(&self.alondra)).unwrap_or_default()
    }

    fn put(&self, body: Value, lang: Lang) -> Reply {
        write(&self.alondra, &self.user, false, &body, lang)
    }
}

#[test]
fn reading_gives_the_folder_its_candidates_and_the_relay() {
    let mac = Mac::new();
    let reply = read(&mac.alondra, &mac.user, true);
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.body["projects_root"], json!(mac.user.join("Projects")));
    assert_eq!(reply.body["candidates"], json!([
        {"path": mac.user.join("Projects"), "repos": 3},
        {"path": mac.user.join("Developer"), "repos": 1},
    ]));
    assert_eq!(reply.body["relay"], json!({"on": true}));
    assert_eq!(read(&mac.alondra, &mac.user, false).body["relay"], json!({"on": false}));
}

#[test]
fn writing_keeps_the_rest_of_options_and_creates_a_missing_folder() {
    let mac = Mac::new();
    std::fs::write(alondra_core::options::path_in(&mac.alondra), "# mine\nprojects_root = \"~/Projects\"\n\n# tail\n").unwrap();
    let reply = mac.put(json!({"projects_root": "~/Code/new"}), Lang::En);
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    let chosen = mac.user.join("Code/new");
    assert!(chosen.is_dir());
    assert_eq!(reply.body["projects_root"], json!(chosen));
    assert_eq!(reply.body["candidates"][0], json!({"path": chosen, "repos": 0}));
    assert_eq!(mac.options(), format!("# mine\nprojects_root = {:?}\n\n# tail\n", chosen.display().to_string()));
}

#[test]
fn a_folder_outside_home_is_accepted() {
    let mac = Mac::new();
    let outside = crate::test_support::Directory::new();
    let reply = mac.put(json!({"projects_root": outside.join("repos")}), Lang::En);
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert!(outside.join("repos").is_dir());
}

#[test]
fn an_unusable_folder_is_refused_with_its_reason_and_nothing_changes() {
    let mac = Mac::new();
    std::fs::write(mac.user.join("file"), "").unwrap();
    std::fs::write(alondra_core::options::path_in(&mac.alondra), "projects_root = \"/kept\"\n").unwrap();
    for (value, code) in [("~/file", "not_a_directory"), ("~/file/sub", "not_a_directory"), ("/", "whole_disk"), ("", "empty"), ("  ", "empty")] {
        let reply = mac.put(json!({"projects_root": value}), Lang::En);
        assert_eq!((reply.status, reply.body["error"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some(code)), "{value}");
        assert!(reply.body["message"].as_str().is_some_and(|m| !m.is_empty()));
        assert_eq!(mac.options(), "projects_root = \"/kept\"\n", "{value}");
    }
    assert!(!mac.user.join("file/sub").exists());
}

#[test]
fn the_reason_is_in_the_device_language_unless_the_body_names_one() {
    let mac = Mac::new();
    let spanish = mac.put(json!({"projects_root": "/"}), Lang::Es);
    assert_eq!(spanish.body["message"], "/ no puede ser la carpeta de proyectos: es todo el disco.");
    let english = mac.put(json!({"projects_root": "/", "lang": "en"}), Lang::Es);
    assert_eq!(english.body["message"], "/ can't be the projects folder: it holds the whole disk.");
}

#[test]
fn a_body_with_anything_else_is_a_bad_request() {
    let mac = Mac::new();
    for body in [json!({"projects_root": "~/x", "name": "Alice"}), json!({}), json!({"projects_root": "~/x", "lang": "fr"})] {
        assert_eq!(mac.put(body.clone(), Lang::En).status, StatusCode::BAD_REQUEST, "{body}");
    }
    assert_eq!(mac.options(), "");
}

#[test]
fn two_writes_in_a_row_leave_the_last_one() {
    let mac = Mac::new();
    assert_eq!(mac.put(json!({"projects_root": "~/First"}), Lang::En).status, StatusCode::OK);
    assert_eq!(mac.put(json!({"projects_root": "~/Second"}), Lang::En).status, StatusCode::OK);
    assert_eq!(read(&mac.alondra, &mac.user, false).body["projects_root"], json!(mac.user.join("Second")));
    assert_eq!(mac.options(), format!("projects_root = {:?}\n", mac.user.join("Second").display().to_string()));
}

#[test]
fn writes_at_the_same_time_all_land_and_leave_a_valid_file() {
    let mac = Mac::new();
    std::fs::write(alondra_core::options::path_in(&mac.alondra), "# mine\n").unwrap();
    for round in 0..20 {
        let replies: Vec<Reply> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8).map(|n| {
                let mac = &mac;
                scope.spawn(move || mac.put(json!({"projects_root": format!("~/Folder-{round}-{n}")}), Lang::En))
            }).collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        for reply in &replies {
            assert_eq!(reply.status, StatusCode::OK, "round {round}: {}", reply.body);
        }
        let text = mac.options();
        let file = alondra_core::options::OptionsFile::parse(&text).unwrap();
        let saved = file.projects_root.unwrap();
        assert!((0..8).any(|n| saved == mac.user.join(format!("Folder-{round}-{n}")).display().to_string()), "{saved}");
        assert!(text.ends_with("# mine\n"), "{text}");
    }
}

#[test]
fn folders_lists_home_and_refuses_anything_outside() {
    let mac = Mac::new();
    let top = folders(&mac.user, None);
    assert_eq!(top.status, StatusCode::OK);
    assert_eq!(top.body["parent"], Value::Null);
    let names: Vec<&str> = top.body["folders"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Developer", "Notes", "Projects"], "Documents is protected and left out");
    std::fs::write(mac.user.join("file"), "").unwrap();
    for path in [PathBuf::from("/"), mac.user.join("missing"), mac.user.join("file"), PathBuf::from("Projects"), mac.user.join("Documents")] {
        let refused = folders(&mac.user, Some(&path));
        assert_eq!((refused.status, refused.body["error"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")), "{}", path.display());
    }
}

#[test]
fn folders_queries_are_named_exactly() {
    assert_eq!(folders_query("/settings/folders"), Some(None));
    assert_eq!(folders_query("/settings/folders?path=%2FUsers%2Fme"), Some(Some(PathBuf::from("/Users/me"))));
    for path in ["/settings/folders?path=", "/settings/folders?x=1", "/settings/folders?path=a&path=b", "/settings/foldersx", "/settings"] {
        assert_eq!(folders_query(path), None, "{path}");
    }
}

async fn unsigned(fixture: &Fixture, method: reqwest::Method, path: &str, body: Option<Value>, origin: bool) -> (StatusCode, Value) {
    let mut request = reqwest::Client::new().request(method, format!("{}{path}", fixture.base));
    if origin {
        request = request.header("origin", "https://example.com");
    }
    if let Some(body) = body {
        request = request.header("content-type", "application/json").body(serde_json::to_vec(&body).unwrap());
    }
    let response = request.send().await.unwrap();
    let status = StatusCode::from_u16(response.status().as_u16()).unwrap();
    (status, response.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn this_mac_reads_and_changes_the_settings_without_signing() {
    let fixture = Fixture::start().await;
    let (status, body) = unsigned(&fixture, reqwest::Method::GET, "/settings", None, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["relay"], json!({"on": false}));
    let chosen = fixture.home.join("Chosen");
    let (status, body) = unsigned(&fixture, reqwest::Method::PUT, "/settings", Some(json!({"projects_root": chosen})), false).await;
    assert_eq!((status, &body["projects_root"]), (StatusCode::OK, &json!(chosen)), "{body}");
    let (status, body) = unsigned(&fixture, reqwest::Method::PUT, "/settings", Some(json!({"projects_root": "/"})), false).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some("whole_disk")));
    let (status, _) = unsigned(&fixture, reqwest::Method::GET, "/settings/folders", None, false).await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = unsigned(&fixture, reqwest::Method::GET, "/settings/folders?path=%2F", None, false).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::NOT_FOUND, Some("not_found")));
}

#[tokio::test]
async fn two_writes_in_a_row_over_http_leave_the_last_one() {
    let fixture = Fixture::start().await;
    let (first, second) = (fixture.home.join("First"), fixture.home.join("Second"));
    for folder in [&first, &second] {
        let (status, body) = fixture.send("PUT", "/settings", &json!({"projects_root": folder})).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, body) = fixture.get("/settings").await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(body["projects_root"], json!(second));
}

#[tokio::test]
async fn a_paired_device_may_and_a_revoked_one_may_not() {
    let fixture = Fixture::start().await;
    let (status, _) = fixture.get("/settings").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = fixture.get("/settings/folders").await;
    assert_eq!(status, StatusCode::OK);
    let (status, body) = fixture.send("PUT", "/settings", &json!({"projects_root": fixture.home.join("Signed")})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(crate::auth::Auth::open(&fixture.home).unwrap().revoke(fixture.device_id()).unwrap());
    let (status, _) = fixture.get("/settings").await;
    assert_ne!(status, StatusCode::OK);
    let (status, _) = fixture.send("PUT", "/settings", &json!({"projects_root": fixture.home.join("Revoked")})).await;
    assert_ne!(status, StatusCode::OK);
    assert!(!fixture.home.join("Revoked").exists());
}

#[tokio::test]
async fn a_web_page_on_this_mac_is_refused() {
    let fixture = Fixture::start().await;
    let target = fixture.home.join("FromWeb");
    for (method, path, body) in [
        (reqwest::Method::PUT, "/settings", Some(json!({"projects_root": target}))),
        (reqwest::Method::GET, "/settings", None),
        (reqwest::Method::GET, "/settings/folders", None),
    ] {
        let (status, _) = unsigned(&fixture, method.clone(), path, body, true).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}");
    }
    assert!(!target.exists());
}

#[test]
fn config_keys_are_written_and_say_whether_the_daemon_restarts() {
    let mac = Mac::new();
    let reply = write_config(&mac.alondra, &json!({"key": "port", "value": "47900"}));
    assert_eq!((reply.status, &reply.body), (StatusCode::OK, &json!({"key": "port", "restart": true})));
    let reply = write_config(&mac.alondra, &json!({"key": "name", "value": "Ada"}));
    assert_eq!(reply.body["restart"], json!(false));
    let file = std::fs::read_to_string(alondra_core::config::path_in(&mac.alondra)).unwrap();
    assert!(file.contains("port = 47900") && file.contains("name = \"Ada\""), "{file}");
    assert_eq!(write_config(&mac.alondra, &json!({"key": "name", "value": null})).status, StatusCode::OK);
    assert!(!std::fs::read_to_string(alondra_core::config::path_in(&mac.alondra)).unwrap().contains("name"));
    for (body, code) in [
        (json!({"key": "auto_update", "value": "true"}), "unknown_key"),
        (json!({"key": "port", "value": "web"}), "invalid_value"),
    ] {
        let reply = write_config(&mac.alondra, &body);
        assert_eq!((reply.status, reply.body["error"].as_str()), (StatusCode::UNPROCESSABLE_ENTITY, Some(code)), "{body}");
    }
    assert_eq!(write_config(&mac.alondra, &json!({"key": "port", "value": "1", "extra": 1})).status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn only_this_macs_terminal_changes_config_keys() {
    let fixture = Fixture::start().await;
    let change = json!({"key": "public_url", "value": "https://mac.example"});
    let (status, body) = fixture.send("PUT", "/settings/config", &change).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a paired device may not: {body}");
    let (status, _) = unsigned(&fixture, reqwest::Method::PUT, "/settings/config", Some(change.clone()), true).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a web page on this Mac may not");
    assert!(!alondra_core::config::path_in(&fixture.home).exists());
    let (status, body) = unsigned(&fixture, reqwest::Method::PUT, "/settings/config", Some(change), false).await;
    assert_eq!((status, &body), (StatusCode::OK, &json!({"key": "public_url", "restart": false})));
    let file = std::fs::read_to_string(alondra_core::config::path_in(&fixture.home)).unwrap();
    assert!(file.contains("public_url = \"https://mac.example\""), "{file}");
}

#[tokio::test]
async fn this_mac_reads_and_changes_the_accounts_and_a_web_page_may_not() {
    let fixture = Fixture::start_with_accounts(Some(alondra_core::accounts::AccountsConfig::default())).await;
    let (status, body) = unsigned(&fixture, reqwest::Method::GET, crate::accounts::PATH, None, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accounts"].as_array().unwrap().len(), 1, "{body}");
    assert_eq!((&body["accounts"][0]["id"], &body["accounts"][0]["default"]), (&json!("default"), &json!(true)));
    assert_eq!(body["config"], json!({"distribution": {"policy": "margin", "reserve": 0.9, "move_when_exhausted": false}, "claude": []}));
    assert_eq!(body["move_when_exhausted"], json!({"on": false, "risk": "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed."}),
        "moving a node off its account's limit is off, and says its risk");
    let version = body["version"].clone();
    let chain = json!({"distribution": {"policy": "chain", "reserve": 0.9, "move_when_exhausted": true}, "claude": []});
    let (status, body) = unsigned(&fixture, reqwest::Method::PUT, crate::accounts::PATH, Some(json!({"version": "0", "accounts": chain})), false).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::CONFLICT, Some("stale")), "{body}");
    let (status, _) = unsigned(&fixture, reqwest::Method::PUT, crate::accounts::PATH, Some(json!({"version": version, "accounts": chain})), true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, body) = unsigned(&fixture, reqwest::Method::PUT, crate::accounts::PATH, Some(json!({"version": version, "accounts": chain})), false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["config"]["distribution"]["policy"], "chain");
    assert_eq!(body["move_when_exhausted"]["on"], true, "{body}");
    assert_ne!(body["version"], version);
    let options = std::fs::read_to_string(alondra_core::options::path_in(&fixture.home)).unwrap();
    assert!(options.contains("[accounts.distribution]\npolicy = \"chain\""), "{options}");
    assert!(options.contains("move_when_exhausted = true"), "{options}");
    let (status, body) = unsigned(&fixture, reqwest::Method::POST, crate::accounts::CHECK, None, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["accounts"][0]["state"], "unavailable", "without Claude Code no sign-in can be checked: {body}");
}

#[test]
fn worktrees_root_config_pins_its_volume_and_unset_restores_default_without_losing_options() {
    let mac = Mac::new();
    let path = mac.user.join("worktrees");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(alondra_core::options::path_in(&mac.alondra), "max_working_nodes = 3\n").unwrap();
    let reply = write_config(&mac.alondra, &json!({"key": "worktrees_root", "value": path}));
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.body);
    assert_eq!(reply.body["restart"], false);
    let file = alondra_core::options::OptionsFile::parse(&mac.options()).unwrap();
    let storage = file.worktrees_root.unwrap();
    assert_eq!(storage.path, path);
    assert!(!storage.volume_id.is_empty());
    let saved = mac.options();
    assert_eq!(write_config(&mac.alondra, &json!({"key": "worktrees_root", "value": path.join("missing")})).status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(mac.options(), saved);
    assert_eq!(write_config(&mac.alondra, &json!({"key": "worktrees_root", "value": null})).status, StatusCode::OK);
    let file = alondra_core::options::OptionsFile::parse(&mac.options()).unwrap();
    assert_eq!(file.worktrees_root, None);
    assert_eq!(file.max_working_nodes, alondra_core::options::NodeLimit::Fixed(3));
}
