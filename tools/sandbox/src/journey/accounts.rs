//! Two Claude accounts on one Mac: its own profile, the default account, and Trabajo, one the
//! user added. New work starts on the account with the most margin; a task whose account ran out
//! or lost its sign-in waits on that account's card while new work starts on the other; the whole
//! Mac waits only while no account can work, and goes on on the first that can again. With
//! moving on, a task whose account ran out goes on with the other one in the same session.
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};

use super::{PATIENCE, User, steps::*, user::{collect, mentions}};
use crate::{
    fake::{
        Fakes,
        native::{REFUSE, Session},
        node::rfc3339,
        script::Step,
        write_json,
    },
    journey::Report,
    sandbox::{Sandbox, Target, http, now_ms},
};

const WORK: &str = "trabajo";
const WORK_NAME: &str = "Trabajo";
const DEFAULT: &str = "default";
const ACCOUNTS: &str = "/settings/accounts";
const CHECK: &str = "/settings/accounts/check";
const USAGE_CARD: &str = "claude-usage-limit-";
const SIGNED_OUT_CARD: &str = "claude-signed-out-";
const HOUR: u64 = 3_600_000;
const WEEK_RESETS_IN: u64 = 4 * 24 * HOUR;
/// The most a usage pause that ended waits before work resumes (`usage_pause::JITTER_MAX_MS`).
const RESUME_JITTER: Duration = Duration::from_secs(45);

pub fn run(report: &mut Report, sandbox: &Sandbox, fakes: &Fakes) {
    let Ok(target) = sandbox.mac(1) else { return };
    let mac = Mac { sandbox, fakes, target };
    let Ok(user) = report.step("a phone pairs directly with Mac 1", || User::paired(sandbox, "accounts", 1, true)) else {
        return;
    };
    report.setup("The Mac's Claude keeps its conversations in ~/.claude/projects, as on any Mac that used it", |report| {
        report.step("the user's Claude has its conversations folder", || Ok(std::fs::create_dir_all(sandbox.dir.join("sdk/home/.claude/projects"))?))
    });
    report.case("A1", "New work starts on the account with the most margin: Trabajo at 10 % of its week, then the default once Trabajo is at 90 %", |report| {
        nodes_at_once(report, &user, None)?;
        let listing = report.step("the Mac's own Claude profile is the default account, signed in", || {
            let listing = mac.accounts()?;
            ensure!(state_of(&listing, DEFAULT) == "signed_in", "the accounts are {listing}");
            Ok(listing)
        })?;
        let alone = wait(report, "the daemon measured the default account's week", || {
            let usage = mac.usage()?;
            Ok((usage["week_percent"].as_f64(), format!("{usage}")))
        })?;
        mac.write_usage(WORK, 5.0, 10.0, now_ms() + 3 * HOUR)?;
        report.step("the user adds the account Trabajo", || {
            let mut accounts = listing["config"].clone();
            accounts["claude"].as_array_mut().context("the section lists no Claude accounts")?.push(json!({"id": WORK, "name": WORK_NAME}));
            mac.exchange("PUT", ACCOUNTS, Some(json!({"version": listing["version"], "accounts": accounts}))).map(drop)
        })?;
        report.step("Trabajo signs in to Claude with its own folder", || {
            mac.claude(WORK, &["auth", "login"])?;
            let listing = mac.exchange("POST", CHECK, None)?;
            ensure!(state_of(&listing, WORK) == "signed_in", "the accounts are {listing}");
            Ok(())
        })?;
        week_reads(report, &mac, &format!("the daemon measures Trabajo at 10 % of its week, the default at {alone} %"), (alone + 10.0) / 2.0)?;
        request(report, fakes, &user, &task_script("req-acc1", "t-acc1", "Sort the backlog"), "Sort the backlog")?;
        let first = launched_on(report, fakes, "t-acc1", WORK)?;
        mac.write_usage(WORK, 5.0, 90.0, now_ms() + 3 * HOUR)?;
        let week = json!({"status": "allowed", "rateLimitType": "seven_day", "utilization": 0.9, "resetsAt": (now_ms() + WEEK_RESETS_IN) / 1000});
        act(report, fakes, &first, "Claude reports Trabajo at 90 % of its week in t-acc1's turn", &[Step::RateLimit { info: week }])?;
        week_reads(report, &mac, "the daemon reads Trabajo at 90 % of its week", (alone + 90.0) / 2.0)?;
        request(report, fakes, &user, &task_script("req-acc2", "t-acc2", "Tidy the docs"), "Tidy the docs")?;
        launched_on(report, fakes, "t-acc2", DEFAULT).map(drop)
    });
    report.case("A2", "The default account runs out: its task waits on its card and new work starts on Trabajo, with no margin left but able to work", |report| {
        mac.write_usage(DEFAULT, 100.0, 30.0, now_ms() + HOUR)?;
        mac.refuse(DEFAULT, Some("rate_limit"))?;
        message(report, &user, "t-acc2", "Also update the changelog")?;
        hold_card(report, &user, USAGE_CARD, "t-acc2", "Claude's usage limit is reached: a task waits")?;
        report.step("the Mac does not wait: Trabajo can still work", || {
            let exhausted = exhausted(&user)?;
            ensure!(exhausted.is_null(), "the Mac waits for usage: {exhausted}");
            Ok(())
        })?;
        request(report, fakes, &user, &task_script("req-acc3", "t-acc3", "Check the links"), "Check the links")?;
        launched_on(report, fakes, "t-acc3", WORK).map(drop)
    });
    report.case("A3", "With both accounts out new work waits for capacity; once Trabajo can work again it goes on there, and the default's task keeps waiting", |report| {
        let first = session(fakes, "t-acc1")?;
        let third = session(fakes, "t-acc3")?;
        let task = user.view("task:t-acc3")?;
        press(report, &user, &task, "cancel:", None, None, "the user cancels t-acc3")?;
        state(report, &user, "t-acc3", CANCELLED)?;
        ended(report, fakes, &user, &third)?;
        mac.write_usage(WORK, 100.0, 90.0, now_ms() + HOUR)?;
        mac.refuse(WORK, Some("rate_limit"))?;
        message(report, &user, "t-acc1", "Also run the linter")?;
        hold_card(report, &user, USAGE_CARD, "t-acc1", "The Claude account Trabajo reached its limit: a task waits")?;
        let paused = wait(report, "the whole Mac waits: no account can work", || {
            let exhausted = exhausted(&user)?;
            Ok(((!exhausted.is_null()).then_some(exhausted.clone()), format!("{exhausted}")))
        })?;
        report.snapshot("usage-exhausted", &paused)?;
        let task = user.view("task:t-acc3")?;
        press(report, &user, &task, "reopen:", None, None, "the user reopens t-acc3")?;
        let waiting = wait(report, "the reopened task waits to start because Claude usage ran out, and no node starts for it", || {
            let view = user.view("task:t-acc3")?;
            let seen = state_line(&view);
            let started = fakes.session_of("t-acc3")?.filter(|session| session.short != third.short);
            if let Some(session) = started {
                bail!("node {} started on {} while no account can work", session.short, session.account);
            }
            Ok(((seen == "Didn't start · Claude usage ran out").then_some(view), seen))
        })?;
        report.snapshot("task-t-acc3-waiting", &waiting)?;
        request(report, fakes, &user, &task_script("req-acc4", "t-acc4", "Rename the module"), "Rename the module")?;
        report.step("no node starts for the new request either", || {
            ensure!(fakes.session_of("t-acc4")?.is_none(), "t-acc4 started while no account can work");
            ensure!(!exhausted(&user)?.is_null(), "the Mac stopped waiting");
            Ok(())
        })?;
        let told = fakes.received(&session(fakes, "t-acc2")?.short)?.len();
        mac.refuse(WORK, None)?;
        mac.write_usage(WORK, 5.0, 90.0, now_ms() + 3 * HOUR)?;
        let reset = json!({"status": "allowed", "rateLimitType": "five_hour", "utilization": 0.05, "resetsAt": (now_ms() + 3 * HOUR) / 1000});
        act(report, fakes, &first, "Claude reports Trabajo's 5-hour window reset", &[Step::RateLimit { info: reset }])?;
        wait_within(report, "the Mac stops waiting, after the up to 45 s Alondra spreads resumes over", RESUME_JITTER + PATIENCE, || {
            let exhausted = exhausted(&user)?;
            Ok((exhausted.is_null().then_some(()), format!("{exhausted}")))
        })?;
        let again = wait(report, "the reopened t-acc3 starts with a new node", || {
            let session = fakes.session_of("t-acc3")?;
            let seen = format!("{:?}", session.as_ref().map(|session| (&session.short, &session.state)));
            Ok((session.filter(|session| session.short != third.short && session.running()), seen))
        })?;
        on_account(report, &again, "t-acc3", WORK)?;
        launched_on(report, fakes, "t-acc4", WORK)?;
        wait(report, "t-acc1's card leaves Inbox once Claude answers on Trabajo", || {
            let inbox = user.inbox()?;
            Ok(((!inbox.iter().any(|id| id.starts_with(USAGE_CARD) && id.contains("-t-acc1-"))).then_some(()), format!("{inbox:?}")))
        })?;
        report.step("the default account's task still waits on its card and Alondra sent it nothing", || {
            let inbox = user.inbox()?;
            ensure!(inbox.iter().any(|id| id.starts_with(USAGE_CARD) && id.contains("-t-acc2-")), "its card left Inbox: {inbox:?}");
            let now = fakes.received(&session(fakes, "t-acc2")?.short)?.len();
            ensure!(now == told, "t-acc2 took {} more inputs", now - told);
            Ok(())
        })
    });
    report.case("A4", "Trabajo's sign-in expires: its task's card says to run claude with Trabajo's profile folder on the Mac and sign in with /login, the account reads signed out and new work starts on the default", |report| {
        let second = session(fakes, "t-acc2")?;
        mac.refuse(DEFAULT, None)?;
        mac.write_usage(DEFAULT, 10.0, 30.0, now_ms() + 3 * HOUR)?;
        let reset = json!({"status": "allowed", "rateLimitType": "five_hour", "utilization": 0.1, "resetsAt": (now_ms() + 3 * HOUR) / 1000});
        act(report, fakes, &second, "Claude reports the default account's 5-hour window reset", &[Step::RateLimit { info: reset }])?;
        wait(report, "the daemon reads the default account's 5 hours at 10 % and Trabajo's at 5 %", || {
            let usage = mac.usage()?;
            Ok((usage["five_hour_percent"].as_f64().filter(|seen| (seen - 7.5).abs() < 0.5).map(drop), format!("{usage}")))
        })?;
        mac.refuse(WORK, Some("authentication_failed"))?;
        report.step("Trabajo's sign-in expires: claude auth status says it is signed out", || mac.claude(WORK, &["auth", "logout"]))?;
        message(report, &user, "t-acc4", "Also add a test")?;
        let (_, card) = hold_card(report, &user, SIGNED_OUT_CARD, "t-acc4", "Sign in to the Claude account Trabajo on the Mac")?;
        report.step("the card says what to run on the Mac: claude with Trabajo's profile folder, signing in with /login; Alondra never signs in", || {
            ensure!(mentions(&card, &format!("/accounts/{WORK} claude and sign in with /login")), "it reads {card}");
            ensure!(mentions(&card, "run CLAUDE_CONFIG_DIR="), "it reads {card}");
            ensure!(!mentions(&card, "alondra accounts"), "it reads {card}");
            Ok(())
        })?;
        report.step("alondra accounts list on the Mac shows Trabajo signed out", || {
            let listing = mac.accounts_cli()?;
            ensure!(state_of(&listing, WORK) == "signed_out", "it lists {listing}");
            ensure!(state_of(&listing, DEFAULT) == "signed_in", "it lists {listing}");
            Ok(())
        })?;
        request(report, fakes, &user, &task_script("req-acc5", "t-acc5", "Update the screenshots"), "Update the screenshots")?;
        launched_on(report, fakes, "t-acc5", DEFAULT)?;
        wait(report, "t-acc2's card leaves Inbox once Claude answers on the default account", || {
            let inbox = user.inbox()?;
            Ok(((!inbox.iter().any(|id| id.starts_with(USAGE_CARD) && id.contains("-t-acc2-"))).then_some(()), format!("{inbox:?}")))
        })?;
        report.step("t-acc4 still waits for Trabajo's sign-in", || {
            let inbox = user.inbox()?;
            ensure!(inbox.iter().any(|id| id.starts_with(SIGNED_OUT_CARD) && id.contains("-t-acc4-")), "its card left Inbox: {inbox:?}");
            Ok(())
        })
    });
    report.case("A5", "With moving on, a task whose account runs out goes on with Trabajo in the same Claude session, told to go on, and its card leaves Inbox", |report| {
        mac.refuse(WORK, None)?;
        let fourth = user.view("task:t-acc4")?;
        press(report, &user, &fourth, "cancel:", None, None, "the user cancels t-acc4, which waited for Trabajo's sign-in")?;
        report.step("Trabajo signs in to Claude again", || mac.claude(WORK, &["auth", "login"]))?;
        wait(report, "the daemon sees Trabajo signed in", || {
            let listing = mac.exchange("POST", CHECK, None)?;
            Ok(((state_of(&listing, WORK) == "signed_in").then_some(()), format!("{listing}")))
        })?;
        mac.write_usage(WORK, 5.0, 30.0, now_ms() + 3 * HOUR)?;
        let listing = report.step("moving is off by default, and the setting says its risk", || {
            let listing = mac.accounts()?;
            ensure!(listing["move_when_exhausted"] == json!({"on": false, "risk": "Anthropic doesn't say whether switching accounts when one reaches its limit is allowed."}),
                "the accounts are {listing}");
            Ok(listing)
        })?;
        report.step("the user turns moving on", || {
            let mut accounts = listing["config"].clone();
            accounts["distribution"]["move_when_exhausted"] = json!(true);
            let after = mac.exchange("PUT", ACCOUNTS, Some(json!({"version": listing["version"], "accounts": accounts})))?;
            ensure!(after["move_when_exhausted"]["on"] == json!(true), "the accounts are {after}");
            Ok(())
        })?;
        let fifth = launched_on(report, fakes, "t-acc5", DEFAULT)?;
        mac.write_usage(DEFAULT, 100.0, 30.0, now_ms() + HOUR)?;
        mac.refuse(DEFAULT, Some("rate_limit"))?;
        message(report, &user, "t-acc5", "Also check the alt texts")?;
        let resumed = wait(report, "t-acc5's session goes on with Trabajo, resumed with its transcript", || {
            let start = fakes.starts(&fifth.short)?.pop();
            Ok((start.filter(|start| start.resume.as_deref() == Some(fifth.session_id.as_str()) && start.account == WORK && start.history > 0), format!("{:?}", fakes.starts(&fifth.short)?)))
        })?;
        report.step("in the folder it worked in", || {
            ensure!(resumed.cwd == fifth.cwd, "it resumed in {}", resumed.cwd.display());
            Ok(())
        })?;
        wait(report, "the session is told to go on", || {
            let received = fakes.received(&fifth.short)?;
            Ok((received.last().filter(|input| input.text.contains("go on with your task from where it stopped")).map(drop), format!("{:?}", received.last())))
        })?;
        wait(report, "t-acc5's usage card leaves Inbox", || {
            let inbox = user.inbox()?;
            Ok(((!inbox.iter().any(|id| id.starts_with(USAGE_CARD) && id.contains("-t-acc5-"))).then_some(()), format!("{inbox:?}")))
        })?;
        on_account(report, &session(fakes, "t-acc5")?, "t-acc5", WORK)
    });
    report.setup("After accounts: every task is cancelled, Trabajo is removed and the API refuses nothing", |report| {
        super::cancel_open_tasks(report, sandbox, fakes)?;
        report.step("the API refuses nothing and the usage endpoint says what it says by default", || {
            for path in [fakes.dir.join(REFUSE), fakes.dir.join("usage").join(format!("{WORK}.json")), fakes.dir.join("usage").join(format!("{DEFAULT}.json"))] {
                let removed = if path.is_dir() { std::fs::remove_dir_all(&path) } else { std::fs::remove_file(&path) };
                if let Some(error) = removed.err().filter(|error| error.kind() != std::io::ErrorKind::NotFound) {
                    bail!("{}: {error}", path.display());
                }
            }
            Ok(())
        })?;
        report.step("the user removes Trabajo and its folder goes", || {
            let listing = mac.accounts()?;
            let mut accounts = listing["config"].clone();
            if let Some(list) = accounts["claude"].as_array_mut() {
                list.retain(|account| account["id"] != WORK);
            }
            accounts["distribution"]["move_when_exhausted"] = json!(false);
            let after = mac.exchange("PUT", ACCOUNTS, Some(json!({"version": listing["version"], "accounts": accounts})))?;
            ensure!(after["accounts"].as_array().is_some_and(|rows| rows.iter().all(|row| row["id"] != WORK)), "the accounts are {after}");
            ensure!(!mac.folder(WORK).exists(), "{} is still there", mac.folder(WORK).display());
            Ok(())
        })?;
        nodes_at_once(report, &user, Some("auto"))
    });
}

struct Mac<'a> {
    sandbox: &'a Sandbox,
    fakes: &'a Fakes,
    target: Target,
}

impl Mac<'_> {
    /// One request to the daemon's local API, which settings take on this Mac only.
    fn exchange(&self, method: &str, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{path}", self.target.local);
        let agent = http();
        let mut response = match method {
            "GET" => agent.get(&url).call()?,
            "PUT" => agent.put(&url).header("content-type", "application/json").send(&serde_json::to_vec(&body.unwrap_or_default())?[..])?,
            _ => agent.post(&url).send_empty()?,
        };
        let status = response.status().as_u16();
        let value: Value = response.body_mut().read_json()?;
        ensure!((200..300).contains(&status), "{method} {path} answered {status}: {value}");
        Ok(value)
    }

    fn accounts(&self) -> Result<Value> {
        self.exchange("GET", ACCOUNTS, None)
    }

    /// The usage of the accounts together, as the TUI's header reads it.
    fn usage(&self) -> Result<Value> {
        Ok(self.exchange("GET", "/daemon", None)?["usage"].clone())
    }

    fn folder(&self, account: &str) -> PathBuf {
        self.target.mac.home.join("accounts").join(account)
    }

    /// The sandbox's `claude` for the account's folder, in the HOME the nodes' Claude runs in.
    fn claude(&self, account: &str, args: &[&str]) -> Result<()> {
        let status = Command::new(self.fakes.dir.join("claude"))
            .args(args)
            .env("HOME", self.sandbox.dir.join("sdk/home"))
            .env("CLAUDE_CONFIG_DIR", self.folder(account))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()?;
        ensure!(status.success(), "claude {} ended {status}", args.join(" "));
        Ok(())
    }

    /// `alondra accounts list --json` on the Mac.
    fn accounts_cli(&self) -> Result<Value> {
        let port = self.target.local.rsplit(':').next().unwrap_or_default();
        let out = Command::new(self.fakes.alondra())
            .args(["accounts", "list", "--json"])
            .env("ALONDRA_HOME", &self.target.mac.home)
            .env("ALONDRA_PORT", port)
            .env("ALONDRA_SANDBOX", "1")
            .env("ALONDRA_LANG", "en")
            .stdin(Stdio::null())
            .output()?;
        ensure!(out.status.success(), "alondra accounts list ended {}: {}", out.status, String::from_utf8_lossy(&out.stderr));
        Ok(serde_json::from_slice(&out.stdout)?)
    }

    /// What Claude's usage endpoint answers for the account, in its own form.
    fn write_usage(&self, account: &str, session: f64, week: f64, session_resets_at: u64) -> Result<()> {
        write_json(&self.fakes.dir.join("usage").join(format!("{account}.json")), &json!({
            "five_hour": {"utilization": session, "resets_at": rfc3339(session_resets_at)},
            "seven_day": {"utilization": week, "resets_at": rfc3339(now_ms() + WEEK_RESETS_IN)},
        }))
    }

    /// What the API refuses every turn on the account with, or nothing.
    fn refuse(&self, account: &str, error: Option<&str>) -> Result<()> {
        let path = self.fakes.dir.join(REFUSE).join(account);
        match error {
            Some(error) => {
                std::fs::create_dir_all(self.fakes.dir.join(REFUSE))?;
                std::fs::write(&path, error)?;
            }
            None => {
                if let Some(error) = std::fs::remove_file(&path).err().filter(|error| error.kind() != std::io::ErrorKind::NotFound) {
                    bail!("{}: {error}", path.display());
                }
            }
        }
        Ok(())
    }
}

fn state_of(listing: &Value, account: &str) -> String {
    listing["accounts"].as_array().into_iter().flatten().find(|row| row["id"] == account).and_then(|row| row["state"].as_str()).unwrap_or("absent").to_owned()
}

fn shown(account: &str) -> &str {
    if account == WORK { WORK_NAME } else { "the default account" }
}

fn session(fakes: &Fakes, task: &str) -> Result<Session> {
    fakes.session_of(task)?.with_context(|| format!("{task} has no node"))
}

fn on_account(report: &mut Report, session: &Session, task: &str, account: &str) -> Result<()> {
    report.step(&format!("{task}'s native Claude runs on {}", shown(account)), || {
        ensure!(session.account == account, "it runs on {}", session.account);
        Ok(())
    })
}

fn launched_on(report: &mut Report, fakes: &Fakes, task: &str, account: &str) -> Result<Session> {
    let session = launched(report, fakes, task)?;
    on_account(report, &session, task, account)?;
    Ok(session)
}

fn week_reads(report: &mut Report, mac: &Mac, what: &str, percent: f64) -> Result<()> {
    wait(report, what, || {
        let usage = mac.usage()?;
        Ok((usage["week_percent"].as_f64().filter(|seen| (seen - percent).abs() < 0.5).map(drop), format!("{usage}")))
    })
}

/// The task's state with its fact, as its view's header reads it.
fn state_line(view: &Value) -> String {
    let mut facts = vec![];
    collect(view, &mut |item| item.get("label").and_then(Value::as_str) == Some("State") && item.get("value").is_some(), &mut facts);
    facts.first().and_then(|fact| fact["value"].as_str()).unwrap_or_default().to_owned()
}

/// The Mac-wide wait for usage the status reads, or null.
fn exhausted(user: &User) -> Result<Value> {
    Ok(user.view("home")?["status"]["usage_exhausted"].clone())
}

fn message(report: &mut Report, user: &User, task: &str, text: &str) -> Result<()> {
    let view = user.view(&format!("task:{task}"))?;
    press(report, user, &view, "msg:", None, Some(text), &format!("the user writes to {task}: “{text}”")).map(drop)
}

/// The card `task` waits on in Inbox, kept with its view, which must read `need`.
fn hold_card(report: &mut Report, user: &User, prefix: &str, task: &str, need: &str) -> Result<(String, Value)> {
    let marker = format!("-{task}-");
    let card = wait(report, &format!("{task} waits on its card in Inbox"), || {
        let inbox = user.inbox()?;
        Ok((inbox.iter().find(|id| id.starts_with(prefix) && id.contains(&marker)).cloned(), format!("{inbox:?}")))
    })?;
    let view = user.view(&format!("card:{card}"))?;
    report.snapshot(&format!("card-{card}"), &view)?;
    report.step(&format!("the card reads “{need}”"), || {
        ensure!(mentions(&view, need), "it reads {view}");
        Ok(())
    })?;
    Ok((card, view))
}

/// Sets how many nodes the Mac runs at once: `option`, or the most it offers.
fn nodes_at_once(report: &mut Report, user: &User, option: Option<&str>) -> Result<()> {
    let machine = user.view("machine")?;
    let action = user.action(&machine, "set_limit:").context("the machine view offers no node limit")?;
    let most = action["options"].as_array().into_iter().flatten().filter_map(|option| option["id"].as_str()?.parse::<u32>().ok()).max().context("no numbered limit")?.to_string();
    let chosen = option.map_or(most, str::to_owned);
    report.step(&format!("the user sets nodes at once to {chosen}"), || user.press(action["id"].as_str().unwrap_or_default(), Some(&chosen), None).map(drop))
}
