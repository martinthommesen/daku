//! daku's daemon-side core.

pub mod availability;
pub mod collector;
pub mod config;
pub mod hollow_backend;
pub mod jobs;
pub mod persistence;
pub mod servicenow;
pub mod settings;
pub mod syslog;

mod server;

pub use collector::{probe_availability_once, start_default_loop};
pub use config::default_environments_path;
pub use daku_protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, ReplayCursor, Request, ResponseOutcome,
    ResponsePayload, RpcError, SequencedEvent, ServerMessage, WireDriverEvent,
};
pub use hollow_backend::HollowBackend;
pub use server::{Backend, EventSink, ServerOptions, serve};
pub use settings::{DaemonSettings, DaemonSettingsStore};
