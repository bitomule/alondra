//! The orchestrator's bounded, read-only project tools. Every component is opened relative to
//! an already-open directory with O_NOFOLLOW, including during enumeration.
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use std::ffi::{CStr, CString, OsString};
use std::fs::{File, OpenOptions};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

const FILE_BYTES: usize = 256 * 1024;
const SEARCH_BYTES: usize = 64 * 1024;
const WALK_ENTRIES: usize = 4096;
const RESULTS: usize = 100;

#[derive(Deserialize)]
#[serde(tag = "operation", content = "arguments", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Query {
    ReadFile(FileQuery),
    GlobFiles(GlobQuery),
    SearchFiles(SearchQuery),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileQuery {
    path: String,
    #[serde(default = "first_line")]
    start_line: usize,
    #[serde(default = "line_count")]
    max_lines: usize,
}
fn first_line() -> usize { 1 }
fn line_count() -> usize { 200 }

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct GlobQuery {
    #[serde(default)]
    path: String,
    pattern: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SearchQuery {
    #[serde(default)]
    path: String,
    query: String,
}

struct Root { path: PathBuf, file: File }

fn component(directory: &File, name: &std::ffi::OsStr) -> Result<File> {
    ensure!(!name.as_bytes().is_empty() && !name.as_bytes().contains(&b'/') && name != "." && name != "..", "invalid path component");
    let name = CString::new(name.as_bytes())?;
    let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK) };
    ensure!(fd >= 0, "path is unavailable or contains a symbolic link");
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    Ok(File::from(fd))
}

impl Root {
    fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize().context("the configured project root is unavailable")?;
        let file = OpenOptions::new().read(true).custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC).open(&path)?;
        Ok(Self { path, file })
    }
    fn relative(&self, path: &str) -> Result<PathBuf> {
        ensure!(path.len() <= 4096 && !path.contains('\0'), "invalid path");
        let path = Path::new(path);
        let relative = if path.is_absolute() { path.strip_prefix(&self.path).context("the path is outside the configured project root")? } else { path };
        ensure!(relative.components().all(|part| matches!(part, Component::Normal(_) | Component::CurDir)), "parent traversal is not allowed");
        Ok(relative.components().filter(|part| !matches!(part, Component::CurDir)).collect())
    }
    fn file(&self, relative: &Path) -> Result<File> {
        let mut file = self.file.try_clone()?;
        for part in relative.components() {
            let Component::Normal(name) = part else { anyhow::bail!("invalid path") };
            file = component(&file, name)?;
        }
        Ok(file)
    }
}

fn text(file: &File, capacity: usize) -> Result<(String, bool)> {
    ensure!(file.metadata()?.is_file(), "only regular files may be read");
    let mut bytes = Vec::new();
    file.take(capacity as u64 + 1).read_to_end(&mut bytes)?;
    let truncated = bytes.len() > capacity;
    bytes.truncate(capacity);
    if truncated {
        while std::str::from_utf8(&bytes).is_err() && bytes.last().is_some_and(|byte| *byte >= 0x80) { bytes.pop(); }
    }
    Ok((String::from_utf8(bytes).context("the file is not UTF-8 text")?, truncated))
}

fn glob_matches(pattern: &str, path: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().collect();
    let path: Vec<char> = path.chars().collect();
    if path.len() > 1024 { return false; }
    fn matches(pattern: &[char], path: &[char], i: usize, j: usize, memo: &mut [Vec<Option<bool>>]) -> bool {
        if let Some(answer) = memo[i][j] { return answer; }
        let answer = if i == pattern.len() { j == path.len() }
            else if pattern[i] == '*' {
                let double = pattern.get(i + 1) == Some(&'*');
                let next = i + if double { 2 } else { 1 };
                matches(pattern, path, next, j, memo)
                    || (double && pattern.get(next) == Some(&'/') && matches(pattern, path, next + 1, j, memo))
                    || (j < path.len() && (double || path[j] != '/') && matches(pattern, path, i, j + 1, memo))
            } else { j < path.len() && ((pattern[i] == '?' && path[j] != '/') || pattern[i] == path[j]) && matches(pattern, path, i + 1, j + 1, memo) };
        memo[i][j] = Some(answer);
        answer
    }
    let mut memo = vec![vec![None; path.len() + 1]; pattern.len() + 1];
    matches(&pattern, &path, 0, 0, &mut memo)
}

struct Walk { visited: usize, omitted: usize, full: bool }

struct DirectoryEntries(*mut libc::DIR);
impl DirectoryEntries {
    fn open(directory: &File) -> Result<Self> {
        let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        ensure!(fd >= 0, "the directory descriptor could not be duplicated");
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() { let error=std::io::Error::last_os_error(); unsafe { libc::close(fd); } return Err(error.into()); }
        Ok(Self(stream))
    }
    fn next(&self) -> Result<Option<OsString>> {
        #[cfg(target_os="macos")]
        let errno = unsafe { libc::__error() };
        #[cfg(not(target_os="macos"))]
        let errno = unsafe { libc::__errno_location() };
        unsafe { *errno = 0; }
        let entry = unsafe { libc::readdir(self.0) };
        if entry.is_null() {
            if unsafe { *errno } != 0 { return Err(std::io::Error::last_os_error().into()); }
            return Ok(None);
        }
        let bytes = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        Ok(Some(OsString::from_vec(bytes.to_vec())))
    }
}
impl Drop for DirectoryEntries { fn drop(&mut self) { unsafe { libc::closedir(self.0); } } }

fn walk(directory: &File, relative: &Path, depth: usize, state: &mut Walk, visit: &mut impl FnMut(&Path, &File) -> Result<bool>) -> Result<()> {
    if depth > 32 { state.omitted += 1; return Ok(()); }
    let entries = DirectoryEntries::open(directory)?;
    while let Some(name) = entries.next()? {
        if name == "." || name == ".." { continue; }
        if state.visited >= WALK_ENTRIES { state.full = true; break; }
        state.visited += 1;
        if matches!(name.to_str(), Some(".git" | "node_modules" | ".build")) || name.to_str().is_some_and(|name| name.starts_with("bazel-")) { continue; }
        let file = match component(directory, &name) { Ok(file) => file, Err(_) => { state.omitted += 1; continue; } };
        let path = relative.join(name);
        let metadata = file.metadata()?;
        if metadata.is_dir() { walk(&file, &path, depth + 1, state, visit)?; }
        else if metadata.is_file() && !visit(&path, &file)? { state.full = true; break; }
        if state.full { break; }
    }
    Ok(())
}

pub(crate) fn execute(root: &Path, query: Query) -> Result<Value> {
    let root = Root::open(root)?;
    match query {
        Query::ReadFile(query) => {
            ensure!((1..=1_000_000).contains(&query.start_line) && (1..=400).contains(&query.max_lines), "invalid line range");
            let relative = root.relative(&query.path)?;
            let file = root.file(&relative)?;
            let (text, truncated) = text(&file, FILE_BYTES)?;
            let lines: Vec<_> = text.lines().collect();
            let start = (query.start_line - 1).min(lines.len());
            let end = start.saturating_add(query.max_lines).min(lines.len());
            Ok(json!({"path":relative,"start_line":query.start_line,"content":lines[start..end].join("\n"),"next_line":(end<lines.len()).then_some(end+1),"truncated":truncated,"preview_bytes":FILE_BYTES,"file_bytes":file.metadata()?.len()}))
        }
        Query::GlobFiles(query) => {
            ensure!(!query.pattern.is_empty() && query.pattern.chars().count() <= 128, "invalid glob pattern");
            let relative = root.relative(&query.path)?;
            let directory = root.file(&relative)?;
            ensure!(directory.metadata()?.is_dir(), "the search path is not a directory");
            let mut matches = vec![];
            let mut state = Walk { visited: 0, omitted: 0, full: false };
            walk(&directory, Path::new(""), 0, &mut state, &mut |path, _| {
                if path.to_str().is_some_and(|path| glob_matches(&query.pattern, path)) { matches.push(relative.join(path)); }
                Ok(matches.len() < RESULTS)
            })?;
            Ok(json!({"paths":matches,"truncated":state.full,"omitted_entries":state.omitted}))
        }
        Query::SearchFiles(query) => {
            ensure!(!query.query.is_empty() && query.query.len() <= 256 && !query.query.contains('\n'), "search requires a bounded literal string");
            let relative = root.relative(&query.path)?;
            let directory = root.file(&relative)?;
            ensure!(directory.metadata()?.is_dir(), "the search path is not a directory");
            let mut matches = vec![];
            let mut previews = 0;
            let mut state = Walk { visited: 0, omitted: 0, full: false };
            walk(&directory, Path::new(""), 0, &mut state, &mut |path, file| {
                let Ok((text, truncated)) = text(file, SEARCH_BYTES) else { return Ok(true); };
                previews += usize::from(truncated);
                for (line, text) in text.lines().enumerate() {
                    if text.contains(&query.query) {
                        let excerpt: String = text.chars().take(512).collect();
                        matches.push(json!({"path":relative.join(path),"line":line+1,"text":excerpt,"line_truncated":text.chars().count()>512}));
                        if matches.len() == RESULTS { return Ok(false); }
                    }
                }
                Ok(true)
            })?;
            Ok(json!({"matches":matches,"truncated":state.full,"omitted_entries":state.omitted,"truncated_file_previews":previews}))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;

    #[test]
    fn bounded_reads_reject_parent_paths_and_symlinks_and_preserve_unicode() {
        let root = Directory::new();
        let outside = Directory::new();
        std::fs::create_dir_all(&*root).unwrap();
        std::fs::create_dir_all(&*outside).unwrap();
        std::fs::write(root.join("notes.txt"), "First\nInformación\nLast\n").unwrap();
        std::fs::write(outside.join("secret"), "Not in scope").unwrap();
        std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
        let query = |path: &str| Query::ReadFile(FileQuery { path: path.into(), start_line: 2, max_lines: 1 });
        assert_eq!(execute(&root, query("notes.txt")).unwrap()["content"], "Información");
        assert!(execute(&root, query("../secret")).is_err());
        assert!(execute(&root, query("link")).is_err());
        assert!(execute(&root, query(outside.join("secret").to_str().unwrap())).is_err());
    }

    #[test]
    fn glob_and_search_never_follow_linked_directories() {
        let root = Directory::new();
        let outside = Directory::new();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(&*outside).unwrap();
        std::fs::write(root.join("src/main.rs"), "public needle\n").unwrap();
        std::fs::write(outside.join("private.rs"), "secret needle\n").unwrap();
        std::os::unix::fs::symlink(&*outside, root.join("escape")).unwrap();
        let paths = execute(&root, Query::GlobFiles(GlobQuery { path: String::new(), pattern: "**/*.rs".into() })).unwrap();
        assert_eq!(paths["paths"], json!(["src/main.rs"]));
        let found = execute(&root, Query::SearchFiles(SearchQuery { path: String::new(), query: "needle".into() })).unwrap();
        assert_eq!(found["matches"].as_array().unwrap().len(), 1);
        assert_eq!(found["matches"][0]["text"], "public needle");
    }
}
