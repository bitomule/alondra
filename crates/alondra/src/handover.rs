//! `alondra handover --dry-run`: what the move to the Agent SDK does with each task in progress,
//! without doing anything. The daemon moves each `claude --bg` node by itself.
use std::time::Duration;

use anyhow::Result;
use serde_json::Value;

use crate::client::Client;

pub fn run(json: bool) -> Result<()> {
    let report: Value = Client::new(Duration::from_secs(10)).get("/sdk-handover")?;
    if json {
        println!("{}", serde_json::to_string_pretty(&crate::version::with_warning(report))?);
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

fn step(step: &str) -> &'static str {
    match step {
        "planned" => tr!("its next input is kept for it", "su próxima entrada está guardada para él"),
        "bg_stopped" => tr!("its claude --bg process stopped", "su proceso claude --bg ya paró"),
        _ => tr!("the Agent SDK took its session", "el Agent SDK ya tomó su sesión"),
    }
}

fn reason(row: &Value) -> String {
    let detail = row["reason"]["detail"].as_str().unwrap_or_default();
    match row["reason"]["code"].as_str().unwrap_or_default() {
        "next_message" => tr!("with its next message, which opens its same session", "con su próximo mensaje, que abre su misma sesión").to_owned(),
        "message_waiting" => tr!("now: a message waits for it", "ahora: le espera un mensaje").to_owned(),
        "back_from_pause" => tr!("when it comes back from its pause", "cuando vuelva de su pausa").to_owned(),
        "back_from_lost_session" => tr!("when Alondra brings it back: its session was lost", "cuando Alondra lo traiga de vuelta: su sesión se perdió").to_owned(),
        "moving" => {
            let step = step(detail);
            trf!("moving: {step}", "pasando: {step}")
        }
        "in_turn" => tr!("it is in a turn", "está en un turno").to_owned(),
        "starting" => tr!("it is starting", "está arrancando").to_owned(),
        "background_work" => trf!("its work in the background goes on: {detail}", "sigue su trabajo en segundo plano: {detail}"),
        "answer_in_turn" => tr!("it waits for your answer inside its turn", "espera tu respuesta dentro de su turno").to_owned(),
        "turn_after_delivery" => tr!("its turn goes on after it delivered", "su turno sigue tras entregar").to_owned(),
        "message_on_its_way" => tr!("a message reached it that it has not taken yet", "le llegó un mensaje que aún no ha tomado").to_owned(),
        "after_merge" => trf!("it works after its PR merged: {detail}", "trabaja tras el merge de su PR: {detail}"),
        "delivery_waiting" => tr!("its delivery waits for your decision; it moves if you send it back with changes", "su entrega espera tu decisión; pasa si la devuelves con cambios").to_owned(),
        "through_sdk" => tr!("it already runs through the Agent SDK", "ya corre por el Agent SDK").to_owned(),
        "no_node" => tr!("no node: its next one starts through the Agent SDK", "sin nodo: el siguiente arranca por el Agent SDK").to_owned(),
        "closing" => tr!("its task is closing; its node closes with it", "su tarea se está cerrando; su nodo se cierra con ella").to_owned(),
        _ => tr!("its session is closed", "su sesión está cerrada").to_owned(),
    }
}

pub(crate) fn render(report: &Value) -> String {
    let rows = report["tasks"].as_array().cloned().unwrap_or_default();
    let mut out = vec![if report["sdk_backend"].as_bool().unwrap_or(false) {
        tr!("What the move to the Agent SDK does with each task in progress (nothing changes now):", "Lo que hace el paso al Agent SDK con cada tarea en curso (ahora no cambia nada):").to_owned()
    } else {
        tr!("This daemon has no SDK backend, so nothing moves. With one, each task in progress would:", "Este daemon no tiene backend del SDK, así que nada pasa. Con uno, cada tarea en curso:").to_owned()
    }];
    if rows.is_empty() {
        out.push(tr!("No task in progress.", "Ninguna tarea en curso.").to_owned());
    }
    for (verdict, title) in [
        ("passes", tr!("Moves to the Agent SDK:", "Pasa al Agent SDK:")),
        ("waits", tr!("Waits until its node stops:", "Espera a que su nodo se detenga:")),
        ("stays", tr!("Stays as it is:", "Se queda como está:")),
    ] {
        let group: Vec<&Value> = rows.iter().filter(|row| row["verdict"] == verdict).collect();
        if group.is_empty() {
            continue;
        }
        out.push(title.to_owned());
        out.extend(group.iter().map(|row| {
            let node = row["node"].as_str().map(|node| format!(" · {node}")).unwrap_or_default();
            format!("  {} ({}){node}: {}", row["title"].as_str().unwrap_or_default(), row["task"].as_str().unwrap_or_default(), reason(row))
        }));
    }
    format!("{}\n", out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::views::Lang;
    use serde_json::json;

    #[test]
    fn each_task_is_listed_under_what_the_move_does_with_it_and_why() {
        crate::i18n::set_for_this_thread(Lang::En);
        let report = json!({"sdk_backend": true, "tasks": [
            {"task": "t-a", "title": "Resting", "node": "a", "verdict": "passes", "reason": {"code": "next_message"}},
            {"task": "t-b", "title": "Building", "node": "b", "verdict": "waits", "reason": {"code": "background_work", "detail": "make build"}},
            {"task": "t-c", "title": "Queued", "node": null, "verdict": "stays", "reason": {"code": "no_node"}},
        ]});
        let text = render(&report);
        let (moves, waits, stays) = (text.find("Moves to").unwrap(), text.find("Waits until").unwrap(), text.find("Stays as").unwrap());
        assert!(moves < waits && waits < stays, "{text}");
        assert!(text.contains("  Resting (t-a) · a: with its next message"), "{text}");
        assert!(text.contains("  Building (t-b) · b: its work in the background goes on: make build"), "{text}");
        assert!(text.contains("  Queued (t-c): no node"), "{text}");
    }

    #[test]
    fn a_daemon_without_an_sdk_backend_says_nothing_moves() {
        crate::i18n::set_for_this_thread(Lang::En);
        let text = render(&json!({"sdk_backend": false, "tasks": []}));
        assert!(text.contains("no SDK backend, so nothing moves") && text.contains("No task in progress."), "{text}");
        crate::i18n::set_for_this_thread(Lang::Es);
        let text = render(&json!({"sdk_backend": true, "tasks": [{"task": "t-a", "title": "Notas", "node": "a", "verdict": "waits", "reason": {"code": "in_turn"}}]}));
        assert!(text.contains("Espera a que su nodo se detenga:\n  Notas (t-a) · a: está en un turno"), "{text}");
    }
}
