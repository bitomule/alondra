//! A legacy HOME shaped like the live one (docs/domain-v2-live-cutover.md):
//! the same record counts with synthetic content. Rehearsals and latency
//! measurements run against this fixture, never against the real HOME.
use super::*;
use alondra_core::pr;
use serde_json::json;
use std::time::Instant;

pub(super) const REQUESTS: usize = 23;
pub(super) const ORIGINAL_TASKS: usize = 41;
pub(super) const ACTIVE_TASKS: usize = 17;
pub(super) const NODES: usize = 18;
pub(super) const PRS: u64 = 121;
pub(super) const FORMAL_PRS: u64 = 112;
pub(super) const QUESTIONS: u64 = 274;
pub(super) const OPEN_QUESTIONS: u64 = 4;
pub(super) const REPORTS: u64 = 287;
pub(super) const DOCUMENTS: u64 = 24;
pub(super) const MESSAGES: u64 = 805;

/// Writes `alondra.db` under `source` and returns the View it holds.
pub(super) fn write_live_shaped_home(source: &Path) -> View {
    let mut view = View::default();
    view.seq = 1;
    view.board.recorded_since = Some(1);
    for index in 0..NODES {
        let mut node = alondra_core::model::Node::new(&format!("node-{index}"), 1);
        node.name = Some(format!("Node {index}"));
        node.alive = index + 1 < NODES;
        view.nodes.insert(node.session_id.clone(), node);
    }
    for index in 0..REQUESTS {
        view.board.requests.insert(
            format!("r-{index}"),
            serde_json::from_value(json!({
                "id":format!("r-{index}"),"text":format!("Synthetic request {index}"),"created_at":index
            }))
            .unwrap(),
        );
    }
    for index in 0..ORIGINAL_TASKS {
        let active = index < ACTIVE_TASKS;
        let mut task = json!({
            "id":format!("t-{index}"),"request":format!("r-{}", index % REQUESTS),
            "title":format!("Synthetic task {index}"),
            "state":if active { "in_review" } else { "done" },
            "state_since":1,"created_at":index,"recorded":true,
            "assignee":{"session_id":format!("node-{}", index % NODES)}
        });
        if (1..9).contains(&index) {
            task["final_report"] = json!(index as u64);
        }
        view.board.tasks.insert(format!("t-{index}"), serde_json::from_value(task).unwrap());
    }
    for id in 1..=PRS {
        let mut item = pr::PullRequest::opened(
            id,
            &format!("https://github.com/owner/repo/pull/{id}"),
            &format!("node-{}", id as usize % NODES),
            None,
            id as i64,
        )
        .unwrap();
        let task = (id - 1) % REQUESTS as u64;
        item.task = Some(format!("t-{task}"));
        if id <= FORMAL_PRS {
            item.head_sha = Some(format!("{id:040x}"));
            item.base = Some("main".into());
            item.merge_methods = vec![pr::MergeMethod::Squash];
            if id as usize > ACTIVE_TASKS {
                item.state = pr::PrState::Merged;
            }
        }
        view.prs.insert(id, item);
    }
    for id in 1..=QUESTIONS {
        let open = id <= OPEN_QUESTIONS;
        let session = if id == OPEN_QUESTIONS { NODES - 1 } else { id as usize % NODES };
        let mut question = json!({
            "id":id,"session_id":format!("node-{session}"),"node_name":null,
            "title":format!("Synthetic question {id}"),"blocking":open,"origin":"ask",
            "status":if open { "open" } else { "answered" },"asked_at":id,
            "task":format!("t-{}", id as usize % ORIGINAL_TASKS),
            "options":[{"label":"Yes"},{"label":"No"}]
        });
        if !open {
            question["answer"] = json!({"text":"","option":"Yes","by":"user","at":id + 1});
        }
        view.questions.insert(id, serde_json::from_value(question).unwrap());
    }
    for id in 1..=REPORTS {
        view.reports.insert(
            id,
            serde_json::from_value(json!({
                "id":id,"session_id":format!("node-{}", id as usize % NODES),"node_name":null,
                "title":format!("Synthetic report {id}"),"body":format!("Body of report {id}"),
                "kind":if id <= DOCUMENTS { "document" } else { "notice" },
                "created_at":id,"task":format!("t-{}", id as usize % ORIGINAL_TASKS)
            }))
            .unwrap(),
        );
    }
    for id in 1..=MESSAGES {
        let mut message = json!({
            "id":id,"session_id":format!("node-{}", id as usize % NODES),"from":"user",
            "text":format!("Synthetic message {id}"),"created_at":id
        });
        if id % 2 == 0 {
            message["about"] = json!({"kind":"task","task":format!("t-{}", id as usize % ORIGINAL_TASKS)});
        }
        view.messages.insert(id, serde_json::from_value(message).unwrap());
    }
    let db = Connection::open(source.join("alondra.db")).unwrap();
    db.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE event_log(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,kind TEXT NOT NULL,session TEXT,body TEXT NOT NULL); CREATE TABLE snapshots(seq INTEGER PRIMARY KEY,at INTEGER NOT NULL,body TEXT NOT NULL);").unwrap();
    db.execute(
        "INSERT INTO snapshots(seq,at,body) VALUES(1,1,?1)",
        [serde_json::to_string(&view).unwrap()],
    )
    .unwrap();
    drop(db);
    view
}

#[test]
fn live_shaped_home_rehearses_with_its_counts_and_reports_command_latency() {
    use alondra_core::domain_v2::*;
    let root = std::env::temp_dir().join(format!(
        "alondra-live-shaped-{}",
        crate::auth::random_hex(8)
    ));
    let source = root.join("source");
    let stage = root.join("stage");
    fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
    write_live_shaped_home(&source);
    let started = Instant::now();
    let rehearsal = rehearse(&source, &stage).unwrap();
    let rehearse_ms = started.elapsed().as_millis();
    let imported = &rehearsal.imported;
    assert_eq!(imported.requests, REQUESTS);
    assert_eq!(imported.tasks, ORIGINAL_TASKS);
    assert_eq!(imported.split_pr_tasks, FORMAL_PRS as usize - REQUESTS);
    assert_eq!(imported.agents, NODES);
    assert_eq!(imported.formal_prs, FORMAL_PRS as usize);
    assert_eq!(imported.reference_only_prs.len(), (PRS - FORMAL_PRS) as usize);
    assert_eq!(imported.questions, QUESTIONS as usize);
    assert_eq!(imported.open_questions_reopened, OPEN_QUESTIONS as usize - 1);
    assert_eq!(imported.open_questions_requiring_reconciliation, 1);
    assert_eq!(imported.reports, REPORTS as usize);
    assert_eq!(imported.documents, DOCUMENTS as usize);
    assert_eq!(imported.messages, MESSAGES as usize);
    assert_eq!(imported.final_presentations, ACTIVE_TASKS);
    let runtime = crate::domain_runtime::DomainRuntime::open(&stage).unwrap();
    let state = runtime.state().unwrap();
    assert_eq!(state.tasks.len(), ORIGINAL_TASKS + imported.split_pr_tasks);
    assert_eq!(
        state.tasks.values().filter(|task| !task.state.terminal()).count(),
        ACTIVE_TASKS,
        "only the 17 in-flight Tasks stay open; historical open questions block nothing"
    );
    // Cost is pinned by what a command reads and writes, never by the wall clock:
    // no full-table load once the store is warm, and a handful of row writes.
    const MAX_WRITES_PER_COMMAND: u64 = 12;
    let counts = || crate::domain_store::statement_counts();
    let rows_written = || runtime.store.lock().unwrap().rows_written();
    let mut latencies = vec![];
    let mut max_statements = 0;
    let mut max_rows = 0;
    for index in 0..20u32 {
        let (writes_before, scans_before) = counts();
        let rows_before = rows_written();
        let revision = runtime.state().unwrap().revision;
        let envelope = CommandEnvelope {
            protocol_version: PROTOCOL_VERSION,
            data_epoch: state.data_epoch.clone(),
            command_id: format!("latency-{index}").into(),
            expected_revision: revision,
            command: if index % 2 == 0 {
                Command::PublishInformation {
                    attachments: vec![],
                    interaction_id: format!("latency-info-{index}").into(),
                    scope: Scope::Task { task_id: "t-0".into() },
                    recipient: Recipient::User,
                    body: format!("Progress {index}"),
                }
            } else {
                Command::CreateRequest {
                    request_id: format!("latency-request-{index}").into(),
                    body: format!("Request {index}"),
                }
            },
        };
        let actor = if index % 2 == 0 {
            Actor::Agent { agent_id: "node-0".into() }
        } else {
            Actor::User { user_id: "device:phone".into() }
        };
        let started = Instant::now();
        runtime.execute(&actor, &envelope, None, || Ok(())).unwrap();
        latencies.push(started.elapsed().as_millis());
        let (writes_after, scans_after) = counts();
        assert_eq!(scans_after, scans_before, "command {index} reloaded a table from SQL");
        let statements = writes_after - writes_before;
        let rows = rows_written() - rows_before;
        assert!(statements <= MAX_WRITES_PER_COMMAND, "command {index} issued {statements} writing statements");
        assert!(rows <= MAX_WRITES_PER_COMMAND, "command {index} wrote {rows} rows");
        max_statements = max_statements.max(statements);
        max_rows = max_rows.max(rows);
    }
    let (_, scans_before) = counts();
    let started = Instant::now();
    let projection = runtime.snapshot(crate::service_overview::MAX_BYTES).unwrap();
    let projection_ms = started.elapsed().as_millis();
    assert_eq!(counts().1, scans_before, "the projection reloaded a table from SQL");
    let (writes_before, _) = counts();
    assert_eq!(runtime.snapshot(crate::service_overview::MAX_BYTES).unwrap(), projection,
        "an unchanged revision serves the cached projection");
    assert_eq!(counts().0, writes_before);
    let max = latencies.iter().copied().max().unwrap();
    let average = latencies.iter().sum::<u128>() / latencies.len() as u128;
    eprintln!(
        "live-shaped HOME: rehearse {rehearse_ms} ms; command latency avg {average} ms, max {max} ms over {} commands (at most {max_statements} writing statements and {max_rows} rows per command); projection {} bytes in {projection_ms} ms",
        latencies.len(),
        projection.len()
    );
    drop(runtime);
    fs::remove_dir_all(root).unwrap();
}
