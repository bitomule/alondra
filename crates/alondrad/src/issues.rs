use alondra_core::issues::{self, Issue};
use alondra_core::now_millis;
use alondra_core::options::OptionsFile;
use serde::Serialize;

use crate::prs::Gh;

/// A list younger than this is served again without asking GitHub.
pub const FRESH_MS: i64 = 10 * 60 * 1000;

#[derive(Serialize, Clone, Debug, Default)]
pub struct IssueList {
    pub fetched_at: i64,
    pub owners: Vec<String>,
    pub issues: Vec<Issue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The configured owners, or else the owners of the GitHub checkouts under the projects root.
pub fn owners(file: &OptionsFile) -> Vec<String> {
    let mut out: Vec<String> = if file.issues.owners.is_empty() {
        let root = file.projects_root(&crate::options::user_home());
        alondra_core::options::projects(&root)
            .iter()
            .filter_map(|(_, path)| issues::origin_of(path))
            .filter_map(|repo| repo.split_once('/').map(|(o, _)| o.to_string()))
            .collect()
    } else {
        file.issues.owners.clone()
    };
    out.sort();
    out.dedup();
    out
}

pub fn excluded(file: &OptionsFile, repo: &str) -> bool {
    file.issues.exclude.iter().any(|x| x.eq_ignore_ascii_case(repo))
}

/// The open issues, from memory unless the list is stale or the user asked to refresh. Only
/// one search runs at a time: a second caller waits for it and gets the same list.
pub async fn list_in(cache: &tokio::sync::Mutex<Option<IssueList>>, file: &OptionsFile, refresh: bool, gh: &impl Gh) -> IssueList {
    let mut cache = cache.lock().await;
    let owners = owners(file);
    let fresh = cache.as_ref().is_some_and(|c| c.owners == owners && c.error.is_none() && now_millis() - c.fetched_at < FRESH_MS);
    if fresh && !refresh {
        return cache.clone().unwrap_or_default();
    }
    let fetched = if owners.is_empty() {
        Err("no owners to search: set [issues] owners in options.toml, or keep GitHub checkouts under the projects root".to_string())
    } else {
        gh.search_issues(&owners).await
    };
    let list = match fetched {
        Ok(v) => IssueList {
            fetched_at: now_millis(),
            issues: v.as_array().into_iter().flatten().filter_map(Issue::from_search).filter(|i| !excluded(file, &i.repo)).collect(),
            owners,
            error: None,
        },
        Err(e) => IssueList { error: Some(e), owners, ..cache.clone().unwrap_or_default() },
    };
    *cache = Some(list.clone());
    list
}

/// The text the orchestrator gets for an issue. A failed read still passes the link on.
pub async fn for_orchestrator_in(cache: &tokio::sync::Mutex<Option<IssueList>>, repo: &str, number: u64, gh: &impl Gh) -> String {
    let association = cache.lock().await.as_ref().and_then(|c| c.issues.iter().find(|i| i.repo == repo && i.number == number).and_then(|i| i.association.clone()));
    match gh.issue_view(repo, number).await {
        Ok(view) => issues::for_orchestrator(repo, number, &view, association.as_deref()),
        Err(e) => format!("Issue {repo}#{number}: https://github.com/{repo}/issues/{number}\nAlondra could not read it ({e}); open the link to see it."),
    }
}
