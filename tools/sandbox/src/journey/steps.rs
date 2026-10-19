//! The steps journeys share: waits on what other processes apply, and the milestones of a task.
use std::time::Instant;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::{PATIENCE, Report, User, user::mentions};
use crate::fake::{
    Fakes,
    node::{self, Node, State},
    script::{RequestScript, Step, TaskScript},
};

pub const WORKING: &str = "Working";
pub const IN_REVIEW: &str = "Ready for review";
pub const FINISHED: &str = "Finished";
pub const CANCELLED: &str = "Cancelled";

/// A step that waits until `probe` finds what it wants; on timeout it says what it saw last.
pub fn wait<T>(report: &mut Report, what: &str, mut probe: impl FnMut() -> Result<(Option<T>, String)>) -> Result<T> {
    report.step(what, || {
        let start = Instant::now();
        loop {
            let (found, seen) = probe()?;
            if let Some(found) = found {
                return Ok(found);
            }
            if start.elapsed() > PATIENCE {
                bail!("not within {} s; last seen: {seen}", PATIENCE.as_secs());
            }
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    })
}

pub fn task_script(request: &str, task: &str, title: &str) -> RequestScript {
    RequestScript {
        request_id: request.into(),
        tasks: vec![TaskScript { task_id: task.into(), title: title.into(), on_start: vec![], on_feedback: vec![], cleans_up: true }],
        answer: None,
    }
}

/// Writes the script, then sends the request from the phone.
pub fn request(report: &mut Report, fakes: &Fakes, user: &User, script: &RequestScript, text: &str) -> Result<()> {
    fakes.write_script(script)?;
    report.step(&format!("the phone sends the request “{text}”"), || user.request(&script.request_id, text).map(drop))
}

/// The node Alondra launched for `task`, once Alondra bound it.
pub fn launched(report: &mut Report, fakes: &Fakes, task: &str) -> Result<Node> {
    wait(report, &format!("the orchestrator creates {task} and Alondra launches its node"), || {
        let node = fakes.node_of(task)?;
        let seen = match &node {
            Some(node) => format!("node {} is {:?}", node.short, node.state),
            None => format!("no node; orchestrator did {:?}", fakes.orchestrator_actions()?.last().map(|a| a["text"].clone())),
        };
        Ok((node.filter(|node| node.state == State::Running && bound(node)), seen))
    })
}

fn bound(node: &Node) -> bool {
    std::fs::read(&node.settings)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
        .is_some_and(|settings| settings["env"].get("ALONDRA_DOMAIN_CAPABILITY").is_some())
}

/// Waits until the task's view reads `state`.
pub fn state(report: &mut Report, user: &User, task: &str, state: &str) -> Result<()> {
    wait(report, &format!("{task} reads “{state}”"), || {
        let seen = user.task_state(task)?;
        Ok(((seen == state).then_some(()), seen))
    })
}

/// Runs `steps` on the node; each must be taken.
pub fn act(report: &mut Report, fakes: &Fakes, node: &Node, what: &str, steps: &[Step]) -> Result<()> {
    report.step(what, || {
        for step in steps {
            let outcome = node::act(fakes, node, step)?;
            if !outcome.ok {
                bail!("{} was refused: {}", serde_json::to_string(step)?, outcome.text);
            }
        }
        node::finish_turn(fakes, node)
    })
}

/// Runs one step that Alondra must refuse; its refusal text must mention `expected`.
pub fn refused(report: &mut Report, fakes: &Fakes, node: &Node, what: &str, step: &Step, expected: &str) -> Result<String> {
    report.step(what, || {
        let outcome = node::act(fakes, node, step)?;
        if outcome.ok {
            bail!("{} was taken: {}", serde_json::to_string(step)?, outcome.text);
        }
        if !outcome.text.contains(expected) {
            bail!("refused without naming “{expected}”: {}", outcome.text);
        }
        Ok(outcome.text)
    })
}

/// The node's review card in Inbox and its view.
pub fn review_card(report: &mut Report, user: &User, node: &Node) -> Result<(String, Value)> {
    let prefix = format!("result-{}-", node.short);
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

/// After the merge: the node cleans up when asked, `claude rm` removes it, and the task finishes.
pub fn closed(report: &mut Report, fakes: &Fakes, user: &User, node: &Node, task: &str, card: &str) -> Result<()> {
    wait(report, "Alondra asks the node to clean up and it calls cleanup_done", || {
        let asked = fakes.pushed(&node.short)?.iter().any(|push| push.text.as_deref().is_some_and(|t| t.contains("cleanup_done")));
        let done = fakes.outcomes(&node.short)?.iter().any(|o| o.step == Step::Cleanup && o.ok);
        Ok(((asked && done).then_some(()), format!("asked: {asked}, cleanup_done taken: {done}")))
    })?;
    wait(report, &format!("Alondra runs claude rm {}", node.short), || {
        let rm = fakes.calls()?.iter().any(|call| call.tool == "claude" && call.args == ["rm", node.short.as_str()]);
        let state = fakes.node(&node.short)?.state;
        Ok(((rm && state == State::Removed).then_some(()), format!("rm called: {rm}, node {state:?}")))
    })?;
    state(report, user, task, FINISHED)?;
    report.step("the card leaves Inbox and the node leaves Fleet", || {
        let inbox = user.inbox()?;
        if inbox.iter().any(|id| id == card) {
            bail!("{card} is still in Inbox: {inbox:?}");
        }
        let fleet = user.view("fleet")?;
        if mentions(&fleet, &node.short) || mentions(&fleet, &node.session_id) {
            bail!("Fleet still lists {}", node.short);
        }
        Ok(())
    })
}

/// The commit the node's worktree is at.
pub fn head(node: &Node) -> Result<String> {
    crate::fake::git(node.worktree.as_deref().unwrap_or(&node.cwd), &["rev-parse", "HEAD"])
}

/// The whole cycle from a bound node to a finished task, with PR `number`.
pub fn deliver_merge_close(report: &mut Report, fakes: &Fakes, user: &User, node: &Node, task: &str, number: u64) -> Result<()> {
    state(report, user, task, WORKING)?;
    act(report, fakes, node, &format!("the node commits, pushes, opens PR #{number}, registers it and delivers"), &Step::work(number, &format!("{task}.txt")))?;
    let (card, view) = review_card(report, user, node)?;
    state(report, user, task, IN_REVIEW)?;
    let head = head(node)?;
    press(report, user, &view, "merge:", None, None, &format!("the user merges PR #{number} from the card"))?;
    merged(report, fakes, number, &head)?;
    closed(report, fakes, user, node, task, &card)
}
