use super::*;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;

const RELAY_LOCAL_BUDGET: usize = 180 * 1024;

struct Fixture {
    root: PathBuf,
    runtime: DomainRuntime,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn publish(runtime: &DomainRuntime, agent: &str, task: &str, id: &str, body: String) {
    let state = runtime.state().unwrap();
    runtime
        .execute(
            &Actor::Agent { agent_id: agent.into() },
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
                    body,
                },
            },
            None,
            || Ok(()),
        )
        .unwrap();
}

/// The live-shaped legacy HOME rehearsed into v2, then filled with report-sized
/// bodies until the whole projection no longer fits the relay, as the real one does.
fn live_shaped() -> Fixture {
    let root = std::env::temp_dir().join(format!("alondra-reads-{}", crate::auth::random_hex(8)));
    let source = root.join("source");
    let stage = root.join("stage");
    std::fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
    std::fs::DirBuilder::new().mode(0o700).create(&source).unwrap();
    crate::domain_migration::synthetic::write_live_shaped_home(&source);
    crate::domain_migration::rehearse(&source, &stage).unwrap();
    let runtime = DomainRuntime::open(&stage).unwrap();
    let mut index = 0;
    while runtime.snapshot(RELAY_LOCAL_BUDGET).is_ok() {
        for _ in 0..40 {
            let body = format!("Report {index}\n\n{}", "Findings and evidence line.\n".repeat(60));
            publish(&runtime, &format!("node-{}", index % 17), &format!("t-{}", index % 17), &format!("fill-info-{index}"), body);
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
fn every_sectioned_read_fits_the_relay_while_the_full_projection_does_not() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    assert!(runtime.snapshot(RELAY_LOCAL_BUDGET).is_err(), "the whole projection must exceed the relay budget for this proof");
    let (status, summary, bytes) = get(runtime, "/v2/domain/summary");
    assert_eq!(status, 200);
    assert!(bytes <= MAX_SUMMARY_BYTES, "summary is {bytes} bytes");
    let summary: Summary = serde_json::from_value(summary).unwrap();
    assert_eq!(summary.sections.len(), SECTIONS.len());
    let projection = runtime.projection().unwrap();
    let orchestrator = projection.interactions.iter().filter(|i| matches!(i.scope, Scope::Orchestrator { .. })).count();
    let mut ids = std::collections::HashSet::new();
    for (base, expected) in [
        ("/v2/domain/inbox", projection.inbox.len()),
        ("/v2/domain/tasks", projection.tasks.len()),
        ("/v2/domain/requests", projection.requests.len()),
        ("/v2/domain/agents", projection.agents.len()),
        ("/v2/domain/pull-requests", projection.pull_requests.len()),
        ("/v2/domain/documents", projection.documents.len()),
        ("/v2/domain/merge-authorizations", projection.merge_authorizations.len()),
        ("/v2/domain/stack-readiness", projection.stack_readiness.len()),
        ("/v2/domain/superseded-acceptances", projection.superseded_acceptances.len()),
        ("/v2/domain/interactions?scope=orchestrator", orchestrator),
    ] {
        let (items, _) = walk(runtime, base);
        assert_eq!(items.len(), expected, "{base}");
        if !base.contains('?') {
            let section = base.trim_start_matches("/v2/domain/").replace('-', "_");
            assert_eq!(summary.sections[&section].count as usize, expected, "{section} count");
        }
        ids.clear();
        for item in &items {
            let id = item.get("id").or_else(|| item["document"].get("id")).or_else(|| item.get("task_id")).unwrap();
            assert!(ids.insert(id.clone()), "{base} repeated {id}");
        }
    }
    let mut per_task = 0;
    for task in &projection.tasks {
        let (status, detail, bytes) = get(runtime, &format!("/v2/domain/tasks/{}", task.id));
        assert_eq!(status, 200);
        assert!(bytes <= MAX_PAGE_BYTES, "task {} detail is {bytes} bytes", task.id);
        let detail: TaskDetail = serde_json::from_value(detail).unwrap();
        assert_eq!(detail.request.id, task.request_id);
        let (items, _) = walk(runtime, &format!("/v2/domain/tasks/{}/interactions", task.id));
        assert_eq!(items.len() as u64, detail.interactions);
        per_task += items.len();
    }
    assert_eq!(per_task + orchestrator, projection.interactions.len(), "every interaction is reachable through its scope");
    let (status, lookup, _) = get(runtime, &format!("/v2/domain/inbox/{}", projection.inbox[0].id));
    assert_eq!(status, 200);
    assert_eq!(lookup["root_id"], projection.inbox[0].id);
    let (status, with, bytes) = get(runtime, "/v2/domain/summary?include=inbox,tasks,agents");
    assert_eq!(status, 200);
    assert!(bytes <= MAX_PAGE_BYTES, "summary with expansions is {bytes} bytes");
    let with: Summary = serde_json::from_value(with).unwrap();
    assert!(with.expansions.contains_key("inbox") && with.expansions.contains_key("agents"), "{:?}", with.pending);
    assert_eq!(with.expansions["inbox"].section_revision, summary.sections["inbox"].revision);
    eprintln!(
        "live-shaped reads: projection {} bytes, {} inbox roots, summary {bytes} bytes with expansions",
        runtime.snapshot(crate::service_overview::MAX_BYTES).unwrap().len(),
        projection.inbox.len()
    );
}

#[test]
fn oversized_text_is_cut_with_a_signal_and_readable_in_chunks() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let body: String = (0..6000).map(|i| format!("línea {i} con evidencia larga\n")).collect();
    assert!(body.len() > MAX_PAGE_BYTES);
    publish(runtime, "node-1", "t-1", "huge-info", body.clone());
    let (_, truncated) = walk(runtime, "/v2/domain/tasks/t-1/interactions");
    assert_eq!(truncated, vec!["huge-info".to_owned()]);
    let (_, inbox_truncated) = walk(runtime, "/v2/domain/inbox");
    assert!(inbox_truncated.contains(&"interaction:huge-info".to_owned()));
    let (status, detail, bytes) = get(runtime, "/v2/domain/interactions/huge-info");
    assert_eq!(status, 200);
    assert!(bytes <= MAX_PAGE_BYTES);
    let detail: Item<Value> = serde_json::from_value(detail).unwrap();
    assert!(detail.truncated);
    let cut = detail.item["body"].as_str().unwrap();
    assert!(body.starts_with(cut) && cut.len() < body.len());
    let mut assembled = String::new();
    loop {
        let (status, chunk, bytes) = get(runtime, &format!("/v2/domain/interactions/huge-info/body?offset={}", assembled.len()));
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
    let (status, _, _) = get(runtime, &format!("/v2/domain/interactions/huge-info/body?offset={inside_multibyte}"));
    assert_eq!(status, 422, "an offset inside a multibyte character is refused");
    let (status, _, _) = get(runtime, &format!("/v2/domain/interactions/huge-info/body?offset={}", body.len() + 1));
    assert_eq!(status, 422);
    let (status, _, _) = get(runtime, "/v2/domain/interactions/absent/body?offset=0");
    assert_eq!(status, 404);
}

#[test]
fn section_tokens_change_only_for_the_sections_a_command_touches() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let before = runtime.reads.snapshot(runtime).unwrap().section_tokens();
    publish(runtime, "node-2", "t-2", "one-more-info", "One more".into());
    let after = runtime.reads.snapshot(runtime).unwrap().section_tokens();
    let changed: Vec<&str> = SECTIONS.iter().copied().filter(|s| before[*s] != after[*s]).collect();
    assert_eq!(changed, ["inbox", "interactions"]);
    assert_eq!(after, runtime.reads.snapshot(runtime).unwrap().section_tokens(), "an unchanged revision keeps its tokens");
}

#[test]
fn paging_by_cursor_is_stable_and_malformed_cursors_are_refused() {
    let fixture = live_shaped();
    let runtime = &fixture.runtime;
    let projection = runtime.projection().unwrap();
    let (single, _) = walk(runtime, "/v2/domain/inbox?limit=1");
    let (bulk, _) = walk(runtime, "/v2/domain/inbox?limit=200");
    assert_eq!(single, bulk);
    assert_eq!(single.len(), projection.inbox.len());
    let (pending, _) = walk(runtime, "/v2/domain/inbox?filter=pending");
    assert_eq!(pending.len(), projection.inbox.iter().filter(|r| r.pending).count());
    let (by_agent, _) = walk(runtime, "/v2/domain/tasks?agent=node-3");
    assert_eq!(by_agent.len(), projection.tasks.iter().filter(|t| t.agent_id.as_ref().is_some_and(|a| a.0 == "node-3")).count());
    assert!(!by_agent.is_empty());
    for cursor in ["notbase64!".to_owned(), URL_SAFE_NO_PAD.encode(b"{\"i\":\"x\"}"), URL_SAFE_NO_PAD.encode(b"[]")] {
        let path = format!("/v2/domain/inbox?cursor={cursor}");
        match parse(&path) {
            Some(route) => assert_eq!(respond(runtime, &route).status().as_u16(), 422, "{cursor}"),
            None => assert_eq!(cursor, "notbase64!"),
        }
    }
    let (status, _, _) = get(runtime, &format!("/v2/domain/tasks?cursor={}", URL_SAFE_NO_PAD.encode(b"{\"o\":1,\"i\":\"x\"}")));
    assert_eq!(status, 422, "an ordered cursor does not fit an id-ordered section");
}

#[test]
fn the_read_grammar_is_exact() {
    for path in [
        "/v2/domain/summary",
        "/v2/domain/summary?include=inbox,tasks",
        "/v2/domain/inbox",
        "/v2/domain/inbox?filter=pending&limit=200",
        "/v2/domain/inbox/interaction:abc-1",
        "/v2/domain/inbox/document:t-1:d-1:r-1",
        "/v2/domain/tasks?request=r-1&agent=node-1",
        "/v2/domain/tasks/t-1",
        "/v2/domain/tasks/t-1/interactions?limit=5",
        "/v2/domain/interactions?scope=orchestrator",
        "/v2/domain/interactions/i-1",
        "/v2/domain/interactions/i-1/body?offset=0",
        "/v2/domain/requests",
        "/v2/domain/requests/r-1",
        "/v2/domain/requests/r-1/body?offset=10",
        "/v2/domain/agents",
        "/v2/domain/pull-requests",
        "/v2/domain/pull-requests/pr-1",
        "/v2/domain/documents",
        "/v2/domain/documents/d-1",
        "/v2/domain/merge-authorizations",
        "/v2/domain/stack-readiness",
        "/v2/domain/superseded-acceptances",
    ] {
        assert!(approved_read_path(path), "{path}");
    }
    for path in [
        "/v2/domain",
        "/v2/domain/",
        "/v2/domain/summary?",
        "/v2/domain/summary?include=",
        "/v2/domain/summary?include=schedules",
        "/v2/domain/summary?include=inbox,inbox",
        "/v2/domain/summary?include=inbox&include=tasks",
        "/v2/domain/inbox?limit=0",
        "/v2/domain/inbox?limit=201",
        "/v2/domain/inbox?filter=all",
        "/v2/domain/inbox?cursor=",
        "/v2/domain/inbox?cursor=a+b",
        "/v2/domain/inbox?other=1",
        "/v2/domain/inbox/../x",
        "/v2/domain/inbox/a/b",
        "/v2/domain/inbox/a?limit=1",
        "/v2/domain/tasks/t-1?limit=1",
        "/v2/domain/tasks/t-1/actions",
        "/v2/domain/tasks/t-1/interactions/x",
        "/v2/domain/interactions",
        "/v2/domain/interactions?scope=all",
        "/v2/domain/interactions?cursor=abc",
        "/v2/domain/interactions/i-1/body",
        "/v2/domain/interactions/i-1/body?offset=",
        "/v2/domain/interactions/i-1/body?offset=-1",
        "/v2/domain/interactions/i-1/body?offset=1&offset=2",
        "/v2/domain/requests/r%201",
        "/v2/domain/documents/d-1/content",
        "/v2/domain/schedules",
        "/v2/domain/changes?data_epoch=e&after=0&wait=0",
    ] {
        assert!(!approved_read_path(path), "{path}");
    }
    assert!(!approved_read_path(&format!("/v2/domain/inbox?cursor={}", "a".repeat(513))));
}
