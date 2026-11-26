//! The specs of this repository, validated: every scenario is declared by a test or is a judgment,
//! and the ledger holds the current fingerprint of everything. It fails when a component with specs
//! changes and `alondra spec run` or a verdict is missing (`docs/architecture/specs.md`).
use std::path::PathBuf;

use alondra_spec_engine::{Files, ReportStatus, State, load};

/// The source files Bazel hands this test, read from its runfiles.
struct Runfiles {
    root: PathBuf,
    paths: Vec<String>,
}

impl Runfiles {
    fn new() -> Runfiles {
        let root = PathBuf::from(std::env::var("TEST_SRCDIR").expect("run it with bazel test")).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into()));
        let list = std::fs::read_to_string(root.join(env!("REPOSITORY_FILES"))).expect("the list of repository files is not in the runfiles");
        let paths = list.split_whitespace().map(str::to_owned).collect();
        Runfiles { root, paths }
    }
}

impl Files for Runfiles {
    fn paths(&self) -> Vec<String> {
        self.paths.clone()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        std::fs::read(self.root.join(path)).ok()
    }
}

#[test]
fn the_specs_of_this_repository_are_validated() {
    let project = match load(&Runfiles::new()) {
        Ok(project) => project,
        Err(problems) => panic!("the specs of this repository do not load: {problems:#?}"),
    };
    assert!(project.settings().specs.enabled, "alondra.toml does not turn specs on");
    assert!(!project.specs().is_empty(), "no spec was found: the runfiles are missing the specs folder");

    let report = project.report(None);

    let pending: Vec<String> = report.items.iter().filter(|item| item.state != State::Valid).map(|item| format!("{} {} {}", item.state.as_str(), item.kind.as_str(), item.subject)).collect();
    assert!(pending.is_empty(), "not validated (alondra spec validate):\n{}", pending.join("\n"));
    assert_eq!(report.status, ReportStatus::Clean);
    assert_eq!(report.leftover, Vec::<String>::new(), "ledger files of subjects that no longer exist");
}
