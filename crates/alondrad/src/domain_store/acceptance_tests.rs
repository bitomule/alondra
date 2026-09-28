//! Executable C/S acceptance cases from the shared domain v2 catalog.
//!
//! Each case builds its arrangement through real commands in a fresh SQLite
//! store, executes the catalog's wire commands, and reads both state and durable
//! effect tables. Unsupported catalog operations are an error, never a pass.
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeSet;

const CATALOG: &str = include_str!("../../../../fixtures/domain-v2/acceptance.json");
const CASES: &[&str] = &["V01", "V03", "V06", "V07", "V23", "V24", "V25", "V45"];
fn expected_rejection(case_id: &str, step: usize) -> Option<&'static str> {
    match (case_id, step) {
        ("V07", 1) => Some("acceptance_missing"),
        ("V23", 0) => Some("not_assigned"),
        ("V45", 1) => Some("acceptance_stale"),
        _ => None,
    }
}

struct TemporaryStore(std::path::PathBuf);
impl TemporaryStore {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "alondra-v2-acceptance-{}",
            crate::auth::random_hex(16)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn database(&self) -> std::path::PathBuf {
        self.0.join("domain-v2.db")
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
fn variant_targets(variants: &Value, name: &str) -> Vec<DeliveryTarget> {
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
        data_epoch: state.data_epoch,
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
                name: id.into(),
            },
        );
        if field(agent, "connected") == true {
            setup_command(
                store,
                &format!("setup-connected-{id}"),
                Actor::System,
                Command::ObserveAgent {
                    agent_id: id.into(),
                    connected: true,
                },
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
        setup_command(
            store,
            &format!("setup-task-{id}"),
            actor("user-a"),
            Command::CreateTask {
                task_id: id.into(),
                request_id: string(field(task, "request_id")).into(),
                title: id.into(),
                agent_id,
                review_prs: field(task, "review_prs") == true,
                dependencies,
                stack: None,
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
                body,
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
                    options,
                }
            }
            other => panic!("unsupported base Interaction: {other}"),
        };
        setup_command(store, &format!("setup-interaction-{id}"), author, command);
    }
    assert!(field(base, "documents").as_array().unwrap().is_empty());
    assert!(field(base, "pull_requests").as_array().unwrap().is_empty());
}
fn setup_variant(store: &mut DomainStore, variants: &Value, name: &str) {
    let variant = field(variants, name);
    if let Some(parent) = variant.get("extends") {
        setup_variant(store, variants, string(parent));
        if field(variant, "accepted") == true {
            let presentation = string(variant_field(variants, name, "presentation_id"));
            let targets = variant_targets(variants, name);
            setup_command(
                store,
                &format!("setup-accept-{name}"),
                actor("user-a"),
                Command::AcceptDelivery {
                    presentation_id: presentation.into(),
                    targets,
                },
            );
        } else {
            panic!("unsupported variant extension {name}");
        }
    } else if name == "unaccepted-text-final" {
        assert_eq!(field(variant, "accepted"), false);
        let targets = variant_targets(variants, name);
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
                is_final: field(variant, "final") == true,
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
        field(variant, "task_status").clone(),
        "variant {name} status"
    );
}
fn measured(
    store: &mut DomainStore,
    before: (i64, i64, i64),
    last_error: Option<&str>,
    rejected_unchanged: bool,
    tasks_before: i64,
) -> Value {
    let state = store.load().unwrap();
    let mut tasks = serde_json::Map::new();
    for (id, task) in &state.tasks {
        tasks.insert(id.0.clone(), json!({"status":task.state}));
    }
    let mut interactions = serde_json::Map::new();
    for (id, interaction) in &state.interactions {
        match &interaction.payload {
            InteractionPayload::Information { read_by } => {
                interactions.insert(id.0.clone(), json!({"read_by": read_by}));
            }
            InteractionPayload::Question { answer, .. } => {
                interactions.insert(id.0.clone(), json!({"answer":answer,"scope":interaction.scope,"recipient":interaction.recipient}));
            }
            _ => {}
        }
    }
    let agents: serde_json::Map<String, Value> = state
        .agents
        .iter()
        .map(|(id, agent)| (id.0.clone(), json!({"connected":agent.connected})))
        .collect();
    let mut presentations = serde_json::Map::new();
    for (id, interaction) in &state.interactions {
        if let InteractionPayload::Delivery { acceptance, .. } = &interaction.payload {
            presentations.insert(
                id.0.clone(),
                json!({"accepted":acceptance.as_ref().is_some_and(|a|a.valid)}),
            );
        }
    }
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
        "documents":{"count":state.documents.len()}, "pull_requests":{"count":state.pull_requests.len()},
        "delta":{"messages":messages-before.0,"outbox":outbox-before.1,"delivery_attempts":attempts-before.2,
            "tasks":count(store,"tasks")-tasks_before},
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
            } else if field(arrangement, "set") == "tasks.task-2.agent_id" {
                let agent_id = string(field(arrangement, "value"));
                setup_command(
                    &mut store,
                    &format!("{case_id}-arrange-assign"),
                    actor("user-a"),
                    Command::AssignTask {
                        task_id: "task-2".into(),
                        agent_id: agent_id.into(),
                    },
                );
            } else {
                panic!("{case_id}: unsupported arrangement {arrangement}");
            }
        }
        let before = (
            count(&store, "domain_messages"),
            count(&store, "domain_outbox"),
            count(&store, "delivery_attempts"),
        );
        let tasks_before = count(&store, "tasks");
        let mut last_error = None;
        let mut rejected_unchanged = false;
        for (index, step) in field(case, "steps").as_array().unwrap().iter().enumerate() {
            assert_eq!(
                field(step, "type"),
                "command",
                "{case_id}: unsupported step {step}"
            );
            let command: Command = serde_json::from_value(field(step, "command").clone()).unwrap();
            let before_state = store.load().unwrap();
            let before_rows = row_counts(&store);
            match run(
                &mut store,
                &format!("{case_id}-step-{index}"),
                actor(string(field(step, "actor"))),
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
            before,
            last_error.as_deref(),
            rejected_unchanged,
            tasks_before,
        );
        for assertion in field(case, "assert").as_array().unwrap() {
            assert_eq!(
                field(assertion, "op"),
                "eq",
                "{case_id}: unsupported assertion {assertion}"
            );
            let path = string(field(assertion, "path"));
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
