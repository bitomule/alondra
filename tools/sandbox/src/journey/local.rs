//! Alondra without the relay: phones and a terminal paired directly with each Mac, a whole task run
//! with the relay stopped, and a revoked phone told so.
use std::{
    io::{BufRead, BufReader},
    net::TcpStream,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;

use super::{Report, User, steps::*, user::mentions};
use crate::{
    fake::Fakes,
    sandbox::{Sandbox, Target, http, wait_for},
};

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs directly with Mac 1, the codes matching", || User::paired(sandbox, "local", 1, true)) else {
        return;
    };
    report.case("L1", "With the relay stopped, a phone paired directly runs a whole task", |report| {
        report.step("the relay stops", || stop_relay(sandbox))?;
        request(report, fakes, &user, &task_script("req-l1", "t-l1", "Work without the relay"), "Work without the relay")?;
        let node = launched(report, fakes, "t-l1")?;
        deliver_merge_close(report, fakes, &user, &node, "t-l1", 31)
    });
    if sandbox.mac_count() >= 2 {
        report.case("L2", "Each Mac shows only its own tasks; a terminal pairs directly with both", |report| {
            let other = report.step("a phone pairs directly with Mac 2", || User::paired(sandbox, "local", 2, true))?;
            request(report, fakes, &other, &task_script("req-l2", "t-l2", "Only on Mac 2"), "Only on Mac 2")?;
            launched(report, fakes, "t-l2")?;
            state(report, &other, "t-l2", WORKING)?;
            report.step("Mac 2 lists the task and Mac 1 does not", || {
                ensure!(mentions(&other.view("tasks")?, "Only on Mac 2"), "Mac 2's Tasks does not list it");
                ensure!(!mentions(&user.view("tasks")?, "Only on Mac 2"), "Mac 1's Tasks lists Mac 2's task");
                Ok(())
            })?;
            for number in 1..=2 {
                let target = sandbox.mac(number)?;
                report.step(&format!("a terminal pairs directly with Mac {number}, approved on the Mac by its code"), || {
                    pair_terminal(sandbox, &target)
                })?;
            }
            report.step("the terminal keeps both Macs", || {
                let saved = std::fs::read_to_string(sandbox.dir.join("client/direct-macs.json")).context("no direct-macs.json")?;
                for number in 1..=2 {
                    let id = sandbox.mac(number)?.mac.id;
                    ensure!(saved.contains(&id), "the terminal did not keep Mac {number} ({id})");
                }
                Ok(())
            })
        });
    }
    report.case("L3", "A revoked phone is told so, sealed, and its request creates nothing", |report| {
        let link = user.phone.direct_link(&user.target)?;
        report.step("Mac 1 revokes the phone", || user.target.mac.revoke_device(&user.target.local, &link.device_id))?;
        report.step("its next request gets a sealed device_revoked answer", || {
            let (status, body) = user.send("POST", "/requests", Some(serde_json::json!({"request_id": "req-l3", "text": "Never created"})))?;
            ensure!(status == 403 && mentions(&body, "device_revoked"), "the revoked phone got {status}: {body}");
            Ok(())
        })?;
        let witness = report.step("another phone pairs directly with Mac 1", || User::paired(sandbox, "witness", 1, true))?;
        report.step("Mac 1 has no task for it", || {
            ensure!(!mentions(&witness.view("tasks")?, "Never created"), "the revoked phone's request was created");
            Ok(())
        })
    });
}

fn stop_relay(sandbox: &Sandbox) -> Result<()> {
    let manifest: Value = serde_json::from_slice(&std::fs::read(sandbox.dir.join("sandbox.json"))?)?;
    let relay = manifest["relay"].as_str().context("no relay in sandbox.json")?;
    let address = relay.trim_start_matches("http://").trim_end_matches('/').to_owned();
    let pid = std::fs::read_to_string(sandbox.dir.join("relay.pid"))?.trim().to_owned();
    Command::new("kill").arg(&pid).status()?;
    wait_for("the relay to stop listening", Duration::from_secs(20), || {
        Ok(TcpStream::connect(&address).is_err().then_some(()))
    })?;
    let alive = Command::new("kill").args(["-0", &pid]).stderr(Stdio::null()).status()?.success();
    ensure!(!alive, "the relay process {pid} is still running");
    Ok(())
}

/// `alondra mac connect --host` in the sandbox's client HOME, approved on the Mac once the codes match.
pub(crate) fn pair_terminal(sandbox: &Sandbox, target: &Target) -> Result<()> {
    let mut connect = Command::new(sandbox.dir.join("bin/alondra"))
        .args(["mac", "connect", "--host", &target.direct])
        .env("ALONDRA_HOME", sandbox.dir.join("client"))
        .env("ALONDRA_SANDBOX", "1")
        .env("ALONDRA_LANG", "en")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut lines = BufReader::new(connect.stdout.take().context("connect stdout")?).lines();
    let code = loop {
        let line = lines.next().context("alondra mac connect printed no code")??;
        if let Some(rest) = line.strip_prefix("Code: ") {
            break rest.split('.').next().unwrap_or_default().trim().to_owned();
        }
    };
    let agent = http();
    let pending = wait_for("the Mac to list the terminal's request", Duration::from_secs(20), || {
        let reply: Value = agent.get(&format!("{}/devices/pending", target.local)).call()?.body_mut().read_json()?;
        Ok(reply["pending"].as_array().and_then(|list| list.iter().find(|request| request["code"] == code.as_str()).cloned()))
    })?;
    let id = pending["id"].as_str().context("pending request id")?;
    let approved = agent.post(&format!("{}/devices/pending/{id}/approve", target.local)).header("content-type", "application/json").send("{}")?;
    ensure!(approved.status() == 200, "the Mac refused to approve the terminal ({})", approved.status());
    let rest: Vec<String> = lines.map_while(Result::ok).collect();
    let status = connect.wait()?;
    if !status.success() || !rest.iter().any(|line| line.starts_with("Paired with")) {
        let mut error = String::new();
        if let Some(mut stderr) = connect.stderr.take() {
            std::io::Read::read_to_string(&mut stderr, &mut error)?;
        }
        bail!("alondra mac connect ended {status}: {} {error}", rest.join(" "));
    }
    Ok(())
}
