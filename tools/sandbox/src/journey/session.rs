//! A node session's life: it crashes and comes back, the user stops it, the daemon restarts under
//! it, and its bearer dies with it.
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
        node::{self, State},
        script::Step,
    },
    sandbox::{Sandbox, wait_for},
};

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 through the relay", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    report.case("S1", "A node whose session crashes is restarted with claude respawn and finishes its task", |report| {
        request(report, fakes, &user, &task_script("req-s1", "t-s1", "Survive a crash"), "Survive a crash")?;
        let node = launched(report, fakes, "t-s1")?;
        state(report, &user, "t-s1", WORKING)?;
        act(report, fakes, &node, "the node's session crashes", &[Step::Die])?;
        let fleet = user.view("fleet")?;
        report.snapshot("fleet-after-crash", &fleet)?;
        wait(report, &format!("Alondra runs claude respawn {}", node.short), || {
            let respawned = fakes.calls()?.iter().any(|call| call.tool == "claude" && call.args == ["respawn", node.short.as_str()]);
            Ok((respawned.then_some(()), format!("respawn called: {respawned}")))
        })?;
        let revived = wait(report, "the session comes back under the same id and reconnects", || {
            let now = fakes.node(&node.short)?;
            Ok(((now.state == State::Running && now.starts >= 2).then_some(now.clone()), format!("{:?}, started {} times", now.state, now.starts)))
        })?;
        report.step("Fleet still lists the node, by its task's name", || {
            let fleet = user.view("fleet")?;
            if !mentions(&fleet, "Survive a crash") && !mentions(&fleet, &node.name) {
                bail!("Fleet does not list the revived node");
            }
            Ok(())
        })?;
        deliver_merge_close(report, fakes, &user, &revived, "t-s1", 21)
    });
    report.case("S2", "A session the user stops is not restarted; its task waits for a new node", |report| {
        request(report, fakes, &user, &task_script("req-s2", "t-s2", "Stop on purpose"), "Stop on purpose")?;
        let node = launched(report, fakes, "t-s2")?;
        state(report, &user, "t-s2", WORKING)?;
        report.step(&format!("the user stops the session with claude stop {}", node.short), || {
            node::end(fakes, &node.short, State::Stopped)
        })?;
        wait(report, "the task is freed and reads “Stopped”", || {
            let seen = user.task_state("t-s2")?;
            Ok(((seen == "Stopped").then_some(()), seen))
        })?;
        report.step("Alondra did not restart it, and Fleet no longer lists it", || {
            if fakes.calls()?.iter().any(|call| call.tool == "claude" && call.args == ["respawn", node.short.as_str()]) {
                bail!("Alondra ran claude respawn {} on a session the user stopped", node.short);
            }
            if mentions(&user.view("fleet")?, &node.short) {
                bail!("Fleet still lists {}", node.short);
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
        report.step("Mac 1's daemon stops and starts again on the same HOME", || restart(sandbox, 1))?;
        wait(report, "the review card is still in Inbox", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().any(|id| *id == card).then_some(()), format!("{inbox:?}")))
        })?;
        state(report, &user, "t-s3", IN_REVIEW)?;
        let view = user.view(&format!("card:{card}"))?;
        let head = head(&node)?;
        press(report, &user, &view, "merge:", None, None, "the user merges PR #23 after the restart")?;
        wait(report, "the reconnected node is told its task is finished and closing", || {
            let pushed = fakes.pushed(&node.short)?;
            let told = pushed.iter().any(|push| push.text.as_deref().is_some_and(|text| text.contains("is finished and is closing")));
            Ok((told.then_some(()), format!("{} messages", pushed.len())))
        })?;
        merged(report, fakes, 23, &head)?;
        closed(report, fakes, &user, &node, "t-s3", &card)
    });
    report.case("S4", "When a session ends, its bearer stops working", |report| {
        request(report, fakes, &user, &task_script("req-s4", "t-s4", "End the session"), "End the session")?;
        let node = launched(report, fakes, "t-s4")?;
        state(report, &user, "t-s4", WORKING)?;
        let bearer = node::capability(&node).context("the session holds no bearer")?;
        report.step("the session ends (SessionEnd)", || node::end(fakes, &node.short, State::Stopped))?;
        report.step("a call with the old bearer is refused", || {
            let (refused, text) = node::call_with(fakes, &node, &bearer, "present_result", json!({
                "task_id": "t-s4", "interaction_id": "late-s4", "need": "Too late", "body": "Too late",
                "targets": [{"kind": "text", "text": "Too late"}], "is_final": false,
            }))?;
            if !refused || !text.contains("forbidden") {
                bail!("the old bearer was {}: {text}", if refused { "refused without saying forbidden" } else { "accepted" });
            }
            Ok(())
        })?;
        report.step("the settings file kept no bearer", || match node::capability(&fakes.node(&node.short)?) {
            Some(_) => bail!("the revoked bearer is still in {}", node.settings.display()),
            None => Ok(()),
        })
    });
}

/// Stops Mac `number`'s daemon and starts it again with the script the sandbox started it with.
fn restart(sandbox: &Sandbox, number: usize) -> Result<()> {
    let target = sandbox.mac(number)?;
    let home = sandbox.dir.join(format!("mac-{number}"));
    let pid: u32 = fs::read_to_string(home.join("alondrad.pid"))?.trim().parse().context("alondrad.pid")?;
    Command::new("kill").arg(pid.to_string()).status()?;
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
