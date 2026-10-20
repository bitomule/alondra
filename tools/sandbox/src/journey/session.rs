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
        native::{self, Session},
        script::Step,
    },
    sandbox::{Sandbox, wait_for},
};

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    report.case("S1", "A node whose native Claude crashes carries on, resumed, once the user says so, and finishes its task", |report| {
        request(report, fakes, &user, &task_script("req-s1", "t-s1", "Survive a crash"), "Survive a crash")?;
        let node = launched(report, fakes, "t-s1")?;
        state(report, &user, "t-s1", WORKING)?;
        let revived = crash_and_carry_on(report, fakes, &user, &node, "t-s1")?;
        report.step("Fleet still lists the node, by its task's name", || {
            let fleet = user.view("fleet")?;
            if !mentions(&fleet, "Survive a crash") {
                bail!("Fleet does not list the revived node");
            }
            Ok(())
        })?;
        deliver_merge_close(report, fakes, &user, &revived, "t-s1", 21)
    });
    report.case("S2", "The user interrupts a node from Fleet: its turn ends with one card and its Claude is not restarted", |report| {
        request(report, fakes, &user, &task_script("req-s2", "t-s2", "Stop on purpose"), "Stop on purpose")?;
        let node = launched(report, fakes, "t-s2")?;
        state(report, &user, "t-s2", WORKING)?;
        let fleet = user.view("fleet")?;
        report.snapshot("fleet-working", &fleet)?;
        let interrupt = interrupt_of(&fleet, "Stop on purpose").context("Fleet offers no «Interrupt» on the working node")?;
        report.step("the user presses «Interrupt» on the node in Fleet", || user.press(interrupt["id"].as_str().unwrap_or_default(), None, None))?;
        let card = wait(report, "the turn ends with a card that asks how to go on", || card_in_inbox(&user, "turn-ended-t-s2-"))?;
        report.snapshot("card-interrupted", &user.view(&format!("card:{card}"))?)?;
        report.step("the node no longer offers «Interrupt» and its Claude was not restarted", || {
            if interrupt_of(&user.view("fleet")?, "Stop on purpose").is_some() {
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
                "task_id": "t-s4", "interaction_id": "late-s4", "need": "Too late", "body": "Too late",
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
}

/// The node's native Claude dies without a word; the task asks how to go on and «Carry on» starts
/// a native Claude that resumes the same session, with its transcript, and gets the instruction.
fn crash_and_carry_on(report: &mut Report, fakes: &Fakes, user: &User, node: &Session, task: &str) -> Result<Session> {
    act(report, fakes, node, "the node's native Claude crashes", &[Step::Die])?;
    let card = wait(report, "the task asks how to go on now that its session closed", || card_in_inbox(user, &format!("session-ended-{task}-")))?;
    let view = user.view(&format!("card:{card}"))?;
    report.snapshot("card-session-ended", &view)?;
    report.snapshot("fleet-after-crash", &user.view("fleet")?)?;
    let carry_on = user.action_labeled(&view, "Carry on").context("the card offers no «Carry on»")?;
    report.step("the user presses «Carry on»", || user.press(carry_on["id"].as_str().unwrap_or_default(), carry_on["option_id"].as_str(), None))?;
    wait(report, "a new native Claude resumes the same session, with its conversation, and is told to go on", || {
        let now = fakes.session(&node.short)?;
        let last = fakes.starts(&node.short)?.pop();
        let resumed = last.as_ref().is_some_and(|start| start.resume.as_deref() == Some(node.session_id.as_str()) && start.history > 0);
        let since = last.as_ref().map_or(u64::MAX, |start| start.at);
        let told = fakes.received(&node.short)?.iter().any(|input| input.at >= since);
        Ok(((now.running() && resumed && told).then_some(now.clone()), format!("{:?}, started {} times, last {last:?}", now.state, now.starts)))
    })
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
