//! The new CLI with the real binary and no daemon: each `--json` command prints one document,
//! the exit codes say who has to act (2 usage, 3 a person, 4 the daemon), and `config set`
//! writes `config.toml` itself while the daemon is stopped.

#[path = "../../test_support.rs"]
mod test_support;

#[cfg(target_os = "macos")]
use std::io::ErrorKind;
#[cfg(target_os = "macos")]
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Command, Output};

struct Mac {
    root: test_support::Directory,
    port: u16,
}

impl Mac {
    fn new() -> Self {
        let root = test_support::Directory::new();
        std::fs::create_dir_all(root.join("home")).unwrap();
        // Darwin rejects destination port zero before any request can reach a daemon.
        Self { root, port: 0 }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_alondra"))
            .args(args)
            .env_clear()
            .env("HOME", &*self.root)
            .env("ALONDRA_HOME", self.root.join("home"))
            .env("ALONDRA_PORT", self.port.to_string())
            .env("ALONDRA_LANG", "en")
            .env("PATH", "/usr/bin:/bin")
            .output()
            .unwrap()
    }

    fn config(&self) -> String {
        std::fs::read_to_string(self.root.join("home/config.toml")).unwrap_or_default()
    }
}

fn document(output: &Output) -> serde_json::Value {
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| panic!("stdout is not one JSON document ({e}): {output:?}"))
}

#[test]
fn report_wait_writes_and_clears_the_file() {
    let root = test_support::Directory::new();
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("wait.json");
    let run = |args: &[&str], configured: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_alondra"));
        command.env_clear().env("HOME", &*root).env("ALONDRA_LANG", "en").args(args);
        if configured { command.env("ALONDRA_WAIT_FILE", &path); }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(output.stdout.is_empty() && output.stderr.is_empty(), "a local report is silent: {output:?}");
    };
    run(&["report-wait", "the deployment", "--progress", "2 of 3", "--detail", "Waiting for acknowledgement", "--pid", &std::process::id().to_string()], true);
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["what"], "the deployment");
    assert_eq!(report["progress"], "2 of 3");
    assert_eq!(report["detail"], "Waiting for acknowledgement");
    assert_eq!(report["pid"], std::process::id());
    assert!(report["since_ms"].as_i64().is_some());
    run(&["report-wait", "the server"], true);
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(report["what"], "the server");
    assert!(report.get("pid").is_none() && report.get("progress").is_none());
    run(&["report-wait", "--clear"], true);
    assert!(!path.exists());
    run(&["report-wait", "--clear"], true);
    run(&["report-wait", "the server"], false);
    run(&["report-wait", "--clear"], false);
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(&*root).unwrap().count(), 0, "no temporary report remains");
}

#[test]
fn each_json_command_prints_one_document_with_its_schema() {
    let mac = Mac::new();
    let list = document(&mac.run(&["config", "list", "--json"]));
    assert_eq!((list["schema"].as_str(), list["ok"].as_bool()), (Some("alondra.config/1"), Some(true)));
    assert_eq!(list["settings"]["port"], mac.port.to_string());
    let one = document(&mac.run(&["--json", "config", "get", "port"]));
    assert_eq!((one["key"].as_str(), one["value"].as_str()), (Some("port"), Some(mac.port.to_string().as_str())));
    let relay = document(&mac.run(&["relay", "status", "--json"]));
    assert_eq!((relay["schema"].as_str(), relay["on"].as_bool()), (Some("alondra.relay/1"), Some(false)));
}

#[test]
fn exit_codes_say_who_has_to_act() {
    let mac = Mac::new();
    let usage = mac.run(&["answer", "q-1", "--json"]);
    assert_eq!(usage.status.code(), Some(2), "{usage:?}");
    assert!(String::from_utf8_lossy(&usage.stderr).contains("--json is not available for `answer`"), "{usage:?}");
    let old = mac.run(&["setup"]);
    assert_eq!(old.status.code(), Some(2), "{old:?}");
    let stderr = String::from_utf8_lossy(&old.stderr);
    assert!(stderr.contains("unrecognized subcommand 'setup'") && stderr.contains("Devices and Macs"), "the new help comes under the error: {stderr}");
    let person = mac.run(&["--no-input", "relay", "on"]);
    assert_eq!(person.status.code(), Some(3), "{person:?}");
    assert!(String::from_utf8_lossy(&person.stderr).contains("alondra relay on in a terminal"), "{person:?}");
    let down = mac.run(&["device", "list"]);
    assert_eq!(down.status.code(), Some(4), "{down:?}");
    assert!(String::from_utf8_lossy(&down.stderr).contains("alondra doctor --fix"), "{down:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn update_finishes_with_the_report_from_the_new_install() {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    let mac = Mac::new();
    let old = mac.root.join("Cellar/alondra/old/bin/alondra");
    let installed = mac.root.join("opt/alondra/bin");
    let tools = mac.root.join("bin");
    std::fs::write(mac.root.join("home/options.toml"), "[accounts]\nnew_provider_setting = true\n").unwrap();
    for path in [old.parent().unwrap(), &installed, &tools] {
        std::fs::create_dir_all(path).unwrap();
    }
    std::fs::copy(env!("CARGO_BIN_EXE_alondra"), &old).unwrap();
    let script = |path: &std::path::Path, body: &str| {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(&tools.join("brew"), "exit 0");
    script(&installed.join("alondrad"), "echo 'alondrad 9.9.9'");
    script(&installed.join("alondra"), r#"
case "$*" in
  'doctor --fix --yes --only service') exit 0 ;;
  'doctor --after-update') echo 'report from installed CLI'; exit 7 ;;
  *) exit 99 ;;
esac"#);
    // Prevent the old doctor's external checks from reaching the user's tools in the red run.
    for name in ["launchctl", "security", "claude", "codex", "gh", "git", "tmux"] {
        script(&tools.join(name), "exit 1");
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let stopped = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let done = Arc::clone(&stopped);
        let server = scope.spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if done.load(Ordering::SeqCst) { break; }
                stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
                let mut request = [0; 4096];
                let _ = stream.read(&mut request);
                let body = r#"{"version":"9.9.9","update":{}}"#;
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        let output = Command::new(&old).arg("update").env_clear()
            .env("HOME", &*mac.root).env("ALONDRA_HOME", mac.root.join("home"))
            .env("ALONDRA_PORT", address.port().to_string()).env("ALONDRA_LANG", "en")
            .env("ALONDRA_UPDATE_URL", format!("http://{address}/latest"))
            .env("PATH", format!("{}:/usr/bin:/bin", tools.display())).output();
        stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(address);
        server.join().unwrap();
        let output = output.unwrap();
        assert_eq!(output.status.code(), Some(7), "{output:?}");
        assert!(String::from_utf8_lossy(&output.stdout).contains("report from installed CLI"), "{output:?}");
    });
}

#[test]
fn config_set_writes_the_file_while_the_daemon_is_stopped() {
    let mac = Mac::new();
    std::fs::write(mac.root.join("home/config.toml"), "auto_update = true\n").unwrap();
    let set = mac.run(&["config", "set", "name", "Ada"]);
    assert!(set.status.success(), "{set:?}");
    assert!(mac.config().contains("name = \"Ada\"") && mac.config().contains("auto_update = true"), "{}", mac.config());
    let refused = mac.run(&["config", "set", "port", "web"]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("port takes a port number from 1 to 65535"), "{refused:?}");
    let unknown = mac.run(&["config", "set", "auto_update", "false"]);
    assert!(String::from_utf8_lossy(&unknown.stderr).contains("auto_update cannot be changed here"), "{unknown:?}");
    assert!(mac.run(&["config", "unset", "name"]).status.success());
    assert!(!mac.config().contains("name"), "{}", mac.config());
    assert!(mac.config().contains("auto_update = true"));
}

#[test]
fn help_more_lists_the_advanced_commands() {
    let mac = Mac::new();
    let more = mac.run(&["help", "more"]);
    assert!(more.status.success(), "{more:?}");
    let text = String::from_utf8_lossy(&more.stdout);
    for command in ["cleanup", "metrics", "state", "delivery", "orchestrator"] {
        assert!(text.contains(&format!("  {command}")), "{command}: {text}");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn stopped_daemon_fixture_uses_an_unclaimable_endpoint() {
    let mac = Mac::new();
    let address = SocketAddr::from(([127, 0, 0, 1], mac.port));
    let competitor = TcpListener::bind(address).unwrap();
    assert_ne!(
        competitor.local_addr().unwrap(), address,
        "daemon-absent endpoint was claimed by another listener"
    );
    let connection = TcpStream::connect(address);
    assert!(
        matches!(&connection, Err(error) if error.kind() == ErrorKind::AddrNotAvailable),
        "offline destination must be rejected locally: {connection:?}"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn send_to_a_task_with_no_node_submits_the_task_action_and_prints_its_receipt() {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    use serde_json::{Value, json};

    let mut mac = Mac::new();
    let key = p256::ecdsa::SigningKey::from_slice(&[9; 32]).unwrap();
    std::fs::write(mac.root.join("home/tui-key.pem"), key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes()).unwrap();
    std::fs::write(mac.root.join("home/tui-device"), "test-terminal").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    mac.port = address.port();
    let stopped = Arc::new(AtomicBool::new(false));
    let done = stopped.clone();
    let server = std::thread::spawn(move || {
        let mut submitted = vec![];
        for stream in listener.incoming() {
            let mut stream = stream.unwrap();
            if done.load(Ordering::SeqCst) { break; }
            stream.set_read_timeout(Some(std::time::Duration::from_secs(5))).unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                assert!(reader.read_line(&mut header).unwrap() > 0);
                if header.trim().is_empty() { break; }
                if let Some(value) = header.to_ascii_lowercase().strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut bytes = vec![0; length];
            reader.read_exact(&mut bytes).unwrap();
            let response = if line.starts_with("GET /views/task%3At-queued?") {
                json!({"protocol":1,"data_epoch":"epoch","view":"task","key":"task:t-queued","client":"tui",
                    "version":"v1","cursor":"c1","status":{"version":"s1","summary":"1 task","badge":0,
                    "counts":[],"gauges":[],"orchestrator":{"text":"Ready","tone":"success"}},
                    "sections":[{"id":"task","collapsed":false,"items":[{"type":"actions","actions":[{
                        "id":"task_msg:t-queued@7","label":"Message","style":"secondary","presentation":"button",
                        "input":{"kind":"text","placeholder":"Write to this task…","required":true,"multiline":true},
                        "attachments":true,"enabled":true,"resolves_card":false}]}]}]})
            } else {
                assert_eq!(line.trim(), "POST /actions HTTP/1.1");
                submitted.push(serde_json::from_slice::<Value>(&bytes).unwrap());
                json!({"protocol":1,"outcome":"applied","stamp":"Message kept: the task's node reads it when it starts"})
            }.to_string();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
        }
        submitted
    });
    let output = mac.run(&["send", "t-queued", "Keep the old API"]);
    stopped.store(true, Ordering::SeqCst);
    let _ = TcpStream::connect(address);
    let submitted = server.join().unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "Message kept: the task's node reads it when it starts");
    assert_eq!(submitted.len(), 1);
    assert_eq!(submitted[0]["action_id"], "task_msg:t-queued@7");
    assert_eq!(submitted[0]["data_epoch"], "epoch");
    assert_eq!(submitted[0]["input"]["text"], "Keep the old API");
    assert_eq!(submitted[0]["input"]["fields"]["from_cli"], true);
}

#[cfg(target_os = "macos")]
#[test]
fn status_json_lists_each_card_once_and_keeps_grouped_task_ids() {
    use std::io::{BufRead, BufReader, Write};
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    use serde_json::{Value, json};

    for (args, empty) in [
        (vec!["status", "--json"], false),
        (vec!["--json", "status"], false),
        (vec!["status", "--json"], true),
    ] {
        let mut mac = Mac::new();
        let key = p256::ecdsa::SigningKey::from_slice(&[9; 32]).unwrap();
        std::fs::write(mac.root.join("home/tui-key.pem"), key.to_pkcs8_pem(LineEnding::LF).unwrap().as_bytes()).unwrap();
        std::fs::write(mac.root.join("home/tui-device"), "test-terminal").unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        mac.port = address.port();
        let stopped = Arc::new(AtomicBool::new(false));
        let done = stopped.clone();
        let server = std::thread::spawn(move || {
            let mut paths = Vec::new();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                if done.load(Ordering::SeqCst) { break; }
                stream.set_read_timeout(Some(std::time::Duration::from_secs(30))).unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut request = String::new();
                reader.read_line(&mut request).unwrap();
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line.trim().is_empty() { break; }
                }
                let home = request.starts_with("GET /views/home?");
                assert!(home || request.starts_with("GET /views/tasks?"), "{request}");
                paths.push(if home { "home" } else { "tasks" });
                let card = |id: &str| json!({"type":"card","id":id,"icon":"question","title":id,"detail":"reference"});
                let task = |id: &str| json!({"id":id,"title":id,"icon":"task","depth":2,"struck":false,
                    "link":{"kind":"view","key":format!("task:{id}")},
                    "badges":[{"text":"Working","tone":"neutral"}]});
                let waits = |id: &str| json!({"id":format!("waits:{id}:dependency"),"title":"Waits for a dependency",
                    "icon":"branch","depth":3,"struck":false,"link":{"kind":"view","key":format!("task:{id}")}});
                let sections: Vec<Value> = if empty { vec![] } else if home {
                    vec![json!({"id":"waiting","collapsed":false,"items":[card("first"),card("second")]}),
                        json!({"id":"detail","collapsed":false,"items":[card("first")]})]
                } else {
                    let mut first = task("t-first");
                    first["children"] = json!([waits("t-first")]);
                    let mut third = task("t-third");
                    third["children"] = json!([task("t-fourth")]);
                    vec![json!({"id":"active","collapsed":false,"items":[{"type":"row","id":"block:in-progress",
                        "title":"In progress","icon":"task","depth":0,"struck":false,"children":[{
                            "id":"group:request","title":"One request","icon":"request","depth":1,"struck":false,
                            "link":{"kind":"view","key":"task:t-second"},
                            "badges":[{"text":"2 tasks","tone":"neutral"}],"children":[first,task("t-second")]}]}]}),
                        json!({"id":"finished","collapsed":false,"items":[
                            {"type":"row","id":"single-request","title":"Finished task","icon":"task","depth":0,"struck":false,
                                "link":{"kind":"view","key":"task:t-finished"},"badges":[{"text":"Done","tone":"success"}],
                                "children":[waits("t-finished")]},
                            {"type":"row","id":"multi-request","title":"Completed request","icon":"request","depth":0,"struck":false,
                                "link":{"kind":"view","key":"task:t-fourth"},"children":[third]},
                            {"type":"row","id":"waits:t-first:another","title":"Waits for another dependency","icon":"branch","depth":1,
                                "struck":false,"link":{"kind":"view","key":"task:t-first"}},
                            {"type":"row","id":"waits:t-outside:dependency","title":"Waits for an omitted dependency","icon":"branch","depth":1,
                                "struck":false,"link":{"kind":"view","key":"task:t-outside"}}
                        ]})]
                };
                let response = json!({"protocol":1,"data_epoch":"epoch","view":if home {"home"} else {"tasks"},
                    "key":if home {"home"} else {"tasks"},"client":"tui","version":"v1","cursor":"c1",
                    "status":{"version":"s1","summary":"Two tasks","badge":2,"counts":[],"gauges":[],
                        "orchestrator":{"text":"Ready","tone":"success"}},"sections":sections}).to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).unwrap();
            }
            paths
        });
        let output = mac.run(&args);
        stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(address);
        let paths = server.join().unwrap();
        let doc = document(&output);
        assert_eq!(paths, ["home", "tasks"]);
        assert_eq!(doc["schema"], "alondra.status/1");
        assert_eq!(doc["header"]["summary"], "Two tasks");
        let cards = doc["cards"].as_array().unwrap();
        let blocks = doc["task_blocks"].as_array().unwrap();
        if empty {
            assert!(cards.is_empty() && blocks.is_empty());
        } else {
            assert_eq!(cards.iter().map(|card| card["id"].as_str().unwrap()).collect::<Vec<_>>(), ["first", "second"]);
            assert_eq!(blocks.len(), 3);
            assert_eq!(blocks[0]["id"], "block:in-progress");
            let first = blocks[0]["tasks"].as_array().unwrap();
            assert_eq!(first.iter().map(|task| task["task_id"].as_str().unwrap()).collect::<Vec<_>>(), ["t-first", "t-second"]);
            assert_eq!(first[1]["title"], "t-second");
            assert_eq!(first[1]["badges"][0]["text"], "Working");
            let single = blocks[1]["tasks"].as_array().unwrap();
            assert_eq!(single.len(), 1);
            assert_eq!(single[0]["task_id"], "t-finished");
            assert_eq!(single[0]["title"], "Finished task");
            assert_eq!(single[0]["badges"][0]["text"], "Done");
            assert_eq!(blocks[2]["tasks"].as_array().unwrap().iter().map(|task| task["task_id"].as_str().unwrap()).collect::<Vec<_>>(),
                ["t-third", "t-fourth"]);
            let tasks: Vec<_> = blocks.iter().flat_map(|block| block["tasks"].as_array().unwrap()).collect();
            let ids: std::collections::BTreeSet<_> = tasks.iter().map(|task| task["task_id"].as_str().unwrap()).collect();
            assert_eq!(ids.len(), tasks.len());
            assert_eq!(ids.len(), 5);
            assert!(tasks.iter().all(|task| task.get("children").is_none()), "task entries contain only their own metadata");
        }
    }
}
