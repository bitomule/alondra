use super::*;
use alondra_core::domain::*;
use std::os::unix::fs::DirBuilderExt;

struct Fixture {
    root: crate::test_support::Directory,
    runtime: DomainRuntime,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn publish(runtime: &DomainRuntime, task: &str, id: &str, body: String) {
    let state = runtime.state().unwrap();
    runtime
        .execute(
            &Actor::System,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: format!("cmd-{id}").into(),
                expected_revision: state.revision,
                command: Command::PublishInformation {
                    attachments: vec![],
                    interaction_id: id.into(),
                    scope: Scope::Task { task_id: task.into() },
                    recipient: Recipient::User,
                    body, card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update"),
                },
            },
            None,
            || Ok(()),
        )
        .unwrap();
}

const AGENTS: usize = 17;

fn run(runtime: &DomainRuntime, actor: &Actor, id: &str, command: Command) {
    let state = runtime.state().unwrap();
    runtime
        .execute(
            actor,
            &CommandEnvelope {
                protocol_version: PROTOCOL_VERSION,
                data_epoch: state.data_epoch.clone(),
                command_id: id.into(),
                expected_revision: state.revision,
                command,
            },
            None,
            || Ok(()),
        )
        .unwrap();
}

/// A live-sized domain v2 store: one Request, Agent and Task per index, then
/// a thousand report-sized cards.
fn live_shaped() -> Fixture {
    let root = crate::test_support::Directory::new();
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let runtime = DomainRuntime::open(&root).unwrap();
    let user = Actor::User { user_id: "device:phone".into() };
    for index in 0..AGENTS {
        run(&runtime, &user, &format!("request-{index}"),
            Command::CreateRequest { request_id: format!("r-{index}").into(), body: format!("Request {index}"), auto_merge: false, maintenance: false, message: false });
        run(&runtime, &Actor::System, &format!("agent-{index}"),
            Command::RegisterAgent { agent_id: format!("node-{index}").into(), name: format!("Node {index}"), at: 0 });
        run(&runtime, &Actor::Orchestrator { generation: 0 }, &format!("task-{index}"), Command::CreateTask { plan_items: None,
            task_id: format!("t-{index}").into(),
            request_id: format!("r-{index}").into(),
            title: format!("Task {index}"),
            agent_id: Some(format!("node-{index}").into()),
            review_prs: true,
            dependencies: Default::default(),
            stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")),
        });
    }
    let mut index = 0;
    while runtime.state().unwrap().interactions.len() < 1_000 {
        for _ in 0..40 {
            let body = format!("Report {index}\n\n{}", "Findings and evidence line.\n".repeat(60));
            publish(&runtime, &format!("t-{}", index % AGENTS), &format!("fill-info-{index}"), body);
            index += 1;
        }
    }
    Fixture { root, runtime }
}

/// Cost is pinned by what a command reads and writes, never by the wall clock:
/// no full-table load once the store is warm, and a handful of row writes.
#[test]
fn a_command_on_a_live_sized_store_writes_a_handful_of_rows_and_reloads_no_table() {
    const MAX_WRITES_PER_COMMAND: u64 = 12;
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let counts = || crate::domain_store::statement_counts();
    let rows_written = || runtime.store.lock().unwrap().rows_written();
    for index in 0..20 {
        let (writes_before, scans_before) = counts();
        let rows_before = rows_written();
        if index % 2 == 0 {
            publish(runtime, "t-0", &format!("cost-info-{index}"), format!("Progress {index}"));
        } else {
            run(runtime, &Actor::User { user_id: "device:phone".into() }, &format!("cost-request-{index}"),
                Command::CreateRequest { request_id: format!("cost-request-{index}").into(), body: format!("Request {index}"), auto_merge: false, maintenance: false, message: false });
        }
        let (writes_after, scans_after) = counts();
        assert_eq!(scans_after, scans_before, "command {index} reloaded a table from SQL");
        assert!(writes_after - writes_before <= MAX_WRITES_PER_COMMAND, "command {index} issued {} writing statements", writes_after - writes_before);
        assert!(rows_written() - rows_before <= MAX_WRITES_PER_COMMAND, "command {index} wrote {} rows", rows_written() - rows_before);
    }
    let (_, scans_before) = counts();
    let read = runtime.read();
    assert_eq!(counts().1, scans_before, "reading the index reloaded a table from SQL");
    let (writes_before, _) = counts();
    assert!(std::sync::Arc::ptr_eq(&read, &runtime.read()), "an unchanged revision serves the same index");
    assert_eq!(counts().0, writes_before);
}
