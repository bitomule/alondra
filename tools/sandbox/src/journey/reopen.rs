//! A task cancelled and reopened goes back through the queue: with every slot taken it waits in the
//! queue saying why, never in progress with no node, and starts with a new node once there is room
//! (t-fastconn-5-remove-mailbox, 15 October).
use anyhow::{Context, bail, ensure};
use serde_json::json;

use super::{User, steps::*};
use crate::{
    fake::{Fakes, native::Session, script::Step},
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
        state(report, &user, "t-r1", "Working")?;
        request(report, fakes, &user, &task_script("req-r2", "t-r2", "Remove the mailbox"), "Remove the mailbox")?;
        let first = launched(report, fakes, "t-r2")?;
        let first_node = node_id(&first)?.to_owned();
        crate::fake::git(&first.cwd, &["commit", "--allow-empty", "-m", "Work no remote has: cancelling keeps its branch"])?;
        let original_branch = crate::fake::git(&first.cwd, &["branch", "--show-current"])?;
        let original_head = head(&first)?;
        native_snapshot(report, fakes, "native-before-cancel", &first)?;
        report.step(&format!("node {first_node} uses native session {} in {}", first.session_id, first.cwd.display()), || {
            ensure!(first.cwd.join(".git").is_file(), "{} is not the task's linked worktree", first.cwd.display());
            Ok(())
        })?;
        state(report, &user, "t-r2", WORKING)?;
        let task = user.view("task:t-r2")?;
        press(report, &user, &task, "cancel:", None, None, "the user cancels the task")?;
        state(report, &user, "t-r2", CANCELLED)?;
        ended(report, fakes, &user, &first)?;
        wait(report, &format!("cancellation removes the original native directory {}", first.cwd.display()), || {
            Ok(((!first.cwd.exists()).then_some(()), format!("{} exists: {}", first.cwd.display(), first.cwd.exists())))
        })?;
        native_snapshot(report, fakes, "native-after-cancel", &fakes.session(&first.short)?)?;
        limit(report, &user, &one)?;
        let task = user.view("task:t-r2")?;
        press(report, &user, &task, "reopen:", None, None, "the user reopens it")?;
        let full = format!("{one} of {one} slots taken");
        let queued = wait(report, "the reopened task waits in the queue behind the working nodes", || {
            let view = user.view("task:t-r2")?;
            let seen = user.task_state("t-r2")?;
            let waits = seen == "Waiting" && super::user::mentions(&view, &full);
            Ok((waits.then_some(view), seen))
        })?;
        report.snapshot("task-t-r2-queued", &queued)?;
        if super::user::mentions(&queued, "closed before it delivered") {
            bail!("the queued task still says it stopped: {queued}");
        }
        limit(report, &user, &two)?;
        let second = wait(report, "with a free slot a new node recovers the original native session", || {
            let session = fakes.session_of("t-r2")?;
            let seen = format!("{:?}", session.as_ref().map(|session| (node_id(session), &session.session_id, &session.cwd, &session.state, session.starts)));
            Ok((session.filter(|session| session.session_id == first.session_id && node_id(session).is_ok_and(|id| id != first_node)
                && session.starts > first.starts && session.running()), seen))
        })?;
        native_snapshot(report, fakes, "native-after-reopen", &second)?;
        report.step("the new node resumes once, with its native history in the reconstructed original worktree", || {
            let starts = fakes.starts(&second.short)?;
            let resumed = starts.last().context("the reopened session has no native start")?;
            ensure!(second.cwd == first.cwd && second.cwd.is_dir(), "the original directory was not reconstructed: {} -> {}", first.cwd.display(), second.cwd.display());
            ensure!(crate::fake::git(&second.cwd, &["branch", "--show-current"])? == original_branch, "the reconstructed worktree changed branch");
            ensure!(head(&second)? == original_head, "the reconstructed worktree changed its commit");
            ensure!(resumed.resume.as_deref() == Some(first.session_id.as_str()) && resumed.history > 0, "the original conversation was not resumed: {resumed:?}");
            ensure!(second.starts == first.starts + 1, "the reopen retried the native start: {} -> {}", first.starts, second.starts);
            Ok(())
        })?;
        state(report, &user, "t-r2", WORKING)?;
        report.step(&format!("node {} works on it with native session {}", node_id(&second)?, second.session_id), || Ok(()))
    });
}

fn node_id(session: &Session) -> anyhow::Result<&str> {
    session.env.iter().find(|(name, _)| name == "ALONDRA_SESSION_ID")
        .map(|(_, value)| value.as_str()).context("the native session has no Alondra node identity")
}

fn native_snapshot(report: &mut Report, fakes: &Fakes, name: &str, session: &Session) -> anyhow::Result<()> {
    report.snapshot(name, &json!({
        "node_id": node_id(session)?, "native_session_id": session.session_id,
        "native_directory": session.cwd, "directory_exists": session.cwd.is_dir(),
        "state": session.state, "starts": fakes.starts(&session.short)?,
    }))
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
