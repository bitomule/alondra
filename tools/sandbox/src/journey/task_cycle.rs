//! A task's life through the relay: request, task, node, delivery, card, merge and close, and the
//! refusals Alondra owes on the way.
use anyhow::{Context, bail};

use super::{Report, User, steps::*};
use crate::{
    fake::{Checks, Fakes, script::Step},
    sandbox::Sandbox,
};

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 through the relay", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    report.case("TC1", "A request becomes a task whose node delivers a PR; merging it closes the task", |report| {
        request(report, fakes, &user, &task_script("req-t1", "t-t1", "Add a greeting file"), "Add a greeting file")?;
        let node = launched(report, fakes, "t-t1")?;
        deliver_merge_close(report, fakes, &user, &node, "t-t1", 1)
    });
    report.case("TC2", "The node asks; the user answers on the card and the answer reaches the node", |report| {
        request(report, fakes, &user, &task_script("req-t2", "t-t2", "Name the greeting file"), "Name the greeting file")?;
        let node = launched(report, fakes, "t-t2")?;
        state(report, &user, "t-t2", WORKING)?;
        let ask = Step::Ask { id: "q-t2".into(), need: "Pick how to name the file".into(), options: vec!["Short name".into(), "Long name".into()] };
        act(report, fakes, &node, "the node asks with two options", &[ask])?;
        wait(report, "the question waits in Inbox", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().any(|id| id == "q-t2").then_some(()), format!("{inbox:?}")))
        })?;
        state(report, &user, "t-t2", "Needs you")?;
        let card = user.view("card:q-t2")?;
        report.snapshot("card-q-t2", &card)?;
        let option = user.action_labeled(&card, "Short name").context("the card offers no “Short name”")?;
        report.step("the user picks “Short name”", || {
            user.press(option["id"].as_str().unwrap_or_default(), option["option_id"].as_str(), None)
        })?;
        wait(report, "the answer reaches the node", || {
            let pushed = fakes.pushed(&node.short)?;
            let answer = pushed.iter().find(|push| push.text.as_deref().is_some_and(|text| text.contains("Short name")));
            Ok((answer.map(drop), format!("{:?}", pushed.iter().map(|p| p.text.clone()).collect::<Vec<_>>())))
        })?;
        state(report, &user, "t-t2", WORKING)?;
        report.step("the question leaves Inbox", || {
            let inbox = user.inbox()?;
            if inbox.iter().any(|id| id == "q-t2") {
                bail!("q-t2 is still in Inbox");
            }
            Ok(())
        })
    });
    report.case("TC3", "The user asks for changes; the node reworks, delivers again and the new head is merged", |report| {
        request(report, fakes, &user, &task_script("req-t3", "t-t3", "Add a farewell file"), "Add a farewell file")?;
        let node = launched(report, fakes, "t-t3")?;
        state(report, &user, "t-t3", WORKING)?;
        act(report, fakes, &node, "the node delivers PR #3", &Step::work(3, "farewell.txt"))?;
        let (first, view) = review_card(report, &user, &node)?;
        press(report, &user, &view, "changes:", None, Some("Say goodbye in Spanish too"), "the user asks for changes with a comment")?;
        wait(report, "the comment reaches the node", || {
            let pushed = fakes.pushed(&node.short)?;
            let found = pushed.iter().any(|push| push.text.as_deref().is_some_and(|text| text.contains("Say goodbye in Spanish too")));
            Ok((found.then_some(()), format!("{} messages", pushed.len())))
        })?;
        state(report, &user, "t-t3", "Making changes")?;
        report.step("the first card leaves Inbox", || {
            if user.inbox()?.iter().any(|id| *id == first) {
                bail!("{first} is still in Inbox");
            }
            Ok(())
        })?;
        let rework = [
            Step::Commit { file: "farewell.txt".into(), text: "Goodbye. Adiós.\n".into() },
            Step::Push,
            Step::Deliver { need: "Review the farewell in two languages".into(), pr: Some(3), is_final: true },
        ];
        act(report, fakes, &node, "the node commits the change, pushes and delivers again", &rework)?;
        let (card, view) = review_card(report, &user, &node)?;
        if card == first {
            bail!("the second delivery reused the first card {first}");
        }
        let head = head(&node)?;
        press(report, &user, &view, "merge:", None, None, "the user merges the reworked PR")?;
        merged(report, fakes, 3, &head)?;
        closed(report, fakes, &user, &node, "t-t3", &card)
    });
    report.case("TC4", "The user cancels a working task; its node is removed", |report| {
        request(report, fakes, &user, &task_script("req-t4", "t-t4", "Add a draft note"), "Add a draft note")?;
        let node = launched(report, fakes, "t-t4")?;
        state(report, &user, "t-t4", WORKING)?;
        let opened = [
            Step::Commit { file: "note.txt".into(), text: "note\n".into() },
            Step::Push,
            Step::OpenPr { number: 4, draft: false, checks: Checks::Success },
            Step::RegisterPr { number: 4 },
        ];
        act(report, fakes, &node, "the node opens and registers PR #4", &opened)?;
        let task = user.view("task:t-t4")?;
        report.snapshot("task-t-t4", &task)?;
        press(report, &user, &task, "cancel:", None, None, "the user cancels the task")?;
        state(report, &user, "t-t4", CANCELLED)?;
        wait(report, &format!("Alondra removes node {}", node.short), || {
            let rm = fakes.calls()?.iter().any(|call| call.tool == "claude" && call.args == ["rm", node.short.as_str()]);
            Ok((rm.then_some(()), format!("rm called: {rm}")))
        })
        .map(drop)
    });
    report.case("TC5", "Closing a delivered PR without merging closes it on GitHub", |report| {
        request(report, fakes, &user, &task_script("req-t5", "t-t5", "Add an unwanted file"), "Add an unwanted file")?;
        let node = launched(report, fakes, "t-t5")?;
        state(report, &user, "t-t5", WORKING)?;
        act(report, fakes, &node, "the node delivers PR #5", &Step::work(5, "unwanted.txt"))?;
        let (card, view) = review_card(report, &user, &node)?;
        press(report, &user, &view, "close_unmerged:", None, None, "the user closes PR #5 without merging")?;
        wait(report, "Alondra closes PR #5 on GitHub", || {
            let pr = fakes.pr(5)?.context("no PR 5")?;
            Ok(((pr.state == "CLOSED").then_some(()), pr.state))
        })?;
        wait(report, "the card leaves Inbox", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().all(|id| *id != card).then_some(()), format!("{inbox:?}")))
        })
    });
    report.case("F1", "A final delivery with an uncommitted file is refused, naming the file", |report| {
        request(report, fakes, &user, &task_script("req-f1", "t-f1", "Add a scratch-free file"), "Add a scratch-free file")?;
        let node = launched(report, fakes, "t-f1")?;
        state(report, &user, "t-f1", WORKING)?;
        let mut work = Step::work(11, "clean.txt");
        let deliver = work.pop().expect("work ends with a delivery");
        act(report, fakes, &node, "the node opens and registers PR #11", &work)?;
        act(report, fakes, &node, "the node leaves scratch.txt uncommitted", &[Step::Dirty { file: "scratch.txt".into() }])?;
        refused(report, fakes, &node, "Alondra refuses the final delivery and names scratch.txt", &deliver, "scratch.txt")?;
        report.step("no review card appears", || {
            let prefix = format!("result-{}-", node.short);
            if user.inbox()?.iter().any(|id| id.starts_with(&prefix)) {
                bail!("a review card appeared for the refused delivery");
            }
            Ok(())
        })?;
        let fix = [Step::Commit { file: "scratch.txt".into(), text: "kept on purpose\n".into() }, Step::Push, deliver];
        act(report, fakes, &node, "the node commits the file, pushes and delivers again", &fix)?;
        review_card(report, &user, &node).map(drop)
    });
    report.case("F2", "A final delivery of a draft PR is refused", |report| {
        request(report, fakes, &user, &task_script("req-f2", "t-f2", "Add a draft file"), "Add a draft file")?;
        let node = launched(report, fakes, "t-f2")?;
        state(report, &user, "t-f2", WORKING)?;
        let opened = [
            Step::Commit { file: "draft.txt".into(), text: "draft\n".into() },
            Step::Push,
            Step::OpenPr { number: 12, draft: true, checks: Checks::Success },
            Step::RegisterPr { number: 12 },
        ];
        act(report, fakes, &node, "the node opens PR #12 as a draft and registers it", &opened)?;
        let deliver = Step::Deliver { need: "Review the draft".into(), pr: Some(12), is_final: true };
        refused(report, fakes, &node, "Alondra refuses the final delivery of the draft", &deliver, "draft").map(drop)
    });
    report.case("F3", "Failing checks send the merge back to the node; the fixed head is merged", |report| {
        request(report, fakes, &user, &task_script("req-f3", "t-f3", "Add a checked file"), "Add a checked file")?;
        let node = launched(report, fakes, "t-f3")?;
        state(report, &user, "t-f3", WORKING)?;
        let mut work = Step::work(13, "checked.txt");
        work[2] = Step::OpenPr { number: 13, draft: false, checks: Checks::Failure };
        act(report, fakes, &node, "the node delivers PR #13, whose checks fail", &work)?;
        let (first, view) = review_card(report, &user, &node)?;
        press(report, &user, &view, "merge:", None, None, "the user merges PR #13")?;
        wait(report, "Alondra hands the task back to the node, naming the failed check", || {
            let pushed = fakes.pushed(&node.short)?;
            let told = pushed.iter().any(|push| push.text.as_deref().is_some_and(|text| text.contains("build")));
            Ok((told.then_some(()), format!("{:?}", pushed.iter().filter_map(|p| p.text.clone()).collect::<Vec<_>>())))
        })?;
        report.step("nothing was merged", || {
            let pr = fakes.pr(13)?.context("no PR 13")?;
            if pr.state != "OPEN" {
                bail!("PR #13 is {}", pr.state);
            }
            Ok(())
        })?;
        state(report, &user, "t-f3", "Couldn't merge")?;
        fakes.update_pr(13, |pr| {
            if let Some(pr) = pr {
                pr.checks = Checks::Success;
            }
        })?;
        let fix = [
            Step::Commit { file: "checked.txt".into(), text: "fixed\n".into() },
            Step::Push,
            Step::Deliver { need: "Review the fixed file".into(), pr: Some(13), is_final: true },
        ];
        act(report, fakes, &node, "the node fixes it, pushes a new head and delivers again", &fix)?;
        let (card, view) = review_card(report, &user, &node)?;
        if card == first {
            bail!("the second delivery reused the first card {first}");
        }
        let head = head(&node)?;
        press(report, &user, &view, "merge:", None, None, "the user merges the fixed PR")?;
        merged(report, fakes, 13, &head)
    });
    report.case("F4", "A second PR for the same task is refused", |report| {
        request(report, fakes, &user, &task_script("req-f4", "t-f4", "Add one PR only"), "Add one PR only")?;
        let node = launched(report, fakes, "t-f4")?;
        state(report, &user, "t-f4", WORKING)?;
        let mut work = Step::work(14, "one.txt");
        work.pop();
        act(report, fakes, &node, "the node opens and registers PR #14", &work)?;
        act(report, fakes, &node, "the node opens PR #15 on the fake GitHub", &[Step::OpenPr { number: 15, draft: false, checks: Checks::Success }])?;
        refused(report, fakes, &node, "Alondra refuses to register PR #15 for the same task", &Step::RegisterPr { number: 15 }, "owns one PR").map(drop)
    });
}
