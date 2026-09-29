//! The one-shot commands that act on the domain or on a session: each resolves what the user
//! named against the daemon's projection and sends the same command or request the TUI sends.

use std::collections::BTreeMap;
use std::time::Duration;

use alondra_core::domain_v2::{AgentId, Command, DomainProjection, InboxItem, InputKind, InputValue, Interaction, InteractionId, InteractionPayload, QuestionOption, Revision, TaskId};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::client::{Client, CommandFailure};
use crate::ops::{self, MessageKind, Recovery};
use crate::runtime::Runtime;

/// The agent a name, short id or session id names; `None` for the orchestrator.
pub fn agent(d: &DomainProjection, runtime: Option<&Runtime>, name: &str) -> Result<Option<AgentId>> {
    if name.eq_ignore_ascii_case("orchestrator") || runtime.and_then(Runtime::orchestrator_node).is_some_and(|o| o.session_id == name || o.short_id == name) {
        return Ok(None);
    }
    let short = |id: &AgentId| runtime.and_then(|r| r.node(id)).map(|n| n.short_id.clone());
    let exact: Vec<&AgentId> = d.agents.iter().filter(|a| a.id.0 == name || a.name.eq_ignore_ascii_case(name) || short(&a.id).as_deref() == Some(name)).map(|a| &a.id).collect();
    let found: Vec<&AgentId> = if exact.is_empty() { d.agents.iter().filter(|a| a.id.0.starts_with(name)).map(|a| &a.id).collect() } else { exact };
    match found.as_slice() {
        [one] => Ok(Some((*one).clone())),
        [] => bail!(trf!("No agent is called {name}", "Ningún agente se llama {name}")),
        many => {
            let names = many.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", ");
            bail!(trf!("{name} names several agents: {names}", "{name} nombra varios agentes: {names}"))
        }
    }
}

fn inbox_item<'a>(d: &'a DomainProjection, id: &InteractionId) -> Option<&'a InboxItem> {
    d.inbox.iter().flat_map(|r| std::iter::once(r).chain(&r.children)).find(|i| matches!(&i.subject, alondra_core::domain_v2::InboxSubject::Interaction { interaction_id } if interaction_id == id))
}

fn options_text(options: &[QuestionOption]) -> String {
    options.iter().enumerate().map(|(i, o)| {
        let writes = if o.inputs.iter().any(|s| s.kind == InputKind::Text) { tr!(" (takes text)", " (admite texto)") } else { "" };
        format!("  {} {} [{}]{writes}", i + 1, o.label, o.id)
    }).collect::<Vec<_>>().join("\n")
}

/// The `answer_question` command for what the user typed: the option by number or id, and the
/// text as that option's text input. Without an option, the only one that takes text.
pub fn answer(d: &DomainProjection, found: &Interaction, option: Option<&str>, text: &str) -> Result<(Revision, Command)> {
    let id = found.id.clone();
    let (InteractionPayload::Question { options, answer } | InteractionPayload::ActionRequest { options, answer }) = &found.payload else {
        bail!(trf!("{id} is not a question", "{id} no es una pregunta"));
    };
    if answer.is_some() {
        bail!(trf!("{id} is already answered", "{id} ya está contestada"));
    }
    let writes = |o: &&QuestionOption| o.inputs.iter().any(|s| s.kind == InputKind::Text);
    let chosen = match option {
        Some(o) => o.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|n| options.get(n)).or_else(|| options.iter().find(|x| x.id == o)),
        None if !text.is_empty() => match options.iter().filter(writes).collect::<Vec<_>>().as_slice() {
            [one] => Some(*one),
            _ => None,
        },
        None => None,
    };
    let Some(chosen) = chosen else {
        let list = options_text(options);
        bail!(trf!("Choose an option with --option:\n{list}", "Elige una opción con --option:\n{list}"));
    };
    let mut inputs = BTreeMap::new();
    let label = &chosen.label;
    match chosen.inputs.iter().find(|s| s.kind == InputKind::Text) {
        Some(input) if !text.is_empty() => {
            inputs.insert(input.id.clone(), InputValue::Text(text.to_string()));
        }
        Some(input) if input.required => bail!(trf!("Option {label} needs your text", "La opción {label} necesita tu texto")),
        None if !text.is_empty() => bail!(trf!("Option {label} takes no text", "La opción {label} no admite texto")),
        _ => {}
    }
    let revision = inbox_item(d, &id).and_then(|i| i.available_actions.iter().find(|a| a.kind == "answer_question")).map_or(d.revision, |a| a.expected_revision);
    Ok((revision, Command::AnswerQuestion { attachments: vec![], interaction_id: id, option_id: chosen.id.clone(), inputs }))
}

fn user() -> Client {
    Client::for_user(Duration::from_secs(15))
}

fn run(client: &Client, d: &DomainProjection, revision: Revision, command: Command) -> Result<Revision> {
    match client.command(&crate::client::envelope(&d.data_epoch, revision, command)) {
        Ok(outcome) => Ok(outcome.revision),
        Err(CommandFailure::Rejected(e)) => Err(anyhow!(crate::client::refusal(&e))),
        Err(CommandFailure::Uncertain(why)) => Err(anyhow!(trf!("Not confirmed: {why}; check before repeating", "Sin confirmar: {why}; revísalo antes de repetir"))),
    }
}

pub fn answer_cmd(interaction: &str, option: Option<&str>, text: &str) -> Result<()> {
    let client = user();
    let d = client.domain()?;
    let id = InteractionId::from(interaction.strip_prefix("interaction:").unwrap_or(interaction));
    let found = client.interaction(&id).with_context(|| trf!("No question {id}", "No hay ninguna pregunta {id}"))?;
    let (revision, command) = answer(&d, &found, option, text)?;
    run(&client, &d, revision, command)?;
    println!("{}", trf!("Answered {interaction}", "Contestada {interaction}"));
    Ok(())
}

/// Cancels each task, one command each against the revision the previous one left; a reason
/// is recorded beside a cancelled task.
pub fn cancel(tasks: &[String], reason: Option<&str>) -> Result<()> {
    let client = user();
    let d = client.domain()?;
    let mut revision = d.revision;
    let mut failed = 0;
    for task in tasks {
        let id = TaskId::from(task.as_str());
        let done = run(&client, &d, revision, Command::CancelTask { task_id: id.clone() }).and_then(|r| {
            revision = r;
            if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
                let (path, body) = ops::cancel_reason(&id, reason);
                client.put(&path, &body).map_err(|e| anyhow!(trf!("cancelled, but the reason was not recorded: {e:#}", "descartada, pero no se guardó el motivo: {e:#}")))?;
            }
            Ok(())
        });
        match done {
            Ok(()) => println!("{}", trf!("Cancelled {task}", "Cancelada {task}")),
            Err(error) => {
                failed += 1;
                eprintln!("{}", trf!("{task} not cancelled: {error:#}", "{task} no se canceló: {error:#}"));
            }
        }
    }
    anyhow::ensure!(failed == 0, "{failed} of {} tasks not cancelled", tasks.len());
    Ok(())
}

fn resolved(client: &Client, name: &str) -> Result<(Option<AgentId>, Option<Runtime>)> {
    let d = client.domain()?;
    let runtime = client.runtime().unwrap_or(None);
    Ok((agent(&d, runtime.as_ref(), name)?, runtime))
}

/// A Task id sends to the Task's agent, about that Task; anything else names the agent.
pub fn send(name: &str, text: &str) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(60));
    let d = client.domain()?;
    let task = d.tasks.iter().find(|t| t.id.0 == name);
    let message = match task {
        Some(t) => {
            let agent = t.agent_id.clone().with_context(|| trf!("{name} has no agent yet", "{name} aún no tiene agente"))?;
            ops::Message { task: Some(t.id.clone()), ..ops::Message::to(Some(agent), MessageKind::Message, text.to_string()) }
        }
        None => ops::Message::to(resolved(&client, name)?.0, MessageKind::Message, text.to_string()),
    };
    let answer = client.post(&message.path(), &message.body())?;
    println!("{}", sent_notice(&answer, name));
    Ok(())
}

fn sent_notice(answer: &Value, name: &str) -> String {
    if let Some(note) = answer.get("note").and_then(Value::as_str) {
        return note.to_string();
    }
    match answer.get("delivery_state").and_then(Value::as_str) {
        Some("uncertain") => trf!("It may not have reached {name}'s session; check it before sending again", "Puede que no haya llegado a la sesión de {name}; revísala antes de reenviar"),
        Some("handed") => trf!("Typed into {name}'s session", "Escrito en la sesión de {name}"),
        _ => crate::delivery::response_notice(answer, None).unwrap_or_else(|| trf!("Sent to {name}", "Enviado a {name}")),
    }
}

pub fn close(name: &str, force: bool) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(90));
    let Some(agent) = resolved(&client, name)?.0 else { bail!(tr!("Alondra manages the orchestrator automatically", "Alondra gestiona el orquestador automáticamente")) };
    let (path, body) = ops::close(&agent, force);
    let answer: Value = client.post(&path, &body)?;
    if answer.get("interactive").and_then(Value::as_bool) == Some(true) {
        println!("{}", trf!("{name} left the fleet; it is an interactive session, so close its terminal with /exit", "{name} ha salido de la flota; es una sesión interactiva, así que cierra su terminal con /exit"));
    } else {
        println!("{}", trf!("Closed {name}", "Cerrado {name}"));
    }
    Ok(())
}

pub fn attach(name: &str) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(10));
    let (agent, runtime) = resolved(&client, name)?;
    let Some(agent) = agent else { bail!(tr!("Alondra manages the orchestrator automatically; open its activity in the Agents screen", "Alondra gestiona el orquestador automáticamente; abre su actividad en la pantalla de agentes")) };
    let short = runtime.as_ref().and_then(|r| r.node(&agent)).map_or_else(|| agent.0.clone(), |n| n.short_id.clone());
    crate::attach::attach(&short, crate::attach::Over::Shell)
}

pub fn recover(action: Recovery, run_id: &str) -> Result<()> {
    let (path, body) = ops::orchestrator(action, run_id);
    user().post(&path, &body)?;
    println!("{}", match action {
        Recovery::Cancel => tr!("Orchestrator stop requested", "Parada del orquestador solicitada"),
        Recovery::Acknowledge => tr!("Previous batch acknowledged; newer work remains queued", "Lote anterior resuelto; el trabajo nuevo sigue en cola"),
        Recovery::Retry => tr!("Orchestrator retry queued", "Reintento del orquestador encolado"),
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::domain_v2::*;

    fn world() -> DomainState {
        let mut s = DomainState::new("e");
        let mut run = |actor: Actor, command: Command| {
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c{}", s.revision).into(), expected_revision: s.revision, command };
            s.execute(&actor, &envelope).unwrap();
        };
        let user = || Actor::User { user_id: "u".into() };
        run(user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into() });
        run(Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into() });
        run(Actor::System, Command::RegisterAgent { agent_id: "agent-ab".into(), name: "Instance AB".into() });
        run(user(), Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Build".into(), agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None });
        let options = vec![
            QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] },
            QuestionOption { id: "other".into(), label: "Other".into(), description: None, inputs: vec![InputSpec { id: "why".into(), kind: InputKind::Text, required: true }] },
        ];
        run(Actor::Agent { agent_id: "agent-a".into() }, Command::AskQuestion { attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, body: "Name?".into(), blocking: true, options });
        s
    }

    #[test]
    fn answer_takes_the_option_by_number_or_id_and_text_goes_to_its_input() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let mut s = world();
        let d = s.projection();
        let q = s.interactions[&InteractionId::from("q")].clone();
        let (_, by_number) = answer(&d, &q, Some("1"), "").unwrap();
        assert!(matches!(&by_number, Command::AnswerQuestion { option_id, inputs, .. } if option_id == "yes" && inputs.is_empty()));
        let (_, by_text) = answer(&d, &q, None, "Lark").unwrap();
        assert!(matches!(&by_text, Command::AnswerQuestion { option_id, inputs, .. } if option_id == "other" && inputs.get("why") == Some(&InputValue::Text("Lark".into()))));
        let refused = answer(&d, &q, None, "").unwrap_err().to_string();
        assert!(refused.contains("--option") && refused.contains("2 Other [other] (takes text)"), "{refused}");
        assert!(answer(&d, &q, Some("yes"), "why not").unwrap_err().to_string().contains("takes no text"));
        let (revision, command) = answer(&d, &q, Some("other"), "Lark").unwrap();
        let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: d.data_epoch.clone(), command_id: "cli".into(), expected_revision: revision, command };
        s.execute(&Actor::User { user_id: "u".into() }, &envelope).unwrap();
        let answered = s.interactions[&InteractionId::from("q")].clone();
        assert!(answer(&s.projection(), &answered, Some("1"), "").unwrap_err().to_string().contains("already answered"));
    }

    #[test]
    fn an_agent_is_named_by_id_name_short_id_or_a_unique_prefix() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let d = world().projection();
        let runtime: Runtime = serde_json::from_value(crate::runtime::fixture("runtime.json")).unwrap();
        let named = |n: &str| agent(&d, Some(&runtime), n).map(|a| a.map(|a| a.0));
        assert_eq!(named("agent-a").unwrap(), Some("agent-a".into()));
        assert_eq!(named("instance ab").unwrap(), Some("agent-ab".into()));
        assert_eq!(named("a1b2c3").unwrap(), Some("agent-a".into()), "the short id from the runtime");
        assert_eq!(named("orchestrator").unwrap(), None);
        assert_eq!(named("alondra-orchestrator").unwrap(), None);
        assert!(named("agent").unwrap_err().to_string().contains("several agents"));
        assert!(named("nobody").unwrap_err().to_string().contains("No agent"));
    }
}
