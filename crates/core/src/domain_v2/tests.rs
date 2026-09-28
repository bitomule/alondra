use super::*;
use std::collections::{BTreeMap, BTreeSet};
fn user() -> Actor {
    Actor::User {
        user_id: "u".into(),
    }
}
fn agent() -> Actor {
    Actor::Agent {
        agent_id: "a".into(),
    }
}
fn exec(s: &mut DomainState, a: Actor, c: Command) -> Result<CommandOutcome, DomainError> {
    s.execute(
        &a,
        &CommandEnvelope {
            protocol_version: 2,
            data_epoch: s.data_epoch.clone(),
            command_id: format!("c-{}", s.revision).into(),
            expected_revision: s.revision,
            command: c,
        },
    )
}
fn fixture() -> DomainState {
    let mut s = DomainState::new("epoch");
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "a".into(),
            name: "Instance A".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateRequest {
            request_id: "r".into(),
            body: "Build requested result".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "t".into(),
            request_id: "r".into(),
            title: "Result".into(),
            agent_id: Some("a".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: None,
        },
    )
    .unwrap();
    s
}
fn text() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::Text {
        text: "The result".into(),
    }]
}
fn present(s: &mut DomainState, id: &str, targets: Vec<DeliveryTarget>) {
    exec(
        s,
        agent(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "t".into(),
            interaction_id: id.into(),
            body: "Result ready".into(),
            targets,
            is_final: true,
        },
    )
    .unwrap();
}
fn reject_unchanged(s: &mut DomainState, a: Actor, c: Command, code: &str) {
    let before = s.clone();
    assert_eq!(exec(s, a, c).unwrap_err().code, code);
    assert_eq!(*s, before, "Rejected command changed state");
}
#[test]
fn information_read_is_not_a_reply_or_acceptance() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Useful result".into(),
        },
    )
    .unwrap();
    let task = s.tasks[&"t".into()].clone();
    let out = exec(
        &mut s,
        user(),
        Command::MarkRead {
            interaction_id: "info".into(),
        },
    )
    .unwrap();
    assert!(out.effects.is_empty());
    assert_eq!(s.tasks[&"t".into()], task);
    assert_eq!(s.projection().counts.unread_information, 0);
}
#[test]
fn explicit_acceptance_then_agent_completion_and_feedback_revocation() {
    let mut s = fixture();
    present(&mut s, "delivery", text());
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "delivery".into(),
        },
        "acceptance_missing",
    );
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "delivery".into(),
            targets: text(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "delivery".into(),
            targets: text(),
            body: "One correction".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "delivery".into(),
        },
        "acceptance_stale",
    );
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "delivery".into(),
            targets: text(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::ObserveAgent {
            agent_id: "a".into(),
            connected: false,
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "delivery".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn reaccepted_feedback_then_reassignment_needs_fresh_acceptance() {
    let mut s = fixture();
    present(&mut s, "delivery", text());
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "delivery".into(),
            targets: text(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::SubmitFeedback {
            attachments: vec![],
            presentation_id: "delivery".into(),
            targets: text(),
            body: "Please revise".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Running);
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "delivery".into(),
            targets: text(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::InReview);
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "b".into(),
        },
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "delivery".into(),
        },
        "acceptance_missing",
    );
}
#[test]
fn typed_option_ids_and_open_question_guard() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Explain decision".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "yes".into(),
                label: "Sí".into(),
                inputs: vec![InputSpec {
                    id: "why".into(),
                    kind: InputKind::Text,
                    required: true,
                }],
            }],
        },
    )
    .unwrap();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: text(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "blocking_interaction",
    );
    reject_unchanged(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "Sí".into(),
            inputs: BTreeMap::new(),
        },
        "invalid_input",
    );
    reject_unchanged(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "yes".into(),
            inputs: BTreeMap::new(),
        },
        "invalid_input",
    );
    let out = exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "yes".into(),
            inputs: BTreeMap::from([("why".into(), InputValue::Text("Reason".into()))]),
        },
    )
    .unwrap();
    assert_eq!(out.effects.len(), 1);
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
}
fn register(s: &mut DomainState) {
    exec(
        s,
        agent(),
        Command::RegisterPr {
            task_id: "t".into(),
            pr_id: "pr".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 1,
            head: "head1".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    )
    .unwrap();
}
fn pr_targets() -> Vec<DeliveryTarget> {
    vec![DeliveryTarget::PullRequest {
        pr_id: "pr".into(),
        head: "head1".into(),
    }]
}
fn authorize(s: &mut DomainState) {
    exec(
        s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            expected_head: "head1".into(),
            method: MergeMethod::Squash,
        },
    )
    .unwrap();
}
#[test]
fn authorization_merge_and_completion_are_three_distinct_steps() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    assert_eq!(s.pull_requests[&"pr".into()].state, PrState::Open);
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "merge_not_confirmed",
    );
    exec(
        &mut s,
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head1".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head1".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
        "delivery_uncertain",
    );
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    assert_ne!(s.tasks[&"t".into()].state, TaskState::Done);
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn head_push_invalidates_authorization_and_merge_external_does_not_accept() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head2".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head2".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
        "stale_head",
    );
    assert!(!s.merge_authorizations[&"auth".into()].valid);
}
#[test]
fn lifetime_pr_survives_cancel_reopen_and_register_is_noop() {
    let mut s = fixture();
    register(&mut s);
    let before = s.clone();
    register(&mut s);
    assert_eq!(before, s);
    exec(
        &mut s,
        user(),
        Command::CancelTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::ReopenTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::RegisterPr {
            task_id: "t".into(),
            pr_id: "pr2".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 2,
            head: "other".into(),
            base: "main".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
        "task_already_has_pr",
    );
}
#[test]
fn closed_request_prevents_reopening_its_terminal_task() {
    let mut s = fixture();
    exec(
        &mut s,
        user(),
        Command::CancelTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CloseRequest {
            request_id: "r".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::ReopenTask {
            task_id: "t".into(),
        },
        "not_found",
    );
    assert!(s.requests[&"r".into()].closed);
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
}
#[test]
fn document_revision_invalidates_entire_joint_delivery() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "https://example.org/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    let targets = vec![
        text()[0].clone(),
        DeliveryTarget::DocumentRevision {
            document_id: "doc".into(),
            revision_id: "v1".into(),
        },
    ];
    present(&mut s, "d", targets.clone());
    exec(
        &mut s,
        user(),
        Command::DecideDocumentReview {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v1".into(),
            accepted: true,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets,
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::ReviseDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v2".into(),
            content_digest: "b".repeat(64),
            content_reference: "https://example.org/design".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_stale",
    );
    assert_eq!(s.document_revisions.len(), 2);
}
#[test]
fn only_a_manager_can_link_another_tasks_document() {
    let mut s = fixture();
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "t2".into(),
            request_id: "r".into(),
            title: "Second task".into(),
            agent_id: Some("b".into()),
            review_prs: false,
            dependencies: BTreeSet::new(),
            stack: None,
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Private draft".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/draft".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "b".into(),
        },
        Command::LinkTaskDocument {
            task_id: "t2".into(),
            document_id: "doc".into(),
            role: "result".into(),
        },
        "forbidden",
    );
    assert!(!s.tasks[&"t2".into()].documents.contains_key(&"doc".into()));
    exec(
        &mut s,
        user(),
        Command::LinkTaskDocument {
            task_id: "t2".into(),
            document_id: "doc".into(),
            role: "shared".into(),
        },
    )
    .unwrap();
    assert!(s.tasks[&"t2".into()].documents.contains_key(&"doc".into()));
}
#[test]
fn sole_document_review_accepts_exact_final_delivery() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    present(
        &mut s,
        "d",
        vec![DeliveryTarget::DocumentRevision {
            document_id: "doc".into(),
            revision_id: "v1".into(),
        }],
    );
    exec(
        &mut s,
        user(),
        Command::DecideDocumentReview {
            task_id: "t".into(),
            document_id: "doc".into(),
            revision_id: "v1".into(),
            accepted: true,
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Closing);
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Done);
}
#[test]
fn cross_task_delivery_and_dependency_cycle_roll_back() {
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "t2".into(),
            request_id: "r".into(),
            title: "Other".into(),
            agent_id: Some("a".into()),
            review_prs: false,
            dependencies: BTreeSet::from(["t".into()]),
            stack: None,
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::SetDependencies {
            task_id: "t".into(),
            dependencies: BTreeSet::from(["t2".into()]),
            stack: None,
        },
        "dependency_cycle",
    );
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t2".into(),
            presentation_id: "d".into(),
        },
        "forbidden",
    );
    reject_unchanged(
        &mut s,
        agent(),
        Command::StartTask {
            task_id: "t2".into(),
        },
        "dependency_not_done",
    );
}
#[test]
fn old_epoch_revision_generation_and_actor_rejected() {
    let mut s = fixture();
    let before = s.clone();
    let mut e = CommandEnvelope {
        protocol_version: 1,
        data_epoch: s.data_epoch.clone(),
        command_id: "x".into(),
        expected_revision: s.revision,
        command: Command::StartTask {
            task_id: "t".into(),
        },
    };
    assert_eq!(
        s.execute(&agent(), &e).unwrap_err().code,
        "incompatible_protocol"
    );
    e.protocol_version = 2;
    e.data_epoch = "old".into();
    assert_eq!(s.execute(&agent(), &e).unwrap_err().code, "stale_epoch");
    assert_eq!(s, before);
    exec(
        &mut s,
        Actor::System,
        Command::AdvanceOrchestratorGeneration { generation: 2 },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        Actor::Orchestrator { generation: 1 },
        Command::StartTask {
            task_id: "t".into(),
        },
        "stale_generation",
    );
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "foreign".into(),
        },
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "not_assigned",
    );
}
#[test]
fn question_explicitly_linked_to_pr_keeps_identity_and_answer() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Choose".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "ok".into(),
                label: "Okay".into(),
                inputs: vec![],
            }],
        },
    )
    .unwrap();
    register(&mut s);
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::LinkInteractionSubject {
            interaction_id: "q".into(),
            subject: InteractionSubject::PullRequest { pr_id: "pr".into() },
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::LinkInteractionSubject {
            interaction_id: "q".into(),
            subject: InteractionSubject::DocumentReview {
                document_id: "doc".into(),
                revision_id: "v1".into(),
            },
        },
        "subject_already_linked",
    );
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
    let p = s.projection();
    assert_eq!(p.inbox[0].id, "pr:pr");
    assert_eq!(p.inbox[0].children[0].id, "interaction:q");
    assert!(matches!(
        s.interactions[&"q".into()].payload,
        InteractionPayload::Question {
            answer: Some(_),
            ..
        }
    ));
}

#[test]
fn projection_wire_roundtrip_and_optional_fixture_export() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::PublishInformation {
            attachments: vec![AttachmentRef {
                id: "fixture-file".into(),
                name: "Report.pdf".into(),
                content_digest: "c".repeat(64),
                content_reference: "files/fixture-file".into(),
                bytes: 321,
            }],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Information with Unicode: revisión ✓".into(),
        },
    )
    .unwrap();
    let information = s.projection();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Choose an option".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "proceed".into(),
                label: "Continuar".into(),
                inputs: vec![InputSpec {
                    id: "explanation".into(),
                    kind: InputKind::Text,
                    required: true,
                }],
            }],
        },
    )
    .unwrap();
    let question = s.projection();
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            option_id: "proceed".into(),
            inputs: BTreeMap::from([("explanation".into(), InputValue::Text("Reviewed".into()))]),
        },
    )
    .unwrap();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: text(),
        },
    )
    .unwrap();
    let accepted = s.projection();
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
    let completed = s.projection();
    let mut s = fixture();
    register(&mut s);
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "PR question".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "proceed".into(),
                label: "Proceed".into(),
                inputs: vec![],
            }],
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::LinkInteractionSubject {
            interaction_id: "q".into(),
            subject: InteractionSubject::PullRequest { pr_id: "pr".into() },
        },
    )
    .unwrap();
    present(&mut s, "d", pr_targets());
    let pr_question = s.projection();
    let fixtures = BTreeMap::from([
        ("information", information),
        ("question", question),
        ("accepted", accepted),
        ("completed", completed),
        ("pr_question", pr_question),
    ]);
    for projection in fixtures.values() {
        let bytes = serde_json::to_vec(projection).unwrap();
        let decoded: DomainProjection = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(&decoded, projection);
    }
    if let Ok(path) = std::env::var("ALONDRA_DOMAIN_V2_FIXTURE_OUTPUT") {
        std::fs::write(path, serde_json::to_vec_pretty(&fixtures).unwrap()).unwrap();
    }
}

#[test]
fn recipient_scope_and_generation_preserve_logical_coordination() {
    let mut s = fixture();
    exec(
        &mut s,
        Actor::System,
        Command::AdvanceOrchestratorGeneration { generation: 1 },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "to-orch".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::Orchestrator,
            body: "Need coordination".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "ok".into(),
                label: "Proceed".into(),
                inputs: vec![],
            }],
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "to-orch".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
        "forbidden",
    );
    exec(
        &mut s,
        Actor::System,
        Command::AdvanceOrchestratorGeneration { generation: 2 },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::Orchestrator { generation: 2 },
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "to-orch".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
    assert_eq!(
        s.interactions[&"to-orch".into()].scope,
        Scope::Task {
            task_id: "t".into()
        }
    );
    exec(
        &mut s,
        Actor::Orchestrator { generation: 2 },
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "coord".into(),
            scope: Scope::Orchestrator {
                request_id: Some("r".into()),
            },
            recipient: Recipient::User,
            body: "Coordinate work".into(),
            blocking: true,
            options: vec![QuestionOption {
                id: "ok".into(),
                label: "Okay".into(),
                inputs: vec![],
            }],
        },
    )
    .unwrap();
    assert_eq!(s.tasks.len(), 1);
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "coord".into(),
            option_id: "ok".into(),
            inputs: BTreeMap::new(),
        },
    )
    .unwrap();
}

#[test]
fn reassign_fences_old_agent_and_preserves_historical_author() {
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: text(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::RegisterAgent {
            agent_id: "b".into(),
            name: "Instance B".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AssignTask {
            task_id: "t".into(),
            agent_id: "b".into(),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "not_assigned",
    );
    assert_eq!(
        s.interactions[&"d".into()].author,
        Author::Agent {
            agent_id: "a".into()
        }
    );
    reject_unchanged(
        &mut s,
        Actor::Agent {
            agent_id: "b".into(),
        },
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
}

#[test]
fn review_off_does_not_authorize_and_external_merge_does_not_complete() {
    let mut s = fixture();
    s.tasks.get_mut(&"t".into()).unwrap().review_prs = false;
    register(&mut s);
    present(&mut s, "d", pr_targets());
    assert!(s.pull_requests[&"pr".into()].review.is_none());
    reject_unchanged(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: pr_targets(),
        },
        "acceptance_missing",
    );
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
}

#[test]
fn partial_delivery_and_unread_information_do_not_replace_final_or_block() {
    let mut s = fixture();
    present(&mut s, "d", text());
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: text(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "t".into(),
            interaction_id: "partial".into(),
            body: "Supplementary partial".into(),
            targets: text(),
            is_final: false,
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "info".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Unread news".into(),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].final_presentation, Some("d".into()));
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
}

#[test]
fn free_text_response_with_or_without_options_and_attachment_bounds() {
    let mut s = fixture();
    let attachment = AttachmentRef {
        id: "file-1".into(),
        name: "Review.pdf".into(),
        content_digest: "a".repeat(64),
        content_reference: "files/file-1".into(),
        bytes: 42,
    };
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![attachment.clone()],
            interaction_id: "q".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Review attachment".into(),
            blocking: true,
            options: vec![],
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![attachment.clone()],
            interaction_id: "q".into(),
            option_id: String::new(),
            inputs: BTreeMap::from([("text".into(), InputValue::Text("My free response".into()))]),
        },
    )
    .unwrap();
    assert_eq!(
        s.interactions[&"q".into()].attachments,
        vec![attachment.clone()]
    );
    let before = s.clone();
    let mut too_big = attachment;
    too_big.bytes = 64 * 1024 * 1024 + 1;
    assert_eq!(
        exec(
            &mut s,
            agent(),
            Command::PublishInformation {
                attachments: vec![too_big],
                interaction_id: "bad".into(),
                scope: Scope::Task {
                    task_id: "t".into()
                },
                recipient: Recipient::User,
                body: String::new()
            }
        )
        .unwrap_err()
        .code,
        "payload_too_large"
    );
    assert_eq!(s, before);
    exec(
        &mut s,
        agent(),
        Command::AskQuestion {
            attachments: vec![],
            interaction_id: "options".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Pick or explain".into(),
            blocking: false,
            options: vec![QuestionOption {
                id: "yes".into(),
                label: "Yes".into(),
                inputs: vec![],
            }],
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AnswerQuestion {
            attachments: vec![],
            interaction_id: "options".into(),
            option_id: String::new(),
            inputs: BTreeMap::from([(
                "text".into(),
                InputValue::Text("Something different".into()),
            )]),
        },
    )
    .unwrap();
}

#[test]
fn stack_readiness_is_explicit_and_merge_requires_upstream_merge_and_retarget() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    let stack = StackDependency {
        upstream_task: "t".into(),
        presentation_id: "d".into(),
        pr_id: "pr".into(),
        head: "head1".into(),
    };
    exec(
        &mut s,
        user(),
        Command::CreateTask {
            task_id: "down".into(),
            request_id: "r".into(),
            title: "Downstream".into(),
            agent_id: Some("a".into()),
            review_prs: true,
            dependencies: BTreeSet::new(),
            stack: Some(stack),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::StartTask {
            task_id: "down".into(),
        },
        "stack_not_ready",
    );
    exec(
        &mut s,
        agent(),
        Command::PublishStackReadiness {
            task_id: "t".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            head: "head1".into(),
            ready: true,
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::StartTask {
            task_id: "down".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::RegisterPr {
            task_id: "down".into(),
            pr_id: "pr-down".into(),
            host: "github.com".into(),
            repository: "owner/repo".into(),
            number: 2,
            head: "head-down".into(),
            base: "upstream-branch".into(),
            allowed_methods: vec![MergeMethod::Squash],
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::PresentResult {
            attachments: vec![],
            task_id: "down".into(),
            interaction_id: "d-down".into(),
            body: "Downstream ready".into(),
            targets: vec![DeliveryTarget::PullRequest {
                pr_id: "pr-down".into(),
                head: "head-down".into(),
            }],
            is_final: true,
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth-down".into(),
            presentation_id: "d-down".into(),
            pr_id: "pr-down".into(),
            expected_head: "head-down".into(),
            method: MergeMethod::Squash,
        },
    )
    .unwrap();
    let claim = Command::ClaimMergeAuthorization {
        authorization_id: "auth-down".into(),
        remote_head: "head-down".into(),
        remote_base: "upstream-branch".into(),
        base_verified: true,
    };
    reject_unchanged(&mut s, agent(), claim.clone(), "stack_not_ready");
    authorize(&mut s);
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    reject_unchanged(&mut s, agent(), claim, "stack_not_ready");
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr-down".into(),
            head: "head-down".into(),
            base: "main".into(),
            state: PrState::Open,
            merged_head: None,
        },
    )
    .unwrap();
    assert!(
        s.merge_authorizations[&"auth-down".into()].valid,
        "retarget without content change preserves acceptance"
    );
    exec(
        &mut s,
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth-down".into(),
            remote_head: "head-down".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
    assert_ne!(
        s.tasks[&"t".into()].state,
        TaskState::Done,
        "readiness and merge do not complete upstream"
    );
}

#[test]
fn cancellation_during_merge_preserves_observation_without_completion() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    authorize(&mut s);
    exec(
        &mut s,
        agent(),
        Command::ClaimMergeAuthorization {
            authorization_id: "auth".into(),
            remote_head: "head1".into(),
            remote_base: "main".into(),
            base_verified: true,
        },
    )
    .unwrap();
    exec(
        &mut s,
        user(),
        Command::CancelTask {
            task_id: "t".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    assert_eq!(s.tasks[&"t".into()].state, TaskState::Cancelled);
    assert_eq!(
        s.merge_authorizations[&"auth".into()].attempt,
        MergeAttemptState::Confirmed
    );
    assert!(!s.merge_authorizations[&"auth".into()].valid);
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "task_not_active",
    );
}

#[test]
fn protocol_rejects_client_actor_and_unsafe_revisions_without_mutation() {
    let request = serde_json::json!({"protocol_version":2,"data_epoch":"epoch","command_id":"c","expected_revision":0,"actor":{"kind":"system"},"command":{"kind":"mark_read","interaction_id":"i"}});
    assert!(serde_json::from_value::<CommandEnvelope>(request).is_err());
    let mut s = fixture();
    let before = s.clone();
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: "stale".into(),
        expected_revision: s.revision - 1,
        command: Command::StartTask {
            task_id: "t".into(),
        },
    };
    assert_eq!(
        s.execute(&agent(), &envelope).unwrap_err().code,
        "stale_revision"
    );
    assert_eq!(s, before);
    s.revision = MAX_REVISION;
    let before = s.clone();
    let envelope = CommandEnvelope {
        protocol_version: 2,
        data_epoch: s.data_epoch.clone(),
        command_id: "overflow".into(),
        expected_revision: MAX_REVISION,
        command: Command::StartTask {
            task_id: "t".into(),
        },
    };
    assert_eq!(
        s.execute(&agent(), &envelope).unwrap_err().code,
        "invalid_input"
    );
    assert_eq!(s, before);
}

#[test]
fn information_read_is_shared_across_authorized_devices() {
    let mut s = fixture();
    exec(
        &mut s,
        agent(),
        Command::PublishInformation {
            attachments: vec![],
            interaction_id: "i".into(),
            scope: Scope::Task {
                task_id: "t".into(),
            },
            recipient: Recipient::User,
            body: "Read once".into(),
        },
    )
    .unwrap();
    exec(
        &mut s,
        Actor::User {
            user_id: "device-1".into(),
        },
        Command::MarkRead {
            interaction_id: "i".into(),
        },
    )
    .unwrap();
    let revision = s.revision;
    let out = exec(
        &mut s,
        Actor::User {
            user_id: "device-2".into(),
        },
        Command::MarkRead {
            interaction_id: "i".into(),
        },
    )
    .unwrap();
    assert!(!out.changed);
    assert_eq!(s.revision, revision);
    let item = &s.projection().inbox[0];
    assert!(!item.pending);
    assert!(!item.available_actions[0].enabled);
}

#[test]
fn one_authorization_accepts_exact_joint_pr_text_and_document_delivery() {
    let mut s = fixture();
    register(&mut s);
    exec(
        &mut s,
        agent(),
        Command::CreateDocument {
            task_id: "t".into(),
            document_id: "doc".into(),
            title: "Design".into(),
            revision_id: "v1".into(),
            content_digest: "a".repeat(64),
            content_reference: "files/design".into(),
            role: "result".into(),
        },
    )
    .unwrap();
    let targets = vec![
        DeliveryTarget::Text {
            text: "Summary".into(),
        },
        DeliveryTarget::DocumentRevision {
            document_id: "doc".into(),
            revision_id: "v1".into(),
        },
        DeliveryTarget::PullRequest {
            pr_id: "pr".into(),
            head: "head1".into(),
        },
    ];
    present(&mut s, "d", targets);
    authorize(&mut s);
    assert!(
        matches!(&s.interactions[&"d".into()].payload,InteractionPayload::Delivery{acceptance:Some(a),..} if a.valid)
    );
    assert_eq!(s.document_reviews[0].decisions.len(), 1);
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "merge_not_confirmed",
    );
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
}

#[test]
fn external_merge_requires_explicit_delivery_acceptance_not_retroactive_authorization() {
    let mut s = fixture();
    register(&mut s);
    present(&mut s, "d", pr_targets());
    exec(
        &mut s,
        Actor::System,
        Command::ObservePrState {
            pr_id: "pr".into(),
            head: "head1".into(),
            base: "main".into(),
            state: PrState::Merged,
            merged_head: Some("head1".into()),
        },
    )
    .unwrap();
    reject_unchanged(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
        "acceptance_missing",
    );
    reject_unchanged(
        &mut s,
        user(),
        Command::AuthorizeMerge {
            authorization_id: "auth".into(),
            presentation_id: "d".into(),
            pr_id: "pr".into(),
            expected_head: "head1".into(),
            method: MergeMethod::Squash,
        },
        "invalid_action_for_kind",
    );
    exec(
        &mut s,
        user(),
        Command::AcceptDelivery {
            presentation_id: "d".into(),
            targets: pr_targets(),
        },
    )
    .unwrap();
    assert!(s.merge_authorizations.is_empty());
    exec(
        &mut s,
        agent(),
        Command::CompleteTask {
            task_id: "t".into(),
            presentation_id: "d".into(),
        },
    )
    .unwrap();
}
