//! The diagrams and mockups of a task's specs. A diagram is a text file of the proposal like any
//! other; a mockup is an image Alondra keeps by its fingerprint in its files folder, and what the
//! task records of it is that fingerprint, its size and its dimensions.
use crate::specs::{engine, fingerprint_of, Files, SpecDirs, SpecDraft, SpecEditError, SpecFileKind, SpecFiles};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

/// The most bytes of a mockup, the engine's own limit.
pub const MAX_MOCKUP_BYTES: usize = 2_097_152;

/// An image Alondra keeps: a mockup of a spec, or a capture a validator judged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredImage {
    /// Where the copy is under Alondra's home: `files/images/<sha256>.<png|jpg>`.
    pub file: String,
    /// `sha256:` and the digest of its bytes.
    pub fingerprint: String,
    pub bytes: u64,
    /// In pixels; 0 when they could not be read.
    pub width: u32,
    pub height: u32,
}

impl StoredImage {
    /// What Alondra records of a PNG or JPEG, and the copy's place; `None` for any other bytes.
    pub fn describe(bytes: &[u8]) -> Option<Self> {
        let extension = match alondra_core::markdown::images::image_extension(bytes)? {
            found @ ("png" | "jpg") => found,
            _ => return None,
        };
        let header = &bytes[..bytes.len().min(MAX_MOCKUP_BYTES)];
        let (width, height) = match engine::read_part("image", engine::PartKind::Mockup, header).ok()?.content {
            engine::PartContent::Mockup { width, height } => (width, height),
            engine::PartContent::Diagram { .. } => return None,
        };
        let fingerprint = fingerprint_of(Some(bytes));
        let digest = fingerprint.strip_prefix("sha256:")?.to_owned();
        Some(Self { file: format!("{}/{digest}.{extension}", alondra_core::markdown::images::FOLDER), fingerprint, bytes: bytes.len() as u64, width, height })
    }

    /// Whether `file` names the copy this fingerprint would be kept at.
    pub fn well_formed(&self) -> bool {
        let route = format!("{}{}", alondra_core::markdown::images::ROUTE, self.file);
        alondra_core::markdown::images::stored_image(&route).is_some()
            && self.fingerprint.strip_prefix("sha256:").is_some_and(|digest| self.file.contains(digest))
    }

    /// The destination a client reads it from.
    pub fn route(&self) -> String {
        format!("{}{}", alondra_core::markdown::images::ROUTE, self.file)
    }
}

/// What one file of a spec change holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SpecContent {
    Text(String),
    Mockup(StoredImage),
}

impl SpecContent {
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Mockup(_) => None,
        }
    }

    pub fn image(&self) -> Option<&StoredImage> {
        match self {
            Self::Mockup(image) => Some(image),
            Self::Text(_) => None,
        }
    }
}

impl From<&str> for SpecContent {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for SpecContent {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

/// The images Alondra keeps, as the rules read them: the domain never touches the disk itself.
pub trait KeptImages {
    /// Keeps a copy of a PNG or JPEG; `None` for other bytes or when it cannot be kept.
    fn keep(&self, bytes: &[u8]) -> Option<StoredImage>;
    /// The bytes of a kept image, only while they still match its fingerprint.
    fn read(&self, image: &StoredImage) -> Option<Vec<u8>>;
}

/// Where nothing keeps images: a mockup reads as absent.
pub struct NoImages;

impl KeptImages for NoImages {
    fn keep(&self, _: &[u8]) -> Option<StoredImage> {
        None
    }

    fn read(&self, _: &StoredImage) -> Option<Vec<u8>> {
        None
    }
}

fn mockup_name(path: &str) -> bool {
    [".png", ".jpg", ".jpeg"].iter().any(|extension| path.ends_with(extension))
}

/// What the file at `path` of `files` holds: a PNG or JPEG as a kept mockup, any other file as
/// its text; `None` when it does not exist or is neither.
pub fn content_of(files: &dyn Files, path: &str, images: &dyn KeptImages) -> Option<SpecContent> {
    let bytes = files.read(path)?;
    if mockup_name(path) && let Some(image) = images.keep(&bytes) {
        return Some(SpecContent::Mockup(image));
    }
    String::from_utf8(bytes).ok().map(SpecContent::Text)
}

/// The files the tags of a spec cite, each as its path from the root of the repository.
pub fn citations(path: &str, spec: &engine::Spec) -> BTreeSet<String> {
    spec.diagrams.iter().chain(&spec.mockups)
        .chain(spec.scenarios.iter().flat_map(|scenario| scenario.diagrams.iter().chain(&scenario.mockups)))
        .map(|file| engine::part_path(path, file))
        .collect()
}

fn spec_of(files: &dyn Files, path: &str) -> Option<engine::Spec> {
    files.read(path).and_then(|bytes| engine::read_spec(path, &bytes).ok())
}

/// What the specs a change touches cite, as `after` holds each part, and which of those another
/// spec of the project cites too.
pub(crate) fn cited_parts(after: &dyn Files, base: &dyn Files, changes: &SpecFiles, dirs: &SpecDirs, images: &dyn KeptImages)
    -> (BTreeMap<String, SpecContent>, BTreeSet<String>) {
    let changed = |path: &str| changes.contains_key(path) && dirs.kind(path) == Some(SpecFileKind::Spec);
    let mut now = BTreeSet::new();
    let mut ever = BTreeSet::new();
    for path in changes.keys().filter(|path| changed(path)) {
        if let Some(spec) = spec_of(after, path) {
            now.extend(citations(path, &spec));
        }
        if let Some(spec) = spec_of(base, path) {
            ever.extend(citations(path, &spec));
        }
    }
    ever.extend(now.iter().cloned());
    let mut shared = BTreeSet::new();
    if !ever.is_empty() {
        for path in after.paths().iter().filter(|path| dirs.kind(path) == Some(SpecFileKind::Spec) && !changed(path)) {
            if let Some(spec) = spec_of(after, path) {
                shared.extend(citations(path, &spec).intersection(&ever).cloned());
            }
        }
    }
    let parts = now.into_iter().filter_map(|path| content_of(after, &path, images).map(|content| (path, content))).collect();
    (parts, shared)
}

/// The title of each decision the specs at `paths` cite, as their decision files give it.
pub fn decision_titles<'a>(files: &dyn Files, paths: impl Iterator<Item = &'a str>, dirs: &SpecDirs) -> BTreeMap<String, String> {
    paths.filter_map(|path| spec_of(files, path)).flat_map(|spec| spec.decisions)
        .filter_map(|id| {
            let text = String::from_utf8(files.read(&dirs.decision_path(&id)?)?).ok()?;
            let title = crate::specs::frontmatter(&text, "title").filter(|title| !title.is_empty())?;
            Some((id, title))
        })
        .collect()
}

/// Whether the project already has the file at `path`, whatever the proposal does with it.
fn in_repository(draft: &SpecDraft, path: &str) -> bool {
    match draft.files.get(path) {
        Some(_) => draft.check.before.get(path).is_some_and(Option::is_some),
        None => draft.check.parts.contains_key(path),
    }
}

fn cited_by_draft(files: &SpecFiles, draft: &SpecDraft) -> BTreeSet<String> {
    files.iter()
        .filter(|(path, _)| draft.check.dirs.kind(path) == Some(SpecFileKind::Spec))
        .filter_map(|(path, content)| Some((path, engine::read_spec(path, content.as_ref()?.text()?.as_bytes()).ok()?)))
        .flat_map(|(path, spec)| citations(path, &spec))
        .collect()
}

/// Removes the parts a changed spec no longer cites and no other spec cites: the ones the proposal
/// added leave it, and the ones the project has are removed with the change.
pub(crate) fn drop_uncited(files: &mut SpecFiles, draft: &SpecDraft, path: &str, was: &engine::Spec, now: &engine::Spec) {
    let kept = cited_by_draft(files, draft);
    for part in citations(path, was).difference(&citations(path, now)) {
        if kept.contains(part) || draft.check.shared.contains(part) {
            continue;
        }
        if in_repository(draft, part) {
            files.insert(part.clone(), None);
        } else {
            files.remove(part);
        }
    }
}

/// A spec taken out of the proposal takes with it what the proposal did to its parts: the project
/// keeps them as they are.
pub(crate) fn withdraw(files: &mut SpecFiles, draft: &SpecDraft, path: &str, was: &engine::Spec) {
    let kept = cited_by_draft(files, draft);
    for part in citations(path, was) {
        if !kept.contains(&part) {
            files.remove(&part);
        }
    }
}

fn tags<'a>(spec: &'a mut engine::Spec, scenario: Option<&str>, kind: engine::PartKind) -> Option<&'a mut Vec<String>> {
    match scenario {
        None => Some(match kind {
            engine::PartKind::Diagram => &mut spec.diagrams,
            engine::PartKind::Mockup => &mut spec.mockups,
        }),
        Some(id) => spec.scenarios.iter_mut().find(|known| known.id == id).map(|known| match kind {
            engine::PartKind::Diagram => &mut known.diagrams,
            engine::PartKind::Mockup => &mut known.mockups,
        }),
    }
}

/// Removes the tag that cites `file`; false when there was none.
pub(crate) fn uncite(spec: &mut engine::Spec, scenario: Option<&str>, kind: engine::PartKind, file: &str) -> bool {
    tags(spec, scenario, kind).is_some_and(|tags| {
        let before = tags.len();
        tags.retain(|known| known != file);
        tags.len() != before
    })
}

/// The file name of a part as the user typed it: lowercase words joined by hyphens, with the
/// extension of its kind added when it has none.
pub(crate) fn file_name(typed: &str, kind: engine::PartKind, image: Option<&crate::specs::StoredImage>) -> Result<String, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("Give the file a name".into());
    }
    if typed.chars().any(|c| c.is_whitespace() || c == '\\' || c.is_control()) {
        return Err("A file name has no spaces: use hyphens".into());
    }
    let extensions: &[&str] = match (kind, image) {
        (engine::PartKind::Diagram, _) => &[".mmd"],
        (engine::PartKind::Mockup, Some(image)) if image.file.ends_with(".png") => &[".png"],
        (engine::PartKind::Mockup, Some(_)) => &[".jpg", ".jpeg"],
        (engine::PartKind::Mockup, None) => &[".png", ".jpg", ".jpeg"],
    };
    if extensions.iter().any(|extension| typed.len() > extension.len() && typed.ends_with(extension)) {
        return Ok(typed.to_owned());
    }
    let last = typed.rsplit('/').next().unwrap_or(typed);
    match last.rsplit_once('.') {
        Some(_) => Err(format!("This file is a {} file", extensions.join(" or "))),
        None => Ok(format!("{typed}{}", extensions[0])),
    }
}

pub(crate) struct Put<'a> {
    pub scenario: Option<&'a str>,
    /// The file name of the part it takes the place of.
    pub replaces: Option<&'a str>,
    pub kind: engine::PartKind,
    pub name: &'a str,
    pub content: crate::specs::SpecContent,
}

/// Writes a part and the tag that cites it into the proposal.
pub(crate) fn put(files: &mut SpecFiles, draft: &SpecDraft, path: &str, was: &engine::Spec, change: Put<'_>) -> Result<(), SpecEditError> {
    let refused = |field: &str, message: String| SpecEditError { field: field.into(), message };
    let mut spec = was.clone();
    let full = engine::part_path(path, change.name);
    let cited = tags(&mut spec, change.scenario, change.kind).ok_or_else(|| refused("scenario", "The spec no longer has that scenario".into()))?;
    match change.replaces {
        Some(old) => {
            let place = cited.iter().position(|known| known == old).ok_or_else(|| refused("name", format!("The spec no longer cites {old}")))?;
            cited[place] = change.name.to_owned();
        }
        None => {
            if cited.iter().any(|known| known == change.name) {
                return Err(refused("name", format!("It already cites {}", change.name)));
            }
            cited.push(change.name.to_owned());
        }
    }
    let replaced_in_place = change.replaces == Some(change.name);
    if !replaced_in_place && (citations(path, was).contains(&full) || draft.check.shared.contains(&full)) {
        let same = draft.files.get(&full).cloned().flatten().or_else(|| draft.check.parts.get(&full).cloned());
        if same.as_ref() != Some(&change.content) {
            return Err(refused("name", format!("{full} already holds something else: choose another name")));
        }
    }
    files.insert(full, Some(change.content));
    let text = engine::write_spec(&spec);
    engine::read_spec(path, text.as_bytes()).map_err(|problems| refused("name", problems.first().map_or_else(String::new, |p| p.message.clone())))?;
    files.insert(path.to_owned(), Some(crate::specs::SpecContent::Text(text)));
    drop_uncited(files, draft, path, was, &spec);
    Ok(())
}
