use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;

/// Daemon-owned settings (`~/.daku/settings.json`). Unknown keys are ignored
/// on load and dropped on write.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Shared collector cadence in seconds; `0` means the default.
    pub poll_interval_secs: u64,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
        }
    }
}

impl DaemonSettings {
    pub fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join(".daku")
            .join("settings.json")
    }
}
