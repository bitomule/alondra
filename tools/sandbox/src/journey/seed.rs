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

fn outcome_card(report: &mut Report, user: &User, id: &str, what: &str) -> Result<()> {
    wait(report, what, || {
        let (card, seen) = card_in_inbox(user, id)?;
        Ok((card.filter(|card| card == id).map(drop), seen))
    })
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

        request(report, &fakes, &user, &task_script("req-documents", "t-documents", "Research how other apps index a library"), "Research how other apps index a library")?;
        let researching = launched(report, &fakes, "t-documents")?;
        state(report, &user, "t-documents", WORKING)?;
        let documents = [
            Step::Commit { file: "research-competitors.md".into(), text: "# How other apps index a library\n\n## Photos\n\nIndexes overnight while charging.\n\n## Lightroom\n\nIndexes on import.\n".into() },
            Step::Commit { file: "research-measurements.md".into(), text: "# Measurements\n\n| Library | Time |\n|---|---|\n| 10 000 photos | 4 min |\n".into() },
            Step::Call { tool: "ask".into(), arguments: serde_json::json!({
                "interaction_id": "q-documents", "scope": {"kind": "task", "task_id": "t-documents"}, "recipient": {"kind": "user"}, "blocking": true,
                "need": "Choose how Redraft indexes the whole library",
                "why": "The research found two ways other apps do it; the index starts once you pick one.",
                "options": [
                    {"id": "overnight", "label": "Overnight while charging", "description": "Like Photos: no cost while you use the iPhone, done in a night"},
                    {"id": "import", "label": "On import", "description": "Like Lightroom: each new photo is indexed at once"},
                ],
                "recommended": "overnight",
                "content": {"kind": "report", "body": "## Summary\n\n- **Viable.** Every photo keeps a 360 px copy on the iPhone.\n- How other apps do it: research-competitors.md\n- Measurements: research-measurements.md"},
            }) },
        ];
        act(report, &fakes, &researching, "the node asks with the two documents it cites", &documents)?;

        request(report, &fakes, &user, &reworking("req-review", "t-review", "Add a greeting file", 41, "greeting.txt"), "Add a greeting file")?;
        let reviewing = launched(report, &fakes, "t-review")?;
        state(report, &user, "t-review", WORKING)?;
        act(report, &fakes, &reviewing, "the node delivers PR #41", &Step::work(41, "greeting.txt"))?;
        review_card(report, &user, &reviewing)?;

        let mut done = task_script("req-done", "t-done", "Fix the typo in the README");
        done.summary = Some("The README spells the install command right again: PR #42 merged.".into());
        request(report, &fakes, &user, &done, "Fix the typo in the README")?;
        let finished = launched(report, &fakes, "t-done")?;
        deliver_merge_close(report, &fakes, &user, &finished, "t-done", 42)?;
        outcome_card(report, &user, "request-done-req-done", "the orchestrator closes the finished request and Alondra tells the user it is done")?;

        let answered = RequestScript {
            request_id: "req-answered".into(), tasks: vec![],
            answer: Some("Yes. Every image is encrypted on the iPhone before it is uploaded, and R2 only keeps the encrypted bytes.".into()),
            headline: Some("Yes: images are already encrypted before they leave the iPhone".into()),
            summary: None,
        };
        request(report, &fakes, &user, &answered, "Are images encrypted before they reach R2?")?;
        outcome_card(report, &user, "req-answered-answer", "the orchestrator answers without tasks and Alondra writes the answer's card")?;

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
        act(report, &fakes, &stopped, "that node's native Claude crashes, and its task goes on by itself in a resumed session", &[Step::Die])?;

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
    println!("Seeded: working, question, research with documents, review, finished, after-merge and interrupted tasks on Mac 1, an answered request, a review on Mac 2, a pairing request on Mac 1.");
    Ok(())
}
