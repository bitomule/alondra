use super::*;
use crate::format::Scenario;
use std::cell::RefCell;

/// What a package may carry that names something of the Mac it came from without being a leak
/// shape: the draft has to leave each of them behind.
const CANARIES: &[&str] = &["canary-task-1", "canary-task-2", "canary-request", "canaryinstance1", "canary-node", "canarydetail", "canary_state", "canary-field"];

fn package() -> Package {
    let lifecycle = [
        json!({"event": "admit", "task": "canary-task-1", "revision": 1, "data": {"canary-field": "canary_state"}}),
        json!({"event": "ask", "task": "canary-task-1", "revision": 2, "data": {}}),
        json!({"event": "present", "task": "canary-task-1", "revision": 3, "data": {}}),
        json!({"event": "accept", "task": "canary-task-1", "revision": 4, "data": {}}),
        json!({"event": "admit", "task": "canary-task-2", "revision": 5, "data": {}}),
    ];
    let tasks = [
        json!({"task": "canary-task-1", "request": "canary-request", "state": "canary_state", "states_in_window": [{"at": "2026-11-10T10:00:00Z", "state": "canary_state"}, {"at": "2026-11-10T10:20:00Z", "state": "waiting"}]}),
        json!({"task": "canary-task-2", "request": "canary-request", "state": "working", "states_in_window": [{"at": "2026-11-10T11:00:00Z", "state": "working"}]}),
    ];
    let host = [json!({"instance": "canaryinstance1", "protocol_versions": [1], "frames": [
        {"type": "hello"},
        {"type": "event", "cursor": 1, "observation": "sdk", "detail": "canarydetail"},
        {"type": "event", "cursor": 2, "observation": "sdk", "detail": "result"},
        {"type": "event", "cursor": 3, "observation": "process_exited", "detail": {"exit_code": 1, "signal": null}},
    ]})];
    let jsonl = |rows: &[Value]| rows.iter().map(|row| format!("{row}\n")).collect::<String>();
    Package::from([
        ("lifecycle.jsonl".into(), jsonl(&lifecycle)),
        ("tasks.jsonl".into(), jsonl(&tasks)),
        ("sdk-host.jsonl".into(), jsonl(&host)),
        ("nodes.jsonl".into(), json!({"node": "canary-node", "task": "canary-task-1"}).to_string() + "\n"),
    ])
}

fn literals() -> Vec<String> {
    vec!["projectcanaryhorizon".into(), "canary-account".into()]
}

#[test]
fn a_package_with_canaries_gives_a_clean_draft_with_its_gaps() {
    let draft = draft("canary-case", &package(), &["canary-host".to_owned()], &literals(), &[]).expect("a clean package drafts");

    let parsed = Scenario::parse(&draft.scenario_toml).expect("the draft is a scenario");
    assert_eq!(parsed.name, "canary-case");
    assert!(draft.scenario_toml.contains("do = \"request\"") && draft.scenario_toml.contains("do = \"task\""));
    assert!(draft.scenario_toml.contains("by = \"20m\""), "{}", draft.scenario_toml);
    assert_eq!(draft.transcripts.len(), 1);
    let transcript = Transcript::parse(&draft.transcripts[0].1).expect("the transcript reads back");
    assert_eq!(transcript.lines.len(), 3);
    let kinds: Vec<&Value> = transcript.inbound().map(|frame| &frame["body"]["observation"]["kind"]).collect();
    assert_eq!(kinds, vec!["message", "result"], "a kind the protocol does not have is not copied");
    assert_eq!(transcript.lines[2], Line::RuntimeExit { code: Some(1) });
    for needle in ["the node asked a question", "task t1, revision 3", "the user accepted a delivery", "task t2", "host-1.ndjson"] {
        assert!(draft.gaps.contains(needle), "{needle} missing from:\n{}", draft.gaps);
    }

    let everything = [draft.scenario_toml.as_str(), draft.gaps.as_str(), draft.transcripts[0].1.as_str()].join("\n").to_lowercase();
    let input: String = package().values().cloned().collect();
    for canary in CANARIES {
        assert!(input.contains(canary), "{canary} is not in the package");
        assert!(!everything.contains(canary), "{canary} is in the draft");
    }
}

#[test]
fn the_same_package_gives_the_same_draft() {
    assert_eq!(draft("x", &package(), &[], &[], &[]).unwrap(), draft("x", &package(), &[], &[], &[]).unwrap());
}

fn write_package(dir: &Path, package: &Package) {
    for (name, text) in package {
        std::fs::write(dir.join(name), text).unwrap();
    }
}

#[test]
fn a_private_literal_in_the_package_writes_nothing_and_says_where() {
    let mut package = package();
    package.insert("tasks.jsonl".into(), package["tasks.jsonl"].replace("\"working\"", "\"ProjectCanaryHorizon\""));
    let source = tempfile::tempdir().unwrap();
    write_package(source.path(), &package);
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("draft");
    let Err(Refusal::Leaks(leaks)) = convert(source.path(), "x", &target, &literals(), &[]) else { panic!("a private literal must refuse the draft") };
    assert_eq!(leaks[0], Leak { file: "tasks.jsonl".into(), line: 2, rule: "private-literal".into() });
    assert!(!target.exists(), "nothing is written when the draft is refused");
}

#[test]
fn a_value_the_report_replaced_showing_again_refuses_the_draft() {
    let mut package = package();
    package.insert("nodes.jsonl".into(), "{\"node\": \"acme-billing-worker\"}\n".into());
    let Err(Refusal::Leaks(leaks)) = draft("x", &package, &["acme-billing-worker".to_owned()], &[], &[]) else { panic!("refused") };
    assert_eq!(leaks[0].file, "nodes.jsonl");
}

#[test]
fn a_secret_shape_in_the_package_refuses_the_draft() {
    let mut package = package();
    package.insert("nodes.jsonl".into(), format!("{{\"token\": \"sk-ant-{}\"}}\n", "canary0123456789abcdef"));
    assert!(matches!(draft("x", &package, &[], &[], &[]), Err(Refusal::Leaks(_))));
}

#[test]
fn every_check_reads_the_package_and_the_draft_and_what_one_finds_writes_nothing() {
    let seen = RefCell::new(Vec::new());
    let record = |files: &[(&str, &str)], _: &[String]| {
        seen.borrow_mut().extend(files.iter().map(|(file, _)| (*file).to_owned()));
        Vec::new()
    };
    draft("x", &package(), &[], &[], &[&record]).unwrap();
    for file in ["tasks.jsonl", "sdk-host.jsonl", "scenario.toml", "gaps.txt", "transcripts/host-1.ndjson"] {
        assert!(seen.borrow().iter().any(|name| name == file), "the check did not read {file}: {:?}", seen.borrow());
    }

    let refuse = |files: &[(&str, &str)], _: &[String]| files.iter().filter(|(file, _)| *file == "gaps.txt").map(|(file, _)| Leak { file: (*file).into(), line: 1, rule: "an IP address".into() }).collect();
    let source = tempfile::tempdir().unwrap();
    write_package(source.path(), &package());
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("draft");
    let Err(Refusal::Leaks(leaks)) = convert(source.path(), "x", &target, &[], &[&refuse]) else { panic!("a check that finds a leak refuses the draft") };
    assert_eq!(leaks, vec![Leak { file: "gaps.txt".into(), line: 1, rule: "an IP address".into() }]);
    assert!(!target.exists());
}

#[test]
fn a_package_with_nothing_to_draft_from_is_refused() {
    assert!(matches!(draft("x", &Package::new(), &[], &[], &[]), Err(Refusal::Empty)));
}

#[test]
fn a_draft_is_written_with_its_transcripts_and_gaps() {
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("case");
    write(&target, &draft("x", &package(), &[], &[], &[]).unwrap()).unwrap();
    assert!(target.join("scenario.toml").is_file() && target.join("gaps.txt").is_file() && target.join("transcripts/host-1.ndjson").is_file());
}

#[test]
fn the_originals_of_a_pseudonym_table_are_read() {
    let table = json!({"by_value": {"ana.lopez": "path-1", "12": "value-1", "ab": "value-2"}, "counts": {}}).to_string();
    assert_eq!(pseudonym_originals(&table), vec!["ana.lopez".to_owned()]);
}

#[test]
fn a_clean_package_is_converted_into_a_folder() {
    let source = tempfile::tempdir().unwrap();
    write_package(source.path(), &package());
    std::fs::write(source.path().join("pseudonyms.json"), json!({"by_value": {"acme-billing": "repo-1"}}).to_string()).unwrap();
    let out = tempfile::tempdir().unwrap();
    let target = out.path().join("draft");
    convert(source.path(), "x", &target, &literals(), &[]).unwrap();
    assert!(target.join("scenario.toml").is_file());
}
