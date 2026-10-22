//! What a TUI restarting on a new binary hands to the new process: the open screen, the
//! selection and scroll, and the text being written. Saved before `exec`, read and deleted by
//! the process that starts.

use std::path::{Path, PathBuf};

use alondra_core::domain::AgentId;
use serde::{Deserialize, Serialize};

use super::dropped::Chip;
use super::tasks::TaskKey;
use super::{Key, Target};

pub const ENV: &str = "ALONDRA_TUI_RESTORE";

#[derive(Serialize, Deserialize)]
pub struct Restore {
    /// The daemon version this restart was for: the new process never restarts again for it.
    pub restarted_for: String,
    pub screen: String,
    pub schedules_open: bool,
    #[serde(default)]
    pub rules_open: bool,
    pub detail_focused: bool,
    pub sel: Option<Key>,
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub task_sel: Option<TaskKey>,
    pub task_scroll: usize,
    pub task_detail_scroll: usize,
    pub agent_sel: Option<AgentId>,
    pub agent_scroll: usize,
    pub agent_detail_scroll: usize,
    pub composer: Option<OpenComposer>,
}

#[derive(Serialize, Deserialize)]
pub struct OpenComposer {
    pub target: Target,
    pub text: String,
    pub chips: Vec<Chip>,
}

pub fn save(restore: &Restore) -> std::io::Result<PathBuf> {
    let tmp = std::env::var_os("TMPDIR").map_or_else(|| PathBuf::from("/tmp"), PathBuf::from);
    save_in(&tmp, restore)
}

fn save_in(dir: &Path, restore: &Restore) -> std::io::Result<PathBuf> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = dir.join(format!("alondra-tui-restore-{}.json", std::process::id()));
    let bytes = serde_json::to_vec(restore)?;
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&path)?;
    file.write_all(&bytes)?;
    Ok(path)
}

/// The file the previous process left, read once and deleted whatever it holds.
pub fn take() -> Option<Restore> {
    let path = std::env::var_os(ENV).map(PathBuf::from)?;
    // SAFETY: called once at startup, before any thread of this process exists.
    unsafe { std::env::remove_var(ENV) };
    take_from(&path)
}

fn take_from(path: &Path) -> Option<Restore> {
    let bytes = std::fs::read(path);
    let _ = std::fs::remove_file(path);
    serde_json::from_slice(&bytes.ok()?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::dropped::Upload;

    #[test]
    fn the_open_composer_travels_to_the_new_process_and_the_file_is_gone_after_it_is_read() {
        let root = crate::test_support::Directory::new();
        let dir = root.join("tmp");
        std::fs::create_dir_all(&dir).unwrap();
        let chip = Chip {
            seq: 3,
            name: "shot.png".into(),
            bytes: 10,
            upload: Upload::Stored(serde_json::from_value(serde_json::json!({
                "id": "att-1", "name": "shot.png", "content_digest": "0".repeat(64),
                "content_reference": "files/att-1", "bytes": 10
            })).unwrap()),
        };
        let restore = Restore {
            restarted_for: "0.2.53".into(),
            screen: "tasks".into(),
            schedules_open: false,
            rules_open: false,
            detail_focused: true,
            sel: Some(Key("card-1".into())),
            list_scroll: 4,
            detail_scroll: 9,
            task_sel: Some(TaskKey::Task("t-1".into())),
            task_scroll: 2,
            task_detail_scroll: 1,
            agent_sel: None,
            agent_scroll: 0,
            agent_detail_scroll: 0,
            composer: Some(OpenComposer { target: Target::General { send: true }, text: "half a reply".into(), chips: vec![chip] }),
        };
        let path = save_in(&dir, &restore).unwrap();

        let back = take_from(&path).expect("the new process reads what the old one saved");

        assert!(!path.exists());
        assert_eq!(back.restarted_for, "0.2.53");
        assert_eq!(back.screen, "tasks");
        assert_eq!(back.sel, Some(Key("card-1".into())));
        assert_eq!((back.list_scroll, back.detail_scroll), (4, 9));
        let composer = back.composer.unwrap();
        assert_eq!(composer.text, "half a reply");
        assert!(matches!(composer.target, Target::General { send: true }));
        assert_eq!(composer.chips[0].name, "shot.png");
        assert!(take_from(&path).is_none());
    }
}
