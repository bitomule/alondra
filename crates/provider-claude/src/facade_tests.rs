use super::*;
use alondra_domain::Event;
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId};
use alondra_provider_contract::Capability;
use alondra_store::{AuthorityScope, CommandEvidence, InputPayload, LifecycleTransaction};
use rusqlite::{Connection as SqlConnection, TransactionBehavior};
use std::collections::BTreeSet;
#[path = "../../test_support.rs"]
mod test_support;
struct Fixture {
    connection: SqlConnection,
    directory: test_support::Directory,
    owner: OwnerGrant,
}
impl Fixture {
    fn new() -> (Self, DispatchAttempt) {
        Self::with_epoch("epoch")
    }
    fn with_epoch(epoch: &str) -> (Self, DispatchAttempt) {
        let directory = test_support::Directory::new();
        std::fs::create_dir_all(&*directory).unwrap();
        let mut connection = SqlConnection::open(directory.join("domain.db")).unwrap();
        connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        connection
            .execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql"))
            .unwrap();
        connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL); INSERT INTO domain_meta VALUES(1,'epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('request','Work',0); INSERT INTO agents(id,name) VALUES('node','Worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('task','request','Task','not_started',0,0,0,0)").unwrap();
        connection
            .execute("UPDATE domain_meta SET data_epoch=?1", [epoch])
            .unwrap();
        connection
            .execute(
                "INSERT INTO task_worktrees VALUES('task',?1,0)",
                [directory.to_str().unwrap()],
            )
            .unwrap();
        let mut raw = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        LifecycleTransaction::install(&mut raw).unwrap();
        raw.commit().unwrap();
        let raw = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut tx =
            LifecycleTransaction::attach(raw, AuthorityScope::new(epoch.into()).unwrap()).unwrap();
        let binding = ProviderBinding::new(
            "binding".into(),
            "claude-agent-sdk".into(),
            "instance".into(),
            "configuration".into(),
            1,
            1,
            BTreeSet::from([Capability::OfferInput]),
            "credential".into(),
            1,
        )
        .unwrap();
        tx.create(
            &"task".into(),
            InputPayload::new(InputId::new("initial").unwrap(), "prompt").unwrap(),
        )
        .unwrap();
        tx.apply(
            &"task".into(),
            0,
            "admit",
            Event::Admit {
                node: "node".into(),
                conversation: ConversationId::new("conversation").unwrap(),
                instance: InstanceId::new("instance").unwrap(),
                host: HostId::new("host").unwrap(),
            },
        )
        .unwrap();
        tx.stage_launch(
            &"task".into(),
            &Placement::LocalDirectory(directory.to_str().unwrap().into()),
            &binding,
        )
        .unwrap();
        let permit = tx.commit().unwrap().permits.pop().unwrap();
        let raw = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut tx =
            LifecycleTransaction::attach(raw, AuthorityScope::new(epoch.into()).unwrap()).unwrap();
        tx.reserve_owner(permit.execution(), 0).unwrap();
        tx.claim(permit).unwrap();
        let mut result = tx.commit().unwrap();
        let owner = result.owners.pop().unwrap();
        let attempt = result.attempts.pop().unwrap();
        (
            Self {
                connection,
                directory,
                owner,
            },
            attempt,
        )
    }
    fn command(&mut self, attempt: DispatchAttempt, bytes: Vec<u8>) -> CommittedCommand {
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut tx = LifecycleTransaction::attach(raw, self.owner.scope().clone()).unwrap();
        tx.record_owned_command(
            attempt,
            self.owner.generation(),
            CommandEvidence::new(bytes).unwrap(),
        )
        .unwrap();
        tx.commit().unwrap().commands.pop().unwrap()
    }
    fn observation(&mut self) -> CommittedObservation {
        let raw = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        let mut tx = LifecycleTransaction::attach(raw, self.owner.scope().clone()).unwrap();
        tx.accept_owned_observation(
            self.owner.execution(),
            self.owner.generation(),
            1,
            CommandEvidence::new(b"actual received bytes".to_vec()).unwrap(),
        )
        .unwrap();
        tx.put_normalization_checkpoint(
            self.owner.execution(),
            1,
            None,
            b"causal checkpoint".to_vec(),
        )
        .unwrap();
        tx.settle_observation(self.owner.execution(), 1, 0).unwrap();
        tx.commit().unwrap().observations.pop().unwrap()
    }
}
fn frame(fixture: &Fixture, attempt: &DispatchAttempt, action: Action) -> Vec<u8> {
    let control = ControlIdentity {
        profile_id: "profile".into(),
        account_id: "account".into(),
        host_instance_id: "20000000-0000-4000-8000-000000000001".into(),
        secret: [3; 32],
        host_release: "release".into(),
    };
    alondra_agent_protocol::encode(&alondra_agent_protocol::Frame::new(Body::Command(
        Command {
            fence: fence(&fixture.owner, &control),
            execution: execution(&fixture.owner),
            command_id: attempt.command_id(),
            deadline_ms: 1000,
            action,
        },
    )))
    .unwrap()
}
#[test]
fn launch_claim_cannot_authorize_close_bytes_in_place_of_its_actual_preparation() {
    let (mut fixture, attempt) = Fixture::new();
    let start = frame(
        &fixture,
        &attempt,
        Action::Start {
            cwd: fixture.directory.to_str().unwrap().into(),
            input_id: MessageUuid::new("20000000-0000-4000-8000-000000000002").unwrap(),
            prompt: "prompt".into(),
        },
    );
    let close = frame(&fixture, &attempt, Action::Close);
    let prepared = PreparedDispatch::new(
        &attempt,
        fixture.owner.generation(),
        PreparedCommand::new(start).unwrap(),
    );
    let committed = fixture.command(attempt, close);
    assert!(
        matches!(
            prepared.validate(&committed),
            Err(AdapterError::InvalidObservation)
        ),
        "a genuine persisted Launch claim must not submit a substituted Close"
    );
}
#[test]
fn exact_committed_preparation_matches_its_original_claim() {
    let (mut fixture, attempt) = Fixture::new();
    let bytes = frame(
        &fixture,
        &attempt,
        Action::Start {
            cwd: fixture.directory.to_str().unwrap().into(),
            input_id: MessageUuid::new("20000000-0000-4000-8000-000000000002").unwrap(),
            prompt: "prompt".into(),
        },
    );
    let prepared = PreparedDispatch::new(
        &attempt,
        fixture.owner.generation(),
        PreparedCommand::new(bytes.clone()).unwrap(),
    );
    let committed = fixture.command(attempt, bytes);
    prepared.validate(&committed).unwrap();
}
#[test]
fn ack_value_matches_received_original_bytes_cursor_and_owner_only() {
    let (mut fixture, _attempt) = Fixture::new();
    let committed = fixture.observation();
    let mut received = OrderedObservation {
        owner_generation: fixture.owner.generation(),
        execution: fixture.owner.execution().clone(),
        cursor: 1,
        evidence: b"actual received bytes".to_vec(),
    };
    validate_ack(&fixture.owner, &received, &committed).unwrap();
    received.evidence = b"substituted same-cursor bytes".to_vec();
    assert!(matches!(
        validate_ack(&fixture.owner, &received, &committed),
        Err(AdapterError::InvalidObservation)
    ));
    received.evidence = b"actual received bytes".to_vec();
    received.cursor = 2;
    assert!(matches!(
        validate_ack(&fixture.owner, &received, &committed),
        Err(AdapterError::InvalidObservation)
    ));
    received.cursor = 1;
    received.owner_generation += 1;
    assert!(matches!(
        validate_ack(&fixture.owner, &received, &committed),
        Err(AdapterError::InvalidObservation)
    ));
}
#[test]
fn newly_committed_owner_cannot_accept_an_older_ack_value() {
    let (mut fixture, _attempt) = Fixture::new();
    let committed = fixture.observation();
    let raw = fixture
        .connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    let mut tx =
        LifecycleTransaction::attach(raw, AuthorityScope::new("epoch".into()).unwrap()).unwrap();
    tx.reserve_owner(fixture.owner.execution(), fixture.owner.generation())
        .unwrap();
    let newer = tx.commit().unwrap().owners.pop().unwrap();
    let received = OrderedObservation {
        owner_generation: fixture.owner.generation(),
        execution: fixture.owner.execution().clone(),
        cursor: 1,
        evidence: b"actual received bytes".to_vec(),
    };
    assert!(matches!(
        validate_ack(&newer, &received, &committed),
        Err(AdapterError::InvalidObservation)
    ));
}

#[test]
fn different_scope_cannot_ack_equal_execution_cursor_and_original_bytes() {
    let (mut first, _attempt) = Fixture::new();
    let (second, _attempt) = Fixture::with_epoch("another-epoch");
    let committed = first.observation();
    let received = OrderedObservation {
        owner_generation: second.owner.generation(),
        execution: second.owner.execution().clone(),
        cursor: 1,
        evidence: b"actual received bytes".to_vec(),
    };
    assert_eq!(
        received.execution,
        *committed.execution(),
        "the control isolates the epoch rather than an execution mismatch"
    );
    assert_eq!(received.owner_generation, committed.owner_generation());
    assert!(committed.matches_evidence(&received.evidence));
    assert!(matches!(
        validate_ack(&second.owner, &received, &committed),
        Err(AdapterError::InvalidObservation)
    ));
}
