use crate::GhCli;
use alondra_engine_ports::github::{DiffError, Gh, GhFailure, HeadError};

fn fixture() -> GhCli {
    GhCli::at(std::env::var_os("FIXTURE_DIFF_GH").expect("Bazel provides the gh fixture").into())
}

#[tokio::test]
async fn the_process_reads_keep_the_github_contract() {
    let gh = fixture();
    for case in alondra_engine_doubles::contract::github::CASES {
        alondra_engine_doubles::contract::github::check(&gh, case).await;
    }
}

#[tokio::test(start_paused = true)]
async fn a_head_process_that_never_answers_uses_the_existing_deadline() {
    let started = tokio::time::Instant::now();
    let result = fixture().head_oid("https://github.com/me/app/pull/99").await;
    assert!(matches!(result, Err(HeadError::Unavailable(GhFailure::TimedOut(_)))), "{result:?}");
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(20));
}

#[tokio::test(start_paused = true)]
async fn a_diff_process_that_never_answers_uses_the_existing_deadline() {
    let started = tokio::time::Instant::now();
    assert_eq!(fixture().diff("https://github.com/me/app/pull/99").await, Err(DiffError::Read));
    assert_eq!(started.elapsed(), std::time::Duration::from_secs(60));
}

#[tokio::test]
async fn missing_executables_keep_the_launch_failure() {
    let directory = crate::test_support::Directory::new();
    let gh = GhCli::at(directory.join("absent"));
    assert!(matches!(gh.head_oid("https://github.com/me/app/pull/1").await,
        Err(HeadError::Unavailable(GhFailure::Launch(std::io::ErrorKind::NotFound, _)))));
    assert!(matches!(gh.diff("https://github.com/me/app/pull/1").await,
        Err(DiffError::Start(GhFailure::Launch(std::io::ErrorKind::NotFound, _)))));
}
