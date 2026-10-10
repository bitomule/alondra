//! Executable C/S acceptance cases from the shared domain v2 catalog.
//!
//! Each case builds its arrangement through real commands in a fresh SQLite
//! store, executes the catalog's wire commands and the observation
//! environment steps a trusted adapter would send, and reads both state and
//! durable effect tables. Unsupported catalog operations are an error, never
//! a pass; assertions that only a client can observe are skipped explicitly.
use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

const CATALOG: &str = include_str!("../../../../fixtures/domain-v2/acceptance.json");
const CASES: &[&str] = &[
    "V01", "V03", "V06", "V07", "V08", "V09", "V10", "V11", "V16", "V17", "V18", "V20", "V21",
    "V23", "V24", "V25", "V45", "V50", "V52",
];
const SCHEDULE_CASES: &[&str] = &["V54", "V55", "V56", "V57", "V58", "V62"];
fn expected_rejection(case_id: &str, step: usize) -> Option<&'static str> {
    match (case_id, step) {
        ("V07", 1) => Some("acceptance_missing"),
        ("V10", 0) => Some("acceptance_stale"),
        ("V18", 1) => Some("acceptance_stale"),
        ("V21", 1) => Some("acceptance_missing"),
        ("V23", 0) => Some("forbidden"),
        ("V24", 0) => Some("agent_busy"),
        ("V45", 1) => Some("acceptance_missing"),
        _ => None,
    }
}
/// A client-only observation the store layer cannot measure.
fn client_assertion(path: &str) -> bool {
    path.starts_with("client.") || path.starts_with("clients.") || path.starts_with("ui.")
}
/// The catalog writes `sha256-<content>` for synthetic digests; the wire needs
/// the real digest of that content.
fn realize_digests(value: &mut Value) {
    if let Some(alias) = value
        .get("content_digest")
        .and_then(Value::as_str)
        .and_then(|digest| digest.strip_prefix("sha256-"))
    {
        let digest = Sha256::digest(alias.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        value["content_digest"] = json!(digest);
    }
}
fn observation(operation: &str, parameters: &Value, state: &DomainState) -> Command {
    let pr_id = PrId::from(string(field(parameters, "pr_id")));
    let head = string(field(parameters, "head")).to_owned();
    let base = state.pull_requests[&pr_id].base.clone();
    match operation {
        "observe_new_head" => Command::ObservePrState {
            pr_id,
            head,
            base,
            state: PrState::Open,
            merged_head: None,
        },
        "observe_merge" => Command::ObservePrState {
            pr_id,
            head: head.clone(),
            base,
            state: PrState::Merged,
            merged_head: Some(head),
        },
        other => panic!("unsupported environment operation {other}"),
    }
}

struct TemporaryStore(crate::test_support::Directory);
impl TemporaryStore {
    fn new() -> Self {
        let path = crate::test_support::Directory::new();
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn database(&self) -> std::path::PathBuf {
        self.0.join("domain.db")
    }
}
impl Drop for TemporaryStore {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

fn field<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .get(key)
        .unwrap_or_else(|| panic!("catalog field {key} missing in {value}"))
}
fn string(value: &Value) -> &str {
    value
        .as_str()
        .unwrap_or_else(|| panic!("expected string: {value}"))
}
fn variant_field<'a>(variants: &'a Value, name: &str, key: &str) -> &'a Value {
    let variant = field(variants, name);
    variant.get(key).unwrap_or_else(|| {
        let parent = string(field(variant, "extends"));
        variant_field(variants, parent, key)
    })
}
fn variant_targets(store: &mut DomainStore, variants: &Value, name: &str) -> Vec<DeliveryTarget> {
    let state = store.load().unwrap();
    field(variants, name)
        .get("targets")
        .unwrap_or_else(|| variant_field(variants, name, "targets"))
        .as_array()
        .unwrap()
        .iter()
        .map(|target| match string(field(target, "kind")) {
            "text" => DeliveryTarget::Text {
                text: string(field(target, "content")).into(),
            },
            "document_revision" => {
                let revision_id = string(field(target, "revision_id"));
                let revision = &state.document_revisions[&DocumentRevisionId::from(revision_id)];
                DeliveryTarget::DocumentRevision {
                    document_id: revision.document_id.clone(),
                    revision_id: revision.id.clone(),
                }
            }
            "pull_request" => DeliveryTarget::PullRequest {
                pr_id: string(field(target, "pr_id")).into(),
                head: string(field(target, "head")).into(),
            },
            other => panic!("unsupported semantic variant target {other}"),
        })
        .collect()
}
fn actor(name: &str) -> Actor {
    match name {
        "user-a" => Actor::User {
            user_id: name.into(),
        },
        "agent-a" | "agent-b" => Actor::Agent {
            agent_id: name.into(),
        },
        "system" => Actor::System,
        _ => panic!("unsupported catalog principal: {name}"),
    }
}
fn run(
    store: &mut DomainStore,
    id: &str,
    actor: Actor,
    command: Command,
) -> Result<Commit, StoreError> {
    let state = store.load().unwrap();
    let envelope = CommandEnvelope {
        protocol_version: PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: id.into(),
        expected_revision: state.revision,
        command,
    };
    store.execute_authenticated(&actor, &envelope, None)
}
fn setup_command(store: &mut DomainStore, id: &str, actor: Actor, command: Command) {
    run(store, id, actor, command).unwrap_or_else(|error| panic!("setup {id}: {error}"));
}
fn count(store: &DomainStore, table: &str) -> i64 {
    store
        .conn
        .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}
fn row_counts(store: &DomainStore) -> Vec<(String, i64)> {
    let mut tables = store.conn.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap();
    let names: Vec<String> = tables
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    names
        .into_iter()
        .map(|table| {
            let rows = count(store, &table);
            (table, rows)
        })
        .collect()
}
fn setup_base(store: &mut DomainStore, base: &Value) {
    for request in field(base, "requests").as_array().unwrap() {
        let id = string(field(request, "id"));
        setup_command(
            store,
            &format!("setup-request-{id}"),
            actor("user-a"),
            Command::CreateRequest {
                request_id: id.into(),
                body: string(field(request, "content")).into(),
                auto_merge: false, maintenance: false, message: false,
            },
        );
    }
    for agent in field(base, "agents").as_array().unwrap() {
        let id = string(field(agent, "id"));
        setup_command(
            store,
            &format!("setup-agent-{id}"),
            Actor::System,
            Command::RegisterAgent {
                agent_id: id.into(),
                name: id.into(), at: 0
            },
        );
        if field(agent, "phase") == "working" {
            setup_command(
                store,
                &format!("setup-working-{id}"),
                Actor::System,
                Command::MoveNode { agent_id: id.into(), event: NodeEvent::TurnStarted, at: 0 },
            );
        }
    }
    for task in field(base, "tasks").as_array().unwrap() {
        let id = string(field(task, "id"));
        let agent_id = field(task, "agent_id").as_str().map(AgentId::from);
        let dependencies = field(task, "dependencies")
            .as_array()
            .unwrap()
            .iter()
            .map(|value| TaskId::from(string(value)))
            .collect::<BTreeSet<_>>();
        let request_id: RequestId = string(field(task, "request_id")).into();
        let loaded = store.load().unwrap();
        if loaded.requests.get(&request_id).is_some_and(|request| request.title.is_none())
            && loaded.tasks.values().any(|other| other.request_id == request_id)
        {
            setup_command(
                store,
                &format!("setup-title-{request_id}"),
                actor("user-a"),
                Command::TitleRequest { request_id: request_id.clone(), title: "Setup".into() },
            );
        }
        setup_command(
            store,
            &format!("setup-task-{id}"),
            Actor::Orchestrator { generation: 0 },
            Command::CreateTask {
                task_id: id.into(),
                request_id: string(field(task, "request_id")).into(),
                title: id.into(),
                agent_id,
                review_prs: field(task, "review_prs") == true,
                dependencies,
                stack: None,
                launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
            },
        );
        if field(task, "status") == "running" {
            setup_command(
                store,
                &format!("setup-start-{id}"),
                actor("user-a"),
                Command::StartTask { task_id: id.into() },
            );
        } else {
            panic!("unsupported base Task status: {}", field(task, "status"));
        }
    }
    for interaction in field(base, "interactions").as_array().unwrap() {
        let id = string(field(interaction, "id"));
        let scope = serde_json::from_value::<Scope>(field(interaction, "scope").clone()).unwrap();
        let recipient = match string(field(field(interaction, "recipient"), "kind")) {
            "user" => Recipient::User,
            other => panic!("unsupported base recipient: {other}"),
        };
        let author = actor(string(field(field(interaction, "author"), "agent_id")));
        let body = string(field(interaction, "content")).into();
        let command = match string(field(interaction, "kind")) {
            "information" => Command::PublishInformation {
                attachments: vec![],
                interaction_id: id.into(),
                scope,
                recipient,
                body, card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
            },
            "question" => {
                let options = field(interaction, "options")
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|option| {
                        assert!(
                            field(option, "inputs").as_array().unwrap().is_empty(),
                            "base input adapter needs extension"
                        );
                        QuestionOption {
                            id: string(field(option, "option_id")).into(),
                            label: string(field(option, "label")).into(),
                            description: option
                                .get("description")
                                .and_then(|d| d.as_str())
                                .map(String::from),
                            inputs: vec![],
                        }
                    })
                    .collect();
                Command::AskQuestion {
                    attachments: vec![],
                    interaction_id: id.into(),
                    scope,
                    recipient,
                    body,
                    blocking: field(interaction, "blocking") == true,
                    options, card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question"),
                }
            }
            other => panic!("unsupported base Interaction: {other}"),
        };
        let author = if matches!(command, Command::PublishInformation { .. }) { Actor::System } else { author };
        setup_command(store, &format!("setup-interaction-{id}"), author, command);
    }
    assert!(field(base, "documents").as_array().unwrap().is_empty());
    assert!(field(base, "pull_requests").as_array().unwrap().is_empty());
}
/// A node delivers only once the user answered its task's questions: the variant names the ones
/// answered before it delivers.
fn answer_first(store: &mut DomainStore, variant: &Value, name: &str) {
    for question in variant.get("answered_first").and_then(Value::as_array).into_iter().flatten() {
        setup_command(store, &format!("setup-answer-{name}-{}", string(question)), actor("user-a"), Command::AnswerQuestion {
            attachments: vec![], interaction_id: string(question).into(), option_id: "yes".into(), inputs: Default::default(),
        });
    }
}
fn setup_variant(store: &mut DomainStore, variants: &Value, name: &str) {
    let variant = field(variants, name);
    answer_first(store, variant, name);
    if name == "pr-final" {
        assert_eq!(field(variant, "accepted"), false);
        let pr = field(variant, "pr");
        let task_id = string(field(variant, "task_id"));
        setup_command(
            store,
            &format!("setup-register-{name}"),
            actor("agent-a"),
            Command::RegisterPr {
                task_id: task_id.into(),
                pr_id: string(field(pr, "id")).into(),
                host: string(field(pr, "host")).into(),
                repository: string(field(pr, "repository")).into(),
                number: field(pr, "number").as_u64().unwrap(),
                head: string(field(pr, "head")).into(),
                base: "main".into(),
                allowed_methods: vec![MergeMethod::Squash],
            },
        );
        let targets = variant_targets(store, variants, name);
        setup_command(
            store,
            &format!("setup-present-{name}"),
            actor("agent-a"),
            Command::PresentResult {
                attachments: vec![],
                task_id: task_id.into(),
                interaction_id: string(field(variant, "presentation_id")).into(),
                body: "Synthetic pull request".into(),
                targets,
                is_final: field(variant, "final") == true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
            },
        );
    } else if name == "authorized-pr" {
        setup_variant(store, variants, string(field(variant, "extends")));
        let authorization = field(variant, "authorization");
        setup_command(
            store,
            &format!("setup-authorize-{name}"),
            actor(string(field(authorization, "actor"))),
            Command::AuthorizeMerge {
                authorization_id: string(field(authorization, "id")).into(),
                presentation_id: string(field(authorization, "presentation_id")).into(),
                pr_id: string(field(authorization, "pr_id")).into(),
                expected_head: string(field(authorization, "head")).into(),
                method: serde_json::from_value(field(authorization, "method").clone()).unwrap(),
            },
        );
    } else if let Some(parent) = variant
        .get("extends")
        .filter(|_| name != "mixed-text-document-final")
    {
        setup_variant(store, variants, string(parent));
        if field(variant, "accepted") == true {
            let targets = variant_targets(store, variants, name);
            setup_command(
                store,
                &format!("setup-accept-{name}"),
                actor("user-a"),
                Command::AcceptDelivery {
                    presentation_id: string(variant_field(variants, name, "presentation_id"))
                        .into(),
                    targets,
                },
            );
        } else {
            panic!("unsupported variant extension {name}");
        }
    } else if matches!(name, "document-v1" | "mixed-text-document-final") {
        assert_eq!(variant_field(variants, name, "accepted"), false);
        let document = variant_field(variants, name, "document");
        let revision = variant_field(variants, name, "revision");
        let task_id = string(variant_field(variants, name, "task_id"));
        let content = string(field(revision, "content"));
        assert_eq!(
            string(field(revision, "content_hash")),
            format!("sha256-{content}"),
            "{name}: fixture digest alias"
        );
        let digest = Sha256::digest(content.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        setup_command(
            store,
            &format!("setup-document-{name}"),
            actor("agent-a"),
            Command::CreateDocument {
                task_id: task_id.into(),
                document_id: string(field(document, "id")).into(),
                title: "Synthetic document".into(),
                revision_id: string(field(revision, "id")).into(),
                content_digest: digest.clone(),
                content_reference: string(field(document, "url")).into(),
                role: "result".into(),
            },
        );
        let saved = store.load().unwrap();
        let saved_revision =
            &saved.document_revisions[&DocumentRevisionId::from(string(field(revision, "id")))];
        assert_eq!(saved_revision.content_digest, digest);
        assert_eq!(
            saved_revision.content_reference,
            string(field(document, "url"))
        );
        let targets = variant_targets(store, variants, name);
        setup_command(
            store,
            &format!("setup-present-{name}"),
            actor("agent-a"),
            Command::PresentResult {
                attachments: vec![],
                task_id: task_id.into(),
                interaction_id: string(variant_field(variants, name, "presentation_id")).into(),
                body: if name == "document-v1" {
                    "Synthetic document"
                } else {
                    "Synthetic answer"
                }
                .into(),
                targets,
                is_final: variant_field(variants, name, "final") == true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
            },
        );
    } else if name == "unaccepted-text-final" {
        assert_eq!(field(variant, "accepted"), false);
        let targets = variant_targets(store, variants, name);
        setup_command(
            store,
            &format!("setup-present-{name}"),
            actor("agent-a"),
            Command::PresentResult {
                attachments: vec![],
                task_id: string(field(variant, "task_id")).into(),
                interaction_id: string(field(variant, "presentation_id")).into(),
                body: "Synthetic answer".into(),
                targets,
                is_final: field(variant, "final") == true, nothing_to_review: false, card: alondra_core::domain::CardSpec::delivery("Review the result", &[], true),
            },
        );
    } else {
        panic!("unsupported catalog variant: {name}");
    }
    let state = store.load().unwrap();
    let task_id = variant_field(variants, name, "task_id");
    let task = &state.tasks[&TaskId::from(string(task_id))];
    assert_eq!(
        serde_json::to_value(task.state).unwrap(),
        variant_field(variants, name, "task_status").clone(),
        "variant {name} status"
    );
}
struct Baseline {
    messages: i64,
    outbox: i64,
    attempts: i64,
    tasks: i64,
    state: std::sync::Arc<DomainState>,
}
fn baseline(store: &mut DomainStore) -> Baseline {
    Baseline {
        messages: count(store, "domain_messages"),
        outbox: count(store, "domain_outbox"),
        attempts: count(store, "delivery_attempts"),
        tasks: count(store, "tasks"),
        state: store.load().unwrap(),
    }
}
fn deliveries(state: &DomainState) -> (usize, usize) {
    let presentations = state
        .interactions
        .values()
        .filter(|i| matches!(i.payload, InteractionPayload::Delivery { .. }))
        .count();
    let acceptances = state
        .interactions
        .values()
        .filter(|i| {
            matches!(
                i.payload,
                InteractionPayload::Delivery {
                    acceptance: Some(_),
                    ..
                }
            )
        })
        .count();
    (presentations, acceptances)
}
fn subject_id(subject: &InteractionSubject) -> &str {
    match subject {
        InteractionSubject::PullRequest { pr_id } => &pr_id.0,
        InteractionSubject::DocumentReview { document_id, .. } => &document_id.0,
    }
}
fn measured(
    store: &mut DomainStore,
    before: &Baseline,
    last_error: Option<&str>,
    rejected_unchanged: bool,
    touched: &BTreeSet<InteractionId>,
) -> Value {
    let state = store.load().unwrap();
    let mut tasks = serde_json::Map::new();
    for (id, task) in &state.tasks {
        tasks.insert(
            id.0.clone(),
            json!({"status":task.state,"current_final_id":task.final_presentation,"agent_id":task.agent_id}),
        );
    }
    let mut interactions = serde_json::Map::new();
    let mut relinked = 0;
    let mut closed = 0;
    for (id, interaction) in &state.interactions {
        let subject = interaction
            .subject
            .as_ref()
            .map(|s| json!({"id": subject_id(s)}));
        let earlier = before.state.interactions.get(id);
        match &interaction.payload {
            InteractionPayload::Information { read_by } => {
                interactions.insert(id.0.clone(), json!({"read_by": read_by}));
            }
            InteractionPayload::Question { answer, .. } => {
                interactions.insert(
                    id.0.clone(),
                    json!({"answer":answer,"scope":interaction.scope,
                    "recipient":interaction.recipient,"subject":subject}),
                );
                if !touched.contains(id) {
                    if earlier.is_some_and(|e| e.subject != interaction.subject) {
                        relinked += 1;
                    }
                    if answer.is_some()
                        && earlier.is_some_and(|e| {
                            matches!(e.payload, InteractionPayload::Question { answer: None, .. })
                        })
                    {
                        closed += 1;
                    }
                }
            }
            _ => {}
        }
    }
    let agents: serde_json::Map<String, Value> = state
        .agents
        .iter()
        .map(|(id, agent)| (id.0.clone(), json!({"phase":agent.phase.key()})))
        .collect();
    let mut presentations = serde_json::Map::new();
    for (id, interaction) in &state.interactions {
        if let InteractionPayload::Delivery { acceptance, .. } = &interaction.payload {
            presentations.insert(
                id.0.clone(),
                json!({
                    "acceptance_recorded":acceptance.is_some(),
                    "acceptance_valid":acceptance.as_ref().is_some_and(|a|a.valid)
                }),
            );
        }
    }
    let mut documents: serde_json::Map<String, Value> = state
        .documents
        .iter()
        .map(|(id, document)| {
            let revision = &state.document_revisions[&document.current_revision];
            (id.0.clone(), json!({"url":revision.content_reference}))
        })
        .collect();
    documents.insert("count".into(), json!(state.documents.len()));
    // The catalog names reviews `doc-review-<n>` in creation order; the domain
    // keys them by (task, document).
    let document_reviews: serde_json::Map<String, Value> = state
        .document_reviews
        .iter()
        .enumerate()
        .map(|(index, review)| {
            let accepted = review
                .decisions
                .iter()
                .rev()
                .find(|decision| decision.accepted)
                .map(|decision| decision.revision_id.clone());
            (
                format!("doc-review-{}", index + 1),
                json!({"accepted_revision_id": accepted}),
            )
        })
        .collect();
    let mut pull_requests: serde_json::Map<String, Value> = state
        .pull_requests
        .iter()
        .map(|(id, pr)| (id.0.clone(), json!({"state": pr.state})))
        .collect();
    pull_requests.insert("count".into(), json!(state.pull_requests.len()));
    let feedback: usize = state
        .interactions
        .values()
        .map(|i| match &i.payload {
            InteractionPayload::Delivery { feedback, .. } => feedback.len(),
            _ => 0,
        })
        .sum();
    let (presentations_before, acceptances_before) = deliveries(&before.state);
    let (presentations_after, acceptances_after) = deliveries(&state);
    let messages = count(store, "domain_messages");
    let outbox = count(store, "domain_outbox");
    let attempts = count(store, "delivery_attempts");
    let handed: i64 = store
        .conn
        .query_row(
            "SELECT count(*) FROM delivery_attempts WHERE state='handed'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    json!({
        "tasks":tasks, "agents":agents, "interactions":interactions, "presentations":presentations,
        "documents":documents, "document_reviews":document_reviews, "pull_requests":pull_requests,
        "task_pr_links":{"count":state.tasks.values().filter(|t| t.pr_id.is_some()).count()},
        "pull_request_reviews":{"count":state.pull_requests.values().filter(|p| p.review.is_some()).count()},
        "authorizations":{"count":state.merge_authorizations.len()},
        "feedback":{"history_count":feedback},
        "unrelated_questions_relinked":relinked,
        "unrelated_questions_closed":closed,
        "delta":{"messages":messages-before.messages,"outbox":outbox-before.outbox,
            "delivery_attempts":attempts-before.attempts,"tasks":count(store,"tasks")-before.tasks,
            "presentations":presentations_after as i64-presentations_before as i64,
            "user_acceptances":acceptances_after as i64-acceptances_before as i64,
            // The store never calls GitHub; a merge is an Agent effect after a claim.
            "external_merge_calls":0},
        "delivery":{"execution_confirmed":handed>0},
        "last_error":{"code":last_error},
        "last_rejected":{"delta":{"rows":if rejected_unchanged {0} else {1},
            "outbox":if rejected_unchanged {0} else {1}}},
    })
}

#[test]
fn shared_catalog_domain_and_sql_cases() {
    let catalog: Value = serde_json::from_str(CATALOG).unwrap();
    assert_eq!(field(&catalog, "contract_version"), 2);
    let scenarios = field(&catalog, "scenarios").as_array().unwrap();
    for case_id in CASES {
        let case = scenarios
            .iter()
            .find(|case| field(case, "id") == case_id)
            .unwrap();
        let temporary = TemporaryStore::new();
        let mut store = DomainStore::open(&temporary.database()).unwrap();
        setup_base(&mut store, field(&catalog, "base"));
        for arrangement in field(case, "arrange").as_array().unwrap() {
            if let Some(name) = arrangement.get("fixture") {
                setup_variant(&mut store, field(&catalog, "variants"), string(name));
            } else if field(arrangement, "set") == "interactions.question-1.blocking" {
                assert_eq!(field(arrangement, "value"), false);
                assert!(
                    !store.load().unwrap().interactions[&InteractionId::from("question-1")]
                        .blocking
                );
            } else {
                panic!("{case_id}: unsupported arrangement {arrangement}");
            }
        }
        let before = baseline(&mut store);
        let mut last_error = None;
        let mut rejected_unchanged = false;
        let mut touched = BTreeSet::new();
        for (index, step) in field(case, "steps").as_array().unwrap().iter().enumerate() {
            let (actor, command) = match string(field(step, "type")) {
                "command" => {
                    let mut wire = field(step, "command").clone();
                    realize_digests(&mut wire);
                    alondra_core::domain::derive_wire_card(wire.as_object_mut().unwrap());
                    let command: Command = serde_json::from_value(wire).unwrap();
                    match &command {
                        Command::LinkInteractionSubject { interaction_id, .. }
                        | Command::AnswerQuestion { interaction_id, .. } => {
                            touched.insert(interaction_id.clone());
                        }
                        _ => {}
                    }
                    (actor(string(field(step, "actor"))), command)
                }
                "environment" => {
                    let state = store.load().unwrap();
                    (
                        Actor::System,
                        observation(
                            string(field(step, "operation")),
                            field(step, "parameters"),
                            &state,
                        ),
                    )
                }
                other => panic!("{case_id}: unsupported step type {other}"),
            };
            let before_state = store.load().unwrap();
            let before_rows = row_counts(&store);
            match run(
                &mut store,
                &format!("{case_id}-step-{index}"),
                actor,
                command,
            ) {
                Ok(commit) => {
                    assert!(
                        expected_rejection(case_id, index).is_none(),
                        "{case_id}: step {index} unexpectedly succeeded"
                    );
                    assert_eq!(
                        commit.state,
                        store.load().unwrap(),
                        "{case_id}: SQL roundtrip"
                    );
                    last_error = None;
                }
                Err(StoreError::Domain(error)) => {
                    assert_eq!(
                        Some(error.code.as_str()),
                        expected_rejection(case_id, index),
                        "{case_id}: step {index} error"
                    );
                    last_error = Some(error.code);
                    rejected_unchanged =
                        store.load().unwrap() == before_state && row_counts(&store) == before_rows;
                    assert!(
                        rejected_unchanged,
                        "{case_id}: rejected command mutated SQL"
                    );
                }
                Err(error) => panic!("{case_id}: unexpected storage error: {error}"),
            }
        }
        let actual = measured(
            &mut store,
            &before,
            last_error.as_deref(),
            rejected_unchanged,
            &touched,
        );
        let layers = field(case, "layers").as_array().unwrap();
        for assertion in field(case, "assert").as_array().unwrap() {
            assert_eq!(
                field(assertion, "op"),
                "eq",
                "{case_id}: unsupported assertion {assertion}"
            );
            let path = string(field(assertion, "path"));
            if client_assertion(path) {
                assert!(
                    layers.iter().any(|layer| layer == "U"),
                    "{case_id}: {path} needs a U layer"
                );
                continue;
            }
            let found = path.split('.').fold(&actual, |node, key| {
                node.get(key)
                    .unwrap_or_else(|| panic!("{case_id}: unmeasured assertion {path} in {actual}"))
            });
            assert_eq!(found, field(assertion, "value"), "{case_id}: {path}");
        }
        store.integrity_check().unwrap();
        let original = store.load().unwrap();
        drop(store);
        let mut restarted = DomainStore::open(&temporary.database()).unwrap();
        assert_eq!(
            restarted.load().unwrap(),
            original,
            "{case_id}: restart roundtrip"
        );
        restarted.integrity_check().unwrap();
    }
}

/// Every catalog scenario states how it is executed, and the statement holds:
/// runner cases are exactly the listed ones, evidence names existing tests, and
/// pending scenarios say which layer they wait for.
#[test]
fn catalog_execution_status_is_explicit_and_evidenced() {
    let catalog: Value = serde_json::from_str(CATALOG).unwrap();
    let legend = field(&catalog, "execution_status_legend")
        .as_object()
        .unwrap();
    let sources = [
        include_str!("tests.rs"),
        include_str!("schedule_acceptance_tests.rs"),
        include_str!("../domain_cutover.rs"),
        include_str!("../domain_authority.rs"),
        include_str!("../domain_api.rs"),
        include_str!("../service/domain_parity_tests.rs"),
        include_str!("../../../core/src/domain/tests.rs"),
    ];
    let mut runner = BTreeSet::new();
    let mut schedule_runner = BTreeSet::new();
    for case in field(&catalog, "scenarios").as_array().unwrap() {
        let id = string(field(case, "id"));
        let status = string(field(case, "execution_status"));
        assert!(
            legend.contains_key(status),
            "{id}: status {status} is not in the legend"
        );
        match status {
            "store_runner" => {
                runner.insert(id);
            }
            "schedule_runner" => {
                schedule_runner.insert(id);
            }
            "test_evidence" => {
                let evidence = field(case, "cs_evidence").as_array().unwrap();
                assert!(
                    !evidence.is_empty(),
                    "{id}: test_evidence needs cs_evidence"
                );
                for name in evidence {
                    let function = string(name).rsplit("::").next().unwrap();
                    assert!(
                        sources
                            .iter()
                            .any(|source| source.contains(&format!("fn {function}("))),
                        "{id}: evidence {} names no existing test",
                        string(name)
                    );
                }
            }
            _ => {
                assert!(
                    case.get("cs_evidence")
                        .is_none_or(|e| e.as_array().unwrap().is_empty()),
                    "{id}: a pending scenario cannot also claim evidence"
                );
            }
        }
    }
    assert_eq!(runner, CASES.iter().copied().collect::<BTreeSet<_>>());
    assert_eq!(
        schedule_runner,
        SCHEDULE_CASES.iter().copied().collect::<BTreeSet<_>>()
    );
}
