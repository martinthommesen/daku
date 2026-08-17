#![recursion_limit = "256"]

//! daku's shared, versioned wire contract.

rust_i18n::i18n!("../../locales", fallback = "en");

const _LOCALE_SOURCES: [&str; 3] = [
    include_str!("../../../locales/app.yml"),
    include_str!("../../../locales/zh-CN.yml"),
    include_str!("../../../locales/ja.yml"),
];

macro_rules! _tr {
    ($key:expr) => {
        crate::i18n::translate($key)
    };
    ($key:expr, $($args:tt)*) => {
        rust_i18n::t!($key, $($args)*).into_owned()
    };
}

pub mod i18n;
pub mod identity;
pub mod settings;
pub mod theme;

mod protocol;

pub use protocol::{
    APP_EXECUTABLE_ENV, ClientMessage, Command, DAEMON_ADDRESS_ENV, DAEMON_TOKEN_ENV, DaemonReady,
    EnvironmentHealth, EnvironmentSummary, MAX_WIRE_MESSAGE_BYTES, PROTOCOL_VERSION, Reachability,
    ReplayCursor, Request, ResponseOutcome, ResponsePayload, RpcError, SamplePoint, SequencedEvent,
    ServerMessage, SignalSnapshotDto, WireDriverEvent,
};
pub use settings::DaemonSettings;
