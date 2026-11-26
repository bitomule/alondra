//! Who owns each file under `~/.alondra/files`, and when it goes. The sweep lists what the rule
//! removes at a given time, `remove_listed` deletes it, and the retention loop does so every hour.
//!
//! - `tasks/<task>/` is a task's scratch: it goes when the task is closed.
//! - `results/<task>/` is a delivery kept for the card: it stays while its task is open or its card
//!   is in the inbox, and `RESULTS_KEPT_FOR` after.
//! - `uploads/`, `images/`, `domain-v2/` and `documents/` hold what cards and messages show: each
//!   stays while a card that names it is in the inbox or its task is open, or a message of an open
//!   task or request names it, and `STORED_KEPT_FOR` after.
//! - Anything else is not Alondra's: it is listed and never deleted.
use alondra_domain_model::{DomainState, InteractionId, Scope};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

const DAY: Duration = Duration::from_secs(24 * 60 * 60);
/// A saved result stays two weeks after its task closes and its card leaves the inbox: long enough
/// to read it again after a trip or a long weekend, and it weighs little.
pub const RESULTS_KEPT_FOR: Duration = Duration::from_secs(14 * 24 * 60 * 60);
/// What a card showed (uploads, images, copies of attachments) stays a month after its card and
/// task end: the part of the folder that weighs, so the period is the longer one.
pub const STORED_KEPT_FOR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

const CARD_FOLDERS: [&str; 4] = ["uploads", "images", "domain-v2", "documents"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Relative to the files folder.
    pub path: String,
    pub bytes: u64,
    /// A task, a card or nobody.
    pub owner: String,
    pub remove: bool,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub entries: Vec<Entry>,
    pub remove_bytes: u64,
    pub keep_bytes: u64,
}

/// A task's scratch folder, where its node and Alondra write what is not part of the work: it goes
/// with the task. A task id that would name another folder has none.
pub fn task_scratch(home: &Path, task: &str) -> Option<PathBuf> {
    (!task.is_empty() && !task.contains(['/', '\\']) && !task.starts_with('.')).then(|| home.join("files/tasks").join(task))
}

pub fn remove_task_scratch(home: &Path, task: &str) -> std::io::Result<()> {
    let Some(folder) = task_scratch(home, task) else { return Ok(()) };
    match std::fs::remove_dir_all(folder) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

/// Every entry under `home/files` with its owner and what the rule does with it at `now`.
pub fn sweep(home: &Path, state: &DomainState, now: SystemTime) -> Report {
    let files = home.join("files");
    let index = References::of(state);
    let mut report = Report::default();
    let Ok(top) = std::fs::read_dir(&files) else { return report };
    let mut tops: Vec<_> = top.flatten().collect();
    tops.sort_by_key(|entry| entry.file_name());
    for entry in tops {
        let name = entry.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "tasks" | "results") && entry.path().is_dir() {
            let mut children: Vec<_> = std::fs::read_dir(entry.path()).into_iter().flatten().flatten().collect();
            children.sort_by_key(|child| child.file_name());
            for child in children {
                let task = child.file_name().to_string_lossy().into_owned();
                let verdict = task_folder(&name, &task, &child.path(), state, now);
                report.push(format!("{name}/{task}"), &child.path(), format!("task {task}"), verdict);
            }
        } else if CARD_FOLDERS.contains(&name.as_str()) && entry.path().is_dir() {
            let mut children: Vec<_> = std::fs::read_dir(entry.path()).into_iter().flatten().flatten().collect();
            children.sort_by_key(|child| child.file_name());
            for child in children {
                let key = format!("{name}/{}", child.file_name().to_string_lossy());
                let (owner, verdict) = stored(&key, &child.path(), &index, state, now);
                report.push(key, &child.path(), owner, verdict);
            }
        } else {
            report.push(name, &entry.path(), "nobody".into(), (false, "it is not in one of Alondra's folders: listed, never deleted".into()));
        }
    }
    report
}

/// What `remove_listed` took off the disk.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Removed {
    pub entries: usize,
    pub bytes: u64,
}

/// Deletes every entry of `report` the rule marked for removal. An entry that is already gone or
/// cannot be deleted stays for the next pass and is logged.
pub fn remove_listed(home: &Path, report: &Report) -> Removed {
    let files = home.join("files");
    let mut removed = Removed::default();
    for entry in report.entries.iter().filter(|entry| entry.remove) {
        let path = files.join(&entry.path);
        let gone = match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&path),
            Ok(_) => std::fs::remove_file(&path),
            Err(error) => Err(error),
        };
        match gone {
            Ok(()) => {
                removed.entries += 1;
                removed.bytes += entry.bytes;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => alondra_foundation::log!("could not remove {}: {error}", entry.path),
        }
    }
    removed
}

impl Report {
    fn push(&mut self, path: String, on_disk: &Path, owner: String, (remove, reason): (bool, String)) {
        let bytes = size(on_disk);
        if remove {
            self.remove_bytes += bytes;
        } else {
            self.keep_bytes += bytes;
        }
        self.entries.push(Entry { path, bytes, owner, remove, reason });
    }
}

fn task_folder(folder: &str, task: &str, on_disk: &Path, state: &DomainState, now: SystemTime) -> (bool, String) {
    if state.tasks.iter().any(|(id, t)| id.0 == task && !t.state.terminal()) {
        return (false, "its task is open".into());
    }
    if folder == "tasks" {
        return (true, "its task is closed".into());
    }
    if result_in_inbox(task, state) {
        return (false, "its card is in the inbox".into());
    }
    keep_for(on_disk, now, RESULTS_KEPT_FOR, "its task is closed and its card was seen")
}

/// A card that names a result kept under `results/<task>/` is still waiting in the inbox.
fn result_in_inbox(task: &str, state: &DomainState) -> bool {
    state.interactions.values().any(|card| {
        state.open(card) && card.card.kept_at.as_deref().is_some_and(|at| at.contains(&format!("files/results/{task}/")))
    })
}

fn stored(key: &str, on_disk: &Path, index: &References, state: &DomainState, now: SystemTime) -> (String, (bool, String)) {
    if index.open_work.contains(&format!("files/{key}")) {
        return ("a message".into(), (false, "a message of an open task or request names it".into()));
    }
    let Some(cards) = index.cards.get(&format!("files/{key}")) else {
        if index.documents.contains(&format!("files/{key}")) {
            return ("a document".into(), (false, "a document revision uses it".into()));
        }
        return ("nobody".into(), unowned(on_disk, now));
    };
    let owner = cards.first().map_or_else(|| "nobody".to_owned(), |id| format!("card {}", id.0));
    let needed = cards.iter().filter_map(|id| state.interactions.get(id)).any(|card| {
        state.open(card)
            || matches!(&card.scope, Scope::Task { task_id } if state.tasks.get(task_id).is_some_and(|t| !t.state.terminal()))
    });
    if needed {
        return (owner, (false, "its card is in the inbox or its task is open".into()));
    }
    (owner, keep_for(on_disk, now, STORED_KEPT_FOR, "its card and task ended"))
}

fn unowned(on_disk: &Path, now: SystemTime) -> (bool, String) {
    keep_for(on_disk, now, STORED_KEPT_FOR, "no task, card or upload owns it")
}

fn keep_for(on_disk: &Path, now: SystemTime, period: Duration, why: &str) -> (bool, String) {
    let age = now.duration_since(newest(on_disk)).unwrap_or_default();
    let days = period.as_secs() / DAY.as_secs();
    if age >= period {
        (true, format!("{why}, and it is older than {days} days"))
    } else {
        (false, format!("{why}; kept until it is {days} days old"))
    }
}

/// When something under `path` last changed: its files, not the folders that hold them.
fn newest(path: &Path) -> SystemTime {
    let own = std::fs::symlink_metadata(path).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    if !path.is_dir() {
        return own;
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|child| newest(&child.path())).max().unwrap_or(own)
}

fn size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return 0 };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path).into_iter().flatten().flatten().map(|child| size(&child.path())).sum()
}

/// The `files/<folder>/<name>` paths that cards, documents and the messages of open work mention.
struct References {
    cards: BTreeMap<String, Vec<InteractionId>>,
    documents: BTreeSet<String>,
    open_work: BTreeSet<String>,
}

impl References {
    fn of(state: &DomainState) -> Self {
        let mut cards: BTreeMap<String, Vec<InteractionId>> = BTreeMap::new();
        let mut open_task_specs = BTreeSet::new();
        for (id, card) in &state.interactions {
            let Ok(text) = serde_json::to_string(card) else { continue };
            for key in mentioned(&text) {
                cards.entry(key).or_default().push(id.clone());
            }
        }
        for task in state.tasks.values().filter(|task| !task.specs.is_default() || !task.validation.is_default()) {
            let Ok(text) = serde_json::to_string(&(&task.specs, &task.validation)) else { continue };
            let held: Vec<InteractionId> = task.specs.approved.iter().map(|approved| approved.card.clone())
                .chain(task.specs.amendments.iter().filter_map(|amendment| amendment.card.clone()))
                .chain(task.specs.proposed_decisions.iter().map(|proposed| proposed.card.clone()))
                .chain(task.validation.delivered.iter().map(|delivered| delivered.delivery.clone()))
                .collect();
            for key in mentioned(&text) {
                if !task.state.terminal() {
                    open_task_specs.insert(key);
                } else {
                    cards.entry(key).or_default().extend(held.iter().cloned());
                }
            }
        }
        let documents = serde_json::to_string(&state.document_revisions).map(|text| mentioned(&text)).unwrap_or_default();
        let mut open_work = open_task_specs;
        for (task, journal) in state.tasks.values().filter(|task| !task.state.terminal()).filter_map(|task| Some((task, state.lifecycle_journal.get(&task.id)?))) {
            let Ok(text) = serde_json::to_string(&(journal, &task.pending_user_inputs)) else { continue };
            open_work.extend(mentioned(&text));
        }
        for request in state.requests.values().filter(|request| !request.closed) {
            open_work.extend(mentioned(&request.body));
        }
        Self { cards, documents, open_work }
    }
}

fn mentioned(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    for (at, _) in text.match_indices("files/") {
        let rest = &text[at..];
        let end = rest.find(|c: char| matches!(c, '"' | '\\' | ')' | ' ' | '?' | '\'' | '<' | '>' | '\n')).unwrap_or(rest.len());
        let mut parts = rest[..end].split('/');
        if let (Some("files"), Some(folder), Some(name)) = (parts.next(), parts.next(), parts.next()) {
            if !name.is_empty() {
                found.insert(format!("files/{folder}/{name}"));
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;

    fn aged(path: &Path, days: u64, now: SystemTime) {
        let file = std::fs::File::open(path).unwrap();
        file.set_modified(now - Duration::from_secs(days * 24 * 60 * 60)).unwrap();
    }

    #[test]
    fn a_file_outside_alondras_folders_is_listed_and_never_deleted() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("files/security-audit")).unwrap();
        let report = home.join("files/security-audit/report.md");
        std::fs::write(&report, "findings").unwrap();
        let now = SystemTime::now();
        aged(&report, 31, now);
        let state = DomainState::new("epoch");
        let swept = sweep(&home, &state, now);
        assert_eq!(swept.entries.len(), 1);
        assert_eq!((swept.entries[0].owner.as_str(), swept.entries[0].remove), ("nobody", false), "{:?}", swept.entries[0]);
        assert_eq!(remove_listed(&home, &swept), Removed::default());
        assert!(report.exists());
    }

    #[test]
    fn an_upload_a_message_of_an_open_task_names_stays_after_thirty_days() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("files/uploads/u-1")).unwrap();
        let image = home.join("files/uploads/u-1/x.png");
        std::fs::write(&image, "png").unwrap();
        let now = SystemTime::now();
        aged(&image, 31, now);
        let mut state = DomainState::new("epoch");
        let task = alondra_domain_model::TaskId::from("t");
        let open: alondra_domain_model::Task = serde_json::from_value(serde_json::json!({
            "id":"t","request_id":"r","agent_id":null,"title":"T","state":"running","revision":1,"started":true,"review_prs":false,
            "final_presentation":null,"pr_id":null,"later_prs":[],"dependencies":[],"documents":{},"archived":false,
        })).unwrap();
        state.tasks.insert(task.clone(), open);
        let told = alondra_domain_model::LifecycleRecord::Told {
            input: "input:t:2".into(), text: Some(format!("Look at this\n\nAttached: {}", image.display())), answers: None,
        };
        state.lifecycle_journal.insert(task.clone(), vec![told]);
        let swept = sweep(&home, &state, now);
        let entry = swept.entries.iter().find(|entry| entry.path == "uploads/u-1").unwrap();
        assert!(!entry.remove, "{entry:?}");
        state.tasks.get_mut(&task).unwrap().state = alondra_domain_model::TaskState::Done;
        assert!(sweep(&home, &state, now).entries.iter().any(|entry| entry.path == "uploads/u-1" && entry.remove), "it goes once its task ends");
    }

    #[test]
    fn a_mockup_the_specs_of_an_open_task_record_stays_after_thirty_days() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("files/images")).unwrap();
        let name = format!("{}.png", "a".repeat(64));
        let copy = home.join("files/images").join(&name);
        std::fs::write(&copy, "png").unwrap();
        let now = SystemTime::now();
        aged(&copy, 31, now);
        let mut state = DomainState::new("epoch");
        let task = alondra_domain_model::TaskId::from("t");
        let mut open: alondra_domain_model::Task = serde_json::from_value(serde_json::json!({
            "id":"t","request_id":"r","agent_id":null,"title":"T","state":"running","revision":1,"started":true,"review_prs":false,
            "final_presentation":null,"pr_id":null,"later_prs":[],"dependencies":[],"documents":{},"archived":false,
        })).unwrap();
        let mockup = alondra_domain_model::StoredImage { file: format!("files/images/{name}"), fingerprint: format!("sha256:{}", "a".repeat(64)), bytes: 3, width: 1, height: 1 };
        open.specs.draft = Some(alondra_domain_model::SpecDraft { level: alondra_domain_model::SpecLevel::Light,
            files: [("specs/billing/sheet.png".to_owned(), Some(alondra_domain_model::SpecContent::Mockup(mockup)))].into(),
            check: Default::default(), revision: 1, edited: false });
        state.tasks.insert(task.clone(), open);
        let key = format!("images/{name}");
        assert!(sweep(&home, &state, now).entries.iter().any(|entry| entry.path == key && !entry.remove), "the plan still needs it");
        state.tasks.get_mut(&task).unwrap().state = alondra_domain_model::TaskState::Done;
        assert!(sweep(&home, &state, now).entries.iter().any(|entry| entry.path == key && entry.remove), "it goes once its task ends");
    }

    #[test]
    fn removing_the_listed_entries_deletes_what_the_rule_marks_and_keeps_the_rest() {
        let home = Directory::new();
        let now = SystemTime::now();
        std::fs::create_dir_all(home.join("files/results/t-old")).unwrap();
        std::fs::create_dir_all(home.join("files/results/t-new")).unwrap();
        std::fs::write(home.join("files/results/t-old/result.md"), "old").unwrap();
        std::fs::write(home.join("files/results/t-new/result.md"), "new").unwrap();
        std::fs::create_dir_all(home.join("files/images")).unwrap();
        std::fs::write(home.join("files/images/stale.png"), "stale").unwrap();
        aged(&home.join("files/results/t-old/result.md"), 15, now);
        aged(&home.join("files/images/stale.png"), 31, now);
        let state = DomainState::new("epoch");
        let removed = remove_listed(&home, &sweep(&home, &state, now));
        assert_eq!(removed, Removed { entries: 2, bytes: ("old".len() + "stale".len()) as u64 });
        assert!(!home.join("files/results/t-old").exists() && !home.join("files/images/stale.png").exists());
        assert!(home.join("files/results/t-new/result.md").exists(), "a result younger than 14 days stays");
        assert_eq!(remove_listed(&home, &sweep(&home, &state, now)), Removed::default(), "nothing is left to remove");
    }

    #[test]
    fn a_folder_of_a_task_that_no_longer_exists_is_scratch_nobody_needs() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("files/tasks/t-gone")).unwrap();
        std::fs::write(home.join("files/tasks/t-gone/notes.txt"), "scratch").unwrap();
        let swept = sweep(&home, &DomainState::new("epoch"), SystemTime::now());
        assert_eq!((swept.entries[0].path.as_str(), swept.entries[0].remove), ("tasks/t-gone", true));
    }

    #[test]
    fn a_saved_result_of_a_closed_task_stays_for_two_weeks() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("files/results/t-old")).unwrap();
        let result = home.join("files/results/t-old/result-1.md");
        std::fs::write(&result, "report").unwrap();
        let now = SystemTime::now();
        aged(&result, 13, now);
        assert!(!sweep(&home, &DomainState::new("epoch"), now).entries[0].remove);
        aged(&result, 15, now);
        assert!(sweep(&home, &DomainState::new("epoch"), now).entries[0].remove);
    }

    #[test]
    fn cards_name_the_files_they_show_in_markdown_or_as_attachments() {
        let text = r#"{"body":"![a](/files?path=files/images/abc.png) and files/uploads/u-1/shot.png.","x":"files/domain-v2/ff00"}"#;
        let found: Vec<_> = mentioned(text).into_iter().collect();
        assert_eq!(found, ["files/domain-v2/ff00", "files/images/abc.png", "files/uploads/u-1"]);
    }

    #[test]
    fn only_the_scratch_folder_of_the_named_task_is_removed() {
        let home = Directory::new();
        std::fs::create_dir_all(home.join("files/tasks/t-a")).unwrap();
        std::fs::create_dir_all(home.join("files/tasks/t-b")).unwrap();
        std::fs::create_dir_all(home.join("files/results/t-a")).unwrap();
        remove_task_scratch(&home, "t-a").unwrap();
        remove_task_scratch(&home, "../results/t-a").unwrap();
        remove_task_scratch(&home, "t-missing").unwrap();
        assert!(!home.join("files/tasks/t-a").exists());
        assert!(home.join("files/tasks/t-b").exists());
        assert!(home.join("files/results/t-a").exists());
    }
}
