//! Brings the images a pull request's description cites from GitHub with the Mac's `gh`, keeps
//! each in the files folder by its SHA256 and says why for each one it could not bring. A private
//! repository's images need GitHub's sign-in, which only the daemon has: the clients read the
//! copies from the daemon, through the same channel as any attachment. Nothing is sent anywhere.
use alondra_core::markdown::images::{self as markdown, IMAGE_MAX_BYTES};
use alondra_view_model::pr_images::{ImageProblem, PrImage, Shown};
use alondra_engine_ports::github::{SIGNED_IMAGE_HOST, encoded_path};
use std::collections::BTreeMap;
use std::path::Path;

/// More images than this in one description are not brought.
pub const MOST: usize = 30;

pub use alondra_engine_ports::github::{Failure, Fetch, GitHubImages};

/// The pull request whose description is read.
pub struct Target<'a> {
    pub host: &'a str,
    pub repository: &'a str,
    pub number: u64,
    pub head: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    File { repository: String, segments: Vec<String>, relative: bool },
    Uploaded { uuid: String },
}

#[derive(Debug, Default, PartialEq)]
pub struct Brought {
    pub images: Vec<PrImage>,
    /// Some image failed for a while and is worth asking again.
    pub retry: bool,
}

/// The images of `body`, each brought or with why not; `known` answers the ones already brought.
pub async fn bring(github: &dyn GitHubImages, home: &Path, target: &Target<'_>, body: &str, known: &BTreeMap<String, Shown>) -> Brought {
    let found: Vec<_> = markdown::images(body)
        .into_iter()
        .filter_map(|image| Some((source(target, &image.url)?, image)))
        .collect();
    let mut shown: BTreeMap<String, Shown> = BTreeMap::new();
    let mut html: Option<Result<String, Failure>> = None;
    let mut brought = Brought::default();
    for (source, image) in found {
        if !shown.contains_key(&image.url) {
            let outcome = if let Some(known) = known.get(&image.url) {
                known.clone()
            } else if shown.len() >= MOST {
                unavailable(target, &image.url, &source, ImageProblem::TooMany)
            } else {
                match fetch(github, target, &source, &mut html).await.and_then(|bytes| keep(home, &bytes)) {
                    Ok(route) => Shown::Stored { route },
                    Err(problem) => unavailable(target, &image.url, &source, problem),
                }
            };
            brought.retry |= matches!(&outcome, Shown::Unavailable { problem: ImageProblem::Retrying, .. });
            shown.insert(image.url.clone(), outcome);
        }
        brought.images.push(PrImage {
            shown: shown[&image.url].clone(),
            span: image.span,
            dest: image.dest,
            url: image.url,
            alt: image.alt,
            html: image.html,
        });
    }
    brought
}

/// The answers of `images` that hold at `head` too: an upload or a file at a commit never changes,
/// and the rest holds while the head is the same. One that failed for a while is asked again.
pub fn lasting(target: &Target<'_>, images: &[PrImage], same_head: bool) -> BTreeMap<String, Shown> {
    images
        .iter()
        .filter(|image| !matches!(image.shown, Shown::Unavailable { problem: ImageProblem::Retrying | ImageProblem::TooMany, .. }))
        .filter(|image| {
            same_head
                || matches!(image.shown, Shown::Stored { .. })
                    && match source(target, &image.url) {
                        Some(Source::Uploaded { .. }) => true,
                        Some(Source::File { segments, relative: false, .. }) => segments.first().is_some_and(|reference| commit(reference)),
                        _ => false,
                    }
        })
        .map(|image| (image.url.clone(), image.shown.clone()))
        .collect()
}

fn source(target: &Target<'_>, url: &str) -> Option<Source> {
    if relative(url) {
        let path = url.split(['?', '#']).next()?.trim_start_matches("./");
        let segments: Vec<String> = path.split('/').filter(|s| !s.is_empty()).map(str::to_owned).collect();
        return (!segments.is_empty()).then(|| Source::File { repository: target.repository.to_owned(), segments, relative: true });
    }
    let rest = url.strip_prefix("https://")?;
    let (host, path) = rest.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let path = path.split(['?', '#']).next()?;
    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let pr_host = alondra_domain_rules::normalize_pr_host(target.host);
    if host == SIGNED_IMAGE_HOST {
        let name = parts.last()?;
        let stem = name.split('.').next()?;
        let uuid = stem.get(stem.len().checked_sub(36)?..)?;
        return uuid_like(uuid).then(|| Source::Uploaded { uuid: uuid.to_ascii_lowercase() });
    }
    if host == "raw.githubusercontent.com" && pr_host == "github.com" && parts.len() >= 4 {
        return Some(Source::File { repository: format!("{}/{}", parts[0], parts[1]), segments: owned(&parts[2..]), relative: false });
    }
    if host != pr_host {
        return None;
    }
    match parts.as_slice() {
        ["user-attachments", "assets", uuid] if uuid_like(uuid) => Some(Source::Uploaded { uuid: uuid.to_ascii_lowercase() }),
        [_, _, "assets", _, uuid] if uuid_like(uuid) => Some(Source::Uploaded { uuid: uuid.to_ascii_lowercase() }),
        [owner, repo, "blob" | "raw", rest @ ..] if rest.len() >= 2 => {
            Some(Source::File { repository: format!("{owner}/{repo}"), segments: owned(rest), relative: false })
        }
        _ => None,
    }
}

fn relative(url: &str) -> bool {
    !url.is_empty() && !url.contains(':') && !url.starts_with(['/', '#', '~', '?']) && !url.chars().any(char::is_control)
}

fn owned(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_owned()).collect()
}

fn uuid_like(text: &str) -> bool {
    text.len() == 36
        && text.char_indices().all(|(at, c)| if [8, 13, 18, 23].contains(&at) { c == '-' } else { c.is_ascii_hexdigit() })
}

fn commit(reference: &str) -> bool {
    reference.len() == 40 && reference.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The reference and path a file link may mean: a branch name can hold `/` too.
fn readings(target: &Target<'_>, segments: &[String], relative: bool) -> Vec<(String, String)> {
    if relative {
        return vec![(target.head.to_owned(), segments.join("/"))];
    }
    if segments.first().is_some_and(|first| commit(first)) {
        return vec![(segments[0].clone(), segments[1..].join("/"))];
    }
    (1..segments.len().min(5)).map(|cut| (segments[..cut].join("/"), segments[cut..].join("/"))).collect()
}

async fn fetch(github: &dyn GitHubImages, target: &Target<'_>, source: &Source, html: &mut Option<Result<String, Failure>>) -> Result<Vec<u8>, ImageProblem> {
    match source {
        Source::File { repository, segments, relative } => {
            let readings = readings(target, segments, *relative);
            for (reference, path) in &readings {
                let path = encoded_path(path);
                match github.size(target.host, repository, &path, reference).await {
                    Ok(bytes) if bytes > IMAGE_MAX_BYTES => return Err(ImageProblem::TooLarge { bytes }),
                    Ok(_) => return github.file(target.host, repository, &path, reference).await.map_err(|failure| problem(failure, repository)),
                    Err(Failure::NotFound) => continue,
                    Err(failure) => return Err(problem(failure, repository)),
                }
            }
            match github.repository(target.host, repository).await {
                Ok(()) => Err(ImageProblem::NotInBranch { reference: readings.first().map(|(reference, _)| reference.clone()).unwrap_or_default() }),
                Err(Failure::NotFound) => Err(ImageProblem::NoAccess { repository: repository.clone() }),
                Err(failure) => Err(problem(failure, repository)),
            }
        }
        Source::Uploaded { uuid } => {
            if html.is_none() {
                *html = Some(github.body_html(target.host, target.repository, target.number).await);
            }
            let signed = match html.as_ref().expect("asked above") {
                Ok(html) => signed_links(html).remove(uuid).ok_or(ImageProblem::Gone)?,
                Err(failure) => return Err(problem(failure.clone(), target.repository)),
            };
            github.download(&signed).await.map_err(|failure| match failure {
                Failure::NotFound => ImageProblem::Gone,
                failure => problem(failure, target.repository),
            })
        }
    }
}

fn problem(failure: Failure, repository: &str) -> ImageProblem {
    match failure {
        Failure::NotFound => ImageProblem::NoAccess { repository: repository.to_owned() },
        Failure::SignedOut => ImageProblem::SignedOut,
        Failure::TooLarge(bytes) => ImageProblem::TooLarge { bytes },
        Failure::Transient => ImageProblem::Retrying,
    }
}

fn keep(home: &Path, bytes: &[u8]) -> Result<String, ImageProblem> {
    if bytes.len() as u64 > IMAGE_MAX_BYTES {
        return Err(ImageProblem::TooLarge { bytes: bytes.len() as u64 });
    }
    let extension = markdown::image_extension(bytes).ok_or(ImageProblem::NotAnImage)?;
    markdown::keep(home, bytes, extension).map_err(|error| {
        log!("could not keep an image of a pull request description: {error}");
        ImageProblem::Retrying
    })
}

fn unavailable(target: &Target<'_>, url: &str, source: &Source, problem: ImageProblem) -> Shown {
    let link = match source {
        Source::File { repository, segments, relative: true } => {
            format!("https://{}/{repository}/blob/{}/{}", alondra_domain_rules::normalize_pr_host(target.host), target.head, encoded_path(&segments.join("/")))
        }
        _ => url.to_owned(),
    };
    Shown::Unavailable { link, problem }
}

/// The signed link of each uploaded image of a rendered description, by the upload's UUID.
fn signed_links(html: &str) -> BTreeMap<String, String> {
    let mut links = BTreeMap::new();
    for quoted in html.split('"').skip(1).step_by(2) {
        let link = quoted.replace("&amp;", "&");
        let Some(rest) = link.strip_prefix("https://").and_then(|rest| rest.strip_prefix(SIGNED_IMAGE_HOST)) else { continue };
        let name = rest.split('?').next().unwrap_or_default().rsplit('/').next().unwrap_or_default();
        let stem = name.split('.').next().unwrap_or_default();
        if let Some(uuid) = stem.get(stem.len().saturating_sub(36)..).filter(|uuid| uuid_like(uuid)) {
            links.entry(uuid.to_ascii_lowercase()).or_insert(link);
        }
    }
    links
}

#[path = "pr_images_fake.rs"]
pub mod fake;
#[cfg(test)]
#[path = "pr_images_tests.rs"]
mod tests;
