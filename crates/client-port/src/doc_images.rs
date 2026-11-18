//! Images a document cites by a path relative to where its author works (`![](docs/shot.png)`).
//! The node copies each into Alondra's files folder by its SHA256 and points the document at the
//! copy, so every client reads it from the daemon like an attachment.
pub use alondra_core::markdown::images::{IMAGE_MAX_BYTES, stored_image};
use alondra_core::markdown::images::{EXTENSIONS, FOLDER, ROUTE, images, store};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

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
    let mut found: Vec<_> = images(markdown).into_iter().filter(|image| relative(&image.url)).map(|image| (image.dest, image.url)).collect();
    found.sort_by_key(|(at, _)| at.start);
    found.dedup_by_key(|(at, _)| at.start);
    found
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
