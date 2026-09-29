//! GitHub facts for domain v2 pull requests. `gh` is the only reader; the
//! daemon never merges a v2 PR itself. An observation records remote reality,
//! it never accepts a delivery or completes a Task.
use crate::domain_runtime::DomainRuntime;
use crate::domain_store::StoreError;
use alondra_core::domain_v2::{
    Actor, AuthorizationId, Command, CommandEnvelope, DomainState, MergeAttemptState,
    PrState, PullRequest, PROTOCOL_VERSION,
};
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

/// What GitHub reports for one PR. `merged_head` is the PR head at merge time,
/// never the merge commit: squash and rebase produce a different commit that
/// can never match the presented and authorized head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PrFacts {
    pub head: String,
    pub base: String,
    pub state: PrState,
    pub merged_head: Option<String>,
}

type Observed<'a> = Pin<Box<dyn Future<Output = Result<PrFacts, String>> + Send + 'a>>;

pub(crate) trait PrObserver: Send + Sync + 'static {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a>;
}

impl<T: PrObserver> PrObserver for Arc<T> {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        (**self).observe(host, repository, number)
    }
}

pub(crate) struct GhCli {
    program: PathBuf,
}

impl GhCli {
    pub(crate) fn new() -> Self {
        Self { program: "gh".into() }
    }
    #[cfg(test)]
    fn at(program: PathBuf) -> Self {
        Self { program }
    }
}

impl PrObserver for GhCli {
    fn observe<'a>(&'a self, host: &'a str, repository: &'a str, number: u64) -> Observed<'a> {
        Box::pin(async move {
            let url = format!("https://{host}/{repository}/pull/{number}");
            let run = tokio::process::Command::new(&self.program)
                .args(["pr", "view", &url, "--json", "state,headRefOid,baseRefName"])
                .kill_on_drop(true)
                .output();
            let output = match tokio::time::timeout(Duration::from_secs(30), run).await {
                Ok(Ok(output)) if output.status.success() => output,
                Ok(Ok(output)) => return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned()),
                Ok(Err(error)) => return Err(error.to_string()),
                Err(_) => return Err("gh pr view did not finish within 30 s".into()),
            };
            let value: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
            parse(&value)
        })
    }
}

fn parse(value: &Value) -> Result<PrFacts, String> {
    let head = value["headRefOid"].as_str()
        .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or("gh reported no PR head")?;
    let base = value["baseRefName"].as_str().filter(|base| !base.is_empty())
        .ok_or("gh reported no PR base")?;
    let state = match value["state"].as_str() {
        Some("OPEN") => PrState::Open,
        Some("CLOSED") => PrState::Closed,
        Some("MERGED") => PrState::Merged,
        other => return Err(format!("gh reported an unknown PR state {other:?}")),
    };
    Ok(PrFacts {
        head: head.to_owned(),
        base: base.to_owned(),
        merged_head: (state == PrState::Merged).then(|| head.to_owned()),
        state,
    })
}

fn execute_system(domain: &DomainRuntime, command: Command) -> Result<(), StoreError> {
    let state = domain.state().map_err(StoreError::Storage)?;
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch,
        command_id: format!("github-{}", crate::auth::random_hex(16)).into(),
        expected_revision: state.revision,
        command,
    };
    domain.execute(&Actor::System, &envelope, None, || Ok(())).map(|_| ())
}

/// PRs worth asking GitHub about: those of a Task that is not finished, plus
/// any PR whose merge attempt is still claimed or uncertain.
fn watched(state: &DomainState) -> Vec<PullRequest> {
    state.pull_requests.values().filter(|pr| {
        state.tasks.get(&pr.task_id).is_some_and(|task| !task.state.terminal())
            || state.merge_authorizations.values().any(|authorization| authorization.pr_id == pr.id
                && matches!(authorization.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain))
    }).cloned().collect()
}

/// Bring the domain's view of each watched PR up to date with GitHub. A PR
/// GitHub cannot report is left as it was.
pub(crate) async fn reconcile(domain: &DomainRuntime, observer: &impl PrObserver) -> Result<()> {
    let state = domain.state()?;
    for pr in watched(&state) {
        let facts = match observer.observe(&pr.host, &pr.repository, pr.number).await {
            Ok(facts) => facts,
            Err(reason) => {
                log!("GitHub could not report {}/{}#{}: {reason}", pr.host, pr.repository, pr.number);
                continue;
            }
        };
        apply(domain, &state, &pr, &facts)?;
    }
    Ok(())
}

fn apply(domain: &DomainRuntime, state: &DomainState, pr: &PullRequest, facts: &PrFacts) -> Result<()> {
    let unchanged = pr.head == facts.head && pr.base == facts.base
        && pr.state == facts.state && pr.merged_head == facts.merged_head;
    if !unchanged {
        match execute_system(domain, Command::ObservePrState {
            pr_id: pr.id.clone(), head: facts.head.clone(), base: facts.base.clone(),
            state: facts.state, merged_head: facts.merged_head.clone(),
        }) {
            Ok(()) => (),
            Err(StoreError::Domain(error)) => log!("PR observation for {} refused: {error}", pr.id),
            Err(StoreError::Storage(error)) => return Err(error),
        }
    }
    // A claimed attempt cannot succeed any more once GitHub merged another
    // head or closed the PR; a merge at the authorized head is confirmed by
    // the observation itself.
    for authorization in state.merge_authorizations.values().filter(|authorization| {
        authorization.pr_id == pr.id
            && matches!(authorization.attempt, MergeAttemptState::Claimed | MergeAttemptState::Uncertain)
    }) {
        let failed = match facts.state {
            PrState::Closed => true,
            PrState::Merged => facts.merged_head.as_deref() != Some(authorization.head.as_str()),
            PrState::Open => false,
        };
        if failed {
            match execute_system(domain, Command::ObserveMergeAttempt {
                authorization_id: authorization.id.clone(), state: MergeAttemptState::Failed,
            }) {
                Ok(()) => (),
                Err(StoreError::Domain(error)) => log!("merge attempt observation for {} refused: {error}", authorization.id),
                Err(StoreError::Storage(error)) => return Err(error),
            }
        }
    }
    Ok(())
}

pub(crate) async fn run(domain: Arc<DomainRuntime>, observer: Arc<impl PrObserver>) -> Result<()> {
    loop {
        reconcile(&domain, &*observer).await?;
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}

/// A claim is admitted only when GitHub confirms the head and base the Agent
/// reports for the authorized PR; the Agent's own word is not the verification.
pub(crate) async fn verify_claim(
    domain: &DomainRuntime, observer: &dyn PrObserver, command: &Command,
) -> Result<(), StoreError> {
    let Command::ClaimMergeAuthorization { authorization_id, remote_head, remote_base, base_verified } = command else {
        return Ok(());
    };
    let state = domain.state().map_err(StoreError::Storage)?;
    let facts = observed_authorization(&state, authorization_id, observer).await?;
    if !base_verified || facts.base != *remote_base {
        return Err(alondra_core::domain_v2::DomainError::new("stack_not_ready",
            "GitHub reports a different base than the claim").into());
    }
    if facts.state != PrState::Open || facts.head != *remote_head {
        return Err(alondra_core::domain_v2::DomainError::new("stale_head",
            "GitHub reports a different head than the claim").into());
    }
    Ok(())
}

async fn observed_authorization(
    state: &DomainState, authorization_id: &AuthorizationId, observer: &dyn PrObserver,
) -> Result<PrFacts, StoreError> {
    let authorization = state.merge_authorizations.get(authorization_id)
        .ok_or_else(|| alondra_core::domain_v2::DomainError::new("not_found", "Authorization missing"))?;
    let pr = state.pull_requests.get(&authorization.pr_id)
        .ok_or_else(|| alondra_core::domain_v2::DomainError::new("not_found", "PR missing"))?;
    observer.observe(&pr.host, &pr.repository, pr.number).await.map_err(|reason| {
        alondra_core::domain_v2::DomainError::new("external_read_failed",
            format!("could not verify the PR on GitHub: {reason}")).into()
    })
}

#[cfg(test)]
pub(crate) struct FakeObserver(pub std::sync::Mutex<std::collections::BTreeMap<u64, Result<PrFacts, String>>>);
#[cfg(test)]
impl FakeObserver {
    pub(crate) fn new() -> Self {
        Self(std::sync::Mutex::new(Default::default()))
    }
    pub(crate) fn set(&self, number: u64, facts: Result<PrFacts, String>) {
        self.0.lock().unwrap().insert(number, facts);
    }
}
#[cfg(test)]
impl PrObserver for FakeObserver {
    fn observe<'a>(&'a self, _: &'a str, _: &'a str, number: u64) -> Observed<'a> {
        let facts = self.0.lock().unwrap().get(&number).cloned()
            .unwrap_or_else(|| Err("no such PR".into()));
        Box::pin(async move { facts })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;
    use std::collections::BTreeSet;
    use std::os::unix::fs::PermissionsExt;

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn command(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";
    const OTHER: &str = "89abcdef0123456789abcdef0123456789abcdef";

    fn facts(head: &str, state: PrState) -> PrFacts {
        PrFacts { head: head.into(), base: "main".into(), state,
            merged_head: (state == PrState::Merged).then(|| head.to_owned()) }
    }

    /// A Task with a registered PR, a presented final and the user's merge authorization.
    fn authorized(domain: &DomainRuntime) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into() });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge: false });
        command(domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        });
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true,
        });
        command(domain, Actor::User { user_id: "u".into() }, "authorize", Command::AuthorizeMerge {
            authorization_id: "auth".into(), presentation_id: "d".into(), pr_id: "pr".into(),
            expected_head: HEAD.into(), method: MergeMethod::Squash,
        });
    }

    fn presented(domain: &DomainRuntime, auto_merge: bool) {
        command(domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "a".into(), name: "A".into() });
        command(domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest { request_id: "r".into(), body: "Work".into(), auto_merge });
        command(domain, Actor::User { user_id: "u".into() }, "task", Command::CreateTask {
            task_id: "t".into(), request_id: "r".into(), title: "Task".into(), agent_id: Some("a".into()),
            review_prs: true, dependencies: BTreeSet::new(), stack: None,
        });
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        command(domain, Actor::Agent { agent_id: "a".into() }, "pr", register.clone());
        assert_eq!(crate::domain_api::auto_merge_notice(domain, &register).is_some(), auto_merge);
        command(domain, Actor::Agent { agent_id: "a".into() }, "present", Command::PresentResult {
            attachments: vec![], task_id: "t".into(), interaction_id: "d".into(), body: "Ready".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }], is_final: true,
        });
    }

    fn complete(domain: &DomainRuntime) -> Result<(), StoreError> {
        let state = domain.state().unwrap();
        domain.execute(&Actor::Agent { agent_id: "a".into() }, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: "complete".into(),
            expected_revision: state.revision,
            command: Command::CompleteTask { task_id: "t".into(), presentation_id: "d".into() },
        }, None, || Ok(())).map(|_| ())
    }

    #[tokio::test]
    async fn a_merge_the_agent_made_is_observed_and_accepted_only_for_an_auto_merge_request() {
        for (auto_merge, code) in [(true, None), (false, Some("acceptance_missing"))] {
            let (_fixture, domain) = open("agent-merge");
            presented(&domain, auto_merge);
            let observer = FakeObserver::new();
            observer.set(7, Ok(facts(HEAD, PrState::Merged)));
            reconcile(&domain, &observer).await.unwrap();
            assert_eq!(domain.state().unwrap().pull_requests[&"pr".into()].state, PrState::Merged);
            let done = complete(&domain);
            assert_eq!(done.as_ref().err().map(|error| match error {
                StoreError::Domain(error) => error.code.as_str(),
                StoreError::Storage(_) => "storage",
            }), code, "{done:?}");
        }
    }

    #[test]
    fn the_notice_names_the_pr_the_method_the_head_and_the_stack_condition() {
        let (_fixture, domain) = open("notice");
        presented(&domain, true);
        let register = Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "owner/repo".into(),
            number: 7, head: HEAD.into(), base: "main".into(), allowed_methods: vec![MergeMethod::Squash],
        };
        let notice = crate::domain_api::auto_merge_notice(&domain, &register).unwrap();
        assert!(notice.contains("https://github.com/owner/repo/pull/7"), "{notice}");
        assert!(notice.contains(&format!("gh pr merge --squash --match-head-commit {HEAD}")), "{notice}");
        assert!(notice.contains("for a stacked PR, once the PR it is stacked on is merged"), "{notice}");
    }

    fn open(tag: &str) -> (Fixture, DomainRuntime) {
        let home = std::env::temp_dir().join(format!("alondra-github-{tag}-{}", crate::auth::random_hex(8)));
        std::fs::create_dir(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        (Fixture(home), domain)
    }

    #[tokio::test]
    async fn reconcile_records_remote_reality_with_the_merged_pr_head_and_never_completes() {
        let (_fixture, domain) = open("reconcile");
        authorized(&domain);
        let observer = FakeObserver::new();
        let before = domain.state().unwrap().revision;
        reconcile(&domain, &observer).await.unwrap();
        assert_eq!(domain.state().unwrap().revision, before, "an unreported PR is left alone");
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        reconcile(&domain, &observer).await.unwrap();
        assert_eq!(domain.state().unwrap().revision, before, "unchanged facts write nothing");

        command(&domain, Actor::Agent { agent_id: "a".into() }, "claim", Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(), remote_head: HEAD.into(), remote_base: "main".into(), base_verified: true,
        });
        observer.set(7, Ok(facts(HEAD, PrState::Merged)));
        reconcile(&domain, &observer).await.unwrap();
        let state = domain.state().unwrap();
        let pr = &state.pull_requests[&"pr".into()];
        assert_eq!(pr.state, PrState::Merged);
        assert_eq!(pr.merged_head.as_deref(), Some(HEAD), "the merged head is the PR head, not a merge commit");
        assert_eq!(state.merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Confirmed);
        assert_ne!(state.tasks[&"t".into()].state, TaskState::Done, "observation never completes");
        command(&domain, Actor::Agent { agent_id: "a".into() }, "complete", Command::CompleteTask {
            task_id: "t".into(), presentation_id: "d".into(),
        });
        assert_eq!(domain.state().unwrap().tasks[&"t".into()].state, TaskState::Done);
    }

    #[tokio::test]
    async fn a_claimed_attempt_fails_when_github_merged_another_head_or_closed_the_pr() {
        for (tag, remote) in [("other-head", facts(OTHER, PrState::Merged)), ("closed", facts(HEAD, PrState::Closed))] {
            let (_fixture, domain) = open(tag);
            authorized(&domain);
            command(&domain, Actor::Agent { agent_id: "a".into() }, "claim", Command::ClaimMergeAuthorization {
                authorization_id: "auth".into(), remote_head: HEAD.into(), remote_base: "main".into(), base_verified: true,
            });
            let observer = FakeObserver::new();
            observer.set(7, Ok(remote.clone()));
            reconcile(&domain, &observer).await.unwrap();
            let state = domain.state().unwrap();
            assert_eq!(state.pull_requests[&"pr".into()].state, remote.state, "{tag}");
            assert_eq!(state.merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Failed, "{tag}");
            assert_ne!(state.tasks[&"t".into()].state, TaskState::Done);
        }
    }

    #[tokio::test]
    async fn a_claim_is_verified_against_github_before_it_is_admitted() {
        let (_fixture, domain) = open("claim");
        authorized(&domain);
        let observer = FakeObserver::new();
        let claim = |head: &str, base: &str, verified: bool| Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(), remote_head: head.into(), remote_base: base.into(), base_verified: verified,
        };
        let code = |result: Result<(), StoreError>| match result {
            Ok(()) => "ok".to_owned(),
            Err(StoreError::Domain(error)) => error.code,
            Err(StoreError::Storage(error)) => panic!("{error:#}"),
        };
        assert_eq!(code(verify_claim(&domain, &observer, &claim(HEAD, "main", true)).await), "external_read_failed");
        observer.set(7, Ok(facts(OTHER, PrState::Open)));
        assert_eq!(code(verify_claim(&domain, &observer, &claim(HEAD, "main", true)).await), "stale_head");
        observer.set(7, Ok(PrFacts { base: "develop".into(), ..facts(HEAD, PrState::Open) }));
        assert_eq!(code(verify_claim(&domain, &observer, &claim(HEAD, "main", true)).await), "stack_not_ready");
        observer.set(7, Ok(facts(HEAD, PrState::Open)));
        assert_eq!(code(verify_claim(&domain, &observer, &claim(HEAD, "main", false)).await), "stack_not_ready");
        assert_eq!(code(verify_claim(&domain, &observer, &claim(OTHER, "main", true)).await), "stale_head",
            "the Agent's own head claim must match GitHub, not only the authorization");
        assert_eq!(code(verify_claim(&domain, &observer, &claim(HEAD, "main", true)).await), "ok");
        assert_eq!(code(verify_claim(&domain, &observer, &Command::ClaimMergeAuthorization {
            authorization_id: "missing".into(), remote_head: HEAD.into(), remote_base: "main".into(), base_verified: true,
        }).await), "not_found");
        assert_eq!(code(verify_claim(&domain, &observer, &Command::CancelTask { task_id: "t".into() }).await), "ok",
            "other commands are not GitHub claims");
        assert_eq!(domain.state().unwrap().merge_authorizations[&"auth".into()].attempt, MergeAttemptState::Unclaimed);
    }

    #[tokio::test]
    async fn gh_output_is_parsed_from_a_fake_binary_and_failures_are_reported() {
        let (fixture, _) = open("gh");
        let program = fixture.0.join("gh");
        let log = fixture.0.join("calls");
        std::fs::write(&program, format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >> {}\ncase \"$*\" in *pull/7*) printf '%s' '{{\"state\":\"MERGED\",\"headRefOid\":\"{HEAD}\",\"baseRefName\":\"main\",\"mergeCommit\":{{\"oid\":\"{OTHER}\"}}}}';; *pull/8*) printf '%s' '{{\"state\":\"OPEN\",\"headRefOid\":\"short\",\"baseRefName\":\"main\"}}';; *) echo 'no pull requests found' >&2; exit 1;; esac\n", log.display())).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let gh = GhCli::at(program);
        let merged = gh.observe("github.com", "owner/repo", 7).await.unwrap();
        assert_eq!(merged, PrFacts { head: HEAD.into(), base: "main".into(), state: PrState::Merged, merged_head: Some(HEAD.into()) });
        assert_eq!(gh.observe("github.com", "owner/repo", 8).await.unwrap_err(), "gh reported no PR head");
        assert!(gh.observe("github.com", "owner/repo", 9).await.unwrap_err().contains("no pull requests found"));
        let calls = std::fs::read_to_string(&log).unwrap();
        assert!(calls.contains("pr view https://github.com/owner/repo/pull/7 --json state,headRefOid,baseRefName"), "{calls}");
        assert!(!calls.contains("merge"), "the daemon never merges a v2 PR");
    }
}
