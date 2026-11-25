//! PR reads in memory, every request scoped to a URL and head changes explicit, and a GitHub no
//! test reaches.
use alondra_domain_rules::{MergeMethod, MergeVerdict, ProviderLocator, ProviderPrKey, PullRequest};
use alondra_engine_ports::Registry;
use alondra_engine_ports::github::{
    Closed, DiffError, Failure, Fetch, Gh, GhFailure, GitHub, GitHubImages, HeadError, Located, Observed, Pending, PrDiff, PrMerger,
    PrObserver, Read, Status, MAX_PR_DIFF_BYTES,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Head(String),
    Diff(String),
}

pub struct PrReads {
    pub heads: VecDeque<Result<String, HeadError>>,
    pub diff: Result<Vec<u8>, DiffError>,
}

#[derive(Default)]
pub struct PullRequests {
    prs: Mutex<BTreeMap<String, PrReads>>,
    calls: Mutex<Vec<Call>>,
    heads_held: Mutex<BTreeMap<String, tokio::sync::oneshot::Receiver<()>>>,
}

impl PullRequests {
    pub fn insert(&self, url: impl Into<String>, reads: PrReads) {
        assert!(!reads.heads.is_empty(), "a PR must have a head response");
        self.prs.lock().unwrap().insert(url.into(), reads);
    }

    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn hold_next_head(&self, url: impl Into<String>) -> tokio::sync::oneshot::Sender<()> {
        let (release, held) = tokio::sync::oneshot::channel();
        assert!(self.heads_held.lock().unwrap().insert(url.into(), held).is_none());
        release
    }
}

impl Gh for PullRequests {
    fn view<'a>(&'a self, _: &'a str) -> Read<'a, Option<serde_json::Value>> {
        panic!("unexpected full PR view")
    }

    fn head_oid<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, HeadError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(Call::Head(url.into()));
            let held = self.heads_held.lock().unwrap().remove(url);
            if let Some(held) = held { held.await.expect("release the held head"); }
            let mut prs = self.prs.lock().unwrap();
            let pr = prs.get_mut(url).expect("head requested for an unconfigured PR");
            if pr.heads.len() > 1 { pr.heads.pop_front().unwrap() } else { pr.heads[0].clone() }
        })
    }

    fn diff<'a>(&'a self, url: &'a str) -> Read<'a, Result<PrDiff, DiffError>> {
        Box::pin(async move {
            self.calls.lock().unwrap().push(Call::Diff(url.into()));
            let prs = self.prs.lock().unwrap();
            let bytes = prs.get(url).expect("diff requested for an unconfigured PR").diff.clone()?;
            let truncated = bytes.len() > MAX_PR_DIFF_BYTES;
            Ok(PrDiff { bytes: bytes.into_iter().take(MAX_PR_DIFF_BYTES + 1).collect(), truncated })
        })
    }
}

/// A GitHub no test reaches: every read and change fails as `gh` fails on a Mac without it, and
/// nothing leaves the process. The server's tests register it in place of the real one.
pub struct Offline;

/// Gives the registry the GitHub no test reaches.
pub fn wire(registry: &Registry) {
    registry.provide(Arc::new(offline()));
}

pub fn offline() -> GitHub {
    GitHub::of(Arc::new(Offline))
}

fn absent() -> GhFailure {
    GhFailure::Launch(std::io::ErrorKind::NotFound, "gh is not installed here".into())
}

fn pending<'a, T: Send + 'a>() -> Pending<'a, T> {
    Box::pin(async { Err(absent()) })
}

impl PrObserver for Offline {
    fn observe<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> Observed<'a> {
        Box::pin(async { Err(absent()) })
    }
    fn close<'a>(&'a self, _: &'a str, _: &'a str, _: u64, _: &'a ProviderPrKey, _: &'a str) -> Closed<'a> {
        Box::pin(async { Err(absent()) })
    }
    fn relocate<'a>(&'a self, _: &'a str, _: &'a ProviderLocator) -> Located<'a> {
        Box::pin(async { Err(absent()) })
    }
}

impl PrMerger for Offline {
    fn status<'a>(&'a self, _: &'a PullRequest) -> Pending<'a, Status> { pending() }
    fn default_branch<'a>(&'a self, _: &'a PullRequest) -> Pending<'a, String> { pending() }
    fn retarget<'a>(&'a self, _: &'a PullRequest, _: &'a ProviderPrKey, _: &'a str) -> Pending<'a, ()> { pending() }
    fn merge<'a>(&'a self, _: &'a PullRequest, _: &'a ProviderPrKey, _: &'a str, _: MergeMethod) -> Pending<'a, ()> { pending() }
    fn merge_bypassing<'a>(&'a self, _: &'a PullRequest, _: &'a ProviderPrKey, _: &'a str, _: MergeMethod) -> Pending<'a, ()> { pending() }
    fn in_queue<'a>(&'a self, _: &'a PullRequest) -> Pending<'a, bool> { pending() }
    fn is_draft<'a>(&'a self, _: &'a PullRequest) -> Pending<'a, bool> { pending() }
    fn ready<'a>(&'a self, _: &'a PullRequest, _: &'a ProviderPrKey) -> Pending<'a, ()> { pending() }
    fn required_checks<'a>(&'a self, _: &'a PullRequest, _: &'a str) -> Pending<'a, Vec<String>> { pending() }
    fn bypassable_checks<'a>(&'a self, _: &'a PullRequest, _: &'a str) -> Pending<'a, Vec<String>> { pending() }
    fn can_merge<'a>(&'a self, _: &'a PullRequest) -> Pending<'a, bool> { pending() }
    fn verdict<'a>(&'a self, _: &'a PullRequest) -> Pending<'a, MergeVerdict> { pending() }
}

impl GitHubImages for Offline {
    fn size<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str, _: &'a str) -> Fetch<'a, u64> {
        Box::pin(async { Err(Failure::Transient) })
    }
    fn file<'a>(&'a self, _: &'a str, _: &'a str, _: &'a str, _: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async { Err(Failure::Transient) })
    }
    fn repository<'a>(&'a self, _: &'a str, _: &'a str) -> Fetch<'a, ()> {
        Box::pin(async { Err(Failure::Transient) })
    }
    fn body_html<'a>(&'a self, _: &'a str, _: &'a str, _: u64) -> Fetch<'a, String> {
        Box::pin(async { Err(Failure::Transient) })
    }
    fn download<'a>(&'a self, _: &'a str) -> Fetch<'a, Vec<u8>> {
        Box::pin(async { Err(Failure::Transient) })
    }
}

impl Gh for Offline {
    fn view<'a>(&'a self, _: &'a str) -> Read<'a, Option<serde_json::Value>> {
        Box::pin(async { None })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::github::{self, Case};

    #[tokio::test]
    async fn the_memory_diff_reads_keep_the_github_contract() {
        for case in github::CASES {
            let gh = PullRequests::default();
            let head = match case {
                Case::InvalidJson => Err(HeadError::InvalidResponse),
                Case::InvalidHead => Err(HeadError::InvalidHead),
                Case::Failed => Err(HeadError::Unavailable(alondra_engine_ports::github::GhFailure::Exit(Some(1), "unavailable".into()))),
                _ => Ok(github::HEAD.into()),
            };
            let diff = match case {
                Case::Failed => Err(DiffError::Finish),
                Case::ExactLimit => Ok(vec![b'a'; MAX_PR_DIFF_BYTES]),
                Case::Truncated => Ok(vec![b'a'; MAX_PR_DIFF_BYTES + 64]),
                _ => Ok(github::DIFF.as_bytes().to_vec()),
            };
            gh.insert(github::url(case), PrReads { heads: [head].into(), diff });
            github::check(&gh, case).await;
        }
    }
}
