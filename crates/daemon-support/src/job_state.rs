//! What Claude Code itself says a background session is doing. This reads Claude Code's
//! undocumented job state (`~/.claude/jobs/<short id>/state.json`), as observed in 2.1.284: the
//! file Claude Code's own agent list renders from. It may change between versions, so anything
//! missing or unparsable is simply "no job state" and the node mod's events and the session
//! process's CPU decide instead.
use alondra_core::Millis;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Working,
    Idle,
    Blocked,
    Done,
    Failed,
    /// The user stopped or killed the session in Claude agents (`claude stop`). A crash is
    /// `crashed`, which this does not read: Claude Code restarts such a session itself.
    Stopped,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub phase: Phase,
    /// What it does now, or the error when it failed.
    pub detail: String,
    /// Tasks Claude Code counts as in flight: background shells, subagents, monitors.
    pub tasks: u64,
    pub updated_at: Millis,
    /// The `--settings` file `claude respawn` passes the session again.
    pub settings: Option<PathBuf>,
    /// The worktree Claude Code made for the session, which `claude rm` deletes with it.
    pub worktree: Option<PathBuf>,
}

/// None unless the file names `session` and carries a state and an update time.
pub fn parse(text: &str, session: &str) -> Option<Job> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let named = |field: &str| value[field].as_str() == Some(session);
    if !named("sessionId") && !named("resumeSessionId") {
        return None;
    }
    let phase = match value["state"].as_str()? {
        "working" => Phase::Working,
        "idle" => Phase::Idle,
        "blocked" => Phase::Blocked,
        "done" => Phase::Done,
        "failed" => Phase::Failed,
        "stopped" => Phase::Stopped,
        _ => return None,
    };
    let updated_at = crate::watcher::parse_rfc3339_millis(value["updatedAt"].as_str()?)?;
    let flags = value["respawnFlags"].as_array();
    let settings = flags.and_then(|flags| flags.iter().position(|flag| flag == "--settings").and_then(|at| flags.get(at + 1)))
        .and_then(|path| path.as_str()).map(PathBuf::from);
    Some(Job {
        phase,
        detail: value["detail"].as_str().unwrap_or_default().to_owned(),
        tasks: value["inFlight"]["tasks"].as_u64().unwrap_or(0),
        updated_at,
        settings,
        worktree: value["worktreePath"].as_str().map(PathBuf::from),
    })
}

/// The folder of the session's job under `claude_home`: the one Claude Code names with the first
/// eight characters of the session id, else any whose file names the session. The caller keeps it.
pub fn locate(claude_home: &Path, session: &str) -> Option<PathBuf> {
    let jobs = claude_home.join("jobs");
    let short: String = session.chars().take(8).collect();
    let names = |dir: &Path| read_in(dir, session).is_some();
    let direct = jobs.join(short);
    if names(&direct) {
        return Some(direct);
    }
    std::fs::read_dir(&jobs).ok()?.filter_map(Result::ok).map(|entry| entry.path()).find(|dir| names(dir))
}

pub fn read_in(dir: &Path, session: &str) -> Option<Job> {
    std::fs::read_to_string(dir.join("state.json")).ok().and_then(|text| parse(&text, session))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = "8f89f635-2e38-49f1-871e-2e220521f157";

    fn file(state: &str, extra: &str) -> String {
        format!(r#"{{"state":"{state}","detail":"d","tempo":"active","sessionId":"{SESSION}","resumeSessionId":"{SESSION}","updatedAt":"2026-10-05T08:14:44.300Z"{extra}}}"#)
    }

    #[test]
    fn every_state_claude_code_writes_is_read() {
        for (text, phase) in [("working", Phase::Working), ("idle", Phase::Idle), ("blocked", Phase::Blocked), ("done", Phase::Done), ("failed", Phase::Failed), ("stopped", Phase::Stopped)] {
            assert_eq!(parse(&file(text, ""), SESSION).unwrap().phase, phase, "{text}");
        }
    }

    #[test]
    fn in_flight_tasks_the_settings_to_respawn_with_and_the_time_are_read() {
        let extra = r#","inFlight":{"tasks":2,"queued":0,"kinds":["local_bash"]},"respawnFlags":["--name","n","--settings","/h/.alondra/runtime/sessions/launch-1.json","--model","opus"]"#;
        let job = parse(&file("working", extra), SESSION).unwrap();
        assert_eq!(job.tasks, 2);
        assert_eq!(job.settings.as_deref(), Some(Path::new("/h/.alondra/runtime/sessions/launch-1.json")));
        assert_eq!(job.updated_at, crate::watcher::parse_rfc3339_millis("2026-10-05T08:14:44.300Z").unwrap());
        assert_eq!(parse(&file("working", ""), SESSION).unwrap().tasks, 0, "a job that never listed in-flight work has none");
    }

    #[test]
    fn a_failed_launch_carries_its_error() {
        let text = r#"{"state":"failed","detail":"exit 1 before init","sessionId":"8f89f635-2e38-49f1-871e-2e220521f157","updatedAt":"2026-10-05T07:39:07.676Z"}"#;
        assert_eq!(parse(text, SESSION).unwrap().detail, "exit 1 before init");
    }

    #[test]
    fn a_file_that_is_missing_unparsable_or_lacks_the_fields_is_no_job_state() {
        let scratch_directory = crate::test_support::Directory::new();
        assert_eq!(parse("{not json", SESSION), None);
        assert_eq!(parse(&file("somethingnew", ""), SESSION), None, "an unknown state");
        assert_eq!(parse(&file("crashed", ""), SESSION), None, "a crash is not a stop");
        assert_eq!(parse(&file("working", "").replace("\"updatedAt\"", "\"touchedAt\""), SESSION), None, "no update time");
        assert_eq!(parse(&file("working", "").replace(SESSION, "another-session"), SESSION), None, "another session's file");
        let home = scratch_directory.parent().unwrap().join(format!("alondra-jobs-{}", crate::auth::random_hex(8)));
        assert_eq!(locate(&home, SESSION), None, "no jobs folder");
        std::fs::create_dir_all(home.join("jobs/8f89f635")).unwrap();
        assert_eq!(locate(&home, SESSION), None, "no state file");
        std::fs::write(home.join("jobs/8f89f635/state.json"), file("working", "")).unwrap();
        assert_eq!(locate(&home, SESSION), Some(home.join("jobs/8f89f635")));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn the_job_is_found_by_the_session_id_when_its_folder_is_not_named_after_it() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!("alondra-jobs-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(home.join("jobs/elsewhere")).unwrap();
        std::fs::write(home.join("jobs/elsewhere/state.json"), file("idle", "")).unwrap();
        let dir = locate(&home, SESSION).unwrap();
        assert_eq!(dir, home.join("jobs/elsewhere"));
        assert_eq!(read_in(&dir, SESSION).unwrap().phase, Phase::Idle);
        std::fs::remove_dir_all(home).unwrap();
    }
}
