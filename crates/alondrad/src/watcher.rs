use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use alondra_core::{DaemonObservation, Event, now_millis};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::hub::Hub;

const RECENT_WINDOW_MS: i64 = 48 * 3600 * 1000;
const SCAN_EVERY: Duration = Duration::from_secs(3);

struct Seen {
    mtime: i64,
    obs: DaemonObservation,
}

pub async fn run(hub: Arc<Hub>, claude_home: PathBuf) {
    let (exit_tx, mut exit_rx) = mpsc::channel::<(String, i32)>(256);
    let register = spawn_pid_watcher(exit_tx);
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut registered: HashMap<String, i32> = HashMap::new();

    reap_dead_on_start(&hub);

    loop {
        let roster = read_roster(&claude_home.join("daemon/roster.json"));
        for (dir, short) in job_dirs(&claude_home.join("jobs")) {
            let state_path = dir.join("state.json");
            let Some(mtime) = mtime_millis(&state_path) else { continue };
            let pid = roster.get(&short).copied();
            if pid.is_none() && now_millis() - mtime > RECENT_WINDOW_MS {
                continue;
            }
            let unchanged = seen
                .get(&short)
                .is_some_and(|s| s.mtime == mtime && s.obs.pid == pid);
            if unchanged {
                continue;
            }
            let Some(mut obs) = read_state(&state_path, &short) else { continue };
            obs.pid = pid;
            obs.updated_at = Some(mtime);
            let meaningful = seen.get(&short).is_none_or(|s| differs(&s.obs, &obs));
            if meaningful && hub.append(Event::DaemonSeen(obs.clone())).is_err() {
                eprintln!("alondrad: no se pudo guardar la observación de {short}");
            }
            if let Some(p) = pid
                && registered.get(&obs.session_id) != Some(&p)
            {
                registered.insert(obs.session_id.clone(), p);
                let _ = register.send((obs.session_id.clone(), p));
            }
            seen.insert(short, Seen { mtime, obs });
        }

        while let Ok((session_id, pid)) = exit_rx.try_recv() {
            registered.remove(&session_id);
            let _ = hub.append(Event::ProcessExited { session_id, pid });
        }

        tokio::select! {
            _ = tokio::time::sleep(SCAN_EVERY) => {}
            Some((session_id, pid)) = exit_rx.recv() => {
                registered.remove(&session_id);
                let _ = hub.append(Event::ProcessExited { session_id, pid });
            }
        }
    }
}

fn reap_dead_on_start(hub: &Hub) {
    let dead: Vec<(String, i32)> = hub.read(|v| {
        v.nodes
            .values()
            .filter(|n| n.alive)
            .filter_map(|n| n.pid.map(|p| (n.session_id.clone(), p)))
            .filter(|(_, p)| !pid_alive(*p))
            .collect()
    });
    for (session_id, pid) in dead {
        let _ = hub.append(Event::ProcessExited { session_id, pid });
    }
}

pub fn pid_alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

fn differs(a: &DaemonObservation, b: &DaemonObservation) -> bool {
    a.state != b.state
        || a.tempo != b.tempo
        || a.detail != b.detail
        || a.needs != b.needs
        || a.name != b.name
        || a.cwd != b.cwd
        || a.pid != b.pid
}

fn job_dirs(jobs: &Path) -> Vec<(PathBuf, String)> {
    let Ok(rd) = std::fs::read_dir(jobs) else { return vec![] };
    rd.filter_map(Result::ok)
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| (e.path(), e.file_name().to_string_lossy().to_string()))
        .collect()
}

fn mtime_millis(p: &Path) -> Option<i64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    Some(m.duration_since(UNIX_EPOCH).ok()?.as_millis() as i64)
}

fn read_roster(p: &Path) -> HashMap<String, i32> {
    let Ok(text) = std::fs::read_to_string(p) else { return HashMap::new() };
    let Ok(v) = serde_json::from_str::<Value>(&text) else { return HashMap::new() };
    v.get("workers")
        .and_then(Value::as_object)
        .map(|w| {
            w.iter()
                .filter_map(|(short, info)| {
                    let pid = info.get("pid")?.as_i64()? as i32;
                    pid_alive(pid).then(|| (short.clone(), pid))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn read_state(p: &Path, short: &str) -> Option<DaemonObservation> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string).filter(|x| !x.is_empty());
    Some(DaemonObservation {
        session_id: s("sessionId")?,
        short_id: short.to_string(),
        name: s("name"),
        cwd: s("worktreePath").or_else(|| s("cwd")),
        pid: None,
        state: s("state"),
        tempo: s("tempo"),
        detail: s("detail"),
        needs: s("needs"),
        updated_at: None,
    })
}

fn spawn_pid_watcher(exits: mpsc::Sender<(String, i32)>) -> std::sync::mpsc::Sender<(String, i32)> {
    let (tx, rx) = std::sync::mpsc::channel::<(String, i32)>();
    std::thread::Builder::new()
        .name("alondra-pids".into())
        .spawn(move || kqueue_loop(rx, exits))
        .expect("no se pudo arrancar el vigilante de procesos");
    tx
}

fn kqueue_loop(rx: std::sync::mpsc::Receiver<(String, i32)>, exits: mpsc::Sender<(String, i32)>) {
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        eprintln!("alondrad: kqueue no disponible");
        return;
    }
    let mut watched: HashMap<i32, String> = HashMap::new();
    let mut out: [libc::kevent; 16] = unsafe { std::mem::zeroed() };
    loop {
        loop {
            match rx.try_recv() {
                Ok((session_id, pid)) => {
                    if watched.contains_key(&pid) {
                        continue;
                    }
                    let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
                    ev.ident = pid as usize;
                    ev.filter = libc::EVFILT_PROC;
                    ev.flags = libc::EV_ADD | libc::EV_ONESHOT;
                    ev.fflags = libc::NOTE_EXIT;
                    let r = unsafe { libc::kevent(kq, &ev, 1, std::ptr::null_mut(), 0, std::ptr::null()) };
                    if r < 0 {
                        let _ = exits.blocking_send((session_id, pid));
                    } else {
                        watched.insert(pid, session_id);
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    unsafe { libc::close(kq) };
                    return;
                }
            }
        }
        let ts = libc::timespec { tv_sec: 1, tv_nsec: 0 };
        let n = unsafe { libc::kevent(kq, std::ptr::null(), 0, out.as_mut_ptr(), out.len() as i32, &ts) };
        for ev in out.iter().take(n.max(0) as usize) {
            let pid = ev.ident as i32;
            if let Some(session_id) = watched.remove(&pid) {
                let _ = exits.blocking_send((session_id, pid));
            }
        }
    }
}
