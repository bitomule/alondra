use std::collections::{BTreeMap, BTreeSet};

use crate::pattern::matches;
use crate::problem::{problem, too_large};
use crate::{Code, LoadedSpec, Part, PartContent, PartKind, Problem, Scenario, Spec, fingerprint};

pub(crate) const MAX_PER_SPEC: usize = 16;
const DIAGRAM_MAX_BYTES: usize = 65_536;
const MOCKUP_MAX_BYTES: usize = 2_097_152;
const DIAGRAM_TYPES: [&str; 6] = ["flowchart", "graph", "sequenceDiagram", "stateDiagram", "stateDiagram-v2", "classDiagram"];
const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
const JPEG_SIGNATURE: &[u8] = b"\xff\xd8\xff";

/// A `@diagram:` or `@mockup:` tag as written in a feature file.
pub(crate) struct Citation {
    pub(crate) kind: PartKind,
    pub(crate) file: String,
    pub(crate) line: usize,
}

fn extensions(kind: PartKind) -> &'static [&'static str] {
    match kind {
        PartKind::Diagram => &[".mmd"],
        PartKind::Mockup => &[".png", ".jpg", ".jpeg"],
    }
}

/// Why the file a tag names cannot be a part of that kind, whatever the file holds.
pub(crate) fn name_problem(kind: PartKind, file: &str) -> Option<(Code, String)> {
    let leaves = file.starts_with('/') || file.contains('\\') || file.split('/').any(|segment| matches!(segment, "" | "." | ".."));
    if leaves && !file.is_empty() {
        return Some((Code::PartOutsideComponent, format!("{file:?} is not a path inside the folder of the component")));
    }
    let admitted = extensions(kind);
    if admitted.iter().any(|extension| file.len() > extension.len() && file.ends_with(extension)) {
        return None;
    }
    Some((Code::InvalidPart, format!("a {} is a {} file, and {file:?} is not one", kind.as_str(), admitted.join(" or "))))
}

fn looks_like_part(path: &str) -> bool {
    [PartKind::Diagram, PartKind::Mockup].into_iter().flat_map(extensions).any(|extension| path.ends_with(extension))
}

/// The path from the root of the repository of a file a tag of the feature at `feature` names.
pub(crate) fn path_of(feature: &str, file: &str) -> String {
    match feature.rsplit_once('/') {
        Some((folder, _)) => format!("{folder}/{file}"),
        None => file.to_string(),
    }
}

/// The parts the tags of one feature or scenario cite, diagrams first.
pub(crate) fn cited<'a>(diagrams: &'a [String], mockups: &'a [String]) -> impl Iterator<Item = (PartKind, &'a str)> {
    let diagrams = diagrams.iter().map(|file| (PartKind::Diagram, file.as_str()));
    diagrams.chain(mockups.iter().map(|file| (PartKind::Mockup, file.as_str())))
}

pub(crate) fn of_spec(spec: &Spec) -> impl Iterator<Item = (PartKind, &str)> {
    cited(&spec.diagrams, &spec.mockups)
}

pub(crate) fn of_scenario(scenario: &Scenario) -> impl Iterator<Item = (PartKind, &str)> {
    cited(&scenario.diagrams, &scenario.mockups)
}

pub(crate) fn read(path: &str, kind: PartKind, bytes: &[u8]) -> Result<Part, Problem> {
    let max = match kind {
        PartKind::Diagram => DIAGRAM_MAX_BYTES,
        PartKind::Mockup => MOCKUP_MAX_BYTES,
    };
    if let Some(found) = too_large(path, bytes, max) {
        return Err(found);
    }
    let content = match kind {
        PartKind::Diagram => diagram(bytes),
        PartKind::Mockup => mockup(bytes),
    }
    .map_err(|why| problem(Code::InvalidPart, path, 0, why))?;
    Ok(Part { path: path.to_string(), fingerprint: fingerprint::of_bytes(bytes), bytes: bytes.len(), content })
}

fn diagram(bytes: &[u8]) -> Result<PartContent, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| "a diagram is UTF-8 text".to_string())?;
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("%%"))
        .and_then(|line| line.split(|c: char| c.is_whitespace() || c == ';').next())
        .unwrap_or_default();
    if !DIAGRAM_TYPES.contains(&first) {
        return Err(format!("a diagram starts with one of {}, and this one starts with {first:?}", DIAGRAM_TYPES.join(", ")));
    }
    Ok(PartContent::Diagram { text: text.to_string() })
}

fn mockup(bytes: &[u8]) -> Result<PartContent, String> {
    let size = if bytes.starts_with(PNG_SIGNATURE) {
        png_size(bytes)
    } else if bytes.starts_with(JPEG_SIGNATURE) {
        jpeg_size(bytes)
    } else {
        return Err("a mockup is a PNG or a JPEG image, and these bytes are neither".to_string());
    };
    match size {
        Some((width, height)) if width > 0 && height > 0 => Ok(PartContent::Mockup { width, height }),
        _ => Err("the width and height of the image cannot be read".to_string()),
    }
}

/// The size in the `IHDR` chunk, which is always the first one.
fn png_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let chunk = bytes.get(PNG_SIGNATURE.len()..PNG_SIGNATURE.len() + 16)?;
    if chunk.get(4..8)? != b"IHDR" {
        return None;
    }
    let word = |at: usize| Some(u32::from_be_bytes(chunk.get(at..at + 4)?.try_into().ok()?));
    Some((word(8)?, word(12)?))
}

/// The size in the first frame header, walking the segments before it.
fn jpeg_size(bytes: &[u8]) -> Option<(u32, u32)> {
    let mut at = 2;
    loop {
        if *bytes.get(at)? != 0xff {
            return None;
        }
        let marker = *bytes.get(at + 1)?;
        match marker {
            0xff => at += 1,
            0x01 | 0xd0..=0xd8 => at += 2,
            0xd9 | 0xda => return None,
            _ => {
                let short = |from: usize| Some(u16::from_be_bytes(bytes.get(from..from + 2)?.try_into().ok()?));
                if matches!(marker, 0xc0..=0xcf) && !matches!(marker, 0xc4 | 0xc8 | 0xcc) {
                    return Some((u32::from(short(at + 7)?), u32::from(short(at + 5)?)));
                }
                let length = usize::from(short(at + 2)?);
                if length < 2 {
                    return None;
                }
                at += 2 + length;
            }
        }
    }
}

/// The files with the extension of a part that lie in the folder of a component whose specs all
/// read, and that no tag cites nor `@evidence:` pattern matches.
pub(crate) fn uncited(
    specs_dir: &str,
    folders: &BTreeSet<String>,
    specs: &[LoadedSpec],
    parts: &BTreeMap<String, Part>,
    refused: &BTreeSet<String>,
    listed: &[String],
) -> Vec<Problem> {
    let prefix = format!("{specs_dir}/");
    let evidence: Vec<&str> = specs
        .iter()
        .flat_map(|loaded| &loaded.spec.scenarios)
        .filter_map(|scenario| scenario.judgment.as_ref()?.pattern.as_deref())
        .collect();
    listed
        .iter()
        .filter(|path| looks_like_part(path) && !parts.contains_key(*path) && !refused.contains(*path))
        .filter(|path| {
            let inside = path.strip_prefix(prefix.as_str()).and_then(|rest| rest.split_once('/'));
            inside.is_some_and(|(folder, _)| folders.contains(folder))
        })
        .filter(|path| !evidence.iter().any(|pattern| matches(pattern, path)))
        .map(|path| problem(Code::UncitedPart, path, 0, "no @diagram: or @mockup: tag cites this file"))
        .collect()
}
