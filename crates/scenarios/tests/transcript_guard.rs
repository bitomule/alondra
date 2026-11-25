//! Every transcript the repository keeps, wherever it is and whatever its name, is anonymized:
//! nothing of a real session, of a person or of this Mac is in it, and its text is markers or what
//! its scenario lists as synthetic. Without the list of private literals the check does not pass:
//! a private name is only found by its list.

use alondra_scenarios_transcript::guard;
use std::path::PathBuf;

fn checkout() -> PathBuf {
    let runfiles = PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"));
    runfiles.join(std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace"))
}

#[test]
fn every_transcript_of_the_repository_is_clean() {
    let root = checkout();
    let declared = std::env::var_os(guard::PRIVATE_LITERALS_ENV).map(|value| root.join(value));
    let literals = guard::literals_from(declared.as_deref().map(std::path::Path::as_os_str)).unwrap_or_else(|error| panic!("{error}"));
    let transcripts = guard::transcripts(&root);
    for kept in ["crates/scenarios/cases", "fixtures/transcripts", "crates/provider-conformance/transcripts"] {
        assert!(transcripts.iter().any(|path| path.starts_with(root.join(kept))), "there is no transcript under {kept}");
    }
    let findings = guard::check_tree(&root, &literals);
    assert!(findings.is_empty(), "a transcript carries what must not be public:\n{}", findings.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"));
}
