//! A Claude account's folder and its sign-in as Claude Code keeps them on this Mac: the folder Alondra
//! makes for an added account, linked to what the user set up, whose sign-in Claude Code keeps
//! under that folder's own name.
use alondra_core::accounts::{self, AccountId};
use alondra_core::claude_account::{SHARED, config_folder, profile};
use std::path::{Path, PathBuf};

/// Creates the account's folder, private to the user, with a link to each of `SHARED` that
/// `shared_from` has. A name already in the folder is left as it is, so running it again only
/// adds what `shared_from` gained since.
pub fn prepare(home: &Path, id: &AccountId, shared_from: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    let folder = accounts::folder(home, id).ok_or_else(|| std::io::Error::other("the default account has no folder of Alondra's"))?;
    let parent = folder.parent().expect("an account folder has a parent");
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?;
    match std::fs::symlink_metadata(&folder) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
            return Err(std::io::Error::other(format!("{} is not a folder", folder.display())));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new().mode(0o700).create(&folder)?,
        Err(e) => return Err(e),
    }
    std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))?;
    for name in SHARED {
        let target = shared_from.join(name);
        let link = folder.join(name);
        if std::fs::symlink_metadata(&target).is_err() || std::fs::symlink_metadata(&link).is_ok() {
            continue;
        }
        std::os::unix::fs::symlink(&target, &link)?;
    }
    Ok(folder)
}

/// Whether a session one account started resumes with the other: both keep their conversations
/// in the same `projects/` folder, the default profile's, which every account links. One whose
/// folder has a `projects/` of its own, or none, does not.
pub fn shares_sessions(home: &Path, user_home: &Path, default_profile: Option<&Path>, one: &AccountId, other: &AccountId) -> bool {
    let projects = |id: &AccountId| config_folder(profile(home, id, default_profile).as_deref(), user_home).join("projects").canonicalize().ok();
    projects(one).is_some_and(|one| Some(one) == projects(other))
}

/// Deletes the account's folder: the links go, never what they point to.
pub fn remove(home: &Path, id: &AccountId) -> std::io::Result<()> {
    let Some(folder) = accounts::folder(home, id) else {
        return Err(std::io::Error::other("the default account is the user's own and is never deleted"));
    };
    match std::fs::symlink_metadata(&folder) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::remove_file(&folder),
        Ok(_) => std::fs::remove_dir_all(&folder),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
#[path = "claude_account_tests.rs"]
mod tests;
