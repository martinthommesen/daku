//! Flow / IntegrationHub errors Signal: errored `sys_flow_context` rows.
//!
//! Flow Designer / IntegrationHub failures are silent by default — no email,
//! no incident — so the Operator's first notice is often a downstream ticket.
//! This Signal counts contexts in `ERROR` state updated in the last hour and,
//! while non-zero, fetches the 10 newest for the drill-in.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const FLOW_SIGNAL_ID: &str = "flow";
pub const FLOW_ERROR_PATH: &str = "/api/now/stats/sys_flow_context?sysparm_count=true&sysparm_query=state=ERROR^sys_updated_on>javascript:gs.hoursAgoStart(1)";
/// Newest errors first, bounded for the drill-in.
pub const FLOW_ERROR_ROWS_PATH: &str = "/api/now/table/sys_flow_context?sysparm_fields=sys_id,name,state,sys_updated_on&sysparm_query=state=ERROR^sys_updated_on>javascript:gs.hoursAgoStart(1)^ORDERBYDESCsys_updated_on&sysparm_limit=10";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct FlowRow {
    sys_id: String,
    name: String,
    state: String,
    sys_updated_on: String,
}

fn row_text(row: &serde_json::Value, key: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(value) if value.is_number() => value.to_string(),
        _ => String::new(),
    }
}

pub fn flow_state(flow_error_1h: u64, thresholds: &Thresholds) -> SignalState {
    if flow_error_1h >= thresholds.flow_error_degraded_at {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

/// Offending error rows, newest first. A failed rows request yields no rows —
/// the count already determined the state.
fn fetch_flow_rows(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> (Vec<FlowRow>, bool) {
    let Ok(response) = client.request(environment, credentials, "GET", FLOW_ERROR_ROWS_PATH, None)
    else {
        return (Vec::new(), false);
    };
    if response.status != 200 {
        return (Vec::new(), false);
    }
    let rows: Vec<FlowRow> = serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
        .ok()
        .and_then(|value| {
            value
                .get("result")
                .and_then(|result| result.as_array())
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .filter_map(|row| {
            let sys_id = row_text(row, "sys_id");
            if sys_id.is_empty() {
                return None;
            }
            Some(FlowRow {
                sys_id,
                name: row_text(row, "name"),
                state: row_text(row, "state"),
                sys_updated_on: row_text(row, "sys_updated_on"),
            })
        })
        .collect();
    crate::collector::take_bounded(rows)
}

#[derive(Default)]
pub struct FlowSignal;

pub type FlowCollector = PerEnvironmentCollector<FlowSignal>;

impl Signal for FlowSignal {
    fn id(&self) -> &'static str {
        FLOW_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let flow_error_1h =
            fetch_aggregate_count(client, environment, credentials, FLOW_ERROR_PATH)?;
        // Rows only while unhealthy: a zero count costs no extra request.
        let (error_rows, error_rows_truncated) = if flow_error_1h > 0 {
            fetch_flow_rows(client, environment, credentials)
        } else {
            (Vec::new(), false)
        };
        Ok(Observation {
            state: flow_state(flow_error_1h, &environment.thresholds),
            payload: serde_json::json!({
                "flow_error_1h": flow_error_1h,
                "error_rows": error_rows,
                "error_rows_truncated": error_rows_truncated,
            }),
            sample: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::test_support::{TempDb, prod};
    use std::sync::Arc;

    use crate::collector::SignalCollector;
    use crate::config::MemoryCredentialStore;
    use crate::persistence::{self, StateStore};
    use crate::servicenow::{
        HttpRequest, HttpResponse, HttpTransport, ServiceNowClient, SystemClock,
    };

    use super::*;

    #[test]
    fn flow_signal_zero_is_healthy() {
        assert_eq!(flow_state(0, &Thresholds::default()), SignalState::Healthy);
    }

    #[test]
    fn flow_signal_nonzero_is_degraded() {
        assert_eq!(flow_state(2, &Thresholds::default()), SignalState::Degraded);
    }

    #[test]
    fn flow_threshold_override_tolerates_noise() {
        let thresholds = Thresholds {
            flow_error_degraded_at: 5,
            ..Thresholds::default()
        };
        assert_eq!(flow_state(4, &thresholds), SignalState::Healthy);
        assert_eq!(flow_state(5, &thresholds), SignalState::Degraded);
    }

    struct FlowCountTransport {
        body: &'static str,
        /// Rows body for table URLs; `None` panics, proving a healthy tick
        /// fetches no rows at all.
        rows: Option<&'static str>,
    }

    impl HttpTransport for FlowCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("/api/now/table/sys_flow_context") {
                let Some(rows) = self.rows else {
                    panic!("healthy flow tick must not fetch rows: {}", request.url);
                };
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: rows.into(),
                });
            }
            assert!(
                request.url.contains("/api/now/stats/sys_flow_context"),
                "flow collector must use Aggregate API: {}",
                request.url
            );
            assert!(
                request.url.contains("state=ERROR"),
                "flow query must count ERROR contexts: {}",
                request.url
            );
            assert!(
                request.url.contains("sys_updated_on") && request.url.contains("hoursAgoStart"),
                "flow query must be date-bound: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    const ERROR_ROWS: &str = r#"{"result":[
        {"sys_id":"f1","name":"Sync orders","state":"ERROR","sys_updated_on":"2026-01-15 12:00:01"}
    ]}"#;

    fn collect_with(body: &'static str, rows: Option<&'static str>) -> (TempDb, StateStore) {
        let db = TempDb::new("flow");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = FlowCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(FlowCountTransport { body, rows }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn flow_signal_zero_writes_healthy_snapshot_without_sample() {
        let (_db, store) = collect_with(include_str!("../tests/fixtures/flow/count_0.json"), None);
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", FLOW_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["flow_error_1h"], 0);
        assert!(
            persistence::load_signal_samples(&connection, "prod", FLOW_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn flow_signal_nonzero_writes_degraded_snapshot_with_rows() {
        let (_db, store) = collect_with(
            include_str!("../tests/fixtures/flow/count_2.json"),
            Some(ERROR_ROWS),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", FLOW_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["flow_error_1h"], 2);
        assert_eq!(payload["error_rows"][0]["name"], "Sync orders");
        assert_eq!(payload["error_rows"][0]["state"], "ERROR");
        assert_eq!(payload["error_rows_truncated"], false);
        assert!(
            persistence::load_signal_samples(&connection, "prod", FLOW_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    struct FlowFailTransport;

    impl HttpTransport for FlowFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn flow_signal_probe_failure_is_down_without_sample() {
        let db = TempDb::new("flow-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = FlowCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(FlowFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", FLOW_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("flow_error_1h").is_none());
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn flow_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("flow-asleep");
        let store = db.store();
        let observed_at = crate::collector::unix_now();
        {
            let connection = store.open().unwrap();
            persist_availability_snapshot(
                &connection,
                "prod",
                &AvailabilityObservation {
                    reachability: Reachability::Asleep,
                    state: SignalState::Healthy,
                    build: None,
                    rtt_ms: 0,
                    error: None,
                },
                observed_at,
            )
            .unwrap();
        }

        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        FlowCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", FLOW_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
