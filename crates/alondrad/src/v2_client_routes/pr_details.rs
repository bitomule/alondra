//! What GitHub says about each open pull request that the domain does not keep: its
//! title, checks and whether it merges cleanly. Refreshed in the background and read from
//! memory, so a client's poll never waits on `gh`.
use alondra_core::domain_v2::{DomainState, PrState};
use alondra_core::pr::Check;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Mutex;

pub(crate) type Viewed<'a> = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>;

/// `gh pr view --json`, behind a pointer so a test can answer for GitHub.
pub(crate) trait PrView: Send + Sync + 'static {
    fn view<'a>(&'a self, url: &'a str) -> Viewed<'a>;
}

pub(crate) struct GhView;

impl PrView for GhView {
    fn view<'a>(&'a self, url: &'a str) -> Viewed<'a> {
        Box::pin(crate::prs::Gh::view(&crate::prs::GhCli, url))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Details {
    pub title: Option<String>,
    pub checks: Vec<Check>,
    pub conflicts: bool,
    pub mergeability_unknown: bool,
}

#[derive(Default)]
pub(crate) struct PrDetails(Mutex<BTreeMap<String, Details>>);

impl PrDetails {
    pub(crate) fn get(&self, pr_id: &str) -> Option<Details> {
        self.0.lock().expect("pull request details poisoned").get(pr_id).cloned()
    }

    pub(crate) fn record(&self, pr_id: &str, details: Details) {
        self.0.lock().expect("pull request details poisoned").insert(pr_id.to_owned(), details);
    }

    fn keep_only(&self, state: &DomainState) {
        self.0.lock().expect("pull request details poisoned")
            .retain(|id, _| state.pull_requests.keys().any(|pr| &pr.0 == id));
    }

    pub(crate) async fn refresh(&self, state: &DomainState, source: &dyn PrView) {
        self.keep_only(state);
        for pr in state.pull_requests.values().filter(|pr| pr.state == PrState::Open) {
            let url = alondra_core::pr::canonical(&pr.repository, pr.number);
            let Some(view) = source.view(&url).await else { continue };
            self.record(&pr.id.0, parse(&pr.id.0, pr.number, &view));
        }
    }
}

fn parse(id: &str, number: u64, view: &Value) -> Details {
    let stub = alondra_core::pr::PullRequest::opened(0, &alondra_core::pr::canonical("o/r", number), id, None, 0)
        .expect("a canonical URL parses");
    let card = crate::prs::refreshed(&stub, view, None);
    Details {
        title: Some(card.title.clone()),
        conflicts: card.conflicts(),
        mergeability_unknown: card.mergeability_unknown(),
        checks: card.checks,
    }
}
