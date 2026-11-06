//! A node session's life through the Agent SDK host: its Claude crashes and the user lets it
//! carry on in a resumed session, the user interrupts it, the daemon restarts under it, and the
//! bearer of a session that ended stops working.
use std::{
    fs,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{Report, User, steps::*, user::mentions};
use crate::{
    fake::{
        Fakes,
        native::{self, Order, Session},
        script::Step,
    },
    sandbox::{Sandbox, wait_for},
};

/// A title longer than a card's headline can hold with what the card asks.
const S5_TITLE: &str = "Plan the onboarding without the login barrier and the GitHub permissions it asks for";
const S6_TITLE: &str = "Publish the release after its merge";

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    report.case("S1", "A node whose native Claude crashes carries on by itself in the resumed session, with no card, and finishes its task", |report| {
        request(report, fakes, &user, &task_script("req-s1", "t-s1", "Survive a crash"), "Survive a crash")?;
        let node = launched(report, fakes, "t-s1")?;
        state(report, &user, "t-s1", WORKING)?;
        let revived = crash_and_carry_on(report, fakes, &user, &node, "t-s1")?;
        report.step("its task is still in progress", || {
            if !in_progress(&user, "t-s1")? {
                bail!("the revived node's task is not in progress");
            }
            Ok(())
        })?;
        deliver_merge_close(report, fakes, &user, &revived, "t-s1", 21)
    });
    report.case("S2", "The user interrupts a node from its task: it pauses without a card and its Claude is not restarted", |report| {
        request(report, fakes, &user, &task_script("req-s2", "t-s2", "Stop on purpose"), "Stop on purpose")?;
        let node = launched(report, fakes, "t-s2")?;
        state(report, &user, "t-s2", WORKING)?;
        report.snapshot("task-working", &user.view("task:t-s2")?)?;
        let interrupt = interrupt_of(&user, "t-s2")?.context("the task offers no «Interrupt» on its working node")?;
        report.step("the user presses «Interrupt» on the task", || user.press(interrupt["id"].as_str().unwrap_or_default(), None, None))?;
        state(report, &user, "t-s2", "Paused")?;
        report.snapshot("task-interrupted", &user.view("task:t-s2")?)?;
        report.step("the interruption creates no decision card", || {
            anyhow::ensure!(card_in_inbox(&user, "turn-ended-t-s2-")?.0.is_none(), "an interruption card was created");
            Ok(())
        })?;
        report.step("the node no longer offers «Interrupt» and its Claude was not restarted", || {
            if interrupt_of(&user, "t-s2")?.is_some() {
                bail!("the node still offers «Interrupt»");
            }
            let starts = fakes.starts(&node.short)?;
            if starts.len() != 1 || !fakes.session(&node.short)?.running() {
                bail!("the session started {} times; running: {}", starts.len(), fakes.session(&node.short)?.running());
            }
            Ok(())
        })
    });
    report.case("S3", "The daemon restarts mid-task: card, task and node survive and the merge goes on", |report| {
        request(report, fakes, &user, &task_script("req-s3", "t-s3", "Survive a restart"), "Survive a restart")?;
        let node = launched(report, fakes, "t-s3")?;
        state(report, &user, "t-s3", WORKING)?;
        act(report, fakes, &node, "the node delivers PR #23", &Step::work(23, "restart.txt"))?;
        let (card, _) = review_card(report, &user, &node)?;
        report.step("Mac 1's daemon dies and starts again on the same HOME", || restart(sandbox, 1))?;
        wait(report, "the review card is still in Inbox", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().any(|id| *id == card).then_some(()), format!("{inbox:?}")))
        })?;
        state(report, &user, "t-s3", IN_REVIEW)?;
        report.step("the node's native Claude kept running under the adopted host", || {
            let now = fakes.session(&node.short)?;
            if !now.running() || fakes.starts(&node.short)?.len() != 1 {
                bail!("the session is {:?} and started {} times", now.state, fakes.starts(&node.short)?.len());
            }
            Ok(())
        })?;
        let view = user.view(&format!("card:{card}"))?;
        let head = head(&node)?;
        press(report, &user, &view, "merge:", None, None, "the user merges PR #23 after the restart")?;
        wait(report, "the node is told its task is finished and closing", || told(fakes, &node, "is finished and is closing"))?;
        merged(report, fakes, 23, &head)?;
        closed(report, fakes, &user, &node, "t-s3", &card)
    });
    report.case("S4", "When a node's session ends, the bearer it held stops working", |report| {
        request(report, fakes, &user, &task_script("req-s4", "t-s4", "End the session"), "End the session")?;
        let node = launched(report, fakes, "t-s4")?;
        state(report, &user, "t-s4", WORKING)?;
        let bearer = node.capability().context("the session holds no bearer")?;
        let resumed = crash_and_carry_on(report, fakes, &user, &node, "t-s4")?;
        report.step("a call with the old bearer is refused", || {
            let (refused, text) = native::call_with(fakes, &node, &bearer, "present_result", json!({
                "task_id": "t-s4", "interaction_id": "late-s4", "title": "Session ended", "need": "Too late", "body": "Too late",
                "targets": [{"kind": "text", "text": "Too late"}], "is_final": false,
            }))?;
            if !refused || !text.contains("forbidden") {
                bail!("the old bearer was {}: {text}", if refused { "refused without saying forbidden" } else { "accepted" });
            }
            Ok(())
        })?;
        report.step("the resumed session holds a bearer of its own", || match resumed.capability() {
            Some(current) if current != bearer => Ok(()),
            Some(_) => bail!("the resumed session got the revoked bearer again"),
            None => bail!("the resumed session holds no bearer"),
        })
    });
    report.case("S5", "Claude asks to leave plan mode with 41 points from a turn it started by itself while the user's message waits: it gets why, shortens the plan, and the user approves it", |report| {
        request(report, fakes, &user, &task_script("req-s5", "t-s5", S5_TITLE), S5_TITLE)?;
        let node = launched(report, fakes, "t-s5")?;
        act(report, fakes, &node, "the node's turn ends while its research runs in the background", &[Step::Speak { said: "Waiting for the research report".into(), behind: Some("research".into()), listed: Some("working".into()) }])?;
        report.step("its research explains what follows: nobody is asked and the node is not woken", || {
            if let Some(card) = user.inbox()?.into_iter().find(|id| id.starts_with("turn-ended-t-s5-") || id.starts_with("stalled-t-s5-")) {
                bail!("{card} asks how the task goes on");
            }
            Ok(())
        })?;
        act(report, fakes, &node, "the research report starts a turn of Claude's own", &[Step::OwnTurn { said: "The report is in: writing the plan".into() }])?;
        state(report, &user, "t-s5", WORKING)?;
        let agent = wait(report, "the task names its working node", || Ok((node_of(&user, S5_TITLE)?, "no node yet".into())))?;
        report.step("the user writes to the node while its turn runs", || {
            let (status, body) = user.send("POST", &format!("/agents/{agent}/messages"), Some(json!({"text": "Submit your plan"})))?;
            anyhow::ensure!((200..300).contains(&status), "{status} {body}");
            Ok(())
        })?;
        wait(report, "the native Claude has the message queued behind its turn", || told(fakes, &node, "Submit your plan"))?;
        refused(report, fakes, &node, "Claude asks to leave plan mode with 41 points and is told why it was refused",
            &Step::LeavePlanMode { points: 41 }, "at most 40 points")?;
        report.step("the node keeps its host and nobody was asked", || {
            anyhow::ensure!(fakes.session(&node.short)?.running() && fakes.starts(&node.short)?.len() == 1, "the native Claude was restarted or stopped");
            let inbox = user.inbox()?;
            anyhow::ensure!(!inbox.iter().any(|id| id.starts_with("host-lost-t-s5-") || id.starts_with("decision-t-s5-")), "{inbox:?}");
            Ok(())
        })?;
        std::thread::scope(|scope| {
            let shorter = scope.spawn(|| native::order(fakes, &node, &Order { steps: vec![Step::LeavePlanMode { points: 2 }], end_turn: false }));
            let card = wait(report, "Claude asks again with a plan of 2 points: its Review card waits in Inbox", || card_in_inbox(&user, "decision-t-s5-"))?;
            let view = user.view(&format!("card:{card}"))?;
            report.snapshot("card-shorter-plan", &view)?;
            let approve = user.action(&view, "accept:").with_context(|| format!("the card offers no «Approve»: {view}"))?;
            report.step("the user approves the plan", || user.press(approve["id"].as_str().unwrap_or_default(), None, None))?;
            report.step("the approval reaches Claude", || {
                let outcome = shorter.join().map_err(|_| anyhow::anyhow!("the order panicked"))??.pop().context("no outcome")?;
                anyhow::ensure!(outcome.ok, "{}", outcome.text);
                Ok(())
            })
        })?;
        act(report, fakes, &node, "Claude's own turn ends and the message that waited runs", &[Step::EndOwnTurn])?;
        state(report, &user, "t-s5", "In the background")?;
        report.step("the turn of the message ends on its research too: nobody is asked", || {
            let inbox = user.inbox()?;
            anyhow::ensure!(!inbox.iter().any(|id| ["turn-ended-t-s5-", "stalled-t-s5-", "host-lost-t-s5-"].iter().any(|card| id.starts_with(card))), "{inbox:?}");
            Ok(())
        })?;
        report.snapshot("task-after-plan", &user.view("task:t-s5")?)
    });
    report.case("S6", "The user force-stops work after the merge: it pauses without a card, then a message resumes it", |report| {
        let mut script = task_script("req-s6", "t-s6", S6_TITLE);
        script.tasks[0].cleans_up = false;
        request(report, fakes, &user, &script, S6_TITLE)?;
        let node = launched(report, fakes, "t-s6")?;
        state(report, &user, "t-s6", WORKING)?;
        act(report, fakes, &node, "the node says it publishes the release after the merge", &[Step::PostMergeWork { reason: Some("release.sh publishes 0.3.0".into()) }])?;
        act(report, fakes, &node, "the node delivers PR #26", &Step::work(26, "release.txt"))?;
        let (_, view) = review_card(report, &user, &node)?;
        let head = head(&node)?;
        press(report, &user, &view, "merge:", None, None, "the user merges PR #26 from the card")?;
        merged(report, fakes, 26, &head)?;
        let hold = fakes.dir.join(native::HOLD_INTERRUPT);
        report.step("Claude will accept an interrupt and keep going", || Ok(fs::write(&hold, b"")?))?;
        let interrupt = wait(report, "the task offers «Interrupt» on its node", || Ok((interrupt_of(&user, "t-s6")?, "no «Interrupt»".into())))?;
        report.step("the user presses «Interrupt» on the task", || user.press(interrupt["id"].as_str().unwrap_or_default(), None, None))?;
        let force = wait(report, "the task offers «Force stop» once Claude accepted the interrupt", || {
            Ok((task_action(&user, "t-s6", "force_stop:")?, "no «Force stop»".into()))
        })?;
        report.step("the user presses «Force stop»", || user.press(force["id"].as_str().unwrap_or_default(), None, None))?;
        state(report, &user, "t-s6", "Paused")?;
        report.snapshot("task-post-merge-stopped", &user.view("task:t-s6")?)?;
        report.step("the force stop creates no decision card", || {
            anyhow::ensure!(card_in_inbox(&user, "session-ended-t-s6-")?.0.is_none(), "a force-stop card was created");
            Ok(())
        })?;
        let agent = node_of(&user, S6_TITLE)?.context("the task has no node")?;
        report.step("the user asks it to finish the release", || {
            let _ = fs::remove_file(&hold);
            let (status, body) = user.send("POST", &format!("/agents/{agent}/messages"), Some(json!({"text": "Finish the release"})))?;
            anyhow::ensure!((200..300).contains(&status), "{status} {body}");
            Ok(())
        })?;
        let resumed = wait(report, "its resumed native session receives the instruction", || {
            let now = fakes.session(&node.short)?;
            let read = fakes.received(&node.short)?.iter().any(|input| input.text.contains("Finish the release"));
            Ok(((now.running() && read).then_some(now), "the instruction has not arrived".into()))
        })?;
        act(report, fakes, &resumed, "the node finishes the release and cleans up", &[Step::PostMergeWork { reason: None }, Step::Cleanup])?;
        state(report, &user, "t-s6", FINISHED)
    });
}

/// The node's native Claude dies without a word. A host's end is not the user's decision: no card
/// asks how to go on, and a native Claude that resumes the same session, with its transcript, is
/// told to go on by itself.
fn crash_and_carry_on(report: &mut Report, fakes: &Fakes, user: &User, node: &Session, task: &str) -> Result<Session> {
    act(report, fakes, node, "the node's native Claude crashes", &[Step::Die])?;
    let resumed = wait(report, "a new native Claude resumes the same session, with its conversation, and is told to go on", || {
        let now = fakes.session(&node.short)?;
        let last = fakes.starts(&node.short)?.pop();
        let resumed = last.as_ref().is_some_and(|start| start.resume.as_deref() == Some(node.session_id.as_str()) && start.history > 0);
        let since = last.as_ref().map_or(u64::MAX, |start| start.at);
        let told = fakes.received(&node.short)?.iter().any(|input| input.at >= since);
        Ok(((now.running() && resumed && told).then_some(now.clone()), format!("{:?}, started {} times, last {last:?}", now.state, now.starts)))
    })?;
    report.snapshot("task-after-crash", &user.view(&format!("task:{task}"))?)?;
    report.step("no card asked the user how it goes on", || {
        anyhow::ensure!(card_in_inbox(user, &format!("session-ended-{task}-"))?.0.is_none(), "a card asks how {task} goes on");
        Ok(())
    })?;
    Ok(resumed)
}


/// Kills Mac `number`'s daemon, as a crash would, and starts it again with the script the sandbox
/// started it with. A crash leaves the SDK hosts running for the daemon that comes back to adopt;
/// a daemon that is only stopped stops them.
pub(super) fn restart(sandbox: &Sandbox, number: usize) -> Result<()> {
    let target = sandbox.mac(number)?;
    let home = sandbox.dir.join(format!("mac-{number}"));
    let pid: u32 = fs::read_to_string(home.join("alondrad.pid"))?.trim().parse().context("alondrad.pid")?;
    Command::new("kill").args(["-KILL", &pid.to_string()]).status()?;
    let daemon = format!("{}/daemon", target.local);
    let agent = crate::sandbox::http();
    wait_for("the daemon to stop", Duration::from_secs(30), || Ok(agent.get(&daemon).call().is_err().then_some(())))?;
    let log = fs::OpenOptions::new().append(true).open(home.join("alondrad.log"))?;
    let child = Command::new(home.join("start")).stdin(Stdio::null()).stdout(log.try_clone()?).stderr(log).spawn()?;
    fs::write(home.join("alondrad.pid"), child.id().to_string())?;
    wait_for("the daemon to answer again", Duration::from_secs(60), || {
        Ok(agent.get(&daemon).call().ok().filter(|reply| reply.status() == 200).map(drop))
    })
}

/// Turns Mac `number` off and on again: the kernel ends every SDK host it ran, with its native
/// Claude, and the daemon starts again with no host left to adopt.
pub(super) fn power_cycle(sandbox: &Sandbox, number: usize) -> Result<()> {
    let groups = || -> Result<Vec<(String, String)>> {
        let node = sandbox.dir.join("sdk/runtime/node");
        let real = fs::canonicalize(&sandbox.dir)?.join("sdk/runtime/node");
        let listing = Command::new("ps").args(["-axo", "pid=,pgid=,command="]).output()?;
        Ok(String::from_utf8_lossy(&listing.stdout)
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                Some((fields.next()?.to_owned(), fields.next()?.to_owned(), fields.next()?.to_owned()))
            })
            .filter(|(_, _, command)| *command == node.to_string_lossy() || *command == real.to_string_lossy())
            .map(|(pid, pgid, _)| (pid, pgid))
            .collect())
    };
    let hosts: Vec<String> = groups()?.into_iter().filter(|(pid, pgid)| pid == pgid).map(|(_, pgid)| pgid).collect();
    anyhow::ensure!(!hosts.is_empty(), "no SDK host runs on Mac {number}");
    for group in &hosts {
        Command::new("kill").args(["-KILL", "--", &format!("-{group}")]).status()?;
    }
    // An ended process the daemon has not reaped yet is a zombie: it runs nothing and holds no lock.
    wait_for("every process of the SDK hosts to end", Duration::from_secs(30), || {
        let listing = Command::new("ps").args(["-axo", "pgid=,stat="]).output()?;
        let alive = String::from_utf8_lossy(&listing.stdout).lines().any(|line| {
            let mut fields = line.split_whitespace();
            let (group, stat) = (fields.next().unwrap_or_default(), fields.next().unwrap_or_default());
            !stat.starts_with('Z') && hosts.iter().any(|host| host == group)
        });
        Ok((!alive).then_some(()))
    })?;
    restart(sandbox, number)
}

/// The command line of every native Claude the sandbox's SDK hosts run.
pub(super) fn native_claudes(sandbox: &Sandbox) -> Result<Vec<String>> {
    let claude = sandbox.dir.join("sdk/runtime/claude");
    let real = fs::canonicalize(&sandbox.dir)?.join("sdk/runtime/claude");
    let listing = Command::new("ps").args(["-axo", "command="]).output()?;
    Ok(String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter(|command| command.starts_with(&format!("{} ", claude.display())) || command.starts_with(&format!("{} ", real.display())))
        .map(str::to_owned)
        .collect())
}
