//! What GitHub says about each open pull request that the domain does not keep: its
//! title, checks and whether it merges cleanly. Refreshed in the background and read from
//! memory, so a client's poll never waits on `gh`.
use alondra_core::domain_v2::{
    Actor, Command, CommandEnvelope, DeliveryTarget, DomainState, Instruction, InteractionPayload, PrState,
    PROTOCOL_VERSION,
};
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
    pub body: Option<String>,
    pub head_ref: Option<String>,
    pub base_ref: Option<String>,
    pub checks: Vec<Check>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
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

fn conflicts_sent(state: &DomainState, pr: &alondra_core::domain_v2::PullRequest) -> bool {
    state.interactions.values().any(|interaction| matches!(&interaction.payload,
        InteractionPayload::Delivery { targets, feedback, .. }
            if feedback.iter().any(|f| f.instruction == Instruction::Conflicts)
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
    use alondra_core::domain_v2::{
        Actor, Command, CommandEnvelope, DeliveryTarget, Instruction, InteractionPayload, MergeMethod, PrState,
    };
    use serde_json::json;

    struct Home(std::path::PathBuf);
    impl Drop for Home {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn run(domain: &crate::domain_runtime::DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch,
            command_id: id.into(), expected_revision: state.revision, command }, None, || Ok(())).unwrap();
    }

    fn present(domain: &crate::domain_runtime::DomainRuntime, id: &str, head: &str) {
        run(domain, Actor::Agent { agent_id: "agent".into() }, id, Command::PresentResult { attachments: vec![],
            task_id: "task".into(), interaction_id: id.into(), body: "Ready".into(), is_final: true,
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: head.into() }], card: alondra_core::domain_v2::CardSpec::delivery("Review the result", &[], true) });
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
        let home = Home(std::env::temp_dir().join(format!("alondra-conflicts-{}", crate::auth::random_hex(8))));
        std::fs::create_dir(&home.0).unwrap();
        let domain = crate::domain_runtime::DomainRuntime::open(&home.0).unwrap();
        let user = Actor::User { user_id: "user".into() };
        run(&domain, Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Agent".into() });
        run(&domain, user.clone(), "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false });
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
