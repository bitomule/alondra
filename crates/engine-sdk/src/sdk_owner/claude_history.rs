use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Component, Path};

const WITNESS_BYTES: u64 = 8 * 1024 * 1024;

pub(super) fn available(environment: &BTreeMap<String, String>, session: &str, directory: &Path) -> bool {
    let Ok(session) = alondra_foundation::MessageUuid::new(session) else { return false };
    if !normal_absolute(directory) || !std::fs::symlink_metadata(directory).is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) {
        return false;
    }
    let Some(home) = environment.get("HOME").map(Path::new).filter(|home| normal_absolute(home)) else { return false };
    let profile = alondra_core::claude_account::configured_profile(&|key: &str| environment.get(key).cloned());
    if profile.as_ref().is_some_and(|profile| !normal_absolute(profile)) { return false; }
    let projects = alondra_core::claude_account::config_folder(profile.as_deref(), home).join("projects");
    // Managed Claude accounts deliberately share this root through a link to the native profile.
    let Ok(projects) = projects.canonicalize() else { return false };
    let Ok(buckets) = std::fs::read_dir(projects) else { return false };
    let file = format!("{}.jsonl", session.as_str());
    buckets.filter_map(Result::ok).any(|bucket| {
        bucket.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            && witnessed(&bucket.path().join(&file), session.as_str(), directory)
    })
}

fn normal_absolute(path: &Path) -> bool {
    path.is_absolute() && path.components().all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

fn witnessed(path: &Path, session: &str, directory: &Path) -> bool {
    let Some(directory) = directory.to_str() else { return false };
    let Ok(before) = std::fs::symlink_metadata(path) else { return false };
    if !before.is_file() || before.file_type().is_symlink() || before.len() == 0 { return false; }
    let Ok(file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC).open(path) else { return false };
    let Ok(opened) = file.metadata() else { return false };
    if !opened.is_file() || opened.dev() != before.dev() || opened.ino() != before.ino() { return false; }
    BufReader::new(file.take(WITNESS_BYTES)).lines().map_while(Result::ok).any(|line| {
        let Ok(record) = serde_json::from_str::<serde_json::Value>(&line) else { return false };
        let Some(kind @ ("user" | "assistant")) = record["type"].as_str() else { return false };
        record["sessionId"].as_str() == Some(session)
            && record["cwd"].as_str() == Some(directory)
            && record["message"]["role"].as_str() == Some(kind)
            && match &record["message"]["content"] {
                serde_json::Value::String(text) => !text.trim().is_empty(),
                serde_json::Value::Array(content) => content.iter().any(|block| block["type"] == "text" && block["text"].as_str().is_some_and(|text| !text.trim().is_empty())),
                _ => false,
            }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;
    use std::path::PathBuf;

    const SESSION: &str = "00000000-0000-4000-8000-000000000001";

    struct Fixture {
        _root: Directory,
        environment: BTreeMap<String, String>,
        directory: PathBuf,
        bucket: PathBuf,
        transcript: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let root = Directory::new();
            let directory = root.join("repo");
            let home = root.join("home");
            let bucket = home.join(".claude/projects/project");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::create_dir_all(&bucket).unwrap();
            let transcript = bucket.join(format!("{SESSION}.jsonl"));
            Self { _root: root, environment: BTreeMap::from([("HOME".into(), home.display().to_string())]), directory, bucket, transcript }
        }

        fn record(&self, kind: &str) -> serde_json::Value {
            serde_json::json!({"type": kind, "sessionId": SESSION, "cwd": self.directory, "message": {"role": kind, "content": "Retained native conversation"}})
        }

        fn write(&self, record: &serde_json::Value) {
            std::fs::write(&self.transcript, format!("{record}\n")).unwrap();
        }

        fn available(&self) -> bool { available(&self.environment, SESSION, &self.directory) }
    }

    #[test]
    fn native_user_and_assistant_history_prove_the_session_in_its_original_directory() {
        let f = Fixture::new();
        for kind in ["user", "assistant"] {
            f.write(&f.record(kind));
            assert!(f.available(), "a native {kind} record proves the retained session");
        }
        let mut blocks = f.record("assistant");
        blocks["message"]["content"] = serde_json::json!([{"type":"text", "text":"Retained assistant text"}]);
        f.write(&blocks);
        assert!(f.available());
    }

    #[test]
    fn missing_empty_or_non_conversation_files_do_not_prove_a_session() {
        let f = Fixture::new();
        assert!(!f.available());
        for body in ["", "\n", "not json\n", "{}\n"] {
            std::fs::write(&f.transcript, body).unwrap();
            assert!(!f.available(), "{body:?}");
        }
        for record in [
            serde_json::json!({"type":"system", "sessionId": SESSION, "cwd": f.directory}),
            serde_json::json!({"type":"user", "sessionId": SESSION, "cwd": f.directory, "message":{"role":"user", "content":""}}),
            serde_json::json!({"type":"assistant", "sessionId": SESSION, "cwd": f.directory, "message":{"role":"assistant", "content":[]}}),
        ] {
            f.write(&record);
            assert!(!f.available(), "{record}");
        }
    }

    #[test]
    fn another_session_directory_role_or_file_path_is_not_a_witness() {
        let f = Fixture::new();
        for (field, value) in [("sessionId", "00000000-0000-4000-8000-000000000002"), ("cwd", "/another/repo")] {
            let mut record = f.record("user");
            record[field] = serde_json::json!(value);
            f.write(&record);
            assert!(!f.available(), "{record}");
        }
        let mut wrong_role = f.record("user");
        wrong_role["message"]["role"] = serde_json::json!("assistant");
        f.write(&wrong_role);
        assert!(!f.available());
        f.write(&f.record("user"));
        assert!(f.available());
        assert!(!available(&f.environment, "../another", &f.directory));
        assert!(!available(&f.environment, SESSION, Path::new("relative/repo")));
        assert!(!available(&BTreeMap::new(), SESSION, &f.directory));
        std::fs::rename(&f.transcript, f.bucket.join("another.jsonl")).unwrap();
        assert!(!f.available());
        let nested = f.bucket.join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::rename(f.bucket.join("another.jsonl"), nested.join(format!("{SESSION}.jsonl"))).unwrap();
        assert!(!f.available(), "only immediate project buckets can hold the native session");
    }

    #[test]
    fn transcript_or_project_bucket_links_are_not_native_history_witnesses() {
        let f = Fixture::new();
        f.write(&f.record("user"));
        let retained = f.bucket.join("retained.jsonl");
        std::fs::rename(&f.transcript, &retained).unwrap();
        std::os::unix::fs::symlink(&retained, &f.transcript).unwrap();
        assert!(!f.available());
        std::fs::remove_file(&f.transcript).unwrap();
        std::fs::rename(&retained, &f.transcript).unwrap();
        let outside = f._root.join("outside-projects");
        std::fs::rename(&f.bucket, &outside).unwrap();
        std::os::unix::fs::symlink(outside, &f.bucket).unwrap();
        assert!(!f.available());
    }

    #[test]
    fn the_selected_profile_and_its_shared_projects_root_supply_the_native_history() {
        let mut f = Fixture::new();
        f.write(&f.record("user"));
        let managed = f._root.join("managed-profile");
        std::fs::create_dir(&managed).unwrap();
        f.environment.insert("CLAUDE_CONFIG_DIR".into(), managed.display().to_string());
        assert!(!f.available(), "history from another profile cannot stand in for the selected one");
        std::os::unix::fs::symlink(f.bucket.parent().unwrap(), managed.join("projects")).unwrap();
        assert!(f.available(), "managed accounts share the native profile's projects root deliberately");
        f.environment.remove("CLAUDE_CONFIG_DIR");
        f.environment.insert("CLAUDE_HOME".into(), managed.display().to_string());
        assert!(f.available());
    }
}
