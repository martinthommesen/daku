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

/// Older than this and the header tints "polled … ago" as stale.
// ponytail: fixed threshold (2.5x default cadence); put poll_interval_secs on
// EnvironmentsUpdated if Operators start tuning cadence.
pub const STALE_AFTER_SECS: i64 = 300;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Freshness {
    pub label: String,
    pub stale: bool,
}

/// "polled 42 s ago" / "polled 3 min ago" / "polled 2 h ago" for the selected
/// Environment, or `None` before the first observation.
pub fn freshness(last_observed_at: Option<i64>, now: i64) -> Option<Freshness> {
    let age = now.saturating_sub(last_observed_at?).max(0);
    let label = if age < 60 {
        format!("polled {age} s ago")
    } else if age < 3600 {
        format!("polled {} min ago", age / 60)
    } else {
        format!("polled {} h ago", age / 3600)
    };
    Some(Freshness {
        label,
        stale: age > STALE_AFTER_SECS,
    })
}

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareRow {
    pub id: String,
    pub label: String,
    pub build: Option<String>,
    pub drift: String,
    pub last_clone: String,
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

    pub fn has_environments(&self) -> bool {
        !self.environments.is_empty()
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

    /// One-line diagnostic for the selected Environment's Signal: the daemon's
    /// persisted `error` (availability) or `detail` (other Signals) string,
    /// or a human phrase for a skipped probe. Empty when there is nothing to say.
    pub fn card_detail(&self, signal_id: &str) -> String {
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
        detail_from_payload(&snapshot.payload_json)
    }

    /// Up to `limit` human lines ("id: 1.0.0 → 1.1.0", "id: missing here",
    /// "id: only here") for the selected Environment's drift snapshot, plus a
    /// "… and N more" line when the exact count exceeds the lines shown.
    /// Empty for the clone source and when there is no drift.
    pub fn drift_mismatch_lines(&self, limit: usize) -> Vec<String> {
        let Some(environment_id) = self.selected_id.as_deref() else {
            return Vec::new();
        };
        let Some(snapshot) = self
            .snapshots
            .get(environment_id)
            .and_then(|map| map.get("drift"))
        else {
            return Vec::new();
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&snapshot.payload_json) else {
            return Vec::new();
        };
        let Some(list) = value.get("mismatch_list").and_then(|item| item.as_array()) else {
            return Vec::new();
        };
        let mut lines: Vec<String> = list
            .iter()
            .take(limit)
            .map(|entry| {
                let id = entry
                    .get("id")
                    .and_then(|item| item.as_str())
                    .unwrap_or("?");
                let version = |key: &str| {
                    entry
                        .get(key)
                        .and_then(|item| item.as_str())
                        .map(str::to_owned)
                };
                match (version("source_version"), version("other_version")) {
                    (Some(source), Some(other)) => format!("{id}: {source} → {other}"),
                    (Some(_), None) => format!("{id}: missing here"),
                    (None, Some(_)) => format!("{id}: only here"),
                    (None, None) => id.to_owned(),
                }
            })
            .collect();
        // `mismatches` stays exact even when the persisted list is capped.
        let total = value
            .get("mismatches")
            .and_then(|item| item.as_u64())
            .unwrap_or(list.len() as u64);
        let remaining = total.saturating_sub(lines.len() as u64);
        if remaining > 0 {
            lines.push(format!("… and {remaining} more"));
        }
        lines
    }

    pub fn compare_rows(&self) -> Vec<CompareRow> {
        self.environments
            .iter()
            .map(|environment| CompareRow {
                id: environment.id.clone(),
                label: environment.label.clone(),
                build: environment_build(&self.snapshots, &environment.id),
                drift: self.signal_summary(&environment.id, "drift"),
                last_clone: self.signal_summary(&environment.id, "last_clone"),
            })
            .collect()
    }

    fn signal_summary(&self, environment_id: &str, signal_id: &str) -> String {
        self.snapshots
            .get(environment_id)
            .and_then(|map| map.get(signal_id))
            .map(|snapshot| summarize_payload(signal_id, &snapshot.payload_json))
            .unwrap_or_default()
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
    if value.get("skipped").is_some() {
        return String::new();
    }
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
        "last_clone" => {
            if value.get("role").and_then(|item| item.as_str()) == Some("source") {
                "clone source".into()
            } else if let Some(days) = value.get("age_days").and_then(|item| item.as_i64()) {
                match days {
                    0 => "today".into(),
                    1 => "1 day ago".into(),
                    days => format!("{days} days ago"),
                }
            } else if value
                .get("completed")
                .is_some_and(serde_json::Value::is_null)
            {
                "no clone found".into()
            } else {
                String::new()
            }
        }
        _ => String::new(),
    }
}

fn detail_from_payload(payload_json: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload_json) else {
        return String::new();
    };
    if let Some(reason) = value.get("skipped").and_then(|item| item.as_str()) {
        // The card's main line already reads "skipped"; do not repeat the word.
        return match reason {
            "asleep" => "Environment asleep".to_owned(),
            "unreachable" => "Environment unreachable".to_owned(),
            "need_two_environments" => "needs two Environments".to_owned(),
            other => other.to_owned(),
        };
    }
    ["error", "detail"]
        .iter()
        .find_map(|key| value.get(*key).and_then(|item| item.as_str()))
        .map(|text| text.chars().take(160).collect())
        .unwrap_or_default()
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
                snap(
                    "outbound",
                    "down",
                    r#"{"reachability":"unreachable","detail":"HTTP 429"}"#,
                ),
                snap(
                    "drift",
                    "degraded",
                    r#"{"mismatches":3,"build_matches":false,"truncated":false,"mismatch_list":[{"id":"com.example.plugin_a","source_version":"1.0.0","other_version":"1.1.0"},{"id":"com.example.plugin_b","source_version":"2.0.0","other_version":null},{"id":"com.example.plugin_c","source_version":null,"other_version":"0.9.0"}],"mismatch_list_truncated":false}"#,
                ),
                snap(
                    "last_clone",
                    "healthy",
                    r#"{"completed":"2026-08-05 09:00:00","age_days":12,"source_id":"prod"}"#,
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
        instance_url: format!("https://{id}.example.service-now.com"),
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
    fn has_environments_reflects_loaded_state() {
        assert!(!DashboardState::new().has_environments());
        assert!(loaded().has_environments());
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

    #[test]
    fn last_clone_summary_shows_age() {
        assert_eq!(
            summarize_payload("last_clone", r#"{"role":"source","supported":true}"#),
            "clone source"
        );
        assert_eq!(
            summarize_payload("last_clone", r#"{"completed":"x","age_days":0}"#),
            "today"
        );
        assert_eq!(
            summarize_payload("last_clone", r#"{"supported":true,"completed":null}"#),
            "no clone found"
        );
    }

    #[test]
    fn summarize_payload_is_empty_for_skipped() {
        assert_eq!(summarize_payload("jobs", r#"{"skipped":"asleep"}"#), "");
        assert_eq!(
            summarize_payload("drift", r#"{"skipped":"need_two_environments"}"#),
            ""
        );
    }

    #[test]
    fn card_detail_reads_error_and_detail() {
        assert_eq!(
            detail_from_payload(
                r#"{"reachability":"unreachable","error":"no credential for environment prod"}"#
            ),
            "no credential for environment prod"
        );
        assert_eq!(
            detail_from_payload(r#"{"reachability":"unreachable","detail":"HTTP 429"}"#),
            "HTTP 429"
        );
        assert_eq!(detail_from_payload("{}"), "");
        assert_eq!(detail_from_payload("not json"), "");
        // `error` is a count in the jobs payload, not a message.
        assert_eq!(detail_from_payload(r#"{"overdue_ready":2,"error":1}"#), "");
    }

    #[test]
    fn card_detail_phrases_skipped() {
        assert_eq!(
            detail_from_payload(r#"{"skipped":"asleep"}"#),
            "Environment asleep"
        );
        assert_eq!(
            detail_from_payload(r#"{"skipped":"need_two_environments"}"#),
            "needs two Environments"
        );
    }

    #[test]
    fn card_detail_for_selected_environment() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(state.card_detail("outbound"), "HTTP 429");
        assert_eq!(state.card_detail("jobs"), "");
    }

    #[test]
    fn freshness_formats_seconds_minutes_hours() {
        let now_secs = freshness(Some(1000), 1042).unwrap();
        assert_eq!(now_secs.label, "polled 42 s ago");
        assert!(!now_secs.stale);
        assert_eq!(
            freshness(Some(1000), 1000 + 180).unwrap().label,
            "polled 3 min ago"
        );
        let hours = freshness(Some(1000), 1000 + 7200).unwrap();
        assert_eq!(hours.label, "polled 2 h ago");
        assert!(hours.stale);
    }

    #[test]
    fn freshness_none_before_first_observation() {
        assert_eq!(freshness(None, 1000), None);
    }

    #[test]
    fn freshness_stale_after_threshold() {
        assert!(!freshness(Some(0), STALE_AFTER_SECS).unwrap().stale);
        assert!(freshness(Some(0), STALE_AFTER_SECS + 1).unwrap().stale);
    }

    #[test]
    fn drift_mismatch_lines_formats_three_kinds() {
        let mut state = loaded();
        state.select("test");
        assert_eq!(
            state.drift_mismatch_lines(10),
            vec![
                "com.example.plugin_a: 1.0.0 → 1.1.0",
                "com.example.plugin_b: missing here",
                "com.example.plugin_c: only here",
            ]
        );
    }

    #[test]
    fn drift_mismatch_lines_respects_limit() {
        let mut state = loaded();
        state.select("test");
        let lines = state.drift_mismatch_lines(2);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2], "… and 1 more");
    }

    #[test]
    fn drift_mismatch_lines_empty_for_source() {
        let mut state = loaded();
        state.select("prod");
        assert!(state.drift_mismatch_lines(10).is_empty());
    }

    #[test]
    fn compare_rows_include_drift_and_last_clone() {
        let rows = loaded().compare_rows();
        let test = rows.iter().find(|row| row.id == "test").unwrap();
        assert_eq!(test.drift, "3 plugins differ");
        assert_eq!(test.last_clone, "12 days ago");
        let prod = rows.iter().find(|row| row.id == "prod").unwrap();
        assert_eq!(prod.drift, "source of truth");
        assert_eq!(prod.last_clone, "");
    }
    #[test]
    fn dashboard_state_removed_selected_environment_falls_back_to_first() {
        let mut state = loaded();
        state.select("test");
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "prod",
                "Production",
                EnvironmentHealth::Healthy,
                Reachability::Reachable,
            )],
        });
        assert_eq!(state.selected_id(), Some("prod"));
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![],
        });
        assert_eq!(state.selected_id(), None);
    }

    #[test]
    fn dashboard_state_select_unknown_id_is_noop() {
        let mut state = loaded();
        state.select("nope");
        assert_eq!(state.selected_id(), Some("prod"));
    }

    #[test]
    fn dashboard_state_disconnected_mutes_every_row() {
        let mut state = loaded();
        state.set_connected(false);
        assert!(state.sidebar().iter().all(|row| row.muted));
        state.set_connected(true);
        assert!(state.sidebar().iter().all(|row| !row.muted));
    }

    #[test]
    fn dashboard_state_none_sample_becomes_zero() {
        let mut state = loaded();
        let points = vec![
            SamplePoint {
                observed_at: 1,
                value_real: None,
            },
            SamplePoint {
                observed_at: 2,
                value_real: Some(4.0),
            },
        ];
        state.apply(&ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "jobs".into(),
            points: points.clone(),
        });
        // Samples for a non-trend Signal are kept but never charted.
        state.apply(&ServerMessage::SignalSamplesUpdated {
            environment_id: "prod".into(),
            signal_id: "availability".into(),
            points,
        });
        let card = |signal_id: &str| {
            state
                .cards()
                .into_iter()
                .find(|card| card.signal_id == signal_id)
                .unwrap()
        };
        assert_eq!(card("jobs").sparkline, vec![0.0, 4.0]);
        assert!(card("availability").sparkline.is_empty());
    }

    #[test]
    fn dashboard_state_single_environment_hides_compare_strip() {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![env(
                "prod",
                "Production",
                EnvironmentHealth::Healthy,
                Reachability::Reachable,
            )],
        });
        assert_eq!(
            state.compare_strip(),
            CompareStrip {
                visible: false,
                has_mismatch: false,
            }
        );
    }

    /// Two Environments, neither flagged `role: source` — the strip falls back
    /// to comparing builds pairwise.
    #[test]
    fn dashboard_state_pairwise_build_mismatch_without_clone_source() {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![
                env(
                    "prod",
                    "Production",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
                env(
                    "test",
                    "Test",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
            ],
        });
        let availability = |build: &str| {
            vec![snap(
                "availability",
                "healthy",
                &format!(r#"{{"build":"{build}"}}"#),
            )]
        };
        for id in ["prod", "test"] {
            state.apply(&ServerMessage::SignalSnapshotsUpdated {
                environment_id: id.into(),
                snapshots: availability("a"),
            });
        }
        assert!(!state.compare_strip().has_mismatch);
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: availability("b"),
        });
        assert!(state.compare_strip().has_mismatch);
    }

    #[test]
    fn dashboard_state_plugin_only_mismatch() {
        let mut state = DashboardState::new();
        state.apply(&ServerMessage::EnvironmentsUpdated {
            environments: vec![
                env(
                    "prod",
                    "Production",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
                env(
                    "test",
                    "Test",
                    EnvironmentHealth::Healthy,
                    Reachability::Reachable,
                ),
            ],
        });
        // One message per Environment: it replaces that Environment's whole map.
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "prod".into(),
            snapshots: vec![
                snap("availability", "healthy", r#"{"build":"a"}"#),
                snap("drift", "healthy", r#"{"role":"source"}"#),
            ],
        });
        let test_drift = |drift: &str| ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: vec![
                snap("availability", "healthy", r#"{"build":"a"}"#),
                snap("drift", "healthy", drift),
            ],
        };
        state.apply(&test_drift(r#"{"mismatches":1,"build_matches":true}"#));
        assert!(state.compare_strip().has_mismatch);
        state.apply(&test_drift(r#"{"mismatches":0,"build_matches":true}"#));
        assert!(!state.compare_strip().has_mismatch);
    }

    #[test]
    fn dashboard_state_card_summary_per_signal() {
        let mut state = loaded();
        assert_eq!(
            state.card_summary("availability"),
            "142 ms · glide-zurich-patch3"
        );
        assert_eq!(state.card_summary("syslog"), "38 errors / h");
        assert_eq!(state.card_summary("mid_ecc"), "3/3 up · queue 12");
        assert_eq!(state.card_summary("outbound"), "4 HTTP fail");
        assert_eq!(state.card_summary("drift"), "source of truth");
        state.select("test");
        assert_eq!(state.card_summary("drift"), "3 plugins differ");
        assert_eq!(state.card_summary("last_clone"), "12 days ago");
        state.apply(&ServerMessage::SignalSnapshotsUpdated {
            environment_id: "test".into(),
            snapshots: vec![snap("jobs", "healthy", "not json")],
        });
        assert_eq!(state.card_summary("jobs"), "");
    }

    #[test]
    fn dashboard_state_ignores_non_dashboard_messages() {
        let mut state = loaded();
        state.apply(&ServerMessage::ShuttingDown);
        assert_eq!(state.sidebar().len(), 2);
        assert_eq!(state.selected_id(), Some("prod"));
    }
}
