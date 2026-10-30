//! A message from the user to an Agent, written down the connection its node mod holds open and
//! submitted to its Claude session by the mod. Nothing is recorded in the domain: no
//! Interaction, no outbox row. Only marking an interaction read, when the user asks for it, is a
//! domain command.
use crate::locks::Poisonable as _;
use crate::domain_runtime::DomainRuntime;
use alondra_core::delivery::State;
use alondra_core::domain::{Actor, AgentId, AttachmentRef, Command, CommandEnvelope, DomainError, InteractionId, PrId, TaskId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::time::Duration;

/// How long a message waits for its acknowledgement before the answer says it is still on its way.
const ACK_WAIT: Duration = Duration::from_millis(if cfg!(test) { 200 } else { 3000 });

#[derive(Deserialize, Clone, Copy, Default, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Kind {
    #[default]
    Message,
    Reply,
    MoreContext,
    PrChanges,
    PrQuestion,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageBody {
    text: String,
    #[serde(default)]
    attachments: Vec<AttachmentRef>,
    #[serde(default)]
    kind: Kind,
    #[serde(default)]
    mark_read: bool,
    interaction_id: Option<InteractionId>,
    pr_id: Option<PrId>,
    /// The Task the message is about; it must be the Agent's.
    task_id: Option<TaskId>,
    /// Sent by Claude on the user's behalf (`alondra send` run inside Claude Code).
    #[serde(default)]
    from_claude: bool,
}

fn refused(code: &str, message: impl Into<String>) -> DomainError {
    DomainError::new(code, message)
}

fn wording(body: &MessageBody, task_title: Option<(&TaskId, &str)>, user: &str, home: &Path) -> Result<String, DomainError> {
    let about = |what: Option<String>, name: &str| what.ok_or_else(|| refused("invalid_input", format!("{name} is required for this kind")));
    let lead = match body.kind {
        Kind::Message => String::new(),
        Kind::Reply => format!("[Reply to {}] ", about(body.interaction_id.as_ref().map(ToString::to_string), "interaction_id")?),
        Kind::MoreContext => format!("[More context for {}] ", about(body.interaction_id.as_ref().map(ToString::to_string), "interaction_id")?),
        Kind::PrChanges => format!("[Changes requested on {}] ", about(body.pr_id.as_ref().map(ToString::to_string), "pr_id")?),
        Kind::PrQuestion => format!("[Question about {}] ", about(body.pr_id.as_ref().map(ToString::to_string), "pr_id")?),
    };
    let task = task_title.map(|(id, title)| format!("[Task {id} “{title}”] ")).unwrap_or_default();
    let mut files = vec![];
    for attachment in &body.attachments {
        if !matches_stored_copy(home, &attachment.content_reference, &attachment.content_digest) {
            return Err(refused("invalid_input", format!("attachment {} does not match its stored copy", attachment.id)));
        }
        files.push(alondra_core::Attachment {
            name: attachment.name.clone(), source_path: attachment.name.clone(),
            stored_path: Some(home.join(&attachment.content_reference).display().to_string()), bytes: Some(attachment.bytes),
        });
    }
    Ok(format!("{task}{lead}{}{}", body.text.trim(), crate::uploads::delivery_note(user, &files)))
}

/// Whether the file `reference` names inside `home`, never through a link or out of it, has the
/// SHA-256 `digest`.
fn matches_stored_copy(home: &Path, reference: &str, digest: &str) -> bool {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let path = Path::new(reference);
    if !path.components().all(|component| matches!(component, std::path::Component::Normal(_))) {
        return false;
    }
    let Ok(mut file) = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(home.join(path)) else {
        return false;
    };
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => hash.update(&buffer[..count]),
            Err(_) => return false,
        }
    }
    hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>() == digest
}

/// Writes the message down the Agent's connection and answers how far it got: `handed` once the
/// session took it, `dispatching` while it is queued behind a turn in progress (the daemon keeps
/// writing it until the session takes it). An Agent with no connection is refused, so the user
/// can send again. `as_user` is for the user's own words, which also end a pause the user put on
/// the node; Alondra's notices go without it, so the model sees who sent them.
pub(crate) async fn deliver(domain: &DomainRuntime, home: &Path, user: &Actor, name: &str, agent: &AgentId, body: MessageBody,
    as_user: bool) -> Result<Value, DomainError> {
    if body.text.trim().is_empty() {
        return Err(refused("invalid_input", "message text is empty"));
    }
    let state = domain.state().map_err(|_| refused("storage_unavailable", "domain state unavailable"))?;
    if !state.agents.contains_key(agent) {
        return Err(refused("not_found", "agent does not exist"));
    }
    if crate::sdk_owner::runs_through_sdk(&state, agent) {
        let told = tell_through_sdk(domain, &state, home, user, name, agent, body, as_user);
        if as_user && told.is_ok() {
            domain.live.gate.resume_by_user_message(domain, &agent.0, alondra_core::now_millis());
        }
        return told;
    }
    if let Some((_, successor)) = crate::sdk_handover::successor(domain, agent) {
        return tell_through_sdk(domain, &state, home, user, name, &successor, body, as_user);
    }
    if domain.live.is_closed(&agent.0) {
        return Err(refused("session_unreachable", "the node is not connected to Alondra: its session is stopped or its Claude Code mod is not running. Nothing was sent"));
    }
    let task = match &body.task_id {
        Some(id) => {
            let task = state.tasks.get(id).ok_or_else(|| refused("not_found", "task does not exist"))?;
            if task.agent_id.as_ref() != Some(agent) {
                return Err(refused("not_assigned", format!("{id} is not this node's task")));
            }
            Some((id, task.title.as_str()))
        }
        None => None,
    };
    let text = wording(&body, task, name, home)?;
    if domain.live.mods.session(&agent.0).is_some_and(|reported| reported.halt().is_some())
        && !crate::domain_holds::waits_on_user(&state, &agent.0)
    {
        return Err(refused("session_stalled", format!("{name}'s turn stopped: the command it waited on or its session is gone, so a message would not reach it. Alondra restarts it; send again once it is back. Nothing was sent")));
    }
    let author = if body.from_claude { format!("Claude, for {name}") } else { name.to_owned() };
    let told = crate::sdk_handover::tell(domain, &agent.0, &author, &text, as_user)
        .map_err(|_| refused("session_unreachable", "the node is not connected to Alondra: its session is stopped or its Claude Code mod is not running. Nothing was sent"))?;
    if as_user {
        domain.live.gate.release_by_user_message(domain, &agent.0);
    }
    let delivered = match told {
        crate::sdk_handover::Told::Pushed(acked) => match tokio::time::timeout(ACK_WAIT, acked).await {
            Ok(Ok(())) => State::Handed,
            _ => State::Dispatching,
        },
        crate::sdk_handover::Told::Moving => State::Dispatching,
    };
    Ok(read_after(domain, user, &body, json!({"delivery_state": delivered})))
}

/// What someone tells the node of an SDK task becomes an input of its lifecycle, kept before it
/// leaves: it reaches the node's Claude inside the turn that runs, or as a turn of its own, and
/// waits for the host that resumes the session when the host is gone. Its delivery is the
/// lifecycle's to follow, so it answers `dispatching`, and `reaches` says where it stands.
fn tell_through_sdk(domain: &DomainRuntime, state: &alondra_core::domain::DomainState, home: &Path, user: &Actor, name: &str,
    agent: &AgentId, body: MessageBody, as_user: bool) -> Result<Value, DomainError> {
    let open = state.tasks.values().find(|task| task.agent_id.as_ref() == Some(agent) && !task.state.terminal());
    let task = match &body.task_id {
        Some(id) => state.tasks.get(id).filter(|task| task.agent_id.as_ref() == Some(agent))
            .ok_or_else(|| refused("not_assigned", format!("{id} is not this node's task")))?,
        None => open.ok_or_else(|| refused("not_assigned", "the node has no open task to tell"))?,
    };
    let text = wording(&body, Some((&task.id, task.title.as_str())), name, home)?;
    let author = if body.from_claude { format!("Claude, for {name}") } else { name.to_owned() };
    let command_id = format!("tell-{}", crate::auth::random_hex(16));
    tell_sdk_node(domain, &task.id, &author, &text, &command_id, as_user)?;
    let lifecycle = domain.read().state().lifecycle_of(&task.id)?;
    let reaches = reaches(&lifecycle);
    Ok(read_after(domain, user, &body, json!({"delivery_state": State::Dispatching, "reaches": reaches})))
}

/// Where an input just told to an SDK task stands, by its lifecycle: `turn` when Claude reads it
/// inside the turn that runs, `now` when it starts a turn on it, else what holds it: the user's
/// pause (`pause`), a message in doubt the user settles first (`doubt`), or a session that is not
/// connected until its host is back (`session`).
fn reaches(lifecycle: &alondra_domain::TaskLifecycle) -> &'static str {
    let cards = lifecycle.cards();
    if cards.iter().any(|(_, reason)| *reason == alondra_domain::WaitReason::Pause) {
        "pause"
    } else if cards.iter().any(|(_, reason)| *reason == alondra_domain::WaitReason::Doubt) {
        "doubt"
    } else if !matches!(lifecycle.resource(), Some(alondra_domain::ResourceEvidence::Available | alondra_domain::ResourceEvidence::Reserved)) {
        "session"
    } else if lifecycle.activity() == alondra_domain::ActivityView::Working {
        "turn"
    } else {
        "now"
    }
}

/// Tells the node of the SDK task `task` `text`, stamped with who sent it and when. A retry with
/// the same `command_id` tells it once. `from_user` is for the user's own words.
pub(crate) fn tell_sdk_node(domain: &DomainRuntime, task: &alondra_core::domain::TaskId, author: &str, text: &str, command_id: &str, from_user: bool)
    -> Result<(), DomainError> {
    let state = domain.state().map_err(|_| refused("storage_unavailable", "domain state unavailable"))?;
    let envelope = alondra_core::domain::CommandEnvelope {
        protocol_version: alondra_core::domain::PROTOCOL_VERSION,
        data_epoch: state.data_epoch.clone(),
        command_id: command_id.into(),
        expected_revision: state.revision,
        command: alondra_core::domain::Command::TellNode {
            task_id: task.clone(),
            text: crate::node_push::stamped(author, alondra_core::now_millis(), text),
            from_user,
        },
    };
    domain.execute_as_system(envelope).map(|_| ()).map_err(|error| match error {
        crate::domain_store::StoreError::Domain(error) => error,
        other => refused("storage_unavailable", format!("{other}")),
    })
}

/// Asks the Agent that owns an open pull request to resolve its conflicts with its base, sent
/// to its session like any other message about its Task.
pub(crate) async fn resolve_conflicts(domain: &DomainRuntime, home: &Path, user: &Actor, name: &str, pr_id: &PrId) -> Result<Value, DomainError> {
    let state = domain.state().map_err(|_| refused("storage_unavailable", "domain state unavailable"))?;
    let pr = state.pull_requests.get(pr_id).ok_or_else(|| refused("not_found", "PR does not exist"))?;
    if pr.state != alondra_core::domain::PrState::Open {
        return Err(refused("pr_not_open", format!("{} is no longer open", pr_url(pr))));
    }
    let agent = state.tasks.get(&pr.task_id).and_then(|task| task.agent_id.clone())
        .ok_or_else(|| refused("not_assigned", "the PR's task has no agent to ask"))?;
    let text = conflicts_text(name, pr, state.branch_update);
    let body = MessageBody { text, attachments: vec![], kind: Kind::Message, mark_read: false,
        interaction_id: None, pr_id: None, task_id: Some(pr.task_id.clone()), from_claude: false };
    deliver(domain, home, user, name, &agent, body, false).await
}

pub(crate) fn pr_url(pr: &alondra_core::domain::PullRequest) -> String {
    format!("https://{}/{}/pull/{}", pr.host, pr.repository, pr.number)
}

/// What the Agent of a conflicting PR is told, whether the user asked or Alondra noticed.
pub(crate) fn conflicts_text(name: &str, pr: &alondra_core::domain::PullRequest, update: alondra_core::domain::BranchUpdate) -> String {
    format!(
        "{name} asks you to resolve the conflicts of {url} with {base}: {how}. When you finish, present the PR again with \
         present_result and its new head. If you cannot resolve them, ask {name}.",
        url = pr_url(pr), base = pr.base, how = update.instruction(&pr.base)
    )
}

/// The same message relayed from a paired device, answered as a local response.
pub(crate) async fn relayed(domain: &DomainRuntime, home: &Path, sender: &str, agent: AgentId, body: Option<&Value>) -> Value {
    let Some(body) = body.cloned().and_then(|body| serde_json::from_value::<MessageBody>(body).ok()) else {
        return json!({"status": 422, "body": refused("invalid_input", "invalid message")});
    };
    let user = Actor::User { user_id: format!("device:{sender}") };
    match deliver(domain, home, &user, &user_name(), &agent, body, true).await {
        Ok(answer) => json!({"status": 200, "body": answer}),
        Err(error) => json!({"status": crate::domain_api::status_for(&error).as_u16(), "body": error}),
    }
}

pub(crate) fn user_name() -> String {
    alondra_core::config::Config::load().map(|config| config.name).unwrap_or_else(|_| "The user".into())
}

fn read_after(domain: &DomainRuntime, user: &Actor, body: &MessageBody, mut answer: Value) -> Value {
    if let (true, Some(interaction)) = (body.mark_read, body.interaction_id.clone()) {
        if let Err(error) = mark_read(domain, user, interaction) {
            answer["mark_read_error"] = json!(error);
        }
    }
    answer
}

/// What the user wrote to this session while it was in its own terminal, once.
pub(crate) fn take_held(domain: &DomainRuntime, agent: &AgentId) -> Vec<String> {
    domain.held.lock_or_recover("the held messages").remove(agent).unwrap_or_default()
}

fn mark_read(domain: &DomainRuntime, user: &Actor, interaction_id: InteractionId) -> Result<(), DomainError> {
    let state = domain.state().map_err(|_| refused("storage_unavailable", "domain state unavailable"))?;
    let envelope = CommandEnvelope {
        protocol_version: alondra_core::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
        command_id: format!("mark-read-{}", crate::auth::random_hex(8)).into(), expected_revision: state.revision,
        command: Command::MarkRead { interaction_id },
    };
    domain.execute(user, &envelope, None, || Ok(())).map(|_| ()).map_err(|error| match error {
        crate::domain_store::StoreError::Domain(error) => error,
        crate::domain_store::StoreError::Storage(_) => refused("storage_unavailable", "domain transaction could not be completed"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_push::testing::Node;
    use std::sync::Arc;

    struct Fixture(crate::test_support::Directory);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn user() -> Actor {
        Actor::User { user_id: "device:d".into() }
    }

    fn world() -> (Fixture, Arc<DomainRuntime>) {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir_all(&home).unwrap();
        let domain = Arc::new(DomainRuntime::open(&home).unwrap());
        let run = |actor: Actor, id: &str, command: Command| {
            let state = domain.state().unwrap();
            let envelope = CommandEnvelope { protocol_version: alondra_core::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
                command_id: id.into(), expected_revision: state.revision, command };
            domain.execute(&actor, &envelope, None, || Ok(())).unwrap();
        };
        run(Actor::System, "a", Command::RegisterAgent { agent_id: "agent-a".into(), name: "A".into(), at: 0 });
        run(Actor::System, "b", Command::RegisterAgent { agent_id: "agent-b".into(), name: "B".into(), at: 0 });
        run(user(), "r", Command::CreateRequest { request_id: "r".into(), body: "Build".into(), auto_merge: false, maintenance: false, message: false });
        run(Actor::Orchestrator { generation: 0 }, "t", Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Build it".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None,
            launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")), });
        (Fixture(home), domain)
    }

    fn body(value: Value) -> MessageBody {
        serde_json::from_value(value).unwrap()
    }

    #[tokio::test]
    async fn conflicts_go_to_the_agent_that_owns_an_open_pr_and_to_nobody_else() {
        let (fixture, domain) = world();
        let run = |actor: Actor, id: &str, command: Command| {
            let state = domain.state().unwrap();
            let envelope = CommandEnvelope { protocol_version: alondra_core::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(),
                command_id: id.into(), expected_revision: state.revision, command };
            domain.execute(&actor, &envelope, None, || Ok(())).unwrap();
        };
        run(Actor::Agent { agent_id: "agent-a".into() }, "pr", Command::RegisterPr {
            task_id: "t".into(), pr_id: "pr".into(), host: "github.com".into(), repository: "me/app".into(), number: 7,
            head: "0123456789abcdef0123456789abcdef01234567".into(), base: "main".into(),
            allowed_methods: vec![alondra_core::domain::MergeMethod::Squash] });
        let node = Node::connect(&domain, "agent-a");
        let answer = resolve_conflicts(&domain, &fixture.0, &user(), "Ana", &"pr".into()).await.unwrap();
        assert_eq!(answer, json!({"delivery_state": "handed"}));
        let sent = node.texts();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].starts_with("[Task t “Build it”] Ana asks you to resolve the conflicts of https://github.com/me/app/pull/7 with main: "), "{}", sent[0]);
        assert!(sent[0].contains("git rebase origin/main") && sent[0].contains("git push --force-with-lease"), "rebase by default: {}", sent[0]);
        assert_eq!(node.lines.lock().unwrap()[0]["as_user"], false, "a notice from Alondra is not the user's words");
        run(user(), "merge", Command::SetBranchUpdate { update: alondra_core::domain::BranchUpdate::Merge });
        resolve_conflicts(&domain, &fixture.0, &user(), "Ana", &"pr".into()).await.unwrap();
        let sent = node.texts();
        assert!(sent[1].contains("git merge origin/main") && !sent[1].contains("rebase"), "the method the user picked: {}", sent[1]);
        assert_eq!(resolve_conflicts(&domain, &fixture.0, &user(), "Ana", &"none".into()).await.unwrap_err().code, "not_found");
        run(Actor::System, "closed", Command::ObservePrState { pr_id: "pr".into(), head: "0123456789abcdef0123456789abcdef01234567".into(),
            base: "main".into(), state: alondra_core::domain::PrState::Closed, merged_head: None });
        assert_eq!(resolve_conflicts(&domain, &fixture.0, &user(), "Ana", &"pr".into()).await.unwrap_err().code, "pr_not_open");
        assert_eq!(node.texts().len(), 2, "a refusal sends nothing");
    }

    #[tokio::test]
    async fn the_text_is_written_down_the_agents_connection_and_nothing_is_recorded() {
        let (fixture, domain) = world();
        let node = Node::connect(&domain, "agent-a");
        let before = domain.state().unwrap();
        let answer = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Use the new API", "task_id": "t"})), true).await.unwrap();
        assert_eq!(answer, json!({"delivery_state": "handed"}));
        assert_eq!(node.texts(), ["[Task t “Build it”] Use the new API"]);
        assert_eq!(node.lines.lock().unwrap()[0]["as_user"], true);
        deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Push your work", "task_id": "t", "from_claude": true})), true).await.unwrap();
        assert_eq!(node.authors(), ["Ana", "Claude, for Ana"], "every message says who sent it");
        assert_eq!(domain.state().unwrap(), before, "a message records nothing in the domain");
    }

    #[tokio::test]
    async fn a_message_the_session_has_not_taken_yet_is_dispatching_and_is_resent_on_the_next_connection() {
        let (fixture, domain) = world();
        let node = Node::connect(&domain, "agent-a");
        node.silent.store(true, std::sync::atomic::Ordering::Relaxed);
        let answer = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(), body(json!({"text": "hi"})), true).await.unwrap();
        assert_eq!(answer, json!({"delivery_state": "dispatching"}));
        domain.live.pushes.detach("agent-a", 1);
        let again = Node::connect(&domain, "agent-a");
        tokio::task::yield_now().await;
        assert_eq!(again.texts(), ["hi"], "unacknowledged, so written again");
    }

    #[tokio::test]
    async fn a_session_whose_turn_stopped_is_refused_instead_of_reported_handed() {
        let (fixture, domain) = world();
        let node = Node::connect(&domain, "agent-a");
        let now = alondra_core::now_millis();
        let hello = serde_json::from_value(json!({"session_id": "agent-a", "version": "2.1.287"})).unwrap();
        domain.connect_node(hello, now);
        domain.report(serde_json::from_value(json!({"session_id": "agent-a", "event": "turn.start", "turn_id": "t"})).unwrap(), now);
        let quiet = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(), body(json!({"text": "quiet"})), true).await;
        assert!(quiet.is_ok(), "a quiet turn takes messages however long it has been quiet");
        domain.live.mods.set_session_alive("agent-a", Some(false));
        let refused = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(), body(json!({"text": "hi"})), true).await.unwrap_err();
        assert_eq!(refused.code, "session_stalled");
        assert_eq!(node.texts(), ["quiet"], "nothing more was written down its connection");
    }

    #[tokio::test]
    async fn a_reply_is_worded_for_its_interaction() {
        let (fixture, domain) = world();
        let node = Node::connect(&domain, "agent-a");
        deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Yes", "kind": "reply", "interaction_id": "q-1"})), true).await.unwrap();
        assert_eq!(node.texts()[0], "[Reply to q-1] Yes");
        let refused = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Yes", "kind": "pr_changes"})), true).await.unwrap_err();
        assert_eq!(refused.code, "invalid_input");
    }

    #[tokio::test]
    async fn refusals_send_nothing() {
        let (fixture, domain) = world();
        let node = Node::connect(&domain, "agent-a");
        let cases = [
            ("agent-z", json!({"text": "hi"}), "not_found"),
            ("agent-a", json!({"text": "  "}), "invalid_input"),
            ("agent-b", json!({"text": "hi", "task_id": "t"}), "not_assigned"),
            ("agent-a", json!({"text": "hi", "task_id": "missing"}), "not_found"),
            ("agent-b", json!({"text": "hi"}), "session_unreachable"),
        ];
        for (agent, request, code) in cases {
            let error = deliver(&domain, &fixture.0, &user(), "Ana", &agent.into(), body(request.clone()), true).await.unwrap_err();
            assert_eq!(error.code, code, "{agent} {request}");
        }
        assert!(node.texts().is_empty());
    }

    #[tokio::test]
    async fn a_reply_that_reads_the_information_marks_it_read_after_sending() {
        let (fixture, domain) = world();
        let state = domain.state().unwrap();
        domain.execute(&Actor::System, &CommandEnvelope {
            protocol_version: alondra_core::domain::PROTOCOL_VERSION, data_epoch: state.data_epoch.clone(), command_id: "i".into(),
            expected_revision: state.revision, command: Command::PublishInformation { attachments: vec![], interaction_id: "info-1".into(),
                scope: alondra_core::domain::Scope::Task { task_id: "t".into() }, recipient: alondra_core::domain::Recipient::User, body: "Done".into(), card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Know, "Read the update") },
        }, None, || Ok(())).unwrap();
        let _node = Node::connect(&domain, "agent-a");
        let answer = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Thanks", "kind": "reply", "interaction_id": "info-1", "mark_read": true})), true).await.unwrap();
        assert_eq!(answer, json!({"delivery_state": "handed"}));
        let read = &domain.state().unwrap().interactions[&InteractionId::from("info-1")].payload;
        assert!(matches!(read, alondra_core::domain::InteractionPayload::Information { read_by } if !read_by.is_empty()), "{read:?}");
    }

    #[tokio::test]
    async fn an_attachment_is_named_by_its_stored_path_once_its_digest_matches() {
        use sha2::Digest as _;
        let (fixture, domain) = world();
        std::fs::create_dir_all(fixture.0.join("files")).unwrap();
        std::fs::write(fixture.0.join("files/shot.png"), b"png").unwrap();
        let digest: String = sha2::Sha256::digest(b"png").iter().map(|b| format!("{b:02x}")).collect();
        let attachment = json!({"id": "att-1", "name": "shot.png", "content_digest": digest, "content_reference": "files/shot.png", "bytes": 3});
        let node = Node::connect(&domain, "agent-a");
        deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Look", "attachments": [attachment]})), true).await.unwrap();
        let sent = node.texts()[0].clone();
        assert!(sent.starts_with("Look\n\nAttached files:"), "{sent}");
        assert!(sent.contains(&fixture.0.join("files/shot.png").display().to_string()), "{sent}");
        let forged = json!({"id": "att-1", "name": "shot.png", "content_digest": "0".repeat(64), "content_reference": "files/shot.png", "bytes": 3});
        let error = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Look", "attachments": [forged]})), true).await.unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(node.texts().len(), 1);
    }

    #[tokio::test]
    async fn a_relayed_message_answers_with_the_local_status() {
        let (fixture, domain) = world();
        let invalid = relayed(&domain, &fixture.0, "phone", "agent-a".into(), Some(&json!({"txt": "hi"}))).await;
        assert_eq!((invalid["status"].as_u64(), invalid["body"]["code"].as_str()), (Some(422), Some("invalid_input")));
        let unknown = relayed(&domain, &fixture.0, "phone", "agent-z".into(), Some(&json!({"text": "hi"}))).await;
        assert_eq!((unknown["status"].as_u64(), unknown["body"]["code"].as_str()), (Some(404), Some("not_found")));
    }

    #[test]
    fn the_client_contract_body_is_accepted() {
        let contract: Value = serde_json::from_str(include_str!("../../../fixtures/client-v2/requests.json")).unwrap();
        let parsed = body(contract["message"]["body"].clone());
        assert_eq!((parsed.kind, parsed.pr_id), (Kind::PrChanges, Some("pr-1".into())));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(contract["message"]["response"], json!({"delivery_state": "handed"}));
    }
}
