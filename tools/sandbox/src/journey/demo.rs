//! The world the demo captures show: invented projects with one task in each state a reader meets
//! (working, waiting for another, a question, a plan to approve, a PR to review), and no data of
//! anyone's own. Everything is created by one request, so the inbox carries one notice, not one per task.
use anyhow::{Result, bail};

use super::{Report, User, steps::*};
use crate::{
    fake::{
        Fakes,
        script::{RequestScript, Step, TaskScript},
    },
    sandbox::Sandbox,
};

const PLAN: &str = "## Phase 1: keep entries on the device\n\n- [ ] Store the last 200 entries on the device\n- [ ] Open them without a connection\n\n## Phase 2: queue edits\n\n- [ ] Queue edits made offline, with the time of each\n- [ ] Send them in order when the connection is back\n\n## Phase 3: tell the user\n\n- [ ] Show a banner while the queue is not empty\n- [ ] Tests with the connection dropping mid-send";

const PLAN_REPORT: &str = "## Offline mode for Lumen\n\nEntries are already stored on the device; only writing needs the network. The queue keeps each edit with the time it was made and replays them in order, so two devices editing the same entry end with the later edit.\n\n| Risk | What covers it |\n|---|---|\n| Two devices edit the same entry | The later edit wins; both are kept in the history |\n| The connection drops mid-send | The queue keeps the edit until the server confirms it |";

const APPEARANCE: &str = "import SwiftUI\n\n/// The appearance the user picked in Settings.\nenum AppearanceChoice: String, CaseIterable, Identifiable {\n    case system, light, dark\n\n    var id: String { rawValue }\n\n    var colorScheme: ColorScheme? {\n        switch self {\n        case .system: nil\n        case .light: .light\n        case .dark: .dark\n        }\n    }\n}\n\nstruct AppearanceSettings: View {\n    @AppStorage(\"appearance\") private var choice = AppearanceChoice.system\n\n    var body: some View {\n        Picker(\"Appearance\", selection: $choice) {\n            ForEach(AppearanceChoice.allCases) { option in\n                Text(option.rawValue.capitalized).tag(option)\n            }\n        }\n        .pickerStyle(.segmented)\n    }\n}\n";

const APPEARANCE_TESTS: &str = "import Testing\n\n@Suite struct AppearanceChoiceTests {\n    @Test func systemFollowsTheDevice() {\n        #expect(AppearanceChoice.system.colorScheme == nil)\n    }\n\n    @Test func darkForcesTheDarkScheme() {\n        #expect(AppearanceChoice.dark.colorScheme == .dark)\n    }\n}\n";

fn task(id: &str, title: &str, model: &str, dependencies: &[&str]) -> TaskScript {
    TaskScript {
        task_id: id.into(), title: title.into(), on_start: vec![], on_feedback: vec![], cleans_up: true,
        dependencies: dependencies.iter().map(|d| (*d).into()).collect(), priority: None, model: Some(model.into()),
    }
}

pub fn run(sandbox: &Sandbox) -> Result<()> {
    let fakes = Fakes::open(&sandbox.dir)?;
    let mut report = Report::new(sandbox.dir.join("demo"))?;
    let user = User::paired(sandbox, "demo", 1, false)?;
    report.case("demo", "A world to photograph", |report| {
        let mut notes = task("t-notes", "2.4 release notes", "sonnet", &[]);
        notes.on_start = vec![Step::Speak {
            said: "Drafting the release notes while the checks run".into(), behind: Some("make test".into()), listed: Some("working".into()),
        }];
        let script = RequestScript {
            request_id: "req-ship".into(),
            tasks: vec![
                notes,
                task("t-release", "Publish 2.4", "sonnet", &["t-notes"]),
                task("t-dark", "Dark mode", "sonnet", &[]),
                task("t-offline", "Offline mode", "opus", &[]),
                task("t-refresh", "Orders refresh rate", "sonnet", &[]),
            ],
            answer: None, headline: None, summary: None, title: Some("Ship Lumen 2.4".into()),
        };
        request(report, &fakes, &user, &script, "Ship Lumen 2.4: release notes, dark mode, offline mode and the Harbor refresh rate")?;
        launched(report, &fakes, "t-notes")?;
        state(report, &user, "t-notes", "In the background")?;
        wait(report, "t-release waits for t-notes", || {
            let seen = user.task_state("t-release").unwrap_or_default();
            Ok(((seen == "Waiting").then_some(()), seen))
        })?;

        let refreshing = launched(report, &fakes, "t-refresh")?;
        state(report, &user, "t-refresh", WORKING)?;
        let ask = Step::Call { tool: "ask".into(), arguments: serde_json::json!({
            "interaction_id": "q-refresh", "scope": {"kind": "task", "task_id": "t-refresh"}, "recipient": {"kind": "user"}, "blocking": true,
            "title": "Refresh interval", "need": "How often should Harbor refresh the order list?",
            "why": "The list refreshes in the background; the interval decides how fresh it is and how much battery it costs.",
            "options": [
                {"id": "minute", "label": "Every minute", "description": "New orders show almost at once; the battery drains faster"},
                {"id": "five", "label": "Every 5 minutes", "description": "A new order can take up to 5 minutes to show; much lighter on the battery"},
            ],
            "recommended": "five",
        }) };
        act(report, &fakes, &refreshing, "the node asks which interval", &[ask])?;

        let planning = launched(report, &fakes, "t-offline")?;
        state(report, &user, "t-offline", WORKING)?;
        let plan = Step::Call { tool: "present_result".into(), arguments: serde_json::json!({
            "task_id": "t-offline", "interaction_id": "plan-offline", "is_final": true,
            "title": "Plan for offline mode", "need": "Approve the plan for offline mode",
            "body": "Three phases: entries kept on the device, a queue of offline edits, and a banner with tests.",
            "targets": [{"kind": "text", "text": "Plan for offline mode"}],
            "content": {"kind": "report", "body": PLAN_REPORT},
            "plan": PLAN,
        }) };
        act(report, &fakes, &planning, "the node proposes a plan in phases", &[plan])?;

        let reviewing = launched(report, &fakes, "t-dark")?;
        state(report, &user, "t-dark", WORKING)?;
        let work = [
            Step::Commit { file: "Theme.swift".into(), text: APPEARANCE.into() },
            Step::Commit { file: "Tests.swift".into(), text: APPEARANCE_TESTS.into() },
            Step::Push,
            Step::OpenPr { number: 12, draft: false, checks: crate::fake::Checks::Success },
            Step::RegisterPr { number: 12 },
            Step::Deliver { need: "Review the dark mode".into(), pr: Some(12), is_final: true },
        ];
        act(report, &fakes, &reviewing, "the node delivers PR #12 with its tests", &work)?;
        review_card(report, &user, &reviewing).map(drop)
    });
    let summary = std::fs::read_to_string(sandbox.dir.join("demo/journeys.md"))?;
    if summary.contains("❌") {
        bail!("seeding the demo failed:\n{summary}");
    }
    println!("Seeded the demo: a task working, one waiting for it, a question, a plan in phases and a PR to review.");
    Ok(())
}
