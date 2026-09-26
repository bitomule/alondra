//! Verify the executable's early-exit boundary, including invalid configuration and occupied ports.
use std::{fs, io::Read, net::TcpListener, path::PathBuf, process::{Command, Stdio}, time::{Duration, Instant}};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!("alondrad-info-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::write(root.join("config.toml"), "not valid TOML {{{").unwrap();
        Self(root)
    }
    fn output(&self, args: &[&str], port: u16) -> (bool, String, String) {
        let mut child = Command::new(env!("CARGO_BIN_EXE_alondrad"))
            .args(args).env_clear().env("HOME", &self.0).env("ALONDRA_HOME", &self.0)
            .env("ALONDRA_LISTEN", "127.0.0.1").env("ALONDRA_PORT", port.to_string())
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            if Instant::now() >= deadline {
                let _ = child.kill(); let _ = child.wait();
                panic!("informational command did not exit promptly: {args:?}");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let mut stdout = String::new(); let mut stderr = String::new();
        child.stdout.take().unwrap().read_to_string(&mut stdout).unwrap();
        child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
        (status.success(), stdout, stderr)
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

#[test]
fn help_and_version_exit_before_config_runtime_and_daemon_side_effects() {
    let fixture = Fixture::new();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();
    for flag in ["--help", "-h", "--version", "-V"] {
        let (success, out, err) = fixture.output(&[flag], port);
        assert!(success, "{flag}: {err}");
        assert!(err.is_empty(), "{flag}: {err}");
        if flag == "--help" || flag == "-h" {
            assert!(out.contains("Usage: alondrad"));
            assert!(out.contains("--test-service-push <device-id>"));
        } else { assert_eq!(out.trim(), format!("alondrad {}", env!("CARGO_PKG_VERSION"))); }
    }
    // Prove the fixture would fail if informational arguments fell through to startup.
    let (success, _, err) = fixture.output(&[], port);
    assert!(!success);
    assert!(err.contains("config.toml unreadable"), "{err}");
    for args in [vec!["--help", "extra"], vec!["--version", "--help"], vec!["--test-service-push"], vec!["--test-service-push", "--help"], vec!["--test-service-push", "phone-1", "extra"]] {
        let (success, _, err) = fixture.output(&args, port);
        assert!(!success, "{args:?}");
        assert!(err.contains("usage: alondrad"), "{err}");
        assert!(!err.contains("config.toml unreadable"), "{err}");
    }
    let files: Vec<_> = fs::read_dir(&fixture.0).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(files, ["config.toml"]);
    assert_eq!(fs::read_to_string(fixture.0.join("config.toml")).unwrap(), "not valid TOML {{{");
}
