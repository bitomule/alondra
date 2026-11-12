use anyhow::{Result, ensure};
use std::io::Write;

pub fn run(what: Option<String>, progress: Option<String>, detail: Option<String>, pid: Option<u32>, clear: bool) -> Result<()> {
    let Some(path) = std::env::var_os("ALONDRA_WAIT_FILE").filter(|path| !path.is_empty()) else { return Ok(()) };
    let path = std::path::Path::new(&path);
    if clear {
        return match std::fs::remove_file(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            result => Ok(result?),
        };
    }
    let what = what.unwrap_or_default();
    ensure!(!what.trim().is_empty(), tr!("What you wait for cannot be empty", "Aquello a lo que esperas no puede estar vacío"));
    let mut report = serde_json::json!({"what": what, "since_ms": alondra_core::now_millis()});
    for (key, value) in [("progress", progress), ("detail", detail)] {
        if let Some(value) = value { report[key] = value.into(); }
    }
    if let Some(pid) = pid { report["pid"] = pid.into(); }
    let bytes = serde_json::to_vec(&report)?;
    ensure!(bytes.len() <= 4096, tr!("A wait report must fit in 4 KiB", "Un aviso de espera debe caber en 4 KiB"));
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap_or(std::path::Path::new(".")))?;
    temporary.write_all(&bytes)?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}
