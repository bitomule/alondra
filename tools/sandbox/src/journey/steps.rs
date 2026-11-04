//! The steps journeys share: waits on what other processes apply, and the milestones of a task. A
//! task's node is the scripted native Claude of an Agent SDK host (`fake::native`).
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{PATIENCE, Report, User, user::collect};
use crate::fake::{
    Fakes,
    native::{self, Order, Session},
    script::{RequestScript, Step, TaskScript},
};

pub const WORKING: &str = "Working";
pub const IN_REVIEW: &str = "Ready for review";
pub const FINISHED: &str = "Finished";
pub const CANCELLED: &str = "Cancelled";
pub const STALL_CLOSED: &str = "Closed without a delivery";

/// A step that waits until `probe` finds what it wants; on timeout it says what it saw last.
pub fn wait<T>(report: &mut Report, what: &str, probe: impl FnMut() -> Result<(Option<T>, String)>) -> Result<T> {
    wait_within(report, what, PATIENCE, probe)
}

/// `wait`, for what another process applies only after a delay of its own on top of `PATIENCE`.
pub fn wait_within<T>(report: &mut Report, what: &str, limit: Duration, mut probe: impl FnMut() -> Result<(Option<T>, String)>) -> Result<T> {
    report.step(what, || {
        let start = Instant::now();
        loop {
            let (found, seen) = probe()?;
            if let Some(found) = found {
                return Ok(found);
            }
            if start.elapsed() > limit {
                bail!("not within {} s; last seen: {seen}", limit.as_secs());
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    })
}

pub fn task_script(request: &str, task: &str, title: &str) -> RequestScript {
    RequestScript {
        request_id: request.into(),
        tasks: vec![TaskScript { task_id: task.into(), title: title.into(), on_start: vec![], on_feedback: vec![], cleans_up: true, dependencies: vec![], priority: None, model: None }],
        answer: None,
        headline: None,
        summary: None,
        title: None,
    }
}

/// Writes the script, then sends the request from the phone.
pub fn request(report: &mut Report, fakes: &Fakes, user: &User, script: &RequestScript, text: &str) -> Result<()> {
    fakes.write_script(script)?;
    report.step(&format!("the phone sends the request “{text}”"), || user.request(&script.request_id, text).map(drop))
}

/// The session of `task`'s node, once its native Claude runs with its bearer and took its mission.
pub fn launched(report: &mut Report, fakes: &Fakes, task: &str) -> Result<Session> {
    wait(report, &format!("the orchestrator creates {task} and Alondra starts its node through the SDK host"), || {
        let session = fakes.session_of(task)?;
        let seen = match &session {
            Some(session) => format!("session {} is {:?}, took {} inputs", session.short, session.state, fakes.received(&session.short)?.len()),
            None => format!("no session; orchestrator did {:?}", fakes.orchestrator_actions()?.last().map(|a| a["text"].clone())),
        };
        let took = |session: &Session| fakes.received(&session.short).is_ok_and(|inputs| !inputs.is_empty());
        Ok((session.filter(|session| session.running() && session.capability().is_some() && took(session)), seen))
    })
}

/// Waits until the task's view reads `state`.
pub fn state(report: &mut Report, user: &User, task: &str, state: &str) -> Result<()> {
    wait(report, &format!("{task} reads “{state}”"), || {
        let seen = user.task_state(task)?;
        Ok(((seen == state).then_some(()), seen))
    })
}

/// Has the session take `steps` in its turn; each must be taken.
pub fn act(report: &mut Report, fakes: &Fakes, session: &Session, what: &str, steps: &[Step]) -> Result<()> {
    report.step(what, || {
        for outcome in native::order(fakes, session, &Order { steps: steps.to_vec(), end_turn: false })? {
            if !outcome.ok {
                bail!("{} was refused: {}", serde_json::to_string(&outcome.step)?, outcome.text);
            }
        }
        Ok(())
    })
}

/// Has the session take one step that Alondra must refuse; its refusal text must mention `expected`.
pub fn refused(report: &mut Report, fakes: &Fakes, session: &Session, what: &str, step: &Step, expected: &str) -> Result<String> {
    report.step(what, || {
        let outcome = native::order(fakes, session, &Order { steps: vec![step.clone()], end_turn: false })?.pop().context("no outcome")?;
        if outcome.ok {
            bail!("{} was taken: {}", serde_json::to_string(step)?, outcome.text);
        }
        if !outcome.text.contains(expected) {
            bail!("refused without naming “{expected}”: {}", outcome.text);
        }
        Ok(outcome.text)
    })
}

/// Whether the session took an input that holds `text`.
pub fn told(fakes: &Fakes, session: &Session, text: &str) -> Result<(Option<()>, String)> {
    let received = fakes.received(&session.short)?;
    let found = received.iter().any(|input| input.text.contains(text));
    Ok((found.then_some(()), format!("{:?}", received.iter().map(|input| &input.text).collect::<Vec<_>>())))
}

/// The node's review card in Inbox and its view.
pub fn review_card(report: &mut Report, user: &User, session: &Session) -> Result<(String, Value)> {
    let prefix = format!("result-{}-", session.short);
    let card = wait(report, "the review card waits in Inbox", || {
        let inbox = user.inbox()?;
        Ok((inbox.iter().rev().find(|id| id.starts_with(&prefix)).cloned(), format!("{inbox:?}")))
    })?;
    let view = user.view(&format!("card:{card}"))?;
    report.snapshot(&format!("card-{card}"), &view)?;
    Ok((card, view))
}

/// Presses the card's action whose id starts with `prefix`.
pub fn press(report: &mut Report, user: &User, card: &Value, prefix: &str, option: Option<&str>, text: Option<&str>, what: &str) -> Result<Value> {
    let action = user.action(card, prefix).with_context(|| format!("the card has no enabled {prefix} action"))?;
    let id = action["id"].as_str().context("action id")?.to_owned();
    let option = option.map(str::to_owned).or_else(|| action["option_id"].as_str().map(str::to_owned));
    report.step(what, || user.press(&id, option.as_deref(), text))
}

/// The fake GitHub merged PR `number` at `head` with squash.
pub fn merged(report: &mut Report, fakes: &Fakes, number: u64, head: &str) -> Result<()> {
    wait(report, &format!("Alondra merges PR #{number} at its head with squash"), || {
        let pr = fakes.pr(number)?.with_context(|| format!("no PR {number}"))?;
        let merge = fakes.calls()?.into_iter().any(|call| {
            call.tool == "gh" && call.args.first().is_some_and(|a| a == "pr") && call.args.get(1).is_some_and(|a| a == "merge")
                && call.args.iter().any(|a| a == "--squash") && call.args.iter().any(|a| a == head)
        });
        Ok(((pr.state == "MERGED" && merge).then_some(()), format!("PR is {}, merge call: {merge}", pr.state)))
    })
}

/// Whether the Mac's sidebar lists the task among those whose node works on them.
pub fn in_progress(user: &User, task: &str) -> Result<bool> {
    let sidebar = user.view_on("sidebar", "mac")?;
    Ok(sidebar["sections"].as_array().into_iter().flatten()
        .filter(|section| section["id"].as_str().is_some_and(|id| id.starts_with("project:")))
        .flat_map(|section| section["items"].as_array().into_iter().flatten())
        .any(|item| item["id"] == task))
}

/// Waits until Alondra ended the session's native Claude, through its host, and its task is no
/// longer in progress.
pub fn ended(report: &mut Report, fakes: &Fakes, user: &User, session: &Session) -> Result<()> {
    wait(report, &format!("Alondra ends node {}'s native Claude and its task leaves those in progress", session.short), || {
        let now = fakes.session(&session.short)?;
        let listed = match now.task_id.as_deref() {
            Some(task) => in_progress(user, task)?,
            None => false,
        };
        Ok(((!now.running() && !listed).then_some(()), format!("session {:?}, alive {}, in progress {listed}", now.state, now.alive())))
    })
}

/// After the merge: the node cleans up when asked, Alondra ends its session and removes its
/// worktree, and the task finishes.
pub fn closed(report: &mut Report, fakes: &Fakes, user: &User, session: &Session, task: &str, card: &str) -> Result<()> {
    wait(report, "Alondra asks the node to clean up and it calls cleanup_done", || {
        let asked = fakes.received(&session.short)?.iter().any(|input| input.text.contains("cleanup_done") || input.text.contains("is finished and is closing"));
        let done = fakes.session_outcomes(&session.short)?.iter().any(|o| o.step == Step::Cleanup && o.ok);
        Ok(((asked && done).then_some(()), format!("asked: {asked}, cleanup_done taken: {done}")))
    })?;
    ended(report, fakes, user, session)?;
    state(report, user, task, FINISHED)?;
    report.step("the card leaves Inbox and the task's worktree is gone", || {
        let inbox = user.inbox()?;
        if inbox.iter().any(|id| id == card) {
            bail!("{card} is still in Inbox: {inbox:?}");
        }
        if session.cwd.exists() {
            bail!("{} is still there", session.cwd.display());
        }
        Ok(())
    })
}

/// The commit the node's worktree is at.
pub fn head(session: &Session) -> Result<String> {
    crate::fake::git(&session.cwd, &["rev-parse", "HEAD"])
}

/// The whole cycle from a running node to a finished task, with PR `number`.
pub fn deliver_merge_close(report: &mut Report, fakes: &Fakes, user: &User, session: &Session, task: &str, number: u64) -> Result<()> {
    state(report, user, task, WORKING)?;
    act(report, fakes, session, &format!("the node commits, pushes, opens PR #{number}, registers it and delivers"), &Step::work(number, &format!("{task}.txt")))?;
    let (card, view) = review_card(report, user, session)?;
    state(report, user, task, IN_REVIEW)?;
    let head = head(session)?;
    press(report, user, &view, "merge:", None, None, &format!("the user merges PR #{number} from the card"))?;
    merged(report, fakes, number, &head)?;
    closed(report, fakes, user, session, task, &card)
}

/// The card waiting in Inbox whose id starts with `prefix`.
pub fn card_in_inbox(user: &User, prefix: &str) -> Result<(Option<String>, String)> {
    let inbox = user.inbox()?;
    Ok((inbox.iter().find(|id| id.starts_with(prefix)).cloned(), format!("{inbox:?}")))
}

/// The enabled action of the screen of the task `task` whose id starts with `prefix`.
pub fn task_action(user: &User, task: &str, prefix: &str) -> Result<Option<Value>> {
    Ok(user.action(&user.view(&format!("task:{task}"))?, prefix))
}

/// The enabled «Interrupt» the screen of the task `task` offers.
pub fn interrupt_of(user: &User, task: &str) -> Result<Option<Value>> {
    task_action(user, task, "interrupt:")
}

/// The node that works on the task titled `title`: the one its screen's «Message» writes to.
pub fn node_of(user: &User, title: &str) -> Result<Option<String>> {
    let mut rows = vec![];
    collect(&user.view("tasks")?, &mut |item| item.pointer("/link/key").and_then(Value::as_str).is_some_and(|key| key.starts_with("task:"))
        && item["title"] == title, &mut rows);
    let Some(task) = rows.first().and_then(|row| row["id"].as_str()) else { return Ok(None) };
    Ok(task_action(user, task, "msg:")?.and_then(|message| {
        let id = message["id"].as_str()?.strip_prefix("msg:")?;
        id.split_once('@').map(|(node, _)| node.to_owned())
    }))
}
