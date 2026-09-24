//! David decided nodes are never "awake" or "asleep" to the user: only their state in Alondra's
//! state machine (working, waiting for an answer, waiting for machine, your turn, stalled, done)
//! is user-facing. Whether a Claude Code session is attached is an internal daemon detail. This
//! scans every user-facing text source for the words that would leak that distinction back in.

use std::path::Path;

/// Case-insensitive, whole-word-ish forbidden stems. `wake` alone is not included: it is also
/// the name of the internal `ALONDRA_WAKE`/`config.wake` attach-capability flag, which is fine
/// to keep as an identifier as long as it is never rendered to the user as "awake"/"wakes up".
const FORBIDDEN: &[&str] = &["awake", "asleep", "wakes up", "wake up", "dormant", "sleeping", "despiert", "dormid"];

fn check(label: &str, text: &str) {
    let lower = text.to_lowercase();
    for word in FORBIDDEN {
        assert!(!lower.contains(word), "{label} still mentions {word:?}, which the user should never see again");
    }
}

#[test]
fn the_tui_and_cli_never_say_awake_or_asleep_again() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    for rel in ["src/tui.rs", "src/tui/draw.rs", "src/doctor.rs"] {
        let path = root.join(rel);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        check(rel, &text);
    }
}

#[test]
fn the_apple_localization_catalogs_never_say_awake_or_asleep_again() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for rel in ["apple/macOS/Resources/Localizable.xcstrings", "apple/iOS/Resources/Localizable.xcstrings", "apple/Shared/Resources/AlondraShared.xcstrings"] {
        let path = repo_root.join(rel);
        let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
        check(rel, &text);
    }
}
