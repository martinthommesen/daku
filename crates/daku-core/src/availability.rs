//! Availability Signal: reachability + build/latency from `glide.war`.

use std::io;
use std::time::Instant;

use daku_protocol::{Reachability, SignalState};
use rusqlite::Connection;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::persistence;
use crate::servicenow::ServiceNowClient;

pub const AVAILABILITY_SIGNAL_ID: &str = "availability";
pub const GLIDE_WAR_PATH: &str = "/api/now/table/sys_properties?sysparm_query=name=glide.war&sysparm_fields=value&sysparm_limit=1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailabilityObservation {
    pub reachability: Reachability,
    pub state: SignalState,
    pub build: Option<String>,
    pub rtt_ms: u64,
    pub error: Option<String>,
}

pub fn classify_availability_response(
    status: u16,
    content_type: &str,
    body: &str,
    rtt_ms: u64,
) -> AvailabilityObservation {
    if is_hibernating(content_type, body) {
        return observation(
            Reachability::Asleep,
            SignalState::Healthy,
            None,
            rtt_ms,
            None,
        );
    }
    if status == 200 && looks_like_table_api(body) {
        return observation(
            Reachability::Reachable,
            SignalState::Healthy,
            parse_glide_war(body),
            rtt_ms,
            None,
        );
    }
    let error = match status {
        429 => Some("HTTP 429".to_owned()),
        _ => None,
    };
    observation(
        Reachability::Unreachable,
        SignalState::Down,
        None,
        rtt_ms,
        error,
    )
}

fn observation(
    reachability: Reachability,
    state: SignalState,
    build: Option<String>,
    rtt_ms: u64,
    error: Option<String>,
) -> AvailabilityObservation {
    AvailabilityObservation {
        reachability,
        state,
        build,
        rtt_ms,
        error,
    }
}

// ponytail: naive "html + hibernat" substring; upgrade if Operator smoke shows a
// different splash (status-only, or a stable title/path) that this misses.
fn is_hibernating(content_type: &str, body: &str) -> bool {
    let html = content_type.to_ascii_lowercase().contains("html")
        || body.to_ascii_lowercase().contains("<html");
    html && body.to_ascii_lowercase().contains("hibernat")
}

fn looks_like_table_api(body: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|value| value.get("result").map(|result| result.is_array()))
        .unwrap_or(false)
}

/// Freshness window for reusing this tick's Availability result in later
/// Signals. Availability runs first in the same tick, so seconds usually
/// separate the two; the window only tolerates a slow tick.
pub const REACHABILITY_REUSE_SECS: i64 = 300;

/// Reachability the Availability Signal recorded for `environment_id` within
/// `max_age_secs` of `observed_at`, if any.
pub fn recent_reachability(
    connection: &Connection,
    environment_id: &str,
    observed_at: i64,
    max_age_secs: i64,
) -> Option<Reachability> {
    let snapshot =
        persistence::load_signal_snapshot(connection, environment_id, AVAILABILITY_SIGNAL_ID)
            .ok()
            .flatten()?;
    if observed_at.saturating_sub(snapshot.observed_at) > max_age_secs {
        return None;
    }
    let payload: serde_json::Value = serde_json::from_str(&snapshot.payload_json).ok()?;
    Reachability::parse(
        payload
            .get("reachability")
            .and_then(|value| value.as_str())?,
    )
}

fn availability_payload(observation: &AvailabilityObservation) -> serde_json::Value {
    serde_json::json!({
        "reachability": observation.reachability.as_str(),
        "rtt_ms": observation.rtt_ms,
        "build": observation.build,
        "error": observation.error,
    })
}

pub fn persist_availability_snapshot(
    connection: &Connection,
    environment_id: &str,
    observation: &AvailabilityObservation,
    observed_at: i64,
) -> io::Result<()> {
    persistence::persist_signal_snapshot(
        connection,
        environment_id,
        AVAILABILITY_SIGNAL_ID,
        observed_at,
        observation.state,
        &availability_payload(observation).to_string(),
    )
}

/// Availability is the Signal every other Signal defers to, so it never skips.
#[derive(Default)]
pub struct AvailabilitySignal;

pub type AvailabilityCollector = PerEnvironmentCollector<AvailabilitySignal>;

impl AvailabilitySignal {
    pub fn observe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> AvailabilityObservation {
        let started = Instant::now();
        let observed = match client.request(environment, credentials, "GET", GLIDE_WAR_PATH, None) {
            Ok(response) => classify_availability_response(
                response.status,
                response.header("content-type").unwrap_or(""),
                &response.body,
                started.elapsed().as_millis() as u64,
            ),
            Err(error) => observation(
                Reachability::Unreachable,
                SignalState::Down,
                None,
                started.elapsed().as_millis() as u64,
                Some(error.to_string()),
            ),
        };
        apply_rtt_threshold(observed, &environment.thresholds)
    }
}

/// A reachable Environment slower than its configured ceiling degrades, with
/// the ceiling in the error string so the card says why. `None` disables the
/// check — the historical behaviour, since availability never had a response-
/// time threshold before.
pub fn apply_rtt_threshold(
    observation: AvailabilityObservation,
    thresholds: &Thresholds,
) -> AvailabilityObservation {
    let Some(max_ms) = thresholds.availability_rtt_degraded_ms else {
        return observation;
    };
    if observation.reachability != Reachability::Reachable || observation.rtt_ms <= max_ms {
        return observation;
    }
    AvailabilityObservation {
        state: SignalState::Degraded,
        error: Some(format!("rtt {} ms > {} ms", observation.rtt_ms, max_ms)),
        ..observation
    }
}

impl Signal for AvailabilitySignal {
    fn id(&self) -> &'static str {
        AVAILABILITY_SIGNAL_ID
    }

    fn gated_by_availability(&self) -> bool {
        false
    }

    fn keeps_samples(&self) -> bool {
        true
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let observed = self.observe(client, credentials, environment);
        // Round-trip time is a trend worth a sparkline; a failed probe's
        // elapsed time is a timeout, not a latency, so it is not sampled.
        let sample =
            (observed.reachability == Reachability::Reachable).then_some(observed.rtt_ms as f64);
        Ok(Observation {
            state: observed.state,
            payload: availability_payload(&observed),
            sample,
        })
    }
}

impl AvailabilityCollector {
    /// Live probe without persisting — `doctor` reads config, never SQLite.
    pub fn probe(&self, environment: &EnvironmentConfig) -> AvailabilityObservation {
        self.signal()
            .observe(self.client(), self.credentials(), environment)
    }
}

fn parse_glide_war(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let build = value
        .get("result")?
        .as_array()?
        .first()?
        .get("value")?
        .as_str()?;
    if build.is_empty() {
        return None;
    }
    Some(build.to_owned())
}

/// Runs a platform probe for the sheet's Test button and doctor, mapping its
/// outcome onto the availability shape: reachable when the probe answered,
/// with the probe state, else unreachable/down with the error.
pub(crate) fn observe_probe_signal(
    signal: &impl crate::collector::Signal,
    client: &ServiceNowClient,
    store: &dyn CredentialStore,
    environment: &EnvironmentConfig,
) -> AvailabilityObservation {
    use daku_protocol::{Reachability, SignalState};
    match signal.probe(client, store, environment) {
        Ok(observation) => AvailabilityObservation {
            reachability: Reachability::Reachable,
            state: observation.state,
            build: None,
            rtt_ms: observation
                .payload
                .get("rtt_ms")
                .and_then(|value| value.as_u64())
                .unwrap_or(0),
            error: None,
        },
        Err(error) => AvailabilityObservation {
            reachability: Reachability::Unreachable,
            state: SignalState::Down,
            build: None,
            rtt_ms: 0,
            error: Some(error.to_string()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Thresholds;
    use crate::test_support::TempDb;

    const OK_JSON: &str = include_str!("../tests/fixtures/availability/ok.json");
    const HIBERNATING_HTML: &str = include_str!("../tests/fixtures/availability/hibernating.html");
    const UNAUTH_JSON: &str = include_str!("../tests/fixtures/availability/401.json");

    #[test]
    fn rtt_threshold_is_disabled_by_default() {
        let observation = classify_availability_response(200, "application/json", OK_JSON, 42);
        let unchanged = apply_rtt_threshold(observation.clone(), &Thresholds::default());
        assert_eq!(unchanged, observation);
        assert_eq!(unchanged.state, SignalState::Healthy);
    }

    #[test]
    fn slow_reachable_environment_degrades_with_ceiling_in_error() {
        let observation = classify_availability_response(200, "application/json", OK_JSON, 1200);
        let thresholds = Thresholds {
            availability_rtt_degraded_ms: Some(500),
            ..Thresholds::default()
        };
        let degraded = apply_rtt_threshold(observation, &thresholds);
        assert_eq!(degraded.state, SignalState::Degraded);
        assert_eq!(degraded.reachability, Reachability::Reachable);
        assert_eq!(degraded.error.as_deref(), Some("rtt 1200 ms > 500 ms"));
        // Fast stays healthy; unreachable never votes on latency.
        let fast = classify_availability_response(200, "application/json", OK_JSON, 100);
        assert_eq!(
            apply_rtt_threshold(fast, &thresholds).state,
            SignalState::Healthy
        );
        let down = classify_availability_response(429, "application/json", "{}", 5000);
        assert_eq!(
            apply_rtt_threshold(down, &thresholds).state,
            SignalState::Down
        );
    }

    #[test]
    fn classify_availability_ok_is_reachable_healthy() {
        let observation = classify_availability_response(200, "application/json", OK_JSON, 42);
        assert_eq!(observation.reachability, Reachability::Reachable);
        assert_eq!(observation.state, SignalState::Healthy);
        assert_eq!(
            observation.build.as_deref(),
            Some("glide-zurich-12-18-2025__patch0-hotfix1")
        );
        assert_eq!(observation.rtt_ms, 42);
    }

    #[test]
    fn classify_availability_hibernate_html_is_asleep() {
        let observation = classify_availability_response(200, "text/html", HIBERNATING_HTML, 80);
        assert_eq!(observation.reachability, Reachability::Asleep);
        assert_ne!(observation.reachability, Reachability::Unreachable);
        assert_eq!(observation.state, SignalState::Healthy);
        assert_eq!(observation.build, None);
    }

    #[test]
    fn classify_availability_200_empty_result_is_reachable() {
        let observation =
            classify_availability_response(200, "application/json", r#"{"result":[]}"#, 10);
        assert_eq!(observation.reachability, Reachability::Reachable);
        assert_eq!(observation.build, None);
        assert_eq!(observation.error, None);
    }

    #[test]
    fn classify_availability_429_records_transient_error() {
        let observation = classify_availability_response(429, "application/json", "{}", 5);
        assert_eq!(observation.reachability, Reachability::Unreachable);
        assert_eq!(observation.error.as_deref(), Some("HTTP 429"));
    }

    #[test]
    fn classify_availability_401_is_unreachable() {
        let observation = classify_availability_response(401, "application/json", UNAUTH_JSON, 15);
        assert_eq!(observation.reachability, Reachability::Unreachable);
        assert_eq!(observation.state, SignalState::Down);
        assert_eq!(observation.build, None);
    }

    #[test]
    fn persist_availability_snapshot_writes_one_row() {
        let db = TempDb::new("avail-persist");
        let store = db.store();
        let connection = store.open().unwrap();
        let observation = classify_availability_response(200, "application/json", OK_JSON, 42);
        persist_availability_snapshot(&connection, "prod", &observation, 1_700_000_000).unwrap();

        let row = persistence::load_signal_snapshot(&connection, "prod", AVAILABILITY_SIGNAL_ID)
            .unwrap()
            .expect("snapshot row");
        assert_eq!(row.signal_id, "availability");
        assert_eq!(row.environment_id, "prod");
        assert_eq!(row.state, "healthy");
        assert_eq!(row.observed_at, 1_700_000_000);
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "reachable");
        assert_eq!(payload["rtt_ms"], 42);
        assert_eq!(payload["build"], "glide-zurich-12-18-2025__patch0-hotfix1");

        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM signal_snapshots", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 1);
    }

    fn asleep_snapshot(connection: &rusqlite::Connection, observed_at: i64) {
        let observation = AvailabilityObservation {
            reachability: Reachability::Asleep,
            state: SignalState::Healthy,
            build: None,
            rtt_ms: 0,
            error: None,
        };
        persist_availability_snapshot(connection, "prod", &observation, observed_at).unwrap();
    }

    #[test]
    fn recent_reachability_reads_fresh_asleep_snapshot() {
        let db = TempDb::new("avail-recent-fresh");
        let store = db.store();
        let connection = store.open().unwrap();
        asleep_snapshot(&connection, 1_700_000_000);

        assert_eq!(
            recent_reachability(&connection, "prod", 1_700_000_010, REACHABILITY_REUSE_SECS),
            Some(Reachability::Asleep)
        );
    }

    #[test]
    fn recent_reachability_ignores_stale_snapshot() {
        let db = TempDb::new("avail-recent-stale");
        let store = db.store();
        let connection = store.open().unwrap();
        asleep_snapshot(&connection, 1_700_000_000);

        assert_eq!(
            recent_reachability(&connection, "prod", 1_700_000_301, REACHABILITY_REUSE_SECS),
            None
        );
        assert_eq!(
            recent_reachability(&connection, "dev", 1_700_000_010, REACHABILITY_REUSE_SECS),
            None
        );
    }
}
