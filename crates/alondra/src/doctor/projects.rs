use std::path::{Path, PathBuf};

use alondra_core::options::OptionsFile;

use super::{Fix, Group, Outcome, Status};

#[derive(Clone, Debug, PartialEq)]
pub struct Folder {
    pub path: PathBuf,
    pub repos: usize,
}

/// What the wizard offers as the projects folder: the cwd first and as the default, plus the
/// nearby folder with more repos when the cwd is easy to accept by mistake. `refused` says why
/// the cwd cannot be offered at all, and then nothing is the default.
#[derive(Clone, Debug, PartialEq)]
pub struct Proposal {
    pub folders: Vec<Folder>,
    pub refused: Option<String>,
}

pub use alondra_core::projects_folder::repos;
use alondra_core::projects_folder::{is_repo, subfolders};

pub fn refusal(path: &Path) -> Option<String> {
    (path == Path::new("/")).then(|| tr!(
        "/ can't be the projects folder: it holds the whole disk. Choose the folder with your git repos.",
        "/ no puede ser la carpeta de proyectos: es todo el disco. Elige la carpeta con tus repos git.").to_string())
}

pub fn propose(cwd: &Path, home: &Path) -> Proposal {
    if let Some(why) = refusal(cwd) {
        return Proposal { folders: vec![], refused: Some(why) };
    }
    let here = Folder { path: cwd.to_path_buf(), repos: repos(cwd) };
    let doubtful = cwd == home || here.repos == 0 || is_repo(cwd);
    let nearby = if !doubtful {
        None
    } else if is_repo(cwd) {
        cwd.parent().filter(|p| refusal(p).is_none()).map(|p| Folder { path: p.to_path_buf(), repos: repos(p) })
    } else {
        subfolders(home).into_iter().filter(|p| p != cwd).map(|p| Folder { repos: repos(&p), path: p })
            .filter(|f| f.repos > 0).max_by_key(|f| f.repos)
    };
    let folders = std::iter::once(here).chain(nearby.filter(|n| n.path != cwd)).collect();
    Proposal { folders, refused: None }
}

/// `~/Projects` rather than `/Users/ana/Projects`.
pub fn shown(path: &Path, home: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rest) if rest.as_os_str().is_empty() => "~".into(),
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => path.display().to_string(),
    }
}

pub fn label(folder: &Folder, home: &Path) -> String {
    let (path, n) = (shown(&folder.path, home), folder.repos);
    match n {
        0 => trf!("{path} · no repos at the top level", "{path} · ningún repo en el primer nivel"),
        1 => trf!("{path} · 1 repo", "{path} · 1 repo"),
        _ => trf!("{path} · {n} repos", "{path} · {n} repos"),
    }
}

pub fn question() -> String {
    tr!("Projects folder — where your git repos live. You can change it later in the app.",
        "Carpeta de proyectos: donde están tus repos git. Puedes cambiarla después en la app.").into()
}

fn chosen_detail(path: &Path, home: &Path) -> String {
    let (path, n) = (shown(path, home), repos(path));
    if n == 1 { trf!("{path} (1 repo)", "{path} (1 repo)") } else { trf!("{path} ({n} repos)", "{path} ({n} repos)") }
}

/// The projects folder `options.toml` names: kept when it still exists, asked for on a first
/// install or when it no longer does. A configured Mac without one runs on `~/Projects`.
pub fn projects_check(options: Option<&str>, file: &Path, home: &Path, cwd: &Path, first_run: bool) -> Outcome {
    let outcome = |status, detail: String, fix| Outcome { id: "projects", name: "projects", group: Group::Service, status, detail, fix };
    let parsed = match OptionsFile::parse(options.unwrap_or("")) {
        Ok(parsed) => parsed,
        Err(e) => {
            let f = file.display();
            return outcome(Status::Fail, trf!("{f} is not valid: {e}", "{f} no es válido: {e}"),
                Some(Fix::Manual(trf!("fix or remove {f}", "corrige o borra {f}"))));
        }
    };
    let saved = parsed.projects_root.as_deref();
    let current = match saved {
        Some(value) => crate::setup_projects::resolve(Path::new(value), home, cwd).ok().filter(|p| p.is_dir()),
        None if first_run => None,
        None => Some(parsed.projects_root(home)).filter(|p| p.is_dir()),
    };
    if let Some(path) = current {
        return outcome(Status::Pass, chosen_detail(&path, home), None);
    }
    let detail = match saved {
        Some(value) => trf!("{value} no longer exists", "{value} ya no existe"),
        None => tr!("not chosen yet", "aún sin elegir").into(),
    };
    let fix = Fix::Choose {
        question: question(),
        proposal: propose(cwd, home),
        instead: tr!("alondra setup --projects-root <folder>", "alondra setup --projects-root <carpeta>").into(),
    };
    outcome(Status::Fail, detail, Some(fix))
}
