//! `claude` as Alondra calls it: the version and plugin checks, the native Claude the Agent SDK
//! host spawns for a task or an orchestrator batch, and, for a daemon without the SDK backend (the
//! previous release in the upgrade rehearsal), orchestrator runs, background sessions and the
//! commands that stop, restart, list and remove them.
use anyhow::Result;

use super::{Fakes, native, node, oauth, orchestrator};

/// While this file is in the fakes' folder, `claude stop` fails and the session goes on: a move to
/// the Agent SDK then stays at its planned step.
pub const HOLD_STOP: &str = "hold-stop";

pub fn run(fakes: &Fakes, args: &[String]) -> Result<i32> {
    fakes.record("claude", args)?;
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--version"] => {
            println!("{} (Claude Code)", node::VERSION);
            Ok(0)
        }
        ["plugin", "validate", ..] | ["plugin", "test", ..] => {
            println!("✔ Validation passed");
            Ok(0)
        }
        ["auth", "status", "--json"] => Ok(auth_status()),
        ["auth", "login"] => auth_login(),
        ["auth", "logout"] => {
            let _ = std::fs::remove_file(account_file());
            Ok(0)
        }
        ["rm", short] => node::remove(fakes, short),
        ["mcp", "remove", "--scope", scope, "--", name] => Ok(oauth::remove(fakes, scope, name)),
        ["stop", _] if fakes.dir.join(HOLD_STOP).exists() => {
            eprintln!("Claude Code could not stop the session");
            Ok(1)
        }
        ["stop", short] => match fakes.node(short) {
            Ok(_) => node::end(fakes, short, node::State::Stopped).map(|()| 0),
            Err(_) => {
                eprintln!("No background session {short}");
                Ok(1)
            }
        },
        _ if native::spawned_by_sdk(args) => native::run(fakes, args),
        ["respawn", short] => node::respawn(fakes, short),
        ["agents", ..] => node::agents(fakes),
        _ if words.contains(&"-p") => orchestrator::run(fakes, args),
        _ if words.contains(&"--bg") => node::launch(fakes, args),
        _ => {
            eprintln!("the sandbox's claude does not know `claude {}`", args.join(" "));
            Ok(1)
        }
    }
}

/// Where the fake keeps the sign-in of the profile it runs for: its `CLAUDE_CONFIG_DIR`, or else
/// `~/.claude`. The file holds the account's email.
pub const ACCOUNT_FILE: &str = ".sandbox-account";

fn account_file() -> std::path::PathBuf {
    let folder = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"));
    folder.join(ACCOUNT_FILE)
}

fn auth_status() -> i32 {
    match std::fs::read_to_string(account_file()) {
        Ok(email) => {
            println!("{}", serde_json::json!({"loggedIn": true, "authMethod": "claude.ai", "email": email.trim(), "subscriptionType": "max"}));
            0
        }
        Err(_) => {
            println!("{}", serde_json::json!({"loggedIn": false}));
            1
        }
    }
}

/// Where a test names the email the next sign-in takes, since Alondra runs `claude auth` with an
/// environment of its own.
pub const LOGIN_AS: &str = ".sandbox-login-as";

/// Signs in as the email `~/.sandbox-login-as` names, or as `<folder>@sandbox.test` without it.
fn auth_login() -> Result<i32> {
    let file = account_file();
    let folder = file.parent().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().trim_start_matches('.').to_owned()).unwrap_or_default();
    let home = std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let email = std::fs::read_to_string(home.join(LOGIN_AS)).map(|e| e.trim().to_owned()).unwrap_or_else(|_| format!("{folder}@sandbox.test"));
    std::fs::create_dir_all(file.parent().unwrap_or(std::path::Path::new(".")))?;
    std::fs::write(&file, &email)?;
    println!("Signed in as {email}");
    Ok(0)
}
