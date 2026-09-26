use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use alondra_core::{BackgroundItem, BackgroundKind, DaemonObservation, Event};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::hub::Hub;
use crate::markdown;

const SCAN_EVERY: Duration = Duration::from_secs(3);
const HEARTBEAT_MS: i64 = 5 * 60 * 1000;

struct Seen {
    mtime: i64,
    obs: DaemonObservation,
    recorded: i64,
}

pub async fn run(hub: Arc<Hub>, claude_home: PathBuf) {
    let (exit_tx, mut exit_rx) = mpsc::channel::<(String, i32)>(256);
    let register = spawn_pid_watcher(exit_tx);
    let mut seen: HashMap<String, Seen> = HashMap::new();
    let mut registered: HashMap<String, i32> = HashMap::new();
    let mut transcripts: HashMap<String, PathBuf> = HashMap::new();
    let projects = claude_home.join("projects");

    reap_dead_on_start(&hub);
    forget_removed_on_start(&hub, &claude_home.join("jobs"));

    loop {
        let roster = read_roster(&claude_home.join("daemon/roster.json"));
        let dirs = job_dirs(&claude_home.join("jobs"));
        let present: std::collections::HashSet<&String> = dirs.iter().map(|(_, s)| s).collect();
        let gone: Vec<String> = seen.keys().filter(|k| !present.contains(k)).cloned().collect();
        for short in gone {
            if let Some(s) = seen.remove(&short) {
                registered.remove(&s.obs.session_id);
                let _ = hub.append(Event::NodeRemoved { session_id: s.obs.session_id });
            }
        }
        for (dir, short) in dirs {
            let state_path = dir.join("state.json");
            let Some(mtime) = mtime_millis(&state_path) else { continue };
            let pid = roster.get(&short).copied();
            let unchanged = seen
                .get(&short)
                .is_some_and(|s| s.mtime == mtime && s.obs.pid == pid);
            if unchanged {
                continue;
            }
            let Some(mut obs) = read_state(&state_path, &short) else { continue };
            match hub.admit(&obs.session_id, false) {
                Ok(true) => {}
                Ok(false) => continue,
                Err(e) => {
                    log!("could not tell whether {short} is Alondra's: {e}");
                    continue;
                }
            }
            obs.pid = pid;
            obs.updated_at = Some(mtime);
            if let Some(path) = transcript_path(&mut transcripts, &projects, &obs.session_id)
                && let Some((text, at)) = last_assistant_text(&path)
            {
                obs.last_text = Some(text);
                obs.last_text_at = at;
            }
            if let Some(path) = transcript_path(&mut transcripts, &projects, &obs.session_id) {
                obs.last_prompt = last_human_prompt(&path);
            }
            let meaningful = seen.get(&short).is_none_or(|s| differs(&s.obs, &obs) || mtime - s.recorded >= HEARTBEAT_MS);
            let mut recorded = seen.get(&short).map_or(mtime, |s| s.recorded);
            if meaningful {
                if hub.append_with(|v| (!v.removed.contains_key(&obs.session_id)
                    && !v.orchestrator_migration.as_ref().is_some_and(|m| m.session_id == obs.session_id))
                    .then(|| Event::DaemonSeen(obs.clone()))).is_err() {
                    log!("could not save the observation of {short}");
                }
                recorded = mtime;
            }
            if let Some(p) = pid
                && registered.get(&obs.session_id) != Some(&p)
            {
                registered.insert(obs.session_id.clone(), p);
                let _ = register.send((obs.session_id.clone(), p));
            }
            seen.insert(short, Seen { mtime, obs, recorded });
        }

        mark_attaches(&hub);

        while let Ok((session_id, pid)) = exit_rx.try_recv() {
            if registered.get(&session_id) == Some(&pid) {
                registered.remove(&session_id);
            }
            let _ = hub.append(Event::ProcessExited { session_id, pid });
        }

        tokio::select! {
            _ = tokio::time::sleep(SCAN_EVERY) => {}
            Some((session_id, pid)) = exit_rx.recv() => {
                if registered.get(&session_id) == Some(&pid) {
                    registered.remove(&session_id);
                }
                let _ = hub.append(Event::ProcessExited { session_id, pid });
            }
        }
    }
}

/// A `claude attach <id>` running on this Mac that Alondra did not open is the user watching
/// that node. An attach from inside `claude agents` leaves no process and is not seen.
fn mark_attaches(hub: &Hub) {
    let Ok(out) = std::process::Command::new("/bin/ps").args(["-Ao", "command"]).output() else { return };
    let attached = attached_shorts(&String::from_utf8_lossy(&out.stdout));
    let changes: Vec<Event> = hub.read(|v| {
        v.nodes
            .values()
            .filter_map(|n| {
                let now = attached.contains(n.short_id.as_str()) && !hub.is_own_attach(&n.short_id);
                (now != n.attached).then(|| Event::AttachChanged { session_id: n.session_id.clone(), attached: now })
            })
            .collect()
    });
    for e in changes {
        let _ = hub.append(e);
    }
}

fn attached_shorts(ps: &str) -> std::collections::HashSet<String> {
    ps.lines()
        .filter_map(|line| {
            let words: Vec<&str> = line.split_whitespace().collect();
            let at = words.iter().position(|w| *w == "attach")?;
            let program = words.get(at.checked_sub(1)?)?;
            let is_claude = program.rsplit('/').next().is_some_and(|p| p == "claude" || p == "claude.exe");
            is_claude.then(|| words.get(at + 1).map(|s| s.to_string())).flatten()
        })
        .collect()
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

fn forget_removed_on_start(hub: &Hub, jobs: &Path) {
    let removed: Vec<String> = hub.read(|v| {
        v.nodes
            .values()
            .filter(|n| n.in_daemon && !jobs.join(&n.short_id).join("state.json").exists())
            .map(|n| n.session_id.clone())
            .collect()
    });
    for session_id in removed {
        let _ = hub.append(Event::NodeRemoved { session_id });
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
        || a.last_text != b.last_text
        || a.tasks != b.tasks
        || a.task_kinds != b.task_kinds
        || a.background != b.background
}

fn transcript_path(cache: &mut HashMap<String, PathBuf>, projects: &Path, session_id: &str) -> Option<PathBuf> {
    if let Some(p) = cache.get(session_id)
        && p.exists()
    {
        return Some(p.clone());
    }
    let file = format!("{session_id}.jsonl");
    let found = std::fs::read_dir(projects)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path().join(&file))
        .find(|p| p.exists())?;
    cache.insert(session_id.to_string(), found.clone());
    Some(found)
}

const TAIL_BYTES: u64 = 512 * 1024;
const MESSAGE_MAX: usize = 16000;

fn last_assistant_text(path: &Path) -> Option<(String, Option<i64>)> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let mut candidates: Vec<(String, Option<i64>)> = vec![];
    for line in text.lines().rev() {
        if candidates.len() >= 3 {
            break;
        }
        if is_human_prompt(line) {
            break;
        }
        if !line.contains("\"assistant\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let Some(blocks) = v.get("message").and_then(|m| m.get("content")).and_then(Value::as_array) else { continue };
        let texts: Vec<&str> = blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        if texts.is_empty() {
            continue;
        }
        let at = v.get("timestamp").and_then(Value::as_str).and_then(parse_rfc3339_millis);
        let joined = clip(&texts.join("\n"), MESSAGE_MAX);
        candidates.push((joined, at));
    }
    let newest_at = candidates.first().and_then(|c| c.1);
    let best = candidates.into_iter().max_by_key(|c| c.0.chars().count())?;
    Some((best.0, newest_at.or(best.1)))
}

/// Cuts at the last blank line outside code that fits in `limit` characters, and says so with a closing `…`.
fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let (mut fence, mut boundary, mut at, mut chars) = (None, None, 0, 0);
    for line in text.split_inclusive('\n') {
        if chars > limit {
            break;
        }
        if line.trim().is_empty() && fence.is_none() && !text[..at].trim().is_empty() {
            boundary = Some(at);
        }
        fence = markdown::fence_after(fence, line);
        at += line.len();
        chars += line.chars().count();
    }
    if let Some(at) = boundary {
        return format!("{}\n\n…", text[..at].trim_end());
    }
    let cut: String = text.chars().take(limit).collect();
    let open = cut.lines().fold(None, markdown::fence_after);
    match open {
        Some(marker) => format!("{}\n{marker}\n\n…", cut.trim_end()),
        None => format!("{}\n\n…", cut.trim_end()),
    }
}

fn last_human_prompt(path: &Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_BYTES);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    let line = text.lines().rev().find(|l| is_human_prompt(l))?;
    let v: Value = serde_json::from_str(line).ok()?;
    let prompt = match v.get("message").and_then(|m| m.get("content"))? {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("\n"),
        _ => return None,
    };
    Some(prompt.chars().take(2000).collect())
}

fn is_human_prompt(line: &str) -> bool {
    if !line.contains("\"user\"") {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(line) else { return false };
    if v.get("type").and_then(Value::as_str) != Some("user") || v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(s)) => !s.starts_with('<'),
        Some(Value::Array(blocks)) => blocks.iter().any(|b| {
            b.get("type").and_then(Value::as_str) == Some("text")
                && b.get("text").and_then(Value::as_str).is_some_and(|t| !t.starts_with('<'))
        }),
        _ => false,
    }
}

fn parse_rfc3339_millis(s: &str) -> Option<i64> {
    let (date, time) = s.split_once('T')?;
    let mut d = date.split('-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let time = time.trim_end_matches('Z');
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.split(':').map(|x| x.parse::<i64>().ok());
    let (h, mi, sec) = (t.next()??, t.next()??, t.next()??);
    let ms: i64 = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400 + h * 3600 + mi * 60 + sec) * 1000) + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_are_cut_on_a_block_boundary() {
        let fence_inside = format!("intro\n\n```\n{}\n\n{}\n```", "a".repeat(10), "b".repeat(20));
        let open_fence = format!("text\n```rust\n{}", "x".repeat(40));
        let cases: Vec<(&str, String, usize, String)> = vec![
            ("short text is untouched", "one\n\ntwo".into(), 20, "one\n\ntwo".into()),
            ("exactly the limit is untouched", "a".repeat(20), 20, "a".repeat(20)),
            ("cut at the last blank line", "first\n\nsecond\n\nthird block is long".into(), 16, "first\n\nsecond\n\n…".into()),
            ("blank line at the limit counts", "0123456789\n\nmore text".into(), 11, "0123456789\n\n…".into()),
            ("blank line inside a fence is ignored", fence_inside, 25, "intro\n\n…".into()),
            ("no boundary: cut at the limit and close the fence", open_fence, 20, "text\n```rust\nxxxxxxx\n```\n\n…".into()),
            ("multibyte characters are cut whole", "ñ".repeat(30), 10, format!("{}\n\n…", "ñ".repeat(10))),
        ];
        for (name, text, limit, want) in cases {
            assert_eq!(clip(&text, limit), want, "{name}");
        }
    }

    #[test]
    fn only_claude_attach_processes_count_as_watching() {
        let ps = "COMMAND\n/Users/d/.claude/local/claude attach 0100014c\nclaude.exe attach 91933d17\nvim attach 12345678\nclaude agents\ntmux new-session -d -s alondra-aa claude attach aa\n";
        let shorts = attached_shorts(ps);
        assert!(shorts.contains("0100014c") && shorts.contains("91933d17") && shorts.contains("aa"));
        assert!(!shorts.contains("12345678"));
        assert_eq!(shorts.len(), 3);
    }

    #[test]
    fn parses_claude_timestamps() {
        assert_eq!(parse_rfc3339_millis("1970-01-01T00:00:01.500Z"), Some(1500));
        assert_eq!(parse_rfc3339_millis("2026-09-22T20:46:20.398Z"), Some(1790109980398));
    }

    #[test]
    fn background_tasks_come_from_the_job_state() {
        let dir = std::env::temp_dir().join(format!("alondra-watcher-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, r#"{"sessionId":"s","state":"working","tempo":"idle","inFlight":{"tasks":2,"queued":0,"kinds":["local_bash","local_agent"]}}"#).unwrap();
        let obs = read_state(&path, "s").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(obs.tasks, Some(2));
        assert_eq!(obs.task_kinds, vec!["local_bash", "local_agent"]);
        assert!(obs.background.is_empty(), "a job state without a fan lists nothing");
    }

    const REAL_FAN: &str = r#"{"sessionId":"s","inFlight":{"tasks":2,"queued":0,"kinds":["local_bash"],"drainableMonitors":0},
        "fan":[{"id":"a1ceb183712a76908","kind":"agent","label":"iOS fixes: PR card, diff, comment, close","startedAt":1790236267138,"doneAt":1790238241015},
               {"id":"b14mhd8w6","kind":"shell","label":"until [ -z \"$(pgrep -f 'bazelisk build //apple:AlondraiOS')\" ]; do sleep 5; done","startedAt":1790237431201}]}"#;

    #[test]
    fn running_background_items_come_from_the_fan_entries_not_done() {
        let v: Value = serde_json::from_str(REAL_FAN).unwrap();
        let items = running(&v, 2, 0);
        assert_eq!(items.len(), 1);
        assert_eq!((items[0].kind, items[0].started_at), (BackgroundKind::Shell, 1790237431201));
        assert!(running(&v, 0, 0).is_empty(), "nothing in flight means nothing runs");
        let long = format!("until   [ -z x ];\n{}", "do sleep 5; ".repeat(10));
        let fan = serde_json::json!({"fan": [
            {"kind": "shell", "label": long, "startedAt": 30},
            {"kind": "monitor", "label": "tail -f log", "startedAt": 20},
            {"kind": "agent", "label": "old", "startedAt": 10},
            {"kind": "shell", "label": "no start"},
            "x"
        ]});
        let items = running(&fan, 2, 1);
        assert_eq!(items.iter().map(|b| b.kind).collect::<Vec<_>>(), vec![BackgroundKind::Agent, BackgroundKind::Other, BackgroundKind::Shell]);
        assert!(items[2].label.chars().count() <= 80, "{}", items[2].label);
        assert!(items[2].label.starts_with("until [ -z x ]; do sleep 5;") && items[2].label.ends_with('…'));
        assert_eq!(running(&fan, 2, 0).iter().map(|b| b.started_at).collect::<Vec<_>>(), vec![20, 30], "only the newest the daemon counts");
    }

    #[test]
    fn the_job_state_carries_the_running_items() {
        let dir = std::env::temp_dir().join(format!("alondra-fan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.json");
        std::fs::write(&path, REAL_FAN).unwrap();
        let obs = read_state(&path, "s").unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(obs.tasks, Some(2));
        assert_eq!(obs.background.iter().map(|b| (b.kind, b.started_at)).collect::<Vec<_>>(), vec![(BackgroundKind::Shell, 1790237431201)]);
    }
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

pub fn claude_home() -> PathBuf {
    std::env::var("CLAUDE_HOME").map(PathBuf::from).unwrap_or_else(|_| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".claude")
    })
}

pub fn is_background_session(claude_home: &Path, session_id: &str) -> bool {
    let Some(short) = session_id.get(..8) else { return false };
    std::fs::read_to_string(claude_home.join("jobs").join(short).join("state.json"))
        .ok()
        .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
        .is_some_and(|v| v.get("sessionId").and_then(|s| s.as_str()) == Some(session_id))
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
    let in_flight = v.get("inFlight");
    let count = |k: &str| in_flight.and_then(|f| f.get(k)).and_then(Value::as_u64).unwrap_or(0) as usize;
    let background = running(&v, count("tasks"), count("queued"));
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
        last_text: None,
        last_text_at: None,
        last_prompt: None,
        intent: s("intent"),
        tasks: in_flight.and_then(|f| f.get("tasks")).and_then(Value::as_u64).map(|n| n as u32),
        task_kinds: in_flight
            .and_then(|f| f.get("kinds"))
            .and_then(Value::as_array)
            .map(|k| k.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default(),
        background,
    })
}

const LABEL_LIMIT: usize = 80;

/// The daemon's `fan` entries still running (no `doneAt`), newest last. Only as many as the
/// daemon counts in flight or queued: with none, nothing runs whatever the list says.
fn running(v: &Value, tasks: usize, queued: usize) -> Vec<BackgroundItem> {
    if tasks == 0 {
        return vec![];
    }
    let mut items: Vec<BackgroundItem> = v
        .get("fan")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|e| e.get("doneAt").is_none_or(Value::is_null))
        .filter_map(|e| {
            let kind = match e.get("kind").and_then(Value::as_str) {
                Some("agent") => BackgroundKind::Agent,
                Some("shell") => BackgroundKind::Shell,
                _ => BackgroundKind::Other,
            };
            let label = e.get("label").and_then(Value::as_str).unwrap_or_default();
            Some(BackgroundItem { kind, label: short_label(label), started_at: e.get("startedAt")?.as_i64()?, suspect: false })
        })
        .collect();
    items.sort_by_key(|b| b.started_at);
    let keep = items.len().min(tasks + queued);
    items.split_off(items.len() - keep)
}

fn short_label(label: &str) -> String {
    let flat = label.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= LABEL_LIMIT {
        return flat;
    }
    let cut: String = flat.chars().take(LABEL_LIMIT - 1).collect();
    format!("{}…", cut.trim_end())
}

fn spawn_pid_watcher(exits: mpsc::Sender<(String, i32)>) -> std::sync::mpsc::Sender<(String, i32)> {
    let (tx, rx) = std::sync::mpsc::channel::<(String, i32)>();
    std::thread::Builder::new()
        .name("alondra-pids".into())
        .spawn(move || kqueue_loop(rx, exits))
        .expect("could not start the process watcher");
    tx
}

fn kqueue_loop(rx: std::sync::mpsc::Receiver<(String, i32)>, exits: mpsc::Sender<(String, i32)>) {
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        log!("kqueue not available");
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

#[cfg(test)]
mod background_tests {
    use super::*;

    #[test]
    fn only_sessions_with_a_matching_job_are_background() {
        let home = std::env::temp_dir().join(format!("alondra-bg-{}", std::process::id()));
        let job = home.join("jobs/abcd1234");
        std::fs::create_dir_all(&job).unwrap();
        std::fs::write(job.join("state.json"), r#"{"sessionId":"abcd1234-0000-0000-0000-000000000000"}"#).unwrap();
        assert!(is_background_session(&home, "abcd1234-0000-0000-0000-000000000000"));
        assert!(!is_background_session(&home, "abcd1234-ffff-0000-0000-000000000000"));
        assert!(!is_background_session(&home, "99999999-0000-0000-0000-000000000000"));
        assert!(!is_background_session(&home, "corto"));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
