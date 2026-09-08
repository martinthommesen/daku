#![recursion_limit = "256"]

//! daku's shared, versioned wire contract.

pub mod environment;
pub mod identity;
pub mod settings;
pub mod theme;

mod protocol;

pub use environment::{
    AuthMethod, EnvironmentConfig, NON_VOTING_SIGNALS, Platform, Thresholds, validate_credential,
};
pub use protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    EnvironmentHealth, EnvironmentSummary, HealthEventDto, HealthEventKind, MAX_WIRE_MESSAGE_BYTES,
    PROTOCOL_VERSION, Reachability, Request, ResponseOutcome, ResponsePayload, RollupPoint,
    RpcError, SamplePoint, ServerMessage, SignalEventDto, SignalSnapshotDto, SignalState,
    instance_url_error, is_supported_instance_url,
};
pub use settings::DaemonSettings;
