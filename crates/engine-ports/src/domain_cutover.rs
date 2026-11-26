//! The marker a cutover of a HOME to the domain store left.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub const MARKER: &str = "domain-cutover.json";
/// The marker's name before it was renamed; startup moves it to `MARKER`.
pub const PREVIOUS_MARKER: &str = "domain-v2-cutover.json";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    Committed,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Marker {
    pub format: u32,
    pub phase: Phase,
    pub backup_manifest_sha256: String,
    pub source_seq: u64,
    pub data_epoch: String,
}

/// A malformed marker must stop startup rather than be taken for a HOME that was never cut over.
pub fn read_marker(home: &Path) -> Result<Option<Marker>> {
    let open = |name: &str| OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(home.join(name));
    let file = match open(MARKER).or_else(|error| match error.kind() {
        std::io::ErrorKind::NotFound => open(PREVIOUS_MARKER),
        _ => Err(error),
    }) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let meta = file.metadata()?;
    ensure!(
        meta.is_file()
            && meta.nlink() == 1
            && meta.uid() == unsafe { libc::geteuid() }
            && meta.permissions().mode() & 0o077 == 0,
        "unsafe domain cutover marker"
    );
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 4096, "domain cutover marker too large");
    let marker: Marker = serde_json::from_slice(&bytes).context("invalid domain cutover marker")?;
    ensure!(marker.format == 1, "unsupported domain cutover marker");
    ensure!(
        marker.backup_manifest_sha256.len() == 64
            && marker
                .backup_manifest_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid domain cutover backup digest"
    );
    ensure!(
        marker.data_epoch.len() == 64
            && marker
                .data_epoch
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "invalid domain cutover epoch"
    );
    Ok(Some(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn marker_is_bounded_private_and_fail_closed() {
        let scratch_directory = crate::test_support::Directory::new();
        let home = scratch_directory.parent().unwrap().join(format!(
            "alondra-domain-cutover-marker-{}",
            alondra_foundation::random_hex(8)
        ));
        fs::create_dir(&home).unwrap();
        assert!(read_marker(&home).unwrap().is_none());
        let marker = Marker {
            format: 1,
            phase: Phase::Prepared,
            backup_manifest_sha256: "a".repeat(64),
            source_seq: 73,
            data_epoch: "b".repeat(64),
        };
        fs::write(home.join(MARKER), serde_json::to_vec(&marker).unwrap()).unwrap();
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read_marker(&home).unwrap(), Some(marker));
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_marker(&home).is_err());
        fs::set_permissions(home.join(MARKER), fs::Permissions::from_mode(0o600)).unwrap();
        fs::write(home.join(MARKER), b"malformed").unwrap();
        assert!(read_marker(&home).is_err());
        fs::remove_dir_all(home).unwrap();
    }
}
