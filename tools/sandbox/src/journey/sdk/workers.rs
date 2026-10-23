//! A worker of the Agent SDK from launch to close: a scheduled task starts through the SDK host; a
//! message sent while the node's turn runs reaches its native Claude; a node plans, delivers with
//! its checklist, is accepted, cleans up and closes; and a subagent's isolated worktree with work
//! left holds the close until the node gives it up, after which nothing of the task is left.
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use super::decisions::{choose, option_action, script_tool, tool_result};
use super::{hold_card, model_requests};
use crate::journey::{Report, User, steps::*, user::{collect, mentions}};
use crate::{fake::Fakes, sandbox::Sandbox};

const SCHEDULED: &str = "Write the weekly notes";
const MID_TURN: &str = "Draft the migration notes";
const TOLD: &str = "Mention the rollback too";
const DELIVER: &str = "Ship the notes format";
/// What the user asks for: the mission holds it, the cleanup request names only the title.
const DELIVER_ASK: &str = "Plan and ship the notes format";
const PLAN_TEXT: &str = "# Notes format\n\n- [ ] Write the notes as plain text\n- [ ] Check the notes render";
const SPLIT: &str = "Split the parser work";
const SPLIT_ASK: &str = "Split the parser work with a subagent";
const DELIVER_SPLIT: &str = "Deliver the parser split now";
const SUBAGENT: &str = "Write the parser draft file in your worktree";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes, user: &User) {
    // S4 commits the project's memory on the repository's main branch; the tasks below deliver from
    // worktrees of it, which Alondra accepts only with every commit on a remote.
    if report.step("the sandbox's repository has its main branch pushed", || {
        let status = std::process::Command::new("git").arg("-C").arg(sandbox.dir.join("repo")).args(["push", "-q", "origin", "HEAD"]).status().context("git")?;
        ensure!(status.success(), "git push failed");
        Ok(())
    }).is_err() {
        return;
    }
    report.case("W1", "A scheduled task starts through the Agent SDK host and its turn ends with one card", |report| {
        report.step("the Mac's projects are the sandbox's folder, whose repository the native Claude may work in", || {
            let root = std::fs::canonicalize(&sandbox.dir)?;
            std::fs::write(sandbox.dir.join("mac-1/options.toml"), format!("projects_root = {:?}\n", root.display().to_string())).context("writing options.toml")
        })?;
        report.step("the user creates a schedule from the phone", || {
            let (status, body) = user.send("POST", "/schedules", Some(json!({
                "title": "Weekly notes", "text": SCHEDULED, "project": "repo", "frequency": "daily", "hour": 10, "minute": 0, "enabled": false,
            })))?;
            ensure!((200..300).contains(&status), "{status} {body}");
            Ok(body)
        })?;
        let schedules = user.view("schedules")?;
        let run = user.action(&schedules, "sched_run:").with_context(|| format!("the schedules offer no «Run Now»: {schedules}"))?;
        report.step("the user runs it now from the phone", || user.press(run["id"].as_str().unwrap_or_default(), None, None).map(drop))?;
        wait(report, "the scheduled mission reaches a native Claude of the SDK host", || {
            let requests = model_requests(sandbox)?;
            Ok((requests.iter().any(|request| request["user"].as_str().is_some_and(|text| text.contains(SCHEDULED))).then_some(()), format!("{} requests", requests.len())))
        })?;
        let card = wait(report, "its turn ends with one card in Inbox", || {
            let inbox = user.inbox()?;
            for id in inbox.iter().filter(|id| id.starts_with("turn-ended-")) {
                if mentions(&user.view(&format!("card:{id}"))?, "Weekly notes") {
                    return Ok((Some(id.clone()), String::new()));
                }
            }
            Ok((None, format!("{inbox:?}")))
        })?;
        report.snapshot("card-scheduled-turn-ended", &user.view(&format!("card:{card}"))?)?;
        report.step("its node runs through the SDK", || {
            ensure!(node_of(user, "Weekly notes")?.is_some_and(|node| node.starts_with("sdk-")), "the scheduled task's node is not an SDK node");
            Ok(())
        })
    });
    report.case("W2", "What the user tells a node while its turn runs reaches its native Claude inside that turn, which ends with one card", |report| {
        let proof = sandbox.dir.join("sdk/tmp/mid-turn-proof");
        script_tool(sandbox, MID_TURN, "toolu_w2_bash", "Bash", json!({"command": format!("sleep 6 && echo checked > {}", proof.display()), "description": "Run the migration check"}))?;
        request(report, fakes, user, &task_script("req-w2", "t-w2", MID_TURN), MID_TURN)?;
        let card = wait(report, "the node's command waits on its permission card", || hold_card(user, "decision-t-w2-"))?;
        let view = user.view(&format!("card:{card}"))?;
        choose(report, user, &view, "allow", "the user allows the command, which runs for seconds")?;
        let node = wait(report, "the task names its working node", || Ok((node_of(user, MID_TURN)?, "no node yet".into())))?;
        report.step("the user tells the node something while its command runs", || {
            let (status, body) = user.send("POST", &format!("/agents/{node}/messages"), Some(json!({"text": TOLD})))?;
            ensure!((200..300).contains(&status), "{status} {body}");
            ensure!(body["delivery_state"] == "dispatching", "{body}");
            Ok(())
        })?;
        wait(report, "the native Claude takes what the user said into the turn it runs, saying who sent it", || {
            let transcript = session_transcript(sandbox, "t-w2")?;
            let queued = transcript.iter().any(|line| line["attachment"]["type"] == "queued_command"
                && line["attachment"]["prompt"].as_str().is_some_and(|prompt| prompt.starts_with("[From ") && prompt.contains(TOLD)));
            let sent = transcript.iter().any(|line| line["type"] == "api-request-blob"
                && line.to_string().contains("The user sent a new message while you were working") && line.to_string().contains(TOLD));
            Ok(((queued && sent).then_some(()), format!("queued: {queued}, sent to the model: {sent}; dispatches of t-w2: {}", dispatches(sandbox, "t-w2")?)))
        })?;
        wait(report, "the turn ends with one card, not two", || {
            let inbox = user.inbox()?;
            let cards = inbox.iter().filter(|id| id.starts_with("turn-ended-t-w2-")).count();
            Ok(((cards == 1).then_some(()), format!("{inbox:?}")))
        })
    });
    report.case("W3", "A node plans, delivers with its checklist, is accepted, cleans up and closes", |report| {
        script_tool(sandbox, DELIVER_ASK, "toolu_w3_1_enter", "EnterPlanMode", json!({}))?;
        script_tool(sandbox, DELIVER_ASK, "toolu_w3_2_write", "Write", json!({"file_path": "{plan_file}", "content": PLAN_TEXT}))?;
        script_tool(sandbox, DELIVER_ASK, "toolu_w3_3_exit", "ExitPlanMode", json!({}))?;
        script_tool(sandbox, DELIVER_ASK, "toolu_w3_4_deliver", "mcp__plugin_alondra_alondra__present_result", json!({
            "task_id": "t-w3", "interaction_id": "w3-final", "need": "Review the notes format", "body": "The notes are plain text and render.",
            "targets": [{"kind": "text", "text": "The notes format"}], "is_final": true,
            "checklist": [
                {"item": "1", "evidence": [{"kind": "command", "value": "make notes"}]},
                {"item": "2", "evidence": [{"kind": "test", "value": "make check-notes", "detail": "2 passed"}]},
            ],
        }))?;
        script_tool(sandbox, &format!("{DELIVER}” is finished and is closing"), "toolu_w3_5_cleanup", "mcp__plugin_alondra_alondra__cleanup_done", json!({"task_id": "t-w3"}))?;
        request(report, fakes, user, &task_script("req-w3", "t-w3", DELIVER), DELIVER_ASK)?;
        let plan = wait(report, "the node's plan waits on its review card", || hold_card(user, "decision-t-w3-"))?;
        let view = user.view(&format!("card:{plan}"))?;
        let approve = user.action(&view, "accept:").with_context(|| format!("the card offers no «Approve»: {view}"))?;
        report.step("the user approves the plan", || user.press(approve["id"].as_str().unwrap_or_default(), None, None).map(drop))?;
        wait(report, "the delivery with its checklist reaches Alondra", || {
            let delivered = tool_result(sandbox, "toolu_w3_4_deliver")?;
            let seen = format!("{delivered:?}; offered: {:?}", offered_tools(sandbox, DELIVER)?);
            Ok((delivered.filter(|result| result["is_error"] != true), seen))
        })?;
        let card = user.view("card:w3-final")?;
        report.snapshot("card-delivery-checklist", &card)?;
        let accept = user.action(&card, "accept:").or_else(|| user.action_labeled(&card, "Accept")).with_context(|| format!("the delivery offers no «Accept»: {card}"))?;
        report.step("the user accepts the result", || user.press(accept["id"].as_str().unwrap_or_default(), accept["option_id"].as_str(), None).map(drop))?;
        wait(report, "Alondra asks the node to clean up", || {
            let asked = model_requests(sandbox)?.iter().any(|request| request.to_string().contains("is finished and is closing"));
            Ok((asked.then_some(()), format!("dispatches of t-w3: {}", dispatches(sandbox, "t-w3")?)))
        })?;
        // cleanup_done closes the session, which may end before Claude sends its result to the model:
        // the task finishing is what shows the node cleaned up.
        state(report, user, "t-w3", FINISHED)?;
        report.step("the task's worktree is gone and it is no longer in progress", || {
            let worktree = sandbox.dir.join("repo/.claude/worktrees/t-w3");
            ensure!(!worktree.exists(), "{} is still there", worktree.display());
            ensure!(!in_progress(user, "t-w3")?, "the task is still in progress");
            Ok(())
        })
    });
    report.case("W4", "A subagent's isolated worktree with work left holds the close until the node gives it up", |report| {
        script_tool(sandbox, SPLIT_ASK, "toolu_w4_1_agent", "Agent", json!({
            "description": "Parser draft", "prompt": SUBAGENT, "subagent_type": "general-purpose", "isolation": "worktree",
        }))?;
        script_tool(sandbox, DELIVER_SPLIT, "toolu_w4_2_deliver", "mcp__plugin_alondra_alondra__present_result", json!({
            "task_id": "t-w4", "interaction_id": "w4-final", "need": "Review the parser split", "body": "The parser work is split.",
            "targets": [{"kind": "text", "text": "The parser split"}], "is_final": true,
        }))?;
        script_tool(sandbox, SUBAGENT, "toolu_w4_sub_write", "Bash", json!({"command": "echo draft > parser-draft.txt", "description": "Save the draft"}))?;
        script_tool(sandbox, &format!("{SPLIT}” is finished and is closing"), "toolu_w4_3_cleanup", "mcp__plugin_alondra_alondra__cleanup_done", json!({"task_id": "t-w4"}))?;
        script_tool(sandbox, "hold work no remote has", "toolu_w4_4_give_up", "mcp__plugin_alondra_alondra__cleanup_done", json!({"task_id": "t-w4", "discard": "{listed_worktrees}"}))?;
        request(report, fakes, user, &task_script("req-w4", "t-w4", SPLIT), SPLIT_ASK)?;
        wait(report, "the subagent runs: the user allows its command, and the node gets the subagent's result", || {
            allow_held(user, "decision-t-w4-")?;
            let result = tool_result(sandbox, "toolu_w4_1_agent")?;
            let seen = format!("{result:?}; offered: {:?}", offered_tools(sandbox, SPLIT)?);
            Ok((result, seen))
        })
        .and_then(|result| report.step("the subagent ran in a worktree of its own and kept it", || {
            ensure!(result["is_error"] != true && result["text"].as_str().is_some_and(|text| text.contains("worktree")), "{result}");
            Ok(())
        }))?;
        let subagent = wait(report, "the subagent kept its worktree and Alondra records it as the task's", || {
            allow_held(user, "decision-t-w4-")?;
            let listed: Vec<String> = std::fs::read_dir(sandbox.dir.join("repo/.claude/worktrees")).map(|entries| entries.filter_map(|entry| entry.ok())
                .map(|entry| entry.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
            let seen = format!("worktrees: {listed:?}; the subagent's result: {:?}; its command: {:?}",
                tool_result(sandbox, "toolu_w4_1_agent")?, tool_result(sandbox, "toolu_w4_sub_write")?);
            let worktrees = subagent_worktrees(sandbox)?;
            Ok((worktrees.into_iter().find(|dir| dir.join("parser-draft.txt").exists()), seen))
        })?;
        let mut allowed = vec![];
        wait(report, "the node's turn ends once every command it asked for is allowed", || {
            if let (Some(card), _) = hold_card(user, "decision-t-w4-")? {
                let view = user.view(&format!("card:{card}"))?;
                if let Some(allow) = option_action(&view, "allow") {
                    user.press(allow["id"].as_str().unwrap_or_default(), Some("allow"), None)?;
                    let mut titles = vec![];
                    collect(&view, &mut |item| item.get("title").is_some() || item.get("summary").is_some(), &mut titles);
                    allowed.push(format!("{card}: {:?}", titles.iter().map(|item| format!("{} {}", item["title"].as_str().unwrap_or_default(), item["summary"].as_str().unwrap_or_default())).collect::<Vec<_>>()));
                }
            }
            let inbox = user.inbox()?;
            Ok((inbox.iter().any(|id| id.starts_with("turn-ended-t-w4-")).then_some(()), format!("allowed: {allowed:?}; observations: {}", observations(sandbox, "t-w4")?)))
        })?;
        let node = node_of(user, SPLIT)?.context("the task names no node")?;
        report.step("the user tells the node to deliver", || {
            let (status, body) = user.send("POST", &format!("/agents/{node}/messages"), Some(json!({"text": DELIVER_SPLIT})))?;
            ensure!((200..300).contains(&status), "{status} {body}");
            Ok(())
        })?;
        wait(report, "the delivery reaches Alondra", || {
            let delivered = tool_result(sandbox, "toolu_w4_2_deliver")?;
            let seen = format!("{delivered:?}");
            Ok((delivered.filter(|result| result["is_error"] != true).map(drop), seen))
        })?;
        let card = user.view("card:w4-final")?;
        let accept = user.action(&card, "accept:").or_else(|| user.action_labeled(&card, "Accept")).with_context(|| format!("the delivery offers no «Accept»: {card}"))?;
        report.step("the user accepts the result", || user.press(accept["id"].as_str().unwrap_or_default(), accept["option_id"].as_str(), None).map(drop))?;
        wait(report, "cleanup_done without giving the worktree up is refused and names it", || {
            let refused = tool_result(sandbox, "toolu_w4_3_cleanup")?;
            Ok((refused.filter(|result| result["is_error"] == true && result["text"].as_str().is_some_and(|text| text.contains(subagent.to_str().unwrap_or_default()))).map(drop), "no refusal yet".into()))
        })?;
        // Giving the worktree up closes the session, which may end before Claude sends the result to
        // the model: the task finishing is what shows the node gave it up.
        state(report, user, "t-w4", FINISHED)?;
        report.step("nothing of the task is left: its worktree and the subagent's are gone", || {
            ensure!(!subagent.exists(), "{} is still there", subagent.display());
            ensure!(!sandbox.dir.join("repo/.claude/worktrees/t-w4").exists(), "the task's own worktree is still there");
            Ok(())
        })
    });
}

/// The tools the last model request of the task told `prompt` offered.
fn offered_tools(sandbox: &Sandbox, prompt: &str) -> Result<Vec<String>> {
    let requests = model_requests(sandbox)?;
    let last = requests.iter().rev().find(|request| request["context"].as_str().is_some_and(|text| text.contains(prompt)));
    Ok(last.and_then(|request| request["tools"].as_array()).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect())
}

/// The task's dispatches as the Mac keeps them: kind, input and status, in order.
fn dispatches(sandbox: &Sandbox, task: &str) -> Result<String> {
    let output = std::process::Command::new("sqlite3").arg("-readonly").arg(sandbox.dir.join("mac-1/domain.db"))
        .arg(format!("SELECT kind||':'||input_id||':'||status FROM lifecycle_dispatches WHERE task_id='{task}' ORDER BY id"))
        .output().context("sqlite3")?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().replace('\n', ", "))
}

/// The last raw observations the task's host kept: type, subtype and state of each.
fn observations(sandbox: &Sandbox, task: &str) -> Result<String> {
    let events = super::host_events(sandbox, task)?;
    let mut seen = vec![];
    for value in events.into_iter().rev().take(16) {
        let mut found = vec![];
        collect(&value, &mut |item| item.get("type").is_some() && (item.get("subtype").is_some() || item.get("state").is_some() || item.get("uuid").is_some()), &mut found);
        let item = found.first().cloned().unwrap_or(value);
        let said = item.pointer("/message/content").and_then(Value::as_array).and_then(|blocks| blocks.first()).map(|block| {
            block["text"].as_str().map(|text| text.chars().take(60).collect::<String>())
                .or_else(|| block["name"].as_str().map(|name| format!("tool {name}")))
                .or_else(|| block["content"].as_str().map(|text| format!("result {}", text.chars().take(60).collect::<String>())))
                .unwrap_or_default()
        }).unwrap_or_default();
        seen.push(format!("{}/{}/{} {said}", item["type"].as_str().unwrap_or("?"), item["subtype"].as_str().unwrap_or(""), item["state"].as_str().unwrap_or("")));
    }
    seen.reverse();
    Ok(seen.join(" "))
}

/// Allows the permission the node of the task holds on a card whose id starts with `prefix`, if any.
fn allow_held(user: &User, prefix: &str) -> Result<()> {
    if let (Some(card), _) = hold_card(user, prefix)? {
        let view = user.view(&format!("card:{card}"))?;
        if let Some(allow) = option_action(&view, "allow") {
            user.press(allow["id"].as_str().unwrap_or_default(), Some("allow"), None)?;
        }
    }
    Ok(())
}

/// The native Claude's own transcript of the session that runs `task`: every line of it.
fn session_transcript(sandbox: &Sandbox, task: &str) -> Result<Vec<Value>> {
    let projects = sandbox.dir.join("sdk/home/.claude/projects");
    let mut lines = vec![];
    let Ok(folders) = std::fs::read_dir(&projects) else { return Ok(lines) };
    for folder in folders.filter_map(|entry| entry.ok()).filter(|entry| entry.file_name().to_string_lossy().ends_with(&format!("-worktrees-{task}"))) {
        for file in std::fs::read_dir(folder.path())?.filter_map(|entry| entry.ok()).filter(|entry| entry.path().extension().is_some_and(|ext| ext == "jsonl")) {
            let text = std::fs::read_to_string(file.path())?;
            lines.extend(text.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()));
        }
    }
    Ok(lines)
}

/// The worktrees Claude Code made for subagents in the sandbox's repository.
fn subagent_worktrees(sandbox: &Sandbox) -> Result<Vec<std::path::PathBuf>> {
    let folder = sandbox.dir.join("repo/.claude/worktrees");
    let entries = match std::fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error).with_context(|| format!("reading {}", folder.display())),
    };
    Ok(entries.filter_map(|entry| entry.ok()).map(|entry| entry.path())
        .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("agent-")))
        .collect())
}
