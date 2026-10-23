//! The one-shot commands that act on the domain or on a session: each resolves what the user
//! named against the daemon's projection and sends the same command or request the TUI sends.

use std::time::Duration;

use alondra_core::domain::{Command, InteractionId, InteractionPayload, QuestionOption, TaskId};
use alondra_core::views::{Action, Item, View};
use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

use crate::client::Client;
use crate::ops::{self, Recovery};

pub(crate) fn view_action(view: &View, predicate: impl Fn(&Action) -> bool) -> Option<Action> {
    fn scan(items: &[Item], predicate: &impl Fn(&Action) -> bool) -> Option<Action> {
        for item in items {
            match item {
                Item::Card(card) => {
                    if let Some(action) = card.actions.iter().find(|a| predicate(a)) { return Some(Action::from(action.clone())); }
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

fn submit(client: &Client, view: &View, action: &Action, option: Option<&str>, input: Value) -> Result<Value> {
    anyhow::ensure!(action.enabled, "{}", action.disabled_reason.as_deref().unwrap_or("This action is unavailable"));
    client.action(&view.data_epoch, &action.id, option, input)
}

fn user() -> Client {
    Client::for_user(Duration::from_secs(15))
}

/// The action that answers what the user typed: the option by number or id, the text going with
/// it as its note; with no option, the card's own field, where the text is the user's answer.
fn answer_choice(view: &View, option: Option<&str>, text: &str) -> Option<Result<(Action, Option<String>)>> {
    let cards = || view.sections.iter().flat_map(|section| &section.items).filter_map(|item| match item {
        Item::Card(card) => Some(card),
        _ => None,
    });
    let buttons: Vec<(Action, String)> = cards().flat_map(|card| card.actions.iter().chain(&card.more).cloned().map(Action::from))
        .filter(|action| action.enabled && action.id.starts_with("ans:"))
        .filter_map(|action| action.option_id.clone().map(|id| (action, id)))
        .collect();
    let reply = cards().find_map(|card| card.reply.clone()).map(Action::from).filter(|action| action.enabled);
    if buttons.is_empty() && reply.is_none() {
        return None;
    }
    let chosen = match option {
        Some(value) => value.parse::<usize>().ok().and_then(|n| n.checked_sub(1)).and_then(|n| buttons.get(n))
            .or_else(|| buttons.iter().find(|(_, id)| id == value))
            .map(|(action, id)| (action.clone(), Some(id.clone()))),
        None if !text.trim().is_empty() => reply.map(|action| (action, None)),
        None => None,
    };
    Some(chosen.ok_or_else(|| {
        let labels = buttons.iter().enumerate().map(|(i, (action, id))| format!("  {} {} [{id}]", i + 1, action.label)).collect::<Vec<_>>().join("\n");
        anyhow!(trf!("Choose an option with --option, or write your answer:\n{labels}", "Elige una opción con --option, o escribe tu respuesta:\n{labels}"))
    }))
}

pub fn answer_cmd(interaction: &str, option: Option<&str>, text: &str) -> Result<()> {
    let client = user();
    let id = interaction.strip_prefix("interaction:").unwrap_or(interaction);
    let view = client.views(&format!("card:{id}"), None).with_context(|| trf!("No question {id}", "No hay ninguna pregunta {id}"))?;
    let (action, option) = answer_choice(&view, option, text).with_context(|| trf!("{id} is not an unanswered question", "{id} no es una pregunta sin responder"))??;
    submit(&client, &view, &action, option.as_deref(), if text.is_empty() { serde_json::json!({}) } else { serde_json::json!({"text":text}) })?;
    println!("{}", trf!("Answered {interaction}", "Contestada {interaction}"));
    Ok(())
}

/// The option the daemon's `set_limit` takes for what was typed: `auto` or 1 to 12.
fn node_limit_option(value: &str) -> Result<String> {
    use alondra_core::options::{AUTO, MAX_WORKING_NODES};
    let value = value.trim().to_ascii_lowercase();
    if value == AUTO {
        return Ok(value);
    }
    match value.parse::<u32>() {
        Ok(count) if (1..=MAX_WORKING_NODES).contains(&count) => Ok(count.to_string()),
        _ => bail!(trf!("max_working_nodes takes auto or a number from 1 to {MAX_WORKING_NODES}",
            "max_working_nodes admite auto o un número del 1 al {MAX_WORKING_NODES}")),
    }
}

fn node_limit_action(view: &View) -> Option<Action> {
    view_action(view, |action| action.id.starts_with("set_limit:"))
}

/// How many nodes work at once on this Mac. The daemon writes it to `options.toml`, as when it
/// is changed from Settings in the apps.
pub fn set_node_limit(value: &str) -> Result<()> {
    let option = node_limit_option(value)?;
    let client = user();
    let view = client.views("machine", None)
        .context(tr!("Alondra is not running on this Mac: start it and try again", "Alondra no está en marcha en este Mac: arráncalo y vuelve a probar"))?;
    let action = node_limit_action(&view)
        .context(tr!("This Alondra cannot change the limit yet: update it", "Este Alondra aún no puede cambiar el límite: actualízalo"))?;
    submit(&client, &view, &action, Some(&option), serde_json::json!({}))?;
    println!("max_working_nodes = {option}");
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

/// The view a message goes through: the orchestrator's, or the Task's, whose node it reaches.
fn send_view_key(name: &str) -> String {
    if name == "orchestrator" { name.to_owned() } else { format!("task:{name}") }
}

/// A Task id sends to the Task's node, about that Task; `orchestrator` sends to the orchestrator,
/// and the daemon makes that maintenance unless `reply` asks for an answer.
pub fn send(name: &str, text: &str, reply: bool) -> Result<()> {
    let client = Client::for_user(Duration::from_secs(60));
    let view = client.views(&send_view_key(name), None).with_context(|| trf!(
        "No task {name}; for the orchestrator use orchestrator",
        "No hay ninguna tarea {name}; para el orquestador usa orchestrator"))?;
    let action = view_action(&view, |a| a.id.starts_with("msg:" )).with_context(|| if name == "orchestrator" {
        tr!("Message action unavailable", "No se puede mandar un mensaje").into()
    } else {
        trf!("{name} has no node right now, so nothing was sent. Ask the orchestrator to start one for it.",
            "{name} no tiene nodo ahora, así que no se envió nada. Pide al orquestador que le arranque uno.")
    })?;
    let fields = serde_json::json!({"from_cli": true, "reply": reply, "from_claude": std::env::var_os("CLAUDECODE").is_some()});
    let answer = submit(&client, &view, &action, None, serde_json::json!({"text":text,"fields":fields}))?;
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
        Some("uncertain") => trf!("{name}'s session did not confirm it got it; check it before sending again", "La sesión de {name} no confirmó que le llegara; revísala antes de reenviar"),
        Some("handed") => trf!("Delivered to {name}'s session", "Entregado a la sesión de {name}"),
        Some("dispatching") => trf!("{name} is busy: the message reaches its session as soon as the current turn ends", "{name} está ocupado: el mensaje llega a su sesión en cuanto termine el turno actual"),
        _ => crate::delivery::response_notice(answer, None).unwrap_or_else(|| trf!("Sent to {name}", "Enviado a {name}")),
    }
}

fn status_row(out: &mut String, row: &alondra_core::views::Row, depth: usize) {
    use std::fmt::Write;
    match row.badges.first() {
        Some(badge) => { let _ = writeln!(out, "{:indent$}{:<24} {}", "", badge.text, row.title, indent = depth * 2); }
        None => { let _ = writeln!(out, "{:indent$}{}", "", row.title, indent = depth * 2); }
    }
    for child in &row.children { status_row(out, child, depth + 1); }
}

/// `alondra status`: the summary, what waits for the user, the tasks and the machine.
pub fn status_text(home: &View, tasks: &View) -> String {
    use std::fmt::Write;
    let mut out = format!("{}\n", home.status.summary);
    for (view, heading) in [(home, tr!("Waiting for You", "Te espera")), (tasks, tr!("Tasks", "Tareas"))] {
        let _ = write!(out, "\n{heading}:\n");
        for section in view.sections.iter().filter(|section| section.id != "detail") {
            if view.key != "home" {
                if let Some(title) = &section.title { let _ = writeln!(out, "  {title}:"); }
            }
            for item in &section.items {
                match item {
                    Item::Card(card) => {
                        let summary = card.body.as_ref().map(|body| body.text.lines().next().unwrap_or_default()).unwrap_or(&card.title);
                        let _ = writeln!(out, "  {:<16} {}", card.eyebrow.as_ref().map_or("item", |label| label.text.as_str()), summary);
                    }
                    Item::Row(row) => status_row(&mut out, row, 1),
                    Item::Markdown(markdown) => { let _ = writeln!(out, "  {}", markdown.text.lines().next().unwrap_or_default()); }
                    _ => {}
                }
            }
        }
    }
    if !home.status.gauges.is_empty() {
        let _ = write!(out, "\n{}:\n", tr!("Machine", "Máquina"));
        for gauge in &home.status.gauges { let _ = writeln!(out, "  {}: {}", gauge.label, gauge.text); }
    }
    out
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
        run(user(), Command::CreateRequest { request_id: "r".into(), body: "Build it".into(), auto_merge: false, maintenance: false, message: false });
        run(Actor::System, Command::RegisterAgent { agent_id: "agent-a".into(), name: "Instance A".into(), at: 0 });
        run(Actor::System, Command::RegisterAgent { agent_id: "agent-ab".into(), name: "Instance AB".into(), at: 0 });
        run(Actor::Orchestrator { generation: 0 }, Command::CreateTask { plan_items: None, task_id: "t".into(), request_id: "r".into(), title: "Build".into(), agent_id: Some("agent-a".into()), review_prs: false, dependencies: Default::default(), stack: None, launch: Some(alondra_core::domain::LaunchPlan::new("node", "/tmp")) });
        let options = vec![
            QuestionOption { id: "yes".into(), label: "Yes".into(), description: None },
            QuestionOption { id: "other".into(), label: "Other".into(), description: None },
        ];
        run(Actor::Agent { agent_id: "agent-a".into() }, Command::AskQuestion { attachments: vec![], interaction_id: "q".into(), scope: Scope::Task { task_id: "t".into() }, recipient: Recipient::User, body: "Name?".into(), blocking: true, options, card: alondra_core::domain::CardSpec::derived(alondra_core::domain::CardKind::Provide, "Answer the question") });
        s
    }

    #[test]
    fn the_cli_answers_a_question_card_with_a_valid_option() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        let mut s = world();
        let q = s.interactions.get_mut(&InteractionId::from("q")).unwrap();
        let InteractionPayload::Question { options, .. } = &q.payload else { unreachable!() };
        q.card = CardSpec::question(&q.body, &options.clone());
        let d = alondra_core::domain::ReadIndex::build(&s);
        let live = alondra_core::views::LiveSnapshot::default();
        let context = alondra_core::views::RenderContext { domain: &s, index: &d, live: &live, client: alondra_core::views::Client::Ios, lang: alondra_core::views::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "" };
        let view = alondra_core::views::build("card:q", context).unwrap();
        let (action, option) = answer_choice(&view, Some("yes"), "").unwrap().unwrap();
        assert!(action.id.starts_with("ans:q@"));
        assert_eq!(option.as_deref(), Some("yes"));
        let (by_number, option) = answer_choice(&view, Some("2"), "Lark").unwrap().unwrap();
        assert!(by_number.id.starts_with("ans:q@"), "the text goes with the option: {}", by_number.id);
        assert_eq!(option.as_deref(), Some("other"));
        let (own, option) = answer_choice(&view, None, "Lark").unwrap().unwrap();
        assert!(own.id.starts_with("reply:q@") && option.is_none(), "the text alone is the user's answer: {}", own.id);
        for (option, text) in [(Some("nope"), ""), (None, ""), (None, "  ")] {
            let refused = answer_choice(&view, option, text).unwrap().err().unwrap().to_string();
            assert!(refused.contains("1 Yes [yes]") && refused.contains("2 Other [other]"), "{refused}");
        }
    }

    #[test]
    fn status_lists_what_waits_and_the_tasks_without_a_fleet() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let s = world();
        let d = alondra_core::domain::ReadIndex::build(&s);
        let live = alondra_core::views::LiveSnapshot::default();
        let view = |key: &str| alondra_core::views::build(key, alondra_core::views::RenderContext { domain: &s, index: &d, live: &live, client: alondra_core::views::Client::Tui, lang: alondra_core::views::Lang::Es, transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "" }).unwrap();
        let text = status_text(&view("home"), &view("tasks"));
        assert!(text.contains("Te espera:") && text.contains("Tareas:") && text.contains("Build"), "{text}");
        assert!(!text.contains("Flota") && !text.contains("Instance A"), "{text}");
    }

    #[test]
    fn status_lists_the_selected_card_once() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::Es);
        let s = world();
        let d = alondra_core::domain::ReadIndex::build(&s);
        let live = alondra_core::views::LiveSnapshot::default();
        let view = |key: &str| alondra_core::views::build(key, alondra_core::views::RenderContext { domain: &s, index: &d, live: &live, client: alondra_core::views::Client::Tui, lang: alondra_core::views::Lang::Es, transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "" }).unwrap();
        let home = view("home");
        assert!(home.sections.iter().any(|section| section.id == "detail"), "the TUI home carries the selected card's detail");
        let text = status_text(&home, &view("tasks"));
        assert_eq!(text.matches("Answer the question").count(), 1, "{text}");
    }

    #[test]
    fn send_reaches_a_task_or_the_orchestrator_and_never_looks_up_a_node_by_name() {
        assert_eq!(send_view_key("orchestrator"), "orchestrator");
        assert_eq!(send_view_key("t-login-fix"), "task:t-login-fix");
        assert_eq!(send_view_key("login-fix"), "task:login-fix");
    }

    #[test]
    fn the_node_limit_is_set_through_the_action_this_macs_settings_offer() {
        crate::i18n::set_for_this_thread(crate::i18n::Lang::En);
        assert_eq!(node_limit_option(" Auto ").unwrap(), "auto");
        assert_eq!(node_limit_option("12").unwrap(), "12");
        for refused in ["0", "13", "four", ""] {
            let error = node_limit_option(refused).unwrap_err().to_string();
            assert!(error.contains("auto or a number from 1 to 12"), "{refused}: {error}");
        }

        let s = world();
        let d = alondra_core::domain::ReadIndex::build(&s);
        let live = alondra_core::views::LiveSnapshot {
            pacing: Some(alondra_core::views::LivePacing { working: 2, limit: 6, fixed: None, auto: 6, memory_free: Some(40), tight: false,
                queued: vec![], paused: vec![] }),
            ..Default::default()
        };
        let context = alondra_core::views::RenderContext { domain: &s, index: &d, live: &live, client: alondra_core::views::Client::Tui, lang: alondra_core::views::Lang::En, transport: alondra_core::views::Transport::Loopback, cursor: "c", rendered_at: "" };
        let view = alondra_core::views::build("machine", context).unwrap();
        let action = node_limit_action(&view).expect("the machine view offers the limit");
        assert!(action.id.starts_with("set_limit:limit@"), "{}", action.id);
        assert!(action.options.iter().any(|option| option.id == node_limit_option("4").unwrap()));
    }
}
