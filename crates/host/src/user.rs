//! Who runs this process and which programs its PATH finds.
use std::path::PathBuf;

/// The user the process runs as, which Claude's Keychain lookup goes by, never an inherited
/// `USER` or `LOGNAME`.
pub fn effective_username() -> Option<String> {
    let mut record = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0u8; 16384];
    let code = unsafe { libc::getpwuid_r(libc::geteuid(), record.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result) };
    if code != 0 || result.is_null() {
        return None;
    }
    let record = unsafe { record.assume_init() };
    if record.pw_name.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(record.pw_name) }.to_str().ok()?;
    (!name.is_empty()).then(|| name.to_owned())
}

/// The executable file `name` that `path` finds first.
pub fn executable(path: &str, name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    std::env::split_paths(path).map(|folder| folder.join(name)).find(|candidate| candidate.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_executable_on_the_path_wins_and_a_plain_file_does_not_count() {
        use std::os::unix::fs::PermissionsExt as _;
        let scratch = crate::test_support::Directory::new();
        let (plain, runnable, later) = (scratch.join("plain"), scratch.join("runnable"), scratch.join("later"));
        for folder in [&plain, &runnable, &later] {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("codex"), "#!/bin/sh\n").unwrap();
        }
        std::fs::set_permissions(runnable.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(later.join("codex"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let path = format!("{}:{}:{}", plain.display(), runnable.display(), later.display());
        assert_eq!(executable(&path, "codex"), Some(runnable.join("codex")));
        assert_eq!(executable(&path, "claude"), None);
    }

    #[test]
    fn the_user_is_the_one_the_process_runs_as() {
        assert!(effective_username().is_some_and(|name| !name.is_empty()));
    }
}
