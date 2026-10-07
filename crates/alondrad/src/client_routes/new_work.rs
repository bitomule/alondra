//! Asking for new work: the options a request can carry, the projects it can be for, the open
//! issues it can come from, and the request itself, which is created together with what the
//! user chose for its launch.
use super::close::Answer;
use super::{Client, Reply};
use crate::domain_store::{RequestLaunch, TransportIdentity};
use alondra_core::domain::{Actor, AttachmentRef, Command, CommandEnvelope, DomainError, PROTOCOL_VERSION};
use alondra_core::options::{Choice, OptionsFile};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What the request path asks of GitHub, so a test can answer for it.
pub(crate) trait Github: Send + Sync + 'static {
    fn search_issues<'a>(&'a self, owners: &'a [String]) -> Answer<'a, Result<Value, String>>;
    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> Answer<'a, Result<Value, String>>;
    /// Closes the PR on GitHub without merging it.
    fn close_pr<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Answer<'a, Result<(), String>>;
}

pub(crate) struct Gh;

impl Github for Gh {
    fn search_issues<'a>(&'a self, owners: &'a [String]) -> Answer<'a, Result<Value, String>> {
        Box::pin(crate::prs::Gh::search_issues(&crate::prs::GhCli, owners))
    }

    fn issue_view<'a>(&'a self, repo: &'a str, number: u64) -> Answer<'a, Result<Value, String>> {
        Box::pin(crate::prs::Gh::issue_view(&crate::prs::GhCli, repo, number))
    }

    fn close_pr<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Answer<'a, Result<(), String>> {
        Box::pin(async move {
            let url = crate::domain_github::pr_url(host, repository, number);
            crate::domain_github::GhCli::new()
                .run(&["pr", "close", &url], None, std::time::Duration::from_secs(60))
                .await
                .map(|_| ())
        })
    }
}

/// The two reads of GitHub the shared issue code makes, over whatever answers for GitHub here.
struct Issues<'a>(&'a dyn Github);

impl crate::prs::Gh for Issues<'_> {
    async fn edit_base(&self, _: &str, _: &str) -> Result<(), String> {
        Err("not available here".into())
    }

    async fn view(&self, _: &str) -> Option<Value> {
        None
    }

    async fn exists(&self, _: &str) -> bool {
        false
    }

    async fn search_issues(&self, owners: &[String]) -> Result<Value, String> {
        self.0.search_issues(owners).await
    }

    async fn issue_view(&self, repo: &str, number: u64) -> Result<Value, String> {
        self.0.issue_view(repo, number).await
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    request_id: String,
    text: String,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    options: BTreeMap<String, Choice>,
    #[serde(default)]
    issue: Option<String>,
    #[serde(default)]
    attachments: Vec<AttachmentRef>,
}

fn refused(message: impl Into<String>) -> Reply {
    Reply { status: StatusCode::BAD_REQUEST, body: json!({"error": message.into()}) }
}

/// The issue a request comes from: the one picked, or else an issue link pasted in the text.
/// A picked issue must be allowed; a pasted link from an excluded repo is text.
fn issue_of(body: &Body, file: &OptionsFile) -> Result<Option<(String, u64)>, String> {
    if let Some(picked) = body.issue.as_deref().filter(|i| !i.trim().is_empty()) {
        let (repo, n) = alondra_core::issues::parse_ref(picked).ok_or_else(|| format!("{picked} is not an issue: use owner/repo#number or its link"))?;
        if crate::issues::excluded(file, &repo) {
            return Err(format!("issues from {repo} are excluded in options.toml"));
        }
        return Ok(Some((repo, n)));
    }
    Ok(alondra_core::issues::find_in_text(&body.text).filter(|(repo, _)| !crate::issues::excluded(file, repo)))
}

impl Client {
    pub(crate) fn github(&self) -> std::sync::Arc<dyn Github> {
        self.github.lock().expect("github poisoned").clone()
    }

    #[cfg(test)]
    pub(crate) fn use_github(&self, github: std::sync::Arc<dyn Github>) {
        *self.github.lock().expect("github poisoned") = github;
    }

    pub(super) fn options_answer(&self, project: Option<String>) -> Response {
        let loaded = crate::options::load_in(&self.home, &self.options);
        let project = project.filter(|p| !p.trim().is_empty());
        let catalog = loaded.file.catalog(project.as_deref());
        Json(json!({"project": project, "options": catalog, "error": loaded.error})).into_response()
    }

    pub(super) fn projects_answer(&self) -> Response {
        let loaded = crate::options::load_in(&self.home, &self.options);
        let root = loaded.file.projects_root(&crate::options::user_home());
        let mut used: BTreeMap<String, Vec<i64>> = BTreeMap::new();
        for node in self.domain.live.nodes().iter().filter(|node| !node.is_orchestrator) {
            used.entry(node.project()).or_default().push(node.created_at);
        }
        let list = alondra_core::options::ranked(alondra_core::options::projects(&root), &used);
        Json(json!({"root": root, "projects": list, "error": loaded.error})).into_response()
    }

    pub(super) async fn issues_answer(&self, refresh: bool) -> Response {
        let file = crate::options::load_in(&self.home, &self.options).file;
        let github = self.github.lock().expect("github poisoned").clone();
        Json(json!(crate::issues::list_in(&self.issue_cache, &file, refresh, &Issues(&*github)).await)).into_response()
    }

    /// A user's message to the orchestrator is a new Request with the default options.
    pub(super) async fn orchestrator_message(
        &self,
        user_id: &str,
        body: &Value,
        transport: Option<TransportIdentity<'_>>,
        authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
    ) -> anyhow::Result<Reply> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Message {
            text: String,
            #[serde(default)]
            attachments: Vec<AttachmentRef>,
        }
        let Ok(message) = serde_json::from_value::<Message>(body.clone()) else {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid message")));
        };
        let request = json!({"request_id": format!("request-{}", crate::auth::random_hex(16)),
            "text": message.text, "options": {}, "attachments": message.attachments});
        self.new_request(user_id, &request, transport, authorize).await
    }

    /// The Request and what the user chose for its launch, in one commit. The options are
    /// resolved now, so a later edit of `options.toml` cannot change what was asked for.
    pub(super) async fn new_request(
        &self,
        user_id: &str,
        body: &Value,
        transport: Option<TransportIdentity<'_>>,
        authorize: &(dyn Fn() -> anyhow::Result<()> + Sync),
    ) -> anyhow::Result<Reply> {
        let Ok(body) = serde_json::from_value::<Body>(body.clone()) else {
            return Ok(Reply::error(DomainError::new("invalid_input", "invalid request body")));
        };
        if let Err(error) = AttachmentRef::validate_list(&body.attachments) {
            return Ok(Reply::error(error));
        }
        let loaded = crate::options::load_in(&self.home, &self.options);
        let issue = match issue_of(&body, &loaded.file) {
            Ok(issue) => issue,
            Err(error) => return Ok(refused(error)),
        };
        let from_issue = issue.as_ref().and_then(|(repo, _)| {
            let name = repo.split_once('/')?.1;
            let root = loaded.file.projects_root(&crate::options::user_home());
            alondra_core::options::projects(&root).into_iter().find(|(project, _)| project.eq_ignore_ascii_case(name)).map(|(project, _)| project)
        });
        let project = body.project.as_deref().filter(|p| !p.trim().is_empty()).or(from_issue.as_deref());
        let mut resolved = match loaded.file.resolve(project, &body.options) {
            Ok(resolved) => resolved,
            Err(error) => return Ok(refused(error)),
        };
        let github = self.github.lock().expect("github poisoned").clone();
        let issue_text = match &issue {
            Some((repo, number)) => {
                resolved.issue = Some(format!("{repo}#{number}"));
                Some(crate::issues::for_orchestrator_in(&self.issue_cache, repo, *number, &Issues(&*github)).await)
            }
            None => None,
        };
        let auto_merge = resolved.auto_merge;
        let launch = RequestLaunch {
            project: project.map(str::to_owned),
            issue: resolved.issue.clone(),
            options: resolved,
            issue_text,
            attachments: body.attachments,
        };
        let state = self.domain.state()?;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("request-{}", body.request_id).into(),
            expected_revision: 0,
            command: Command::CreateRequest { request_id: body.request_id.into(), body: body.text, auto_merge },
        };
        let outcome = self.domain.execute_with(&Actor::User { user_id: user_id.to_owned() }, &envelope, transport, Some(&launch), || {
            authorize().map_err(|_| DomainError::new("forbidden", "device approval was revoked").into())
        });
        match outcome {
            Ok(outcome) => Ok(Reply::ok(json!(outcome))),
            Err(crate::domain_store::StoreError::Domain(error)) => Ok(Reply::error(error)),
            Err(crate::domain_store::StoreError::Storage(error)) => Err(error),
        }
    }
}
