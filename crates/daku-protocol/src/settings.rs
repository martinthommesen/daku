use serde::{Deserialize, Serialize};

pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 120;
/// Slow-cadence collectors (drift inventories, clone history, scan
/// findings) run on this cadence; `0` means the default. Must not run
/// faster than the shared cadence — the daemon clamps it there.
pub const DEFAULT_SLOW_POLL_INTERVAL_SECS: u64 = 1800;

/// Daemon-owned settings (`~/.daku/settings.json`). Unknown keys are ignored
/// on load and dropped on write.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// Shared collector cadence in seconds; `0` means the default.
    pub poll_interval_secs: u64,
    /// Slow-cadence collectors in seconds; `0` means the default. Clamped
    /// to the shared cadence — slow must never outrun default.
    pub slow_poll_interval_secs: u64,
    /// Health-event fan-out. The daemon POSTs each new health/build event as
    /// JSON. `None` (the default) disables it. Only loopback `http` and any
    /// `https` URL are honoured — see `daku_core::webhook`.
    pub webhook_url: Option<String>,
}

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            slow_poll_interval_secs: DEFAULT_SLOW_POLL_INTERVAL_SECS,
            webhook_url: None,
        }
    }
}
