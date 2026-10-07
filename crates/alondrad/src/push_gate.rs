//! The last look before a user push leaves: a push only ever announces a card the user can act
//! on when it arrives.
use crate::client_routes::{Client, Merge};
use alondra_core::domain::{
    DeliveryTarget, DomainState, InboxItem, InboxSubject, InteractionId, InteractionPayload, PrState,
};
use std::time::Duration;

/// How long a push waits for GitHub to say whether its PR merges before it goes anyway.
pub(crate) const UNKNOWN_MERGE_PATIENCE: Duration = Duration::from_secs(10 * 60);
const SEND_ATTEMPTS: usize = 3;
const SEND_PAUSE: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Gate {
    Send,
    Wait(&'static str),
    Drop(&'static str),
}

pub(crate) async fn check(
    state: &DomainState,
    client: Option<&Client>,
    interaction_id: Option<&str>,
    waited: Duration,
) -> Gate {
    let Some(id) = interaction_id else { return Gate::Send };
    let id = InteractionId::from(id);
    if !card_pending(&state.projection().inbox, &id) {
        return Gate::Drop("card_not_pending");
    }
    let (Some(client), Some(interaction)) = (client, state.interactions.get(&id)) else { return Gate::Send };
    let InteractionPayload::Delivery { targets, .. } = &interaction.payload else { return Gate::Send };
    for target in targets {
        let DeliveryTarget::PullRequest { pr_id, head } = target else { continue };
        let Some(pr) = state.pull_requests.get(pr_id).filter(|pr| pr.state == PrState::Open) else { continue };
        match client.merge_state(pr, head, SEND_ATTEMPTS, SEND_PAUSE).await {
            Merge::Clean => {}
            Merge::Conflicts => {
                crate::client_routes::hand_back_conflicts(
                    &client.domain,
                    &client.details,
                    &crate::domain_messages::user_name(),
                );
                return Gate::Drop("pr_conflicts");
            }
            Merge::Unknown if waited < UNKNOWN_MERGE_PATIENCE => return Gate::Wait("pr_mergeability_unknown"),
            Merge::Unknown => {}
        }
    }
    Gate::Send
}

/// The earlier cards this one repeats: same Task and author, same kind, words and card, each
/// withdrawn unanswered. A node whose call was cut puts the same card to the user again under a
/// new id; the user was already told about it, so its push is not a new notice.
pub(crate) fn earlier_copies(state: &DomainState, id: &InteractionId) -> Vec<InteractionId> {
    let Some(card) = state.interactions.get(id) else { return vec![] };
    state.interactions.values()
        .filter(|earlier| earlier.id != card.id && earlier.withdrawn && earlier.order < card.order)
        .filter(|earlier| earlier.scope == card.scope && earlier.author == card.author)
        .filter(|earlier| earlier.body == card.body && earlier.card == card.card)
        .filter(|earlier| same_kind(&earlier.payload, &card.payload))
        .map(|earlier| earlier.id.clone())
        .collect()
}

fn same_kind(earlier: &InteractionPayload, card: &InteractionPayload) -> bool {
    match (earlier, card) {
        (InteractionPayload::Question { options: a, answer: None }, InteractionPayload::Question { options: b, .. })
        | (InteractionPayload::ActionRequest { options: a, answer: None, .. }, InteractionPayload::ActionRequest { options: b, .. }) => a == b,
        (InteractionPayload::Delivery { targets: a, is_final: x, .. }, InteractionPayload::Delivery { targets: b, is_final: y, .. }) => a == b && x == y,
        (InteractionPayload::Information { .. }, InteractionPayload::Information { .. }) => true,
        _ => false,
    }
}

fn card_pending(items: &[InboxItem], id: &InteractionId) -> bool {
    items.iter().any(|item| {
        let InboxSubject::Interaction { interaction_id } = &item.subject;
        item.pending && interaction_id == id
    })
}

#[cfg(test)]
mod tests {
    use super::{Gate, UNKNOWN_MERGE_PATIENCE, check};
    use crate::domain_runtime::DomainRuntime;
    use crate::client_routes::{Client, FakePrView};
    use alondra_core::domain::{
        Actor, CardSpec, Command, CommandEnvelope, DeliveryTarget, Instruction, InteractionPayload, MergeMethod,
    };
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    const HEAD: &str = "0123456789abcdef0123456789abcdef01234567";

    struct Fixture {
        home: std::path::PathBuf,
        domain: Arc<DomainRuntime>,
        client: Arc<Client>,
        github: Arc<FakePrView>,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    impl Fixture {
        fn presented() -> Self {
            let home = std::env::temp_dir().join(format!("alondra-push-gate-{}", crate::auth::random_hex(8)));
            std::fs::create_dir(&home).unwrap();
            let domain = Arc::new(DomainRuntime::open(&home).unwrap());
            let client = Client::open(domain.clone(), &home, Default::default());
            let github = Arc::new(FakePrView::default());
            client.use_pr_view(github.clone());
            let fixture = Self { home, domain, client, github };
            let user = Actor::User { user_id: "user".into() };
            let agent = Actor::Agent { agent_id: "agent".into() };
            fixture.run(Actor::System, "agent", Command::RegisterAgent { agent_id: "agent".into(), name: "Agent".into() });
            fixture.run(user.clone(), "request", Command::CreateRequest { request_id: "request".into(), body: "Work".into(), auto_merge: false });
            fixture.run(user, "task", Command::CreateTask { task_id: "task".into(), request_id: "request".into(), title: "Task".into(),
                agent_id: Some("agent".into()), review_prs: false, dependencies: Default::default(), stack: None });
            fixture.run(agent.clone(), "pr", Command::RegisterPr { task_id: "task".into(), pr_id: "pr".into(),
                host: "github.com".into(), repository: "me/app".into(), number: 7, head: HEAD.into(), base: "main".into(),
                allowed_methods: vec![MergeMethod::Squash] });
            let targets = vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }];
            fixture.run(agent, "result", Command::PresentResult { attachments: vec![], task_id: "task".into(),
                interaction_id: "result".into(), body: "Ready".into(), is_final: true,
                card: CardSpec::delivery("Review the result", &targets, true), targets });
            fixture
        }

        fn run(&self, actor: Actor, id: &str, command: Command) {
            let state = self.domain.state().unwrap();
            self.domain.execute(&actor, &CommandEnvelope { protocol_version: 2, data_epoch: state.data_epoch.clone(),
                command_id: id.into(), expected_revision: state.revision, command }, None, || Ok(())).unwrap();
        }

        fn github_says(&self, mergeable: &str, merge_state: &str) {
            self.github.answer(Some(json!({"state": "OPEN", "headRefOid": HEAD,
                "mergeable": mergeable, "mergeStateStatus": merge_state})));
        }

        async fn gate(&self, interaction_id: Option<&str>, waited: Duration) -> Gate {
            check(&self.domain.state().unwrap(), Some(&self.client), interaction_id, waited).await
        }

        fn handed_back(&self) -> bool {
            matches!(&self.domain.state().unwrap().interactions[&"result".into()].payload,
                InteractionPayload::Delivery { feedback, .. } if feedback.iter().any(|f| f.instruction == Instruction::Conflicts))
        }
    }

    #[tokio::test]
    async fn a_clean_pr_card_is_pushed() {
        let fixture = Fixture::presented();
        fixture.github_says("MERGEABLE", "CLEAN");
        assert_eq!(fixture.gate(Some("result"), Duration::ZERO).await, Gate::Send);
        assert!(!fixture.handed_back());
    }

    #[tokio::test]
    async fn a_pr_that_conflicts_when_its_push_leaves_goes_back_to_its_node_and_never_reaches_the_user() {
        let fixture = Fixture::presented();
        fixture.github_says("CONFLICTING", "DIRTY");
        assert_eq!(fixture.gate(Some("result"), Duration::ZERO).await, Gate::Drop("pr_conflicts"));
        assert!(fixture.handed_back(), "the node is told to resolve the conflicts");
        assert!(!fixture.domain.state().unwrap().projection().inbox.iter().any(|item| item.pending), "no card left");
    }

    #[tokio::test]
    async fn a_card_that_left_the_inbox_before_its_push_is_never_pushed() {
        let fixture = Fixture::presented();
        fixture.run(Actor::User { user_id: "user".into() }, "feedback", Command::SubmitFeedback { attachments: vec![],
            presentation_id: "result".into(),
            targets: vec![DeliveryTarget::PullRequest { pr_id: "pr".into(), head: HEAD.into() }],
            body: "Rename it".into(), instruction: Instruction::Changes });
        fixture.github_says("MERGEABLE", "CLEAN");
        assert_eq!(fixture.gate(Some("result"), Duration::ZERO).await, Gate::Drop("card_not_pending"));
        assert_eq!(fixture.gate(Some("never-created"), Duration::ZERO).await, Gate::Drop("card_not_pending"));
    }

    #[tokio::test]
    async fn a_pr_github_cannot_report_waits_for_a_while_then_is_pushed() {
        let fixture = Fixture::presented();
        assert_eq!(fixture.gate(Some("result"), Duration::ZERO).await, Gate::Wait("pr_mergeability_unknown"));
        assert_eq!(fixture.gate(Some("result"), UNKNOWN_MERGE_PATIENCE).await, Gate::Send);
    }

    #[tokio::test]
    async fn a_push_without_a_card_is_unchanged() {
        let fixture = Fixture::presented();
        assert_eq!(fixture.gate(None, Duration::ZERO).await, Gate::Send);
    }
}
