//! What GitHub says about each open pull request that the domain does not keep: its
//! title, checks and whether it merges cleanly. Refreshed in the background and read from
//! memory, so a client's poll never waits on `gh`.
use alondra_core::domain::{
    Actor, Command, CommandEnvelope, DeliveryTarget, DomainError, DomainState, Instruction, InteractionPayload,
    PrState, PullRequest, PROTOCOL_VERSION,
};
use alondra_core::pr::Check;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub(crate) type Viewed<'a> = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>;

/// `gh pr view --json`, behind a pointer so a test can answer for GitHub.
pub(crate) trait PrView: Send + Sync + 'static {
    fn view<'a>(&'a self, url: &'a str) -> Viewed<'a>;
}

pub(crate) struct GhView;

/// GitHub as a test sets it: `None` is GitHub not answering.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct FakePrView(pub Mutex<Option<Value>>);

#[cfg(test)]
impl FakePrView {
    pub(crate) fn answer(&self, view: Option<Value>) {
        *self.0.lock().unwrap() = view;
    }
}

#[cfg(test)]
impl PrView for FakePrView {
    fn view<'a>(&'a self, _: &'a str) -> Viewed<'a> {
        let view = self.0.lock().unwrap().clone();
        Box::pin(async move { view })
    }
}

impl PrView for GhView {
    fn view<'a>(&'a self, url: &'a str) -> Viewed<'a> {
        Box::pin(crate::prs::Gh::view(&crate::prs::GhCli, url))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub(crate) struct Details {
    pub title: Option<String>,
    pub body: Option<String>,
    pub head_ref: Option<String>,
    pub head_sha: Option<String>,
    pub base_ref: Option<String>,
    pub checks: Vec<Check>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub conflicts: bool,
    pub mergeability_unknown: bool,
}

/// Whether a PR merges into its base, as far as GitHub has told us.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Merge {
    Clean,
    Conflicts,
    Unknown,
}

impl Details {
    fn merge(&self) -> Merge {
        if self.conflicts {
            Merge::Conflicts
        } else if self.mergeability_unknown {
            Merge::Unknown
        } else {
            Merge::Clean
        }
    }
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

impl super::Client {
    pub(crate) fn use_pr_view(&self, view: Arc<dyn PrView>) {
        *self.pr_view.lock().expect("pull request view poisoned") = view;
    }

    /// Whether `pr` merges at `head`, asked up to `attempts` times while GitHub is still computing.
    /// A clean answer for this head is trusted; a conflict or an unknown is asked again, since a
    /// base that moved can clear either without a new head.
    pub(crate) async fn merge_state(&self, pr: &PullRequest, head: &str, attempts: usize, pause: Duration) -> Merge {
        if self.details.get(&pr.id.0)
            .is_some_and(|details| details.head_sha.as_deref() == Some(head) && details.merge() == Merge::Clean)
        {
            return Merge::Clean;
        }
        let view = self.pr_view.lock().expect("pull request view poisoned").clone();
        let url = alondra_core::pr::canonical(&pr.repository, pr.number);
        for attempt in 0..attempts {
            if attempt > 0 {
                tokio::time::sleep(pause).await;
            }
            let Some(answer) = view.view(&url).await else { return Merge::Unknown };
            let details = parse(&pr.id.0, pr.number, &answer);
            let merge = details.merge();
            self.details.record(&pr.id.0, details);
            if merge != Merge::Unknown {
                return merge;
            }
        }
        Merge::Unknown
    }

    /// A node presenting a PR that GitHub says conflicts is told to resolve it first: the card and
    /// its push would only reach the user to be handed back a minute later.
    pub(crate) async fn verify_presentation(&self, command: &Command) -> Result<(), DomainError> {
        let Command::PresentResult { targets, interaction_id, .. } = command else { return Ok(()) };
        let Ok(state) = self.domain.state() else { return Ok(()) };
        if state.interactions.contains_key(interaction_id) {
            return Ok(());
        }
        for target in targets {
            let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
            let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open) else { continue };
            if self.merge_state(pr, head, PRESENT_ATTEMPTS, PRESENT_PAUSE).await == Merge::Conflicts {
                return Err(DomainError::new("pr_conflicts", presentation_conflicts_text(pr)));
            }
        }
        Ok(())
    }
}

const PRESENT_ATTEMPTS: usize = 3;
const PRESENT_PAUSE: Duration = Duration::from_secs(2);

fn presentation_conflicts_text(pr: &PullRequest) -> String {
    let url = alondra_core::pr::canonical(&pr.repository, pr.number);
    let base = &pr.base;
    format!("GitHub reports that {url} conflicts with {base}, so nothing was presented: bring {base} into the branch, \
        resolve the conflicts, run the tests and push, then present the PR again with present_result and its new head. \
        If you cannot resolve them, ask the user.")
}

/// A presented PR that GitHub says conflicts goes back to its Agent with the words of the user's
/// Resolve conflicts, once per head and without a card: the user only hears of it if the Agent
/// asks.
pub(crate) fn hand_back_conflicts(domain: &crate::domain_runtime::DomainRuntime, details: &PrDetails, name: &str) {
    let Ok(state) = domain.state() else { return };
    for pr_id in state.pull_requests.keys() {
        let Ok(state) = domain.state() else { return };
        let Some(pr) = state.pull_requests.get(pr_id) else { continue };
        if !details.get(&pr.id.0).is_some_and(|details| details.conflicts) || conflicts_sent(&state, pr) {
            continue;
        }
        let Some(presentation) = crate::views::actions::pr_presentation(&state, pr) else { continue };
        let InteractionPayload::Delivery { targets, acceptance, .. } = &presentation.payload else { continue };
        if acceptance.as_ref().is_some_and(|acceptance| acceptance.valid) || state.handed_back(presentation).is_some() {
            continue;
        }
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: crate::views::actions::conflicts_command_id(pr),
            expected_revision: state.revision,
            command: Command::SubmitFeedback {
                attachments: vec![],
                presentation_id: presentation.id.clone(),
                targets: targets.clone(),
                body: crate::domain_messages::conflicts_text(name, pr),
                instruction: Instruction::Conflicts,
            },
        };
        if let Err(error) = domain.execute(&Actor::System, &envelope, None, || Ok(())) {
            log!("could not hand the conflicts of {} back to its node: {error}", pr.id);
        }
    }
}

fn conflicts_sent(state: &DomainState, pr: &alondra_core::domain::PullRequest) -> bool {
    state.interactions.values().any(|interaction| matches!(&interaction.payload,
        InteractionPayload::Delivery { targets, feedback, .. }
            if feedback.iter().any(|f| f.instruction == Instruction::Conflicts && alondra_core::domain::merge_failure(&f.body).is_none())
                && targets.iter().any(|target| matches!(target, DeliveryTarget::PullRequest { pr_id, head }
                    if pr_id == &pr.id && head == &pr.head))))
}

fn parse(id: &str, number: u64, view: &Value) -> Details {
    let stub = alondra_core::pr::PullRequest::opened(0, &alondra_core::pr::canonical("o/r", number), id, None, 0)
        .expect("a canonical URL parses");
    let card = crate::prs::refreshed(&stub, view, None);
    Details {
        title: Some(card.title.clone()),
        body: card.body.clone(),
        head_ref: card.head.clone(),
        head_sha: card.head_sha.clone(),
        base_ref: card.base.clone(),
        conflicts: card.conflicts(),
        mergeability_unknown: card.mergeability_unknown(),
        additions: card.additions,
        deletions: card.deletions,
        changed_files: card.changed_files,
        checks: card.checks,
    }
}

#[cfg(test)]
mod tests {
    use super::{Details, PrDetails, hand_back_conflicts, parse};
    use alondra_core::domain::{
        Actor, Command, CommandEnvelope, DeliveryTarget, Instruction, InteractionPayload, MergeMethod, PrState,
    };
    use serde_json::json;

    struct Home(std::path::PathBuf);
    impl Drop for Home {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn run(domain: &crate::domain_runtime::DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(),
            command_id: id.into(), expected_revision: state.revision, command }, None, || Ok(())).unwrap();
    }

    fn present(domain: &crate::domain_runtime::DomainRuntime, id: &str, head: &str) {
        run(domain, Actor::Agent { agent_id: "agent".into() }, id, Command::PresentResult { attachments: vec![],
            task_id: "task".into(), interaction_id: id.into(), body: "Ready".into(), is_final: true, nothing_to_review: false,
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: head.into() }], card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true) });
    }

    fn conflicts_told(domain: &crate::domain_runtime::DomainRuntime) -> Vec<(String, String)> {
        domain.state().unwrap().interactions.values().flat_map(|interaction| match &interaction.payload {
            InteractionPayload::Delivery { feedback, .. } => feedback.iter()
                .filter(|f| f.instruction == Instruction::Conflicts)
                .map(|f| (interaction.id.0.clone(), f.body.clone())).collect(),
            _ => vec![],
        }).collect()
    }

    #[test]
    fn a_conflicting_presented_pr_goes_back_to_its_node_once_per_head() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = Home(scratch_directory.parent().unwrap().join(format!("alondra-conflicts-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&home.0).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home.0).unwrap();
        let user = Actor::User { user_id: "user".into() };
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Agent".into() });
        run(&domain, user.clone(), "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, user, "task", Command::CreateTask { task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent".into()), review_prs: false, dependencies: Default::default(), stack: None });
        run(&domain, Actor::Agent { agent_id: "agent".into() }, "pr", Command::RegisterPr { task_id: "task".into(),
            pr_id: "pr".into(), host: "github.com".into(), repository: "me/app".into(), number: 7, head: "head1".into(),
            base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
        present(&domain, "first", "head1");
        let details = PrDetails::default();
        details.record("pr", Details { conflicts: false, ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        assert!(conflicts_told(&domain).is_empty(), "a PR that merges cleanly stays with the user");

        details.record("pr", Details { conflicts: true, ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        hand_back_conflicts(&domain, &details, "Ana");
        let told = conflicts_told(&domain);
        assert_eq!(told.len(), 1, "once per head: {told:?}");
        assert!(told[0].1.starts_with("Ana asks you to resolve the conflicts of https://github.com/me/app/pull/7 with main"), "{}", told[0].1);
        assert!(!domain.state().unwrap().projection().inbox.iter().any(|item| item.pending), "no card for the user");

        present(&domain, "again", "head1");
        hand_back_conflicts(&domain, &details, "Ana");
        assert_eq!(conflicts_told(&domain).len(), 1, "presenting the same head again is not a new head");

        run(&domain, Actor::System, "observed", Command::ObservePrState { pr_id: "pr".into(), head: "head2".into(),
            base: "main".into(), state: PrState::Open, merged_head: None });
        present(&domain, "pushed", "head2");
        hand_back_conflicts(&domain, &details, "Ana");
        assert_eq!(conflicts_told(&domain).iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["first", "pushed"]);
    }

    #[test]
    fn a_merge_failure_is_not_a_conflict_already_sent() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = Home(scratch_directory.parent().unwrap().join(format!("alondra-conflicts-merge-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&home.0).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home.0).unwrap();
        let user = Actor::User { user_id: "user".into() };
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Agent".into() });
        run(&domain, user.clone(), "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false });
        run(&domain, user, "task", Command::CreateTask { task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
            agent_id: Some("agent".into()), review_prs: false, dependencies: Default::default(), stack: None });
        run(&domain, Actor::Agent { agent_id: "agent".into() }, "pr", Command::RegisterPr { task_id: "task".into(),
            pr_id: "pr".into(), host: "github.com".into(), repository: "me/app".into(), number: 7, head: "head1".into(),
            base: "main".into(), allowed_methods: vec![MergeMethod::Squash] });
        present(&domain, "first", "head1");
        run(&domain, Actor::System, "merge-failed", Command::SubmitFeedback { attachments: vec![], presentation_id: "first".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: "head1".into() }],
            body: alondra_core::domain::merge_failed_text(7, "https://github.com/me/app/pull/7", "main", "review required"),
            instruction: Instruction::Conflicts });
        present(&domain, "again", "head1");
        let details = PrDetails::default();
        details.record("pr", Details { conflicts: true, ..Default::default() });
        hand_back_conflicts(&domain, &details, "Ana");
        assert_eq!(conflicts_told(&domain).iter().filter(|(id, _)| id == "again").count(), 1,
            "a real conflict at the same head still goes back");
    }

    #[test]
    fn details_keep_the_change_size_and_each_check_by_name() {
        let details = parse("pr-1", 7, &json!({
            "title": "Editor", "additions": 120, "deletions": 30, "changedFiles": 4,
            "mergeable": "MERGEABLE", "state": "OPEN",
            "statusCheckRollup": [
                {"name": "build", "status": "COMPLETED", "conclusion": "SUCCESS"},
                {"name": "ui tests", "status": "IN_PROGRESS"}]
        }));
        assert_eq!((details.additions, details.deletions, details.changed_files), (120, 30, 4));
        assert_eq!(details.checks.iter().map(|check| check.name.as_str()).collect::<Vec<_>>(), ["build", "ui tests"]);
    }
}
