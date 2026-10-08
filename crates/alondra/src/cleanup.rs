//! `alondra cleanup`: closes the sessions of finished tasks, cutting the turn of the ones stuck
//! working, closes the ones stopped without a task, and frees the tasks of sessions stopped in
//! Claude agents.
use std::time::Duration;

use anyhow::Result;
use serde_json::{Value, json};

use crate::client::Client;

pub fn run(dry_run: bool, json: bool) -> Result<()> {
    let report = Client::for_user(Duration::from_secs(300)).post("/cleanup", &json!({"dry_run": dry_run}))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report));
    }
    Ok(())
}

fn rows(report: &Value, key: &str) -> Vec<Value> {
    report[key].as_array().cloned().unwrap_or_default()
}

fn text<'a>(row: &'a Value, field: &str) -> &'a str {
    row[field].as_str().unwrap_or_default()
}

fn why(code: &str) -> &'static str {
    match code {
        "task_done" => tr!("its task is done", "su tarea está hecha"),
        "task_cancelled" => tr!("its task was cancelled", "su tarea se canceló"),
        _ => tr!("no task, stopped for more than a day", "sin tarea, parada hace más de un día"),
    }
}

fn kept(row: &Value) -> String {
    let detail = text(row, "detail");
    match text(row, "reason") {
        "busy" => tr!("it keeps working even after its turn was cut", "sigue trabajando aunque se le cortó el turno").to_owned(),
        "permission" => tr!("it is waiting for a permission", "espera un permiso").to_owned(),
        _ => trf!("its worktree has work that is not saved: {detail}", "su worktree tiene trabajo sin guardar: {detail}"),
    }
}

pub(crate) fn render(report: &Value) -> String {
    let dry_run = report["dry_run"].as_bool().unwrap_or(false);
    let (closing, left, failed) = (rows(report, if dry_run { "close" } else { "closed" }), rows(report, "kept"), rows(report, "failed"));
    let freeing = rows(report, if dry_run { "free" } else { "freed" });
    if closing.is_empty() && left.is_empty() && failed.is_empty() && freeing.is_empty() {
        return format!("{}\n", tr!("Nothing to close.", "Nada que cerrar."));
    }
    let mut out: Vec<String> = vec![];
    if !closing.is_empty() {
        let n = closing.len();
        out.push(if dry_run {
            trf!("Would close {n} sessions:", "Cerraría {n} sesiones:")
        } else {
            trf!("Closed {n} sessions:", "Cerradas {n} sesiones:")
        });
        out.extend(closing.iter().map(|row| {
            let stuck = if row["stuck"].as_bool().unwrap_or(false) {
                tr!(", stuck working: its turn is cut first", ", atascada trabajando: antes se le corta el turno")
            } else {
                ""
            };
            format!("  {} ({}): {}{stuck}", text(row, "name"), text(row, "short_id"), why(text(row, "why")))
        }));
    }
    if !freeing.is_empty() {
        let n = freeing.len();
        out.push(if dry_run {
            trf!("Would free the tasks of {n} sessions stopped in Claude agents, to relaunch them:", "Liberaría las tareas de {n} sesiones paradas en Claude agents, para relanzarlas:")
        } else {
            trf!("Freed the tasks of {n} sessions stopped in Claude agents, to relaunch them:", "Liberadas las tareas de {n} sesiones paradas en Claude agents, para relanzarlas:")
        });
        out.extend(freeing.iter().map(|row| {
            let tasks: Vec<&str> = row["tasks"].as_array().map(|tasks| tasks.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            format!("  {} ({}): {}", text(row, "name"), text(row, "short_id"), tasks.join(", "))
        }));
    }
    if !left.is_empty() {
        let n = left.len();
        out.push(trf!("Left as they are ({n}):", "Se quedan como están ({n}):"));
        out.extend(left.iter().map(|row| format!("  {} ({}): {}", text(row, "name"), text(row, "short_id"), kept(row))));
    }
    if !failed.is_empty() {
        let n = failed.len();
        out.push(trf!("Could not close ({n}):", "No se pudieron cerrar ({n}):"));
        out.extend(failed.iter().map(|row| format!("  {} ({}): {}", text(row, "name"), text(row, "short_id"), text(row, "error"))));
    }
    if dry_run && !(closing.is_empty() && freeing.is_empty()) {
        out.push(tr!("Run `alondra cleanup` to do it.", "Ejecuta `alondra cleanup` para hacerlo.").to_owned());
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::views::Lang;

    fn report(dry_run: bool) -> Value {
        let session = |name: &str, short: &str, why: &str| json!({"session": format!("{short}-0000"), "short_id": short, "name": name, "why": why, "task": null});
        json!({
            "dry_run": dry_run,
            "close": if dry_run { vec![session("Node 1", "a1", "task_done"), session("Node 2", "a2", "stopped_without_task")] } else { vec![] },
            "free": if dry_run { vec![json!({"session": "d1-0000", "short_id": "d1", "name": "Node 7", "tasks": ["Fix the list"]})] } else { vec![] },
            "closed": if dry_run { vec![] } else { vec![session("Node 1", "a1", "task_cancelled")] },
            "kept": [
                {"session": "b1-0000", "short_id": "b1", "name": "Node 3", "reason": "busy", "detail": null},
                {"session": "b2-0000", "short_id": "b2", "name": "Node 4", "reason": "permission", "detail": null},
                {"session": "b3-0000", "short_id": "b3", "name": "Node 5", "reason": "worktree", "detail": "a.txt is not committed"},
            ],
            "failed": [{"session": "c1-0000", "short_id": "c1", "name": "Node 6", "error": "claude rm said no"}],
        })
    }

    #[test]
    fn the_cleanup_report_reads_well_in_english_and_spanish() {
        crate::i18n::set_for_this_thread(Lang::En);
        let dry = render(&report(true));
        for expected in ["Would close 2 sessions:", "  Node 1 (a1): its task is done", "  Node 2 (a2): no task, stopped for more than a day",
            "Left as they are (3):", "  Node 3 (b1): it keeps working even after its turn was cut",
            "  Node 4 (b2): it is waiting for a permission",
            "Would free the tasks of 1 sessions stopped in Claude agents, to relaunch them:", "  Node 7 (d1): Fix the list",
            "  Node 5 (b3): its worktree has work that is not saved: a.txt is not committed", "Could not close (1):", "  Node 6 (c1): claude rm said no",
            "Run `alondra cleanup` to do it."] {
            assert!(dry.contains(expected), "{expected} missing from\n{dry}");
        }
        let done = render(&report(false));
        assert!(done.contains("Closed 1 sessions:") && done.contains("  Node 1 (a1): its task was cancelled"), "{done}");
        assert!(!done.contains("Run `alondra cleanup`"), "{done}");
        assert_eq!(render(&json!({"dry_run": false, "close": [], "closed": [], "kept": [], "failed": []})), "Nothing to close.\n");

        crate::i18n::set_for_this_thread(Lang::Es);
        let spanish = render(&report(true));
        for expected in ["Cerraría 2 sesiones:", "su tarea está hecha", "sin tarea, parada hace más de un día", "Se quedan como están (3):",
            "sigue trabajando aunque se le cortó el turno", "Liberaría las tareas de 1 sesiones paradas",
            "espera un permiso", "su worktree tiene trabajo sin guardar: a.txt is not committed", "No se pudieron cerrar (1):", "Ejecuta `alondra cleanup` para hacerlo."] {
            assert!(spanish.contains(expected), "{expected} missing from\n{spanish}");
        }
        assert_eq!(render(&json!({"dry_run": true, "close": [], "closed": [], "kept": [], "failed": []})), "Nada que cerrar.\n");
    }
}
