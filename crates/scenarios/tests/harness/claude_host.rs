//! The edge answers Claude's command line and the host's probes from the world, per account, and
//! nothing else.

use alondra_agent_runtime::owned_host::{HostLauncher, ProbeRun};
use alondra_scenarios::claude_host::{ClaudeEdge, assistant_errors};
use serde_json::{Value, json};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command as Process;
use std::sync::{Arc, Mutex};

fn edge() -> (tempfile::TempDir, ClaudeEdge, Arc<Mutex<Vec<String>>>) {
    let home = tempfile::Builder::new().prefix("ce-").tempdir().unwrap();
    let leaks = Arc::new(Mutex::new(Vec::new()));
    let edge = ClaudeEdge::new(home.path(), leaks.clone());
    (home, edge, leaks)
}

/// The bootstrap of the host `host-1`, which runs task `t1`.
fn bootstrap() -> Vec<u8> {
    let bootstrap = json!({
        "host_instance_id": "host-1",
        "control_secret": "01".repeat(32),
        "fence": {"profile_id": "default", "account_id": "default", "data_epoch": "epoch", "host_id": "host"},
        "execution": {"task_id": "t1", "agent_id": "sdk-t1", "conversation_id": "conversation", "process_instance_id": "process", "generation": 1},
        "manifest": {"host_release": "0.0.0", "host_sha256": "a".repeat(64), "executable_sha256": "b".repeat(64), "sdk_version": "sdk"},
        "native": {"session_id": "session"},
    });
    serde_json::to_vec(&bootstrap).unwrap()
}

/// A process the test started, killed and reaped when the test ends however it ends.
struct Owned(std::process::Child);

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn status(edge: &ClaudeEdge, folder: Option<&Path>) -> Value {
    let mut command = Process::new("/usr/local/bin/claude");
    command.args(["auth", "status", "--json"]);
    if let Some(folder) = folder {
        command.env("CLAUDE_CONFIG_DIR", folder);
    }
    let output = edge.run(ProbeRun { command, input: Vec::new(), timeout: None }).unwrap();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn an_expired_sign_in_is_seen_only_on_its_own_account() {
    let (home, edge, leaks) = edge();
    let work = alondra_core::accounts::folders(home.path()).join("work");
    edge.set_signed_in("work", false);
    assert_eq!(status(&edge, Some(&work))["loggedIn"], json!(false));
    assert_eq!(status(&edge, None)["loggedIn"], json!(true), "the default account is still signed in");
    edge.set_signed_in("work", true);
    assert_eq!(status(&edge, Some(&work))["loggedIn"], json!(true));
    assert!(leaks.lock().unwrap().is_empty());
}

#[test]
fn a_run_the_edge_does_not_answer_is_a_leak() {
    let (_home, edge, leaks) = edge();
    let command = Process::new("/usr/bin/git");
    assert!(edge.run(ProbeRun { command, input: Vec::new(), timeout: None }).is_err());
    assert_eq!(leaks.lock().unwrap().len(), 1, "{:?}", leaks.lock().unwrap());
}

#[test]
fn the_errors_a_scenario_names_are_the_sdks_own() {
    let errors = assistant_errors();
    for error in ["authentication_failed", "rate_limit", "server_error", "overloaded"] {
        assert!(errors.iter().any(|known| known == error), "{error} is in sdk-errors.json");
    }
    assert!(!errors.iter().any(|known| known == "outage"));
}

/// The daemon kills a host's group right after writing it a frame the relay had not passed on yet:
/// the frame goes with the process and is never handled, so it stops counting once the process
/// exited, and the wait for calm does not wait for it to its patience.
#[test]
fn a_frame_lost_with_a_host_s_process_stops_counting_once_the_process_exits() {
    let runtime = alondra_core::clock::VirtualRuntime::new(1_700_000_000_000).unwrap();
    runtime.block_on(async {
        let (home, edge, _) = edge();
        let mut process = Owned(Process::new("/bin/sleep").arg("600").process_group(0).spawn().unwrap());
        let pid = process.0.id();
        let _host = edge.start_host(&bootstrap(), pid, home.path().join("b")).unwrap();
        alondra_core::clock::frame_sent("host-1");
        process.0.kill().unwrap();
        process.0.wait().unwrap();
        let rested = alondra_core::clock::spawn_blocking_unwatched(|| alondra_core::clock::until_idle(std::time::Duration::from_secs(20), || true)).await.unwrap();
        assert!(rested, "{} frames still counted for a host whose process exited", alondra_core::clock::frames_in_transit());
    });
}

/// The daemon stops a host it never attached to, as one it adopts on a restart and stops at once,
/// while nothing else of the daemon works: the relay's exit wakes a wait for calm that already
/// looked, and the host it finds is settled.
#[test]
fn a_wait_for_calm_sees_a_host_settle_when_the_daemon_stops_its_process() {
    let runtime = alondra_core::clock::VirtualRuntime::new(1_700_000_000_000).unwrap();
    runtime.block_on(async {
        let (home, edge, _) = edge();
        let process = Arc::new(Mutex::new(Owned(Process::new("/bin/sleep").arg("600").process_group(0).spawn().unwrap())));
        let pid = process.lock().unwrap().0.id();
        let _host = edge.start_host(&bootstrap(), pid, home.path().join("b")).unwrap();
        let (watching, stopped) = (edge.clone(), process.clone());
        let stopping = std::sync::atomic::AtomicBool::new(true);
        let calm = alondra_core::clock::spawn_blocking_unwatched(move || {
            alondra_core::clock::until_idle(std::time::Duration::from_secs(20), move || {
                if stopping.swap(false, std::sync::atomic::Ordering::SeqCst) {
                    stopped.lock().unwrap().0.kill().unwrap();
                }
                watching.committed()
            })
        })
        .await
        .unwrap();
        assert!(calm, "the wait for calm looked again once the host's process exited, and found it settled");
    });
}

/// How long a wait for calm of these tests waits with nothing moving: what it waits for is played by
/// the test, so the wait ends only on what the test says.
const PATIENCE: std::time::Duration = std::time::Duration::from_millis(50);

/// The daemon wrote a frame to a host whose relay a loaded Mac does not run: nothing the clock
/// counts moves until the relay gets a core and the host handles the frame, and the wait for calm
/// waits for it instead of giving up after its patience.
#[test]
fn a_wait_for_calm_waits_for_a_host_whose_process_waits_for_a_core() {
    let runtime = alondra_core::clock::VirtualRuntime::new(1_700_000_000_000).unwrap();
    runtime.block_on(async {
        let (home, edge, _) = edge();
        let process = Owned(Process::new("/bin/sleep").arg("600").process_group(0).spawn().unwrap());
        let relay = process.0.id();
        let _host = edge.start_host(&bootstrap(), relay, home.path().join("b")).unwrap();
        let (asked, looks) = std::sync::mpsc::channel();
        let asked = Mutex::new(asked);
        edge.read_cores_with(move |pid| match pid == relay {
            true => {
                let _ = asked.lock().unwrap().send(());
                1
            }
            false => 1,
        });
        alondra_core::clock::frame_sent("host-1");
        let clock = alondra_core::clock::Clock::current();
        let mac = std::thread::spawn(move || {
            for _ in 0..3 {
                looks.recv().unwrap();
            }
            clock.frame_handled("host-1");
        });
        let waiting = edge.clone();
        let calm = alondra_core::clock::spawn_blocking_unwatched(move || waiting.until_calm(PATIENCE, || true)).await.unwrap();
        assert!(calm, "the wait for calm gave up on a frame its host's process was still waiting for a core to handle");
        mac.join().unwrap();
    });
}

/// A host whose relay is still starting, unattached, is waited for while its process waits for a
/// core; once it waits for nothing and starts no further, the wait gives up as it does for a host
/// that is stuck from the start.
#[test]
fn a_host_that_starts_no_further_and_waits_for_no_core_is_stuck() {
    let runtime = alondra_core::clock::VirtualRuntime::new(1_700_000_000_000).unwrap();
    runtime.block_on(async {
        let (home, edge, _) = edge();
        let process = Owned(Process::new("/bin/sleep").arg("600").process_group(0).spawn().unwrap());
        let relay = process.0.id();
        let _host = edge.start_host(&bootstrap(), relay, home.path().join("b")).unwrap();
        for starved in [3, 0] {
            let asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let looked = asked.clone();
            edge.read_cores_with(move |pid| match pid == relay {
                true => usize::from(looked.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < starved),
                false => 1,
            });
            let waiting = edge.clone();
            let calm = alondra_core::clock::spawn_blocking_unwatched(move || waiting.until_calm(PATIENCE, || waiting.committed())).await.unwrap();
            assert!(!calm, "a host no daemon attached to is never calm");
            assert_eq!(
                asked.load(std::sync::atomic::Ordering::SeqCst),
                starved + 1,
                "the wait lasted while the host's process waited for a core, {starved} limits, and ended at the first it did not"
            );
        }
    });
}

/// A thread of the daemon, besides the one that waits, that waits for a core is the daemon still
/// moving, and the process of a host that is gone counts no more.
#[test]
fn a_thread_of_the_daemon_that_waits_for_a_core_moves_the_daemon() {
    let (home, edge, _) = edge();
    let mut process = Owned(Process::new("/bin/sleep").arg("600").process_group(0).spawn().unwrap());
    let relay = process.0.id();
    let _host = edge.start_host(&bootstrap(), relay, home.path().join("b")).unwrap();
    let daemon = Arc::new(std::sync::atomic::AtomicUsize::new(1));
    let threads = daemon.clone();
    edge.read_cores_with(move |pid| match pid == relay {
        true => 1,
        false => threads.load(std::sync::atomic::Ordering::SeqCst),
    });
    assert!(edge.waiting_for_cores(), "the host's process waits for a core");
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    assert!(!edge.waiting_for_cores(), "only the thread that asks runs, and the host is gone");
    daemon.store(2, std::sync::atomic::Ordering::SeqCst);
    assert!(edge.waiting_for_cores(), "another thread of the daemon waits for a core");
}

/// The kernel says how many threads of a process run or wait for a core: at least the one that
/// asks about its own process, and none of a process that is gone.
#[test]
fn the_threads_that_wait_for_a_core_are_read_from_the_kernel() {
    assert!(alondra_scenarios::cores::runnable_threads(std::process::id()) >= 1, "the thread that asks runs");
    let mut process = Process::new("/bin/sleep").arg("600").spawn().unwrap();
    let gone = process.id();
    process.kill().unwrap();
    process.wait().unwrap();
    assert_eq!(alondra_scenarios::cores::runnable_threads(gone), 0, "a process that is gone runs nothing");
}
