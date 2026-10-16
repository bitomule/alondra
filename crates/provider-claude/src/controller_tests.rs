//! Local raw-evidence/controller transaction regression, not a native SDK integration test.
#[path = "../../agent-application/tests/support/controller.rs"]
mod fixture_controller;
use fixture_controller::WorkerController;
use super::normalizer::Normalizer;
use super::records::{InputRecord, SessionRecord};
use super::wire::{ControlIdentity, execution, fence};
use alondra_agent_application::{DurableAdapter, InputMetadata};
use alondra_agent_protocol::{
    Body, Event as WireEvent, Frame, NativeCorrelation, Observation as WireObservation,
    SdkEventKind,
};
use alondra_domain::{ActivityView, Event};
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId, MessageUuid};
use alondra_provider_contract::*;
use alondra_store::{
    AdapterInputMetadata, AdapterState, AuthorityScope, InputPayload, LifecycleTransaction,
    OwnerGrant,
};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex};
#[path = "../../test_support.rs"]
mod test_support;
const SESSION: &str = "20000000-0000-4000-8000-000000000001";
const FIRST: &str = "20000000-0000-4000-8000-000000000002";
const SECOND: &str = "20000000-0000-4000-8000-000000000003";
const THIRD: &str = "20000000-0000-4000-8000-000000000004";
#[derive(Default)]
struct RawFixture {
    events: VecDeque<Value>,
    acknowledgements: Vec<u64>,
    offered: Vec<Vec<u8>>,
    next: u64,
}
struct FixtureAdapter {
    binding: ProviderBinding,
    owner: Option<OwnerGrant>,
    normalizer: Normalizer,
    fixture: Arc<Mutex<RawFixture>>,
}
impl FixtureAdapter {
    fn new(fixture: Arc<Mutex<RawFixture>>) -> Self {
        Self {
            binding: ProviderBinding::new(
                "binding".into(),
                "raw-fixture".into(),
                "instance".into(),
                "config".into(),
                1,
                1,
                BTreeSet::from([Capability::OfferInput]),
                "credential".into(),
                1,
            )
            .unwrap(),
            owner: None,
            normalizer: Normalizer {
                session: None,
                fence: None,
            },
            fixture,
        }
    }
}
fn storage(error: alondra_store::Error) -> AdapterError {
    AdapterError::Storage(error.to_string())
}
fn input_record(value: &str) -> Vec<u8> {
    serde_json::to_vec(&InputRecord {
        message_id: MessageUuid::new(value).unwrap(),
    })
    .unwrap()
}
impl DurableAdapter for FixtureAdapter {
    type Normalizer = Normalizer;
    fn normalizer(&self) -> &Normalizer {
        &self.normalizer
    }
    fn owner_generation(&self) -> Result<u64, AdapterError> {
        self.owner
            .as_ref()
            .map(OwnerGrant::generation)
            .ok_or(AdapterError::Disconnected)
    }
    fn stage_input(&mut self, mut metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        metadata
            .put_conversation(
                serde_json::to_vec(&SessionRecord {
                    session_id: MessageUuid::new(SESSION).unwrap(),
                })
                .unwrap(),
            )
            .map_err(storage)?;
        metadata.put_input(input_record(FIRST)).map_err(storage)
    }
    fn load_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<Vec<u8>, AdapterError> {
        metadata.input().map_err(storage)
    }
    fn install_owner(
        &mut self,
        owner: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        self.normalizer.session = Some(serde_json::from_slice(state.conversation()).unwrap());
        self.normalizer.fence = Some(fence(
            &owner,
            &ControlIdentity {
                profile_id: "fixture".into(),
                account_id: "fixture".into(),
                host_instance_id: "fixture".into(),
                secret: [0; 32],
                host_release: "fixture".into(),
            },
        ));
        self.owner = Some(owner);
        Ok(())
    }
}
impl Adapter for FixtureAdapter {
    type NativeInput = Vec<u8>;
    fn binding(&self) -> &ProviderBinding {
        &self.binding
    }
    fn prepare(
        &mut self,
        _: Invocation<'_>,
        native: Vec<u8>,
    ) -> Result<PreparedCommand, AdapterError> {
        PreparedCommand::new(native)
    }
    fn submit(
        &mut self,
        _: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError> {
        self.fixture
            .lock()
            .unwrap()
            .offered
            .push(command.bytes().to_vec());
        Ok(Handoff::Offered)
    }
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        let owner = self.owner.as_ref().ok_or(AdapterError::Disconnected)?;
        let mut fixture = self.fixture.lock().unwrap();
        let Some(mut payload) = fixture.events.pop_front() else {
            return Ok(None);
        };
        payload["session_id"] = json!(SESSION);
        fixture.next += 1;
        let frame = Frame::new(Body::Event(WireEvent {
            fence: self.normalizer.fence.clone().unwrap(),
            execution: execution(owner),
            cursor: fixture.next,
            observation: WireObservation::Sdk {
                kind: SdkEventKind::Message,
                native: NativeCorrelation::default(),
                payload,
                failures: vec![],
            },
        }));
        let framed =
            alondra_agent_protocol::encode(&frame).map_err(|_| AdapterError::InvalidObservation)?;
        Ok(Some(OrderedObservation {
            owner_generation: owner.generation(),
            execution: owner.execution().clone(),
            cursor: fixture.next,
            evidence: framed[4..].to_vec(),
        }))
    }
    fn acknowledge(
        &mut self,
        _: &alondra_foundation::ExecutionStamp,
        cursor: u64,
    ) -> Result<(), AdapterError> {
        self.fixture.lock().unwrap().acknowledgements.push(cursor);
        Ok(())
    }
}
#[test]
fn raw_normalizer_commits_initial_trigger_once_and_preserves_followup_fifo() {
    let directory = test_support::Directory::new();
    std::fs::create_dir_all(&*directory).unwrap();
    let path = directory.join("domain.db");
    let connection = Connection::open(&path).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    connection
        .execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql"))
        .unwrap();
    connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL); INSERT INTO domain_meta VALUES(1,'epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('request','Work',0); INSERT INTO agents(id,name) VALUES('node','Worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('task','request','Task','not_started',0,0,0,0)").unwrap();
    connection
        .execute(
            "INSERT INTO task_worktrees VALUES('task',?1,0)",
            [directory.to_str().unwrap()],
        )
        .unwrap();
    let fixture = Arc::new(Mutex::new(RawFixture::default()));
    let mut controller = WorkerController::open(
        connection,
        AuthorityScope::new("epoch".into()).unwrap(),
        FixtureAdapter::new(fixture.clone()),
    )
    .unwrap();
    controller
        .launch(
            &"task".into(),
            InputPayload::new(InputId::new("initial").unwrap(), "Initial prompt").unwrap(),
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").unwrap(),
                instance: InstanceId::new("instance").unwrap(),
                host: HostId::new("host").unwrap(),
            },
            Placement::LocalDirectory(directory.to_str().unwrap().into()),
        )
        .unwrap();
    let permit = controller.next_permit().unwrap();
    let attempt = controller.claim(permit).unwrap();
    assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
    fixture
        .lock()
        .unwrap()
        .events
        .push_back(json!({"type":"system","subtype":"init"}));
    assert!(controller.observe().unwrap());
    assert_eq!(
        controller.load(&"task".into()).unwrap().pending_inputs(),
        vec![&InputId::new("initial").unwrap()]
    );
    fixture.lock().unwrap().events.push_back(json!({"type":"assistant","uuid":"20000000-0000-4000-8000-000000000010","parent_tool_use_id":null,"user_message_uuid":FIRST,"user_message_uuids":[FIRST]}));
    assert!(
        controller
            .observe()
            .expect("initial TurnStarted consumes its trigger exactly once")
    );
    let active = controller.load(&"task".into()).unwrap();
    assert_eq!(active.activity(), ActivityView::Working);
    assert!(active.pending_inputs().is_empty());
    let raw = Connection::open(&path).unwrap();
    raw.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    // These cards exercise the existing raw store fixture, not the missing ReadIndex app seam.
    for business in ["second", "third"] {
        raw.execute("INSERT INTO interactions(id,scope_kind,task_id,author_kind,recipient_kind,body,blocking,kind,revision,ordering,options,withdrawn,card) VALUES(?1,'task','task','system','user','What next?',1,'question',0,0,'[{\"id\":\"answer\",\"label\":\"Answer\"}]',0,'{\"kind\":\"provide\",\"need\":\"Your decision\"}')", [business]).unwrap();
    }
    let mut transaction = LifecycleTransaction::attach(
        rusqlite::Transaction::new_unchecked(&raw, rusqlite::TransactionBehavior::Deferred)
            .unwrap(),
        AuthorityScope::new("epoch".into()).unwrap(),
    )
    .unwrap();
    for (business, native) in [("second", SECOND), ("third", THIRD)] {
        let aggregate = transaction.load(&"task".into()).unwrap();
        transaction
            .apply(
                &"task".into(),
                aggregate.revision(),
                &format!("ask-{business}"),
                Event::Ask {
                    card: business.into(),
                },
            )
            .unwrap();
        transaction
            .put_input(
                &"task".into(),
                InputPayload::new(
                    InputId::new(business).unwrap(),
                    format!("Prompt {business}"),
                )
                .unwrap(),
            )
            .unwrap();
        transaction
            .apply(
                &"task".into(),
                aggregate.revision() + 1,
                &format!("answer-{business}"),
                Event::Answer {
                    card: business.into(),
                    input: InputId::new(business).unwrap(),
                },
            )
            .unwrap();
        let settled = raw.execute("UPDATE interactions SET answer='{}' WHERE task_id='task' AND id=?1 AND scope_kind='task' AND answer IS NULL AND withdrawn=0", [business]).unwrap();
        assert_eq!(
            settled, 1,
            "fixture owner settles this exact card on the journal transaction"
        );
        transaction
            .put_adapter_input(
                active.execution().unwrap(),
                &InputId::new(business).unwrap(),
                AdapterInputMetadata::new(input_record(native)).unwrap(),
            )
            .unwrap();
        transaction
            .stage_input(&"task".into(), &InputId::new(business).unwrap())
            .unwrap();
    }
    let mut committed = transaction.commit().unwrap();
    assert_eq!(
        committed.permits.len(),
        1,
        "only the first pending reply may be offered"
    );
    let permit = committed.permits.pop().unwrap();
    assert_eq!(permit.input(), &InputId::new("second").unwrap());
    let attempt = controller.claim(permit).unwrap();
    assert!(
        controller.next_permit().is_none(),
        "third has no permit while second is merely claimed"
    );
    assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
    let permit = controller
        .next_permit()
        .expect("third becomes eligible after second is proven handed");
    assert_eq!(permit.input(), &InputId::new("third").unwrap());
    let attempt = controller.claim(permit).unwrap();
    assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
    assert!(
        controller.next_permit().is_none(),
        "no duplicated third offer"
    );
    assert_eq!(
        fixture.lock().unwrap().offered,
        vec![
            input_record(FIRST),
            input_record(SECOND),
            input_record(THIRD)
        ]
    );
    assert_eq!(
        controller.load(&"task".into()).unwrap().pending_inputs(),
        vec![
            &InputId::new("second").unwrap(),
            &InputId::new("third").unwrap()
        ]
    );
    fixture.lock().unwrap().events.push_back(json!({"type":"assistant","uuid":"20000000-0000-4000-8000-000000000011","parent_tool_use_id":null,"user_message_uuid":SECOND,"user_message_uuids":[FIRST,SECOND]}));
    assert!(controller.observe().unwrap());
    assert_eq!(
        controller.load(&"task".into()).unwrap().pending_inputs(),
        vec![&InputId::new("third").unwrap()]
    );
    assert!(
        controller.next_permit().is_none(),
        "already handed third input is not offered again after second consumption"
    );
    fixture.lock().unwrap().events.push_back(json!({"type":"assistant","uuid":"20000000-0000-4000-8000-000000000012","parent_tool_use_id":null,"user_message_uuid":THIRD,"user_message_uuids":[FIRST,SECOND,THIRD]}));
    assert!(controller.observe().unwrap());
    assert!(
        controller
            .load(&"task".into())
            .unwrap()
            .pending_inputs()
            .is_empty()
    );
    assert_eq!(fixture.lock().unwrap().acknowledgements, vec![1, 2, 3, 4]);
    assert_eq!(
        fixture.lock().unwrap().offered,
        vec![
            input_record(FIRST),
            input_record(SECOND),
            input_record(THIRD)
        ]
    );
    let evidence: i64 = raw
        .query_row("SELECT count(*) FROM provider_observations", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(evidence, 4);
    drop(raw);
    drop(controller);
}
