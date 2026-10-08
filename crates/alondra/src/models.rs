//! `alondra models`: how each model the orchestrator chose did, to tune the model policy.
use std::time::Duration;

use alondra_core::model_report::{Attempt, Group, ModelReport, Outcome};
use anyhow::{Result, bail};

use crate::client::Client;

pub fn run(since: Option<&str>, json: bool) -> Result<()> {
    let since = since.map(window).transpose()?;
    let path = match since {
        Some(window) => format!("/models?since={}", alondra_core::now_millis() - window),
        None => "/models".to_owned(),
    };
    let report: ModelReport = Client::new(Duration::from_secs(10)).get(&path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        print!("{}", render(&report, since));
    }
    Ok(())
}

/// `7d`, `12h` or `2w` as milliseconds.
fn window(text: &str) -> Result<i64> {
    let text = text.trim();
    let cut = text.char_indices().last().map_or(0, |(at, _)| at);
    let (number, unit) = text.split_at(cut);
    let hours = match unit {
        "h" => Some(1),
        "d" => Some(24),
        "w" => Some(24 * 7),
        _ => None,
    };
    match (hours, number.parse::<i64>()) {
        (Some(hours), Ok(n)) if n > 0 => Ok(n * hours * 3_600_000),
        _ => bail!(trf!("{text} is not a period: use a number with h, d or w, like 7d", "{text} no es un periodo: usa un número con h, d o w, como 7d")),
    }
}

fn duration(ms: i64) -> String {
    let minutes = ms / 60_000;
    if minutes < 90 {
        format!("{minutes} min")
    } else {
        format!("{:.1} h", ms as f64 / 3_600_000.0)
    }
}

fn group_line(name: &str, g: &Group) -> String {
    let (tasks, clean, reopened, relaunched, cancelled, questions, changes) =
        (g.tasks, g.percent(g.done_clean), g.percent(g.reopened), g.percent(g.relaunched), g.percent(g.cancelled), g.questions, g.changes_requested);
    let mut line = trf!(
        "  {name}: {tasks} tasks · {clean}% done clean · {reopened}% reopened · {relaunched}% relaunched",
        "  {name}: {tasks} tareas · {clean}% terminadas a la primera · {reopened}% reabiertas · {relaunched}% relanzadas"
    );
    if !g.relaunched_to.is_empty() {
        let moves: Vec<String> = g.relaunched_to.iter().map(|(model, n)| format!("→ {model} {n}")).collect();
        line.push_str(&format!(" ({})", moves.join(", ")));
    }
    line.push_str(&trf!(
        " · {cancelled}% cancelled · {questions} questions to you · {changes} with changes requested",
        " · {cancelled}% descartadas · {questions} preguntas para ti · {changes} con cambios pedidos"
    ));
    let (rounds, mean, by_reviewer, serious) = (g.change_rounds, g.mean_change_rounds(), g.reviewer_rounds, g.serious_findings);
    line.push_str(&trf!(
        " · {rounds} rounds of changes (mean {mean:.1}, {by_reviewer} from a reviewer) · {serious} serious findings",
        " · {rounds} rondas de cambios (media {mean:.1}, {by_reviewer} del revisor) · {serious} fallos graves"
    ));
    if let Some(median) = g.median_ms_to_done.map(duration) {
        line.push_str(&trf!(" · median {median} to done", " · mediana {median} hasta terminar"));
    }
    if let Some(cost) = g.mean_cost_usd {
        let costed = g.costed;
        line.push_str(&trf!(" · mean ${cost:.2} ({costed} measured)", " · media {cost:.2} $ ({costed} medidas)"));
    }
    line
}

fn outcome(o: Outcome) -> &'static str {
    match o {
        Outcome::Reopened => tr!("reopened", "reabierta"),
        Outcome::Relaunched => tr!("relaunched", "relanzada"),
        Outcome::Cancelled => tr!("cancelled", "descartada"),
        Outcome::ChangesRequested => tr!("changes requested", "cambios pedidos"),
    }
}

fn trouble_lines(a: &Attempt) -> Vec<String> {
    let outcomes: Vec<String> = a.outcomes.iter().map(|o| match (o, &a.relaunched_to) {
        (Outcome::Relaunched, Some(to)) => format!("{} → {to}", outcome(*o)),
        _ => outcome(*o).to_owned(),
    }).collect();
    let effort = a.effort.as_deref().map(|e| format!(" ({e})")).unwrap_or_default();
    let mut lines = vec![format!("  {}  {}{effort} — {}", a.task_id, a.title, outcomes.join(", "))];
    if a.change_rounds > 0 || a.serious_findings > 0 {
        let (rounds, serious) = (a.change_rounds, a.serious_findings);
        lines.push(trf!("      {rounds} rounds of changes · {serious} serious findings", "      {rounds} rondas de cambios · {serious} fallos graves"));
    }
    let reason = a.reason.as_deref().unwrap_or(tr!("no reason recorded", "sin motivo registrado"));
    lines.push(trf!("      why: {reason}", "      por qué: {reason}"));
    lines
}

pub fn render(report: &ModelReport, window: Option<i64>) -> String {
    let mut out = vec![match window {
        Some(ms) => {
            let days = (ms as f64 / 86_400_000.0 * 10.0).round() / 10.0;
            trf!("Model decisions in the last {days} days", "Decisiones de modelo de los últimos {days} días")
        }
        None => tr!("Every recorded model decision", "Todas las decisiones de modelo registradas").to_owned(),
    }];
    if report.by_model.is_empty() {
        out.push(tr!("No launches recorded yet.", "Aún no hay lanzamientos registrados.").to_owned());
        return out.join("\n") + "\n";
    }
    let unknown = tr!("unknown", "desconocido");
    out.push(String::new());
    out.push(tr!("By model and effort:", "Por modelo y esfuerzo:").to_owned());
    for g in &report.by_model {
        let name = format!("{} · {}", g.model.as_deref().unwrap_or(unknown), g.effort.as_deref().unwrap_or(tr!("default effort", "esfuerzo por defecto")));
        out.push(group_line(&name, g));
    }
    out.push(String::new());
    out.push(tr!("By approach:", "Por enfoque:").to_owned());
    for g in &report.by_approach {
        out.push(group_line(g.approach.as_deref().unwrap_or(unknown), g));
    }
    if !report.by_role.is_empty() {
        out.push(String::new());
        out.push(tr!("By role:", "Por rol:").to_owned());
        for g in &report.by_role {
            let role = match g.role.as_deref() {
                Some("design") => tr!("design", "diseño"),
                Some("implementation") => tr!("implementation", "implementación"),
                Some("review") => tr!("review", "revisión"),
                _ => unknown,
            };
            out.push(group_line(role, g));
        }
    }
    out.push(String::new());
    if report.sonnet_trouble.is_empty() {
        out.push(tr!("No Sonnet task went badly.", "Ninguna tarea con Sonnet fue mal.").to_owned());
    } else {
        out.push(tr!("Sonnet tasks that went badly:", "Tareas con Sonnet que fueron mal:").to_owned());
        out.extend(report.sonnet_trouble.iter().flat_map(trouble_lines));
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod tests {
    use super::*;
    use alondra_core::views::Lang;

    fn report() -> ModelReport {
        let sonnet = Group {
            model: Some("sonnet".into()), effort: Some("medium".into()), tasks: 4, done_clean: 2, reopened: 1, relaunched: 1,
            relaunched_to: [("opus".to_owned(), 1)].into(), cancelled: 1, questions: 3, changes_requested: 1,
            change_rounds: 6, reviewer_rounds: 4, serious_findings: 2,
            median_ms_to_done: Some(42 * 60_000), mean_cost_usd: Some(1.5), costed: 2, ..Group::default()
        };
        ModelReport {
            since: None, until: 0,
            by_model: vec![sonnet.clone()],
            by_approach: vec![Group { model: None, effort: None, approach: Some("solo".into()), ..sonnet.clone() }],
            by_role: vec![Group { model: None, effort: None, role: Some("design".into()), ..sonnet }],
            sonnet_trouble: vec![Attempt { task_id: "t-1".into(), title: "Editor colours".into(), at: 0, model: Some("sonnet".into()),
                effort: Some("medium".into()), reason: Some("small CSS change".into()), outcomes: vec![Outcome::Reopened, Outcome::Relaunched],
                relaunched_to: Some("opus".into()), change_rounds: 3, reviewer_rounds: 2, serious_findings: 2 }],
            attempts: vec![],
        }
    }

    #[test]
    fn the_report_reads_in_the_users_language() {
        crate::i18n::set_for_this_thread(Lang::En);
        let text = render(&report(), Some(7 * 86_400_000));
        for expected in ["last 7 days", "sonnet · medium: 4 tasks · 50% done clean · 25% reopened · 25% relaunched (→ opus 1)",
            "25% cancelled · 3 questions to you · 1 with changes requested · 6 rounds of changes (mean 1.5, 4 from a reviewer) · 2 serious findings · median 42 min to done · mean $1.50 (2 measured)",
            "      3 rounds of changes · 2 serious findings",
            "By approach:", "  solo: 4 tasks", "By role:", "  design: 4 tasks", "t-1  Editor colours (medium) — reopened, relaunched → opus", "why: small CSS change"] {
            assert!(text.contains(expected), "{expected} missing from\n{text}");
        }
        crate::i18n::set_for_this_thread(Lang::Es);
        let text = render(&report(), None);
        for expected in ["Todas las decisiones", "Por rol:", "  diseño: 4 tareas", "4 tareas · 50% terminadas a la primera", "Tareas con Sonnet que fueron mal:", "reabierta, relanzada → opus", "6 rondas de cambios (media 1.5, 4 del revisor) · 2 fallos graves", "3 rondas de cambios · 2 fallos graves", "por qué: small CSS change"] {
            assert!(text.contains(expected), "{expected} missing from\n{text}");
        }
        assert!(render(&ModelReport::default(), None).contains("Aún no hay lanzamientos"));
    }

    #[test]
    fn a_period_is_hours_days_or_weeks() {
        crate::i18n::set_for_this_thread(Lang::En);
        assert_eq!(window("7d").unwrap(), 7 * 86_400_000);
        assert_eq!(window("12h").unwrap(), 12 * 3_600_000);
        assert_eq!(window("2w").unwrap(), 14 * 86_400_000);
        for bad in ["", "d", "0d", "7", "7y", "-1d", "7é"] {
            assert!(window(bad).is_err(), "{bad}");
        }
    }
}
