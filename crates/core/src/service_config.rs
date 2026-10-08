//! Shared writes to `~/.alondra/service.toml`, the Mac's pinned device list. The CLI (relay
//! pairing) and the daemon (direct pairing) both append devices; they serialize on one lock
//! and rewrite the file atomically, keeping every field they do not know.
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

pub const FILE_NAME: &str = "service.toml";

pub struct ServiceConfigLock {
    file: File,
}

impl Drop for ServiceConfigLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl ServiceConfigLock {
    pub fn exclusive(home: &Path) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(home.join("service-config.lock"))?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("service config lock must be a regular file"));
        }
        file.lock()?;
        Ok(Self { file })
    }
}

pub struct NewDevice<'a> {
    pub device_id: &'a str,
    pub signing_key: &'a str,
    pub agreement_key: &'a str,
}

/// Appends a device to `service.toml`. The same device with the same keys is a no-op; the same
/// ID with other keys is refused. The caller holds [`ServiceConfigLock`].
pub fn add_device(home: &Path, _lock: &ServiceConfigLock, device: &NewDevice<'_>) -> io::Result<()> {
    let path = home.join(FILE_NAME);
    let mut table: toml::Table = toml::from_str(&fs::read_to_string(&path)?)
        .map_err(|error| io::Error::other(format!("invalid {}: {error}", path.display())))?;
    let devices = table
        .entry("devices")
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| io::Error::other("service devices must be a list"))?;
    let field = |entry: &toml::Value, key: &str| entry.get(key).and_then(toml::Value::as_str).map(str::to_owned);
    if let Some(existing) = devices.iter().find(|entry| field(entry, "device_id").as_deref() == Some(device.device_id)) {
        let same = field(existing, "signing_key").as_deref() == Some(device.signing_key)
            && field(existing, "agreement_key").as_deref() == Some(device.agreement_key);
        return if same { Ok(()) } else { Err(io::Error::other("device ID already has other keys")) };
    }
    let mut entry = toml::Table::new();
    entry.insert("device_id".into(), device.device_id.into());
    entry.insert("signing_key".into(), device.signing_key.into());
    entry.insert("agreement_key".into(), device.agreement_key.into());
    devices.push(toml::Value::Table(entry));
    let text = toml::to_string_pretty(&table).map_err(io::Error::other)?;
    write_atomically(&path, text.as_bytes())
}

/// Removes a device from `service.toml`; a device that is not there is already removed.
pub fn remove_device(home: &Path, _lock: &ServiceConfigLock, device_id: &str) -> io::Result<()> {
    let path = home.join(FILE_NAME);
    let mut table: toml::Table = toml::from_str(&fs::read_to_string(&path)?)
        .map_err(|error| io::Error::other(format!("invalid {}: {error}", path.display())))?;
    let Some(devices) = table.get_mut("devices").and_then(toml::Value::as_array_mut) else {
        return Ok(());
    };
    let before = devices.len();
    devices.retain(|entry| entry.get("device_id").and_then(toml::Value::as_str) != Some(device_id));
    if devices.len() == before {
        return Ok(());
    }
    let text = toml::to_string_pretty(&table).map_err(io::Error::other)?;
    write_atomically(&path, text.as_bytes())
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Ok(meta) = fs::symlink_metadata(path)
        && (!meta.is_file() || meta.file_type().is_symlink())
    {
        return Err(io::Error::other("unsafe service config path"));
    }
    let temp = path.with_extension(format!("toml.{}", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(&temp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(&temp, path)?;
    File::open(path.parent().ok_or_else(|| io::Error::other("service config directory"))?)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> crate::test_support::Directory {
        let home = crate::test_support::Directory::new();
        fs::create_dir_all(&home).unwrap();
        home
    }

    #[test]
    fn appends_a_device_and_keeps_unknown_fields() {
        let home = home();
        fs::write(
            home.join(FILE_NAME),
            "mac_id = \"mac-1\"\nsigning_key_path = \"s\"\nagreement_key_path = \"a\"\ndevices = []\n\n[direct]\nport = 47999\n",
        )
        .unwrap();
        let lock = ServiceConfigLock::exclusive(&home).unwrap();
        let device = NewDevice { device_id: "client-1", signing_key: "sk", agreement_key: "ak" };
        add_device(&home, &lock, &device).unwrap();
        add_device(&home, &lock, &device).unwrap();
        let table: toml::Table = toml::from_str(&fs::read_to_string(home.join(FILE_NAME)).unwrap()).unwrap();
        assert_eq!(table["devices"].as_array().unwrap().len(), 1);
        assert_eq!(table["direct"]["port"].as_integer(), Some(47999));
        assert_eq!(table["mac_id"].as_str(), Some("mac-1"));
        let conflicting = NewDevice { device_id: "client-1", signing_key: "other", agreement_key: "ak" };
        assert!(add_device(&home, &lock, &conflicting).is_err());
        remove_device(&home, &lock, "client-1").unwrap();
        remove_device(&home, &lock, "client-1").unwrap();
        let table: toml::Table = toml::from_str(&fs::read_to_string(home.join(FILE_NAME)).unwrap()).unwrap();
        assert!(table["devices"].as_array().unwrap().is_empty());
        assert_eq!(table["direct"]["port"].as_integer(), Some(47999));
    }
}
