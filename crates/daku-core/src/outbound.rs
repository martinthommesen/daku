//! Outbound / integration-failures Signal: 1h HTTP 4xx/5xx count on `sys_outbound_http_log`.

use daku_protocol::SignalState;

use crate::collector::{Observation, PerEnvironmentCollector, Signal};
use crate::config::{CredentialStore, EnvironmentConfig, Thresholds};
use crate::servicenow::{ServiceNowClient, fetch_aggregate_count};

pub const OUTBOUND_SIGNAL_ID: &str = "outbound";
pub const OUTBOUND_HTTP_PATH: &str = "/api/now/stats/sys_outbound_http_log?sysparm_count=true&sysparm_query=http_status>=400^sys_created_on>javascript:gs.hoursAgoStart(1)";
/// Newest failures first, bounded for the drill-in.
pub const OUTBOUND_FAILURE_ROWS_PATH: &str = "/api/now/table/sys_outbound_http_log?sysparm_fields=sys_id,url,http_status,sys_created_on&sysparm_query=http_status>=400^sys_created_on>javascript:gs.hoursAgoStart(1)^ORDERBYDESCsys_created_on&sysparm_limit=10";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
struct OutboundRow {
    sys_id: String,
    url: String,
    http_status: String,
    sys_created_on: String,
}

/// Keeps scheme + host + path; drops query and fragment, which can carry
/// third-party secrets the drill-in never needs.
fn redact_url(url: String) -> String {
    let without_fragment = url.split('#').next().unwrap_or("").to_owned();
    without_fragment.split('?').next().unwrap_or("").to_owned()
}

fn row_text(row: &serde_json::Value, key: &str) -> String {
    match row.get(key) {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(value) if value.is_number() => value.to_string(),
        _ => String::new(),
    }
}

pub fn outbound_state(outbound_http_4xx_5xx_1h: u64, thresholds: &Thresholds) -> SignalState {
    if outbound_http_4xx_5xx_1h >= thresholds.outbound_failures_degraded_at {
        SignalState::Degraded
    } else {
        SignalState::Healthy
    }
}

/// Offending failure rows, newest first. A failed rows request yields no
/// rows — the count already determined the state.
fn fetch_outbound_rows(
    client: &ServiceNowClient,
    environment: &EnvironmentConfig,
    credentials: &dyn CredentialStore,
) -> (Vec<OutboundRow>, bool) {
    let Ok(response) = client.request(
        environment,
        credentials,
        "GET",
        OUTBOUND_FAILURE_ROWS_PATH,
        None,
    ) else {
        return (Vec::new(), false);
    };
    if response.status != 200 {
        return (Vec::new(), false);
    }
    let rows: Vec<OutboundRow> =
        serde_json::from_slice::<serde_json::Value>(response.body.as_bytes())
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
                Some(OutboundRow {
                    sys_id,
                    url: redact_url(row_text(row, "url")),
                    http_status: row_text(row, "http_status"),
                    sys_created_on: row_text(row, "sys_created_on"),
                })
            })
            .collect();
    crate::collector::take_bounded(rows)
}

#[derive(Default)]
pub struct OutboundSignal;

pub type OutboundCollector = PerEnvironmentCollector<OutboundSignal>;

impl Signal for OutboundSignal {
    fn id(&self) -> &'static str {
        OUTBOUND_SIGNAL_ID
    }

    fn probe(
        &self,
        client: &ServiceNowClient,
        credentials: &dyn CredentialStore,
        environment: &EnvironmentConfig,
    ) -> anyhow::Result<Observation> {
        let outbound_http_4xx_5xx_1h =
            fetch_aggregate_count(client, environment, credentials, OUTBOUND_HTTP_PATH)?;
        // Rows only while unhealthy: a zero count costs no extra request.
        let (failure_rows, failure_rows_truncated) = if outbound_http_4xx_5xx_1h > 0 {
            fetch_outbound_rows(client, environment, credentials)
        } else {
            (Vec::new(), false)
        };
        Ok(Observation {
            state: outbound_state(outbound_http_4xx_5xx_1h, &environment.thresholds),
            payload: serde_json::json!({
                "outbound_http_4xx_5xx_1h": outbound_http_4xx_5xx_1h,
                "failure_rows": failure_rows,
                "failure_rows_truncated": failure_rows_truncated,
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
    fn outbound_signal_zero_is_healthy() {
        assert_eq!(
            outbound_state(0, &Thresholds::default()),
            SignalState::Healthy
        );
    }

    #[test]
    fn outbound_signal_nonzero_is_degraded() {
        assert_eq!(
            outbound_state(3, &Thresholds::default()),
            SignalState::Degraded
        );
    }

    #[test]
    fn outbound_threshold_override_tolerates_noise() {
        let thresholds = Thresholds {
            outbound_failures_degraded_at: 5,
            ..Thresholds::default()
        };
        assert_eq!(outbound_state(4, &thresholds), SignalState::Healthy);
        assert_eq!(outbound_state(5, &thresholds), SignalState::Degraded);
    }

    struct OutboundCountTransport {
        body: &'static str,
        /// Rows body for table URLs; `None` panics, proving a healthy tick
        /// fetches no rows at all.
        rows: Option<&'static str>,
    }

    impl HttpTransport for OutboundCountTransport {
        fn execute(&self, request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            if request.url.contains("/api/now/table/sys_outbound_http_log") {
                let Some(rows) = self.rows else {
                    panic!("healthy outbound tick must not fetch rows: {}", request.url);
                };
                return Ok(HttpResponse {
                    status: 200,
                    headers: vec![("content-type".into(), "application/json".into())],
                    body: rows.into(),
                });
            }
            assert!(
                request.url.contains("/api/now/stats/sys_outbound_http_log"),
                "outbound collector must use Aggregate API: {}",
                request.url
            );
            assert!(
                request.url.contains("http_status>=400")
                    || request.url.contains("http_status%3E=400"),
                "outbound query must count HTTP 4xx/5xx: {}",
                request.url
            );
            assert!(
                request.url.contains("sys_created_on") && request.url.contains("hoursAgoStart"),
                "outbound query must be date-bound: {}",
                request.url
            );
            Ok(HttpResponse {
                status: 200,
                headers: vec![("content-type".into(), "application/json".into())],
                body: self.body.into(),
            })
        }
    }

    const FAILURE_ROWS: &str = r#"{"result":[
        {"sys_id":"o1","url":"https://partner.example.com/hook?token=secret","http_status":500,"sys_created_on":"2026-01-15 12:00:01"}
    ]}"#;

    fn collect_with(body: &'static str, rows: Option<&'static str>) -> (TempDb, StateStore) {
        let db = TempDb::new("outbound");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = OutboundCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(OutboundCountTransport { body, rows }, SystemClock),
            store,
        );
        collector.collect().unwrap();
        let reopened = db.store();
        (db, reopened)
    }

    #[test]
    fn outbound_signal_zero_writes_healthy_snapshot_without_sample() {
        let (_db, store) = collect_with(
            include_str!("../tests/fixtures/outbound/count_0.json"),
            None,
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "healthy");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["outbound_http_4xx_5xx_1h"], 0);
        assert!(
            persistence::load_signal_samples(&connection, "prod", OUTBOUND_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn outbound_signal_nonzero_writes_degraded_snapshot() {
        let (_db, store) = collect_with(
            include_str!("../tests/fixtures/outbound/count_3.json"),
            Some(FAILURE_ROWS),
        );
        let connection = store.open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "degraded");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["outbound_http_4xx_5xx_1h"], 3);
        assert_eq!(
            payload["failure_rows"][0]["url"], "https://partner.example.com/hook",
            "query strings never reach the drill-in"
        );
        assert_eq!(payload["failure_rows"][0]["http_status"], "500");
        assert_eq!(payload["failure_rows_truncated"], false);
        assert!(
            persistence::load_signal_samples(&connection, "prod", OUTBOUND_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    struct OutboundFailTransport;

    impl HttpTransport for OutboundFailTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            anyhow::bail!("offline")
        }
    }

    #[test]
    fn outbound_signal_probe_failure_is_down_without_sample() {
        let db = TempDb::new("outbound-fail");
        let store = db.store();
        let credentials = Arc::new(MemoryCredentialStore::default());
        credentials.insert("prod", r#"{"username":"reader","password":"secret"}"#);
        let collector = OutboundCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(OutboundFailTransport, SystemClock),
            store,
        );
        collector.collect().unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "down");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["reachability"], "unreachable");
        assert!(payload.get("outbound_http_4xx_5xx_1h").is_none());
        assert!(
            persistence::load_signal_samples(&connection, "prod", OUTBOUND_SIGNAL_ID)
                .unwrap()
                .is_empty()
        );
    }

    struct NoProbeTransport;

    impl HttpTransport for NoProbeTransport {
        fn execute(&self, _request: &HttpRequest) -> anyhow::Result<HttpResponse> {
            panic!("must not probe an asleep Environment");
        }
    }

    #[test]
    fn outbound_signal_skips_when_availability_asleep() {
        use crate::availability::{AvailabilityObservation, persist_availability_snapshot};
        use daku_protocol::{Reachability, SignalState};

        let db = TempDb::new("outbound-asleep");
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
        OutboundCollector::new(
            vec![prod()],
            credentials,
            ServiceNowClient::new(NoProbeTransport, SystemClock),
            store,
        )
        .collect()
        .unwrap();

        let connection = db.store().open().unwrap();
        let row = persistence::load_signal_snapshot(&connection, "prod", OUTBOUND_SIGNAL_ID)
            .unwrap()
            .expect("snapshot");
        assert_eq!(row.state, "skipped");
        let payload: serde_json::Value = serde_json::from_str(&row.payload_json).unwrap();
        assert_eq!(payload["skipped"], "asleep");
    }
}
