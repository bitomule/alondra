//! Tasks whose nodes need an MCP server that is not connected wait on one card per server, and go
//! on once the user signs in, or without it. The sandbox's Claude has three servers that need a
//! sign-in (`fake::oauth`); its sign-in page sends the browser straight back with a code.
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

use super::{User, steps::*};
use crate::{
    fake::{
        Fakes,
        native::Session,
        script::Step,
    },
    journey::Report,
    sandbox::Sandbox,
};

const SERVERS: [&str; 3] = ["linear", "notion", "files"];

fn wait_for(task: &str, server: &str) -> Step {
    Step::Call { tool: "wait_for_mcp".into(), arguments: json!({"task_id": task, "server": server}) }
}

/// A GET to this Mac, as a browser on it makes: the status and the `Location` it answers with.
fn browse(url: &str) -> Result<(u16, Option<String>)> {
    let rest = url.strip_prefix("http://localhost:").context("a sign-in page on this Mac")?;
    let (port, path) = rest.split_once('/').context("a path")?;
    let mut stream = TcpStream::connect(("127.0.0.1", port.parse::<u16>()?))?;
    write!(stream, "GET /{path} HTTP/1.1\r\nHost: localhost:{port}\r\nConnection: close\r\n\r\n")?;
    let mut reader = BufReader::new(stream);
    let mut status = String::new();
    reader.read_line(&mut status)?;
    let code = status.split_whitespace().nth(1).context("a status")?.parse()?;
    let mut location = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Location: ") {
            location = Some(format!("http://localhost:{port}{}", value.trim()));
        }
    }
    let _ = reader.read_to_end(&mut vec![]);
    Ok((code, location))
}

/// The one card Waiting for You shows for the tasks that wait for `server`.
fn sign_in_card(report: &mut Report, user: &User, server: &str, waiting: usize) -> Result<(String, Value)> {
    let need = match waiting {
        1 => format!("Sign in to {server}: a task waits for it"),
        _ => format!("Sign in to {server}: {waiting} tasks wait for it"),
    };
    let card = wait(report, &format!("Waiting for You shows one card: “{need}”"), || {
        let inbox = user.inbox()?;
        let cards: Vec<&String> = inbox.iter().filter(|id| id.starts_with("mcp-sign-in-")).collect();
        for card in &cards {
            if super::user::mentions(&user.view(&format!("card:{card}"))?, &need) {
                return Ok((Some((*card).clone()), format!("{cards:?}")));
            }
        }
        Ok((None, format!("{cards:?}")))
    })?;
    let view = user.view(&format!("card:{card}"))?;
    report.snapshot(&format!("card-{card}"), &view)?;
    Ok((card, view))
}

fn connect(report: &mut Report, user: &User, card: &str) -> Result<String> {
    report.step("Connect opens the sign-in on a waiting node and hands back its page", || {
        let (status, body) = user.send("POST", &format!("/mcp-sign-ins/{card}/connect"), Some(json!({})))?;
        if status != 200 || body["returns_to_claude"] != json!(true) {
            bail!("connect answered {status}: {body}");
        }
        body["url"].as_str().map(str::to_owned).context("a sign-in page")
    })
}

fn waiting(user: &User, tasks: &[&str]) -> Result<Vec<String>> {
    let mut still = vec![];
    for task in tasks {
        if user.task_state(task)? != WORKING {
            still.push((*task).to_owned());
        }
    }
    Ok(still)
}

fn went_on(report: &mut Report, fakes: &Fakes, sessions: &[&Session], text: &str) -> Result<()> {
    for session in sessions {
        wait(report, &format!("{} is told “{text}…”", session.short), || told(fakes, session, text))?;
    }
    Ok(())
}

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(user) = report.step("a phone pairs with Mac 1 with the account", || User::paired(sandbox, "journey", 1, false)) else {
        return;
    };
    let servers: Vec<Value> = SERVERS.iter().map(|name| json!({"name": name, "url": format!("https://mcp.{name}.example.com/mcp")})).collect();
    let written = report.step("the sandbox's Claude has three MCP servers, none signed in", || {
        let _ = std::fs::remove_dir_all(fakes.dir.join("mcp-signed-in"));
        Ok(std::fs::write(fakes.dir.join("mcp-servers.json"), serde_json::to_vec(&servers)?)?)
    });
    if written.is_err() {
        return;
    }
    report.case("M1", "Two tasks wait for Linear on one card; signing in on the Mac sends both on", |report| {
        request(report, fakes, &user, &task_script("req-m1a", "t-m1a", "Triage the Linear backlog"), "Triage the Linear backlog")?;
        let first = launched(report, fakes, "t-m1a")?;
        request(report, fakes, &user, &task_script("req-m1b", "t-m1b", "Close the stale Linear issues"), "Close the stale Linear issues")?;
        let second = launched(report, fakes, "t-m1b")?;
        act(report, fakes, &first, "the first node waits for Linear", &[wait_for("t-m1a", "linear")])?;
        act(report, fakes, &second, "the second node waits for Linear", &[wait_for("t-m1b", "linear")])?;
        let (card, _) = sign_in_card(report, &user, "linear", 2)?;
        let page = connect(report, &user, &card)?;
        let again = connect(report, &user, &card)?;
        report.step("another device gets the same page", || if again == page { Ok(()) } else { bail!("{again} is not {page}") })?;
        let back = report.step("the browser on the Mac opens the page, which sends it back to Claude", || {
            let (status, back) = browse(&page)?;
            if status != 302 {
                bail!("the page answered {status}");
            }
            back.context("no way back")
        })?;
        report.step("Claude's listener takes the return", || match browse(&back)? {
            (200, _) => Ok(()),
            (status, _) => bail!("the return answered {status}"),
        })?;
        went_on(report, fakes, &[&first, &second], "linear is connected")?;
        wait(report, "no card waits for Linear any more", || {
            let inbox = user.inbox()?;
            Ok((inbox.iter().all(|id| *id != card).then_some(()), format!("{inbox:?}")))
        })?;
        wait(report, "both tasks work again", || {
            let still = waiting(&user, &["t-m1a", "t-m1b"])?;
            Ok((still.is_empty().then_some(()), format!("{still:?}")))
        })
    });
    report.case("M2", "Away from the Mac, where the browser came back to reaches Claude through the relay", |report| {
        request(report, fakes, &user, &task_script("req-m2", "t-m2", "Summarise the Notion roadmap"), "Summarise the Notion roadmap")?;
        let node = launched(report, fakes, "t-m2")?;
        act(report, fakes, &node, "the node waits for Notion", &[wait_for("t-m2", "notion")])?;
        let (card, _) = sign_in_card(report, &user, "notion", 1)?;
        let page = connect(report, &user, &card)?;
        let back = report.step("the browser away from the Mac comes back to a page that does not load", || {
            browse(&page)?.1.context("no way back")
        })?;
        report.step("a wrong address is refused and changes nothing", || {
            let (status, body) = user.send("POST", &format!("/mcp-sign-ins/{card}/return"), Some(json!({"address": "https://evil.example.com/callback?code=c&state=s"})))?;
            if status != 422 {
                bail!("answered {status}: {body}");
            }
            Ok(())
        })?;
        report.step("the user pastes the address and it reaches Claude", || {
            let (status, body) = user.send("POST", &format!("/mcp-sign-ins/{card}/return"), Some(json!({"address": back})))?;
            if status != 200 {
                bail!("answered {status}: {body}");
            }
            Ok(())
        })?;
        went_on(report, fakes, &[&node], "notion is connected")?;
        let code = back.split("code=").nth(1).and_then(|rest| rest.split('&').next()).context("a code")?.to_owned();
        report.step("the one-time code is written nowhere: the Mac's HOME, its SDK hosts' journals", || {
            let home = sandbox.mac(1)?.mac.home;
            let mut roots = vec![home.clone()];
            if let Ok(state) = std::fs::read_to_string(home.join("sdk-state-root")) {
                roots.push(state.trim().into());
            }
            let mut found = vec![];
            for root in &roots {
                found.extend(crate::fake::written_under(root, code.as_bytes())?);
            }
            if found.is_empty() { Ok(()) } else { bail!("found in {found:?}") }
        })
    });
    report.case("M3", "Go on without the server sends the waiting task on without it", |report| {
        request(report, fakes, &user, &task_script("req-m3", "t-m3", "Tidy the shared files"), "Tidy the shared files")?;
        let node = launched(report, fakes, "t-m3")?;
        act(report, fakes, &node, "the node waits for files", &[wait_for("t-m3", "files")])?;
        let (card, _) = sign_in_card(report, &user, "files", 1)?;
        report.step("the user goes on without it", || {
            let (status, body) = user.send("POST", &format!("/mcp-sign-ins/{card}/without"), Some(json!({})))?;
            if status != 200 {
                bail!("answered {status}: {body}");
            }
            Ok(())
        })?;
        went_on(report, fakes, &[&node], "files is not available")
    });
}
