//! A PR is the same PR for everyone only by its host, repository and number: two repositories
//! reuse the same numbers, so an id a node makes up (`pr-163`) names another task's PR as soon as
//! a second repository reaches #163. Alondra gives the id when the PR is registered, and a node
//! that later names "its PR" always gets the one its task owns, whatever id it writes.

use alondra_core::domain::{hosted_pr_id, normalize_pr_host, normalize_pr_repository, DomainError, DomainState, InteractionId, PrId, Scope, TaskId};
use serde_json::Value;

pub fn assign(state: &DomainState, method: &str, path: &str, id: Option<&str>, body: &mut Value) -> Result<(), DomainError> {
    let Some(id) = id else { return Ok(()) };
    match method {
        "POST" if path.ends_with("/pull-requests") && path.starts_with("/tasks/") => register(body),
        "POST" if path.ends_with("/results") && path.starts_with("/tasks/") => {
            let Some(own) = owned(state, &TaskId(id.into())) else {
                if array(body, "targets").any(|t| t["kind"] == "pull_request") {
                    let mut error = DomainError::new("pr_mismatch", "targets: your task has no PR yet: call register_pr first, then present it");
                    error.subject = Some("targets".into());
                    return Err(error);
                }
                return Ok(());
            };
            for target in array(body, "targets").filter(|t| t["kind"] == "pull_request") {
                target["pr_id"] = Value::String(own.0.clone());
            }
            for mark in array(body, "checklist") {
                for evidence in mark.get_mut("evidence").and_then(Value::as_array_mut).into_iter().flatten() {
                    if evidence["kind"] == "pr" {
                        evidence["value"] = Value::String(own.0.clone());
                    }
                }
            }
        }
        "POST" if path.ends_with("/stack-readiness") && path.starts_with("/tasks/") => {
            if let Some(own) = owned(state, &TaskId(id.into())) {
                body["pr_id"] = Value::String(own.0);
            }
        }
        "PUT" if path.ends_with("/subject") && path.starts_with("/interactions/") => {
            let Some(Scope::Task { task_id }) = state.interactions.get(&InteractionId(id.into())).map(|i| &i.scope) else { return Ok(()) };
            let Some(own) = owned(state, task_id) else { return Ok(()) };
            if let Some(subject) = body.get_mut("subject").filter(|s| s["kind"] == "pull_request") {
                subject["pr_id"] = Value::String(own.0);
            }
        }
        _ => {}
    }
    Ok(())
}

fn register(body: &mut Value) {
    let (Some(host), Some(repository), Some(number)) = (
        body.get("host").and_then(Value::as_str),
        body.get("repository").and_then(Value::as_str),
        body.get("number").and_then(Value::as_u64),
    ) else {
        return;
    };
    let host = normalize_pr_host(host);
    let repository = normalize_pr_repository(repository);
    body["pr_id"] = Value::String(hosted_pr_id(&host, &repository, number).0);
    body["host"] = Value::String(host);
    body["repository"] = Value::String(repository);
}

fn owned(state: &DomainState, task: &TaskId) -> Option<PrId> {
    state.tasks.get(task)?.pr_id.clone()
}

fn array<'a>(body: &'a mut Value, key: &str) -> impl Iterator<Item = &'a mut Value> {
    body.get_mut(key).and_then(Value::as_array_mut).into_iter().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_registered_pr_is_named_by_its_host_repository_and_number_whatever_id_the_node_sent() {
        let mut body = json!({"pr_id":"pr-163","host":"https://GitHub.com/","repository":"bitomule/lumo","number":163});
        assign(&DomainState::new("epoch"), "POST", "/tasks/lumo/pull-requests", Some("lumo"), &mut body).unwrap();
        assert_eq!(body["pr_id"], "github.com/bitomule/lumo#163");
        assert_eq!(body["host"], "github.com");
    }

    #[test]
    fn a_repository_with_capitals_names_the_same_pr_as_its_lowercase_spelling() {
        for repository in ["acmecorp-private/Parcel", "acmecorp-private/parcel", " Acmecorp-Private/PARCEL "] {
            let mut body = json!({"host":"github.com","repository":repository,"number":65});
            assign(&DomainState::new("epoch"), "POST", "/tasks/parcel/pull-requests", Some("parcel"), &mut body).unwrap();
            assert_eq!(body["pr_id"], "github.com/acmecorp-private/parcel#65", "{repository}");
            assert_eq!(body["repository"], "acmecorp-private/parcel", "{repository}");
        }
    }

    #[test]
    fn presenting_a_pr_before_registering_one_says_to_register_it() {
        let mut body = json!({"targets":[{"kind":"pull_request"}]});
        let error = assign(&DomainState::new("epoch"), "POST", "/tasks/lumo/results", Some("lumo"), &mut body).unwrap_err();
        assert_eq!(error.code, "pr_mismatch");
        assert!(error.message.contains("call register_pr first"), "{}", error.message);
        let mut text = json!({"targets":[{"kind":"text","text":"Done"}]});
        assign(&DomainState::new("epoch"), "POST", "/tasks/lumo/results", Some("lumo"), &mut text).unwrap();
    }
}
