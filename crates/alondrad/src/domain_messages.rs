//! A message from the user to an Agent, typed straight into its Claude session the way the
//! legacy `send` did: `claude attach` opens the background session, which also wakes one that
//! was stopped. Nothing is recorded in the domain: no Interaction, no outbox row. Only marking
//! an interaction read, when the user asks for it, is a domain command.
use crate::domain_runtime::DomainRuntime;
use crate::domain_transport::Terminal;
use crate::wake::WriteFailure;
use alondra_core::delivery::State;
use alondra_core::domain_v2::{Actor, AgentId, AttachmentRef, Command, CommandEnvelope, DomainError, InteractionId, PrId, TaskId};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

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
}

/// A Claude session as `claude agents --json --all` lists it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Session {
    pub short: String,
    pub interactive: bool,
}

pub(crate) trait Sessions: Sync + Send {
    fn find(&self, session_id: &str) -> impl Future<Output = Option<Session>> + Send;
}

pub(crate) struct ClaudeAgents;

impl Sessions for ClaudeAgents {
    async fn find(&self, session_id: &str) -> Option<Session> {
        let out = tokio::time::timeout(Duration::from_secs(20), tokio::process::Command::new("claude")
            .args(["agents", "--json", "--all"]).kill_on_drop(true).stdin(std::process::Stdio::null()).output())
            .await.ok()?.ok()?;
        session_in(&String::from_utf8_lossy(&out.stdout), session_id)
    }
}

pub(crate) fn session_in(json: &str, session_id: &str) -> Option<Session> {
    let Ok(Value::Array(list)) = serde_json::from_str::<Value>(json) else { return None };
    list.iter().find(|s| s.get("sessionId").and_then(Value::as_str) == Some(session_id)).and_then(|s| Some(Session {
        short: s.get("id")?.as_str()?.to_string(),
        interactive: s.get("kind").and_then(Value::as_str) == Some("interactive"),
    }))
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
        crate::domain_migration::verify_content(home, &attachment.content_reference, &attachment.content_digest)
            .map_err(|_| refused("invalid_input", format!("attachment {} does not match its stored copy", attachment.id)))?;
        files.push(alondra_core::Attachment {
            name: attachment.name.clone(), source_path: attachment.name.clone(),
            stored_path: Some(home.join(&attachment.content_reference).display().to_string()), bytes: Some(attachment.bytes),
        });
    }
    Ok(format!("{task}{lead}{}{}", body.text.trim(), crate::uploads::delivery_note(user, &files)))
}

/// Types the message into the Agent's session and answers how far it got: `handed` once it
/// was submitted, `uncertain` when the write may have landed. A session that could not be
/// reached is refused, so the user can send again.
pub(crate) async fn deliver(domain: &DomainRuntime, home: &Path, user: &Actor, name: &str, agent: &AgentId, body: MessageBody,
    sessions: &impl Sessions, terminal: &Arc<impl Terminal>) -> Result<Value, DomainError> {
    if body.text.trim().is_empty() {
        return Err(refused("invalid_input", "message text is empty"));
    }
    let state = domain.state().map_err(|_| refused("storage_unavailable", "domain state unavailable"))?;
    if !state.agents.contains_key(agent) {
        return Err(refused("not_found", "agent does not exist"));
    }
    let task = match &body.task_id {
        Some(id) => {
            let task = state.tasks.get(id).ok_or_else(|| refused("not_found", "task does not exist"))?;
            if task.agent_id.as_ref() != Some(agent) {
                return Err(refused("not_assigned", format!("{id} is not this agent's task")));
            }
            Some((id, task.title.as_str()))
        }
        None => None,
    };
    let text = wording(&body, task, name, home)?;
    let short = match sessions.find(&agent.0).await {
        Some(Session { interactive: true, .. }) => {
            return Err(refused("interactive_session", "this agent runs in an interactive terminal; write to it there"));
        }
        Some(session) => session.short,
        None => crate::domain_transport::terminals(&state).remove(agent)
            .ok_or_else(|| refused("session_unreachable", "no background session is known for this agent"))?,
    };
    let terminal = terminal.clone();
    let delivered = match tokio::task::spawn_blocking(move || terminal.write(&short, &text)).await {
        Ok(Ok(())) => State::Handed,
        Ok(Err(WriteFailure::NotStarted)) => return Err(refused("session_unreachable", "the agent's session did not open; nothing was typed")),
        Ok(Err(WriteFailure::Uncertain)) | Err(_) => State::Uncertain,
    };
    let mut answer = json!({"delivery_state": delivered});
    if let (true, Some(interaction)) = (body.mark_read, body.interaction_id) {
        if let Err(error) = mark_read(domain, user, interaction) {
            answer["mark_read_error"] = json!(error);
        }
    }
    Ok(answer)
}

fn mark_read(domain: &DomainRuntime, user: &Actor, interaction_id: InteractionId) -> Result<(), DomainError> {
    let state = domain.state().map_err(|_| refused("storage_unavailable", "domain state unavailable"))?;
    let envelope = CommandEnvelope {
        protocol_version: alondra_core::domain_v2::PROTOCOL_VERSION, data_epoch: state.data_epoch,
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
    use std::sync::Mutex;

    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct Listed(Option<Session>);
    impl Sessions for Listed {
        async fn find(&self, _: &str) -> Option<Session> {
            self.0.clone()
        }
    }

    #[derive(Default)]
    struct Typed(Mutex<Vec<(String, String)>>, Mutex<Option<WriteFailure>>);
    impl Terminal for Typed {
        fn write(&self, short: &str, text: &str) -> Result<(), WriteFailure> {
            if let Some(failure) = self.1.lock().unwrap().take() {
                return Err(failure);
            }
            self.0.lock().unwrap().push((short.into(), text.into()));
            Ok(())
        }
    }

    fn user() -> Actor {
        Actor::User { user_id: "device:d".into() }
    }

    fn world() -> (Fixture, DomainRuntime) {
        let home = std::env::temp_dir().join(format!("alondra-v2-messages-{}", crate::auth::random_hex(8)));
        std::fs::create_dir_all(&home).unwrap();
        let domain = DomainRuntime::open(&home).unwrap();
        let run = |actor: Actor, id: &str, command: Command| {
            let state = domain.state().unwrap();
            let envelope = CommandEnvelope { protocol_version: alondra_core::domain_v2::PROTOCOL_VERSION, data_epoch: state.data_epoch,
                command_id: id.into(), expected_revision: state.revision, command };
            domain.execute(&actor, &envelope, None, || Ok(())).unwrap();
        };
        run(Actor::System, "a", Command::RegisterAgent { agent_id: "agent-a".into(), name: "A".into() });
        run(Actor::System, "b", Command::RegisterAgent { agent_id: "agent-b".into(), name: "B".into() });
        run(user(), "r", Command::CreateRequest { request_id: "r".into(), body: "Build".into() });
        run(user(), "t", Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Build it".into(),
            agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None });
        (Fixture(home), domain)
    }

    fn body(value: Value) -> MessageBody {
        serde_json::from_value(value).unwrap()
    }

    fn session(short: &str, interactive: bool) -> Listed {
        Listed(Some(Session { short: short.into(), interactive }))
    }

    #[tokio::test]
    async fn the_text_is_typed_into_the_agents_session_and_nothing_is_recorded() {
        let (fixture, domain) = world();
        let terminal = Arc::new(Typed::default());
        let before = domain.state().unwrap();
        let answer = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Use the new API", "task_id": "t"})), &session("7c5dcf5d", false), &terminal).await.unwrap();
        assert_eq!(answer, json!({"delivery_state": "handed"}));
        assert_eq!(*terminal.0.lock().unwrap(), [("7c5dcf5d".to_string(), "[Task t “Build it”] Use the new API".to_string())]);
        assert_eq!(domain.state().unwrap(), before, "a message records nothing in the domain");
    }

    #[tokio::test]
    async fn a_reply_is_worded_for_its_interaction() {
        let (fixture, domain) = world();
        let terminal = Arc::new(Typed::default());
        deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Yes", "kind": "reply", "interaction_id": "q-1"})), &session("s", false), &terminal).await.unwrap();
        assert_eq!(terminal.0.lock().unwrap()[0].1, "[Reply to q-1] Yes");
        let refused = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Yes", "kind": "pr_changes"})), &session("s", false), &terminal).await.unwrap_err();
        assert_eq!(refused.code, "invalid_input");
    }

    #[tokio::test]
    async fn refusals_type_nothing() {
        let (fixture, domain) = world();
        let terminal = Arc::new(Typed::default());
        let cases = [
            ("agent-z", json!({"text": "hi"}), session("s", false), "not_found"),
            ("agent-a", json!({"text": "  "}), session("s", false), "invalid_input"),
            ("agent-b", json!({"text": "hi", "task_id": "t"}), session("s", false), "not_assigned"),
            ("agent-a", json!({"text": "hi", "task_id": "missing"}), session("s", false), "not_found"),
            ("agent-a", json!({"text": "hi"}), session("s", true), "interactive_session"),
            ("agent-a", json!({"text": "hi"}), Listed(None), "session_unreachable"),
        ];
        for (agent, request, sessions, code) in cases {
            let error = deliver(&domain, &fixture.0, &user(), "Ana", &agent.into(), body(request.clone()), &sessions, &terminal).await.unwrap_err();
            assert_eq!(error.code, code, "{agent} {request}");
        }
        assert!(terminal.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_session_that_did_not_open_is_refused_and_a_partial_write_is_uncertain() {
        let (fixture, domain) = world();
        let terminal = Arc::new(Typed::default());
        *terminal.1.lock().unwrap() = Some(WriteFailure::NotStarted);
        let error = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(), body(json!({"text": "hi"})), &session("s", false), &terminal).await.unwrap_err();
        assert_eq!(error.code, "session_unreachable");
        *terminal.1.lock().unwrap() = Some(WriteFailure::Uncertain);
        let answer = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(), body(json!({"text": "hi"})), &session("s", false), &terminal).await.unwrap();
        assert_eq!(answer["delivery_state"], "uncertain");
    }

    #[tokio::test]
    async fn a_reply_that_reads_the_information_marks_it_read_after_typing() {
        let (fixture, domain) = world();
        let state = domain.state().unwrap();
        domain.execute(&Actor::Agent { agent_id: "agent-a".into() }, &CommandEnvelope {
            protocol_version: alondra_core::domain_v2::PROTOCOL_VERSION, data_epoch: state.data_epoch, command_id: "i".into(),
            expected_revision: state.revision, command: Command::PublishInformation { attachments: vec![], interaction_id: "info-1".into(),
                scope: alondra_core::domain_v2::Scope::Task { task_id: "t".into() }, recipient: alondra_core::domain_v2::Recipient::User, body: "Done".into() },
        }, None, || Ok(())).unwrap();
        let terminal = Arc::new(Typed::default());
        let answer = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Thanks", "kind": "reply", "interaction_id": "info-1", "mark_read": true})), &session("s", false), &terminal).await.unwrap();
        assert_eq!(answer, json!({"delivery_state": "handed"}));
        let read = &domain.state().unwrap().interactions[&InteractionId::from("info-1")].payload;
        assert!(matches!(read, alondra_core::domain_v2::InteractionPayload::Information { read_by } if !read_by.is_empty()), "{read:?}");
    }

    #[tokio::test]
    async fn an_attachment_is_named_by_its_stored_path_once_its_digest_matches() {
        use sha2::Digest as _;
        let (fixture, domain) = world();
        std::fs::create_dir_all(fixture.0.join("files")).unwrap();
        std::fs::write(fixture.0.join("files/shot.png"), b"png").unwrap();
        let digest: String = sha2::Sha256::digest(b"png").iter().map(|b| format!("{b:02x}")).collect();
        let attachment = json!({"id": "att-1", "name": "shot.png", "content_digest": digest, "content_reference": "files/shot.png", "bytes": 3});
        let terminal = Arc::new(Typed::default());
        deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Look", "attachments": [attachment]})), &session("s", false), &terminal).await.unwrap();
        let typed = terminal.0.lock().unwrap()[0].1.clone();
        assert!(typed.starts_with("Look\n\nAna attached a file"), "{typed}");
        assert!(typed.contains(&fixture.0.join("files/shot.png").display().to_string()), "{typed}");
        let forged = json!({"id": "att-1", "name": "shot.png", "content_digest": "0".repeat(64), "content_reference": "files/shot.png", "bytes": 3});
        let error = deliver(&domain, &fixture.0, &user(), "Ana", &"agent-a".into(),
            body(json!({"text": "Look", "attachments": [forged]})), &session("s", false), &terminal).await.unwrap_err();
        assert_eq!(error.code, "invalid_input");
        assert_eq!(terminal.0.lock().unwrap().len(), 1);
    }

    #[test]
    fn the_client_contract_body_is_accepted() {
        let contract: Value = serde_json::from_str(include_str!("../../../fixtures/client-v2/requests.json")).unwrap();
        let parsed = body(contract["message"]["body"].clone());
        assert_eq!((parsed.kind, parsed.pr_id), (Kind::PrChanges, Some("pr-1".into())));
        assert_eq!(parsed.attachments.len(), 1);
        assert_eq!(contract["message"]["response"], json!({"delivery_state": "handed"}));
    }

    #[test]
    fn the_short_id_comes_from_claude_agents() {
        let listed = r#"[{"id":"7c5dcf5d","kind":"background","sessionId":"7c5dcf5d-aaaa"},{"id":"11112222","kind":"interactive","sessionId":"1111-bbbb"}]"#;
        assert_eq!(session_in(listed, "7c5dcf5d-aaaa"), Some(Session { short: "7c5dcf5d".into(), interactive: false }));
        assert_eq!(session_in(listed, "1111-bbbb"), Some(Session { short: "11112222".into(), interactive: true }));
        assert_eq!(session_in(listed, "other"), None);
        assert_eq!(session_in("not json", "7c5dcf5d-aaaa"), None);
    }
}
