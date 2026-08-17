//! daku's daemon-side core.

pub mod hollow_backend;
pub mod persistence;
pub mod settings;

mod server;

pub use hollow_backend::HollowBackend;
pub use server::{Backend, EventSink, ServerOptions, serve};
pub use settings::{DaemonSettings, DaemonSettingsStore};
pub use daku_protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, ReplayCursor, Request, ResponseOutcome,
    ResponsePayload, RpcError, SequencedEvent, ServerMessage, WireDriverEvent,
};
