//! Which files of a pull request the user marked as viewed. GitHub keeps the marks, the same
//! Viewed boxes as github.com, and clears one by itself when a push changes its file: Alondra
//! only reads and sets them, for the account `gh` is signed in with.
use crate::domain_github::GhCli;
use alondra_core::domain::PullRequest;
use serde::Serialize;
use serde_json::Value;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Viewed {
    Viewed,
    Unviewed,
    /// Viewed once, then a push changed the file.
    Changed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Mark {
    pub path: String,
    pub viewed: Viewed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Marks {
    /// GitHub's node id of the pull request, which the mutations take.
    pub node: String,
    pub head: String,
    pub files: Vec<Mark>,
}

/// GitHub lists at most 3000 files of a pull request.
const MAX_FILES: usize = 3000;
const LIMIT: Duration = Duration::from_secs(20);

const READ: &str = "query($owner:String!,$name:String!,$number:Int!,$after:String){repository(owner:$owner,name:$name){\
pullRequest(number:$number){id headRefOid files(first:100,after:$after){pageInfo{hasNextPage endCursor} nodes{path viewerViewedState}}}}}";
const MARK: &str = "mutation($id:ID!,$path:String!){markFileAsViewed(input:{pullRequestId:$id,path:$path}){clientMutationId}}";
const UNMARK: &str = "mutation($id:ID!,$path:String!){unmarkFileAsViewed(input:{pullRequestId:$id,path:$path}){clientMutationId}}";

pub(crate) async fn read(gh: &GhCli, pr: &PullRequest) -> Result<Marks, String> {
    let (owner, name) = pr.repository.split_once('/').ok_or("the repository is not owner/name")?;
    let host = alondra_core::domain::normalize_pr_host(&pr.host);
    let query = format!("query={READ}");
    let (owner, name, number) = (format!("owner={owner}"), format!("name={name}"), format!("number={}", pr.number));
    let mut marks: Option<Marks> = None;
    let mut after: Option<String> = None;
    loop {
        let cursor = after.as_ref().map(|cursor| format!("after={cursor}"));
        let mut args = vec!["api", "graphql", "--hostname", &host, "-f", &query, "-f", &owner, "-f", &name, "-F", &number];
        if let Some(cursor) = &cursor {
            args.extend(["-f", cursor]);
        }
        let page = parse_page(&serde_json::from_slice(&gh.run(&args, None, LIMIT).await?).map_err(|error| error.to_string())?)?;
        let marks = marks.get_or_insert_with(|| Marks { node: page.node.clone(), head: page.head.clone(), files: Vec::new() });
        if page.head != marks.head {
            return Err("the pull request head changed while its files were read".into());
        }
        marks.files.extend(page.files);
        match page.next {
            Some(next) if marks.files.len() < MAX_FILES => after = Some(next),
            _ => break,
        }
    }
    marks.ok_or_else(|| "GitHub listed no page".into())
}

pub(crate) async fn set(gh: &GhCli, host: &str, node: &str, path: &str, viewed: bool) -> Result<(), String> {
    let host = alondra_core::domain::normalize_pr_host(host);
    let query = format!("query={}", if viewed { MARK } else { UNMARK });
    let (id, path) = (format!("id={node}"), format!("path={path}"));
    let answer = gh.run(&["api", "graphql", "--hostname", &host, "-f", &query, "-f", &id, "-f", &path], None, LIMIT).await?;
    let answer: Value = serde_json::from_slice(&answer).map_err(|error| error.to_string())?;
    match answer.get("errors") {
        Some(errors) => Err(errors.to_string()),
        None => Ok(()),
    }
}

struct Page {
    node: String,
    head: String,
    files: Vec<Mark>,
    next: Option<String>,
}

fn parse_page(answer: &Value) -> Result<Page, String> {
    if let Some(errors) = answer.get("errors") {
        return Err(errors.to_string());
    }
    let pr = &answer["data"]["repository"]["pullRequest"];
    let text = |value: &Value, what: &str| value.as_str().map(str::to_owned).ok_or_else(|| format!("GitHub sent no {what}"));
    let files = pr["files"]["nodes"].as_array().ok_or("GitHub sent no files")?.iter()
        .map(|file| Ok(Mark {
            path: text(&file["path"], "file path")?,
            viewed: match file["viewerViewedState"].as_str() {
                Some("VIEWED") => Viewed::Viewed,
                Some("DISMISSED") => Viewed::Changed,
                _ => Viewed::Unviewed,
            },
        }))
        .collect::<Result<Vec<_>, String>>()?;
    let info = &pr["files"]["pageInfo"];
    let next = if info["hasNextPage"].as_bool() == Some(true) { Some(text(&info["endCursor"], "page cursor")?) } else { None };
    Ok(Page { node: text(&pr["id"], "pull request id")?, head: text(&pr["headRefOid"], "head")?, files, next })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    fn pr() -> PullRequest {
        PullRequest {
            id: "github.com/me/app#7".into(), task_id: "task-1".into(), host: "github.com".into(), repository: "me/app".into(),
            number: 7, head: HEAD.into(), base: "main".into(), state: alondra_core::domain::PrState::Open,
            merged_head: None, allowed_methods: vec![], review: None,
        }
    }

    fn directory() -> crate::test_support::Directory {
        let directory = crate::test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        directory
    }

    /// A `gh` that answers the file reads with `pages`, one per call, and logs every call.
    fn gh(dir: &crate::test_support::Directory, pages: &[serde_json::Value]) -> (GhCli, std::path::PathBuf) {
        let log = dir.join("calls");
        let mut script = format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$*\" in\n", log.display());
        for (index, page) in pages.iter().enumerate().rev() {
            let cursor = if index == 0 { "*".to_owned() } else { format!("*after=c{index}*") };
            script.push_str(&format!("  *viewerViewedState{cursor}) printf '%s' '{page}';;\n"));
        }
        script.push_str("  *markFileAsViewed*|*unmarkFileAsViewed*) printf '%s' '{\"data\":{}}';;\nesac\n");
        let program = dir.join("gh");
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        (GhCli::at(program), log)
    }

    fn page(head: &str, files: &[(&str, &str)], next: Option<&str>) -> serde_json::Value {
        serde_json::json!({"data": {"repository": {"pullRequest": {"id": "PR_node", "headRefOid": head, "files": {
            "pageInfo": {"hasNextPage": next.is_some(), "endCursor": next},
            "nodes": files.iter().map(|(path, state)| serde_json::json!({"path": path, "viewerViewedState": state})).collect::<Vec<_>>(),
        }}}}})
    }

    #[tokio::test]
    async fn reads_every_page_of_marks_and_names_a_file_changed_since_it_was_viewed() {
        let dir = directory();
        let (gh, log) = gh(&dir, &[
            page(HEAD, &[("src/a.rs", "VIEWED"), ("src/b.rs", "UNVIEWED")], Some("c1")),
            page(HEAD, &[("src/c.rs", "DISMISSED")], None),
        ]);
        let marks = read(&gh, &pr()).await.unwrap();
        assert_eq!(marks.node, "PR_node");
        assert_eq!(marks.head, HEAD);
        assert_eq!(marks.files, [
            Mark { path: "src/a.rs".into(), viewed: Viewed::Viewed },
            Mark { path: "src/b.rs".into(), viewed: Viewed::Unviewed },
            Mark { path: "src/c.rs".into(), viewed: Viewed::Changed },
        ]);
        let calls = std::fs::read_to_string(log).unwrap();
        assert_eq!(calls.lines().count(), 2);
        assert!(calls.lines().all(|call| call.contains("--hostname github.com") && call.contains("owner=me") && call.contains("name=app") && call.contains("number=7")), "{calls}");
        assert!(calls.lines().nth(1).unwrap().contains("after=c1"), "{calls}");
    }

    #[tokio::test]
    async fn a_head_that_moves_between_pages_gives_no_marks() {
        let dir = directory();
        let other = "fedcba9876543210fedcba9876543210fedcba98";
        let (gh, _) = gh(&dir, &[page(HEAD, &[("a", "VIEWED")], Some("c1")), page(other, &[("b", "VIEWED")], None)]);
        assert!(read(&gh, &pr()).await.is_err());
    }

    #[tokio::test]
    async fn graphql_errors_are_failures_not_empty_marks() {
        let dir = directory();
        let (gh, _) = gh(&dir, &[serde_json::json!({"errors": [{"message": "Could not resolve to a PullRequest"}]})]);
        assert!(read(&gh, &pr()).await.is_err());
    }

    #[tokio::test]
    async fn marking_and_unmarking_send_the_matching_mutation_for_the_node_and_path() {
        let dir = directory();
        let (gh, log) = gh(&dir, &[]);
        set(&gh, "github.com", "PR_node", "src/a.rs", true).await.unwrap();
        set(&gh, "github.com", "PR_node", "src/a.rs", false).await.unwrap();
        let calls = std::fs::read_to_string(log).unwrap();
        let calls: Vec<_> = calls.lines().collect();
        assert!(calls[0].contains("markFileAsViewed") && !calls[0].contains("unmarkFileAsViewed"), "{calls:?}");
        assert!(calls[1].contains("unmarkFileAsViewed"), "{calls:?}");
        assert!(calls.iter().all(|call| call.contains("id=PR_node") && call.contains("path=src/a.rs")), "{calls:?}");
    }
}
