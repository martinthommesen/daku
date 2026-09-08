use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::environment::{AuthMethod, EnvironmentConfig, Thresholds};
use crate::settings::DaemonSettings;

pub const PROTOCOL_VERSION: u32 = 8;
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
    UpdateSettings {
        settings: DaemonSettings,
    },
    /// Validates and persists an Environment plus, optionally, its Credential.
    /// New ids are added; known ids are updated. Ends with a client-side
    /// reload (`070`), which re-reads the file this writes.
    /// `environment` is boxed: the config grows a field per threshold wave
    /// and must not inflate every `ClientMessage` by value.
    SaveEnvironment {
        environment: Box<EnvironmentConfig>,
        /// Exact Keychain blob (`{"client_id","client_secret"}` for OAuth,
        /// `{"username","password"}` for basic). `None` leaves the stored
        /// Credential untouched. Refused when the daemon allows non-loopback
        /// binds. Never logged.
        credential_json: Option<String>,
    },
    /// Removes an Environment from the file and deletes its Keychain item.
    DeleteEnvironment {
        id: String,
    },
    /// Dry-run availability probe for unsaved edits. Writes nothing.
    TestEnvironment {
        environment: Box<EnvironmentConfig>,
        /// Ephemeral Credential for testing a new Environment before saving.
        /// Falls back to the stored item when `None`.
        credential_json: Option<String>,
    },
    /// Renders a Markdown digest of one Environment's local history
    /// (transitions, builds, current states). Read-only; backs the weekly
    /// digest notification and any future digest surface.
    GetDigest {
        environment_id: String,
        /// Window in days, clamped to 1–90 by the handler.
        days: i64,
    },
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

impl EnvironmentHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Down => "down",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "healthy" => Self::Healthy,
            "degraded" => Self::Degraded,
            "down" => Self::Down,
            _ => return None,
        })
    }
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
    /// Full config for the edit sheet (`106`): auth method, clone-source
    /// flag, per-Environment thresholds and expected drift. The daemon
    /// re-publishes after every reload, so edits never go stale.
    pub auth_method: AuthMethod,
    pub clone_source: bool,
    pub thresholds: Thresholds,
    pub expected_drift: Vec<String>,
    pub sort_order: i64,
}

/// ADR-0004: Environment URLs carry Credentials on every request, so they are
/// https only, with no userinfo and no query/fragment. Trailing `/` is
/// tolerated (`join_url` trims it). Returns the reason a URL is unsupported.
///
/// The daemon enforces this when it loads `environments.json`; the desktop
/// re-checks it because `instance_url` arrives over the wire and reaches the
/// OS URL opener.
pub fn instance_url_error(url: &str) -> Option<&'static str> {
    let Some(rest) = url.strip_prefix("https://") else {
        return Some("instance_url must start with https://");
    };
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Some("instance_url has no host");
    }
    if host.contains('@') {
        return Some("instance_url must not contain userinfo");
    }
    if rest.contains('?') || rest.contains('#') {
        return Some("instance_url must not contain a query or fragment");
    }
    None
}

/// `true` when [`instance_url_error`] finds nothing to complain about.
pub fn is_supported_instance_url(url: &str) -> bool {
    instance_url_error(url).is_none()
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

/// One hourly aggregate over raw Signal samples. `avg_real` draws the
/// trend line, `max_real` its ticks, `sample_count` its coverage.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RollupPoint {
    pub hour_start: i64,
    pub avg_real: Option<f64>,
    pub max_real: Option<f64>,
    pub sample_count: i64,
}

/// What changed: a rolled-up health transition or a build-string change.
/// Written by `publish_dashboard`, rendered by the Recent timeline.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthEventKind {
    Health,
    Build,
}

impl HealthEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Build => "build",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "health" => Self::Health,
            "build" => Self::Build,
            _ => return None,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthEventDto {
    pub observed_at: i64,
    pub kind: HealthEventKind,
    /// Previous rollup; `None` for the bootstrap build event.
    pub from_health: Option<EnvironmentHealth>,
    pub to_health: EnvironmentHealth,
    /// Build string after the change; `None` for pure health transitions.
    pub build: Option<String>,
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
    HealthEventsUpdated {
        environment_id: String,
        events: Vec<HealthEventDto>,
    },
    SignalRollupsUpdated {
        environment_id: String,
        signal_id: String,
        points: Vec<RollupPoint>,
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
            Self::HealthEventsUpdated { environment_id, .. } => {
                Some(format!("3:health-events:{environment_id}"))
            }
            Self::SignalRollupsUpdated {
                environment_id,
                signal_id,
                ..
            } => Some(format!("4:rollups:{environment_id}:{signal_id}")),
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
    Settings {
        settings: DaemonSettings,
    },
    EnvironmentTest {
        reachability: Reachability,
        state: SignalState,
        build: Option<String>,
        error: Option<String>,
        rtt_ms: u64,
    },
    Digest {
        markdown: String,
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
                auth_method: crate::environment::AuthMethod::Basic,
                clone_source: false,
                thresholds: crate::environment::Thresholds::default(),
                expected_drift: Vec::new(),
                sort_order: 0,
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "environmentsUpdated");
        assert_eq!(json["environments"][0]["platformId"], "servicenow");
        assert_eq!(json["environments"][0]["authMethod"], "basic");
        assert_eq!(json["environments"][0]["cloneSource"], false);
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
        assert_eq!(PROTOCOL_VERSION, 8);
    }

    #[test]
    fn digest_command_round_trips() {
        let command = Command::GetDigest {
            environment_id: "prod".into(),
            days: 7,
        };
        let json = serde_json::to_value(&command).unwrap();
        assert_eq!(json["type"], "getDigest");
        assert_eq!(json["environmentId"], "prod");
        assert_eq!(json["days"], 7);
        let back: Command = serde_json::from_value(json).unwrap();
        assert!(matches!(back, Command::GetDigest { .. }));
        let payload = ResponsePayload::Digest {
            markdown: "# Digest".into(),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["type"], "digest");
        let back: ResponsePayload = serde_json::from_value(json).unwrap();
        assert!(matches!(back, ResponsePayload::Digest { .. }));
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
        let health_events = ServerMessage::HealthEventsUpdated {
            environment_id: "prod".into(),
            events: Vec::new(),
        }
        .dashboard_cache_key()
        .expect("health events key");

        assert_eq!(environments, "0:environments");
        assert!(environments < snapshots);
        assert!(snapshots < samples);
        assert!(samples < health_events);
        assert_eq!(
            health_events, "3:health-events:prod",
            "health events replay after samples"
        );
        let rollups = ServerMessage::SignalRollupsUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: Vec::new(),
        }
        .dashboard_cache_key()
        .expect("rollups key");
        assert_eq!(rollups, "4:rollups:prod:jobs");
        assert!(health_events < rollups);
        assert_eq!(ServerMessage::ShuttingDown.dashboard_cache_key(), None);
    }

    #[test]
    fn health_events_updated_round_trips() {
        let message = ServerMessage::HealthEventsUpdated {
            environment_id: "prod".into(),
            events: vec![HealthEventDto {
                observed_at: 1_700_000_000,
                kind: HealthEventKind::Health,
                from_health: Some(EnvironmentHealth::Healthy),
                to_health: EnvironmentHealth::Degraded,
                build: None,
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "healthEventsUpdated");
        assert_eq!(json["environmentId"], "prod");
        assert_eq!(json["events"][0]["kind"], "health");
        assert_eq!(json["events"][0]["toHealth"], "degraded");
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        match back {
            ServerMessage::HealthEventsUpdated {
                environment_id,
                events,
            } => {
                assert_eq!(environment_id, "prod");
                assert_eq!(events.len(), 1);
                assert_eq!(events[0].kind, HealthEventKind::Health);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(
            HealthEventKind::parse("build"),
            Some(HealthEventKind::Build)
        );
        assert_eq!(HealthEventKind::parse("bogus"), None);
    }

    #[test]
    fn signal_rollups_updated_round_trips() {
        let message = ServerMessage::SignalRollupsUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: vec![RollupPoint {
                hour_start: 1_700_000_000,
                avg_real: Some(2.5),
                max_real: Some(4.0),
                sample_count: 30,
            }],
        };
        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(json["type"], "signalRollupsUpdated");
        assert_eq!(json["signalId"], "jobs");
        assert_eq!(json["points"][0]["hourStart"], 1_700_000_000);
        assert_eq!(json["points"][0]["avgReal"], 2.5);
        let back: ServerMessage = serde_json::from_value(json).unwrap();
        match back {
            ServerMessage::SignalRollupsUpdated { points, .. } => {
                assert_eq!(points.len(), 1);
                assert_eq!(points[0].sample_count, 30);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn instance_url_rules_match_adr_0004() {
        assert!(is_supported_instance_url("https://acme.service-now.com"));
        assert!(is_supported_instance_url("https://acme.service-now.com/"));
        assert_eq!(
            instance_url_error("http://acme.service-now.com"),
            Some("instance_url must start with https://")
        );
        assert_eq!(
            instance_url_error("https://"),
            Some("instance_url has no host")
        );
        assert_eq!(
            instance_url_error("https://user@acme.service-now.com"),
            Some("instance_url must not contain userinfo")
        );
        assert_eq!(
            instance_url_error("https://acme.service-now.com/?x=1"),
            Some("instance_url must not contain a query or fragment")
        );
        assert_eq!(
            instance_url_error("https://acme.service-now.com/#f"),
            Some("instance_url must not contain a query or fragment")
        );
    }
}
