//! `sfltool` asks for an administrator on macOS 26 and later, and a check that runs it on its own
//! shows that prompt again and again: no Alondra code runs it.

use std::path::{Path, PathBuf};

fn sources(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display())) {
            sources(&entry.unwrap().path(), out);
        }
    } else if path.extension().is_some_and(|e| e == "rs") {
        out.push(path.to_path_buf());
    }
}

#[test]
fn no_alondra_source_runs_sfltool() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let mut files = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap().flatten() {
        sources(&entry.path().join("src"), &mut files);
    }
    assert!(files.iter().any(|f| f.ends_with("doctor/system.rs")), "the doctor's sources are not in {}", crates.display());
    let runs: Vec<String> = files.iter()
        .filter(|f| std::fs::read_to_string(f).unwrap().lines().any(|line| line.contains("\"sfltool") || line.contains("/sfltool")))
        .map(|f| f.display().to_string())
        .collect();
    assert!(runs.is_empty(), "sfltool is run from {runs:?}");
}
