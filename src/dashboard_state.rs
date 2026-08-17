//! Pure dashboard model: protocol events in, sidebar/detail/compare out.

use std::collections::HashMap;

use daku_protocol::{
    EnvironmentHealth, EnvironmentSummary, Reachability, SamplePoint, ServerMessage,
    SignalSnapshotDto,
};

pub const SIGNAL_IDS: [&str; 7] = [
    "availability",
    "jobs",
    "syslog",
    "mid_ecc",
    "outbound",
    "drift",
    "last_clone",
];

pub const WAITING: &str = "Waiting";

pub fn signal_label(signal_id: &str) -> &'static str {
    match signal_id {
        "availability" => "Availability",
        "jobs" => "Scheduled jobs",
        "syslog" => "Syslog errors",
        "mid_ecc" => "MID / ECC",
        "outbound" => "Outbound",
        "drift" => "Version / plugins",
        "last_clone" => "Last clone",
        _ => "Signal",
    }
}

const TREND_SIGNALS: [&str; 2] = ["jobs", "syslog"];

#[derive(Clone, Debug, Default)]
pub struct DashboardState {
    connected: bool,
    environments: Vec<EnvironmentSummary>,
    selected_id: Option<String>,
    snapshots: HashMap<String, HashMap<String, SignalSnapshotDto>>,
    samples: HashMap<(String, String), Vec<SamplePoint>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    pub id: String,
    pub label: String,
    pub health: EnvironmentHealth,
    pub muted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SignalCard {
    pub signal_id: &'static str,
    pub status: String,
    pub sparkline: Vec<f64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareStrip {
    pub visible: bool,
    pub has_mismatch: bool,
}

impl DashboardState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_connected(&mut self, connected: bool) {
        self.connected = connected;
    }

    pub fn connected(&self) -> bool {
        self.connected
    }

    pub fn apply_all(&mut self, messages: &[ServerMessage]) {
        for message in messages {
            self.apply(message);
        }
    }

    pub fn apply(&mut self, message: &ServerMessage) {
        match message {
            ServerMessage::EnvironmentsUpdated { environments } => {
                self.environments = environments.clone();
                if self.selected_id.as_ref().is_none_or(|id| {
                    !self
                        .environments
                        .iter()
                        .any(|environment| &environment.id == id)
                }) {
                    self.selected_id = self
                        .environments
                        .first()
                        .map(|environment| environment.id.clone());
                }
            }
            ServerMessage::SignalSnapshotsUpdated {
                environment_id,
                snapshots,
            } => {
                self.snapshots.insert(
                    environment_id.clone(),
                    snapshots
                        .iter()
                        .map(|snapshot| (snapshot.signal_id.clone(), snapshot.clone()))
                        .collect(),
                );
            }
            ServerMessage::SignalSamplesUpdated {
                environment_id,
                signal_id,
                points,
            } => {
                self.samples
                    .insert((environment_id.clone(), signal_id.clone()), points.clone());
            }
            _ => {}
        }
    }

    pub fn select(&mut self, id: &str) {
        if self
            .environments
            .iter()
            .any(|environment| environment.id == id)
        {
            self.selected_id = Some(id.to_owned());
        }
    }

    pub fn selected_id(&self) -> Option<&str> {
        self.selected_id.as_deref()
    }

    pub fn selected(&self) -> Option<&EnvironmentSummary> {
        let id = self.selected_id.as_deref()?;
        self.environments
            .iter()
            .find(|environment| environment.id == id)
    }

    pub fn sidebar(&self) -> Vec<SidebarRow> {
        self.environments
            .iter()
            .map(|environment| SidebarRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                health: environment.health,
                muted: !self.connected,
            })
            .collect()
    }

    pub fn cards(&self) -> Vec<SignalCard> {
        let environment_id = self.selected_id.as_deref().unwrap_or("");
        let snapshots = self.snapshots.get(environment_id);
        SIGNAL_IDS
            .iter()
            .map(|&signal_id| {
                let snapshot = snapshots.and_then(|map| map.get(signal_id));
                let sparkline = if TREND_SIGNALS.contains(&signal_id) {
                    self.samples
                        .get(&(environment_id.to_owned(), signal_id.to_owned()))
                        .map(|points| {
                            points
                                .iter()
                                .map(|point| point.value_real.unwrap_or(0.0))
                                .collect()
                        })
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                SignalCard {
                    signal_id,
                    status: snapshot
                        .map(|snapshot| snapshot.state.clone())
                        .unwrap_or_else(|| WAITING.to_owned()),
                    sparkline,
                }
            })
            .collect()
    }

    pub fn compare_strip(&self) -> CompareStrip {
        if self.environments.len() < 2 {
            return CompareStrip {
                visible: false,
                has_mismatch: false,
            };
        }
        let source_id = self.clone_source_id();
        let source_build = source_id.and_then(|id| environment_build(&self.snapshots, id));
        let builds: Vec<_> = self
            .environments
            .iter()
            .filter_map(|environment| environment_build(&self.snapshots, &environment.id))
            .collect();
        let build_mismatch = if let Some(source) = source_build {
            builds.iter().any(|build| build != &source)
        } else {
            builds.windows(2).any(|pair| pair[0] != pair[1])
        };
        let plugin_mismatch = self.environments.iter().any(|environment| {
            source_id != Some(environment.id.as_str())
                && self
                    .snapshots
                    .get(&environment.id)
                    .and_then(|map| map.get("drift"))
                    .is_some_and(|snapshot| drift_mismatch(&snapshot.payload_json))
        });
        let has_mismatch = build_mismatch || plugin_mismatch;
        CompareStrip {
            visible: true,
            has_mismatch,
        }
    }

    fn clone_source_id(&self) -> Option<&str> {
        self.environments
            .iter()
            .find(|environment| {
                let Some(snapshot) = self
                    .snapshots
                    .get(&environment.id)
                    .and_then(|map| map.get("drift"))
                else {
                    return false;
                };
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&snapshot.payload_json)
                else {
                    return false;
                };
                value.get("role").and_then(|item| item.as_str()) == Some("source")
            })
            .map(|environment| environment.id.as_str())
    }

    pub fn card_summary(&self, signal_id: &str) -> String {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return String::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
        else {
            return String::new();
        };
        summarize_payload(signal_id, &snapshot.payload_json)
    }

    pub fn compare_rows(&self) -> Vec<(String, String, Option<String>)> {
        self.environments
            .iter()
            .map(|environment| {
                (
                    environment.id.clone(),
                    environment.label.clone(),
                    environment_build(&self.snapshots, &environment.id),
                )
            })
            .collect()
    }
}

fn environment_build(
    snapshots: &HashMap<String, HashMap<String, SignalSnapshotDto>>,
    environment_id: &str,
) -> Option<String> {
    let payload = snapshots.get(environment_id)?.get("availability")?;
    serde_json::from_str::<serde_json::Value>(&payload.payload_json)
        .ok()?
        .get("build")?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn drift_mismatch(payload_json: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return false;
    };
    value.get("build_matches") == Some(&serde_json::Value::Bool(false))
        || value
            .get("mismatches")
            .and_then(|item| item.as_u64())
            .is_some_and(|count| count > 0)
}

fn summarize_payload(signal_id: &str, payload_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return String::new();
    };
    match signal_id {
        "availability" => match (
            value.get("rtt_ms").and_then(|item| item.as_u64()),
            value.get("build").and_then(|item| item.as_str()),
        ) {
            (Some(ms), Some(build)) => format!("{ms} ms · {build}"),
            (Some(ms), None) => format!("{ms} ms"),
            (None, Some(build)) => build.to_owned(),
            _ => String::new(),
        },
        "jobs" => format!(
            "{} overdue · {} error",
            value
                .get("overdue_ready")
                .and_then(|item| item.as_u64())
                .unwrap_or(0),
            value
                .get("error")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "syslog" => format!(
            "{} errors / h",
            value
                .get("error_count_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "mid_ecc" => {
            let total = value
                .get("agents_total")
                .and_then(|item| item.as_u64())
                .unwrap_or(0);
            let unhealthy = value
                .get("agents_unhealthy")
                .and_then(|item| item.as_u64())
                .unwrap_or(0);
            format!(
                "{}/{} up · queue {}",
                total.saturating_sub(unhealthy),
                total,
                value
                    .get("ecc_output_ready")
                    .and_then(|item| item.as_u64())
                    .unwrap_or(0)
            )
        }
        "outbound" => format!(
            "{} HTTP fail",
            value
                .get("outbound_http_4xx_5xx_1h")
                .and_then(|item| item.as_u64())
                .unwrap_or(0)
        ),
        "drift" => {
            if value.get("role").and_then(|item| item.as_str()) == Some("source") {
                "source of truth".into()
            } else if let Some(count) = value.get("mismatches").and_then(|item| item.as_u64()) {
                format!("{count} plugins differ")
            } else {
                String::new()
            }
        }
        "last_clone" => value
            .get("completed")
            .and_then(|item| item.as_str())
            .unwrap_or("")
            .to_owned(),
        _ => String::new(),
    }
}

pub fn ui_fixture_enabled() -> bool {
    matches!(std::env::var("DAKU_UI_FIXTURE").as_deref(), Ok("1"))
}

pub fn fixture_events() -> Vec<ServerMessage> {
    vec![
        ServerMessage::EnvironmentsUpdated {
            environments: vec![
                env(
                    "prod",
                    "Production",
                    EnvironmentHealth::Degraded,
                    Reachability::Reachable,
                ),
                env(
                    "test",
                    "Test",
                    EnvironmentHealth::Healthy,
                    Reachability::Asleep,
                ),
            ],
        },
        ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![
                snap(
                    "availability",
                    "healthy",
                    r#"{"reachability":"reachable","rtt_ms":142,"build":"glide-zurich-patch3"}"#,
                ),
                snap("jobs", "degraded", r#"{"overdue_ready":2,"error":1}"#),
                snap("syslog", "degraded", r#"{"error_count_1h":38}"#),
                snap(
                    "mid_ecc",
                    "healthy",
                    r#"{"agents_total":3,"agents_unhealthy":0,"ecc_output_ready":12,"ecc_error":0}"#,
                ),
                snap("outbound", "degraded", r#"{"outbound_http_4xx_5xx_1h":4}"#),
                snap("drift", "healthy", r#"{"role":"source"}"#),
            ],
        },
        ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: vec![
                snap(
                    "availability",
                    "healthy",
                    r#"{"reachability":"asleep","rtt_ms":20,"build":"glide-yokohama-patch1"}"#,
                ),
                snap("jobs", "healthy", r#"{"overdue_ready":0,"error":0}"#),
                snap("syslog", "healthy", r#"{"error_count_1h":4}"#),
                snap(
                    "mid_ecc",
                    "healthy",
                    r#"{"agents_total":2,"agents_unhealthy":0,"ecc_output_ready":3,"ecc_error":0}"#,
                ),
                snap("outbound", "healthy", r#"{"outbound_http_4xx_5xx_1h":0}"#),
                snap(
                    "drift",
                    "degraded",
                    r#"{"mismatches":3,"build_matches":false,"truncated":false}"#,
                ),
                snap(
                    "last_clone",
                    "healthy",
                    r#"{"supported":true,"completed":"2026-08-05 09:00:00"}"#,
                ),
            ],
        },
        ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: vec![
                SamplePoint {
                    observed_at: 10,
                    value_real: Some(1.0),
                },
                SamplePoint {
                    observed_at: 20,
                    value_real: Some(2.0),
                },
                SamplePoint {
                    observed_at: 30,
                    value_real: Some(3.0),
                },
            ],
        },
        ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "syslog".into(),
            points: vec![],
        },
    ]
}

fn env(
    id: &str,
    label: &str,
    health: EnvironmentHealth,
    reachability: Reachability,
) -> EnvironmentSummary {
    EnvironmentSummary {
        id: id.into(),
        label: label.into(),
        platform_id: "servicenow".into(),
        health,
        reachability,
        last_observed_at: Some(1_700_000_000),
    }
}

fn snap(signal_id: &str, state: &str, payload_json: &str) -> SignalSnapshotDto {
    SignalSnapshotDto {
        signal_id: signal_id.into(),
        state: state.into(),
        observed_at: 1_700_000_000,
        payload_json: payload_json.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn loaded() -> DashboardState {
        let mut state = DashboardState::new();
        state.set_connected(true);
        state.apply_all(&fixture_events());
        state
    }

    #[test]
    fn dashboard_state_environments_updated_preserves_ids_labels_order() {
        let state = loaded();
        let rows = state.sidebar();
        assert_eq!(
            rows.iter()
                .map(|row| (row.id.as_str(), row.label.as_str()))
                .collect::<Vec<_>>(),
            vec![("prod", "Production"), ("test", "Test")]
        );
    }

    #[test]
    fn dashboard_state_health_degraded_maps_dot() {
        let state = loaded();
        assert_eq!(state.sidebar()[0].health, EnvironmentHealth::Degraded);
        assert!(!state.sidebar()[0].muted);
    }

    #[test]
    fn dashboard_state_asleep_reachability_does_not_change_health() {
        let state = loaded();
        let test = state
            .sidebar()
            .into_iter()
            .find(|row| row.id == "test")
            .unwrap();
        assert_eq!(test.health, EnvironmentHealth::Healthy);
        let selected = {
            let mut state = loaded();
            state.select("test");
            state.selected().cloned().unwrap()
        };
        assert_eq!(selected.reachability, Reachability::Asleep);
        assert_eq!(selected.health, EnvironmentHealth::Healthy);
        assert_ne!(selected.health, EnvironmentHealth::Degraded);
    }

    #[test]
    fn dashboard_state_jobs_samples_fill_sparkline() {
        let state = loaded();
        let jobs = state
            .cards()
            .into_iter()
            .find(|card| card.signal_id == "jobs")
            .unwrap();
        assert_eq!(jobs.sparkline.len(), 3);
        assert_eq!(state.card_summary("jobs"), "2 overdue · 1 error");
        let syslog = state
            .cards()
            .into_iter()
            .find(|card| card.signal_id == "syslog")
            .unwrap();
        assert!(syslog.sparkline.is_empty());
    }

    #[test]
    fn dashboard_state_missing_snapshot_is_waiting() {
        let state = loaded();
        let last_clone = state
            .cards()
            .into_iter()
            .find(|card| card.signal_id == "last_clone")
            .unwrap();
        assert_eq!(last_clone.status, WAITING);
        assert_eq!(last_clone.status, "Waiting");
    }

    #[test]
    fn dashboard_state_compare_strip_build_mismatch() {
        let state = loaded();
        let strip = state.compare_strip();
        assert!(strip.visible);
        assert!(strip.has_mismatch);
    }
}
