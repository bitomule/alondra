//! Marking a file of a pull request as viewed, or not, in GitHub, for the head the reader saw.
use super::{Client, Reply};
use crate::pr_viewed::Viewed;
use alondra_core::domain::{DomainError, PrId, PrState};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileViewed {
    path: String,
    viewed: bool,
    head: String,
}

const MAX_PATH: usize = 4096;

impl Client {
    pub(super) async fn file_viewed(&self, pr_id: &str, body: &Value) -> Reply {
        let Ok(request) = serde_json::from_value::<FileViewed>(body.clone()) else {
            return Reply::error(DomainError::new("invalid_input", "invalid request"));
        };
        if request.path.is_empty() || request.path.len() > MAX_PATH {
            return Reply::error(DomainError::new("invalid_input", "invalid file path"));
        }
        let Ok(state) = self.domain.state() else {
            return Reply::error(DomainError::new("storage_unavailable", "domain state is unavailable"));
        };
        let Some(pr) = state.pull_requests.get(&PrId(pr_id.to_owned())).cloned() else {
            return Reply::error(DomainError::new("not_found", "pull request does not exist"));
        };
        let url = crate::domain_github::pr_url(&pr.host, &pr.repository, pr.number);
        if pr.state != PrState::Open {
            return Reply::error(DomainError::new("pr_not_open", format!("{url} is no longer open")));
        }
        if request.head != pr.head {
            return Reply::error(DomainError::new("stale_head", "registered PR head changed"));
        }
        let github = self.github();
        let marks = match github.viewed_files(&pr).await {
            Ok(marks) => marks,
            Err(error) => {
                log!("could not read the viewed files of {url}: {error}");
                return Reply::error(DomainError::new("external_read_failed", "could not read the files of the pull request"));
            }
        };
        if marks.head != pr.head {
            return Reply::error(DomainError::new("stale_head", "PR head changed"));
        }
        if !marks.files.iter().any(|file| file.path == request.path) {
            return Reply::error(DomainError::new("invalid_input", "the file is not in the pull request"));
        }
        if let Err(error) = github.set_viewed(&pr.host, &marks.node, &request.path, request.viewed).await {
            log!("could not mark {} of {url}: {error}", request.path);
            return Reply::error(DomainError::new("external_write_failed", "GitHub did not save the mark"));
        }
        let viewed = if request.viewed { Viewed::Viewed } else { Viewed::Unviewed };
        Reply::ok(json!({"pr_id": pr.id, "head": pr.head, "path": request.path, "viewed": viewed}))
    }
}
