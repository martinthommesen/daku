//! Desktop-owned preferences and lightweight state helpers.

use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

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
        mute_active(&self.mutes, environment_id, now)
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

/// One shared mute predicate for the persisted settings and the dashboard
/// snapshot: true while `now` is before the deadline for `id`. Expired
/// entries read as unmuted everywhere; each side prunes on its own write
/// path (`prune_mutes` on save, `apply_mutes` on apply).
pub fn mute_active(mutes: &HashMap<String, i64>, id: &str, now: i64) -> bool {
    mutes.get(id).is_some_and(|until| now < *until)
}

/// Persists desktop preferences atomically (`0600`). Mutes call this on every
/// change.
pub fn save_app_settings(settings: &AppSettings) -> io::Result<()> {
    save_app_settings_at(&default_app_settings_path(), settings)
}

pub fn save_app_settings_at(path: &Path, settings: &AppSettings) -> io::Result<()> {
    write_json_atomically(path, settings)
}

fn configuration_directory() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".daku")
}

/// Operator exports land here (`ExportSnapshot`): one timestamped directory
/// per export. Same permissions story as everything else daku writes.
pub fn export_root() -> PathBuf {
    configuration_directory().join("exports")
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

/// Reads `app.json`, minting it with defaults when absent. A stale
/// `daemon_exposure` block from before the hosted-daemon deletion loads
/// fine: serde ignores unknown fields, and nothing reads it anymore.
pub fn load_or_create_app_settings_at(path: &Path) -> io::Result<AppSettings> {
    match read_app_settings(path)? {
        None => {
            let settings = AppSettings::default();
            write_json_atomically(path, &settings)?;
            Ok(settings)
        }
        Some(bytes) => serde_json::from_slice::<AppSettings>(&bytes).map_err(to_io_error),
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
    fn missing_app_settings_are_written_with_defaults() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert_eq!(settings.mutes.len(), 0);
        assert!(path.exists());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn a_stale_daemon_exposure_block_loads_and_is_left_alone() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let original = r#"{"daemon_exposure":{"token":"abc","enabled":true,"port":34123}}"#;
        fs::write(path, original).unwrap();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert!(settings.mutes.is_empty());
        // Untouched on disk: nothing migrates or rewrites it.
        assert_eq!(fs::read_to_string(path).unwrap(), original);
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
    fn a_persisted_file_with_legacy_keys_loads_without_a_rewrite() {
        let settings_file = TempSettings::new();
        let path = settings_file.path();
        let original =
            r#"{"daemon_exposure":{"token":"abc"},"theme":"dark","analytics_enabled":false}"#;
        fs::write(path, original).unwrap();
        let settings = load_or_create_app_settings_at(path).unwrap();
        assert!(settings.notify_signals.is_empty());
        assert_eq!(fs::read_to_string(path).unwrap(), original);
    }
}
