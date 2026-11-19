//! PR reads in memory. Every request is scoped to a URL, and head changes are explicit.
use alondra_domain_rules::PullRequest;
use alondra_engine_ports::github::{DiffError, Gh, HeadError, Marks, PrDiff, Read, MAX_PR_DIFF_BYTES};
use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Call {
    Head(String),
    Diff(String),
    Marks(String),
}

pub struct PrReads {
    pub heads: VecDeque<Result<String, HeadError>>,
    pub diff: Result<Vec<u8>, DiffError>,
    pub marks: Result<Marks, String>,
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

    fn viewed_files<'a>(&'a self, pr: &'a PullRequest) -> Read<'a, Result<Marks, String>> {
        Box::pin(async move {
            let url = format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number);
            self.calls.lock().unwrap().push(Call::Marks(url.clone()));
            self.prs.lock().unwrap().get(&url).expect("marks requested for an unconfigured PR").marks.clone()
        })
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
            let marks = match case {
                Case::Failed | Case::InvalidJson => Err("unavailable".into()),
                Case::EmptyMarks => Ok(alondra_engine_ports::github::Marks { files: vec![], ..github::marks() }),
                _ => Ok(github::marks()),
            };
            gh.insert(github::url(case), PrReads { heads: [head].into(), diff, marks });
            github::check(&gh, case).await;
        }
    }
}
