use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::settings::DaemonSettings;

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_WIRE_MESSAGE_BYTES: usize = 48 * 1024 * 1024;
pub const DAEMON_TOKEN_ENV: &str = "DAKU_DAEMON_TOKEN";
pub const DAEMON_ADDRESS_ENV: &str = "DAKU_DAEMON_ADDRESS";
pub const APP_EXECUTABLE_ENV: &str = "DAKU_APP_EXECUTABLE";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DaemonReady {
    pub address: String,
    pub protocol_version: u32,
    pub pid: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientMessage {
    Hello {
        protocol_version: u32,
        token: String,
        client_id: Uuid,
        #[serde(default)]
        resume_from: Vec<ReplayCursor>,
    },
    Request(Request),
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub request_id: Uuid,
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub command: Command,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayCursor {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub epoch: Uuid,
    pub sequence: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum Command {
    Ping,
    GetSettings,
    UpdateSettings { settings: DaemonSettings },
    LoadTaskState,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WireDriverEvent {
    pub kind: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

impl WireDriverEvent {
    pub fn new(kind: impl Into<String>, payload: serde_json::Value) -> Self {
        Self {
            kind: kind.into(),
            payload,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SequencedEvent {
    pub session_id: Uuid,
    pub runtime_id: Uuid,
    pub epoch: Uuid,
    pub sequence: u64,
    pub event: WireDriverEvent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentHealth {
    Healthy,
    Degraded,
    Down,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Reachability {
    Reachable,
    Unreachable,
    Asleep,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub id: String,
    pub label: String,
    pub platform_id: String,
    pub health: EnvironmentHealth,
    pub reachability: Reachability,
    pub last_observed_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalSnapshotDto {
    pub signal_id: String,
    pub state: String,
    pub observed_at: i64,
    pub payload_json: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SamplePoint {
    pub observed_at: i64,
    pub value_real: Option<f64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ServerMessage {
    Hello {
        protocol_version: u32,
        daemon_version: String,
    },
    Rejected {
        message: String,
    },
    Response {
        request_id: Uuid,
        outcome: ResponseOutcome,
    },
    Event(SequencedEvent),
    TaskStateChanged {
        revision: u64,
    },
    EnvironmentsUpdated {
        environments: Vec<EnvironmentSummary>,
    },
    SignalSnapshotsUpdated {
        environment_id: String,
        snapshots: Vec<SignalSnapshotDto>,
    },
    SignalSamplesUpdated {
        environment_id: String,
        signal_id: String,
        points: Vec<SamplePoint>,
    },
    ShuttingDown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "status",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ResponseOutcome {
    Ok { payload: ResponsePayload },
    Error { error: RpcError },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ResponsePayload {
    Ack,
    Settings {
        settings: DaemonSettings,
    },
    TaskState {
        projects: Vec<serde_json::Value>,
        sessions: Vec<serde_json::Value>,
        default_cwd: PathBuf,
        projectless_root: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RpcError {
    pub message: String,
}

impl From<anyhow::Error> for RpcError {
    fn from(error: anyhow::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handshake_field_names_are_stable() {
        let message = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "secret".into(),
            client_id: Uuid::from_u128(2),
            resume_from: vec![ReplayCursor {
                session_id: Uuid::nil(),
                runtime_id: Uuid::from_u128(1),
                epoch: Uuid::from_u128(3),
                sequence: 9,
            }],
        };
        let json = serde_json::to_value(message).unwrap();

        assert_eq!(json["type"], "hello");
        assert_eq!(json["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(json["resumeFrom"][0]["sequence"], 9);
    }

    #[test]
    fn environments_updated_round_trips() {
        let message = ServerMessage::EnvironmentsUpdated {
            environments: vec![EnvironmentSummary {
                id: "prod".into(),
                label: "Production".into(),
                platform_id: "servicenow".into(),
                health: EnvironmentHealth::Healthy,
                reachability: Reachability::Asleep,
                last_observed_at: Some(1_700_000_000),
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "environmentsUpdated");
        assert_eq!(json["environments"][0]["platformId"], "servicenow");
        assert_eq!(json["environments"][0]["health"], "healthy");
        assert_eq!(json["environments"][0]["reachability"], "asleep");
        assert_eq!(json["environments"][0]["lastObservedAt"], 1_700_000_000);
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        match back {
            ServerMessage::EnvironmentsUpdated { environments } => {
                assert_eq!(environments[0].health, EnvironmentHealth::Healthy);
                assert_eq!(environments[0].reachability, Reachability::Asleep);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn signal_snapshots_updated_round_trips() {
        let message = ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![SignalSnapshotDto {
                signal_id: "jobs".into(),
                state: "degraded".into(),
                observed_at: 11,
                payload_json: r#"{"overdue":1}"#.into(),
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "signalSnapshotsUpdated");
        assert_eq!(json["environmentId"], "prod");
        assert_eq!(json["snapshots"][0]["signalId"], "jobs");
        assert_eq!(json["snapshots"][0]["payloadJson"], r#"{"overdue":1}"#);
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ServerMessage::SignalSnapshotsUpdated { .. }));
    }

    #[test]
    fn signal_samples_updated_round_trips_including_empty_points() {
        let empty = ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "syslog".into(),
            points: vec![],
        };
        let json = serde_json::to_value(&empty).unwrap();
        assert_eq!(json["type"], "signalSamplesUpdated");
        assert_eq!(json["signalId"], "syslog");
        assert_eq!(json["points"].as_array().unwrap().len(), 0);
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        match back {
            ServerMessage::SignalSamplesUpdated { points, .. } => assert!(points.is_empty()),
            other => panic!("unexpected {other:?}"),
        }

        let with_points = ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: vec![SamplePoint {
                observed_at: 20,
                value_real: Some(3.0),
            }],
        };
        let json = serde_json::to_value(&with_points).unwrap();
        assert_eq!(json["points"][0]["observedAt"], 20);
        assert_eq!(json["points"][0]["valueReal"], 3.0);
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        match back {
            ServerMessage::SignalSamplesUpdated { points, .. } => {
                assert_eq!(points.len(), 1);
                assert_eq!(points[0].value_real, Some(3.0));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn protocol_version_is_daku_domain() {
        assert_eq!(PROTOCOL_VERSION, 1);
    }
}
