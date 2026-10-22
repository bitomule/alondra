//! Throwaway Macs, relay and phone for `make e2e-sandbox`. Never points at a real Alondra.
mod fake;
mod iroh_check;
mod iroh_cli;
mod issuer;
mod journey;
mod mac;
mod phone;
mod relay;
mod sandbox;
mod scenario;
#[cfg(test)]
#[path = "../../../crates/test_support.rs"]
mod test_support;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "alondra-sandbox")]
struct Cli {
    /// The sandbox directory `tools/e2e-sandbox.sh` created; `.scratch/sandbox` in the repository by default.
    #[arg(long, env = "SANDBOX_DIR", global = true)]
    sandbox: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Writes the JWKS of the sandbox token issuer from its PKCS#8 RSA key.
    Jwks { key: PathBuf, out: PathBuf },
    /// Writes a relay config that trusts the sandbox issuer.
    RelayConfig {
        #[arg(long)]
        listen: String,
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        registry: PathBuf,
        out: PathBuf,
    },
    /// Creates a Mac's service keys and `service.toml`, and prints its Mac ID.
    ProvisionMac {
        home: PathBuf,
        #[arg(long)]
        relay: String,
        /// The port of the Mac's direct listener.
        #[arg(long)]
        direct_port: u16,
        /// Where the direct listener binds: loopback, or 0.0.0.0 to reach it from a real phone.
        #[arg(long, default_value = "127.0.0.1")]
        direct_listen: String,
        /// The UDP port of the Mac's iroh endpoint, bound where the direct listener is.
        #[arg(long, default_value_t = 0)]
        iroh_port: u16,
    },
    /// Registers a provisioned Mac with the relay under the sandbox owner.
    EnrollMac {
        home: PathBuf,
        #[arg(long)]
        relay: String,
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        key: PathBuf,
    },
    /// Acts as a phone paired with sandbox Macs.
    Phone {
        #[arg(long, default_value = "phone")]
        name: String,
        /// The sandbox Mac, from 1.
        #[arg(long, default_value_t = 1)]
        mac: usize,
        #[command(subcommand)]
        action: PhoneAction,
    },
    /// Runs the automatic scenarios against every Mac of a running sandbox.
    Scenario,
    /// Checks Mac 1's iroh endpoint against its direct listener and measures both.
    Iroh,
    /// The real alondra reads Mac 1 over iroh, directly and through the iroh relay only.
    IrohCli,
    /// The sandbox's `claude` (FAKES=1): what `fakes/claude` runs.
    #[command(hide = true)]
    FakeClaude {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The sandbox's `gh` (FAKES=1): what `fakes/gh` runs.
    #[command(hide = true)]
    FakeGh {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// The process of one fake background session.
    #[command(hide = true)]
    FakeNode { short: String },
    /// Runs scripted end-to-end journeys (FAKES=1): task-cycle, session, local, or all; `seed`
    /// instead leaves a varied world for a person or the tester agent to explore.
    Journey { names: Vec<String> },
    /// Moves a fake node (FAKES=1): what its session does next.
    Fake {
        #[command(subcommand)]
        action: FakeAction,
    },
}

#[derive(Subcommand)]
enum FakeAction {
    /// Lists the fake nodes with their task and state.
    Nodes,
    /// Makes the node of a task (or with a short id) do something, and prints what happened.
    Node {
        /// A task id, or a node's short id.
        node: String,
        /// work (commit, push, PR, final delivery), ask, deliver, cleanup, end (exits) or die (crashes).
        verb: String,
        /// One step, or a list of steps, as JSON instead of a verb, e.g. '{"do":"dirty","file":"x.txt"}'.
        #[arg(long)]
        step: Option<String>,
    },
}

impl From<FakeAction> for fake::node::Order {
    fn from(action: FakeAction) -> Self {
        match action {
            FakeAction::Nodes => fake::node::Order::List,
            FakeAction::Node { node, verb, step } => fake::node::Order::Act { node, verb, step },
        }
    }
}

#[derive(Subcommand)]
enum PhoneAction {
    /// Approves the phone on the Mac and waits until the relay carries its requests. With
    /// --direct, pairs over the Mac's direct listener instead: it compares the code with the
    /// one the Mac shows and approves it there, as a person would.
    Pair {
        #[arg(long)]
        direct: bool,
        /// With --direct: leave the decision to the Mac's side instead of approving it.
        #[arg(long, requires = "direct")]
        wait: bool,
    },
    /// Sends one encrypted request and prints the decrypted response; --direct skips the relay.
    Request {
        method: String,
        path: String,
        body: Option<String>,
        #[arg(long)]
        request_id: Option<String>,
        #[arg(long)]
        direct: bool,
    },
    /// Presses the enabled action of a view, found by its label or the start of its id, as the
    /// user would on the card or screen; --text fills its text field.
    Press {
        view: String,
        action: String,
        #[arg(long)]
        text: Option<String>,
        #[arg(long)]
        direct: bool,
    },
    /// Revokes the phone on the Mac and waits until the relay refuses it.
    Revoke,
}

/// Where `tools/e2e-sandbox.sh` puts a sandbox by default: inside the checkout `cwd` belongs to, so
/// nothing a node does with it writes outside its worktree.
fn default_sandbox(cwd: &Path) -> PathBuf {
    let root = std::process::Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| PathBuf::from(String::from_utf8_lossy(&output.stdout).trim_end()))
        .unwrap_or_else(|| cwd.to_path_buf());
    root.join(".scratch/sandbox")
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let dir = match cli.sandbox {
        Some(dir) => dir,
        None => default_sandbox(&std::env::current_dir()?),
    };
    match cli.command {
        Command::Jwks { key, out } => issuer::write_jwks(&key, &out),
        Command::RelayConfig { listen, issuer, registry, out } => {
            relay::write_config(&listen, &issuer, &registry, &out)
        }
        Command::ProvisionMac { home, relay, direct_port, direct_listen, iroh_port } => {
            println!("{}", mac::provision(&home, &relay, direct_port, &direct_listen, iroh_port)?);
            Ok(())
        }
        Command::EnrollMac { home, relay, issuer, key } => {
            let token = issuer::Issuer::open(&key, &issuer)?.token()?;
            mac::Mac::open(&home)?.enroll(&relay::Relay::new(&relay), &token)
        }
        Command::Phone { name, mac, action } => {
            let sandbox = sandbox::Sandbox::open(&dir)?;
            let target = sandbox.mac(mac)?;
            let phone = phone::Phone::open(&sandbox, &name)?;
            match action {
                PhoneAction::Pair { direct: false, .. } => phone.pair(&target),
                PhoneAction::Pair { direct: true, wait: false } => phone.pair_direct(&target).map(drop),
                PhoneAction::Pair { direct: true, wait: true } => phone.pair_direct_waiting(&target).map(drop),
                PhoneAction::Request { method, path, body, request_id, direct } => {
                    let body = body.map(|text| serde_json::from_str(&text)).transpose()?;
                    let reply = if direct {
                        phone.request_direct(&target, &method, &path, body, request_id)?
                    } else {
                        phone.request(&target, &method, &path, body, request_id)?
                    };
                    println!("{}", serde_json::to_string_pretty(&reply)?);
                    Ok(())
                }
                PhoneAction::Press { view, action, text, direct } => {
                    let user = journey::User { phone, target, direct };
                    let shown = user.view(&view)?;
                    let found = user.action_labeled(&shown, &action).or_else(|| user.action(&shown, &action))
                        .with_context(|| format!("{view} has no enabled action {action}"))?;
                    let id = found["id"].as_str().context("the action has no id")?;
                    println!("{}", user.press(id, found["option_id"].as_str(), text.as_deref())?);
                    Ok(())
                }
                PhoneAction::Revoke => phone.revoke(&target),
            }
        }
        Command::Scenario => scenario::run(&sandbox::Sandbox::open(&dir)?),
        Command::Iroh => iroh_check::run(&sandbox::Sandbox::open(&dir)?),
        Command::IrohCli => iroh_cli::run(&sandbox::Sandbox::open(&dir)?),
        Command::FakeClaude { args } => std::process::exit(fake::claude(&fake::Fakes::open(&dir)?, &args)?),
        Command::FakeGh { args } => std::process::exit(fake::gh(&fake::Fakes::open(&dir)?, &args)?),
        Command::FakeNode { short } => fake::node::run(&fake::Fakes::open(&dir)?, &short),
        Command::Journey { names } if names == ["seed"] => journey::seed::run(&sandbox::Sandbox::open(&dir)?),
        Command::Journey { names } => journey::run(&sandbox::Sandbox::open(&dir)?, &names),
        Command::Fake { action } => fake::node::command(&fake::Fakes::open(&dir)?, action.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_sandbox_is_inside_the_checkout() {
        let checkout = test_support::Directory::new();
        let nested = checkout.join("tools/sandbox");
        std::fs::create_dir_all(&nested).unwrap();
        let init = std::process::Command::new("git").arg("init").arg("-q").arg(&*checkout).status().unwrap();
        assert!(init.success());
        let root = std::fs::canonicalize(&*checkout).unwrap();
        assert_eq!(default_sandbox(&nested), root.join(".scratch/sandbox"));
    }
}
