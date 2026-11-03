//! Publishes only a successfully authenticated staged credential; never invokes native logout.
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};

pub trait Keychain: Send + Sync {
    fn read(&self, account: &str) -> Result<Option<Vec<u8>>, String>;
    fn save(&self, account: &str, bytes: &[u8]) -> Result<(), String>;
}

pub struct NativeKeychain;

impl Keychain for NativeKeychain {
    fn read(&self, account: &str) -> Result<Option<Vec<u8>>, String> {
        #[cfg(target_os = "macos")]
        { match security_framework::passwords::get_generic_password("Codex Auth", account) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.code() == -25300 => Ok(None),
            Err(_) => Err("The native Codex credential store could not be read safely".into()),
        } }
        #[cfg(not(target_os = "macos"))]
        { let _ = account; Err("Native Codex Keychain storage is unavailable".into()) }
    }
    fn save(&self, account: &str, bytes: &[u8]) -> Result<(), String> {
        #[cfg(target_os = "macos")]
        { security_framework::passwords::set_generic_password("Codex Auth", account, bytes).map_err(|_| "The native Codex credential store could not be updated".into()) }
        #[cfg(not(target_os = "macos"))]
        { let _ = (account, bytes); Err("Native Codex Keychain storage is unavailable".into()) }
    }
}

fn file_digest(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let file = match OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("The existing Codex credential could not be checked safely".into()),
    };
    let metadata = file.metadata().map_err(|_| "Could not check the native Codex credential")?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 { return Err("The native Codex credential is not a regular credential file".into()); }
    let mut digest = Sha256::new();
    let mut limited = file.take(1024 * 1024 + 1);
    let mut buffer = [0; 8192];
    loop {
        let size = limited.read(&mut buffer).map_err(|_| "Could not check the native Codex credential")?;
        if size == 0 { break; }
        digest.update(&buffer[..size]);
    }
    Ok(Some(digest.finalize().to_vec()))
}

pub struct Destination {
    profile: PathBuf,
    storage: String,
    file_digest: Option<Vec<u8>>,
    keychain_account: String,
    keychain_digest: Option<Vec<u8>>,
}

fn keychain_account(profile: &Path) -> String {
    let digest = Sha256::digest(profile.to_string_lossy().as_bytes());
    format!("cli|{}", digest[..8].iter().map(|byte| format!("{byte:02x}")).collect::<String>())
}

impl Destination {
    pub fn inspect(profile: PathBuf, storage: &str, keychain: &dyn Keychain) -> Result<Self, String> {
        use std::os::unix::fs::DirBuilderExt as _;
        if !matches!(storage, "file" | "keyring" | "auto") { return Err("The native Codex credential store is unsupported".into()); }
        if !profile.exists() { fs::DirBuilder::new().recursive(true).mode(0o700).create(&profile).map_err(|_| "Could not create the native Codex profile")?; }
        let profile = profile.canonicalize().map_err(|_| "Could not resolve the native Codex profile")?;
        let keychain_account = keychain_account(&profile);
        let keychain_digest = if storage == "file" { None } else {
            keychain.read(&keychain_account)?.map(|bytes| Sha256::digest(bytes).to_vec())
        };
        let file_digest = file_digest(&profile.join("auth.json"))?;
        Ok(Self { profile, storage: storage.into(), file_digest, keychain_account, keychain_digest })
    }

    fn unchanged(&self, keychain: &dyn Keychain) -> Result<(), String> {
        if file_digest(&self.profile.join("auth.json"))? != self.file_digest { return Err("The native Codex session changed during sign-in; its credentials were left unchanged".into()); }
        if self.storage != "file" && keychain.read(&self.keychain_account)?.map(|bytes| Sha256::digest(bytes).to_vec()) != self.keychain_digest {
            return Err("The native Codex session changed during sign-in; its credentials were left unchanged".into());
        }
        Ok(())
    }

    pub fn publish(&self, bytes: &[u8], attempt: &str, keychain: &dyn Keychain) -> Result<(), String> {
        self.unchanged(keychain)?;
        if self.storage != "file" {
            match keychain.save(&self.keychain_account, bytes) {
                Ok(()) => return Ok(()),
                Err(error) if self.storage != "auto" || self.keychain_digest.is_some() => return Err(error),
                Err(_) => {},
            }
        }
        let pending_path = self.profile.join(format!(".alondra-auth-{attempt}"));
        let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).custom_flags(libc::O_NOFOLLOW).open(&pending_path)
            .map_err(|_| "Could not prepare the new Codex credential; the previous session was preserved")?;
        let pending = PendingFile(pending_path);
        file.write_all(bytes).and_then(|_| file.sync_all()).map_err(|_| "Could not save the new Codex credential; the previous session was preserved")?;
        self.unchanged(keychain)?;
        fs::rename(&pending.0, self.profile.join("auth.json")).map_err(|_| "Could not publish the new Codex credential; the previous session was preserved")?;
        // The rename is the commit. A subsequent directory sync cannot turn it into a failed login.
        if let Ok(directory) = fs::File::open(&self.profile) { let _ = directory.sync_all(); }
        Ok(())
    }
}

struct PendingFile(PathBuf);
impl Drop for PendingFile { fn drop(&mut self) { let _ = fs::remove_file(&self.0); } }

pub fn recover_pending(profile: &Path, attempt: &str) {
    if attempt.len() != 32 || !attempt.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !profile.symlink_metadata().is_ok_and(|meta| meta.is_dir() && !meta.file_type().is_symlink()) { return; }
    let pending = profile.join(format!(".alondra-auth-{attempt}"));
    if pending.symlink_metadata().is_ok_and(|meta| meta.is_file() && !meta.file_type().is_symlink()) { let _ = fs::remove_file(pending); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Directory;
    use std::sync::Mutex;
    use std::os::unix::fs::PermissionsExt as _;

    #[derive(Default)]
    struct FakeKeychain { bytes: Mutex<Option<Vec<u8>>>, refuse: bool }
    impl Keychain for FakeKeychain {
        fn read(&self, _: &str) -> Result<Option<Vec<u8>>, String> { Ok(self.bytes.lock().unwrap().clone()) }
        fn save(&self, _: &str, bytes: &[u8]) -> Result<(), String> {
            if self.refuse { return Err("synthetic save failure".into()); }
            *self.bytes.lock().unwrap() = Some(bytes.to_vec()); Ok(())
        }
    }

    #[test]
    fn codex_login_keychain_account_matches_the_native_profile_namespace() {
        assert_eq!(keychain_account(Path::new("/Users/fixture/.codex")), "cli|e425276ea4797f46");
    }

    #[test]
    fn codex_login_failed_publication_never_truncates_the_prior_file() {
        let directory = Directory::new(); fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("auth.json"), b"prior credential").unwrap();
        let store = FakeKeychain::default();
        let destination = Destination::inspect(directory.to_path_buf(), "file", &store).unwrap();
        fs::create_dir(directory.join(".alondra-auth-attempt")).unwrap();
        assert!(destination.publish(b"new credential", "attempt", &store).is_err());
        assert_eq!(fs::read(directory.join("auth.json")).unwrap(), b"prior credential");
    }

    #[test]
    fn codex_login_success_publishes_private_file_and_refuses_concurrent_replacement() {
        let directory = Directory::new(); fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("auth.json"), b"prior credential").unwrap();
        let store = FakeKeychain::default();
        let destination = Destination::inspect(directory.to_path_buf(), "file", &store).unwrap();
        fs::write(directory.join("auth.json"), b"newer native session").unwrap();
        assert!(destination.publish(b"staged credential", "attempt", &store).is_err());
        assert_eq!(fs::read(directory.join("auth.json")).unwrap(), b"newer native session");
        let destination = Destination::inspect(directory.to_path_buf(), "file", &store).unwrap();
        destination.publish(b"staged credential", "attempt", &store).unwrap();
        assert_eq!(fs::read(directory.join("auth.json")).unwrap(), b"staged credential");
        assert_eq!(fs::metadata(directory.join("auth.json")).unwrap().permissions().mode() & 0o777, 0o600);
    }

    #[test]
    fn codex_login_keychain_save_failure_retains_previous_item_and_file() {
        let directory = Directory::new(); fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("auth.json"), b"prior file").unwrap();
        let store = FakeKeychain { bytes: Mutex::new(Some(b"prior keychain".to_vec())), refuse: true };
        let destination = Destination::inspect(directory.to_path_buf(), "auto", &store).unwrap();
        assert!(destination.publish(b"new credential", "attempt", &store).is_err());
        assert_eq!(store.read("").unwrap().unwrap(), b"prior keychain");
        assert_eq!(fs::read(directory.join("auth.json")).unwrap(), b"prior file");
    }

    #[test]
    fn codex_login_auto_fallback_is_atomic_and_keychain_success_preserves_file() {
        let directory = Directory::new(); fs::create_dir_all(&directory).unwrap();
        fs::write(directory.join("auth.json"), b"prior file").unwrap();
        let store = FakeKeychain { refuse: true, ..Default::default() };
        Destination::inspect(directory.to_path_buf(), "auto", &store).unwrap().publish(b"new file", "attempt", &store).unwrap();
        assert_eq!(fs::read(directory.join("auth.json")).unwrap(), b"new file");
        let store = FakeKeychain::default();
        Destination::inspect(directory.to_path_buf(), "keyring", &store).unwrap().publish(b"new keychain", "attempt", &store).unwrap();
        assert_eq!(store.read("").unwrap().unwrap(), b"new keychain");
        assert_eq!(fs::read(directory.join("auth.json")).unwrap(), b"new file");
    }
}
