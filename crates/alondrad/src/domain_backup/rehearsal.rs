//! Cutover rehearsal against a read-only snapshot of a live HOME.
//!
//! The live HOME is only read: SQLite through read-only connections and the
//! backup API (a WAL database is copied with its committed WAL frames), other
//! files through plain reads. No lock is taken on it and nothing is written to
//! it. Backup, archive and restore run through this binary's own CLI against
//! the snapshot. The daemon then starts on the empty domain v2 store with its
//! relay and APNs configuration removed, wake off, a loopback update URL, a
//! private HOME and TMPDIR, and a PATH whose `claude`, `gh`, `git` and friends
//! only record that they were called.
use super::*;
use crate::domain_runtime::DomainRuntime;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};

/// Every database a backup keeps, plus the domain sidecar.
pub(crate) const SNAPSHOT_DATABASES: [&str; 5] = [
    "schedules.db",
    "alondra.db",
    "service-receipts.sqlite",
    "service-upload-attempts.sqlite",
    "domain-v2.db",
];
/// Files that would reach relay, APNs or a paired device from the rehearsal daemon.
const OUTWARD_FILES: [&str; 3] = ["service.toml", "service.toml.staged", "apns.json"];
const STUBBED_COMMANDS: [&str; 11] = [
    "claude", "gh", "git", "tmux", "brew", "launchctl", "osascript", "open", "curl",
    "terminal-notifier", "ssh",
];

#[derive(Debug, Serialize)]
pub(crate) struct Check {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Report {
    pub live_home: String,
    pub work: String,
    pub numbers: BTreeMap<String, Value>,
    pub checks: Vec<Check>,
    pub timings_ms: BTreeMap<String, u128>,
}

impl Report {
    fn check(&mut self, name: impl Into<String>, ok: bool, detail: impl Into<String>) -> bool {
        self.checks.push(Check { name: name.into(), ok, detail: detail.into() });
        ok
    }
    fn fail(&mut self, name: &str, error: &anyhow::Error) {
        self.check(name, false, format!("{error:#}"));
    }
    fn time(&mut self, name: &str, elapsed: Duration) {
        self.timings_ms.insert(name.into(), elapsed.as_millis());
    }
    pub fn passed(&self) -> bool {
        !self.checks.is_empty() && self.checks.iter().all(|check| check.ok)
    }
    pub fn text(&self) -> String {
        let mut out = format!(
            "Alondra cutover rehearsal: {}\nlive HOME {} (read only), work {}\n\nNumbers\n",
            if self.passed() { "PASSED" } else { "FAILED" },
            self.live_home,
            self.work
        );
        for (name, value) in &self.numbers {
            out.push_str(&format!("  {name}: {value}\n"));
        }
        out.push_str("\nTimings (ms)\n");
        for (name, ms) in &self.timings_ms {
            out.push_str(&format!("  {name}: {ms}\n"));
        }
        let failed: Vec<_> = self.checks.iter().filter(|check| !check.ok).collect();
        out.push_str(&format!(
            "\nChecks: {} passed, {} failed\n",
            self.checks.len() - failed.len(),
            failed.len()
        ));
        for check in failed {
            out.push_str(&format!("  FAIL {}: {}\n", check.name, check.detail));
        }
        out
    }
}

/// Copies what a cutover backup preserves, plus the domain sidecar, from a
/// running HOME into a new private directory. Each database is one consistent
/// SQLite snapshot; the set is ordered so every cross-reference resolves.
pub(crate) fn snapshot_live_home(live: &Path, target: &Path) -> Result<Value> {
    ensure!(live.is_absolute() && target.is_absolute(), "rehearsal paths must be absolute");
    let live = live.canonicalize()?;
    destination_outside(&live, target)?;
    ensure!(
        crate::domain_cutover::read_marker(&live)?.is_none(),
        "live HOME already carries a domain cutover marker"
    );
    fs::DirBuilder::new().mode(0o700).create(target)?;
    let started = Instant::now();
    let seq_before = legacy_seq(&live.join("alondra.db"))?;
    let mut databases = 0;
    for name in SNAPSHOT_DATABASES {
        if live.join(name).exists() {
            sqlite_backup(&live.join(name), &target.join(name))?;
            databases += 1;
        }
    }
    let mut files = Vec::new();
    for name in BACKUP_FILES {
        if live.join(name).exists() {
            copy_regular(&live.join(name), &target.join(name))?;
            files.push(describe_file(target, &target.join(name))?);
        }
    }
    for directory in BACKUP_TREES {
        if live.join(directory).exists() {
            copy_tree(&live.join(directory), &target.join(directory), target, &mut files)?;
        }
    }
    let window = started.elapsed();
    let seq_after = legacy_seq(&live.join("alondra.db"))?;
    Ok(json!({
        "databases": databases,
        "files": files.len(),
        "file_bytes": files.iter().map(|file| file.bytes).sum::<u64>(),
        "window_ms": window.as_millis() as u64,
        "live_seq_before": seq_before,
        "live_seq_after": seq_after,
        "snapshot_seq": legacy_seq(&target.join("alondra.db"))?,
    }))
}

fn digest_file(path: &Path) -> Result<String> {
    Ok(describe_file(path.parent().context("file has no parent")?, path)?.sha256)
}

/// Schema and every row, so a restored SQLite file compares by content rather
/// than by page layout or header counters.
pub(crate) fn sqlite_content_digest(path: &Path) -> Result<String> {
    let connection = Connection::open_with_flags(
        canonical_leaf(path)?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    connection.execute_batch("BEGIN")?;
    let mut hash = Sha256::new();
    let mut tables = connection.prepare("SELECT name,sql FROM sqlite_schema WHERE sql IS NOT NULL ORDER BY type,name")?;
    let schema = tables
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (name, sql) in &schema {
        hash.update(sql.as_bytes());
        if !sql.starts_with("CREATE TABLE") {
            continue;
        }
        let mut rows = Vec::new();
        let mut query = connection.prepare(&format!("SELECT * FROM \"{}\"", name.replace('"', "\"\"")))?;
        let columns = query.column_count();
        let mut cursor = query.query([])?;
        while let Some(row) = cursor.next()? {
            let values = (0..columns)
                .map(|index| row.get::<_, rusqlite::types::Value>(index).map(|value| format!("{value:?}")))
                .collect::<rusqlite::Result<Vec<_>>>()?;
            rows.push(values.join("\u{1f}"));
        }
        rows.sort();
        for row in rows {
            hash.update(row.as_bytes());
            hash.update(b"\x1e");
        }
    }
    connection.execute_batch("COMMIT")?;
    Ok(hex(&hash.finalize()))
}

/// Content digests of the files a backup preserves, taken before anything runs on the snapshot.
pub(crate) fn content_digests(home: &Path) -> Result<BTreeMap<String, String>> {
    let mut digests = BTreeMap::new();
    for name in BACKUP_DATABASES {
        if home.join(name).exists() {
            digests.insert(name.to_owned(), sqlite_content_digest(&home.join(name))?);
        }
    }
    let mut files = Vec::new();
    for name in BACKUP_FILES {
        if home.join(name).exists() {
            files.push(describe_file(home, &home.join(name))?);
        }
    }
    for directory in BACKUP_TREES {
        if home.join(directory).exists() {
            collect_stage_files(home, &home.join(directory), &mut files)?;
        }
    }
    digests.extend(files.into_iter().map(|file| (file.path, file.sha256)));
    Ok(digests)
}

struct Sandbox {
    work: PathBuf,
    home: PathBuf,
    exe: PathBuf,
}

impl Sandbox {
    fn env(&self, command: &mut Command) {
        command
            .env_clear()
            .env("HOME", self.work.join("fakehome"))
            .env("TMPDIR", self.work.join("tmp"))
            .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.work.join("stubs").display()))
            .env("ALONDRA_HOME", &self.home)
            .env("ALONDRA_SERVICE_LABEL", "com.alondra.cutover-rehearsal")
            .env("ALONDRA_UPDATE_URL", "http://127.0.0.1:9/")
            .env("ALONDRA_AUTO_UPDATE", "0")
            .env("ALONDRA_WAKE", "0")
            .stdin(Stdio::null());
        if let Ok(user) = std::env::var("USER") {
            command.env("USER", user);
        }
    }

    fn cli(&self, binary: &Path, args: &[&Path]) -> Result<(String, Duration)> {
        let mut command = Command::new(binary);
        command.args(args);
        self.env(&mut command);
        let started = Instant::now();
        let output = command.output()?;
        let elapsed = started.elapsed();
        ensure!(
            output.status.success(),
            "{} failed: {}",
            args.first().map_or(String::new(), |flag| flag.display().to_string()),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok((String::from_utf8_lossy(&output.stdout).into_owned(), elapsed))
    }

    fn start(&self, port: u16, log: &Path) -> Result<Child> {
        let mut command = Command::new(&self.exe);
        self.env(&mut command);
        command
            .env("ALONDRA_PORT", port.to_string())
            .env("ALONDRA_LISTEN", "127.0.0.1")
            .stdout(File::create(log)?)
            .stderr(File::create(log.with_extension("stderr.log"))?);
        Ok(command.spawn()?)
    }

    fn stub_calls(&self) -> String {
        fs::read_to_string(self.work.join("stub-calls.log")).unwrap_or_default()
    }
}

fn free_port() -> Result<u16> {
    let port = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?.port();
    ensure!(port != alondra_core::DEFAULT_PORT, "picked the live daemon's port");
    Ok(port)
}

async fn wait_listening(child: &mut Child, base: &str) -> Result<Duration> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(2)).build()?;
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("daemon exited during startup: {status}");
        }
        if let Ok(response) = client.get(format!("{base}/daemon")).send().await {
            if response.status().is_success() {
                return Ok(started.elapsed());
            }
        }
        ensure!(started.elapsed() < Duration::from_secs(120), "daemon did not listen within 120 s");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

fn stop(child: &mut Child) -> Result<Duration> {
    let started = Instant::now();
    unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGTERM) };
    loop {
        if child.try_wait()?.is_some() {
            return Ok(started.elapsed());
        }
        if started.elapsed() > Duration::from_secs(30) {
            child.kill()?;
            child.wait()?;
            anyhow::bail!("daemon ignored SIGTERM for 30 s");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn log_problems(log: &Path) -> Vec<String> {
    let text = fs::read_to_string(log.with_extension("stderr.log")).unwrap_or_default()
        + &fs::read_to_string(log).unwrap_or_default();
    text.lines()
        .filter(|line| {
            let line = line.to_ascii_lowercase();
            ["error", "fail", "panic", "refus", "disabled"].iter().any(|word| line.contains(word))
        })
        .take(20)
        .map(str::to_owned)
        .collect()
}

fn run_daemon(report: &mut Report, sandbox: &Sandbox, runtime: &tokio::runtime::Runtime, label: &str) -> Result<()> {
    let port = free_port()?;
    let base = format!("http://127.0.0.1:{port}");
    let log = sandbox.work.join(format!("{label}.log"));
    let mut child = sandbox.start(port, &log)?;
    let started = runtime.block_on(wait_listening(&mut child, &base));
    let stopped = stop(&mut child);
    report.time(&format!("{label}: startup"), started?);
    report.time(&format!("{label}: stop"), stopped?);
    let problems = log_problems(&log);
    report.check(format!("{label}: log has no errors"), problems.is_empty(), problems.join(" | "));
    Ok(())
}

fn write_stubs(work: &Path) -> Result<()> {
    let stubs = work.join("stubs");
    fs::DirBuilder::new().mode(0o700).create(&stubs)?;
    for name in STUBBED_COMMANDS {
        let path = stubs.join(name);
        fs::write(
            &path,
            format!("#!/bin/sh\necho \"{name} $*\" >> '{}'\nexit 1\n", work.join("stub-calls.log").display()),
        )?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Runs every stage and records each outcome; a stage that cannot run fails
/// its own check and the stages that need its result are skipped.
pub(crate) fn run(live: &Path, work: &Path, legacy: &Path) -> Result<Report> {
    ensure!(live.is_absolute() && work.is_absolute() && legacy.is_absolute(), "rehearsal paths must be absolute");
    destination_outside(live, work)?;
    ensure!(fs::symlink_metadata(legacy)?.is_file(), "legacy daemon must be a regular file");
    fs::DirBuilder::new().mode(0o700).create(work)?;
    let work = &work.canonicalize()?;
    let live = &live.canonicalize()?;
    for directory in ["tmp", "fakehome", "fakehome/Projects"] {
        fs::DirBuilder::new().mode(0o700).create(work.join(directory))?;
    }
    write_stubs(work)?;
    let sandbox = Sandbox { work: work.to_owned(), home: work.join("home"), exe: std::env::current_exe()? };
    let mut report = Report {
        live_home: live.display().to_string(),
        work: work.display().to_string(),
        ..Report::default()
    };
    let started = Instant::now();
    match snapshot_live_home(live, &sandbox.home) {
        Ok(snapshot) => {
            report.time("snapshot", started.elapsed());
            report.numbers.insert("snapshot".into(), snapshot);
        }
        Err(error) => {
            report.fail("snapshot", &error);
            return Ok(report);
        }
    }
    if let Err(error) = rehearse_snapshot(&mut report, &sandbox, legacy) {
        report.fail("rehearsal stopped", &error);
    }
    let calls = sandbox.stub_calls();
    report.numbers.insert("intercepted_commands".into(), json!(calls.lines().collect::<Vec<_>>()));
    report.check("the empty domain daemon attempted no outside command", calls.is_empty(), calls.lines().collect::<Vec<_>>().join("; "));
    Ok(report)
}

/// Writes `<prefix>.json` and `<prefix>.txt`, prints the text, and returns whether every check passed.
pub(crate) fn run_and_write(live: &Path, work: &Path, legacy: &Path, prefix: &Path) -> Result<bool> {
    let report = run(live, work, legacy)?;
    let text = report.text();
    fs::write(prefix.with_extension("json"), serde_json::to_vec_pretty(&report)?)?;
    fs::write(prefix.with_extension("txt"), &text)?;
    print!("{text}");
    Ok(report.passed())
}

/// Differences between the restored HOME and the content digests taken before the cutover.
pub(crate) fn home_differences(reference: &BTreeMap<String, String>, home: &Path) -> Result<Vec<String>> {
    let mut differences = Vec::new();
    for (path, digest) in reference {
        let file = home.join(path);
        if !file.exists() {
            differences.push(format!("{path} missing"));
            continue;
        }
        let found = if BACKUP_DATABASES.contains(&path.as_str()) {
            sqlite_content_digest(&file)?
        } else {
            digest_file(&file)?
        };
        if &found != digest {
            differences.push(format!("{path} changed"));
        }
    }
    Ok(differences)
}

fn legacy_cli(legacy: &Path) -> PathBuf {
    legacy.with_file_name("alondra")
}

fn rehearse_snapshot(report: &mut Report, sandbox: &Sandbox, legacy: &Path) -> Result<()> {
    let home = &sandbox.home;
    let seq_before = legacy_seq(&home.join("alondra.db"))?;
    report.numbers.insert("legacy_seq".into(), json!(seq_before));
    let reference = content_digests(home)?;
    let integrity: Vec<_> = SNAPSHOT_DATABASES
        .iter()
        .filter(|name| home.join(name).exists())
        .filter_map(|name| {
            let connection = Connection::open_with_flags(home.join(name), OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
            let result: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0)).ok()?;
            (result != "ok").then(|| format!("{name}: {result}"))
        })
        .collect();
    report.check("snapshot integrity", integrity.is_empty(), integrity.join("; "));

    let backup = sandbox.work.join("backup");
    let cutover_args = [Path::new("--cutover-domain"), &backup, legacy, &legacy_cli(legacy)];
    let (output, elapsed) = sandbox.cli(&sandbox.exe, &cutover_args)?;
    report.time("cutover", elapsed);
    let cutover: Value = serde_json::from_str(&output)?;
    report.check("cutover committed", cutover["marker"]["phase"] == "committed", cutover["marker"].to_string());
    report.check("backup holds the legacy store", backup.join("alondra.db").exists(), "backup/alondra.db");
    report.check("legacy binaries kept in the backup",
        backup.join(LEGACY_BINARIES).join("alondrad").exists() && backup.join(LEGACY_BINARIES).join("alondra").exists(),
        LEGACY_BINARIES);
    let archive = home.join(crate::domain_cutover::ARCHIVE);
    let archived = crate::domain_cutover::LEGACY_STORES
        .iter()
        .filter(|name| reference.contains_key(**name))
        .all(|name| !home.join(name).exists() && archive.join(name).exists());
    report.check("legacy stores archived inside HOME", archived, crate::domain_cutover::ARCHIVE);
    let (again, _) = sandbox.cli(&sandbox.exe, &cutover_args)?;
    report.check("a second cutover run only reports",
        serde_json::from_str::<Value>(&again)?["already_committed"] == true, "already committed");

    let connection = Connection::open_with_flags(home.join("domain-v2.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    let foreign: i64 = connection.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| row.get(0))?;
    report.check("committed store integrity", integrity == "ok" && foreign == 0, format!("{integrity}, {foreign} foreign key errors"));
    drop(connection);
    let state = DomainRuntime::open(home)?.state()?;
    report.check("domain store is empty",
        state.revision == 0 && state.requests.is_empty() && state.tasks.is_empty() && state.interactions.is_empty()
            && state.schedule.schedules.is_empty(),
        format!("revision {}, {} requests, {} tasks", state.revision, state.requests.len(), state.tasks.len()));

    let mut removed = Vec::new();
    for name in OUTWARD_FILES {
        if home.join(name).exists() {
            fs::remove_file(home.join(name))?;
            removed.push(name);
        }
    }
    report.numbers.insert("outward_files_removed_from_copy".into(), json!(removed));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    run_daemon(report, sandbox, &runtime, "first start")?;
    run_daemon(report, sandbox, &runtime, "restart")?;

    let (_, elapsed) = sandbox.cli(&sandbox.exe, &[Path::new("--rollback-domain"), &backup])?;
    report.time("rollback", elapsed);
    let differences = home_differences(&reference, home)?;
    report.check("rollback restores the legacy snapshot exactly", differences.is_empty(), differences.join("; "));
    report.check("rollback removes the marker", crate::domain_cutover::read_marker(home)?.is_none(), "marker");
    report.check("rollback restores the legacy sequence", legacy_seq(&home.join("alondra.db"))? == seq_before,
        format!("{seq_before}"));
    let restored = Sandbox { home: home.clone(), work: sandbox.work.clone(), exe: legacy.to_owned() };
    match restored.cli(legacy, &[Path::new("--preflight")]) {
        Ok((out, _)) => report.check("legacy binary starts on the restored HOME", out.contains("preflight ok"), out.trim()),
        Err(error) => report.check("legacy binary starts on the restored HOME", false, format!("{error:#}")),
    };
    match sandbox.cli(&sandbox.exe, &[Path::new("--preflight")]) {
        Ok(_) => report.check("the domain binary refuses the restored legacy HOME", false, "preflight passed"),
        Err(error) => report.check("the domain binary refuses the restored legacy HOME",
            format!("{error:#}").contains("still holds legacy data"), format!("{error:#}")),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("alondra-rehearsal-{name}-{}", crate::auth::random_hex(8)));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        root
    }

    fn tree_digest(home: &Path) -> BTreeMap<String, String> {
        let mut files = Vec::new();
        collect_stage_files(home, home, &mut files).unwrap();
        files
            .into_iter()
            .filter(|file| !file.path.ends_with("-shm"))
            .map(|file| (file.path, file.sha256))
            .collect()
    }

    fn legacy_events(home: &Path) -> Connection {
        let events = Connection::open(home.join("alondra.db")).unwrap();
        events
            .execute_batch(
                "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
                 CREATE TABLE event_log(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL);
                 CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);
                 INSERT INTO snapshots(seq,at,body) VALUES(1,1,'{}');",
            )
            .unwrap();
        events
    }

    #[test]
    fn snapshot_keeps_uncheckpointed_wal_frames_and_leaves_the_live_home_unchanged() {
        let root = root("snapshot");
        let live = root.join("live");
        fs::DirBuilder::new().mode(0o700).create(&live).unwrap();
        fs::DirBuilder::new().mode(0o700).create(live.join("files")).unwrap();
        fs::write(live.join("files").join("blob"), b"attachment").unwrap();
        fs::write(live.join("config.toml"), "name = \"Test\"\n").unwrap();
        let writer = legacy_events(&live);
        writer.execute("INSERT INTO event_log(seq,at,kind,body) VALUES(2,1,'acknowledged','{}')", []).unwrap();
        assert!(fs::metadata(live.join("alondra.db-wal")).unwrap().len() > 0, "the event must sit in the WAL");
        let before = tree_digest(&live);
        let copy = root.join("copy");
        let snapshot = snapshot_live_home(&live, &copy).unwrap();
        assert_eq!(snapshot["snapshot_seq"], json!(2));
        assert_eq!(legacy_seq(&copy.join("alondra.db")).unwrap(), 2);
        assert_eq!(fs::read(copy.join("files").join("blob")).unwrap(), b"attachment");
        assert_eq!(fs::read_to_string(copy.join("config.toml")).unwrap(), "name = \"Test\"\n");
        assert_eq!(tree_digest(&live), before);
        assert!(snapshot_live_home(&live, &live.join("inside")).is_err());
        drop(writer);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn snapshot_order_covers_every_database_a_backup_keeps() {
        for name in BACKUP_DATABASES {
            assert!(SNAPSHOT_DATABASES.contains(&name), "{name}");
        }
    }

    #[test]
    fn sqlite_content_digest_ignores_layout_and_sees_rows() {
        let root = root("digest");
        let first = root.join("first.db");
        let connection = Connection::open(&first).unwrap();
        connection.execute_batch("CREATE TABLE t(a TEXT); INSERT INTO t VALUES('x'),('y');").unwrap();
        drop(connection);
        let second = root.join("second.db");
        sqlite_backup(&first, &second).unwrap();
        assert_eq!(sqlite_content_digest(&first).unwrap(), sqlite_content_digest(&second).unwrap());
        Connection::open(&second).unwrap().execute("UPDATE t SET a='z' WHERE a='y'", []).unwrap();
        assert_ne!(sqlite_content_digest(&first).unwrap(), sqlite_content_digest(&second).unwrap());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_cutover_on_a_snapshot_rolls_back_to_the_same_content() {
        let root = root("checks");
        let live = root.join("live");
        fs::DirBuilder::new().mode(0o700).create(&live).unwrap();
        legacy_events(&live).execute("INSERT INTO event_log(seq,at,kind,body) VALUES(2,1,'note','{}')", []).unwrap();
        let legacy = root.join("alondrad");
        fs::write(&legacy, b"legacy daemon --preflight").unwrap();
        let home = root.join("home");
        snapshot_live_home(&live, &home).unwrap();
        let reference = content_digests(&home).unwrap();
        let backup = root.join("backup");
        let wait = crate::domain_cutover::LockWait { attempts: 1, interval: Duration::ZERO };
        crate::domain_cutover::cutover_offline(&home, &backup, &[&legacy], &wait, |_| ()).unwrap();
        crate::domain_cutover::rollback_offline(&home, &backup, &wait, |_| ()).unwrap();
        assert_eq!(home_differences(&reference, &home).unwrap(), Vec::<String>::new());
        Connection::open(home.join("alondra.db")).unwrap().execute("UPDATE snapshots SET at=at+1", []).unwrap();
        let differences = home_differences(&reference, &home).unwrap();
        assert!(differences.contains(&"alondra.db changed".to_owned()), "{differences:?}");
        fs::remove_dir_all(root).unwrap();
    }
}
