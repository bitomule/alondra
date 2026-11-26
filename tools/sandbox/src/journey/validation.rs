//! A task validated against its specs (`SANDBOX` opt-in `validation`): the author's plan is
//! approved, an implementer changes the code and says it is ready, and a new validator records each
//! check, commits the ledger and delivers. The delivery's card then shows the specs it validated.
use anyhow::{Context, Result, bail};
use serde_json::json;
use std::collections::BTreeMap;

use super::{Report, User, steps::*};
use crate::{fake::{Fakes, native::{self, Order}, script::Step}, sandbox::Sandbox};
use alondra_spec_engine as engine;

const TASK: &str = "t-validation";
const SETTINGS: &str = "format = 1\n\n[specs]\nenabled = true\n";
const BOUNDARY: &str = "id = \"billing\"\nowns = [\"src/billing/**\"]\n\n[[suite]]\nid = \"unit\"\nrun = [\"cargo\", \"test\", \"--package\", \"billing\"]\ntests = [\"src/billing/tests/*.rs\"]\n";
const BEFORE_PARTIAL: &str = "@spec:billing.partial\nFeature: Partial refunds\n  A customer gets back any amount up to what is left of the order.\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n\n  @id:more-than-left\n  Scenario: More than what is left\n    Given a paid order of 30 EUR with 18 EUR refunded\n    When 20 EUR are refunded\n    Then the refund is refused with \"Only 12.00 EUR is left to refund\"\n\n  @id:stands-alone @boundary:dependencies\n  Scenario: Billing uses no other component\n    Given the billing code\n    When its dependencies are listed\n    Then none is another component\n";
const FLOW: &str = "flowchart LR\n  Order[Paid order] --> Check{Within what is left?}\n  Check -->|yes| Refund[Refund]\n  Check -->|no| Refuse[Refusal]\n";
const SHOT: &str = "shots/refund-sheet.png";
const CAPTURE: &str = "src/billing/captures/refund-sheet.png";
const PARTIAL: &str = "@spec:billing.partial\n@diagram:refund-flow.mmd\nFeature: Partial refunds\n  A customer gets back any amount up to what is left of the order, and nothing more.\n\n  @id:part\n  Scenario: Part of an order\n    Given a paid order of 30 EUR\n    When 10 EUR are refunded\n    Then the customer receives 10 EUR\n    And 20 EUR are left to refund\n\n  @id:more-than-left\n  Scenario: More than what is left\n    Given a paid order of 30 EUR with 18 EUR refunded\n    When 20 EUR are refunded\n    Then the refund is refused with \"Only 12.00 EUR is left to refund\"\n\n  @id:sheet-shows-what-is-left @judged:capture @evidence:src/billing/captures/*.png @mockup:refund-sheet.png\n  Scenario: The refund sheet shows what is left\n    Given a paid order of 30 EUR with 10 EUR refunded\n    When the refund sheet opens\n    Then it shows \"20.00 EUR left\"\n\n  @id:stands-alone @boundary:dependencies\n  Scenario: Billing uses no other component\n    Given the billing code\n    When its dependencies are listed\n    Then none is another component\n";
const TESTS: &str = "// spec: billing.partial/part\n#[test]\nfn refunds_part_of_an_order() {\n    let mut order = Order::paid(3000);\n    assert_eq!(order.refund(1000), Ok(1000));\n    assert_eq!(order.left(), 2000);\n}\n\n// spec: billing.partial/more-than-left\n#[test]\nfn refuses_more_than_what_is_left() {\n    let mut order = Order::paid(3000);\n    order.refund(1800).unwrap();\n    assert_eq!(order.refund(2000), Err(Refused::Left(1200)));\n}\n\n// spec: billing.partial/stands-alone\n#[test]\nfn billing_uses_no_other_component() {\n    assert!(dependencies().is_empty());\n}\n";
const BEFORE: &str = "pub struct Order { paid: u32 }\n";
const AFTER: &str = "pub struct Order { paid: u32, refunded: u32 }\n\nimpl Order {\n    pub fn refund(&mut self, amount: u32) -> Result<u32, Refused> {\n        let left = self.paid - self.refunded;\n        if amount > left { return Err(Refused::Left(left)); }\n        self.refunded += amount;\n        Ok(amount)\n    }\n}\n";
const README: &str = "# Shop\n\nRefunds of part of an order.\n";

struct Tree(BTreeMap<String, Vec<u8>>);

impl engine::Files for Tree {
    fn paths(&self) -> Vec<String> {
        self.0.keys().cloned().collect()
    }

    fn read(&self, path: &str) -> Option<Vec<u8>> {
        self.0.get(path).cloned()
    }
}

/// The repository as its default branch holds it.
fn base_files() -> Tree {
    let base = [("alondra.toml", SETTINGS), ("specs/billing/boundary.toml", BOUNDARY), ("specs/billing/partial.feature", BEFORE_PARTIAL),
        ("src/billing/tests/refunds.rs", TESTS), ("src/billing/lib.rs", BEFORE)];
    let mut files: BTreeMap<String, Vec<u8>> = base.iter().map(|(path, text)| ((*path).to_owned(), text.as_bytes().to_vec())).collect();
    files.insert(SHOT.into(), super::png(1170, 2532));
    Tree(files)
}

/// What the implementer's work holds, with the approved specs Alondra wrote into it.
fn implemented_files() -> Tree {
    let mut files = base_files().0;
    for (path, text) in [("specs/billing/partial.feature", PARTIAL), ("specs/billing/refund-flow.mmd", FLOW), ("src/billing/lib.rs", AFTER), ("README.md", README)] {
        files.insert(path.into(), text.as_bytes().to_vec());
    }
    files.insert("specs/billing/refund-sheet.png".into(), super::png(1170, 2532));
    files.insert(CAPTURE.into(), super::png(1170, 2400));
    Tree(files)
}

/// The ledger `alondra spec run` and `alondra spec record` write for everything the engine lists
/// on the implementer's commit, each met.
fn ledger() -> Result<Vec<(String, String)>> {
    let tree = implemented_files();
    let project = engine::load(&tree).map_err(|problems| anyhow::anyhow!("{problems:?}"))?;
    let mut entries = vec![];
    for item in project.report(None).items.iter().filter(|item| item.state != engine::State::Valid) {
        let entry = match item.kind {
            engine::Kind::Suite => project.suite_passed(&item.component, item.subject.rsplit('/').next().unwrap_or_default()),
            engine::Kind::Passes => continue,
            kind => {
                let judged = (kind == engine::Kind::Judged).then(|| engine::Evidence { name: CAPTURE.into(), bytes: tree.0[CAPTURE].clone() });
                project.verdict(&item.subject, kind, engine::Verdict::Met, &judged.into_iter().collect::<Vec<_>>())
            }
        }.map_err(|problem| anyhow::anyhow!(problem.message))?;
        entries.push((entry.path, String::from_utf8(entry.bytes)?));
    }
    Ok(entries)
}

fn git(dir: &std::path::Path, args: &[&str]) -> Result<()> {
    let output = std::process::Command::new("git").current_dir(dir).args(["-c", "user.name=Ana", "-c", "user.email=ana@example.com"]).args(args).output()?;
    if !output.status.success() {
        bail!("git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
    }
    Ok(())
}

fn project(fakes: &Fakes) -> Result<()> {
    let repo = fakes.repo();
    for (path, text) in base_files().0 {
        let file = repo.join(path);
        std::fs::create_dir_all(file.parent().context("a file has a folder")?)?;
        std::fs::write(file, text)?;
    }
    git(&repo, &["add", "-A"])?;
    git(&repo, &["commit", "-q", "-m", "Turn specs on for billing"])?;
    git(&repo, &["push", "-q", "origin", "HEAD"])
}

/// The node of the task once a session other than `before` runs it.
fn next_node(report: &mut Report, fakes: &Fakes, before: &str, what: &str) -> Result<native::Session> {
    wait(report, what, || {
        let session = fakes.session_of(TASK)?;
        let seen = format!("{:?}", session.as_ref().map(|session| session.short.clone()));
        let took = |session: &native::Session| fakes.received(&session.short).is_ok_and(|inputs| !inputs.is_empty());
        Ok((session.filter(|session| session.short != before && session.running() && session.capability().is_some() && took(session)), seen))
    })
}

fn turn(report: &mut Report, fakes: &Fakes, session: &native::Session, what: &str, steps: Vec<Step>) -> Result<()> {
    report.step(what, || {
        for outcome in native::order(fakes, session, &Order { steps: steps.clone(), end_turn: true })? {
            if !outcome.ok {
                bail!("{} was refused: {}", serde_json::to_string(&outcome.step)?, outcome.text);
            }
        }
        Ok(())
    })
}

fn record(subject: &str, check: &str, reasoning: &str, nearest: Option<&str>) -> Step {
    let mut arguments = json!({"task_id": TASK, "subject": subject, "check": check, "verdict": "met", "reasoning": reasoning});
    if check == "suite" {
        arguments["output"] = json!("running 3 tests\ntest refunds_part_of_an_order ... ok\ntest refuses_more_than_what_is_left ... ok\ntest billing_uses_no_other_component ... ok\n\ntest result: ok. 3 passed; 0 failed");
    }
    if let Some(nearest) = nearest {
        arguments["nearest_wrong"] = json!(nearest);
    }
    Step::Call { tool: "record_validation".into(), arguments }
}

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "validation", 1, false)) else {
        return;
    };
    report.case("VAL1", "A task with specs is validated by a new agent and reaches the user with its specs, tests and evidence", |report| {
        report.step("the repository turns specs on for billing", || project(fakes))?;
        let mut script = task_script("req-validation", TASK, "Refund part of an order");
        script.tasks[0].staged = true;
        request(report, fakes, &user, &script, "Let customers get part of an order refunded")?;
        let author = launched(report, fakes, TASK)?;
        let specs = json!([
            {"path": "specs/billing/partial.feature", "text": PARTIAL},
            {"path": "specs/billing/refund-flow.mmd", "text": FLOW},
            {"path": "specs/billing/refund-sheet.png", "source": SHOT},
        ]);
        turn(report, fakes, &author, "the author proposes the plan's specs and its first turn ends", vec![Step::Call { tool: "propose_specs".into(), arguments: json!({"task_id": TASK, "level": "light", "files": specs}) }])?;
        std::thread::scope(|scope| {
            let plan = scope.spawn(|| native::order(fakes, &author, &Order { steps: vec![Step::OwnTurn { said: "Plan".into() }, Step::LeavePlanMode { points: 1 }], end_turn: false }));
            let card = wait(report, "the author's plan waits in Inbox", || card_in_inbox(&user, &format!("decision-{TASK}-")))?;
            let view = user.view(&format!("card:{card}"))?;
            let approve = user.action(&view, "accept:").with_context(|| format!("the card offers no «Approve»: {view}"))?;
            report.step("the user approves the plan", || user.press(approve["id"].as_str().unwrap_or_default(), None, None))?;
            report.step("the approval reaches the author", || {
                plan.join().map_err(|_| anyhow::anyhow!("the order panicked"))??;
                Ok(())
            })
        })?;
        act(report, fakes, &author, "the author's turn ends", &[Step::EndOwnTurn])?;
        let implementer = next_node(report, fakes, &author.short, "a new implementer takes the task")?;
        turn(report, fakes, &implementer, "the implementer changes the code, opens its pull request and says it is ready", vec![
            Step::Commit { file: "src/billing/lib.rs".into(), text: AFTER.into() },
            Step::Commit { file: "README.md".into(), text: README.into() },
            Step::Capture { file: CAPTURE.into(), width: 1170, height: 2400 },
            Step::Push, Step::OpenPr { number: 61, draft: false, checks: Default::default() }, Step::RegisterPr { number: 61 },
            Step::Call { tool: "ready_for_validation".into(), arguments: json!({"task_id": TASK}) },
        ])?;
        let validator = next_node(report, fakes, &implementer.short, "a new validator takes the task")?;
        let mut steps = vec![
            record("billing/unit", "suite", "", None),
            record("billing.partial/part", "adequate", "The test refunds 10 of 30 EUR and checks both what comes back and the 20 EUR left.", Some("Refunding the amount without lowering what is left")),
            record("billing.partial/more-than-left", "adequate", "The test refunds 18 EUR first, then asks for 20 and checks the refusal names the 12 EUR left.", Some("Refusing with another amount in the message")),
            {
                let mut judged = record("billing.partial/sheet-shows-what-is-left", "judged", "The capture shows the sheet with 20.00 EUR left under the amount.", None);
                if let Step::Call { arguments, .. } = &mut judged {
                    arguments["evidence"] = json!([CAPTURE]);
                }
                judged
            },
            record("billing.partial/stands-alone", "adequate", "The test lists the dependencies of billing and checks there are none.", Some("A dependency added only for tests")),
        ];
        steps.extend(ledger()?.into_iter().map(|(file, text)| Step::Commit { file, text }));
        steps.push(Step::Push);
        act(report, fakes, &validator, "the validator records each check, commits the ledger and pushes it", &steps)?;
        let head = head(&validator)?;
        let deliver = |language: &str| {
            let (title, body) = if language == "es" {
                ("Devolver parte de un pedido", "Un cliente recupera cualquier importe hasta lo que queda del pedido. Todos los escenarios de billing.partial están validados.")
            } else {
                ("Refund part of an order", "A customer gets back any amount up to what is left of the order. Every scenario of billing.partial is validated.")
            };
            Step::Call { tool: "present_result".into(), arguments: json!({
                "task_id": TASK, "interaction_id": format!("result-validation-{language}"), "is_final": true, "language": language,
                "title": title,
                "content": {"kind": "report", "body": body},
                "targets": [{"kind": "pull_request", "head": head}],
                "checklist": [{"item": "1", "evidence": [{"kind": "scenario", "value": "billing.partial/part"}, {"kind": "scenario", "value": "billing.partial/more-than-left"}]}],
            }) }
        };
        let mut tried = ["en", "es"].into_iter();
        let mut language = tried.next().unwrap_or("en");
        let delivered = wait(report, "the validator delivers, in the user's language", || {
            let outcome = native::order(fakes, &validator, &Order { steps: vec![deliver(language)], end_turn: false })?.pop().context("no outcome")?;
            if outcome.ok {
                return Ok((Some(format!("result-validation-{language}")), "delivered".into()));
            }
            if outcome.text.contains("card_language_mismatch") {
                language = tried.next().context("no language was taken")?;
            } else if !outcome.text.contains("delivery_review_pending") && !outcome.text.contains("github_unreachable") {
                bail!("the delivery was refused: {}", outcome.text);
            }
            Ok((None, outcome.text))
        })?;
        let view = user.view(&format!("card:{delivered}"))?;
        report.snapshot("card-validated-delivery", &view)?;
        report.step("the card shows the specs it validated", || {
            anyhow::ensure!(super::user::mentions(&view, "4 of 4 scenarios validated") || super::user::mentions(&view, "4 de 4 escenarios validados"), "{view}");
            Ok(())
        })
    });
}
