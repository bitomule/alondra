//! The one-shot commands that act on the domain or on a session: each resolves what the user
//! named against the daemon's projection and sends the same command or request the TUI sends.

use std::collections::BTreeMap;
use std::time::Duration;

use alondra_core::domain::{AgentId, Command, DomainProjection, InboxItem, InputKind, InputValue, Interaction, InteractionId, InteractionPayload, QuestionOption, Revision, TaskId};
use alondra_core::views::{Action, Item, Link, View};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::client::Client;
use crate::ops::{self, Recovery};
use crate::runtime::Runtime;

fn view_action(view: &View, predicate: impl Fn(&Action) -> bool) -> Option<Action> {
    fn scan(items: &[Item], predicate: &impl Fn(&Action) -> bool) -> Option<Action> {
        for item in items {
            match item {
                Item::Card(card) => {
                    if let Some(action) = card.actions.iter().find(|a| predicate(a)) { return Some(action.clone()); }
                }
                Item::Actions(bar) => if let Some(action) = bar.actions.iter().find(|a| predicate(a)) { return Some(action.clone()); },
                Item::Row(row) => {
                    if let Some(action) = row.actions.iter().find(|a| predicate(a)) { return Some(action.clone()); }
                }
                Item::Form(form) => if predicate(&form.submit) { return Some(form.submit.clone()); },
                _ => {}
            }
        }
        None
    }
    view.sections.iter().find_map(|section| scan(&section.items, &predicate))
}

fn local_attach(view: &View) -> Option<String> {
    fn scan(items: &[Item]) -> Option<String> {
        for item in items {
            let actions: &[Action] = match item {
                Item::Card(card) => &card.actions,
                Item::Actions(bar) => &bar.actions,
                Item::Row(row) => &row.actions,
                Item::Form(form) => std::slice::from_ref(&form.submit),
                _ => &[],
            };
            for action in actions {
                if let Some(Link::Local { local: alondra_core::views::LocalLink::Attach { session } }) = &action.link {
                    return Some(session.clone());
                }
            }
        }
        None
    }
    view.sections.iter().find_map(|section| scan(&section.items))
}

fn submit(client: &Client, view: &View, action: &Action, option: Option<&str>, input: Value) -> Result<Value> {
    anyhow::ensure!(action.enabled, "{}", action.disabled_reason.as_deref().unwrap_or("This action is unavailable"));
    client.action(&view.data_epoch, &action.id, option, input)
}

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
        [] => bail!(trf!("No node is called {name}", "Ningún nodo se llama {name}")),
        many => {
            let names = many.iter().map(|a| a.0.as_str()).collect::<Vec<_>>().join(", ");
            bail!(trf!("{name} names several nodes: {names}", "{name} nombra varios nodos: {names}"))
        }
    }
}

fn inbox_item<'a>(d: &'a DomainProjection, id: &InteractionId) -> Option<&'a InboxItem> {
    d.inbox.iter().find(|i| matches!(&i.subject, alondra_core::domain::InboxSubject::Interaction { interaction_id } if interaction_id == id))
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

pub fn answer_cmd(interaction: &str, option: Option<&str>, text: &str) -> Result<()> {
    let client = user();
    let id = interaction.strip_prefix("interaction:").unwrap_or(interaction);
    let view = client.views(&format!("card:{id}"), None).with_context(|| trf!("No question {id}", "No hay ninguna pregunta {id}"))?;
    let action = view_action(&view, |action| action.enabled && action.id.starts_with("ans:"))
        .context(trf!("{id} is not an unanswered question", "{id} no es una pregunta sin responder"))?;
    let selected = match option {
        Some(value) => value.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|n| action.options.get(n))
            .or_else(|| action.options.iter().find(|candidate| candidate.id == value)),
        None if !text.is_empty() => action.options.iter().find(|candidate| matches!(candidate.input, alondra_core::views::Input::Text { .. })),
        _ => None,
    }.with_context(|| {
        let labels = action.options.iter().enumerate().map(|(i, o)| format!("  {} {} [{}]", i + 1, o.label, o.id)).collect::<Vec<_>>().join("\n");
        trf!("Choose an option with --option:\n{labels}", "Elige una opción con --option:\n{labels}")
    })?;
    let input_spec = &selected.input;
    let label = &selected.label;
    match input_spec {
        alondra_core::views::Input::Text { required, .. } if text.is_empty() && *required => bail!(trf!("Option {label} needs your text", "La opción {label} necesita tu texto")),
        alondra_core::views::Input::None if !text.is_empty() => bail!(trf!("Option {label} takes no text", "La opción {label} no admite texto")),
        _ => {}
    }
    submit(&client, &view, &action, Some(&selected.id), if text.is_empty() { serde_json::json!({}) } else { serde_json::json!({"text":text}) })?;
    println!("{}", trf!("Answered {interaction}", "Contestada {interaction}"));
    Ok(())
}

/// Cancels each task, one command each against the revision the previous one left; a reason
/// is recorded beside a cancelled task.
pub fn cancel(tasks: &[String], reason: Option<&str>) -> Result<()> {
    let client = user();
    let mut failed = 0;
    for task in tasks {
        let done = client.views(&format!("task:{task}"), None).and_then(|view| {
            let action = view_action(&view, |action| action.id.starts_with("cancel:")).context("Task cannot be cancelled")?;
            submit(&client, &view, &action, None, serde_json::json!({}))?;
            if let Some(reason) = reason.map(str::trim).filter(|r| !r.is_empty()) {
                let (path, body) = ops::cancel_reason(&TaskId::from(task.as_str()), reason);
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

fn resolved(client: &Client, name: &str) -> Result<(Option<String>, Option<String>)> {
    if name.eq_ignore_ascii_case("orchestrator") { return Ok((None, None)); }
    let fleet = client.views("fleet", None)?;
    let mut found = Vec::new();
    fn rows(items: &[Item], out: &mut Vec<(String, String)>) {
        for item in items { if let Item::Row(row) = item {
            if let Some(Link::View { key }) = &row.link {
                if let Some(id) = key.strip_prefix("node:") { out.push((id.into(), row.title.clone())); }
            }
            rows(&row.children.iter().cloned().map(Item::Row).collect::<Vec<_>>(), out);
        }}
    }
    for section in &fleet.sections { rows(&section.items, &mut found); }
    let exact: Vec<_> = found.iter().filter(|(id, title)| id == name || title.eq_ignore_ascii_case(name) || id.rsplit('-').next() == Some(name)).collect();
    let mut matches: Vec<(String, String)> = if exact.is_empty() {
        found.iter().filter(|(id, _)| id.starts_with(name)).cloned().collect()
    } else {
        exact.into_iter().map(|(id, title)| (id.clone(), title.clone())).collect()
    };
    if matches.is_empty() {
        for (id, _) in &found {
            if client.views(&format!("node:{id}"), None).ok().and_then(|view| local_attach(&view)).as_deref() == Some(name) {
                matches.push((id.clone(), String::new()));
            }
        }
    }
    match matches.as_slice() {
        [one] => Ok((Some(one.0.clone()), Some(one.0.clone()))),
        [] => bail!(trf!("No node is called {name}", "Ningún nodo se llama {name}")),
        _ => bail!(trf!("{name} names several nodes", "{name} nombra varios nodos")),
    }
}

/// A Task id sends to the Task's agent, about that Task; anything else names the agent.
pub fn send(name: &str, text: &str) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(60));
    let view = if name.starts_with("t-") {
        client.views(&format!("task:{name}"), None).with_context(|| trf!("No task {name}", "No hay ninguna tarea {name}"))?
    } else if name.eq_ignore_ascii_case("orchestrator") {
        client.views("node:orchestrator", None)?
    } else {
        let agent = resolved(&client, name)?.0.context("Agent not found")?;
        client.views(&format!("node:{agent}"), None)?
    };
    let action = view_action(&view, |a| a.id.starts_with("msg:" )).context("Message action unavailable")?;
    let answer = submit(&client, &view, &action, None, serde_json::json!({"text":text}))?;
    anyhow::ensure!(answer.get("outcome").and_then(Value::as_str) == Some("applied"), "{}",
        answer.get("message").and_then(Value::as_str).map(str::to_owned)
            .unwrap_or_else(|| trf!("The message was not sent to {name}", "El mensaje no se envió a {name}")));
    println!("{}", sent_notice(&answer, name));
    Ok(())
}

fn sent_notice(answer: &Value, name: &str) -> String {
    if let Some(stamp) = answer.get("stamp").and_then(Value::as_str) { return stamp.to_string(); }
    if let Some(note) = answer.get("note").and_then(Value::as_str) {
        return note.to_string();
    }
    match answer.get("delivery_state").and_then(Value::as_str) {
        Some("uncertain") => trf!("It may not have reached {name}'s session; check it before sending again", "Puede que no haya llegado a la sesión de {name}; revísala antes de reenviar"),
        Some("handed") => trf!("Delivered to {name}'s session", "Entregado a la sesión de {name}"),
        Some("dispatching") => trf!("{name} is busy: the message reaches its session as soon as the current turn ends", "{name} está ocupado: el mensaje llega a su sesión en cuanto termine el turno actual"),
        _ => crate::delivery::response_notice(answer, None).unwrap_or_else(|| trf!("Sent to {name}", "Enviado a {name}")),
    }
}

pub fn close(name: &str, force: bool) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(90));
    let Some(agent) = resolved(&client, name)?.0 else { bail!(tr!("Alondra manages the orchestrator automatically", "Alondra gestiona el orquestador automáticamente")) };
    let view = client.views(&format!("node:{agent}"), None)?;
    let action = view_action(&view, |a| a.id.starts_with("close:" )).context("Close action unavailable")?;
    let answer = submit(&client, &view, &action, None, serde_json::json!({"fields":{"force":force}}))?;
    if answer.get("detail").and_then(|detail| detail.get("interactive")).and_then(Value::as_bool) == Some(true) {
        println!("{}", trf!("{name} left the fleet; it is an interactive session, so close its terminal with /exit", "{name} ha salido de la flota; es una sesión interactiva, así que cierra su terminal con /exit"));
    } else {
        println!("{}", trf!("Closed {name}", "Cerrado {name}"));
    }
    Ok(())
}

pub fn attach(name: &str) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(10));
    let (agent, _runtime) = resolved(&client, name)?;
    let Some(agent) = agent else { bail!(tr!("Alondra manages the orchestrator automatically; open its activity in the Fleet screen", "Alondra gestiona el orquestador automáticamente; abre su actividad en la pantalla Flota")) };
    let view = client.views(&format!("node:{agent}"), None)?;
    let session = local_attach(&view).context("This node has no attachable session")?;
    crate::attach::attach(&session, crate::attach::Over::Shell)
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
    use alondra_core::domain::*;

    fn world() -> DomainState {
        let mut s = DomainState::new("e");
        let mut run = |actor: Actor, command: Command| {
            let envelope = CommandEnvelope { protocol_version: PROTOCOL_VERSION, data_epoch: s.data_epoch.clone(), command_id: format!("c{}", s.revision).into(), expected_revision: s.revision, command };
            s.execute(&actor, &envelope).unwrap();
        };
        let user = || Actor::User { user_id: "u".into() };
        run(user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false });
        run(Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into() });
        run(Actor::System, Command::RegisterAgent { agent_id: "agent-ab".into(), name: "Instance AB".into() });
        run(user(), Command::CreateTask { task_id: "t".into(), request_id: "r".into(), title: "Build".into(), agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None });
        let options = vec![
            QuestionOption { id: "yes".into(), label: "Yes".into(), description: None, inputs: vec![] },
            QuestionOption { id: "other".into(), label: "Other".into(), description: None, inputs: vec![InputSpec { id: "why".into(), kind: InputKind::Text, required: true }] },
        ];
        run(Actor::Agent { agent_id: "agent-a".into() }, Command::AskQuestion { attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, body: "Name?".into(), blocking: true, options, card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question") });
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
        assert!(named("agent").unwrap_err().to_string().contains("several nodes"));
        assert!(named("nobody").unwrap_err().to_string().contains("No node"));
    }
}
