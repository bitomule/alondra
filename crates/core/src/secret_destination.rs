//! Where the value of a secret the user types on a card goes: a git-ignored file of the task's
//! repository, as `NAME=value`. The value never enters the domain: the answer only says where it
//! was written.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

const FILE_MAX: usize = 255;
const VARIABLE_MAX: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SecretDestination {
    /// A path relative to the repository's root, which git ignores.
    pub file: String,
    /// The variable the value is written to, as `NAME=value`.
    pub variable: String,
}

impl SecretDestination {
    /// What the domain and the node get instead of the value.
    pub fn written(&self) -> String {
        format!("{} written to {}", self.variable, self.file)
    }

    /// Why the destination cannot hold a secret, judged on its text alone.
    pub fn shape_problem(&self) -> Option<&'static str> {
        let variable = self.variable.as_bytes();
        let valid_variable = !variable.is_empty() && variable.len() <= VARIABLE_MAX
            && (variable[0].is_ascii_alphabetic() || variable[0] == b'_')
            && variable.iter().all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_');
        if !valid_variable {
            return Some("variable is a name of letters, digits and _ that does not start with a digit, like STRIPE_KEY");
        }
        let file = Path::new(&self.file);
        if self.file.is_empty() || self.file.len() > FILE_MAX || self.file.contains(['\0', '\n', '\r', '\\'])
            || !file.components().all(|component| matches!(component, Component::Normal(_)))
        {
            return Some("file is a path inside the repository, relative to its root, like .env or config/.env.local, without .. or a leading /");
        }
        None
    }

    /// Why the file under `root`, a checkout of the repository, cannot hold the secret: it leaves
    /// the checkout through a link, it is not a regular file, or git does not ignore it.
    pub fn place_problem(&self, root: &Path) -> Option<String> {
        if let Some(problem) = self.shape_problem() {
            return Some(problem.into());
        }
        let mut path = root.to_path_buf();
        let parts: Vec<_> = Path::new(&self.file).components().collect();
        for (index, part) in parts.iter().enumerate() {
            path.push(part);
            match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => return Some(format!("{} is a link: Alondra only writes a secret to a file of the repository itself", self.file)),
                Ok(meta) if index + 1 < parts.len() && !meta.is_dir() => return Some(format!("{} is not inside a folder of the repository", self.file)),
                Ok(meta) if index + 1 == parts.len() && !meta.is_file() => return Some(format!("{} is not a regular file", self.file)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(error) => return Some(format!("{} cannot be read: {error}", self.file)),
                Ok(_) => {}
            }
        }
        let ignored = Command::new("git").arg("-C").arg(root).args(["check-ignore", "-q", "--", &self.file])
            .output().is_ok_and(|out| out.status.success());
        (!ignored).then(|| format!("git does not ignore {} in this repository: add it to .gitignore, or choose an ignored file, so the secret is never committed", self.file))
    }

    /// Writes `NAME=value` to the file under `root`, replacing an earlier line of the same name and
    /// keeping the others, readable by the user alone.
    pub fn write(&self, root: &Path, value: &str) -> Result<PathBuf, String> {
        if let Some(problem) = self.place_problem(root) {
            return Err(problem);
        }
        let value = value.trim();
        if value.is_empty() || value.contains('\0') {
            return Err("The secret is empty".into());
        }
        let path = root.join(&self.file);
        let dir = path.parent().ok_or("The file has no folder")?;
        std::fs::create_dir_all(dir).map_err(|error| format!("Alondra could not create the folder of {}: {error}", self.file))?;
        if let Some(problem) = self.place_problem(root) {
            return Err(problem);
        }
        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(format!("Alondra could not read {}: {error}", self.file)),
        };
        let line = format!("{}={}", self.variable, quoted(value));
        let prefix = format!("{}=", self.variable);
        let mut lines: Vec<String> = existing.lines().filter(|old| !old.trim_start().strip_prefix("export ").unwrap_or(old.trim_start()).starts_with(&prefix)).map(str::to_owned).collect();
        lines.push(line);
        let text = lines.join("\n") + "\n";
        let temp = dir.join(format!(".{}.alondra-{}", path.file_name().unwrap_or_default().to_string_lossy(), std::process::id()));
        let written = (|| {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&temp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::set_permissions(&temp, std::fs::Permissions::from_mode(0o600))?;
            std::fs::rename(&temp, &path)
        })();
        if let Err(error) = written {
            let _ = std::fs::remove_file(&temp);
            return Err(format!("Alondra could not write {}: {error}", self.file));
        }
        Ok(path)
    }
}

/// The value as a dotenv file reads it back: bare when it is plain, else quoted.
fn quoted(value: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_-./:+=@,%~".contains(c);
    if value.chars().all(plain) {
        value.into()
    } else if !value.contains(['\'', '\n', '\r']) {
        format!("'{value}'")
    } else {
        let escaped = value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r").replace('$', "\\$").replace('`', "\\`");
        format!("\"{escaped}\"")
    }
}

#[cfg(test)]
#[path = "secret_destination_tests.rs"]
mod tests;
