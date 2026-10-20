//! A task cancelled and reopened goes back through the fleet: with every slot taken it waits in the
//! queue saying why, never in progress with no node, and starts with a new node once there is room
//! (t-fastconn-5-remove-mailbox, 15 October).
use anyhow::{Context, bail};

use super::{User, steps::*};
use crate::{
    fake::{Fakes, script::Step},
    journey::Report,
    sandbox::Sandbox,
};

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    report.case("R1", "A task reopened with every slot taken waits in the queue saying why, then starts with a new node", |report| {
        limit(report, &user, "2")?;
        let mut busy = task_script("req-r1", "t-r1", "Keep the build green");
        busy.tasks[0].on_start = vec![Step::Speak { said: "The tests are running".into(), behind: Some("make test".into()), listed: Some("working".into()) }];
        request(report, fakes, &user, &busy, "Keep the build green")?;
        launched(report, fakes, "t-r1")?;
        state(report, &user, "t-r1", WORKING)?;
        request(report, fakes, &user, &task_script("req-r2", "t-r2", "Remove the mailbox"), "Remove the mailbox")?;
        let first = launched(report, fakes, "t-r2")?;
        state(report, &user, "t-r2", WORKING)?;
        let task = user.view("task:t-r2")?;
        press(report, &user, &task, "cancel:", None, None, "the user cancels the task")?;
        state(report, &user, "t-r2", CANCELLED)?;
        wait(report, &format!("Alondra removes node {} and it leaves Fleet", first.short), || {
            let rm = fakes.calls()?.iter().any(|call| call.tool == "claude" && call.args == ["rm", first.short.as_str()]);
            let fleet = user.view("fleet")?;
            let listed = super::user::mentions(&fleet, &first.short) || super::user::mentions(&fleet, &first.session_id);
            Ok(((rm && !listed).then_some(()), format!("rm called: {rm}, still in Fleet: {listed}")))
        })?;
        limit(report, &user, "1")?;
        let task = user.view("task:t-r2")?;
        press(report, &user, &task, "reopen:", None, None, "the user reopens it")?;
        let queued = wait(report, "the reopened task waits in the queue behind the one working node", || {
            let view = user.view("task:t-r2")?;
            let seen = user.task_state("t-r2")?;
            let waits = seen == "Queued" && super::user::mentions(&view, "1 of 1 working");
            Ok((waits.then_some(view), seen))
        })?;
        report.snapshot("task-t-r2-queued", &queued)?;
        if super::user::mentions(&queued, "closed before it delivered") || super::user::mentions(&queued, "Stopped") {
            bail!("the queued task still says it stopped: {queued}");
        }
        limit(report, &user, "2")?;
        let second = wait(report, "with a free slot the fleet starts it with a new node", || {
            let node = fakes.node_of("t-r2")?;
            let seen = format!("{:?}", node.as_ref().map(|node| (&node.short, &node.state)));
            Ok((node.filter(|node| node.short != first.short && node.state == crate::fake::node::State::Running), seen))
        })?;
        state(report, &user, "t-r2", WORKING)?;
        report.step(&format!("node {} works on it", second.short), || Ok(()))
    });
}

fn limit(report: &mut Report, user: &User, count: &str) -> anyhow::Result<()> {
    report.step(&format!("the user sets {count} nodes at once"), || {
        let machine = user.view("machine")?;
        let action = user.action(&machine, "set_limit:").context("the machine view offers no node limit")?;
        user.press(action["id"].as_str().unwrap_or_default(), Some(count), None).map(drop)
    })
}
