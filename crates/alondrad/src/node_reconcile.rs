//! Which sessions really exist, as `claude agents` lists them: every node Alondra launched takes
//! what the listing says of it, absent included, through the node machine. It runs when the daemon
//! starts, before the gate admits anything, and every minute after, so a session that ended without
//! a word (killed, removed, the daemon down meanwhile) never keeps a slot.
use crate::domain_runtime::DomainRuntime;
use crate::node_close::Sessions;
use alondra_core::domain::{AgentId, Listed};
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

pub(crate) const EVERY: Duration = Duration::from_secs(60);

/// What the listing says of each session, by session id; `None` when it is not the JSON list.
/// Claude Code keeps a session's row after its process stops: it stops the process of a session
/// that sat settled for a while (an hour, eight when linked to claude.ai, a minute when the Mac is
/// short of memory), and lists a `pid` only while the process lives. A `done` row is a finished
/// turn either way, as a message resumes it. Any other row without a process, when the listing
/// names processes, has no session behind it, whatever its state says; a listing that names none
/// is read by its states alone.
pub(crate) fn parse(listing: &str) -> Option<BTreeMap<String, Listed>> {
    let list = serde_json::Deserializer::from_str(listing.get(listing.find('[')?..)?).into_iter::<Vec<Value>>().next()?.ok()?;
    let names_processes = list.iter().any(|session| session.get("pid").is_some_and(Value::is_u64));
    Some(list.iter().filter_map(|session| {
        let id = session.get("sessionId").and_then(Value::as_str)?;
        let running = session.get("pid").is_some_and(Value::is_u64);
        let listed = match session.get("state").and_then(Value::as_str)? {
            "done" => Listed::Done,
            _ if names_processes && !running => Listed::Absent,
            "working" => Listed::Working,
            "idle" => Listed::Idle,
            "blocked" => Listed::Blocked,
            "failed" if running => Listed::Idle,
            _ => Listed::Absent,
        };
        Some((id.to_owned(), listed))
    }).collect())
}

fn needs_claude_listing(state: &alondra_core::domain::DomainState, agent: &alondra_core::domain::Agent) -> bool {
    if !agent.phase.live() || Some(&agent.id) == state.orchestrator_session.as_ref() || crate::sdk_owner::runs_through_sdk(state, &agent.id) { return false; }
    let tasks: Vec<_> = state.tasks.values().filter(|task| task.agent_id.as_ref() == Some(&agent.id)).collect();
    if !tasks.is_empty() { return tasks.iter().any(|task| task.provider == alondra_core::accounts::Provider::Claude); }
    state.accounts.iter().any(|account| account.provider == alondra_core::accounts::Provider::Claude)
        || (state.accounts.is_empty() && state.account_config_version.is_empty())
}

/// Lists the real sessions once and hands the node machine what it says of every live node
/// Alondra launched, and of every other live node whose session is gone and that holds no open
/// task, so it closes without ever being brought back. A node of the Agent SDK is never in the
/// listing and is left to its host. False when the listing could not be read: nothing is moved
/// then.
pub(crate) async fn once(domain: &DomainRuntime, sessions: &dyn Sessions) -> bool {
    let Ok(state) = domain.state() else { return false };
    if !state.agents.values().any(|agent| needs_claude_listing(&state, agent)) { return true; }
    drop(state);
    let asked = alondra_core::now_millis();
    let listing = match sessions.claude(vec!["agents".into(), "--json".into(), "--all".into()]).await {
        Ok(listing) => listing,
        Err(error) => {
            log!("could not list Claude's sessions to check the nodes: {error}");
            return false;
        }
    };
    let Some(sessions) = parse(&listing) else {
        log!("claude agents did not answer with its list of sessions");
        return false;
    };
    let Ok(state) = domain.state() else { return false };
    let open_task = |agent: &AgentId| state.tasks.values().any(|task| task.agent_id.as_ref() == Some(agent) && !task.state.terminal());
    let listed: BTreeMap<AgentId, Listed> = state.agents.values()
        .filter(|agent| needs_claude_listing(&state, agent))
        .map(|agent| (agent, sessions.get(&agent.id.0).copied().unwrap_or(Listed::Absent)))
        .filter(|(agent, listed)| domain.launched_by_alondra(&agent.id).unwrap_or(false) || (matches!(listed, Listed::Absent | Listed::Done) && !open_task(&agent.id)))
        .map(|(agent, listed)| (agent.id.clone(), listed))
        .collect();
    match crate::domain_agent_lifecycle::reconcile_at(domain, listed, asked) {
        Ok(moved) => {
            if moved {
                domain.live.gate.poke();
            }
            true
        }
        Err(error) => {
            log!("could not bring the nodes in line with Claude's sessions: {error}");
            false
        }
    }
}

/// Every minute after the first look the daemon takes before the gate opens.
pub(crate) async fn run(domain: std::sync::Arc<DomainRuntime>, sessions: std::sync::Arc<dyn Sessions>) {
    loop {
        tokio::time::sleep(EVERY).await;
        once(&domain, sessions.as_ref()).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_runtime::DomainRuntime;
    use crate::node_close::Answer;
    use alondra_core::domain::{Actor, Command, CommandEnvelope, LaunchPlan, NodeEvent, NodePhase, ORCHESTRATOR_READER, PROTOCOL_VERSION};
    use std::sync::Mutex;

    struct NoClaude;
    impl Sessions for NoClaude {
        fn claude<'a>(&'a self, _: Vec<String>) -> Answer<'a, Result<String,String>> { panic!("this daemon has no legacy Claude node to list") }
        fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> { Box::pin(async { None }) }
    }

    #[tokio::test]
    async fn a_daemon_without_legacy_nodes_never_invokes_claude_discovery() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        assert!(once(&domain, &NoClaude).await);
    }

    #[test]
    fn a_committed_codex_task_is_never_a_legacy_claude_session() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        launch(&domain, "native");
        let mut state = (*domain.state().unwrap()).clone();
        state.tasks.get_mut(&"t-native".into()).unwrap().provider = alondra_core::accounts::Provider::Codex;
        assert!(!needs_claude_listing(&state, &state.agents[&"native".into()]));
    }

    struct Claude(Mutex<String>);

    impl Sessions for Claude {
        fn claude<'a>(&'a self, args: Vec<String>) -> Answer<'a, Result<String, String>> {
            Box::pin(async move {
                assert_eq!(args, ["agents", "--json", "--all"]);
                Ok(self.0.lock().unwrap().clone())
            })
        }
        fn leftovers<'a>(&'a self, _: &'a str) -> Answer<'a, Option<String>> {
            Box::pin(async { None })
        }
    }

    fn run(domain: &DomainRuntime, actor: Actor, id: &str, command: Command) {
        let state = domain.state().unwrap();
        domain.execute(&actor, &CommandEnvelope {
            protocol_version: PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: id.into(),
            expected_revision: state.revision, command,
        }, None, || Ok(())).unwrap();
    }

    /// A node Alondra launched on its own task, in a turn.
    fn launch(domain: &DomainRuntime, id: &str) {
        let now = alondra_core::now_millis();
        run(domain, Actor::Orchestrator { generation: 0 }, &format!("create-{id}"), Command::CreateTask { plan_items: None,
            task_id: format!("t-{id}").into(), request_id: "request".into(), title: format!("Task {id}"), agent_id: None, review_prs: false,
            dependencies: Default::default(), stack: None, launch: Some(LaunchPlan::new("node", "/tmp")),
        });
        run(domain, Actor::System, &format!("register-{id}"), Command::RegisterAgent { agent_id: id.into(), name: id.into(), at: now });
        run(domain, Actor::System, &format!("orchestrator-tool-assign-{id}"), Command::AssignTask { task_id: format!("t-{id}").into(), agent_id: id.into(), model: None });
        run(domain, Actor::Agent { agent_id: id.into() }, &format!("start-{id}"), Command::StartTask { task_id: format!("t-{id}").into() });
        run(domain, Actor::System, &format!("turn-{id}"), Command::MoveNode { agent_id: id.into(), event: NodeEvent::TurnStarted, at: now });
    }

    fn open(home: &std::path::Path) -> DomainRuntime {
        let domain = DomainRuntime::open(home).unwrap();
        if domain.state().unwrap().requests.is_empty() {
            run(&domain, Actor::User { user_id: "u".into() }, "request", Command::CreateRequest {
                request_id: "request".into(), body: "Work".into(), auto_merge: false, maintenance: false, message: false,
            });
            run(&domain, Actor::Orchestrator { generation: 0 }, "title", Command::TitleRequest { request_id: "request".into(), title: "Work".into() });
        }
        domain
    }

    fn phase(domain: &DomainRuntime, id: &str) -> NodePhase {
        domain.state().unwrap().agents[&id.into()].phase.clone()
    }

    /// 9 October: 10 nodes read as connected, 3 sessions existed, and the ghosts held the slots
    /// until the daemon was restarted by hand.
    #[tokio::test]
    async fn a_restart_frees_the_slots_of_sessions_that_ended_while_nobody_watched() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        for id in ["alive", "vanished", "finished"] {
            launch(&domain, id);
        }
        run(&domain, Actor::User { user_id: "u".into() }, "cancel-finished", Command::CancelTask { task_id: "t-finished".into() });
        assert_eq!(domain.state().unwrap().slots().len(), 2);
        drop(domain);

        let domain = open(&home);
        let claude = Claude(Mutex::new(r#"[{"id":"alive","sessionId":"alive","state":"working"},{"id":"gone","sessionId":"finished","state":"done"}]"#.into()));
        assert!(once(&domain, &claude).await);
        let state = domain.state().unwrap();
        assert_eq!(state.slots().iter().map(|agent| agent.id.0.as_str()).collect::<Vec<_>>(), ["alive"], "only the session that exists works");
        assert_eq!(phase(&domain, "vanished"), NodePhase::Lost, "its task is open: it waits for the gate to be brought back, holding no slot");
        assert_eq!(phase(&domain, "finished"), NodePhase::Closed, "its task is over and its session gone: it closes");

        let revision = state.revision;
        assert!(once(&domain, &claude).await);
        assert_eq!(domain.state().unwrap().revision, revision, "the same listing again moves nothing");
    }

    /// 12 October: a scheduled run whose task was cancelled before its session started never
    /// linked the node to its launch, so the listing skipped it and it counted as working for days.
    #[tokio::test]
    async fn a_node_alondra_cannot_tell_it_launched_closes_once_its_session_is_gone() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        let now = alondra_core::now_millis();
        for id in ["unlinked-gone", "unlinked-alive"] {
            run(&domain, Actor::System, &format!("register-{id}"), Command::RegisterAgent { agent_id: id.into(), name: format!("scheduled/{id}"), at: now });
            run(&domain, Actor::System, &format!("turn-{id}"), Command::MoveNode { agent_id: id.into(), event: NodeEvent::TurnStarted, at: now });
        }
        assert!(!domain.launched_by_alondra(&"unlinked-gone".into()).unwrap());
        let claude = Claude(Mutex::new(r#"[{"id":"gone","sessionId":"unlinked-gone","state":"done"},{"id":"alive","sessionId":"unlinked-alive","state":"working"}]"#.into()));
        assert!(once(&domain, &claude).await);
        assert_eq!(phase(&domain, "unlinked-gone"), NodePhase::Closed);
        assert_eq!(phase(&domain, "unlinked-alive"), NodePhase::Working, "a session that exists is not Alondra's to judge");
    }

    #[tokio::test]
    async fn a_listing_that_cannot_be_read_moves_nothing() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        launch(&domain, "alive");
        assert!(!once(&domain, &Claude(Mutex::new("error: not logged in".into()))).await);
        assert_eq!(phase(&domain, "alive"), NodePhase::Working);
    }

    /// Domains written before the node machine kept `connected`: a connected node of a finished
    /// task, one of an open task whose session is gone, and one with nothing.
    #[tokio::test]
    async fn an_older_domain_takes_phases_from_its_connected_flags_and_the_first_listing_settles_them() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        for id in ["alive", "open-gone", "done-ghost"] {
            launch(&domain, id);
        }
        run(&domain, Actor::User { user_id: "u".into() }, "cancel-ghost", Command::CancelTask { task_id: "t-done-ghost".into() });
        drop(domain);
        let db = rusqlite::Connection::open(home.join("domain.db")).unwrap();
        db.execute_batch("PRAGMA foreign_keys=OFF;
            ALTER TABLE agents ADD COLUMN connected INTEGER NOT NULL DEFAULT 1 CHECK(connected IN(0,1));
            UPDATE agents SET connected=0 WHERE id='open-gone';
            ALTER TABLE agents DROP COLUMN phase; ALTER TABLE agents DROP COLUMN since;").unwrap();
        drop(db);

        let domain = open(&home);
        assert_eq!(phase(&domain, "alive"), NodePhase::Working);
        assert_eq!(phase(&domain, "open-gone"), NodePhase::Lost);
        let claude = Claude(Mutex::new(r#"[{"id":"alive","sessionId":"alive","state":"working"}]"#.into()));
        assert!(once(&domain, &claude).await);
        assert_eq!(phase(&domain, "done-ghost"), NodePhase::Closed);
        assert_eq!(domain.state().unwrap().slots().len(), 1);
    }

    /// 346 notices to the orchestrator stayed unread: nothing ever read them once handed over.
    #[test]
    fn a_notice_handed_to_the_orchestrator_is_read_and_one_still_queued_is_not() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        domain.tell_orchestrator("handed", None, "Task one ended".into()).unwrap();
        domain.tell_orchestrator("queued", None, "Task two ended".into()).unwrap();
        {
            let mut store = domain.store.lock().unwrap();
            let pending = store.pending_messages().unwrap();
            let handed = pending.iter().find(|message| message.interaction_id.as_deref() == Some("handed")).unwrap();
            let claim = store.claim_orchestrator_message(handed.id).unwrap().unwrap();
            assert!(store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap());
        }
        crate::domain_orchestrator::read_handed_notices(&domain);
        crate::domain_orchestrator::read_handed_notices(&domain);
        let state = domain.state().unwrap();
        let read = |id: &str| match &state.interactions[&id.into()].payload {
            alondra_core::domain::InteractionPayload::Information { read_by } => read_by.iter().cloned().collect::<Vec<_>>(),
            _ => panic!("information"),
        };
        assert_eq!(read("handed"), [ORCHESTRATOR_READER]);
        assert!(read("queued").is_empty(), "a notice not handed yet still waits");
    }

    /// 0.2.64's first start on the real data swept 443 such notices back to back. The live views
    /// take the store again and again with work in between, and the sweep took it in every gap, so
    /// the HTTP server's workers waited on it for seconds and the daemon stopped answering.
    #[test]
    fn the_startup_sweep_leaves_the_store_to_a_caller_that_needs_it_again_and_again() {
        const NOTICES: u64 = 150;
        const TAKES: u64 = 20;
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        for n in 0..NOTICES {
            domain.tell_orchestrator(&format!("notice-{n}"), None, format!("Task {n} ended")).unwrap();
        }
        {
            let mut store = domain.store.lock().unwrap();
            for message in store.pending_messages().unwrap() {
                let claim = store.claim_orchestrator_message(message.id).unwrap().unwrap();
                assert!(store.finish_message(claim.attempt_id, claim.version, alondra_core::delivery::State::Handed, None).unwrap());
            }
        }
        let start = domain.state().unwrap().revision;
        let read = || domain.state().unwrap().revision - start;

        let read_meanwhile = std::thread::scope(|scope| {
            scope.spawn(|| crate::domain_orchestrator::read_handed_notices_in_background(&domain));
            while read() == 0 {
                std::thread::yield_now();
            }
            let before = read();
            for _ in 0..TAKES {
                drop(domain.store.lock().unwrap());
                let working = std::time::Instant::now();
                while working.elapsed() < std::time::Duration::from_micros(300) {
                    std::hint::spin_loop();
                }
            }
            read() - before
        });

        assert_eq!(read(), NOTICES, "every handed notice is read");
        assert!(read_meanwhile <= TAKES / 2, "the sweep read {read_meanwhile} notices while another caller took the store {TAKES} times");
    }

    /// The daemon gets the Agent SDK, whose moves stop once planned: what bringing a node back
    /// asks for is seen in its plan. The folder of its hosts, kept while the test runs.
    fn moving_to_the_sdk(domain: &std::sync::Arc<DomainRuntime>, home: &std::path::Path) -> crate::sdk_owner::tests::HostRoot {
        let state = crate::sdk_owner::tests::HostRoot::new();
        domain.enable_sdk(crate::sdk_owner::tests::fixture_backend(home, &state).0);
        *domain.handovers.crash_after.lock().unwrap() = Some(crate::domain_store::HandoverStep::Planned);
        state
    }

    fn moves(domain: &DomainRuntime, node: &str) -> Vec<crate::domain_store::HandoverPlan> {
        domain.store.lock().unwrap().handovers().unwrap().into_iter().filter(|handover| handover.node.0 == node).map(|handover| handover.plan).collect()
    }

    fn tasks_view(domain: &DomainRuntime) -> String {
        use alondra_core::views::{build, Client, Lang, LiveSnapshot, RenderContext, Transport};
        let state = domain.state().unwrap();
        let view = build("tasks", RenderContext {
            domain: &state, index: &alondra_core::domain::ReadIndex::build(&state), live: &LiveSnapshot::default(), client: Client::Mac, lang: Lang::Es,
            transport: Transport::Loopback, cursor: "", rendered_at: "",
        }).unwrap();
        serde_json::to_string(&view).unwrap()
    }

    fn paused_for_usage(domain: &DomainRuntime, node: &str) {
        let now = alondra_core::now_millis();
        let usage = alondra_core::domain::Pause {
            reason: alondra_core::domain::PauseReason::Usage, since: now, cut: None, turn_since: None,
            cleanup: alondra_core::domain::PauseCleanup::Done, released: false,
        };
        run(domain, Actor::System, &format!("pause-{node}"), Command::MoveNode { agent_id: node.into(), event: NodeEvent::Paused { pause: usage }, at: now });
        run(domain, Actor::System, &format!("turn-end-{node}"), Command::MoveNode { agent_id: node.into(), event: NodeEvent::TurnEnded, at: now });
    }

    /// A node of `launch` connected from `worktree`, with uncommitted work in it, and the hello its
    /// mod says again when its session is resumed.
    fn in_worktree(domain: &DomainRuntime, home: &std::path::Path, node: &str) -> (std::path::PathBuf, serde_json::Value, u64) {
        let worktree = home.join(format!("worktrees/{node}"));
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join("staged.rs"), "uncommitted work").unwrap();
        let hello = serde_json::json!({"session_id": node, "version": "2.1.291", "cwd": worktree.to_str().unwrap()});
        let connection = domain.connect_node(serde_json::from_value(hello.clone()).unwrap(), alondra_core::now_millis());
        drop(domain.live.pushes.attach(node, connection));
        (worktree, hello, connection)
    }

    /// Claude Code stopped the settled session: its mod's connection drops.
    fn retired(domain: &DomainRuntime, node: &str, connection: u64) {
        domain.live.pushes.detach(node, connection);
        domain.live.mods.disconnect(node, connection);
    }

    const QUICK: crate::node_revival::Timing = crate::node_revival::Timing { grace: Duration::from_millis(30) };

    /// 10–12 October, “El orquestador deja de verse”: the node ended its turn paused for the usage
    /// limit, with its work uncommitted in its worktree. Eight hours later Claude Code stopped the
    /// settled session (`bg retire …: settled, idle 8h`) and listed it `done` with no process. When
    /// usage came back the session was resumed, and nothing told it to carry on: it sat idle until
    /// Claude Code stopped it again eight hours later.
    #[tokio::test]
    async fn a_paused_node_whose_session_claude_code_stopped_comes_back_through_the_sdk_and_carries_on_in_its_worktree() {
        const NODE: &str = "c0ffee00-81db-4cdb-8e95-6a2aaab612d3";
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = std::sync::Arc::new(open(&home));
        let _hosts = moving_to_the_sdk(&domain, &home);
        launch(&domain, NODE);
        let (worktree, hello, connection) = in_worktree(&domain, &home, NODE);
        paused_for_usage(&domain, NODE);
        retired(&domain, NODE, connection);
        let claude = Claude(Mutex::new(format!(
            r#"[{{"id":"c0ffee00","sessionId":"{NODE}","state":"done"}},{{"id":"another","sessionId":"another","state":"working","pid":56103}}]"#)));
        assert!(once(&domain, &claude).await);
        assert!(!domain.live.mods.connected(NODE), "nothing says it is connected");
        let tasks = tasks_view(&domain);
        assert!(!tasks.contains("Sin conexión") && tasks.contains("Por el límite de uso"), "it says what it waits for: {tasks}");

        crate::node_revival::revive(&domain, NODE, &QUICK).await;
        assert!(moves(&domain, NODE).is_empty(), "a paused node comes back through the gate, not revival");
        crate::admission::pass(&domain, &home).await.unwrap();
        assert!(!matches!(phase(&domain, NODE), NodePhase::Paused(_)), "the gate resumed it");
        crate::node_revival::revive(&domain, NODE, &QUICK).await;
        let moves = moves(&domain, NODE);
        assert_eq!(moves.len(), 1, "the same session moves to the SDK, conversation and all: {moves:?}");
        assert_eq!(moves[0].session, NODE);
        assert_eq!(Some(moves[0].cwd.as_str()), worktree.to_str(), "in the same worktree");
        assert!(moves[0].input.ends_with("Your usage limit has reset. Carry on with what you were doing."), "{}", moves[0].input);
        assert!(!matches!(phase(&domain, NODE), NodePhase::Paused(_)), "its pause is over");
        assert_eq!(std::fs::read_to_string(worktree.join("staged.rs")).unwrap(), "uncommitted work");
    }

    /// The same without a pause: a node whose turn ended with its task still open and nothing for
    /// the user waits on the card with its last words; it loses its session to Claude Code, and
    /// nothing is typed into it: the user's answer is what it waits for.
    #[tokio::test]
    async fn a_resumed_session_waiting_on_the_card_with_its_words_is_left_to_the_user() {
        const NODE: &str = "c0ffee01-81db-4cdb-8e95-6a2aaab612d3";
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = std::sync::Arc::new(open(&home));
        let _hosts = moving_to_the_sdk(&domain, &home);
        launch(&domain, NODE);
        let (_, _, connection) = in_worktree(&domain, &home, NODE);
        run(&domain, Actor::System, "turn-end", Command::MoveNode { agent_id: NODE.into(), event: NodeEvent::TurnEnded, at: alondra_core::now_millis() });
        retired(&domain, NODE, connection);
        crate::node_revival::revive(&domain, NODE, &QUICK).await;
        assert!(moves(&domain, NODE).is_empty(), "nothing is typed into it: it moves to the SDK with the user's answer");
        let state = domain.state().unwrap();
        assert!(state.open_user_card(&format!("t-{NODE}").into()).is_some_and(|card| card.id.0.starts_with(alondra_core::domain::UNHEARD_PREFIX)));
    }

    /// A row Claude Code keeps after the process stopped, other than `done`, is no session: a
    /// node listed `blocked` without one is brought back, not left resting on a ghost.
    #[tokio::test]
    async fn a_node_whose_row_has_no_process_is_brought_back() {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = open(&home);
        launch(&domain, "ghost");
        run(&domain, Actor::System, "turn-end", Command::MoveNode { agent_id: "ghost".into(), event: NodeEvent::TurnEnded, at: alondra_core::now_millis() });
        assert_eq!(phase(&domain, "ghost"), NodePhase::WaitingUser { in_turn: false });
        let claude = Claude(Mutex::new(r#"[{"id":"ghost","sessionId":"ghost","state":"blocked"},{"id":"alive","sessionId":"alive","state":"working","pid":44234}]"#.into()));
        assert!(once(&domain, &claude).await);
        assert_eq!(phase(&domain, "ghost"), NodePhase::Lost);
    }

    #[test]
    fn the_listing_says_each_session_by_its_id_and_a_stopped_one_is_absent() {
        let listing = r#"[{"id":"9462eaef","sessionId":"9462eaef-2d59","state":"done"},{"id":"200ea49d","sessionId":"200ea49d-a71d","state":"working"},
            {"id":"316f9b7e","sessionId":"316f9b7e-3d4a","state":"idle"},{"id":"30fdb363","sessionId":"30fdb363-1965","state":"stopped"}]
            warning: something on stderr"#;
        let parsed = parse(listing).unwrap();
        assert_eq!(parsed["200ea49d-a71d"], Listed::Working);
        assert_eq!(parsed["316f9b7e-3d4a"], Listed::Idle);
        assert_eq!(parsed["9462eaef-2d59"], Listed::Done);
        assert_eq!(parsed["30fdb363-1965"], Listed::Absent);
        assert_eq!(parse("claude: command not found"), None);
    }

    /// What `claude agents --json --all` listed on 13 October: a row keeps its last state after
    /// Claude Code stops its process, and only a running one names its `pid`.
    #[test]
    fn a_listing_that_names_processes_reads_a_row_without_one_as_gone() {
        let listing = r#"[{"id":"f537833a","sessionId":"f537833a","state":"done"},
            {"id":"b1464d88","sessionId":"b1464d88","state":"done","pid":43635,"status":"idle"},
            {"id":"57988e41","sessionId":"57988e41","state":"failed","pid":41821,"status":"idle"},
            {"id":"db23219f","sessionId":"db23219f","state":"blocked"},
            {"id":"8906cf70","sessionId":"8906cf70","state":"blocked","pid":56103,"status":"busy"},
            {"id":"d9228a21","sessionId":"d9228a21","state":"working","pid":44234,"status":"busy"},
            {"id":"5d6f5f9e","sessionId":"5d6f5f9e","state":"stopped"}]"#;
        let parsed = parse(listing).unwrap();
        assert_eq!(parsed["f537833a"], Listed::Done, "done, its process stopped: a message resumes it");
        assert_eq!(parsed["b1464d88"], Listed::Done, "done, its process still there");
        assert_eq!(parsed["57988e41"], Listed::Idle);
        assert_eq!(parsed["db23219f"], Listed::Absent, "blocked when its process stopped");
        assert_eq!(parsed["8906cf70"], Listed::Blocked);
        assert_eq!(parsed["d9228a21"], Listed::Working);
        assert_eq!(parsed["5d6f5f9e"], Listed::Absent);
    }
}
