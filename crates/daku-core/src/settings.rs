//! Daemon-owned, user-editable configuration.

use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::io::Write as _;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

pub use daku_protocol::settings::DaemonSettings;
use parking_lot::Mutex;
use uuid::Uuid;

pub struct DaemonSettingsStore {
    path: PathBuf,
    settings: Mutex<DaemonSettings>,
}

impl DaemonSettingsStore {
    pub fn open(path: PathBuf) -> io::Result<Self> {
        let (settings, write_current) = match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(settings) => (settings, false),
                Err(error) => {
                    let backup = quarantine_corrupt_settings(&path)?;
                    eprintln!(
                        "daku daemon moved invalid settings to {}: {error}",
                        backup.display()
                    );
                    (DaemonSettings::default(), true)
                }
            },
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                (DaemonSettings::default(), false)
            }
            Err(error) => return Err(error),
        };
        if write_current {
            write_atomic(&path, &settings)?;
        }
        Ok(Self {
            path,
            settings: Mutex::new(settings),
        })
    }

    pub fn get(&self) -> DaemonSettings {
        self.settings.lock().clone()
    }

    pub fn replace(&self, settings: DaemonSettings) -> io::Result<()> {
        let mut current = self.settings.lock();
        write_atomic(&self.path, &settings)?;
        *current = settings;
        Ok(())
    }
}

fn quarantine_corrupt_settings(path: &Path) -> io::Result<PathBuf> {
    let extension = format!("json.corrupt-{}", Uuid::new_v4().simple());
    let backup = path.with_extension(extension);
    fs::rename(path, &backup)?;
    Ok(backup)
}

fn write_atomic(path: &Path, settings: &DaemonSettings) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(settings).map_err(to_io_error)?;
    let temporary = path.with_extension("json.tmp");
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary)?;
    file.write_all(&data)?;
    file.sync_all()?;
    fs::rename(&temporary, path)?;
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

fn to_io_error(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn temp_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!("daku-settings-{}", Uuid::new_v4()));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[cfg(unix)]
    #[test]
    fn daemon_settings_file_is_0600() {
        let directory = temp_directory();
        let path = directory.join("settings.json");
        let store = DaemonSettingsStore::open(path.clone()).unwrap();
        store.replace(DaemonSettings::default()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        fs::remove_dir_all(directory).ok();
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn settings_file_with_unknown_keys_loads_and_rewrites_typed() {
        let directory = temp_directory();
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"theme":"dark","poll_interval_secs":45,"future":42}"#,
        )
        .unwrap();

        let store = DaemonSettingsStore::open(path.clone()).unwrap();
        assert_eq!(store.get().poll_interval_secs, 45);
        store.replace(store.get()).unwrap();

        let value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(value["poll_interval_secs"], 45);
        assert!(value.get("theme").is_none());
        assert!(value.get("future").is_none());
        fs::remove_dir_all(directory).ok();
    }

    #[test]
    fn corrupt_settings_are_quarantined() {
        let directory = temp_directory();
        let path = directory.join("settings.json");
        fs::write(&path, "not json").unwrap();

        let store = DaemonSettingsStore::open(path.clone()).unwrap();
        assert_eq!(store.get(), DaemonSettings::default());
        let quarantined = fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("settings.json.corrupt-")
            })
            .count();
        assert_eq!(quarantined, 1);
        serde_json::from_slice::<DaemonSettings>(&fs::read(&path).unwrap()).unwrap();
        fs::remove_dir_all(directory).ok();
    }
}
