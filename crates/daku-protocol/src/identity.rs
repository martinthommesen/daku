//! Shared application identity used by the daemon and desktop client.

#[cfg(debug_assertions)]
pub const APP_NAME: &str = "daku Debug";
#[cfg(not(debug_assertions))]
pub const APP_NAME: &str = "daku";

#[cfg(debug_assertions)]
pub const APP_ID: &str = "app.daku.dev";
#[cfg(not(debug_assertions))]
pub const APP_ID: &str = "app.daku";

/// Operator data directory name under `$HOME` (`~/.daku/`, ADR-0007).
pub const DATA_DIRECTORY_NAME: &str = "daku";
