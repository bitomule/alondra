//! Verify the executable's early-exit boundary, including invalid configuration and occupied ports.
#[path = "../../test_support.rs"]
mod test_support;

use std::{fs, io::Read, net::TcpListener, process::{Command, Stdio}, time::{Duration, Instant}};

const HANG_GUARD: Duration = Duration::from_secs(300);

struct Fixture(crate::test_support::Directory);
impl Fixture {
    fn new() -> Self {
        let root = crate::test_support::Directory::new();
        fs::create_dir(&root).unwrap();
        fs::write(root.join("config.toml"), "not valid TOML {{{").unwrap();
        Self(root)
    }
    fn output(&self, args: &[&str], port: u16) -> (bool, String, String) {
        let scratch = crate::test_support::Directory::new();
        fs::create_dir(&scratch).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_alondrad"))
            .args(args).env_clear().env("HOME", &self.0).env("ALONDRA_HOME", &self.0)
            .env("TMPDIR", &scratch)
            .env("ALONDRA_LISTEN", "127.0.0.1").env("ALONDRA_PORT", port.to_string())
            .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let hang_guard = Instant::now() + HANG_GUARD;
        let status = loop {
            if let Some(status) = child.try_wait().unwrap() { break status; }
            if Instant::now() >= hang_guard {
                let _ = child.kill(); let _ = child.wait();
                panic!("hang guard: alondrad {args:?} still running after {HANG_GUARD:?}");
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
    for args in [vec!["--help", "extra"], vec!["--version", "--help"], vec!["--test-service-push"], vec!["--test-service-push", "--help"], vec!["--test-service-push", "phone-1", "extra"], vec!["--check-database"], vec!["--check-database", "--help"], vec!["--check-database", "db", "extra"]] {
        let (success, _, err) = fixture.output(&args, port);
        assert!(!success, "{args:?}");
        assert!(err.contains("usage: alondrad"), "{err}");
        assert!(!err.contains("config.toml unreadable"), "{err}");
    }
    let files: Vec<_> = fs::read_dir(&fixture.0).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(files, ["config.toml"]);
    assert_eq!(fs::read_to_string(fixture.0.join("config.toml")).unwrap(), "not valid TOML {{{");
}

#[test]
fn database_check_is_read_only_and_exits_before_configuration_or_listeners() {
    let fixture = Fixture::new();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = occupied.local_addr().unwrap().port();
    let path = fixture.0.join("evidence.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF; CREATE TABLE parents(id INTEGER PRIMARY KEY);
        CREATE TABLE children(parent INTEGER REFERENCES parents(id)); INSERT INTO parents VALUES(1);").unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], port);
    assert!(!success);
    assert!(err.contains("requires the existing service-home.lock"), "{err}");
    assert!(!fixture.0.join("service-home.lock").exists());
    assert_eq!(fs::read(&path).unwrap(), before);
    fs::write(fixture.0.join("service-home.lock"), []).unwrap();
    let (success, out, err) = fixture.output(&["--check-database", path.to_str().unwrap()], port);
    assert!(success, "{err}");
    assert!(out.starts_with("database check ok:"), "{out}");
    assert_eq!(fs::read(&path).unwrap(), before);

    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.pragma_update(None, "foreign_keys", false).unwrap();
    connection.execute("INSERT INTO children VALUES(42)", []).unwrap();
    drop(connection);
    let before = fs::read(&path).unwrap();
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], port);
    assert!(!success);
    assert!(err.contains("foreign key check failed"), "{err}");
    assert_eq!(fs::read(&path).unwrap(), before);

    let missing = fixture.0.join("missing.db");
    let (success, _, err) = fixture.output(&["--check-database", missing.to_str().unwrap()], port);
    assert!(!success);
    assert!(!err.contains("config.toml unreadable"), "{err}");
    let mut files: Vec<_> = fs::read_dir(&fixture.0).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    files.sort();
    assert_eq!(files, ["config.toml", "evidence.db", "service-home.lock"]);
}

#[test]
fn database_check_reports_corruption_in_an_unused_btree() {
    use std::os::unix::fs::FileExt;
    let fixture = Fixture::new();
    fs::write(fixture.0.join("service-home.lock"), []).unwrap();
    let path = fixture.0.join("evidence.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("CREATE TABLE evidence(id INTEGER PRIMARY KEY, body BLOB); INSERT INTO evidence VALUES(1,zeroblob(8192));").unwrap();
    let page: i64 = connection.query_row("SELECT rootpage FROM sqlite_schema WHERE name='evidence'", [], |row| row.get(0)).unwrap();
    let size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0)).unwrap();
    drop(connection);
    let file = fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all_at(&[0xff], u64::try_from((page - 1) * size).unwrap()).unwrap();
    drop(file);
    let before = fs::read(&path).unwrap();
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], 0);
    assert!(!success);
    assert!(err.contains("integrity check failed") || err.contains("malformed"), "{err}");
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn database_check_preserves_wal_files_and_refuses_a_running_home() {
    fn files(fixture: &Fixture) -> std::collections::BTreeMap<std::ffi::OsString, Vec<u8>> {
        fs::read_dir(&fixture.0).unwrap().map(|entry| {
            let entry = entry.unwrap();
            (entry.file_name(), fs::read(entry.path()).unwrap())
        }).collect()
    }
    let fixture = Fixture::new();
    let path = fixture.0.join("evidence.db");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA journal_mode=WAL; CREATE TABLE parents(id INTEGER PRIMARY KEY);
        CREATE TABLE children(parent INTEGER REFERENCES parents(id)); INSERT INTO children VALUES(42);").unwrap();
    let lock = fs::OpenOptions::new().write(true).create_new(true).open(fixture.0.join("service-home.lock")).unwrap();
    lock.lock_shared().unwrap();
    let before = files(&fixture);
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], 0);
    assert!(!success);
    assert!(err.contains("HOME is busy"), "{err}");
    assert_eq!(files(&fixture), before);
    lock.unlock().unwrap();

    // All schema and the violation exist only in WAL. The private copy must include them.
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], 0);
    assert!(!success);
    assert!(err.contains("foreign key check failed"), "{err}");
    assert_eq!(files(&fixture), before);
    connection.execute("DELETE FROM children", []).unwrap();
    let before = files(&fixture);
    let (success, out, err) = fixture.output(&["--check-database", path.to_str().unwrap()], 0);
    assert!(success, "{err}");
    assert!(out.starts_with("database check ok:"), "{out}");
    assert_eq!(files(&fixture), before);
    drop(connection);
    assert!(!fixture.0.join("evidence.db-wal").exists());
    let before = files(&fixture);
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], 0);
    assert!(success, "{err}");
    assert_eq!(files(&fixture), before, "a stopped WAL database must gain no source sidecars");
}

#[test]
fn database_check_recovers_a_hot_journal_only_on_its_private_copy() {
    let origin = Fixture::new();
    let source = origin.0.join("source.db");
    let connection = rusqlite::Connection::open(&source).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA cache_size=1;
        CREATE TABLE parents(id INTEGER PRIMARY KEY); CREATE TABLE children(parent INTEGER REFERENCES parents(id));
        CREATE TABLE evidence(body BLOB);
        WITH RECURSIVE rows(n) AS (VALUES(1) UNION ALL SELECT n+1 FROM rows WHERE n<200)
        INSERT INTO evidence SELECT zeroblob(8192) FROM rows;
        BEGIN IMMEDIATE; INSERT INTO children VALUES(42); UPDATE evidence SET body=zeroblob(8193);").unwrap();
    let fixture = Fixture::new();
    fs::write(fixture.0.join("service-home.lock"), []).unwrap();
    let path = fixture.0.join("evidence.db");
    let journal = fixture.0.join("evidence.db-journal");
    fs::copy(&source, &path).unwrap();
    fs::copy(origin.0.join("source.db-journal"), &journal).unwrap();
    connection.execute_batch("ROLLBACK").unwrap();
    let (before, before_journal) = (fs::read(&path).unwrap(), fs::read(&journal).unwrap());
    assert_ne!(&before_journal[..8], &[0; 8], "the fixture must have a hot journal header");
    let (success, _, err) = fixture.output(&["--check-database", path.to_str().unwrap()], 0);
    assert!(success, "{err}");
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(fs::read(&journal).unwrap(), before_journal);
}

#[test]
fn a_legacy_home_is_refused_before_the_daemon_takes_its_lock() {
    let root = crate::test_support::Directory::new();
    fs::create_dir(&root).unwrap();
    let fixture = Fixture(root);
    let root = &fixture.0;
    fs::write(root.join("alondra.db"), b"legacy bytes").unwrap();
    let cutover = fs::OpenOptions::new().create(true).truncate(false).write(true)
        .open(root.join("service-home.lock")).unwrap();
    cutover.lock().unwrap();
    let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let (success, _, err) = fixture.output(&[], port);
    assert!(!success);
    assert!(err.contains("still holds legacy data"), "{err}");
    assert!(!err.contains("busy"), "the refusal must come before the HOME lock: {err}");
    cutover.unlock().unwrap();
}
