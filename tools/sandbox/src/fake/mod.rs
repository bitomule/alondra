//! Stand-ins for `claude` and `gh` that a sandbox started with `FAKES=1` puts first on its
//! daemons' PATH: no model and no GitHub. They drive the real `alondra` binaries the way Claude
//! Code and its plugin do, follow the scripts a run writes, and write down every call.
mod claude;
mod gh;
mod mcp;
pub mod node;
mod orchestrator;
pub mod script;

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::sandbox::now_ms;

pub use claude::run as claude;
pub use gh::{Checks, run as gh};

/// The fakes' state in `<sandbox>/fakes`.
#[derive(Clone)]
pub struct Fakes {
    pub sandbox: PathBuf,
    pub dir: PathBuf,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Call {
    pub at: u64,
    pub tool: String,
    pub args: Vec<String>,
    /// The `ALONDRA_HOME` of the daemon that ran it.
    pub home: Option<String>,
}

impl Fakes {
    pub fn open(sandbox: &Path) -> Result<Self> {
        let dir = sandbox.join("fakes");
        if !dir.is_dir() {
            bail!("{} has no fakes; start the sandbox with FAKES=1", sandbox.display());
        }
        Ok(Self { sandbox: sandbox.into(), dir })
    }

    pub fn alondra(&self) -> PathBuf {
        self.sandbox.join("bin/alondra")
    }

    pub fn tool(&self) -> PathBuf {
        self.sandbox.join("bin/alondra-sandbox")
    }

    /// The repository nodes work in, whose `origin` is the bare `remote.git` next to it.
    pub fn repo(&self) -> PathBuf {
        self.sandbox.join("repo")
    }

    pub fn remote(&self) -> PathBuf {
        self.sandbox.join("remote.git")
    }

    pub fn record(&self, tool: &str, args: &[String]) -> Result<()> {
        let call = Call {
            at: now_ms(),
            tool: tool.into(),
            args: args.to_vec(),
            home: std::env::var("ALONDRA_HOME").ok(),
        };
        append(&self.dir.join("calls.jsonl"), &serde_json::to_value(&call)?)
    }

    pub fn calls(&self) -> Result<Vec<Call>> {
        read_lines(&self.dir.join("calls.jsonl"))
    }

    /// Holds `name` against the other fake processes until dropped.
    pub fn lock(&self, name: &str) -> Result<Lock> {
        let path = self.dir.join(format!("{name}.lock"));
        let start = std::time::Instant::now();
        loop {
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Lock(path)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if start.elapsed() > Duration::from_secs(20) {
                        bail!("{} stayed locked for 20 s", path.display());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

pub struct Lock(PathBuf);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

pub fn append(path: &Path, value: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    // One write per line, so lines from processes appending at once never interleave.
    file.write_all(format!("{}\n", serde_json::to_string(value)?).as_bytes())?;
    Ok(())
}

pub fn read_lines<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Vec<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).with_context(|| format!("{}: {line}", path.display())))
        .collect()
}

pub fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("partial");
    fs::write(&partial, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&partial, path)?;
    Ok(())
}

/// Runs git in `dir` as the sandbox's own author; the output, or why it failed.
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=Alondra Sandbox", "-c", "user.email=sandbox@alondra.invalid", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .context("git")?;
    if !out.status.success() {
        bail!("git {} in {}: {}", args.join(" "), dir.display(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The value after `flag` in `args`.
pub fn flag_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.windows(2).find(|pair| pair[0] == flag).map(|pair| pair[1].as_str())
}
