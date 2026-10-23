//! Images a document cites by a path relative to where its author works (`![](docs/shot.png)`).
//! The node copies each into Alondra's files folder by its SHA256 and points the document at the
//! copy, so every client reads it from the daemon like an attachment.
use crate::tui::md::html;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

pub const IMAGE_MAX_BYTES: u64 = 10 * 1024 * 1024;
const FOLDER: &str = "files/images";
const ROUTE: &str = "/files?path=";
const EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "heic", "svg"];

/// The stored copy an image destination names, as a reference under the daemon's HOME.
pub fn stored_image(url: &str) -> Option<&str> {
    let reference = url.strip_prefix(ROUTE)?;
    let name = reference.strip_prefix(FOLDER)?.strip_prefix('/')?;
    let (digest, extension) = name.split_once('.')?;
    let valid = digest.len() == 64
        && digest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && EXTENSIONS.contains(&extension);
    valid.then_some(reference)
}

#[derive(Debug, PartialEq)]
pub enum Skipped {
    Outside,
    Missing,
    TooLarge,
    NotAnImage,
}

/// The document with every relative image it could copy pointing at its copy, and the images it
/// left as they were, with why.
pub fn carry(markdown: &str, cwd: &Path, home: &Path) -> (String, Vec<(String, Skipped)>) {
    let places = relative_images(markdown);
    if places.is_empty() {
        return (markdown.to_owned(), vec![]);
    }
    let mut routes = BTreeMap::new();
    let mut skipped = vec![];
    for (_, dest) in &places {
        if routes.contains_key(dest) || skipped.iter().any(|(d, _)| d == dest) {
            continue;
        }
        match copy(dest, cwd, home) {
            Ok(route) => {
                routes.insert(dest.clone(), route);
            }
            Err(why) => skipped.push((dest.clone(), why)),
        }
    }
    let mut out = markdown.to_owned();
    let mut places: Vec<_> = places.into_iter().filter_map(|(at, dest)| Some((at, routes.get(&dest)?.clone()))).collect();
    places.sort_by_key(|(at, _)| std::cmp::Reverse(at.start));
    for (at, route) in places {
        out.replace_range(at, &route);
    }
    (out, skipped)
}

/// Where each relative image destination is written in `markdown`, by its byte range.
fn relative_images(markdown: &str) -> Vec<(Range<usize>, String)> {
    let parser = Parser::new_ext(markdown, Options::all()).into_offset_iter();
    let definitions: Vec<_> = parser
        .reference_definitions()
        .iter()
        .map(|(_, def)| (def.dest.to_string(), def.span.clone()))
        .collect();
    let mut found = vec![];
    let mut referenced = vec![];
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Image { link_type, dest_url, .. }) if relative(&dest_url) => match link_type {
                LinkType::Inline => {
                    let written = &markdown[range.clone()];
                    if let Some(at) = written.rfind(&*dest_url) {
                        let start = range.start + at;
                        found.push((start..start + dest_url.len(), dest_url.to_string()));
                    }
                }
                _ => referenced.push(dest_url.to_string()),
            },
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) => found.extend(html_images(markdown, range)),
            _ => {}
        }
    }
    for (dest, span) in definitions {
        if referenced.contains(&dest)
            && let Some(at) = markdown[span.clone()].rfind(&dest)
        {
            let start = span.start + at;
            found.push((start..start + dest.len(), dest));
        }
    }
    found.sort_by_key(|(at, _)| at.start);
    found.dedup_by_key(|(at, _)| at.start);
    found
}

fn html_images(markdown: &str, range: Range<usize>) -> Vec<(Range<usize>, String)> {
    let src = &markdown[range.clone()];
    let (pieces, _) = html::pieces(src);
    pieces
        .into_iter()
        .filter_map(|piece| match piece {
            html::Piece::Tag(tag) if tag.name == "img" && !tag.closing => {
                let dest = tag.attr("src").filter(|s| relative(s))?.to_owned();
                let tag_at = tag.raw.as_ptr() as usize - src.as_ptr() as usize;
                let value_at = tag.raw.find("src").and_then(|s| tag.raw[s..].find(&dest).map(|v| s + v))?;
                let start = range.start + tag_at + value_at;
                Some((start..start + dest.len(), dest))
            }
            _ => None,
        })
        .collect()
}

fn relative(dest: &str) -> bool {
    !dest.is_empty()
        && !dest.contains(':')
        && !dest.starts_with(['/', '#', '~', '?'])
        && !dest.chars().any(char::is_control)
}

fn copy(dest: &str, cwd: &Path, home: &Path) -> Result<String, Skipped> {
    let path = dest.split(['?', '#']).next().unwrap_or(dest);
    let extension = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .filter(|e| EXTENSIONS.contains(&e.as_str()))
        .ok_or(Skipped::NotAnImage)?;
    let root = cwd.canonicalize().map_err(|_| Skipped::Missing)?;
    let file = cwd.join(path).canonicalize().map_err(|_| Skipped::Missing)?;
    if !file.starts_with(&root) {
        return Err(Skipped::Outside);
    }
    let meta = std::fs::metadata(&file).map_err(|_| Skipped::Missing)?;
    if !meta.is_file() {
        return Err(Skipped::Missing);
    }
    if meta.len() > IMAGE_MAX_BYTES {
        return Err(Skipped::TooLarge);
    }
    let bytes = std::fs::read(&file).map_err(|_| Skipped::Missing)?;
    if bytes.len() as u64 > IMAGE_MAX_BYTES {
        return Err(Skipped::TooLarge);
    }
    let digest: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let reference = format!("{FOLDER}/{digest}.{extension}");
    store(&home.join(&reference), &bytes).map_err(|_| Skipped::Missing)?;
    Ok(format!("{ROUTE}{reference}"))
}

/// Writes `bytes` at `target` once: a content-addressed copy already there is the same file.
pub fn store(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if target.is_file() {
        return Ok(());
    }
    let folder = target.parent().ok_or(std::io::ErrorKind::InvalidInput)?;
    std::fs::create_dir_all(folder)?;
    let partial = folder.join(format!(".{}.partial-{}", target.file_name().and_then(|n| n.to_str()).unwrap_or("copy"), std::process::id()));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&partial);
    })
}

/// What the node reads about the images it cited that stay as they were.
pub fn notice(skipped: &[(String, Skipped)]) -> Option<String> {
    if skipped.is_empty() {
        return None;
    }
    let each: Vec<String> = skipped
        .iter()
        .map(|(dest, why)| {
            let why = match why {
                Skipped::Outside => "it is outside your working folder",
                Skipped::Missing => "there is no such file in your working folder",
                Skipped::TooLarge => "it is larger than 10 MB",
                Skipped::NotAnImage => "it is not a png, jpg, gif, webp, heic or svg image",
            };
            format!("{dest} ({why})")
        })
        .collect();
    Some(format!(
        "These images were not copied, so the user sees only their alt text: {}. Fix the path and send it again if they should show.",
        each.join("; ")
    ))
}

/// The document a node cites by its stored path, with its relative images carried: a new stored
/// file and its SHA256, or `None` when nothing changed.
pub fn carry_document(reference: &str, digest: &str, cwd: &Path, home: &Path) -> Option<(PathBuf, String, Vec<(String, Skipped)>)> {
    let path = Path::new(reference);
    let path = if path.is_absolute() { path.to_path_buf() } else { home.join(path) };
    let bytes = std::fs::read(&path).ok()?;
    let actual: String = Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if actual != digest {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    let (carried, skipped) = carry(&text, cwd, home);
    if carried == text {
        return (!skipped.is_empty()).then(|| (path, actual, skipped));
    }
    let digest: String = Sha256::digest(carried.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
    let target = home.join("files/documents").join(format!("{digest}.md"));
    store(&target, carried.as_bytes()).ok()?;
    Some((target, digest, skipped))
}

#[cfg(test)]
#[path = "doc_images_tests.rs"]
mod tests;
