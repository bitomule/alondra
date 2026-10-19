//! Actual SDK child-process fixture. The wrapper observes/delegates; it creates no facts.
#[path = "../../agent-application/tests/support/controller.rs"]
mod fixture_controller;
use fixture_controller::WorkerController;
use super::*;
use alondra_agent_application::Error as ControllerError;
use alondra_store::{AuthorityScope, InputPayload};
use alondra_agent_protocol::{FramedReader, ReceivedFrame};
use alondra_domain::ActivityView;
use alondra_foundation::{ConversationId, HostId, InputId, InstanceId};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Cursor;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
#[path = "../../test_support.rs"]
mod test_support;

#[derive(Default)]
struct Probe {
    received: Vec<OrderedObservation>,
    acks: Vec<(u64, bool)>,
    cleanup: Option<Result<Cleanup, String>>,
}
struct Cleanup {
    stop: Result<(), String>,
    exit: Result<ExitStatus, String>,
}
struct OwnedSdkFixture {
    inner: ClaudeAdapter,
    database: PathBuf,
    probe: Arc<Mutex<Probe>>,
}
impl Drop for OwnedSdkFixture {
    fn drop(&mut self) {
        let cleanup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let host = self
                .inner
                .host
                .as_mut()
                .ok_or_else(|| "owned host was not installed".to_owned())?;
            let stop = host.request_stop().map_err(|error| error.to_string());
            let exit = host.wait().map_err(|error| error.to_string());
            Ok(Cleanup { stop, exit })
        }))
        .unwrap_or_else(|_| Err("owned fixture cleanup panicked".into()));
        self.probe
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cleanup = Some(cleanup);
    }
}
impl DurableAdapter for OwnedSdkFixture {
    type Normalizer = Normalizer;
    fn normalizer(&self) -> &Normalizer {
        self.inner.normalizer()
    }
    fn owner_generation(&self) -> Result<u64, AdapterError> {
        self.inner.owner_generation()
    }
    fn stage_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<(), AdapterError> {
        self.inner.stage_input(metadata)
    }
    fn load_input(&mut self, metadata: InputMetadata<'_, '_>) -> Result<InputRecord, AdapterError> {
        self.inner.load_input(metadata)
    }
    fn install_owner(
        &mut self,
        owner: OwnerGrant,
        state: AdapterState,
    ) -> Result<(), AdapterError> {
        self.inner.install_owner(owner, state)
    }
}
impl Adapter for OwnedSdkFixture {
    type NativeInput = InputRecord;
    fn binding(&self) -> &ProviderBinding {
        self.inner.binding()
    }
    fn prepare(
        &mut self,
        invocation: Invocation<'_>,
        native: InputRecord,
    ) -> Result<PreparedCommand, AdapterError> {
        self.inner.prepare(invocation, native)
    }
    fn submit(
        &mut self,
        invocation: Invocation<'_>,
        command: &PreparedCommand,
    ) -> Result<Handoff, AdapterError> {
        self.inner.submit(invocation, command)
    }
    fn next(&mut self) -> Result<Option<OrderedObservation>, AdapterError> {
        let next = self.inner.next()?;
        if let Some(batch) = &next {
            self.probe.lock().unwrap().received.push(batch.clone());
        }
        Ok(next)
    }
    fn acknowledge(&mut self, stamp: &ExecutionStamp, cursor: u64) -> Result<(), AdapterError> {
        // An independent connection sees committed data only; do not supply policy to the adapter.
        let connection = rusqlite::Connection::open(&self.database)
            .map_err(|error| AdapterError::Storage(error.to_string()))?;
        let visible: bool = connection.query_row("SELECT EXISTS(SELECT 1 FROM provider_observations o JOIN provider_normalization_checkpoints c USING(task_id,generation,binding_id) WHERE o.task_id=?1 AND o.generation=?2 AND o.cursor=?3 AND c.cursor>=o.cursor)", params![stamp.task().0, i64::try_from(stamp.generation()).unwrap(), i64::try_from(cursor).unwrap()], |row| row.get(0)).map_err(|error| AdapterError::Storage(error.to_string()))?;
        self.inner.acknowledge(stamp, cursor)?;
        self.probe.lock().unwrap().acks.push((cursor, visible));
        Ok(())
    }
}
fn runfile(variable: &str) -> PathBuf {
    PathBuf::from(std::env::var_os("TEST_SRCDIR").expect("Bazel runfiles root"))
        .join(std::env::var_os("TEST_WORKSPACE").expect("Bazel workspace"))
        .join(std::env::var_os(variable).expect("declared rootpath data"))
        .canonicalize()
        .expect("resolve actual declared runfile")
}
fn private_directory(path: &Path) {
    DirBuilder::new().mode(0o700).create(path).unwrap();
}
fn hash(path: &Path) -> String {
    format!("{:x}", Sha256::digest(fs::read(path).unwrap()))
}
fn decode_evidence(bytes: &[u8]) -> ReceivedFrame {
    let mut framed = u32::try_from(bytes.len()).unwrap().to_be_bytes().to_vec();
    framed.extend_from_slice(bytes);
    FramedReader::new(Cursor::new(framed))
        .read_received()
        .unwrap()
        .unwrap()
}
fn binding() -> ProviderBinding {
    ProviderBinding::new(
        "sdk-binding".into(),
        "claude-agent-sdk".into(),
        "sdk-instance".into(),
        "sdk-fixture".into(),
        1,
        1,
        BTreeSet::from([alondra_provider_contract::Capability::OfferInput]),
        "fixture-no-login".into(),
        1,
    )
    .unwrap()
}
fn database(path: &Path, cwd: &Path) -> rusqlite::Connection {
    let connection = rusqlite::Connection::open(path).unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON").unwrap();
    connection
        .execute_batch(include_str!("../../alondrad/src/domain_store/schema.sql"))
        .unwrap();
    connection.execute_batch("CREATE TABLE task_worktrees(task_id TEXT PRIMARY KEY,dir TEXT NOT NULL,at INTEGER NOT NULL); INSERT INTO domain_meta VALUES(1,'sdk-epoch',0,0); INSERT INTO requests(id,body,closed) VALUES('sdk-request','Work',0); INSERT INTO agents(id,name) VALUES('sdk-node','Worker'); INSERT INTO tasks(id,request_id,title,state,revision,started,review_prs,archived) VALUES('sdk-task','sdk-request','SDK fixture task','not_started',0,0,0,0)").unwrap();
    connection
        .execute(
            "INSERT INTO task_worktrees VALUES('sdk-task',?1,0)",
            [cwd.to_str().unwrap()],
        )
        .unwrap();
    connection
}
// Python preserves signed Darwin dev_t; MetadataExt exposes its `as u64` representation.
fn captured_device(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().map(|device| device as u64))
}
fn assert_device_number_boundaries() {
    assert_eq!(captured_device(&json!(-1)), Some(u64::MAX));
    assert_eq!(captured_device(&json!(i64::MIN)), Some(1_u64 << 63));
    assert_eq!(captured_device(&json!(0)), Some(0));
    assert_eq!(captured_device(&json!(i64::MAX)), Some(i64::MAX as u64));
    assert_eq!(captured_device(&json!(u64::MAX)), Some(u64::MAX));
    assert_eq!(captured_device(&json!(1.5)), None);
    assert_eq!(captured_device(&json!("-1")), None);
    assert_eq!(captured_device(&Value::Null), None);
}
fn scenario(prompt: &str, terminal_gap: bool) {
    let directory = test_support::Directory::for_unix_socket();
    private_directory(&directory);
    let state = directory.join("host");
    let home = directory.join("home");
    let cwd = directory.join("workspace");
    for path in [&state, &home, &cwd] {
        private_directory(path);
    }
    let node = runfile("FIXTURE_NODE");
    let bundle = runfile("FIXTURE_HOST");
    let native = directory.join("native");
    fs::copy(runfile("FIXTURE_NATIVE"), &native).unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
    let capture = directory.join("native-capture.json");
    let database_path = directory.join("domain.db");
    let connection = database(&database_path, &cwd);
    let environment = BTreeMap::from([
        ("HOME".into(), home.to_str().unwrap().into()),
        ("PATH".into(), "/usr/bin:/bin".into()),
    ]);
    let mut native_environment = environment.clone();
    native_environment.insert("FIXTURE_CAPTURE".into(), capture.to_str().unwrap().into());
    let configuration = ClaudeConfiguration {
        artifact: VerifiedArtifact::verify(&node, &bundle, &hash(&node), &hash(&bundle)).unwrap(),
        state_directory: state.clone(),
        host_environment: environment,
        native_executable: native,
        native_directory: cwd.clone(),
        native_environment,
        profile_id: "sdk-profile".into(),
        account_id: "sdk-account".into(),
        host_release: "sdk-client-fixture".into(),
        io_timeout: Duration::from_secs(30),
        confinement: None,
        worker: None,
    };
    let probe = Arc::new(Mutex::new(Probe::default()));
    let adapter = OwnedSdkFixture {
        inner: ClaudeAdapter::new(binding(), configuration).unwrap(),
        database: database_path.clone(),
        probe: probe.clone(),
    };
    let mut controller = WorkerController::open(
        connection,
        AuthorityScope::new("sdk-epoch".into()).unwrap(),
        adapter,
    )
    .unwrap();
    let body = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        controller
            .launch(
                &"sdk-task".into(),
                InputPayload::new(InputId::new("sdk-initial").unwrap(), prompt).unwrap(),
                alondra_domain::Event::Admit {
                    node: "sdk-node".into(),
                    conversation: ConversationId::new("sdk-conversation").unwrap(),
                    instance: InstanceId::new("sdk-execution").unwrap(),
                    host: HostId::new("sdk-host").unwrap(),
                },
                Placement::LocalDirectory(cwd.to_str().unwrap().into()),
            )
            .unwrap();
        let permit = controller
            .next_permit()
            .expect("only real committed launch permit");
        let attempt = controller.claim(permit).unwrap();
        assert_eq!(controller.dispatch(attempt).unwrap(), Handoff::Offered);
        assert_eq!(
            controller
                .load(&"sdk-task".into())
                .unwrap()
                .pending_inputs(),
            vec![&InputId::new("sdk-initial").unwrap()],
            "technical handoff cannot consume a business input"
        );
        let mut reached = false;
        for _ in 0..32 {
            match controller.observe() {
                Err(ControllerError::RequiresDomainCommand) if terminal_gap => {
                    reached = true;
                    break;
                }
                Ok(true) => {
                    if !terminal_gap
                        && controller
                            .load(&"sdk-task".into())
                            .unwrap()
                            .pending_inputs()
                            .is_empty()
                    {
                        reached = true;
                        break;
                    }
                }
                other => panic!("unexpected actual SDK observation: {other:?}"),
            }
        }
        assert!(
            reached,
            "bounded semantic observation sequence did not reach its actual SDK boundary"
        );
        let task = controller.load(&"sdk-task".into()).unwrap();
        assert!(task.holds_slot());
        if terminal_gap {
            assert_eq!(
                task.pending_inputs(),
                vec![&InputId::new("sdk-initial").unwrap()]
            );
        } else {
            assert!(task.pending_inputs().is_empty());
            assert_eq!(task.activity(), ActivityView::Working);
        }
        let raw = rusqlite::Connection::open(&database_path).unwrap();
        let session: Vec<u8> = raw
            .query_row(
                "SELECT metadata FROM provider_conversation_metadata WHERE task_id='sdk-task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let session: SessionRecord = serde_json::from_slice(&session).unwrap();
        let native: Vec<u8> = raw.query_row("SELECT metadata FROM provider_input_metadata WHERE task_id='sdk-task' AND input_id='sdk-initial'", [], |row| row.get(0)).unwrap();
        let native: InputRecord = serde_json::from_slice(&native).unwrap();
        let capture: Value = serde_json::from_slice(&fs::read(&capture).unwrap()).unwrap();
        assert!(capture["args"].as_array().unwrap().contains(&json!(format!(
            "--session-id={}",
            session.session_id.as_str()
        ))));
        let bootstrap = fs::metadata(state.join("host.lock")).unwrap();
        let bootstrap_identity = (bootstrap.dev(), bootstrap.ino());
        let native_stdin = &capture["descriptors"]["0"];
        assert!(
            native_stdin.is_object(),
            "actual native FD0 descriptor witness is required"
        );
        for descriptor in ["0", "3", "4", "5"] {
            let captured = &capture["descriptors"][descriptor];
            if captured.is_object() {
                assert_ne!(
                    (
                        captured_device(&captured["dev"]).unwrap_or_else(|| panic!(
                            "integer native device witness required: {captured}"
                        )),
                        captured["ino"].as_u64().unwrap()
                    ),
                    bootstrap_identity,
                    "native descriptor {descriptor} must not inherit the Rust-owned bootstrap file"
                );
            }
        }
        let contender = OpenOptions::new()
            .read(true)
            .write(true)
            .open(state.join("host.lock"))
            .unwrap();
        assert!(
            contender.try_lock().is_err(),
            "live actual host retains inherited kernel ownership before cleanup"
        );
        drop(contender);
        let witness = probe.lock().unwrap();
        let last = witness.received.last().unwrap();
        let decoded = decode_evidence(&last.evidence);
        let Body::Event(event) = &decoded.frame().body else {
            panic!("real event required");
        };
        let alondra_agent_protocol::Observation::Sdk { payload, .. } = &event.observation else {
            panic!("SDK payload required");
        };
        assert_eq!(
            payload["type"],
            if terminal_gap { "result" } else { "assistant" }
        );
        assert_eq!(payload["session_id"], session.session_id.as_str());
        assert_eq!(payload["user_message_uuid"], native.message_id.as_str());
        assert_eq!(
            payload["user_message_uuids"],
            json!([native.message_id.as_str()])
        );
        let committed: i64 = raw
            .query_row(
                "SELECT cursor FROM provider_normalization_checkpoints WHERE task_id='sdk-task'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let committed = u64::try_from(committed).unwrap();
        assert!(
            witness.acks.iter().all(|(_, visible)| *visible),
            "every emitted ACK follows independently visible SQL receipt/checkpoint commit"
        );
        assert_eq!(witness.acks.last().unwrap().0, committed);
        if terminal_gap {
            assert!(last.cursor > committed);
            assert!(
                !witness
                    .acks
                    .iter()
                    .any(|(cursor, _)| *cursor == last.cursor)
            );
        } else {
            assert_eq!(last.cursor, committed);
            assert!(!witness.received.iter().any(|batch| matches!(&decode_evidence(&batch.evidence).frame().body, Body::Event(event) if matches!(&event.observation, alondra_agent_protocol::Observation::Sdk { payload, .. } if payload["type"]=="result"))));
        }
        let commands: i64 = raw
            .query_row("SELECT count(*) FROM provider_commands", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(commands, 1);
        for batch in &witness.received {
            let stored: Option<Vec<u8>> = raw.query_row("SELECT bytes FROM provider_observations WHERE task_id='sdk-task' AND cursor=?1", [i64::try_from(batch.cursor).unwrap()], |row| row.get(0)).optional().unwrap();
            if batch.cursor <= committed {
                assert_eq!(stored.as_ref(), Some(&batch.evidence));
            } else {
                assert!(
                    stored.is_none(),
                    "blocked terminal transaction cannot create accepted receipt"
                );
            }
        }
    }));
    drop(controller);
    let mut observed = probe
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let cleanup = observed
        .cleanup
        .take()
        .expect("fixture drop records actual owned Child cleanup");
    if let Err(panic) = body {
        match &cleanup {
            Err(_) => eprintln!("owned fixture cleanup panicked or could not find its host"),
            Ok(cleanup) => {
                if cleanup.stop.is_err() {
                    eprintln!("owned fixture stop request failed during panic cleanup");
                }
                if !matches!(&cleanup.exit, Ok(status) if status.success()) {
                    eprintln!("owned fixture exit failed during panic cleanup");
                }
            }
        }
        drop(observed);
        std::panic::resume_unwind(panic);
    }
    let cleanup = cleanup.unwrap();
    assert!(cleanup.stop.is_ok(), "{:#?}", cleanup.stop);
    assert!(cleanup.exit.unwrap().success());
    let starts = fs::read_to_string(capture.with_file_name("native-capture.json.starts")).unwrap();
    let starts = starts
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        starts.len(),
        1,
        "append-only external program startup witness counts actual native process entries"
    );
    let pids = starts
        .iter()
        .map(|entry| entry["pid"].as_u64().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(pids.len(), 1);
    assert!(pids.iter().all(|pid| *pid > 0));
    let mut journal = FramedReader::new(fs::File::open(state.join("outbox")).unwrap());
    let mut found = 0;
    let mut starts = 0;
    while let Some(frame) = journal.read_received().unwrap() {
        match &frame.frame().body {
            Body::Event(event) => {
                if let Some(batch) = observed
                    .received
                    .iter()
                    .find(|batch| batch.cursor == event.cursor)
                {
                    assert_eq!(frame.payload(), batch.evidence.as_slice());
                    found += 1;
                }
            }
            Body::Command(command) if matches!(&command.action, Action::Start { .. }) => {
                starts += 1
            }
            _ => {}
        }
    }
    assert_eq!(
        found,
        observed.received.len(),
        "host retained every witnessed exact sender payload, including blocked terminal evidence"
    );
    assert_eq!(starts, 1);
}
#[test]
fn actual_sdk_root_assistant_commits_first_input_consumption_before_owned_stop() {
    assert_device_number_boundaries();
    scenario("consume-and-hold", false);
}
#[test]
fn actual_sdk_result_only_consumption_preserves_unacknowledged_terminal_application_gap() {
    scenario("normal-result-only", true);
}


struct FacadeFixture {
    worker: ClaudeWorker,
    connection: rusqlite::Connection,
    capture: PathBuf,
    scope: AuthorityScope,
    cleanup: Option<Cleanup>,
    _directory: test_support::Directory,
}
impl FacadeFixture {
    fn staged_owner(
        epoch: &str,
        instance: &str,
    ) -> (
        Self,
        OwnerGrant,
        AdapterState,
        alondra_store::DispatchPermit,
    ) {
        Self::staged_runtime(epoch, instance, binding(), "workspace")
    }
    fn staged_runtime(
        epoch: &str,
        instance: &str,
        provider: ProviderBinding,
        native_workspace: &str,
    ) -> (
        Self,
        OwnerGrant,
        AdapterState,
        alondra_store::DispatchPermit,
    ) {
        let directory = test_support::Directory::for_unix_socket();
        private_directory(&directory);
        let state = directory.join("host");
        let home = directory.join("home");
        let cwd = directory.join("workspace");
        for path in [&state, &home, &cwd] {
            private_directory(path);
        }
        let native_cwd = directory.join(native_workspace);
        if native_cwd != cwd {
            private_directory(&native_cwd);
        }
        let node = runfile("FIXTURE_NODE");
        let bundle = runfile("FIXTURE_HOST");
        let native = directory.join("native");
        fs::copy(runfile("FIXTURE_NATIVE"), &native).unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();
        let capture = directory.join("native-capture.json");
        let environment = BTreeMap::from([
            ("HOME".into(), home.to_str().unwrap().into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]);
        let mut native_environment = environment.clone();
        native_environment.insert("FIXTURE_CAPTURE".into(), capture.to_str().unwrap().into());
        let configuration = ClaudeConfiguration {
            artifact: VerifiedArtifact::verify(&node, &bundle, &hash(&node), &hash(&bundle))
                .unwrap(),
            state_directory: state,
            host_environment: environment,
            native_executable: native,
            native_directory: native_cwd,
            native_environment,
            profile_id: "sdk-profile".into(),
            account_id: "sdk-account".into(),
            host_release: "sdk-facade-fixture".into(),
            io_timeout: Duration::from_secs(30),
            confinement: None,
            worker: None,
        };
        let connection = database(&directory.join("domain.db"), &cwd);
        connection
            .execute("UPDATE domain_meta SET data_epoch=?1", [epoch])
            .unwrap();
        let worker = ClaudeWorker::configured(provider, configuration).unwrap();
        let mut fixture = Self {
            worker,
            connection,
            capture,
            scope: AuthorityScope::new(epoch.into()).unwrap(),
            cleanup: None,
            _directory: directory,
        };
        assert!(
            !fixture
                .capture
                .with_file_name("native-capture.json.starts")
                .exists(),
            "configuration is inert"
        );
        let mut raw = rusqlite::Transaction::new_unchecked(
            &fixture.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        alondra_store::LifecycleTransaction::install(&mut raw).unwrap();
        raw.commit().unwrap();
        let raw = rusqlite::Transaction::new_unchecked(
            &fixture.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        let mut tx =
            alondra_store::LifecycleTransaction::attach(raw, fixture.scope.clone()).unwrap();
        let input = InputId::new("sdk-initial").unwrap();
        tx.create(
            &"sdk-task".into(),
            InputPayload::new(input.clone(), "consume-and-hold").unwrap(),
        )
        .unwrap();
        tx.apply(
            &"sdk-task".into(),
            0,
            "facade-admit",
            alondra_domain::Event::Admit {
                node: "sdk-node".into(),
                conversation: ConversationId::new("sdk-conversation").unwrap(),
                instance: InstanceId::new(instance).unwrap(),
                host: HostId::new("sdk-host").unwrap(),
            },
        )
        .unwrap();
        tx.bind_provider(&"sdk-task".into(), fixture.worker.binding())
            .unwrap();
        let stamp = tx
            .load(&"sdk-task".into())
            .unwrap()
            .execution()
            .unwrap()
            .clone();
        InputMetadata::with_execution(&mut tx, &stamp, &input, |metadata| {
            fixture.worker.stage_input(metadata)
        })
        .unwrap();
        tx.stage_launch(
            &"sdk-task".into(),
            &Placement::LocalDirectory(cwd.to_str().unwrap().into()),
            fixture.worker.binding(),
        )
        .unwrap();
        tx.reserve_owner(&stamp, 0).unwrap();
        let mut committed = tx.commit().unwrap();
        let owner = committed.owners.pop().unwrap();
        let permit = committed.permits.pop().unwrap();
        let raw = rusqlite::Transaction::new_unchecked(
            &fixture.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        let tx = alondra_store::LifecycleTransaction::attach(raw, fixture.scope.clone()).unwrap();
        let state = tx.adapter_state(&owner).unwrap();
        tx.commit().unwrap();
        (fixture, owner, state, permit)
    }
    fn new() -> (Self, alondra_store::DispatchAttempt) {
        let (mut fixture, owner, state, permit) = Self::staged_owner("sdk-epoch", "sdk-execution");
        fixture.worker.install_owner(owner, state).unwrap();
        assert!(
            !fixture
                .capture
                .with_file_name("native-capture.json.starts")
                .exists(),
            "committed control owner starts an inert host, not a native query"
        );
        let raw = rusqlite::Transaction::new_unchecked(
            &fixture.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        let mut tx =
            alondra_store::LifecycleTransaction::attach(raw, fixture.scope.clone()).unwrap();
        tx.claim(permit).unwrap();
        let attempt = tx.commit().unwrap().attempts.pop().unwrap();
        (fixture, attempt)
    }
    fn prepare(&mut self, attempt: &alondra_store::DispatchAttempt) -> PreparedCommand {
        let raw = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        let mut tx = alondra_store::LifecycleTransaction::attach(raw, self.scope.clone()).unwrap();
        let command = InputMetadata::with_attempt(&mut tx, attempt, |metadata| {
            self.worker.prepare(attempt, metadata)
        })
        .unwrap();
        tx.commit().unwrap();
        command
    }
    fn commit_command(
        &mut self,
        attempt: alondra_store::DispatchAttempt,
        bytes: Vec<u8>,
    ) -> alondra_store::CommittedCommand {
        let raw = rusqlite::Transaction::new_unchecked(
            &self.connection,
            rusqlite::TransactionBehavior::Immediate,
        )
        .unwrap();
        let mut tx = alondra_store::LifecycleTransaction::attach(raw, self.scope.clone()).unwrap();
        tx.record_owned_command(
            attempt,
            self.worker.owner_generation().unwrap(),
            alondra_store::CommandEvidence::new(bytes).unwrap(),
        )
        .unwrap();
        tx.commit().unwrap().commands.pop().unwrap()
    }
    fn assert_no_host_effects(&self) {
        assert!(self.worker.adapter.host.is_none(), "rejected provenance must not install a host");
        assert_eq!(
            fs::read_dir(&self.worker.adapter.configuration.state_directory).unwrap().count(),
            0,
            "rejected provenance must not create any host lock, socket or journal"
        );
        assert!(!self.capture.with_file_name("native-capture.json.starts").exists());
    }
    fn stop(&mut self) -> Result<(), String> {
        self.finish_cleanup(false)
    }
    fn finish_cleanup(&mut self, report_stop_error: bool) -> Result<(), String> {
        if self.cleanup.is_none() {
            let Some(host) = self.worker.adapter.host.as_mut() else {
                return Ok(());
            };
            let stop = host.request_stop().map_err(|error| error.to_string());
            let stop = if report_stop_error {
                stop.and_then(|()| {
                    Err("fixture-reported error after actual stop request".to_owned())
                })
            } else {
                stop
            };
            // Actual exit is attempted independently, even when requesting stop reports an error.
            let exit = host.wait().map_err(|error| error.to_string());
            self.cleanup = Some(Cleanup { stop, exit });
        }
        let cleanup = self.cleanup.as_ref().expect("cleanup result was retained");
        let mut failures = Vec::new();
        if let Err(error) = &cleanup.stop {
            failures.push(format!("stop request: {error}"));
        }
        match &cleanup.exit {
            Ok(status) if status.success() => {}
            Ok(status) => failures.push(format!("actual host exit: {status}")),
            Err(error) => failures.push(format!("exit observation failed: {error}")),
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }
}
impl Drop for FacadeFixture {
    fn drop(&mut self) {
        // Explicit cleanup callers already receive both outcomes; unwind cleanup preserves the primary panic.
        if self.cleanup.is_none()
            && let Err(error) = self.stop()
        {
            if std::thread::panicking() {
                eprintln!("Fixture cleanup also failed: {error}");
            } else {
                panic!("Fixture cleanup failed: {error}");
            }
        }
    }
}

#[test]
fn actual_facade_refuses_committed_close_for_its_privately_prepared_launch() {
    let (mut fixture, attempt) = FacadeFixture::new();
    let prepared = fixture.prepare(&attempt);
    let mut frame = FramedReader::new(Cursor::new(prepared.bytes().to_vec()))
        .read()
        .unwrap()
        .unwrap();
    let Body::Command(command) = &mut frame.body else {
        panic!("prepared command frame required")
    };
    assert!(matches!(command.action, Action::Start { .. }));
    command.action = Action::Close;
    let substituted = alondra_agent_protocol::encode(&frame).unwrap();
    let committed = fixture.commit_command(attempt, substituted);
    assert!(
        matches!(
            fixture.worker.submit(&committed),
            Err(AdapterError::InvalidObservation)
        ),
        "the actual facade must reject persisted bytes that differ from its preparation"
    );
    assert!(
        !fixture
            .capture
            .with_file_name("native-capture.json.starts")
            .exists(),
        "a rejected technical command cannot launch the native SDK child"
    );
    fixture.stop().unwrap();
}
fn facade_ack_scenario(substitute_raw: bool) {
    let (mut fixture, attempt) = FacadeFixture::new();
    let prepared = fixture.prepare(&attempt);
    let committed = fixture.commit_command(attempt, prepared.bytes().to_vec());
    assert_eq!(fixture.worker.submit(&committed).unwrap(), Handoff::Offered);
    let raw = rusqlite::Transaction::new_unchecked(
        &fixture.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(
        raw,
        AuthorityScope::new("sdk-epoch".into()).unwrap(),
    )
    .unwrap();
    tx.record_committed_receipt(committed, alondra_store::DispatchReceipt::Handed)
        .unwrap();
    tx.commit().unwrap();
    let received = fixture.worker.poll().unwrap().unwrap();
    assert!(!received.evidence.is_empty());
    let raw = rusqlite::Transaction::new_unchecked(
        &fixture.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(
        raw,
        AuthorityScope::new("sdk-epoch".into()).unwrap(),
    )
    .unwrap();
    if substitute_raw {
        tx.accept_owned_observation(
            &received.execution,
            received.owner_generation,
            received.cursor,
            alondra_store::CommandEvidence::new(b"substituted raw at same cursor".to_vec())
                .unwrap(),
        )
        .unwrap();
        tx.put_normalization_checkpoint(
            &received.execution,
            received.cursor,
            None,
            b"SQL-only checkpoint".to_vec(),
        )
        .unwrap();
        tx.settle_observation(&received.execution, received.cursor, 0).unwrap();
    } else {
        let pending = alondra_agent_application::stage_observation(
            &mut tx,
            fixture.worker.binding(),
            fixture.worker.owner_generation().unwrap(),
            &received,
            fixture.worker.normalizer(),
        )
        .unwrap();
        fixture_controller::record_facts(&mut tx, pending).unwrap();
    }
    let committed = tx.commit().unwrap().observations.pop().unwrap();
    if substitute_raw {
        assert!(
            matches!(
                fixture.worker.acknowledge(&committed),
                Err(AdapterError::InvalidObservation)
            ),
            "SQL evidence for different bytes cannot ACK this received cursor"
        );
        assert!(
            fixture.worker.pending.is_some(),
            "rejected ACK retains the exact received evidence"
        );
    } else {
        fixture.worker.acknowledge(&committed).unwrap();
        assert!(
            fixture.worker.pending.is_none(),
            "only successful exact ACK clears the retained receipt"
        );
    }
    fixture.stop().unwrap();
}
#[test]
fn actual_facade_refuses_committed_substitution_for_received_raw_cursor() {
    facade_ack_scenario(true);
}
#[test]
fn actual_facade_accepts_exact_committed_raw_checkpoint_ack() {
    facade_ack_scenario(false);
}

#[test]
fn actual_facade_rejects_adapter_state_from_another_execution() {
    let (mut first, owner, own_state, _permit) =
        FacadeFixture::staged_owner("sdk-epoch", "sdk-execution");
    let (mut second, second_owner, foreign_state, _permit) =
        FacadeFixture::staged_owner("sdk-epoch", "another-execution");
    assert_eq!(owner.scope(), second_owner.scope());
    assert_eq!(owner.binding(), second_owner.binding());
    assert_ne!(
        owner.execution(),
        second_owner.execution(),
        "both states are genuine but their executions differ"
    );
    assert!(own_state.checkpoint().is_none());
    assert!(
        foreign_state.checkpoint().is_none(),
        "the existing checkpoint refusal must not mask state correlation"
    );
    let result = first.worker.install_owner(owner, foreign_state);
    assert!(
        matches!(result, Err(AdapterError::InvalidObservation)),
        "owner installation must reject genuine AdapterState from another execution before launching a host"
    );
    assert!(first.worker.adapter.host.is_none());
    assert!(
        !first
            .worker
            .adapter
            .configuration
            .state_directory
            .join("host.lock")
            .exists()
    );
    first.stop().unwrap();
    second.stop().unwrap();
}
#[test]
fn actual_facade_rejects_foreign_epoch_input_metadata_with_equal_execution_and_input() {
    let (mut first, attempt) = FacadeFixture::new();
    let (mut second, second_owner, _state, _permit) =
        FacadeFixture::staged_owner("foreign-sdk-epoch", "sdk-execution");
    assert_eq!(
        attempt.execution(),
        second_owner.execution(),
        "execution correlation alone must not mask the data epoch mismatch"
    );
    assert_eq!(attempt.binding(), second_owner.binding());
    assert_ne!(attempt.scope(), second_owner.scope());
    let raw = rusqlite::Transaction::new_unchecked(
        &second.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(raw, second.scope.clone()).unwrap();
    let result = InputMetadata::with_execution(
        &mut tx,
        attempt.execution(),
        attempt.input().id(),
        |metadata| {
            assert_eq!(metadata.execution(), attempt.execution());
            assert_eq!(metadata.input_id(), attempt.input().id());
            first.worker.prepare(&attempt, metadata)
        },
    );
    assert!(matches!(tx.commit(), Err(alondra_store::Error::Poisoned)), "the caught preparation failure cannot commit");
    assert!(
        matches!(result, Err(alondra_agent_application::Error::Adapter(AdapterError::InvalidObservation))),
        "preparation must reject same-stamp metadata loaded from another genuine data epoch"
    );
    assert!(first.worker.prepared.is_none());
    assert!(
        !first
            .capture
            .with_file_name("native-capture.json.starts")
            .exists()
    );
    first.stop().unwrap();
    second.stop().unwrap();
}

#[test]
fn facade_fixture_reported_stop_error_still_observes_actual_exit_before_directory_drop() {
    let (mut fixture, _attempt) = FacadeFixture::new();
    let root = fixture._directory.to_path_buf();
    assert!(root.exists());
    let error = fixture.finish_cleanup(true).unwrap_err();
    assert!(error.contains("fixture-reported error after actual stop request"));
    let cleanup = fixture.cleanup.as_ref().unwrap();
    assert!(cleanup.stop.is_err());
    assert!(
        cleanup.exit.as_ref().unwrap().success(),
        "a reported stop error must not skip the real wait barrier"
    );
    assert!(
        root.exists(),
        "the Directory guard remains owned through observed actual exit"
    );
    drop(fixture);
    assert!(
        !root.exists(),
        "only the structured owner drop cleans the closed fixture root"
    );
}

fn revised_configuration_binding() -> ProviderBinding {
    let original = binding();
    ProviderBinding::new(
        original.id().into(),
        original.driver().into(),
        original.instance().into(),
        original.configuration().into(),
        original.configuration_revision() + 1,
        original.capability_revision(),
        original.capabilities().clone(),
        original.credential().into(),
        original.credential_generation(),
    )
    .unwrap()
}

#[test]
fn actual_facade_rejects_stale_state_generation_for_the_same_execution() {
    let (mut fixture, first_owner, first_state, _permit) =
        FacadeFixture::staged_owner("sdk-epoch", "sdk-execution");
    let raw = rusqlite::Transaction::new_unchecked(
        &fixture.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(raw, fixture.scope.clone()).unwrap();
    tx.reserve_owner(first_owner.execution(), first_owner.generation()).unwrap();
    let second_owner = tx.commit().unwrap().owners.pop().unwrap();
    assert_eq!(first_owner.scope(), second_owner.scope());
    assert_eq!(first_owner.execution(), second_owner.execution());
    assert_eq!(first_owner.binding(), second_owner.binding());
    assert_eq!(second_owner.generation(), first_owner.generation() + 1);
    assert!(first_state.checkpoint().is_none());
    let result = fixture.worker.install_owner(second_owner, first_state);
    assert!(
        matches!(result, Err(AdapterError::InvalidObservation)),
        "same execution cannot install AdapterState from an earlier committed owner generation"
    );
    fixture.assert_no_host_effects();
    fixture.stop().unwrap();
}

#[test]
fn actual_facade_rejects_foreign_state_epoch_with_equal_execution_binding_and_generation() {
    let (mut first, owner, _own_state, _permit) =
        FacadeFixture::staged_owner("sdk-epoch", "sdk-execution");
    let (mut second, foreign_owner, foreign_state, _permit) =
        FacadeFixture::staged_owner("foreign-sdk-epoch", "sdk-execution");
    assert_eq!(owner.execution(), foreign_owner.execution());
    assert_eq!(owner.binding(), foreign_owner.binding());
    assert_eq!(owner.generation(), foreign_owner.generation());
    assert_ne!(owner.scope(), foreign_owner.scope());
    assert!(foreign_state.checkpoint().is_none());
    let result = first.worker.install_owner(owner, foreign_state);
    assert!(
        matches!(result, Err(AdapterError::InvalidObservation)),
        "equal execution and generation cannot install AdapterState from another data epoch"
    );
    first.assert_no_host_effects();
    first.stop().unwrap();
    second.stop().unwrap();
}

#[test]
fn actual_facade_rejects_foreign_state_binding_with_equal_epoch_execution_and_generation() {
    let (mut first, owner, _own_state, _permit) =
        FacadeFixture::staged_owner("sdk-epoch", "sdk-execution");
    let (mut second, foreign_owner, foreign_state, _permit) = FacadeFixture::staged_runtime(
        "sdk-epoch",
        "sdk-execution",
        revised_configuration_binding(),
        "workspace",
    );
    assert_eq!(owner.scope(), foreign_owner.scope());
    assert_eq!(owner.execution(), foreign_owner.execution());
    assert_eq!(owner.generation(), foreign_owner.generation());
    assert_eq!(owner.binding(), first.worker.binding());
    assert_eq!(owner.binding().id(), foreign_owner.binding().id());
    assert_eq!(owner.binding().driver(), foreign_owner.binding().driver());
    assert_eq!(owner.binding().instance(), foreign_owner.binding().instance());
    assert_eq!(owner.binding().configuration(), foreign_owner.binding().configuration());
    assert_eq!(owner.binding().capability_revision(), foreign_owner.binding().capability_revision());
    assert_eq!(owner.binding().capabilities(), foreign_owner.binding().capabilities());
    assert_eq!(owner.binding().credential(), foreign_owner.binding().credential());
    assert_eq!(owner.binding().credential_generation(), foreign_owner.binding().credential_generation());
    assert_eq!(foreign_owner.binding().configuration_revision(), owner.binding().configuration_revision() + 1);
    assert!(foreign_state.checkpoint().is_none());
    let result = first.worker.install_owner(owner, foreign_state);
    assert!(
        matches!(result, Err(AdapterError::InvalidObservation)),
        "matching owner identifiers cannot install state from another immutable configuration revision"
    );
    first.assert_no_host_effects();
    first.stop().unwrap();
    second.stop().unwrap();
}

#[test]
fn actual_facade_rejects_foreign_metadata_binding_with_equal_epoch_execution_and_input() {
    let (mut first, attempt) = FacadeFixture::new();
    let (mut second, foreign_owner, _state, _permit) = FacadeFixture::staged_runtime(
        "sdk-epoch",
        "sdk-execution",
        revised_configuration_binding(),
        "workspace",
    );
    assert_eq!(attempt.scope(), foreign_owner.scope());
    assert_eq!(attempt.execution(), foreign_owner.execution());
    assert_eq!(attempt.binding().id(), foreign_owner.binding().id());
    assert_ne!(attempt.binding(), foreign_owner.binding());
    assert_eq!(attempt.binding(), first.worker.binding());
    let raw = rusqlite::Transaction::new_unchecked(
        &second.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(raw, second.scope.clone()).unwrap();
    let result = InputMetadata::with_execution(
        &mut tx,
        attempt.execution(),
        attempt.input().id(),
        |metadata| {
            assert_eq!(metadata.execution(), attempt.execution());
            assert_eq!(metadata.input_id(), attempt.input().id());
            assert_eq!(metadata.binding().unwrap(), *foreign_owner.binding());
            first.worker.prepare(&attempt, metadata)
        },
    );
    assert!(matches!(tx.commit(), Err(alondra_store::Error::Poisoned)), "the caught preparation failure cannot commit");
    assert!(
        matches!(result, Err(alondra_agent_application::Error::Adapter(AdapterError::InvalidObservation))),
        "same epoch and input identifiers cannot prepare native metadata from another binding revision"
    );
    assert!(first.worker.prepared.is_none());
    assert!(!first.capture.with_file_name("native-capture.json.starts").exists());
    first.stop().unwrap();
    second.stop().unwrap();
}

#[test]
fn actual_facade_refuses_configured_workspace_different_from_the_claimed_owned_directory() {
    let (mut fixture, owner, state, permit) = FacadeFixture::staged_runtime(
        "sdk-epoch",
        "sdk-execution",
        binding(),
        "another-workspace",
    );
    let configured = fixture.worker.adapter.configuration.native_directory.clone();
    fixture.worker.install_owner(owner, state).unwrap();
    assert!(fixture.worker.adapter.host.is_some(), "a valid control owner may launch an inert host");
    let raw = rusqlite::Transaction::new_unchecked(
        &fixture.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(raw, fixture.scope.clone()).unwrap();
    tx.claim(permit).unwrap();
    let attempt = tx.commit().unwrap().attempts.pop().unwrap();
    assert_eq!(attempt.scope(), &fixture.scope);
    assert_eq!(attempt.binding(), fixture.worker.binding());
    let Some(Placement::LocalDirectory(claimed)) = attempt.placement() else {
        panic!("a real local-directory claim is required");
    };
    assert_ne!(Path::new(claimed), configured.as_path());
    assert!(Path::new(claimed).is_dir());
    assert!(configured.is_dir());
    let raw = rusqlite::Transaction::new_unchecked(
        &fixture.connection,
        rusqlite::TransactionBehavior::Immediate,
    )
    .unwrap();
    let mut tx = alondra_store::LifecycleTransaction::attach(raw, fixture.scope.clone()).unwrap();
    let result = InputMetadata::with_attempt(&mut tx, &attempt, |metadata| {
        fixture.worker.prepare(&attempt, metadata)
    });
    assert!(matches!(tx.commit(), Err(alondra_store::Error::Poisoned)), "the caught preparation failure cannot commit");
    assert!(
        matches!(result, Err(alondra_agent_application::Error::Adapter(AdapterError::Unsupported))),
        "the native query must not use a configured directory different from its genuine claimed workspace"
    );
    assert!(fixture.worker.prepared.is_none());
    assert!(!fixture.capture.with_file_name("native-capture.json.starts").exists());
    fixture.stop().unwrap();
}

fn control_binding() -> ProviderBinding {
    use alondra_provider_contract::Capability;
    ProviderBinding::new(
        "sdk-control-binding".into(),
        "claude-agent-sdk".into(),
        "sdk-instance".into(),
        "sdk-fixture".into(),
        1,
        2,
        BTreeSet::from([Capability::OfferInput, Capability::Interrupt, Capability::SetModel, Capability::SetPermissionMode, Capability::McpStatus]),
        "fixture-no-login".into(),
        1,
    )
    .unwrap()
}
impl FacadeFixture {
    fn transaction(&self) -> alondra_store::LifecycleTransaction<'_> {
        let raw = rusqlite::Transaction::new_unchecked(&self.connection, rusqlite::TransactionBehavior::Immediate).unwrap();
        alondra_store::LifecycleTransaction::attach(raw, self.scope.clone()).unwrap()
    }
    /// A launched turn the fake Claude holds open, with every host event committed and acknowledged.
    fn working() -> Self {
        let (mut fixture, owner, state, permit) = Self::staged_runtime("sdk-epoch", "sdk-execution", control_binding(), "workspace");
        fixture.worker.install_owner(owner, state).unwrap();
        let mut tx = fixture.transaction();
        tx.claim(permit).unwrap();
        let attempt = tx.commit().unwrap().attempts.pop().unwrap();
        let prepared = fixture.prepare(&attempt);
        let committed = fixture.commit_command(attempt, prepared.bytes().to_vec());
        assert_eq!(fixture.worker.submit(&committed).unwrap(), Handoff::Offered);
        let mut tx = fixture.transaction();
        tx.record_committed_receipt(committed, alondra_store::DispatchReceipt::Handed).unwrap();
        tx.commit().unwrap();
        while fixture.transaction().load(&"sdk-task".into()).unwrap().activity() != ActivityView::Working {
            let received = fixture.worker.poll().unwrap().unwrap();
            let mut tx = fixture.transaction();
            let pending = alondra_agent_application::stage_observation(
                &mut tx,
                fixture.worker.binding(),
                fixture.worker.owner_generation().unwrap(),
                &received,
                fixture.worker.normalizer(),
            )
            .unwrap();
            fixture_controller::record_facts(&mut tx, pending).unwrap();
            let committed = tx.commit().unwrap().observations.pop().unwrap();
            fixture.worker.acknowledge(&committed).unwrap();
        }
        fixture
    }
    /// The domain asks for `kind`; the commit issues its permit and the claim its attempt.
    fn claimed_control(&mut self, id: &str, kind: alondra_store::Control) -> alondra_store::ControlAttempt {
        let task: alondra_foundation::TaskId = "sdk-task".into();
        let control = alondra_foundation::OperationId::new(id).unwrap();
        let mut tx = self.transaction();
        let revision = tx.load(&task).unwrap().revision();
        tx.apply(&task, revision, id, alondra_domain::Event::RequestControl { control: control.clone(), kind: kind.clone() }).unwrap();
        tx.stage_control(&task, &control, &kind).unwrap();
        let permit = tx.commit().unwrap().control_permits.into_iter().find(|permit| permit.control() == &control).unwrap();
        let mut tx = self.transaction();
        assert!(tx.claim_control(permit).unwrap());
        tx.commit().unwrap().control_attempts.pop().unwrap()
    }
    fn commit_control(&mut self, attempt: alondra_store::ControlAttempt, bytes: Vec<u8>) -> alondra_store::CommittedControl {
        let mut tx = self.transaction();
        tx.record_owned_control(attempt, self.worker.owner_generation().unwrap(), alondra_store::CommandEvidence::new(bytes).unwrap()).unwrap();
        tx.commit().unwrap().control_commands.pop().unwrap()
    }
    fn received_controls(&self) -> Vec<Value> {
        fs::read_to_string(self.capture.with_file_name("native-capture.json.controls"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
}
fn action_of(prepared: &PreparedCommand) -> (alondra_foundation::CommandId, Action) {
    let frame = FramedReader::new(Cursor::new(prepared.bytes().to_vec())).read().unwrap().unwrap();
    let Body::Command(command) = frame.body else { panic!("a prepared control is a command") };
    (command.command_id, command.action)
}

// Each control of the domain is prepared as its own host action under the committed attempt's
// command id, sent only with its exact committed bytes, and reaches the native Claude.
#[test]
fn actual_facade_prepares_and_submits_each_control_as_its_host_action() {
    use alondra_agent_protocol::PermissionMode as WireMode;
    use alondra_store::{Control, PermissionMode};
    let mut fixture = FacadeFixture::working();
    let turn = fixture.transaction().load(&"sdk-task".into()).unwrap().turn().cloned();
    for (id, kind, action) in [
        ("control-model", Control::SetModel { model: "fixture-model".into() }, Action::SetModel { model: "fixture-model".into() }),
        ("control-mode", Control::SetPermissionMode { mode: PermissionMode::DontAsk }, Action::SetPermissionMode { mode: WireMode::DontAsk }),
        ("control-mcp", Control::ReadMcpStatus, Action::McpStatus),
        ("control-interrupt", Control::Interrupt { turn: turn.clone() }, Action::Interrupt),
    ] {
        let attempt = fixture.claimed_control(id, kind);
        let prepared = fixture.worker.prepare_control(&attempt).unwrap();
        assert_eq!(action_of(&prepared), (attempt.command_id(), action), "{id}");
        assert!(matches!(fixture.worker.prepare_control(&attempt), Err(AdapterError::InvalidObservation)), "one prepared control at a time");
        let committed = fixture.commit_control(attempt, prepared.bytes().to_vec());
        assert_eq!(fixture.worker.submit_control(&committed).unwrap(), Handoff::Offered, "{id}");
        assert!(matches!(fixture.worker.submit_control(&committed), Err(AdapterError::InvalidObservation)), "a control is sent once");
        let mut tx = fixture.transaction();
        tx.record_control_receipt(committed, alondra_store::DispatchReceipt::Handed).unwrap();
        tx.commit().unwrap();
    }
    let received = fixture.received_controls();
    let subtypes: Vec<&str> = received.iter().map(|control| control["subtype"].as_str().unwrap()).collect();
    assert_eq!(subtypes, ["set_model", "set_permission_mode", "mcp_status", "interrupt"]);
    assert_eq!((received[0]["model"].as_str(), received[1]["mode"].as_str()), (Some("fixture-model"), Some("dontAsk")));
    fixture.stop().unwrap();
}

// The committed evidence must be exactly what the worker prepared: a substituted action is
// refused before anything is written to the host.
#[test]
fn actual_facade_refuses_a_committed_control_that_differs_from_its_preparation() {
    let mut fixture = FacadeFixture::working();
    let attempt = fixture.claimed_control("control-model", alondra_store::Control::SetModel { model: "fixture-model".into() });
    let prepared = fixture.worker.prepare_control(&attempt).unwrap();
    let mut frame = FramedReader::new(Cursor::new(prepared.bytes().to_vec())).read().unwrap().unwrap();
    let Body::Command(command) = &mut frame.body else { panic!("prepared command frame required") };
    command.action = Action::SetPermissionMode { mode: alondra_agent_protocol::PermissionMode::DontAsk };
    let committed = fixture.commit_control(attempt, alondra_agent_protocol::encode(&frame).unwrap());
    assert!(matches!(fixture.worker.submit_control(&committed), Err(AdapterError::InvalidObservation)));
    assert!(fixture.received_controls().is_empty(), "nothing reached Claude");
    fixture.stop().unwrap();
}
