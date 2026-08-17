//! daku's daemon-side core.

pub mod availability;
pub mod collector;
pub mod config;
pub mod drift;
pub mod health;
pub mod jobs;
pub mod last_clone;
pub mod mid_ecc;
pub mod outbound;
pub mod persistence;
pub mod servicenow;
pub mod settings;
pub mod settings_backend;
pub mod syslog;

mod server;

#[cfg(test)]
pub(crate) mod test_support;

pub use collector::{
    DoctorReport, DoctorRow, probe_availability_once, run_doctor, start_default_loop,
};
pub use config::default_environments_path;
pub use daku_protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    EnvironmentHealth, EnvironmentSummary, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, Reachability,
    Request, ResponseOutcome, ResponsePayload, RpcError, SamplePoint, ServerMessage,
    SignalSnapshotDto, SignalState,
};
pub use server::{Backend, ServerOptions, serve};
pub use settings::{DaemonSettings, DaemonSettingsStore};
pub use settings_backend::SettingsBackend;
