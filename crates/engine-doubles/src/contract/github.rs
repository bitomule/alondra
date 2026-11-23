//! The same PR read contract is run against the memory double and the process adapter.
use alondra_domain_rules::{PrState, PullRequest};
use alondra_engine_ports::github::{DiffError, Gh, GhFailure, HeadError, Mark, Marks, Viewed, MAX_PR_DIFF_BYTES};

pub const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
pub const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n+a\ndiff --git a/src/b.rs b/src/b.rs\n+b\n";

#[derive(Clone, Copy, Debug)]
pub enum Case { Complete, EmptyMarks, Failed, InvalidJson, InvalidHead, ExactLimit, Truncated }
pub const CASES: [Case; 7] = [Case::Complete, Case::EmptyMarks, Case::Failed, Case::InvalidJson, Case::InvalidHead, Case::ExactLimit, Case::Truncated];

pub fn number(case: Case) -> u64 { case as u64 + 1 }
pub fn url(case: Case) -> String { format!("https://github.com/me/app/pull/{}", number(case)) }
pub fn marks() -> Marks {
    Marks { node: "PR_node".into(), head: HEAD.into(), files: vec![
        Mark { path: "src/a.rs".into(), viewed: Viewed::Viewed },
        Mark { path: "src/b.rs".into(), viewed: Viewed::Changed },
        Mark { path: "src/c.rs".into(), viewed: Viewed::Unviewed },
    ] }
}

pub async fn check(gh: &dyn Gh, case: Case) {
    let url = url(case);
    let head = gh.head_oid(&url).await;
    match case {
        Case::InvalidJson => assert_eq!(head, Err(HeadError::InvalidResponse)),
        Case::InvalidHead => assert_eq!(head, Err(HeadError::InvalidHead)),
        Case::Failed => assert!(matches!(head, Err(HeadError::Unavailable(GhFailure::Exit(Some(1), _))))),
        _ => assert_eq!(head, Ok(HEAD.into())),
    }
    let diff = gh.diff(&url).await;
    match case {
        Case::Failed => assert_eq!(diff, Err(DiffError::Finish)),
        Case::ExactLimit | Case::Truncated => {
            let diff = diff.unwrap();
            let truncated = matches!(case, Case::Truncated);
            assert_eq!(diff.truncated, truncated);
            assert_eq!(diff.bytes, vec![b'a'; MAX_PR_DIFF_BYTES + usize::from(truncated)]);
        }
        _ => {
            let diff = diff.unwrap();
            assert!(!diff.truncated);
            assert_eq!(diff.bytes, DIFF.as_bytes());
        }
    }
    let pr = PullRequest {
        id: "pr".into(), task_id: "task".into(), host: "github.com".into(), repository: "me/app".into(),
        number: number(case), head: HEAD.into(), base: "main".into(), state: PrState::Open,
        merged_head: None, allowed_methods: vec![], review: None, aliases: Default::default(), address_read_at: 0, identity: Default::default(), tracking: Default::default(), missing: None, relocation_asked_at: None,
    };
    let got = gh.viewed_files(&pr).await;
    match case {
        Case::Failed | Case::InvalidJson => assert!(got.is_err()),
        Case::EmptyMarks => assert_eq!(got.unwrap(), Marks { files: vec![], ..marks() }),
        _ => assert_eq!(got.unwrap(), marks()),
    }
}
