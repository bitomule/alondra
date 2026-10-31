//! A report on disk (`~/.alondra/reports/<id>/`, readable text files and its manifest), the package
//! made of exactly those files, and the file a report is saved to and opened from.
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::canary::{Hit, Private};
use super::collect::{Omitted, Section, Status, Window};
use super::time;

/// The most a package may weigh compressed.
pub const MAX_COMPRESSED: usize = 5 * 1024 * 1024;
pub const MANIFEST: &str = "manifest.json";
pub const DESCRIPTION: &str = "description.txt";
pub const PSEUDONYMS: &str = "pseudonyms.json";
/// How sending the report goes; it stays on this Mac.
pub const SENDING: &str = "sending.json";
const LOCAL_ONLY: &[&str] = &[PSEUDONYMS, SENDING, "sending.json.next", ".sending.lock"];
pub const SECTIONS: &[&str] = &[
    "versions.json", "machine.json", "machine-samples.csv", "doctor.json", "daemon.log", "lifecycle.jsonl", "tasks.jsonl", "nodes.jsonl", "sdk-host.jsonl", "apps.jsonl",
];
/// Cut, oldest lines first, in this order, when the package is too big; the rest is never cut.
const TRIMMABLE: &[&str] = &["daemon.log", "lifecycle.jsonl"];
const HEADER: &str = "alondra-report 1 plain";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Trim {
    pub section: String,
    pub lines: usize,
}

pub struct Draft {
    pub id: String,
    pub created_at: i64,
    pub window: Window,
    pub sections: Vec<Section>,
    pub description: Option<String>,
    pub omitted: Omitted,
    pub log_capped: bool,
    pub daemon_running: bool,
}

/// A report id: `r-<date>-<random>`.
pub fn new_id(now: i64) -> String {
    let mut random = [0u8; 4];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut random).expect("the system's random numbers");
    let date: String = time::utc(now)[..10].chars().filter(|c| c.is_ascii_digit()).collect();
    format!("r-{date}-{}", random.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

pub fn is_id(id: &str) -> bool {
    let parts: Vec<&str> = id.split('-').collect();
    parts.len() == 3 && parts[0] == "r" && parts[1].len() == 8 && parts[1].chars().all(|c| c.is_ascii_digit()) && parts[2].len() == 8 && parts[2].chars().all(|c| c.is_ascii_hexdigit())
}

/// The files of the package, in order: the manifest first.
pub type Files = Vec<(String, String)>;

/// Cuts the oldest lines of the trimmable sections until the package fits, and returns its files.
pub fn assemble(draft: &mut Draft) -> (Files, Vec<Trim>) {
    let mut trims: Vec<Trim> = Vec::new();
    loop {
        let files = files_of(draft, &trims);
        let size = compressed(&files).len();
        if size <= MAX_COMPRESSED {
            return (files, trims);
        }
        let Some(at) = TRIMMABLE.iter().find_map(|name| draft.sections.iter().position(|s| s.name == *name && !s.text.is_empty())) else {
            return (files, trims);
        };
        let section = &mut draft.sections[at];
        let lines = section.text.lines().count();
        let share = (size - MAX_COMPRESSED) as f64 / size as f64;
        let cut = ((lines as f64 * share * 1.1).ceil() as usize).clamp(1, lines);
        let kept: Vec<&str> = section.text.lines().skip(cut).collect();
        section.text = if kept.is_empty() { String::new() } else { kept.join("\n") + "\n" };
        if section.text.is_empty() {
            section.status = Status::Empty("cut so the report fits in 5 MB".into());
        }
        match trims.iter_mut().find(|t| t.section == section.name) {
            Some(trim) => trim.lines += cut,
            None => trims.push(Trim { section: section.name.to_owned(), lines: cut }),
        }
    }
}

fn files_of(draft: &Draft, trims: &[Trim]) -> Files {
    let mut files = Vec::new();
    if let Some(description) = draft.description.as_ref().filter(|d| !d.trim().is_empty()) {
        files.push((DESCRIPTION.to_owned(), description.trim().to_owned() + "\n"));
    }
    for section in draft.sections.iter().filter(|s| !s.text.is_empty()) {
        files.push((section.name.to_owned(), section.text.clone()));
    }
    let manifest = manifest(draft, &files, trims);
    files.insert(0, (MANIFEST.to_owned(), serde_json::to_string_pretty(&manifest).unwrap_or_default() + "\n"));
    files
}

fn manifest(draft: &Draft, files: &Files, trims: &[Trim]) -> Value {
    let sections: Vec<Value> = draft
        .sections
        .iter()
        .map(|section| match (&section.status, files.iter().find(|(name, _)| name == section.name)) {
            (_, Some((_, text))) => json!({"name": section.name, "bytes": text.len(), "sha256": sha256(text.as_bytes())}),
            (Status::Empty(reason), None) => json!({"name": section.name, "empty": reason}),
            (Status::Ok, None) => json!({"name": section.name, "empty": "nothing to report"}),
        })
        .collect();
    let description = files.iter().find(|(name, _)| name == DESCRIPTION).map(|(_, text)| json!({"bytes": text.len(), "sha256": sha256(text.as_bytes())}));
    json!({
        "format": "alondra.report",
        "format_version": 1,
        "id": draft.id,
        "created_at": time::utc(draft.created_at),
        "window": {"from": time::utc(draft.window.from), "to": time::utc(draft.window.to), "hours": draft.window.hours},
        "alondra": env!("CARGO_PKG_VERSION"),
        "daemon_running": draft.daemon_running,
        "description": description,
        "sections": sections,
        "omitted": draft.omitted,
        "trimmed": trims,
        "log_read_capped_at_bytes": draft.log_capped.then_some(super::daemon_log::READ_CAP),
    })
}

pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

/// The package: the files as one JSON document, compressed.
pub fn compressed(files: &Files) -> Vec<u8> {
    let document = json!({"files": files.iter().map(|(name, text)| json!({"name": name, "text": text})).collect::<Vec<_>>()});
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(&serde_json::to_vec(&document).unwrap_or_default()).expect("compress into memory");
    encoder.finish().expect("compress into memory")
}

/// Every hit of the final check in the files; the description is checked for secrets only.
pub fn check(files: &Files, private: &Private) -> Vec<Hit> {
    files
        .iter()
        .flat_map(|(name, text)| super::check::section(name, text, private))
        .collect()
}

/// Writes the report's files and its pseudonyms (readable only by the user) into a new folder.
pub fn write(dir: &Path, files: &Files, pseudonyms: &impl Serialize) -> Result<()> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    for (name, text) in files {
        std::fs::write(dir.join(name), text)?;
    }
    let mut table = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(dir.join(PSEUDONYMS))?;
    table.write_all(&serde_json::to_vec_pretty(pseudonyms)?)?;
    Ok(())
}

/// The files of a report folder exactly as its manifest lists them; refuses a folder where a file
/// changed, went missing or was added after the preview.
pub fn read(dir: &Path) -> Result<Files> {
    let manifest_text = std::fs::read_to_string(dir.join(MANIFEST)).with_context(|| format!("{} has no {MANIFEST}", dir.display()))?;
    let manifest: Value = serde_json::from_str(&manifest_text).context("the manifest is not valid")?;
    let mut expected: Vec<(String, String)> = manifest["sections"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some((s["name"].as_str()?.to_owned(), s["sha256"].as_str()?.to_owned())))
        .collect();
    if let Some(sha) = manifest["description"]["sha256"].as_str() {
        expected.insert(0, (DESCRIPTION.to_owned(), sha.to_owned()));
    }
    let mut files = vec![(MANIFEST.to_owned(), manifest_text)];
    for (name, sha) in &expected {
        if !is_package_file(name) {
            bail!(trf!("the manifest names a file a report never has: {name}", "el manifiesto nombra un fichero que un informe nunca tiene: {name}"));
        }
        let text = std::fs::read_to_string(dir.join(name)).with_context(|| format!("{name} is missing"))?;
        if sha256(text.as_bytes()) != *sha {
            bail!(trf!("{name} changed after the preview; create the report again with alondra report", "{name} cambió después de la vista previa; vuelve a crear el informe con alondra report"));
        }
        files.push((name.clone(), text));
    }
    for entry in std::fs::read_dir(dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name != MANIFEST && !LOCAL_ONLY.contains(&name.as_str()) && !expected.iter().any(|(e, _)| *e == name) {
            bail!(trf!("{name} was added to the report after the preview; create the report again with alondra report", "{name} se añadió al informe después de la vista previa; vuelve a crear el informe con alondra report"));
        }
    }
    Ok(files)
}

fn is_package_file(name: &str) -> bool {
    name == MANIFEST || name == DESCRIPTION || SECTIONS.contains(&name)
}

/// The saved file: a header, then the package in base64 lines.
pub fn armored(id: &str, files: &Files) -> String {
    let encoded = base64::engine::general_purpose::STANDARD.encode(compressed(files));
    let lines: Vec<&str> = encoded.as_bytes().chunks(76).map(|c| std::str::from_utf8(c).unwrap_or_default()).collect();
    format!("{HEADER} {id}\n{}\n", lines.join("\n"))
}

/// The report id and the files of a saved file.
pub fn unarmored(text: &str) -> Result<(String, Files)> {
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    let Some(id) = header.strip_prefix(HEADER).map(str::trim).filter(|id| is_id(id)) else {
        bail!(trf!("this file is not an Alondra report", "este fichero no es un informe de Alondra"));
    };
    let encoded: String = lines.flat_map(|l| l.trim().chars()).collect();
    let packed = base64::engine::general_purpose::STANDARD.decode(encoded).context("the report is damaged")?;
    let mut json = Vec::new();
    flate2::read::GzDecoder::new(packed.as_slice()).take(256 * 1024 * 1024).read_to_end(&mut json).context("the report is damaged")?;
    let document: Value = serde_json::from_slice(&json).context("the report is damaged")?;
    let mut files = Files::new();
    for file in document["files"].as_array().into_iter().flatten() {
        let (Some(name), Some(text)) = (file["name"].as_str(), file["text"].as_str()) else { bail!("the report is damaged") };
        if !is_package_file(name) {
            bail!(trf!("the report has a file it should not have: {name}", "el informe tiene un fichero que no debería: {name}"));
        }
        files.push((name.to_owned(), text.to_owned()));
    }
    Ok((id.to_owned(), files))
}

/// Where a report received from someone else is kept.
pub fn received_dir(home: &Path, id: &str) -> PathBuf {
    home.join("reports").join("received").join(id)
}

/// Keeps the newest 10 reports of the last 30 days; the received ones stay.
pub fn prune(reports: &Path, now: i64) {
    let Ok(entries) = std::fs::read_dir(reports) else { return };
    let mut own: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            is_id(&name).then(|| (name, e.path()))
        })
        .collect();
    own.sort();
    own.reverse();
    let cutoff: String = time::utc(now - 30 * 86_400_000)[..10].chars().filter(|c| c.is_ascii_digit()).collect();
    for (index, (name, path)) in own.iter().enumerate() {
        if index >= 10 || name[2..10] < *cutoff {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}
