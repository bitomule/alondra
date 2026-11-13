//! What a task's native Claude asks the user through the SDK host: a command's permission, a
//! question and a plan; the user interrupting a turn from its task; a command that runs for seconds and ends its
//! turn; and a node's command that Alondra's node guard refuses without asking anyone. The model
//! double asks for each tool call when a task's prompt names it (`sdk/model-tools`), and logs the
//! result Claude sent back. A plan goes through plan mode as the native Claude runs it: the model
//! enters plan mode, writes its plan to the plan file plan mode names, and asks to leave it; Claude
//! Code then asks with that file's plan.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::{hold_card, model_requests};
use crate::journey::{Report, User, steps::*, user::{collect, mentions}};
use crate::{fake::Fakes, sandbox::Sandbox};

const PERMISSION: &str = "Prove the permission card";
const QUESTION: &str = "Ask which format the notes use";
const PLAN: &str = "Plan the notes format change";
const PLAN_TEXT: &str = "# Notes format\n\n- [ ] Write the notes as plain text\n- [ ] Check the notes render";
const INTERRUPT: &str = "Wait for the slow check";
const SECONDS: &str = "Run the four-second check";
const GUARD: &str = "List the paired devices";
const ANSWER: &str = "Plain text";
/// The command S8 interrupts: it would run for minutes and leave its proof once it finished.
const SLOW: &str = "sleep 617";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes, user: &User) {
    let tmp = sandbox.dir.join("sdk/tmp");
    report.case("S5", "Claude's command waits on a permission card; «Allow» from the phone runs it", |report| {
        let proof = tmp.join("permission-proof");
        script_tool(sandbox, PERMISSION, "toolu_s5_bash", "Bash", json!({"command": format!("echo allowed > {}", proof.display()), "description": "Write the permission proof"}))?;
        request(report, fakes, user, &task_script("req-s5", "t-s5", PERMISSION), PERMISSION)?;
        let card = wait(report, "Claude's command waits on its permission card in Inbox", || hold_card(user, "decision-t-s5-"))?;
        let view = user.view(&format!("card:{card}"))?;
        report.snapshot("card-permission", &view)?;
        report.step("the card offers «Allow» and «Deny», and nothing ran before the user chose", || {
            for option in ["allow", "deny"] {
                ensure!(option_action(&view, option).is_some(), "no «{option}» on the card: {view}");
            }
            ensure!(!proof.exists(), "{} exists before the user allowed the command", proof.display());
            Ok(())
        })?;
        choose(report, user, &view, "allow", "the user presses «Allow»")?;
        let result = wait(report, "the command ran and its output reached the model", || {
            let result = tool_result(sandbox, "toolu_s5_bash")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("Claude ran the command it asked for", || {
            ensure!(result["is_error"] != true, "the command failed: {result}");
            ensure!(std::fs::read_to_string(&proof).is_ok_and(|text| text.trim() == "allowed"), "{} was not written", proof.display());
            Ok(())
        })?;
        turn_ended(report, user, "t-s5", &card)
    });
    report.case("S6", "Claude's question waits on its card; the option the user picks reaches Claude", |report| {
        script_tool(sandbox, QUESTION, "toolu_s6_ask", "AskUserQuestion", json!({"questions": [{
            "question": "Which format should the notes use?", "header": "Format", "multiSelect": false,
            "options": [{"label": "Markdown", "description": "Headings and lists"}, {"label": ANSWER, "description": "No markup at all"}],
        }]}))?;
        request(report, fakes, user, &task_script("req-s6", "t-s6", QUESTION), QUESTION)?;
        let card = wait(report, "Claude's question waits on its card in Inbox", || hold_card(user, "decision-t-s6-"))?;
        let view = user.view(&format!("card:{card}"))?;
        report.snapshot("card-question", &view)?;
        let option = user.action_labeled(&view, ANSWER).with_context(|| format!("the card offers no «{ANSWER}»: {view}"))?;
        report.step(&format!("the user picks «{ANSWER}»"), || user.press(option["id"].as_str().unwrap_or_default(), option["option_id"].as_str(), None))?;
        let result = wait(report, "the answer reaches Claude as the question's result", || {
            let result = tool_result(sandbox, "toolu_s6_ask")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step(&format!("Claude got «{ANSWER}» for its question"), || {
            ensure!(result["is_error"] != true && result["text"].as_str().is_some_and(|text| text.contains(ANSWER)), "{result}");
            Ok(())
        })?;
        turn_ended(report, user, "t-s6", &card)
    });
    report.case("S7", "Claude's plan waits on its Review card; «Approve» from the phone lets Claude go on with it", |report| {
        script_tool(sandbox, PLAN, "toolu_s7_1_enter", "EnterPlanMode", json!({}))?;
        script_tool(sandbox, PLAN, "toolu_s7_2_write", "Write", json!({"file_path": "{plan_file}", "content": PLAN_TEXT}))?;
        script_tool(sandbox, PLAN, "toolu_s7_3_exit", "ExitPlanMode", json!({}))?;
        request(report, fakes, user, &task_script("req-s7", "t-s7", PLAN), PLAN)?;
        let card = wait(report, "Claude's plan waits on its Review card in Inbox", || hold_card(user, "decision-t-s7-"))?;
        let view = user.view(&format!("card:{card}"))?;
        report.snapshot("card-plan", &view)?;
        report.step("the Review card shows the plan Claude wrote, and Claude was not told anything yet", || {
            ensure!(mentions(&view, "Notes format") && mentions(&view, "2 steps"), "the card does not show the plan: {view}");
            ensure!(tool_result(sandbox, "toolu_s7_3_exit")?.is_none(), "Claude got an answer before the user chose");
            Ok(())
        })?;
        let approve = user.action(&view, "accept:").with_context(|| format!("the card offers no «Approve»: {view}"))?;
        report.step("the user presses «Approve»", || user.press(approve["id"].as_str().unwrap_or_default(), None, None))?;
        let result = wait(report, "the approval reaches Claude as the result of leaving plan mode", || {
            let result = tool_result(sandbox, "toolu_s7_3_exit")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("Claude goes on with the plan it wrote, approved", || {
            ensure!(result["is_error"] != true && result["text"].as_str().is_some_and(|text| text.contains("approved your plan") && text.contains("- [ ] Check the notes render")), "{result}");
            Ok(())
        })?;
        turn_ended(report, user, "t-s7", &card)
    });
    report.case("S8", "The user interrupts a node from its task while its command runs; the turn ends with one card", |report| {
        let proof = tmp.join("slow-proof");
        script_tool(sandbox, INTERRUPT, "toolu_s8_bash", "Bash", json!({"command": format!("{SLOW} && echo finished > {}", proof.display()), "description": "Wait for the slow check"}))?;
        request(report, fakes, user, &task_script("req-s8", "t-s8", INTERRUPT), INTERRUPT)?;
        let card = wait(report, "Claude's slow command waits on its permission card", || hold_card(user, "decision-t-s8-"))?;
        let view = user.view(&format!("card:{card}"))?;
        choose(report, user, &view, "allow", "the user allows the slow command")?;
        wait(report, "the slow command runs", || Ok((running(SLOW)?.then_some(()), "not running".into())))?;
        wait(report, "Claude Code tracks the running command as its work", || Ok((tracked(sandbox, "t-s8", "toolu_s8_bash")?.then_some(()), "not tracked yet".into())))?;
        report.snapshot("task-working", &user.view("task:t-s8")?)?;
        let interrupt = interrupt_of(user, "t-s8")?.context("the task offers no «Interrupt» on its working node")?;
        report.step("the user presses «Interrupt» on the task", || user.press(interrupt["id"].as_str().unwrap_or_default(), None, None))?;
        let ended = wait(report, "the turn ends with a card that asks how to go on", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().find(|id| id.starts_with("turn-ended-t-s8-")).cloned(), format!("{inbox:?}")))
        })?;
        report.snapshot("card-interrupted", &user.view(&format!("card:{ended}"))?)?;
        report.step("one card of the task is left and the node no longer offers «Interrupt»", || {
            let inbox = user.inbox()?;
            let cards: Vec<&String> = inbox.iter().filter(|id| id.contains("t-s8-")).collect();
            ensure!(cards.len() == 1, "{cards:?}");
            ensure!(interrupt_of(user, "t-s8")?.is_none(), "the task still offers «Interrupt»");
            Ok(())
        })?;
        report.step("the slow command was stopped before it finished", || {
            ensure!(!running(SLOW)?, "{SLOW} still runs");
            ensure!(!proof.exists(), "{} exists: the command finished", proof.display());
            Ok(())
        })?;
        report.snapshot("task-interrupted", &user.view("task:t-s8")?)
    });
    report.case("S9", "A node's command that acts as the user is refused by the node guard, with no card", |report| {
        script_tool(sandbox, GUARD, "toolu_s9_bash", "Bash", json!({"command": guarded_command(), "description": "List the paired devices"}))?;
        request(report, fakes, user, &task_script("req-s9", "t-s9", GUARD), GUARD)?;
        let result = wait(report, "the refusal reaches Claude as the command's result", || {
            let result = tool_result(sandbox, "toolu_s9_bash")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("Claude got the node guard's reason", || {
            ensure!(result["is_error"] == true && result["text"].as_str().is_some_and(|text| text.contains("they act as the user")), "{result}");
            Ok(())
        })?;
        wait(report, "the turn ends with its own card", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().any(|id| id.starts_with("turn-ended-t-s9-")).then_some(()), format!("{inbox:?}")))
        })?;
        report.step("no permission card asked the user", || {
            let inbox = user.inbox()?;
            ensure!(!inbox.iter().any(|id| id.starts_with("decision-t-s9-")), "{inbox:?}");
            Ok(())
        })
    });
    report.case("S10", "A command that runs for seconds finishes, and the turn ends with one card", |report| {
        let proof = tmp.join("seconds-proof");
        script_tool(sandbox, SECONDS, "toolu_s10_bash", "Bash", json!({"command": format!("sleep 4 && echo finished > {}", proof.display()), "description": "Run the four-second check"}))?;
        request(report, fakes, user, &task_script("req-s10", "t-s10", SECONDS), SECONDS)?;
        let card = wait(report, "the command waits on its permission card", || hold_card(user, "decision-t-s10-"))?;
        let view = user.view(&format!("card:{card}"))?;
        choose(report, user, &view, "allow", "the user allows the command")?;
        wait(report, "the command finishes and its output reaches the model", || {
            let result = tool_result(sandbox, "toolu_s10_bash")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("Claude Code tracked the command as its work while it ran", || {
            ensure!(tracked(sandbox, "t-s10", "toolu_s10_bash")?, "no task_started for the command");
            Ok(())
        })?;
        turn_ended(report, user, "t-s10", &card)?;
        report.step("the node is no longer working", || {
            ensure!(interrupt_of(user, "t-s10")?.is_none(), "the task still offers «Interrupt»");
            Ok(())
        })
    });
}

/// A user-only subcommand of Alondra's CLI, which a node must never run.
fn guarded_command() -> String {
    ["alondra", "device", "list"].join(" ")
}

/// The model double answers a task's prompt that holds `when` with this tool call.
pub(super) fn script_tool(sandbox: &Sandbox, when: &str, id: &str, name: &str, input: Value) -> Result<()> {
    let folder = sandbox.dir.join("sdk/model-tools");
    std::fs::create_dir_all(&folder).with_context(|| format!("creating {}", folder.display()))?;
    let path = folder.join(format!("{id}.json"));
    std::fs::write(&path, json!({"when": when, "id": id, "name": name, "input": input}).to_string()).with_context(|| format!("writing {}", path.display()))
}

pub(super) fn script_tool_after(sandbox: &Sandbox, after: &str, id: &str, name: &str, input: Value) -> Result<()> {
    script_tool(sandbox, "", id, name, input)?;
    let path = sandbox.dir.join(format!("sdk/model-tools/{id}.json"));
    let mut call: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
    call["after_tool_result"] = json!(after);
    std::fs::write(path, call.to_string())?;
    Ok(())
}

/// What the model got back for its tool call `id`.
pub(super) fn tool_result(sandbox: &Sandbox, id: &str) -> Result<Option<Value>> {
    Ok(model_requests(sandbox)?.into_iter()
        .flat_map(|request| request["results"].as_array().cloned().unwrap_or_default())
        .find(|result| result["tool_use_id"] == id))
}

/// The enabled action of `view` that picks `option`.
pub(super) fn option_action(view: &Value, option: &str) -> Option<Value> {
    let mut found = vec![];
    collect(view, &mut |item| {
        item.get("option_id").and_then(Value::as_str) == Some(option) && item.get("id").is_some() && item.get("enabled") != Some(&Value::Bool(false))
    }, &mut found);
    found.into_iter().next()
}

pub(super) fn choose(report: &mut Report, user: &User, view: &Value, option: &str, what: &str) -> Result<()> {
    let action = option_action(view, option).with_context(|| format!("the card offers no «{option}»: {view}"))?;
    report.step(what, || user.press(action["id"].as_str().unwrap_or_default(), Some(option), None)).map(drop)
}

/// The decision card left Inbox and the turn ended with the card that asks how to go on.
pub(super) fn turn_ended(report: &mut Report, user: &User, task: &str, card: &str) -> Result<()> {
    let prefix = format!("turn-ended-{task}-");
    wait(report, "the turn ends: the decision card is gone and the turn's card asks how to go on", || {
        let inbox = user.inbox()?;
        Ok((((inbox.iter().any(|id| id.starts_with(&prefix))) && !inbox.iter().any(|id| id == card)).then_some(()), format!("{inbox:?}")))
    })
}

/// Whether Claude Code reported the command of `tool_use` as work it tracks (`task_started`), as
/// the task's host kept it.
fn tracked(sandbox: &Sandbox, task: &str, tool_use: &str) -> Result<bool> {
    let tool_use = format!("\"tool_use_id\":\"{tool_use}\"");
    Ok(super::host_events(sandbox, task)?.iter().map(Value::to_string).any(|event| event.contains("\"subtype\":\"task_started\"") && event.contains(&tool_use)))
}

/// Whether a process runs exactly `command`.
fn running(command: &str) -> Result<bool> {
    let output = std::process::Command::new("ps").args(["-axo", "command="]).output().context("ps")?;
    Ok(String::from_utf8_lossy(&output.stdout).lines().any(|line| line.trim() == command))
}
