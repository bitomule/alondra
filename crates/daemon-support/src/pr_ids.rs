//! A PR is the same PR for everyone only by its host, repository and number: two repositories
//! reuse the same numbers, so an id a node makes up (`pr-163`) names another task's PR as soon as
//! a second repository reaches #163. Alondra gives the id when the PR is registered, and a node
//! that later names "its PR" gets a registered PR of its own task. Explicit registered targets
//! keep their identity so the domain can validate their ownership and state.

use alondra_core::domain::{hosted_pr_id, normalize_pr_host, normalize_pr_repository, DomainError, DomainState, InteractionId, PrId, PullRequest, Scope, TaskId};
use serde_json::Value;

pub fn assign(state: &DomainState, method: &str, path: &str, id: Option<&str>, body: &mut Value) -> Result<(), DomainError> {
    let Some(id) = id else { return Ok(()) };
    match method {
        "POST" if path.ends_with("/pull-requests") && path.starts_with("/tasks/") => register(body),
        "POST" if path.ends_with("/results") && path.starts_with("/tasks/") => {
            let task = TaskId(id.into());
            let Some(own) = presented(state, &task) else {
                if array(body, "targets").any(|t| t["kind"] == "pull_request") {
                    let reading = state.tasks.get(&task).is_some_and(|task| !task.opened_prs.is_empty());
                    let mut error = DomainError::new("pr_mismatch", if reading {
                        "targets: Alondra is still reading the PR you opened from GitHub: send the same result again in a moment"
                    } else {
                        "targets: your task has no PR yet: call register_pr first, then present it"
                    });
                    error.subject = Some("targets".into());
                    return Err(error);
                }
                return Ok(());
            };
            name_targets(body, &open_prs(state, &task), &own, state);
            for mark in array(body, "checklist") {
                for evidence in mark.get_mut("evidence").and_then(Value::as_array_mut).into_iter().flatten() {
                    let named = evidence["value"].as_str().is_some_and(|value| state.pull_requests.get(&PrId(value.to_owned())).is_some_and(|pr| pr.task_id == task));
                    if evidence["kind"] == "pr" && !named {
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
    state.open_prs(state.tasks.get(task)?).next().map(|pr| pr.id.clone())
}

/// The PR a delivery of the task means when it says "my PR": an open one, else its first.
fn presented(state: &DomainState, task: &TaskId) -> Option<PrId> {
    state.main_pr(state.tasks.get(task)?).map(|pr| pr.id.clone())
}

fn open_prs<'a>(state: &'a DomainState, task: &TaskId) -> Vec<&'a PullRequest> {
    state.tasks.get(task).map_or_else(Vec::new, |task| state.open_prs(task).collect())
}

/// Which PR of the task each PR target means: the open one at the head it gives, else the one a
/// delivery means by "my PR". With several open a head none is at is left out: the domain presents
/// the commit each is at, and it adds the open PRs no target names.
fn name_targets(body: &mut Value, open: &[&PullRequest], own: &PrId, state: &DomainState) {
    for target in array(body, "targets").filter(|t| t["kind"] == "pull_request") {
        // A named registered target of this task is intentional, even when closed: the domain must reject it,
        // rather than silently deliver a different PR.
        if target.get("pr_id").and_then(Value::as_str).and_then(|id| state.pull_requests.get(&PrId(id.into())))
            .is_some_and(|pr| pr.task_id == state.pull_requests[own].task_id) { continue; }
        let head = target.get("head").and_then(Value::as_str).map(str::to_owned);
        let at = open.iter().find(|pr| head.as_deref() == Some(pr.head.as_str()));
        target["pr_id"] = Value::String(at.map_or(&own.0, |pr| &pr.id.0).clone());
        if at.is_none() && open.len() > 1 {
            target.as_object_mut().map(|target| target.remove("head"));
        }
    }
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
        let mut body = json!({"pr_id":"pr-163","host":"https://GitHub.com/","repository":"acme/lumo","number":163});
        assign(&DomainState::new("epoch"), "POST", "/tasks/lumo/pull-requests", Some("lumo"), &mut body).unwrap();
        assert_eq!(body["pr_id"], "github.com/acme/lumo#163");
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

    fn task_with_two_open_prs() -> DomainState {
        let pr = |repository: &str, number: u64, head: &str| alondra_core::domain::PullRequest {
            id: hosted_pr_id("github.com", repository, number), task_id: "t".into(), host: "github.com".into(), repository: repository.into(), number,
            head: head.into(), base: "main".into(), state: alondra_core::domain::PrState::Open, merged_head: None, allowed_methods: vec![], review: None,
        };
        let mut state: DomainState = serde_json::from_value(serde_json::to_value(DomainState::new("epoch")).unwrap()).unwrap();
        let (first, second) = (pr("me/app", 1, "aaa"), pr("me/site", 2, "bbb"));
        let mut task: alondra_core::domain::Task = serde_json::from_value(json!({
            "id":"t","request_id":"r","agent_id":null,"title":"T","state":"running","revision":1,"started":true,"review_prs":false,
            "final_presentation":null,"pr_id":first.id.0,"later_prs":[second.id.0],"dependencies":[],"documents":{},"archived":false,
        })).unwrap();
        task.opened_prs.clear();
        state.tasks.insert("t".into(), task);
        state.pull_requests.insert(first.id.clone(), first);
        state.pull_requests.insert(second.id.clone(), second);
        state
    }

    #[test]
    fn a_pr_target_means_the_open_pr_at_its_head_and_a_head_none_is_at_is_left_out() {
        let state = task_with_two_open_prs();
        let mut body = json!({"targets":[{"kind":"pull_request","head":"bbb"}]});
        assign(&state, "POST", "/tasks/t/results", Some("t"), &mut body).unwrap();
        assert_eq!(body["targets"][0], json!({"kind":"pull_request","pr_id":"github.com/me/site#2","head":"bbb"}));

        let mut body = json!({"targets":[{"kind":"pull_request","head":"ccc"}]});
        assign(&state, "POST", "/tasks/t/results", Some("t"), &mut body).unwrap();
        assert_eq!(body["targets"][0], json!({"kind":"pull_request","pr_id":"github.com/me/app#1"}));
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

    #[test]
    fn a_closed_first_pr_never_owns_a_new_subject_or_changes_an_explicit_target() {
        let mut state = task_with_two_open_prs();
        state.pull_requests.get_mut(&"github.com/me/app#1".into()).unwrap().state = alondra_core::domain::PrState::Closed;
        let mut subject = json!({});
        assign(&state, "POST", "/tasks/t/stack-readiness", Some("t"), &mut subject).unwrap();
        assert_eq!(subject["pr_id"], "github.com/me/site#2");
        let mut result = json!({"targets":[{"kind":"pull_request"}]});
        assign(&state, "POST", "/tasks/t/results", Some("t"), &mut result).unwrap();
        assert_eq!(result["targets"][0]["pr_id"], "github.com/me/site#2");
        let mut closed = json!({"targets":[{"kind":"pull_request","pr_id":"github.com/me/app#1"}]});
        assign(&state, "POST", "/tasks/t/results", Some("t"), &mut closed).unwrap();
        assert_eq!(closed["targets"][0]["pr_id"], "github.com/me/app#1", "the domain must see and reject the closed target");
    }
}
