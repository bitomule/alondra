//! The real `alondra` against a sandbox Mac over iroh: a terminal paired directly reads over the
//! Mac's iroh endpoint, then with no address but the iroh relay, which admits it only because the
//! Mac bound the terminal's endpoint there; revoking the terminal shuts it out of the relay.
use std::{path::PathBuf, process::Command, time::Duration};

use anyhow::{Context, Result, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

use crate::sandbox::{Sandbox, Target, http, wait_for};

const READS: &str = "20";

/// `alondra-sandbox iroh-cli`: needs a sandbox started with `SANDBOX_IROH_RELAY`.
pub fn run(sandbox: &Sandbox) -> Result<()> {
    let token = std::fs::read_to_string(sandbox.dir.join("iroh-access-token"))
        .context("start the sandbox with SANDBOX_IROH_RELAY and SANDBOX_IROH_TOKEN (make e2e-iroh-cli)")?;
    let target = sandbox.mac(1)?;
    let client = sandbox.dir.join("client");
    crate::journey::pair_terminal(sandbox, &target)?;
    let terminal = Terminal { binary: sandbox.dir.join("bin/alondra"), home: client.clone() };

    let direct = terminal.ping(&target)?;
    ensure!(direct["iroh"] == true, "the terminal did not reach Mac 1 over iroh: {direct}");
    println!(
        "ok: alondra reads Mac 1 over iroh ({}), median {:.1} ms, p95 {:.1} ms, connecting {:.0} ms",
        direct["via"], direct["median_ms"].as_f64().unwrap_or_default(), direct["p95_ms"].as_f64().unwrap_or_default(),
        direct["connect_ms"].as_f64().unwrap_or_default()
    );

    let node = terminal.node()?;
    let access = |node: &str| -> Result<String> {
        let mut response = http()
            .post(&format!("{}/v1/iroh/access", sandbox.relay.origin()))
            .header("authorization", &format!("Bearer {}", token.trim()))
            .header("x-iroh-nodeid", node)
            .send_empty()?;
        Ok(response.body_mut().read_to_string()?)
    };
    wait_for("the relay to admit the terminal's iroh endpoint", Duration::from_secs(40), || {
        Ok((access(&node)? == "true").then_some(()))
    })?;
    println!("ok: the relay admits the terminal's iroh endpoint, bound by Mac 1 with the terminal's own hello");

    terminal.only_the_relay(&target)?;
    let relayed = terminal.ping(&target)?;
    ensure!(relayed["iroh"] == true, "with only the iroh relay, the terminal did not reach Mac 1: {relayed}");
    println!(
        "ok: with no address but the iroh relay, alondra still reads Mac 1 ({}), median {:.1} ms, p95 {:.1} ms, connecting {:.0} ms",
        relayed["via"], relayed["median_ms"].as_f64().unwrap_or_default(), relayed["p95_ms"].as_f64().unwrap_or_default(),
        relayed["connect_ms"].as_f64().unwrap_or_default()
    );

    target.mac.revoke_device(&target.local, &terminal.device_id()?)?;
    wait_for("the relay to shut the revoked terminal out", Duration::from_secs(40), || {
        Ok((access(&node)? == "false").then_some(()))
    })?;
    let refused = terminal.run(&["mac", "ping", &target.mac.id, "--count", "1"])?;
    ensure!(!refused.status.success(), "a revoked terminal still read Mac 1");
    println!("ok: revoking the terminal on Mac 1 shuts it out of the iroh relay, and the Mac refuses it");
    Ok(())
}

struct Terminal {
    binary: PathBuf,
    home: PathBuf,
}

impl Terminal {
    fn run(&self, args: &[&str]) -> Result<std::process::Output> {
        Ok(Command::new(&self.binary)
            .args(args)
            .env("ALONDRA_HOME", &self.home)
            .env("ALONDRA_SANDBOX", "1")
            .env("ALONDRA_LANG", "en")
            .output()?)
    }

    fn ping(&self, target: &Target) -> Result<Value> {
        let output = self.run(&["--json", "mac", "ping", &target.mac.id, "--count", READS])?;
        if !output.status.success() {
            bail!("alondra mac ping failed: {}", String::from_utf8_lossy(&output.stderr));
        }
        Ok(serde_json::from_slice(&output.stdout)?)
    }

    /// This terminal's iroh endpoint ID, from the key `alondra` keeps.
    fn node(&self) -> Result<String> {
        let encoded = std::fs::read_to_string(self.home.join("iroh-client.key"))?;
        let bytes: [u8; 32] = URL_SAFE_NO_PAD
            .decode(encoded.trim())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("not an iroh key"))?;
        Ok(iroh::SecretKey::from_bytes(&bytes).public().to_string())
    }

    fn device_id(&self) -> Result<String> {
        let store: Value = serde_json::from_slice(&std::fs::read(self.home.join("direct-macs.json"))?)?;
        Ok(store["macs"][0]["device_id"].as_str().context("no paired Mac")?.to_owned())
    }

    /// Leaves the terminal nothing but the iroh relay: no iroh address, no HTTP route.
    fn only_the_relay(&self, target: &Target) -> Result<()> {
        let dials = self.home.join("direct-iroh.json");
        let mut known: Value = serde_json::from_slice(&std::fs::read(&dials)?)?;
        let dial = &mut known[&target.mac.id];
        ensure!(dial["relay"].is_string(), "Mac 1 named no iroh relay in its routes: {dial}");
        dial["addrs"] = json!([]);
        std::fs::write(&dials, serde_json::to_vec(&known)?)?;
        let pairings = self.home.join("direct-macs.json");
        let mut store: Value = serde_json::from_slice(&std::fs::read(&pairings)?)?;
        store["macs"][0]["routes"] = json!(["127.0.0.1:9"]);
        std::fs::write(&pairings, serde_json::to_vec(&store)?)?;
        Ok(())
    }
}
