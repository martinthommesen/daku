//! Desktop-owned preferences and lightweight state helpers.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::process::DaemonExposureSettings;
use daku_protocol::i18n::AppLanguage;
use daku_protocol::identity::DATA_DIRECTORY_NAME;
use daku_protocol::theme::ThemePreference;

pub const DEFAULT_SIDEBAR_WIDTH: f32 = 252.0;
pub const DEFAULT_RIGHT_PANEL_WIDTH: f32 = 460.0;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct PersistedWindowState {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub analytics_enabled: bool,
    pub theme: ThemePreference,
    pub language: AppLanguage,
    pub daemon_exposure: DaemonExposureSettings,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            analytics_enabled: true,
            theme: ThemePreference::System,
            language: AppLanguage::default(),
            daemon_exposure: DaemonExposureSettings::default(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct AppState {
    app_state_version: u32,
    #[serde(default = "Uuid::new_v4")]
    analytics_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    window_state: Option<PersistedWindowState>,
}

const APP_STATE_VERSION: u32 = 1;

fn configuration_directory() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".daku")
}

fn default_app_settings_path() -> PathBuf {
    if cfg!(debug_assertions) {
        StateStore::default_path().with_file_name("app.json")
    } else {
        configuration_directory().join("app.json")
    }
}

fn default_app_state_path() -> PathBuf {
    StateStore::default_path().with_file_name("state.json")
}

fn default_legacy_settings_paths() -> Vec<PathBuf> {
    if cfg!(debug_assertions) {
        vec![StateStore::default_path().with_file_name("settings.json")]
    } else {
        vec![configuration_directory().join("settings.json")]
    }
}

fn read_app_state_file(path: &Path) -> Option<AppState> {
    let bytes = fs::read(path).ok()?;
    let app_state = serde_json::from_slice::<AppState>(&bytes).ok()?;
    (app_state.app_state_version == APP_STATE_VERSION).then_some(app_state)
}

pub fn load_window_state() -> Option<PersistedWindowState> {
    read_app_state_file(&default_app_state_path())?.window_state
}

fn read_app_settings_source(
    app_settings_path: &Path,
    legacy_settings_paths: &[PathBuf],
) -> io::Result<Option<(Vec<u8>, bool)>> {
    match fs::read(app_settings_path) {
        Ok(bytes) => return Ok(Some((bytes, true))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    for path in legacy_settings_paths {
        match fs::read(path) {
            Ok(bytes) => return Ok(Some((bytes, false))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(None)
}

pub fn load_or_create_app_settings() -> io::Result<AppSettings> {
    let path = default_app_settings_path();
    let source = read_app_settings_source(&path, &default_legacy_settings_paths())?;
    let loaded_from_primary = source.as_ref().is_some_and(|(_, primary)| *primary);
    let token_was_persisted = source
        .as_ref()
        .and_then(|(bytes, _)| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .and_then(|value| {
            value
                .get("daemon_exposure")
                .and_then(|daemon| daemon.get("token"))
                .and_then(serde_json::Value::as_str)
                .map(|token| !token.trim().is_empty())
        })
        .unwrap_or(false);
    let mut settings: AppSettings = source
        .map(|(bytes, _)| serde_json::from_slice::<AppSettings>(&bytes).map_err(to_io_error))
        .transpose()?
        .unwrap_or_default();
    let minted = settings.daemon_exposure.ensure_token();
    if !loaded_from_primary || !token_was_persisted || minted {
        write_json_atomically(&path, &settings)?;
    }
    Ok(settings)
}

pub struct StateStore {
    path: PathBuf,
    app_state_path: PathBuf,
}

impl StateStore {
    pub fn default_path() -> PathBuf {
        if cfg!(debug_assertions) {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(Path::parent)
                .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
                .join("temp")
                .join("app.db")
        } else {
            dirs::data_local_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join(DATA_DIRECTORY_NAME)
                .join("app.db")
        }
    }

    pub fn remote(_daemon: crate::DaemonSupervisor) -> Self {
        Self {
            path: Self::default_path(),
            app_state_path: default_app_state_path(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn save_window_state(&self, window_state: PersistedWindowState) -> io::Result<()> {
        let app_state = AppState {
            app_state_version: APP_STATE_VERSION,
            analytics_id: Uuid::new_v4(),
            window_state: Some(window_state),
        };
        write_json_atomically(&self.app_state_path, &app_state)
    }
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

    #[test]
    fn desktop_settings_paths_are_build_specific() {
        let app_settings_path = default_app_settings_path();
        let legacy_settings_paths = default_legacy_settings_paths();

        #[cfg(debug_assertions)]
        {
            let state_path = StateStore::default_path();
            assert_eq!(app_settings_path, state_path.with_file_name("app.json"));
            assert_eq!(
                legacy_settings_paths,
                [state_path.with_file_name("settings.json")]
            );
        }

        #[cfg(not(debug_assertions))]
        {
            assert_eq!(
                app_settings_path,
                configuration_directory().join("app.json")
            );
            assert_eq!(
                legacy_settings_paths,
                [configuration_directory().join("settings.json")]
            );
        }
    }
}
