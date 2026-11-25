//! What Alondra reads from GitHub with `gh` about a pull request, an issue or a branch.
use crate::cli::GhCli;
pub use alondra_engine_ports::github::Gh;
use alondra_engine_ports::github::{AuthoredPr, GhFailure, Read};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;
use std::time::Duration;

const FIELDS: &str = "title,body,baseRefName,headRefName,additions,deletions,changedFiles,statusCheckRollup,mergeable,mergeStateStatus,state,headRefOid,isDraft,fullDatabaseId,id,url";
const LIMIT: Duration = Duration::from_secs(30);

#[derive(Deserialize)]
struct Found {
    repository: Repository,
    number: u64,
    url: String,
}

#[derive(Deserialize)]
struct Repository {
    #[serde(rename = "nameWithOwner")]
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Branch {
    #[serde(rename = "headRefName")]
    name: String,
}

impl GhCli {
    async fn json(&self, args: &[&str]) -> Result<Value, String> {
        let out = self.run(args, None, LIMIT).await?;
        serde_json::from_slice(&out).map_err(|e| e.to_string())
    }
}

impl Gh for GhCli {
    fn head_oid<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, alondra_engine_ports::github::HeadError>> {
        Box::pin(crate::diff::head(self, url))
    }

    fn diff<'a>(&'a self, url: &'a str) -> Read<'a, Result<alondra_engine_ports::github::PrDiff, alondra_engine_ports::github::DiffError>> {
        Box::pin(crate::diff::read(self, url))
    }

    fn pr_graphql<'a>(&'a self, host: &'a str, owner: &'a str, name: &'a str, number: u64, query: &'a str) -> Read<'a, Result<Vec<u8>, alondra_engine_ports::github::GhFailure>> {
        Box::pin(async move {
            let (query, owner, name, number) = (format!("query={query}"), format!("owner={owner}"), format!("name={name}"), format!("number={number}"));
            self.run(&["api", "graphql", "--hostname", host, "-f", &query, "-f", &owner, "-f", &name, "-F", &number], None, LIMIT).await
        })
    }

    fn view<'a>(&'a self, url: &'a str) -> Read<'a, Option<Value>> {
        Box::pin(async move { self.json(&["pr", "view", url, "--json", FIELDS]).await.ok() })
    }

    /// One search across every owner: a call per repo would cost one request each.
    fn search_issues<'a>(&'a self, owners: &'a [String]) -> Read<'a, Result<Value, String>> {
        Box::pin(async move {
            let mut args = vec!["search", "issues", "--state", "open", "--limit", "100"];
            for owner in owners {
                args.extend(["--owner", owner.as_str()]);
            }
            args.extend(["--json", "repository,number,title,url,author,authorAssociation,labels,updatedAt,commentsCount"]);
            self.json(&args).await
        })
    }

    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> Read<'a, Result<Value, String>> {
        Box::pin(async move {
            let number = number.to_string();
            self.json(&["issue", "view", &number, "-R", repo, "--json", "url,title,body,author,labels,comments"]).await
        })
    }

    fn authored_open_prs(&self) -> Read<'_, Result<Vec<AuthoredPr>, GhFailure>> {
        Box::pin(async move {
            let args = ["search", "prs", "--author", "@me", "--state", "open", "--limit", "1000", "--json", "repository,number,url"];
            let found: Vec<Found> = serde_json::from_slice(&self.run(&args, None, LIMIT).await?).map_err(GhFailure::unreadable)?;
            Ok(found.into_iter().map(|found| AuthoredPr { repository: found.repository.name_with_owner, number: found.number, url: found.url }).collect())
        })
    }

    fn head_ref<'a>(&'a self, url: &'a str) -> Read<'a, Result<String, GhFailure>> {
        Box::pin(async move {
            let out = self.run(&["pr", "view", url, "--json", "headRefName"], None, LIMIT).await?;
            serde_json::from_slice::<Branch>(&out).map(|branch| branch.name).map_err(GhFailure::unreadable)
        })
    }

    fn branch_pr<'a>(&'a self, dir: &'a Path) -> Read<'a, Option<(String, String, u64)>> {
        Box::pin(self.branch_pr_within(dir, self.branch_pr_limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A `gh` that answers the search of authored PRs and the branch of PR 7, and fails otherwise.
    fn scripted_gh() -> (crate::test_support::Directory, GhCli) {
        use std::os::unix::fs::PermissionsExt;
        let dir = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*dir).unwrap();
        let program = dir.join("gh");
        std::fs::write(&program, "#!/bin/sh\ncase \"$*\" in\n\
            \"search prs --author @me --state open --limit 1000 --json repository,number,url\") \
            printf '%s' '[{\"repository\":{\"nameWithOwner\":\"Owner/Site\"},\"number\":7,\"url\":\"https://github.com/Owner/Site/pull/7\"}]';;\n\
            \"pr view https://github.com/Owner/Site/pull/7 --json headRefName\") printf '%s' '{\"headRefName\":\"feature\"}';;\n\
            *) echo 'no pull requests found' >&2; exit 1;;\nesac\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let gh = GhCli::at(program);
        (dir, gh)
    }

    #[tokio::test]
    async fn the_authored_open_prs_and_a_prs_branch_are_read_through_the_port() {
        let (_dir, gh) = scripted_gh();
        let github: &dyn Gh = &gh;
        assert_eq!(github.authored_open_prs().await.unwrap(), [AuthoredPr {
            repository: "Owner/Site".into(), number: 7, url: "https://github.com/Owner/Site/pull/7".into(),
        }]);
        assert_eq!(github.head_ref("https://github.com/Owner/Site/pull/7").await.unwrap(), "feature");
        let failure = github.head_ref("https://github.com/Owner/Site/pull/8").await.unwrap_err();
        assert!(failure.to_string().contains("no pull requests found"), "{failure}");
    }
}
