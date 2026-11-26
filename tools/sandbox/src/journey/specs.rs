//! A project with specs (`SANDBOX` opt-in `specs`): its repository turns specs on, a task in stages
//! starts as its author, which proposes the specs of its plan. What follows is the user's, on the
//! plan's card: the author presents its plan with `alondra-sandbox fake node t-specs --step
//! '[{"do":"own_turn","said":"Plan"},{"do":"leave_plan_mode","points":1}]'`.
use anyhow::{Context, Result, bail};
use serde_json::json;

use super::{Report, User, steps::*};
use crate::{fake::{Fakes, script::Step}, sandbox::Sandbox};

const SHOT: &str = "shots/refund-sheet.png";
const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n\n[decisions]\nenabled = true\n";
const BOUNDARY: &str = "id = \"billing\"\nowns = [\"src/billing/**\"]\n";
const REFUNDS: &str = "@spec:billing.refunds\nFeature: Refunds return the money\n  A paid order is refunded whole, once.\n\n  @id:full\n  Scenario: Full refund\n    Given a paid order of 30 EUR\n    When the order is refunded\n    Then the customer receives 30 EUR\n\n  @id:stands-alone @boundary:dependencies\n  Scenario: Billing uses no other component\n    Given the billing code\n    When its dependencies are listed\n    Then none is another component\n";
const FLOW: &str = "flowchart LR\n  Order[Paid order] --> Check{Within what is left?}\n  Check -->|yes| Refund[Refund]\n  Check -->|no| Refuse[Refusal]\n";
const PARTIAL: &str = "@spec:billing.partial\n@decision:refund-what-is-left\n@diagram:refund-flow.mmd\nFeature: Partial refunds\n  A customer gets back any amount up to what is left of the order, and nothing more.\n\n  @id:part @mockup:refund-sheet.png\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n    And 20 EUR are left to refund\n\n  @id:more-than-left\n  Scenario: More than what is left\n    Given a paid order of 30 EUR with 18 EUR refunded\n    When 20 EUR are refunded\n    Then the refund is refused with \"Only 12.00 EUR is left to refund\"\n\n  @id:at-once\n  Scenario: Two refunds at the same time\n    Given a paid order of 30 EUR\n    When 20 EUR and 15 EUR are refunded at the same time\n    Then one refund is made and the other is refused with \"Only 10.00 EUR is left to refund\"\n";
const DECISION: &str = "+++\nid = \"refund-what-is-left\"\ntitle = \"A refund never exceeds what is left of its order\"\nstatus = \"proposed\"\ndate = \"2026-11-26\"\nsupersedes = []\nsuperseded_by = \"\"\n+++\n\n## Context\n\nSupport agents refund from two Macs at once.\n\n## Decision\n\nEvery refund is checked against what is left of its order, one at a time per order.\n\n## Consequences\n\nA second refund of the same order waits for the first.\n\n## Confirmation\n\nThe scenario billing.partial/at-once.\n";

fn git(dir: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git").current_dir(dir).args(["-c", "user.name=Ana", "-c", "user.email=ana@example.com"]).args(args).output()?;
    if !output.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

/// The sandbox's repository turns specs on, with a component `billing` and its spec of refunds.
fn project(fakes: &Fakes) -> Result<()> {
    let repo = fakes.repo();
    for (path, bytes) in [("alondra.toml", SETTINGS.as_bytes().to_vec()), ("specs/billing/boundary.toml", BOUNDARY.as_bytes().to_vec()),
        ("specs/billing/refunds.feature", REFUNDS.as_bytes().to_vec()), (SHOT, super::png(1170, 2532))] {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().context("a file has a folder")?)?;
        std::fs::write(file, bytes)?;
    }
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-q", "-m", "Turn specs on"])?;
    git(&repo, &["push", "-q", "origin", "HEAD"])
}

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "specs", 1, false)) else {
        return;
    };
    report.case("SPECS1", "In a project with specs, the author of a task in stages proposes the specs of its plan", |report| {
        report.step("the repository turns specs on", || project(fakes))?;
        let mut script = task_script("req-specs", "t-specs", "Refund part of an order");
        script.tasks[0].staged = true;
        request(report, fakes, &user, &script, "Let customers get part of an order refunded")?;
        let author = launched(report, fakes, "t-specs")?;
        let files = json!([
            {"path": "specs/billing/partial.feature", "text": PARTIAL},
            {"path": "docs/decisions/refund-what-is-left.md", "text": DECISION},
            {"path": "specs/billing/refund-flow.mmd", "text": FLOW},
            {"path": "specs/billing/refund-sheet.png", "source": SHOT},
        ]);
        let propose = Step::Call { tool: "propose_specs".into(), arguments: json!({"task_id": "t-specs", "level": "light", "files": files}) };
        act(report, fakes, &author, "the author proposes the plan's specs", &[propose])
    });
}
