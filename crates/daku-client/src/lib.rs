//! Rust transport and lifecycle support for clients of `daku-daemon`.

mod client;
pub mod persistence;
mod process;

pub use client::DaemonClient;
pub use process::{
    DEFAULT_EXPOSED_DAEMON_PORT, DaemonExposureSettings, DaemonProcess, DaemonSupervisor,
    parse_allowed_origins,
};
pub use daku_protocol::*;
