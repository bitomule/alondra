//! What a checkpoint records and how it is compared with what the case accepted: the views by
//! client and language, normalized so that nothing that changes from run to run is in them, and
//! the pushes sent since the last checkpoint. Instants stay as the daemon wrote them: the virtual
//! clock moves only when a step moves it, so each one is the same in every run.

use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

pub type Files = BTreeMap<String, String>;

/// Keys whose values differ from run to run without meaning anything to a reader.
const VOLATILE: &[&str] = &["data_epoch", "version", "cursor"];

/// The ids the daemon draws at random (`sdk-` and sixteen hex digits, the node of a task) and the
/// revision that ends the id of a row (`@25`): they differ from run to run and say nothing to a reader.
fn mask_ids(text: &str) -> String {
    let mut masked = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(['@', 's']) {
        masked.push_str(&rest[..at]);
        let tail = &rest[at..];
        let digits = tail.strip_prefix('@').map(|after| after.bytes().take_while(u8::is_ascii_digit).count()).unwrap_or(0);
        let hex = tail.strip_prefix("sdk-").map(|after| after.bytes().take_while(u8::is_ascii_hexdigit).count()).unwrap_or(0);
        if digits > 0 {
            masked.push_str("@<revision>");
            rest = &tail[1 + digits..];
        } else if hex >= 16 {
            masked.push_str("<node>");
            rest = &tail[4 + hex..];
        } else {
            masked.push_str(&tail[..1]);
            rest = &tail[1..];
        }
    }
    masked.push_str(rest);
    masked
}

pub fn normalize(value: &mut Value) {
    match value {
        Value::String(text) => *text = mask_ids(text),
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
