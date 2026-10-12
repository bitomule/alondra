//! The CLI's old commands were removed at once, without aliases (docs/onboarding-and-cli-design.md
//! §3.5). This scans every text the repo ships or tells people to type, so no message, doc, script
//! or app screen sends anyone to a command that no longer exists. The plans and the design that
//! record the old API are history and stay as written.

use std::path::{Path, PathBuf};

const OLD: &[&str] = &[
    "setup", "pair", "devices", "machines", "connect", "login", "logout", "inbox",
    "orchestrator-cancel", "orchestrator-acknowledge", "orchestrator-retry",
];

const SCANNED: &[&str] = &[
    "crates", "plugin", "plugin-orchestrator", "apple", "web/app", "web/e2e", "tools", "deploy",
    "Makefile", "README.md", "INSTALL.md", "AGENTS.md", "docs/direct.md",
];

const SKIPPED_DIRS: &[&str] = &["node_modules", "target", "build", ".build", "DerivedData", "dist"];

const THIS_FILE: &str = "crates/alondra/tests/no_old_commands.rs";

/// Every old command a text names: `alondra <old>` as a whole word, `setup --<flag>` and
/// `config --get`.
fn old_names(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _) in text.match_indices("alondra ") {
        let rest = &text[at + "alondra ".len()..];
        let word: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
        if OLD.contains(&word.as_str()) {
            found.push(format!("alondra {word}"));
        }
    }
    for pattern in ["setup --", "config --get"] {
        if text.contains(pattern) {
            found.push(pattern.trim().into());
        }
    }
    found
}

/// The checkout this test runs from: Bazel's runfiles link the Makefile back to it.
fn workspace() -> PathBuf {
    let makefile = std::env::var("RUNFILES_DIR").or_else(|_| std::env::var("TEST_SRCDIR"))
        .map(|root| PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into())).join("Makefile"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Makefile"));
    let real = makefile.canonicalize().unwrap_or_else(|e| panic!("{}: {e}", makefile.display()));
    real.parent().expect("the Makefile is in the checkout's root").to_path_buf()
}

fn files(path: &Path, out: &mut Vec<PathBuf>) {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return };
    if meta.is_file() {
        out.push(path.to_path_buf());
    } else if meta.is_dir() {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if SKIPPED_DIRS.contains(&name) || name.starts_with("bazel-") || (name.starts_with('.') && name != ".claude-plugin") {
            return;
        }
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            files(&entry.path(), out);
        }
    }
}

#[test]
fn no_text_in_the_repo_names_a_removed_command() {
    let root = workspace();
    let mut paths = Vec::new();
    for scanned in SCANNED {
        files(&root.join(scanned), &mut paths);
    }
    assert!(paths.len() > 500, "the scan reached only {} files under {}", paths.len(), root.display());
    let mut left = Vec::new();
    for path in paths {
        let relative = path.strip_prefix(&root).unwrap_or(&path).display().to_string();
        if relative == THIS_FILE {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for name in old_names(&text) {
            left.push(format!("{relative}: {name}"));
        }
    }
    assert!(left.is_empty(), "texts that still name a removed command:\n{}", left.join("\n"));
}

#[test]
fn the_scan_finds_old_names_and_lets_the_new_ones_through() {
    assert_eq!(old_names("then run `alondra pair` again"), ["alondra pair"]);
    assert_eq!(old_names("alondra setup\nalondra orchestrator-retry r --confirm"), ["alondra setup", "alondra orchestrator-retry"]);
    assert_eq!(old_names("rerun setup --projects-root"), ["setup --"]);
    assert_eq!(old_names("LABEL = $(alondra config --get service_label)"), ["config --get"]);
    for new in ["alondra device invite", "alondra mac connect --host x", "alondra account login", "alondra pairing code",
                "alondra orchestrator retry r", "alondra doctor --fix", "alondra config get port", "Alondra setup wizard"] {
        assert!(old_names(new).is_empty(), "{new}");
    }
}
