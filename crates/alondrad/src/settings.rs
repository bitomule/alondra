//! This Mac's settings the apps show and change: the projects folder and whether the relay is
//! on. The daemon is the only writer of `options.toml` for them, and a change applies without a
//! restart because every reader loads the file again.
use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use alondra_core::projects_folder::{self, RootError};
use alondra_core::views::Lang;
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::json;

use crate::client_routes::Reply;

pub(crate) const PATH: &str = "/settings";
pub(crate) const FOLDERS: &str = "/settings/folders";
/// The `config.toml` keys `alondra config set` changes; only this Mac's own CLI reaches it.
pub(crate) const CONFIG: &str = "/settings/config";

pub(crate) static WRITER: Mutex<()> = Mutex::new(());

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Change {
    projects_root: String,
    #[serde(default)]
    lang: Option<String>,
}

/// The browse a path and query ask for: `None` when it is not a folders read.
pub(crate) fn folders_query(path_and_query: &str) -> Option<Option<PathBuf>> {
    let (path, query) = path_and_query.split_once('?').map_or((path_and_query, None), |(p, q)| (p, Some(q)));
    if path != FOLDERS {
        return None;
    }
    let Some(query) = query else { return Some(None) };
    let mut pairs = url::form_urlencoded::parse(query.as_bytes());
    let (key, value) = pairs.next()?;
    (key == "path" && !value.is_empty() && value.len() <= 4096 && pairs.next().is_none())
        .then(|| Some(PathBuf::from(value.into_owned())))
}

pub(crate) fn approved_read(path_and_query: &str) -> bool {
    path_and_query == PATH || folders_query(path_and_query).is_some()
}

pub(crate) fn read(home: &Path, user_home: &Path, relay_on: bool) -> Reply {
    let loaded = crate::options::load_in(home, &Mutex::new(None));
    let root = loaded.file.projects_root(user_home);
    Reply {
        status: StatusCode::OK,
        body: json!({
            "projects_root": root,
            "candidates": projects_folder::candidates(&root, user_home),
            "relay": {"on": relay_on},
            "direct_connection": crate::reachability::current(),
            "error": loaded.error,
        }),
    }
}

pub(crate) fn folders(user_home: &Path, path: Option<&Path>) -> Reply {
    match projects_folder::browse(user_home, path) {
        Some(listing) => Reply { status: StatusCode::OK, body: json!(listing) },
        None => Reply {
            status: StatusCode::NOT_FOUND,
            body: json!({"error": "not_found", "message": "that folder is not in your home folder"}),
        },
    }
}

/// Saves the projects folder a client picked, creating it when missing. `lang` is the device's
/// language when the body names none.
pub(crate) fn write(home: &Path, user_home: &Path, relay_on: bool, body: &serde_json::Value, lang: Lang) -> Reply {
    let Ok(change) = serde_json::from_value::<Change>(body.clone()) else {
        return Reply { status: StatusCode::BAD_REQUEST, body: json!({"error": "invalid_input", "message": "invalid settings"}) };
    };
    let lang = match change.lang.as_deref() {
        Some("es") => Lang::Es,
        Some("en") => Lang::En,
        None => lang,
        Some(_) => return Reply { status: StatusCode::BAD_REQUEST, body: json!({"error": "invalid_input", "message": "invalid language"}) },
    };
    let _writing = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(refused) = store(home, user_home, &change.projects_root) {
        return refused.reply(lang);
    }
    read(home, user_home, relay_on)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigChange {
    key: String,
    value: Option<String>,
}

/// Writes one key of `config.toml`, or takes it out with a null value. `restart` says whether the
/// daemon reads it only when it starts, so the CLI restarts it.
pub(crate) fn write_config(home: &Path, body: &serde_json::Value) -> Reply {
    let Ok(change) = serde_json::from_value::<ConfigChange>(body.clone()) else {
        return Reply { status: StatusCode::BAD_REQUEST, body: json!({"error": "invalid_input", "message": "invalid setting"}) };
    };
    let _writing = WRITER.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    use alondra_core::config::{SetError, restarts_daemon, set_key};
    match set_key(home, &change.key, change.value.as_deref()) {
        Ok(()) => Reply { status: StatusCode::OK, body: json!({"key": change.key, "restart": restarts_daemon(&change.key)}) },
        Err(SetError::UnknownKey) => Reply { status: StatusCode::UNPROCESSABLE_ENTITY, body: json!({"error": "unknown_key", "message": change.key}) },
        Err(SetError::Invalid) => Reply { status: StatusCode::UNPROCESSABLE_ENTITY, body: json!({"error": "invalid_value", "message": change.key}) },
        Err(SetError::File(why)) => Reply { status: StatusCode::INTERNAL_SERVER_ERROR, body: json!({"error": "file", "message": why}) },
    }
}

enum Refused {
    Root(RootError),
    CannotCreate(String),
}

impl Refused {
    fn reply(self, lang: Lang) -> Reply {
        let es = lang == Lang::Es;
        let (code, message) = match self {
            Refused::Root(RootError::Empty) => ("empty", if es { "Elige una carpeta.".to_owned() } else { "Choose a folder.".to_owned() }),
            Refused::Root(RootError::NotADirectory(path)) => {
                let shown = path.display();
                ("not_a_directory", if es { format!("{shown} es un archivo, no una carpeta.") } else { format!("{shown} is a file, not a folder.") })
            }
            Refused::Root(RootError::WholeDisk) => ("whole_disk", if es {
                "/ no puede ser la carpeta de proyectos: es todo el disco.".to_owned()
            } else {
                "/ can't be the projects folder: it holds the whole disk.".to_owned()
            }),
            Refused::Root(RootError::Unreadable(why)) | Refused::CannotCreate(why) => ("cannot_create", if es {
                format!("No se pudo usar esa carpeta: {why}")
            } else {
                format!("Couldn't use that folder: {why}")
            }),
        };
        Reply { status: StatusCode::UNPROCESSABLE_ENTITY, body: json!({"error": code, "message": message}) }
    }
}

fn store(home: &Path, user_home: &Path, value: &str) -> Result<(), Refused> {
    let root = projects_folder::resolve_choice(Path::new(value.trim()), user_home, user_home).map_err(Refused::Root)?;
    let file = alondra_core::options::path_in(home);
    let original = match std::fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Refused::CannotCreate(format!("{}: {e}", file.display()))),
    };
    projects_folder::with_root(&original, &root).map_err(Refused::CannotCreate)?;
    std::fs::create_dir_all(&root).map_err(|e| Refused::CannotCreate(format!("{}: {e}", root.display())))?;
    let canonical = root.canonicalize().map_err(|e| Refused::CannotCreate(e.to_string()))?;
    let text = projects_folder::with_root(&original, &canonical).map_err(Refused::CannotCreate)?;
    replace(&file, text.as_bytes()).map_err(|e| Refused::CannotCreate(format!("{}: {e}", file.display())))
}

pub(crate) fn replace(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let next = path.with_extension(format!("toml.{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&next)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&next, path)
}

#[cfg(test)]
mod tests;
