//! Every transcript the repository keeps, a scenario's under `crates/scenarios` or a replayed one
//! under `fixtures/transcripts`, is anonymized: nothing of a real session, of a person or of this
//! Mac is in it, and its text is markers or what its scenario lists as synthetic.

use alondra_scenarios_transcript::guard;
use std::path::PathBuf;

fn workspace_dir(path: &str) -> PathBuf {
    let runfiles = PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"));
    let workspace = std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace");
    runfiles.join(workspace).join(path)
}

#[test]
fn every_transcript_of_the_repository_is_clean() {
    for root in [workspace_dir("crates/scenarios"), workspace_dir("fixtures/transcripts")] {
        assert!(!guard::transcripts(&root).is_empty(), "there is no transcript under {}", root.display());
        let findings = guard::check_tree(&root, &guard::literals_from_env());
        assert!(findings.is_empty(), "a transcript carries what must not be public:\n{}", findings.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"));
    }
}
