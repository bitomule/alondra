//! `capture --task <id> --out <file>`: reads the frames the SDK host of a task wrote on this Mac,
//! anonymizes them and writes the transcript for a reviewer to read. The output goes under a
//! `.scratch` folder and is never written into `cases/`; nothing is written when the guard that
//! checks the repository would refuse the result.

use alondra_scenarios_transcript::anonymize::{Anonymizer, header_of};
use alondra_scenarios_transcript::guard;
use serde_json::Value;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_FRAME: usize = 1_048_576;

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

/// The frames of an outbox: a 4-byte big-endian length, then that many bytes of JSON. A truncated
/// tail is how a host stops writing.
fn outbox_frames(path: &Path) -> std::io::Result<Vec<Value>> {
    let mut reader = std::io::BufReader::new(std::fs::File::open(path)?);
    let mut frames = Vec::new();
    loop {
        let mut header = [0u8; 4];
        if reader.read_exact(&mut header).is_err() {
            return Ok(frames);
        }
        let length = u32::from_be_bytes(header) as usize;
        let mut payload = vec![0u8; length.min(MAX_FRAME)];
        if length == 0 || length > MAX_FRAME || reader.read_exact(&mut payload).is_err() {
            return Ok(frames);
        }
        if let Ok(frame) = serde_json::from_slice(&payload) {
            frames.push(frame);
        }
    }
}

fn home() -> PathBuf {
    std::env::var_os("ALONDRA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".alondra")))
        .unwrap_or_else(|| fail("neither ALONDRA_HOME nor HOME is set"))
}

fn belongs_to(frames: &[Value], task: &str) -> bool {
    frames.iter().any(|frame| frame["body"]["execution"]["task_id"] == task)
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let option = |name: &str| arguments.iter().position(|argument| argument == name).and_then(|at| arguments.get(at + 1));
    let (Some(task), Some(out)) = (option("--task"), option("--out")) else { fail("usage: capture --task <id> --out <file under .scratch>") };
    let out = PathBuf::from(out);
    if !out.components().any(|part| part.as_os_str() == ".scratch") {
        fail("the transcript is written under a .scratch folder; copying it into cases/ is for a reviewer to do after reading it");
    }
    let root = home().join("sdk");
    let mut frames = Vec::new();
    let mut instances: Vec<PathBuf> = std::fs::read_dir(&root).map(|entries| entries.flatten().map(|entry| entry.path()).collect()).unwrap_or_default();
    instances.sort();
    for instance in instances {
        if let Ok(found) = outbox_frames(&instance.join("outbox")).map(|found| found.into_iter().collect::<Vec<_>>())
            && belongs_to(&found, task)
        {
            frames.extend(found);
        }
    }
    if !frames.iter().any(|frame| frame["body"]["type"] == "event") {
        fail("no SDK host of that task wrote events on this Mac");
    }
    let scenario = out.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let literals = guard::literals_from_env();
    let header = header_of(&frames, &scenario, &std::env::var("CAPTURED_AT").unwrap_or_else(|_| "unknown".into()));
    let text = Anonymizer::new(literals.clone()).transcript(&frames, header).render();
    let findings = guard::scan_transcript(&text, &literals, &Default::default());
    if !findings.is_empty() {
        for (line, rule) in &findings {
            eprintln!("line {line}: {rule}");
        }
        fail("the anonymized transcript still carries what must not be public; nothing was written");
    }
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|error| fail(&error.to_string()));
    }
    std::fs::write(&out, text).unwrap_or_else(|error| fail(&error.to_string()));
    println!("{} written; read it before moving it into a case", out.display());
}
