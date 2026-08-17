use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::settings::DaemonSettings;

pub const PROTOCOL_VERSION: u32 = 4;
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
    },
    Request(Request),
    Shutdown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub request_id: Uuid,
    pub command: Command,
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

/// Per-Signal snapshot state. `Skipped` means the Signal deliberately did not
/// probe this tick (asleep/unreachable Environment, or not applicable); it
/// never votes in the Environment health rollup.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignalState {
    Healthy,
    Degraded,
    Down,
    Skipped,
}

impl SignalState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Down => "down",
            Self::Skipped => "skipped",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "healthy" => Self::Healthy,
            "degraded" => Self::Degraded,
            "down" => Self::Down,
            "skipped" => Self::Skipped,
            _ => return None,
        })
    }
}

impl Reachability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reachable => "reachable",
            Self::Unreachable => "unreachable",
            Self::Asleep => "asleep",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "reachable" => Self::Reachable,
            "unreachable" => Self::Unreachable,
            "asleep" => Self::Asleep,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub id: String,
    pub label: String,
    /// Instance base URL — non-secret, but "sensitive by default": it travels
    /// only over the loopback wire and is shown to the Operator, never logged.
    pub instance_url: String,
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

impl ServerMessage {
    /// Cache key for "latest dashboard state" replay. `EnvironmentsUpdated`
    /// sorts first so a replaying client sets its selection before snapshots
    /// and samples arrive. `None` for non-dashboard messages.
    pub fn dashboard_cache_key(&self) -> Option<String> {
        match self {
            Self::EnvironmentsUpdated { .. } => Some("0:environments".to_owned()),
            Self::SignalSnapshotsUpdated { environment_id, .. } => {
                Some(format!("1:snapshots:{environment_id}"))
            }
            Self::SignalSamplesUpdated {
                environment_id,
                signal_id,
                ..
            } => Some(format!("2:samples:{environment_id}:{signal_id}")),
            _ => None,
        }
    }
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
    Settings { settings: DaemonSettings },
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
    fn signal_state_round_trips_strings() {
        for state in [
            SignalState::Healthy,
            SignalState::Degraded,
            SignalState::Down,
            SignalState::Skipped,
        ] {
            assert_eq!(SignalState::parse(state.as_str()), Some(state));
            assert_eq!(
                serde_json::to_string(&state).unwrap(),
                format!("\"{}\"", state.as_str())
            );
        }
        assert_eq!(SignalState::parse("bogus"), None);
        for reachability in [
            Reachability::Reachable,
            Reachability::Unreachable,
            Reachability::Asleep,
        ] {
            assert_eq!(
                Reachability::parse(reachability.as_str()),
                Some(reachability)
            );
        }
    }

    #[test]
    fn handshake_field_names_are_stable() {
        let message = ClientMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            token: "secret".into(),
            client_id: Uuid::from_u128(2),
        };
        let json = serde_json::to_value(message).unwrap();

        assert_eq!(json["type"], "hello");
        assert_eq!(json["protocolVersion"], PROTOCOL_VERSION);
        assert!(json.get("clientId").is_some());
        assert!(json.get("resumeFrom").is_none());
    }

    #[test]
    fn request_carries_only_id_and_command() {
        let json = serde_json::to_value(Request {
            request_id: Uuid::from_u128(7),
            command: Command::Ping,
        })
        .unwrap();
        // Sorted: `serde_json` key order depends on its `preserve_order`
        // feature, which workspace feature unification turns on.
        let mut keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["command", "requestId"]);
    }

    #[test]
    fn environments_updated_round_trips() {
        let message = ServerMessage::EnvironmentsUpdated {
            environments: vec![EnvironmentSummary {
                id: "prod".into(),
                label: "Production".into(),
                instance_url: "https://prod.example.service-now.com".into(),
                platform_id: "servicenow".into(),
                health: EnvironmentHealth::Healthy,
                reachability: Reachability::Asleep,
                last_observed_at: Some(1_700_000_000),
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "environmentsUpdated");
        assert_eq!(json["environments"][0]["platformId"], "servicenow");
        assert_eq!(
            json["environments"][0]["instanceUrl"],
            "https://prod.example.service-now.com"
        );
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
        assert_eq!(PROTOCOL_VERSION, 4);
    }

    #[test]
    fn dashboard_cache_key_orders_environments_first() {
        let environments = ServerMessage::EnvironmentsUpdated {
            environments: Vec::new(),
        }
        .dashboard_cache_key()
        .expect("environments key");
        let snapshots = ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: Vec::new(),
        }
        .dashboard_cache_key()
        .expect("snapshots key");
        let samples = ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "drift".into(),
            points: Vec::new(),
        }
        .dashboard_cache_key()
        .expect("samples key");

        assert_eq!(environments, "0:environments");
        assert!(environments < snapshots);
        assert!(snapshots < samples);
        assert_eq!(ServerMessage::ShuttingDown.dashboard_cache_key(), None);
    }
}
