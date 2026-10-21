use alondra_agent_protocol::*;
use alondra_foundation::{CommandId, MessageUuid};
use serde_json::{Value, json};
use std::io::{self, Cursor, Read, Write};

const START: &[u8] = include_bytes!("../fixtures/start.json");
const DECISION: &[u8] = include_bytes!("../fixtures/decision.json");
const CATALOG: &str = include_str!("../fixtures/sdk-errors.json");
const HELLO: &[u8] = include_bytes!("../fixtures/hello.json");
const RECEIPT: &[u8] = include_bytes!("../fixtures/receipt.json");
const EVENT: &[u8] = include_bytes!("../fixtures/event.json");

fn frame(bytes: &[u8]) -> Frame {
    serde_json::from_slice(bytes).unwrap()
}
fn command() -> Command {
    match frame(START).body {
        Body::Command(c) => c,
        _ => panic!("fixture command"),
    }
}
fn event() -> Event {
    match frame(EVENT).body {
        Body::Event(e) => e,
        _ => panic!("fixture event"),
    }
}
fn receipt() -> Receipt {
    match frame(RECEIPT).body {
        Body::Receipt(r) => r,
        _ => panic!("fixture receipt"),
    }
}
fn raw_frame(bytes: &[u8]) -> Vec<u8> {
    let mut result = (bytes.len() as u32).to_be_bytes().to_vec();
    result.extend_from_slice(bytes);
    result
}

#[test]
fn shared_wire_fixtures_roundtrip_and_exact_command_digest() {
    for bytes in [START, DECISION, HELLO, RECEIPT, EVENT] {
        let expected = frame(bytes);
        let encoded = encode(&expected).unwrap();
        assert_eq!(&encoded[4..], bytes, "shared fixture bytes changed");
        assert_eq!(
            FramedReader::new(Cursor::new(encoded)).read().unwrap(),
            Some(expected)
        );
    }
    assert_eq!(
        command_digest(&command()).unwrap(),
        receipt().command_sha256
    );
    assert_eq!(
        receipt().command_sha256,
        "4cb067672d9a45e3f61fbdd00612a2b8cb400441f7ea6482c028513ce8a45a2b"
    );
}

#[test]
fn digest_covers_original_bytes_not_a_receivers_json_serialization() {
    let expected = frame(DECISION);
    let mut outgoing = FramedReader::new(Cursor::new(raw_frame(DECISION)));
    let (decoded, original_digest) = outgoing.read_with_digest().unwrap().unwrap();
    assert_eq!(decoded, expected);
    if let Body::Command(command) = decoded.body {
        assert_eq!(command_digest(&command).unwrap(), original_digest);
    } else {
        panic!("decision fixture");
    }
    // Reordered keys and whitespace are valid JSON, but are different wire payloads.
    let reordered =
        serde_json::to_vec_pretty(&serde_json::from_slice::<Value>(DECISION).unwrap()).unwrap();
    let (decoded, reordered_digest) = FramedReader::new(Cursor::new(raw_frame(&reordered)))
        .read_with_digest()
        .unwrap()
        .unwrap();
    assert_eq!(decoded, expected);
    assert_ne!(reordered_digest, original_digest);
    assert_eq!(
        original_digest,
        "670845a722a6d297426d0ba0394d339ff07a200fc42319ba08af82fc53a5d38f"
    );
}

#[test]
fn inspected_sdk_error_catalog_matches_independent_baseline_fixture() {
    let catalog: Value = serde_json::from_str(CATALOG).unwrap();
    for (surface, name) in [
        (FailureSurface::Assistant, "SDKAssistantMessageError"),
        (FailureSurface::Startup, "SDKStartupFailureReason"),
        (FailureSurface::Terminal, "TerminalReason"),
        (FailureSurface::Exit, "ExitReason"),
        (FailureSurface::Hook, "SDKHookResponseOutcome"),
        (FailureSurface::RateLimit, "SDKRateLimitStatus"),
    ] {
        assert_eq!(
            serde_json::to_value(known_codes(surface)).unwrap(),
            catalog[name]
        );
    }
    let mut result = catalog["SDKResultErrorSubtype"].as_array().unwrap().clone();
    result.push(json!("success_is_error"));
    assert_eq!(
        serde_json::to_value(known_codes(FailureSurface::Result)).unwrap(),
        Value::Array(result)
    );
}

struct Chunks {
    bytes: Cursor<Vec<u8>>,
    max: usize,
    interrupted: bool,
}
impl Read for Chunks {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.interrupted {
            self.interrupted = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        let size = output.len().min(self.max);
        self.bytes.read(&mut output[..size])
    }
}

#[test]
fn fragmented_coalesced_and_interrupted_reads_keep_frame_boundaries() {
    let mut bytes = raw_frame(HELLO);
    bytes.extend(raw_frame(START));
    bytes.extend(raw_frame(EVENT));
    for size in 1..17 {
        let mut reader = FramedReader::new(Chunks {
            bytes: Cursor::new(bytes.clone()),
            max: size,
            interrupted: true,
        });
        for expected in [HELLO, START, EVENT] {
            assert_eq!(reader.read().unwrap(), Some(frame(expected)));
        }
        assert!(reader.read().unwrap().is_none());
    }
}

#[test]
fn every_truncated_frame_fails_closed_and_poisons_reader() {
    let bytes = raw_frame(START);
    assert!(
        FramedReader::new(Cursor::new(Vec::<u8>::new()))
            .read()
            .unwrap()
            .is_none()
    );
    for end in 1..bytes.len() {
        let mut reader = FramedReader::new(Cursor::new(&bytes[..end]));
        if end < 4 {
            assert!(matches!(reader.read(), Err(Error::TruncatedHeader)));
        } else {
            assert!(matches!(reader.read(), Err(Error::TruncatedPayload)));
        }
        assert!(matches!(reader.read(), Err(Error::Poisoned)));
    }
}

struct HeaderOnly {
    header: Cursor<[u8; 4]>,
}
impl Read for HeaderOnly {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        assert!(
            (self.header.position() as usize) < 4,
            "oversized body was read"
        );
        self.header.read(output)
    }
}

#[test]
fn oversized_prefix_is_rejected_before_body_read_or_allocation() {
    for length in [(MAX_FRAME_BYTES + 1) as u32, u32::MAX] {
        let mut reader = FramedReader::new(HeaderOnly {
            header: Cursor::new(length.to_be_bytes()),
        });
        assert!(matches!(reader.read(), Err(Error::FrameTooLarge)));
        assert!(matches!(reader.read(), Err(Error::Poisoned)));
    }
}

#[test]
fn unknown_wire_types_versions_fields_and_malformed_json_fail_closed() {
    let mut unknown_version = serde_json::from_slice::<Value>(START).unwrap();
    unknown_version["protocol_version"] = json!(2);
    let mut unknown_type = serde_json::from_slice::<Value>(START).unwrap();
    unknown_type["body"]["type"] = json!("approve_everything");
    let mut unknown_action = serde_json::from_slice::<Value>(START).unwrap();
    unknown_action["body"]["action"]["type"] = json!("resume_unowned");
    let mut unknown_field = serde_json::from_slice::<Value>(START).unwrap();
    unknown_field["body"]["action"]["allow_bypass"] = json!(true);
    let cases = [
        serde_json::to_vec(&unknown_version).unwrap(),
        serde_json::to_vec(&unknown_type).unwrap(),
        serde_json::to_vec(&unknown_action).unwrap(),
        serde_json::to_vec(&unknown_field).unwrap(),
        b"{oops".to_vec(),
        b"{\"protocol_version\":1,\"protocol_version\":1}".to_vec(),
        vec![0xff],
        Vec::new(),
    ];
    for case in cases {
        let mut reader = FramedReader::new(Cursor::new(raw_frame(&case)));
        assert!(reader.read().is_err());
        assert!(matches!(reader.read(), Err(Error::Poisoned)));
    }
}

#[test]
fn outgoing_payload_is_bounded_before_writing_any_bytes() {
    let mut large = command();
    large.action = Action::Offer {
        input_id: MessageUuid::new("00000000-0000-4000-8000-000000000002").unwrap(),
        prompt: "x".repeat(MAX_FRAME_BYTES),
    };
    let mut bytes = Vec::new();
    assert!(matches!(
        FramedWriter::new(&mut bytes).write(&Frame::new(Body::Command(large))),
        Err(Error::FrameTooLarge)
    ));
    assert!(bytes.is_empty());
    let mut sdk = event();
    if let Observation::Sdk { payload, .. } = &mut sdk.observation {
        *payload = json!({"nested": "x".repeat(MAX_FRAME_BYTES * 2)});
    }
    assert!(matches!(
        encode(&Frame::new(Body::Event(sdk))),
        Err(Error::FrameTooLarge)
    ));
}

struct FailingWriter {
    calls: usize,
    written: usize,
    fail_flush: bool,
}
impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.written != 0 {
            return Err(io::ErrorKind::BrokenPipe.into());
        }
        let size = if self.fail_flush { bytes.len() } else { 3 };
        self.written += size;
        Ok(size)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.fail_flush {
            Err(io::ErrorKind::BrokenPipe.into())
        } else {
            Ok(())
        }
    }
}

#[test]
fn partial_write_and_flush_failure_poison_writer_without_retry() {
    for fail_flush in [false, true] {
        let mut target = FailingWriter {
            calls: 0,
            written: 0,
            fail_flush,
        };
        {
            let mut writer = FramedWriter::new(&mut target);
            assert!(matches!(writer.write(&frame(START)), Err(Error::Io(_))));
            assert!(matches!(writer.write(&frame(START)), Err(Error::Poisoned)));
        }
        assert_eq!(target.calls, if fail_flush { 1 } else { 2 });
    }
}

#[test]
fn handshake_requires_owned_host_sdk_version_and_every_capability() {
    let hello = match frame(HELLO).body {
        Body::Hello(h) => h,
        _ => panic!(),
    };
    let expected_fence = hello.fence.clone();
    let required = ["streaming_input", "exact_decisions"];
    let expected = HostExpectation {
        fence: &expected_fence,
        host_release: "fixture-release",
        host_sha256: &"a".repeat(64),
        required_capabilities: &required,
    };
    assert_eq!(hello.negotiate(&expected).unwrap(), PROTOCOL_VERSION);
    let mut wrong = hello.clone();
    wrong.protocol_versions = vec![2];
    assert!(matches!(
        wrong.negotiate(&expected),
        Err(Error::UnsupportedVersion)
    ));
    let mut wrong = hello.clone();
    wrong.capabilities.pop();
    assert!(matches!(
        wrong.negotiate(&expected),
        Err(Error::MissingCapability)
    ));
    let mut wrong = hello.clone();
    wrong.fence.account_id = "other-account".into();
    assert!(matches!(
        wrong.negotiate(&expected),
        Err(Error::FenceMismatch)
    ));
    for field in ["release", "hash", "sdk"] {
        let mut wrong = hello.clone();
        match field {
            "release" => wrong.host_release = "other".into(),
            "hash" => wrong.host_sha256 = "b".repeat(64),
            _ => wrong.sdk_version = "0.3.301".into(),
        }
        assert!(matches!(
            wrong.negotiate(&expected),
            Err(Error::HostMismatch)
        ));
    }
}

fn mutated_commands() -> Vec<Command> {
    let mut cases = Vec::new();
    macro_rules! change {
        ($field:ident, $value:expr) => {{
            let mut c = command();
            c.fence.$field = $value;
            cases.push(c);
        }};
    }
    change!(profile_id, "other".into());
    change!(account_id, "other".into());
    change!(data_epoch, "other".into());
    change!(host_id, "other".into());
    macro_rules! change {
        ($field:ident, $value:expr) => {{
            let mut c = command();
            let Execution::Task(execution) = &mut c.execution else { panic!("a task's command") };
            execution.$field = $value;
            cases.push(c);
        }};
    }
    let mut batch = command();
    batch.execution = Execution::Orchestrator(OrchestratorExecution {
        lane: OrchestratorLane::Orchestrator,
        run_id: "run".into(),
        conversation_id: "conversation".into(),
        generation: 1,
    });
    cases.push(batch);
    change!(task_id, "other".into());
    change!(agent_id, "other".into());
    change!(conversation_id, "other".into());
    change!(process_instance_id, "other".into());
    change!(generation, 8);
    cases
}

#[test]
fn all_authority_fences_reject_commands_receipts_and_events_atomically() {
    let original = command();
    for changed in mutated_commands() {
        let mut registry =
            CommandRegistry::new(original.fence.clone(), original.execution.clone()).unwrap();
        assert!(registry.record(&changed).is_err());
        assert!(registry.is_empty());
        registry.record(&original).unwrap();
        let mut stale = receipt();
        stale.fence = changed.fence.clone();
        stale.execution = changed.execution.clone();
        assert!(registry.receipt(&stale).is_err());
        assert_eq!(registry.receipt(&receipt()).unwrap(), CommandStatus::New);
        let mut sequence =
            EventSequence::new(original.fence.clone(), original.execution.clone()).unwrap();
        let mut stale = event();
        stale.fence = changed.fence;
        stale.execution = changed.execution;
        assert!(sequence.commit(&stale).is_err());
        assert_eq!(sequence.cursor(), 0);
        assert_eq!(sequence.commit(&event()).unwrap(), SequenceStatus::Next);
    }
}

// A node sends more commands over its life than the registry holds at once: only the ones still
// in flight count, and a resolved one makes room.
#[test]
fn the_command_bound_counts_only_commands_still_in_flight() {
    let original = command();
    let mut registry =
        CommandRegistry::new(original.fence.clone(), original.execution.clone()).unwrap();
    let resolved = |id: String, outcome: ReceiptOutcome| {
        let mut sent = original.clone();
        sent.command_id = CommandId(id);
        let mut answered = receipt();
        answered.command_id = sent.command_id.clone();
        answered.command_sha256 = alondra_agent_protocol::command_digest(&sent).unwrap();
        answered.outcome = outcome;
        (sent, answered)
    };
    for index in 0..300 {
        let (sent, answered) = resolved(format!("resolved-{index}"), ReceiptOutcome::Accepted { phase: ReceiptPhase::Offered });
        assert_eq!(registry.record(&sent).unwrap(), CommandStatus::New, "command {index}");
        registry.receipt(&answered).unwrap();
    }
    assert_eq!(registry.len(), MAX_TRACKED_COMMANDS);
    for index in 0..MAX_TRACKED_COMMANDS {
        let (sent, answered) = resolved(format!("admitted-{index}"), ReceiptOutcome::Accepted { phase: ReceiptPhase::Admitted });
        registry.record(&sent).unwrap();
        registry.receipt(&answered).unwrap();
    }
    let (extra, _) = resolved("one-too-many".into(), ReceiptOutcome::Accepted { phase: ReceiptPhase::Admitted });
    assert!(matches!(registry.record(&extra), Err(Error::CommandCapacity)), "commands whose outcome is unknown stay bounded");
}

#[test]
fn duplicate_command_conflict_capacity_and_receipt_phases_are_explicit() {
    let original = command();
    let mut registry =
        CommandRegistry::new(original.fence.clone(), original.execution.clone()).unwrap();
    assert_eq!(registry.record(&original).unwrap(), CommandStatus::New);
    assert_eq!(
        registry.record(&original).unwrap(),
        CommandStatus::Duplicate
    );
    let mut conflict = original.clone();
    conflict.action = Action::Close;
    assert!(matches!(
        registry.record(&conflict),
        Err(Error::CommandConflict)
    ));
    let mut received = receipt();
    assert_eq!(registry.receipt(&received).unwrap(), CommandStatus::New);
    assert_eq!(
        registry.receipt(&received).unwrap(),
        CommandStatus::Duplicate
    );
    received.outcome = ReceiptOutcome::Accepted {
        phase: ReceiptPhase::Admitted,
    };
    assert!(matches!(
        registry.receipt(&received),
        Err(Error::ReceiptConflict)
    ));
    received.outcome = ReceiptOutcome::Accepted {
        phase: ReceiptPhase::Applied,
    };
    assert_eq!(registry.receipt(&received).unwrap(), CommandStatus::New);
    received.outcome = ReceiptOutcome::Rejected {
        code: "late".into(),
        message: "late".into(),
    };
    assert!(matches!(
        registry.receipt(&received),
        Err(Error::ReceiptConflict)
    ));
    let mut unknown = receipt();
    unknown.command_id = "unknown".into();
    assert!(matches!(
        registry.receipt(&unknown),
        Err(Error::UnknownCommand)
    ));
    let mut wrong_hash = receipt();
    wrong_hash.command_sha256 = "b".repeat(64);
    assert!(matches!(
        registry.receipt(&wrong_hash),
        Err(Error::CommandConflict)
    ));
    for index in 1..MAX_TRACKED_COMMANDS {
        let mut next = original.clone();
        next.command_id = CommandId(format!("command-{index}"));
        registry.record(&next).unwrap();
    }
    assert_eq!(registry.len(), MAX_TRACKED_COMMANDS);
    assert_eq!(
        registry.record(&original).unwrap(),
        CommandStatus::Duplicate
    );
    let mut next = original.clone();
    next.command_id = "room-of-the-resolved".into();
    assert_eq!(registry.record(&next).unwrap(), CommandStatus::New, "the applied original makes room");
    next.command_id = "one-too-many".into();
    assert!(matches!(
        registry.record(&next),
        Err(Error::CommandCapacity)
    ));
}

#[test]
fn cursor_gaps_duplicates_conflicts_and_stale_events_preserve_last_commit() {
    let first = event();
    let mut sequence = EventSequence::new(first.fence.clone(), first.execution.clone()).unwrap();
    let mut future = first.clone();
    future.cursor = 2;
    assert!(matches!(sequence.commit(&future), Err(Error::CursorGap)));
    assert_eq!(sequence.cursor(), 0);
    assert_eq!(sequence.check(&first).unwrap(), SequenceStatus::Next);
    assert_eq!(sequence.cursor(), 0, "checking is not persistence");
    assert_eq!(sequence.commit(&first).unwrap(), SequenceStatus::Next);
    assert_eq!(sequence.commit(&first).unwrap(), SequenceStatus::Duplicate);
    let mut conflict = first.clone();
    conflict.observation = Observation::ProcessExited {
        exit_code: None,
        signal: Some("SIGKILL".into()),
    };
    assert!(matches!(
        sequence.commit(&conflict),
        Err(Error::CursorConflict)
    ));
    assert_eq!(sequence.cursor(), 1);
    assert_eq!(sequence.commit(&future).unwrap(), SequenceStatus::Next);
    assert!(matches!(sequence.commit(&first), Err(Error::StaleCursor)));
    assert_eq!(sequence.cursor(), 2);
}

#[test]
fn unknown_sdk_evidence_survives_roundtrip_but_cannot_authorize_application() {
    let mut evidence = event();
    if let Observation::Sdk {
        kind,
        failures,
        payload,
        ..
    } = &mut evidence.observation
    {
        *kind = SdkEventKind::Unsupported;
        *payload =
            json!({"type":"future_event", "subtype":"future_subtype", "native_field":{"keep":42}});
        failures.clear();
    }
    let encoded = encode(&Frame::new(Body::Event(evidence.clone()))).unwrap();
    assert_eq!(
        FramedReader::new(Cursor::new(encoded))
            .read()
            .unwrap()
            .unwrap()
            .body,
        Body::Event(evidence.clone())
    );
    assert!(matches!(
        evidence.validate_for_application(),
        Err(Error::UnsupportedSdk)
    ));
    if let Observation::Sdk { kind, failures, .. } = &mut evidence.observation {
        *kind = SdkEventKind::Diagnostic;
        failures.push(SdkFailure {
            surface: FailureSurface::Assistant,
            code: "new_provider_error".into(),
            details: json!({"keep":"entire error"}),
        });
    }
    assert!(matches!(
        evidence.validate_for_application(),
        Err(Error::UnsupportedSdk)
    ));
}

#[test]
fn every_inspected_sdk_error_code_and_opaque_details_are_preserved() {
    let surfaces = [
        FailureSurface::Assistant,
        FailureSurface::Startup,
        FailureSurface::Result,
        FailureSurface::Terminal,
        FailureSurface::Exit,
        FailureSurface::Hook,
        FailureSurface::RateLimit,
        FailureSurface::Settings,
        FailureSurface::Mirror,
        FailureSurface::Auth,
        FailureSurface::Control,
        FailureSurface::Iterator,
        FailureSurface::Transport,
    ];
    let expected_counts = [13, 19, 5, 19, 5, 3, 3, 2, 1, 1, 3, 2, 2];
    for (surface, expected_count) in surfaces.into_iter().zip(expected_counts) {
        assert_eq!(known_codes(surface).len(), expected_count);
        for code in known_codes(surface) {
            let failure = SdkFailure {
                surface,
                code: (*code).into(),
                details: json!({"nested":{"cause":["original",42]},"is_error":true}),
            };
            assert!(failure.is_known());
            let mut evidence = event();
            if let Observation::Sdk { failures, .. } = &mut evidence.observation {
                *failures = vec![failure.clone()];
            }
            let encoded = encode(&Frame::new(Body::Event(evidence.clone()))).unwrap();
            let decoded = FramedReader::new(Cursor::new(encoded))
                .read()
                .unwrap()
                .unwrap();
            assert_eq!(decoded.body, Body::Event(evidence.clone()));
            evidence.validate_for_application().unwrap();
        }
    }
    if let Observation::Sdk {
        payload, failures, ..
    } = event().observation
    {
        assert_eq!(payload["subtype"], "success");
        assert_eq!(payload["is_error"], true);
        assert_eq!(payload["usage"]["cost_usd"], json!(0.125));
        assert_eq!(failures[0].code, "success_is_error");
        assert_eq!(failures[1].code, "api_error");
    } else {
        panic!("fixture must retain error-bearing success");
    }
}

#[test]
fn empty_ids_unsafe_numbers_and_nonobject_decisions_are_rejected() {
    let mut empty = command();
    let Execution::Task(execution) = &mut empty.execution else { panic!("a task's command") };
    execution.agent_id.0.clear();
    assert!(encode(&Frame::new(Body::Command(empty))).is_err());
    let mut unsafe_number = command();
    let Execution::Task(execution) = &mut unsafe_number.execution else { panic!("a task's command") };
    execution.generation = MAX_SAFE_INTEGER + 1;
    assert!(encode(&Frame::new(Body::Command(unsafe_number))).is_err());
    let mut zero = event();
    zero.cursor = 0;
    assert!(encode(&Frame::new(Body::Event(zero))).is_err());
    let mut invalid = serde_json::from_slice::<Value>(START).unwrap();
    invalid["body"]["action"] = json!({"type":"respond","request_id":"request","epoch":1,"kind":"permission","tool_use_id":"toolu","decision":{"behavior":"allow","updated_input":[]}});
    assert!(
        FramedReader::new(Cursor::new(raw_frame(
            &serde_json::to_vec(&invalid).unwrap()
        )))
        .read()
        .is_err()
    );
}

#[test]
fn sdk_input_identifiers_match_native_uuid_contract() {
    let original = command();
    original.validate().unwrap();
    for input_id in [
        "input-test",
        "",
        "00000000-0000-4000-8000-00000000000g",
        "00000000000040008000000000000002",
    ] {
        let mut invalid: Value = serde_json::from_slice(START).unwrap();
        invalid["body"]["action"]["input_id"] = json!(input_id);
        assert!(matches!(
            FramedReader::new(Cursor::new(raw_frame(
                &serde_json::to_vec(&invalid).unwrap()
            )))
            .read(),
            Err(Error::Json(_))
        ));
        invalid["body"]["action"] = json!({"type":"offer", "input_id": input_id, "prompt":"next"});
        assert!(matches!(
            FramedReader::new(Cursor::new(raw_frame(
                &serde_json::to_vec(&invalid).unwrap()
            )))
            .read(),
            Err(Error::Json(_))
        ));
    }
    let mut uppercase: Value = serde_json::from_slice(START).unwrap();
    uppercase["body"]["action"]["input_id"] = json!("ABCDEF00-0000-4000-8000-000000000002");
    let frame: Frame = serde_json::from_value(uppercase).unwrap();
    if let Body::Command(Command {
        action: Action::Start { input_id, .. },
        ..
    }) = frame.body
    {
        assert_eq!(input_id.as_str(), "abcdef00-0000-4000-8000-000000000002");
    } else {
        panic!("start command required");
    }
}

#[test]
fn duplicate_field_in_otherwise_valid_frame_is_rejected_specifically() {
    let complete: Frame = serde_json::from_slice(START).unwrap();
    complete.validate().unwrap();
    let valid = std::str::from_utf8(START).unwrap();
    let duplicated = valid.replacen("{", "{\"protocol_version\":1,", 1);
    let mut reader = FramedReader::new(Cursor::new(raw_frame(duplicated.as_bytes())));
    match reader.read() {
        Err(Error::Json(error)) => assert!(
            error
                .to_string()
                .contains("duplicate field `protocol_version`")
        ),
        other => panic!("expected duplicate-field rejection, got {other:?}"),
    }
    assert!(matches!(reader.read(), Err(Error::Poisoned)));
}

#[test]
fn process_exit_requires_both_nullable_evidence_fields() {
    let mut valid: Value = serde_json::from_slice(EVENT).unwrap();
    valid["body"]["observation"] =
        json!({"type":"process_exited", "exit_code":null, "signal":null});
    let bytes = serde_json::to_vec(&valid).unwrap();
    assert!(
        FramedReader::new(Cursor::new(raw_frame(&bytes)))
            .read()
            .unwrap()
            .is_some()
    );
    for missing in ["exit_code", "signal"] {
        let mut invalid = valid.clone();
        invalid["body"]["observation"]
            .as_object_mut()
            .unwrap()
            .remove(missing);
        let mut reader = FramedReader::new(Cursor::new(raw_frame(
            &serde_json::to_vec(&invalid).unwrap(),
        )));
        assert!(
            matches!(reader.read(), Err(Error::Json(_))),
            "missing {missing} must be rejected"
        );
        assert!(matches!(reader.read(), Err(Error::Poisoned)));
    }
}

#[test]
fn native_correlations_allow_absence_but_reject_explicit_null() {
    let mut valid: Value = serde_json::from_slice(EVENT).unwrap();
    valid["body"]["observation"]["native"] = json!({});
    assert!(
        FramedReader::new(Cursor::new(raw_frame(&serde_json::to_vec(&valid).unwrap())))
            .read()
            .unwrap()
            .is_some()
    );
    for key in [
        "message_id",
        "turn_id",
        "tool_use_id",
        "parent_tool_use_id",
        "task_id",
        "run_id",
    ] {
        let mut invalid = valid.clone();
        invalid["body"]["observation"]["native"][key] = Value::Null;
        let mut reader = FramedReader::new(Cursor::new(raw_frame(
            &serde_json::to_vec(&invalid).unwrap(),
        )));
        assert!(
            matches!(reader.read(), Err(Error::Json(_))),
            "explicit null {key} must be rejected"
        );
        assert!(matches!(reader.read(), Err(Error::Poisoned)));
    }
}

#[test]
fn typed_integer_tokens_match_shared_host_corpus_without_restricting_sdk_payloads() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/integer-tokens.json")).unwrap();
    for case in cases {
        let raw = case["json"].as_str().unwrap();
        let accepted = FramedReader::new(Cursor::new(raw_frame(raw.as_bytes())))
            .read()
            .is_ok();
        assert_eq!(
            accepted,
            case["accepted"].as_bool().unwrap(),
            "integer token case {}",
            case["name"]
        );
    }
}

#[test]
fn opaque_objects_reject_duplicate_keys_in_every_json_surface() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/opaque-objects.json")).unwrap();
    let mut mismatches = Vec::new();
    for case in &cases {
        let raw = case["json"].as_str().unwrap();
        let decoded = FramedReader::new(Cursor::new(raw_frame(raw.as_bytes()))).read();
        if decoded.is_ok() != case["accepted"].as_bool().unwrap() {
            mismatches.push(case["name"].as_str().unwrap());
        }
        if case["accepted"].as_bool().unwrap() {
            let expected: Value = serde_json::from_str(raw).unwrap();
            let decoded = decoded.unwrap().unwrap();
            assert_eq!(
                serde_json::to_value(&decoded).unwrap(),
                expected,
                "{}",
                case["name"]
            );
            let encoded = encode(&decoded).unwrap();
            let roundtrip = FramedReader::new(Cursor::new(encoded))
                .read()
                .unwrap()
                .unwrap();
            assert_eq!(
                serde_json::to_value(roundtrip).unwrap(),
                expected,
                "{}",
                case["name"]
            );
        }
    }
    assert!(
        mismatches.is_empty(),
        "opaque object acceptance mismatches: {mismatches:?}"
    );
}

#[test]
fn proven_preoffer_rejection_after_admission_is_terminal_but_not_after_handoff() {
    let command = command();
    let mut registry =
        CommandRegistry::new(command.fence.clone(), command.execution.clone()).unwrap();
    registry.record(&command).unwrap();
    let mut response = receipt();
    response.outcome = ReceiptOutcome::Accepted {
        phase: ReceiptPhase::Admitted,
    };
    assert_eq!(registry.receipt(&response).unwrap(), CommandStatus::New);
    response.outcome = ReceiptOutcome::Rejected {
        code: "config_refused".into(),
        message: "Configuration refused before offering input".into(),
    };
    assert_eq!(registry.receipt(&response).unwrap(), CommandStatus::New);
    assert_eq!(
        registry.receipt(&response).unwrap(),
        CommandStatus::Duplicate
    );
    for phase in [
        ReceiptPhase::Admitted,
        ReceiptPhase::Offered,
        ReceiptPhase::Applied,
    ] {
        response.outcome = ReceiptOutcome::Accepted { phase };
        assert!(matches!(
            registry.receipt(&response),
            Err(Error::ReceiptConflict)
        ));
    }
    for phase in [ReceiptPhase::Offered, ReceiptPhase::Applied] {
        let mut registry =
            CommandRegistry::new(command.fence.clone(), command.execution.clone()).unwrap();
        registry.record(&command).unwrap();
        response.outcome = ReceiptOutcome::Accepted { phase };
        registry.receipt(&response).unwrap();
        response.outcome = ReceiptOutcome::Rejected {
            code: "too_late".into(),
            message: "Offer may already have effects".into(),
        };
        assert!(matches!(
            registry.receipt(&response),
            Err(Error::ReceiptConflict)
        ));
    }
}

#[test]
fn nesting_limit_matches_shared_full_frame_boundary_and_ignores_string_syntax() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/nesting-depth.json")).unwrap();
    for case in cases {
        let raw = case["json"].as_str().unwrap();
        let mut reader = FramedReader::new(Cursor::new(raw_frame(raw.as_bytes())));
        let decoded = reader.read();
        assert_eq!(
            decoded.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "{}",
            case["name"]
        );
        if case["accepted"].as_bool().unwrap() {
            let encoded = encode(&decoded.unwrap().unwrap()).unwrap();
            assert!(FramedReader::new(Cursor::new(encoded)).read().is_ok());
        } else {
            assert!(matches!(reader.read(), Err(Error::Poisoned)));
        }
    }
    assert_eq!(MAX_FRAME_DEPTH, 127);
}

#[test]
fn outgoing_depth_is_bounded_before_transport_or_correlation_mutation() {
    let mut nested = Value::Null;
    for _ in 0..200 {
        nested = Value::Array(vec![nested]);
    }
    let mut sdk = event();
    if let Observation::Sdk { payload, .. } = &mut sdk.observation {
        *payload = json!({"nested": nested.clone()});
    }
    let mut details = event();
    if let Observation::Sdk { failures, .. } = &mut details.observation {
        failures[0].details = nested.clone();
    }
    let mut decision = event();
    decision.observation = Observation::DecisionRequested {
        request_id: "request-depth".into(),
        tool_use_id: Some("tool-depth".into()),
        kind: DecisionKind::Permission,
        payload: json!({"nested": nested.clone()}),
    };
    let mut tool = command();
    tool.action = Action::Respond {
        request_id: "request-depth".into(),
        epoch: 1,
        kind: DecisionKind::Permission,
        tool_use_id: Some("tool-depth".into()),
        decision: Decision::Allow {
            updated_input: json!({"nested": nested}).as_object().unwrap().clone(),
        },
        asked: serde_json::Map::new(),
    };
    let candidates = [
        ("sdk payload", Frame::new(Body::Event(sdk))),
        ("failure details", Frame::new(Body::Event(details))),
        ("decision payload", Frame::new(Body::Event(decision))),
        ("updated input", Frame::new(Body::Command(tool))),
    ];
    let mut mismatches = Vec::new();
    for (name, candidate) in candidates {
        if !matches!(encode(&candidate), Err(Error::FrameTooDeep)) {
            mismatches.push(format!("{name}: encode"));
        }
        let mut bytes = Vec::new();
        if !matches!(
            FramedWriter::new(&mut bytes).write(&candidate),
            Err(Error::FrameTooDeep)
        ) {
            mismatches.push(format!("{name}: writer"));
        }
        if !bytes.is_empty() {
            mismatches.push(format!("{name}: external bytes"));
        }
        match &candidate.body {
            Body::Command(command) => {
                let mut registry =
                    CommandRegistry::new(command.fence.clone(), command.execution.clone()).unwrap();
                if !matches!(registry.record(command), Err(Error::FrameTooDeep)) {
                    mismatches.push(format!("{name}: command digest"));
                }
                if !registry.is_empty() {
                    mismatches.push(format!("{name}: registry mutation"));
                }
            }
            Body::Event(event) => {
                let sequence =
                    EventSequence::new(event.fence.clone(), event.execution.clone()).unwrap();
                if !matches!(sequence.check(event), Err(Error::FrameTooDeep)) {
                    mismatches.push(format!("{name}: event digest"));
                }
                assert_eq!(sequence.cursor(), 0);
            }
            _ => unreachable!(),
        }
    }
    assert!(
        mismatches.is_empty(),
        "unbounded outgoing depth: {mismatches:?}"
    );
}

const CHALLENGE: &[u8] = include_bytes!("../fixtures/challenge.json");
const ATTACH: &[u8] = include_bytes!("../fixtures/attach.json");
const ATTACHED: &[u8] = include_bytes!("../fixtures/attached.json");
const ACKNOWLEDGE: &[u8] = include_bytes!("../fixtures/acknowledge.json");
const OWNER_PROOF: &str = include_str!("../fixtures/owner-proof.json");

fn attach() -> Attach {
    match frame(ATTACH).body {
        Body::Attach(attach) => attach,
        _ => panic!("fixture attach"),
    }
}

#[test]
fn ownership_wire_golden_frames_roundtrip_without_changing_commands() {
    for bytes in [CHALLENGE, ATTACH, ATTACHED, ACKNOWLEDGE] {
        let expected = frame(bytes);
        expected.validate().unwrap();
        let encoded = encode(&expected).unwrap();
        assert_eq!(&encoded[4..], bytes, "ownership fixture bytes changed");
        assert_eq!(
            FramedReader::new(Cursor::new(encoded)).read().unwrap(),
            Some(expected)
        );
    }
    assert_eq!(
        command_digest(&command()).unwrap(),
        "4cb067672d9a45e3f61fbdd00612a2b8cb400441f7ea6482c028513ce8a45a2b"
    );
}

const ATTACH_ORCHESTRATOR: &[u8] = include_bytes!("../fixtures/attach-orchestrator.json");
const OWNER_PROOF_ORCHESTRATOR: &str = include_str!("../fixtures/owner-proof-orchestrator.json");

#[test]
fn an_orchestrator_batch_is_its_own_execution_with_its_own_proof() {
    let expected = frame(ATTACH_ORCHESTRATOR);
    expected.validate().unwrap();
    assert_eq!(&encode(&expected).unwrap()[4..], ATTACH_ORCHESTRATOR, "the batch's wire bytes are its own");
    let Body::Attach(claim) = expected.body else { panic!("fixture attach") };
    let Execution::Orchestrator(batch) = &claim.execution else { panic!("an orchestrator batch, not a task: {:?}", claim.execution) };
    assert_eq!((batch.lane, batch.run_id.as_str(), batch.generation), (OrchestratorLane::Orchestrator, "run-test", 1));
    assert_eq!(serde_json::to_value(OrchestratorLane::Orchestrator).unwrap(), ORCHESTRATOR_LANE);
    let fixture: Value = serde_json::from_str(OWNER_PROOF_ORCHESTRATOR).unwrap();
    let secret = std::array::from_fn(|index| index as u8);
    assert_eq!(owner_proof(&secret, &claim).unwrap(), fixture["proof"]);
    verify_owner_proof(&secret, &claim).unwrap();
    assert_ne!(owner_proof(&secret, &claim).unwrap(), owner_proof(&secret, &attach()).unwrap());
    let raw = std::str::from_utf8(ATTACH_ORCHESTRATOR).unwrap();
    for mixed in [
        raw.replace(r#""lane":"orchestrator""#, r#""lane":"task""#),
        raw.replace(r#""lane":"orchestrator","#, r#""lane":"orchestrator","task_id":"t","#),
        raw.replace(r#""lane":"orchestrator","#, ""),
    ] {
        assert!(serde_json::from_str::<Frame>(&mixed).is_err(), "neither a task nor a batch: {mixed}");
    }
}

#[test]
fn ownership_proof_matches_independent_unicode_golden_and_rejects_wrong_secret() {
    let fixture: Value = serde_json::from_str(OWNER_PROOF).unwrap();
    let secret = std::array::from_fn(|index| index as u8);
    let claim = attach();
    assert_eq!(
        fixture["fixture_secret_hex"],
        "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
    );
    assert_eq!(claim.fence.profile_id, "perfil-東京");
    assert_eq!(claim.fence.account_id, "account/é");
    assert_eq!(owner_proof(&secret, &claim).unwrap(), fixture["proof"]);
    verify_owner_proof(&secret, &claim).unwrap();
    assert!(matches!(
        verify_owner_proof(&[0; 32], &claim),
        Err(Error::Invalid("owner proof"))
    ));
    let mut template = claim;
    template.proof.clear();
    assert_eq!(owner_proof(&secret, &template).unwrap(), fixture["proof"]);
    assert!(verify_owner_proof(&secret, &template).is_err());
}

#[test]
fn ownership_proof_authenticates_every_claim_segment_and_tag() {
    let secret = std::array::from_fn(|index| index as u8);
    let original: Value = serde_json::from_slice(ATTACH).unwrap();
    for pointer in [
        "/body/nonce",
        "/body/host_instance_id",
        "/body/owner_generation",
        "/body/fence/profile_id",
        "/body/fence/account_id",
        "/body/fence/data_epoch",
        "/body/fence/host_id",
        "/body/execution/task_id",
        "/body/execution/agent_id",
        "/body/execution/conversation_id",
        "/body/execution/process_instance_id",
        "/body/execution/generation",
        "/body/host_release",
        "/body/host_sha256",
        "/body/executable_sha256",
        "/body/sdk_version",
        "/body/proof",
    ] {
        let mut value = original.clone();
        let field = value.pointer_mut(pointer).unwrap();
        *field = if let Some(number) = field.as_u64() {
            json!(number + 1)
        } else if pointer.ends_with("sha256")
            || pointer.ends_with("nonce")
            || pointer.ends_with("proof")
        {
            json!("a".repeat(64))
        } else {
            json!(format!("{}x", field.as_str().unwrap()))
        };
        let candidate: Frame = serde_json::from_value(value).unwrap();
        candidate.validate().unwrap();
        let Body::Attach(claim) = candidate.body else {
            panic!("attach");
        };
        assert!(
            matches!(
                verify_owner_proof(&secret, &claim),
                Err(Error::Invalid("owner proof"))
            ),
            "unauthenticated segment: {pointer}"
        );
    }
    let mut first = attach();
    first.fence.profile_id = "ab".into();
    first.fence.account_id = "c".into();
    let mut second = first.clone();
    second.fence.profile_id = "a".into();
    second.fence.account_id = "bc".into();
    assert_ne!(
        owner_proof(&secret, &first).unwrap(),
        owner_proof(&secret, &second).unwrap(),
        "segment boundaries were lost"
    );
}

fn assert_rejected_ownership_value(value: Value) {
    let payload = serde_json::to_vec(&value).unwrap();
    let mut reader = FramedReader::new(Cursor::new(raw_frame(&payload)));
    assert!(reader.read().is_err(), "ownership shape accepted: {value}");
    assert!(matches!(reader.read(), Err(Error::Poisoned)));
}

#[test]
fn ownership_envelopes_reject_unknown_missing_and_unbounded_claims() {
    for bytes in [CHALLENGE, ATTACH, ATTACHED, ACKNOWLEDGE] {
        let original: Value = serde_json::from_slice(bytes).unwrap();
        for key in original["body"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| *key != "type")
        {
            let mut missing = original.clone();
            missing["body"].as_object_mut().unwrap().remove(key);
            assert_rejected_ownership_value(missing);
        }
        let mut extra = original.clone();
        extra["body"]["unexpected"] = json!(true);
        assert_rejected_ownership_value(extra);
        for invalid in [
            json!(""),
            json!("a".repeat(257)),
            json!("bad\0identity"),
            Value::Null,
        ] {
            let mut candidate = original.clone();
            candidate["body"]["host_instance_id"] = invalid;
            assert_rejected_ownership_value(candidate);
        }
        for key in ["nonce", "host_sha256", "executable_sha256", "proof"] {
            if original["body"].get(key).is_none() {
                continue;
            }
            for invalid in [
                json!("a".repeat(63)),
                json!("A".repeat(64)),
                json!("g".repeat(64)),
                Value::Null,
            ] {
                let mut candidate = original.clone();
                candidate["body"][key] = invalid;
                assert_rejected_ownership_value(candidate);
            }
        }
        for key in ["owner_generation", "cursor"] {
            if original["body"].get(key).is_none() {
                continue;
            }
            for invalid in [
                json!(0),
                json!(MAX_SAFE_INTEGER + 1),
                json!(-1),
                Value::Null,
            ] {
                let mut candidate = original.clone();
                candidate["body"][key] = invalid;
                assert_rejected_ownership_value(candidate);
            }
            let mut boundary = original.clone();
            boundary["body"][key] = json!(MAX_SAFE_INTEGER);
            let bytes = serde_json::to_vec(&boundary).unwrap();
            FramedReader::new(Cursor::new(raw_frame(&bytes)))
                .read()
                .unwrap()
                .unwrap();
        }
    }
}

#[test]
fn ownership_integer_lexemes_and_duplicate_fields_fail_closed() {
    for (bytes, field, number) in [
        (ATTACH, "owner_generation", "2"),
        (ATTACHED, "owner_generation", "2"),
        (ACKNOWLEDGE, "owner_generation", "2"),
        (ACKNOWLEDGE, "cursor", "1"),
    ] {
        let original = std::str::from_utf8(bytes).unwrap();
        let token = format!("\"{field}\":{number}");
        assert!(original.contains(&token));
        for invalid in [
            format!("{number}.0"),
            format!("{number}e0"),
            "9007199254740991.1".into(),
            "-0".into(),
        ] {
            let payload = original.replace(&token, &format!("\"{field}\":{invalid}"));
            let mut reader = FramedReader::new(Cursor::new(raw_frame(payload.as_bytes())));
            assert!(
                matches!(reader.read(), Err(Error::Json(_))),
                "noncanonical {field}: {invalid}"
            );
            assert!(matches!(reader.read(), Err(Error::Poisoned)));
        }
        let payload = original.replace(&token, &format!("{token},{token}"));
        let mut reader = FramedReader::new(Cursor::new(raw_frame(payload.as_bytes())));
        let Err(Error::Json(error)) = reader.read() else {
            panic!("duplicate {field}");
        };
        assert!(error.to_string().contains("duplicate field"));
        assert!(matches!(reader.read(), Err(Error::Poisoned)));
    }
}
#[test]
fn owner_proof_rejects_invalid_claim_fields_before_authentication() {
    let secret = std::array::from_fn(|index| index as u8);
    let original: Value = serde_json::from_slice(ATTACH).unwrap();
    for (pointer, invalid) in [
        ("/body/fence/profile_id", json!("")),
        ("/body/execution/generation", json!(0)),
        ("/body/host_instance_id", json!("")),
        ("/body/nonce", json!("A".repeat(64))),
        ("/body/owner_generation", json!(0)),
        ("/body/host_release", json!("x".repeat(257))),
        ("/body/host_sha256", json!("x".repeat(64))),
        ("/body/executable_sha256", json!("x".repeat(64))),
        ("/body/sdk_version", json!("x".repeat(65))),
    ] {
        let mut value = original.clone();
        *value.pointer_mut(pointer).unwrap() = invalid;
        let candidate: Frame = serde_json::from_value(value).unwrap();
        let Body::Attach(claim) = candidate.body else {
            panic!("attach");
        };
        assert!(
            owner_proof(&secret, &claim).is_err(),
            "invalid proof claim: {pointer}"
        );
        assert!(verify_owner_proof(&secret, &claim).is_err());
    }
}
const HOST_PROOF: &str = include_str!("../fixtures/host-proof.json");

fn attached() -> Attached {
    match frame(ATTACHED).body {
        Body::Attached(attached) => attached,
        _ => panic!("fixture attached"),
    }
}

#[test]
fn host_response_matches_independent_golden_and_rejects_reflection() {
    let secret = std::array::from_fn(|index| index as u8);
    let pending = attach();
    let expected = attached();
    let fixture: Value = serde_json::from_str(HOST_PROOF).unwrap();
    assert_eq!(host_proof(&secret, &pending).unwrap(), fixture["proof"]);
    assert_eq!(expected.proof, fixture["proof"]);
    verify_host_proof(&secret, &pending, &expected).unwrap();
    let mut reflection = expected.clone();
    reflection.proof = pending.proof.clone();
    assert!(matches!(
        verify_host_proof(&secret, &pending, &reflection),
        Err(Error::Invalid("host proof"))
    ));
    assert!(verify_host_proof(&[0; 32], &pending, &expected).is_err());
    let mut invalid_request = pending;
    invalid_request.proof = "a".repeat(64);
    assert!(matches!(
        host_proof(&secret, &invalid_request),
        Err(Error::Invalid("owner proof"))
    ));
    assert!(matches!(
        verify_host_proof(&secret, &invalid_request, &expected),
        Err(Error::Invalid("owner proof"))
    ));
}

#[test]
fn host_response_rejects_replay_against_changed_pending_claim() {
    let secret = std::array::from_fn(|index| index as u8);
    let original: Value = serde_json::from_slice(ATTACH).unwrap();
    for pointer in [
        "/body/nonce",
        "/body/host_instance_id",
        "/body/owner_generation",
        "/body/fence/profile_id",
        "/body/fence/account_id",
        "/body/fence/data_epoch",
        "/body/fence/host_id",
        "/body/execution/task_id",
        "/body/execution/agent_id",
        "/body/execution/conversation_id",
        "/body/execution/process_instance_id",
        "/body/execution/generation",
        "/body/host_release",
        "/body/host_sha256",
        "/body/executable_sha256",
        "/body/sdk_version",
    ] {
        let mut value = original.clone();
        let field = value.pointer_mut(pointer).unwrap();
        *field = if let Some(number) = field.as_u64() {
            json!(number + 1)
        } else if pointer.ends_with("sha256") || pointer.ends_with("nonce") {
            json!("a".repeat(64))
        } else {
            json!(format!("{}x", field.as_str().unwrap()))
        };
        let candidate: Frame = serde_json::from_value(value).unwrap();
        let Body::Attach(mut pending) = candidate.body else {
            panic!("attach");
        };
        pending.proof = owner_proof(&secret, &pending).unwrap();
        verify_owner_proof(&secret, &pending).unwrap();
        // Match public response identity so only its obsolete MAC can reject it.
        let mut replay = attached();
        replay.fence = pending.fence.clone();
        replay.execution = pending.execution.clone();
        replay.host_instance_id = pending.host_instance_id.clone();
        replay.owner_generation = pending.owner_generation;
        replay.validate().unwrap();
        assert!(
            matches!(
                verify_host_proof(&secret, &pending, &replay),
                Err(Error::Invalid("host proof"))
            ),
            "replayed pending segment: {pointer}"
        );
    }
}

#[test]
fn host_response_compares_complete_received_identity_to_own_pending_request() {
    let secret = std::array::from_fn(|index| index as u8);
    let pending = attach();
    let original: Value = serde_json::from_slice(ATTACHED).unwrap();
    for pointer in [
        "/body/fence/profile_id",
        "/body/fence/account_id",
        "/body/fence/data_epoch",
        "/body/fence/host_id",
        "/body/execution/task_id",
        "/body/execution/agent_id",
        "/body/execution/conversation_id",
        "/body/execution/process_instance_id",
        "/body/execution/generation",
        "/body/host_instance_id",
        "/body/owner_generation",
    ] {
        let mut value = original.clone();
        let field = value.pointer_mut(pointer).unwrap();
        *field = if let Some(number) = field.as_u64() {
            json!(number + 1)
        } else {
            json!(format!("{}x", field.as_str().unwrap()))
        };
        let candidate: Frame = serde_json::from_value(value).unwrap();
        candidate.validate().unwrap();
        let Body::Attached(received) = candidate.body else {
            panic!("attached");
        };
        let result = verify_host_proof(&secret, &pending, &received);
        if pointer.contains("/fence/") {
            assert!(matches!(result, Err(Error::FenceMismatch)), "{pointer}");
        } else if pointer.contains("/execution/") {
            assert!(matches!(result, Err(Error::ExecutionMismatch)), "{pointer}");
        } else {
            assert!(matches!(result, Err(Error::HostMismatch)), "{pointer}");
        }
    }
}

#[test]
fn received_frame_captures_exact_sender_payload_and_digest_from_one_read() {
    use sha2::{Digest, Sha256};
    let mut value: Value = serde_json::from_slice(START).unwrap();
    value["body"]["action"]["prompt"] = json!("café 東京 / quoted \"text\"");
    let expected: Frame = serde_json::from_value(value.clone()).unwrap();
    let pretty = serde_json::to_string_pretty(&value).unwrap();
    let escaped = pretty.replace('é', "\\u00e9").replace('/', "\\/");
    let payload = format!(" \n{escaped}\t").into_bytes();
    let mut wire = raw_frame(&payload);
    wire.extend(raw_frame(HELLO));
    let mut reader = FramedReader::new(Cursor::new(wire));
    let received = reader.read_received().unwrap().unwrap();
    assert_eq!(received.frame(), &expected);
    assert_eq!(received.payload(), payload);
    assert_eq!(received.digest(), format!("{:x}", Sha256::digest(&payload)));
    let encoded = encode(&expected).unwrap();
    assert_ne!(received.payload(), &encoded[4..]);
    assert_ne!(
        received.digest(),
        format!("{:x}", Sha256::digest(&encoded[4..]))
    );
    let (decoded, original_bytes, original_digest) = received.into_parts();
    assert_eq!(decoded, expected);
    assert_eq!(original_bytes, payload);
    assert_eq!(
        original_digest,
        format!("{:x}", Sha256::digest(&original_bytes))
    );
    let (following, following_digest) = reader.read_with_digest().unwrap().unwrap();
    assert_eq!(following, frame(HELLO));
    assert_eq!(following_digest, format!("{:x}", Sha256::digest(HELLO)));
    assert!(reader.read_received().unwrap().is_none());
}

#[test]
fn control_actions_and_results_match_shared_host_corpus() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/controls.json")).unwrap();
    for case in cases {
        let raw = case["json"].as_str().unwrap();
        let read = FramedReader::new(Cursor::new(raw_frame(raw.as_bytes()))).read();
        assert_eq!(
            read.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "control case {}",
            case["name"]
        );
        if let Ok(Some(frame)) = read {
            assert_eq!(
                serde_json::to_value(&frame).unwrap(),
                serde_json::from_str::<Value>(raw).unwrap(),
                "control case {} round trips",
                case["name"]
            );
            if let Body::Event(event) = frame.body {
                event.validate_for_application().unwrap();
            }
        }
    }
}

#[test]
fn control_actions_have_one_wire_spelling() {
    let mut set_model = command();
    set_model.action = Action::SetModel {
        model: "fixture-model".into(),
    };
    let wire = serde_json::to_value(&set_model.action).unwrap();
    assert_eq!(wire, json!({"type":"set_model","model":"fixture-model"}));
    for (mode, spelling) in [
        (PermissionMode::Default, "default"),
        (PermissionMode::Plan, "plan"),
        (PermissionMode::DontAsk, "dont_ask"),
    ] {
        let action = Action::SetPermissionMode { mode };
        let wire = serde_json::to_value(&action).unwrap();
        assert_eq!(wire, json!({"type":"set_permission_mode","mode":spelling}));
        assert_eq!(serde_json::from_value::<Action>(wire).unwrap(), action);
    }
    assert_eq!(
        serde_json::to_value(Action::McpStatus).unwrap(),
        json!({"type":"mcp_status"})
    );
    set_model.validate().unwrap();
    for model in ["", " \t", "fixture\u{7}", &"m".repeat(MAX_MODEL_BYTES + 1)] {
        set_model.action = Action::SetModel {
            model: model.into(),
        };
        assert!(set_model.validate().is_err(), "model {model:?}");
    }
}

#[test]
fn no_permission_mode_that_grants_permissions_is_representable() {
    for mode in [
        "acceptEdits",
        "accept_edits",
        "auto",
        "bypassPermissions",
        "bypass_permissions",
        "dontAsk",
        "Default",
    ] {
        assert!(
            serde_json::from_value::<PermissionMode>(json!(mode)).is_err(),
            "{mode} must not deserialize"
        );
        assert!(
            serde_json::from_value::<Action>(json!({"type":"set_permission_mode","mode":mode}))
                .is_err(),
            "{mode} must not reach the host"
        );
    }
}

#[test]
fn control_result_is_typed_bounded_and_applicable() {
    let result = ControlResult::McpStatus {
        command_id: CommandId("command-test".into()),
        servers: vec![McpServer {
            name: "fixture".into(),
            status: "failed".into(),
            error: Some("unreachable".into()),
        }],
    };
    let mut observed = event();
    observed.observation = Observation::Sdk {
        kind: SdkEventKind::ControlResult,
        native: NativeCorrelation::default(),
        payload: serde_json::to_value(&result).unwrap(),
        failures: Vec::new(),
    };
    assert_eq!(
        serde_json::to_value(&result).unwrap(),
        json!({"control":"mcp_status","command_id":"command-test","servers":[{"name":"fixture","status":"failed","error":"unreachable"}]})
    );
    observed.validate_for_application().unwrap();
    let encoded = encode(&Frame::new(Body::Event(observed.clone()))).unwrap();
    assert_eq!(
        FramedReader::new(Cursor::new(encoded))
            .read()
            .unwrap()
            .unwrap()
            .body,
        Body::Event(observed.clone())
    );
    if let Observation::Sdk { payload, .. } = &mut observed.observation {
        payload["servers"] = json!(vec![
            json!({"name":"s","status":"connected"});
            MAX_MCP_SERVERS + 1
        ]);
    }
    assert!(observed.validate().is_err());
    if let Observation::Sdk { payload, .. } = &mut observed.observation {
        *payload = json!({"type":"result","subtype":"success"});
    }
    assert!(
        observed.validate().is_err(),
        "a control result is never an untyped SDK object"
    );
}

#[test]
fn decision_answers_and_settlements_match_shared_host_corpus() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../fixtures/decisions.json")).unwrap();
    for case in cases {
        let raw = case["json"].as_str().unwrap();
        let read = FramedReader::new(Cursor::new(raw_frame(raw.as_bytes()))).read();
        assert_eq!(
            read.is_ok(),
            case["accepted"].as_bool().unwrap(),
            "decision case {}",
            case["name"]
        );
        if let Ok(Some(frame)) = read {
            assert_eq!(
                serde_json::to_value(&frame).unwrap(),
                serde_json::from_str::<Value>(raw).unwrap(),
                "decision case {} round trips",
                case["name"]
            );
            if let Body::Event(event) = frame.body {
                event.validate_for_application().unwrap();
            }
        }
    }
}

#[test]
fn an_answer_names_its_request_and_only_answers_its_kind() {
    let mut respond = command();
    let answer = |kind, tool_use_id: Option<&str>, decision| Action::Respond {
        request_id: "request-test".into(),
        epoch: 42,
        kind,
        tool_use_id: tool_use_id.map(Into::into),
        decision,
        asked: serde_json::Map::new(),
    };
    let allow = || Decision::Allow {
        updated_input: serde_json::Map::new(),
    };
    let deny = || Decision::Deny {
        message: "No".into(),
    };
    let accept = || Decision::Accept { content: None };
    for kind in [
        DecisionKind::Permission,
        DecisionKind::Question,
        DecisionKind::Plan,
    ] {
        for decision in [allow(), deny()] {
            respond.action = answer(kind, Some("toolu-test"), decision.clone());
            respond.validate().unwrap();
            respond.action = answer(kind, None, decision);
            assert!(respond.validate().is_err(), "{kind:?} names its tool use");
        }
        for decision in [accept(), Decision::Decline {}] {
            respond.action = answer(kind, Some("toolu-test"), decision);
            assert!(respond.validate().is_err(), "{kind:?} is not an elicitation");
        }
    }
    for decision in [accept(), Decision::Decline {}] {
        respond.action = answer(DecisionKind::Elicitation, None, decision.clone());
        respond.validate().unwrap();
        respond.action = answer(DecisionKind::Elicitation, Some("toolu-test"), decision);
        assert!(respond.validate().is_err(), "an elicitation has no tool use");
    }
    for decision in [allow(), deny()] {
        respond.action = answer(DecisionKind::Elicitation, None, decision);
        assert!(respond.validate().is_err(), "a tool decision never answers an elicitation");
    }
    respond.action = answer(DecisionKind::Permission, Some("toolu-test"), allow());
    let Action::Respond { epoch, .. } = &mut respond.action else { unreachable!() };
    *epoch = 0;
    assert!(respond.validate().is_err(), "an epoch is a host cursor");
    assert!(encode(&Frame::new(Body::Command(respond))).is_err());
    assert_eq!(
        serde_json::to_value(answer(DecisionKind::Elicitation, None, Decision::Decline {})).unwrap(),
        json!({"type":"respond","request_id":"request-test","epoch":42,"kind":"elicitation","tool_use_id":null,"decision":{"behavior":"decline"},"asked":{}})
    );

    let mut requested = event();
    requested.observation = Observation::DecisionRequested {
        request_id: "request-test".into(),
        tool_use_id: None,
        kind: DecisionKind::Elicitation,
        payload: json!({"serverName":"fixture-server"}),
    };
    requested.validate_for_application().unwrap();
    assert_eq!(
        serde_json::to_value(&requested.observation).unwrap(),
        json!({"type":"decision_requested","request_id":"request-test","kind":"elicitation","payload":{"serverName":"fixture-server"}})
    );
    let Observation::DecisionRequested { kind, .. } = &mut requested.observation else { unreachable!() };
    *kind = DecisionKind::Permission;
    assert!(requested.validate().is_err(), "a permission names its tool use");

    let mut settled = event();
    settled.observation = Observation::DecisionSettled {
        request_id: "request-test".into(),
        epoch: 42,
        outcome: DecisionOutcome::Cancelled,
    };
    settled.validate_for_application().unwrap();
    let Observation::DecisionSettled { epoch, .. } = &mut settled.observation else { unreachable!() };
    *epoch = MAX_SAFE_INTEGER + 1;
    assert!(settled.validate().is_err());
}
