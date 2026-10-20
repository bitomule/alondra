//! `claude` as Alondra calls it: the version and plugin checks, the native Claude the Agent SDK
//! host spawns for a task or an orchestrator batch, and, for a daemon without the SDK backend (the
//! previous release in the upgrade rehearsal), orchestrator runs, background sessions and the
//! commands that stop, restart, list and remove them.
use anyhow::Result;

use super::{Fakes, native, node, orchestrator};

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
        ["rm", short] => node::remove(fakes, short),
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
