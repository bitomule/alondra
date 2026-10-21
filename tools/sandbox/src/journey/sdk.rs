//! A task through the Agent SDK backend (`SDK=1`): the orchestrator, itself one turn of the pinned
//! native Claude through the SDK host, creates it with Alondra's MCP tools; Alondra admits it and
//! the SDK host runs the task's native Claude against the local model double. Its first turn
//! ends with nothing that explains the next step, so one card asks the user; «Carry on» with a
//! note from the phone commits and reaches Claude, whose next turn ends with a card of its own.
mod decisions;
mod relay;
mod workers;

use anyhow::{Context, Result};
use serde_json::Value;

use super::{Report, User, steps::*};
use crate::{fake::{Fakes, script::RequestScript}, sandbox::Sandbox};

const NOTE: &str = "Add the tests first";
const WRITTEN: &str = "Write the changelog too";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "journey", 1, false)) else {
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
        report.snapshot("fleet-before-power-cycle", &user.view("fleet")?)?;
        report.step("the Mac shuts down: the SDK host, its native Claude and the daemon all end", || super::session::power_cycle(sandbox, 1))?;
        report.step("no native Claude runs until the user answers", || {
            let running = super::session::native_claudes(sandbox)?;
            anyhow::ensure!(running.is_empty(), "{running:?}");
            Ok(())
        })?;
        report.snapshot("fleet-after-power-cycle", &user.view("fleet")?)?;
        let card = user.view(&format!("card:{second}"))?;
        let reply = user.action(&card, "reply:").context("the card offers no «Reply» after the Mac came back")?;
        report.step("the user writes on the card what to do", || {
            user.press(reply["id"].as_str().unwrap_or_default(), None, Some(WRITTEN))
        })?;
        wait(report, "what the user wrote on the card reaches the resumed native Claude", || {
            let requests = model_requests(sandbox)?;
            let told = requests.iter().skip(2).find(|request| request["user"].as_str().is_some_and(|text| text.contains(WRITTEN)));
            Ok((told.map(drop), format!("{} requests", requests.len())))
        })?;
        report.step("the new native Claude resumed the stored session", || {
            let running = super::session::native_claudes(sandbox)?;
            anyhow::ensure!(running.len() == 1 && running[0].contains("--resume"), "{running:?}");
            Ok(())
        })?;
        report.step("the resumed session carries the whole conversation, not a fresh one", || {
            let requests = model_requests(sandbox)?;
            let messages = |text: &str| requests.iter().find(|request| request["user"].as_str().is_some_and(|user| user.contains(text)))
                .and_then(|request| request["messages"].as_u64()).unwrap_or(0);
            let (before, after) = (messages(NOTE), messages(WRITTEN));
            anyhow::ensure!(after > before, "after the power cycle the request carried {after} messages, before it {before}: the context was lost");
            Ok(())
        })?;
        report.snapshot("fleet", &user.view("fleet")?)
    });
    report.case("S2", "Claude's sign-in is refused: the task waits on the sign-in card, and «I signed in» lets it go on", |report| {
        refuse(sandbox, Some(401))?;
        request(report, fakes, &user, &task_script("req-s2", "t-s2", "Write the upgrade guide"), "Write the upgrade guide")?;
        let card = wait(report, "the native Claude's 401 ends its turn and the sign-in card waits in Inbox", || hold_card(&user, "claude-signed-out-t-s2-"))?;
        report.step("a refused sign-in is no generic turn-end card", || {
            let inbox = user.inbox()?;
            anyhow::ensure!(!inbox.iter().any(|id| id.starts_with("turn-ended-t-s2-")), "{inbox:?}");
            Ok(())
        })?;
        let view = user.view(&format!("card:{card}"))?;
        report.snapshot("card-signed-out", &view)?;
        refuse(sandbox, None)?;
        let signed_in = user.action_labeled(&view, "★ I signed in").context("the card offers no «I signed in»")?;
        report.step("the user signs in outside Alondra and presses «I signed in»", || {
            user.press(signed_in["id"].as_str().unwrap_or_default(), signed_in["option_id"].as_str(), None)
        })?;
        wait(report, "the task goes on: Claude gets told to carry on", || {
            let requests = model_requests(sandbox)?;
            Ok(((told_to_go_on(&requests) == 1).then_some(()), format!("{} requests", requests.len())))
        })?;
        wait(report, "Claude answered: no sign-in card is left", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().all(|id| !id.starts_with("claude-signed-out-")).then_some(()), format!("{inbox:?}")))
        })?;
        Ok(())
    });
    report.case("S3", "The API is overloaded past Claude Code's retries: the task waits, unannounced, and «Try now» lets it go on", |report| {
        refuse(sandbox, Some(529))?;
        request(report, fakes, &user, &task_script("req-s3", "t-s3", "Write the FAQ"), "Write the FAQ")?;
        let card = wait(report, "the native Claude's 529 ends its turn and the API card waits in Inbox", || hold_card(&user, "claude-outage-t-s3-"))?;
        let view = user.view(&format!("card:{card}"))?;
        report.snapshot("card-outage", &view)?;
        refuse(sandbox, None)?;
        let try_now = user.action_labeled(&view, "★ Try now").context("the card offers no «Try now»")?;
        report.step("the user presses «Try now»", || user.press(try_now["id"].as_str().unwrap_or_default(), try_now["option_id"].as_str(), None))?;
        wait(report, "the task goes on: Claude gets told to carry on", || {
            let requests = model_requests(sandbox)?;
            Ok(((told_to_go_on(&requests) == 2).then_some(()), format!("{} requests", requests.len())))
        })?;
        wait(report, "Claude answered: no API card is left", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().all(|id| !id.starts_with("claude-outage-")).then_some(()), format!("{inbox:?}")))
        })?;
        Ok(())
    });
    report.case("S13", "A long conversation resting on its card compacts once before its prompt cache expires, and the card stays", |report| {
        const MISSION: &str = "Summarize the roadmap";
        large_context(sandbox, true)?;
        request(report, fakes, &user, &task_script("req-s13", "t-s13", MISSION), MISSION)?;
        let card = wait(report, "the turn ends and its card waits in Inbox", || card_in_inbox(&user, "turn-ended-t-s13-"))?;
        large_context(sandbox, false)?;
        let mission = |sandbox: &Sandbox| -> Result<Vec<Value>> {
            Ok(model_requests(sandbox)?.into_iter().filter(|request| request["context"].as_str().is_some_and(|context| context.contains(MISSION))).collect())
        };
        let compactions = |sandbox: &Sandbox| {
            let log = std::fs::read_to_string(sandbox.dir.join("mac-1/alondrad.log")).unwrap_or_default();
            log.lines().filter(|line| line.contains("compacting t-s13 while it rests")).count()
        };
        wait(report, "with nobody answering, Alondra compacts the resting conversation before its cache expires", || {
            let compacted = compactions(sandbox);
            Ok(((compacted > 0).then_some(()), format!("{compacted} compactions logged")))
        })?;
        wait(report, "Claude asks the model for the conversation's summary", || {
            let requests = mission(sandbox)?;
            Ok(((requests.len() >= 2).then_some(()), format!("{} requests carry the mission", requests.len())))
        })?;
        report.step("Alondra compacted the conversation once", || {
            let compacted = compactions(sandbox);
            anyhow::ensure!(compacted == 1, "{compacted} compactions logged");
            Ok(())
        })?;
        report.step("the same card waits, and no other card of the task appeared", || {
            let inbox = user.inbox()?;
            let cards: Vec<&String> = inbox.iter().filter(|id| id.contains("t-s13")).collect();
            anyhow::ensure!(cards == [&card], "{inbox:?}");
            Ok(())
        })?;
        report.step("the model got the mission and one compaction, nothing else", || {
            let requests = mission(sandbox)?;
            anyhow::ensure!(requests.len() == 2, "{requests:?}");
            Ok(())
        })?;
        report.snapshot("card-after-compaction", &user.view(&format!("card:{card}"))?)
    });
    report.case("S4", "A task's native Claude loads the user's default profile, the project and Alondra's plugin", |report| {
        write_parity(sandbox)?;
        request(report, fakes, &user, &task_script("req-s4", "t-s4", "Write the parity notes"), "Write the parity notes")?;
        let first = wait(report, "the task's native Claude asks the model", || {
            let requests = model_requests(sandbox)?;
            let found = requests.into_iter().find(|request| request["request"].is_number() && request["user"].as_str().is_some_and(|text| text.contains("Write the parity notes")));
            Ok((found, "no request of the task yet".to_owned()))
        })?;
        let context = first["context"].as_str().unwrap_or_default().to_owned();
        for (what, marker) in MEMORIES {
            report.step(&format!("Claude loaded {what}"), || {
                anyhow::ensure!(context.contains(marker), "{marker} is not in what Claude Code put in front of the model");
                Ok(())
            })?;
        }
        let init = native_init(sandbox, "t-s4")?.context("the native Claude's init was not kept")?;
        report.step("init lists the user's skill, the user's plugin's skill and Alondra's", || {
            let skills: Vec<&str> = init["skills"].as_array().map(|skills| skills.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let missing: Vec<&str> = SKILLS.into_iter().filter(|skill| !skills.contains(skill)).collect();
            anyhow::ensure!(missing.is_empty(), "missing {missing:?} from {skills:?}");
            Ok(())
        })?;
        report.step("Alondra's MCP server from the node plugin is connected", || {
            let servers = init["mcp_servers"].as_array().cloned().unwrap_or_default();
            anyhow::ensure!(servers.iter().any(|server| server["name"] == "plugin:alondra:alondra" && server["status"] == "connected"), "{servers:?}");
            Ok(())
        })
    });
    decisions::run(report, sandbox, fakes, &user);
    relay::run(report, sandbox, fakes);
    workers::run(report, sandbox, fakes, &user);
    report.case("S11", "The Mac shuts down before Claude shows it read its mission: each task asks before sending it again", |report| {
        hold(sandbox, true)?;
        for (request_id, task, title) in DOUBTFUL {
            request(report, fakes, &user, &task_script(request_id, task, title), title)?;
            wait(report, &format!("{task}'s native Claude got its mission and the model has not answered"), || {
                let requests = model_requests(sandbox)?;
                Ok((requests.iter().any(|request| request["user"].as_str().is_some_and(|text| text.contains(title))).then_some(()), format!("{} requests", requests.len())))
            })?;
        }
        report.step("the Mac shuts down: every SDK host, its native Claude and the daemon end", || super::session::power_cycle(sandbox, 1))?;
        hold(sandbox, false)?;
        let mut doubts = vec![];
        for (_, task, title) in DOUBTFUL {
            let card = wait(report, &format!("{task} asks whether to send its mission again"), || hold_card(&user, &format!("handoff-doubt-{task}-")))?;
            let view = user.view(&format!("card:{card}"))?;
            report.step(&format!("{task}'s card carries the whole message"), || {
                anyhow::ensure!(view.to_string().contains(title), "{view}");
                Ok(())
            })?;
            doubts.push(view);
        }
        report.snapshot("card-doubtful-message", &doubts[0])?;
        report.step("nothing is sent while the user decides", || {
            let running = super::session::native_claudes(sandbox)?;
            anyhow::ensure!(running.is_empty(), "{running:?}");
            Ok(())
        })?;
        let (_, resent, mission) = DOUBTFUL[0];
        let again = user.action_labeled(&doubts[0], "Send it again").context("the card offers no «Send it again»")?;
        report.step(&format!("the user sends {resent}'s message again"), || user.press(again["id"].as_str().unwrap_or_default(), again["option_id"].as_str(), None))?;
        wait(report, "the same mission reaches a native Claude once more", || {
            let requests = model_requests(sandbox)?;
            let told = requests.iter().filter(|request| request["user"].as_str().is_some_and(|text| text.contains(mission))).count();
            Ok(((told == 2).then_some(()), format!("{told} requests with the mission")))
        })?;
        wait(report, &format!("{resent}'s turn ends with a card of its own"), || hold_card(&user, &format!("turn-ended-{resent}-")))?;
        let (_, received, _) = DOUBTFUL[1];
        let taken = user.action_labeled(&doubts[1], "Take it as received").context("the card offers no «Take it as received»")?;
        report.step(&format!("the user takes {received}'s message as received"), || user.press(taken["id"].as_str().unwrap_or_default(), taken["option_id"].as_str(), None))?;
        let ended = wait(report, &format!("{received} asks how to go on"), || hold_card(&user, &format!("session-ended-{received}-")))?;
        report.snapshot("card-session-ended", &user.view(&format!("card:{ended}"))?)?;
        report.snapshot("fleet", &user.view("fleet")?)
    });
    report.case("S12", "Five messages, five batches: each is a new conversation of the orchestrator's native Claude, and what it sends the model does not grow", |report| {
        let mut batches = vec![];
        for n in 1..=CONTEXT_BATCHES {
            let request_id = format!("req-ctx-{n}");
            let script = RequestScript { request_id: request_id.clone(), tasks: vec![], answer: Some(format!("Noted: message {n} needs no task")) };
            request(report, fakes, &user, &script, &format!("Message {n} for the orchestrator"))?;
            let batch = wait(report, &format!("the orchestrator closes {request_id} and its turn ends"), || {
                let requests = orchestrator_requests(sandbox)?;
                let ours: Vec<&Value> = requests.iter().filter(|request| request["user"].as_str().is_some_and(|text| text.contains(&format!("The user created Request {request_id}")))).collect();
                let closed = requests.iter().any(|request| request["results"].as_array().is_some_and(|results| results.iter().any(|result| result["tool_use_id"] == format!("toolu_close_{request_id}"))));
                let first = ours.first().map(|request| serde_json::json!({"request": request_id, "messages": request["messages"], "bytes": request["bytes"]}));
                Ok((first.filter(|_| closed), format!("{} requests of this batch, closed {closed}", ours.len())))
            })?;
            batches.push(batch);
        }
        report.snapshot("orchestrator-context", &serde_json::json!(batches))?;
        report.step("each batch starts with the same context: none carries the conversations before it", || {
            let messages: Vec<u64> = batches.iter().map(|batch| batch["messages"].as_u64().unwrap_or(0)).collect();
            anyhow::ensure!(messages.iter().all(|count| *count == messages[0]), "messages the first request of each batch carried: {messages:?} ({batches:?})");
            Ok(())
        })
    });
}

/// How many batches S12 measures.
const CONTEXT_BATCHES: usize = 5;

/// Three tasks whose mission Claude had not shown it read when the Mac shut down.
const DOUBTFUL: [(&str, &str, &str); 3] = [
    ("req-s11", "t-s11", "Write the migration notes"),
    ("req-s12", "t-s12", "Write the rollout plan"),
    ("req-s13", "t-s13", "Write the support guide"),
];

/// The model double takes the tasks' requests and holds them unanswered while it is set.
fn hold(sandbox: &Sandbox, on: bool) -> Result<()> {
    let marker = sandbox.dir.join("sdk/model-hold");
    if on {
        return std::fs::write(&marker, "").with_context(|| format!("writing {}", marker.display()));
    }
    match std::fs::remove_file(&marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error).context("removing the hold"),
        _ => Ok(()),
    }
}

/// How many requests the model answered that told a held task to go on.
fn told_to_go_on(requests: &[Value]) -> usize {
    requests.iter().filter(|request| request["request"].is_number() && request["user"].as_str().is_some_and(|text| text.contains("Claude answers again"))).count()
}

/// What Claude Code puts in front of the model from each settings source, by a marker in it: the
/// user's CLAUDE.md, the project's (committed, since the task works in a worktree of the repo) and
/// the local CLAUDE.local.md.
const MEMORIES: [(&str, &str); 3] = [
    ("the user's CLAUDE.md (user source)", "PARITY-USER-MEMORY"),
    ("the project's CLAUDE.md (project source)", "PARITY-PROJECT-MEMORY"),
    ("the local CLAUDE.local.md (local source)", "PARITY-LOCAL-MEMORY"),
];
/// What the native Claude's init lists: the user's skill, the skill of the plugin the user enabled
/// in their settings, and the skill of Alondra's node plugin.
const SKILLS: [&str; 3] = ["user-parity", "parity-plugin:plugin-parity", "alondra:alondra"];

fn write_parity(sandbox: &Sandbox) -> Result<()> {
    let home = sandbox.dir.join("sdk/home");
    let repo = sandbox.dir.join("repo");
    let marketplace = home.join("parity-marketplace");
    let skill = |name: &str| format!("---\nname: {name}\ndescription: A skill the parity check looks for.\n---\n\nParity.\n");
    let files = [
        (home.join(".claude/CLAUDE.md"), "PARITY-USER-MEMORY\n".to_owned()),
        (home.join(".claude/skills/user-parity/SKILL.md"), skill("user-parity")),
        (home.join(".claude/settings.json"), serde_json::json!({
            "extraKnownMarketplaces": {"parity": {"source": {"source": "directory", "path": marketplace}}},
            "enabledPlugins": {"parity-plugin@parity": true},
        }).to_string()),
        (marketplace.join(".claude-plugin/marketplace.json"), serde_json::json!({
            "name": "parity", "owner": {"name": "Parity"},
            "plugins": [{"name": "parity-plugin", "source": "./parity-plugin", "description": "The parity check's plugin"}],
        }).to_string()),
        (marketplace.join("parity-plugin/.claude-plugin/plugin.json"), serde_json::json!({"name": "parity-plugin", "version": "1.0.0"}).to_string()),
        (marketplace.join("parity-plugin/skills/plugin-parity/SKILL.md"), skill("plugin-parity")),
        (repo.join("CLAUDE.md"), "PARITY-PROJECT-MEMORY\n".to_owned()),
        (repo.join("CLAUDE.local.md"), "PARITY-LOCAL-MEMORY\n".to_owned()),
    ];
    for (path, text) in files {
        std::fs::create_dir_all(path.parent().context("a parent folder")?)?;
        std::fs::write(&path, text).with_context(|| format!("writing {}", path.display()))?;
    }
    for args in [&["add", "CLAUDE.md"][..], &["commit", "-q", "-m", "Project memory for the parity check"]] {
        let status = std::process::Command::new("git").arg("-C").arg(&repo).args(args).status().context("git")?;
        anyhow::ensure!(status.success(), "git {args:?} failed in {}", repo.display());
    }
    Ok(())
}

/// The events the SDK hosts of `task` kept in their own outboxes, under the backend's state root,
/// in cursor order. The Mac keeps a
/// raw observation's bytes only until it is normalized, so the hosts' journals are what tells what
/// the native Claude said.
pub(super) fn host_events(sandbox: &Sandbox, task: &str) -> Result<Vec<Value>> {
    let mut events = std::collections::BTreeMap::new();
    let backend: Value = serde_json::from_slice(&std::fs::read(sandbox.dir.join("mac-1/sdk-backend.json")).context("the Mac's SDK backend")?)?;
    let root = backend["state_root"].as_str().context("the SDK backend's state root")?;
    let Ok(folders) = std::fs::read_dir(root) else { return Ok(vec![]) };
    for folder in folders {
        let Ok(raw) = std::fs::read(folder?.path().join("outbox")) else { continue };
        let mut offset = 0;
        while let Some(header) = raw.get(offset..offset + 4) {
            let length = u32::from_be_bytes(header.try_into()?) as usize;
            let Some(payload) = raw.get(offset + 4..offset + 4 + length) else { break };
            offset += 4 + length;
            let Ok(frame) = serde_json::from_slice::<Value>(payload) else { continue };
            let body = &frame["body"];
            if body["type"] == "event" && body["execution"]["task_id"] == task {
                if let Some(cursor) = body["cursor"].as_u64() {
                    events.insert(cursor, frame);
                }
            }
        }
    }
    Ok(events.into_values().collect())
}

/// The native Claude's init for `task`, as its host kept it.
fn native_init(sandbox: &Sandbox, task: &str) -> Result<Option<Value>> {
    fn init(value: &Value) -> Option<&Value> {
        match value {
            Value::Object(map) if map.get("type") == Some(&Value::from("system")) && map.get("subtype") == Some(&Value::from("init")) => Some(value),
            Value::Object(map) => map.values().find_map(init),
            Value::Array(items) => items.iter().find_map(init),
            _ => None,
        }
    }
    Ok(host_events(sandbox, task)?.iter().find_map(|event| init(event).cloned()))
}

fn hold_card(user: &User, prefix: &str) -> Result<(Option<String>, String)> {
    let inbox = user.inbox()?;
    Ok((inbox.iter().find(|id| id.starts_with(prefix)).cloned(), format!("{inbox:?}")))
}

/// The model double answers the tasks' requests with `status` while it is set.
fn refuse(sandbox: &Sandbox, status: Option<u16>) -> Result<()> {
    let marker = sandbox.dir.join("sdk/model-refuse");
    match status {
        Some(status) => std::fs::write(&marker, status.to_string()).with_context(|| format!("writing {}", marker.display())),
        None => match std::fs::remove_file(&marker) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error).context("removing the refusal"),
            _ => Ok(()),
        },
    }
}

/// The model double reports a 250 000-token conversation whose request wrote a five-minute prompt
/// cache while it is set.
fn large_context(sandbox: &Sandbox, on: bool) -> Result<()> {
    let marker = sandbox.dir.join("sdk/model-usage");
    if on {
        let usage = serde_json::json!({"input_tokens": 3, "cache_creation_input_tokens": 2000, "cache_read_input_tokens": 248_000, "output_tokens": 0,
            "cache_creation": {"ephemeral_5m_input_tokens": 2000, "ephemeral_1h_input_tokens": 0}});
        return std::fs::write(&marker, usage.to_string()).with_context(|| format!("writing {}", marker.display()));
    }
    match std::fs::remove_file(&marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error).context("removing the usage"),
        _ => Ok(()),
    }
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
