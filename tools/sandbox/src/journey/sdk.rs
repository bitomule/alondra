//! A task through the Agent SDK backend (`SDK=1`): the orchestrator, itself one turn of the pinned
//! native Claude through the SDK host, creates it with Alondra's MCP tools; Alondra admits it and
//! the SDK host runs the task's native Claude against the local model double. Its first turn
//! ends with nothing that explains the next step, so one card asks the user; «Carry on» with a
//! note from the phone commits and reaches Claude, whose next turn ends with a card of its own.
use anyhow::{Context, Result};
use serde_json::Value;

use super::{Report, User, steps::*};
use crate::{fake::Fakes, sandbox::Sandbox};

const NOTE: &str = "Add the tests first";
const WRITTEN: &str = "Write the changelog too";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 through the relay", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    report.case("S1", "A task runs through the Agent SDK; its turn-end card asks once and «Carry on» from the phone reaches Claude", |report| {
        request(report, fakes, &user, &task_script("req-s1", "t-s1", "Write the release notes"), "Write the release notes")?;
        let first = wait(report, "the native Claude's first turn ends and its card waits in Inbox", || turn_card(&user, None))?;
        let batches = orchestrator_requests(sandbox)?;
        report.step("the orchestrator's batch ran through the SDK host, not claude -p", || {
            anyhow::ensure!(batches.iter().any(|request| request["user"].as_str().is_some_and(|text| text.contains("The user created Request req-s1"))), "{batches:?}");
            let fallback = fakes.dir.join("orchestrator");
            let prompts = std::fs::read_dir(&fallback).map(|entries| entries.filter_map(|entry| entry.ok()).filter(|entry| entry.file_name().to_string_lossy().starts_with("prompt-")).count()).unwrap_or(0);
            anyhow::ensure!(prompts == 0, "{prompts} batches went to claude -p in {}", fallback.display());
            Ok(())
        })?;
        let requests = model_requests(sandbox)?;
        report.step("the model got the task's mission once", || {
            anyhow::ensure!(requests.len() == 1, "{} requests reached the model: {requests:?}", requests.len());
            anyhow::ensure!(requests[0]["user"].as_str().is_some_and(|text| text.contains("Write the release notes")), "{requests:?}");
            Ok(())
        })?;
        let card = user.view(&format!("card:{first}"))?;
        report.snapshot("card-turn-ended", &card)?;
        report.snapshot("fleet-before-restart", &user.view("fleet")?)?;
        report.step("the Mac's daemon restarts and adopts the running host", || super::session::restart(sandbox, 1))?;
        report.snapshot("fleet-after-restart", &user.view("fleet")?)?;
        let carry_on = user.action_labeled(&card, "Carry on").context("the card offers no «Carry on»")?;
        report.step("the user presses «Carry on» with a note after the restart", || {
            user.press(carry_on["id"].as_str().unwrap_or_default(), carry_on["option_id"].as_str(), Some(NOTE))
        })?;
        wait(report, "what the user wrote reaches the same native Claude after the restart", || {
            let requests = model_requests(sandbox)?;
            let told = requests.iter().skip(1).find(|request| request["user"].as_str().is_some_and(|text| text.contains(NOTE)));
            Ok((told.map(drop), format!("{} requests", requests.len())))
        })?;
        report.step("the carry-on continued the same conversation, not a fresh one", || {
            let requests = model_requests(sandbox)?;
            let first_messages = requests.first().and_then(|request| request["messages"].as_u64()).unwrap_or(0);
            let note_messages = requests.iter().find(|request| request["user"].as_str().is_some_and(|text| text.contains(NOTE)))
                .and_then(|request| request["messages"].as_u64()).unwrap_or(0);
            anyhow::ensure!(note_messages > first_messages, "the carry-on request carried {note_messages} messages, the first {first_messages}: the conversation did not grow");
            Ok(())
        })?;
        let second = wait(report, "its next turn ends with a card of its own", || turn_card(&user, Some(&first)))?;
        let card = user.view(&format!("card:{second}"))?;
        report.snapshot("card-turn-ended-again", &card)?;
        let reply = user.action(&card, "reply:").context("the card offers no «Reply»")?;
        report.step("the user writes on the card what to do", || {
            user.press(reply["id"].as_str().unwrap_or_default(), None, Some(WRITTEN))
        })?;
        wait(report, "what the user wrote on the card reaches the native Claude", || {
            let requests = model_requests(sandbox)?;
            let told = requests.iter().skip(2).find(|request| request["user"].as_str().is_some_and(|text| text.contains(WRITTEN)));
            Ok((told.map(drop), format!("{} requests", requests.len())))
        })?;
        report.snapshot("fleet", &user.view("fleet")?)
    });
}

fn turn_card(user: &User, not: Option<&str>) -> Result<(Option<String>, String)> {
    let inbox = user.inbox()?;
    let card = inbox.iter().find(|id| id.starts_with("turn-ended-t-s1-") && Some(id.as_str()) != not).cloned();
    Ok((card, format!("{inbox:?}")))
}

/// What the task's native Claude asked the model.
fn model_requests(sandbox: &Sandbox) -> Result<Vec<Value>> {
    Ok(requests_to_model(sandbox)?.into_iter().filter(|request| request["orchestrator"] != true).collect())
}

/// What the orchestrator's native Claude asked the model.
fn orchestrator_requests(sandbox: &Sandbox) -> Result<Vec<Value>> {
    Ok(requests_to_model(sandbox)?.into_iter().filter(|request| request["orchestrator"] == true).collect())
}

fn requests_to_model(sandbox: &Sandbox) -> Result<Vec<Value>> {
    let log = sandbox.dir.join("sdk/model-requests.jsonl");
    let text = match std::fs::read_to_string(&log) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(error).with_context(|| format!("reading {}", log.display())),
    };
    text.lines().map(|line| serde_json::from_str(line).context("a model request line")).collect()
}
