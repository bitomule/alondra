#[path = "support/controller.rs"]
mod fixture_controller;
use fixture_controller::WorkerController;
use alondra_agent_application::{
    DurableAdapter, InputMetadata, Normalization, ObservationMetadata,
    ObservationNormalizer, stage_observation,
};
use alondra_domain::{ActivityView, Event};
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId, TaskId, TurnId, WorkId};
use alondra_provider_contract::{
    Adapter, AdapterError, Capability, Handoff, Invocation, Observation, OrderedObservation,
    Placement, PreparedCommand, ProviderBinding,
};
use alondra_store::{
    AdapterState, AuthorityScope, InputPayload, LifecycleTransaction, ObservationAcceptance,
    OwnerGrant,
};
use rusqlite::{Connection, TransactionBehavior};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
#[path = "../../test_support.rs"]
mod test_support;

// A process-free service boundary with its own request and event vocabulary.
// Tests drive commands; none inject aggregate or normalized provider facts.
#[derive(Default)]
struct RemoteService {
    owners: BTreeMap<String, u64>,
    requests: Vec<Vec<u8>>,
    events: BTreeMap<String, VecDeque<Vec<u8>>>,
    acknowledged: BTreeMap<String, u64>,
    lose_one_ack: bool,
}
impl RemoteService {
    fn grant(&mut self, grant: &OwnerGrant) -> Result<(), AdapterError> {
        let owner = self
            .owners
            .entry(grant.execution().task().0.clone())
            .or_default();
        if grant.generation() <= *owner {
            return Err(AdapterError::InvalidObservation);
        }
        *owner = grant.generation();
        Ok(())
    }
    fn submit(&mut self, grant: &OwnerGrant, bytes: &[u8]) -> Result<Handoff, AdapterError> {
        if self.owners.get(&grant.execution().task().0) != Some(&grant.generation()) {
            return Ok(Handoff::RefusedBeforeOffer);
        }
        let request: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| AdapterError::InvalidObservation)?;
        if request["workspace"] != "service/workspace"
            || request["task"] != grant.execution().task().0
            || request["generation"] != grant.execution().generation()
        {
            return Err(AdapterError::InvalidObservation);
        }
        self.requests.push(bytes.to_vec());
        let task = &grant.execution().task().0;
        let events = self.events.entry(task.clone()).or_default();
        for (sequence, event) in [
            (1, serde_json::json!({"status":"workspace_ready"})),
            (
                2,
                serde_json::json!({"status":"prompt_running","native_input":request["native_input"],"turn":"remote/turn"}),
            ),
            (
                3,
                serde_json::json!({"status":"child_job_created","turn":"remote/turn","job":"remote/job-λ"}),
            ),
        ] {
            events.push_back(serde_json::to_vec(&serde_json::json!({"task":task,"generation":grant.execution().generation(),"sequence":sequence,"event":event})).unwrap());
        }
        Ok(Handoff::Offered)
    }
}
struct RemoteAdapter {
    binding: ProviderBinding,
    service: Arc<Mutex<RemoteService>>,
    owner: Option<OwnerGrant>,
    normalizer: RemoteNormalizer,
}
impl RemoteAdapter {
    fn new(service: Arc<Mutex<RemoteService>>) -> Self {
        Self {
            binding: ProviderBinding::new(
                "remote-binding".into(),
                "controlled-remote-service".into(),
                "remote-instance".into(),
                "remote-config".into(),
                1,
                1,
                BTreeSet::from([Capability::OfferInput, Capability::BackgroundWork]),
                "remote-credential-reference".into(),
                1,
            )
            .unwrap(),
            service,
            owner: None,
            normalizer: RemoteNormalizer,
        }
    }
}
fn metadata_error(error: alondra_store::Error) -> AdapterError {
    AdapterError::Storage(error.to_string())
}
impl DurableAdapter for RemoteAdapter {
    type Normalizer = RemoteNormalizer;
    fn normalizer(&self) -> &RemoteNormalizer {
        &self.normalizer
    }
    fn owner_generation(&self) -> Result<u64, AdapterError> {
        self.owner
            .as_ref()
            .map(OwnerGrant::generation)
            .ok_or(AdapterError::Disconnected)
    }
    fn stage_input(&mut self, mut metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        let conversation = serde_json::to_vec(&serde_json::json!([
            "remote-conversation",
            metadata.execution().task().0,
            metadata.execution().conversation().as_str()
        ]))
        .unwrap();
        let input = serde_json::to_vec(&serde_json::json!([
            "remote-input",
            metadata.execution().task().0,
            metadata.input_id().as_str()
        ]))
        .unwrap();
        metadata
            .put_conversation(conversation)
            .map_err(metadata_error)?;
        metadata.put_input(input).map_err(metadata_error)
    }
    fn load_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<Vec<u8>, AdapterError> {
        let expected = serde_json::to_vec(&serde_json::json!([
            "remote-conversation",
            metadata.execution().task().0,
            metadata.execution().conversation().as_str()
        ]))
        .unwrap();
        if metadata.conversation().map_err(metadata_error)?.as_deref() != Some(expected.as_slice()) {
            return Err(AdapterError::InvalidObservation);
        }
        metadata.input().map_err(metadata_error)?.ok_or(AdapterError::InvalidObservation)
    }
    fn install_owner(
        &mut self,
        grant: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        if state.conversation().is_empty() {
            return Err(AdapterError::InvalidObservation);
        }
        if grant.binding() != &self.binding {
            return Err(AdapterError::InvalidObservation);
        }
        self.service.lock().unwrap().grant(&grant)?;
        self.owner = Some(grant);
        Ok(())
    }
}
impl Adapter for RemoteAdapter {
    type NativeInput = Vec<u8>;
    fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    fn prepare(
        &mut self,
        invocation: Invocation<'_>,
        native: Vec<u8>,
    ) -> Result<PreparedCommand, AdapterError> {
        let Some(Placement::RemoteWorkspace(workspace)) = invocation.placement else {
            return Err(AdapterError::Unsupported);
        };
        let native = String::from_utf8(native).map_err(|_| AdapterError::InvalidObservation)?;
        PreparedCommand::new(serde_json::to_vec(&serde_json::json!({"task":invocation.execution.task().0,"generation":invocation.execution.generation(),"command":invocation.command.0,"workspace":workspace,"native_input":native,"prompt":invocation.text})).unwrap())
    }
    fn submit(
        &mut self,
        invocation: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError> {
        let owner = self.owner.as_ref().ok_or(AdapterError::Disconnected)?;
        if owner.execution() != invocation.execution || owner.binding() != invocation.binding {
            return Err(AdapterError::InvalidObservation);
        }
        self.service.lock().unwrap().submit(owner, command.bytes())
    }
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        let owner = self.owner.as_ref().ok_or(AdapterError::Disconnected)?;
        let Some(bytes) = self
            .service
            .lock()
            .unwrap()
            .events
            .entry(owner.execution().task().0.clone())
            .or_default()
            .pop_front()
        else {
            return Ok(None);
        };
        let event: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| AdapterError::InvalidObservation)?;
        if event["task"] != owner.execution().task().0
            || event["generation"] != owner.execution().generation()
        {
            return Err(AdapterError::InvalidObservation);
        }
        Ok(Some(OrderedObservation {
            owner_generation: owner.generation(),
            execution: owner.execution().clone(),
            cursor: event["sequence"]
                .as_u64()
                .ok_or(AdapterError::InvalidObservation)?,
            evidence: bytes,
        }))
    }
    fn acknowledge(
        &mut self,
        execution: &alondra_foundation::ExecutionStamp,
        cursor: u64,
    ) -> Result<(), AdapterError> {
        let mut service = self.service.lock().unwrap();
        if service.lose_one_ack {
            service.lose_one_ack = false;
            return Err(AdapterError::Disconnected);
        }
        service
            .acknowledged
            .insert(execution.task().0.clone(), cursor);
        Ok(())
    }
}
struct RemoteNormalizer;
impl ObservationNormalizer for RemoteNormalizer {
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> Result<Normalization, AdapterError> {
        let event: serde_json::Value = serde_json::from_slice(&batch.evidence)
            .map_err(|_| AdapterError::InvalidObservation)?;
        let fact = match event["event"]["status"].as_str() {
            Some("workspace_ready") => Observation::Available,
            Some("prompt_running") => {
                let key = event["event"]["native_input"]
                    .as_str()
                    .ok_or(AdapterError::InvalidObservation)?
                    .as_bytes()
                    .to_vec();
                let inputs = metadata.lookup_inputs(&[key]).map_err(metadata_error)?;
                Observation::TurnStarted {
                    input: inputs[0].input().clone(),
                    turn: TurnId::new(
                        event["event"]["turn"]
                            .as_str()
                            .ok_or(AdapterError::InvalidObservation)?,
                    )
                    .map_err(|_| AdapterError::InvalidObservation)?,
                }
            }
            Some("child_job_created") => Observation::WorkStarted {
                turn: TurnId::new(
                    event["event"]["turn"]
                        .as_str()
                        .ok_or(AdapterError::InvalidObservation)?,
                )
                .map_err(|_| AdapterError::InvalidObservation)?,
                work: WorkId::new(
                    event["event"]["job"]
                        .as_str()
                        .ok_or(AdapterError::InvalidObservation)?,
                )
                .map_err(|_| AdapterError::InvalidObservation)?,
            },
            _ => return Err(AdapterError::InvalidObservation),
        };
        Normalization::new(vec![fact], metadata.cursor().to_be_bytes().to_vec())
    }
}

fn database(directory: &test_support::Directory) -> Connection {
    std::fs::create_dir_all(&**directory).unwrap();
    let connection = Connection::open(directory.join("domain.db")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    connection
        .execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql"))
        .unwrap();
    connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL); INSERT INTO domain_meta VALUES(1,'remote-epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('request','Remote work',0); INSERT INTO agents(id,name) VALUES('node','Worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('task','request','Remote task','not_started',0,0,0,0)").unwrap();
    connection
}
fn launch(controller: &mut WorkerController<RemoteAdapter>) {
    controller
        .launch(
            &TaskId("task".into()),
            InputPayload::new(
                InputId::new("opaque/input-business").unwrap(),
                "Actual service prompt",
            )
            .unwrap(),
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("opaque/conversation").unwrap(),
                instance: InstanceId::new("remote/resource").unwrap(),
                host: HostId::new("remote-service-endpoint").unwrap(),
            },
            Placement::RemoteWorkspace("service/workspace".into()),
        )
        .unwrap();
}
#[test]
fn same_controller_drives_process_free_service_and_commits_before_ack() {
    let directory = test_support::Directory::new();
    let service = Arc::new(Mutex::new(RemoteService::default()));
    let mut controller = WorkerController::open(
        database(&directory),
        AuthorityScope::new("remote-epoch".into()).unwrap(),
        RemoteAdapter::new(service.clone()),
    )
    .unwrap();
    launch(&mut controller);
    let permit = controller.next_permit().unwrap();
    let attempt = controller.claim(permit).unwrap();
    assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
    assert_eq!(
        controller
            .load(&"task".into())
            .unwrap()
            .pending_inputs()
            .len(),
        1,
        "handoff is not business consumption"
    );
    assert!(controller.observe().unwrap());
    assert_eq!(
        controller
            .load(&"task".into())
            .unwrap()
            .pending_inputs()
            .len(),
        1,
        "resource availability is not consumption"
    );
    service.lock().unwrap().lose_one_ack = true;
    assert!(matches!(
        controller.observe(),
        Err(alondra_agent_application::Error::Adapter(
            AdapterError::Disconnected
        ))
    ));
    let after_commit = controller.load(&"task".into()).unwrap();
    assert!(after_commit.pending_inputs().is_empty());
    let revision = after_commit.revision();
    assert_eq!(service.lock().unwrap().acknowledged.get("task"), Some(&1));
    assert!(
        controller.observe().unwrap(),
        "exact duplicate evidence retries ACK without another fact"
    );
    assert_eq!(
        controller.load(&"task".into()).unwrap().revision(),
        revision
    );
    assert_eq!(service.lock().unwrap().acknowledged.get("task"), Some(&2));
    assert!(controller.observe().unwrap());
    let running = controller.load(&"task".into()).unwrap();
    assert_eq!(running.activity(), ActivityView::Working);
    assert!(running.holds_slot());
    assert_eq!(service.lock().unwrap().requests.len(), 1);
    let raw = Connection::open(directory.join("domain.db")).unwrap();
    let (commands,receipts):(i64,i64)=raw.query_row("SELECT (SELECT count(*) FROM provider_commands),(SELECT count(*) FROM provider_observations)",[],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
    assert_eq!(commands, 1);
    assert_eq!(receipts, 3);
    drop(raw);
    drop(controller);
}

#[test]
fn shared_business_input_ids_stay_task_scoped_through_remote_service_and_store() {
    let directory = test_support::Directory::new();
    let connection = database(&directory);
    connection.execute_batch("INSERT INTO agents(id,name) VALUES('second-node','Second worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('second-task','request','Second remote task','not_started',0,0,0,0)").unwrap();
    let second_connection = Connection::open(directory.join("domain.db")).unwrap();
    second_connection
        .execute_batch("PRAGMA foreign_keys=ON")
        .unwrap();
    let service = Arc::new(Mutex::new(RemoteService::default()));
    let mut first = WorkerController::open(
        connection,
        AuthorityScope::new("remote-epoch".into()).unwrap(),
        RemoteAdapter::new(service.clone()),
    )
    .unwrap();
    let mut second = WorkerController::open(
        second_connection,
        AuthorityScope::new("remote-epoch".into()).unwrap(),
        RemoteAdapter::new(service.clone()),
    )
    .unwrap();
    for (controller, task, node, prompt) in [
        (&mut first, "task", "node", "First task exact prompt"),
        (
            &mut second,
            "second-task",
            "second-node",
            "Second task different prompt",
        ),
    ] {
        controller
            .launch(
                &task.into(),
                InputPayload::new(InputId::new("shared/input").unwrap(), prompt).unwrap(),
                Event::Admit {
                    node: node.into(),
                    conversation: ConversationId::new(format!("conversation/{task}")).unwrap(),
                    instance: InstanceId::new(format!("remote/resource/{task}")).unwrap(),
                    host: HostId::new("remote-service-endpoint").unwrap(),
                },
                Placement::RemoteWorkspace("service/workspace".into()),
            )
            .unwrap();
        let permit = controller.next_permit().unwrap();
        let attempt = controller.claim(permit).unwrap();
        assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
        assert_eq!(
            controller
                .load(&task.into())
                .unwrap()
                .pending_inputs()
                .len(),
            1
        );
        assert!(controller.observe().unwrap());
        assert!(controller.observe().unwrap());
        assert!(
            controller
                .load(&task.into())
                .unwrap()
                .pending_inputs()
                .is_empty()
        );
    }
    let requests = service
        .lock()
        .unwrap()
        .requests
        .iter()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["task"], "task");
    assert_eq!(requests[0]["prompt"], "First task exact prompt");
    assert_eq!(requests[1]["task"], "second-task");
    assert_eq!(requests[1]["prompt"], "Second task different prompt");
    assert_ne!(requests[0]["native_input"], requests[1]["native_input"]);
    let connection = Connection::open(directory.join("domain.db")).unwrap();
    let mappings: i64 = connection
        .query_row(
            "SELECT count(*) FROM provider_input_metadata WHERE input_id='shared/input'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(mappings, 2);
    drop(connection);
    drop(first);
    drop(second);
}

enum Probe {
    Real,
    CatchesMetadataError,
    Refuses,
    Panics,
}
struct ProbeNormalizer(Probe);
impl ObservationNormalizer for ProbeNormalizer {
    fn normalize(
        &self,
        batch: &OrderedObservation,
        metadata: &ObservationMetadata<'_, '_>,
    ) -> Result<Normalization, AdapterError> {
        match self.0 {
            Probe::Real => RemoteNormalizer.normalize(batch, metadata),
            Probe::CatchesMetadataError => {
                assert!(matches!(
                    metadata.lookup_inputs(&vec![b"native".to_vec(); 65]),
                    Err(alondra_store::Error::MetadataCapacity)
                ));
                Normalization::new(vec![Observation::Available], b"checkpoint".to_vec())
            }
            Probe::Refuses => Err(AdapterError::InvalidObservation),
            Probe::Panics => panic!("normalizer panicked after the raw write"),
        }
    }
}
fn scope() -> AuthorityScope {
    AuthorityScope::new("remote-epoch".into()).unwrap()
}
fn raw_observations(directory: &test_support::Directory) -> i64 {
    Connection::open(directory.join("domain.db"))
        .unwrap()
        .query_row(
            "SELECT (SELECT count(*) FROM provider_observations)+(SELECT count(*) FROM provider_observation_heads)",
            [],
            |row| row.get(0),
        )
        .unwrap()
}
/// Stages one valid raw observation through the probe, catching whatever it does, then commits.
fn stage_and_commit(probe: Probe) -> (bool, alondra_store::Result<()>, i64) {
    let directory = test_support::Directory::new();
    let mut controller = WorkerController::open(
        database(&directory),
        scope(),
        RemoteAdapter::new(Arc::new(Mutex::new(RemoteService::default()))),
    )
    .unwrap();
    launch(&mut controller);
    let mut connection = Connection::open(directory.join("domain.db")).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    let raw = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let mut transaction = LifecycleTransaction::attach(raw, scope()).unwrap();
    let execution = transaction
        .load(&"task".into())
        .unwrap()
        .execution()
        .unwrap()
        .clone();
    let binding = transaction.execution_binding(&execution).unwrap();
    let batch = OrderedObservation {
        owner_generation: 1,
        execution,
        cursor: 1,
        evidence: br#"{"event":{"status":"workspace_ready"}}"#.to_vec(),
    };
    let staged = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        stage_observation(&mut transaction, &binding, 1, &batch, &ProbeNormalizer(probe))
    }));
    let accepted = matches!(staged, Ok(Ok(ObservationAcceptance::Accepted)));
    let committed = transaction.commit().map(drop);
    drop(connection);
    let rows = raw_observations(&directory);
    drop(controller);
    (accepted, committed, rows)
}
#[test]
fn real_normalizer_commits_the_staged_observation() {
    let (accepted, committed, rows) = stage_and_commit(Probe::Real);
    assert!(accepted);
    assert!(committed.is_ok(), "{committed:?}");
    assert_eq!(rows, 2, "raw observation and its head are durable");
}
#[test]
fn normalizer_that_catches_a_metadata_error_and_returns_ok_poisons_the_commit() {
    let (accepted, committed, rows) = stage_and_commit(Probe::CatchesMetadataError);
    assert!(!accepted, "a swallowed metadata failure cannot be accepted");
    assert!(matches!(committed, Err(alondra_store::Error::Poisoned)), "{committed:?}");
    assert_eq!(rows, 0, "an independent connection sees no raw row");
}
#[test]
fn normalizer_error_caught_by_the_caller_poisons_the_commit() {
    let (accepted, committed, rows) = stage_and_commit(Probe::Refuses);
    assert!(!accepted);
    assert!(matches!(committed, Err(alondra_store::Error::Poisoned)), "{committed:?}");
    assert_eq!(rows, 0, "an independent connection sees no raw row");
}
#[test]
fn normalizer_panic_caught_by_the_caller_poisons_the_commit() {
    let (accepted, committed, rows) = stage_and_commit(Probe::Panics);
    assert!(!accepted);
    assert!(matches!(committed, Err(alondra_store::Error::Poisoned)), "{committed:?}");
    assert_eq!(rows, 0, "an independent connection sees no raw row");
}
/// Admits the task and stages its first input through the adapter, catching the adapter's result.
fn guarded_adapter_input(
    load_before_staging: bool,
) -> (
    Result<(), alondra_agent_application::Error>,
    alondra_store::Result<()>,
    i64,
) {
    let directory = test_support::Directory::new();
    let mut adapter = RemoteAdapter::new(Arc::new(Mutex::new(RemoteService::default())));
    let mut connection = database(&directory);
    let mut install = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    LifecycleTransaction::install(&mut install).unwrap();
    install.commit().unwrap();
    let raw = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let mut transaction = LifecycleTransaction::attach(raw, scope()).unwrap();
    let task = TaskId("task".into());
    let input = InputId::new("opaque/input-business").unwrap();
    transaction
        .create(&task, InputPayload::new(input.clone(), "Actual service prompt").unwrap())
        .unwrap();
    transaction
        .apply(
            &task,
            0,
            "worker-admission",
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("opaque/conversation").unwrap(),
                instance: InstanceId::new("remote/resource").unwrap(),
                host: HostId::new("remote-service-endpoint").unwrap(),
            },
        )
        .unwrap();
    transaction.bind_provider(&task, adapter.binding()).unwrap();
    transaction
        .stage_launch(
            &task,
            &Placement::RemoteWorkspace("service/workspace".into()),
            adapter.binding(),
        )
        .unwrap();
    let execution = transaction.load(&task).unwrap().execution().unwrap().clone();
    let adapted = InputMetadata::with_execution(&mut transaction, &execution, &input, |metadata| {
        if load_before_staging {
            adapter.load_input(metadata).map(drop)
        } else {
            adapter.stage_input(metadata)
        }
    });
    let committed = transaction.commit().map(drop);
    let tasks: i64 = Connection::open(directory.join("domain.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM lifecycle_tasks", [], |row| row.get(0))
        .unwrap();
    drop(connection);
    (adapted, committed, tasks)
}
#[test]
fn guarded_adapter_success_commits_its_metadata() {
    let (adapted, committed, tasks) = guarded_adapter_input(false);
    assert!(adapted.is_ok(), "{adapted:?}");
    assert!(committed.is_ok(), "{committed:?}");
    assert_eq!(tasks, 1);
}
#[test]
fn adapter_refusal_caught_by_the_caller_poisons_the_commit() {
    let (adapted, committed, tasks) = guarded_adapter_input(true);
    assert!(
        matches!(
            adapted,
            Err(alondra_agent_application::Error::Adapter(AdapterError::InvalidObservation))
        ),
        "{adapted:?}"
    );
    assert!(matches!(committed, Err(alondra_store::Error::Poisoned)), "{committed:?}");
    assert_eq!(tasks, 0, "an independent connection sees none of the earlier valid writes");
}
