//! Desktop-owned preferences and lightweight state helpers.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use serde::{Deserialize, Serialize};

use crate::process::DaemonExposureSettings;

/// Desktop-owned preferences (`app.json`). The daemon owns `settings.json`.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub daemon_exposure: DaemonExposureSettings,
}

fn configuration_directory() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".daku")
}

fn default_app_settings_path() -> PathBuf {
    if cfg!(debug_assertions) {
        // Checkout-local so a dev build never shares app.json with an installed Daku.app.
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
            .join("temp")
            .join("app.json")
    } else {
        configuration_directory().join("app.json")
    }
}

fn read_app_settings(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn load_or_create_app_settings() -> io::Result<AppSettings> {
    load_or_create_app_settings_at(&default_app_settings_path())
}

/// Reads `app.json`, minting a `daemon_exposure.token` when absent. The token
/// must be persisted or a configured browser client breaks on every restart.
pub fn load_or_create_app_settings_at(path: &Path) -> io::Result<AppSettings> {
    let source = read_app_settings(path)?;
    let token_was_persisted = source
        .as_ref()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .and_then(|value| {
            value
                .get("daemon_exposure")
                .and_then(|daemon| daemon.get("token"))
                .and_then(serde_json::Value::as_str)
                .map(|token| !token.trim().is_empty())
        })
        .unwrap_or(false);
    let mut settings: AppSettings = source
        .map(|bytes| serde_json::from_slice::<AppSettings>(&bytes).map_err(to_io_error))
        .transpose()?
        .unwrap_or_default();
    let minted = settings.daemon_exposure.ensure_token();
    if !token_was_persisted || minted {
        write_json_atomically(path, &settings)?;
    }
    Ok(settings)
}

fn write_json_atomically(path: &Path, value: &impl Serialize) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let data = serde_json::to_vec_pretty(value).map_err(to_io_error)?;
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

fn to_io_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique app-settings path; removed on drop, so a failing assertion does
    /// not leave the file behind.
    struct TempSettings(PathBuf);

    impl TempSettings {
        fn new() -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("daku-app-settings-{}.json", uuid::Uuid::new_v4())),
            )
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempSettings {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn missing_app_settings_are_written_with_a_token() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert!(!settings.daemon_exposure.token.trim().is_empty());
        assert!(path.exists());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn an_empty_token_is_minted_and_rewritten() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        fs::write(path, r#"{"daemon_exposure":{"token":""}}"#).unwrap();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert!(!settings.daemon_exposure.token.trim().is_empty());
        let written: AppSettings = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        assert_eq!(
            written.daemon_exposure.token,
            settings.daemon_exposure.token
        );
    }

    #[test]
    fn a_persisted_token_survives_legacy_keys_without_a_rewrite() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let original =
            r#"{"daemon_exposure":{"token":"abc"},"theme":"dark","analytics_enabled":false}"#;
        fs::write(path, original).unwrap();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert_eq!(settings.daemon_exposure.token, "abc");
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
