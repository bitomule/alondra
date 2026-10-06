use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;

use crate::{issuer::Issuer, mac::Mac};

/// `sandbox.json`, written by `tools/e2e-sandbox.sh` once every process answers.
#[derive(Deserialize)]
struct Manifest {
    relay: String,
    issuer: String,
    macs: Vec<ManifestMac>,
}

#[derive(Deserialize)]
struct ManifestMac {
    home: PathBuf,
    port: u16,
}

pub struct Sandbox {
    pub dir: PathBuf,
    pub relay: crate::relay::Relay,
    issuer: String,
    macs: Vec<ManifestMac>,
}

pub struct Target {
    pub number: usize,
    pub mac: Mac,
    pub local: String,
}

impl Sandbox {
    pub fn open(dir: &Path) -> Result<Self> {
        let path = dir.join("sandbox.json");
        let manifest: Manifest = serde_json::from_slice(
            &std::fs::read(&path)
                .with_context(|| format!("no sandbox at {}; run make e2e-sandbox", dir.display()))?,
        )?;
        Ok(Self {
            dir: dir.into(),
            relay: crate::relay::Relay::new(&manifest.relay),
            issuer: manifest.issuer,
            macs: manifest.macs,
        })
    }

    pub fn mac_count(&self) -> usize {
        self.macs.len()
    }

    pub fn mac(&self, number: usize) -> Result<Target> {
        let entry = number
            .checked_sub(1)
            .and_then(|index| self.macs.get(index))
            .ok_or_else(|| anyhow!("the sandbox has Macs 1 to {}", self.macs.len()))?;
        Ok(Target {
            number,
            mac: Mac::open(&entry.home)?,
            local: format!("http://127.0.0.1:{}", entry.port),
        })
    }

    pub fn token(&self) -> Result<String> {
        Issuer::open(&self.dir.join("issuer/key.der"), &self.issuer)?.token()
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

pub fn random<const N: usize>() -> Result<[u8; N]> {
    let mut value = [0u8; N];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| anyhow!("OS randomness failed"))?;
    Ok(value)
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn http() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(70)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into()
}

/// Retries `attempt` until it returns `Some`, for state another process applies on its own.
pub fn wait_for<T>(what: &str, limit: Duration, mut attempt: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let start = std::time::Instant::now();
    loop {
        if let Some(value) = attempt()? {
            return Ok(value);
        }
        if start.elapsed() > limit {
            bail!("timed out after {}s waiting for {what}", limit.as_secs());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
}
