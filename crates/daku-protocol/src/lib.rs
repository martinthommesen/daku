#![recursion_limit = "256"]

//! daku's shared, versioned wire contract.

pub mod identity;
pub mod settings;
pub mod theme;

mod protocol;

pub use protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    EnvironmentHealth, EnvironmentSummary, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, Reachability,
    Request, ResponseOutcome, ResponsePayload, RpcError, SamplePoint, ServerMessage,
    SignalSnapshotDto,
};
pub use settings::DaemonSettings;
