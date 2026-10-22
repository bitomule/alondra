//! What a task's detail shows of its node of the Agent SDK while it works: its steps, the call it
//! runs, what it and its subagent do right now, its shell and subagent in the background, what it
//! said last, its context and its cost. The phone allows each of its commands. Alondra reads all
//! of it from what the SDK host observed; the node's plugin, quiet under the SDK, reports nothing.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::decisions::{option_action, script_tool};
use crate::journey::{Report, User, steps::*, user::mentions};
use crate::{fake::Fakes, sandbox::Sandbox};

const WHEN: &str = "Show the live readings";
/// The shell the node leaves in the background, and how Claude describes it; S14 stops it when it
/// is done.
const SHELL: &str = "sleep 913";
const SHELL_ROW: &str = "Wait in the background";
/// The call the node runs in the foreground while the detail is read.
const CALL: &str = "sleep 40";
const SUBAGENT_WHEN: &str = "Check the links slowly";
const SUBAGENT: &str = "Check the links";
/// The call its subagent runs meanwhile.
const SUBAGENT_CALL: &str = "sleep 45";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes, user: &User) {
    report.case("S14", "A task's detail shows what its node of the Agent SDK plans, runs, runs in the background, said and cost", |report| {
        script_tool(sandbox, WHEN, "toolu_s14_1_create", "TaskCreate", json!({"subject": "Write the changelog", "description": "List what changed", "activeForm": "Writing the changelog"}))?;
        script_tool(sandbox, WHEN, "toolu_s14_2_create", "TaskCreate", json!({"subject": "Check the links", "description": "Open every link", "activeForm": "Checking the links"}))?;
        script_tool(sandbox, WHEN, "toolu_s14_3_update", "TaskUpdate", json!({"taskId": "1", "status": "in_progress"}))?;
        script_tool(sandbox, WHEN, "toolu_s14_4_shell", "Bash", json!({"command": SHELL, "description": SHELL_ROW, "run_in_background": true}))?;
        script_tool(sandbox, WHEN, "toolu_s14_5_agent", "Agent", json!({"description": SUBAGENT, "subagent_type": "general-purpose",
            "prompt": format!("{SUBAGENT_WHEN}: run the check and report back."), "run_in_background": true}))?;
        script_tool(sandbox, SUBAGENT_WHEN, "toolu_s14_6_check", "Bash", json!({"command": SUBAGENT_CALL, "description": "Check the links one by one"}))?;
        script_tool(sandbox, WHEN, "toolu_s14_7_call", "Bash", json!({"command": CALL, "description": "Wait for the check"}))?;
        request(report, fakes, user, &task_script("req-s14", "t-s14", WHEN), WHEN)?;

        let detail = wait(report, "while its call runs, the task's detail shows it, its steps, its subagent's call and its shell in the background", || {
            allow_every_command(user)?;
            let Ok(detail) = user.view("task:t-s14") else { return Ok((None, "the orchestrator has not created the task yet".into())) };
            let shown = mentions(&detail, &format!("Bash: {CALL}"))
                && section(&detail, "steps").is_some_and(|steps| mentions(&steps, "Check the links"))
                && section(&detail, "background").is_some_and(|background| mentions(&background, SHELL_ROW))
                && section(&detail, "activity").is_some_and(|now| mentions(&now, SUBAGENT_CALL));
            Ok((shown.then_some(detail.clone()), detail.to_string()))
        })?;
        report.snapshot("task-running-a-call", &detail)?;
        report.step("its steps are the node's task list, the one it works on first", || {
            let steps = section(&detail, "steps").context("no «Steps» section")?;
            ensure!(mentions(&steps, "Writing the changelog") && mentions(&steps, "Check the links"), "{steps}");
            Ok(())
        })?;
        report.step("«Right now» shows the node's call and its subagent's, and the readings count the shell and the subagent", || {
            let now = section(&detail, "activity").context("no «Right now» section")?;
            ensure!(mentions(&now, CALL) && mentions(&now, SUBAGENT_CALL), "{now}");
            let subagents = section(&detail, "subagents").context("no «Subagents» section")?;
            ensure!(mentions(&subagents, SUBAGENT), "{subagents}");
            ensure!(mentions(&detail, "1 subagent · 1 shell"), "the readings do not count them: {detail}");
            Ok(())
        })?;

        let detail = wait(report, "once the turn ends the detail shows what the node said last, its context and its cost", || {
            allow_every_command(user)?;
            let detail = user.view("task:t-s14")?;
            let shown = section(&detail, "last_message").is_some() && mentions(&detail, "Context") && mentions(&detail, "Cost");
            Ok((shown.then_some(detail.clone()), detail.to_string()))
        })?;
        report.snapshot("task-after-turn", &detail)?;
        report.step("with its turn over, «Right now» no longer shows the node's own call", || {
            ensure!(section(&detail, "activity").is_none_or(|now| !mentions(&now, CALL)), "{detail}");
            Ok(())
        })?;
        wait(report, "once its subagent ends, nothing runs right now", || {
            let detail = user.view("task:t-s14")?;
            Ok((section(&detail, "activity").is_none().then_some(()), detail.to_string()))
        })?;
        report.step("the background shell stops", || {
            let stopped = std::process::Command::new("pkill").args(["-f", &format!("^{SHELL}$")]).status().context("pkill")?;
            ensure!(stopped.success(), "{SHELL} was not running");
            Ok(())
        })?;
        wait(report, "the detail no longer shows the shell", || {
            let detail = user.view("task:t-s14")?;
            let gone = section(&detail, "background").is_none_or(|background| !mentions(&background, SHELL_ROW));
            Ok((gone.then_some(()), detail.to_string()))
        })
    });
}

/// The phone allows each command of the task that waits on its permission card: the user's
/// settings ask before every `sleep`.
fn allow_every_command(user: &User) -> Result<()> {
    for card in user.inbox()?.into_iter().filter(|id| id.starts_with("decision-t-s14-")) {
        let view = user.view(&format!("card:{card}"))?;
        if let Some(allow) = option_action(&view, "allow") {
            user.press(allow["id"].as_str().unwrap_or_default(), Some("allow"), None)?;
        }
    }
    Ok(())
}

/// The section of a view with this id.
fn section(view: &Value, id: &str) -> Option<Value> {
    view["sections"].as_array()?.iter().find(|section| section["id"] == id).cloned()
}
