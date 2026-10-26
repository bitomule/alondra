//! A crash report from one Mac to whoever runs the report repository: Mac 1 builds it with the
//! real `alondra report` and sends it with the sandbox's `gh`, as a secret gist and an issue in the
//! repository the sandbox relay announces; sending again changes nothing; Mac 2, signed in as the
//! repository's owner, lists it and opens it into the same files. A report sent while GitHub cannot
//! be reached stays pending and Mac 1's daemon sends it on its own once GitHub answers again.
//! Deleting it closes its issue, and only its sender deletes its gist.
use std::{path::Path, process::Command};

use anyhow::{Context, Result, bail, ensure};

use super::steps::wait;
use crate::{
    fake::{Fakes, github_api::{LOGIN, OWNER, REPORTS}},
    journey::Report,
    sandbox::{Sandbox, Target},
};

const DESCRIPTION: &str = "The node stopped right after the Mac woke up";

/// Runs `alondra <args>` as Mac `target`'s user, signed in to the sandbox's GitHub as `login`.
fn alondra(sandbox: &Sandbox, target: &Target, login: &str, args: &[&str]) -> Result<String> {
    let port = target.local.rsplit(':').next().unwrap_or_default();
    let path = format!("{}:{}", sandbox.dir.join("fakes").display(), std::env::var("PATH").unwrap_or_default());
    let out = Command::new(sandbox.dir.join("bin/alondra"))
        .args(args)
        .env("ALONDRA_HOME", &target.mac.home)
        .env("ALONDRA_PORT", port)
        .env("ALONDRA_LANG", "en")
        .env("ALONDRA_SANDBOX", "1")
        .env("ALONDRA_SANDBOX_TIMING", "fast")
        .env("FAKE_GH_LOGIN", login)
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .output()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        bail!("alondra {} ended {}: {text}{}", args.join(" "), out.status, String::from_utf8_lossy(&out.stderr));
    }
    Ok(text)
}

/// Builds a report on Mac `target` and returns its id.
fn build(sandbox: &Sandbox, target: &Target) -> Result<String> {
    let out = alondra(sandbox, target, LOGIN, &["report", "--no-input", "--window", "24h", "--description", DESCRIPTION])?;
    ensure!(out.contains("The final check found nothing private."), "the report did not pass its final check:\n{out}");
    out.lines()
        .find_map(|line| line.strip_prefix("Crash report ").and_then(|rest| rest.split_whitespace().next()).map(str::to_owned))
        .with_context(|| format!("alondra report printed no id:\n{out}"))
}

fn issue_of(fakes: &Fakes, id: &str) -> Result<Option<serde_json::Value>> {
    let marker = format!("- Report: {id}\n");
    Ok(fakes.reports()?.issues.into_iter().find(|issue| issue["body"].as_str().is_some_and(|body| body.contains(&marker))))
}

fn sending_state(home: &Path, id: &str) -> Option<String> {
    let sending: serde_json::Value = serde_json::from_slice(&std::fs::read(home.join("reports").join(id).join("sending.json")).ok()?).ok()?;
    sending["state"].as_str().map(str::to_owned)
}

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let (Ok(user), Ok(owner)) = (sandbox.mac(1), sandbox.mac(2)) else { return };
    let mut sent: Option<(String, u64)> = None;
    report.case("C1", "A report sent from Mac 1 goes to a secret gist and an issue in the repository the relay announces, the issue holding only the version, macOS and failure", |report| {
        let id = report.step("alondra report builds a report on Mac 1 that passes its final check", || build(sandbox, &user))?;
        let out = report.step("alondra report send puts it in the report repository", || alondra(sandbox, &user, LOGIN, &["report", "send", &id, "--kind", "session-ended"]))?;
        let url = report.step("it says where it went", || {
            out.lines()
                .find_map(|line| line.strip_prefix("Sent: ").and_then(|rest| rest.split(". ").next()).map(str::to_owned))
                .filter(|url| url.starts_with(&format!("https://github.com/{REPORTS}/issues/")))
                .with_context(|| format!("alondra report send said:\n{out}"))
        })?;
        let issue = report.step("its issue says only the version, macOS and the kind of failure", || {
            let issue = issue_of(fakes, &id)?.context("no issue names the report")?;
            let text = format!("{}\n{}", issue["title"].as_str().unwrap_or_default(), issue["body"].as_str().unwrap_or_default());
            ensure!(text.contains("session-ended") && text.contains("- macOS: "), "the issue is:\n{text}");
            let home = user.mac.home.display().to_string();
            for private in [DESCRIPTION, home.as_str(), "daemon.log", "alondrad"] {
                ensure!(!text.contains(private), "the issue shows {private:?}:\n{text}");
            }
            Ok(issue)
        })?;
        report.step("the report's file is in a secret gist of the sender, in parts the issue links", || {
            let reports = fakes.reports()?;
            let gist = reports.gists.iter().find(|g| g.description == format!("Alondra crash report {id}")).context("no gist holds the report")?;
            ensure!(gist.owner == LOGIN, "the gist is {}'s", gist.owner);
            ensure!(issue["body"].as_str().is_some_and(|b| b.contains(&format!("- Gist: https://gist.github.com/{}", gist.id))), "the issue does not link the gist");
            ensure!(!gist.files.is_empty() && gist.files.iter().all(|(name, part)| name.starts_with("part-") && part.len() <= 900 * 1024), "{} parts", gist.files.len());
            ensure!(issue["labels"].as_array().is_some_and(Vec::is_empty), "GitHub keeps no label of someone who is not the repository's");
            Ok(())
        })?;
        report.step("sending it again finds the same issue and adds nothing", || {
            let again = alondra(sandbox, &user, LOGIN, &["report", "send", &id])?;
            ensure!(again.contains(&url), "the second send said:\n{again}");
            let reports = fakes.reports()?;
            ensure!(reports.issues.len() == 1 && reports.gists.len() == 1, "{} issues, {} gists", reports.issues.len(), reports.gists.len());
            Ok(())
        })?;
        sent = Some((id, issue["number"].as_u64().context("the issue has no number")?));
        Ok(())
    });
    report.case("C2", "Whoever runs the repository lists the report from Mac 2 and opens it into the same files", |report| {
        let (id, number) = sent.clone().context("C1 sent no report")?;
        report.step("alondra report list shows it with who sent it", || {
            let out = alondra(sandbox, &owner, OWNER, &["report", "list"])?;
            ensure!(out.contains(&format!("#{number}")) && out.contains(LOGIN) && out.contains("A node's session ended on its own"), "the list is:\n{out}");
            Ok(())
        })?;
        report.step("alondra report open shows who sent it and what they wrote", || {
            let out = alondra(sandbox, &owner, OWNER, &["report", "open", &number.to_string()])?;
            ensure!(out.contains(&format!("Issue #{number}, sent by {LOGIN}")) && out.contains(DESCRIPTION), "report open said:\n{out}");
            Ok(())
        })?;
        report.step("Mac 2 holds exactly the files Mac 1 previewed", || {
            let (sent_dir, received) = (user.mac.home.join("reports").join(&id), owner.mac.home.join("reports/received").join(&id));
            let mut names: Vec<String> = std::fs::read_dir(&received)?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            names.sort();
            ensure!(names.iter().any(|n| n == "manifest.json") && names.iter().any(|n| n == "daemon.log"), "received {names:?}");
            for name in &names {
                ensure!(std::fs::read(sent_dir.join(name))? == std::fs::read(received.join(name))?, "{name} differs");
            }
            ensure!(!received.join("pseudonyms.json").exists(), "the table of pseudonyms left Mac 1");
            Ok(())
        })?;
        Ok(())
    });
    report.case("C3", "A report sent while GitHub cannot be reached stays pending, and Mac 1's daemon sends it on its own once GitHub answers", |report| {
        let id = report.step("alondra report builds a second report on Mac 1", || build(sandbox, &user))?;
        report.step("without a network, alondra report send leaves it pending", || {
            fakes.github_offline(true)?;
            let out = alondra(sandbox, &user, LOGIN, &["report", "send", &id, "--kind", "not-resumed"]);
            fakes.github_offline(false).ok();
            let out = out?;
            ensure!(out.contains("It stays pending on this Mac"), "send said:\n{out}");
            ensure!(sending_state(&user.mac.home, &id).as_deref() == Some("pending"), "sending.json is {:?}", sending_state(&user.mac.home, &id));
            ensure!(issue_of(fakes, &id)?.is_none(), "an issue appeared while offline");
            Ok(())
        })?;
        wait(report, "the daemon sends it once GitHub answers again, with the kind the user chose", || {
            let issue = issue_of(fakes, &id)?;
            let state = sending_state(&user.mac.home, &id);
            let done = issue.as_ref().is_some_and(|i| i["title"].as_str().is_some_and(|t| t.ends_with("not-resumed"))) && state.as_deref() == Some("sent");
            Ok((done.then_some(()), format!("issue {}, sending.json {state:?}", issue.is_some())))
        })?;
        report.step("it made one issue", || {
            let count = fakes.reports()?.issues.iter().filter(|i| i["body"].as_str().is_some_and(|b| b.contains(&id))).count();
            ensure!(count == 1, "{count} issues");
            Ok(())
        })
    });
    report.case("C4", "Deleting a report closes its issue; only its sender deletes its gist", |report| {
        let (id, number) = sent.clone().context("C1 sent no report")?;
        let gist_of = || -> Result<bool> { Ok(fakes.reports()?.gists.iter().any(|g| g.description == format!("Alondra crash report {id}"))) };
        report.step("the repository's owner closes the issue and is told only the sender deletes the gist", || {
            let out = alondra(sandbox, &owner, OWNER, &["report", "delete", &number.to_string()])?;
            ensure!(out.starts_with(&format!("Closed issue #{number}")) && out.contains(&format!("belongs to {LOGIN}")), "delete said:\n{out}");
            let closed = fakes.reports()?.issues.iter().any(|i| i["number"] == number && i["state"] == "closed");
            ensure!(closed && gist_of()?, "closed {closed}, gist {}", gist_of()?);
            Ok(())
        })?;
        report.step("its sender deletes the gist", || {
            let out = alondra(sandbox, &user, LOGIN, &["report", "delete", &number.to_string()])?;
            ensure!(out.starts_with("Deleted the report's gist"), "delete said:\n{out}");
            ensure!(!gist_of()?, "the gist is still there");
            Ok(())
        })?;
        report.step("opening it now says the report is gone", || {
            let error = alondra(sandbox, &owner, OWNER, &["report", "open", &number.to_string()]).err().context("it still opened")?;
            ensure!(error.to_string().contains("was deleted"), "{error}");
            Ok(())
        })
    });
}
