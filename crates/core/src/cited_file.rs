//! Reads a Markdown file a card includes from the worktree where its author works.
use crate::domain::{CITED_FILE_MAX_BYTES, worktree_markdown_path};
use std::path::Path;

/// The file at `path`, relative to the author's worktree `cwd`; `None` when it is not a Markdown
/// file of that worktree or not a readable text file. A file far beyond what a card shows is not
/// read at all.
pub fn read_cited_file(cwd: Option<&Path>, path: &str) -> Option<String> {
    if !worktree_markdown_path(path) {
        return None;
    }
    let file = cwd?.join(path);
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
    fn a_cited_file_is_read_only_from_the_worktree_where_its_author_works() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("docs")).unwrap();
        std::fs::write(dir.path().join("docs/plan.md"), "# Plan").unwrap();
        assert_eq!(read_cited_file(Some(dir.path()), "docs/plan.md").as_deref(), Some("# Plan"));
        let absolute = dir.path().join("docs/plan.md");
        assert_eq!(read_cited_file(Some(dir.path()), absolute.to_str().unwrap()), None, "an absolute path is outside the worktree");
        assert_eq!(read_cited_file(Some(&dir.path().join("docs")), "../docs/plan.md"), None, "a path never climbs out of the worktree");
        assert_eq!(read_cited_file(None, "docs/plan.md"), None, "a relative path needs the author's folder");
        assert_eq!(read_cited_file(Some(dir.path()), "docs/missing.md"), None);
        assert_eq!(read_cited_file(Some(dir.path()), "docs"), None, "a folder is not a file");
        std::fs::write(dir.path().join("big.md"), "x".repeat(5 * CITED_FILE_MAX_BYTES)).unwrap();
        assert!(read_cited_file(Some(dir.path()), "big.md").unwrap().len() > CITED_FILE_MAX_BYTES, "a huge file still reads as too long");
    }
}
