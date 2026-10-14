//! `claude` as Alondra calls it: the version and plugin checks, orchestrator runs, background
//! sessions and the commands that stop, restart, list and remove them.
use anyhow::Result;

use super::{Fakes, node, orchestrator};

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
        ["stop", short] => match fakes.node(short) {
            Ok(_) => node::end(fakes, short, node::State::Stopped).map(|()| 0),
            Err(_) => {
                eprintln!("No background session {short}");
                Ok(1)
            }
        },
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
