//! A task cancelled and reopened goes back through the queue: with every slot taken it waits in the
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
        // The cases before leave some nodes in a turn of their own: the limits count from them.
        let busy = report.step("the nodes already working are counted", || working(&user))?;
        let (one, two) = ((busy + 1).to_string(), (busy + 2).to_string());
        limit(report, &user, &two)?;
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
        ended(report, fakes, &user, &first)?;
        limit(report, &user, &one)?;
        let task = user.view("task:t-r2")?;
        press(report, &user, &task, "reopen:", None, None, "the user reopens it")?;
        let full = format!("{one} of {one} working");
        let queued = wait(report, "the reopened task waits in the queue behind the working nodes", || {
            let view = user.view("task:t-r2")?;
            let seen = user.task_state("t-r2")?;
            let waits = seen == "Queued" && super::user::mentions(&view, &full);
            Ok((waits.then_some(view), seen))
        })?;
        report.snapshot("task-t-r2-queued", &queued)?;
        if super::user::mentions(&queued, "closed before it delivered") || super::user::mentions(&queued, "Stopped") {
            bail!("the queued task still says it stopped: {queued}");
        }
        limit(report, &user, &two)?;
        let second = wait(report, "with a free slot the queue starts it with a new node", || {
            let session = fakes.session_of("t-r2")?;
            let seen = format!("{:?}", session.as_ref().map(|session| (&session.short, &session.state)));
            Ok((session.filter(|session| session.short != first.short && session.running()), seen))
        })?;
        state(report, &user, "t-r2", WORKING)?;
        report.step(&format!("node {} works on it", second.short), || Ok(()))
    });
}

/// The nodes that take a slot now, as every view's status counts them.
fn working(user: &User) -> anyhow::Result<u64> {
    let home = user.view("home")?;
    home["status"]["counts"].as_array().into_iter().flatten()
        .find(|count| count["id"] == "working").and_then(|count| count["value"].as_u64())
        .with_context(|| format!("the status counts no working nodes: {}", home["status"]))
}

fn limit(report: &mut Report, user: &User, count: &str) -> anyhow::Result<()> {
    report.step(&format!("the user sets {count} nodes at once"), || {
        let machine = user.view("machine")?;
        let action = user.action(&machine, "set_limit:").context("the machine view offers no node limit")?;
        user.press(action["id"].as_str().unwrap_or_default(), Some(count), None).map(drop)
    })
}
