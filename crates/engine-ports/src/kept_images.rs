//! The copies of images Alondra keeps by their digest in its files folder, as the spec rules read
//! them: a task's mockups and the captures its validator judged.
use crate::stored_files::open_stored_file;
use alondra_domain_model::specs::{fingerprint_of, KeptImages, StoredImage};
use std::io::Read;
use std::path::{Path, PathBuf};

pub struct HomeImages(PathBuf);

impl HomeImages {
    pub fn new(home: &Path) -> Self {
        Self(home.to_path_buf())
    }
}

impl KeptImages for HomeImages {
    fn keep(&self, bytes: &[u8]) -> Option<StoredImage> {
        if bytes.len() as u64 > alondra_core::markdown::images::IMAGE_MAX_BYTES {
            return None;
        }
        let image = StoredImage::describe(bytes)?;
        let target = self.0.join(&image.file);
        // A copy already there is as new as this use of it, so retention never takes it before
        // the command that records it is committed.
        let stored = alondra_core::markdown::images::store(&target, bytes)
            .and_then(|()| std::fs::File::options().append(true).open(&target)?.set_modified(std::time::SystemTime::now()));
        match stored {
            Ok(()) => Some(image),
            Err(error) => {
                alondra_foundation::log!("{} could not be kept: {error}", image.file);
                None
            }
        }
    }

    fn read(&self, image: &StoredImage) -> Option<Vec<u8>> {
        if !image.well_formed() {
            return None;
        }
        let mut file = open_stored_file(&self.0, &self.0.join(&image.file)).ok()?;
        let mut bytes = vec![];
        std::io::Read::by_ref(&mut file).take(alondra_core::markdown::images::IMAGE_MAX_BYTES + 1).read_to_end(&mut bytes).ok()?;
        (fingerprint_of(Some(&bytes)) == image.fingerprint).then_some(bytes)
    }
}
