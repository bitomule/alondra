//! `bazel run //crates/scenarios:capture -- --task <id> --out .scratch/<name>.ndjson`: reads the
//! frames the hosts of a task wrote on this Mac, Claude's or Codex's, both what the daemon sent them
//! and what they answered, anonymizes them and writes one transcript per host for a reviewer to
//! read. The output goes under the `.scratch` folder of the worktree and is never written into
//! `cases/`; nothing is written when the guard that checks the repository would refuse the result.

use alondra_scenarios_transcript::anonymize::{Anonymizer, header_of};
use alondra_scenarios_transcript::{guard, outbox, scratch};
use std::path::{Path, PathBuf};

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn home() -> PathBuf {
    std::env::var_os("ALONDRA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".alondra")))
        .unwrap_or_else(|| fail("neither ALONDRA_HOME nor HOME is set"))
}

/// The file of the `number`th of `count` hosts: `out` itself when there is one.
fn file_of(out: &Path, number: usize, count: usize) -> PathBuf {
    if count == 1 {
        return out.to_path_buf();
    }
    let stem = out.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    out.with_file_name(format!("{stem}-{number}.ndjson"))
}

fn main() {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let option = |name: &str| arguments.iter().position(|argument| argument == name).and_then(|at| arguments.get(at + 1));
    let (Some(task), Some(out)) = (option("--task"), option("--out")) else { fail("usage: capture --task <id> --out <file under .scratch>") };
    let out = scratch::destination(Path::new(out)).unwrap_or_else(|why| fail(&format!("{why}; copying a transcript into cases/ is for a reviewer to do after reading it")));
    let literals = guard::literals_from_env().unwrap_or_else(|error| fail(&error.to_string()));
    let hosts: Vec<outbox::Host> = outbox::hosts_of(&home(), task).into_iter().filter(|host| host.frames.iter().any(|frame| frame["body"]["type"] == "event")).collect();
    if hosts.is_empty() {
        fail("no host of that task wrote events on this Mac");
    }
    let scenario = out.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let captured_at = std::env::var("CAPTURED_AT").unwrap_or_else(|_| "unknown".into());
    let mut anonymizer = Anonymizer::new(literals.clone());
    let mut written = Vec::new();
    for (index, host) in hosts.iter().enumerate() {
        let header = header_of(&host.frames, host.provider, &scenario, &captured_at);
        let text = anonymizer.transcript(&host.frames, header).render();
        let findings = guard::scan_transcript(&text, &literals, &Default::default());
        if !findings.is_empty() {
            for (line, rule) in &findings {
                eprintln!("host {}, line {line}: {rule}", index + 1);
            }
            fail("the anonymized transcript still carries what must not be public; nothing was written");
        }
        written.push((file_of(&out, index + 1, hosts.len()), text));
    }
    for (file, text) in &written {
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap_or_else(|error| fail(&error.to_string()));
        }
        std::fs::write(file, text).unwrap_or_else(|error| fail(&error.to_string()));
        println!("{} written; read it before moving it into a case", file.display());
    }
}
