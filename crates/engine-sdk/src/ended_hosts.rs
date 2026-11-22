//! The folders of hosts that ended, kept beside the folder of the host that replaced them as
//! evidence of what it received. Nothing else reads them once their task is over or a later host
//! ended after them.
use crate::domain_runtime::DomainRuntime;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::SystemTime;

const ENDED: &str = ".ended-";

/// Who an ended host's folder belongs to.
pub enum Owner {
    /// No task is known by that node: its folders are kept, since nothing says they are spent.
    Unknown,
    /// The task is over or cancelled.
    Finished,
    /// The task is open; `doubts` when an input it owes may still be settled from what the ended
    /// hosts received.
    Open { doubts: bool },
}

/// The folders to delete: every one of a finished task, and of an open task all but the last host
/// that ended, unless the task still has an input to settle.
pub fn spent(folders: &[(String, SystemTime)], owner: impl Fn(&str) -> Owner) -> Vec<String> {
    let mut by_node: BTreeMap<&str, Vec<&(String, SystemTime)>> = BTreeMap::new();
    for folder in folders {
        if let Some((node, _)) = folder.0.split_once(ENDED) {
            by_node.entry(node).or_default().push(folder);
        }
    }
    let mut spent = Vec::new();
    for (node, mut ended) in by_node {
        ended.sort_by_key(|(name, at)| (*at, name.clone()));
        match owner(node) {
            Owner::Unknown | Owner::Open { doubts: true } => {}
            Owner::Finished => spent.extend(ended.iter().map(|(name, _)| name.clone())),
            Owner::Open { doubts: false } => {
                ended.pop();
                spent.extend(ended.iter().map(|(name, _)| name.clone()));
            }
        }
    }
    spent
}

/// Deletes the spent folders of `root` and returns how many went.
pub fn sweep(domain: &DomainRuntime, root: &Path) -> std::io::Result<usize> {
    let Ok(state) = domain.state() else { return Ok(0) };
    let mut folders = Vec::new();
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.contains(ENDED) && entry.file_type()?.is_dir() {
            folders.push((name, entry.metadata()?.modified()?));
        }
    }
    let task_of = |node: &str| state.tasks.values().find(|task| task.agent_id.as_ref().is_some_and(|agent| agent.0 == node));
    let owner = |node: &str| match task_of(node) {
        None => Owner::Unknown,
        Some(task) if task.over() || task.archived => Owner::Finished,
        Some(task) => Owner::Open {
            doubts: state.lifecycle_of(&task.id).is_ok_and(|lifecycle| !lifecycle.pending_inputs().is_empty()),
        },
    };
    let mut removed = 0;
    for name in spent(&folders, owner) {
        std::fs::remove_dir_all(root.join(&name))?;
        removed += 1;
    }
    Ok(removed)
}

#[cfg(all(test, not(feature = "whole-engine")))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(seconds: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn folders() -> Vec<(String, SystemTime)> {
        ["done.ended-a", "done.ended-b", "open.ended-a", "open.ended-b", "open.ended-c", "owing.ended-a", "owing.ended-b", "alien.ended-a", "alien"]
            .iter().enumerate().map(|(index, name)| ((*name).to_owned(), at(index as u64))).collect()
    }

    #[test]
    fn a_finished_tasks_folders_go_an_open_tasks_keep_only_the_last_and_an_owing_one_keeps_all() {
        let owner = |node: &str| match node {
            "done" => Owner::Finished,
            "open" => Owner::Open { doubts: false },
            "owing" => Owner::Open { doubts: true },
            _ => Owner::Unknown,
        };
        let mut gone = spent(&folders(), owner);
        gone.sort();
        assert_eq!(gone, ["done.ended-a", "done.ended-b", "open.ended-a", "open.ended-b"]);
    }
}
