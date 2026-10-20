//! The run's evidence: each case and step with what it expected and saw, the views at each
//! milestone, every fake call, and the daemons' logs when something failed.
use std::{fmt::Write as _, fs, path::PathBuf};

use anyhow::{Result, bail};
use serde::Serialize;
use serde_json::Value;

use crate::{fake::Fakes, sandbox::{Sandbox, now_ms}};

#[derive(Serialize, Clone)]
pub struct StepReport {
    pub what: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub ms: u64,
}

#[derive(Serialize, Clone)]
pub struct CaseReport {
    pub id: String,
    pub title: String,
    pub ok: bool,
    pub steps: Vec<StepReport>,
}

pub struct Report {
    pub out: PathBuf,
    cases: Vec<CaseReport>,
    current: Option<CaseReport>,
    snapshots: usize,
}

impl Report {
    pub fn new(out: PathBuf) -> Result<Self> {
        fs::create_dir_all(&out)?;
        Ok(Self { out, cases: vec![], current: None, snapshots: 0 })
    }

    /// Runs one case; a failed step ends the case, not the run.
    /// Runs the case, unless `JOURNEY_CASES` names others: a comma-separated list of case ids to
    /// run alone while working on them.
    pub fn case(&mut self, id: &str, title: &str, body: impl FnOnce(&mut Self) -> Result<()>) {
        if std::env::var("JOURNEY_CASES").is_ok_and(|only| !only.split(',').any(|case| case.trim() == id)) {
            return;
        }
        self.run_case(id, title, body);
    }

    fn run_case(&mut self, id: &str, title: &str, body: impl FnOnce(&mut Self) -> Result<()>) {
        println!("== {id} · {title}");
        self.current = Some(CaseReport { id: id.into(), title: title.into(), ok: true, steps: vec![] });
        self.snapshots = 0;
        if let Err(error) = body(self) {
            let case = self.current.as_mut().expect("a case is open");
            case.ok = false;
            if case.steps.last().is_none_or(|step| step.ok) {
                case.steps.push(StepReport { what: "the case went on".into(), ok: false, detail: Some(format!("{error:#}")), ms: 0 });
            }
            println!("   FAILED: {error:#}");
        }
        let case = self.current.take().expect("a case is open");
        self.cases.push(case);
        let _ = self.write();
    }

    /// Prepares the sandbox for what follows, as a case that `JOURNEY_CASES` never skips.
    pub fn setup(&mut self, title: &str, body: impl FnOnce(&mut Self) -> Result<()>) {
        self.run_case("setup", title, body);
    }

    /// One step: what it expects; its failure says what it saw instead.
    pub fn step<T>(&mut self, what: &str, body: impl FnOnce() -> Result<T>) -> Result<T> {
        let start = now_ms();
        let result = body();
        let ms = now_ms() - start;
        let (ok, detail) = match &result {
            Ok(_) => (true, None),
            Err(error) => (false, Some(format!("{error:#}"))),
        };
        println!("   {} {what} ({ms} ms){}", if ok { "ok  " } else { "FAIL" }, detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default());
        if let Some(case) = self.current.as_mut() {
            case.steps.push(StepReport { what: what.into(), ok, detail, ms });
            if !ok {
                case.ok = false;
            }
        }
        result
    }

    /// Keeps a view as it was at this point of the case.
    pub fn snapshot(&mut self, name: &str, view: &Value) -> Result<()> {
        let case = self.current.as_ref().map_or("run", |case| case.id.as_str());
        self.snapshots += 1;
        let dir = self.out.join("views").join(case);
        fs::create_dir_all(&dir)?;
        let file = format!("{:02}-{}.json", self.snapshots, name.replace([':', '/'], "_"));
        fs::write(dir.join(file), serde_json::to_vec_pretty(view)?)?;
        Ok(())
    }

    fn write(&self) -> Result<()> {
        fs::write(self.out.join("journeys.json"), serde_json::to_vec_pretty(&self.cases)?)?;
        let mut md = String::from("# Alondra sandbox journeys\n\n");
        let passed = self.cases.iter().filter(|case| case.ok).count();
        writeln!(md, "{passed} of {} cases passed.\n", self.cases.len())?;
        for case in &self.cases {
            writeln!(md, "## {} {} · {}\n", if case.ok { "✅" } else { "❌" }, case.id, case.title)?;
            for step in &case.steps {
                write!(md, "- {} {} ({} ms)", if step.ok { "OK" } else { "FALLO" }, step.what, step.ms)?;
                if let Some(detail) = &step.detail {
                    write!(md, "\n  - {}", detail.replace('\n', "\n    "))?;
                }
                md.push('\n');
            }
            md.push('\n');
        }
        fs::write(self.out.join("journeys.md"), md)?;
        Ok(())
    }

    /// Writes the evidence and fails when a case failed.
    pub fn finish(&self, fakes: &Fakes, sandbox: &Sandbox) -> Result<()> {
        self.write()?;
        let fakes_out = self.out.join("fakes");
        fs::create_dir_all(&fakes_out)?;
        for name in ["calls.jsonl", "orchestrator/actions.jsonl", "github/prs.json"] {
            let from = fakes.dir.join(name);
            if from.exists() {
                fs::copy(&from, fakes_out.join(name.replace('/', "-")))?;
            }
        }
        for node in fakes.all_nodes()? {
            let dir = fakes.dir.join("nodes").join(&node.short);
            for name in ["node.json", "pushed.jsonl", "steps.jsonl", "hooks.jsonl", "node.log"] {
                if dir.join(name).exists() {
                    fs::copy(dir.join(name), fakes_out.join(format!("node-{}-{name}", node.short)))?;
                }
            }
        }
        let failed: Vec<&str> = self.cases.iter().filter(|case| !case.ok).map(|case| case.id.as_str()).collect();
        if failed.is_empty() {
            println!("All {} cases passed; report in {}", self.cases.len(), self.out.join("journeys.md").display());
            return Ok(());
        }
        let logs = self.out.join("logs");
        fs::create_dir_all(&logs)?;
        for number in 1..=sandbox.mac_count() {
            let log = sandbox.dir.join(format!("mac-{number}/alondrad.log"));
            if log.exists() {
                fs::copy(&log, logs.join(format!("mac-{number}-alondrad.log")))?;
            }
        }
        if sandbox.dir.join("relay.log").exists() {
            fs::copy(sandbox.dir.join("relay.log"), logs.join("relay.log"))?;
        }
        bail!("{} failed; report in {}", failed.join(", "), self.out.join("journeys.md").display())
    }
}
