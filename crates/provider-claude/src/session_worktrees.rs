//! The worktrees Claude Code made for the isolated subagents of a task's sessions, from the note it
//! keeps of each: `projects/<folder>/<session>/subagents/agent-<id>.meta.json` in its config
//! folder, with `worktreePath`, `worktreeBranch` and `spawnedWithWorktree`, nested subagents under
//! the root session. https://code.claude.com/docs/en/worktrees
use alondra_provider_contract::{Conversations, SessionWorktree, SessionWorktrees};
use std::path::{Component, Path, PathBuf};

const NOTE_BYTES: u64 = 64 * 1024;

/// Claude Code's notes, in the config folder `claude_home` names.
pub struct SubagentNotes {
    pub claude_home: fn() -> PathBuf,
}

impl SessionWorktrees for SubagentNotes {
    fn made_by(&self, conversations: &Conversations<'_>) -> Vec<SessionWorktree> {
        let home = (self.claude_home)();
        let mut sessions: Vec<String> = conversations.agent.map(str::to_owned).into_iter()
            .chain(conversations.records.iter().filter_map(|record| crate::conversation_session(record)))
            .collect();
        sessions.sort();
        sessions.dedup();
        let mut made: Vec<_> = sessions.iter().flat_map(|session| noted(&home, session)).collect();
        made.sort();
        made.dedup();
        made
    }
}

/// What Claude Code noted of the isolated subagents session `session` ran. Links and anything but
/// a plain note are not read.
fn noted(claude_home: &Path, session: &str) -> Vec<SessionWorktree> {
    let plain = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir());
    let session_like = !session.is_empty() && session.len() <= 64 && session.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    let Some(projects) = session_like.then(|| claude_home.join("projects").canonicalize().ok()).flatten() else { return vec![] };
    let mut noted = vec![];
    for bucket in std::fs::read_dir(projects).into_iter().flatten().filter_map(Result::ok) {
        let folder = bucket.path().join(session).join("subagents");
        if !plain(&bucket.path()) || !plain(&bucket.path().join(session)) || !plain(&folder) {
            continue;
        }
        for entry in std::fs::read_dir(folder).into_iter().flatten().filter_map(Result::ok) {
            let path = entry.path();
            let named = path.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("agent-") && name.ends_with(".meta.json"));
            if !named || !std::fs::symlink_metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() <= NOTE_BYTES) {
                continue;
            }
            let Some(note) = std::fs::read(&path).ok().and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok()) else { continue };
            if note["spawnedWithWorktree"].as_bool() != Some(true) {
                continue;
            }
            let Some(worktree) = note["worktreePath"].as_str().map(PathBuf::from) else { continue };
            if !worktree.is_absolute() || !worktree.components().all(|part| matches!(part, Component::RootDir | Component::Normal(_))) {
                continue;
            }
            noted.push(SessionWorktree { path: worktree, branch: note["worktreeBranch"].as_str().map(str::to_owned) });
        }
    }
    noted
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    thread_local! {
        static HOME: RefCell<PathBuf> = RefCell::new(PathBuf::new());
    }

    fn home() -> PathBuf {
        HOME.with(|home| home.borrow().clone())
    }

    fn notes_in(home: &Path) -> SubagentNotes {
        HOME.with(|current| *current.borrow_mut() = home.to_owned());
        SubagentNotes { claude_home: self::home }
    }

    fn note(home: &Path, session: &str, name: &str, body: serde_json::Value) {
        let folder = home.join("projects").join("-Users-ana-repo").join(session).join("subagents");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(name), body.to_string()).unwrap();
    }

    fn by_agent(agent: &str) -> Conversations<'_> {
        Conversations { agent: Some(agent), records: &[] }
    }

    #[test]
    fn the_worktrees_claude_code_noted_for_a_sessions_subagents_are_read_and_nothing_else() {
        let home = tempfile::tempdir().unwrap();
        let session = "4e335a6e-67f5-4ec2-9ea4-95c262b7dd56";
        note(home.path(), session, "agent-a1.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a1", "worktreeBranch": "worktree-agent-a1", "spawnedWithWorktree": true}));
        note(home.path(), session, "agent-a2.meta.json", serde_json::json!({"agentType": "Explore", "spawnedWithWorktree": false}));
        note(home.path(), session, "agent-a3.meta.json", serde_json::json!({"worktreePath": "r/agent-a3", "spawnedWithWorktree": true}));
        note(home.path(), session, "agent-a4.meta.json", serde_json::json!({"worktreePath": "/r/../etc", "spawnedWithWorktree": true}));
        note(home.path(), session, "agent-a5.jsonl", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a5", "spawnedWithWorktree": true}));
        note(home.path(), "other-session", "agent-a6.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a6", "spawnedWithWorktree": true}));
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("x.json"), serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-a7", "spawnedWithWorktree": true}).to_string()).unwrap();
        let folder = home.path().join("projects/-Users-ana-repo").join(session).join("subagents");
        std::os::unix::fs::symlink(outside.path().join("x.json"), folder.join("agent-a7.meta.json")).unwrap();
        let notes = notes_in(home.path());
        assert_eq!(notes.made_by(&by_agent(session)),
            vec![SessionWorktree { path: "/r/.claude/worktrees/agent-a1".into(), branch: Some("worktree-agent-a1".into()) }]);
        assert!(notes.made_by(&by_agent("../-Users-ana-repo")).is_empty(), "a session is one name, never a path");
    }

    #[test]
    fn the_native_session_a_conversations_record_names_is_read_and_a_record_claude_did_not_write_names_nothing() {
        let home = tempfile::tempdir().unwrap();
        let native = "0f3c2a1b-5d4e-4f60-8a9b-c0d1e2f3a4b5";
        note(home.path(), native, "agent-c1.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-c1", "spawnedWithWorktree": true}));
        let notes = notes_in(home.path());
        let records = vec![serde_json::json!({"session_id": native}).to_string().into_bytes(), serde_json::json!({"thread_id": native}).to_string().into_bytes()];
        assert_eq!(notes.made_by(&Conversations { agent: Some("sdk-node"), records: &records }).len(), 1);
        assert!(notes.made_by(&Conversations { agent: None, records: &records[1..] }).is_empty());
    }

    #[test]
    fn a_projects_folder_that_links_to_the_shared_one_is_read_through_its_link() {
        let shared = tempfile::tempdir().unwrap();
        note(shared.path(), "s1", "agent-b1.meta.json", serde_json::json!({"worktreePath": "/r/.claude/worktrees/agent-b1", "spawnedWithWorktree": true}));
        let account = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(shared.path().join("projects"), account.path().join("projects")).unwrap();
        assert_eq!(notes_in(account.path()).made_by(&by_agent("s1")).len(), 1);
    }
}
