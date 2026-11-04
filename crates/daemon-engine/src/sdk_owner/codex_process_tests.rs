use super::*;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command as ProcessCommand, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;
use std::time::Duration;

struct ActualDaemon {
    process: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
}
impl ActualDaemon {
    fn start(slice: &Slice, profile: &Path, sentinels: &Path) -> Self {
        let mut process = ProcessCommand::new("/usr/bin/sandbox-exec").arg("-f").arg(profile).arg(runfile("FIXTURE_DAEMON"))
            .env_clear().env("HOME", &*slice._home).env("ALONDRA_HOME", &*slice._home).env("ALONDRA_SANDBOX", "1")
            .env("ALONDRA_PORT", "0").env("ALONDRA_WAKE", "0").env("ALONDRA_NAME", "Daemon adoption fixture")
            .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", sentinels.display())).env("TMPDIR", &*slice._home)
            .current_dir(&*slice._home).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
        let stderr = process.stderr.take().unwrap();
        let (send, lines) = mpsc::channel();
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break; };
                if send.send(line).is_err() { break; }
            }
        });
        let mut daemon = Self { process, lines, reader: Some(reader) };
        let mut diagnostic = String::new();
        loop {
            let line = daemon.lines.recv_timeout(Duration::from_secs(45)).unwrap_or_else(|why| panic!("The actual daemon did not finish startup: {why}; {diagnostic}"));
            if diagnostic.len() < 128 * 1024 { diagnostic.push_str(&line); diagnostic.push('\n'); }
            if line.contains("domain listener on http://127.0.0.1:0") { break; }
        }
        assert!(daemon.process.try_wait().unwrap().is_none(), "the production daemon exited after readiness: {diagnostic}");
        daemon
    }

    fn crash(&mut self) {
        use std::os::unix::process::ExitStatusExt;
        self.process.kill().unwrap();
        assert_eq!(self.process.wait().unwrap().signal(), Some(libc::SIGKILL));
        if let Some(reader) = self.reader.take() { reader.join().unwrap(); }
    }
}
impl Drop for ActualDaemon {
    fn drop(&mut self) {
        if self.process.try_wait().ok().flatten().is_none() { let _ = self.process.kill(); }
        let _ = self.process.wait();
        if let Some(reader) = self.reader.take() { let _ = reader.join(); }
    }
}

fn protected_user_home() -> PathBuf {
    use std::ffi::CStr;
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::zeroed();
    let mut buffer = vec![0_u8; 32 * 1024];
    let mut result = std::ptr::null_mut();
    // The reentrant account lookup uses caller-owned storage; only the public home path is copied.
    let status = unsafe { libc::getpwuid_r(libc::geteuid(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
    assert_eq!(status, 0); assert!(!result.is_null());
    // A successful lookup initialized the passwd entry and its strings in the live buffer.
    let directory = unsafe { CStr::from_ptr(entry.assume_init().pw_dir) };
    PathBuf::from(directory.to_str().unwrap())
}

fn daemon_sandbox(slice: &Slice) -> PathBuf {
    let quote = |path: &Path| serde_json::to_string(path.to_str().unwrap()).unwrap();
    let protected = protected_user_home();
    let mut profile = format!("(version 1)\n(allow default)\n(deny file-read* file-write* (subpath {}))\n(allow file-read* file-write* (subpath {}))\n(allow file-read* file-write* (subpath {}))\n(deny file-read* file-write* (subpath \"/Library/Keychains\"))\n(deny network-outbound)\n(allow network-outbound (remote ip \"localhost:*\"))\n(allow network-outbound (remote unix-socket (subpath {})))\n(allow network-outbound (remote unix-socket (subpath {})))\n(deny signal)\n(allow signal (target same-sandbox))\n(deny mach-lookup (global-name \"com.apple.securityd\") (global-name \"com.apple.securityd.xpc\"))\n(deny process-exec (literal \"/usr/bin/security\") (literal \"/bin/launchctl\"))\n",
        quote(&protected), quote(&slice._home), quote(&slice.backend.state_root), quote(&slice._home), quote(&slice.backend.state_root));
    for key in ["FIXTURE_DAEMON", "FIXTURE_NODE", "FIXTURE_HOST", "FIXTURE_CODEX_HOST", "CARGO_BIN_EXE_alondra"] {
        let asset = if key == "CARGO_BIN_EXE_alondra" { std::fs::canonicalize(option_env!("CARGO_BIN_EXE_alondra").expect("the test names the CLI")).unwrap() } else { runfile(key) };
        profile.push_str(&format!("(allow file-read* (literal {}))\n", quote(&asset)));
    }
    let path = slice._home.join("daemon.sb"); std::fs::write(&path, profile).unwrap(); path
}

fn readonly_database(slice: &Slice) -> Connection {
    Connection::open_with_flags(&slice.database, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).unwrap()
}

#[derive(Debug, PartialEq, Eq)]
struct DurableNativeWork {
    instance: String,
    conversation: String,
    requests: Vec<(String, String, String)>,
    permission_cards: Vec<(String, String, Option<String>, bool)>,
    command_events: Vec<String>,
}
fn durable_native_work(slice: &Slice) -> DurableNativeWork {
    let database = readonly_database(slice);
    let (instance, conversation) = database.query_row("SELECT e.instance_id,e.conversation_id FROM lifecycle_executions e JOIN lifecycle_current_execution c USING(task_id,generation,node_id) WHERE e.task_id=?1", [TASK], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
    let requests = database.prepare("SELECT request_id,kind,CAST(payload AS TEXT) FROM lifecycle_decision_requests WHERE task_id=?1 ORDER BY epoch").unwrap()
        .query_map([TASK], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().map(Result::unwrap).collect();
    let permission_cards = database.prepare("SELECT i.id,i.card,i.answer,i.withdrawn FROM interactions i JOIN interaction_holds h ON h.interaction_id=i.id WHERE i.task_id=?1 AND h.hold='permission' ORDER BY i.id").unwrap()
        .query_map([TASK], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap().map(Result::unwrap).collect();
    let command_events = database.prepare("SELECT event FROM lifecycle_events WHERE task_id=?1 AND event LIKE '%owned-command%' ORDER BY revision").unwrap()
        .query_map([TASK], |row| row.get(0)).unwrap().map(Result::unwrap).collect();
    DurableNativeWork { instance, conversation, requests, permission_cards, command_events }
}

fn daemon_fixture(mode: &str) -> (Slice, Driver, PathBuf, PathBuf) {
    let mut slice = Slice::with_database_name(HostRoot::new(), crate::domain_database_migration::DATABASE);
    let sentinels = slice._home.join("sentinels"); std::fs::create_dir(&sentinels).unwrap();
    let calls = slice._home.join("forbidden-native-calls");
    for name in ["claude", "security", "launchctl"] {
        let script = format!("#!/usr/bin/python3\nwith open({}, 'a') as target: target.write({} + '\\n')\nraise SystemExit(97)\n", serde_json::to_string(calls.to_str().unwrap()).unwrap(), serde_json::to_string(name).unwrap());
        let path = sentinels.join(name); std::fs::write(&path, script).unwrap(); std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    slice.backend.native = sentinels.join("claude"); slice.backend.native_sha256 = backend::digest(&slice.backend.native).unwrap();
    slice.backend.native_environment.insert("ALONDRA_PORT".into(), "47811".into());
    slice.backend.native_environment.insert("PATH".into(), format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", sentinels.display()));
    let trace = configured(&slice, mode);
    let mut config = alondra_core::accounts::AccountsConfig::fresh(); config.connect(Provider::Codex);
    std::fs::write(alondra_core::options::path_in(&slice._home), config.to_toml()).unwrap();
    std::fs::write(slice._home.join("config.toml"), "name = \"Daemon adoption fixture\"\nlisten = \"127.0.0.1\"\nwake = false\nlegacy_bridge = false\nauto_update = false\nresume_dead_nodes = false\ncompact_idle_nodes_at = 0\ncompact_orchestrator_at = 0\n").unwrap();
    std::fs::write(slice._home.join("sdk-backend.json"), serde_json::to_vec(&slice.backend).unwrap()).unwrap();
    use std::os::unix::ffi::OsStrExt;
    let checkpoint = slice._home.join("checkpoint-release");
    let checkpoint_path = std::ffi::CString::new(checkpoint.as_os_str().as_bytes()).unwrap();
    // This fixture-owned FIFO schedules a schema-valid native observation after the final adoption.
    assert_eq!(unsafe { libc::mkfifo(checkpoint_path.as_ptr(), 0o600) }, 0);
    let sidecar = slice._home.join("codex-native.account.json");
    let mut native_fixture = serde_json::json!({"checkpointRelease":checkpoint});
    if mode == "blocked_command" {
        let release = slice._home.join("command-release");
        let path = std::ffi::CString::new(release.as_os_str().as_bytes()).unwrap();
        // The FIFO is new, below the retained fixture directory, and has no external target.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        native_fixture["ownedCommand"] = serde_json::json!({"release":release,"pidFile":slice._home.join("command-pid.json")});
    }
    std::fs::write(sidecar, serde_json::to_vec(&native_fixture).unwrap()).unwrap();
    let mut driver = launch(&slice);
    loop {
        let state = slice.domain.state().unwrap();
        let arrived = if mode == "permission" { state.interactions.values().any(|card| matches!(&card.scope, Scope::Task { task_id } if task_id.0 == TASK) && card.id.0.starts_with(DECISION_CARDS)) }
            else { state.lifecycle_of(&TASK.into()).unwrap().activity() == ActivityView::OwnWork };
        if arrived { break; }
        advance(&mut driver);
    }
    let profile = daemon_sandbox(&slice);
    (slice, driver, trace, profile)
}

fn native_frames(path: &Path) -> Vec<serde_json::Value> {
    let bytes = std::fs::read(path).unwrap(); let mut offset = 0; let mut frames = Vec::new();
    while offset + 4 <= bytes.len() {
        let length = u32::from_be_bytes(bytes[offset..offset+4].try_into().unwrap()) as usize;
        offset += 4;
        assert!(length <= 8 * 1024 * 1024);
        if offset + length > bytes.len() { break; }
        frames.push(serde_json::from_slice(&bytes[offset..offset+length]).unwrap());
        offset += length;
    }
    frames
}

fn attached_host_generation(path: &Path) -> i64 {
    native_frames(path).iter().filter(|frame| frame["body"]["type"] == "attached")
        .map(|frame| frame["body"]["owner_generation"].as_i64().unwrap()).last().unwrap_or(0)
}

fn prove_live_observation_after_final_adoption(slice: &Slice, journal: &Path) {
    use std::io::Write;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    use std::os::unix::fs::OpenOptionsExt;
    let watched = std::fs::File::open(journal).unwrap();
    // kqueue returns a new owned descriptor; the fixture keeps both watched files alive through it.
    let descriptor = unsafe { libc::kqueue() }; assert!(descriptor >= 0);
    let queue = unsafe { OwnedFd::from_raw_fd(descriptor) };
    let mut change: libc::kevent = unsafe { std::mem::zeroed() };
    change.ident = watched.as_raw_fd() as libc::uintptr_t;
    change.filter = libc::EVFILT_VNODE; change.flags = libc::EV_ADD | libc::EV_CLEAR;
    change.fflags = libc::NOTE_WRITE | libc::NOTE_EXTEND;
    assert_eq!(unsafe { libc::kevent(queue.as_raw_fd(), &change, 1, std::ptr::null_mut(), 0, std::ptr::null()) }, 0);
    let mut release = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(slice._home.join("checkpoint-release")).unwrap();
    release.write_all(b"observe").unwrap(); drop(release);
    loop {
        let database = readonly_database(slice);
        let cursor = native_frames(journal).iter().find(|frame| frame["body"]["observation"]["payload"]["method"] == "thread/tokenUsage/updated" && frame["body"]["observation"]["payload"]["params"]["tokenUsage"]["total"]["totalTokens"] == 37).map(|frame| frame["body"]["cursor"].as_u64().unwrap());
        let committed = cursor.is_some_and(|cursor| database.query_row("SELECT EXISTS(SELECT 1 FROM provider_observations o JOIN provider_normalization_checkpoints c ON c.task_id=o.task_id AND c.generation=o.generation JOIN lifecycle_current_execution e ON e.task_id=o.task_id AND e.generation=o.generation WHERE o.task_id=?1 AND o.cursor=?2 AND c.cursor>=o.cursor)", rusqlite::params![TASK,i64::try_from(cursor).unwrap()], |row| row.get::<_,bool>(0)).unwrap());
        if committed { return; }
        let timeout = libc::timespec { tv_sec: 45, tv_nsec: 0 };
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        // A native event writes the host journal before observation, and its post-commit ACK writes it again.
        assert_eq!(unsafe { libc::kevent(queue.as_raw_fd(), std::ptr::null(), 0, &mut event, 1, &timeout) }, 1, "the actual daemon did not normalize the native observation after live adoption");
        assert_eq!(event.flags & libc::EV_ERROR, 0);
    }
}

struct RetainedNativeFixture { slice: Slice, driver: Option<Driver> }
impl Drop for RetainedNativeFixture {
    fn drop(&mut self) { drop(self.driver.take()); self.slice._state.kill_hosts(); }
}

fn prove_actual_daemon_adoption(mode: &str) {
    let (slice, driver, trace, profile) = daemon_fixture(mode);
    let mut retained = RetainedNativeFixture { slice, driver: Some(driver) };
    let slice = &retained.slice;
    let group = retained.driver.as_ref().unwrap().worker.host_group().unwrap(); let native: Vec<_> = alondra_process_group::members(group).unwrap().into_iter().filter(|member| !member.zombie).collect();
    assert!(native.len() >= 2, "the native app-server and persistent host are actual distinct live processes");
    let expected = durable_native_work(slice);
    if mode == "permission" {
        assert_eq!(expected.requests.len(), 1); assert_eq!(expected.requests[0].1, "permission");
        assert_eq!(expected.permission_cards.len(), 1); assert_eq!(expected.permission_cards[0].2, None); assert!(!expected.permission_cards[0].3);
    } else {
        let child: serde_json::Value = serde_json::from_slice(&std::fs::read(slice._home.join("command-pid.json")).unwrap()).unwrap();
        assert!(native.iter().any(|member| Some(i64::from(member.pid)) == child["pid"].as_i64()), "the actual barrier-blocked child belongs to the persistent native host group");
        assert!(!expected.command_events.is_empty());
    }
    drop(retained.driver.take());
    let sentinels = slice._home.join("sentinels");
    let mut generation = owner_generation(&slice.database);
    for cycle in 0..3 {
        let mut daemon = ActualDaemon::start(slice, &profile, &sentinels);
        generation += 1; assert_eq!(owner_generation(&slice.database), generation, "main.startup adopt_all must claim the live execution in cycle {cycle}");
        let journal = slice.backend.state_root.join(&expected.instance).join("outbox");
        assert_eq!(attached_host_generation(&journal), generation, "the existing host durably accepted the new owner, rather than merely reserving SQL ownership");
        if cycle == 2 { prove_live_observation_after_final_adoption(slice, &journal); }
        assert_eq!(durable_native_work(slice), expected, "the durable pending permission/owned work survives the actual process replacement");
        assert!(native.iter().all(|member| alondra_process_group::alive(member).unwrap()), "host, native app-server and owned child retain PID and birth identity");
        let requests = transcript(&trace);
        for method in ["initialize", "thread/start", "turn/start"] { assert_eq!(requests.iter().filter(|message| message["method"] == method).count(), 1, "no repeated {method} during live actual-daemon adoption"); }
        assert!(requests.iter().all(|message| message["method"] != "thread/resume"));
        assert!(!slice._home.join("forbidden-native-calls").exists(), "CodeX-only adoption cannot execute Claude, security or launchctl");
        daemon.crash();
    }
    eprintln!("ACTUAL_DAEMON_ADOPTION mode={mode} replacements=2 original_host_group={group} native_members={native:?} final_owner_generation={generation}");
}

#[test]
#[ignore = "opt-in: actual candidate alondrad processes and two SIGKILL/restart cycles under Seatbelt; temporary HOME and fake native Codex only"]
fn an_actual_daemon_preserves_a_codex_permission_across_two_process_crashes() { prove_actual_daemon_adoption("permission"); }

#[test]
#[ignore = "opt-in: actual candidate alondrad processes and barrier-blocked native child across two SIGKILL/restart cycles; no external inference"]
fn an_actual_daemon_preserves_a_codex_owned_child_across_two_process_crashes() { prove_actual_daemon_adoption("blocked_command"); }
