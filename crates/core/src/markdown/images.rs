//! The images a Markdown text cites, where each is written, and the copies Alondra keeps of them in
//! its files folder by their SHA256, which every client reads from the daemon like an attachment.
use super::html;
use pulldown_cmark::{Event, LinkType, Options, Parser, Tag, TagEnd};
use sha2::{Digest, Sha256};
use std::ops::Range;
use std::path::Path;

pub const IMAGE_MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const FOLDER: &str = "files/images";
pub const ROUTE: &str = "/files?path=";
pub const EXTENSIONS: [&str; 7] = ["png", "jpg", "jpeg", "gif", "webp", "heic", "svg"];

/// One image of a text: `span` is all of it as written (`![alt](dest)` or the `<img>` tag), `dest`
/// where its destination is written, which for a reference image is in its definition.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    pub span: Range<usize>,
    pub dest: Range<usize>,
    pub url: String,
    pub alt: String,
    pub html: bool,
}

/// Every image `markdown` cites, in the order they are written.
pub fn images(markdown: &str) -> Vec<Image> {
    let parser = Parser::new_ext(markdown, Options::all()).into_offset_iter();
    let definitions: Vec<_> = parser
        .reference_definitions()
        .iter()
        .map(|(_, def)| (def.dest.to_string(), def.span.clone()))
        .collect();
    let mut found = vec![];
    let mut open: Option<Image> = None;
    for (event, range) in parser {
        match event {
            Event::Start(Tag::Image { link_type, dest_url, .. }) if open.is_none() => {
                let dest = match link_type {
                    LinkType::Inline => markdown[range.clone()].rfind(&*dest_url).map(|at| range.start + at),
                    _ => definitions
                        .iter()
                        .find(|(dest, _)| dest.as_str() == &*dest_url)
                        .and_then(|(dest, span)| markdown[span.clone()].rfind(dest.as_str()).map(|at| span.start + at)),
                };
                open = dest.map(|start| Image {
                    span: range.clone(),
                    dest: start..start + dest_url.len(),
                    url: dest_url.to_string(),
                    alt: String::new(),
                    html: false,
                });
            }
            Event::Text(text) | Event::Code(text) if open.is_some() => {
                if let Some(image) = open.as_mut() {
                    image.alt.push_str(&text);
                }
            }
            Event::End(TagEnd::Image) => found.extend(open.take()),
            Event::Start(Tag::HtmlBlock) | Event::InlineHtml(_) if open.is_none() => found.extend(html_images(markdown, range)),
            _ => {}
        }
    }
    found.sort_by_key(|image| image.span.start);
    found.dedup_by_key(|image| image.span.start);
    found
}

fn html_images(markdown: &str, range: Range<usize>) -> Vec<Image> {
    let src = &markdown[range.clone()];
    let (pieces, _) = html::pieces(src);
    pieces
        .into_iter()
        .filter_map(|piece| match piece {
            html::Piece::Tag(tag) if tag.name == "img" && !tag.closing => {
                let url = tag.attr("src").filter(|s| !s.is_empty())?.to_owned();
                let tag_at = range.start + (tag.raw.as_ptr() as usize - src.as_ptr() as usize);
                let value_at = tag.raw.find("src").and_then(|s| tag.raw[s..].find(&url).map(|v| s + v))?;
                Some(Image {
                    span: tag_at..tag_at + tag.raw.len(),
                    dest: tag_at + value_at..tag_at + value_at + url.len(),
                    alt: tag.attr("alt").unwrap_or_default().to_owned(),
                    url,
                    html: true,
                })
            }
            _ => None,
        })
        .collect()
}

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

/// Keeps `bytes` in HOME's images folder and answers the destination that shows them.
pub fn keep(home: &Path, bytes: &[u8], extension: &str) -> std::io::Result<String> {
    let digest: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    let reference = format!("{FOLDER}/{digest}.{extension}");
    store(&home.join(&reference), bytes)?;
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

/// The kind of image `bytes` hold, by how the file starts, whatever its name says.
pub fn image_extension(bytes: &[u8]) -> Option<&'static str> {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some("webp")
    } else if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" && [&b"heic"[..], b"heix", b"mif1", b"msf1"].contains(&&bytes[8..12]) {
        Some("heic")
    } else if text.trim_start().starts_with("<svg") || (text.trim_start().starts_with("<?xml") && text.contains("<svg")) {
        Some("svg")
    } else {
        None
    }
}

#[cfg(test)]
#[path = "images_tests.rs"]
mod tests;
