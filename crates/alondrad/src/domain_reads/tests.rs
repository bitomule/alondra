use super::*;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;


struct Fixture {
    root: PathBuf,
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
                data_epoch: state.data_epoch,
                command_id: format!("cmd-{id}").into(),
                expected_revision: state.revision,
                command: Command::PublishInformation {
                    attachments: vec![],
                    interaction_id: id.into(),
                    scope: Scope::Task { task_id: task.into() },
                    recipient: Recipient::User,
                    body, card: alondra_core::domain_v2::CardSpec::derived(alondra_core::domain_v2::CardKind::Know, "Read the update"),
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
                data_epoch: state.data_epoch,
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
/// enough report-sized bodies to exercise resource pagination.
fn live_shaped() -> Fixture {
    let root = std::env::temp_dir().join(format!("alondra-reads-{}", crate::auth::random_hex(8)));
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    let runtime = DomainRuntime::open(&root).unwrap();
    let user = Actor::User { user_id: "device:phone".into() };
    for index in 0..AGENTS {
        run(&runtime, &user, &format!("request-{index}"),
            Command::CreateRequest { request_id: format!("r-{index}").into(), body: format!("Request {index}"), auto_merge: false });
        run(&runtime, &Actor::System, &format!("agent-{index}"),
            Command::RegisterAgent { agent_id: format!("node-{index}").into(), name: format!("Node {index}") });
        run(&runtime, &user, &format!("task-{index}"), Command::CreateTask {
            task_id: format!("t-{index}").into(),
            request_id: format!("r-{index}").into(),
            title: format!("Task {index}"),
            agent_id: Some(format!("node-{index}").into()),
            review_prs: true,
            dependencies: Default::default(),
            stack: None,
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

fn get(runtime: &DomainRuntime, path: &str) -> (u16, Value, usize) {
    let route = parse(path).unwrap_or_else(|| panic!("{path} must parse"));
    let response = respond(runtime, &route);
    let status = response.status().as_u16();
    let bytes = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap(), bytes.len())
}

fn walk(runtime: &DomainRuntime, base: &str) -> (Vec<Value>, Vec<String>) {
    let mut items = vec![];
    let mut truncated = vec![];
    let mut cursor: Option<String> = None;
    loop {
        let path = match &cursor {
            Some(c) => format!("{base}{}cursor={c}", if base.contains('?') { "&" } else { "?" }),
            None => base.to_owned(),
        };
        let (status, page, bytes) = get(runtime, &path);
        assert_eq!(status, 200, "{path}: {page}");
        assert!(bytes <= MAX_PAGE_BYTES, "{path} is {bytes} bytes");
        let page: Page<Value> = serde_json::from_value(page).unwrap();
        assert!(!page.items.is_empty() || page.total == 0, "{path} served an empty page");
        items.extend(page.items);
        truncated.extend(page.truncated);
        assert!(page.total as usize >= items.len(), "{path} total below served rows");
        match page.next_cursor {
            Some(next) => cursor = Some(next),
            None => {
                assert_eq!(page.total as usize, items.len(), "{base}: total and served rows differ");
                return (items, truncated);
            }
        }
    }
}

#[test]
fn every_resource_read_fits_the_relay_and_covers_its_section() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let (status, summary, bytes) = get(runtime, "/summary");
    assert_eq!(status, 200);
    assert!(bytes <= MAX_SUMMARY_BYTES, "summary is {bytes} bytes");
    let summary: Summary = serde_json::from_value(summary).unwrap();
    assert_eq!(summary.sections.len(), SECTIONS.len());
    let mut ids = std::collections::HashSet::new();
    for (base, section) in [
        ("/inbox", "inbox"),
        ("/tasks", "tasks"),
        ("/requests", "requests"),
        ("/agents", "agents"),
        ("/pull-requests", "pull_requests"),
        ("/documents", "documents"),
        ("/merge-authorizations", "merge_authorizations"),
        ("/stack-readiness", "stack_readiness"),
        ("/superseded-acceptances", "superseded_acceptances"),
    ] {
        let (items, _) = walk(runtime, base);
        assert_eq!(items.len() as u64, summary.sections[section].count, "{section} count");
        ids.clear();
        for item in &items {
            let id = item.get("id").or_else(|| item["document"].get("id")).or_else(|| item.get("task_id")).unwrap();
            assert!(ids.insert(id.clone()), "{base} repeated {id}");
        }
    }
    let (orchestrator_items, _) = walk(runtime, "/interactions?scope=orchestrator");
    let mut per_task = 0;
    let (task_items, _) = walk(runtime, "/tasks");
    for task in &task_items {
        let task_id = task["id"].as_str().unwrap();
        let (status, detail, bytes) = get(runtime, &format!("/tasks/{task_id}"));
        assert_eq!(status, 200);
        assert!(bytes <= MAX_PAGE_BYTES, "task {task_id} detail is {bytes} bytes");
        let detail: TaskDetail = serde_json::from_value(detail).unwrap();
        assert_eq!(detail.request.id.0.as_str(), task["request_id"].as_str().unwrap());
        let (items, _) = walk(runtime, &format!("/tasks/{task_id}/interactions"));
        assert_eq!(items.len() as u64, detail.interactions);
        per_task += items.len();
    }
    assert_eq!(per_task + orchestrator_items.len(), summary.sections["interactions"].count as usize,
        "every interaction is reachable through its scope");
    let (inbox_items, _) = walk(runtime, "/inbox");
    let first_id = inbox_items[0]["id"].as_str().unwrap();
    let (status, lookup, _) = get(runtime, &format!("/inbox/{first_id}"));
    assert_eq!(status, 200);
    assert_eq!(lookup["root_id"], first_id);
    assert!(first_id.contains(':'), "the lookup below proves an encoded colon");
    let encoded = first_id.replace(':', "%3A");
    let (status, encoded_lookup, _) = get(runtime, &format!("/inbox/{encoded}"));
    assert_eq!(status, 200);
    assert_eq!(encoded_lookup, lookup, "a percent-encoded id names the same item");
    let (status, with, bytes) = get(runtime, "/summary?include=inbox,tasks,agents");
    assert_eq!(status, 200);
    assert!(bytes <= MAX_PAGE_BYTES, "summary with expansions is {bytes} bytes");
    let with: Summary = serde_json::from_value(with).unwrap();
    assert!(with.expansions.contains_key("inbox") && with.expansions.contains_key("agents"), "{:?}", with.pending);
    assert_eq!(with.expansions["inbox"].section_revision, summary.sections["inbox"].revision);
    eprintln!("live-shaped resource reads: {} inbox roots, summary {bytes} bytes with expansions", inbox_items.len());
}

#[test]
fn oversized_text_is_cut_with_a_signal_and_readable_in_chunks() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let body: String = (0..6000).map(|i| format!("línea {i} con evidencia larga\n")).collect();
    assert!(body.len() > MAX_PAGE_BYTES);
    publish(runtime, "t-1", "huge-info", body.clone());
    let (_, truncated) = walk(runtime, "/tasks/t-1/interactions");
    assert_eq!(truncated, vec!["huge-info".to_owned()]);
    let (_, inbox_truncated) = walk(runtime, "/inbox");
    assert!(inbox_truncated.contains(&"interaction:huge-info".to_owned()));
    let (status, detail, bytes) = get(runtime, "/interactions/huge-info");
    assert_eq!(status, 200);
    assert!(bytes <= MAX_PAGE_BYTES);
    let detail: Item<Value> = serde_json::from_value(detail).unwrap();
    assert!(detail.truncated);
    let cut = detail.item["body"].as_str().unwrap();
    assert!(body.starts_with(cut) && cut.len() < body.len());
    let mut assembled = String::new();
    loop {
        let (status, chunk, bytes) = get(runtime, &format!("/interactions/huge-info/body?offset={}", assembled.len()));
        assert_eq!(status, 200, "{chunk}");
        assert!(bytes <= MAX_CHUNK_ENCODED + 512);
        let chunk: TextChunk = serde_json::from_value(chunk).unwrap();
        assert_eq!(chunk.offset as usize, assembled.len());
        assert_eq!(chunk.total_bytes as usize, body.len());
        assembled.push_str(&chunk.data);
        if !chunk.has_more {
            break;
        }
    }
    assert_eq!(assembled, body);
    let inside_multibyte = body.find('í').unwrap() + 1;
    let (status, _, _) = get(runtime, &format!("/interactions/huge-info/body?offset={inside_multibyte}"));
    assert_eq!(status, 422, "an offset inside a multibyte character is refused");
    let (status, _, _) = get(runtime, &format!("/interactions/huge-info/body?offset={}", body.len() + 1));
    assert_eq!(status, 422);
    let (status, _, _) = get(runtime, "/interactions/absent/body?offset=0");
    assert_eq!(status, 404);
}

#[test]
fn section_tokens_change_only_for_the_sections_a_command_touches() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let before = runtime.reads.snapshot(runtime).unwrap().section_tokens();
    publish(runtime, "t-2", "one-more-info", "One more".into());
    let after = runtime.reads.snapshot(runtime).unwrap().section_tokens();
    let changed: Vec<&str> = SECTIONS.iter().copied().filter(|s| before[*s] != after[*s]).collect();
    assert_eq!(changed, ["inbox", "interactions"]);
    assert_eq!(after, runtime.reads.snapshot(runtime).unwrap().section_tokens(), "an unchanged revision keeps its tokens");
}

#[test]
fn paging_by_cursor_is_stable_and_malformed_cursors_are_refused() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let (single, _) = walk(runtime, "/inbox?limit=1");
    let (bulk, _) = walk(runtime, "/inbox?limit=200");
    assert_eq!(single, bulk);
    let (status, summary, _) = get(runtime, "/summary");
    assert_eq!(status, 200);
    let summary: Summary = serde_json::from_value(summary).unwrap();
    assert_eq!(single.len() as u64, summary.sections["inbox"].count);
    let (pending, _) = walk(runtime, "/inbox?filter=pending");
    assert!(pending.iter().all(|item| item["pending"] == true));
    let (by_agent, _) = walk(runtime, "/tasks?agent=node-3");
    assert!(by_agent.iter().all(|task| task["agent_id"] == "node-3"));
    assert!(!by_agent.is_empty());
    for cursor in ["notbase64!".to_owned(), URL_SAFE_NO_PAD.encode(b"{\"i\":\"x\"}"), URL_SAFE_NO_PAD.encode(b"[]")] {
        let path = format!("/inbox?cursor={cursor}");
        match parse(&path) {
            Some(route) => assert_eq!(respond(runtime, &route).status().as_u16(), 422, "{cursor}"),
            None => assert_eq!(cursor, "notbase64!"),
        }
    }
    let (status, _, _) = get(runtime, &format!("/tasks?cursor={}", URL_SAFE_NO_PAD.encode(b"{\"o\":1,\"i\":\"x\"}")));
    assert_eq!(status, 422, "an ordered cursor does not fit an id-ordered section");
}

#[test]
fn the_read_grammar_is_exact() {
    for path in [
        "/summary",
        "/summary?include=inbox,tasks",
        "/inbox",
        "/inbox?filter=pending&limit=200",
        "/inbox/interaction:abc-1",
        "/inbox/document:t-1:d-1:r-1",
        "/inbox/interaction%3Aabc-1",
        "/inbox/interaction%3aabc-1",
        "/interactions/i%2D1",
        "/tasks?request=r-1&agent=node-1",
        "/tasks/t-1",
        "/tasks/t-1/interactions?limit=5",
        "/interactions?scope=orchestrator",
        "/interactions/i-1",
        "/interactions/i-1/body?offset=0",
        "/requests",
        "/requests/r-1",
        "/requests/r-1/body?offset=10",
        "/agents",
        "/pull-requests",
        "/pull-requests/pr-1",
        "/pull-requests/github.com%2Fowner%2Frepo%23154",
        "/documents",
        "/documents/d-1",
        "/merge-authorizations",
        "/stack-readiness",
        "/superseded-acceptances",
    ] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in [
        "/v2/domain",
        "/",
        "/summary?",
        "/summary?include=",
        "/summary?include=schedules",
        "/summary?include=inbox,inbox",
        "/summary?include=inbox&include=tasks",
        "/inbox?limit=0",
        "/inbox?limit=201",
        "/inbox?filter=all",
        "/inbox?cursor=",
        "/inbox?cursor=a+b",
        "/inbox?other=1",
        "/inbox/../x",
        "/inbox/a%2Fb",
        "/pull-requests/github.com%2Fowner%2Frepo%23154%3F",
        "/pull-requests/github.com/owner/repo%23154",
        "/inbox/a%25",
        "/inbox/a%3",
        "/inbox/a%+1",
        "/inbox/%2e%2e",
        "/interactions/i%3A1",
        "/inbox/a/b",
        "/inbox/a?limit=1",
        "/tasks/t-1?limit=1",
        "/tasks/t-1/actions",
        "/tasks/t-1/interactions/x",
        "/interactions",
        "/interactions?scope=all",
        "/interactions?cursor=abc",
        "/interactions/i-1/body",
        "/interactions/i-1/body?offset=",
        "/interactions/i-1/body?offset=-1",
        "/interactions/i-1/body?offset=1&offset=2",
        "/requests/r%201",
        "/documents/d-1/content",
        "/schedules",
        "/changes?data_epoch=e&after=0&wait=0",
    ] {
        assert!(!approved_read_path(path), "{path}");
    }
    assert!(!approved_read_path(&format!("/inbox?cursor={}", "a".repeat(513))));
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
                Command::CreateRequest { request_id: format!("cost-request-{index}").into(), body: format!("Request {index}"), auto_merge: false });
        }
        let (writes_after, scans_after) = counts();
        assert_eq!(scans_after, scans_before, "command {index} reloaded a table from SQL");
        assert!(writes_after - writes_before <= MAX_WRITES_PER_COMMAND, "command {index} issued {} writing statements", writes_after - writes_before);
        assert!(rows_written() - rows_before <= MAX_WRITES_PER_COMMAND, "command {index} wrote {} rows", rows_written() - rows_before);
    }
    let (_, scans_before) = counts();
    let resource_snapshot = runtime.reads.snapshot(runtime).unwrap();
    assert_eq!(counts().1, scans_before, "the projection reloaded a table from SQL");
    let (writes_before, _) = counts();
    let repeated = runtime.reads.snapshot(runtime).unwrap();
    assert!(std::sync::Arc::ptr_eq(&resource_snapshot, &repeated),
        "an unchanged revision serves the same cached resource index");
    assert_eq!(counts().0, writes_before);
}
