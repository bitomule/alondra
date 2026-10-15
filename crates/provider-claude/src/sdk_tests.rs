//! Actual SDK child-process fixture. The wrapper observes/delegates; it creates no facts.
use super::*;
use alondra_agent_application::Error as ControllerError;
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
