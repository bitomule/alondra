//! The images of a pull request's description as the daemon brought them from GitHub: each points
//! at the copy the daemon keeps, or says in its alt text why it could not be brought, so every
//! client shows it without reaching GitHub itself.
use super::text::{t, tf};
use super::Lang;
use serde::{Deserialize, Serialize};
use std::ops::Range;

/// One image of a description, found at `span` (and its destination at `dest`) of that exact text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrImage {
    pub span: Range<usize>,
    pub dest: Range<usize>,
    pub url: String,
    pub alt: String,
    pub html: bool,
    pub shown: Shown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shown {
    Stored { route: String },
    /// `link` opens the image on GitHub, where a signed-in browser may still see it.
    Unavailable { link: String, problem: ImageProblem },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum ImageProblem {
    /// `reference` is the commit or branch the description reads the file at.
    NotInBranch { reference: String },
    Gone,
    NoAccess { repository: String },
    SignedOut,
    TooLarge { bytes: u64 },
    NotAnImage,
    TooMany,
    Retrying,
}

impl ImageProblem {
    /// The image does not exist where the description says: the author has to fix the description.
    pub fn missing(&self) -> bool {
        matches!(self, Self::NotInBranch { .. } | Self::Gone)
    }

    pub fn text(&self, lang: Lang) -> String {
        match self {
            Self::NotInBranch { reference } => {
                let commit = reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit());
                let reference = if commit { &reference[..7] } else { reference.as_str() };
                tf!(lang, "not in the branch ({reference})", "no está en la rama ({reference})", reference = reference)
            }
            Self::Gone => t!(lang, "GitHub no longer has it", "GitHub ya no la tiene").into(),
            Self::NoAccess { repository } => tf!(
                lang,
                "GitHub gives no access to {repository}",
                "GitHub no da acceso a {repository}",
                repository = repository
            ),
            Self::SignedOut => t!(lang, "gh is not signed in to GitHub on the Mac", "gh no tiene sesión de GitHub en el Mac").into(),
            Self::TooLarge { bytes } => {
                let size = crate::machine_incident::bytes_text(*bytes, lang);
                tf!(lang, "{size}, more than the 10 MB limit", "pesa {size}, más del máximo de 10 MB", size = size)
            }
            Self::NotAnImage => t!(lang, "it is not an image", "no es una imagen").into(),
            Self::TooMany => t!(lang, "the description has more than 30 images", "la descripción tiene más de 30 imágenes").into(),
            Self::Retrying => t!(
                lang,
                "could not be downloaded from GitHub; trying again",
                "no se ha podido descargar de GitHub; se reintenta"
            )
            .into(),
        }
    }
}

/// `markdown` with each image of `images` pointing at its copy or saying why it has none. An image
/// whose destination is no longer written where it was found is left as it is.
pub fn show(markdown: &str, images: &[PrImage], lang: Lang) -> String {
    let mut edits: Vec<(Range<usize>, String)> = vec![];
    for image in images.iter().filter(|image| markdown.get(image.dest.clone()) == Some(image.url.as_str())) {
        let edit = match &image.shown {
            Shown::Stored { route } if image.alt.trim().is_empty() && image.html => {
                let tag = markdown.get(image.span.clone()).unwrap_or_default();
                let at = image.dest.start.saturating_sub(image.span.start);
                let Some(after) = tag.get(at + image.url.len()..) else { continue };
                let named = format!("<img alt=\"{}\"{}{route}{after}", attribute(&label(image, lang)), &tag[4..at]);
                (image.span.clone(), named)
            }
            Shown::Stored { route } if image.alt.trim().is_empty() => (image.span.clone(), written(false, &label(image, lang), route)),
            Shown::Stored { route } => (image.dest.clone(), route.clone()),
            Shown::Unavailable { link, problem } => {
                let label = format!("{} — {}", label(image, lang), problem.text(lang));
                (image.span.clone(), written(image.html, &label, link))
            }
        };
        if markdown.get(edit.0.clone()).is_some() && !edits.iter().any(|(at, _)| at.start < edit.0.end && edit.0.start < at.end) {
            edits.push(edit);
        }
    }
    edits.sort_by_key(|(at, _)| std::cmp::Reverse(at.start));
    let mut out = markdown.to_owned();
    for (at, text) in edits {
        out.replace_range(at, &text);
    }
    out
}

/// `text`, which holds `body` or is a part of it, with the images of `body` shown.
pub fn show_in(text: &str, body: &str, images: &[PrImage], lang: Lang) -> String {
    let trimmed = body.trim();
    if trimmed.is_empty() || images.is_empty() {
        return text.to_owned();
    }
    if let Some(at) = text.find(trimmed) {
        let mut out = text.to_owned();
        out.replace_range(at..at + trimmed.len(), show(body, images, lang).trim());
        return out;
    }
    let Some(at) = body.find(text).filter(|_| !text.is_empty()) else { return text.to_owned() };
    let within = at..at + text.len();
    let shifted: Vec<PrImage> = images
        .iter()
        .filter(|image| within.start <= image.span.start && image.span.end <= within.end)
        .filter(|image| within.start <= image.dest.start && image.dest.end <= within.end)
        .map(|image| PrImage { span: image.span.start - at..image.span.end - at, dest: image.dest.start - at..image.dest.end - at, ..image.clone() })
        .collect();
    show(text, &shifted, lang)
}

fn label(image: &PrImage, lang: Lang) -> String {
    let alt = image.alt.trim();
    if !alt.is_empty() && !alt.eq_ignore_ascii_case("image") {
        return alt.to_owned();
    }
    let path = image.url.split(['?', '#']).next().unwrap_or_default();
    let name = path.rsplit('/').find(|segment| !segment.is_empty()).unwrap_or(path);
    let uploaded = name.len() == 36 && name.matches('-').count() == 4;
    if uploaded || name.is_empty() { t!(lang, "Image", "Imagen").into() } else { name.to_owned() }
}

fn attribute(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;")
}

fn written(html: bool, label: &str, link: &str) -> String {
    if html {
        return format!("<img alt=\"{}\" src=\"{}\">", attribute(label), attribute(link));
    }
    let label: String = label.chars().flat_map(|c| if matches!(c, '[' | ']' | '\\') { vec!['\\', c] } else { vec![c] }).collect();
    if link.contains([' ', '(', ')', '<', '>']) {
        format!("![{label}](<{}>)", link.replace(['<', '>'], ""))
    } else {
        format!("![{label}]({link})")
    }
}

#[cfg(test)]
#[path = "pr_images_tests.rs"]
mod tests;
