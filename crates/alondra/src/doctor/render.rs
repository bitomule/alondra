use super::{Group, Outcome, Status};

pub const GROUPS: [Group; 4] = [Group::Service, Group::Claude, Group::Devices, Group::Tools];

pub fn group_name(group: Group) -> &'static str {
    match group {
        Group::Service => tr!("Service", "Servicio"),
        Group::Claude => "Claude",
        Group::Devices => tr!("Devices", "Dispositivos"),
        Group::Tools => tr!("Tools", "Herramientas"),
    }
}

pub fn mark(status: Status) -> &'static str {
    match status {
        Status::Pass => "✓",
        Status::Warn => "!",
        Status::Fail => "✗",
    }
}

pub fn summary(outcomes: &[Outcome]) -> String {
    let fails = outcomes.iter().filter(|o| o.status == Status::Fail).count();
    let warns = outcomes.iter().filter(|o| o.status == Status::Warn).count();
    let warnings = match warns {
        1 => tr!("1 warning", "1 aviso").to_string(),
        n => trf!("{n} warnings", "{n} avisos"),
    };
    match (fails, warns) {
        (0, 0) => tr!("Nothing is broken.", "Nada está roto.").to_string(),
        (0, _) => trf!("{warnings}. Nothing is broken.", "{warnings}. Nada está roto."),
        (1, 0) => tr!("1 problem.", "1 problema.").to_string(),
        (n, 0) => trf!("{n} problems.", "{n} problemas."),
        (1, _) => trf!("1 problem, {warnings}.", "1 problema, {warnings}."),
        (n, _) => trf!("{n} problems, {warnings}.", "{n} problemas, {warnings}."),
    }
}

/// `alondra doctor`: a header, the checks by group with their fix under them, and a summary.
/// tmux and tuicr share one line while both pass.
pub fn grouped(header: &str, outcomes: &[Outcome]) -> Vec<String> {
    let tools_pass = outcomes.iter().filter(|o| o.group == Group::Tools).all(|o| o.status == Status::Pass);
    let joined = outcomes.iter().filter(|o| o.group == Group::Tools).map(|o| o.name).collect::<Vec<_>>().join(" · ");
    let width = outcomes.iter().filter(|o| o.group != Group::Tools || !tools_pass).map(|o| o.name.chars().count()).max().unwrap_or(0);
    let mut out = vec![header.to_string(), String::new()];
    for group in GROUPS {
        let members: Vec<&Outcome> = outcomes.iter().filter(|o| o.group == group).collect();
        if members.is_empty() {
            continue;
        }
        out.push(group_name(group).to_string());
        if group == Group::Tools && tools_pass {
            out.push(format!("  ✓ {joined}"));
            continue;
        }
        for o in members {
            out.push(format!("  {} {:<width$}  {}", mark(o.status), o.name, o.detail).trim_end().to_string());
            if o.status != Status::Pass && let Some(fix) = o.fix.as_ref().map(|f| f.text()).filter(|f| !f.is_empty()) {
                out.push(format!("  {:<width$}    → {fix}", ""));
            }
        }
    }
    out.push(String::new());
    out.push(summary(outcomes));
    out
}
