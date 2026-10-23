//! The phone answers what a task's native Claude asks through the SDK host, over the encrypted
//! direct connection: «Allow», «Allow always» and «Deny» reach exactly the request their card
//! shows. An answer built for one request never decides the next one, and an answer to a request
//! Claude no longer waits on reaches nothing.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::decisions::{choose, option_action, script_tool, tool_result};
use super::hold_card;
use crate::journey::{Report, User, steps::*};
use crate::{fake::Fakes, sandbox::Sandbox};

const ALLOW: &str = "Write the allowed proof";
const ALWAYS: &str = "Commit the always proof";
const DENY: &str = "Write the denied proof";
const TWO: &str = "Write the two proofs";
const CANCELLED: &str = "Wait for the cancelled check";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a second phone pairs with Mac 1 directly, with no mailbox", || User::paired(sandbox, "journey-direct", 1, true)) else {
        return;
    };
    let tmp = sandbox.dir.join("sdk/tmp");
    report.case("R1", "«Allow» from the phone over the direct connection runs the command Claude asked for", |report| {
        let proof = tmp.join("relay-allowed");
        script_tool(sandbox, ALLOW, "toolu_r1_bash", "Bash", json!({"command": format!("echo allowed > {}", proof.display()), "description": "Write the allowed proof"}))?;
        request(report, fakes, &user, &task_script("req-r1", "t-r1", ALLOW), ALLOW)?;
        let card = wait(report, "the permission card waits in Inbox", || hold_card(&user, "decision-t-r1-"))?;
        let view = user.view(&format!("card:{card}"))?;
        report.step("the card shows the command Claude asked to run", || {
            ensure!(view.to_string().contains("relay-allowed"), "{view}");
            Ok(())
        })?;
        choose(report, &user, &view, "allow", "the user presses «Allow»")?;
        let result = wait(report, "the command ran and its output reached the model", || {
            let result = tool_result(sandbox, "toolu_r1_bash")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("Claude ran exactly what its card showed", || {
            ensure!(result["is_error"] != true, "{result}");
            ensure!(std::fs::read_to_string(&proof).is_ok_and(|text| text.trim() == "allowed"), "{} was not written", proof.display());
            Ok(())
        })
    });
    report.case("R2", "«Allow always» from the phone runs the command and keeps its rule in the repository's local settings", |report| {
        script_tool(sandbox, ALWAYS, "toolu_r2_bash", "Bash", json!({"command": "git commit --allow-empty -m always-proof", "description": "Commit the always proof"}))?;
        request(report, fakes, &user, &task_script("req-r2", "t-r2", ALWAYS), ALWAYS)?;
        let card = wait(report, "the permission card waits in Inbox", || hold_card(&user, "decision-t-r2-"))?;
        let view = user.view(&format!("card:{card}"))?;
        report.snapshot("card-permission-always", &view)?;
        choose(report, &user, &view, "always", "the user presses «Allow always»")?;
        let result = wait(report, "the command ran and its output reached the model", || {
            let result = tool_result(sandbox, "toolu_r2_bash")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("Claude ran the command it asked for, unrefused", || {
            ensure!(!result["text"].as_str().is_some_and(|text| text.contains("denied") || text.contains("permission")), "{result}");
            Ok(())
        })?;
        wait(report, "the rule is kept in the local settings of the task's repository", || {
            let settings = local_settings(sandbox)?;
            let rules = settings["permissions"]["allow"].as_array().cloned().unwrap_or_default();
            Ok((rules.iter().any(|rule| rule.as_str().is_some_and(|rule| rule.starts_with("Bash(git commit"))).then_some(()), format!("{settings}")))
        })
    });
    report.case("R3", "«Deny» from the phone tells Claude no, and the command never runs", |report| {
        let proof = tmp.join("relay-denied");
        script_tool(sandbox, DENY, "toolu_r3_bash", "Bash", json!({"command": format!("echo denied > {}", proof.display()), "description": "Write the denied proof"}))?;
        request(report, fakes, &user, &task_script("req-r3", "t-r3", DENY), DENY)?;
        let card = wait(report, "the permission card waits in Inbox", || hold_card(&user, "decision-t-r3-"))?;
        let view = user.view(&format!("card:{card}"))?;
        choose(report, &user, &view, "deny", "the user presses «Deny»")?;
        let result = wait(report, "Claude got the denial as the command's result", || {
            let result = tool_result(sandbox, "toolu_r3_bash")?;
            Ok((result.clone(), format!("{result:?}")))
        })?;
        report.step("the denial says the user denied it and nothing ran", || {
            ensure!(result["is_error"] == true && result["text"].as_str().is_some_and(|text| text.contains("The user denied it")), "{result}");
            ensure!(!proof.exists(), "{} was written", proof.display());
            Ok(())
        })
    });
    report.case("R4", "An answer built for Claude's first request never decides its second one", |report| {
        let first_proof = tmp.join("relay-first");
        let second_proof = tmp.join("relay-second");
        script_tool(sandbox, TWO, "toolu_r4_1_first", "Bash", json!({"command": format!("echo first > {}", first_proof.display()), "description": "Write the first proof"}))?;
        script_tool(sandbox, TWO, "toolu_r4_2_second", "Bash", json!({"command": format!("echo second > {}", second_proof.display()), "description": "Write the second proof"}))?;
        request(report, fakes, &user, &task_script("req-r4", "t-r4", TWO), TWO)?;
        let first = wait(report, "the first request's card waits in Inbox", || hold_card(&user, "decision-t-r4-"))?;
        let first_view = user.view(&format!("card:{first}"))?;
        let allow = option_action(&first_view, "allow").with_context(|| format!("no «Allow»: {first_view}"))?;
        choose(report, &user, &first_view, "allow", "the user allows the first command")?;
        let second = wait(report, "the second request asks with a card of its own", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().find(|id| id.starts_with("decision-t-r4-") && **id != first).cloned(), format!("{inbox:?}")))
        })?;
        report.step("the first card's answer, sent again, is refused and the second card still waits", || {
            let (status, body) = user.try_press(allow["id"].as_str().unwrap_or_default(), Some("allow"))?;
            ensure!(body["outcome"] != "applied", "the old answer was applied again: {status} {body}");
            ensure!(user.inbox()?.contains(&second), "the second card no longer waits");
            ensure!(!second_proof.exists(), "the second command ran");
            Ok(())
        })?;
        let second_view = user.view(&format!("card:{second}"))?;
        report.step("the second card shows the second command", || {
            ensure!(second_view.to_string().contains("relay-second"), "{second_view}");
            Ok(())
        })?;
        choose(report, &user, &second_view, "deny", "the user denies the second command")?;
        wait(report, "only the first command ran", || {
            let denied = tool_result(sandbox, "toolu_r4_2_second")?;
            Ok((denied.filter(|_| first_proof.exists() && !second_proof.exists()), "waiting for the second result".into()))
        })
        .map(drop)
    });
    report.case("R5", "An answer to a request Claude stopped waiting on reaches nothing", |report| {
        let proof = tmp.join("relay-cancelled");
        script_tool(sandbox, CANCELLED, "toolu_r5_bash", "Bash", json!({"command": format!("echo late > {}", proof.display()), "description": "Write the late proof"}))?;
        request(report, fakes, &user, &task_script("req-r5", "t-r5", CANCELLED), CANCELLED)?;
        let card = wait(report, "the permission card waits in Inbox", || hold_card(&user, "decision-t-r5-"))?;
        let view = user.view(&format!("card:{card}"))?;
        let allow = option_action(&view, "allow").with_context(|| format!("no «Allow»: {view}"))?;
        let interrupt = wait(report, "the task offers «Interrupt» for its working node", || {
            Ok((interrupt_of(&user, "t-r5")?, "no «Interrupt»".into()))
        })?;
        report.step("the user interrupts the node from its task", || user.press(interrupt["id"].as_str().unwrap_or_default(), None, None).map(drop))?;
        wait(report, "the permission card leaves Inbox with the request", || {
            let inbox = user.inbox()?;
            Ok(((!inbox.contains(&card)).then_some(()), format!("{inbox:?}")))
        })?;
        report.step("«Allow» pressed late is refused and nothing runs", || {
            let (status, body) = user.try_press(allow["id"].as_str().unwrap_or_default(), Some("allow"))?;
            ensure!(body["outcome"] != "applied", "a late answer was applied: {status} {body}");
            ensure!(!proof.exists(), "{} was written", proof.display());
            Ok(())
        })
    });
}

/// The local settings of the sandbox's repository, kept in its main checkout, where Claude Code
/// reads them from every worktree.
fn local_settings(sandbox: &Sandbox) -> Result<Value> {
    let path = sandbox.dir.join("repo/.claude/settings.local.json");
    match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).with_context(|| format!("reading {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}
