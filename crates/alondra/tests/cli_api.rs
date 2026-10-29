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
