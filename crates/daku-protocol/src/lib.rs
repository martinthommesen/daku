#![recursion_limit = "256"]

//! daku's shared, versioned wire contract.

pub mod environment;
pub mod identity;
pub mod payload;
pub mod settings;

mod protocol;

pub use environment::{
    AuthMethod, CredentialShapeError, EnvironmentConfig, NON_VOTING_SIGNALS, Platform, Thresholds,
    split_github_repo, validate_credential,
};
pub use payload::{
    TypedPayload, drift_mismatch, drift_role, parse, parse_build, parse_reachability,
    skipped_reason,
};
pub use protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV,
    DASHBOARD_CHUNK_BUDGET_BYTES, DaemonReady, EnvironmentHealth, EnvironmentSummary,
    HealthEventDto, HealthEventKind, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, Reachability,
    Request, ResponseOutcome, ResponsePayload, RollupPoint, RpcError, SamplePoint, ServerMessage,
    SignalEventDto, SignalSnapshotDto, SignalState, environment_id_error, instance_url_error,
    is_supported_environment_id, is_supported_instance_url,
};
pub use settings::DaemonSettings;
