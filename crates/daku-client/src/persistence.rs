//! Desktop-owned preferences and lightweight state helpers.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::process::DaemonExposureSettings;

fn default_notifications_on() -> bool {
    true
}

/// Quiet hours in local time: notifications that would fire with a local
/// hour in `[start_hour, end_hour)` stay silent. Wraps midnight (`22..7`
/// quiets 22:00–06:59). Out-of-range hours read as unset — a hand-edited
/// `app.json` can never silence everything by typo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct QuietHours {
    pub start_hour: u8,
    pub end_hour: u8,
}

impl QuietHours {
    pub fn contains(&self, hour: u8) -> bool {
        if self.start_hour > 23 || self.end_hour > 23 {
            return false;
        }
        if self.start_hour <= self.end_hour {
            (self.start_hour..self.end_hour).contains(&hour)
        } else {
            hour >= self.start_hour || hour < self.end_hour
        }
    }
}

/// Desktop-owned preferences (`app.json`). The daemon owns `settings.json`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
    pub daemon_exposure: DaemonExposureSettings,
    /// Operator mutes: Environment id → unix seconds until which attention
    /// surfaces (notifications, menu-bar dot, Dock badge) stay silent.
    /// Expired entries are pruned on read. The daemon keeps collecting.
    pub mutes: HashMap<String, i64>,
    /// Master switch for health-change notifications. Mutes still apply
    /// per-Environment when this is on.
    #[serde(default = "default_notifications_on")]
    pub notifications_enabled: bool,
    /// Per-signal notification switches: signal id → enabled. Absent reads
    /// as on, so a new Signal notifies until the Operator mutes it.
    pub notify_signals: HashMap<String, bool>,
    /// Quiet hours; `None` (the default) notifies around the clock.
    pub quiet_hours: Option<QuietHours>,
    /// Weekly digest: one Monday-morning notification per Environment with
    /// the week's transitions. Off by default.
    pub digest_weekly: bool,
    /// Last weekly-digest send (unix seconds). Guards the Monday slot so a
    /// long-running Monday sends once.
    pub digest_last_sent: Option<i64>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            daemon_exposure: DaemonExposureSettings::default(),
            mutes: HashMap::new(),
            notifications_enabled: true,
            notify_signals: HashMap::new(),
            quiet_hours: None,
            digest_weekly: false,
            digest_last_sent: None,
        }
    }
}

impl AppSettings {
    /// True while `now` is before the mute deadline. Expired entries read as
    /// unmuted (and are pruned on the next save).
    pub fn is_muted(&self, environment_id: &str, now: i64) -> bool {
        self.mutes
            .get(environment_id)
            .is_some_and(|until| now < *until)
    }

    /// Per-signal switch; absent reads as on.
    pub fn signal_notify_enabled(&self, signal_id: &str) -> bool {
        self.notify_signals.get(signal_id).copied().unwrap_or(true)
    }

    pub fn set_signal_notify(&mut self, signal_id: &str, enabled: bool) {
        if enabled {
            self.notify_signals.remove(signal_id);
        } else {
            self.notify_signals.insert(signal_id.to_owned(), false);
        }
    }

    pub fn mute_until(&mut self, environment_id: &str, until: i64) {
        self.mutes.insert(environment_id.to_owned(), until);
    }

    pub fn unmute(&mut self, environment_id: &str) {
        self.mutes.remove(environment_id);
    }

    pub fn prune_mutes(&mut self, now: i64) {
        self.mutes.retain(|_, until| now < *until);
    }
}

/// Persists desktop preferences atomically (`0600`). Mutes call this on every
/// change; the daemon-exposure token path in `load_or_create_app_settings_at`
/// already writes through `write_json_atomically`.
pub fn save_app_settings(settings: &AppSettings) -> io::Result<()> {
    save_app_settings_at(&default_app_settings_path(), settings)
}

pub fn save_app_settings_at(path: &Path, settings: &AppSettings) -> io::Result<()> {
    let mut settings = settings.clone();
    settings.daemon_exposure.ensure_token();
    write_json_atomically(path, &settings)
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
    fn mutes_round_trip_and_expire() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let mut settings = load_or_create_app_settings_at(path).unwrap();
        assert!(!settings.is_muted("prod", 1_700_000_000));
        settings.mute_until("prod", 1_700_000_100);
        assert!(settings.is_muted("prod", 1_700_000_000));
        assert!(!settings.is_muted("prod", 1_700_000_100));
        assert!(!settings.is_muted("test", 1_700_000_000));
        save_app_settings_at(path, &settings).unwrap();
        let reloaded = load_or_create_app_settings_at(path).unwrap();
        assert!(reloaded.is_muted("prod", 1_700_000_000));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn unmute_and_prune_drop_mutes() {
        let mut settings = AppSettings::default();
        settings.mute_until("prod", 100);
        settings.mute_until("test", 300);
        settings.unmute("prod");
        assert!(!settings.is_muted("prod", 50));
        assert!(settings.is_muted("test", 200));
        settings.prune_mutes(400);
        assert!(!settings.is_muted("test", 400));
        assert!(settings.mutes.is_empty());
    }

    #[test]
    fn signal_switches_default_on_and_store_only_off() {
        let mut settings = AppSettings::default();
        assert!(settings.signal_notify_enabled("jobs"));
        settings.set_signal_notify("jobs", false);
        assert!(!settings.signal_notify_enabled("jobs"));
        assert!(settings.signal_notify_enabled("syslog"));
        // Re-enabling removes the entry: absent reads as on.
        settings.set_signal_notify("jobs", true);
        assert!(settings.signal_notify_enabled("jobs"));
        assert!(!settings.notify_signals.contains_key("jobs"));
    }

    #[test]
    fn quiet_hours_and_switches_round_trip() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let mut settings = load_or_create_app_settings_at(path).unwrap();
        settings.set_signal_notify("syslog", false);
        settings.quiet_hours = Some(QuietHours {
            start_hour: 22,
            end_hour: 7,
        });
        save_app_settings_at(path, &settings).unwrap();
        let reloaded = load_or_create_app_settings_at(path).unwrap();
        assert!(!reloaded.signal_notify_enabled("syslog"));
        assert!(reloaded.signal_notify_enabled("jobs"));
        assert_eq!(
            reloaded.quiet_hours,
            Some(QuietHours {
                start_hour: 22,
                end_hour: 7,
            })
        );
    }

    #[test]
    fn legacy_settings_default_to_notify_all_day() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        fs::write(path, r#"{"daemon_exposure":{"token":"abc"}}"#).unwrap();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert!(settings.notify_signals.is_empty());
        assert!(settings.signal_notify_enabled("scan"));
        assert_eq!(settings.quiet_hours, None);
    }

    #[test]
    fn legacy_settings_without_mutes_load_empty() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        fs::write(path, r#"{"daemon_exposure":{"token":"abc"}}"#).unwrap();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert!(settings.mutes.is_empty());
        assert!(!settings.is_muted("prod", 1_700_000_000));
        assert!(settings.notifications_enabled, "legacy files default to on");
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
