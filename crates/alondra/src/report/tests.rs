use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use rusqlite::Connection;
use serde_json::{Value, json};

use super::bundle::{self, Draft};
use super::canary::{self, Private};
use super::collect::{self, Host, Section, Status, Window};
use super::daemon_log;
use super::redact::{Kind, Redactor, is_number, is_version};
use super::templates::{Catalog, Piece, fill, pieces};
use super::*;
use crate::test_support::Directory;

fn workspace_file(path: &str) -> PathBuf {
    match std::env::var("TEST_SRCDIR") {
        Ok(root) => PathBuf::from(root).join(std::env::var("TEST_WORKSPACE").unwrap_or_else(|_| "_main".into())).join(path),
        Err(_) => PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").join(path),
    }
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(workspace_file(&format!("fixtures/report/{name}"))).unwrap()
}

pub(super) fn private_values() -> Vec<String> {
    fixture("private.txt").lines().filter(|l| !l.is_empty()).map(str::to_owned).collect()
}

fn assert_has_none_of(text: &str, values: &[String]) {
    let lowered = text.to_lowercase();
    for value in values {
        assert!(!lowered.contains(&value.to_lowercase()), "{value:?} leaked into:\n{text}");
    }
}

pub(super) const NOW: i64 = 1_792_850_000_000;

pub(super) fn hours_before(hours: i64) -> Window {
    Window { from: NOW - hours * 3_600_000, to: NOW, hours: hours as u32 }
}

pub(super) struct FakeMac {
    home: PathBuf,
    daemon: Option<Value>,
    commands: HashMap<String, String>,
    pub(super) report_repo: Option<String>,
}

impl FakeMac {
    pub(super) fn new(home: &Path) -> FakeMac {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let commands = [
            ("sysctl -n hw.model", "Mac15,8\n"),
            ("sysctl -n machdep.cpu.brand_string", "Apple M3 Max\n"),
            ("sysctl -n hw.memsize", "68719476736\n"),
            ("sysctl -n kern.boottime", "{ sec = 1792800000, usec = 0 } Thu Oct 24 10:00:00 2026\n"),
            ("sw_vers -productVersion", "26.4\n"),
            ("sw_vers -buildVersion", "25E246\n"),
            ("scutil --get ComputerName", "Ana's MacBook Pro\n"),
            ("hostname", "anas-macbook-pro.local\n"),
            ("gh config get -h github.com user", "analopez-acme\n"),
            ("claude --version", "2.1.30 (Claude Code)\n"),
            ("launchctl print gui/501/com.bitomule.alondra", "\tstate = running\n\tprogram = /Users/ana.lopez/.alondra/bin/alondrad-launcher\n\truns = 4\n\tlast exit code = 75: EX_TEMPFAIL\n"),
            ("pmset -g log", "2026-10-24 01:10:05 +0200 Sleep               \tEntering Sleep state due to 'Idle Sleep' Using AC (Charge:100%)\n2026-10-24 06:30:00 +0200 Wake                \tWake from Deep Idle [CDNVA] : due to UserActivity Assertion\n"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        FakeMac { home: home.to_path_buf(), daemon: None, commands, report_repo: None }
    }
}

impl Host for FakeMac {
    fn home(&self) -> &Path {
        &self.home
    }
    fn now(&self) -> i64 {
        NOW
    }
    fn daemon(&self) -> Option<Value> {
        self.daemon.clone()
    }
    fn doctor(&self) -> Option<Vec<(String, String)>> {
        Some(vec![("service".into(), "pass".into()), ("alondrad".into(), "fail".into()), ("not-a-check".into(), "fail".into())])
    }
    fn run(&self, program: &str, args: &[&str]) -> Option<String> {
        self.commands.get(&std::iter::once(program).chain(args.iter().copied()).collect::<Vec<_>>().join(" ")).cloned()
    }
    fn bin_dir(&self) -> Option<PathBuf> {
        None
    }
    fn service_label(&self) -> String {
        "com.bitomule.alondra".into()
    }
    fn uid(&self) -> u32 {
        501
    }
    fn report_repo(&self) -> Option<String> {
        self.report_repo.clone()
    }
}

fn redacted_fixture_log() -> (Vec<String>, u64, Redactor<'static>) {
    let catalog = Catalog::built();
    let mut log = std::io::Cursor::new(fixture("alondrad.log").into_bytes());
    let read = daemon_log::read_back(&mut log, 0, daemon_log::READ_CAP).unwrap();
    let mut redactor = Redactor::new(&catalog.vocabulary);
    let out = daemon_log::redact(&read.lines, catalog, &mut redactor);
    (out.text, out.without_template, redactor)
}

#[test]
fn a_real_log_comes_out_as_its_templates_in_utc_with_nothing_private() {
    let (lines, without_template, redactor) = redacted_fixture_log();
    let text = lines.join("\n") + "\n";
    assert_has_none_of(&text, &private_values());
    assert_eq!(text, fixture("daemon.redacted.log"), "the redacted fixture log changed:\n{text}");
    assert_eq!(without_template, 3, "the continuation, the unstamped line of an older build and the line no template has");
    assert!(lines[0].starts_with("2026-10-23T07:00:01.120Z "), "{}", lines[0]);
    assert!(!redactor.pseudonyms.is_empty());
}

#[test]
fn a_task_keeps_one_pseudonym_across_the_log() {
    let (lines, _, _) = redacted_fixture_log();
    let named: Vec<&String> = lines.iter().filter(|l| l.contains("went to its SDK host")).collect();
    let pseudonym = named[0].split(" of ").nth(1).unwrap().split(' ').next().unwrap();
    assert!(pseudonym.starts_with("task-"), "{pseudonym}");
    assert!(lines.iter().filter(|l| l.contains(&format!("next {pseudonym},"))).count() >= 2, "{lines:#?}");
}

#[test]
fn every_log_call_of_the_daemon_is_in_the_catalog() {
    let root = workspace_file("");
    let mut naive = 0;
    let mut extracted = 0;
    for dir in ["crates/alondrad/src", "crates/alondrad/launcher"] {
        for file in extract::rust_files(&root.join(dir)) {
            let source = std::fs::read_to_string(&file).unwrap();
            naive += source.match_indices("log!(").filter(|(at, _)| !source[..*at].ends_with(|c: char| c.is_alphanumeric() || c == '_')).count();
            extracted += extract::calls(&source, &["log"]).len();
        }
    }
    assert!(naive > 250, "{naive}");
    assert_eq!(extracted, naive, "a log! call the catalog cannot read");
    let (catalog, _) = extract::build(&root);
    let built = Catalog::built();
    assert_eq!(Catalog::parse(&catalog, "").templates.len(), built.templates.len(), "the catalog this build carries is not the one of its sources");
}

#[test]
fn a_new_log_call_enters_the_catalog_by_itself() {
    let root = Directory::new();
    std::fs::create_dir_all(root.join("crates/alondrad/src")).unwrap();
    std::fs::write(
        root.join("crates/alondrad/src/brand_new.rs"),
        "fn f() {\n    log!(\n        \"the brand new watcher of {task} gave up after {} tries\",\n        n\n    );\n}\nenum Brand { NewWatcher, GaveUp(u8) }\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
    std::fs::write(root.join("crates/core/src/lib.rs"), "pub enum Fresh { /// The first one\n QuiteNew, Older { at: u8 } }\n").unwrap();
    let (catalog, vocabulary) = extract::build(&root);
    assert!(catalog.contains("crates/alondrad/src/brand_new.rs:2\tthe brand new watcher of {task} gave up after {} tries"), "{catalog}");
    let words: Vec<&str> = vocabulary.lines().collect();
    for word in ["NewWatcher", "new_watcher", "GaveUp", "QuiteNew", "quite-new", "Older", "watcher"] {
        assert!(words.contains(&word), "{word} missing from {words:?}");
    }
    assert!(!words.contains(&"The"), "a doc comment is not a variant");
    let catalog = Catalog::parse(&catalog, &vocabulary);
    let mut redactor = Redactor::new(&catalog.vocabulary);
    let line = redactor.log_message(&catalog, "the brand new watcher of t-acme-sso-login gave up after 3 tries").unwrap();
    assert_eq!(line, "the brand new watcher of task-1 gave up after 3 tries");
}

#[test]
fn a_line_written_to_stderr_enters_the_catalog_and_other_writes_do_not() {
    let root = Directory::new();
    std::fs::create_dir_all(root.join("crates/core/src")).unwrap();
    std::fs::write(
        root.join("crates/core/src/lib.rs"),
        "fn f(out: &mut String) {\n    let _ = writeln!(std::io::stderr(), \"Alondra dropped {} cards\", n);\n    let _ = write!(\n        std::io::stderr().lock(),\n        \"a full disk kept {task}\"\n    );\n    let _ = writeln!(out, \"a card body of {task}\");\n    let _ = writeln!(f);\n}\n",
    )
    .unwrap();
    let (catalog, _) = extract::build(&root);
    assert!(catalog.contains("crates/core/src/lib.rs:2\tAlondra dropped {} cards"), "{catalog}");
    assert!(catalog.contains("crates/core/src/lib.rs:3\ta full disk kept {task}"), "{catalog}");
    assert!(!catalog.contains("a card body"), "{catalog}");
}

#[test]
fn every_write_to_stderr_of_the_daemon_is_in_the_catalog() {
    let root = workspace_file("");
    let mut naive = 0;
    let mut extracted = 0;
    for dir in extract::LOG_SOURCES {
        for file in extract::rust_files(&root.join(dir)) {
            let source = std::fs::read_to_string(&file).unwrap();
            let flat: String = source.split_whitespace().collect();
            naive += flat.matches("writeln!(std::io::stderr()").count() + flat.matches("write!(std::io::stderr()").count();
            extracted += extract::stderr_writes(&source).len();
        }
    }
    assert!(naive >= 5, "{naive}");
    assert_eq!(extracted, naive, "a write to stderr the catalog cannot read");
}

#[test]
fn a_template_matches_only_its_whole_fixed_text() {
    let template = pieces("input {} of {task} went to its SDK host ({}): {handoff:?}");
    assert_eq!(template[0], Piece::Text("input ".into()));
    assert_eq!(fill(&template, "input i-1 of t-a went to its SDK host (sdk-1): Offered"), Some(vec!["i-1", "t-a", "sdk-1", "Offered"]));
    assert_eq!(fill(&template, "input i-1 of t-a went to its SDK host (sdk-1)"), None);
    assert_eq!(fill(&template, "prefix input i-1 of t-a went to its SDK host (sdk-1): x"), None);
    assert_eq!(fill(&pieces("{{literal}} {} done"), "{literal} 3 done"), Some(vec!["3"]));
}

#[test]
fn safe_shapes() {
    for safe in ["12", "0.8", "250ms", "1.5s", "41%", "3GiB", "-2"] {
        assert!(is_number(safe), "{safe}");
    }
    for version in ["0.3.5", "v2.1.30", "1.2.3-beta.1", "26.4"] {
        assert!(is_version(version), "{version}");
    }
    assert!(!is_version("192.168.1.20"), "an IPv4 address is never a version");
    let catalog = Catalog::built();
    let mut redactor = Redactor::new(&catalog.vocabulary);
    assert_eq!(redactor.value("2026-10-24T05:10:11.637+02:00", Kind::Value), "2026-10-24T03:10:11.637Z");
    assert_eq!(redactor.value("sdk-0abdea5e130f80de", Kind::Value), "sdk-0abdea5e130f80de");
    assert_eq!(redactor.value("https://github.com/acme/billing/pull/42", Kind::Value), "repo-1/pull/42");
    assert_eq!(redactor.value("/Users/ana.lopez/Code/acme-billing", Kind::Value), "path-1");
    assert_eq!(redactor.value("t-fix-acme-invoice-rounding, t-acme-sso-login", Kind::Value), "task-1, task-2");
    assert_eq!(redactor.value("acme-billing is broken", Kind::Path), "path-2");
    assert_eq!(redactor.value("fix the acme invoice", Kind::Task), "task-3");
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    #[test]
    fn whatever_fills_a_template_s_holes_never_shows_unless_it_is_safe(
        index in 0usize..10_000,
        values in proptest::collection::vec("(/Users/)?[a-z]{3,9}([-./ ][a-z]{3,9}){1,3}", 1..8),
    ) {
        let catalog = Catalog::built();
        let template = &catalog.templates[index % catalog.templates.len()];
        let mut injected = Vec::new();
        let message: String = template.pieces.iter().map(|piece| match piece {
            Piece::Text(text) => text.clone(),
            Piece::Hole(_) => {
                let value = values[injected.len() % values.len()].clone();
                injected.push(value.clone());
                value
            }
        }).collect();
        let mut redactor = Redactor::new(&catalog.vocabulary);
        let Some(out) = redactor.log_message(catalog, &message) else { return Ok(()) };
        for value in injected {
            let all_code = value.split([' ', '-', '.', '/']).all(|w| w.is_empty() || catalog.vocabulary.contains(w));
            let in_fixed_text = template.pieces.iter().any(|p| matches!(p, Piece::Text(t) if t.contains(&value)));
            prop_assert!(all_code || in_fixed_text || !out.contains(&value), "{value:?} shows in {out:?}");
        }
    }
}

#[test]
fn a_huge_log_is_read_back_only_up_to_the_cap() {
    struct Endless {
        length: u64,
        position: u64,
        served: u64,
    }
    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let line = b"2026-10-25T06:00:00.000+02:00 alondrad: exiting with 75 so launchd starts the new binary\n";
            let n = buf.len().min((self.length - self.position) as usize);
            for (i, byte) in buf[..n].iter_mut().enumerate() {
                *byte = line[((self.position + i as u64) % line.len() as u64) as usize];
            }
            self.position += n as u64;
            self.served += n as u64;
            Ok(n)
        }
    }
    impl Seek for Endless {
        fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
            self.position = match to {
                SeekFrom::Start(at) => at,
                SeekFrom::End(back) => (self.length as i64 + back) as u64,
                SeekFrom::Current(by) => (self.position as i64 + by) as u64,
            };
            Ok(self.position)
        }
    }
    let mut log = Endless { length: 4 * 1024 * 1024 * 1024, position: 0, served: 0 };
    let read = daemon_log::read_back(&mut log, 0, daemon_log::READ_CAP).unwrap();
    assert!(read.capped);
    assert_eq!(read.bytes_read, daemon_log::READ_CAP);
    assert_eq!(log.served, daemon_log::READ_CAP, "it read more than the cap");
    assert!(read.lines.len() > 600_000);
}

#[test]
fn the_window_stops_at_the_first_older_line_and_the_rotated_log_comes_first() {
    let home = Directory::new();
    std::fs::create_dir_all(&*home).unwrap();
    std::fs::write(home.join("alondrad.log.1"), "2026-10-24T01:00:00.000+02:00 alondrad: exiting with 1 so launchd starts the new binary\n2026-10-24T09:00:00.000+02:00 alondrad: exiting with 2 so launchd starts the new binary\n").unwrap();
    std::fs::write(home.join("alondrad.log"), "2026-10-24T10:00:00.000+02:00 alondrad: exiting with 3 so launchd starts the new binary\n").unwrap();
    let from = super::time::parse_iso("2026-10-24T08:00:00.000+02:00").unwrap();
    let read = daemon_log::read_window(&home, from, daemon_log::READ_CAP).unwrap();
    let messages: Vec<&str> = read.lines.iter().map(|l| l.message.as_str()).collect();
    assert_eq!(messages, ["exiting with 2 so launchd starts the new binary", "exiting with 3 so launchd starts the new binary"]);
    std::fs::remove_file(home.join("alondrad.log.1")).unwrap();
    assert_eq!(daemon_log::read_window(&home, from, daemon_log::READ_CAP).unwrap().lines.len(), 1, "a missing rotated log is no error");
}

const PRIVATE_TEXT: &[&str] = &["Fix the Acme invoice rounding", "Ana asked for the Q3 numbers", "acme/billing", "feature/acme-sso", "/Users/ana.lopez/Code/acme-billing", "the secret answer", "Use the staging database", "claude-acme-key"];

fn domain_fixture(home: &Path, user_version: i64, with_closings: bool) {
    std::fs::create_dir_all(home).unwrap();
    let db = Connection::open(home.join("domain.db")).unwrap();
    db.execute_batch(&format!(
        "PRAGMA user_version = {user_version};
         CREATE TABLE command_audit(revision INTEGER PRIMARY KEY, actor TEXT, command_id TEXT, data_epoch TEXT, command TEXT, at INTEGER);
         CREATE TABLE requests(id TEXT PRIMARY KEY, body TEXT, closed INTEGER, created_at INTEGER, title TEXT);
         CREATE TABLE agents(id TEXT PRIMARY KEY, name TEXT, phase TEXT, since INTEGER);
         CREATE TABLE tasks(id TEXT PRIMARY KEY, request_id TEXT, agent_id TEXT, title TEXT, state TEXT, revision INTEGER, started INTEGER, review_prs INTEGER, final_presentation TEXT, pr_id TEXT, archived INTEGER);
         CREATE TABLE task_history(revision INTEGER, task_id TEXT, state TEXT, summary TEXT);
         CREATE TABLE domain_messages(id INTEGER PRIMARY KEY, revision INTEGER, ordinal INTEGER, recipient_kind TEXT, agent_id TEXT, task_id TEXT, interaction_id TEXT, message TEXT);
         CREATE TABLE delivery_attempts(id INTEGER PRIMARY KEY, message_id INTEGER, state TEXT, version INTEGER, reason TEXT);
         CREATE TABLE interactions(id TEXT PRIMARY KEY, scope_kind TEXT, task_id TEXT, kind TEXT, body TEXT, blocking INTEGER, recipient_kind TEXT, answer TEXT, is_final INTEGER, acceptance_valid INTEGER, withdrawn INTEGER, ordering INTEGER, card TEXT);
         CREATE TABLE lifecycle_executions(task_id TEXT, generation INTEGER, node_id TEXT, instance_id TEXT, conversation_id TEXT, host_id TEXT, backend TEXT);
         CREATE TABLE lifecycle_events(task_id TEXT, revision INTEGER, command_id TEXT, event TEXT);
         CREATE TABLE pull_requests(id TEXT PRIMARY KEY, task_id TEXT, host TEXT, repository TEXT, number INTEGER, head TEXT, base TEXT, state TEXT);
         CREATE TABLE task_models(task_id TEXT PRIMARY KEY, model TEXT, chosen_by TEXT);
         CREATE TABLE api_failures(agent_id TEXT PRIMARY KEY, failures INTEGER, first_at INTEGER, last_at INTEGER, card TEXT);
         CREATE TABLE task_worktree_records(task_id TEXT, path TEXT, origin TEXT, branch TEXT, discard INTEGER, end TEXT);
         {}",
        if with_closings { "CREATE TABLE task_closings(task_id TEXT PRIMARY KEY, since INTEGER, failures INTEGER, last_error TEXT);" } else { "" }
    ))
    .unwrap();
    let node = "df27c53d-db4d-4b6d-9400-5b62abb33d38";
    db.execute_batch(&format!(
        "INSERT INTO command_audit VALUES (10, 'user', 'c-10', 'e', '{{}}', {at});
         INSERT INTO requests VALUES ('req-acme', 'Ana asked for the Q3 numbers', 0, 0, 'Fix the Acme invoice rounding');
         INSERT INTO agents VALUES ('{node}', 'claude-acme-key', '{{\"kind\":\"waiting_background\",\"background\":{{\"label\":\"/Users/ana.lopez/Code/acme-billing\",\"since\":{at}}}}}', {at});
         INSERT INTO tasks VALUES ('t-fix-acme-invoice-rounding', 'req-acme', '{node}', 'Fix the Acme invoice rounding', 'running', 3, 1, 1, NULL, 'pr-1', 0);
         INSERT INTO task_history VALUES (10, 't-fix-acme-invoice-rounding', 'for_your_review', 'Ana asked for the Q3 numbers');
         INSERT INTO domain_messages VALUES (1, 10, 0, 'agent', '{node}', 't-fix-acme-invoice-rounding', NULL, 'Use the staging database');
         INSERT INTO delivery_attempts VALUES (1, 1, 'discarded', 1, 'daemon_restarted_after_claim');
         INSERT INTO interactions VALUES ('q-1', 'task', 't-fix-acme-invoice-rounding', 'question', 'Use the staging database?', 1, 'user', NULL, NULL, NULL, 0, 1, '{{\"kind\":\"ask\",\"need\":\"x\"}}');
         INSERT INTO lifecycle_executions VALUES ('t-fix-acme-invoice-rounding', 1, '{node}', 'sdk-0abdea5e130f80de', 'conv-1', 'sdk-host', 'sdk');
         INSERT INTO pull_requests VALUES ('pr-1', 't-fix-acme-invoice-rounding', 'github.com', 'acme/billing', 42, 'feature/acme-sso', 'main', 'open');
         INSERT INTO task_models VALUES ('t-fix-acme-invoice-rounding', 'opus', 'orchestrator');
         INSERT INTO task_worktree_records VALUES ('t-fix-acme-invoice-rounding', '/Users/ana.lopez/Code/acme-billing', 'task', 'feature/acme-sso', 0, NULL);",
        at = NOW - 3_600_000
    ))
    .unwrap();
    let events = [
        json!({"kind": "admit", "data": {"node": node, "conversation": "conv-1", "instance": "sdk-0abdea5e130f80de", "host": "sdk-host"}}),
        json!({"kind": "decide", "data": {"card": "q-1", "decision": {"kind": "deny", "data": {"message": "the secret answer"}}}}),
        json!({"kind": "runtime", "data": {"stamp": {"task": "t-fix-acme-invoice-rounding", "node": node, "conversation": "conv-1", "instance": "sdk-0abdea5e130f80de", "host": "sdk-host", "generation": 1}, "cursor": 7, "fact": {"kind": "host_ended", "data": {"unheard_card": "q-1", "resume": null}}}}),
        json!({"kind": "request_control", "data": {"control": "ctl-1", "kind": {"kind": "set_model", "data": {"model": "claude-acme-key"}}}}),
        json!({"kind": "a_kind_from_a_newer_build", "data": {"text": "Fix the Acme invoice rounding"}}),
    ];
    for (revision, event) in events.iter().enumerate() {
        db.execute("INSERT INTO lifecycle_events VALUES ('t-fix-acme-invoice-rounding', ?1, ?2, ?3)", rusqlite::params![revision as i64 + 1, format!("lifecycle-{revision}"), event.to_string()]).unwrap();
    }
    if with_closings {
        db.execute_batch("INSERT INTO task_closings VALUES ('t-fix-acme-invoice-rounding', 1792846400000, 2, '/Users/ana.lopez/Code/acme-billing is busy');").unwrap();
    }
}

fn outbox_fixture(home: &Path) {
    let dir = home.join("sdk/sdk-0abdea5e130f80de");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("launch-options.json"), r#"{"model":"opus","effort":"high","disallowed_tools":["Bash"]}"#).unwrap();
    std::fs::write(dir.join("host.lock"), r#"{"control_secret":"feedfacefeedfacefeedfacefeedface"}"#).unwrap();
    let frames = [
        json!({"protocol_version": 1, "body": {"type": "command", "command_id": "c1", "action": {"type": "start", "cwd": "/Users/ana.lopez/Code/acme-billing", "prompt": "Fix the Acme invoice rounding"}}}),
        json!({"protocol_version": 1, "body": {"type": "event", "cursor": 1, "observation": {"type": "sdk", "kind": "message", "payload": {"content": "Use the staging database", "file": "/Users/ana.lopez/Code/acme-billing/invoice.rs"}}}}),
        json!({"protocol_version": 1, "body": {"type": "event", "cursor": 2, "observation": {"type": "sdk", "kind": "result", "payload": {"terminal_reason": "completed", "subtype": "success", "is_error": false, "result": "Ana asked for the Q3 numbers"}}}}),
        json!({"protocol_version": 1, "body": {"type": "event", "cursor": 3, "observation": {"type": "process_exited", "exit_code": 0, "signal": null}}}),
        json!({"protocol_version": 1, "body": {"type": "mystery", "text": "Fix the Acme invoice rounding"}}),
    ];
    let mut outbox = Vec::new();
    for frame in frames {
        let bytes = frame.to_string().into_bytes();
        outbox.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
        outbox.extend_from_slice(&bytes);
    }
    outbox.extend_from_slice(&[0, 0, 1]);
    std::fs::write(dir.join("outbox"), outbox).unwrap();
}

pub(super) fn full_home(home: &Path) {
    domain_fixture(home, 5, true);
    outbox_fixture(home);
    std::fs::write(home.join("alondrad.log"), fixture("alondrad.log").replace("2026-10-23", "2026-10-25").replace("2026-10-24", "2026-10-25")).unwrap();
    std::fs::create_dir_all(home.join("machine-samples")).unwrap();
    std::fs::write(
        home.join("machine-samples/2026-10-25.csv"),
        format!("{}\n{},41,2.5,16,room,false,3,warning,0.2,4.1,,,,,,,\n{},55,1.0,16,/Users/ana.lopez,true,2,normal,,,,,,,,,\n", super::collect::SAMPLE_COLUMNS.join(","), NOW - 600_000, NOW - 300_000),
    )
    .unwrap();
}

fn files_text(files: &bundle::Files) -> String {
    files.iter().map(|(name, text)| format!("== {name}\n{text}")).collect()
}

#[test]
fn a_report_of_a_mac_with_its_daemon_down_has_every_section_and_nothing_private() {
    let home = Directory::new();
    full_home(&home);
    let mac = FakeMac::new(&home);
    let built = build(&mac, hours_before(72), Some("It stopped after the Mac slept".into())).unwrap();
    let all = files_text(&built.files);
    let mut private: Vec<String> = private_values();
    private.extend(PRIVATE_TEXT.iter().map(|s| s.to_string()));
    private.extend(["Ana's MacBook Pro", "anas-macbook-pro", "analopez-acme", "feedfacefeedface", "req-acme", "+02:00"].map(String::from));
    assert_has_none_of(&all, &private);
    assert!(built.hits.is_empty(), "{:?}", built.hits);
    let names: Vec<&str> = built.files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["manifest.json", "description.txt", "versions.json", "machine.json", "machine-samples.csv", "doctor.json", "daemon.log", "lifecycle.jsonl", "tasks.jsonl", "nodes.jsonl", "sdk-host.jsonl"]);
    let get = |name: &str| built.files.iter().find(|(n, _)| n == name).unwrap().1.clone();
    let versions: Value = serde_json::from_str(&get("versions.json")).unwrap();
    assert_eq!(versions["daemon_running"], false);
    assert_eq!(versions["claude"], "2.1.30");
    let machine: Value = serde_json::from_str(&get("machine.json")).unwrap();
    assert_eq!(machine["chip"], "Apple M3 Max");
    assert_eq!(machine["launchd_job"], json!({"state": "running", "last_exit_code": 75, "runs": 4}));
    assert_eq!(machine["sleep_and_wake"], json!([{"at": "2026-10-23T23:10:05.000Z", "kind": "Sleep"}, {"at": "2026-10-24T04:30:00.000Z", "kind": "Wake"}]));
    assert_eq!(serde_json::from_str::<Value>(&get("doctor.json")).unwrap(), json!({"checks": [{"id": "service", "status": "pass"}, {"id": "alondrad", "status": "fail"}]}));
    assert!(get("sdk-host.jsonl").contains("not available: the daemon is not running"));
    assert!(get("machine-samples.csv").contains(",value-"), "a value no sample column holds is replaced");
    let manifest: Value = serde_json::from_str(&get("manifest.json")).unwrap();
    assert_eq!(manifest["omitted"]["lifecycle_events_not_decoded"], 1);
    assert_eq!(manifest["omitted"]["sdk_host_frames_of_unknown_type"], 1);
    assert_eq!(manifest["daemon_running"], false);
    assert!(!names.contains(&bundle::PSEUDONYMS), "the table of pseudonyms never enters the package");
    let table = built.dir.join(bundle::PSEUDONYMS);
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&table).unwrap().permissions().mode() & 0o777, 0o600);
    assert!(std::fs::read_to_string(&table).unwrap().contains("t-fix-acme-invoice-rounding"));
}

#[test]
fn the_state_sections_keep_only_structure_and_one_pseudonym_per_id() {
    let home = Directory::new();
    full_home(&home);
    let built = build(&FakeMac::new(&home), hours_before(72), None).unwrap();
    let get = |name: &str| built.files.iter().find(|(n, _)| n == name).unwrap().1.clone();
    let task: Value = serde_json::from_str(get("tasks.jsonl").lines().next().unwrap()).unwrap();
    let pseudonym = task["task"].as_str().unwrap().to_owned();
    assert!(pseudonym.starts_with("task-"));
    assert_eq!(task["state"], "running");
    assert_eq!(task["pull_request"], "open");
    assert_eq!(task["messages"], 1);
    assert_eq!(task["message_deliveries"], json!([{"state": "discarded", "reason": "daemon_restarted_after_claim"}]));
    assert_eq!(task["open_questions"], json!([{"kind": "question", "blocking": true, "for": "user"}]));
    assert_eq!(task["closing"]["failures"], 2);
    let lifecycle = get("lifecycle.jsonl");
    assert!(lifecycle.lines().all(|l| l.contains(&format!("\"task\":\"{pseudonym}\""))), "{lifecycle}");
    let ended: Value = serde_json::from_str(lifecycle.lines().find(|l| l.contains("host_ended")).unwrap()).unwrap();
    assert_eq!(ended["instance"], "sdk-0abdea5e130f80de");
    assert_eq!(ended["node"], task["node"]);
    let node: Value = serde_json::from_str(get("nodes.jsonl").lines().next().unwrap()).unwrap();
    assert_eq!(node["node"], task["node"]);
    assert_eq!(node["phase"]["kind"], "waiting_background");
    assert_eq!(node["last_session_end"]["result"]["terminal_reason"], "completed");
    assert_eq!(node["last_session_end"]["process"]["exit_code"], 0);
    let host: Value = serde_json::from_str(get("sdk-host.jsonl").lines().next().unwrap()).unwrap();
    let kinds: Vec<&str> = host["frames"].as_array().unwrap().iter().map(|f| f["type"].as_str().unwrap()).collect();
    assert_eq!(kinds, ["command", "event", "event", "event"]);
    assert!(get("daemon.log").contains(&format!("of {pseudonym} went to its SDK host")), "the log names the task as the state does");
}

#[test]
fn a_database_of_an_older_version_is_read_and_a_newer_one_is_not() {
    let home = Directory::new();
    domain_fixture(&home, 4, false);
    let built = build(&FakeMac::new(&home), hours_before(72), None).unwrap();
    assert!(built.files.iter().any(|(n, t)| n == "tasks.jsonl" && t.contains("\"closing\":null")));
    let newer = Directory::new();
    domain_fixture(&newer, 6, true);
    let built = build(&FakeMac::new(&newer), hours_before(72), None).unwrap();
    let tasks = built.sections.iter().find(|s| s.name == "tasks.jsonl").unwrap();
    assert_eq!(tasks.status, Status::Empty("domain.db is of a newer version (6) than this alondra reads (5)".into()));
}

#[test]
fn an_empty_home_still_makes_a_report() {
    let home = Directory::new();
    std::fs::create_dir_all(&*home).unwrap();
    let built = build(&FakeMac::new(&home), hours_before(24), None).unwrap();
    assert!(built.hits.is_empty());
    let empty: Vec<&str> = built.sections.iter().filter(|s| s.status != Status::Ok).map(|s| s.name).collect();
    assert_eq!(empty, ["machine-samples.csv", "daemon.log", "lifecycle.jsonl", "tasks.jsonl", "nodes.jsonl", "sdk-host.jsonl"]);
}

#[test]
fn the_final_check_finds_every_shape_and_every_value_of_this_mac() {
    for (line, what) in [
        ("key sk-ant-api03-abcdef", "an Anthropic key"),
        ("token ghp_0123456789abcdef", "a GitHub token"),
        ("github_pat_11ABCDEF", "a GitHub token"),
        ("jwt eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig", "a JWT"),
        ("-----BEGIN PRIVATE KEY-----", "a key"),
        ("Authorization: Bearer abc", "an Authorization header"),
        ("mail ana.lopez@acme.com", "an email"),
        ("host 192.168.1.20 down", "an IP address"),
        ("host fe80::1c2b:3cff:fe4d:5e6f down", "an IP address"),
    ] {
        assert_eq!(canary::shapes(line).first(), Some(&what), "{line}");
    }
    for clean in ["2026-10-25T05:43:41.854Z version 0.3.5 took 1.5s", "Event::HostEnded at 14:13:20", "repo-1/pull/42 task-3"] {
        assert!(canary::shapes(clean).is_empty(), "{clean}: {:?}", canary::shapes(clean));
    }
    let home = Directory::new();
    full_home(&home);
    let mac = FakeMac::new(&home);
    let catalog = Catalog::built();
    let private = private::of_this_mac(&mac, &catalog.vocabulary);
    for leak in ["Fix the Acme invoice rounding", "acme/billing", "feature/acme-sso", "Ana's MacBook Pro", "analopez-acme", "claude-acme-key", "ana.lopez"] {
        let hits = canary::scan("daemon.log", &format!("fine\nsomething {leak} here\n"), Some(&private));
        assert_eq!(hits.len(), 1, "{leak}: {hits:?}");
        assert_eq!(hits[0].line, 2);
    }
    assert!(canary::scan("description.txt", "my acme/billing repo broke", None).is_empty(), "the user's own words are checked for secrets only");
    assert!(!canary::scan("description.txt", "my token ghp_0123456789abcdef", None).is_empty());
}

#[test]
fn a_report_the_final_check_refuses_is_never_saved() {
    let home = Directory::new();
    full_home(&home);
    let mac = FakeMac::new(&home);
    let built = build(&mac, hours_before(72), None).unwrap();
    let leaked = "2026-10-25T05:00:00.000Z repo acme/billing is busy\n";
    std::fs::write(built.dir.join("daemon.log"), leaked).unwrap();
    let manifest_path = built.dir.join(bundle::MANIFEST);
    let mut manifest: Value = serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    for section in manifest["sections"].as_array_mut().unwrap() {
        if section["name"] == "daemon.log" {
            section["sha256"] = json!(bundle::sha256(leaked.as_bytes()));
        }
    }
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
    let out = home.join("out.txt");
    let error = save(&mac, &built.id, &out).unwrap_err().to_string();
    assert!(error.contains("daemon.log, line 1"), "{error}");
    assert!(!out.exists());
}

#[test]
fn a_report_changed_after_its_preview_is_refused() {
    let home = Directory::new();
    full_home(&home);
    let mac = FakeMac::new(&home);
    let built = build(&mac, hours_before(72), None).unwrap();
    let out = home.join("out.txt");
    std::fs::write(built.dir.join("versions.json"), "{}\n").unwrap();
    assert!(save(&mac, &built.id, &out).unwrap_err().to_string().contains("versions.json changed after the preview"));
    let again = build(&mac, hours_before(72), None).unwrap();
    std::fs::write(again.dir.join("notes.txt"), "extra").unwrap();
    assert!(save(&mac, &again.id, &out).unwrap_err().to_string().contains("notes.txt was added"));
    assert!(!out.exists());
}

#[test]
fn a_saved_report_opens_into_the_same_files() {
    let home = Directory::new();
    full_home(&home);
    let mac = FakeMac::new(&home);
    let built = build(&mac, hours_before(72), Some("It stopped after the Mac slept".into())).unwrap();
    let file = home.join(default_file_name(&built.id));
    save(&mac, &built.id, &file).unwrap();
    assert!(std::fs::read_to_string(&file).unwrap().starts_with(&format!("alondra-report 1 plain {}\n", built.id)));
    assert!(save(&mac, &built.id, &file).is_err(), "never overwrites a file");
    let reader = Directory::new();
    std::fs::create_dir_all(&*reader).unwrap();
    let (id, dir, files) = open_file(&reader, &file).unwrap();
    assert_eq!(id, built.id);
    assert_eq!(files, built.files);
    assert_eq!(std::fs::read_to_string(dir.join("tasks.jsonl")).unwrap(), built.files.iter().find(|(n, _)| n == "tasks.jsonl").unwrap().1);
    let summary = received_summary(&id, &dir, &files);
    assert!(summary.contains("«It stopped after the Mac slept»"), "{summary}");
    assert!(summary.contains("alondrad (fail)"), "{summary}");
    assert!(summary.contains("host_ended"), "{summary}");
    std::fs::write(&file, "alondra-report 1 plain r-20261025-00000000\nnot base64!\n").unwrap();
    assert!(open_file(&reader, &file).is_err());
}

#[test]
fn a_report_over_five_megabytes_loses_its_oldest_log_lines_first() {
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut random_line = |at: usize| {
        let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let body: String = (0..120)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                alphabet[(seed % 64) as usize] as char
            })
            .collect();
        format!("line {at:06} {body}")
    };
    let log: String = (0..70_000).map(|at| random_line(at) + "\n").collect();
    let versions = "{\"cli\": \"0.3.5\"}\n".to_owned();
    let mut draft = Draft {
        id: "r-20261025-00000000".into(),
        created_at: NOW,
        window: hours_before(72),
        sections: vec![
            Section { name: "versions.json", text: versions.clone(), status: Status::Ok },
            Section { name: "daemon.log", text: log, status: Status::Ok },
            Section { name: "lifecycle.jsonl", text: "{\"event\":\"admit\"}\n".repeat(10), status: Status::Ok },
        ],
        description: None,
        omitted: Default::default(),
        log_capped: false,
        daemon_running: true,
    };
    let (files, trims) = bundle::assemble(&mut draft);
    assert!(bundle::compressed(&files).len() <= bundle::MAX_COMPRESSED);
    assert_eq!(trims.len(), 1, "{trims:?}");
    assert_eq!(trims[0].section, "daemon.log");
    let kept = &files.iter().find(|(n, _)| n == "daemon.log").unwrap().1;
    assert!(kept.lines().last().unwrap().starts_with(&random_line_number(69_999)), "the newest lines stay");
    assert!(!kept.contains("line 000000 "), "the oldest lines go");
    assert_eq!(files.iter().find(|(n, _)| n == "versions.json").unwrap().1, versions);
    let manifest: Value = serde_json::from_str(&files[0].1).unwrap();
    assert_eq!(manifest["trimmed"][0]["section"], "daemon.log");
    assert_eq!(manifest["trimmed"][0]["lines"], trims[0].lines);
}

fn random_line_number(at: usize) -> String {
    format!("line {at:06} ")
}

#[test]
fn the_preview_speaks_the_language_of_the_user() {
    let home = Directory::new();
    full_home(&home);
    let mac = FakeMac::new(&home);
    std::fs::remove_file(home.join("domain.db")).unwrap();
    crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
    let built = build(&mac, hours_before(72), None).unwrap();
    let text = summary(&built);
    assert!(text.contains("no hay domain.db en este Mac"), "{text}");
    assert!(text.contains("líneas del log que no son de ninguna plantilla del código: "), "{text}");
    assert!(!text.contains("there is no") && !text.contains("daemon_log_lines_without_template"), "{text}");
}

#[test]
fn only_the_known_windows() {
    assert_eq!(window("24h", NOW).unwrap().from, NOW - 86_400_000);
    assert_eq!(window("7d", NOW).unwrap().hours, 168);
    assert!(window("3w", NOW).is_err());
}

#[test]
fn private_values_too_short_or_of_the_code_say_nothing() {
    let catalog = Catalog::built();
    let mut private = Private::default();
    private.add("x", "ana", &catalog.vocabulary);
    private.add("x", "running", &catalog.vocabulary);
    private.add("x", "0.3.5", &catalog.vocabulary);
    assert_eq!(private.len(), 0);
    assert!(canary::scan("daemon.log", "the task is running", Some(&private)).is_empty());
}
