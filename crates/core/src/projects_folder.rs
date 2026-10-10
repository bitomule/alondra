//! The projects folder: resolving what a user picks, counting the git repos in a folder, the
//! folders worth offering and browsing the folders under HOME. Shared by the CLI and the daemon.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::options::OptionsFile;

/// Folders a browse lists at most; a HOME with more is cut and says so.
pub const MAX_LISTED: usize = 500;

/// The folders in HOME that macOS guards behind a privacy prompt. A background process that
/// enters one waits until someone answers on the Mac, so suggestions and browsing skip them.
pub const PROTECTED: [&str; 7] = ["Desktop", "Documents", "Downloads", "Library", "Movies", "Music", "Pictures"];

/// Whether `path` is one of HOME's protected folders or inside one.
pub fn protected(home: &Path, path: &Path) -> bool {
    path.strip_prefix(home).ok()
        .and_then(|rest| rest.components().next())
        .is_some_and(|first| PROTECTED.iter().any(|name| first.as_os_str() == *name))
}

#[derive(Debug, PartialEq)]
pub enum RootError {
    Empty,
    /// The path, or the existing ancestor of a missing one, is not a directory.
    NotADirectory(PathBuf),
    WholeDisk,
    Unreadable(String),
}

#[derive(Deserialize)]
struct RootValue {
    projects_root: Option<toml::Spanned<String>>,
}

/// The folder `value` names, from `~`, `~/…`, an absolute or a cwd-relative path. An existing
/// folder comes back canonical; a missing one as written, once its nearest existing ancestor is
/// known to be a directory, so it can be created.
pub fn resolve(value: &Path, home: &Path, cwd: &Path) -> Result<PathBuf, RootError> {
    if value.as_os_str().is_empty() {
        return Err(RootError::Empty);
    }
    let path = if value == Path::new("~") { home.to_owned() }
        else if let Ok(rest) = value.strip_prefix("~/") { home.join(rest) }
        else if value.is_absolute() { value.to_owned() } else { cwd.join(value) };
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => path.canonicalize().map_err(|e| RootError::Unreadable(e.to_string())),
        Ok(_) => Err(RootError::NotADirectory(path)),
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) => {
            let mut ancestor = path.as_path();
            while !ancestor.exists() {
                ancestor = ancestor.parent().ok_or_else(|| RootError::Unreadable(format!("no existing parent for {}", path.display())))?;
            }
            if ancestor.is_dir() { Ok(path) } else { Err(RootError::NotADirectory(ancestor.to_owned())) }
        }
        Err(e) => Err(RootError::Unreadable(format!("{}: {e}", path.display()))),
    }
}

/// `resolve`, refusing `/`, which holds the whole disk.
pub fn resolve_choice(value: &Path, home: &Path, cwd: &Path) -> Result<PathBuf, RootError> {
    let path = resolve(value, home, cwd)?;
    if path == Path::new("/") { Err(RootError::WholeDisk) } else { Ok(path) }
}

/// `options.toml` with `projects_root` set to `root`, keeping every other line as written.
pub fn with_root(original: &str, root: &Path) -> Result<String, String> {
    let value = root.to_str().ok_or("Projects directory must be valid UTF-8")?;
    let encoded = toml::Value::String(value.to_owned()).to_string();
    let parsed: RootValue = toml::from_str(original).map_err(|e| e.to_string())?;
    let mut text = original.to_owned();
    if let Some(existing) = parsed.projects_root {
        text.replace_range(existing.span(), &encoded);
    } else {
        text = format!("projects_root = {encoded}\n{text}");
    }
    OptionsFile::parse(&text)?;
    Ok(text)
}

pub fn is_repo(dir: &Path) -> bool {
    dir.join(".git").exists()
}

/// The folders directly inside `dir`, hidden ones left out, by path.
pub fn subfolders(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return vec![] };
    let mut found: Vec<PathBuf> = entries.filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir() && !p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.')))
        .collect();
    found.sort();
    found
}

/// Git repos directly inside `dir`.
pub fn repos(dir: &Path) -> usize {
    subfolders(dir).iter().filter(|p| is_repo(p)).count()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Candidate {
    pub path: PathBuf,
    pub repos: usize,
}

/// What a settings screen offers: the current folder first, then every folder directly in HOME
/// that holds a repo, most repos first.
pub fn candidates(current: &Path, home: &Path) -> Vec<Candidate> {
    let mut others: Vec<Candidate> = subfolders(home).into_iter()
        .filter(|p| p != current && !protected(home, p))
        .map(|p| Candidate { repos: repos(&p), path: p })
        .filter(|c| c.repos > 0)
        .collect();
    others.sort_by(|a, b| b.repos.cmp(&a.repos).then_with(|| a.path.cmp(&b.path)));
    std::iter::once(Candidate { path: current.to_owned(), repos: repos(current) }).chain(others).collect()
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub repo: bool,
    pub repos: usize,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Listing {
    pub path: PathBuf,
    pub parent: Option<PathBuf>,
    pub folders: Vec<Entry>,
    pub truncated: bool,
}

/// The folders inside `path`, which must be HOME or a folder under it once links are followed;
/// anything else is `None`, so a browse never leaves HOME. Protected folders are neither listed
/// nor opened.
pub fn browse(home: &Path, path: Option<&Path>) -> Option<Listing> {
    if path.is_some_and(|p| protected(home, p)) {
        return None;
    }
    let home = home.canonicalize().ok()?;
    let asked = match path {
        None => home.clone(),
        Some(p) if p.is_absolute() && !p.components().any(|c| c == std::path::Component::ParentDir) => p.canonicalize().ok()?,
        Some(_) => return None,
    };
    if !asked.starts_with(&home) || protected(&home, &asked) || !asked.is_dir() {
        return None;
    }
    let all: Vec<PathBuf> = subfolders(&asked).into_iter().filter(|p| !protected(&home, p)).collect();
    let truncated = all.len() > MAX_LISTED;
    let folders = all.into_iter().take(MAX_LISTED)
        .map(|p| Entry {
            name: p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            repo: is_repo(&p),
            repos: repos(&p),
            path: p,
        })
        .collect();
    let parent = (asked != home).then(|| asked.parent().map(Path::to_owned)).flatten();
    Some(Listing { path: asked, parent, folders, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    fn home() -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        std::fs::create_dir(&*home).unwrap();
        for name in ["a", "b", "c"] { repo(&home.join("Projects").join(name)); }
        repo(&home.join("Developer/x"));
        std::fs::create_dir_all(home.join("Documents/notes")).unwrap();
        repo(&home.join(".hidden/y"));
        repo(&home.join("Documents/z"));
        home
    }

    #[test]
    fn candidates_are_the_current_folder_then_home_folders_with_repos() {
        let home = home();
        std::fs::create_dir(home.join("Notes")).unwrap();
        let listed = candidates(&home.join("Notes"), &home);
        let got: Vec<(PathBuf, usize)> = listed.into_iter().map(|c| (c.path, c.repos)).collect();
        assert_eq!(got, vec![(home.join("Notes"), 0), (home.join("Projects"), 3), (home.join("Developer"), 1)]);
        assert_eq!(candidates(&home.join("Projects"), &home).len(), 2, "the current folder is not listed twice");
        assert!(candidates(&home.join("Projects"), &home).iter().all(|c| c.path != home.join("Documents")),
            "a protected folder is never suggested, even with repos");
    }

    #[test]
    fn a_missing_folder_resolves_unless_an_ancestor_is_a_file() {
        let home = home();
        assert_eq!(resolve(Path::new("~/New/deep"), &home, Path::new("/")), Ok(home.join("New/deep")));
        std::fs::write(home.join("file"), "").unwrap();
        assert_eq!(resolve(Path::new("~/file/sub"), &home, Path::new("/")), Err(RootError::NotADirectory(home.join("file"))));
        assert_eq!(resolve(Path::new("~/file"), &home, Path::new("/")), Err(RootError::NotADirectory(home.join("file"))));
        assert_eq!(resolve(Path::new(""), &home, Path::new("/")), Err(RootError::Empty));
        assert_eq!(resolve_choice(Path::new("/"), &home, Path::new("/")), Err(RootError::WholeDisk));
    }

    #[test]
    fn browse_lists_visible_folders_with_their_repos_and_stays_in_home() {
        let home = home();
        let top = browse(&home, None).unwrap();
        assert_eq!(top.parent, None);
        let names: Vec<&str> = top.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Developer", "Projects"], "hidden and protected folders are left out");
        assert_eq!(top.folders[1].repos, 3);
        let projects = browse(&home, Some(&home.join("Projects"))).unwrap();
        assert_eq!(projects.parent.as_deref(), Some(&*home.canonicalize().unwrap()));
        assert!(projects.folders.iter().all(|f| f.repo && f.repos == 0));
        assert!(!projects.truncated);
    }

    #[test]
    fn browse_refuses_paths_outside_home() {
        let home = home();
        let outside = crate::test_support::Directory::new();
        std::fs::create_dir(&*outside).unwrap();
        std::os::unix::fs::symlink(&*outside, home.join("link")).unwrap();
        std::fs::write(home.join("file"), "").unwrap();
        for path in [outside.to_path_buf(), home.join("link"), home.join("Projects/../.."), PathBuf::from("Projects"),
            home.join("missing"), home.join("file"), home.join("Documents"), home.join("Documents/z")] {
            assert_eq!(browse(&home, Some(&path)), None, "{}", path.display());
        }
    }

    #[test]
    fn browse_cuts_a_long_folder() {
        let home = home();
        std::fs::create_dir_all(home.join("Big")).unwrap();
        for n in 0..=MAX_LISTED { std::fs::create_dir(home.join(format!("Big/{n:04}"))).unwrap(); }
        let big = browse(&home, Some(&home.join("Big"))).unwrap();
        assert_eq!((big.folders.len(), big.truncated), (MAX_LISTED, true));
    }

    #[test]
    fn protected_folders_are_the_privacy_guarded_ones_in_home() {
        let home = Path::new("/Users/me");
        for path in ["/Users/me/Documents", "/Users/me/Library/Mobile Documents", "/Users/me/Desktop/x"] {
            assert!(protected(home, Path::new(path)), "{path}");
        }
        for path in ["/Users/me", "/Users/me/Projects", "/Users/me/Projects/Documents", "/Users/me/Documentsx", "/Documents"] {
            assert!(!protected(home, Path::new(path)), "{path}");
        }
    }

    #[test]
    fn with_root_replaces_only_the_value() {
        let text = "# mine\nprojects_root = \"/old\" # here\n\n# tail\n";
        let written = with_root(text, Path::new("/new")).unwrap();
        assert_eq!(written, "# mine\nprojects_root = \"/new\" # here\n\n# tail\n");
        assert_eq!(with_root("", Path::new("/new")).unwrap(), "projects_root = \"/new\"\n");
    }
}
