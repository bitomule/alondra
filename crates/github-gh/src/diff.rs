//! Bounded PR reads. Process deadlines belong to the adapter, not its callers.
use crate::GhCli;
use alondra_core::clock;
use alondra_engine_ports::github::{DiffError, GhFailure, HeadError, PrDiff, MAX_PR_DIFF_BYTES};
use serde_json::Value;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(crate) async fn head(gh: &GhCli, url: &str) -> Result<String, HeadError> {
    let output = gh.run(&["pr", "view", url, "--json", "headRefOid"], None, Duration::from_secs(20))
        .await.map_err(HeadError::Unavailable)?;
    let value: Value = serde_json::from_slice(&output).map_err(|_| HeadError::InvalidResponse)?;
    value.get("headRefOid").and_then(Value::as_str)
        .filter(|head| head.len() == 40 && head.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_owned).ok_or(HeadError::InvalidHead)
}

pub(crate) async fn read(gh: &GhCli, url: &str) -> Result<PrDiff, DiffError> {
    let mut child = tokio::process::Command::new(gh.program())
        .args(["pr", "diff", url])
        .stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true)
        .spawn().map_err(|error| DiffError::Start(GhFailure::Launch(error.kind(), error.to_string())))?;
    let stdout = child.stdout.take().expect("piped stdout");
    let mut bytes = Vec::new();
    let read = clock::timeout(Duration::from_secs(60),
        stdout.take((MAX_PR_DIFF_BYTES + 1) as u64).read_to_end(&mut bytes)).await;
    if !matches!(read, Ok(Ok(_))) {
        let _ = child.kill().await;
        return Err(DiffError::Read);
    }
    let truncated = bytes.len() > MAX_PR_DIFF_BYTES;
    if truncated {
        let _ = child.kill().await;
        let _ = child.wait().await;
    } else {
        let status = clock::timeout(Duration::from_secs(5), child.wait()).await;
        if !matches!(status, Ok(Ok(status)) if status.success()) {
            return Err(DiffError::Finish);
        }
    }
    Ok(PrDiff { bytes, truncated })
}
