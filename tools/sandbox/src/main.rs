//! Throwaway Macs, relay and phone for `make e2e-sandbox`. Never points at a real Alondra.
mod issuer;
mod mac;
mod phone;
mod relay;
mod sandbox;
mod scenario;

use std::path::PathBuf;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "alondra-sandbox")]
struct Cli {
    /// The sandbox directory `tools/e2e-sandbox.sh` created.
    #[arg(long, env = "SANDBOX_DIR", default_value = "/tmp/alondra-sandbox", global = true)]
    sandbox: PathBuf,
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
}

#[derive(Subcommand)]
enum PhoneAction {
    /// Approves the phone on the Mac and waits until the relay carries its requests. With
    /// --direct, pairs over the Mac's direct listener instead: it compares the code with the
    /// one the Mac shows and approves it there, as a person would.
    Pair {
        #[arg(long)]
        direct: bool,
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
    /// Revokes the phone on the Mac and waits until the relay refuses it.
    Revoke,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Jwks { key, out } => issuer::write_jwks(&key, &out),
        Command::RelayConfig { listen, issuer, registry, out } => {
            relay::write_config(&listen, &issuer, &registry, &out)
        }
        Command::ProvisionMac { home, relay, direct_port, direct_listen } => {
            println!("{}", mac::provision(&home, &relay, direct_port, &direct_listen)?);
            Ok(())
        }
        Command::EnrollMac { home, relay, issuer, key } => {
            let token = issuer::Issuer::open(&key, &issuer)?.token()?;
            mac::Mac::open(&home)?.enroll(&relay::Relay::new(&relay), &token)
        }
        Command::Phone { name, mac, action } => {
            let sandbox = sandbox::Sandbox::open(&cli.sandbox)?;
            let target = sandbox.mac(mac)?;
            let phone = phone::Phone::open(&sandbox, &name)?;
            match action {
                PhoneAction::Pair { direct: false } => phone.pair(&target),
                PhoneAction::Pair { direct: true } => phone.pair_direct(&target).map(drop),
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
                PhoneAction::Revoke => phone.revoke(&target),
            }
        }
        Command::Scenario => scenario::run(&sandbox::Sandbox::open(&cli.sandbox)?),
    }
}
