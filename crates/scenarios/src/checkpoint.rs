//! What a checkpoint records and how it is compared with what the case accepted: the views by
//! client and language, normalized so that nothing that changes from run to run is in them, and
//! the pushes sent since the last checkpoint.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub type Files = BTreeMap<String, String>;

/// Keys whose values differ from run to run without meaning anything to a reader.
const VOLATILE: &[&str] = &["data_epoch", "version", "cursor"];

/// An ISO 8601 instant such as `2026-11-15T09:00:25.000+00:00`. The loops of the daemon and the
/// blocking threads under them race the paused clock, so an instant moves by seconds from run to
/// run; what the view says in words, to the minute, stays in it.
fn is_instant(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 19 && bytes[4] == b'-' && bytes[7] == b'-' && bytes[10] == b'T' && bytes[13] == b':'
        && bytes[..4].iter().all(u8::is_ascii_digit)
}

pub fn normalize(value: &mut Value) {
    match value {
        Value::String(text) if is_instant(text) => *text = "<instant>".into(),
        Value::Object(map) => {
            for (key, inner) in map.iter_mut() {
                if VOLATILE.contains(&key.as_str()) && inner.is_string() {
                    *inner = Value::String(format!("<{key}>"));
                } else {
                    normalize(inner);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize),
        _ => {}
    }
}

pub fn render(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("a json value renders");
    text.push('\n');
    text
}

/// A line-by-line difference, readable in a terminal: removed lines with `-`, added ones with `+`.
pub fn diff(accepted: &str, found: &str) -> String {
    let accepted: Vec<&str> = accepted.lines().collect();
    let found: Vec<&str> = found.lines().collect();
    let mut out = String::new();
    let mut shown = 0;
    for index in 0..accepted.len().max(found.len()) {
        let (old, new) = (accepted.get(index), found.get(index));
        if old == new {
            continue;
        }
        if shown == 40 {
            out.push_str("… more differences\n");
            break;
        }
        shown += 1;
        out.push_str(&format!("line {}:\n", index + 1));
        if let Some(old) = old {
            out.push_str(&format!("- {old}\n"));
        }
        if let Some(new) = new {
            out.push_str(&format!("+ {new}\n"));
        }
    }
    out
}

/// The accepted files of one checkpoint, as they are in `cases/<case>/checkpoints/<name>/`.
pub fn accepted(case: &Path, name: &str) -> Files {
    let directory = case.join("checkpoints").join(name);
    let mut files = Files::new();
    if let Ok(entries) = std::fs::read_dir(directory) {
        for entry in entries.flatten() {
            if let Ok(text) = std::fs::read_to_string(entry.path()) {
                files.insert(entry.file_name().to_string_lossy().into_owned(), text);
            }
        }
    }
    files
}

pub fn compare(accepted: &Files, found: &Files) -> BTreeMap<String, String> {
    let mut differences = BTreeMap::new();
    for name in accepted.keys().chain(found.keys()) {
        let (old, new) = (accepted.get(name), found.get(name));
        if old != new {
            let text = match (old, new) {
                (Some(old), Some(new)) => diff(old, new),
                (None, Some(_)) => "not accepted yet; run `bazel run //crates/scenarios:accept -- <case>`".to_owned(),
                (Some(_), None) => "accepted, but no longer read".to_owned(),
                (None, None) => unreachable!("the name comes from one of the two"),
            };
            differences.insert(name.clone(), text);
        }
    }
    differences
}

pub fn write(case: &Path, name: &str, found: &Files) -> std::io::Result<()> {
    let directory = case.join("checkpoints").join(name);
    if directory.exists() {
        std::fs::remove_dir_all(&directory)?;
    }
    std::fs::create_dir_all(&directory)?;
    for (file, text) in found {
        std::fs::write(directory.join(file), text)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn what_changes_from_run_to_run_is_masked_and_the_rest_is_kept() {
        let mut view = json!({"data_epoch": "abc", "sections": [{"version": "v1", "title": "Inbox", "at": "2026-11-15T09:00:25.000+00:00"}]});
        normalize(&mut view);
        assert_eq!(view, json!({"data_epoch": "<data_epoch>", "sections": [{"version": "<version>", "title": "Inbox", "at": "<instant>"}]}));
    }

    #[test]
    fn a_difference_names_the_file_and_the_lines() {
        let accepted = Files::from([("home.ios.es.json".to_owned(), "a\nb\n".to_owned())]);
        let found = Files::from([("home.ios.es.json".to_owned(), "a\nc\n".to_owned())]);
        let differences = compare(&accepted, &found);
        assert_eq!(differences["home.ios.es.json"], "line 2:\n- b\n+ c\n");
        assert!(compare(&accepted, &accepted).is_empty());
    }
}
