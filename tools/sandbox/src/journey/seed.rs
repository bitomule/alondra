//! A varied world for the tester agent to look at: tasks in every state a user meets, a node whose
//! session closed, a Mac 2 with work of its own, and a pairing request waiting on Mac 1.
use anyhow::{Result, bail};

use super::{Report, User, steps::*};
use crate::{
    fake::{
        Fakes,
        script::{RequestScript, Step},
    },
    phone::Phone,
    sandbox::Sandbox,
};

fn reworking(request: &str, task: &str, title: &str, pr: u64, file: &str) -> RequestScript {
    let mut script = task_script(request, task, title);
    script.tasks[0].on_feedback = vec![
        Step::Commit { file: file.into(), text: "reworked after the review\n".into() },
        Step::Push,
        Step::Deliver { need: format!("Review the reworked {file}"), pr: Some(pr), is_final: true },
    ];
    script
}

pub fn run(sandbox: &Sandbox) -> Result<()> {
    let fakes = Fakes::open(&sandbox.dir)?;
    let mut report = Report::new(sandbox.dir.join("seed"))?;
    let user = User::paired(sandbox, "seed", 1, false)?;
    report.case("seed", "A world to explore", |report| {
        request(report, &fakes, &user, &task_script("req-working", "t-working", "Write the release notes"), "Write the release notes")?;
        launched(report, &fakes, "t-working")?;

        request(report, &fakes, &user, &task_script("req-question", "t-question", "Choose a name for the CLI flag"), "Choose a name for the CLI flag")?;
        let asking = launched(report, &fakes, "t-question")?;
        state(report, &user, "t-question", WORKING)?;
        let ask = Step::Ask { id: "q-flag".into(), need: "Pick the name of the new flag".into(), options: vec!["--quiet".into(), "--silent".into()] };
        act(report, &fakes, &asking, "the node asks", &[ask])?;

        request(report, &fakes, &user, &reworking("req-review", "t-review", "Add a greeting file", 41, "greeting.txt"), "Add a greeting file")?;
        let reviewing = launched(report, &fakes, "t-review")?;
        state(report, &user, "t-review", WORKING)?;
        act(report, &fakes, &reviewing, "the node delivers PR #41", &Step::work(41, "greeting.txt"))?;
        review_card(report, &user, &reviewing)?;

        request(report, &fakes, &user, &task_script("req-done", "t-done", "Fix the typo in the README"), "Fix the typo in the README")?;
        let finished = launched(report, &fakes, "t-done")?;
        deliver_merge_close(report, &fakes, &user, &finished, "t-done", 42)?;

        let mut after_merge = task_script("req-after-merge", "t-after-merge", "Publish the release once it is merged");
        after_merge.tasks[0].cleans_up = false;
        request(report, &fakes, &user, &after_merge, "Publish the release once it is merged")?;
        let publishing = launched(report, &fakes, "t-after-merge")?;
        state(report, &user, "t-after-merge", WORKING)?;
        let declare = Step::PostMergeWork { reason: Some("release.sh publishes 0.3.0".into()) };
        act(report, &fakes, &publishing, "the node says it publishes the release after the merge", &[declare])?;
        act(report, &fakes, &publishing, "the node delivers PR #44", &Step::work(44, "release.txt"))?;
        let (_, view) = review_card(report, &user, &publishing)?;
        let head = head(&publishing)?;
        press(report, &user, &view, "merge:", None, None, "the user merges PR #44 from the card")?;
        merged(report, &fakes, 44, &head)?;

        request(report, &fakes, &user, &task_script("req-stopped", "t-stopped", "Migrate the settings file"), "Migrate the settings file")?;
        let stopped = launched(report, &fakes, "t-stopped")?;
        state(report, &user, "t-stopped", WORKING)?;
        act(report, &fakes, &stopped, "that node's native Claude crashes, so the task asks how to go on", &[Step::Die])?;

        if sandbox.mac_count() >= 2 {
            let other = User::paired(sandbox, "seed", 2, false)?;
            request(report, &fakes, &other, &reworking("req-mac2", "t-mac2", "Profile the start-up time", 43, "profile.txt"), "Profile the start-up time")?;
            let mac2 = launched(report, &fakes, "t-mac2")?;
            state(report, &other, "t-mac2", WORKING)?;
            act(report, &fakes, &mac2, "Mac 2's node delivers PR #43", &Step::work(43, "profile.txt"))?;
        }

        report.step("a phone asks to pair with Mac 1 and waits for approval", || {
            let phone = Phone::open(sandbox, "visitor")?;
            phone.start_direct(&sandbox.mac(1)?).map(drop)
        })
    });
    let summary = std::fs::read_to_string(sandbox.dir.join("seed/journeys.md"))?;
    if summary.contains("❌") {
        bail!("seeding failed:\n{summary}");
    }
    println!("Seeded: working, question, review, finished, after-merge and interrupted tasks on Mac 1, a review on Mac 2, a pairing request on Mac 1.");
    Ok(())
}
