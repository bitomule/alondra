//! Reads a Markdown file a card cites from where its author works, so the card can carry it.
use crate::domain::CITED_FILE_MAX_BYTES;
use std::path::{Path, PathBuf};

/// The file at `path`, relative to `cwd`, absolute, or under the home folder (`~/`); `None` when it
/// is not a readable text file. A file far beyond what a card shows is not read at all.
pub fn read_cited_file(cwd: Option<&Path>, path: &str) -> Option<String> {
    let file = match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var_os("HOME")?).join(rest),
        None if Path::new(path).is_absolute() => PathBuf::from(path),
        None => cwd?.join(path),
    };
    let size = std::fs::metadata(&file).ok().filter(|meta| meta.is_file())?.len();
    if size > 4 * CITED_FILE_MAX_BYTES as u64 {
        return Some("x".repeat(CITED_FILE_MAX_BYTES + 1));
    }
    std::fs::read_to_string(file).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cited_file_is_read_relative_to_where_its_author_works_or_from_its_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/plan.md"), "# Plan").unwrap();
        assert_eq!(read_cited_file(Some(dir.path()), "docs/plan.md").as_deref(), Some("# Plan"));
        let absolute = dir.path().join("docs/plan.md");
        assert_eq!(read_cited_file(None, absolute.to_str().unwrap()).as_deref(), Some("# Plan"));
        assert_eq!(read_cited_file(None, "docs/plan.md"), None, "a relative path needs the author's folder");
        assert_eq!(read_cited_file(Some(dir.path()), "docs/missing.md"), None);
        assert_eq!(read_cited_file(Some(dir.path()), "docs"), None, "a folder is not a file");
        std::fs::write(dir.path().join("big.md"), "x".repeat(5 * CITED_FILE_MAX_BYTES)).unwrap();
        assert!(read_cited_file(Some(dir.path()), "big.md").unwrap().len() > CITED_FILE_MAX_BYTES, "a huge file still reads as too long");
    }
}
